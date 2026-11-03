//! One-off setup: register the agent with `POST /agents` and print the id
//! and API key to copy into `.env`. Run once; a second run is a 409.

use switch_client::{ConnectionModel, Error, Http, IntegrationProfile, RegisterRequest};

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
    let reg_key = std::env::var("SWITCH_REGISTRATION_KEY")
        .map_err(|_| "SWITCH_REGISTRATION_KEY is not set")?;

    let req = RegisterRequest {
        name: "baba_yaga".into(),
        description: "Scans repos for vulnerable dependencies and runs the fix loop in this room"
            .into(),
        connector_type: "custom_rust".into(),
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

    let resp = match http.register(&reg_key, &req).await {
        Ok(r) => r,
        Err(Error::Auth(body)) => {
            return Err(format!(
                "401: SWITCH_REGISTRATION_KEY must be an API key of type `registration` ({body})"
            )
            .into())
        }
        Err(Error::Conflict(body)) => {
            return Err(format!(
                "409: `{}` is already registered ({body}). Reuse the saved SWITCH_AGENT_KEY; \
                 overwrite: true would issue a new key and invalidate the old one",
                req.name
            )
            .into())
        }
        Err(e) => return Err(e.into()),
    };

    println!("SWITCH_AGENT_ID={}", resp.id);
    println!("SWITCH_AGENT_KEY={}", resp.api_key);
    Ok(())
}
