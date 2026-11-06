//! `bay init`: the README quickstart as one idempotent command. Each step
//! says what it does, the result, and a checkpoint. Running it again keeps
//! what already works: a registered agent, open addressing, the chosen room
//! and an existing config file.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::PathBuf;
use std::str::FromStr;

use anyhow::{bail, Context, Result};
use url::Url;

use super::api::{Agent, Registration, Room, SetupApi};
use super::discover::{self, ServerEnv};
use super::prompt::Prompter;
use super::*;
use crate::config::{Config, Repo};

const STEPS: usize = 7;
/// Retries for a step that loops on a person's answer.
const MAX_TRIES: usize = 5;

pub struct Options {
    pub env_file: PathBuf,
    pub config: PathBuf,
    /// `--repo` values; asked for when empty.
    pub repos: Vec<String>,
}

pub struct Init<'a, A, P, S, W> {
    pub api: &'a A,
    pub prompt: &'a mut P,
    pub server: &'a S,
    pub out: &'a mut W,
    pub opts: &'a Options,
    /// SWITCH_* values already known: the env file, then the environment.
    pub known: BTreeMap<String, String>,
    /// The Console server's settings, read once on first use.
    discovered: Option<BTreeMap<String, String>>,
}

impl<'a, A: SetupApi, P: Prompter, S: ServerEnv, W: Write> Init<'a, A, P, S, W> {
    pub fn new(
        api: &'a A,
        prompt: &'a mut P,
        server: &'a S,
        out: &'a mut W,
        opts: &'a Options,
        known: BTreeMap<String, String>,
    ) -> Self {
        Self {
            api,
            prompt,
            server,
            out,
            opts,
            known,
            discovered: None,
        }
    }

    pub async fn run(mut self) -> Result<()> {
        let base = self.step_server().await?;
        let agent = self.step_agent(&base).await?;
        let handle = self.step_addressing(&base, &agent).await?;
        let room = self.step_room(&base, &agent, &handle).await?;
        self.step_config(&room)?;
        self.step_next()?;
        Ok(())
    }

    fn known(&self, key: &str) -> Option<String> {
        self.known.get(key).filter(|v| !v.is_empty()).cloned()
    }

    fn discovered(&mut self, key: &str) -> Option<String> {
        let server = self.server;
        self.discovered
            .get_or_insert_with(|| {
                server.read(&[
                    discover::PUBLIC_URL,
                    discover::REGISTRATION_TOKEN,
                    discover::ADMIN_EMAIL,
                    discover::ADMIN_PASSWORD,
                ])
            })
            .get(key)
            .cloned()
    }

    fn header(&mut self, n: usize, title: &str) -> Result<()> {
        writeln!(self.out, "\n[{n}/{STEPS}] {title}")?;
        Ok(())
    }

    fn say(&mut self, line: &str) -> Result<()> {
        writeln!(self.out, "  {line}")?;
        Ok(())
    }

    fn checkpoint(&mut self, line: &str) -> Result<()> {
        writeln!(self.out, "  Checkpoint: {line}")?;
        Ok(())
    }

    fn save(&mut self, pairs: &[(&str, &str)]) -> Result<()> {
        envfile::write(&self.opts.env_file, pairs)?;
        for (k, v) in pairs {
            self.known.insert(k.to_string(), v.to_string());
        }
        Ok(())
    }

    /// Step 1: find the server and check that it answers.
    async fn step_server(&mut self) -> Result<Url> {
        self.header(1, "Switch server")?;
        let mut candidate = match self.known(SWITCH_URL) {
            Some(u) => {
                self.say(&format!("Found SWITCH_URL: {u}"))?;
                Some(u)
            }
            None => match self.discovered(discover::PUBLIC_URL) {
                Some(u) => {
                    self.say(&format!(
                        "Found a Console-run server: GATEWAY_PUBLIC_URL={u}"
                    ))?;
                    Some(u)
                }
                None => {
                    self.say("No SWITCH_URL and no Console-run server found.")?;
                    None
                }
            },
        };
        for _ in 0..MAX_TRIES {
            let answer = self.prompt.ask("Switch server URL", candidate.as_deref())?;
            let base = match Url::parse(answer.trim()) {
                Ok(u) => u,
                Err(e) => {
                    self.say(&format!("{answer} is not a URL: {e}"))?;
                    candidate = None;
                    continue;
                }
            };
            match self.api.health(&base).await {
                Ok(()) => {
                    self.say(&format!("GET {base}health: ok"))?;
                    let url = answer.trim().to_string();
                    self.save(&[(SWITCH_URL, &url)])?;
                    self.checkpoint(&format!("the server answers at {url}."))?;
                    return Ok(base);
                }
                Err(e) => {
                    self.say(&format!("GET /health at {base} failed: {e:#}"))?;
                    self.say(
                        "Hint: Console changes its port on restart; check GATEWAY_PUBLIC_URL.",
                    )?;
                    candidate = None;
                }
            }
        }
        bail!("no working Switch server URL")
    }

