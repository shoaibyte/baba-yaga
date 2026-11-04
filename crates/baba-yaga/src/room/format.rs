//! Plain-text rendering for the room. No emoji, no markdown tables: Discord
//! renders those badly, and the bridge passes text through as is.

use crate::scanner::finding::FindingGroup;
use crate::state::GroupState;

/// How the agent is addressed on Discord.
pub const MENTION: &str = "@baba_yaga";
/// Thread openers stay under this many characters.
pub const MAX_GROUP_CHARS: usize = 1500;
/// Discord rejects messages over 2000 characters; keep a margin.
pub const MAX_MESSAGE_CHARS: usize = 1900;
const MAX_LISTED: usize = 8;
const MAX_SUMMARY_CHARS: usize = 100;

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

fn severity(score: Option<f32>) -> String {
    match score {
        Some(s) => format!("{} {s:.1}", severity_label(Some(s))),
        None => severity_label(None).to_string(),
    }
}

fn fix(group: &FindingGroup) -> &str {
    group
        .highest_fixed
        .as_deref()
        .unwrap_or("no fixed version published")
}

/// The first message of a group's thread, under `MAX_GROUP_CHARS`:
/// header, severity and reachability, up to eight advisories, and a "more"
/// line when some are left out. The command hint follows as the first reply
/// (`render_hint`).
pub fn render_group(group: &FindingGroup) -> String {
    let ids = group.vuln_ids();
    // Drop listed advisories until the text fits; the header stays.
    for listed in (0..=MAX_LISTED.min(ids.len())).rev() {
        let text = group_text(group, listed);
        if text.chars().count() <= MAX_GROUP_CHARS {
            return text;
        }
    }
    truncate(&group_text(group, 0), MAX_GROUP_CHARS)
}

fn group_text(group: &FindingGroup, listed: usize) -> String {
    let reachable = match group.any_reachable {
        Some(true) => "reachable: yes, vulnerable code is called",
        Some(false) => "reachable: no, vulnerable code is not called",
        None => "reachable: unknown",
    };
    let mut sources: Vec<&str> = group
        .findings
        .iter()
        .map(|f| f.source_path.as_str())
        .collect();
    sources.sort_unstable();
    sources.dedup();

    let mut lines = vec![
        format!(
            "{} / {} {} -> fix: {}",
            group.repo,
            group.package,
            group.version,
            fix(group)
        ),
        format!(
            "{}, {}, {}, {}, in {}",
            group.ecosystem,
            severity(group.max_severity),
            reachable,
            advisories(group.vuln_ids().len()),
            sources.join(", ")
        ),
    ];
    // Findings are sorted highest severity first; list each id once.
    let mut seen = Vec::new();
    for f in &group.findings {
        if seen.len() == listed {
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
            "- {} ({}): {}",
            f.id,
            severity(f.severity_score),
            truncate(summary, MAX_SUMMARY_CHARS)
        ));
    }
    let total = group.vuln_ids().len();
    if total > seen.len() {
        lines.push(format!("more: {}", total - seen.len()));
    }
    lines.join("\n")
}

/// The first reply in a group's thread. Discord shows a thread only once a
/// message is posted in it, so this reply is what gives people a thread to
/// answer in.
pub fn render_hint() -> String {
    format!("Commands here: {MENTION} approve, {MENTION} snooze 30d, {MENTION} status")
}

