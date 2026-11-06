//! `bay init` and `bay doctor`: first-time setup and its checks.

pub mod api;
pub mod discover;
pub mod doctor;
pub mod envfile;
pub mod init;
pub mod prompt;

use std::path::Path;

use anyhow::{bail, Context, Result};

use crate::config::{Config, Repo};

/// The `.env` variables `init` writes.
pub const SWITCH_URL: &str = "SWITCH_URL";
pub const SWITCH_AGENT_ID: &str = "SWITCH_AGENT_ID";
pub const SWITCH_AGENT_KEY: &str = "SWITCH_AGENT_KEY";
pub const SWITCH_REGISTRATION_KEY: &str = "SWITCH_REGISTRATION_KEY";
pub const SWITCH_ROOM_ID: &str = "SWITCH_ROOM_ID";

pub const DEFAULT_AGENT_NAME: &str = "baba-yaga";
pub const DEFAULT_FIXER: &str = "patrol-fixer";
/// Daily at 02:00 UTC.
pub const DEFAULT_SCHEDULE: &str = "0 0 2 * * *";

/// How Discord addresses an agent: the server stores an API-registered
/// name with underscores for hyphens (`baba-yaga` becomes `baba_yaga`).
pub fn discord_handle(name: &str) -> String {
    name.replace('-', "_")
}

/// A repo from what a person typed: a git URL as is, or a local directory,
/// which must exist and is stored as an absolute path. The name is the last
/// path segment without `.git`.
pub fn repo_from_input(input: &str) -> Result<Repo> {
    let input = input.trim();
    let is_url = input.contains("://") || input.starts_with("git@");
    let url = if is_url {
        input.to_string()
    } else {
        let path = Path::new(input);
        if !path.is_dir() {
            bail!("{input} is not a directory");
        }
        path.canonicalize()
            .with_context(|| format!("failed to resolve {input}"))?
            .to_string_lossy()
            .into_owned()
    };
    let name = url
        .trim_end_matches('/')
        .rsplit(['/', ':'])
        .next()
        .unwrap_or_default()
        .trim_end_matches(".git")
        .to_string();
    if name.is_empty() {
        bail!("cannot name a repo from {input}");
    }
    Ok(Repo {
        name,
        url,
        branch: None,
    })
}

/// Make repo names unique by suffixing `-2`, `-3`, ... in order.
pub fn unique_names(repos: &mut [Repo]) {
    let mut seen: Vec<String> = Vec::new();
    for r in repos.iter_mut() {
        let base = r.name.clone();
        let mut n = 2;
        while seen.contains(&r.name) {
            r.name = format!("{base}-{n}");
            n += 1;
        }
        seen.push(r.name.clone());
    }
}

/// `baba-yaga.toml` text, laid out like `baba-yaga.example.toml` with its
/// comments. `min_severity` is left commented out, so every finding is
/// reported. The result always passes `Config::parse`.
pub fn config_text(room: &str, fixer: &str, schedule: &str, repos: &[Repo]) -> Result<String> {
    let q = |s: &str| toml::Value::String(s.to_string()).to_string();
    let mut t = String::new();
    t.push_str("# Written by `bay init`. See baba-yaga.example.toml for every field.\n\n");
    t.push_str("# Switch room id the patrol reports to. SWITCH_ROOM_ID in .env wins over it.\n");
    t.push_str(&format!("room = {}\n\n", q(room)));
    t.push_str("# Cron with a seconds field: sec min hour day-of-month month day-of-week.\n");
    t.push_str("# Evaluated in UTC.\n");
    t.push_str(&format!("schedule = {}\n\n", q(schedule)));
    t.push_str("state_path = \"baba-yaga.db\"\n\n");
    t.push_str("# Drop findings below this CVSS score. Findings without a score are kept.\n");
    t.push_str("# min_severity = 7.0\n\n");
    t.push_str("# Go reachability via osv-scanner call analysis (default true). Needs `go` on\n");
    t.push_str("# PATH and a module that builds; skipped with a log line otherwise.\n");
    t.push_str("call_analysis = true\n\n");
    t.push_str("# Agent that approved findings are handed to with a targeted message in the\n");
    t.push_str("# finding's thread (its name in the room, which is also its Discord mention).\n");
    t.push_str(&format!("fixer_agent = {}\n", q(fixer)));
    for r in repos {
        t.push_str("\n[[repos]]\n");
        t.push_str(&format!("name = {}\n", q(&r.name)));
        t.push_str(&format!("url = {}\n", q(&r.url)));
        if let Some(b) = &r.branch {
            t.push_str(&format!("branch = {}\n", q(b)));
        }
    }
    Config::parse(&t).context("generated config does not parse")?;
    Ok(t)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discord_handle_uses_underscores() {
        assert_eq!(discord_handle("baba-yaga"), "baba_yaga");
        assert_eq!(discord_handle("baba_yaga"), "baba_yaga");
        assert_eq!(discord_handle("a-b-c"), "a_b_c");
    }

    #[test]
    fn repo_input_url_and_path() {
        let r = repo_from_input("https://github.com/o/api.git").unwrap();
        assert_eq!(
            (r.name.as_str(), r.url.as_str()),
            ("api", "https://github.com/o/api.git")
        );
        assert_eq!(
            repo_from_input("git@github.com:o/web.git").unwrap().name,
            "web"
        );
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("samples");
        std::fs::create_dir(&sub).unwrap();
        let r = repo_from_input(sub.to_str().unwrap()).unwrap();
        assert_eq!(r.name, "samples");
        assert!(Path::new(&r.url).is_absolute());
        assert!(repo_from_input(dir.path().join("missing").to_str().unwrap()).is_err());
    }

    #[test]
    fn config_writer_round_trips() {
        let mut repos = vec![
            repo_from_input("https://github.com/o/api.git").unwrap(),
            repo_from_input("https://gitlab.com/x/api").unwrap(),
            Repo {
                name: "q\"uote".into(),
                url: "/tmp/with \"quotes\"".into(),
                branch: Some("main".into()),
            },
        ];
        unique_names(&mut repos);
        let text = config_text("room-1", "patrol-fixer", DEFAULT_SCHEDULE, &repos).unwrap();
        let c = Config::parse(&text).unwrap();
        assert_eq!(c.room, "room-1");
        assert_eq!(c.schedule, "0 0 2 * * *");
        assert_eq!(c.fixer_agent.as_deref(), Some("patrol-fixer"));
        assert_eq!(c.min_severity, None);
        assert!(c.call_analysis);
        let names: Vec<&str> = c.repos.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, ["api", "api-2", "q\"uote"]);
        assert_eq!(c.repos[2].url, "/tmp/with \"quotes\"");
        assert_eq!(c.repos[2].branch.as_deref(), Some("main"));
        assert!(text.contains("# Cron with a seconds field"));
    }

    #[test]
    fn config_writer_rejects_a_bad_schedule() {
        assert!(config_text("r", "f", "every day", &[]).is_err());
    }
}
