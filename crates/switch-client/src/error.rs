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

    /// HTTP 401, or close code 4401 on the socket.
    #[error("authentication failed")]
    Auth,

    /// A `refused` frame at open. On the wire it is `{status, detail}`; the
    /// socket then closes with 4000 + status.
    #[error("connection refused ({code}): {reason}")]
    Refused { code: u16, reason: String },

    /// An `evicted` frame. On the wire it is `{code, reason}`; `reason` here
    /// carries the eviction code (`taken_over`, `heartbeat_lapsed`, ...).
    #[error("evicted: {reason}")]
    Evicted { reason: String },

    /// HTTP 409: name already registered, or an unknown or dead connection id.
    #[error("conflict: {0}")]
    Conflict(String),

    #[error("protocol: {0}")]
    Protocol(String),
}
