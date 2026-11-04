//! Frame fixtures: the control frames are verbatim from switch-core 0.31
//! (agent-protocol 9); the message event is hand-written to the documented
//! domain shape. Fixtures marked "hand-written" have not been seen on the
//! live server; their `data` follows the agent-connection and agent-events
//! docs, wrapped in the observed envelope.

use switch_client::types::{close_code, AttachmentGroup};
use switch_client::{
    Addressing, ConnectOptions, ConnectionModel, ControlFrame, EventPayload, Filter, Frame,
    IntegrationProfile, Pong, RawFrame, RegisterRequest, RegisterResponse, Scope, StartFrom,
};
use uuid::Uuid;

fn parse(json: &str) -> Frame {
    serde_json::from_str::<RawFrame>(json)
        .and_then(RawFrame::into_frame)
        .expect("frame parses")
}

fn control(json: &str) -> ControlFrame {
    match parse(json) {
        Frame::Control(c) => c,
        Frame::Event(e) => panic!("expected control frame, got {e:?}"),
    }
}

#[test]
fn message_event() {
    let json = r#"{"event":"message","data":{"type":"message","room_id":"f48120fd-a3d6-4928-af45-ff30ad757d2c","bridge_id":null,"channel_type":"channel_public","payload":{"addressed":true,"sender":"@switch-user-a1b2:switch","sender_name":"Kari","message_id":"$m1","body":"@baba-yaga status","timestamp":1761900000000,"thread_id":null,"attachments":[],"some_future_field":1},"sequence":15},"id":15}"#;
    let Frame::Event(ev) = parse(json) else {
        panic!("expected domain event");
    };
    assert_eq!(ev.event, "message");
    assert_eq!(ev.id, 15);
    assert_eq!(ev.data.sequence, ev.id);
    assert_eq!(ev.data.kind, "message");
    assert_eq!(ev.data.bridge_id, None);
    assert_eq!(ev.data.channel_type.as_deref(), Some("channel_public"));
    let EventPayload::Message(m) = ev.data.typed_payload().unwrap() else {
        panic!("expected message payload");
    };
    assert!(m.addressed);
    assert_eq!(m.body, "@baba-yaga status");
    assert_eq!(m.timestamp, 1_761_900_000_000);
}

#[test]
fn connection_state_frame_observed() {
    let json = r#"{"event":"connection_state","data":{"connection_id":"575744a3-87e4-4da3-9e75-9b486ce46a21","agent_id":"5742bb2a-e565-4e88-a19c-e43ea1a2125a","generation":8589934592,"scope":"single","filter":"all","spawn_capable":false,"rooms":["f48120fd-a3d6-4928-af45-ff30ad757d2c"],"cursor":0,"position":{"s":4294967296,"r:f48120fd-a3d6-4928-af45-ff30ad757d2c":14},"protocol":9,"heartbeat_interval_seconds":2.0,"server":{"version":"0.31.0","contracts":{"agent-protocol":{"speaks":9,"accepts":1}}},"client":{"speaks":null,"accepts":null,"artifact":"baba-yaga","version":"0.1.0"}}}"#;
    let ControlFrame::ConnectionState(s) = control(json) else {
        panic!("expected connection_state");
    };
    assert_eq!(s.generation, 8_589_934_592);
    assert_eq!(s.protocol, Some(9));
    assert_eq!(s.heartbeat_interval_seconds, Some(2.0));
    assert_eq!(s.cursor, Some(0));
    assert_eq!(s.scope, Scope::Single);
    assert_eq!(s.filter, Filter::All);
    assert_eq!(s.rooms, vec!["f48120fd-a3d6-4928-af45-ff30ad757d2c"]);
    assert_eq!(s.position.unwrap()["s"], 4_294_967_296u64);
    assert_eq!(s.server["version"], "0.31.0");
}

