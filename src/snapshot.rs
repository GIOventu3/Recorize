use crate::error::{io_at, Error, Result};
use crate::exclude::Exclude;
use crate::manifest::{FileKind, SavedFile};
use crate::util::{copy_hashed, push_rel, safe_unix_path};
use filetime::FileTime;
use std::fs::{self, Metadata};
use std::path::{Component, Path, PathBuf};
use std::time::UNIX_EPOCH;

#[derive(Debug, Clone)]
pub struct WatchRoot {
    pub host: PathBuf,
    /// Absolute system path this directory occupies, such as `/home`.
    pub system_path: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedFile {
    pub source: PathBuf,
    pub kind: FileKind,
    pub size: u64,
}

pub fn plan_tree(roots: &[WatchRoot], exclude: &Exclude) -> Result<Vec<PlannedFile>> {
    let mut planned = Vec::new();
    for root in roots {
        if exclude.matches_key(&root.system_path) {
            continue;
        }
        visit(root, exclude, &mut |path, _system, meta| {
            planned.push(PlannedFile {
                source: path.to_path_buf(),
                kind: kind_of(meta),
                size: if meta.is_file() { meta.len() } else { 0 },
            });
            Ok(())
        })?;
    }
    Ok(planned)
}

pub fn capture_tree(
    roots: &[WatchRoot],
    files_root: &Path,
    exclude: &Exclude,
) -> Result<(Vec<SavedFile>, Vec<String>)> {
    let mut saved = Vec::new();
    let mut warnings = Vec::new();
    for root in roots {
        if exclude.matches_key(&root.system_path) {
            continue;
        }
        if !root.host.exists() {
            warnings.push(format!("{} is missing", root.system_path));
            continue;
        }
        let meta = fs::symlink_metadata(&root.host).map_err(|source| io_at(&root.host, source))?;
        if meta.file_type().is_symlink() {
            warnings.push(format!(
                "{} is a symlink; recorize does not follow a watch root",
                root.system_path
            ));
            continue;
        }
        let result = visit(root, exclude, &mut |path, system, meta| {
            match capture_one(path, system, meta, files_root) {
                Ok(Some(entry)) => saved.push(entry),
                Ok(None) if is_special(meta) => {
                    warnings.push(format!("skipped special file {system}"));
                }
                Ok(None) => {}
                Err(err) => warnings.push(format!("{err}")),
            }
            Ok(())
        });
        if let Err(err) = result {
            warnings.push(err.to_string());
        }
    }
    saved.sort_by(|a, b| a.stored.cmp(&b.stored));
    Ok((saved, warnings))
}

fn capture_one(
    path: &Path,
    system: &str,
    meta: &Metadata,
    files_root: &Path,
) -> Result<Option<SavedFile>> {
    if is_special(meta) {
        return Ok(None);
    }
    let Some(stored) = stored_from_system(system)? else {
        return Ok(None);
    };
    let dest = push_rel(files_root, &stored);
    let file_type = meta.file_type();
    let mode = mode_of(meta);
    let (uid, gid) = owner_of(meta);
    let mtime_unix = mtime_of(meta);

    if file_type.is_symlink() {
        let target = fs::read_link(path).map_err(|source| io_at(path, source))?;
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent).map_err(|source| io_at(parent, source))?;
        }
        create_symlink(&target, &dest)?;
        return Ok(Some(SavedFile {
            stored,
            original: system.to_string(),
            kind: FileKind::Symlink,
            mode,
            uid,
            gid,
            mtime_unix,
            size: 0,
            sha256: None,
            link_target: Some(target.to_string_lossy().into_owned()),
        }));
    }

    if file_type.is_dir() {
        fs::create_dir_all(&dest).map_err(|source| io_at(&dest, source))?;
        apply_meta(&dest, mode, uid, gid, mtime_unix)?;
        return Ok(Some(SavedFile {
            stored,
            original: system.to_string(),
            kind: FileKind::Dir,
            mode,
            uid,
            gid,
            mtime_unix,
            size: 0,
            sha256: None,
            link_target: None,
        }));
    }

    if file_type.is_file() {
        let (size, sha256) = copy_hashed(path, &dest)?;
        apply_meta(&dest, mode, uid, gid, mtime_unix)?;
        return Ok(Some(SavedFile {
            stored,
            original: system.to_string(),
            kind: FileKind::File,
            mode,
            uid,
            gid,
            mtime_unix,
            size,
            sha256: Some(sha256),
            link_target: None,
        }));
    }

    Ok(None)
}

