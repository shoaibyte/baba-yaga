//! Owns the WebSocket to `/agents/{agent_id}/connection/ws`: opening it with
//! the bearer key and `ConnectOptions`, answering every `ping` with a `Pong`
//! before processing events, tracking the cursor and generation from
//! `connection_state`, resuming with `start_from` and `expected_generation`
//! after a drop, and handling `gap`, `evicted`, `refused` and close codes.
//!
//! See `docs/WS_DESIGN.md` for the reasoning behind the rules below.

use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::header::{HeaderValue, AUTHORIZATION};
use tokio_tungstenite::tungstenite::Message;
use url::Url;
use uuid::Uuid;

use crate::error::Error;
use crate::types::{
    close_code, ConnectOptions, ControlFrame, DomainEvent, Evicted, Frame, Pong, RawFrame,
    StartFrom,
};

/// Ping interval assumed until `connection_state` says otherwise.
const DEFAULT_HEARTBEAT: Duration = Duration::from_secs(2);
/// With no frame for this many heartbeat intervals the socket is treated as
/// dead. A silently dropped network never delivers a close frame.
const IDLE_HEARTBEATS: f64 = 5.0;
/// Consecutive 4409 closes tolerated before giving up. A claim left by a
/// previous process lapses after the 6 s heartbeat TTL; backoff 1+2+4+8 s
/// between the first five covers that comfortably.
const MAX_CONFLICTS: u32 = 5;
/// Floor for the idle timeout, so a very short heartbeat does not cause
/// reconnects on a briefly busy machine.
const MIN_IDLE: Duration = Duration::from_secs(10);

/// Everything needed to open the socket. `Debug` redacts the key.
#[derive(Clone)]
pub struct WsConfig {
    /// `ws://` or `wss://` base of the agent bridge, e.g. `ws://localhost:8080`.
    pub ws_base: Url,
    pub agent_id: String,
    pub agent_key: String,
    /// Declared at open, so catch-up covers them.
    pub rooms: Vec<String>,
    pub client_name: String,
    pub client_version: String,
}

impl std::fmt::Debug for WsConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WsConfig")
            .field("ws_base", &self.ws_base.as_str())
            .field("agent_id", &self.agent_id)
            .field("agent_key", &"<redacted>")
            .field("rooms", &self.rooms)
            .field("client_name", &self.client_name)
            .field("client_version", &self.client_version)
            .finish()
    }
}

/// The running connection task. Dropping the handle stops the task, the same
/// as calling [`ConnectionHandle::shutdown`] without waiting for it.
#[derive(Debug)]
pub struct ConnectionHandle {
    /// Fixed for the task's whole life and reused on every reconnect. Send it
    /// as `X-Switch-Connection-Id` on operations bound to this connection.
    pub connection_id: Uuid,
    shutdown: Option<oneshot::Sender<()>>,
    join: JoinHandle<Result<(), Error>>,
    connected: watch::Receiver<bool>,
}

impl ConnectionHandle {
    /// Ask the task to close the socket and wait for it to finish. Returns the
    /// task's result: `Ok` on a clean stop, or the error that stopped it
    /// earlier (eviction, refusal, auth).
    pub async fn shutdown(mut self) -> Result<(), Error> {
        if let Some(tx) = self.shutdown.take() {
            // The task may already have stopped; then there is no receiver.
            let _ = tx.send(());
        }
        (&mut self.join)
            .await
            .map_err(|e| Error::Protocol(format!("connection task failed: {e}")))?
    }

    /// Wait until a `connection_state` has arrived on the current socket.
    /// Operations bound to the connection fail before that. Errors if the
    /// task stops first or `timeout` passes.
    pub async fn wait_connected(&self, timeout: Duration) -> Result<(), Error> {
        let mut rx = self.connected.clone();
        // Map away the borrow `wait_for` returns before `rx` goes out of scope.
        let waited =
            tokio::time::timeout(timeout, async { rx.wait_for(|c| *c).await.map(|_| ()) }).await;
        match waited {
            Ok(Ok(())) => Ok(()),
            Ok(Err(_)) => Err(Error::Protocol("connection task stopped".into())),
            Err(_) => Err(Error::Protocol(format!(
                "not connected after {}s",
                timeout.as_secs()
            ))),
        }
    }

