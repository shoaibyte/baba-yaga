//! Replies "pong" to any addressed message containing "ping". Survives
//! reconnects; Ctrl-C ends it.

use switch_client::{Client, EventPayload};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();
    let env = |k: &str| std::env::var(k).map_err(|_| format!("{k} is not set"));

    let (client, mut events) = Client::connect(
        env("SWITCH_URL")?.parse()?,
        env("SWITCH_AGENT_ID")?,
        env("SWITCH_AGENT_KEY")?,
        env("SWITCH_ROOM_ID")?,
    )
    .await?;

    while let Some(ev) = events.recv().await {
        if let Ok(EventPayload::Message(m)) = ev.data.typed_payload() {
            if m.addressed && m.body.contains("ping") {
                let r = client.post("pong").await?;
                tracing::info!(seq = ev.id, event_id = %r.event_id, "answered ping");
            }
        }
    }
    // The task closed the channel: it stopped for good. Say why.
    client.shutdown().await?;
    Ok(())
}
