use crate::error::{io_at, Error, Result};
use crate::manifest::{FileKind, GenerationManifest, SavedFile};
use crate::snapshot::{apply_meta, copy_tree};
use crate::store::generation_dir;
use crate::util::{hash_file, safe_rel, safe_unix_path, valid_package_name};
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, Default)]
pub struct RestoreReport {
    pub restored: usize,
    pub skipped: usize,
}

/// Restore selected original paths from a verified generation into a sysroot.
/// Existing entries are preserved unless `overwrite` is explicitly enabled.
pub fn restore_generation(
    recovery: &Path,
    id: u64,
    sysroot: &Path,
    selected: &[String],
    overwrite: bool,
) -> Result<RestoreReport> {
    let generation = generation_dir(recovery, id);
    let manifest = GenerationManifest::read(&generation.join("manifest.json"))?;
    let root = fs::canonicalize(sysroot).map_err(|source| io_at(sysroot, source))?;
    for path in selected {
        if !safe_unix_path(path) {
            return Err(Error::msg(format!("restore path `{path}` must be absolute")));
        }
    }
    let mut entries: Vec<&SavedFile> = manifest
        .files
        .iter()
        .filter(|entry| selected.is_empty() || selected.iter().any(|path| selected_path(entry, path)))
        .collect();
    if entries.is_empty() {
        return Err(Error::msg("no saved files match the requested restore paths"));
    }

    // Create parent directories before files and restore directory metadata last.
    entries.sort_by(|a, b| {
        kind_order(a.kind)
            .cmp(&kind_order(b.kind))
            .then_with(|| a.original.cmp(&b.original))
    });
    let mut report = RestoreReport::default();
    let mut dirs_for_metadata = Vec::new();
    for entry in entries {
        let rel = safe_rel(&entry.stored)
            .ok_or_else(|| Error::msg(format!("unsafe stored path `{}`", entry.stored)))?;
        if !safe_unix_path(&entry.original) {
            return Err(Error::msg(format!("unsafe original path `{}`", entry.original)));
        }
        if entry.stored != entry.original.trim_start_matches('/') {
            return Err(Error::msg(format!(
                "stored path does not match original path {}",
                entry.original
            )));
        }
        let destination = root.join(&rel);
        ensure_inside_root(&root, &destination)?;
        check_parents(&root, &destination)?;
        let source = generation.join("files").join(rel);

        match entry.kind {
            FileKind::Dir => {
                let source_meta = fs::symlink_metadata(&source)
                    .map_err(|err| io_at(&source, err))?;
                if !source_meta.is_dir() || source_meta.file_type().is_symlink() {
                    return Err(Error::msg(format!(
                        "saved directory {} is not a directory",
                        entry.original
                    )));
                }
                let mut restored = false;
                match fs::symlink_metadata(&destination) {
                    Ok(meta) if meta.is_dir() && !meta.file_type().is_symlink() => {
                        if overwrite {
                            dirs_for_metadata.push((destination, entry));
                            restored = true;
                        } else {
                            report.skipped += 1;
                        }
                    }
                    Ok(_) if !overwrite => report.skipped += 1,
                    Ok(_) => {
                        remove_non_directory(&destination)?;
                        fs::create_dir_all(&destination)
                            .map_err(|source| io_at(&destination, source))?;
                        dirs_for_metadata.push((destination, entry));
                        restored = true;
                    }
                    Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                        fs::create_dir_all(&destination)
                            .map_err(|source| io_at(&destination, source))?;
                        dirs_for_metadata.push((destination, entry));
                        restored = true;
                    }
                    Err(source) => return Err(io_at(&destination, source)),
                }
                if restored {
                    report.restored += 1;
                }
            }
            FileKind::File => {
                let source_meta = fs::symlink_metadata(&source)
                    .map_err(|err| io_at(&source, err))?;
                if !source_meta.is_file() || source_meta.file_type().is_symlink() {
                    return Err(Error::msg(format!(
                        "saved file {} is not a regular file",
                        entry.original
                    )));
                }
                let expected = entry.sha256.as_deref().ok_or_else(|| {
                    Error::msg(format!("{} has no recorded checksum", entry.original))
                })?;
                let actual = hash_file(&source)?;
                if actual != expected {
                    return Err(Error::msg(format!(
                        "saved file checksum mismatch for {}",
                        entry.original
                    )));
                }
                if destination.exists() || fs::symlink_metadata(&destination).is_ok() {
                    if !overwrite {
                        report.skipped += 1;
                        continue;
                    }
                    remove_non_directory(&destination)?;
                }
                if let Some(parent) = destination.parent() {
                    fs::create_dir_all(parent).map_err(|source| io_at(parent, source))?;
                }
                fs::copy(&source, &destination).map_err(|source| io_at(&destination, source))?;
                apply_meta(
                    &destination,
                    entry.mode,
                    entry.uid,
                    entry.gid,
                    entry.mtime_unix,
                )?;
                report.restored += 1;
            }
            FileKind::Symlink => {
                let source_meta = fs::symlink_metadata(&source)
                    .map_err(|err| io_at(&source, err))?;
                if !source_meta.file_type().is_symlink() {
                    return Err(Error::msg(format!(
                        "saved symlink {} is not a symlink",
                        entry.original
                    )));
                }
                let target = fs::read_link(&source).map_err(|err| io_at(&source, err))?;
                let expected = entry.link_target.as_deref().unwrap_or_default();
                if target.to_string_lossy() != expected {
                    return Err(Error::msg(format!(
                        "saved symlink target mismatch for {}",
                        entry.original
                    )));
                }
                if destination.exists() || fs::symlink_metadata(&destination).is_ok() {
                    if !overwrite {
                        report.skipped += 1;
                        continue;
                    }
                    remove_non_directory(&destination)?;
                }
                if let Some(parent) = destination.parent() {
                    fs::create_dir_all(parent).map_err(|source| io_at(parent, source))?;
                }
                create_symlink(&target, &destination)?;
                report.restored += 1;
            }
        }
    }

    for (path, entry) in dirs_for_metadata.into_iter().rev() {
        apply_meta(
            &path,
            entry.mode,
            entry.uid,
            entry.gid,
            entry.mtime_unix,
        )?;
    }
    Ok(report)
}

