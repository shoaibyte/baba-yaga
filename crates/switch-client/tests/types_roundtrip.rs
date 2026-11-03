//! One literal JSON example per frame type, written by hand from the
//! agent-connection and agent-events docs pages.

use switch_client::types::{close_code, AttachmentGroup};
use switch_client::{
    Addressing, ConnectOptions, ConnectionModel, ControlFrame, EventPayload, Filter, Frame,
    IntegrationProfile, Pong, RegisterRequest, RegisterResponse, Scope, StartFrom,
};
use uuid::Uuid;

fn parse(json: &str) -> Frame {
    serde_json::from_str(json).expect("frame parses")
}

fn control(json: &str) -> ControlFrame {
    match parse(json) {
        Frame::Control(c) => c,
        Frame::Event(e) => panic!("expected control frame, got {e:?}"),
    }
}

#[test]
fn message_event() {
    let json = r#"{"event": "message",
        "data": {"type": "message", "room_id": "!room:switch", "bridge_id": null,
                 "channel_type": "channel_public",
                 "payload": {"addressed": true, "sender": "@switch-user-a1b2:switch",
                             "sender_name": "Kari", "message_id": "$m1", "body": "@baba-yaga status",
                             "timestamp": 1761900000000, "thread_id": null,
                             "attachments": [{"filename": "a.txt", "mimetype": "text/plain",
                                              "size": 12, "mxc": "mxc://switch/abc", "msgtype": "m.file"}],
                             "some_future_field": 1},
                 "sequence": 4813},
        "id": 4813}"#;
    let Frame::Event(ev) = parse(json) else {
        panic!("expected domain event");
    };
    assert_eq!(ev.event, "message");
    assert_eq!(ev.id, 4813);
    assert_eq!(ev.data.sequence, ev.id);
    assert_eq!(ev.data.bridge_id, None);
    assert_eq!(ev.data.channel_type.as_deref(), Some("channel_public"));
    let EventPayload::Message(m) = ev.data.typed_payload().unwrap() else {
        panic!("expected message payload");
    };
    assert!(m.addressed);
    assert_eq!(m.body, "@baba-yaga status");
    assert_eq!(m.timestamp, 1_761_900_000_000);
    assert_eq!(m.attachments.len(), 1);
    assert_eq!(m.attachments[0].size, Some(12));
}

#[test]
fn message_with_attachment_group() {
    let json = r#"{"addressed": false, "sender": "@switch-user-a1b2:switch", "message_id": "$m2",
        "body": "", "timestamp": 1, "attachments": [],
        "com.switch.attachment_group": {"id": "g1", "index": 0, "total": 2}}"#;
    let m: switch_client::MessagePayload = serde_json::from_str(json).unwrap();
    let AttachmentGroup { id, index, total } = m.attachment_group.unwrap();
    assert_eq!((id.as_str(), index, total), ("g1", 0, 2));
}

#[test]
fn command_event() {
    let json = r#"{"event": "command",
        "data": {"type": "command", "room_id": "!room:switch", "bridge_id": "b1", "channel_type": null,
                 "payload": {"command": "reset", "args": "reviewer", "user_id": "@switch-user-a1b2:switch",
                             "user_name": "Kari", "thread_id": null},
                 "sequence": 7},
        "id": 7}"#;
    let Frame::Event(ev) = parse(json) else {
        panic!("expected domain event");
    };
    let EventPayload::Command(c) = ev.data.typed_payload().unwrap() else {
        panic!("expected command payload");
    };
    assert_eq!(c.command, "reset");
    assert_eq!(c.args, "reviewer");
}

#[test]
fn room_join_event() {
    let json = r#"{"event": "room_join",
        "data": {"type": "room_join", "room_id": "!room:switch", "bridge_id": null, "channel_type": null,
                 "payload": {"member": "@switch-agent-9f3c:switch", "member_name": "fixer",
                             "timestamp": 1761900000000, "listening": true},
                 "sequence": 8},
        "id": 8}"#;
    let Frame::Event(ev) = parse(json) else {
        panic!("expected domain event");
    };
    let EventPayload::RoomJoin(j) = ev.data.typed_payload().unwrap() else {
        panic!("expected room_join payload");
    };
    assert!(j.listening);
    assert_eq!(j.member_name.as_deref(), Some("fixer"));
}

#[test]
fn unknown_event_type_keeps_payload() {
    let json = r#"{"event": "reaction",
        "data": {"type": "reaction", "room_id": "!r:s", "payload": {"x": 1}, "sequence": 9},
        "id": 9}"#;
    let Frame::Event(ev) = parse(json) else {
        panic!("expected domain event");
    };
    assert!(matches!(
        ev.data.typed_payload().unwrap(),
        EventPayload::Unknown(_)
    ));
}

#[test]
fn connection_state_frame() {
    let json = r#"{"type": "connection_state",
        "connection_id": "6f1c1d2e-3a4b-4c5d-8e9f-0a1b2c3d4e5f", "agent_id": "agent-123",
        "generation": 3, "scope": "single", "filter": "all", "spawn_capable": false,
        "rooms": ["!a:switch", "!b:switch"], "cursor": 4813, "protocol": 6,
        "heartbeat_interval_seconds": 2,
        "server": {"version": "1.2.3", "protocol": 6, "protocol_accepts": 1},
        "client": {"client": "baba-yaga", "client_version": "0.1.0"}}"#;
    let ControlFrame::ConnectionState(s) = control(json) else {
        panic!("expected connection_state");
    };
    assert_eq!(s.generation, 3);
    assert_eq!(s.scope, Scope::Single);
    assert_eq!(s.filter, Filter::All);
    assert_eq!(s.rooms, vec!["!a:switch", "!b:switch"]);
    assert_eq!(s.cursor, Some(4813));
    assert_eq!(s.heartbeat_interval_seconds, Some(2.0));
}