    /// True while a socket is open and has sent `connection_state`.
    pub fn is_connected(&self) -> bool {
        *self.connected.borrow()
    }

    /// True once the task has stopped for any reason.
    pub fn is_finished(&self) -> bool {
        self.join.is_finished()
    }
}

/// Spawn the connection task. Domain events arrive on `events`; pings,
/// reconnects and control frames are handled inside and never reach it. The
/// task stops when `events`' receiver is dropped, on shutdown, or on a final
/// error (taken over, closed, refused, auth).
pub fn spawn(cfg: WsConfig, events: mpsc::Sender<DomainEvent>) -> ConnectionHandle {
    let connection_id = Uuid::new_v4();
    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    let (connected_tx, connected_rx) = watch::channel(false);
    let join = tokio::spawn(run(cfg, connection_id, events, shutdown_rx, connected_tx));
    ConnectionHandle {
        connection_id,
        shutdown: Some(shutdown_tx),
        join,
        connected: connected_rx,
    }
}

// ---------------------------------------------------------------------------
// Task
// ---------------------------------------------------------------------------

/// State kept across reconnects. `connection_id` lives outside: it never
/// changes.
#[derive(Debug, Default)]
struct Resume {
    /// Highest domain event id handed to the application. Sent in every pong
    /// and as `start_from` on reconnect.
    cursor: u64,
    /// From the last `connection_state`; sent as `expected_generation` so a
    /// reconnect only takes back this client's own connection.
    generation: Option<u64>,
}

/// How one socket session ended.
#[derive(Debug)]
enum SessionEnd {
    /// Shutdown requested, or the handle was dropped.
    Shutdown,
    /// The application dropped the event receiver.
    ReceiverGone,
    /// A final error decided during the session (`refused`, bad config).
    Stop(Error),
    /// The socket ended or an `evicted` arrived; `after_close` decides.
    Closed {
        code: Option<u16>,
        evicted: Option<Evicted>,
        why: String,
    },
}

async fn run(
    cfg: WsConfig,
    connection_id: Uuid,
    events: mpsc::Sender<DomainEvent>,
    mut shutdown: oneshot::Receiver<()>,
    connected: watch::Sender<bool>,
) -> Result<(), Error> {
    let mut resume = Resume::default();
    let mut backoff = Backoff::default();
    // Consecutive sessions that ended in 4409; any other end resets it.
    let mut conflicts = 0u32;
    loop {
        let end = session(
            &cfg,
            connection_id,
            &mut resume,
            &mut backoff,
            &events,
            &mut shutdown,
            &connected,
        )
        .await;
        connected.send_replace(false);
        let why = match end {
            SessionEnd::Shutdown => {
                tracing::info!(%connection_id, "connection shut down");
                return Ok(());
            }
            SessionEnd::ReceiverGone => {
                tracing::info!(%connection_id, "event receiver dropped; stopping");
                return Ok(());
            }
            SessionEnd::Stop(e) => {
                tracing::error!(%connection_id, error = %e, "connection stopped");
                return Err(e);
            }
            SessionEnd::Closed { code, evicted, why } => {
                if code == Some(close_code::CONFLICT) && evicted.is_none() {
                    conflicts += 1;
                } else {
                    conflicts = 0;
                }
                match after_close(code, evicted.as_ref(), conflicts) {
                    Decision::Reconnect => why,
                    Decision::Stop(e) => {
                        tracing::error!(%connection_id, error = %e, "connection stopped");
                        return Err(e);
                    }
                }
            }
        };
        let delay = backoff.next_delay();
        tracing::warn!(
            %connection_id,
            attempt = backoff.attempt(),
            delay_s = delay.as_secs(),
            cursor = resume.cursor,
            reason = %why,
            "reconnecting"
        );
        tokio::select! {
            _ = tokio::time::sleep(delay) => {}
            _ = &mut shutdown => {
                tracing::info!(%connection_id, "connection shut down");
                return Ok(());
            }
        }
    }
}

