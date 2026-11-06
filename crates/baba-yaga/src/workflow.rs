//! The fix/review/approve loop: posts each (repo, package) group as a thread
//! in the room, routes addressed replies parsed by `room::commands` to state
//! changes, and hands approved groups to a fixer agent.

use std::str::FromStr;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use chrono::{DateTime, Utc};
use serde_json::{json, Value};
use switch_client::{Client, DomainEvent, EventPayload, MessagePayload};
use url::Url;

use crate::audit::{self, Audit};
use crate::config::{Config, Repo};
use crate::room::commands::{self, Command};
use crate::room::format;
use crate::scanner::{self, finding::FindingGroup};
use crate::state::{Change, GroupState, State, Status};

/// Pause between room posts, so a first scan with many groups does not
/// trip the chat platform's rate limit.
const POST_GAP: Duration = Duration::from_millis(400);

/// Switch connection settings from the environment (`.env`). The key is
/// never logged.
pub struct SwitchEnv {
    pub url: Url,
    pub agent_id: String,
    pub agent_key: String,
    pub room_id: String,
}

impl SwitchEnv {
    /// `SWITCH_ROOM_ID` wins over the config's `room` when both are set.
    pub fn load(config_room: Option<&str>) -> Result<Self> {
        let var = |k: &str| std::env::var(k).with_context(|| format!("{k} is not set"));
        let room_id = match std::env::var("SWITCH_ROOM_ID") {
            Ok(r) => r,
            Err(_) => config_room
                .map(str::to_string)
                .context("SWITCH_ROOM_ID is not set and the config has no room")?,
        };
        Ok(Self {
            url: var("SWITCH_URL")?
                .parse()
                .context("SWITCH_URL is not a URL")?,
            agent_id: var("SWITCH_AGENT_ID")?,
            agent_key: var("SWITCH_AGENT_KEY")?,
            room_id,
        })
    }
}

// ---------------------------------------------------------------------------
// Publishing groups
// ---------------------------------------------------------------------------

/// Where group posts go. A trait so tests can count what is posted.
pub(crate) trait RoomPoster {
    /// Post at the top of the room; returns the message id.
    async fn post(&self, text: &str) -> Result<String>;
    /// Reply in the thread that `thread_id` (any message in it) belongs to.
    async fn reply(&self, thread_id: &str, text: &str) -> Result<()>;
    /// False for a dry run, which records nothing in state.
    fn writes_state(&self) -> bool {
        true
    }
}

/// Over a live connection. Thread replies need one: `POST /message`
/// ignores `thread_id` (see docs/NOTES.md).
pub(crate) struct Live<'a>(pub &'a Client);

impl RoomPoster for Live<'_> {
    async fn post(&self, text: &str) -> Result<String> {
        Ok(self.0.post(text).await?.event_id)
    }

    async fn reply(&self, thread_id: &str, text: &str) -> Result<()> {
        self.0.post_in_thread(thread_id, text).await?;
        Ok(())
    }
}

/// How the handoff reaches the fixer. A trait so tests can fake the send.
pub(crate) trait FixerChannel {
    /// Send `text` to the agent named `target`, inside the thread that
    /// `thread_id` (any message in it) belongs to.
    async fn send_to(&self, target: &str, thread_id: &str, text: &str) -> Result<()>;
}

/// `send_targeted_message` takes `thread_id` (Stage 3 ops probe, see
/// docs/PROGRESS.md), so the handoff is both targeted and in the thread.
impl FixerChannel for Live<'_> {
    async fn send_to(&self, target: &str, thread_id: &str, text: &str) -> Result<()> {
        let _: Value = self
            .0
            .op(
                "send_targeted_message",
                json!({"body": text, "target_names": [target], "thread_id": thread_id}),
            )
            .await?;
        Ok(())
    }
}

/// Print to stdout; post nothing, store nothing.
pub(crate) struct DryRun;

impl RoomPoster for DryRun {
    async fn post(&self, text: &str) -> Result<String> {
        println!("--- post ---\n{text}\n");
        Ok("dry-run".into())
    }

    async fn reply(&self, thread_id: &str, text: &str) -> Result<()> {
        println!("--- reply in {thread_id} ---\n{text}\n");
        Ok(())
    }

