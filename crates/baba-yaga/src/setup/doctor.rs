//! `bay doctor`: read-only checks of everything `serve` needs, one line
//! each with PASS, FAIL or SKIP and a hint. Changes nothing and opens no
//! WebSocket, so it is safe to run while `serve` is running.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use url::Url;

use super::api::{Agent, SetupApi};
use super::*;
use crate::config::Config;
use crate::workflow::is_fixer;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Pass,
    Fail,
    Skip,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Check {
    pub name: String,
    pub outcome: Outcome,
    /// What was found. Never a secret.
    pub detail: String,
    /// What to do about a FAIL or a SKIP.
    pub hint: Option<String>,
}

impl Check {
    fn pass(name: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            outcome: Outcome::Pass,
            detail: detail.into(),
            hint: None,
        }
    }

    fn fail(name: impl Into<String>, detail: impl Into<String>, hint: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            outcome: Outcome::Fail,
            detail: detail.into(),
            hint: Some(hint.into()),
        }
    }

    fn skip(name: impl Into<String>, detail: impl Into<String>, hint: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            outcome: Outcome::Skip,
            detail: detail.into(),
            hint: Some(hint.into()),
        }
    }
}

/// One line per check, names aligned, the hint indented under its line.
pub fn format(checks: &[Check]) -> String {
    let width = checks.iter().map(|c| c.name.len()).max().unwrap_or(0);
    let mut out = String::new();
    for c in checks {
        let tag = match c.outcome {
            Outcome::Pass => "PASS",
            Outcome::Fail => "FAIL",
            Outcome::Skip => "SKIP",
        };
        out.push_str(&format!("{tag}  {:<width$}  {}\n", c.name, c.detail));
        if let Some(h) = &c.hint {
            out.push_str(&format!("      {:<width$}  hint: {h}\n", ""));
        }
    }
    let fails = checks.iter().filter(|c| c.outcome == Outcome::Fail).count();
    out.push_str(&match fails {
        0 => "All checks passed or were skipped.\n".to_string(),
        1 => "1 check failed.\n".to_string(),
        n => format!("{n} checks failed.\n"),
    });
    out
}

pub fn any_failed(checks: &[Check]) -> bool {
    checks.iter().any(|c| c.outcome == Outcome::Fail)
}

/// Local programs, behind a trait for tests.
pub trait Tools {
    /// The first line `program args` prints, or `None` if it cannot run.
    fn version(&self, program: &str, args: &[&str]) -> Option<String>;
    /// `git ls-remote` succeeds for `url`.
    fn ls_remote(&self, url: &str) -> Result<(), String>;
}

pub struct System;

impl Tools for System {
    fn version(&self, program: &str, args: &[&str]) -> Option<String> {
        let out = Command::new(program).args(args).output().ok()?;
        if !out.status.success() {
            return None;
        }
        let text = String::from_utf8_lossy(&out.stdout);
        let text = if text.trim().is_empty() {
            String::from_utf8_lossy(&out.stderr).into_owned()
        } else {
            text.into_owned()
        };
        text.lines().next().map(|l| l.trim().to_string())
    }

    fn ls_remote(&self, url: &str) -> Result<(), String> {
        let out = Command::new("git")
            .args(["ls-remote", url, "HEAD"])
            // Never wait for a password or a host key question.
            .env("GIT_TERMINAL_PROMPT", "0")
            .env(
                "GIT_SSH_COMMAND",
                "ssh -o BatchMode=yes -o ConnectTimeout=10",
            )
            .output()
            .map_err(|e| format!("cannot run git: {e}"))?;
        if out.status.success() {
            Ok(())
        } else {
            let err = String::from_utf8_lossy(&out.stderr);
            Err(err
                .lines()
                .last()
                .unwrap_or("git ls-remote failed")
                .trim()
                .to_string())
        }
    }
}

pub struct Inputs {
    pub env_file: PathBuf,
    pub config: PathBuf,
    /// SWITCH_* values: the env file, then the environment on top.
    pub known: BTreeMap<String, String>,
}