/// Copy a sealed generation to another mounted directory for offline storage.
pub fn export_generation(recovery: &Path, id: u64, destination: &Path) -> Result<PathBuf> {
    let generation = generation_dir(recovery, id);
    GenerationManifest::read(&generation.join("manifest.json"))?;
    if !generation.is_dir() {
        return Err(Error::msg(format!("generation {id:08} does not exist")));
    }
    fs::create_dir_all(destination).map_err(|source| io_at(destination, source))?;
    let recovery_root = fs::canonicalize(recovery).map_err(|source| io_at(recovery, source))?;
    let export_root = fs::canonicalize(destination).map_err(|source| io_at(destination, source))?;
    if export_root.starts_with(&recovery_root) {
        return Err(Error::msg(
            "export destination must be outside the recovery store",
        ));
    }
    let exported = destination.join(format!("recorize-generation-{id:08}"));
    if exported.exists() {
        return Err(Error::msg(format!(
            "{} already exists; choose another export destination",
            exported.display()
        )));
    }
    copy_tree(&generation, &exported)?;
    Ok(exported)
}

/// Install the package set from a generation into a separately mounted,
/// effectively empty root, then restore the generation's saved files.
/// This does not partition or format disks and does not install a bootloader.
pub fn reinstall_generation(recovery: &Path, id: u64, target: &Path) -> Result<()> {
    let generation = generation_dir(recovery, id);
    let manifest = GenerationManifest::read(&generation.join("manifest.json"))?;
    let root = fs::canonicalize(target).map_err(|source| io_at(target, source))?;
    let running_root = fs::canonicalize("/").map_err(|source| io_at("/", source))?;
    if root == running_root || !is_mountpoint(&root)? {
        return Err(Error::msg(
            "reinstall target must be a separate mounted filesystem, not the running root",
        ));
    }
    let mounts = mounted_paths()?;
    if !install_target_is_empty(&root, &mounts)? {
        return Err(Error::msg(
            "reinstall target must be empty except for lost+found and mounted child filesystems; ReCorize will not erase existing files",
        ));
    }

    let mut packages = vec!["base".to_string(), "linux-firmware".to_string()];
    for kernel in ["linux-cachyos", "linux", "linux-lts", "linux-zen"] {
        if manifest.packages_all.iter().any(|package| package.name == kernel) {
            packages.push(kernel.to_string());
        }
    }
    if packages.len() == 2 {
        packages.push(if manifest.os.is_cachyos() {
            "linux-cachyos".to_string()
        } else {
            "linux".to_string()
        });
    }
    packages.extend(
        manifest
            .packages_explicit
            .iter()
            .filter(|name| valid_package_name(name))
            .cloned(),
    );
    let mut unique = HashSet::new();
    packages.retain(|name| unique.insert(name.clone()));

    let stage = std::env::temp_dir().join(format!(
        "recorize-reinstall-{}-{id}",
        std::process::id()
    ));
    fs::create_dir(&stage).map_err(|source| io_at(&stage, source))?;
    let stage_result = stage_pacman_config(&manifest, &stage);
    let config_path = match stage_result {
        Ok(path) => path,
        Err(err) => {
            let _ = fs::remove_dir_all(&stage);
            return Err(err);
        }
    };

    println!("Installing saved package selection into {}", root.display());
    let result = (|| {
        let status = Command::new("pacstrap")
            .arg("-C")
            .arg(&config_path)
            .arg(&root)
            .args(&packages)
            .status()
            .map_err(|source| io_at("pacstrap", source))?;
        if !status.success() {
            return Err(Error::msg(format!("pacstrap exited with {status}")));
        }
        restore_generation(recovery, id, &root, &[], true)?;
        let fstab = Command::new("genfstab")
            .arg("-U")
            .arg(&root)
            .output()
            .map_err(|source| io_at("genfstab", source))?;
        if !fstab.status.success() {
            return Err(Error::msg("genfstab failed after package installation"));
        }
        let fstab_path = root.join("etc/fstab");
        fs::write(&fstab_path, fstab.stdout).map_err(|source| io_at(&fstab_path, source))?;
        println!("Packages and saved files restored; generated {}.", fstab_path.display());
        println!("Install and configure a bootloader for this mounted system before rebooting.");
        Ok(())
    })();
    let _ = fs::remove_dir_all(&stage);
    result
}

