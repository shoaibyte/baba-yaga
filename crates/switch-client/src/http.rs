//! HTTP calls to the agent bridge. Every call except `health` carries
//! `Authorization: Bearer <key>`.

use reqwest::Response;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use url::Url;
use uuid::Uuid;

use crate::error::Error;
use crate::types::{Addressing, PostMessageResponse, RegisterRequest, RegisterResponse};

const CONNECTION_ID_HEADER: &str = "X-Switch-Connection-Id";
/// Per-request limit for `Http::new`, so a bridge that accepts the
/// connection and never answers cannot hang the caller.
const REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

#[derive(Debug, Clone)]
pub struct Http {
    base: Url,
    client: reqwest::Client,
}

/// `{"result": ...}`, the envelope of every operation response.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct OpResult<T> {
    result: T,
}

impl Http {
    /// `base` is the bridge base URL, which is usually not the Gateway URL.
    pub fn new(base: Url) -> Self {
        let client = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .build()
            .expect("static reqwest config");
        Self::with_client(base, client)
    }

    pub fn with_client(base: Url, client: reqwest::Client) -> Self {
        Self { base, client }
    }

    pub fn base(&self) -> &Url {
        &self.base
    }

    /// `POST /agents`, authenticated with a registration key. Register once
    /// and keep the result: the API key is returned only here.
    pub async fn register(
        &self,
        reg_key: &str,
        req: &RegisterRequest,
    ) -> Result<RegisterResponse, Error> {
        let url = self.url(&["agents"])?;
        tracing::debug!(name = %req.name, "registering agent");
        let resp = self
            .client
            .post(url)
            .bearer_auth(reg_key)
            .json(req)
            .send()
            .await?;
        Ok(check(resp).await?.json().await?)
    }

    /// Set who may address the agent. Needs no connection.
    pub async fn update_addressing(
        &self,
        agent_key: &str,
        agent_id: &str,
        addressing: Addressing,
    ) -> Result<(), Error> {
        let _: Value = self
            .call_op(
                agent_key,
                agent_id,
                "update_agent_detail",
                json!({ "agent_id": agent_id, "addressing": addressing }),
                None,
            )
            .await?;
        Ok(())
    }

    /// `POST /agents/{id}/message` with `{room_id, content}`. Needs no
    /// connection. The response shape is observed, not documented; see
    /// [`PostMessageResponse`].
    pub async fn post_message(
        &self,
        agent_key: &str,
        agent_id: &str,
        room_id: &str,
        content: &str,
    ) -> Result<PostMessageResponse, Error> {
        let url = self.url(&["agents", agent_id, "message"])?;
        tracing::debug!(room_id, content_len = content.len(), "posting message");
        let resp = self
            .client
            .post(url)
            .bearer_auth(agent_key)
            .json(&json!({ "room_id": room_id, "content": content }))
            .send()
            .await?;
        Ok(check(resp).await?.json().await?)
    }

    /// `POST /agents/{id}/ops/{op}`. `args` is the arguments object itself,
    /// not wrapped. The connection id header binds the call to a room; an
    /// operation that needs a room fails without it.
    pub async fn call_op<T: DeserializeOwned>(
        &self,
        agent_key: &str,
        agent_id: &str,
        op: &str,
        args: Value,
        connection_id: Option<Uuid>,
    ) -> Result<T, Error> {
        let url = self.url(&["agents", agent_id, "ops", op])?;
        let mut req = self.client.post(url).bearer_auth(agent_key).json(&args);
        if let Some(id) = connection_id {
            req = req.header(CONNECTION_ID_HEADER, id.to_string());
        }
        tracing::debug!(op, ?connection_id, "calling operation");
        let resp = check(req.send().await?).await?;
        let wrapped: OpResult<T> = resp.json().await?;
        Ok(wrapped.result)
    }

    /// `GET /health`. Public; the probe for "is this the bridge base URL".
    pub async fn health(&self) -> Result<(), Error> {
        let url = self.url(&["health"])?;
        check(self.client.get(url).send().await?).await?;
        Ok(())
    }

    /// Append path segments to the base URL, percent-encoding each one.
    fn url(&self, segments: &[&str]) -> Result<Url, Error> {
        let mut url = self.base.clone();
        url.path_segments_mut()
            .map_err(|()| Error::Protocol(format!("base URL cannot be a base: {}", self.base)))?
            .pop_if_empty()
            .extend(segments);
        Ok(url)
    }
}

/// Pass a 2xx response through; turn anything else into an error.
async fn check(resp: Response) -> Result<Response, Error> {
    let status = resp.status();
    if status.is_success() {
        return Ok(resp);
    }
    let body = resp.text().await.unwrap_or_default();
    tracing::warn!(%status, "switch request failed");
    Err(status_to_error(status.as_u16(), body))
}

/// Map 401 to `Auth`, 409 to `Conflict` and any other status to `Status`,
/// each carrying the response body.
fn status_to_error(code: u16, body: String) -> Error {
    match code {
        401 => Error::Auth(body),
        409 => Error::Conflict(body),
        _ => Error::Status { code, body },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn other_status_becomes_typed_status_error() {
        let err = status_to_error(500, "boom".into());
        assert!(matches!(&err, Error::Status { code: 500, body } if body == "boom"));
        assert_eq!(err.to_string(), "500: boom");
        assert!(matches!(
            status_to_error(401, String::new()),
            Error::Auth(_)
        ));
        assert!(matches!(
            status_to_error(409, String::new()),
            Error::Conflict(_)
        ));
    }

    #[test]
    fn url_joins_segments_under_a_base_path() {
        let http = Http::new(Url::parse("https://example.test/bridge/").unwrap());
        let url = http.url(&["agents", "a b", "ops", "list_rooms"]).unwrap();
        assert_eq!(
            url.as_str(),
            "https://example.test/bridge/agents/a%20b/ops/list_rooms"
        );
    }

    #[test]
    fn url_without_trailing_slash() {
        let http = Http::new(Url::parse("https://example.test").unwrap());
        assert_eq!(
            http.url(&["health"]).unwrap().as_str(),
            "https://example.test/health"
        );
    }
}