    fn writes_state(&self) -> bool {
        false
    }
}

#[derive(Debug, Default)]
pub struct Summary {
    pub groups: usize,
    pub new_threads: usize,
    /// Command hints posted in threads, new or older ones without a hint.
    pub hints: usize,
    pub updates: usize,
    pub unchanged: usize,
    pub snoozed: usize,
    pub resolved: usize,
    pub rejected: usize,
    pub failed_repos: usize,
}

impl std::fmt::Display for Summary {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} packages with findings: {} new threads, {} hints, {} updates, {} unchanged, {} snoozed, {} resolved, {} rejected",
            self.groups,
            self.new_threads,
            self.hints,
            self.updates,
            self.unchanged,
            self.snoozed,
            self.resolved,
            self.rejected
        )?;
        if self.failed_repos > 0 {
            write!(f, ", {} repos failed to scan", self.failed_repos)?;
        }
        Ok(())
    }
}

/// Scan `repos` off the async threads (osv-scanner is a blocking child
/// process) and group the findings. A repo that fails is logged and counted.
pub async fn scan_groups(
    repos: &[Repo],
    min_severity: Option<f32>,
    call_analysis: bool,
    summary: &mut Summary,
) -> Result<Vec<FindingGroup>> {
    let mut findings = Vec::new();
    for repo in repos {
        let r = repo.clone();
        match tokio::task::spawn_blocking(move || scanner::scan_repo(&r, call_analysis)).await? {
            Ok(f) => findings.extend(f),
            Err(e) => {
                tracing::error!(repo = %repo.name, error = %format!("{e:#}"), "scan failed");
                summary.failed_repos += 1;
            }
        }
    }
    Ok(scanner::group(scanner::filter_severity(
        findings,
        min_severity,
    )))
}

/// Post every group that needs it and record it in `state` (if any):
/// a new group gets a root message and the command hint as its first reply,
/// a changed group gets a one-line update in its thread, snoozed and
/// resolved and rejected groups are skipped. A thread from before hints existed gets its
/// hint once; the root is never posted again.
pub(crate) async fn publish<P: RoomPoster>(
    groups: &[FindingGroup],
    state: Option<&State>,
    poster: &P,
    now: DateTime<Utc>,
    summary: &mut Summary,
) -> Result<()> {
    let state = state.filter(|_| poster.writes_state());
    summary.groups += groups.len();
    for g in groups {
        let key = g.key();
        let (change, existing) = match state {
            Some(s) => (s.classify(g)?, s.get(&key)?),
            None => (Change::New, None),
        };
        let record = |s: &State| s.record(g, now);

        if let Some(ex) = &existing {
            if ex.is_snoozed(now) {
                summary.snoozed += 1;
                state.map(record).transpose()?;
                continue;
            }
            match ex.status {
                Status::Resolved => summary.resolved += 1,
                Status::Rejected => summary.rejected += 1,
                _ => {}
            }
            if matches!(ex.status, Status::Resolved | Status::Rejected) {
                state.map(record).transpose()?;
                continue;
            }
        }

        match existing
            .as_ref()
            .and_then(|e| e.thread_id.clone().map(|t| (e, t)))
        {
            None => {
                let id = poster.post(&format::render_group(g)).await?;
                if let Some(s) = state {
                    s.record(g, now)?;
                    s.set_thread(&key, &id, now)?;
                }
                tracing::info!(%key, thread_id = %id, "thread opened");
                summary.new_threads += 1;
                tokio::time::sleep(POST_GAP).await;
                post_hint(poster, state, &key, &id, summary).await?;
            }
            Some((ex, thread_id)) => {
                if !ex.hint_posted {
                    post_hint(poster, state, &key, &thread_id, summary).await?;
                }
                if change == Change::Changed {
                    poster
                        .reply(&thread_id, &format::render_update(&ex.group, g))
                        .await?;
                    if let Some(s) = state {
                        s.record(g, now)?;
                        s.mark_posted(&key, now)?;
                    }
                    tracing::info!(%key, %thread_id, "thread updated");
                    summary.updates += 1;
                    tokio::time::sleep(POST_GAP).await;
                } else {
                    state.map(record).transpose()?;
                    summary.unchanged += 1;
                }
            }
        }
    }
    Ok(())
}

