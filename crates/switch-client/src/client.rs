//! The public entry point. `Client::connect` checks the bridge, spawns the
//! connection task from `ws`, and returns the client together with an
//! `mpsc::Receiver<DomainEvent>` of domain events. Pings, reconnects and
//! control frames stay inside the task.

use serde::de::DeserializeOwned;
use serde_json::Value;
use tokio::sync::mpsc;
use url::Url;
use uuid::Uuid;

use crate::error::Error;
use crate::http::Http;
use crate::types::{DomainEvent, PostMessageResponse};
use crate::ws::{self, ConnectionHandle, WsConfig};

/// Room for events the application has not read yet. When it is full the
/// connection task waits, and pings go unanswered meanwhile.
const EVENT_CAPACITY: usize = 256;

/// A connected agent bound to one room. `Debug` redacts the key.
pub struct Client {
    pub http: Http,
    pub agent_id: String,
    agent_key: String,
    pub room_id: String,
    handle: ConnectionHandle,
}

impl std::fmt::Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Client")
            .field("base", &self.http.base().as_str())
            .field("agent_id", &self.agent_id)
            .field("agent_key", &"<redacted>")
            .field("room_id", &self.room_id)
            .field("connection_id", &self.handle.connection_id)
            .finish()
    }
}

impl Client {
    /// Check `GET /health` on `base`, then open the event connection for
    /// `room_id`. A wrong base URL fails here rather than in the task.
    pub async fn connect(
        base: Url,
        agent_id: impl Into<String>,
        agent_key: impl Into<String>,
        room_id: impl Into<String>,
    ) -> Result<(Client, mpsc::Receiver<DomainEvent>), Error> {
        let agent_id = agent_id.into();
        let agent_key = agent_key.into();
        let room_id = room_id.into();

        let http = Http::new(base.clone());
        http.health().await?;

        let cfg = WsConfig {
            ws_base: ws_base(&base)?,
            agent_id: agent_id.clone(),
            agent_key: agent_key.clone(),
            rooms: vec![room_id.clone()],
            client_name: "baba-yaga".into(),
            client_version: env!("CARGO_PKG_VERSION").into(),
        };
        let (tx, rx) = mpsc::channel(EVENT_CAPACITY);
        let handle = ws::spawn(cfg, tx);
        tracing::info!(connection_id = %handle.connection_id, %room_id, "client connecting");

        let client = Client {
            http,
            agent_id,
            agent_key,
            room_id,
            handle,
        };
        Ok((client, rx))
    }

    /// Post `content` to the configured room.
    pub async fn post(&self, content: &str) -> Result<PostMessageResponse, Error> {
        self.http
            .post_message(&self.agent_key, &self.agent_id, &self.room_id, content)
            .await
    }

    /// Call an operation bound to this connection, so operations that need a
    /// room get it from the connection.
    pub async fn op<T: DeserializeOwned>(&self, name: &str, args: Value) -> Result<T, Error> {
        self.http
            .call_op(
                &self.agent_key,
                &self.agent_id,
                name,
                args,
                Some(self.handle.connection_id),
            )
            .await
    }

    pub fn connection_id(&self) -> Uuid {
        self.handle.connection_id
    }

    /// Wait until the socket is up, so operations bound to it can run.
    pub async fn wait_connected(&self, timeout: std::time::Duration) -> Result<(), Error> {
        self.handle.wait_connected(timeout).await
    }

    /// True once the connection task has stopped; `shutdown` returns why.
    pub fn is_closed(&self) -> bool {
        self.handle.is_finished()
    }

    /// Close the socket and wait for the task. Returns the error that stopped
    /// it, if it had already stopped on its own.
    pub async fn shutdown(self) -> Result<(), Error> {
        self.handle.shutdown().await
    }
}

/// `http` -> `ws`, `https` -> `wss`, keeping host, port and path.
fn ws_base(base: &Url) -> Result<Url, Error> {
    let scheme = match base.scheme() {
        "http" => "ws",
        "https" => "wss",
        other => {
            return Err(Error::Protocol(format!(
                "base URL scheme must be http or https, got {other}"
            )))
        }
    };
    let mut ws = base.clone();
    ws.set_scheme(scheme)
        .map_err(|()| Error::Protocol(format!("cannot use {scheme} for {base}")))?;
    Ok(ws)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ws_base_maps_scheme_and_keeps_rest() {
        let u = |s: &str| ws_base(&Url::parse(s).unwrap()).map(|u| u.to_string());
        assert_eq!(
            u("http://localhost:51234").unwrap(),
            "ws://localhost:51234/"
        );
        assert_eq!(
            u("https://switch.example/bridge/").unwrap(),
            "wss://switch.example/bridge/"
        );
        assert!(u("ftp://x").is_err());
    }
}