/// Open one socket and read it until it ends. Pings are answered inline, so
/// nothing else ever writes to the socket.
async fn session(
    cfg: &WsConfig,
    connection_id: Uuid,
    resume: &mut Resume,
    backoff: &mut Backoff,
    events: &mpsc::Sender<DomainEvent>,
    shutdown: &mut oneshot::Receiver<()>,
    connected: &watch::Sender<bool>,
) -> SessionEnd {
    let req = match build_request(cfg, connection_id, resume) {
        Ok(r) => r,
        Err(e) => return SessionEnd::Stop(e),
    };
    let ws = tokio::select! {
        r = tokio_tungstenite::connect_async(req) => r,
        _ = &mut *shutdown => return SessionEnd::Shutdown,
    };
    let ws = match ws {
        Ok((ws, _resp)) => ws,
        Err(e) => return connect_failed(e),
    };
    tracing::debug!(%connection_id, cursor = resume.cursor, "socket open");
    let (mut tx, mut rx) = ws.split();
    let mut idle = idle_timeout(DEFAULT_HEARTBEAT.as_secs_f64());

    loop {
        let next = tokio::select! {
            r = tokio::time::timeout(idle, rx.next()) => r,
            _ = &mut *shutdown => {
                let _ = tx.send(Message::Close(None)).await;
                return SessionEnd::Shutdown;
            }
        };
        let msg = match next {
            Err(_) => {
                return closed(None, None, format!("no frame for {}s", idle.as_secs()));
            }
            Ok(None) => return closed(None, None, "socket ended".into()),
            Ok(Some(Err(e))) => return closed(None, None, format!("socket error: {e}")),
            Ok(Some(Ok(m))) => m,
        };
        let text = match msg {
            Message::Text(t) => t,
            Message::Close(frame) => {
                let code = frame.as_ref().map(|f| u16::from(f.code));
                let why = match &frame {
                    Some(f) => format!("closed with {} {}", u16::from(f.code), f.reason),
                    None => "closed without a code".into(),
                };
                return closed(code, None, why);
            }
            // WebSocket-level pings are answered by tungstenite itself.
            _ => continue,
        };

        let frame =
            match serde_json::from_str::<RawFrame>(text.as_str()).and_then(RawFrame::into_frame) {
                Ok(f) => f,
                Err(e) => {
                    tracing::warn!(%connection_id, error = %e, "unparsed frame skipped");
                    tracing::debug!(%connection_id, frame = %text.as_str(), "unparsed frame");
                    continue;
                }
            };

        match frame {
            Frame::Control(ControlFrame::Ping) => {
                let pong = match serde_json::to_string(&Pong {
                    cursor: resume.cursor,
                }) {
                    Ok(p) => p,
                    Err(e) => return SessionEnd::Stop(e.into()),
                };
                if let Err(e) = tx.send(Message::Text(pong.into())).await {
                    return closed(None, None, format!("pong failed: {e}"));
                }
            }
            Frame::Event(ev) => {
                resume.cursor = ev.id;
                tracing::debug!(%connection_id, seq = ev.id, kind = %ev.data.kind, "event");
                // A full channel blocks here, and pings go unanswered until
                // the application catches up. If that outlasts the heartbeat
                // the server evicts with `heartbeat_lapsed` and we resume.
                tokio::select! {
                    r = events.send(ev) => {
                        if r.is_err() {
                            let _ = tx.send(Message::Close(None)).await;
                            return SessionEnd::ReceiverGone;
                        }
                    }
                    _ = &mut *shutdown => {
                        let _ = tx.send(Message::Close(None)).await;
                        return SessionEnd::Shutdown;
                    }
                }
            }
            Frame::Control(ControlFrame::ConnectionState(s)) => {
                resume.generation = Some(s.generation);
                if let Some(h) = s.heartbeat_interval_seconds {
                    idle = idle_timeout(h);
                }
                backoff.reset();
                connected.send_replace(true);
                tracing::info!(
                    %connection_id,
                    generation = s.generation,
                    protocol = ?s.protocol,
                    rooms = ?s.rooms,
                    server_cursor = ?s.cursor,
                    cursor = resume.cursor,
                    "connected"
                );
            }
            Frame::Control(ControlFrame::Gap(g)) => {
                tracing::warn!(
                    %connection_id,
                    from = ?g.from_sequence,
                    resumed_at = g.resumed_at,
                    rooms = ?g.rooms,
                    all_rooms = g.all_rooms,
                    reason = ?g.reason,
                    "gap: events lost; history re-read is the application's job"
                );
                resume.cursor = g.resumed_at;
            }
            Frame::Control(ControlFrame::Evicted(e)) => {
                tracing::warn!(%connection_id, code = %e.code, reason = ?e.reason, "evicted");
                let why = format!("evicted: {}", e.code);
                return closed(None, Some(e), why);
            }
            Frame::Control(ControlFrame::Refused(r)) => {
                let code = close_code::for_refusal(r.status);
                tracing::warn!(%connection_id, code, detail = %r.detail, "refused");
                // A conflict may be a previous process's claim that has not
                // lapsed yet; let `after_close` count and retry it.
                if code == close_code::CONFLICT {
                    return closed(Some(code), None, format!("refused {code}: {}", r.detail));
                }
                return SessionEnd::Stop(Error::Refused {
                    code,
                    reason: r.detail,
                });
            }
            Frame::Control(ControlFrame::SubscriptionChanged(c)) => {
                tracing::info!(%connection_id, rooms = ?c.rooms, reason = ?c.reason, "subscription changed");
            }
            Frame::Control(ControlFrame::RoomReleased(r)) => {
                tracing::info!(%connection_id, room_id = %r.room_id, session_id = ?r.session_id, "room released");
            }
            Frame::Control(ControlFrame::Unknown { event, .. }) => {
                tracing::debug!(%connection_id, %event, "unknown control frame");
            }
        }
    }
}

