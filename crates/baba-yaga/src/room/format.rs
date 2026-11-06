//! Text for the room, laid out for Discord through the Switch bridge.
//!
//! What renders there: **bold**, `inline code`, "- " bullets, blank lines,
//! and plain URLs as links. Tables, headings and emoji do not, so none are
//! used. Discord names a thread after the first line of its root message
//! (about 60 characters), so line 1 of a root is a short, complete title.
//! A root says what, how bad and what to do in four lines; the advisories
//! are one reply down (`render_details`).

use chrono::{DateTime, Utc};

use crate::scanner::finding::FindingGroup;
use crate::state::{GroupState, Status};

/// How the agent is addressed on Discord.
pub const MENTION: &str = "@baba_yaga";
/// Every message stays under this many characters (Discord allows 2000).
pub const MESSAGE_LIMIT: usize = 1800;
/// Lines in a root message.
pub const ROOT_LINES: usize = 4;
/// Advisories listed in the details reply.
const MAX_DETAILS: usize = 5;
const MAX_SUMMARY_CHARS: usize = 90;
/// Status rows kept when the full list would not fit.
const MAX_STATUS_ROWS: usize = 15;
/// Versions longer than this are cut to `SHORT_VERSION_CHARS` plus "…".
const MAX_VERSION_CHARS: usize = 24;
const SHORT_VERSION_CHARS: usize = 20;

/// CVSS v3 qualitative rating for a base score.
pub fn severity_label(score: Option<f32>) -> &'static str {
    match score {
        None => "UNSCORED",
        Some(s) if s >= 9.0 => "CRITICAL",
        Some(s) if s >= 7.0 => "HIGH",
        Some(s) if s >= 4.0 => "MEDIUM",
        Some(s) if s > 0.0 => "LOW",
        Some(_) => "NONE",
    }
}

/// `HIGH 8.9`, or `UNSCORED`.
fn severity(score: Option<f32>) -> String {
    match score {
        Some(s) => format!("{} {s:.1}", severity_label(Some(s))),
        None => severity_label(None).to_string(),
    }
}

/// A version as shown: Go pseudo-versions and other long strings are cut,
/// `0.0.0-20190620200207-3b0461eec859` becomes `0.0.0-20190620200207…`.
pub fn short_version(v: &str) -> String {
    if v.chars().count() > MAX_VERSION_CHARS {
        let kept: String = v.chars().take(SHORT_VERSION_CHARS).collect();
        format!("{kept}…")
    } else {
        v.to_string()
    }
}

/// The short fix version, or `None` when no fix is released.
fn fix(group: &FindingGroup) -> Option<String> {
    group.highest_fixed.as_deref().map(short_version)
}

/// `<package> <version> → <fix>`, or `..., no fix released`.
fn title(group: &FindingGroup) -> String {
    let current = format!("{} {}", group.package, short_version(&group.version));
    match fix(group) {
        Some(f) => format!("{current} → {f}"),
        None => format!("{current}, no fix released"),
    }
}

fn reachable(group: &FindingGroup) -> &'static str {
    match group.any_reachable {
        Some(true) => "yes",
        Some(false) => "no",
        None => "unknown",
    }
}

/// The thread's root message, four lines: title, severity, where, and how
/// to answer. `multi_repo` adds the repo name, which a single-repo setup
/// does not need.
pub fn render_group(group: &FindingGroup, multi_repo: bool) -> String {
    let mut sources: Vec<String> = group
        .findings
        .iter()
        .map(|f| format!("`{}`", f.source_path))
        .collect();
    sources.sort_unstable();
    sources.dedup();
    let repo = if multi_repo {
        format!("{} · ", group.repo)
    } else {
        String::new()
    };
    let text = format!(
        "{}\n**{}** · {} · reachable: {}\n{repo}{} · {}\nReply here: `{MENTION} approve` · `snooze 30d` · `reject`",
        title(group),
        severity(group.max_severity),
        advisories(group.vuln_ids().len()),
        reachable(group),
        group.ecosystem,
        sources.join(", "),
    );
    debug_assert_eq!(text.lines().count(), ROOT_LINES);
    limit(&text)
}