    /// Steps 2 and 3: the registration key, then register once, or keep a
    /// registered agent whose key still works.
    async fn step_agent(&mut self, base: &Url) -> Result<Agent> {
        if let (Some(id), Some(key)) = (self.known(SWITCH_AGENT_ID), self.known(SWITCH_AGENT_KEY)) {
            let agent = Agent { id, key };
            match self.api.check_key(base, &agent).await {
                Ok(()) => {
                    self.header(2, "Registration key")?;
                    self.say("Not needed: an agent is already registered and its key works.")?;
                    self.checkpoint("nothing to do.")?;
                    self.header(3, "Register the agent")?;
                    if self
                        .prompt
                        .confirm(&format!("Keep the registered agent {}?", agent.id), true)?
                    {
                        self.say(&format!("Kept SWITCH_AGENT_ID={}.", agent.id))?;
                        self.checkpoint("the agent key in .env works.")?;
                        return Ok(agent);
                    }
                }
                Err(e) => {
                    self.say(&format!(
                        "The agent in .env ({}) does not work here: {e:#}. Registering again.",
                        agent.id
                    ))?;
                }
            }
        }

        self.header(2, "Registration key")?;
        let reg_key = if let Some(k) = self.known(SWITCH_REGISTRATION_KEY) {
            self.say("Found SWITCH_REGISTRATION_KEY (not shown).")?;
            k
        } else if let Some(k) = self.discovered(discover::REGISTRATION_TOKEN) {
            self.say("Found AGENT_REGISTRATION_TOKEN in the Console server process (not shown).")?;
            k
        } else {
            self.say("No registration key found. It is the AGENT_REGISTRATION_TOKEN the server was started with.")?;
            self.prompt.secret("Registration key")?
        };
        self.checkpoint("a registration key is ready.")?;

        self.header(3, "Register the agent")?;
        let mut name = self.prompt.ask("Agent name", Some(DEFAULT_AGENT_NAME))?;
        for _ in 0..MAX_TRIES {
            self.say(&format!("Registering {name} with POST /agents ..."))?;
            let agent = match self.api.register(base, &reg_key, &name).await? {
                Registration::Registered(agent) => {
                    self.say(&format!("Registered: SWITCH_AGENT_ID={}.", agent.id))?;
                    agent
                }
                Registration::NameTaken => {
                    self.say(&format!(
                        "409: an agent named {name} is already registered."
                    ))?;
                    if self.prompt.confirm(
                        "Use that agent? You need its SWITCH_AGENT_ID and SWITCH_AGENT_KEY",
                        true,
                    )? {
                        let agent = Agent {
                            id: self.prompt.ask("SWITCH_AGENT_ID", None)?,
                            key: self.prompt.secret("SWITCH_AGENT_KEY")?,
                        };
                        if let Err(e) = self.api.check_key(base, &agent).await {
                            self.say(&format!("That key does not work: {e:#}"))?;
                            continue;
                        }
                        agent
                    } else {
                        name = self.prompt.ask("New agent name", None)?;
                        continue;
                    }
                }
            };
            let url = self.known(SWITCH_URL).unwrap_or_default();
            self.save(&[
                (SWITCH_URL, &url),
                (SWITCH_AGENT_ID, &agent.id),
                (SWITCH_AGENT_KEY, &agent.key),
                (SWITCH_REGISTRATION_KEY, &reg_key),
            ])?;
            self.say(&format!(
                "Wrote SWITCH_URL, SWITCH_AGENT_ID, SWITCH_AGENT_KEY and SWITCH_REGISTRATION_KEY to {} (mode 0600).",
                self.opts.env_file.display()
            ))?;
            self.checkpoint("the agent is registered and its key is saved, not shown.")?;
            return Ok(agent);
        }
        bail!("could not register an agent")
    }

