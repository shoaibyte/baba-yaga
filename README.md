# baba-yaga

Baba-Yaga is a dependency vulnerability patrol that lives in a Switch room. On a schedule it scans Go, Python, Node and PHP repositories with osv-scanner, posts one thread per vulnerable package, and waits for a person in the room to approve, snooze or reject each finding. When a finding is approved, it hands it to a second agent, the fixer, in the same thread. The fixer bumps the package on a branch and opens a pull request, and Baba-Yaga tracks the thread until that PR exists. The pitch in one line: vulnerability findings become chat threads, people decide in the thread, and an agent does the fix.

Demo video: TBD

## Where to look

- [`crates/switch-client/src/ws.rs`](crates/switch-client/src/ws.rs): the Switch agent protocol client, written by hand: WebSocket, ping and pong with the cursor, resume.
- [`crates/baba-yaga/src/workflow.rs`](crates/baba-yaga/src/workflow.rs): the approval and handoff loop.
- [`template/patrol.template.yaml`](template/patrol.template.yaml): the room template that creates the room and the fixer.
- [`docs/NOTES.md`](docs/NOTES.md): everything we learned about Switch 0.31 that the docs do not say.

## How Switch is used

Each of these Switch features carries part of the workflow:

- **A custom agent on the agent protocol.** Baba-Yaga is not a Console-managed agent. It is a Rust client (`crates/switch-client`) that registers with `POST /agents`, holds a WebSocket to the server, answers every ping with a pong that carries its cursor, and resumes from that cursor after a drop or an eviction.
- **Rooms and threads.** Each vulnerable package gets a root message and a thread. Updates, commands, the approval and the fixer's reply all stay in that thread, which keeps one finding to one conversation on Discord.
- **Targeted messages to a second agent.** On approve, Baba-Yaga calls `send_targeted_message` with the fixer as the target and the thread id, so the handoff lands in the finding's thread and addresses only the fixer.
- **Room documents.** Every decision is appended to a local `audit.log`, and the log is mirrored to a room document named `baba-yaga-audit` that everyone in the room can read.
- **Addressing policy as part of the approval gate.** Baba-Yaga's addressing is opened so that room members can address it at all. Its own check then lets only the room owner (`sender_role: owner`) or a user listed as a room participant approve or reject.
- **A room template.** `template/patrol.template.yaml` creates the room and the fixer agent in one run.

Without Switch, there would be no shared room where people and both agents see the same thread. Approvals would have no place to happen, the fixer would have no way to receive a handoff addressed to it, and the audit document would have no readers. What would remain is a scanner that prints JSON.

## Architecture

The system has three zones. On the left are the sensors: osv-scanner run over each configured repository, with Go call analysis to mark findings whose vulnerable code is actually called. In the middle is the Switch room, bridged to Discord. It holds Baba-Yaga, the people who decide, and the fixer agent. Baba-Yaga keeps its own state in SQLite: one row per package, its thread, its status and the PR link. On the right is the outside world the fixer acts on: the git repository, the package registry and GitHub, where the pull request is opened.

```
  sensors                  Switch room (Discord bridge)               outside world
  -------                  ----------------------------               -------------
  osv-scanner  -->  Baba-Yaga --- thread per package --- people
  (per repo,          |   ^          approve / snooze / reject
   Go call            |   |
   analysis)          |   +-- fixer reply (branch, PR link)
                      v                                        
                 send_targeted_message ------> fixer agent ----> git branch fix/...
                      |                                     ----> package registry
                 SQLite state, audit.log                    ----> GitHub pull request
                 room document baba-yaga-audit
```

Status of a package moves `open -> approved -> assigned -> pr_open`, or to `snoozed` or `rejected`. It becomes `resolved` when the scanner stops reporting it. If an assigned or pr_open package comes back at a new installed version that still has advisories, it returns to `open` so it can be approved again.

## Quickstart

From a clean clone, on macOS or Linux.

1. Install the prerequisites:
   - Rust (stable, via rustup): `curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh`
   - osv-scanner 2.x: `brew install osv-scanner`, or `go install github.com/google/osv-scanner/v2/cmd/osv-scanner@latest`
   - Go on `PATH`, only for Go reachability (call analysis). Without it, Go findings are reported without reachability and a log line says so.
   - Switch Console, or a self-hosted Switch server with Docker. See https://docs.switchagents.ai/switch-rooms/getting-started/install-switch-console and https://docs.switchagents.ai/switch-rooms/deploy/self-host.

