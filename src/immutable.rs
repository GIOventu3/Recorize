use crate::error::{io_at, Error, Result};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Linux inode flag. A sealed file cannot be modified, renamed, or unlinked
/// until a process with CAP_LINUX_IMMUTABLE clears the flag.
pub const FS_IMMUTABLE_FL: i64 = 0x00000010;

pub fn with_immutable_bit(flags: i64) -> i64 {
    flags | FS_IMMUTABLE_FL
}

pub fn without_immutable_bit(flags: i64) -> i64 {
    flags & !FS_IMMUTABLE_FL
}

pub fn has_immutable_bit(flags: i64) -> bool {
    flags & FS_IMMUTABLE_FL != 0
}

pub trait VolumeSeal {
    fn supports_immutable(&self) -> bool;
    fn seal_inode(&mut self, path: &Path) -> Result<()>;
    fn unseal_inode(&mut self, path: &Path) -> Result<()>;
    fn seal_tree(&mut self, root: &Path) -> Result<()>;
    fn unseal_tree(&mut self, root: &Path) -> Result<()>;
    fn is_immutable(&mut self, path: &Path) -> Result<bool>;
}

#[cfg(target_os = "linux")]
mod linux {
    use super::VolumeSeal;
    use crate::error::{io_at, Error, Result};
    use std::fs::OpenOptions;
    use std::os::unix::fs::OpenOptionsExt;
    use std::os::unix::io::AsRawFd;
    use std::path::Path;
    use walkdir::WalkDir;

    const FS_IMMUTABLE_FL: libc::c_long = 0x00000010;

    #[cfg(target_pointer_width = "64")]
    const FS_IOC_GETFLAGS: libc::c_ulong = 0x8008_6601;
    #[cfg(target_pointer_width = "64")]
    const FS_IOC_SETFLAGS: libc::c_ulong = 0x4008_6602;
    #[cfg(target_pointer_width = "32")]
    const FS_IOC_GETFLAGS: libc::c_ulong = 0x8004_6601;
    #[cfg(target_pointer_width = "32")]
    const FS_IOC_SETFLAGS: libc::c_ulong = 0x4004_6602;

    #[derive(Debug, Default)]
    pub struct LinuxSeal;

    impl VolumeSeal for LinuxSeal {
        fn supports_immutable(&self) -> bool {
            true
        }

        fn seal_inode(&mut self, path: &Path) -> Result<()> {
            set_immutable(path, true)
        }

        fn unseal_inode(&mut self, path: &Path) -> Result<()> {
            set_immutable(path, false)
        }

        fn seal_tree(&mut self, root: &Path) -> Result<()> {
            walk_set(root, true)?;
            sync_storage();
            Ok(())
        }

        fn unseal_tree(&mut self, root: &Path) -> Result<()> {
            walk_set(root, false)
        }

        fn is_immutable(&mut self, path: &Path) -> Result<bool> {
            match read_flags(path) {
                Ok(flags) => Ok(super::has_immutable_bit(flags as i64)),
                Err(Error::Io { source, .. }) if source.raw_os_error() == Some(libc::ELOOP) => {
                    Ok(false)
                }
                Err(err) => Err(err),
            }
        }
    }

    fn walk_set(root: &Path, immutable: bool) -> Result<()> {
        if !root.exists() {
            return Err(Error::msg(format!("{} does not exist", root.display())));
        }
        // contents_first seals children before the directory that holds them.
        let walker = WalkDir::new(root).contents_first(true).follow_links(false);
        for entry in walker {
            let entry = entry.map_err(|err| {
                let path = err
                    .path()
                    .map(Path::to_path_buf)
                    .unwrap_or_else(|| root.to_path_buf());
                match err.into_io_error() {
                    Some(source) => io_at(path, source),
                    None => Error::msg(format!("{}: directory walk failed", path.display())),
                }
            })?;
            set_immutable(entry.path(), immutable)?;
        }
        Ok(())
    }