fn visit(
    root: &WatchRoot,
    exclude: &Exclude,
    visit_fn: &mut dyn FnMut(&Path, &str, &Metadata) -> Result<()>,
) -> Result<()> {
    let mut stack = vec![root.host.clone()];
    while let Some(path) = stack.pop() {
        let system = system_path_for(root, &path)?;
        if path != root.host && exclude.matches_key(&system) {
            continue;
        }
        let meta = match fs::symlink_metadata(&path) {
            Ok(meta) => meta,
            Err(source) => return Err(io_at(&path, source)),
        };
        visit_fn(&path, &system, &meta)?;
        if meta.is_dir() && !meta.file_type().is_symlink() {
            let entries = fs::read_dir(&path).map_err(|source| io_at(&path, source))?;
            let mut children = Vec::new();
            for entry in entries {
                let entry = entry.map_err(|source| io_at(&path, source))?;
                children.push(entry.path());
            }
            children.sort();
            for child in children.into_iter().rev() {
                stack.push(child);
            }
        }
    }
    Ok(())
}

pub fn system_path_for(root: &WatchRoot, host: &Path) -> Result<String> {
    let rel = host.strip_prefix(&root.host).map_err(|_| {
        Error::msg(format!(
            "{} is outside {}",
            host.display(),
            root.host.display()
        ))
    })?;
    let mut out = root.system_path.trim_end_matches('/').to_string();
    if out.is_empty() {
        out = "/".to_string();
    }
    for component in rel.components() {
        match component {
            Component::Normal(part) => {
                if out != "/" {
                    out.push('/');
                }
                out.push_str(&part.to_string_lossy());
            }
            Component::CurDir => {}
            _ => {
                return Err(Error::msg(format!(
                    "{} cannot be mapped under {}",
                    host.display(),
                    root.system_path
                )))
            }
        }
    }
    Ok(out)
}

fn stored_from_system(system: &str) -> Result<Option<String>> {
    if system == "/" {
        return Ok(None);
    }
    if !safe_unix_path(system) {
        return Err(Error::msg(format!(
            "{system} cannot be stored in a recovery generation"
        )));
    }
    Ok(Some(system.trim_start_matches('/').to_string()))
}

fn kind_of(meta: &Metadata) -> FileKind {
    if meta.file_type().is_symlink() {
        FileKind::Symlink
    } else if meta.is_dir() {
        FileKind::Dir
    } else {
        FileKind::File
    }
}

fn is_special(meta: &Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::FileTypeExt;
        let kind = meta.file_type();
        kind.is_fifo() || kind.is_socket() || kind.is_block_device() || kind.is_char_device()
    }
    #[cfg(not(unix))]
    {
        let _ = meta;
        false
    }
}

fn mode_of(meta: &Metadata) -> u32 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.permissions().mode()
    }
    #[cfg(not(unix))]
    {
        let _ = meta;
        0o644
    }
}

fn owner_of(meta: &Metadata) -> (u32, u32) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        (meta.uid(), meta.gid())
    }
    #[cfg(not(unix))]
    {
        let _ = meta;
        (0, 0)
    }
}

fn mtime_of(meta: &Metadata) -> i64 {
    meta.modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0)
}