    /// Step 4: open the agent's addressing so people in the room can
    /// address it. Returns its Discord handle.
    async fn step_addressing(&mut self, base: &Url, agent: &Agent) -> Result<String> {
        self.header(4, "Addressing")?;
        let info = self.api.agent_detail(base, agent).await?;
        let handle = if info.name.is_empty() {
            discord_handle(DEFAULT_AGENT_NAME)
        } else {
            discord_handle(&info.name)
        };
        match info.addressing_open() {
            Some(true) => {
                self.say("Already open: addressing_policy rules are [].")?;
                self.checkpoint("people in the room can address the agent.")?;
                return Ok(handle);
            }
            None => {
                self.say("Could not read the addressing policy from get_agent_detail.")?;
            }
            Some(false) => {
                self.say("Closed: only some senders may address the agent.")?;
            }
        }
        if !self.prompt.confirm(
            "Open it now with the gateway admin login? (No skips this step)",
            true,
        )? {
            self.say("Skipped. Without it, room messages do not reach the agent.")?;
            self.checkpoint("skipped; run `bay init` again or `bay doctor` later.")?;
            return Ok(handle);
        }
        let (email, password) = match (
            self.discovered(discover::ADMIN_EMAIL),
            self.discovered(discover::ADMIN_PASSWORD),
        ) {
            (Some(e), Some(p))
                if self.prompt.confirm(
                    "Use the gateway admin login from the Console server process?",
                    true,
                )? =>
            {
                (e, p)
            }
            (e, _) => (
                self.prompt.ask("Gateway admin email", e.as_deref())?,
                self.prompt.secret("Gateway admin password")?,
            ),
        };
        self.say("Logging in to the gateway and setting addressing-policy to {\"rules\":[]} ...")?;
        self.api
            .open_addressing(base, &email, &password, &agent.id)
            .await?;
        let after = self.api.agent_detail(base, agent).await?;
        let rules = after
            .addressing_rules
            .as_ref()
            .map_or("unknown".to_string(), |r| {
                serde_json::Value::from(r.clone()).to_string()
            });
        self.say(&format!("Read back: addressing_policy rules = {rules}"))?;
        if after.addressing_open() != Some(true) {
            bail!("addressing is still not open after the update");
        }
        self.checkpoint("people in the room can address the agent.")?;
        Ok(handle)
    }

    /// Step 5: pick the room, waiting for the person to create one and
    /// invite the agent if there is none.
    async fn step_room(&mut self, base: &Url, agent: &Agent, handle: &str) -> Result<Room> {
        self.header(5, "Room")?;
        let mut rooms = self.api.list_rooms(base, agent).await?;
        for _ in 0..MAX_TRIES {
            if !rooms.is_empty() {
                break;
            }
            self.say("The agent is in no room yet.")?;
            self.prompt.pause(&format!(
                "Create a room in Console, then /invite-agent agent:{handle}"
            ))?;
            rooms = self.api.list_rooms(base, agent).await?;
        }
        if rooms.is_empty() {
            bail!("the agent is still in no room");
        }
        let room = self.choose_room(&rooms)?;
        self.save(&[(SWITCH_ROOM_ID, &room.id)])?;
        self.say(&format!(
            "Wrote SWITCH_ROOM_ID={} ({}).",
            room.id, room.name
        ))?;
        self.checkpoint(&format!("the agent will post in {}.", room.name))?;
        Ok(room)
    }

    fn choose_room(&mut self, rooms: &[Room]) -> Result<Room> {
        if let Some(current) = self
            .known(SWITCH_ROOM_ID)
            .and_then(|id| rooms.iter().find(|r| r.id == id))
        {
            if self.prompt.confirm(
                &format!("Keep room {} ({})?", current.name, current.id),
                true,
            )? {
                return Ok(current.clone());
            }
        }
        if let [only] = rooms {
            if self
                .prompt
                .confirm(&format!("Use room {} ({})?", only.name, only.id), true)?
            {
                return Ok(only.clone());
            }
        }
        for (i, r) in rooms.iter().enumerate() {
            self.say(&format!("{}. {} ({})", i + 1, r.name, r.id))?;
        }
        for _ in 0..MAX_TRIES {
            let answer = self.prompt.ask("Room number", None)?;
            match answer.parse::<usize>() {
                Ok(n) if (1..=rooms.len()).contains(&n) => return Ok(rooms[n - 1].clone()),
                _ => self.say(&format!("Pick a number from 1 to {}.", rooms.len()))?,
            }
        }
        bail!("no room chosen")
    }

