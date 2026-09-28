use crate::error::{io_at, Error, Result};
use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};

pub fn sha256_hex(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}

pub fn hex(bytes: &[u8]) -> String {
    const LUT: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(LUT[(byte >> 4) as usize] as char);
        out.push(LUT[(byte & 0xf) as usize] as char);
    }
    out
}

pub fn copy_hashed(src: &Path, dest: &Path) -> Result<(u64, String)> {
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).map_err(|source| io_at(parent, source))?;
    }
    let mut input = File::open(src).map_err(|source| io_at(src, source))?;
    let mut output = File::create(dest).map_err(|source| io_at(dest, source))?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    let mut total = 0u64;
    loop {
        let n = input.read(&mut buf).map_err(|source| io_at(src, source))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        output
            .write_all(&buf[..n])
            .map_err(|source| io_at(dest, source))?;
        total += n as u64;
    }
    output.sync_all().map_err(|source| io_at(dest, source))?;
    Ok((total, hex(&hasher.finalize())))
}

pub fn hash_file(path: &Path) -> Result<String> {
    let mut input = File::open(path).map_err(|source| io_at(path, source))?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = input.read(&mut buf).map_err(|source| io_at(path, source))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex(&hasher.finalize()))
}

/// Display form used for exclude matching. Rooted Unix paths keep their leading slash.
pub fn path_key(path: &Path) -> String {
    let mut out = String::new();
    for component in path.components() {
        match component {
            Component::RootDir => out.push('/'),
            Component::Prefix(prefix) => {
                out.push_str(&prefix.as_os_str().to_string_lossy());
            }
            Component::Normal(part) => {
                if !out.is_empty() && !out.ends_with('/') {
                    out.push('/');
                }
                out.push_str(&part.to_string_lossy());
            }
            Component::CurDir | Component::ParentDir => {}
        }
    }
    out
}

pub fn push_rel(root: &Path, rel: &str) -> PathBuf {
    let mut path = root.to_path_buf();
    for part in rel.split('/') {
        if !part.is_empty() {
            path.push(part);
        }
    }
    path
}

pub fn safe_rel(stored: &str) -> Option<PathBuf> {
    if stored.is_empty() || stored.starts_with('/') || stored.contains('\0') {
        return None;
    }
    let path = Path::new(stored);
    if path
        .components()
        .any(|component| !matches!(component, Component::Normal(_)))
    {
        return None;
    }
    Some(path.to_path_buf())
}

pub fn safe_unix_path(path: &str) -> bool {
    if !path.starts_with('/') || path.contains('\0') {
        return false;
    }
    path.split('/')
        .skip(1)
        .all(|part| !part.is_empty() && part != "." && part != "..")
}

pub fn is_within(path: &str, parent: &str) -> bool {
    let path = path.trim_end_matches('/');
    let parent = parent.trim_end_matches('/');
    if parent.is_empty() {
        return false;
    }
    path == parent || path.starts_with(&format!("{parent}/"))
}

pub fn join_sysroot(sysroot: &str, system_path: &str) -> Result<String> {
    if !safe_unix_path(system_path) && system_path != "/" {
        return Err(Error::msg(format!(
            "system path `{system_path}` is not an absolute Unix path"
        )));
    }
    let root = sysroot.trim_end_matches('/');
    if root.is_empty() {
        return Ok(system_path.to_string());
    }
    if system_path == "/" {
        return Ok(if root.is_empty() {
            "/".to_string()
        } else {
            root.to_string()
        });
    }
    Ok(format!("{root}{system_path}"))
}

pub fn valid_package_name(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphanumeric() => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '@' | '.' | '_' | '+' | '-'))
}

pub fn valid_package_version(version: &str) -> bool {
    !version.is_empty()
        && version
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '+' | '-' | ':'))
}

pub fn valid_block_device(path: &str) -> bool {
    let Some(rest) = path.strip_prefix("/dev/") else {
        return false;
    };
    !rest.is_empty()
        && !rest.contains("..")
        && rest
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '-' | '_'))
}

pub fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub fn hostname() -> String {
    #[cfg(unix)]
    {
        let mut buf = [0u8; 256];
        let rc = unsafe { libc::gethostname(buf.as_mut_ptr() as *mut libc::c_char, buf.len()) };
        if rc == 0 {
            let end = buf.iter().position(|b| *b == 0).unwrap_or(buf.len());
            if let Ok(name) = std::str::from_utf8(&buf[..end]) {
                if !name.is_empty() {
                    return name.to_string();
                }
            }
        }
    }
    "unknown".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_of_empty_is_well_known() {
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn safe_rel_rejects_escape() {
        assert!(safe_rel("home/alice/a.txt").is_some());
        assert!(safe_rel("../etc/passwd").is_none());
        assert!(safe_rel("foo/../../x").is_none());
        assert!(safe_rel("/etc/passwd").is_none());
        assert!(safe_rel("").is_none());
    }

    #[test]
    fn within_respects_path_boundary() {
        assert!(is_within(
            "/var/lib/recorize/recovery/generations",
            "/var/lib/recorize/recovery"
        ));
        assert!(!is_within(
            "/var/lib/recorize/recovery-extra",
            "/var/lib/recorize/recovery"
        ));
    }

    #[test]
    fn sysroot_join() {
        assert_eq!(
            join_sysroot("/", "/etc/pacman.conf").unwrap(),
            "/etc/pacman.conf"
        );
        assert_eq!(
            join_sysroot("/mnt", "/etc/pacman.conf").unwrap(),
            "/mnt/etc/pacman.conf"
        );
        assert_eq!(join_sysroot("/mnt/", "/").unwrap(), "/mnt");
        assert!(join_sysroot("/mnt", "etc/pacman.conf").is_err());
        assert!(join_sysroot("/mnt", "/etc/../passwd").is_err());
    }

    #[test]
    fn package_names() {
        assert!(valid_package_name("linux-cachyos"));
        assert!(valid_package_name("lib32-glibc"));
        assert!(!valid_package_name("-bad"));
        assert!(!valid_package_name("has space"));
        assert!(valid_package_version("1:2024.04.07-1"));
        assert!(!valid_package_version("1.0;reboot"));
    }

    #[test]
    fn block_devices() {
        assert!(valid_block_device("/dev/sdb1"));
        assert!(valid_block_device("/dev/nvme0n1p1"));
        assert!(valid_block_device("/dev/disk/by-uuid/ABC"));
        assert!(!valid_block_device("/dev/../etc/passwd"));
        assert!(!valid_block_device("sdb1"));
    }
}
