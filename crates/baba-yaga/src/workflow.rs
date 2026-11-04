//! The fix/review/approve loop: posts each (repo, package) group as a thread
//! in the room, routes addressed replies parsed by `room::commands` to state
//! changes, and hands approved groups to a fixer agent.

use std::str::FromStr;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use chrono::{DateTime, Utc};
use serde_json::{json, Value};
use switch_client::{Client, DomainEvent, EventPayload, Http, MessagePayload};
use url::Url;

use crate::audit::{self, Audit};
use crate::config::{Config, Repo};
use crate::room::commands::{self, Command};
use crate::room::format;
use crate::scanner::{self, finding::FindingGroup};
use crate::state::{Change, State, Status};

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

/// Where group posts go.
pub enum Poster<'a> {
    /// Over the live connection: top-level posts and thread replies.
    Live(&'a Client),
    /// HTTP only, no socket: top-level posts only, because `POST /message`
    /// ignores `thread_id` (see docs/NOTES.md).
    Http { http: &'a Http, env: &'a SwitchEnv },
    /// Print to stdout; post nothing, store nothing.
    DryRun,
}

impl Poster<'_> {
    /// Post at the top of the room; returns the message id.
    async fn post(&self, text: &str) -> Result<String> {
        Ok(match self {
            Poster::Live(c) => c.post(text).await?.event_id,
            Poster::Http { http, env } => {
                http.post_message(&env.agent_key, &env.agent_id, &env.room_id, text)
                    .await?
                    .event_id
            }
            Poster::DryRun => {
                println!("--- post ---\n{text}\n");
                "dry-run".into()
            }
        })
    }

    /// Reply in a thread. `Ok(false)` when this poster cannot.
    async fn reply(&self, thread_id: &str, text: &str) -> Result<bool> {
        match self {
            Poster::Live(c) => {
                c.post_in_thread(thread_id, text).await?;
                Ok(true)
            }
            Poster::Http { .. } => Ok(false),
            Poster::DryRun => {
                println!("--- reply in {thread_id} ---\n{text}\n");
                Ok(true)
            }
        }
    }

    fn writes_state(&self) -> bool {
        !matches!(self, Poster::DryRun)
    }
}

#[derive(Debug, Default)]
pub struct Summary {
    pub groups: usize,
    pub new_threads: usize,
    pub updates: usize,
    /// Changed groups whose thread could not be updated (HTTP-only poster);
    /// left unrecorded so the next live scan posts the update.
    pub pending_updates: usize,
    pub unchanged: usize,
    pub snoozed: usize,
    pub resolved: usize,
    pub failed_repos: usize,
}

impl std::fmt::Display for Summary {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} packages with findings: {} new threads, {} updates, {} unchanged, {} snoozed, {} resolved",
            self.groups, self.new_threads, self.updates, self.unchanged, self.snoozed, self.resolved
        )?;
        if self.pending_updates > 0 {
            write!(f, ", {} updates left for serve", self.pending_updates)?;
        }
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
/// a new group gets a thread, a changed group gets a one-line update in its
/// thread, snoozed and resolved groups are skipped.
pub async fn publish(
    groups: &[FindingGroup],
    state: Option<&State>,
    poster: &Poster<'_>,
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
            if ex.status == Status::Resolved {
                summary.resolved += 1;
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
            }
            Some((ex, thread_id)) if change == Change::Changed => {
                let text = format::render_update(&ex.group, g);
                if poster.reply(&thread_id, &text).await? {
                    if let Some(s) = state {
                        s.record(g, now)?;
                        s.mark_posted(&key, now)?;
                    }
                    tracing::info!(%key, %thread_id, "thread updated");
                    summary.updates += 1;
                    tokio::time::sleep(POST_GAP).await;
                } else {
                    tracing::info!(%key, %thread_id, "changed; update left for serve");
                    summary.pending_updates += 1;
                }
            }
            Some(_) => {
                state.map(record).transpose()?;
                summary.unchanged += 1;
            }
        }
    }
    Ok(())
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
            "Baba-Yaga online. Watching {} repos. Next scan: {}.",
            cfg.repos.len(),
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
            &Poster::Live(&self.client),
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
                    Command::Approve => {
                        self.state.set_status(&g.key, Status::Approved)?;
                        self.record(&g.key, "approved", &who).await?;
                        let mut text = format!("Approved by {who}, handing to the fixer.");
                        match &self.cfg.fixer_agent {
                            Some(fixer) => self.hand_to_fixer(fixer, &g.group, thread).await?,
                            None => text.push_str(
                                " No fixer agent is configured yet, so nothing was sent.",
                            ),
                        }
                        text
                    }
                    Command::Reject => {
                        self.state.set_status(&g.key, Status::Resolved)?;
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
            Command::Status => format::render_status(&self.state.list_open(Utc::now())?),
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

    // TODO(stage 5): the fixer protocol. For now one targeted message in the
    // thread names the package and the fix version.
    async fn hand_to_fixer(
        &self,
        fixer: &str,
        group: &FindingGroup,
        thread: Option<&str>,
    ) -> Result<()> {
        let body = format!(
            "Please open a pull request upgrading {} in {} ({}) from {} to {}. Advisories: {}.",
            group.package,
            group.repo,
            group.ecosystem,
            group.version,
            group.highest_fixed.as_deref().unwrap_or("a fixed version"),
            group.vuln_ids().join(", ")
        );
        let _: Value = self
            .client
            .op(
                "send_targeted_message",
                json!({"body": body, "target_names": [fixer], "thread_id": thread}),
            )
            .await?;
        Ok(())
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

/// Scan once and post over HTTP, without a socket, then exit. With state,
/// groups already posted are skipped, and changed ones are left unrecorded
/// so `serve` posts the update in their thread.
pub async fn report(opts: ReportOptions) -> Result<()> {
    let state = opts.state_path.as_deref().map(State::open).transpose()?;
    let env;
    let http;
    let poster = if opts.dry_run {
        Poster::DryRun
    } else {
        env = SwitchEnv::load(opts.config_room.as_deref())?;
        http = Http::new(env.url.clone());
        http.health()
            .await
            .with_context(|| format!("GET /health failed on {}", env.url))?;
        Poster::Http {
            http: &http,
            env: &env,
        }
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
    publish(&groups, state.as_ref(), &poster, now, &mut summary).await?;
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
        publish(&groups, Some(&state), &Poster::DryRun, now, &mut summary)
            .await
            .unwrap();
        assert_eq!(summary.new_threads, 2);
        assert!(state.list_open(now).unwrap().is_empty());
    }
}
