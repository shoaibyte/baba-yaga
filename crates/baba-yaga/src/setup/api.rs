//! The Switch calls `init` and `doctor` make, behind a trait so both can be
//! tested with a fake server. None of them opens a WebSocket: a second
//! connection would take over from a running `serve`.
//!
//! Shapes observed on switch-core 0.31 (2026-11-07):
//! - `list_rooms` -> `[{room_id, name, description, connected, archived}]`
//! - `get_room_detail {room_id}` -> `{id, name, agent_names: [..], ..}`,
//!   readable without a connection (`list_participants` is not).
//! - `get_agent_detail {agent_id}` -> `{name, addressing_policy: {rules}, ..}`,
//!   readable with the agent's own key.
//! - Gateway: `POST /gateway/auth/login {email, password}` sets a session
//!   cookie; `PUT /gateway/agents/{id}/addressing-policy` takes
//!   `{"policy": {"rules": []}}`.

use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};
use switch_client::{ConnectionModel, Error, Http, IntegrationProfile, RegisterRequest};
use url::Url;

/// A room the agent is in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Room {
    pub id: String,
    pub name: String,
}

/// What `get_agent_detail` says about the agent.
#[derive(Debug, Clone, PartialEq)]
pub struct AgentInfo {
    /// The stored name, which is also its Discord handle (`baba_yaga`).
    pub name: String,
    /// The addressing rules; `None` when the field is missing.
    pub addressing_rules: Option<Vec<Value>>,
}

impl AgentInfo {
    /// An empty rules list means anyone may address the agent.
    pub fn addressing_open(&self) -> Option<bool> {
        self.addressing_rules.as_ref().map(Vec::is_empty)
    }
}

/// Agent credentials. `Debug` hides the key.
#[derive(Clone, PartialEq, Eq)]
pub struct Agent {
    pub id: String,
    pub key: String,
}

impl std::fmt::Debug for Agent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Agent")
            .field("id", &self.id)
            .field("key", &"<redacted>")
            .finish()
    }
}

#[derive(Debug)]
pub enum Registration {
    Registered(Agent),
    /// 409: an agent with this name exists.
    NameTaken,
}

pub trait SetupApi {
    /// `GET /health`.
    async fn health(&self, base: &Url) -> Result<()>;
    /// `POST /agents` with the registration key.
    async fn register(&self, base: &Url, reg_key: &str, name: &str) -> Result<Registration>;
    /// The agent key works: `GET /agents/{id}/ops`.
    async fn check_key(&self, base: &Url, agent: &Agent) -> Result<()>;
    async fn agent_detail(&self, base: &Url, agent: &Agent) -> Result<AgentInfo>;
    async fn list_rooms(&self, base: &Url, agent: &Agent) -> Result<Vec<Room>>;
    /// Gateway admin login, then open the agent's addressing (empty rules).
    async fn open_addressing(
        &self,
        base: &Url,
        email: &str,
        password: &str,
        agent_id: &str,
    ) -> Result<()>;
}

/// The real server.
pub struct Live;

impl Live {
    fn http(base: &Url) -> Http {
        Http::new(base.clone())
    }
}

/// A switch-client error as one readable line. Connection failures name
/// the OS cause, which reqwest's own message leaves out.
fn explain(e: Error) -> anyhow::Error {
    match &e {
        Error::Http(re) if re.is_connect() || re.is_timeout() => {
            let mut cause: &dyn std::error::Error = re;
            while let Some(next) = cause.source() {
                cause = next;
            }
            anyhow!("cannot connect: {cause}")
        }
        _ => anyhow!("{e}"),
    }
}

impl SetupApi for Live {
    async fn health(&self, base: &Url) -> Result<()> {
        Self::http(base).health().await.map_err(explain)
    }

    async fn register(&self, base: &Url, reg_key: &str, name: &str) -> Result<Registration> {
        let req = RegisterRequest {
            name: name.into(),
            description:
                "Scans repos for vulnerable dependencies and runs the fix loop in this room".into(),
            connector_type: "custom_rust".into(),
            integration_profile: IntegrationProfile {
                connection_model: ConnectionModel::AlwaysOn,
                message_exchange: true,
                pre_invocation_mediation: vec![],
                post_invocation_mediation: vec![],
                event_reporting: vec![],
                command_capabilities: None,
            },
            display_name: Some("Baba-Yaga".into()),
            icon_url: None,
            tools: None,
            models: None,
            metadata: None,
            overwrite: None,
        };
        match Self::http(base).register(reg_key, &req).await {
            Ok(r) => Ok(Registration::Registered(Agent {
                id: r.id,
                key: r.api_key,
            })),
            Err(Error::Conflict(_)) => Ok(Registration::NameTaken),
            Err(Error::Auth(_)) => {
                bail!("401: the registration key was refused; it must be the server's AGENT_REGISTRATION_TOKEN")
            }
            Err(e) => Err(explain(e)),
        }
    }

