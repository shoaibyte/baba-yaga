//! baba-yaga: a Switch agent that patrols repositories for vulnerable
//! dependencies.

mod audit;
mod config;
mod room;
mod scanner;
mod scheduler;
mod state;
mod workflow;

use std::path::{Path, PathBuf};

use anyhow::{bail, Result};
use clap::{Parser, Subcommand};
use tracing_subscriber::EnvFilter;

use config::{Config, Repo};

#[derive(Debug, Parser)]
#[command(version, about)]
struct Cli {
    /// Path to the config file.
    #[arg(long, global = true, default_value = "baba-yaga.toml")]
    config: PathBuf,

    #[command(subcommand)]
    command: Cmd,
}

#[derive(Debug, Subcommand)]
enum Cmd {
    /// Connect to Switch and run the patrol on its schedule.
    Serve,
    /// Scan once, post each package with findings as a thread over HTTP
    /// (no socket), and exit. For CI. Groups already posted are skipped;
    /// updates to existing threads are left for `serve`, which can reply in
    /// threads.
    Report {
        /// Only scan the configured repo with this name.
        #[arg(long, conflicts_with = "path")]
        repo: Option<String>,
        /// Scan this local directory instead of the configured repos. The
        /// config file is then optional.
        #[arg(long)]
        path: Option<PathBuf>,
        /// Use no state file: post every group as a new thread.
        #[arg(long)]
        no_state: bool,
        /// Print the posts instead of sending them. Writes no state.
        #[arg(long)]
        dry_run: bool,
    },
    /// Run the scanner and print findings as JSON. Touches no state.
    Scan {
        /// Only scan the configured repo with this name.
        #[arg(long, conflicts_with = "path")]
        repo: Option<String>,
        /// Scan this local directory instead of the configured repos. The
        /// config file is then optional.
        #[arg(long)]
        path: Option<PathBuf>,
        /// Print one entry per (repo, ecosystem, package) instead of one per
        /// advisory.
        #[arg(long)]
        group: bool,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    // A missing .env is fine; the environment may already be set.
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .init();

    let cli = Cli::parse();
    match cli.command {
        Cmd::Serve => workflow::serve(&Config::load(&cli.config)?).await,
        Cmd::Report {
            repo,
            path,
            no_state,
            dry_run,
        } => {
            let t = targets(&cli.config, repo, path)?;
            let state_path = match (no_state || dry_run, &t.config) {
                (true, _) => None,
                (false, Some(c)) => Some(c.state_path.clone()),
                (false, None) => Some(PathBuf::from(DEFAULT_STATE_PATH)),
            };
            let opts = workflow::ReportOptions {
                repos: t.repos,
                min_severity: t.min_severity,
                call_analysis: t.call_analysis,
                config_room: t.config.map(|c| c.room),
                state_path,
                dry_run,
            };
            workflow::report(opts).await
        }
        Cmd::Scan { repo, path, group } => scan(&cli.config, repo, path, group),
    }
}

/// State file `report` uses when there is no config file.
const DEFAULT_STATE_PATH: &str = "baba-yaga.db";

/// What to scan, and with which settings.
struct Targets {
    repos: Vec<Repo>,
    min_severity: Option<f32>,
    call_analysis: bool,
    /// Loaded when the file exists; required unless `path` is given.
    config: Option<Config>,
}

fn targets(config_path: &Path, only: Option<String>, path: Option<PathBuf>) -> Result<Targets> {
    match path {
        Some(dir) => {
            let config = if config_path.exists() {
                Some(Config::load(config_path)?)
            } else {
                None
            };
            let name = dir
                .canonicalize()?
                .file_name()
                .map_or_else(|| "local".to_string(), |n| n.to_string_lossy().into_owned());
            let repo = Repo {
                name,
                url: dir.to_string_lossy().into_owned(),
                branch: None,
            };
            Ok(Targets {
                repos: vec![repo],
                min_severity: config.as_ref().and_then(|c| c.min_severity),
                call_analysis: config.as_ref().is_none_or(|c| c.call_analysis),
                config,
            })
        }
        None => {
            let config = Config::load(config_path)?;
            let repos: Vec<Repo> = config
                .repos
                .iter()
                .filter(|r| only.as_ref().is_none_or(|n| &r.name == n))
                .cloned()
                .collect();
            if repos.is_empty() {
                bail!("no repos to scan");
            }
            Ok(Targets {
                repos,
                min_severity: config.min_severity,
                call_analysis: config.call_analysis,
                config: Some(config),
            })
        }
    }
}

fn scan(
    config_path: &Path,
    only: Option<String>,
    path: Option<PathBuf>,
    grouped: bool,
) -> Result<()> {
    let t = targets(config_path, only, path)?;
    let mut findings = Vec::new();
    for repo in &t.repos {
        findings.extend(scanner::scan_repo(repo, t.call_analysis)?);
    }
    let findings = scanner::filter_severity(findings, t.min_severity);
    tracing::info!(count = findings.len(), "scan finished");
    if grouped {
        let groups = scanner::group(findings);
        tracing::info!(groups = groups.len(), "grouped");
        println!("{}", serde_json::to_string_pretty(&groups)?);
    } else {
        println!("{}", serde_json::to_string_pretty(&findings)?);
    }
    Ok(())
}
