//! Wire types for the Switch agent protocol.
//!
//! Field names follow the docs pages `agent-protocol`, `agent-connection` and
//! `agent-events` under <https://docs.switchagents.ai/switch-rooms/internals/>.
//! No type uses `deny_unknown_fields`: the server may add fields at any time.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use url::form_urlencoded;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Registration: POST /agents
// ---------------------------------------------------------------------------

/// Body of `POST /agents`. `name` must match `^[a-z0-9][a-z0-9._-]*$`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegisterRequest {
    pub name: String,
    pub description: String,
    /// Free-form label; Switch does not check it against a list.
    pub connector_type: String,
    pub integration_profile: IntegrationProfile,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub models: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Value>,
    /// Re-registering an existing name with `overwrite: true` issues a new
    /// API key and invalidates the old one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub overwrite: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IntegrationProfile {
    pub connection_model: ConnectionModel,
    pub message_exchange: bool,
    /// Send `[]` unless needed.
    #[serde(default)]
    pub pre_invocation_mediation: Vec<Value>,
    #[serde(default)]
    pub post_invocation_mediation: Vec<Value>,
    #[serde(default)]
    pub event_reporting: Vec<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command_capabilities: Option<Value>,
}

/// How Switch reports the agent's liveness. The docs say this set is
/// expected to be removed, so nothing should depend on it staying as is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConnectionModel {
    AlwaysOn,
    SessionAddressable,
    AutoSession,
    SessionPassive,
}

/// Response of `POST /agents`. The API key is returned once. `Debug`
/// redacts it so a stray `{:?}` or `tracing` field cannot leak it.
#[derive(Clone, Serialize, Deserialize)]
pub struct RegisterResponse {
    pub id: String,
    pub api_key: String,
}

impl std::fmt::Debug for RegisterResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RegisterResponse")
            .field("id", &self.id)
            .field("api_key", &"<redacted>")
            .finish()
    }
}

/// Response of `POST /agents/{id}/message`. Not documented: this shape was
/// observed on switch-core 0.31, e.g.
/// `{"event_id": "sw_01d3...", "ok": true}`. Unknown fields are ignored.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PostMessageResponse {
    pub event_id: String,
    pub ok: bool,
}

/// Who may address the agent, set through the `update_agent_detail` operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Addressing {
    OwnerOnly,
    OwnerAndOwnerAgents,
    Anyone,
}

// ---------------------------------------------------------------------------
// Connection options: GET /agents/{agent_id}/connection/ws?...
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Scope {
    /// One room at a time.
    #[default]
    Single,
    /// Every room the agent belongs to.
    All,
}