/// The first reply in a group's thread: the advisories, highest severity
/// first, up to five, a link to the rest on osv.dev, and the fix versions
/// when there is more than one. Posting it is also what makes Discord show
/// the thread.
pub fn render_details(group: &FindingGroup) -> String {
    let total = group.vuln_ids().len();
    let mut lines = vec![format!("**Advisories ({total})**")];
    // Findings are sorted highest severity first; list each id once.
    let mut seen: Vec<&str> = Vec::new();
    for f in &group.findings {
        if seen.len() == MAX_DETAILS {
            break;
        }
        if seen.contains(&f.id.as_str()) {
            continue;
        }
        seen.push(f.id.as_str());
        let summary = f
            .summary
            .as_deref()
            .or(f.aliases.first().map(String::as_str))
            .unwrap_or("no summary");
        lines.push(format!(
            "- `{}` {}: {}",
            f.id,
            severity(f.severity_score),
            summary_head(summary)
        ));
    }
    if total > seen.len() {
        lines.push(format!(
            "more: {} at {}",
            total - seen.len(),
            osv_list_url(group)
        ));
    }
    if group.fix_candidates.len() > 1 {
        let all: Vec<String> = group
            .fix_candidates
            .iter()
            .map(|v| short_version(v))
            .collect();
        lines.push(format!("Fix candidates: {}", all.join(", ")));
    }
    limit(&lines.join("\n"))
}

/// The summary cut to `MAX_SUMMARY_CHARS`. A cut that leaves a backtick
/// unclosed loses that backtick, so the rest of the line is not code.
fn summary_head(summary: &str) -> String {
    let mut s = truncate(summary, MAX_SUMMARY_CHARS);
    if s.matches('`').count() % 2 == 1 {
        if let Some(i) = s.rfind('`') {
            s.remove(i);
        }
    }
    s
}

/// osv.dev's search for the package, with the query encoded.
fn osv_list_url(group: &FindingGroup) -> String {
    url::Url::parse_with_params(
        "https://osv.dev/list",
        [
            ("q", group.package.as_str()),
            ("ecosystem", group.ecosystem.as_str()),
        ],
    )
    .map(String::from)
    .unwrap_or_else(|_| "https://osv.dev/list".to_string())
}

/// The thread reply for a group the scanner stopped reporting.
pub fn render_gone(package: &str) -> String {
    limit(&format!("**Resolved:** {package} is no longer reported."))
}

/// The thread reply for a group that was with the fixer but whose new
/// installed version is still reported.
pub fn render_reopened(group: &FindingGroup) -> String {
    let fix = match fix(group) {
        Some(v) => format!("fix {v}"),
        None => "no fix released".to_string(),
    };
    limit(&format!(
        "**Reopened:** installed {} still has {} ({fix}). Reply `{MENTION} approve` to hand it to the fixer again.",
        short_version(&group.version),
        advisories(group.vuln_ids().len())
    ))
}

/// The thread reply for a resolved group that the scanner reports again.
pub fn render_reappeared(group: &FindingGroup) -> String {
    let fix = match fix(group) {
        Some(v) => format!("fix {v}"),
        None => "no fix released".to_string(),
    };
    limit(&format!(
        "**Reappeared:** {} {} is reported again ({}, {fix}). Reply `{MENTION} approve` to hand it to the fixer.",
        group.package,
        short_version(&group.version),
        advisories(group.vuln_ids().len())
    ))
}