fn closed(code: Option<u16>, evicted: Option<Evicted>, why: String) -> SessionEnd {
    SessionEnd::Closed { code, evicted, why }
}

/// A failed upgrade. A 401 on the upgrade response is final; anything else
/// (server down, refused TCP, DNS) is worth retrying.
fn connect_failed(e: tokio_tungstenite::tungstenite::Error) -> SessionEnd {
    use tokio_tungstenite::tungstenite::Error as WsError;
    if let WsError::Http(resp) = &e {
        if resp.status().as_u16() == 401 {
            return SessionEnd::Stop(Error::Auth("websocket upgrade returned 401".into()));
        }
    }
    closed(None, None, format!("connect failed: {e}"))
}

fn idle_timeout(heartbeat_s: f64) -> Duration {
    let h = if heartbeat_s.is_finite() && heartbeat_s > 0.0 {
        heartbeat_s.min(60.0)
    } else {
        DEFAULT_HEARTBEAT.as_secs_f64()
    };
    Duration::from_secs_f64(h * IDLE_HEARTBEATS).max(MIN_IDLE)
}

/// The socket URL and upgrade request, with the bearer header. Resume state
/// goes into `start_from` and `expected_generation`.
fn build_request(
    cfg: &WsConfig,
    connection_id: Uuid,
    resume: &Resume,
) -> Result<tokio_tungstenite::tungstenite::handshake::client::Request, Error> {
    let mut opts = ConnectOptions::new(connection_id);
    opts.rooms = cfg.rooms.clone();
    opts.client = Some(cfg.client_name.clone());
    opts.client_version = Some(cfg.client_version.clone());
    opts.expected_generation = resume.generation;
    if resume.cursor > 0 {
        opts.start_from = StartFrom::Sequence(resume.cursor);
    }

    let mut url = cfg.ws_base.clone();
    url.path_segments_mut()
        .map_err(|()| Error::Protocol(format!("ws base cannot be a base: {}", cfg.ws_base)))?
        .pop_if_empty()
        .extend(["agents", cfg.agent_id.as_str(), "connection", "ws"]);
    url.set_query(Some(&opts.query_string()));

    let mut req = url.as_str().into_client_request()?;
    let bearer = HeaderValue::from_str(&format!("Bearer {}", cfg.agent_key))
        .map_err(|_| Error::Protocol("agent key is not a valid header value".into()))?;
    req.headers_mut().insert(AUTHORIZATION, bearer);
    Ok(req)
}

