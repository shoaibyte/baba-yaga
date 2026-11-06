//! baba-yaga: a Switch agent that patrols repositories for vulnerable
//! dependencies.
//!
//! The library holds everything; the two binaries, `baba-yaga` and its
//! short alias `bay`, both call [`run`].

mod audit;
mod config;
mod room;
mod scanner;
mod scheduler;
mod setup;
mod state;
mod workflow;

use std::path::{Path, PathBuf};

use anyhow::{bail, Result};
use clap::{Parser, Subcommand};
use tracing_subscriber::EnvFilter;

use config::{Config, Repo};

/// Shown under the help of both binaries.
const NAMES_NOTE: &str = "Installed as both `baba-yaga` and `bay`; they are the same program.";

#[derive(Debug, Parser)]
#[command(version, about, after_help = NAMES_NOTE)]
struct Cli {
    /// Path to the config file.
    #[arg(long, global = true, default_value = "baba-yaga.toml")]
    config: PathBuf,

    #[command(subcommand)]
    command: Cmd,
}

#[derive(Debug, Subcommand)]
enum Cmd {
    /// Set up Switch, .env and the config step by step: find the server,
    /// register the agent once, open its addressing, pick the room and
    /// write baba-yaga.toml. Safe to run again; it keeps what works.
    Init {
        /// Take defaults and fail on any value that would need a question.
        #[arg(long)]
        non_interactive: bool,
        /// The env file to read and write.
        #[arg(long, default_value = ".env")]
        env_file: PathBuf,
        /// A repo to scan: local path or git URL. Repeat for more. Asked
        /// for when not given.
        #[arg(long = "repo", value_name = "PATH_OR_URL")]
        repos: Vec<String>,
    },
    /// Check everything `serve` needs, read-only: tools, .env, the server,
    /// the agent key, addressing, the room, the config, the repos and the
    /// fixer. Safe while `serve` runs. Exits 1 if a check fails.
    Doctor {
        /// The env file to read.
        #[arg(long, default_value = ".env")]
        env_file: PathBuf,
    },
    /// Connect to Switch and run the patrol on its schedule.
    Serve,
    /// Scan once, post each package with findings as a thread, and exit.
    /// For CI. Opens a socket while posting, because thread replies need
    /// one; stop `serve` first, the agent holds one connection at a time.
    /// Groups already posted are skipped.
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

/// The entry point of both binaries.
pub fn run() -> Result<()> {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(run_async())
}

async fn run_async() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .init();

    let cli = Cli::parse();
    if !matches!(cli.command, Cmd::Init { .. } | Cmd::Doctor { .. }) {
        // A missing .env is fine; the environment may already be set.
        // `init` and `doctor` read their own --env-file instead.
        dotenvy::dotenv().ok();
    }
    match cli.command {
        Cmd::Init {
            non_interactive,
            env_file,
            repos,
        } => init(cli.config, env_file, repos, non_interactive).await,
        Cmd::Doctor { env_file } => doctor(cli.config, env_file).await,
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

async fn init(
    config: PathBuf,
    env_file: PathBuf,
    repos: Vec<String>,
    non_interactive: bool,
) -> Result<()> {
    use setup::{init, prompt};
    let known = known_env(&env_file)?;
    let opts = init::Options {
        env_file,
        config,
        repos,
    };
    let api = setup::api::Live;
    let server = setup::discover::ConsoleProcess;
    let mut out = std::io::stdout();
    if non_interactive {
        let mut p = prompt::NonInteractive;
        init::Init::new(&api, &mut p, &server, &mut out, &opts, known)
            .run()
            .await
    } else {
        let mut p = prompt::Terminal;
        init::Init::new(&api, &mut p, &server, &mut out, &opts, known)
            .run()
            .await
    }
}

/// The SWITCH_* values from `env_file`, with the environment on top, as
/// `serve` sees them (dotenv does not override what is already set).
fn known_env(env_file: &Path) -> Result<std::collections::BTreeMap<String, String>> {
    let mut known = setup::envfile::read(env_file)?;
    for key in [
        setup::SWITCH_URL,
        setup::SWITCH_AGENT_ID,
        setup::SWITCH_AGENT_KEY,
        setup::SWITCH_REGISTRATION_KEY,
        setup::SWITCH_ROOM_ID,
    ] {
        if let Ok(v) = std::env::var(key) {
            known.insert(key.to_string(), v);
        }
    }
    Ok(known)
}

async fn doctor(config: PathBuf, env_file: PathBuf) -> Result<()> {
    use setup::doctor;
    let inputs = doctor::Inputs {
        known: known_env(&env_file)?,
        env_file,
        config,
    };
    let checks = doctor::run(&setup::api::Live, &doctor::System, &inputs).await;
    print!("{}", doctor::format(&checks));
    if doctor::any_failed(&checks) {
        std::process::exit(1);
    }
    Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn cli_is_valid_and_help_names_both_binaries() {
        Cli::command().debug_assert();
        let help = Cli::command().render_long_help().to_string();
        assert!(help.contains("`baba-yaga` and `bay`"), "{help}");
    }
}
