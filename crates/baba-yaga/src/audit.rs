//! Append-only record of every decision in the loop: who approved, rejected
//! or snoozed which group, and when.
//!
//! The local `audit.log` beside the state file is the record. After each
//! append its newest 900 KB are copied to a room document named
//! `baba-yaga-audit`, so people in the room can read it. The document is a
//! mirror: it is written whole from the local file, never read back, because
//! reading a room document needs `load_internal_documents`, whose result
//! shape has not been observed yet.

use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, SecondsFormat, Utc};
use switch_client::{Client, NewDocument};

use crate::state::State;

pub const DOC_NAME: &str = "baba-yaga-audit";
/// Under the 1 MiB document cap, with room for the platform's own overhead.
pub const DOC_CAP_BYTES: usize = 900 * 1024;
const META_DOC_ID: &str = "audit_doc_id";

pub struct Audit {
    path: PathBuf,
    /// Set after a create fails because the name is taken by a document this
    /// state does not know: mirroring is then off until restart.
    mirror_off: bool,
}

impl Audit {
    /// `audit.log` in the state file's directory.
    pub fn beside(state_path: &Path) -> Self {
        let path = state_path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .map_or_else(|| PathBuf::from("audit.log"), |p| p.join("audit.log"));
        Self {
            path,
            mirror_off: false,
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Append one line locally, then mirror to the room document. A mirror
    /// failure is logged, not returned: the local line is the record.
    pub async fn append(&mut self, client: &Client, state: &State, line: &str) -> Result<()> {
        self.append_local(line)?;
        if let Err(e) = self.mirror(client, state).await {
            tracing::warn!(error = %format!("{e:#}"), "audit document not updated");
        }
        Ok(())
    }

    fn append_local(&self, line: &str) -> Result<()> {
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .with_context(|| format!("failed to open {}", self.path.display()))?;
        writeln!(f, "{line}")?;
        Ok(())
    }

    async fn mirror(&mut self, client: &Client, state: &State) -> Result<()> {
        if self.mirror_off {
            return Ok(());
        }
        let text = std::fs::read_to_string(&self.path)?;
        let content = tail_lines(&text, DOC_CAP_BYTES);
        if let Some(id) = state.meta(META_DOC_ID)? {
            match client.update_document(&id, content).await {
                Ok(_) => return Ok(()),
                // Deleted from the room UI, most likely; make a new one.
                Err(e) => tracing::warn!(error = %e, "audit document update failed; recreating"),
            }
        }
        let doc = NewDocument {
            name: DOC_NAME.into(),
            description: "Decisions taken on baba-yaga findings in this room".into(),
            instructions: "Append-only log written by baba-yaga: one line per approve, \
                           reject or snooze, newest last. Do not edit."
                .into(),
            content: content.to_string(),
        };
        match client.create_document(&doc).await {
            Ok(created) => {
                state.set_meta(META_DOC_ID, &created.document_id)?;
                tracing::info!(document_id = %created.document_id, "audit document created");
                Ok(())
            }
            Err(e) => {
                self.mirror_off = true;
                Err(e).context("could not create the audit document; mirroring is off")
            }
        }
    }
}

/// `<ISO time> <key> <action> by <who>`.
pub fn line(now: DateTime<Utc>, key: &str, action: &str, who: &str) -> String {
    format!(
        "{} {key} {action} by {who}",
        now.to_rfc3339_opts(SecondsFormat::Secs, true)
    )
}

/// The newest whole lines of `text` that fit in `max` bytes.
pub fn tail_lines(text: &str, max: usize) -> &str {
    if text.len() <= max {
        return text;
    }
    let mut start = text.len() - max;
    while !text.is_char_boundary(start) {
        start += 1;
    }
    // Already at a line start: keep that line.
    if text.as_bytes()[start - 1] == b'\n' {
        return &text[start..];
    }
    // Otherwise skip the partial line.
    match text[start..].find('\n') {
        Some(i) => &text[start + i + 1..],
        None => "",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn line_format() {
        let t = Utc.with_ymd_and_hms(2026, 11, 5, 9, 30, 0).unwrap();
        assert_eq!(
            line(t, "samples:npm:axios", "approved", "shoaib2933"),
            "2026-11-05T09:30:00Z samples:npm:axios approved by shoaib2933"
        );
    }

    #[test]
    fn tail_drops_oldest_whole_lines() {
        let text = "aaaa\nbbbb\ncccc\n";
        assert_eq!(tail_lines(text, 100), text);
        // Exactly two whole lines fit in 10 bytes.
        assert_eq!(tail_lines(text, 10), "bbbb\ncccc\n");
        assert_eq!(tail_lines(text, 11), "bbbb\ncccc\n");
        // One byte less: the partial "bbbb" line is dropped.
        assert_eq!(tail_lines(text, 9), "cccc\n");
        assert_eq!(tail_lines(text, 3), "");
        // Never splits a multi-byte character.
        assert_eq!(tail_lines("åååå\nb\n", 3), "b\n");
        assert_eq!(tail_lines("a\nåååå\nb\n", 8), "b\n");
    }

    #[test]
    fn audit_log_sits_beside_state() {
        assert_eq!(
            Audit::beside(Path::new("/tmp/x/state.db")).path(),
            Path::new("/tmp/x/audit.log")
        );
        assert_eq!(
            Audit::beside(Path::new("state.db")).path(),
            Path::new("audit.log")
        );
    }

    #[test]
    fn local_append_accumulates() {
        let dir = tempfile::tempdir().unwrap();
        let audit = Audit::beside(&dir.path().join("state.db"));
        audit.append_local("one").unwrap();
        audit.append_local("two").unwrap();
        assert_eq!(std::fs::read_to_string(audit.path()).unwrap(), "one\ntwo\n");
    }
}