    fn set_immutable(path: &Path, immutable: bool) -> Result<()> {
        let flags = match read_flags(path) {
            Ok(flags) => flags,
            Err(Error::Io { source, .. }) if source.raw_os_error() == Some(libc::ELOOP) => {
                // Symlink inodes are left alone. The sealed parent directory
                // already forbids renaming or unlinking them, and open() must
                // not follow the link onto some other inode.
                return Ok(());
            }
            Err(err) => return Err(err),
        };
        let updated = if immutable {
            flags | FS_IMMUTABLE_FL
        } else {
            flags & !FS_IMMUTABLE_FL
        };
        if updated == flags {
            return Ok(());
        }
        write_flags(path, updated)
    }

    fn read_flags(path: &Path) -> Result<libc::c_long> {
        let file = open_nofollow(path)?;
        let mut flags: libc::c_long = 0;
        let rc = unsafe { libc::ioctl(file.as_raw_fd(), FS_IOC_GETFLAGS, &mut flags) };
        if rc == 0 {
            Ok(flags)
        } else {
            Err(ioctl_error(path))
        }
    }

    fn write_flags(path: &Path, flags: libc::c_long) -> Result<()> {
        let file = open_nofollow(path)?;
        let rc = unsafe { libc::ioctl(file.as_raw_fd(), FS_IOC_SETFLAGS, &flags) };
        if rc == 0 {
            Ok(())
        } else {
            Err(ioctl_error(path))
        }
    }

    fn open_nofollow(path: &Path) -> Result<std::fs::File> {
        OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path)
            .map_err(|source| io_at(path, source))
    }

    fn ioctl_error(path: &Path) -> Error {
        let err = std::io::Error::last_os_error();
        let code = err.raw_os_error().unwrap_or(0);
        let hint = match code {
            libc::EPERM => "need CAP_LINUX_IMMUTABLE (run as root)",
            libc::EOPNOTSUPP | libc::ENOTTY => {
                "this filesystem does not support the immutable attribute; use ext4, btrfs, xfs, or f2fs"
            }
            _ => "ioctl failed",
        };
        Error::msg(format!("{}: {hint} ({err})", path.display()))
    }

    fn sync_storage() {
        unsafe { libc::sync() }
    }
}

#[cfg(target_os = "linux")]
pub use linux::LinuxSeal;

/// Records seal calls so the generation protocol can be tested without Linux ioctls.
#[derive(Debug, Default)]
pub struct RecordingSeal {
    pub immutable: BTreeMap<PathBuf, bool>,
    pub unseal_log: Vec<PathBuf>,
    pub seal_tree_log: Vec<PathBuf>,
}

impl VolumeSeal for RecordingSeal {
    fn supports_immutable(&self) -> bool {
        true
    }

    fn seal_inode(&mut self, path: &Path) -> Result<()> {
        self.immutable.insert(path.to_path_buf(), true);
        Ok(())
    }

    fn unseal_inode(&mut self, path: &Path) -> Result<()> {
        self.unseal_log.push(path.to_path_buf());
        self.immutable.insert(path.to_path_buf(), false);
        Ok(())
    }

    fn seal_tree(&mut self, root: &Path) -> Result<()> {
        self.seal_tree_log.push(root.to_path_buf());
        self.immutable.insert(root.to_path_buf(), true);
        Ok(())
    }

    fn unseal_tree(&mut self, root: &Path) -> Result<()> {
        self.unseal_log.push(root.to_path_buf());
        self.immutable.insert(root.to_path_buf(), false);
        Ok(())
    }

    fn is_immutable(&mut self, path: &Path) -> Result<bool> {
        Ok(*self.immutable.get(path).unwrap_or(&false))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bit_helpers() {
        assert!(!has_immutable_bit(0));
        let flags = with_immutable_bit(0x2);
        assert!(has_immutable_bit(flags));
        assert_eq!(without_immutable_bit(flags), 0x2);
    }
}