// ---------------------------------------------------------------------------
// Reconnect rules
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub(crate) enum Decision {
    Reconnect,
    Stop(Error),
}

/// What to do after a session ends. An `evicted` frame, when one arrived,
/// decides on its own: the close that follows it (normally 1000) adds
/// nothing. Without one, the close code decides; no code at all means the
/// network or the server went away, so reconnect. `conflicts` is the number
/// of consecutive sessions, this one included, that ended in 4409: the first
/// `MAX_CONFLICTS - 1` are retried, the next one stops.
pub(crate) fn after_close(
    code: Option<u16>,
    last_evicted: Option<&Evicted>,
    conflicts: u32,
) -> Decision {
    if let Some(e) = last_evicted {
        return match e.code.as_str() {
            "heartbeat_lapsed" | "no_stream" | "unfenced" | "connection_error" => {
                Decision::Reconnect
            }
            "taken_over" => Decision::Stop(Error::Evicted {
                reason: "taken_over".into(),
            }),
            "closed" => Decision::Stop(Error::Evicted {
                reason: format!(
                    "closed: {}",
                    e.reason.as_deref().unwrap_or("no reason given")
                ),
            }),
            // A code this client does not know. Retrying under backoff is
            // safer than stopping a long-running agent for good.
            _ => Decision::Reconnect,
        };
    }
    match code {
        Some(close_code::UNAUTHORIZED) => {
            Decision::Stop(Error::Auth("socket closed with 4401".into()))
        }
        Some(close_code::CONFLICT) if conflicts < MAX_CONFLICTS => Decision::Reconnect,
        Some(c @ (close_code::BAD_REQUEST | close_code::FORBIDDEN | close_code::CONFLICT)) => {
            Decision::Stop(Error::Refused {
                code: c,
                reason: format!("socket closed with {c}"),
            })
        }
        // 1011, 1012, 1000 without an eviction, other codes, or none.
        _ => Decision::Reconnect,
    }
}

/// Reconnect delays: 1, 2, 4, 8, 16, then 30 s for every later attempt.
/// Reset after a `connection_state` proves the connection works.
#[derive(Debug, Default)]
pub(crate) struct Backoff {
    attempt: u32,
}

impl Backoff {
    const CAP: Duration = Duration::from_secs(30);

    pub(crate) fn next_delay(&mut self) -> Duration {
        let secs = 1u64 << self.attempt.min(5);
        self.attempt = self.attempt.saturating_add(1);
        Duration::from_secs(secs).min(Self::CAP)
    }

    /// The number of the attempt the last `next_delay` was for, from 1.
    pub(crate) fn attempt(&self) -> u32 {
        self.attempt
    }

