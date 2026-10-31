//! HTTP calls to the agent bridge. Every call except `health` carries
//! `Authorization: Bearer <key>`.

use reqwest::{Response, StatusCode};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use url::Url;
use uuid::Uuid;

use crate::error::Error;
use crate::types::{Addressing, RegisterRequest, RegisterResponse};

const CONNECTION_ID_HEADER: &str = "X-Switch-Connection-Id";

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
        Self::with_client(base, reqwest::Client::new())
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
    /// connection. Returns the response body as-is.
    // TODO(verify): the response body of this route is not documented.
    pub async fn post_message(
        &self,
        agent_key: &str,
        agent_id: &str,
        room_id: &str,
        content: &str,
    ) -> Result<Value, Error> {
        let url = self.url(&["agents", agent_id, "message"])?;
        let resp = self
            .client
            .post(url)
            .bearer_auth(agent_key)
            .json(&json!({ "room_id": room_id, "content": content }))
            .send()
            .await?;
        let resp = check(resp).await?;
        let text = resp.text().await?;
        if text.trim().is_empty() {
            return Ok(Value::Null);
        }
        Ok(serde_json::from_str(&text)?)
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

/// Map 401 to `Auth`, 409 to `Conflict`, and any other non-2xx to `Protocol`.
async fn check(resp: Response) -> Result<Response, Error> {
    let status = resp.status();
    if status.is_success() {
        return Ok(resp);
    }
    let body = resp.text().await.unwrap_or_default();
    tracing::warn!(%status, "switch request failed");
    match status {
        StatusCode::UNAUTHORIZED => Err(Error::Auth),
        StatusCode::CONFLICT => Err(Error::Conflict(body)),
        _ => Err(Error::Protocol(format!("{status}: {body}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