#[test]
fn ping_frame() {
    assert!(matches!(control(r#"{"type": "ping"}"#), ControlFrame::Ping));
}

#[test]
fn gap_frame() {
    let json = r#"{"type": "gap", "from_sequence": 100, "resumed_at": 250,
        "resumed_position": null, "rooms": ["!a:switch"], "all_rooms": false, "reason": "retention"}"#;
    let ControlFrame::Gap {
        resumed_at,
        all_rooms,
        ..
    } = control(json)
    else {
        panic!("expected gap");
    };
    assert_eq!(resumed_at, 250);
    assert!(!all_rooms);
}

#[test]
fn evicted_frame() {
    let json = r#"{"type": "evicted", "code": "taken_over", "reason": "another open took the connection"}"#;
    let ControlFrame::Evicted { code, .. } = control(json) else {
        panic!("expected evicted");
    };
    assert_eq!(code, "taken_over");
}

#[test]
fn subscription_changed_frame() {
    let json = r#"{"type": "subscription_changed", "rooms": ["!b:switch"], "reason": "claimed"}"#;
    let ControlFrame::SubscriptionChanged { rooms, .. } = control(json) else {
        panic!("expected subscription_changed");
    };
    assert_eq!(rooms, vec!["!b:switch"]);
}

#[test]
fn room_released_frame() {
    let json = r#"{"type": "room_released", "room_id": "!a:switch", "session_id": null}"#;
    let ControlFrame::RoomReleased {
        room_id,
        session_id,
    } = control(json)
    else {
        panic!("expected room_released");
    };
    assert_eq!(room_id, "!a:switch");
    assert_eq!(session_id, None);
}

#[test]
fn refused_frame() {
    let json = r#"{"type": "refused", "status": 409, "detail": "room already claimed"}"#;
    let ControlFrame::Refused { status, detail } = control(json) else {
        panic!("expected refused");
    };
    assert_eq!(status, 409);
    assert_eq!(detail, "room already claimed");
    assert_eq!(close_code::for_refusal(status), close_code::CONFLICT);
}

#[test]
fn unknown_control_frame_does_not_fail() {
    let json = r#"{"type": "something_new", "anything": [1, 2, 3]}"#;
    assert!(matches!(control(json), ControlFrame::Unknown));
}

#[test]
fn pong_serializes() {
    let json = serde_json::to_value(Pong { cursor: 4813 }).unwrap();
    assert_eq!(json, serde_json::json!({"type": "pong", "cursor": 4813}));
    let back: Pong = serde_json::from_value(json).unwrap();
    assert_eq!(back.cursor, 4813);
}

#[test]
fn connect_options_query_string() {
    let id = Uuid::parse_str("6f1c1d2e-3a4b-4c5d-8e9f-0a1b2c3d4e5f").unwrap();
    assert_eq!(
        ConnectOptions::new(id).query_string(),
        "connection_id=6f1c1d2e-3a4b-4c5d-8e9f-0a1b2c3d4e5f&scope=single&filter=all&start_from=head"
    );

    let mut opts = ConnectOptions::new(id);
    opts.rooms = vec!["!a:switch".into(), "!b:switch".into()];
    opts.scope = Scope::All;
    opts.filter = Filter::Addressed;
    opts.start_from = StartFrom::Sequence(4813);
    opts.expected_generation = Some(3);
    opts.client = Some("baba-yaga".into());
    opts.client_version = Some("0.1.0".into());
    assert_eq!(
        opts.query_string(),
        "connection_id=6f1c1d2e-3a4b-4c5d-8e9f-0a1b2c3d4e5f&scope=all&filter=addressed\
         &start_from=4813&rooms=%21a%3Aswitch%2C%21b%3Aswitch&expected_generation=3\
         &client=baba-yaga&client_version=0.1.0"
    );
}

#[test]
fn register_request_and_response() {
    let req = RegisterRequest {
        name: "baba-yaga".into(),
        description: "Dependency vulnerability patrol".into(),
        connector_type: "baba-yaga".into(),
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
    let json = serde_json::to_value(&req).unwrap();
    assert_eq!(json["integration_profile"]["connection_model"], "always_on");
    assert_eq!(
        json["integration_profile"]["pre_invocation_mediation"],
        serde_json::json!([])
    );
    assert!(json.get("icon_url").is_none());

    let resp: RegisterResponse =
        serde_json::from_str(r#"{"id": "agent-123", "api_key": "k", "extra": true}"#).unwrap();
    assert_eq!(resp.id, "agent-123");

    assert_eq!(
        serde_json::to_value(Addressing::OwnerAndOwnerAgents).unwrap(),
        "owner_and_owner_agents"
    );
}

#[test]
fn register_response_debug_redacts_api_key() {
    let resp: switch_client::RegisterResponse =
        serde_json::from_str(r#"{"id":"agent-1","api_key":"sk-not-a-real-key"}"#).unwrap();
    let debug = format!("{resp:?}");
    assert!(debug.contains("agent-1"));
    assert!(!debug.contains("sk-not-a-real-key"));
    assert_eq!(resp.api_key, "sk-not-a-real-key");
}