/// One line for a thread whose group changed since it was posted.
pub fn render_update(old: &FindingGroup, new: &FindingGroup) -> String {
    let old_ids = old.vuln_ids();
    let added: Vec<String> = new
        .vuln_ids()
        .into_iter()
        .filter(|id| !old_ids.contains(id))
        .collect();
    let mut parts = Vec::new();
    if !added.is_empty() {
        let shown: Vec<&str> = added.iter().take(5).map(String::as_str).collect();
        let more = added.len().saturating_sub(shown.len());
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
    if new.version != old.version {
        parts.push(format!("installed version now {}", new.version));
    }
    if new.highest_fixed != old.highest_fixed {
        parts.push(format!("fix now {}", fix(new)));
    }
    if parts.is_empty() {
        parts.push("no visible change".into());
    }
    truncate(&format!("Update: {}.", parts.join("; ")), MAX_MESSAGE_CHARS)
}

/// Every open group, one line each, for the `status` command.
pub fn render_status(groups: &[GroupState]) -> String {
    if groups.is_empty() {
        return "No open findings.".into();
    }
    let mut lines = vec![format!("Open findings: {} packages", groups.len())];
    let mut used = lines[0].len();
    for (i, g) in groups.iter().enumerate() {
        let line = format!(
            "{} / {} {}: {}, {}, {}, fix {}",
            g.group.repo,
            g.group.package,
            g.group.version,
            g.status.as_str(),
            advisories(g.group.vuln_ids().len()),
            severity(g.group.max_severity),
            fix(&g.group)
        );
        // Room for this line plus a possible "more" line.
        if used + line.len() + 1 + 20 > MAX_MESSAGE_CHARS {
            lines.push(format!("more: {}", groups.len() - i));
            break;
        }
        used += line.len() + 1;
        lines.push(line);
    }
    lines.join("\n")
}

pub fn render_help() -> String {
    format!(
        "Commands (mention {MENTION}):\n\
         approve: in a finding's thread, hand it to the fixer (room members only)\n\
         reject: in a finding's thread, close it as not needed (room members only)\n\
         snooze 30d or snooze 2w: in a finding's thread, hide it for a while\n\
         status: list open findings\n\
         scan or scan <repo>: scan now\n\
         help: this list"
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

/// At most `max` characters, ending in "..." when cut. Cuts on a char
/// boundary, never inside a multi-byte character.
pub fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let kept: String = s.chars().take(max.saturating_sub(3)).collect();
    format!("{}...", kept.trim_end())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scanner::{group, Finding};

    fn finding(id: &str, score: f32, summary: &str) -> Finding {
        Finding {
            repo: "samples".into(),
            ecosystem: "npm".into(),
            package: "axios".into(),
            version: "0.21.1".into(),
            id: id.into(),
            aliases: vec![],
            summary: Some(summary.into()),
            severity_score: Some(score),
            fixed_version: Some("1.18.0".into()),
            reachable: None,
            source_path: "node/package-lock.json".into(),
        }
    }

    #[test]
    fn single_advisory_is_singular() {
        let g = group(vec![finding("GHSA-a", 9.8, "Prototype Pollution")]).remove(0);
        assert!(render_group(&g).contains(", 1 advisory, in "));
    }

    #[test]
    fn small_group_renders_fully() {
        let g = group(vec![
            finding("GHSA-a", 7.5, "Server-Side Request Forgery"),
            finding("GHSA-b", 5.3, "Inefficient Regular Expression"),
        ])
        .remove(0);
        assert_eq!(
            render_group(&g),
            "samples / axios 0.21.1 -> fix: 1.18.0\n\
             npm, HIGH 7.5, reachable: unknown, 2 advisories, in node/package-lock.json\n\
             - GHSA-a (HIGH 7.5): Server-Side Request Forgery\n\
             - GHSA-b (MEDIUM 5.3): Inefficient Regular Expression"
        );
    }

    #[test]
    fn count_is_singular_only_for_one() {
        assert_eq!(count(0, "repo", "repos"), "0 repos");
        assert_eq!(count(1, "repo", "repos"), "1 repo");
        assert_eq!(count(2, "repo", "repos"), "2 repos");
    }

    #[test]
    fn hint_text() {
        assert_eq!(
            render_hint(),
            "Commands here: @baba_yaga approve, @baba_yaga snooze 30d, @baba_yaga status"
        );
    }

    #[test]
    fn thirty_findings_stay_under_limit_and_say_more() {
        let long = "x".repeat(400);
        let findings: Vec<Finding> = (0..30)
            .map(|i| finding(&format!("GHSA-{i:04}-long-identifier"), 9.0, &long))
            .collect();
        let g = group(findings).remove(0);
        let text = render_group(&g);
        assert!(text.chars().count() <= MAX_GROUP_CHARS, "{}", text.len());
        assert!(text.ends_with("\nmore: 22"), "{text}");
        assert_eq!(text.lines().filter(|l| l.starts_with("- ")).count(), 8);
        // Each summary is cut at 100 characters.
        let item = text.lines().find(|l| l.starts_with("- ")).unwrap();
        assert!(item.ends_with("..."));
        assert!(item.len() < 160);
        assert!(text.is_ascii());
    }

    #[test]
    fn update_names_new_ids_and_fix() {
        let old = group(vec![finding("GHSA-a", 7.5, "s")]).remove(0);
        let mut f = finding("GHSA-b", 7.5, "s");
        f.fixed_version = Some("1.19.0".into());
        let new = group(vec![finding("GHSA-a", 7.5, "s"), f]).remove(0);
        assert_eq!(
            render_update(&old, &new),
            "Update: 1 advisory added (GHSA-b); fix now 1.19.0."
        );
    }

    #[test]
    fn truncate_is_char_safe() {
        assert_eq!(truncate("abc", 5), "abc");
        assert_eq!(truncate("abcdefgh", 6), "abc...");
        assert_eq!(truncate("ååååååå", 5), "åå...");
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
