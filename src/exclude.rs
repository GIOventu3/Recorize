use crate::util::{is_within, path_key};
use std::path::Path;

#[derive(Debug, Clone)]
pub struct Exclude {
    pub globs: Vec<String>,
    pub prefixes: Vec<String>,
    pub default_components: bool,
}

impl Default for Exclude {
    fn default() -> Self {
        Self {
            globs: Vec::new(),
            prefixes: Vec::new(),
            default_components: true,
        }
    }
}

impl Exclude {
    pub fn matches_path(&self, path: &Path) -> bool {
        self.matches_key(&path_key(path))
    }

    pub fn matches_key(&self, path: &str) -> bool {
        if self
            .prefixes
            .iter()
            .any(|prefix| is_within(path, prefix) || path == prefix.trim_end_matches('/'))
        {
            return true;
        }
        if self.default_components && has_default_exclude_component(path) {
            return true;
        }
        self.globs.iter().any(|glob| glob_match(glob, path))
    }
}

pub fn has_default_exclude_component(path: &str) -> bool {
    let parts: Vec<&str> = path.split('/').filter(|part| !part.is_empty()).collect();
    if parts
        .iter()
        .any(|part| *part == ".cache" || *part == ".thumbnails")
    {
        return true;
    }
    parts.windows(3).any(|window| window == [".local", "share", "Trash"])
}

pub fn glob_match(pattern: &str, text: &str) -> bool {
    fn rec(pattern: &[u8], text: &[u8]) -> bool {
        if pattern.is_empty() {
            return text.is_empty();
        }
        if pattern[0] == b'*' {
            let double = pattern.len() > 1 && pattern[1] == b'*';
            let mut rest = if double { &pattern[2..] } else { &pattern[1..] };
            if double && rest.first() == Some(&b'/') {
                rest = &rest[1..];
            }
            if rec(rest, text) {
                return true;
            }
            let mut index = 0;
            while index < text.len() {
                if !double && text[index] == b'/' {
                    break;
                }
                index += 1;
                if rec(rest, &text[index..]) {
                    return true;
                }
            }
            false
        } else if text.is_empty() || pattern[0] != text[0] {
            false
        } else {
            rec(&pattern[1..], &text[1..])
        }
    }
    rec(pattern.as_bytes(), text.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn star_does_not_cross_slash() {
        assert!(glob_match("/home/*/.cache", "/home/alice/.cache"));
        assert!(!glob_match("/home/*/.cache", "/home/alice/docs"));
        assert!(!glob_match("/home/*", "/home/alice/docs"));
    }

    #[test]
    fn double_star_crosses_slash() {
        assert!(glob_match(
            "/home/*/.cache/**",
            "/home/alice/.cache/blob/a"
        ));
        assert!(glob_match("/var/lib/recorize/**", "/var/lib/recorize/a/b"));
    }

    #[test]
    fn exact_file() {
        assert!(glob_match("/etc/hostname", "/etc/hostname"));
        assert!(!glob_match("/etc/hostname", "/etc/hostname.bak"));
    }

    #[test]
    fn default_components_and_prefix() {
        let exclude = Exclude {
            globs: vec!["/opt/scratch/**".into()],
            prefixes: vec!["/var/lib/recorize/recovery".into()],
            default_components: true,
        };
        assert!(exclude.matches_key("/home/a/.cache/x"));
        assert!(exclude.matches_key("/home/a/.local/share/Trash/files/z"));
        assert!(exclude.matches_key("/var/lib/recorize/recovery/LATEST"));
        assert!(!exclude.matches_key("/var/lib/recorize/recovery-notes"));
        assert!(exclude.matches_key("/opt/scratch/a"));
        assert!(!exclude.matches_key("/home/a/Documents/note.txt"));
    }
}