impl Scope {
    pub fn as_str(self) -> &'static str {
        match self {
            Scope::Single => "single",
            Scope::All => "all",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Filter {
    /// Everything in the covered rooms.
    #[default]
    All,
    /// Only notifiable events. Never delivers `command` events.
    Addressed,
}

impl Filter {
    pub fn as_str(self) -> &'static str {
        match self {
            Filter::All => "all",
            Filter::Addressed => "addressed",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StartFrom {
    #[default]
    Head,
    /// Resume after this per-agent sequence number.
    Sequence(u64),
}

/// Query parameters for opening the socket. Reuse `connection_id` on every
/// reconnect, and pass `expected_generation` from the last `connection_state`
/// so a reconnect only takes back this client's own connection.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectOptions {
    pub connection_id: Uuid,
    /// Claimed at open, with takeover. Declare rooms here rather than after
    /// open, or catch-up skips their buffered events.
    #[serde(default)]
    pub rooms: Vec<String>,
    #[serde(default)]
    pub scope: Scope,
    #[serde(default)]
    pub filter: Filter,
    #[serde(default)]
    pub start_from: StartFrom,
    #[serde(default)]
    pub expected_generation: Option<u64>,
    #[serde(default)]
    pub spawn_capable: bool,
    #[serde(default)]
    pub protocol: Option<u32>,
    #[serde(default)]
    pub protocol_accepts: Option<u32>,
    #[serde(default)]
    pub client: Option<String>,
    #[serde(default)]
    pub client_version: Option<String>,
}

impl ConnectOptions {
    pub fn new(connection_id: Uuid) -> Self {
        Self {
            connection_id,
            rooms: Vec::new(),
            scope: Scope::default(),
            filter: Filter::default(),
            start_from: StartFrom::default(),
            expected_generation: None,
            spawn_capable: false,
            protocol: None,
            protocol_accepts: None,
            client: None,
            client_version: None,
        }
    }

    /// The query string for the socket URL, without the leading `?`.
    pub fn query_string(&self) -> String {
        let mut q = form_urlencoded::Serializer::new(String::new());
        q.append_pair("connection_id", &self.connection_id.to_string());
        q.append_pair("scope", self.scope.as_str());
        q.append_pair("filter", self.filter.as_str());
        match self.start_from {
            StartFrom::Head => q.append_pair("start_from", "head"),
            StartFrom::Sequence(n) => q.append_pair("start_from", &n.to_string()),
        };
        if !self.rooms.is_empty() {
            q.append_pair("rooms", &self.rooms.join(","));
        }
        if let Some(g) = self.expected_generation {
            q.append_pair("expected_generation", &g.to_string());
        }
        if self.spawn_capable {
            q.append_pair("spawn_capable", "true");
        }
        if let Some(p) = self.protocol {
            q.append_pair("protocol", &p.to_string());
        }
        if let Some(p) = self.protocol_accepts {
            q.append_pair("protocol_accepts", &p.to_string());
        }
        if let Some(c) = &self.client {
            q.append_pair("client", c);
        }
        if let Some(v) = &self.client_version {
            q.append_pair("client_version", v);
        }
        q.finish()
    }
}

// ---------------------------------------------------------------------------
// Frames
// ---------------------------------------------------------------------------

/// One JSON text message on the socket: a domain event (carries `event` and
/// `id`) or a control frame (carries `type`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Frame {
    Event(DomainEvent),
    Control(ControlFrame),
}

/// `{"event": "message", "data": {...}, "id": 4813}`. `id` is a JSON number
/// and equals `data.sequence`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DomainEvent {
    pub event: String,
    pub data: EventData,
    pub id: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventData {
    /// `message`, `command` or `room_join`.
    #[serde(rename = "type")]
    pub kind: String,
    pub room_id: String,
    #[serde(default)]
    pub bridge_id: Option<String>,
    #[serde(default)]
    pub channel_type: Option<String>,
    #[serde(default)]
    pub payload: Value,
    /// Per-agent sequence across all rooms; not a room `seq`.
    pub sequence: u64,
}

impl EventData {
    /// Decode `payload` according to `type`. Unknown event types are kept as
    /// raw JSON rather than failing.
    pub fn typed_payload(&self) -> Result<EventPayload, serde_json::Error> {
        Ok(match self.kind.as_str() {
            "message" => EventPayload::Message(serde_json::from_value(self.payload.clone())?),
            "command" => EventPayload::Command(serde_json::from_value(self.payload.clone())?),
            "room_join" => EventPayload::RoomJoin(serde_json::from_value(self.payload.clone())?),
            _ => EventPayload::Unknown(self.payload.clone()),
        })
    }
}

/// A decoded domain event payload. Serialize only: the variant is chosen by
/// `EventData::kind`, so it cannot be deserialized on its own.
#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
pub enum EventPayload {
    Message(MessagePayload),
    Command(CommandPayload),
    RoomJoin(RoomJoinPayload),
    Unknown(Value),
}

/// Control frames, tagged by `type`. They carry no `id` and are never filtered.
// TODO(verify): the docs show the JSON shape of the domain envelope and of the
// pong, but not of control frames. Assumed flat `{"type": "...", ...fields}`
// like the pong.
// TODO(verify): the classification diagram on agent-events lists four control
// frames; agent-connection lists seven (adds ping, refused, room_released).
// Following agent-connection, which is the page that defines them.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ControlFrame {
    /// Always the first frame on a connection.
    ConnectionState(ConnectionState),
    /// Answer immediately with a `Pong`, before processing events.
    Ping,
    Gap {
        #[serde(default)]
        from_sequence: Option<u64>,
        /// Take this as the new cursor.
        resumed_at: u64,
        #[serde(default)]
        resumed_position: Option<Value>,
        #[serde(default)]
        rooms: Vec<String>,
        /// True when the loss covers every room the agent is in.
        #[serde(default)]
        all_rooms: bool,
        #[serde(default)]
        reason: Option<String>,
    },
    Evicted {
        /// `taken_over` (final), `heartbeat_lapsed`, `no_stream`, `unfenced`,
        /// `connection_error` (resume), or `closed` (read `reason`).
        code: String,
        #[serde(default)]
        reason: Option<String>,
    },
    SubscriptionChanged {
        #[serde(default)]
        rooms: Vec<String>,
        #[serde(default)]
        reason: Option<String>,
    },
    /// Sent only to clients declaring agent-protocol 6 or later.
    RoomReleased {
        room_id: String,
        #[serde(default)]
        session_id: Option<String>,
    },
    /// Sent at open just before the socket closes with 4000 + `status`.
    Refused {
        status: u16,
        #[serde(default)]
        detail: String,
    },
    /// Any frame type this client does not know yet.
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectionState {
    pub connection_id: Uuid,
    pub agent_id: String,
    /// Keep for `expected_generation` on the next reconnect.
    pub generation: u64,
    pub scope: Scope,
    pub filter: Filter,
    #[serde(default)]
    pub spawn_capable: bool,
    /// The covered room set, sorted. The server's answer, not an echo.
    #[serde(default)]
    pub rooms: Vec<String>,
    // TODO(verify): the docs do not say whether `cursor` can be null on a
    // first connect at head.
    #[serde(default)]
    pub cursor: Option<u64>,
    #[serde(default)]
    pub protocol: Option<u32>,
    // TODO(verify): the docs say pings come "every 2 seconds" and also give
    // this field. Prefer the field when present.
    #[serde(default)]
    pub heartbeat_interval_seconds: Option<f64>,
    #[serde(default)]
    pub server: Value,
    #[serde(default)]
    pub client: Value,
}

/// The only frame a client sends: `{"type": "pong", "cursor": 4813}`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename = "pong")]
pub struct Pong {
    /// The highest sequence processed.
    pub cursor: u64,
}

// ---------------------------------------------------------------------------
// Event payloads
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessagePayload {
    /// Act on a message only when this is true.
    #[serde(default)]
    pub addressed: bool,
    /// Participant id `@<localpart>:<server>`. Opaque: match, don't parse.
    pub sender: String,
    #[serde(default)]
    pub sender_name: Option<String>,
    pub message_id: String,
    #[serde(default)]
    pub body: String,
    /// Milliseconds.
    pub timestamp: i64,
    #[serde(default)]
    pub thread_id: Option<String>,
    #[serde(default)]
    pub attachments: Vec<Attachment>,
    // TODO(verify): agent-events says batch markers are in the event
    // "content"; it does not say whether they reach the agent payload.
    #[serde(
        default,
        rename = "com.switch.attachment_group",
        skip_serializing_if = "Option::is_none"
    )]
    pub attachment_group: Option<AttachmentGroup>,
}

