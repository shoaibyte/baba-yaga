//! Owns the WebSocket to `/agents/{agent_id}/connection/ws`: opening it with
//! the bearer key and `ConnectOptions`, answering every `ping` with a `Pong`
//! before processing events, tracking the cursor and generation from
//! `connection_state`, resuming with `start_from` and `expected_generation`
//! after a drop, and handling `gap`, `evicted`, `refused` and close codes.

// written by hand, do not generate