/// The command hint as the thread's first reply, so the thread exists for
/// people to answer in.
async fn post_hint<P: RoomPoster>(
    poster: &P,
    state: Option<&State>,
    key: &str,
    thread_id: &str,
    summary: &mut Summary,
) -> Result<()> {
    poster.reply(thread_id, &format::render_hint()).await?;
    if let Some(s) = state {
        s.set_hint_posted(key)?;
    }
    tracing::info!(%key, %thread_id, "hint posted in thread");
    summary.hints += 1;
    tokio::time::sleep(POST_GAP).await;
    Ok(())
}

// ---------------------------------------------------------------------------
// Fixer handoff
// ---------------------------------------------------------------------------

/// Send an approved group to the fixer in its thread and mark it assigned.
/// Returns the reply for the thread. A failed send is reported in that reply
/// and leaves the group approved.
pub(crate) async fn hand_off<C: FixerChannel>(
    state: &State,
    channel: &C,
    fixer: &str,
    repo_path: &str,
    g: &GroupState,
    thread_id: &str,
    who: &str,
) -> Result<String> {
    let body = format::render_handoff(fixer, &g.group, repo_path);
    match channel.send_to(fixer, thread_id, &body).await {
        Ok(()) => {
            state.set_status(&g.key, Status::Assigned)?;
            tracing::info!(key = %g.key, %fixer, %thread_id, "handed to the fixer");
            Ok(format!("Approved by {who}. Handed to @{fixer}."))
        }
        Err(e) => {
            let e = format!("{e:#}");
            tracing::error!(key = %g.key, %fixer, error = %e, "handoff failed");
            Ok(format!(
                "Approved by {who}, but the handoff to @{fixer} failed: {e}. The finding stays approved."
            ))
        }
    }
}

/// Whether `sender_name` is the fixer: case-insensitive, and `_` equals `-`,
/// since the Discord bridge may render either.
pub(crate) fn is_fixer(sender_name: &str, fixer: &str) -> bool {
    let norm = |s: &str| s.trim_start_matches('@').to_lowercase().replace('_', "-");
    norm(sender_name) == norm(fixer)
}

/// The first token in `body` that starts with "http" or contains "fix/",
/// without surrounding quotes, brackets or trailing punctuation.
pub(crate) fn pr_link(body: &str) -> Option<String> {
    body.split_whitespace()
        .map(|t| {
            let wrap = |c: char| matches!(c, '`' | '"' | '\'' | '<' | '>' | '(' | ')' | '[' | ']');
            t.trim_start_matches(wrap).trim_end_matches(|c: char| {
                wrap(c) || matches!(c, '.' | ',' | ';' | ':' | '!' | '?')
            })
        })
        .find(|t| t.starts_with("http") || t.contains("fix/"))
        .map(str::to_string)
}

/// What a fixer message did to state.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum FixerReply {
    /// Not from the fixer, not in a thread, or not in an assigned group's thread.
    Ignored,
    /// In an assigned group's thread but with no link or `fix/` branch, such
    /// as an acknowledgement. The group stays assigned.
    NoLink { key: String },
    /// The group moved to `pr_open` with this link.
    PrOpen { key: String, link: String },
}

/// A message from the fixer in the thread of an assigned group moves the
/// group to `pr_open`, but only when it carries evidence: an http link or a
/// `fix/` branch name. The fixer acknowledges a handoff at once, before it
/// has done anything, so its first message alone proves nothing.
pub(crate) fn track_fixer_reply(
    state: &State,
    fixer: &str,
    sender_name: Option<&str>,
    thread_id: Option<&str>,
    body: &str,
) -> Result<FixerReply> {
    let (Some(name), Some(thread)) = (sender_name, thread_id) else {
        return Ok(FixerReply::Ignored);
    };
    if !is_fixer(name, fixer) {
        return Ok(FixerReply::Ignored);
    }
    let Some(g) = state.find_by_thread(thread)? else {
        return Ok(FixerReply::Ignored);
    };
    if g.status != Status::Assigned {
        return Ok(FixerReply::Ignored);
    }
    match pr_link(body) {
        Some(link) => {
            state.set_pr_open(&g.key, Some(&link))?;
            Ok(FixerReply::PrOpen { key: g.key, link })
        }
        None => Ok(FixerReply::NoLink { key: g.key }),
    }
}