/// Run every check, in order.
pub async fn run<A: SetupApi, T: Tools>(api: &A, tools: &T, inp: &Inputs) -> Vec<Check> {
    let mut checks = Vec::new();
    let get = |k: &str| inp.known.get(k).filter(|v| !v.is_empty()).cloned();

    // Tools.
    checks.push(match tools.version("osv-scanner", &["--version"]) {
        Some(v) => Check::pass("osv-scanner", v),
        None => Check::fail(
            "osv-scanner",
            "not found on PATH",
            "brew install osv-scanner, or go install github.com/google/osv-scanner/v2/cmd/osv-scanner@latest",
        ),
    });
    checks.push(match tools.version("go", &["version"]) {
        Some(v) => Check::pass("go", v),
        None => Check::skip(
            "go",
            "not found on PATH; Go reachability (call analysis) is off",
            "install Go to mark Go findings whose vulnerable code is called",
        ),
    });

    // .env.
    let config = Config::load(&inp.config);
    let room_id = get(SWITCH_ROOM_ID).or_else(|| config.as_ref().ok().map(|c| c.room.clone()));
    checks.push(env_check(inp, &get, room_id.is_some()));

    // The server.
    let base = get(SWITCH_URL).and_then(|u| Url::parse(&u).ok());
    let healthy = match &base {
        None => {
            checks.push(Check::skip(
                "server",
                "no valid SWITCH_URL",
                "run `bay init`",
            ));
            false
        }
        Some(b) => match api.health(b).await {
            Ok(()) => {
                checks.push(Check::pass("server", format!("GET {b}health answered")));
                true
            }
            Err(e) => {
                checks.push(Check::fail(
                    "server",
                    format!("GET /health at {b} failed: {e:#}"),
                    "Console changes its port on restart; update SWITCH_URL from GATEWAY_PUBLIC_URL",
                ));
                false
            }
        },
    };

    // The agent.
    let agent = match (get(SWITCH_AGENT_ID), get(SWITCH_AGENT_KEY)) {
        (Some(id), Some(key)) => Some(Agent { id, key }),
        _ => None,
    };
    let agent = match (&base, &agent, healthy) {
        (Some(b), Some(a), true) => match api.check_key(b, a).await {
            Ok(()) => {
                checks.push(Check::pass("agent key", format!("works for {}", a.id)));
                Some((b, a))
            }
            Err(e) => {
                checks.push(Check::fail(
                    "agent key",
                    format!("refused for {}: {e:#}", a.id),
                    "the key is shown once at registration; run `bay init` to register again",
                ));
                None
            }
        },
        (_, None, _) => {
            checks.push(Check::skip(
                "agent key",
                "no SWITCH_AGENT_ID or SWITCH_AGENT_KEY",
                "run `bay init`",
            ));
            None
        }
        _ => {
            checks.push(Check::skip(
                "agent key",
                "the server is not reachable",
                "fix the server check first",
            ));
            None
        }
    };

    // Addressing, the room and the fixer, all with the agent key.
    let skipped = |name: &str| {
        Check::skip(
            name,
            "the agent key is not usable",
            "fix the agent key check first",
        )
    };
    let mut handle = discord_handle(DEFAULT_AGENT_NAME);
    match agent {
        None => {
            checks.push(skipped("addressing"));
            checks.push(skipped("room"));
        }
        Some((b, a)) => {
            checks.push(match api.agent_detail(b, a).await {
                Ok(info) => {
                    if !info.name.is_empty() {
                        handle = discord_handle(&info.name);
                    }
                    match info.addressing_open() {
                        Some(true) => {
                            Check::pass("addressing", "open: addressing_policy rules are []")
                        }
                        Some(false) => Check::fail(
                            "addressing",
                            "closed: the rules limit who may address the agent",
                            "run `bay init` (step 4) to open it with the gateway admin login",
                        ),
                        None => Check::skip(
                            "addressing",
                            "get_agent_detail has no addressing_policy",
                            "check it in the gateway, or run `bay init`",
                        ),
                    }
                }
                Err(e) => Check::fail(
                    "addressing",
                    format!("get_agent_detail failed: {e:#}"),
                    "run `bay init`",
                ),
            });
            checks.push(match (&room_id, api.list_rooms(b, a).await) {
                (None, _) => Check::fail(
                    "room",
                    "no SWITCH_ROOM_ID and no room in the config",
                    "run `bay init` (step 5)",
                ),
                (_, Err(e)) => Check::fail(
                    "room",
                    format!("list_rooms failed: {e:#}"),
                    "run `bay init`",
                ),
                (Some(id), Ok(rooms)) => match rooms.iter().find(|r| &r.id == id) {
                    Some(r) => Check::pass("room", format!("{} ({})", r.name, r.id)),
                    None => Check::fail(
                        "room",
                        format!("{id} is not among the agent's {} rooms", rooms.len()),
                        format!("in the room's Discord channel: /invite-agent agent:{handle}"),
                    ),
                },
            });
        }
    }

    // The config and its repos.
    match &config {
        Ok(c) => {
            checks.push(Check::pass(
                "config",
                format!(
                    "{} parses: {} repos, schedule {}",
                    inp.config.display(),
                    c.repos.len(),
                    c.schedule
                ),
            ));
            if c.repos.is_empty() {
                checks.push(Check::fail(
                    "repos",
                    "the config lists no repos",
                    "add a [[repos]] entry",
                ));
            }
            for r in &c.repos {
                checks.push(repo_check(tools, &r.name, &r.url));
            }
        }
        Err(e) if !inp.config.exists() => checks.push(Check::fail(
            "config",
            format!("{} not found ({e})", inp.config.display()),
            "run `bay init` (step 6)",
        )),
        Err(e) => checks.push(Check::fail(
            "config",
            format!("{e:#}"),
            "compare with baba-yaga.example.toml",
        )),
    }

    // The fixer in the room.
    let fixer = config.as_ref().ok().and_then(|c| c.fixer_agent.clone());
    checks.push(match (fixer, agent, &room_id) {
        (None, _, _) => Check::skip(
            "fixer",
            "no fixer_agent in the config; approvals hand nothing over",
            "set fixer_agent = \"patrol-fixer\"",
        ),
        (Some(_), None, _) | (Some(_), _, None) => Check::skip(
            "fixer",
            "needs a usable agent key and a room",
            "fix the checks above first",
        ),
        (Some(f), Some((b, a)), Some(room)) => match api.room_agents(b, a, room).await {
            Ok(names) if names.iter().any(|n| is_fixer(n, &f)) => {
                Check::pass("fixer", format!("{f} is in the room"))
            }
            Ok(names) => Check::fail(
                "fixer",
                format!("{f} is not in the room (agents: {})", names.join(", ")),
                format!("add {f} to the room in Console, or import template/patrol.template.yaml"),
            ),
            Err(e) => Check::skip(
                "fixer",
                format!("get_room_detail failed: {e:#}"),
                "check the room's agents in Console",
            ),
        },
    });
    checks
}

