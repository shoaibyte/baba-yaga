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

## 2026-11-04 16:25 to 16:32 Stage 3

- Added `Http::list_ops` and `ConnectionHandle::wait_connected` / `Client::wait_connected`. Ops bound to the connection fail until `connection_state` has arrived, so callers wait first.
- `examples/ops_probe.rs` run twice (16:25 raw JSON, 16:31 typed methods). Both runs:
  - 57 operations listed. `post_message` op: `{body, thread_id?}`. `send_targeted_message`: `{body, target_names?, target_roles?, thread_id?}`. `create_room_document`: `{name, description, instructions, content}` (content max 1 MiB). `update_room_document`: `{document_id, name?, description?, instructions?, content?}`. `read_context`: `{limit=50, since?, before?, room_id?}`. `list_participants`: no arguments.
  - Thread reply via the op: landed in the thread (`read_context` shows it under the root's replies).
  - Thread reply via POST /message with `thread_id`: 200 OK, but posted as a new top-level message. Field ignored.
  - Document create, update and delete all succeeded; the probe deletes its own document, so none is left behind.
  - Participants: baba_yaga (agent, live), shoaib2933 (user).
- `post_in_thread` uses the op, so no "re: <thread_id>" fallback was needed.
- Added typed methods on Http and Client: `post_in_thread`, `create_document`, `update_document`, `delete_document`, `read_context`, `list_participants`, with result types `EventId`, `DocumentId`, `DocumentStatus`, `Participant`, `Context`. Tests use the observed JSON.
- Deviation: the spec had `post_in_thread(room_id, thread_id, content)`. The op has no room argument (it posts to the connection's room), so `Http::post_in_thread` takes the connection id and `Client::post_in_thread` uses the client's room.
- Consequence for Stage 4: `report` needs a socket for thread replies. Decision below in the Stage 4 entry.

Messages posted this stage: 6 (two runs, three "[probe]" messages each). Room documents left behind: none.

## 2026-11-04 16:33 to 16:50 Stage 4

Commits, in order: cf7903c (source paths relative to the scan root, a fix found on the first samples scan), 0cb2883 grouping, 4ba532d state v2, 5b0f94e thread text and commands, b2abd1e serve loop, f142905 report, 115df1b singular "1 advisory", c60bcbe audit log.

What was built
- `FindingGroup` per (repo, ecosystem, package), with `highest_fixed` (approximate semver-ish compare; ignores git commit hashes OSV lists as PyPI fixes), `max_severity`, `any_reachable`. `scan --group` prints groups.
- State v2: one row per group, key `repo:ecosystem:package`, vuln ids as JSON, thread_id, status (open, assigned, pr_open, reviewed, approved, snoozed, resolved), snoozed_until, first_seen, last_seen, last_posted, plus a `meta` table. v1 is dropped on open (PRAGMA user_version 2).
- `render_group` (under 1500 chars, eight advisories, "more: N", command hint), `render_update`, `render_status` (one line per group, under 1900 chars), `render_help`. Commands: approve, reject, snooze Nd/Nw, status, scan [repo], help; noise words allowed, a negation before the verb makes it Unknown.
- `serve`: posts the online line, scans at once, then on the cron schedule (UTC); handles addressed messages between scans. Thread replies go through the `post_message` op.
- `report [--path DIR] [--no-state] [--dry-run]`: HTTP only, no socket. Writes state unless `--no-state`, so a later `serve` with the same state file does not repost. Changed groups that already have a thread are left unrecorded for `serve`, because HTTP cannot reply in threads.
- Audit: `audit.log` beside the state file, mirrored whole (newest 900 KB) to the room document `baba-yaga-audit`. The document id is kept in state `meta`.

Live checks
- 16:36 `report --path samples --dry-run`: 12 posts, longest 1163 chars, all ASCII.
- 16:38 `report --path samples --no-state`: 12 threads opened (Go 2, npm 3, PyPI 5, Packagist 2). The brief said 11, but those numbers add up to 12. certifi and idna have findings; chardet and markupsafe have none.
- 16:39 `serve` with a fresh state file (scratchpad config, schedule `0 */30 * * * *`): connected, posted "Baba-Yaga online. Watching 1 repos. Next scan: 2026-11-04 16:00 UTC.", first scan opened 12 threads (duplicates of report's, accepted as planned). Stayed connected 5 min 22 s: no warning, error or reconnect. Waiting line: `next=2026-11-04 16:00 UTC`.
- 16:45 killed and restarted `serve` on the same state file: connected at once, first scan "0 new threads, 0 updates, 12 unchanged". State dedup works. No 4409 on the immediate restart (the killed socket's claim was released at once), so the 4409 retry path is still untested live.
- Decision on report and serve: report writes state unless `--no-state`. For this check report used `--no-state`, and serve used a fresh state file and posted duplicates.

Messages posted this stage: 26 (12 report, 1 online, 12 serve, 1 online on restart). Room documents: none created (no approve, reject or snooze happened, so the audit mirror never ran).

### Needs human (Stage 4)
- Delete the duplicate threads: either report's 12 (16:38) or serve's 12 (16:39). Serve's state file points at serve's set (thread ids `sw_…` in the scratchpad state.db), so keep serve's if you will reuse that state.
- With serve running, in one thread from serve's set: `@baba_yaga status`, `@baba_yaga approve`, `@baba_yaga snooze 30d`, and one at the top level, `@baba_yaga help`. Check the replies land in the thread, and that `audit.log` plus the `baba-yaga-audit` room document appear after approve or snooze.
- Same for `reject` from a non-owner account, if you have one, to see "Only a room member can approve or reject." (any listed user counts as a member, so this may need a user outside the room).

### Untested assumptions
- A human reply inside a thread arrives with `payload.thread_id` equal to the root message id (what `find_by_thread` looks up). Ops normalise to the root, but the incoming event field has not been seen with a value yet.
- Whether a reply inside the agent's own thread counts as `addressed` without an explicit @baba_yaga. The hint tells people to mention it.
- Room member check matches `sender_name` against participant names; there is no shared id between message senders and `list_participants`.
- `load_internal_documents` result shape (needs `ids`), so the audit document is written but never read back.