/// The first `n` characters of `s`, for logs.
pub(crate) fn head(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

// ---------------------------------------------------------------------------
// serve
// ---------------------------------------------------------------------------

struct Agent<'a> {
    cfg: &'a Config,
    client: Client,
    state: State,
    audit: Audit,
}

/// Connect to Switch and run the loop: scan now, then on the schedule, and
/// answer addressed messages in between. Runs until the connection stops
/// for good.
pub async fn serve(cfg: &Config) -> Result<()> {
    let env = SwitchEnv::load(Some(&cfg.room))?;
    let schedule = cron::Schedule::from_str(&cfg.schedule)
        .with_context(|| format!("invalid schedule {:?}", cfg.schedule))?;
    let state = State::open(&cfg.state_path)?;
    let (client, mut events) =
        Client::connect(env.url, env.agent_id, env.agent_key, env.room_id).await?;
    client
        .wait_connected(Duration::from_secs(15))
        .await
        .context("could not connect to Switch")?;
    let audit = Audit::beside(&cfg.state_path);
    tracing::info!(path = %audit.path().display(), "audit log");
    let mut agent = Agent {
        cfg,
        client,
        state,
        audit,
    };

    agent
        .client
        .post(&format!(
            "Baba-Yaga online. Watching {}. Next scan: {}.",
            format::count(cfg.repos.len(), "repo", "repos"),
            fmt_time(next_tick(&schedule)?)
        ))
        .await?;
    agent.scan_and_post(None).await;

    loop {
        let next = next_tick(&schedule)?;
        let wait = (next - Utc::now()).to_std().unwrap_or_default();
        tracing::info!(next = %fmt_time(next), "waiting");
        tokio::select! {
            _ = tokio::time::sleep(wait) => {
                agent.scan_and_post(None).await;
            }
            ev = events.recv() => match ev {
                Some(ev) => {
                    if let Err(e) = agent.handle_event(ev).await {
                        tracing::error!(error = %format!("{e:#}"), "command failed");
                    }
                }
                None => {
                    // The connection task stopped for good; say why.
                    agent.client.shutdown().await?;
                    bail!("connection closed");
                }
            },
        }
    }
}

