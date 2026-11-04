//! Learn the operation shapes from the live server: the catalogue, thread
//! replies (op and plain route), room documents and participants. Every step
//! prints its result and carries on when one fails. Posts three "[probe]"
//! messages and deletes the probe document it creates.
//!
//! Observed on switch-core 0.31: the `post_message` op with `thread_id`
//! replies in the thread; `POST /message` ignores `thread_id`.

use std::time::Duration;

use serde_json::json;
use switch_client::{Client, NewDocument};

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

    // (c) Two ways to reply in its thread. Only the op works (observed);
    // the route is kept to notice if that changes.
    match client
        .post_in_thread(&root.event_id, "[probe] stage 3: reply via op")
        .await
    {
        Ok(r) => println!("REPLY_OP ok event_id={}", r.event_id),
        Err(e) => println!("REPLY_OP error: {e}"),
    }
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
    match client.read_context(10).await {
        Ok(ctx) => {
            for t in ctx
                .threads
                .iter()
                .filter(|t| t.root.body.starts_with("[probe] stage 3"))
            {
                let replies: Vec<&str> = t.replies.iter().map(|r| r.body.as_str()).collect();
                println!(
                    "THREAD root={} body={:?} replies={replies:?}",
                    t.root.id, t.root.body
                );
            }
            println!(
                "CONTEXT threads={} truncated={} oldest_timestamp={:?}",
                ctx.threads.len(),
                ctx.truncated,
                ctx.oldest_timestamp
            );
        }
        Err(e) => println!("CONTEXT error: {e}"),
    }

    // (d) A room document, updated once, then deleted again.
    let doc = NewDocument {
        name: "baba-yaga-probe".into(),
        description: "Probe document, safe to delete".into(),
        instructions: "Created by the baba-yaga ops probe; ignore it.".into(),
        content: "probe".into(),
    };
    match client.create_document(&doc).await {
        Ok(created) => {
            println!("DOC_CREATE ok document_id={}", created.document_id);
            match client
                .update_document(&created.document_id, "probe 2")
                .await
            {
                Ok(u) => println!("DOC_UPDATE ok status={}", u.status),
                Err(e) => println!("DOC_UPDATE error: {e}"),
            }
            match client.delete_document(&created.document_id).await {
                Ok(d) => println!("DOC_DELETE ok status={}", d.status),
                Err(e) => println!("DOC_DELETE error: {e}"),
            }
        }
        Err(e) => println!("DOC_CREATE error: {e}"),
    }

    match client.list_participants().await {
        Ok(ps) => {
            for p in ps {
                println!(
                    "PARTICIPANT name={} type={} status={:?}",
                    p.name, p.kind, p.status
                );
            }
        }
        Err(e) => println!("PARTICIPANTS error: {e}"),
    }

    client.shutdown().await?;
    Ok(())
}