    /// Step 6: write the config if there is none; an existing one is kept.
    fn step_config(&mut self, room: &Room) -> Result<()> {
        self.header(6, "Config")?;
        let path = self.opts.config.clone();
        if path.exists() {
            let c = Config::load(&path)?;
            self.say(&format!(
                "Kept {} ({} repos); it parses.",
                path.display(),
                c.repos.len()
            ))?;
            if c.room != room.id {
                self.say(&format!(
                    "Its room is {}; SWITCH_ROOM_ID in .env ({}) wins.",
                    c.room, room.id
                ))?;
            }
            self.checkpoint("the config is in place.")?;
            return Ok(());
        }
        let fixer = self.prompt.ask("Fixer agent name", Some(DEFAULT_FIXER))?;
        let schedule = self.ask_schedule()?;
        let mut repos = self.collect_repos()?;
        unique_names(&mut repos);
        let text = config_text(&room.id, &fixer, &schedule, &repos)?;
        std::fs::write(&path, text)
            .with_context(|| format!("failed to write {}", path.display()))?;
        self.say(&format!(
            "Wrote {}: room, fixer_agent = {fixer}, schedule = {schedule}, {} repos.",
            path.display(),
            repos.len()
        ))?;
        self.checkpoint("the config parses.")?;
        Ok(())
    }

    fn ask_schedule(&mut self) -> Result<String> {
        for _ in 0..MAX_TRIES {
            let s = self.prompt.ask(
                "Scan schedule (cron with seconds, UTC)",
                Some(DEFAULT_SCHEDULE),
            )?;
            match cron::Schedule::from_str(&s) {
                Ok(_) => return Ok(s),
                Err(e) => self.say(&format!("Not a valid schedule: {e}"))?,
            }
        }
        bail!("no valid schedule")
    }

    fn collect_repos(&mut self) -> Result<Vec<Repo>> {
        if !self.opts.repos.is_empty() {
            return self.opts.repos.iter().map(|r| repo_from_input(r)).collect();
        }
        let mut repos = Vec::new();
        loop {
            let q = if repos.is_empty() {
                "Repo to scan: local path or git URL"
            } else {
                "Another repo (empty to finish)"
            };
            let answer = self.prompt.ask(q, Some(""))?;
            if answer.trim().is_empty() {
                if repos.is_empty() {
                    self.say("At least one repo is needed.")?;
                    continue;
                }
                return Ok(repos);
            }
            match repo_from_input(&answer) {
                Ok(r) => {
                    self.say(&format!("Added {} ({}).", r.name, r.url))?;
                    repos.push(r);
                }
                Err(e) => self.say(&format!("{e:#}"))?,
            }
        }
    }

