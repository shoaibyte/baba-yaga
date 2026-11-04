//! Finding state in SQLite, one row per (repo, ecosystem, package) group:
//! which advisories were seen, which thread the group lives in, and where it
//! is in the fix/review/approve loop.

use std::path::Path;

use anyhow::{bail, Context, Result};
use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, OptionalExtension, Row};

use crate::scanner::finding::FindingGroup;

const SCHEMA_VERSION: i64 = 3;

/// Version 2. Version 1 had one row per advisory (`findings`). This is
/// pre-release, so the migration drops v1 rather than converting it.
const SCHEMA_V2: &str = "
DROP TABLE IF EXISTS findings;
CREATE TABLE IF NOT EXISTS groups (
    key            TEXT PRIMARY KEY,  -- repo:ecosystem:package
    vuln_ids       TEXT NOT NULL,     -- JSON array, sorted
    highest_fixed  TEXT,
    version        TEXT NOT NULL,
    thread_id      TEXT,
    status         TEXT NOT NULL,
    snoozed_until  TEXT,
    first_seen     TEXT NOT NULL,
    last_seen      TEXT NOT NULL,
    last_posted    TEXT,
    json           TEXT NOT NULL      -- the FindingGroup
);
CREATE INDEX IF NOT EXISTS groups_status ON groups (status);
CREATE INDEX IF NOT EXISTS groups_thread ON groups (thread_id);
CREATE TABLE IF NOT EXISTS meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
";

/// Version 3: whether the in-thread command hint was posted. Rows from v2
/// start at 0, so the next scan posts their hint.
const MIGRATE_V3: &str = "ALTER TABLE groups ADD COLUMN hint_posted INTEGER NOT NULL DEFAULT 0;";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Open,
    Assigned,
    PrOpen,
    Reviewed,
    Approved,
    Snoozed,
    /// Fixed, or rejected by a room member.
    Resolved,
}

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Status::Open => "open",
            Status::Assigned => "assigned",
            Status::PrOpen => "pr_open",
            Status::Reviewed => "reviewed",
            Status::Approved => "approved",
            Status::Snoozed => "snoozed",
            Status::Resolved => "resolved",
        }
    }

    pub fn parse(s: &str) -> Result<Self> {
        Ok(match s {
            "open" => Status::Open,
            "assigned" => Status::Assigned,
            "pr_open" => Status::PrOpen,
            "reviewed" => Status::Reviewed,
            "approved" => Status::Approved,
            "snoozed" => Status::Snoozed,
            "resolved" => Status::Resolved,
            other => bail!("unknown status {other:?} in state"),
        })
    }
}

/// How a scanned group compares with what state already holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Change {
    /// Not in state yet.
    New,
    /// New advisory ids, a different highest fix, or a different version.
    Changed,
    Unchanged,
}

/// One stored group and its loop state.
#[derive(Debug, Clone, PartialEq)]
pub struct GroupState {
    pub key: String,
    pub group: FindingGroup,
    pub thread_id: Option<String>,
    pub status: Status,
    pub snoozed_until: Option<DateTime<Utc>>,
    pub first_seen: DateTime<Utc>,
    pub last_seen: DateTime<Utc>,
    pub last_posted: Option<DateTime<Utc>>,
    /// The command hint was posted in the thread. Posting it is what makes
    /// the thread exist on Discord.
    pub hint_posted: bool,
}

impl GroupState {
    /// Snoozed with a snooze still running at `now`.
    pub fn is_snoozed(&self, now: DateTime<Utc>) -> bool {
        self.status == Status::Snoozed && self.snoozed_until.is_some_and(|t| t > now)
    }
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

    #[cfg(test)]
    pub fn open_in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Self> {
        let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if version > SCHEMA_VERSION {
            bail!("state db schema {version} is newer than this build ({SCHEMA_VERSION})");
        }
        if version < SCHEMA_VERSION {
            let tx = conn.unchecked_transaction()?;
            if version < 2 {
                tx.execute_batch(SCHEMA_V2)?;
            }
            if version < 3 {
                tx.execute_batch(MIGRATE_V3)?;
            }
            tx.pragma_update(None, "user_version", SCHEMA_VERSION)?;
            tx.commit()?;
        }
        Ok(Self { conn })
    }

