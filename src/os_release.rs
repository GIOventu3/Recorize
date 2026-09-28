use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OsKind {
    Arch,
    Cachyos,
    ArchLike,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OsInfo {
    pub id: String,
    pub id_like: Vec<String>,
    pub name: String,
    pub kind: OsKind,
}

impl Default for OsInfo {
    fn default() -> Self {
        Self {
            id: String::new(),
            id_like: Vec::new(),
            name: String::new(),
            kind: OsKind::Other,
        }
    }
}

impl OsInfo {
    pub fn is_cachyos(&self) -> bool {
        self.kind == OsKind::Cachyos
    }

    pub fn is_arch_family(&self) -> bool {
        matches!(
            self.kind,
            OsKind::Arch | OsKind::Cachyos | OsKind::ArchLike
        )
    }
}

pub fn parse_os_release(text: &str) -> OsInfo {
    let mut id = String::new();
    let mut id_like = Vec::new();
    let mut name = String::new();
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let value = unquote(value.trim());
        match key.trim() {
            "ID" => id = value,
            "NAME" => name = value,
            "ID_LIKE" => {
                id_like = value.split_whitespace().map(|s| s.to_string()).collect();
            }
            _ => {}
        }
    }
    let kind = classify(&id, &name, &id_like);
    OsInfo {
        id,
        id_like,
        name,
        kind,
    }
}

fn classify(id: &str, name: &str, id_like: &[String]) -> OsKind {
    if id.eq_ignore_ascii_case("cachyos") || name.to_ascii_lowercase().contains("cachyos") {
        return OsKind::Cachyos;
    }
    if id.eq_ignore_ascii_case("arch") {
        return OsKind::Arch;
    }
    if id_like.iter().any(|item| item.eq_ignore_ascii_case("arch")) {
        return OsKind::ArchLike;
    }
    OsKind::Other
}

fn unquote(value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.len() >= 2 {
        let bytes = trimmed.as_bytes();
        if (bytes[0] == b'"' && bytes[trimmed.len() - 1] == b'"')
            || (bytes[0] == b'\'' && bytes[trimmed.len() - 1] == b'\'')
        {
            return trimmed[1..trimmed.len() - 1].to_string();
        }
    }
    trimmed.to_string()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Microarch {
    Generic,
    V3,
    V4,
    Znver4,
}

impl Microarch {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Generic => "x86-64",
            Self::V3 => "x86-64-v3",
            Self::V4 => "x86-64-v4",
            Self::Znver4 => "znver4",
        }
    }
}

/// Highest CachyOS repository level named in an existing pacman.conf.
pub fn level_from_repo_names<'a>(names: impl IntoIterator<Item = &'a str>) -> Microarch {
    let mut level = Microarch::Generic;
    for name in names {
        let name = name.to_ascii_lowercase();
        if name.contains("znver4") {
            return Microarch::Znver4;
        }
        if name.contains("v4") {
            level = Microarch::V4;
        } else if name.contains("v3") && !matches!(level, Microarch::V4) {
            level = Microarch::V3;
        }
    }
    level
}

pub fn parse_ld_help(text: &str) -> Microarch {
    let v4 = text.lines().any(|line| {
        let line = line.to_ascii_lowercase();
        line.contains("x86-64-v4") && line.contains("supported")
    });
    if v4 {
        return Microarch::V4;
    }
    let v3 = text.lines().any(|line| {
        let line = line.to_ascii_lowercase();
        line.contains("x86-64-v3") && line.contains("supported")
    });
    if v3 {
        Microarch::V3
    } else {
        Microarch::Generic
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_cachyos_and_arch() {
        let cachy = parse_os_release("NAME=\"CachyOS Linux\"\nID=cachyos\nID_LIKE=arch\n");
        assert_eq!(cachy.kind, OsKind::Cachyos);
        assert!(cachy.is_arch_family());
        assert_eq!(cachy.id_like, vec!["arch".to_string()]);

        let arch = parse_os_release("NAME=\"Arch Linux\"\nID=arch\n");
        assert_eq!(arch.kind, OsKind::Arch);

        let like = parse_os_release("ID=endeavouros\nID_LIKE=\"arch\"\n");
        assert_eq!(like.kind, OsKind::ArchLike);

        let other = parse_os_release("ID=debian\n");
        assert_eq!(other.kind, OsKind::Other);
        assert!(!other.is_arch_family());
    }

    #[test]
    fn repo_level_prefers_znver_then_v4_then_v3() {
        assert_eq!(
            level_from_repo_names(["cachyos", "cachyos-core-v3", "core"]),
            Microarch::V3
        );
        assert_eq!(
            level_from_repo_names(["cachyos-v4", "cachyos-extra-v4"]),
            Microarch::V4
        );
        assert_eq!(
            level_from_repo_names(["cachyos-znver4", "cachyos-core-v4"]),
            Microarch::Znver4
        );
    }

    #[test]
    fn ld_help_levels() {
        let help = "x86-64-v2 (supported, searched)\nx86-64-v3 (supported, searched)\n";
        assert_eq!(parse_ld_help(help), Microarch::V3);
        let v4 = "x86-64-v4 (supported, searched)\n";
        assert_eq!(parse_ld_help(v4), Microarch::V4);
    }
}
