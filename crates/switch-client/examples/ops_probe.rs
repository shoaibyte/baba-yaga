//! Learn the operation shapes from the live server: the catalogue, thread
//! replies (op and plain route), room documents and participants. Every step
//! prints its result and carries on when one fails. Posts three "[probe]"
//! messages and deletes the probe document it creates.

use std::time::Duration;

use serde_json::{json, Value};
use switch_client::{Client, Error};

const SHOWN_OPS: [&str; 6] = [
    "post_message",
    "send_targeted_message",
    "create_room_document",
    "update_room_document",
    "read_context",
    "list_participants",
];

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt().init();
    let env = |k: &str| std::env::var(k).map_err(|_| format!("{k} is not set"));
    let base = env("SWITCH_URL")?;
    let key = env("SWITCH_AGENT_KEY")?;
    let room = env("SWITCH_ROOM_ID")?;

    let (client, _events) = Client::connect(
        base.parse()?,
        env("SWITCH_AGENT_ID")?,
        key.clone(),
        room.clone(),
    )
    .await?;
    client.wait_connected(Duration::from_secs(10)).await?;

    // (a) The catalogue.
    let ops = client.http.list_ops(&key, &client.agent_id).await?;
    let names: Vec<&str> = ops["operations"]
        .as_object()
        .map(|m| m.keys().map(String::as_str).collect())
        .unwrap_or_default();
    println!("OPS ({}) {}", names.len(), names.join(" "));
    for name in SHOWN_OPS {
        println!("SCHEMA {name} {}", ops["operations"][name]["input_schema"]);
    }

    // (b) A root message.
    let root = client.post("[probe] stage 3: thread root").await?;
    println!("ROOT event_id={}", root.event_id);

    // (c) Two ways to reply in its thread.
    let via_op: Result<Value, Error> = client
        .op(
            "post_message",
            json!({"body": "[probe] stage 3: reply via op", "thread_id": root.event_id}),
        )
        .await;
    show("REPLY_OP", &via_op);

    let via_route = reqwest::Client::new()
        .post(format!(
            "{}/agents/{}/message",
            base.trim_end_matches('/'),
            client.agent_id
        ))
        .bearer_auth(&key)
        .json(&json!({
            "room_id": room,
            "content": "[probe] stage 3: reply via /message",
            "thread_id": root.event_id,
        }))
        .send()
        .await?;
    let status = via_route.status();
    println!(
        "REPLY_ROUTE status={status} body={}",
        via_route.text().await?
    );

    // Where did the replies land? A reply in the thread shows up in the
    // root's `replies`; one that ignored thread_id shows up as its own root.
    let ctx: Result<Value, Error> = client.op("read_context", json!({"limit": 10})).await;
    match &ctx {
        Ok(ctx) => {
            for t in ctx["threads"].as_array().into_iter().flatten() {
                let id = t["root"]["id"].as_str().unwrap_or_default();
                let body = t["root"]["body"].as_str().unwrap_or_default();
                if !body.starts_with("[probe] stage 3") {
                    continue;
                }
                let replies: Vec<&str> = t["replies"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|r| r["body"].as_str())
                    .collect();
                println!("THREAD root={id} body={body:?} replies={replies:?}");
            }
            if let Some(first) = ctx["threads"].as_array().and_then(|a| a.last()) {
                println!("CONTEXT_ENTRY {}", first["root"]);
            }
            println!(
                "CONTEXT truncated={} oldest_timestamp={}",
                ctx["truncated"], ctx["oldest_timestamp"]
            );
        }
        Err(e) => println!("CONTEXT error: {e}"),
    }

    // (d) A room document, updated once, then deleted again.
    let created: Result<Value, Error> = client
        .op(
            "create_room_document",
            json!({
                "name": "baba-yaga-probe",
                "description": "Probe document, safe to delete",
                "instructions": "Created by the baba-yaga ops probe; ignore it.",
                "content": "probe",
            }),
        )
        .await;
    show("DOC_CREATE", &created);
    if let Ok(doc) = &created {
        if let Some(id) = doc["document_id"].as_str() {
            let updated: Result<Value, Error> = client
                .op(
                    "update_room_document",
                    json!({"document_id": id, "content": "probe 2"}),
                )
                .await;
            show("DOC_UPDATE", &updated);
            let loaded: Result<Value, Error> =
                client.op("load_internal_documents", json!({})).await;
            show("DOC_LOAD", &loaded);
            let deleted: Result<Value, Error> = client
                .op("delete_room_document", json!({"document_id": id}))
                .await;
            show("DOC_DELETE", &deleted);
        }
    }

    let participants: Result<Value, Error> = client.op("list_participants", json!({})).await;
    show("PARTICIPANTS", &participants);

    client.shutdown().await?;
    Ok(())
}

fn show(label: &str, r: &Result<Value, Error>) {
    match r {
        Ok(v) => println!("{label} ok {v}"),
        Err(e) => println!("{label} error: {e}"),
    }
}