/// A pointer to media, never bytes. Fetch content from the media routes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Attachment {
    #[serde(default)]
    pub filename: Option<String>,
    #[serde(default)]
    pub mimetype: Option<String>,
    #[serde(default)]
    pub size: Option<u64>,
    #[serde(default)]
    pub mxc: Option<String>,
    #[serde(default)]
    pub msgtype: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AttachmentGroup {
    pub id: String,
    pub index: u32,
    pub total: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandPayload {
    pub command: String,
    // TODO(verify): documented as "empty by default" and holding a role name
    // for `reset` and `compact`, but its JSON type is not given.
    #[serde(default)]
    pub args: Value,
    pub user_id: String,
    #[serde(default)]
    pub user_name: Option<String>,
    #[serde(default)]
    pub thread_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoomJoinPayload {
    pub member: String,
    #[serde(default)]
    pub member_name: Option<String>,
    // TODO(verify): unit not given for room_join; message timestamps are ms.
    pub timestamp: i64,
    #[serde(default)]
    pub listening: bool,
}

// ---------------------------------------------------------------------------
// Close codes
// ---------------------------------------------------------------------------

/// WebSocket close codes from agent-connection.
pub mod close_code {
    /// Normal close, usually after an `evicted` frame.
    pub const NORMAL: u16 = 1000;
    /// The server failed to deliver.
    pub const SERVER_ERROR: u16 = 1011;
    /// Sent by the web server on shutdown, not by Switch. Reconnect.
    pub const SERVICE_RESTART: u16 = 1012;
    /// A refusal closes with this plus the `refused` frame's status.
    pub const REFUSED_BASE: u16 = 4000;
    /// Missing `connection_id`, or a bad `scope`, `filter` or `start_from`.
    pub const BAD_REQUEST: u16 = 4400;
    /// Missing or invalid `Authorization`. No `refused` frame precedes it.
    pub const UNAUTHORIZED: u16 = 4401;
    /// A declared room can't be served.
    pub const FORBIDDEN: u16 = 4403;
    /// Taken over, protocol mismatch, room already claimed, too many
    /// connections, or `managed_by_controller`.
    pub const CONFLICT: u16 = 4409;

    /// The close code that follows a `refused` frame with this status.
    pub fn for_refusal(status: u16) -> u16 {
        REFUSED_BASE + status
    }
}
