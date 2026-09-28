use crate::os_release::{Microarch, OsKind};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Repo {
    pub name: String,
    pub servers: Vec<String>,
    pub includes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PacmanConf {
    pub repos: Vec<Repo>,
    pub architecture: Option<String>,
}

pub fn parse_pacman_conf(text: &str) -> PacmanConf {
    let mut repos = Vec::new();
    let mut current: Option<Repo> = None;
    let mut architecture = None;
    let mut in_options = false;

    for raw in text.lines() {
        let line = strip_inline_comment(raw).trim();
        if line.is_empty() {
            continue;
        }
        if let Some(name) = section_name(line) {
            if let Some(repo) = current.take() {
                repos.push(repo);
            }
            if name.eq_ignore_ascii_case("options") {
                in_options = true;
            } else {
                in_options = false;
                current = Some(Repo {
                    name,
                    servers: Vec::new(),
                    includes: Vec::new(),
                });
            }
            continue;
        }
        let Some((key, value)) = split_key(line) else {
            continue;
        };
        if in_options && key.eq_ignore_ascii_case("architecture") {
            architecture = Some(value);
            continue;
        }
        if let Some(repo) = current.as_mut() {
            if key.eq_ignore_ascii_case("server") {
                repo.servers.push(value);
            } else if key.eq_ignore_ascii_case("include") {
                repo.includes.push(value);
            }
        }
    }
    if let Some(repo) = current.take() {
        repos.push(repo);
    }
    PacmanConf {
        repos,
        architecture,
    }
}

fn strip_inline_comment(line: &str) -> &str {
    let trimmed = line.trim();
    if trimmed.starts_with('#') {
        return "";
    }
    line
}

fn section_name(line: &str) -> Option<String> {
    let line = line.trim();
    if line.starts_with('[') && line.ends_with(']') && line.len() > 2 {
        Some(line[1..line.len() - 1].trim().to_string())
    } else {
        None
    }
}

fn split_key(line: &str) -> Option<(String, String)> {
    let (key, value) = line.split_once('=')?;
    let key = key.trim();
    if key.is_empty() {
        return None;
    }
    Some((key.to_string(), value.trim().to_string()))
}

pub fn fallback_pacman_conf(kind: OsKind, level: Microarch) -> String {
    let mut out = String::from(
        "# Written by recorize when the sealed pacman.conf could not be restored.\n\
         [options]\n\
         HoldPkg = pacman glibc\n\
         Architecture = auto\n\
         CheckSpace\n\
         SigLevel = Required DatabaseOptional\n\
         LocalFileSigLevel = Optional\n\n",
    );
    if kind == OsKind::Cachyos {
        match level {
            Microarch::Znver4 => {
                push_repo(
                    &mut out,
                    "cachyos-znver4",
                    "/etc/pacman.d/cachyos-v4-mirrorlist",
                );
                push_repo(
                    &mut out,
                    "cachyos-core-znver4",
                    "/etc/pacman.d/cachyos-v4-mirrorlist",
                );
                push_repo(
                    &mut out,
                    "cachyos-extra-znver4",
                    "/etc/pacman.d/cachyos-v4-mirrorlist",
                );
            }
            Microarch::V4 => {
                push_repo(&mut out, "cachyos-v4", "/etc/pacman.d/cachyos-v4-mirrorlist");
                push_repo(
                    &mut out,
                    "cachyos-core-v4",
                    "/etc/pacman.d/cachyos-v4-mirrorlist",
                );
                push_repo(
                    &mut out,
                    "cachyos-extra-v4",
                    "/etc/pacman.d/cachyos-v4-mirrorlist",
                );
            }
            Microarch::V3 => {
                push_repo(&mut out, "cachyos-v3", "/etc/pacman.d/cachyos-v3-mirrorlist");
                push_repo(
                    &mut out,
                    "cachyos-core-v3",
                    "/etc/pacman.d/cachyos-v3-mirrorlist",
                );
                push_repo(
                    &mut out,
                    "cachyos-extra-v3",
                    "/etc/pacman.d/cachyos-v3-mirrorlist",
                );
            }
            Microarch::Generic => {}
        }
        push_repo(&mut out, "cachyos", "/etc/pacman.d/cachyos-mirrorlist");
    }
    push_repo(&mut out, "core", "/etc/pacman.d/mirrorlist");
    push_repo(&mut out, "extra", "/etc/pacman.d/mirrorlist");
    push_repo(&mut out, "multilib", "/etc/pacman.d/mirrorlist");
    out
}

fn push_repo(out: &mut String, name: &str, include: &str) {
    out.push('[');
    out.push_str(name);
    out.push_str("]\nInclude = ");
    out.push_str(include);
    out.push_str("\n\n");
}

#[cfg(test)]
mod tests {
    use super::*;

    const CACHY: &str = r#"
[options]
Architecture = auto

#[testing]
#Include = /etc/pacman.d/mirrorlist

[cachyos-v3]
Include = /etc/pacman.d/cachyos-v3-mirrorlist

[cachyos-core-v3]
Include = /etc/pacman.d/cachyos-v3-mirrorlist

[cachyos]
Include = /etc/pacman.d/cachyos-mirrorlist

[core]
Include = /etc/pacman.d/mirrorlist

[extra]
Include = /etc/pacman.d/mirrorlist

[multilib]
Include = /etc/pacman.d/mirrorlist
"#;

    #[test]
    fn parses_enabled_cachyos_and_arch_repos() {
        let conf = parse_pacman_conf(CACHY);
        let names: Vec<_> = conf.repos.iter().map(|repo| repo.name.as_str()).collect();
        assert_eq!(
            names,
            vec![
                "cachyos-v3",
                "cachyos-core-v3",
                "cachyos",
                "core",
                "extra",
                "multilib"
            ]
        );
        assert!(!names.contains(&"testing"));
        assert_eq!(conf.architecture.as_deref(), Some("auto"));
        assert_eq!(
            conf.repos[0].includes,
            vec!["/etc/pacman.d/cachyos-v3-mirrorlist".to_string()]
        );
    }

    #[test]
    fn fallback_keeps_cachyos_repos_above_arch() {
        let text = fallback_pacman_conf(OsKind::Cachyos, Microarch::V3);
        let conf = parse_pacman_conf(&text);
        let names: Vec<_> = conf.repos.iter().map(|repo| repo.name.as_str()).collect();
        assert_eq!(
            names,
            vec![
                "cachyos-v3",
                "cachyos-core-v3",
                "cachyos-extra-v3",
                "cachyos",
                "core",
                "extra",
                "multilib"
            ]
        );
        let arch = fallback_pacman_conf(OsKind::Arch, Microarch::Generic);
        let arch_names: Vec<_> = parse_pacman_conf(&arch)
            .repos
            .iter()
            .map(|repo| repo.name.as_str())
            .collect();
        assert_eq!(arch_names, vec!["core", "extra", "multilib"]);
    }
}
