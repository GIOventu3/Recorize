use crate::error::{io_at, Error, Result};
use crate::util::safe_unix_path;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;

pub const DEFAULT_RECOVERY: &str = "/var/lib/recorize/recovery";
pub const CONFIG_SYSTEM_PATH: &str = "/etc/recorize/config.toml";
pub const CONFIRM_REINSTALL: &str = "REINSTALL";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WatchPath {
    pub path: String,
    /// `manual`, `transaction`, or `always`.
    #[serde(default = "default_when")]
    pub on: String,
}

fn default_when() -> String {
    "manual".to_string()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Config {
    #[serde(default = "default_recovery")]
    pub recovery_dir: String,
    #[serde(default)]
    pub watch: Vec<WatchPath>,
    #[serde(default)]
    pub exclude: Vec<String>,
    #[serde(default = "default_true")]
    pub default_excludes: bool,
    #[serde(default = "default_pacman_conf")]
    pub pacman_conf: String,
    #[serde(default = "default_os_release")]
    pub os_release: String,
}

fn default_recovery() -> String {
    DEFAULT_RECOVERY.to_string()
}

fn default_true() -> bool {
    true
}

fn default_pacman_conf() -> String {
    "/etc/pacman.conf".to_string()
}

fn default_os_release() -> String {
    "/etc/os-release".to_string()
}

impl Default for Config {
    fn default() -> Self {
        Self {
            recovery_dir: default_recovery(),
            watch: Vec::new(),
            exclude: Vec::new(),
            default_excludes: true,
            pacman_conf: default_pacman_conf(),
            os_release: default_os_release(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SaveScope {
    All,
    Transaction,
}

impl SaveScope {
    pub fn parse(value: &str) -> Result<Self> {
        match value {
            "all" => Ok(Self::All),
            "transaction" => Ok(Self::Transaction),
            _ => Err(Error::msg(
                "scope must be `all` or `transaction`",
            )),
        }
    }
}

impl WatchPath {
    pub fn included_in(&self, scope: SaveScope) -> bool {
        match scope {
            SaveScope::All => true,
            SaveScope::Transaction => matches!(self.on.as_str(), "transaction" | "always"),
        }
    }
}

impl Config {
    pub fn validate(&self) -> Result<()> {
        if !safe_unix_path(&self.recovery_dir) {
            return Err(Error::msg(
                "recovery_dir must be an absolute Unix path without `..`",
            ));
        }
        if !safe_unix_path(&self.pacman_conf) || !safe_unix_path(&self.os_release) {
            return Err(Error::msg(
                "pacman_conf and os_release must be absolute Unix paths",
            ));
        }
        for watch in &self.watch {
            if !safe_unix_path(&watch.path) {
                return Err(Error::msg(format!(
                    "watch path `{}` must be an absolute Unix path",
                    watch.path
                )));
            }
            if !matches!(watch.on.as_str(), "manual" | "transaction" | "always") {
                return Err(Error::msg(format!(
                    "watch path `{}` has unknown schedule `{}`",
                    watch.path, watch.on
                )));
            }
        }
        Ok(())
    }

    pub fn load(path: &Path) -> Result<Self> {
        let text = fs::read_to_string(path).map_err(|source| io_at(path, source))?;
        let config: Config = toml::from_str(&text)
            .map_err(|err| Error::msg(format!("{}: {err}", path.display())))?;
        config.validate()?;
        Ok(config)
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        self.validate()?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|source| io_at(parent, source))?;
        }
        let text = toml::to_string_pretty(self)
            .map_err(|err| Error::msg(format!("config encode: {err}")))?;
        fs::write(path, text).map_err(|source| io_at(path, source))?;
        Ok(())
    }

    pub fn watches_for(&self, scope: SaveScope) -> Vec<&WatchPath> {
        self.watch
            .iter()
            .filter(|watch| watch.included_in(scope))
            .collect()
    }
}

pub fn default_config_toml() -> String {
    let config = Config {
        watch: vec![
            WatchPath {
                path: "/etc".to_string(),
                on: "transaction".to_string(),
            },
            WatchPath {
                path: "/home".to_string(),
                on: "manual".to_string(),
            },
        ],
        ..Config::default()
    };
    toml::to_string_pretty(&config).expect("default config serializes")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transaction_scope_skips_manual_watches() {
        let config = Config {
            watch: vec![
                WatchPath {
                    path: "/etc".into(),
                    on: "transaction".into(),
                },
                WatchPath {
                    path: "/home".into(),
                    on: "manual".into(),
                },
                WatchPath {
                    path: "/root".into(),
                    on: "always".into(),
                },
            ],
            ..Config::default()
        };
        let names: Vec<_> = config
            .watches_for(SaveScope::Transaction)
            .into_iter()
            .map(|watch| watch.path.as_str())
            .collect();
        assert_eq!(names, vec!["/etc", "/root"]);
        assert_eq!(config.watches_for(SaveScope::All).len(), 3);
    }

    #[test]
    fn default_toml_roundtrip() {
        let text = default_config_toml();
        let config: Config = toml::from_str(&text).unwrap();
        config.validate().unwrap();
        assert_eq!(config.recovery_dir, DEFAULT_RECOVERY);
        assert!(config.watch.iter().any(|watch| watch.path == "/etc"));
    }
}