    /// Step 7: what to run next.
    fn step_next(&mut self) -> Result<()> {
        self.header(7, "Next")?;
        let mut flags = String::new();
        if self.opts.config != std::path::Path::new("baba-yaga.toml") {
            flags.push_str(&format!(" --config {}", self.opts.config.display()));
        }
        let env_flag = if self.opts.env_file != std::path::Path::new(".env") {
            format!(" --env-file {}", self.opts.env_file.display())
        } else {
            String::new()
        };
        self.say(&format!("bay{flags} doctor{env_flag}"))?;
        self.say(&format!("bay{flags} serve"))?;
        if !env_flag.is_empty() {
            self.say(&format!(
                "serve reads .env from the current directory; run it where {} is, named .env.",
                self.opts.env_file.display()
            ))?;
        }
        self.checkpoint("setup is done.")?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::super::api::fake::FakeApi;
    use super::super::prompt::{NonInteractive, Scripted};
    use super::*;

    struct NoServer;
    impl ServerEnv for NoServer {
        fn read(&self, _: &[&str]) -> BTreeMap<String, String> {
            BTreeMap::new()
        }
    }

    /// A Console server process with every setting.
    struct Console;
    impl ServerEnv for Console {
        fn read(&self, keys: &[&str]) -> BTreeMap<String, String> {
            [
                (discover::PUBLIC_URL, "http://localhost:51970"),
                (discover::REGISTRATION_TOKEN, "reg-token"),
                (discover::ADMIN_EMAIL, "admin@example.test"),
                (discover::ADMIN_PASSWORD, "admin-pass"),
            ]
            .into_iter()
            .filter(|(k, _)| keys.contains(k))
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
        }
    }

    fn room(id: &str, name: &str) -> Room {
        Room {
            id: id.into(),
            name: name.into(),
        }
    }

    fn fake() -> FakeApi {
        FakeApi {
            healthy: true,
            admin: Some(("admin@example.test".into(), "admin-pass".into())),
            ..FakeApi::default()
        }
    }

    struct Dir {
        _tmp: tempfile::TempDir,
        opts: Options,
        samples: String,
    }

    fn dir() -> Dir {
        let tmp = tempfile::tempdir().unwrap();
        let samples = tmp.path().join("samples");
        std::fs::create_dir(&samples).unwrap();
        Dir {
            opts: Options {
                env_file: tmp.path().join(".env"),
                config: tmp.path().join("baba-yaga.toml"),
                repos: vec![],
            },
            samples: samples.to_string_lossy().into_owned(),
            _tmp: tmp,
        }
    }

    async fn run<P: Prompter, S: ServerEnv>(
        api: &FakeApi,
        prompt: &mut P,
        server: &S,
        d: &Dir,
    ) -> (Result<()>, String) {
        let known = envfile::read(&d.opts.env_file).unwrap();
        let mut out = Vec::new();
        let r = Init::new(api, prompt, server, &mut out, &d.opts, known)
            .run()
            .await;
        (r, String::from_utf8(out).unwrap())
    }

    #[tokio::test]
    async fn fresh_setup_from_a_console_server_then_rerun_changes_nothing() {
        let api = fake();
        api.rooms_later
            .borrow_mut()
            .push(room("room-1", "security-patrol"));
        let d = dir();
        let mut p = Scripted::new(&[
            "",         // server URL: the discovered one
            "",         // agent name: baba-yaga
            "",         // open addressing: yes
            "",         // use the Console admin login: yes
            "",         // Enter after the Discord step
            "",         // use the only room: yes
            "",         // fixer: patrol-fixer
            "",         // schedule: default
            &d.samples, // first repo
            "",         // done
        ]);
        let (r, out) = run(&api, &mut p, &Console, &d).await;
        r.unwrap();

        // Every step has a checkpoint, and the Discord step names the handle.
        assert_eq!(out.matches("Checkpoint:").count(), 7, "{out}");
        assert!(p
            .asked
            .contains(&"Create a room in Console, then /invite-agent agent:baba_yaga".to_string()));
        // No secret reaches the output.
        for secret in ["key-baba-yaga", "reg-token", "admin-pass"] {
            assert!(!out.contains(secret), "{secret} leaked:\n{out}");
        }
        assert!(*api.addressing_open.borrow());

        let env = envfile::read(&d.opts.env_file).unwrap();
        assert_eq!(env[SWITCH_URL], "http://localhost:51970");
        assert_eq!(env[SWITCH_AGENT_ID], "id-baba-yaga");
        assert_eq!(env[SWITCH_AGENT_KEY], "key-baba-yaga");
        assert_eq!(env[SWITCH_REGISTRATION_KEY], "reg-token");
        assert_eq!(env[SWITCH_ROOM_ID], "room-1");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&d.opts.env_file)
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        let c = Config::load(&d.opts.config).unwrap();
        assert_eq!(c.room, "room-1");
        assert_eq!(c.schedule, DEFAULT_SCHEDULE);
        assert_eq!(c.fixer_agent.as_deref(), Some("patrol-fixer"));
        assert_eq!(c.repos.len(), 1);
        assert_eq!(c.repos[0].name, "samples");

        // A second run keeps everything and registers nothing.
        let config_before = std::fs::read_to_string(&d.opts.config).unwrap();
        let env_before = std::fs::read_to_string(&d.opts.env_file).unwrap();
        api.calls.borrow_mut().clear();
        let mut p = Scripted::new(&[
            "", // server URL from .env
            "", // keep the registered agent
            "", // keep the room
        ]);
        let (r, out) = run(&api, &mut p, &NoServer, &d).await;
        r.unwrap();
        assert!(!api.calls.borrow().iter().any(|c| c.starts_with("register")));
        assert!(!api
            .calls
            .borrow()
            .iter()
            .any(|c| c.starts_with("open_addressing")));
        assert!(out.contains("Already open"), "{out}");
        assert_eq!(
            std::fs::read_to_string(&d.opts.config).unwrap(),
            config_before
        );
        assert_eq!(
            std::fs::read_to_string(&d.opts.env_file).unwrap(),
            env_before
        );
        assert!(p.answers.is_empty());
    }