/// The task for the fixer agent, posted in the finding's thread. No mention:
/// `send_targeted_message` adds the target's own. `repo_path` is the
/// configured repo URL or local directory; the lockfile is the first
/// finding's manifest, relative to the repo root. When the advisories list
/// more than one fixed version, the others are named too, because the
/// highest can be a release the registry has deprecated. Versions are given
/// in full: the fixer needs them exact.
pub fn render_handoff(group: &FindingGroup, repo_path: &str) -> String {
    let lockfile = group
        .findings
        .first()
        .map_or("unknown", |f| f.source_path.as_str());
    let others: Vec<&str> = group
        .fix_candidates
        .iter()
        .map(String::as_str)
        .filter(|v| Some(*v) != group.highest_fixed.as_deref())
        .collect();
    let (other_versions, prefer) = if others.is_empty() {
        (String::new(), "")
    } else {
        (
            format!(", other fix versions: {}", others.join(", ")),
            " If the registry marks the fix version as deprecated, use the highest other fix version that is not.",
        )
    };
    format!(
        "Please fix: repo {repo_path}, ecosystem {}, package {}, current {}, fix {}{other_versions}, lockfile {lockfile}.{prefer} Reply in this thread when the branch is pushed.",
        group.ecosystem,
        group.package,
        group.version,
        group
            .highest_fixed
            .as_deref()
            .unwrap_or("no fixed version published"),
    )
}

/// One line for a thread whose group changed since it was posted: what
/// changed, then the installed and fix versions now.
pub fn render_update(old: &FindingGroup, new: &FindingGroup) -> String {
    let old_ids = old.vuln_ids();
    let new_ids = new.vuln_ids();
    let added: Vec<&String> = new_ids.iter().filter(|id| !old_ids.contains(id)).collect();
    let removed = old_ids.iter().filter(|id| !new_ids.contains(id)).count();
    let mut parts = Vec::new();
    if !added.is_empty() {
        let shown: Vec<String> = added.iter().take(5).map(|id| format!("`{id}`")).collect();
        let more = added.len() - shown.len();
        let tail = if more > 0 {
            format!(", more: {more}")
        } else {
            String::new()
        };
        parts.push(format!(
            "{} added ({}{tail})",
            advisories(added.len()),
            shown.join(", ")
        ));
    }
    if removed > 0 {
        parts.push(format!("{} no longer listed", advisories(removed)));
    }
    if new.version != old.version {
        parts.push("installed version changed".into());
    }
    if new.highest_fixed != old.highest_fixed {
        parts.push("fix version changed".into());
    }
    if parts.is_empty() {
        parts.push("advisory details changed".into());
    }
    let fix = fix(new).unwrap_or_else(|| "none released".into());
    limit(&format!(
        "**Update:** {}; installed {}; fix {fix}.",
        parts.join("; "),
        short_version(&new.version)
    ))
}

/// For the `status` command: open groups, then rejected ones under their
/// own header, each sorted by severity (highest first), then package. When
/// the whole list does not fit one message, the first 15 rows are kept and
/// the rest counted.
pub fn render_status(open: &[GroupState], rejected: &[GroupState], multi_repo: bool) -> String {
    let rows = |groups: &[GroupState]| -> Vec<String> {
        let mut sorted: Vec<&GroupState> = groups.iter().collect();
        sorted.sort_by(|a, b| {
            b.group
                .max_severity
                .unwrap_or(-1.0)
                .total_cmp(&a.group.max_severity.unwrap_or(-1.0))
                .then_with(|| a.group.package.cmp(&b.group.package))
                .then_with(|| a.group.repo.cmp(&b.group.repo))
        });
        sorted
            .into_iter()
            .map(|g| status_row(g, multi_repo))
            .collect()
    };
    let (open_rows, rejected_rows) = (rows(open), rows(rejected));
    let total = open_rows.len() + rejected_rows.len();
    let build = |keep: usize| -> String {
        let mut lines = vec![format!("**Open findings: {}**", open_rows.len())];
        lines.extend(open_rows.iter().take(keep).cloned());
        let rejected_kept = keep
            .saturating_sub(open_rows.len())
            .min(rejected_rows.len());
        if rejected_kept > 0 {
            lines.push(format!("**Rejected ({})**", rejected_rows.len()));
            lines.extend(rejected_rows.iter().take(rejected_kept).cloned());
        }
        let shown = keep.min(total);
        if shown < total {
            lines.push(format!("and {} more", total - shown));
        }
        lines.join("\n")
    };
    let full = build(total);
    if full.chars().count() < MESSAGE_LIMIT {
        return full;
    }
    // Fifteen rows, or fewer if long names still push it over.
    for keep in (0..=MAX_STATUS_ROWS.min(total)).rev() {
        let text = build(keep);
        if text.chars().count() < MESSAGE_LIMIT {
            return text;
        }
    }
    limit(&build(0))
}