impl Agent<'_> {
    /// Scan (all repos, or the one named `only`), post, and log a summary.
    /// Errors are logged, not returned: a failed scan must not stop serve.
    async fn scan_and_post(&mut self, only: Option<&str>) -> Option<Summary> {
        match self.try_scan_and_post(only).await {
            Ok(summary) => {
                tracing::info!(%summary, "scan finished");
                Some(summary)
            }
            Err(e) => {
                tracing::error!(error = %format!("{e:#}"), "scan failed");
                None
            }
        }
    }

    async fn try_scan_and_post(&mut self, only: Option<&str>) -> Result<Summary> {
        let now = Utc::now();
        for key in self.state.reopen_expired(now)? {
            tracing::info!(%key, "snooze expired");
        }
        let repos: Vec<Repo> = self
            .cfg
            .repos
            .iter()
            .filter(|r| only.is_none_or(|n| r.name == n))
            .cloned()
            .collect();
        if repos.is_empty() {
            bail!("no configured repo named {}", only.unwrap_or("(any)"));
        }
        let mut summary = Summary::default();
        let groups = scan_groups(
            &repos,
            self.cfg.min_severity,
            self.cfg.call_analysis,
            &mut summary,
        )
        .await?;
        publish(
            &groups,
            Some(&self.state),
            &Live(&self.client),
            now,
            &mut summary,
        )
        .await?;
        Ok(summary)
    }

    async fn handle_event(&mut self, ev: DomainEvent) -> Result<()> {
        let Ok(EventPayload::Message(m)) = ev.data.typed_payload() else {
            return Ok(());
        };
        // The fixer's messages are tracked, never answered: answering an
        // agent that listens to everything invites a loop.
        if let Some(fixer) = &self.cfg.fixer_agent {
            if m.sender_name.as_deref().is_some_and(|n| is_fixer(n, fixer)) {
                match track_fixer_reply(
                    &self.state,
                    fixer,
                    m.sender_name.as_deref(),
                    m.thread_id.as_deref(),
                    &m.body,
                )? {
                    FixerReply::PrOpen { key, link } => {
                        tracing::info!(%key, pr_url = %link, "fixer replied");
                        self.record(&key, "fixer replied", fixer).await?;
                    }
                    FixerReply::NoLink { key } => {
                        tracing::info!(
                            %key,
                            body = %head(&m.body, 200),
                            "fixer message without a link; group stays assigned"
                        );
                    }
                    FixerReply::Ignored => {
                        tracing::debug!(seq = ev.id, thread_id = ?m.thread_id, "fixer message ignored");
                    }
                }
                return Ok(());
            }
        }
        if !m.addressed {
            return Ok(());
        }
        let command = commands::parse(&m.body);
        let who = m.sender_name.clone().unwrap_or_else(|| m.sender.clone());
        tracing::info!(seq = ev.id, %who, ?command, thread_id = ?m.thread_id, "command");
        let thread = m.thread_id.as_deref();
        let group = match thread {
            Some(t) => self.state.find_by_thread(t)?,
            None => None,
        };

        let reply = match command {
            Command::Approve | Command::Reject | Command::Snooze { .. } => {
                let Some(g) = group else {
                    return self
                        .reply(
                            thread,
                            "Reply inside a finding's thread to approve, reject or snooze it.",
                        )
                        .await;
                };
                match command {
                    Command::Approve | Command::Reject if !self.may_decide(&m).await? => {
                        "Only a room member can approve or reject.".to_string()
                    }
                    Command::Approve if matches!(g.status, Status::Assigned | Status::PrOpen) => {
                        format!(
                            "Already with the fixer (status {}); not sent again.",
                            g.status.as_str()
                        )
                    }
                    Command::Approve => {
                        self.state.set_status(&g.key, Status::Approved)?;
                        self.record(&g.key, "approved", &who).await?;
                        match (&self.cfg.fixer_agent, thread) {
                            (Some(fixer), Some(t)) => {
                                let repo_path = self.repo_path(&g.group.repo);
                                hand_off(
                                    &self.state,
                                    &Live(&self.client),
                                    fixer,
                                    repo_path,
                                    &g,
                                    t,
                                    &who,
                                )
                                .await?
                            }
                            _ => format!(
                                "Approved by {who}, handing to the fixer. No fixer agent is configured yet, so nothing was sent."
                            ),
                        }
                    }
                    Command::Reject => {
                        self.state.set_status(&g.key, Status::Rejected)?;
                        self.record(&g.key, "rejected", &who).await?;
                        format!("Rejected by {who}; closing this finding.")
                    }
                    Command::Snooze { days } => {
                        let until = Utc::now() + chrono::Duration::days(i64::from(days));
                        self.state.snooze(&g.key, until)?;
                        let action = format!("snoozed until {}", until.format("%Y-%m-%d"));
                        self.record(&g.key, &action, &who).await?;
                        format!("Snoozed by {who} until {}.", until.format("%Y-%m-%d"))
                    }
                    _ => unreachable!("outer match limits the command"),
                }
            }
            Command::Status => format::render_status(
                &self.state.list_open(Utc::now())?,
                &self.state.list_status(Status::Rejected)?,
            ),
            Command::Scan { repo } => {
                self.reply(thread, "Scanning now.").await?;
                match self.scan_and_post(repo.as_deref()).await {
                    Some(summary) => format!("Scan done: {summary}."),
                    None => "Scan failed; see the agent log.".to_string(),
                }
            }
            Command::Help => format::render_help(),
            Command::Unknown => format!("I did not understand that. Try {} help.", format::MENTION),
        };
        self.reply(thread, &reply).await
    }

    async fn record(&mut self, key: &str, action: &str, who: &str) -> Result<()> {
        let line = audit::line(Utc::now(), key, action, who);
        self.audit.append(&self.client, &self.state, &line).await
    }

    /// In the thread when there is one, else at the top of the room.
    async fn reply(&self, thread: Option<&str>, text: &str) -> Result<()> {
        match thread {
            Some(t) => {
                self.client.post_in_thread(t, text).await?;
            }
            None => {
                self.client.post(text).await?;
            }
        }
        Ok(())
    }

    /// The room owner, or any user listed as a participant. Participants are
    /// matched on `sender_name`, the only identifier both sides share.
    async fn may_decide(&self, m: &MessagePayload) -> Result<bool> {
        if m.sender_role.as_deref() == Some("owner") {
            return Ok(true);
        }
        let Some(name) = m.sender_name.as_deref() else {
            return Ok(false);
        };
        let participants = self.client.list_participants().await?;
        Ok(participants
            .iter()
            .any(|p| p.kind == "user" && p.name == name))
    }

    /// The configured URL or directory of the repo named `name`.
    fn repo_path<'b>(&'b self, name: &'b str) -> &'b str {
        self.cfg
            .repos
            .iter()
            .find(|r| r.name == name)
            .map_or(name, |r| r.url.as_str())
    }
}