pub fn apply_meta(path: &Path, mode: u32, uid: u32, gid: u32, mtime_unix: i64) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(mode))
            .map_err(|source| io_at(path, source))?;
        lchown(path, uid, gid)?;
    }
    #[cfg(not(unix))]
    {
        let _ = (mode, uid, gid);
    }
    if mtime_unix >= 0 {
        let time = FileTime::from_unix_time(mtime_unix, 0);
        filetime::set_file_mtime(path, time).map_err(|source| io_at(path, source))?;
    }
    Ok(())
}

#[cfg(unix)]
fn lchown(path: &Path, uid: u32, gid: u32) -> Result<()> {
    use std::os::unix::ffi::OsStrExt;
    let mut bytes = path.as_os_str().as_bytes().to_vec();
    bytes.push(0);
    let rc = unsafe { libc::lchown(bytes.as_ptr() as *const libc::c_char, uid, gid) };
    if rc == 0 {
        Ok(())
    } else {
        Err(io_at(path, std::io::Error::last_os_error()))
    }
}

fn create_symlink(target: &Path, dest: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(target, dest).map_err(|source| io_at(dest, source))
    }
    #[cfg(not(unix))]
    {
        let _ = (target, dest);
        Err(Error::msg(
            "symlink snapshots are written when recorize runs on Linux",
        ))
    }
}

pub fn copy_tree(src: &Path, dest: &Path) -> Result<u64> {
    let mut count = 0u64;
    let mut stack = vec![(src.to_path_buf(), dest.to_path_buf())];
    while let Some((from, to)) = stack.pop() {
        let meta = fs::symlink_metadata(&from).map_err(|source| io_at(&from, source))?;
        if meta.file_type().is_symlink() {
            let target = fs::read_link(&from).map_err(|source| io_at(&from, source))?;
            if let Some(parent) = to.parent() {
                fs::create_dir_all(parent).map_err(|source| io_at(parent, source))?;
            }
            create_symlink(&target, &to)?;
            count += 1;
            continue;
        }
        if meta.is_dir() {
            fs::create_dir_all(&to).map_err(|source| io_at(&to, source))?;
            count += 1;
            let entries = fs::read_dir(&from).map_err(|source| io_at(&from, source))?;
            for entry in entries {
                let entry = entry.map_err(|source| io_at(&from, source))?;
                stack.push((entry.path(), to.join(entry.file_name())));
            }
            continue;
        }
        if meta.is_file() {
            let _ = copy_hashed(&from, &to)?;
            count += 1;
        }
    }
    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::sha256_hex;
    use std::fs;

    fn scratch(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!("recorize-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn capture_copies_files_and_skips_excluded_tree() {
        let root = scratch("capture");
        let home = root.join("home");
        fs::create_dir_all(home.join("alice").join(".cache")).unwrap();
        fs::create_dir_all(home.join("alice").join("docs")).unwrap();
        fs::write(home.join("alice").join("docs").join("note.txt"), b"hello").unwrap();
        fs::write(home.join("alice").join(".cache").join("junk"), b"nope").unwrap();
        let recovery = root.join("recovery");
        fs::create_dir_all(&recovery).unwrap();

        let exclude = Exclude {
            globs: Vec::new(),
            prefixes: vec!["/var/lib/recorize/recovery".into()],
            default_components: true,
        };
        let files_root = root.join("out");
        let roots = vec![
            WatchRoot {
                host: home,
                system_path: "/home".into(),
            },
            WatchRoot {
                host: recovery,
                system_path: "/var/lib/recorize/recovery".into(),
            },
        ];
        let (saved, warnings) = capture_tree(&roots, &files_root, &exclude).unwrap();
        assert!(warnings.is_empty());
        let note = saved
            .iter()
            .find(|entry| entry.stored == "home/alice/docs/note.txt")
            .unwrap();
        assert_eq!(note.original, "/home/alice/docs/note.txt");
        assert_eq!(note.sha256.as_deref(), Some(sha256_hex(b"hello").as_str()));
        assert!(saved.iter().all(|entry| !entry.stored.contains(".cache")));
        assert!(saved
            .iter()
            .all(|entry| !entry.original.contains("recovery")));
        let _ = fs::remove_dir_all(&root);
    }
}