fn status_row(g: &GroupState, multi_repo: bool) -> String {
    let repo = if multi_repo {
        format!("{} · ", g.group.repo)
    } else {
        String::new()
    };
    let fix = fix(&g.group).unwrap_or_else(|| "no fix".into());
    let status = match g.status {
        Status::PrOpen => "PR open".to_string(),
        Status::Snoozed => match g.snoozed_until {
            Some(t) => format!("snoozed until {}", t.format("%Y-%m-%d")),
            None => "snoozed".to_string(),
        },
        s => s.as_str().to_string(),
    };
    format!(
        "{repo}`{}` {} → {fix} · **{}** · {} adv · {status}",
        g.group.package,
        short_version(&g.group.version),
        severity(g.group.max_severity),
        g.group.vuln_ids().len(),
    )
}

/// The line posted when serve starts.
pub fn render_online(repos: usize, next_scan: DateTime<Utc>) -> String {
    format!(
        "Baba-Yaga online · watching {} · next scan {}",
        count(repos, "repo", "repos"),
        next_scan.format("%Y-%m-%d %H:%M UTC")
    )
}

pub fn render_help() -> String {
    format!(
        "**Commands** (mention {MENTION}):\n\
         `approve` — in a finding's thread, hand it to the fixer (room members only)\n\
         `reject` — in a finding's thread, close it as not needed (room members only)\n\
         `snooze 30d` or `snooze 2w` — in a finding's thread, hide it for a while\n\
         `status` — list open findings\n\
         `scan` or `scan <repo>` — scan now\n\
         `help` — this list"
    )
}

/// `1 repo`, `2 repos`.
pub fn count(n: usize, one: &str, many: &str) -> String {
    if n == 1 {
        format!("1 {one}")
    } else {
        format!("{n} {many}")
    }
}

fn advisories(n: usize) -> String {
    count(n, "advisory", "advisories")
}

/// At most `max` characters, ending in "…" when cut. Cuts on a char
/// boundary, never inside a multi-byte character.
pub fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let kept: String = s.chars().take(max.saturating_sub(1)).collect();
    format!("{}…", kept.trim_end())
}