    pub(crate) fn reset(&mut self) {
        self.attempt = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn evicted(code: &str, reason: Option<&str>) -> Evicted {
        Evicted {
            code: code.into(),
            reason: reason.map(Into::into),
            room_id: None,
        }
    }

    #[test]
    fn service_restart_and_server_error_reconnect() {
        assert!(matches!(
            after_close(Some(1012), None, 0),
            Decision::Reconnect
        ));
        assert!(matches!(
            after_close(Some(1011), None, 0),
            Decision::Reconnect
        ));
        assert!(matches!(after_close(None, None, 0), Decision::Reconnect));
    }

    #[test]
    fn unauthorized_stops_with_auth() {
        assert!(matches!(
            after_close(Some(4401), None, 0),
            Decision::Stop(Error::Auth(_))
        ));
    }

    #[test]
    fn bad_request_and_forbidden_stop_with_refused() {
        for c in [4400, 4403] {
            match after_close(Some(c), None, 0) {
                Decision::Stop(Error::Refused { code, .. }) => assert_eq!(code, c),
                other => panic!("{c}: expected refused, got {other:?}"),
            }
        }
    }

    #[test]
    fn conflict_retries_four_times_then_stops_on_the_fifth() {
        for n in 1..MAX_CONFLICTS {
            assert!(
                matches!(after_close(Some(4409), None, n), Decision::Reconnect),
                "conflict {n} should retry"
            );
        }
        match after_close(Some(4409), None, MAX_CONFLICTS) {
            Decision::Stop(Error::Refused { code, .. }) => assert_eq!(code, 4409),
            other => panic!("expected refused after {MAX_CONFLICTS} conflicts, got {other:?}"),
        }
    }

    #[test]
    fn taken_over_stops_even_after_normal_close() {
        let e = evicted("taken_over", Some("another open took the connection"));
        match after_close(Some(1000), Some(&e), 0) {
            Decision::Stop(Error::Evicted { reason }) => assert_eq!(reason, "taken_over"),
            other => panic!("expected evicted stop, got {other:?}"),
        }
    }

    #[test]
    fn closed_eviction_carries_reason() {
        let e = evicted("closed", Some("agent deleted"));
        match after_close(Some(1000), Some(&e), 0) {
            Decision::Stop(Error::Evicted { reason }) => {
                assert_eq!(reason, "closed: agent deleted")
            }
            other => panic!("expected evicted stop, got {other:?}"),
        }
    }

    #[test]
    fn resumable_evictions_reconnect() {
        for code in [
            "heartbeat_lapsed",
            "no_stream",
            "unfenced",
            "connection_error",
            "new_code",
        ] {
            let e = evicted(code, None);
            assert!(
                matches!(after_close(Some(1000), Some(&e), 0), Decision::Reconnect),
                "{code}"
            );
        }
    }

    #[test]
    fn backoff_doubles_to_cap_and_resets() {
        let mut b = Backoff::default();
        let secs: Vec<u64> = (0..8).map(|_| b.next_delay().as_secs()).collect();
        assert_eq!(secs, vec![1, 2, 4, 8, 16, 30, 30, 30]);
        assert_eq!(b.attempt(), 8);
        b.reset();
        assert_eq!(b.next_delay().as_secs(), 1);
        assert_eq!(b.attempt(), 1);
    }

    #[test]
    fn idle_timeout_has_floor_and_scales() {
        assert_eq!(idle_timeout(2.0), Duration::from_secs(10));
        assert_eq!(idle_timeout(4.0), Duration::from_secs(20));
        assert_eq!(idle_timeout(f64::NAN), Duration::from_secs(10));
        assert_eq!(idle_timeout(1e9), Duration::from_secs(300));
    }

    #[test]
    fn request_carries_resume_state_and_never_the_key_in_url() {
        let cfg = WsConfig {
            ws_base: Url::parse("ws://localhost:8080/").unwrap(),
            agent_id: "agent-1".into(),
            agent_key: "sk-secret".into(),
            rooms: vec!["room-a".into()],
            client_name: "baba-yaga".into(),
            client_version: "0.1.0".into(),
        };
        let id = Uuid::nil();
        let fresh = build_request(&cfg, id, &Resume::default()).unwrap();
        let q = fresh.uri().to_string();
        assert!(q.starts_with("ws://localhost:8080/agents/agent-1/connection/ws?"));
        assert!(q.contains("start_from=head"));
        assert!(!q.contains("expected_generation"));
        assert!(!q.contains("sk-secret"));
        assert_eq!(fresh.headers()[AUTHORIZATION], "Bearer sk-secret");

        let resumed = Resume {
            cursor: 9_007_194_959_773_696,
            generation: Some(8_589_934_592),
        };
        let q = build_request(&cfg, id, &resumed).unwrap().uri().to_string();
        assert!(q.contains("start_from=9007194959773696"));
        assert!(q.contains("expected_generation=8589934592"));
        assert!(!format!("{cfg:?}").contains("sk-secret"));
    }
}