/// The four variables `serve` needs; the room may come from the config.
fn env_check(inp: &Inputs, get: &dyn Fn(&str) -> Option<String>, have_room: bool) -> Check {
    let name = ".env";
    let mut missing: Vec<&str> = [SWITCH_URL, SWITCH_AGENT_ID, SWITCH_AGENT_KEY]
        .into_iter()
        .filter(|k| get(k).is_none())
        .collect();
    if !have_room {
        missing.push(SWITCH_ROOM_ID);
    }
    if !missing.is_empty() {
        return Check::fail(
            name,
            format!("missing {}", missing.join(", ")),
            "run `bay init`",
        );
    }
    if !inp.env_file.exists() {
        return Check::pass(name, "no file; the variables come from the environment");
    }
    if let Some(mode) = world_readable(&inp.env_file) {
        return Check::fail(
            name,
            format!(
                "{} has mode {mode:o} and holds the agent key",
                inp.env_file.display()
            ),
            format!("chmod 600 {}", inp.env_file.display()),
        );
    }
    let room = if get(SWITCH_ROOM_ID).is_some() {
        "SWITCH_ROOM_ID"
    } else {
        "the room from the config"
    };
    Check::pass(
        name,
        format!(
            "{}: SWITCH_URL, SWITCH_AGENT_ID, SWITCH_AGENT_KEY, {room}",
            inp.env_file.display()
        ),
    )
}

/// The file mode when group or others can read the file.
#[cfg(unix)]
fn world_readable(path: &Path) -> Option<u32> {
    use std::os::unix::fs::PermissionsExt;
    let mode = std::fs::metadata(path).ok()?.permissions().mode() & 0o777;
    (mode & 0o077 != 0).then_some(mode)
}

#[cfg(not(unix))]
fn world_readable(_: &Path) -> Option<u32> {
    None
}