/// The next scheduled time after now. The schedule is evaluated in UTC.
fn next_tick(schedule: &cron::Schedule) -> Result<DateTime<Utc>> {
    schedule
        .upcoming(Utc)
        .next()
        .context("schedule has no future run")
}

fn fmt_time(t: DateTime<Utc>) -> String {
    t.format("%Y-%m-%d %H:%M UTC").to_string()
}

// ---------------------------------------------------------------------------
// report
// ---------------------------------------------------------------------------

pub struct ReportOptions {
    pub repos: Vec<Repo>,
    pub min_severity: Option<f32>,
    pub call_analysis: bool,
    /// Fallback when `SWITCH_ROOM_ID` is not set.
    pub config_room: Option<String>,
    /// `None`: post every group as a new thread and remember nothing.
    pub state_path: Option<std::path::PathBuf>,
    pub dry_run: bool,
}

/// Scan once, post, then exit. Posting opens a socket for as long as it
/// takes, because the hint in each new thread is a thread reply and those
/// need a live connection. With state, groups already posted are skipped.
pub async fn report(opts: ReportOptions) -> Result<()> {
    let state = opts.state_path.as_deref().map(State::open).transpose()?;
    // Fail on a missing variable before the scan, not after.
    let env = if opts.dry_run {
        None
    } else {
        Some(SwitchEnv::load(opts.config_room.as_deref())?)
    };
    let now = Utc::now();
    if let Some(s) = &state {
        s.reopen_expired(now)?;
    }
    let mut summary = Summary::default();
    let groups = scan_groups(
        &opts.repos,
        opts.min_severity,
        opts.call_analysis,
        &mut summary,
    )
    .await?;
    match env {
        None => publish(&groups, state.as_ref(), &DryRun, now, &mut summary).await?,
        Some(env) => {
            let (client, mut events) =
                Client::connect(env.url, env.agent_id, env.agent_key, env.room_id).await?;
            // Nothing here answers messages; keep the channel empty so the
            // connection task never waits on it.
            let drain = tokio::spawn(async move { while events.recv().await.is_some() {} });
            let posted = async {
                client
                    .wait_connected(Duration::from_secs(15))
                    .await
                    .context("could not connect to Switch")?;
                publish(&groups, state.as_ref(), &Live(&client), now, &mut summary).await
            }
            .await;
            client.shutdown().await?;
            drain.await?;
            posted?;
        }
    }
    tracing::info!(%summary, "report finished");
    if summary.failed_repos > 0 {
        bail!("{} repos failed to scan", summary.failed_repos);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scanner::Finding;
    use chrono::TimeZone;
    use std::cell::RefCell;

    fn finding(package: &str, id: &str) -> Finding {
        Finding {
            repo: "samples".into(),
            ecosystem: "npm".into(),
            package: package.into(),
            version: "1.0.0".into(),
            id: id.into(),
            aliases: vec![],
            summary: None,
            severity_score: Some(7.5),
            fixed_version: Some("1.0.1".into()),
            reachable: None,
            source_path: "node/package-lock.json".into(),
        }
    }

    #[test]
    fn next_tick_is_in_the_future() {
        let s = cron::Schedule::from_str("0 */30 * * * *").unwrap();
        let t = next_tick(&s).unwrap();
        assert!(t > Utc::now());
        assert!(t - Utc::now() <= chrono::Duration::minutes(30));
    }

    #[tokio::test]
    async fn dry_run_posts_every_group_and_writes_nothing() {
        let state = State::open_in_memory().unwrap();
        let groups = scanner::group(vec![
            finding("lodash", "GHSA-1"),
            finding("axios", "GHSA-2"),
        ]);
        let now = Utc.with_ymd_and_hms(2026, 11, 4, 12, 0, 0).unwrap();
        let mut summary = Summary::default();
        publish(&groups, Some(&state), &DryRun, now, &mut summary)
            .await
            .unwrap();
        assert_eq!(summary.new_threads, 2);
        assert!(state.list_open(now).unwrap().is_empty());
    }

    /// Records posts instead of sending them. Roots get ids root-1, root-2...
    #[derive(Default)]
    struct Recorder {
        posts: RefCell<Vec<String>>,
        replies: RefCell<Vec<(String, String)>>,
    }

    impl RoomPoster for Recorder {
        async fn post(&self, text: &str) -> Result<String> {
            let mut posts = self.posts.borrow_mut();
            posts.push(text.into());
            Ok(format!("root-{}", posts.len()))
        }

        async fn reply(&self, thread_id: &str, text: &str) -> Result<()> {
            self.replies
                .borrow_mut()
                .push((thread_id.into(), text.into()));
            Ok(())
        }
    }

    /// Records handoffs; fails them all when `fail` is set.
    #[derive(Default)]
    struct FakeFixer {
        fail: bool,
        sent: RefCell<Vec<(String, String, String)>>,
    }

    impl FixerChannel for FakeFixer {
        async fn send_to(&self, target: &str, thread_id: &str, text: &str) -> Result<()> {
            if self.fail {
                bail!("409: target not in room");
            }
            self.sent
                .borrow_mut()
                .push((target.into(), thread_id.into(), text.into()));
            Ok(())
        }
    }

    /// One posted, approved lodash group in thread sw_root.
    fn approved_group() -> (State, GroupState) {
        let state = State::open_in_memory().unwrap();
        let g = scanner::group(vec![finding("lodash", "GHSA-1")]).remove(0);
        state.record(&g, t0()).unwrap();
        state.set_thread(&g.key(), "sw_root", t0()).unwrap();
        state.set_status(&g.key(), Status::Approved).unwrap();
        let row = state.get(&g.key()).unwrap().unwrap();
        (state, row)
    }

    #[tokio::test]
    async fn approved_goes_to_assigned_then_pr_open() {
        let (state, g) = approved_group();
        let fixer = FakeFixer::default();
        let reply = hand_off(
            &state,
            &fixer,
            "patrol-fixer",
            "/w/samples",
            &g,
            "sw_reply",
            "alice",
        )
        .await
        .unwrap();
        assert_eq!(reply, "Approved by alice. Handed to @patrol-fixer.");
        let sent = fixer.sent.borrow();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].0, "patrol-fixer");
        assert_eq!(sent[0].1, "sw_reply");
        assert!(sent[0]
            .2
            .starts_with("@patrol-fixer please fix: repo /w/samples, "));
        assert_eq!(state.get(&g.key).unwrap().unwrap().status, Status::Assigned);

        // Someone else talking in the thread changes nothing.
        let other = track_fixer_reply(
            &state,
            "patrol-fixer",
            Some("alice"),
            Some("sw_root"),
            "fix/x",
        );
        assert_eq!(other.unwrap(), FixerReply::Ignored);

        // An acknowledgement without a link leaves it assigned.
        let ack = track_fixer_reply(
            &state,
            "patrol-fixer",
            Some("patrol-fixer"),
            Some("sw_root"),
            "On it, starting now.",
        )
        .unwrap();
        assert_eq!(ack, FixerReply::NoLink { key: g.key.clone() });
        assert_eq!(state.get(&g.key).unwrap().unwrap().status, Status::Assigned);

        let moved = track_fixer_reply(
            &state,
            "patrol-fixer",
            Some("Patrol_Fixer"),
            Some("sw_root"),
            "Pushed `fix/lodash-4.17.21`. PR: https://github.com/o/r/pull/3",
        )
        .unwrap();
        assert_eq!(
            moved,
            FixerReply::PrOpen {
                key: g.key.clone(),
                link: "fix/lodash-4.17.21".to_string()
            }
        );
        let row = state.get(&g.key).unwrap().unwrap();
        assert_eq!(row.status, Status::PrOpen);
        assert_eq!(row.pr_url.as_deref(), Some("fix/lodash-4.17.21"));

        // Only the first reply after assignment counts.
        let again = track_fixer_reply(
            &state,
            "patrol-fixer",
            Some("patrol-fixer"),
            Some("sw_root"),
            "https://x",
        );
        assert_eq!(again.unwrap(), FixerReply::Ignored);
    }

    #[tokio::test]
    async fn failed_handoff_keeps_approved_and_says_why() {
        let (state, g) = approved_group();
        let fixer = FakeFixer {
            fail: true,
            ..Default::default()
        };
        let reply = hand_off(&state, &fixer, "patrol-fixer", "/w", &g, "sw_root", "alice")
            .await
            .unwrap();
        assert!(reply.contains("handoff to @patrol-fixer failed: 409: target not in room"));
        assert_eq!(state.get(&g.key).unwrap().unwrap().status, Status::Approved);
    }

    #[test]
    fn fixer_name_match() {
        assert!(is_fixer("patrol-fixer", "patrol-fixer"));
        assert!(is_fixer("Patrol_Fixer", "patrol-fixer"));
        assert!(is_fixer("@patrol_fixer", "patrol-fixer"));
        assert!(!is_fixer("patrol-fixer2", "patrol-fixer"));
        assert!(!is_fixer("baba_yaga", "patrol-fixer"));
    }

    #[test]
    fn pr_link_takes_first_url_or_fix_branch() {
        assert_eq!(
            pr_link("Opened https://github.com/o/r/pull/3."),
            Some("https://github.com/o/r/pull/3".into())
        );
        assert_eq!(
            pr_link("Branch fix/lodash pushed, see https://x"),
            Some("fix/lodash".into())
        );
        assert_eq!(pr_link("done"), None);
    }

    fn t0() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 4, 12, 0, 0).unwrap()
    }

    #[tokio::test]
    async fn new_group_gets_root_then_hint_in_its_thread() {
        let state = State::open_in_memory().unwrap();
        let groups = scanner::group(vec![finding("lodash", "GHSA-1")]);
        let poster = Recorder::default();
        let mut summary = Summary::default();
        publish(&groups, Some(&state), &poster, t0(), &mut summary)
            .await
            .unwrap();
        assert_eq!(poster.posts.borrow().len(), 1);
        assert_eq!(
            *poster.replies.borrow(),
            vec![("root-1".to_string(), format::render_hint())]
        );
        let row = state.get(&groups[0].key()).unwrap().unwrap();
        assert_eq!(row.thread_id.as_deref(), Some("root-1"));
        assert!(row.hint_posted);
        assert_eq!((summary.new_threads, summary.hints), (1, 1));
    }

    #[tokio::test]
    async fn old_thread_without_hint_gets_exactly_one() {
        let state = State::open_in_memory().unwrap();
        let groups = scanner::group(vec![finding("lodash", "GHSA-1")]);
        let key = groups[0].key();
        // As serve left it before hints: posted root, no hint.
        state.record(&groups[0], t0()).unwrap();
        state.set_thread(&key, "sw_root", t0()).unwrap();
        assert!(!state.get(&key).unwrap().unwrap().hint_posted);

        let poster = Recorder::default();
        for _ in 0..2 {
            let mut summary = Summary::default();
            publish(&groups, Some(&state), &poster, t0(), &mut summary)
                .await
                .unwrap();
        }
        assert!(poster.posts.borrow().is_empty(), "root is not reposted");
        assert_eq!(
            *poster.replies.borrow(),
            vec![("sw_root".to_string(), format::render_hint())]
        );
        assert!(state.get(&key).unwrap().unwrap().hint_posted);
    }
}