/// Under `MESSAGE_LIMIT` characters.
fn limit(s: &str) -> String {
    truncate(s, MESSAGE_LIMIT - 1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scanner::{group, Finding};

    fn finding(id: &str, score: Option<f32>, summary: &str) -> Finding {
        Finding {
            repo: "samples".into(),
            ecosystem: "npm".into(),
            package: "axios".into(),
            version: "0.21.1".into(),
            id: id.into(),
            aliases: vec![],
            summary: Some(summary.into()),
            severity_score: score,
            fixed_version: Some("1.18.0".into()),
            reachable: None,
            source_path: "node/package-lock.json".into(),
        }
    }

    fn axios() -> FindingGroup {
        group(vec![
            finding("GHSA-a", Some(7.5), "Server-Side Request Forgery"),
            finding("GHSA-b", Some(5.3), "Inefficient Regular Expression"),
        ])
        .remove(0)
    }

    #[test]
    fn root_single_repo() {
        assert_eq!(
            render_group(&axios(), false),
            "axios 0.21.1 → 1.18.0\n\
             **HIGH 7.5** · 2 advisories · reachable: unknown\n\
             npm · `node/package-lock.json`\n\
             Reply here: `@baba_yaga approve` · `snooze 30d` · `reject`"
        );
    }

    #[test]
    fn root_multi_repo_names_the_repo() {
        let mut f = finding("GHSA-a", Some(9.8), "Prototype Pollution");
        f.reachable = Some(true);
        let g = group(vec![f]).remove(0);
        assert_eq!(
            render_group(&g, true),
            "axios 0.21.1 → 1.18.0\n\
             **CRITICAL 9.8** · 1 advisory · reachable: yes\n\
             samples · npm · `node/package-lock.json`\n\
             Reply here: `@baba_yaga approve` · `snooze 30d` · `reject`"
        );
    }

    #[test]
    fn root_without_fix_or_score() {
        let mut f = finding("GHSA-a", None, "s");
        f.fixed_version = None;
        let g = group(vec![f]).remove(0);
        let text = render_group(&g, false);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines[0], "axios 0.21.1, no fix released");
        assert_eq!(lines[1], "**UNSCORED** · 1 advisory · reachable: unknown");
    }

    #[test]
    fn root_shortens_go_pseudo_versions() {
        let mut f = finding("GO-2022-0969", Some(7.5), "s");
        f.ecosystem = "Go".into();
        f.package = "golang.org/x/net".into();
        f.version = "0.0.0-20190620200207-3b0461eec859".into();
        f.fixed_version = Some("0.7.0".into());
        f.reachable = Some(false);
        f.source_path = "go/go.mod".into();
        let g = group(vec![f]).remove(0);
        assert_eq!(
            render_group(&g, false),
            "golang.org/x/net 0.0.0-20190620200207… → 0.7.0\n\
             **HIGH 7.5** · 1 advisory · reachable: no\n\
             Go · `go/go.mod`\n\
             Reply here: `@baba_yaga approve` · `snooze 30d` · `reject`"
        );
        assert_eq!(short_version("1.2.3"), "1.2.3");
        assert_eq!(short_version(&"9".repeat(24)), "9".repeat(24));
        assert_eq!(
            short_version(&"9".repeat(25)),
            format!("{}…", "9".repeat(20))
        );
    }

    #[test]
    fn root_limits_hold_for_huge_groups() {
        let long = "x".repeat(400);
        let findings: Vec<Finding> = (0..30)
            .map(|i| {
                let mut f = finding(&format!("GHSA-{i:04}"), Some(9.0), &long);
                f.source_path = format!("dir{i}/{}/package-lock.json", "d".repeat(80));
                f
            })
            .collect();
        let g = group(findings).remove(0);
        for multi in [false, true] {
            let text = render_group(&g, multi);
            assert!(text.lines().count() <= ROOT_LINES, "{text}");
            assert!(text.chars().count() < MESSAGE_LIMIT);
        }
        assert!(render_details(&g).chars().count() < MESSAGE_LIMIT);
        assert!(render_group(&axios(), false).lines().count() == ROOT_LINES);
    }

    #[test]
    fn details_list_five_and_link_the_rest() {
        let mut findings: Vec<Finding> = (0..6)
            .map(|i| {
                finding(
                    &format!("GHSA-{i}"),
                    Some(4.0 + i as f32),
                    &format!("Summary {i}"),
                )
            })
            .collect();
        findings[0].summary = Some("y".repeat(120));
        findings[1].fixed_version = Some("1.17.0".into());
        let g = group(findings).remove(0);
        assert_eq!(
            render_details(&g),
            "**Advisories (6)**\n\
                 - `GHSA-5` CRITICAL 9.0: Summary 5\n\
                 - `GHSA-4` HIGH 8.0: Summary 4\n\
                 - `GHSA-3` HIGH 7.0: Summary 3\n\
                 - `GHSA-2` MEDIUM 6.0: Summary 2\n\
                 - `GHSA-1` MEDIUM 5.0: Summary 1\n\
                 more: 1 at https://osv.dev/list?q=axios&ecosystem=npm\n\
                 Fix candidates: 1.17.0, 1.18.0"
        );
    }

    #[test]
    fn details_short_group_and_long_summary() {
        let g = group(vec![finding("GHSA-a", None, &"y".repeat(120))]).remove(0);
        let text = render_details(&g);
        assert_eq!(
            text,
            format!(
                "**Advisories (1)**\n- `GHSA-a` UNSCORED: {}…",
                "y".repeat(89)
            )
        );
    }

    fn stored(package: &str, score: Option<f32>, status: Status) -> GroupState {
        let mut f = finding("GHSA-a", score, "s");
        f.package = package.into();
        let t = chrono::DateTime::UNIX_EPOCH;
        GroupState {
            key: format!("samples:npm:{package}"),
            group: group(vec![f]).remove(0),
            thread_id: None,
            status,
            snoozed_until: None,
            first_seen: t,
            last_seen: t,
            last_posted: None,
            hint_posted: true,
            pr_url: None,
        }
    }

    #[test]
    fn cut_summary_closes_no_code_span() {
        let cut = summary_head(&format!("{} `_.unset`, `_.omit_and_more`", "z".repeat(70)));
        assert_eq!(cut.matches('`').count() % 2, 0, "{cut}");
        assert!(cut.ends_with('…'));
        assert_eq!(summary_head("a `b` c"), "a `b` c");
    }

    #[test]
    fn status_orders_by_severity_then_package() {
        let open = [
            stored("minimist", Some(5.6), Status::Open),
            stored("lodash", Some(9.1), Status::PrOpen),
            stored("axios", Some(5.6), Status::Assigned),
            stored("jinja2", None, Status::Approved),
        ];
        let rejected = [stored("urllib3", Some(8.1), Status::Rejected)];
        assert_eq!(
            render_status(&open, &rejected, false),
            "**Open findings: 4**\n\
             `lodash` 0.21.1 → 1.18.0 · **CRITICAL 9.1** · 1 adv · PR open\n\
             `axios` 0.21.1 → 1.18.0 · **MEDIUM 5.6** · 1 adv · assigned\n\
             `minimist` 0.21.1 → 1.18.0 · **MEDIUM 5.6** · 1 adv · open\n\
             `jinja2` 0.21.1 → 1.18.0 · **UNSCORED** · 1 adv · approved\n\
             **Rejected (1)**\n\
             `urllib3` 0.21.1 → 1.18.0 · **HIGH 8.1** · 1 adv · rejected"
        );
        assert_eq!(render_status(&[], &[], false), "**Open findings: 0**");
        let mut snoozed = stored("axios", Some(7.5), Status::Snoozed);
        snoozed.snoozed_until = Some(chrono::DateTime::UNIX_EPOCH);
        assert_eq!(
            render_status(&[snoozed], &[], true),
            "**Open findings: 1**\n\
             samples · `axios` 0.21.1 → 1.18.0 · **HIGH 7.5** · 1 adv · snoozed until 1970-01-01"
        );
    }

    #[test]
    fn status_truncates_to_fifteen_rows() {
        let open: Vec<GroupState> = (0..40)
            .map(|i| {
                stored(
                    &format!("package-with-a-long-name-{i:02}"),
                    Some(5.0),
                    Status::Open,
                )
            })
            .collect();
        let rejected = [stored("urllib3", Some(8.1), Status::Rejected)];
        let text = render_status(&open, &rejected, false);
        assert!(text.chars().count() < MESSAGE_LIMIT);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines[0], "**Open findings: 40**");
        assert_eq!(lines.len(), 1 + 15 + 1);
        assert!(lines[1].starts_with("`package-with-a-long-name-00`"));
        assert_eq!(lines[16], "and 26 more");
        assert!(!text.contains("Rejected"));
    }

    #[test]
    fn short_messages() {
        assert_eq!(
            render_gone("lodash"),
            "**Resolved:** lodash is no longer reported."
        );
        assert_eq!(
            render_reopened(&axios()),
            "**Reopened:** installed 0.21.1 still has 2 advisories (fix 1.18.0). \
             Reply `@baba_yaga approve` to hand it to the fixer again."
        );
        let next = chrono::TimeZone::with_ymd_and_hms(&Utc, 2026, 11, 7, 6, 0, 0).unwrap();
        assert_eq!(
            render_online(1, next),
            "Baba-Yaga online · watching 1 repo · next scan 2026-11-07 06:00 UTC"
        );
        let help = render_help();
        assert!(help.starts_with("**Commands** (mention @baba_yaga):\n`approve` — "));
        assert_eq!(help.lines().count(), 7);
    }

    #[test]
    fn update_says_what_changed_and_the_versions_now() {
        let old = group(vec![finding("GHSA-a", Some(7.5), "s")]).remove(0);
        let mut f = finding("GHSA-b", Some(7.5), "s");
        f.fixed_version = Some("1.19.0".into());
        let new = group(vec![finding("GHSA-a", Some(7.5), "s"), f]).remove(0);
        assert_eq!(
            render_update(&old, &new),
            "**Update:** 1 advisory added (`GHSA-b`); fix version changed; installed 0.21.1; fix 1.19.0."
        );
        assert_eq!(
            render_update(&new, &old),
            "**Update:** 1 advisory no longer listed; fix version changed; installed 0.21.1; fix 1.18.0."
        );
    }

    #[test]
    fn count_is_singular_only_for_one() {
        assert_eq!(count(0, "repo", "repos"), "0 repos");
        assert_eq!(count(1, "repo", "repos"), "1 repo");
        assert_eq!(count(2, "repo", "repos"), "2 repos");
    }

    #[test]
    fn handoff_text() {
        let g = group(vec![finding("GHSA-a", Some(7.5), "s")]).remove(0);
        assert_eq!(
            render_handoff(&g, "/work/samples"),
            "Please fix: repo /work/samples, ecosystem npm, package axios, \
             current 0.21.1, fix 1.18.0, lockfile node/package-lock.json. \
             Reply in this thread when the branch is pushed."
        );
    }

    #[test]
    fn handoff_names_other_fix_versions() {
        let mut a = finding("GHSA-a", Some(7.5), "s");
        let mut b = finding("GHSA-b", Some(7.5), "s");
        let mut c = finding("GHSA-c", Some(7.5), "s");
        a.fixed_version = Some("4.18.0".into());
        b.fixed_version = Some("4.17.21".into());
        c.fixed_version = Some("4.17.12".into());
        let g = group(vec![a, b, c]).remove(0);
        assert_eq!(
            render_handoff(&g, "/w"),
            "Please fix: repo /w, ecosystem npm, package axios, current 0.21.1, \
             fix 4.18.0, other fix versions: 4.17.12, 4.17.21, lockfile node/package-lock.json. \
             If the registry marks the fix version as deprecated, use the highest other fix \
             version that is not. Reply in this thread when the branch is pushed."
        );
    }

    #[test]
    fn truncate_is_char_safe() {
        assert_eq!(truncate("abc", 5), "abc");
        assert_eq!(truncate("abcdefgh", 6), "abcde…");
        assert_eq!(truncate("ååååååå", 5), "åååå…");
    }

    #[test]
    fn severity_bands() {
        assert_eq!(severity_label(Some(10.0)), "CRITICAL");
        assert_eq!(severity_label(Some(7.0)), "HIGH");
        assert_eq!(severity_label(Some(6.9)), "MEDIUM");
        assert_eq!(severity_label(Some(0.1)), "LOW");
        assert_eq!(severity_label(Some(0.0)), "NONE");
    }
}
