//! Reading and updating `.env`. Updates keep every comment and unrelated
//! line, replace a key where it first appears, drop later duplicates of it,
//! and append keys that are new. The file is written with mode 0600 because
//! it holds the agent key.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::Path;

use anyhow::{Context, Result};

/// The key/value pairs in `path`, parsed the way `serve` reads them. A
/// missing file is empty.
pub fn read(path: &Path) -> Result<BTreeMap<String, String>> {
    if !path.exists() {
        return Ok(BTreeMap::new());
    }
    let mut out = BTreeMap::new();
    for item in dotenvy::from_path_iter(path)
        .with_context(|| format!("failed to read {}", path.display()))?
    {
        let (k, v) = item.with_context(|| format!("failed to parse {}", path.display()))?;
        out.entry(k).or_insert(v);
    }
    Ok(out)
}

/// Set `pairs` in `path`, creating it if needed, and leave it at mode 0600.
pub fn write(path: &Path, pairs: &[(&str, &str)]) -> Result<()> {
    let existing = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e).with_context(|| format!("failed to read {}", path.display())),
    };
    let text = merge(&existing, pairs);
    let dir = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    // Write a private temporary file beside the target, then rename, so the
    // key is never in a file anyone else can read, even briefly.
    let mut tmp = tempfile::Builder::new()
        .prefix(".env.")
        .permissions(private())
        .tempfile_in(dir)
        .with_context(|| format!("failed to create a temporary file in {}", dir.display()))?;
    tmp.write_all(text.as_bytes())?;
    tmp.as_file().sync_all()?;
    tmp.persist(path)
        .with_context(|| format!("failed to write {}", path.display()))?;
    std::fs::set_permissions(path, private())?;
    Ok(())
}

#[cfg(unix)]
fn private() -> std::fs::Permissions {
    use std::os::unix::fs::PermissionsExt;
    std::fs::Permissions::from_mode(0o600)
}

#[cfg(not(unix))]
fn private() -> std::fs::Permissions {
    std::fs::metadata(".")
        .map(|m| m.permissions())
        .expect("current directory is readable")
}

/// `existing` with `pairs` set. Pure, for tests.
pub fn merge(existing: &str, pairs: &[(&str, &str)]) -> String {
    let mut done: Vec<&str> = Vec::new();
    let mut lines: Vec<String> = Vec::new();
    for line in existing.lines() {
        match line_key(line).and_then(|k| pairs.iter().find(|(pk, _)| *pk == k)) {
            Some((k, v)) => {
                if !done.contains(k) {
                    lines.push(format!("{k}={}", quote(v)));
                    done.push(k);
                }
                // A later duplicate of a key we set is dropped.
            }
            None => lines.push(line.to_string()),
        }
    }
    for (k, v) in pairs {
        if !done.contains(k) {
            lines.push(format!("{k}={}", quote(v)));
            done.push(k);
        }
    }
    let mut text = lines.join("\n");
    text.push('\n');
    text
}

/// The key a `KEY=value` or `export KEY=value` line sets; `None` for
/// comments, blank lines and anything else.
fn line_key(line: &str) -> Option<&str> {
    let t = line.trim_start();
    if t.starts_with('#') {
        return None;
    }
    let t = t.strip_prefix("export ").unwrap_or(t);
    let (k, _) = t.split_once('=')?;
    let k = k.trim();
    (!k.is_empty() && k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')).then_some(k)
}

/// Values that dotenv would misread unquoted (spaces, `#`, quotes) are
/// single-quoted. URLs, ids and keys are left as they are.
fn quote(v: &str) -> String {
    let plain = v
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || "._:/@+-=%,~".contains(c));
    if plain {
        v.to_string()
    } else {
        format!("'{}'", v.replace('\'', "'\\''"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_replaces_dedupes_appends_and_keeps_comments() {
        let existing = "# Bridge base URL.\n\
                        SWITCH_URL=http://old:1\n\
                        \n\
                        # Shown once.\n\
                        SWITCH_AGENT_KEY=\n\
                        OTHER=keep\n\
                        SWITCH_URL=http://dup:2\n";
        let out = merge(
            existing,
            &[
                ("SWITCH_URL", "http://localhost:51970"),
                ("SWITCH_AGENT_KEY", "k1"),
                ("SWITCH_ROOM_ID", "room-1"),
            ],
        );
        assert_eq!(
            out,
            "# Bridge base URL.\n\
             SWITCH_URL=http://localhost:51970\n\
             \n\
             # Shown once.\n\
             SWITCH_AGENT_KEY=k1\n\
             OTHER=keep\n\
             SWITCH_ROOM_ID=room-1\n"
        );
        assert_eq!(out.matches("SWITCH_URL=").count(), 1);
    }

    #[test]
    fn merge_handles_export_and_comments_that_look_like_keys() {
        let out = merge(
            "export SWITCH_URL=a\n# SWITCH_URL=commented\n",
            &[("SWITCH_URL", "b")],
        );
        assert_eq!(out, "SWITCH_URL=b\n# SWITCH_URL=commented\n");
        assert_eq!(merge("", &[("A", "1")]), "A=1\n");
    }

    #[test]
    fn odd_values_are_quoted_and_read_back() {
        assert_eq!(quote("p@ss word#1"), "'p@ss word#1'");
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".env");
        write(&path, &[("A", "p@ss word#1"), ("B", "it's")]).unwrap();
        let back = read(&path).unwrap();
        assert_eq!(back["A"], "p@ss word#1");
        assert_eq!(back["B"], "it's");
    }

    #[cfg(unix)]
    #[test]
    fn written_file_is_private_and_idempotent() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".env");
        std::fs::write(&path, "# keep me\nSWITCH_URL=x\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        let pairs = [("SWITCH_URL", "http://h:1"), ("SWITCH_AGENT_ID", "id-1")];
        write(&path, &pairs).unwrap();
        write(&path, &pairs).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "# keep me\nSWITCH_URL=http://h:1\nSWITCH_AGENT_ID=id-1\n"
        );
        // No temporary files left behind.
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }
}