    /// Compare a scanned group with the stored one, without writing.
    pub fn classify(&self, group: &FindingGroup) -> Result<Change> {
        let Some(old) = self.get(&group.key())? else {
            return Ok(Change::New);
        };
        let old_ids = old.group.vuln_ids();
        let has_new_id = group.vuln_ids().iter().any(|id| !old_ids.contains(id));
        Ok(
            if has_new_id
                || group.highest_fixed != old.group.highest_fixed
                || group.version != old.group.version
            {
                Change::Changed
            } else {
                Change::Unchanged
            },
        )
    }

    /// Store a scanned group: insert it as open, or refresh its ids, fix,
    /// version and `last_seen`. Status, thread and snooze are left alone.
    pub fn record(&self, group: &FindingGroup, now: DateTime<Utc>) -> Result<()> {
        let now = now.to_rfc3339();
        self.conn.execute(
            "INSERT INTO groups (key, vuln_ids, highest_fixed, version, status, first_seen, last_seen, json)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6, ?7)
             ON CONFLICT (key) DO UPDATE SET
                 vuln_ids = excluded.vuln_ids,
                 highest_fixed = excluded.highest_fixed,
                 version = excluded.version,
                 last_seen = excluded.last_seen,
                 json = excluded.json",
            params![
                group.key(),
                serde_json::to_string(&group.vuln_ids())?,
                group.highest_fixed,
                group.version,
                Status::Open.as_str(),
                now,
                serde_json::to_string(group)?,
            ],
        )?;
        Ok(())
    }

    pub fn get(&self, key: &str) -> Result<Option<GroupState>> {
        self.conn
            .query_row(
                &format!("SELECT {COLUMNS} FROM groups WHERE key = ?1"),
                params![key],
                read_row,
            )
            .optional()?
            .transpose()
    }

    /// The group whose thread root is `thread_id`.
    pub fn find_by_thread(&self, thread_id: &str) -> Result<Option<GroupState>> {
        self.conn
            .query_row(
                &format!("SELECT {COLUMNS} FROM groups WHERE thread_id = ?1"),
                params![thread_id],
                read_row,
            )
            .optional()?
            .transpose()
    }

    /// Remember the thread a group was posted in.
    pub fn set_thread(&self, key: &str, thread_id: &str, now: DateTime<Utc>) -> Result<()> {
        self.update(
            key,
            "thread_id = ?2, last_posted = ?3",
            params![key, thread_id, now.to_rfc3339()],
        )
    }

    pub fn set_hint_posted(&self, key: &str) -> Result<()> {
        self.update(key, "hint_posted = 1", params![key])
    }

    pub fn mark_posted(&self, key: &str, now: DateTime<Utc>) -> Result<()> {
        self.update(key, "last_posted = ?2", params![key, now.to_rfc3339()])
    }

    /// Set a status and clear any snooze.
    pub fn set_status(&self, key: &str, status: Status) -> Result<()> {
        self.update(
            key,
            "status = ?2, snoozed_until = NULL",
            params![key, status.as_str()],
        )
    }

    pub fn snooze(&self, key: &str, until: DateTime<Utc>) -> Result<()> {
        self.update(
            key,
            "status = ?2, snoozed_until = ?3",
            params![key, Status::Snoozed.as_str(), until.to_rfc3339()],
        )
    }

    /// Put every snoozed group whose snooze has run out at `now` back to
    /// open. Returns their keys.
    pub fn reopen_expired(&self, now: DateTime<Utc>) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare(
            "UPDATE groups SET status = ?1, snoozed_until = NULL
             WHERE status = ?2 AND snoozed_until <= ?3
             RETURNING key",
        )?;
        let keys = stmt
            .query_map(
                params![
                    Status::Open.as_str(),
                    Status::Snoozed.as_str(),
                    now.to_rfc3339()
                ],
                |r| r.get::<_, String>(0),
            )?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(keys)
    }

    /// Groups not resolved and not snoozed at `now`, by key.
    pub fn list_open(&self, now: DateTime<Utc>) -> Result<Vec<GroupState>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {COLUMNS} FROM groups
             WHERE status != ?1 AND NOT (status = ?2 AND snoozed_until > ?3)
             ORDER BY key"
        ))?;
        let rows = stmt.query_map(
            params![
                Status::Resolved.as_str(),
                Status::Snoozed.as_str(),
                now.to_rfc3339()
            ],
            read_row,
        )?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row??);
        }
        Ok(out)
    }

    pub fn meta(&self, key: &str) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row("SELECT value FROM meta WHERE key = ?1", params![key], |r| {
                r.get(0)
            })
            .optional()?)
    }

    pub fn set_meta(&self, key: &str, value: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO meta (key, value) VALUES (?1, ?2)
             ON CONFLICT (key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )?;
        Ok(())
    }

    fn update(&self, key: &str, set: &str, args: impl rusqlite::Params) -> Result<()> {
        let n = self
            .conn
            .execute(&format!("UPDATE groups SET {set} WHERE key = ?1"), args)?;
        if n == 0 {
            bail!("unknown group {key}");
        }
        Ok(())
    }
}

