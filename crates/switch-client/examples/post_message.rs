//! One-off check: post into a room as an already registered agent, over
//! plain HTTP with no WebSocket connection. The message is the first CLI
//! argument, or a default line.

use switch_client::{Error, Http};

const DEFAULT_MESSAGE: &str = "Baba-Yaga online. First message over the protocol.";

#[tokio::main]
async fn main() -> std::process::ExitCode {
    match run().await {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            // Display, not Debug: keeps response bodies readable.
            eprintln!("error: {e}");
            std::process::ExitCode::FAILURE
        }
    }
}

async fn run() -> Result<(), Box<dyn std::error::Error>> {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt::init();

    let base = std::env::var("SWITCH_URL").map_err(|_| "SWITCH_URL is not set")?;
    let agent_id = std::env::var("SWITCH_AGENT_ID").map_err(|_| "SWITCH_AGENT_ID is not set")?;
    let agent_key = std::env::var("SWITCH_AGENT_KEY").map_err(|_| "SWITCH_AGENT_KEY is not set")?;
    let room_id = std::env::var("SWITCH_ROOM_ID").map_err(|_| "SWITCH_ROOM_ID is not set")?;
    let content = std::env::args()
        .nth(1)
        .unwrap_or_else(|| DEFAULT_MESSAGE.to_string());

    let http = Http::new(base.parse()?);
    http.health().await.map_err(|e| match &e {
        Error::Http(re) if re.is_connect() || re.is_timeout() => {
            // reqwest's Display stops at "error sending request"; the OS
            // cause (connection refused, DNS failure) is at the end of the chain.
            let mut cause: &dyn std::error::Error = re;
            while let Some(next) = cause.source() {
                cause = next;
            }
            format!("cannot reach {base}: {cause}")
        }
        _ => format!(
            "{base} is not the bridge base URL (GET /health failed: {e}); \
             the Gateway URL will not work"
        ),
    })?;

    let resp = match http
        .post_message(&agent_key, &agent_id, &room_id, &content)
        .await
    {
        Ok(v) => v,
        Err(Error::Auth(_)) => {
            return Err("401: SWITCH_AGENT_KEY is not a valid agent API key".into())
        }
        Err(
            e @ Error::Status {
                code: 403 | 404, ..
            },
        ) => {
            return Err(format!(
                "{e}\nthe agent may not be a member of room {room_id}; \
                 invite it with /invite-agent in the channel"
            )
            .into())
        }
        Err(e) => return Err(e.into()),
    };

    println!("{}", serde_json::to_string_pretty(&resp)?);
    Ok(())
}
