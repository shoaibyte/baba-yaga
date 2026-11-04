# Progress log

Append-only. Times are Europe/Oslo. Server: switch-core 0.31 on localhost, agent-protocol 9.

## 2026-11-04 16:10 Stage 2 decisions applied

- Cursor stays "advance before send" (at-most-once); recorded in WS_DESIGN.md.
- 4409 now retried: a run of 4409 closes (or `refused` with status 409) is retried with backoff; the fifth in a row stops with `Error::Refused`. Unit test `conflict_retries_four_times_then_stops_on_the_fifth`. Commit b9492cd.
- Health check against SWITCH_URL: 200. Port unchanged.

## 2026-11-04 16:14 to 16:20 Stage 2 live check

1. `echo` started 16:14:05 with `RUST_LOG=info,switch_client=debug`. Connected at once:
   `connected connection_id=c8c2493a-… generation=8589934595 protocol=Some(9) rooms=["f48120fd-…"] server_cursor=Some(0) cursor=0`
   No warning, error or eviction in the following 60 s, so pongs are accepted.
2. Paused with `kill -STOP` for 20 s, then `kill -CONT`. Log:
   `WARN evicted code=heartbeat_lapsed reason=Some("heartbeat lapsed; reopen the stream and resume from your cursor")`
   `WARN reconnecting attempt=1 delay_s=1 cursor=0 reason=evicted: heartbeat_lapsed`
   `INFO connected … generation=8589934596 … cursor=0` (same connection_id, generation +1)
   No further reconnects in the next 30 s, so the backoff reset works.
3. Posted "[probe] stage 2: subscription check" over HTTP (`event_id=sw_9a02…`). Echo did NOT receive it as an event. `read_context` shows the message in the room with sender `baba_yaga`. Conclusion: the server does not deliver an agent's own posts back to it. The subscription itself is the same one that delivered Discord pings to ws_probe today, so I did not treat this as broken. Only a message from another sender can confirm it, which is under "needs human".
4. The POST /message `event_id` equals the room message id seen in `read_context`, so it can be used as a `thread_id`.
5. The 8:45 PM human ping (`sw_b914…`), sent while no client was connected, got a threaded reply from Switch's auto-reply ("I'm currently offline"). Offline auto-replies go into a thread on the triggering message.

Messages posted this stage: 1.

### Needs human (Stage 2)
- Run `cargo run --example echo`, send `@baba_yaga ping` from Discord, expect "pong". Then pause it (`kill -STOP`, 20 s, `kill -CONT`) and ping during the pause; expect "pong" after resume. This is the only test of catch-up via `start_from`, because a cursor of 0 in my run means nothing was delivered to resume from.
