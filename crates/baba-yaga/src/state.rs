//! Finding state in SQLite: what has been seen, and where each finding is in
//! the fix/review/approve loop.

use std::path::Path;

use anyhow::{bail, Context, Result};
use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, OptionalExtension};

use crate::scanner::Finding;

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS findings (
    dedup_key     TEXT PRIMARY KEY,
    status        TEXT NOT NULL,
    first_seen    TEXT NOT NULL,
    last_seen     TEXT NOT NULL,
    thread_id     TEXT,
    pr_url        TEXT,
    snoozed_until TEXT,
    json          TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS findings_status ON findings (status);
";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Open,
    Approved,
    Rejected,
    Snoozed,
    Fixed,
}

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Status::Open => "open",
            Status::Approved => "approved",
            Status::Rejected => "rejected",
            Status::Snoozed => "snoozed",
            Status::Fixed => "fixed",
        }
    }
}

/// Whether `upsert_finding` saw the finding for the first time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Upsert {
    New,
    Seen,
}

pub struct State {
    conn: Connection,
}

impl State {
    pub fn open(path: &Path) -> Result<Self> {
        let conn = Connection::open(path)
            .with_context(|| format!("failed to open state db {}", path.display()))?;
        Self::init(conn)
    }

    pub fn open_in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Self> {
        conn.execute_batch(SCHEMA)?;
        Ok(Self { conn })
    }

    /// Insert a new finding as open, or refresh `last_seen` and the stored
    /// JSON of one already known. Status and thread are left alone.
    pub fn upsert_finding(&self, finding: &Finding, now: DateTime<Utc>) -> Result<Upsert> {
        let key = finding.dedup_key();
        let json = serde_json::to_string(finding)?;
        let now = now.to_rfc3339();
        let existed: bool = self
            .conn
            .query_row(
                "SELECT 1 FROM findings WHERE dedup_key = ?1",
                params![key],
                |_| Ok(()),
            )
            .optional()?
            .is_some();
        self.conn.execute(
            "INSERT INTO findings (dedup_key, status, first_seen, last_seen, json)
             VALUES (?1, ?2, ?3, ?3, ?4)
             ON CONFLICT (dedup_key) DO UPDATE SET last_seen = excluded.last_seen, json = excluded.json",
            params![key, Status::Open.as_str(), now, json],
        )?;
        Ok(if existed { Upsert::Seen } else { Upsert::New })
    }

    pub fn set_status(&self, dedup_key: &str, status: Status) -> Result<()> {
        let n = self.conn.execute(
            "UPDATE findings SET status = ?2 WHERE dedup_key = ?1",
            params![dedup_key, status.as_str()],
        )?;
        if n == 0 {
            bail!("unknown finding {dedup_key}");
        }
        Ok(())
    }

    /// Mark a finding snoozed until `until`; `list_open` returns it again after.
    pub fn snooze(&self, dedup_key: &str, until: DateTime<Utc>) -> Result<()> {
        let n = self.conn.execute(
            "UPDATE findings SET status = ?2, snoozed_until = ?3 WHERE dedup_key = ?1",
            params![dedup_key, Status::Snoozed.as_str(), until.to_rfc3339()],
        )?;
        if n == 0 {
            bail!("unknown finding {dedup_key}");
        }
        Ok(())
    }

    /// Open findings, plus snoozed ones whose snooze has run out at `now`,
    /// oldest first.
    pub fn list_open(&self, now: DateTime<Utc>) -> Result<Vec<Finding>> {
        let mut stmt = self.conn.prepare(
            "SELECT json FROM findings
             WHERE status = ?1 OR (status = ?2 AND snoozed_until <= ?3)
             ORDER BY first_seen, dedup_key",
        )?;
        let rows = stmt.query_map(
            params![
                Status::Open.as_str(),
                Status::Snoozed.as_str(),
                now.to_rfc3339()
            ],
            |row| row.get::<_, String>(0),
        )?;
        let mut out = Vec::new();
        for json in rows {
            out.push(serde_json::from_str(&json?)?);
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, TimeZone};

    fn finding(id: &str) -> Finding {
        Finding {
            repo: "web".into(),
            ecosystem: "npm".into(),
            package: "lodash".into(),
            version: "4.17.11".into(),
            id: id.into(),
            aliases: vec![],
            summary: Some("Prototype Pollution".into()),
            severity_score: Some(9.1),
            fixed_version: Some("4.17.12".into()),
            reachable: None,
            source_path: "package-lock.json".into(),
        }
    }

    fn t0() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 10, 31, 12, 0, 0).unwrap()
    }

    #[test]
    fn upsert_reports_new_then_seen() {
        let s = State::open_in_memory().unwrap();
        let f = finding("GHSA-1");
        assert_eq!(s.upsert_finding(&f, t0()).unwrap(), Upsert::New);
        assert_eq!(
            s.upsert_finding(&f, t0() + Duration::hours(1)).unwrap(),
            Upsert::Seen
        );
        assert_eq!(s.list_open(t0()).unwrap(), vec![f]);
    }

    #[test]
    fn upsert_keeps_status() {
        let s = State::open_in_memory().unwrap();
        let f = finding("GHSA-1");
        s.upsert_finding(&f, t0()).unwrap();
        s.set_status(&f.dedup_key(), Status::Rejected).unwrap();
        s.upsert_finding(&f, t0()).unwrap();
        assert!(s.list_open(t0()).unwrap().is_empty());
    }

    #[test]
    fn set_status_unknown_key_fails() {
        let s = State::open_in_memory().unwrap();
        assert!(s.set_status("nope", Status::Approved).is_err());
    }

    #[test]
    fn snoozed_findings_return_when_due() {
        let s = State::open_in_memory().unwrap();
        let a = finding("GHSA-a");
        let b = finding("GHSA-b");
        s.upsert_finding(&a, t0()).unwrap();
        s.upsert_finding(&b, t0()).unwrap();
        s.snooze(&a.dedup_key(), t0() + Duration::days(7)).unwrap();

        assert_eq!(s.list_open(t0()).unwrap(), vec![b.clone()]);
        assert_eq!(s.list_open(t0() + Duration::days(7)).unwrap(), vec![a, b]);
    }
}
