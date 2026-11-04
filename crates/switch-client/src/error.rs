use thiserror::Error;

/// Every failure the Switch client can report.
#[derive(Debug, Error)]
pub enum Error {
    #[error("http: {0}")]
    Http(#[from] reqwest::Error),

    #[error("websocket: {0}")]
    Ws(#[from] tokio_tungstenite::tungstenite::Error),

    #[error("json: {0}")]
    Json(#[from] serde_json::Error),

    /// HTTP 401, or close code 4401 on the socket. Carries the response
    /// body or close reason, which may be empty.
    #[error("authentication failed: {0}")]
    Auth(String),

    /// A `refused` frame at open. On the wire it is `{status, detail}`; the
    /// socket then closes with 4000 + status.
    #[error("connection refused ({code}): {reason}")]
    Refused { code: u16, reason: String },

    /// A final `evicted` frame. On the wire it is `{code, reason, room_id}`.
    /// `reason` here is `taken_over`, or `closed: <server reason>`. Resumable
    /// evictions never surface: the connection task reconnects instead.
    #[error("evicted: {reason}")]
    Evicted { reason: String },

    /// HTTP 409: name already registered, or an unknown or dead connection id.
    #[error("conflict: {0}")]
    Conflict(String),

    /// Any other non-2xx HTTP status, with the response body (may be empty).
    #[error("{code}: {body}")]
    Status { code: u16, body: String },

    /// A response that does not have the documented shape (bad JSON, a
    /// missing `result`), or a client-side misuse. Never an HTTP status.
    #[error("protocol: {0}")]
    Protocol(String),
}
