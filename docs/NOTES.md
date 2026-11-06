- Agent names on Discord: the API-registered agent got an underscore (baba-yaga is @baba_yaga in the channel). Console-managed agents keep the hyphen: the fixer is `patrol-fixer` both as its Switch handle and as its Discord mention.
- Console 0.40 runs switch-core natively (no Docker); the API port is random per restart, read it from Console's agent-controller process or set SWITCH_URL again.
- Message payloads carry `sender_role` (observed `"owner"` for the room owner), plus `sender_kind`, `on_behalf_of` and `opening_message`, all null so far. None of these are in the agent-events docs.
- Domain events carry `data.missed`, e.g. `{"count": 1, "reason": null}`. Undocumented; meaning of `count` unknown (it was 1 on a live message with no known loss).
- Sequence numbers exceed 2^53 (e.g. 9007194959773696 = 2^53 - 2^32): they are packed values, not counters. Keep them u64, never route them through f64 or JSON-in-JS, and never compute `cursor + 1`.
- The client pong is the flat `{"type":"pong","cursor":N}`, not the server's event envelope; switch-core 0.31 accepts it (40 s+ with no eviction).
- The Switch server runs on localhost, so turning Wi-Fi off does not drop the agent's socket. To test reconnect, pause the process (`kill -STOP` / `kill -CONT`) or restart switch-core.
- An agent does not receive its own posts as events (POST /message from baba_yaga never reached baba_yaga's socket, though `read_context` shows it).
- The `event_id` returned by POST /agents/{id}/message is the room message id (`sw_…`), the same id `read_context` lists, so it can be passed as `thread_id`.
- When no client is connected, Switch answers an addressed message with an auto-reply ("I'm currently offline …") posted as a thread reply to that message.
- Threads: the `post_message` and `send_targeted_message` ops take an optional `thread_id` (any message id in the thread; normalised to the root). Room documents: `content` max 1 MiB.
- Threading, confirmed live: reply with the `post_message` op and `thread_id` = any message id in the thread. `POST /agents/{id}/message` accepts a `thread_id` field, returns 200, and silently posts at the top level instead. So thread replies always need a live connection (`X-Switch-Connection-Id`); there is no connectionless way.
- A root message shows on Discord as a flat message; the thread appears only once a reply is posted in it. So every finding root is followed at once by the command hint as its first in-thread reply (`hint_posted` in state, schema v3). `report` therefore opens a socket while it posts.
- Room documents: `create_room_document` -> `{"document_id"}`; `update_room_document` and `delete_room_document` -> `{"document_id", "status": "ok"}`. Names are unique per room; only the creating agent can update or delete. `load_internal_documents` requires `ids` (400 without). Content cap 1 MiB per the schema.
- `list_participants` -> `[{id, name, type: "agent"|"user", status, alias}]`; `status` is `live` for a connected agent and null for users. The room owner shows up as a `user`; `sender_role: "owner"` on message events is the only owner marker seen so far.
- `read_context` entries: `{id, kind, sender, sender_name, body, timestamp, attachments}`; `sender` for an agent is `@switch-agent-<8 hex>:localhost`.
- osv-scanner reports absolute, symlink-resolved manifest paths. `source_path` is made relative to the canonical scan root, so a relative `--path` works.
- OSV lists git commit hashes as fixed "versions" for some PyPI advisories (requests, urllib3, certifi, idna), some starting with a digit (`74ea7cf…`). `highest_fixed` ignores anything that looks like a hash unless nothing else is listed.
- The `cron` crate needs a seconds field: the 5-field `*/30 * * * *` is written `0 */30 * * * *`. baba-yaga evaluates schedules in UTC.
- Addressing for an API-registered agent (verified 2026-11-04 on switch-core 0.31): `update_agent_detail` with the agent's own key is refused ("An agent can't change its own settings"), and Console cannot edit the agent because it is owned by "Agent Bootstrap". The Gateway API with the admin cookie works; an empty rules list means anyone may address the agent:

  ```sh
  # admin email/password are the GATEWAY_ADMIN_EMAIL / GATEWAY_ADMIN_PASSWORD the server was started with
  # (Console: ps eww -p $(pgrep -f switch_core.main) | tr ' ' '\n' | grep '^GATEWAY_ADMIN_')
  curl -s -c /tmp/gw.jar -X POST -H 'Content-Type: application/json' \
    -d '{"email":"<admin email>","password":"<admin password>"}' \
    "$SWITCH_URL/gateway/auth/login"
  curl -s -b /tmp/gw.jar -X PUT -H 'Content-Type: application/json' \
    -d '{"policy":{"rules":[]}}' \
    "$SWITCH_URL/gateway/agents/$SWITCH_AGENT_ID/addressing-policy"
  rm /tmp/gw.jar
  ```
- `/invite-agent agent:baba_yaga` adds the agent to the room on Discord (verified).