    #[tokio::test]
    async fn name_taken_falls_back_to_the_existing_agent() {
        let api = fake();
        api.taken.borrow_mut().push("baba-yaga".into());
        api.agents
            .borrow_mut()
            .push(("id-old".into(), "key-old".into()));
        *api.addressing_open.borrow_mut() = true;
        api.rooms.borrow_mut().push(room("room-1", "patrol"));
        let d = dir();
        let mut p = Scripted::new(&[
            "http://localhost:4000", // server URL typed
            "reg-token",             // registration key (hidden)
            "",                      // agent name: baba-yaga, taken
            "y",                     // use the existing agent
            "id-old",                // its id
            "key-old",               // its key (hidden)
            "",                      // use the only room
            "",
            "",
            &d.samples,
            "",
        ]);
        let (r, out) = run(&api, &mut p, &NoServer, &d).await;
        r.unwrap();
        assert!(out.contains("409: an agent named baba-yaga is already registered."));
        assert!(!out.contains("key-old") && !out.contains("reg-token"));
        let env = envfile::read(&d.opts.env_file).unwrap();
        assert_eq!(env[SWITCH_AGENT_ID], "id-old");
        assert_eq!(env[SWITCH_URL], "http://localhost:4000");
    }

    #[tokio::test]
    async fn non_interactive_fails_on_the_first_missing_value() {
        let api = fake();
        let d = dir();
        // No .env and no Console server: the URL is missing.
        let (r, _) = run(&api, &mut NonInteractive, &NoServer, &d).await;
        let msg = format!("{:#}", r.unwrap_err());
        assert!(msg.contains("Switch server URL"), "{msg}");

        // With a server but no room, it stops at the Discord step.
        *api.addressing_open.borrow_mut() = true;
        let (r, _) = run(&api, &mut NonInteractive, &Console, &d).await;
        let msg = format!("{:#}", r.unwrap_err());
        assert!(msg.contains("/invite-agent agent:baba_yaga"), "{msg}");
    }

    #[tokio::test]
    async fn non_interactive_with_repos_completes() {
        let api = fake();
        *api.addressing_open.borrow_mut() = true;
        api.rooms.borrow_mut().push(room("room-1", "patrol"));
        let mut d = dir();
        d.opts.repos = vec![d.samples.clone(), "https://github.com/o/api.git".into()];
        let (r, out) = run(&api, &mut NonInteractive, &Console, &d).await;
        r.unwrap();
        // Non-default paths are carried into the next commands.
        let config = d.opts.config.display().to_string();
        let env = d.opts.env_file.display().to_string();
        assert!(
            out.contains(&format!("bay --config {config} doctor --env-file {env}")),
            "{out}"
        );
        assert!(
            out.contains(&format!("bay --config {config} serve")),
            "{out}"
        );
        let c = Config::load(&d.opts.config).unwrap();
        assert_eq!(c.repos.len(), 2);
    }

    #[tokio::test]
    async fn unhealthy_server_is_retried_then_given_up() {
        let api = FakeApi::default(); // not healthy
        let d = dir();
        let mut p = Scripted::new(&["http://a:1", "http://b:2", "c", "http://d:4", "http://e:5"]);
        let (r, out) = run(&api, &mut p, &NoServer, &d).await;
        assert!(r.is_err());
        assert!(out.contains("Console changes its port"));
        assert!(out.contains("c is not a URL"));
        assert!(!d.opts.env_file.exists());
    }
}