fn stage_pacman_config(
    manifest: &GenerationManifest,
    stage: &Path,
) -> Result<PathBuf> {
    let mirrors = stage.join("mirrorlists");
    for mirrorlist in &manifest.mirrorlists {
        if !safe_unix_path(&mirrorlist.path) {
            return Err(Error::msg(format!(
                "unsafe mirrorlist path `{}` in generation",
                mirrorlist.path
            )));
        }
        let rel = safe_rel(mirrorlist.path.trim_start_matches('/'))
            .ok_or_else(|| Error::msg("invalid mirrorlist path in generation"))?;
        let destination = mirrors.join(rel);
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent).map_err(|source| io_at(parent, source))?;
        }
        fs::write(&destination, &mirrorlist.contents)
            .map_err(|source| io_at(&destination, source))?;
    }
    let mut config_text = manifest.pacman_conf.clone();
    for mirrorlist in &manifest.mirrorlists {
        let rel = safe_rel(mirrorlist.path.trim_start_matches('/'))
            .ok_or_else(|| Error::msg("invalid mirrorlist path in generation"))?;
        let staged = mirrors.join(rel);
        let staged = staged
            .to_str()
            .ok_or_else(|| Error::msg("temporary mirrorlist path is not valid UTF-8"))?;
        config_text = config_text.replace(&mirrorlist.path, staged);
    }
    let config_path = stage.join("pacman.conf");
    if config_text.trim().is_empty() {
        return Err(Error::msg("generation does not contain a pacman.conf"));
    }
    fs::write(&config_path, config_text).map_err(|source| io_at(&config_path, source))?;
    Ok(config_path)
}

fn mounted_paths() -> Result<HashSet<PathBuf>> {
    let text = fs::read_to_string("/proc/self/mountinfo")
        .map_err(|source| io_at("/proc/self/mountinfo", source))?;
    let mut paths = HashSet::new();
    for line in text.lines() {
        if let Some(value) = line.split_whitespace().nth(4) {
            let decoded = value
                .replace("\\040", " ")
                .replace("\\011", "\t")
                .replace("\\012", "\n")
                .replace("\\134", "\\");
            let path = PathBuf::from(decoded);
            paths.insert(fs::canonicalize(&path).unwrap_or(path));
        }
    }
    Ok(paths)
}

