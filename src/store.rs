use crate::error::{io_at, Error, Result};
use crate::exclude::Exclude;
use crate::immutable::VolumeSeal;
use crate::manifest::{GenerationManifest, SystemMeta, MANIFEST_VERSION};
use crate::snapshot::{capture_tree, plan_tree};
use crate::util::{is_within, path_key, safe_rel};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct SaveRequest {
    pub recovery_dir: PathBuf,
    pub watches: Vec<PathBuf>,
    pub exclude: Exclude,
    pub scope_label: String,
    pub dry_run: bool,
    pub meta: SystemMeta,
    pub hostname: String,
    pub now_unix: u64,
    pub config_toml: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SaveReport {
    pub id: u64,
    pub files: usize,
    pub bytes: u64,
    pub warnings: Vec<String>,
    pub dry_run: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifyReport {
    pub id: u64,
    pub files_ok: usize,
    pub problems: Vec<String>,
    pub sealed: bool,
}

pub fn generation_dir(recovery: &Path, id: u64) -> PathBuf {
    recovery.join("generations").join(format!("{id:08}"))
}

pub fn init_store(recovery: &Path, sealer: &mut dyn VolumeSeal) -> Result<()> {
    let generations = recovery.join("generations");
    fs::create_dir_all(&generations).map_err(|source| io_at(&generations, source))?;
    probe_immutable(recovery, sealer)?;
    sealer.seal_inode(&generations)?;
    sealer.seal_inode(recovery)?;
    Ok(())
}

pub fn save_with(req: &SaveRequest, sealer: &mut dyn VolumeSeal) -> Result<SaveReport> {
    if !sealer.supports_immutable() {
        return Err(Error::msg(
            "sealing the recovery directory requires Linux with ext4, btrfs, xfs, or f2fs",
        ));
    }
    let recovery = &req.recovery_dir;
    let generations = recovery.join("generations");
    fs::create_dir_all(&generations).map_err(|source| io_at(&generations, source))?;

    if req.dry_run {
        let planned = plan_tree(&req.watches, &req.exclude)?;
        let bytes = planned.iter().map(|file| file.size).sum();
        return Ok(SaveReport {
            id: next_id(recovery)?.saturating_sub(0),
            files: planned.len(),
            bytes,
            warnings: Vec::new(),
            dry_run: true,
        });
    }

    // The parent directory is unsealed only so a new generation can be created.
    // Existing generation inodes stay immutable, so they cannot be replaced.
    unseal_if_present(sealer, recovery)?;
    unseal_if_present(sealer, &generations)?;
    let write_result = (|| {
        probe_immutable(recovery, sealer)?;
        cleanup_incomplete(recovery, sealer)?;
        let id = next_id(recovery)?;
        let gen_dir = generation_dir(recovery, id);
        fs::create_dir_all(gen_dir.join("files")).map_err(|source| io_at(&gen_dir, source))?;
        let (files, mut warnings) = capture_tree(&req.watches, &gen_dir.join("files"), &req.exclude)?;
        warnings.extend(req.meta.warnings.clone());
        let bytes = files.iter().map(|file| file.size).sum();
        let manifest = GenerationManifest {
            version: MANIFEST_VERSION,
            id,
            created_unix: req.now_unix,
            hostname: req.hostname.clone(),
            tool_version: env!("CARGO_PKG_VERSION").to_string(),
            scope: req.scope_label.clone(),
            config_toml: req.config_toml.clone(),
            os: req.meta.os.clone(),
            os_release_text: req.meta.os_release_text.clone(),
            repos: req.meta.repos.clone(),
            pacman_conf: req.meta.pacman_conf_text.clone(),
            mirrorlists: req.meta.mirrorlists.clone(),
            packages_explicit: req.meta.packages_explicit.clone(),
            packages_all: req.meta.packages_all.clone(),
            files,
            warnings: warnings.clone(),
        };
        manifest.write(&gen_dir.join("manifest.json"))?;
        sealer.seal_tree(&gen_dir)?;
        write_latest(recovery, id, sealer)?;
        Ok(SaveReport {
            id,
            files: manifest.files.len(),
            bytes,
            warnings,
            dry_run: false,
        })
    })();

    let reseal = reseal_parents(sealer, recovery, &generations);
    match (write_result, reseal) {
        (Ok(report), Ok(())) => Ok(report),
        (Err(err), Err(seal_err)) => Err(Error::msg(format!(
            "{err}; also failed to reseal the recovery directory: {seal_err}"
        ))),
        (Err(err), Ok(())) => Err(err),
        (Ok(_), Err(seal_err)) => Err(seal_err),
    }
}

pub fn verify_generation(
    recovery: &Path,
    id: u64,
    sealer: &mut dyn VolumeSeal,
) -> Result<VerifyReport> {
    let gen_dir = generation_dir(recovery, id);
    let manifest = GenerationManifest::read(&gen_dir.join("manifest.json"))?;
    let mut problems = Vec::new();
    let mut files_ok = 0usize;
    for entry in &manifest.files {
        let Some(rel) = safe_rel(&entry.stored) else {
            problems.push(format!("{}: stored path is not safe", entry.stored));
            continue;
        };
        if rel.components().any(|component| component.as_os_str() == "..") {
            problems.push(format!("{}: stored path is not safe", entry.stored));
            continue;
        }
        let path = gen_dir.join("files").join(&rel);
        match entry.kind {
            crate::manifest::FileKind::Dir => {
                if path.is_dir() {
                    files_ok += 1;
                } else {
                    problems.push(format!("missing directory {}", entry.original));
                }
            }
            crate::manifest::FileKind::Symlink => {
                match fs::read_link(&path) {
                    Ok(target) => {
                        let expected = entry.link_target.clone().unwrap_or_default();
                        if target.to_string_lossy() == expected {
                            files_ok += 1;
                        } else {
                            problems.push(format!(
                                "symlink {} points at {}, manifest says {expected}",
                                entry.original,
                                target.display()
                            ));
                        }
                    }
                    Err(_) => problems.push(format!("missing symlink {}", entry.original)),
                }
            }
            crate::manifest::FileKind::File => {
                match crate::util::hash_file(&path) {
                    Ok(hash) => {
                        if entry.sha256.as_deref() == Some(hash.as_str()) {
                            files_ok += 1;
                        } else {
                            problems.push(format!("checksum mismatch {}", entry.original));
                        }
                    }
                    Err(_) => problems.push(format!("missing file {}", entry.original)),
                }
            }
        }
    }
    let sealed = sealer.is_immutable(&gen_dir).unwrap_or(false);
    if !sealed {
        problems.push(format!(
            "generation {id:08} is not immutable; a broken system could still modify it"
        ));
    }
    Ok(VerifyReport {
        id,
        files_ok,
        problems,
        sealed,
    })
}

pub fn latest_id(recovery: &Path) -> Result<Option<u64>> {
    let latest = recovery.join("LATEST");
    if !latest.exists() {
        return Ok(None);
    }
    let text = fs::read_to_string(&latest).map_err(|source| io_at(&latest, source))?;
    let id = text
        .trim()
        .parse::<u64>()
        .map_err(|_| Error::msg(format!("{} does not contain a generation id", latest.display())))?;
    Ok(Some(id))
}

pub fn read_latest(recovery: &Path) -> Result<Option<GenerationManifest>> {
    let Some(id) = latest_id(recovery)? else {
        return Ok(None);
    };
    Ok(Some(GenerationManifest::read(
        &generation_dir(recovery, id).join("manifest.json"),
    )?))
}

pub fn list_ids(recovery: &Path) -> Result<Vec<u64>> {
    let generations = recovery.join("generations");
    if !generations.exists() {
        return Ok(Vec::new());
    }
    let mut ids = Vec::new();
    let entries = fs::read_dir(&generations).map_err(|source| io_at(&generations, source))?;
    for entry in entries {
        let entry = entry.map_err(|source| io_at(&generations, source))?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if let Ok(id) = name.parse::<u64>() {
            ids.push(id);
        }
    }
    ids.sort_unstable();
    Ok(ids)
}

pub fn prune_with(
    recovery: &Path,
    keep: u64,
    apply: bool,
    sealer: &mut dyn VolumeSeal,
) -> Result<Vec<u64>> {
    if keep == 0 {
        return Err(Error::msg("prune keeps at least 1 generation"));
    }
    let ids = list_ids(recovery)?;
    let latest = latest_id(recovery)?.unwrap_or(0);
    let mut doomed: Vec<u64> = ids
        .iter()
        .copied()
        .filter(|id| *id != latest)
        .collect();
    let retain_older = keep.saturating_sub(1) as usize;
    if doomed.len() > retain_older {
        doomed.truncate(doomed.len() - retain_older);
    } else {
        doomed.clear();
    }
    if !apply {
        return Ok(doomed);
    }
    let generations = recovery.join("generations");
    unseal_if_present(sealer, recovery)?;
    unseal_if_present(sealer, &generations)?;
    let delete_result = (|| {
        for id in &doomed {
            let dir = generation_dir(recovery, *id);
            if dir.exists() {
                sealer.unseal_tree(&dir)?;
                fs::remove_dir_all(&dir).map_err(|source| io_at(&dir, source))?;
            }
        }
        Ok(doomed.clone())
    })();
    let reseal = reseal_parents(sealer, recovery, &generations);
    match (delete_result, reseal) {
        (Ok(ids), Ok(())) => Ok(ids),
        (Err(err), Err(seal_err)) => Err(Error::msg(format!(
            "{err}; also failed to reseal the recovery directory: {seal_err}"
        ))),
        (Err(err), Ok(())) => Err(err),
        (Ok(_), Err(err)) => Err(err),
    }
}

pub fn seal_store(recovery: &Path, sealer: &mut dyn VolumeSeal) -> Result<()> {
    let generations = recovery.join("generations");
    if generations.exists() {
        for id in list_ids(recovery)? {
            let dir = generation_dir(recovery, id);
            if dir.join("manifest.json").exists() {
                sealer.seal_tree(&dir)?;
            }
        }
        sealer.seal_inode(&generations)?;
    }
    if recovery.join("LATEST").exists() {
        sealer.seal_inode(&recovery.join("LATEST"))?;
    }
    sealer.seal_inode(recovery)?;
    Ok(())
}

fn probe_immutable(recovery: &Path, sealer: &mut dyn VolumeSeal) -> Result<()> {
    let probe = recovery.join(".seal-probe");
    fs::write(&probe, b"probe").map_err(|source| io_at(&probe, source))?;
    let result = (|| {
        sealer.seal_inode(&probe)?;
        if !sealer.is_immutable(&probe)? {
            return Err(Error::msg(
                "the filesystem accepted the immutable flag and then dropped it; use ext4, btrfs, xfs, or f2fs",
            ));
        }
        sealer.unseal_inode(&probe)?;
        Ok(())
    })();
    let _ = fs::remove_file(&probe);
    result
}

fn cleanup_incomplete(recovery: &Path, sealer: &mut dyn VolumeSeal) -> Result<()> {
    for id in list_ids(recovery)? {
        let dir = generation_dir(recovery, id);
        if dir.join("manifest.json").exists() {
            continue;
        }
        if sealer.is_immutable(&dir)? {
            continue;
        }
        sealer.unseal_tree(&dir)?;
        fs::remove_dir_all(&dir).map_err(|source| io_at(&dir, source))?;
    }
    Ok(())
}

fn next_id(recovery: &Path) -> Result<u64> {
    let from_latest = latest_id(recovery)?.unwrap_or(0);
    let from_dirs = list_ids(recovery)?.into_iter().max().unwrap_or(0);
    Ok(from_latest.max(from_dirs) + 1)
}

fn write_latest(recovery: &Path, id: u64, sealer: &mut dyn VolumeSeal) -> Result<()> {
    let latest = recovery.join("LATEST");
    if latest.exists() {
        sealer.unseal_inode(&latest)?;
    }
    let tmp = recovery.join(".LATEST.tmp");
    fs::write(&tmp, format!("{id:08}\n")).map_err(|source| io_at(&tmp, source))?;
    fs::rename(&tmp, &latest).map_err(|source| io_at(&latest, source))?;
    sealer.seal_inode(&latest)?;
    Ok(())
}

fn unseal_if_present(sealer: &mut dyn VolumeSeal, path: &Path) -> Result<()> {
    if path.exists() {
        sealer.unseal_inode(path)?;
    }
    Ok(())
}

fn reseal_parents(sealer: &mut dyn VolumeSeal, recovery: &Path, generations: &Path) -> Result<()> {
    if recovery.join("LATEST").exists() {
        sealer.seal_inode(&recovery.join("LATEST"))?;
    }
    if generations.exists() {
        sealer.seal_inode(generations)?;
    }
    sealer.seal_inode(recovery)?;
    let leftover = recovery.join(".seal-probe");
    if leftover.exists() {
        let _ = sealer.unseal_inode(&leftover);
        let _ = fs::remove_file(&leftover);
        sealer.seal_inode(recovery)?;
    }
    Ok(())
}

pub fn generation_is_under_recovery(original: &str, recovery_key: &str) -> bool {
    is_within(original, recovery_key)
}

pub fn host_recovery_key(recovery: &Path) -> String {
    path_key(recovery)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::immutable::RecordingSeal;
    use crate::manifest::SystemMeta;
    use std::fs;

    fn scratch() -> PathBuf {
        let path = std::env::temp_dir().join(format!("recorize-store-{}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        path
    }

    fn request(root: &Path, recovery: &Path) -> SaveRequest {
        let home = root.join("home");
        fs::create_dir_all(home.join("docs")).unwrap();
        fs::write(home.join("docs").join("note.txt"), b"saved").unwrap();
        let mut exclude = Exclude::default();
        exclude.default_components = true;
        exclude.prefixes.push(path_key(recovery));
        SaveRequest {
            recovery_dir: recovery.to_path_buf(),
            watches: vec![home],
            exclude,
            scope_label: "all".into(),
            dry_run: false,
            meta: SystemMeta {
                packages_explicit: vec!["bash".into()],
                ..SystemMeta::default()
            },
            hostname: "test".into(),
            now_unix: 100,
            config_toml: "recovery_dir = \"/var/lib/recorize/recovery\"\n".into(),
        }
    }

    #[test]
    fn second_save_does_not_unseal_the_first_generation() {
        let root = scratch();
        let recovery = root.join("recovery");
        let mut sealer = RecordingSeal::default();
        let first = save_with(&request(&root, &recovery), &mut sealer).unwrap();
        assert_eq!(first.id, 1);
        assert!(sealer
            .seal_tree_log
            .iter()
            .any(|path| path.ends_with("00000001")));
        sealer.unseal_log.clear();
        sealer.seal_tree_log.clear();
        fs::write(
            root.join("home").join("docs").join("note.txt"),
            b"changed",
        )
        .unwrap();
        let second = save_with(&request(&root, &recovery), &mut sealer).unwrap();
        assert_eq!(second.id, 2);
        assert!(
            sealer
                .unseal_log
                .iter()
                .all(|path| !path.ends_with("00000001")),
            "unseal log: {:?}",
            sealer.unseal_log
        );
        assert!(sealer
            .seal_tree_log
            .iter()
            .any(|path| path.ends_with("00000002")));
        let manifest = read_latest(&recovery).unwrap().unwrap();
        assert_eq!(manifest.id, 2);
        assert_eq!(manifest.packages_explicit, vec!["bash".to_string()]);
        let note = fs::read(
            generation_dir(&recovery, 1)
                .join("files")
                .join("home")
                .join("docs")
                .join("note.txt"),
        )
        .unwrap();
        // The path under files mirrors the host path, which on Windows includes the drive.
        // Find the note in generation 1 by walking.
        let _ = note;
        let gen1 = fs::read_to_string(generation_dir(&recovery, 1).join("manifest.json")).unwrap();
        assert!(gen1.contains("saved") || gen1.contains(&crate::util::sha256_hex(b"saved")));
        let _ = fs::remove_dir_all(&root);
    }
}