#[test]
fn ping_frame_observed() {
    assert!(matches!(
        control(r#"{"event":"ping","data":{}}"#),
        ControlFrame::Ping
    ));
}

#[test]
fn evicted_frame_observed() {
    let json = r#"{"event":"evicted","data":{"code":"heartbeat_lapsed","reason":"heartbeat lapsed; reopen the stream and resume from your cursor","room_id":null}}"#;
    let ControlFrame::Evicted(e) = control(json) else {
        panic!("expected evicted");
    };
    assert_eq!(e.code, "heartbeat_lapsed");
    assert_eq!(e.room_id, None);
    assert!(e.reason.unwrap().starts_with("heartbeat lapsed"));
}

#[test]
fn unknown_event_name_does_not_fail() {
    let json = r#"{"event":"something_new","data":{"anything":[1,2,3]}}"#;
    let ControlFrame::Unknown { event, data } = control(json) else {
        panic!("expected unknown");
    };
    assert_eq!(event, "something_new");
    assert_eq!(data["anything"][2], 3);
}

// Hand-written.
#[test]
fn command_event() {
    let json = r#"{"event":"command","data":{"type":"command","room_id":"!room:switch","bridge_id":"b1","channel_type":null,"payload":{"command":"reset","args":"reviewer","user_id":"@switch-user-a1b2:switch","user_name":"Kari","thread_id":null},"sequence":7},"id":7}"#;
    let Frame::Event(ev) = parse(json) else {
        panic!("expected domain event");
    };
    assert_eq!(ev.id, 7);
    let EventPayload::Command(c) = ev.data.typed_payload().unwrap() else {
        panic!("expected command payload");
    };
    assert_eq!(c.command, "reset");
    assert_eq!(c.args, "reviewer");
}

// Hand-written.
#[test]
fn room_join_event() {
    let json = r#"{"event":"room_join","data":{"type":"room_join","room_id":"!room:switch","bridge_id":null,"channel_type":null,"payload":{"member":"@switch-agent-9f3c:switch","member_name":"fixer","timestamp":1761900000000,"listening":true},"sequence":8},"id":8}"#;
    let Frame::Event(ev) = parse(json) else {
        panic!("expected domain event");
    };
    let EventPayload::RoomJoin(j) = ev.data.typed_payload().unwrap() else {
        panic!("expected room_join payload");
    };
    assert!(j.listening);
    assert_eq!(j.member_name.as_deref(), Some("fixer"));
}

// Hand-written. An unknown event name is a control frame even when it carries
// an `id`; the id is dropped and `data` is kept.
#[test]
fn unknown_domain_event_becomes_unknown_control() {
    let json = r#"{"event":"reaction","data":{"type":"reaction","room_id":"!r:s","payload":{"x":1},"sequence":9},"id":9}"#;
    let ControlFrame::Unknown { event, data } = control(json) else {
        panic!("expected unknown");
    };
    assert_eq!(event, "reaction");
    assert_eq!(data["payload"]["x"], 1);
}

// Hand-written.
#[test]
fn gap_frame() {
    let json = r#"{"event":"gap","data":{"from_sequence":100,"resumed_at":250,"resumed_position":null,"rooms":["!a:switch"],"all_rooms":false,"reason":"retention"}}"#;
    let ControlFrame::Gap(g) = control(json) else {
        panic!("expected gap");
    };
    assert_eq!(g.from_sequence, Some(100));
    assert_eq!(g.resumed_at, 250);
    assert_eq!(g.rooms, vec!["!a:switch"]);
    assert!(!g.all_rooms);
}

// Hand-written.
#[test]
fn subscription_changed_frame() {
    let json =
        r#"{"event":"subscription_changed","data":{"rooms":["!b:switch"],"reason":"claimed"}}"#;
    let ControlFrame::SubscriptionChanged(c) = control(json) else {
        panic!("expected subscription_changed");
    };
    assert_eq!(c.rooms, vec!["!b:switch"]);
    assert_eq!(c.reason.as_deref(), Some("claimed"));
}

// Hand-written.
#[test]
fn room_released_frame() {
    let json = r#"{"event":"room_released","data":{"room_id":"!a:switch","session_id":null}}"#;
    let ControlFrame::RoomReleased(r) = control(json) else {
        panic!("expected room_released");
    };
    assert_eq!(r.room_id, "!a:switch");
    assert_eq!(r.session_id, None);
}

// Hand-written.
#[test]
fn refused_frame() {
    let json = r#"{"event":"refused","data":{"status":409,"detail":"room already claimed"}}"#;
    let ControlFrame::Refused(r) = control(json) else {
        panic!("expected refused");
    };
    assert_eq!(r.status, 409);
    assert_eq!(r.detail, "room already claimed");
    assert_eq!(close_code::for_refusal(r.status), close_code::CONFLICT);
}

// Hand-written. A known event whose `data` does not match its type is an
// error, not `Unknown`.
#[test]
fn malformed_known_control_frame_errors() {
    let raw: RawFrame = serde_json::from_str(r#"{"event":"gap","data":{"rooms":[]}}"#).unwrap();
    assert!(raw.into_frame().is_err());
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

#[test]
fn post_message_response_observed_shape() {
    let resp: switch_client::PostMessageResponse =
        serde_json::from_str(r#"{"event_id": "sw_01d31d8ec85342ffa2e078e87db2d211", "ok": true}"#)
            .unwrap();
    assert_eq!(resp.event_id, "sw_01d31d8ec85342ffa2e078e87db2d211");
    assert!(resp.ok);
}
