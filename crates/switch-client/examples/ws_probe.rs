use futures_util::{SinkExt, StreamExt};
use switch_client::{ConnectOptions, ControlFrame, Frame, Pong, RawFrame};
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::header::AUTHORIZATION;
use tokio_tungstenite::tungstenite::Message;
use uuid::Uuid;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt().init();

    let base = std::env::var("SWITCH_URL").map_err(|_| "SWITCH_URL is not set")?;
    let agent_id = std::env::var("SWITCH_AGENT_ID").map_err(|_| "SWITCH_AGENT_ID is not set")?;
    let agent_key = std::env::var("SWITCH_AGENT_KEY").map_err(|_| "SWITCH_AGENT_KEY is not set")?;
    let room_id = std::env::var("SWITCH_ROOM_ID").map_err(|_| "SWITCH_ROOM_ID is not set")?;

    let ws_base = base
        .replacen("https://", "wss://", 1)
        .replacen("http://", "ws://", 1);

    let mut opts = ConnectOptions::new(Uuid::new_v4());
    opts.rooms = vec![room_id.clone()];
    opts.client = Some("baba-yaga".into());
    opts.client_version = Some(env!("CARGO_PKG_VERSION").into());
    let url = format!(
        "{ws_base}/agents/{agent_id}/connection/ws?{}",
        opts.query_string()
    );

    let mut req = url.into_client_request()?;
    req.headers_mut()
        .insert(AUTHORIZATION, format!("Bearer {agent_key}").parse()?);
    let (ws, resp) = connect_async(req).await?;
    tracing::info!(status = %resp.status(), "socket open");

    let (mut tx, mut rx) = ws.split();
    let mut cursor: u64 = 0;
    while let Some(msg) = rx.next().await {
        let msg = msg?;
        match msg {
            Message::Text(text) => {
                println!("RAW {text}");
                match serde_json::from_str::<RawFrame>(&text).and_then(RawFrame::into_frame) {
                    Ok(Frame::Control(ControlFrame::Ping)) => {
                        let pong = serde_json::to_string(&Pong { cursor })?;
                        tx.send(Message::Text(pong.into())).await?;
                    }
                    Ok(Frame::Event(ev)) => {
                        cursor = ev.id;
                        println!("EVENT seq={} type={}", ev.id, ev.data.kind);
                    }
                    Ok(other) => println!("CTRL {other:?}"),
                    Err(e) => println!("UNPARSED {e}"),
                }
            }
            Message::Close(frame) => {
                println!("CLOSE {frame:?}");
                break;
            }
            _ => {}
        }
    }

    Ok(())
}