fn repo_check<T: Tools>(tools: &T, name: &str, url: &str) -> Check {
    let label = format!("repo {name}");
    if Path::new(url).is_dir() {
        return Check::pass(label, format!("{url} exists"));
    }
    if url.contains("://") || url.starts_with("git@") {
        return match tools.ls_remote(url) {
            Ok(()) => Check::pass(label, format!("{url} is reachable")),
            Err(e) => Check::fail(
                label,
                format!("git ls-remote {url} failed: {e}"),
                "check the URL and that git can read it without a prompt",
            ),
        };
    }
    Check::fail(
        label,
        format!("{url} is not a directory"),
        "use an absolute path or a git URL",
    )
}

#[cfg(test)]
mod tests {
    use super::super::api::fake::FakeApi;
    use super::super::api::Room;
    use super::*;

    struct FakeTools {
        osv: bool,
        go: bool,
        remotes_ok: bool,
    }

    impl Tools for FakeTools {
        fn version(&self, program: &str, _: &[&str]) -> Option<String> {
            match program {
                "osv-scanner" if self.osv => Some("osv-scanner version: 2.2.1".into()),
                "go" if self.go => Some("go version go1.25.1 darwin/arm64".into()),
                _ => None,
            }
        }

        fn ls_remote(&self, _: &str) -> Result<(), String> {
            if self.remotes_ok {
                Ok(())
            } else {
                Err("fatal: repository not found".into())
            }
        }
    }

    fn tools() -> FakeTools {
        FakeTools {
            osv: true,
            go: true,
            remotes_ok: true,
        }
    }

    struct Setup {
        _tmp: tempfile::TempDir,
        inp: Inputs,
    }

    /// A working setup: .env (0600), config with a local and a remote repo.
    fn setup() -> Setup {
        let tmp = tempfile::tempdir().unwrap();
        let samples = tmp.path().join("samples");
        std::fs::create_dir(&samples).unwrap();
        let env_file = tmp.path().join(".env");
        let pairs = [
            (SWITCH_URL, "http://localhost:51970"),
            (SWITCH_AGENT_ID, "id-baba-yaga"),
            (SWITCH_AGENT_KEY, "key-baba-yaga"),
            (SWITCH_ROOM_ID, "room-1"),
        ];
        envfile::write(&env_file, &pairs).unwrap();
        let config = tmp.path().join("baba-yaga.toml");
        let repos = vec![
            repo_from_input(samples.to_str().unwrap()).unwrap(),
            repo_from_input("https://github.com/o/api.git").unwrap(),
        ];
        std::fs::write(
            &config,
            config_text("room-1", "patrol-fixer", DEFAULT_SCHEDULE, &repos).unwrap(),
        )
        .unwrap();
        let known = envfile::read(&env_file).unwrap();
        Setup {
            _tmp: tmp,
            inp: Inputs {
                env_file,
                config,
                known,
            },
        }
    }

    fn api() -> FakeApi {
        let api = FakeApi {
            healthy: true,
            room_agents: vec![(
                "room-1".into(),
                vec!["baba_yaga".into(), "patrol-fixer".into()],
            )],
            ..FakeApi::default()
        };
        api.agents
            .borrow_mut()
            .push(("id-baba-yaga".into(), "key-baba-yaga".into()));
        *api.addressing_open.borrow_mut() = true;
        api.rooms.borrow_mut().push(Room {
            id: "room-1".into(),
            name: "security-patrol".into(),
        });
        api
    }

    fn outcomes(checks: &[Check]) -> Vec<(String, Outcome)> {
        checks.iter().map(|c| (c.name.clone(), c.outcome)).collect()
    }