    async fn check_key(&self, base: &Url, agent: &Agent) -> Result<()> {
        Self::http(base)
            .list_ops(&agent.key, &agent.id)
            .await
            .map(|_| ())
            .map_err(explain)
    }

    async fn agent_detail(&self, base: &Url, agent: &Agent) -> Result<AgentInfo> {
        let v: Value = Self::http(base)
            .call_op(
                &agent.key,
                &agent.id,
                "get_agent_detail",
                json!({ "agent_id": agent.id }),
                None,
            )
            .await
            .map_err(explain)?;
        Ok(parse_agent_detail(&v))
    }

    async fn list_rooms(&self, base: &Url, agent: &Agent) -> Result<Vec<Room>> {
        let v: Value = Self::http(base)
            .call_op(&agent.key, &agent.id, "list_rooms", json!({}), None)
            .await
            .map_err(explain)?;
        Ok(parse_rooms(&v))
    }
    async fn open_addressing(
        &self,
        base: &Url,
        email: &str,
        password: &str,
        agent_id: &str,
    ) -> Result<()> {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()?;
        let login = under(base, &["gateway", "auth", "login"])?;
        let resp = client
            .post(login)
            .json(&json!({ "email": email, "password": password }))
            .send()
            .await
            .context("gateway login request failed")?;
        if !resp.status().is_success() {
            // The body is not shown: a login error has nothing a person
            // needs, and it is the one response that could echo the email.
            bail!("gateway login refused ({})", resp.status());
        }
        let cookie = session_cookie(resp.headers());
        if cookie.is_empty() {
            bail!("gateway login set no session cookie");
        }
        let url = under(base, &["gateway", "agents", agent_id, "addressing-policy"])?;
        let resp = client
            .put(url)
            .header(reqwest::header::COOKIE, cookie)
            .json(&json!({ "policy": { "rules": [] } }))
            .send()
            .await
            .context("addressing-policy request failed")?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            bail!("addressing-policy update refused ({status}): {body}");
        }
        Ok(())
    }
}

/// `segments` appended to `base`, each percent-encoded.
fn under(base: &Url, segments: &[&str]) -> Result<Url> {
    let mut url = base.clone();
    url.path_segments_mut()
        .map_err(|()| anyhow!("{base} cannot be a base URL"))?
        .pop_if_empty()
        .extend(segments);
    Ok(url)
}

