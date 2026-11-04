# How the Switch connection works

Code: `crates/switch-client/src/ws.rs` (the task) and `client.rs` (the front door).

## One task owns the socket

`Client::connect` first calls `GET /health`, so a wrong `SWITCH_URL` fails at once with a clear error. Then it starts one background task with `ws::spawn` and hands the application two things: the `Client`, for posting and calling operations, and a channel receiver that delivers domain events (`message`, `command`, `room_join`, and any new event type that carries an `id`).

The task is the only code that touches the socket. It opens it, reads every frame, answers pings, and reopens it when it drops. The application never sees a ping, a reconnect, or any other control frame. It only sees a steady stream of events, or the end of that stream when the task stops for good.

## Three pieces of state

The task keeps three values for its whole life. They let a new socket continue where the old one stopped.

- **connection_id**: a UUID picked once when the task starts and sent on every reconnect. The server uses it to recognise "the same client coming back" instead of a second client. Operations that need a room send it as `X-Switch-Connection-Id`.
- **cursor**: the id of the last domain event the task handed to the application. Every pong reports it, and a reconnect asks to start right after it (`start_from=<cursor>`), so no event is skipped or repeated. Ids are packed numbers above 2^53. The code only stores, compares and sends them, never adds to them.
- **generation**: taken from each `connection_state` frame and sent back as `expected_generation` on the next reconnect. It means "take back my own connection, not someone else's". If another process has since claimed the agent, the server refuses instead of silently stealing it back.

## What each frame does

- **ping** (every 2 seconds): send `{"type":"pong","cursor":N}` straight away. Nothing else writes to the socket, so the pong never waits behind other work.
- **connection_state** (always first): store the generation, note the heartbeat interval, log the rooms, and reset the reconnect delay, because the connection has just proven it works.
- **domain event**: set the cursor to its id, then put it on the channel. If the application has dropped its receiver, nobody is listening any more, so the task closes the socket and stops cleanly.
- **gap**: the server has lost some events (retention, for example). The task logs a warning and moves the cursor to `resumed_at`. Fetching the missing history is the application's job.
- **evicted**: the server is ending this connection. The code says whether to come back (see below).
- **refused**: the server will not open this connection. Stop, except for status 409, which is retried as described below.
- **subscription_changed**, **room_released**: log them and carry on.
- **anything unknown**: log it at debug level and carry on, so a newer server cannot break the client.

## When to reconnect and when to stop

When a socket ends, one pure function, `after_close`, decides what happens next. It is unit-tested on its own.

If an `evicted` frame arrived, its code decides, and the close that follows does not matter:

- `heartbeat_lapsed`, `no_stream`, `unfenced`, `connection_error`: temporary, so reconnect and resume from the cursor.
- `taken_over`: another process now owns this agent. Stop; reconnecting would only fight it.
- `closed`: the server closed the agent deliberately. Stop, and keep the server's reason in the error.
- A code we have never seen: reconnect, under the same backoff.

With no eviction, the close code decides:

- 4401: the key is wrong. Stop with an auth error.
- 4400, 4403: bad request or room not allowed. Stop with a refusal.
- 4409 (conflict, often "room already claimed"): retry with backoff. A process that has just restarted can find its old claim still alive; that claim lapses after the 6-second heartbeat TTL, and the backoff of 1, 2, 4 and 8 seconds outlasts it. After five 4409s in a row, stop with a refusal. A `refused` frame with status 409 counts the same way.
- 1011, 1012, any other code, or no close frame at all (network drop, server crash): reconnect.

A network that disappears silently never sends a close frame. So if no frame at all arrives for five heartbeat intervals (at least 10 seconds), the task treats the socket as dead and reconnects.

Between reconnects the task waits 1, 2, 4, 8, 16 seconds, then 30 seconds for every later attempt. Each attempt is logged at warn with its number and reason. The delay goes back to 1 second as soon as a `connection_state` arrives.

## Stopping

`Client::shutdown` (or dropping the client) tells the task to close the socket and exit. When the task stops on its own, for example after `taken_over`, the event channel closes. The application's receive loop then ends, and `shutdown` returns the error that stopped the task.

## Known trade-offs

- The cursor moves forward when an event is put on the channel, not when the application has finished with it. If the process crashes, events still waiting in the channel (up to 256) are lost, because the server has already been told they were processed.
- If the application stops reading and the channel fills up, the task waits for room and pings go unanswered meanwhile. After a few seconds the server evicts with `heartbeat_lapsed`, and the task reconnects and resumes. Nothing is lost, but the reconnect shows up in the logs.