fn is_mountpoint(path: &Path) -> Result<bool> {
    Ok(mounted_paths()?.contains(path))
}

fn install_target_is_empty(root: &Path, mounts: &HashSet<PathBuf>) -> Result<bool> {
    let entries = fs::read_dir(root).map_err(|source| io_at(root, source))?;
    for entry in entries {
        let entry = entry.map_err(|source| io_at(root, source))?;
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path).map_err(|source| io_at(&path, source))?;
        if entry.file_name().to_string_lossy() == "lost+found"
            && metadata.is_dir()
            && !metadata.file_type().is_symlink()
        {
            continue;
        }
        if metadata.is_dir()
            && !metadata.file_type().is_symlink()
            && directory_is_mount_scaffold(&path, mounts)?
        {
            continue;
        }
        return Ok(false);
    }
    Ok(true)
}

fn directory_is_mount_scaffold(path: &Path, mounts: &HashSet<PathBuf>) -> Result<bool> {
    if mounts.contains(path) {
        return Ok(true);
    }
    for entry in fs::read_dir(path).map_err(|source| io_at(path, source))? {
        let entry = entry.map_err(|source| io_at(path, source))?;
        let child = entry.path();
        if mounts.contains(&child) {
            continue;
        }
        let metadata = fs::symlink_metadata(&child).map_err(|source| io_at(&child, source))?;
        if !metadata.is_dir()
            || metadata.file_type().is_symlink()
            || !directory_is_mount_scaffold(&child, mounts)?
        {
            return Ok(false);
        }
    }
    Ok(true)
}

fn selected_path(entry: &SavedFile, requested: &str) -> bool {
    entry.original == requested
        || (entry.original.starts_with(requested.trim_end_matches('/'))
            && entry.original.as_bytes().get(requested.trim_end_matches('/').len()) == Some(&b'/'))
}

fn kind_order(kind: FileKind) -> u8 {
    match kind {
        FileKind::Dir => 0,
        FileKind::File => 1,
        FileKind::Symlink => 2,
    }
}

fn ensure_inside_root(root: &Path, destination: &Path) -> Result<()> {
    if destination.starts_with(root) {
        Ok(())
    } else {
        Err(Error::msg(format!(
            "restore path {} escapes sysroot {}",
            destination.display(),
            root.display()
        )))
    }
}

fn check_parents(root: &Path, destination: &Path) -> Result<()> {
    // For offline sysroots, reject existing symlink parents so a crafted mount
    // tree cannot redirect a restore outside the selected root.
    if root == Path::new("/") {
        return Ok(());
    }
    let relative = destination
        .strip_prefix(root)
        .map_err(|_| Error::msg("restore destination is outside sysroot"))?;
    let mut parent = root.to_path_buf();
    let components: Vec<_> = relative.components().collect();
    for component in components.iter().take(components.len().saturating_sub(1)) {
        parent.push(component.as_os_str());
        match fs::symlink_metadata(&parent) {
            Ok(meta) if meta.file_type().is_symlink() => {
                return Err(Error::msg(format!(
                    "refusing to restore through symlink parent {}",
                    parent.display()
                )))
            }
            Ok(meta) if !meta.is_dir() => {
                return Err(Error::msg(format!(
                    "restore parent {} is not a directory",
                    parent.display()
                )))
            }
            _ => {}
        }
    }
    Ok(())
}

fn remove_non_directory(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.is_dir() && !meta.file_type().is_symlink() => Err(Error::msg(format!(
            "refusing to replace directory {} with a file",
            path.display()
        ))),
        Ok(_) => fs::remove_file(path).map_err(|source| io_at(path, source)),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(io_at(path, source)),
    }
}

fn create_symlink(target: &Path, destination: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(target, destination)
            .map_err(|source| io_at(destination, source))
    }
    #[cfg(not(unix))]
    {
        let _ = target;
        Err(Error::msg("symlink restoration requires Unix"))
    }
}