/// `name=value` pairs from every `Set-Cookie`, joined for a `Cookie` header.
fn session_cookie(headers: &reqwest::header::HeaderMap) -> String {
    headers
        .get_all(reqwest::header::SET_COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .filter_map(|v| v.split(';').next())
        .map(str::trim)
        .filter(|p| p.contains('='))
        .collect::<Vec<_>>()
        .join("; ")
}

pub fn parse_rooms(v: &Value) -> Vec<Room> {
    v.as_array()
        .into_iter()
        .flatten()
        .filter(|r| !r.get("archived").and_then(Value::as_bool).unwrap_or(false))
        .filter_map(|r| {
            let id = r
                .get("room_id")
                .or_else(|| r.get("id"))
                .and_then(Value::as_str)?;
            let name = r.get("name").and_then(Value::as_str).unwrap_or(id);
            Some(Room {
                id: id.to_string(),
                name: name.to_string(),
            })
        })
        .collect()
}

pub fn parse_agent_detail(v: &Value) -> AgentInfo {
    AgentInfo {
        name: v
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        addressing_rules: v
            .get("addressing_policy")
            .and_then(|p| p.get("rules"))
            .and_then(Value::as_array)
            .cloned(),
    }
}

#[cfg(test)]
pub mod fake {
    //! A fake server for `init` and `doctor` tests.
    use std::cell::RefCell;

    use super::*;

    #[derive(Default)]
    pub struct FakeApi {
        pub healthy: bool,
        /// Names already registered.
        pub taken: RefCell<Vec<String>>,
        /// Agents whose key works, as (id, key).
        pub agents: RefCell<Vec<(String, String)>>,
        pub addressing_open: RefCell<bool>,
        pub rooms: RefCell<Vec<Room>>,
        pub admin: Option<(String, String)>,
        /// Every call, with secrets left out.
        pub calls: RefCell<Vec<String>>,
        /// Rooms that appear after the first `list_rooms` call.
        pub rooms_later: RefCell<Vec<Room>>,
    }

    impl FakeApi {
        fn log(&self, s: impl Into<String>) {
            self.calls.borrow_mut().push(s.into());
        }

        fn known(&self, a: &Agent) -> Result<()> {
            if self
                .agents
                .borrow()
                .iter()
                .any(|(i, k)| *i == a.id && *k == a.key)
            {
                Ok(())
            } else {
                bail!("authentication failed")
            }
        }
    }

    impl SetupApi for FakeApi {
        async fn health(&self, base: &Url) -> Result<()> {
            self.log(format!("health {base}"));
            if self.healthy {
                Ok(())
            } else {
                bail!("cannot connect: Connection refused")
            }
        }

        async fn register(&self, _: &Url, reg_key: &str, name: &str) -> Result<Registration> {
            self.log(format!("register {name}"));
            if reg_key != "reg-token" {
                bail!("401: the registration key was refused");
            }
            if self.taken.borrow().iter().any(|n| n == name) {
                return Ok(Registration::NameTaken);
            }
            self.taken.borrow_mut().push(name.to_string());
            let agent = Agent {
                id: format!("id-{name}"),
                key: format!("key-{name}"),
            };
            self.agents
                .borrow_mut()
                .push((agent.id.clone(), agent.key.clone()));
            Ok(Registration::Registered(agent))
        }

        async fn check_key(&self, _: &Url, a: &Agent) -> Result<()> {
            self.log(format!("check_key {}", a.id));
            self.known(a)
        }

        async fn agent_detail(&self, _: &Url, a: &Agent) -> Result<AgentInfo> {
            self.log("agent_detail");
            self.known(a)?;
            Ok(AgentInfo {
                name: a.id.trim_start_matches("id-").replace('-', "_"),
                addressing_rules: Some(if *self.addressing_open.borrow() {
                    vec![]
                } else {
                    vec![json!({"allow": "owner"})]
                }),
            })
        }

        async fn list_rooms(&self, _: &Url, a: &Agent) -> Result<Vec<Room>> {
            self.log("list_rooms");
            self.known(a)?;
            let rooms = self.rooms.borrow().clone();
            let later = std::mem::take(&mut *self.rooms_later.borrow_mut());
            self.rooms.borrow_mut().extend(later);
            Ok(rooms)
        }

        async fn open_addressing(
            &self,
            _: &Url,
            email: &str,
            password: &str,
            agent_id: &str,
        ) -> Result<()> {
            self.log(format!("open_addressing {agent_id}"));
            match &self.admin {
                Some((e, p)) if e == email && p == password => {
                    *self.addressing_open.borrow_mut() = true;
                    Ok(())
                }
                _ => bail!("gateway login refused (401 Unauthorized)"),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn observed_shapes_parse() {
        let rooms = json!([
            {"room_id": "r1", "name": "security-patrol", "description": "d",
             "connected": false, "archived": false},
            {"room_id": "r2", "name": "old", "archived": true},
            {"id": "r3"}
        ]);
        assert_eq!(
            parse_rooms(&rooms),
            vec![
                Room {
                    id: "r1".into(),
                    name: "security-patrol".into()
                },
                Room {
                    id: "r3".into(),
                    name: "r3".into()
                },
            ]
        );
        assert!(parse_rooms(&json!({})).is_empty());

        let detail = json!({"name": "baba_yaga", "owner_name": "Agent Bootstrap",
                            "addressing_policy": {"rules": []}});
        let info = parse_agent_detail(&detail);
        assert_eq!(info.name, "baba_yaga");
        assert_eq!(info.addressing_open(), Some(true));
        assert_eq!(
            parse_agent_detail(&json!({"name": "x"})).addressing_open(),
            None
        );
    }

    #[test]
    fn cookie_header_from_set_cookie() {
        let mut h = reqwest::header::HeaderMap::new();
        h.append(
            reqwest::header::SET_COOKIE,
            "session=abc; Path=/; HttpOnly".parse().unwrap(),
        );
        h.append(
            reqwest::header::SET_COOKIE,
            "csrf=x1; Path=/".parse().unwrap(),
        );
        assert_eq!(session_cookie(&h), "session=abc; csrf=x1");
    }

    #[test]
    fn gateway_urls_keep_a_base_path() {
        let base = Url::parse("http://h:1/bridge").unwrap();
        assert_eq!(
            under(&base, &["gateway", "agents", "a b", "addressing-policy"])
                .unwrap()
                .as_str(),
            "http://h:1/bridge/gateway/agents/a%20b/addressing-policy"
        );
    }

    #[test]
    fn agent_debug_hides_the_key() {
        let a = Agent {
            id: "id-1".into(),
            key: "secret-key".into(),
        };
        assert!(!format!("{a:?}").contains("secret-key"));
    }
}