2. Start the local Switch server from Switch Console and find its API address. Console runs the server on a random local port, which changes when Console restarts. Find it with:

   ```sh
   ps eww -p $(pgrep -f switch_core.main) | tr ' ' '\n' | grep '^GATEWAY_PUBLIC_URL='
   ```

   The address is the value of `GATEWAY_PUBLIC_URL`, for example `http://localhost:51970`. Set `SWITCH_URL` to it, then check it:

   ```sh
   curl -s "$SWITCH_URL/health"
   ```

3. Connect Discord to the server, following https://docs.switchagents.ai/switch-rooms/deploy/messaging-apps/discord. When Console asks to link your Discord user account, do it: that is what makes you the room owner (`sender_role: owner`) for the approval gate.

4. Register Baba-Yaga and fill in `.env`:

   The registration key is the `AGENT_REGISTRATION_TOKEN` the server was started with. Read it and put it in `.env` as `SWITCH_REGISTRATION_KEY`:

   ```sh
   cp .env.example .env
   ps eww -p $(pgrep -f switch_core.main) | tr ' ' '\n' | grep '^AGENT_REGISTRATION_TOKEN=' | cut -d= -f2-
   # Set SWITCH_URL (step 2) and SWITCH_REGISTRATION_KEY (the value above) in .env.
   cargo run -p switch-client --example register
   # Copy the printed SWITCH_AGENT_ID and SWITCH_AGENT_KEY into .env. The key is shown once.
   ```

   Run the register example once. A second run returns 409 because the name is taken.

5. Open Baba-Yaga's addressing through the Gateway API, so that people in the room can address it. An API-registered agent is owned by "Agent Bootstrap", so Console cannot edit it; an empty rules list means anyone may address it. With `.env` loaded (`set -a; . ./.env; set +a`):

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

6. Create the room and the fixer. Either:
   - **With the template:** in Switch Console, open Templates, select Import template, choose `template/patrol.template.yaml` and run it. It creates the agent `patrol-fixer` and the room `patrol-security`, and posts a kickoff to the fixer.
   - **By hand in Console:** create a room, then create a Claude Code agent named `patrol-fixer`, set Who can talk to your agent to Anyone, paste the instructions from the template's `instructions:` block, and add it to the room.

   The fixer must accept messages from Anyone: Baba-Yaga is not one of your own agents, so with "Me and my agents" the fixer would ignore the handoff. The template sets this.

   Then invite Baba-Yaga into the room. In the Discord channel (verified on Discord):

   ```
   /invite-agent agent:baba_yaga
   ```

   The agent registered as baba-yaga, but the server stores the name as baba_yaga and Discord addresses it that way; Console-created agents like patrol-fixer keep their hyphen.

   Get the room id from the rooms Baba-Yaga is in:

   ```sh
   set -a; . ./.env; set +a
   curl -s -X POST -H "Authorization: Bearer $SWITCH_AGENT_KEY" -H 'Content-Type: application/json' -d '{}' "$SWITCH_URL/agents/$SWITCH_AGENT_ID/ops/list_rooms"
   ```

   Put the room id in `.env` as `SWITCH_ROOM_ID`, or in the config below as `room`.

7. Write the config:

   ```sh
   cp baba-yaga.example.toml baba-yaga.toml
   # Set room, the repos to scan, and fixer_agent = "patrol-fixer".
   ```

   To try it on the bundled samples, use one repo with `url` set to the absolute path of `samples/`.

8. Start the patrol:

   ```sh
   RUST_LOG=info cargo run -p baba-yaga -- serve
   ```

9. What you should see: the log says `connected`, and the room gets "Baba-Yaga online · watching 1 repo · next scan ...". After the first scan, every vulnerable package has its own thread, titled "<package> <version> → <fix>", with the advisory details as the first reply. In a thread, `@baba_yaga approve` gets "Approved by <your Discord name>. Handed to patrol-fixer.", and the fixer's message appears in the same thread. The first time the fixer pushes, Console may show a permission prompt for its `git push`; allow it there. Once the fixer replies with a branch or PR link, `@baba_yaga status` lists the package as "PR open".

