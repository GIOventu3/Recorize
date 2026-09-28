#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileIssue {
    pub package: String,
    pub path: String,
    pub reason: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IssueSeverity {
    Repair,
    Ignore,
}

pub fn severity(reason: &str) -> IssueSeverity {
    let reason = reason.to_ascii_lowercase();
    if reason.contains("modification time") {
        return IssueSeverity::Ignore;
    }
    if reason.contains("no such file")
        || reason.contains("size mismatch")
        || reason.contains("permission")
        || reason.contains("uid mismatch")
        || reason.contains("gid mismatch")
        || reason.contains("checksum")
        || reason.contains("md5")
        || reason.contains("sha256")
        || reason.contains("mismatch")
    {
        return IssueSeverity::Repair;
    }
    IssueSeverity::Ignore
}

pub fn parse_qkk(text: &str) -> Vec<FileIssue> {
    let mut issues = Vec::new();
    for raw in text.lines() {
        let Some(issue) = parse_line(raw) else {
            continue;
        };
        issues.push(issue);
    }
    issues
}

pub fn packages_to_repair(text: &str) -> Vec<String> {
    let mut names = Vec::new();
    for issue in parse_qkk(text) {
        if severity(&issue.reason) != IssueSeverity::Repair {
            continue;
        }
        if !names.iter().any(|existing: &String| existing == &issue.package) {
            names.push(issue.package);
        }
    }
    names
}

fn parse_line(raw: &str) -> Option<FileIssue> {
    let line = raw.trim();
    if line.is_empty() {
        return None;
    }
    let line = line
        .strip_prefix("warning:")
        .or_else(|| line.strip_prefix("error:"))
        .map(str::trim)
        .unwrap_or(line);
    let (reason, head) = split_reason(line)?;
    let (package, path) = head.split_once(':')?;
    let package = package.trim();
    let path = path.trim();
    if package.is_empty() || path.is_empty() || !path.starts_with('/') {
        return None;
    }
    Some(FileIssue {
        package: package.to_string(),
        path: path.to_string(),
        reason: reason.to_string(),
    })
}

fn split_reason(line: &str) -> Option<(&str, &str)> {
    let open = line.rfind(" (")?;
    let close = line.rfind(')')?;
    if close < open {
        return None;
    }
    let reason = line[open + 2..close].trim();
    let head = line[..open].trim();
    if reason.is_empty() || head.is_empty() {
        None
    } else {
        Some((reason, head))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repair_set_skips_mtime_only() {
        let text = "\
warning: filesystem: /etc/passwd (Modification time mismatch)
bash: /usr/bin/bash (No such file or directory)
warning: glibc: /usr/lib/libc.so.6 (Size mismatch)
filesystem: /usr/bin/mount (Permissions mismatch)
pacman: /etc/pacman.conf (Modification time mismatch)
warning: openssl: /usr/bin/openssl (Checksum mismatch)
";
        assert_eq!(
            packages_to_repair(text),
            vec![
                "bash".to_string(),
                "glibc".to_string(),
                "filesystem".to_string(),
                "openssl".to_string()
            ]
        );
    }
}