const COLUMNS: &str =
    "key, json, thread_id, status, snoozed_until, first_seen, last_seen, last_posted, hint_posted";

/// Map one row; the inner `Result` carries JSON and date errors, which
/// rusqlite's closure type cannot.
fn read_row(row: &Row<'_>) -> rusqlite::Result<Result<GroupState>> {
    let key: String = row.get(0)?;
    let json: String = row.get(1)?;
    let thread_id: Option<String> = row.get(2)?;
    let status: String = row.get(3)?;
    let snoozed_until: Option<String> = row.get(4)?;
    let first_seen: String = row.get(5)?;
    let last_seen: String = row.get(6)?;
    let last_posted: Option<String> = row.get(7)?;
    let hint_posted: bool = row.get(8)?;
    Ok((|| {
        Ok(GroupState {
            key,
            group: serde_json::from_str(&json)?,
            thread_id,
            status: Status::parse(&status)?,
            snoozed_until: snoozed_until.as_deref().map(parse_time).transpose()?,
            first_seen: parse_time(&first_seen)?,
            last_seen: parse_time(&last_seen)?,
            last_posted: last_posted.as_deref().map(parse_time).transpose()?,
            hint_posted,
        })
    })())
}

fn parse_time(s: &str) -> Result<DateTime<Utc>> {
    Ok(DateTime::parse_from_rfc3339(s)
        .with_context(|| format!("bad time {s:?} in state"))?
        .with_timezone(&Utc))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scanner::{group, Finding};
    use chrono::{Duration, TimeZone};

    fn finding(package: &str, id: &str, fixed: &str) -> Finding {
        Finding {
            repo: "web".into(),
            ecosystem: "npm".into(),
            package: package.into(),
            version: "4.17.11".into(),
            id: id.into(),
            aliases: vec![],
            summary: Some("Prototype Pollution".into()),
            severity_score: Some(9.1),
            fixed_version: Some(fixed.into()),
            reachable: None,
            source_path: "package-lock.json".into(),
        }
    }

    fn lodash(ids: &[(&str, &str)]) -> FindingGroup {
        group(
            ids.iter()
                .map(|(id, fix)| finding("lodash", id, fix))
                .collect(),
        )
        .remove(0)
    }

    fn t0() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 10, 31, 12, 0, 0).unwrap()
    }

    #[test]
    fn classify_new_changed_unchanged() {
        let s = State::open_in_memory().unwrap();
        let g = lodash(&[("GHSA-1", "4.17.12")]);
        assert_eq!(s.classify(&g).unwrap(), Change::New);
        s.record(&g, t0()).unwrap();
        assert_eq!(s.classify(&g).unwrap(), Change::Unchanged);

        let more = lodash(&[("GHSA-1", "4.17.12"), ("GHSA-2", "4.17.12")]);
        assert_eq!(s.classify(&more).unwrap(), Change::Changed);
        let higher_fix = lodash(&[("GHSA-1", "4.17.21")]);
        assert_eq!(s.classify(&higher_fix).unwrap(), Change::Changed);
        // An advisory that went away is not a change worth posting.
        s.record(&more, t0()).unwrap();
        assert_eq!(s.classify(&g).unwrap(), Change::Unchanged);
    }

    #[test]
    fn record_keeps_status_thread_and_first_seen() {
        let s = State::open_in_memory().unwrap();
        let g = lodash(&[("GHSA-1", "4.17.12")]);
        s.record(&g, t0()).unwrap();
        s.set_thread(&g.key(), "sw_root", t0()).unwrap();
        s.set_status(&g.key(), Status::Approved).unwrap();
        s.record(&g, t0() + Duration::hours(1)).unwrap();

        let row = s.get("web:npm:lodash").unwrap().unwrap();
        assert_eq!(row.status, Status::Approved);
        assert_eq!(row.thread_id.as_deref(), Some("sw_root"));
        assert_eq!(row.first_seen, t0());
        assert_eq!(row.last_seen, t0() + Duration::hours(1));
        assert_eq!(row.last_posted, Some(t0()));
        assert_eq!(row.group, g);
        assert_eq!(s.find_by_thread("sw_root").unwrap().unwrap().key, row.key);
        assert!(s.find_by_thread("sw_other").unwrap().is_none());
    }

    #[test]
    fn open_snoozed_open_after_expiry() {
        let s = State::open_in_memory().unwrap();
        let g = lodash(&[("GHSA-1", "4.17.12")]);
        let key = g.key();
        s.record(&g, t0()).unwrap();
        assert_eq!(s.list_open(t0()).unwrap().len(), 1);

        s.snooze(&key, t0() + Duration::days(30)).unwrap();
        let row = s.get(&key).unwrap().unwrap();
        assert_eq!(row.status, Status::Snoozed);
        assert!(row.is_snoozed(t0()));
        assert!(s.list_open(t0()).unwrap().is_empty());
        assert!(s.reopen_expired(t0()).unwrap().is_empty());

        let later = t0() + Duration::days(30);
        assert!(!row.is_snoozed(later));
        assert_eq!(s.list_open(later).unwrap().len(), 1, "due snooze is listed");
        assert_eq!(s.reopen_expired(later).unwrap(), vec![key.clone()]);
        let row = s.get(&key).unwrap().unwrap();
        assert_eq!(row.status, Status::Open);
        assert_eq!(row.snoozed_until, None);
    }

    #[test]
    fn resolved_is_not_open_and_unknown_key_fails() {
        let s = State::open_in_memory().unwrap();
        let g = lodash(&[("GHSA-1", "4.17.12")]);
        s.record(&g, t0()).unwrap();
        s.set_status(&g.key(), Status::Resolved).unwrap();
        assert!(s.list_open(t0()).unwrap().is_empty());
        assert!(s.set_status("nope", Status::Approved).is_err());
    }

    #[test]
    fn v1_database_is_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.db");
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch("CREATE TABLE findings (dedup_key TEXT PRIMARY KEY);")
                .unwrap();
        }
        let s = State::open(&path).unwrap();
        let has_v1: bool = s
            .conn
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE name = 'findings'",
                [],
                |r| r.get::<_, i64>(0),
            )
            .unwrap()
            > 0;
        assert!(!has_v1);
        s.set_meta("audit_doc", "doc-1").unwrap();
        drop(s);
        // Reopening at the current version keeps data.
        let s = State::open(&path).unwrap();
        assert_eq!(s.meta("audit_doc").unwrap().as_deref(), Some("doc-1"));
    }

    #[test]
    fn v2_database_gains_hint_posted_and_keeps_rows() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.db");
        let g = lodash(&[("GHSA-1", "4.17.12")]);
        {
            // A v2 file with one posted group, as serve left it.
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(SCHEMA_V2).unwrap();
            conn.pragma_update(None, "user_version", 2).unwrap();
            conn.execute(
                "INSERT INTO groups (key, vuln_ids, highest_fixed, version, thread_id, status,
                                     first_seen, last_seen, last_posted, json)
                 VALUES (?1, '[\"GHSA-1\"]', '4.17.12', '4.17.11', 'sw_root', 'open', ?2, ?2, ?2, ?3)",
                params![g.key(), t0().to_rfc3339(), serde_json::to_string(&g).unwrap()],
            )
            .unwrap();
        }
        let s = State::open(&path).unwrap();
        let version: i64 = s
            .conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(version, 3);
        let row = s.get(&g.key()).unwrap().unwrap();
        assert_eq!(row.thread_id.as_deref(), Some("sw_root"));
        assert!(!row.hint_posted, "existing rows start without a hint");

        s.set_hint_posted(&g.key()).unwrap();
        drop(s);
        let s = State::open(&path).unwrap();
        assert!(s.get(&g.key()).unwrap().unwrap().hint_posted);
    }
}