    #[tokio::test]
    async fn healthy_setup_passes_everything() {
        let s = setup();
        let checks = run(&api(), &tools(), &s.inp).await;
        assert!(!any_failed(&checks), "{}", format(&checks));
        let names: Vec<&str> = checks.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "osv-scanner",
                "go",
                ".env",
                "server",
                "agent key",
                "addressing",
                "room",
                "config",
                "repo samples",
                "repo api",
                "fixer"
            ]
        );
        assert!(checks.iter().all(|c| c.outcome == Outcome::Pass));
        assert!(!format(&checks).contains("key-baba-yaga"));
    }

    #[tokio::test]
    async fn failures_and_skips_carry_hints() {
        let s = setup();
        let api = api();
        *api.addressing_open.borrow_mut() = false;
        let tools = FakeTools {
            osv: false,
            go: false,
            remotes_ok: false,
        };
        let checks = run(&api, &tools, &s.inp).await;
        let got = outcomes(&checks);
        assert!(got.contains(&("osv-scanner".into(), Outcome::Fail)));
        assert!(got.contains(&("go".into(), Outcome::Skip)));
        assert!(got.contains(&("addressing".into(), Outcome::Fail)));
        assert!(got.contains(&("repo api".into(), Outcome::Fail)));
        assert!(got.contains(&("repo samples".into(), Outcome::Pass)));
        assert!(checks
            .iter()
            .filter(|c| c.outcome != Outcome::Pass)
            .all(|c| c.hint.is_some()));
        assert!(any_failed(&checks));
    }

    #[tokio::test]
    async fn dead_server_skips_what_depends_on_it() {
        let s = setup();
        let api = FakeApi::default();
        let checks = run(&api, &tools(), &s.inp).await;
        let got = outcomes(&checks);
        assert!(got.contains(&("server".into(), Outcome::Fail)));
        for name in ["agent key", "addressing", "room", "fixer"] {
            assert!(
                got.contains(&(name.into(), Outcome::Skip)),
                "{name}: {got:?}"
            );
        }
        // Only the server was called.
        assert_eq!(api.calls.borrow().len(), 1);
    }

    #[tokio::test]
    async fn missing_env_room_and_fixer_are_reported() {
        let mut s = setup();
        s.inp.known.remove(SWITCH_AGENT_KEY);
        let api = api();
        let checks = run(&api, &tools(), &s.inp).await;
        let env = checks.iter().find(|c| c.name == ".env").unwrap();
        assert_eq!(env.outcome, Outcome::Fail);
        assert_eq!(env.detail, "missing SWITCH_AGENT_KEY");

        // Room not among the agent's rooms; fixer not in the room.
        let s = setup();
        let api = FakeApi {
            room_agents: vec![("room-1".into(), vec!["baba_yaga".into()])],
            ..api
        };
        api.rooms.borrow_mut().clear();
        let checks = run(&api, &tools(), &s.inp).await;
        let room = checks.iter().find(|c| c.name == "room").unwrap();
        assert_eq!(room.outcome, Outcome::Fail);
        assert_eq!(
            room.hint.as_deref(),
            Some("in the room's Discord channel: /invite-agent agent:baba_yaga")
        );
        let fixer = checks.iter().find(|c| c.name == "fixer").unwrap();
        assert_eq!(fixer.outcome, Outcome::Fail);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn readable_env_file_fails() {
        use std::os::unix::fs::PermissionsExt;
        let s = setup();
        std::fs::set_permissions(&s.inp.env_file, std::fs::Permissions::from_mode(0o644)).unwrap();
        let checks = run(&api(), &tools(), &s.inp).await;
        let env = checks.iter().find(|c| c.name == ".env").unwrap();
        assert_eq!(env.outcome, Outcome::Fail);
        assert!(env.hint.as_deref().unwrap().starts_with("chmod 600 "));
    }

    #[test]
    fn formatter_aligns_names_and_indents_hints() {
        let checks = vec![
            Check::pass("go", "go version go1.25.1"),
            Check::fail(
                "osv-scanner",
                "not found on PATH",
                "brew install osv-scanner",
            ),
            Check::skip("fixer", "no fixer_agent", "set fixer_agent"),
        ];
        // Hints sit under the detail column: 6 for the tag, the name width
        // (11, "osv-scanner") and 2 for the gap.
        let pad = " ".repeat(6 + 11 + 2);
        let expected = [
            "PASS  go           go version go1.25.1".to_string(),
            "FAIL  osv-scanner  not found on PATH".to_string(),
            format!("{pad}hint: brew install osv-scanner"),
            "SKIP  fixer        no fixer_agent".to_string(),
            format!("{pad}hint: set fixer_agent"),
            "1 check failed.".to_string(),
        ]
        .join("\n")
            + "\n";
        assert_eq!(format(&checks), expected);
        assert_eq!(format(&[]), "All checks passed or were skipped.\n");
    }
}