Changes to `baba-yaga.toml` take effect when `serve` is restarted.

## Commands

Address Baba-Yaga in the room. Approve, reject and snooze work only inside a finding's thread. After the mention, the message must be exactly one command from the table, in any case, with trailing punctuation allowed. Any other message in a thread gets no reply, so people can talk there without triggering anything.

| Command | Where | What it does |
|---|---|---|
| `@baba_yaga approve` | Finding thread | Room owner or room members only. Marks the finding approved and hands it to the fixer in the thread. |
| `@baba_yaga reject` | Finding thread | Room owner or room members only. Closes the finding as not needed; later scans skip it. |
| `@baba_yaga snooze 30d` | Finding thread | Hides the finding for a number of days (`d`) or weeks (`w`). It reopens when the snooze runs out. |
| `@baba_yaga status` | Anywhere | Lists open findings with their status, then rejected ones. |
| `@baba_yaga scan [repo]` | Anywhere | Scans now, all repos or the one named, and posts the result. |
| `@baba_yaga help` | Anywhere | Lists the commands. |

## CI mode

`report` scans once, posts each package with findings as a thread, and exits:

```sh
cargo run -p baba-yaga -- report --dry-run          # print the posts, send nothing
cargo run -p baba-yaga -- report --path samples      # scan a directory instead of the configured repos
```

It opens a socket while it posts, because thread replies need one, so stop `serve` first: the agent holds one connection at a time. `action/action.yml` is a placeholder GitHub Action for this mode. It is not yet wired to a public runner, and a CI runner would also need network access to the Switch server.

## Samples

`samples/` pins known-vulnerable dependency versions on purpose, as fixed input for demos and tests. Never deploy them or use them as a starting point for real code.

- `go/`: golang.org/x/text and golang.org/x/net
- `python/`: requests, urllib3 and jinja2
- `node/`: lodash, minimist and axios
- `php/`: symfony/http-foundation and guzzlehttp/guzzle

`php/composer.lock` is hand-written and lists only the two pinned packages, so that the demo needs no PHP toolchain. osv-scanner reads it, but `composer install` will not work.

## Limitations

- Room members are matched by display name (`sender_name`), the only identifier the message event and the participant list share. A user who can change their display name to a member's could pass the approval check.
- Schedules and times are in UTC.
- Switch Console picks a random local port each time it restarts, so `SWITCH_URL` in `.env` must be updated after a restart.
- The fix version is the highest fixed version the advisories list, and the registry may have deprecated it. The handoff lists the other fixed versions and asks the fixer to avoid a deprecated one, but Baba-Yaga does not check the registry itself.
- Reachability is analysed for Go only. Findings in other ecosystems are reported without it.
- Vulnerabilities in the PHP interpreter itself, such as PHP2Shell, are out of scope: Baba-Yaga reads lockfiles, and a lockfile does not name the interpreter version.
- There is no reviewer agent yet. A person reviews the fixer's pull request, and Baba-Yaga does not move a finding beyond `pr_open`.

## Development

Workspace layout:

- `crates/switch-client`: the Switch agent protocol client: HTTP, WebSocket with ping, pong and resume, operations, typed events. Its `examples/` hold `register`, `echo`, `post_message`, `ops_probe` and `ws_probe`.
- `crates/baba-yaga`: the patrol. `scanner/` runs osv-scanner and groups findings, `state.rs` is the SQLite state, `room/` holds command parsing and message text, `workflow.rs` holds serve, report and the fixer handoff, and `audit.rs` the audit log.
- `template/`: the Switch room template. `action/`: the CI placeholder. `samples/`: vulnerable sample projects.

Checks, run before every commit:

```sh
cargo fmt --all
cargo clippy --all-targets -- -D warnings
cargo test
```

Other docs:

- `docs/NOTES.md`: observed Switch behaviour that the public docs do not cover.
- `docs/PROGRESS.md`: build log by stage, with live test results.
- `docs/WS_DESIGN.md`: the WebSocket client design (cursor, reconnect, eviction).

## License

MIT. See LICENSE.
