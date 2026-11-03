//! baba-yaga: a Switch agent that patrols repositories for vulnerable
//! dependencies.

mod audit;
mod config;
// Used by the workflow, which is not written yet.
#[allow(dead_code)]
mod room;
mod scanner;
mod scheduler;
#[allow(dead_code)]
mod state;
mod workflow;

use std::path::PathBuf;

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
    /// Post a report of open findings to the room and exit.
    Report,
    /// Run the scanner and print findings as JSON. Touches no state.
    Scan {
        /// Only scan the configured repo with this name.
        #[arg(long, conflicts_with = "path")]
        repo: Option<String>,
        /// Scan this local directory instead of the configured repos. The
        /// config file is then optional.
        #[arg(long)]
        path: Option<PathBuf>,
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
        Cmd::Report => workflow::report(&Config::load(&cli.config)?).await,
        Cmd::Scan { repo, path } => scan(&cli.config, repo, path),
    }
}

fn scan(config_path: &std::path::Path, only: Option<String>, path: Option<PathBuf>) -> Result<()> {
    let (repos, min_severity, call_analysis) = match path {
        Some(dir) => {
            let (min, call_analysis) = if config_path.exists() {
                let config = Config::load(config_path)?;
                (config.min_severity, config.call_analysis)
            } else {
                (None, true)
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
            (vec![repo], min, call_analysis)
        }
        None => {
            let config = Config::load(config_path)?;
            let repos: Vec<Repo> = config
                .repos
                .into_iter()
                .filter(|r| only.as_ref().is_none_or(|n| &r.name == n))
                .collect();
            if repos.is_empty() {
                bail!("no repos to scan");
            }
            (repos, config.min_severity, config.call_analysis)
        }
    };

    let mut findings = Vec::new();
    for repo in &repos {
        findings.extend(scanner::scan_repo(repo, call_analysis)?);
    }
    let findings = scanner::filter_severity(findings, min_severity);
    tracing::info!(count = findings.len(), "scan finished");
    println!("{}", serde_json::to_string_pretty(&findings)?);
    Ok(())
}
