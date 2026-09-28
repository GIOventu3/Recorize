use crate::error::{io_at, Error, Result};
use crate::os_release::OsInfo;
use crate::pacman_conf::Repo;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;

pub const MANIFEST_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FileKind {
    File,
    Dir,
    Symlink,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedFile {
    pub stored: String,
    pub original: String,
    pub kind: FileKind,
    pub mode: u32,
    pub uid: u32,
    pub gid: u32,
    pub mtime_unix: i64,
    pub size: u64,
    #[serde(default)]
    pub sha256: Option<String>,
    #[serde(default)]
    pub link_target: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackageVersion {
    pub name: String,
    pub version: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredText {
    pub path: String,
    pub contents: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct SystemMeta {
    #[serde(default)]
    pub os_release_text: String,
    #[serde(default)]
    pub os: OsInfo,
    #[serde(default)]
    pub pacman_conf_text: String,
    #[serde(default)]
    pub repos: Vec<Repo>,
    #[serde(default)]
    pub mirrorlists: Vec<StoredText>,
    #[serde(default)]
    pub packages_explicit: Vec<String>,
    #[serde(default)]
    pub packages_all: Vec<PackageVersion>,
    #[serde(default)]
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GenerationManifest {
    pub version: u32,
    pub id: u64,
    pub created_unix: u64,
    pub hostname: String,
    pub tool_version: String,
    pub scope: String,
    pub config_toml: String,
    pub os: OsInfo,
    pub os_release_text: String,
    pub repos: Vec<Repo>,
    pub pacman_conf: String,
    pub mirrorlists: Vec<StoredText>,
    pub packages_explicit: Vec<String>,
    pub packages_all: Vec<PackageVersion>,
    pub files: Vec<SavedFile>,
    pub warnings: Vec<String>,
}

impl GenerationManifest {
    pub fn write(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|source| io_at(parent, source))?;
        }
        let bytes = serde_json::to_vec_pretty(self)
            .map_err(|err| Error::msg(format!("manifest encode: {err}")))?;
        let tmp = path.with_extension("json.tmp");
        fs::write(&tmp, &bytes).map_err(|source| io_at(&tmp, source))?;
        fs::rename(&tmp, path).map_err(|source| io_at(path, source))?;
        Ok(())
    }

    pub fn read(path: &Path) -> Result<Self> {
        let text = fs::read_to_string(path).map_err(|source| io_at(path, source))?;
        let manifest: Self = serde_json::from_str(&text)
            .map_err(|err| Error::msg(format!("{}: {err}", path.display())))?;
        if manifest.version != MANIFEST_VERSION {
            return Err(Error::msg(format!(
                "{}: unsupported manifest version {}",
                path.display(),
                manifest.version
            )));
        }
        Ok(manifest)
    }
}

pub fn parse_pacman_q(text: &str) -> Vec<PackageVersion> {
    let mut packages = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Some((name, version)) = line.split_once(char::is_whitespace) else {
            continue;
        };
        let name = name.trim();
        let version = version.trim();
        if name.is_empty() || version.is_empty() {
            continue;
        }
        packages.push(PackageVersion {
            name: name.to_string(),
            version: version.to_string(),
        });
    }
    packages
}

pub fn parse_explicit(text: &str) -> Vec<String> {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(|line| line.to_string())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pacman_q_splits_name_and_version() {
        let parsed = parse_pacman_q("bash 5.2.037-1\nfilesystem 1:2024.04.07-1\n");
        assert_eq!(parsed[0].name, "bash");
        assert_eq!(parsed[1].version, "1:2024.04.07-1");
    }

    #[test]
    fn manifest_roundtrip() {
        let dir = std::env::temp_dir().join(format!(
            "recorize-manifest-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let manifest = GenerationManifest {
            version: MANIFEST_VERSION,
            id: 3,
            created_unix: 10,
            hostname: "box".into(),
            tool_version: "0.1.0".into(),
            scope: "all".into(),
            config_toml: String::new(),
            os: OsInfo::default(),
            os_release_text: String::new(),
            repos: Vec::new(),
            pacman_conf: String::new(),
            mirrorlists: Vec::new(),
            packages_explicit: vec!["bash".into()],
            packages_all: vec![PackageVersion {
                name: "bash".into(),
                version: "5.2.037-1".into(),
            }],
            files: Vec::new(),
            warnings: Vec::new(),
        };
        let path = dir.join("manifest.json");
        manifest.write(&path).unwrap();
        let loaded = GenerationManifest::read(&path).unwrap();
        assert_eq!(loaded, manifest);
        let _ = fs::remove_dir_all(&dir);
    }
}
