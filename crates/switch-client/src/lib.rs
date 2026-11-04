//! The Switch agent protocol: registration, operations, and the event
//! connection. No scanner code and no business logic belong here.
//!
//! Wire contract: <https://docs.switchagents.ai/switch-rooms/internals/agent-protocol>,
//! `agent-connection` and `agent-events` on the same site.

pub mod client;
pub mod error;
pub mod http;
pub mod types;
pub mod ws;

pub use error::Error;
pub use http::Http;
pub use types::{
    close_code, Addressing, Attachment, CommandPayload, ConnectOptions, ConnectionModel,
    ConnectionState, ControlFrame, DomainEvent, EventData, EventPayload, Evicted, Filter, Frame,
    Gap, IntegrationProfile, MessagePayload, Missed, Pong, PostMessageResponse, RawFrame, Refused,
    RegisterRequest, RegisterResponse, RoomJoinPayload, RoomReleased, Scope, StartFrom,
    SubscriptionChanged,
};
pub use ws::{ConnectionHandle, WsConfig};

pub type Result<T, E = Error> = std::result::Result<T, E>;
