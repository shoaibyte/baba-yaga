//! `baba-yaga.toml`: which room to report to, when to scan, and what.

use std::path::{Path, PathBuf};
use std::str::FromStr;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// Switch room id the patrol reports to.
    pub room: String,
    /// Cron expression with a seconds field, as the `cron` crate expects:
    /// `sec min hour day-of-month month day-of-week [year]`. Evaluated in UTC.
    pub schedule: String,
    /// SQLite database holding finding state.
    pub state_path: PathBuf,
    #[serde(default)]
    pub repos: Vec<Repo>,
    /// Drop findings below this CVSS score. Unscored findings are kept.
    #[serde(default)]
    pub min_severity: Option<f32>,
    /// Run osv-scanner's Go call analysis to fill in `reachable`. Skipped,
    /// with a log line, when `go` is not on PATH.
    #[serde(default = "default_true")]
    pub call_analysis: bool,
    /// Agent name (as in `list_participants`) that approved findings are
    /// handed to with a targeted message. Unset: approvals send nothing.
    #[serde(default)]
    pub fixer_agent: Option<String>,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Repo {
    pub name: String,
    /// Git URL, or a local directory to scan in place.
    pub url: String,
    #[serde(default)]
    pub branch: Option<String>,
}

impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        Self::parse(&text).with_context(|| format!("invalid config in {}", path.display()))
    }

    pub fn parse(text: &str) -> Result<Self> {
        let config: Config = toml::from_str(text)?;
        cron::Schedule::from_str(&config.schedule)
            .with_context(|| format!("invalid schedule {:?}", config.schedule))?;
        Ok(config)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn example_config_parses() {
        let config = Config::parse(include_str!("../../../baba-yaga.example.toml")).unwrap();
        assert_eq!(config.repos.len(), 2);
        assert_eq!(config.repos[0].branch.as_deref(), Some("main"));
        assert_eq!(config.repos[1].branch, None);
        assert_eq!(config.min_severity, Some(7.0));
        assert!(config.call_analysis);
        assert_eq!(config.fixer_agent.as_deref(), Some("patrol-fixer"));
    }

    #[test]
    fn call_analysis_defaults_on() {
        let text = r#"
            room = "!r:switch"
            schedule = "0 0 7 * * *"
            state_path = "x.db"
        "#;
        let config = Config::parse(text).unwrap();
        assert!(config.call_analysis);
        assert_eq!(config.fixer_agent, None);
    }

    #[test]
    fn bad_schedule_is_rejected() {
        let text = r#"
            room = "!r:switch"
            schedule = "every morning"
            state_path = "x.db"
        "#;
        assert!(Config::parse(text).is_err());
    }
}
