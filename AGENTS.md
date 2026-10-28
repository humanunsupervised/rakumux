# Rakumux: Terminal Sessions for Coding Agents

Rakumux is a terminal-first manager for long-running terminal sessions, built for running coding
agents (Claude Code, Codex, OpenCode, Pi, …) side by side. A **session** is a folder plus a
terminal. One per-user daemon, **`rakumux-ptyd`**, owns every session; the **`rakumux`** TUI and
CLI are clients of it, and so is **`rakumux-api`**, a local HTTP API that the **`rakumux-mcp`**
server uses to let coding agents drive sessions. Source: `humanunsupervised/rakumux`;
website: <https://rakumux.com>. The core is licensed under Apache-2.0 (`LICENSE`).
The mobile app is maintained separately in `humanunsupervised/rakumux-mobile` and is not included
in this repository's license grant. Public preparation and release checks: `PUBLISHING.md`.

Hosted account, AI and billing implementation is proprietary and belongs only in
private `humanunsupervised/rakumux-cloud`, outside this repository. Public `landing/`
remains static marketing. The native app, local `rakumux-api` / MCP, local orchestration
and canonical wire schemas stay public. Do not add cloud implementation as a nested
folder, branch import, submodule or release asset here. Existing Apache-2.0 marketing
and contract code retains its license when copied privately. Run
`node tests/public-source-boundary.mjs` before source publication; it is not a complete
secret/history audit. See `PUBLISHING.md` for that release boundary.

## Speculative edge cases (read first)

- Before adding complexity for a speculative or vanishingly unlikely race or security edge case, explain the concrete failure mode, likelihood, and complexity cost to the user and get their buy-in. Do not silently expand scope for theoretical robustness.

## Design principles (read first)

- Cut taglines, helper text and secondary labels. If a screen needs them to make sense, fix the layout.
- Make the best decision that will elevate all three: the user experience (UX), developer experience (DX), and agent experience (AX) always, without breaking anything.

## Architectural requirements

1. **One server, many clients.** Exactly one `rakumux-ptyd` per user owns every session's
   terminal and the session registry. Any number of TUIs, CLI calls and agents connect at the same
   time. Several TUIs may watch the same session; at most one is its controller. No client is
   special and no client may assume it is the only one; closing every TUI leaves all sessions
   running.
2. **No git operations.** No worktree creation, git status, merge helpers or env-copy, and no
   `rakumux-cli`; the product never runs `git`. The one exception is read-only **git context**:
   the daemon reads `.git` files to show a session's branch, linked worktree and repository
   name (`ptyd/src/git.rs`). Users run git inside a session like any other program.
3. **Sessions belong to folders.** A folder is a directory (canonical path; the wire calls it a
   scope, `ScopeRoot`, and keeps `project_*` requests and `ProjectView`). Every session belongs to
   the folder it was started in, and its cwd must be inside it when created. The TUI shows the
   sessions of all folders by default, live (sessions in a new folder appear as they start), and
   narrows them with a folder filter (one folder or several; not saved); the CLI defaults to the
   current folder (`--folder`, `--all`). Agents (session callers) only ever see their own folder.
   See `tui/src/app/folders.rs` and `api/README.md`.
4. **The daemon owns the session API.** Registry, scope confinement, identity, permissions, waits,
   lifecycle and audit live in the daemon. TUI and CLI are thin clients of
   `session_core::client::SessionApi`.
5. **No shell services layer.** All logic is Rust. Shell scripts only launch the binaries, set the
   terminal title, check for updates and build. Runtime dependencies: none besides Rakumux's own
   binaries and bash (no `git`, `tmux`, `jq` or `python3`).
6. **Contracts first.** The crates' public types and traits (below) are the architecture. Change
   them only additively, and keep the wire protocol backwards compatible.

## Architecture approach: DDD + Clean Architecture

- **Domain language**: session, folder (scope), terminal, attachment (controller / observer),
  status (live / exited / dead), lifecycle (working / blocked / done / idle), report, caller (user /
  session), revive, resume, pin, git context. Use these words in code, docs and UI.
- **Domain**: `session_core::model` and `ids` (pure data, serde, no I/O beyond path
  canonicalisation).
- **Application**: the daemon's control layer (`ptyd/src/control`, behind `control_api::Control`)
  implements the use cases (create, rename, kill, delete, revive, report, read, wait, send, submit,
  touch) and the policy. Clients orchestrate UI and call `SessionApi`; they hold no business rules
  about sessions.
- **Infrastructure**: PTYs and emulators (`ptyd/src/terminal`, behind `host::TerminalHost`), the
  SQLite registry, scrollback files (`persist`), process inspection (`procinfo`), the socket server
  and framing (`server`, `session_core::frame`), the client transport (`session_core::client::Client`).
- **Dependency direction**: `tui` → `session-core` + `vt`; `ptyd` → `session-core` + `vt`;
  `api` → `session-core`; `mcp` → `api` (its `discovery` and `dto` modules) + `session-core`;
  `session-core` and `vt` depend on no Rakumux crate. `tui`, `api` and `mcp` never depend on
  `ptyd`; `mcp` talks to the daemon only through the HTTP API. In the daemon,
  `server` → `Control` → `TerminalHost`; `terminal` knows nothing about scopes, callers or policy.
- The UI never opens the socket itself, never reads the registry and never touches a PTY.
- A session's git context follows its running agent's self-reported working directory when
  available, otherwise its terminal cwd. Reports never change its scope or registry cwd.
- Name modules by capability (`naming`, `terminal_view`, `layout`, `help`, `control`, `persist`), not
  by transport.
- Make invalid states hard to represent, return explicit errors (`ApiError` with an `ErrorCode`),
  and avoid silent fallbacks. No `unwrap()`/`expect()` on daemon request paths.

## Components

1. **`rakumux.sh`**: launcher and primary entry point. Sources `shell/gwr_load.sh`, handles
   `-h/--help` and `--check-updates`, resolves the `tui` and `rakumux-ptyd` binaries
   (`GW_TUI_BIN`, `GW_PTYD_BIN` overrides), exports `RAKUMUX_BIN`, sets the terminal title, starts
   a silent background update check, then `exec tui --ptyd-path <path> "$@"`.
2. **`tui` binary** (`tui/src`, installed as `rakumux`): `main.rs` first calls `cli::dispatch`; a
   recognised subcommand runs and exits, otherwise the TUI starts.
3. **`rakumux-ptyd`** (`tui/crates/ptyd`): the daemon. One per user (per Rakumux home): every
   client finds the running one through `~/.rakumux/run/service.json`, whatever the login (SSH,
   mosh, a phone), and user clients spawn it only when none runs (detached: stdio on `/dev/null`,
   `setsid`); it stays up with zero clients. It **restarts in place** (`daemon_restart`, SIGUSR2):
   it `exec`s the new binary in the same process and hands over the PTYs, screens, tokens and
   socket, so running programs keep running (`ptyd/src/handoff.rs`). When programs can't survive
   (a crash, a reboot), the next start revives the sessions that were running, each with its resume
   command (`integrations/recovery.rs`; see "Recovery" below).
4. **`session-core`** (`tui/crates/session-core`, lib `session_core`): the shared contract: model,
   protocol, frame codec, ids, filesystem layout, and the blocking client.
5. **`vt`** (`tui/crates/vt`): terminal emulation on `vt100`, shared by daemon (responding emulator
   that answers terminal queries) and TUI (passive mirror), plus key/mouse/paste encoders.
6. **`shell/`**: launcher helpers (`gwr_*.sh`, `gwr/check-updates.sh`), build helpers (`build/`),
   `rakumux-local.sh` for running a local checkout, and `rakumux-command-hook.sh` (command
   history for AI naming).
7. **`install/`**: release and distribution (npm package, Homebrew formula, release scripts).
8. **`rakumux-api`** (`api/`, workspace member through `package.workspace = "../tui"`): HTTP/1.1
   and JSON on 127.0.0.1, a thin adapter over `session_core::client` (contract:
   `api/openapi.yaml`). Routes `/v1/health`, `/v1/folders`, `/v1/sessions[/{id}[/content]]`
   (`?all=true`: every folder), `POST /v1/sessions/{id}/revive|kill`, `DELETE /v1/sessions/{id}`,
   and the WebSockets `GET /v1/events` (session events) and `GET /v1/sessions/{id}/attach` (the
   live terminal: checkpoint, then output as binary frames; input and resize/promote/demote/fit
   back; `api/src/streams.rs`, tokio-tungstenite over a hyper upgrade). It says `folder` (a
   session's folder: the daemon's `scope`) and still accepts the deprecated `/v1/projects` and
   `project` names, returning `project` next to `folder`. Writes `api.json` (url, token, pid,
   version, `remote_urls`) while it runs; on loopback: bearer token, loopback `Host`, no browser
   `Origin`; `X-Rakumux-Session-Id`/`-Token` make it act as that session. **Remote access**
   (`api/src/remote.rs`): while `remote.json` is enabled it adds a remote
   listener on every Tailscale address of the machine (`session_core::tailscale::addresses`: in
   `100.64.0.0/10` or `fd7a:115c:a1e0::/48` **and** on Tailscale's own interface, `tailscale*`, or
   listed by `tailscale status`; the ranges alone also catch ISP carrier-grade NAT and other VPNs) at
   `remote.json`'s port (7337), or on `listen` / `--remote-listen ADDR:PORT` instead; there the
   bearer is the remote token, `Host` isn't checked, `Origin` is allowed, identity headers are
   refused and the caller is the user. Runs in the foreground or with `--detach`; a user client of
   the daemon (spawns it on demand), one API per Rakumux home. See `api/README.md`.
9. **`rakumux-mcp`** (`mcp/`): MCP server on stdio for coding agents, built on `rmcp` (exact
   pinned, confined to `mcp/src/server.rs`). Tools: `list_folders`, `list_sessions`,
   `get_session`, `create_session`, `read_session`, `rename_session`; a `folder` argument (alias
   `project`, deprecated) narrows names or places new sessions. Finds the API through
   `api.json` and starts `rakumux-api --detach` when needed; forwards `RAKUMUX_SESSION_ID`/
   `RAKUMUX_SESSION_TOKEN` as identity headers; returns terminal text sanitized, capped and inside
   a nonce-delimited untrusted block. See `mcp/README.md`.
10. **Account and AI session titles** (`tui/crates/sensagent`, `tui/src/account.rs`): a
   per-home service, the hidden `rakumux status-service` (`rakumux_sensagent::service::serve`),
   holds the Rakumux account (Clerk OAuth device sign-in, credentials under
   `~/.rakumux/sensagent/credentials`), follows the daemon's sessions as a user client and asks
   `https://rakumux.com/api/v1/ai/session-status?title=1` for a short title and description of
   each live session (contract `contracts/session-status/`; descriptions cached in
   `~/.rakumux/sensagent/status/status.sqlite3`). It is started detached (`setsid`)
   on demand: by `rakumux account login` or the TUI's sign-in, or when the home has a saved
   account; it then runs until SIGTERM. Clients talk to it over a private Unix socket
   (`rakumux_sensagent::owner`). It shares sessions only while the account's AI session titles
   switch (`ai_session_summaries` in `GET /api/v1/features`, read every 2 minutes) is on: then
   it consents for the folders of live sessions (never private ones), and off it stops. **One
   title per chat**: an agent conversation the daemon follows (`session_turn`) is titled once,
   when its agent has written 5 messages (`sessions.ai_summary.every_messages`) or its turn
   finishes sooner, from the prompt and its messages (never the screen); after that only when
   asked: `OwnerRequest::Summarize` (the ↻ after a hovered session's name, or `s`), from the
   latest prompt and the newest messages, or the screen for other programs. Nothing refreshes on
   a timer. **The title names the session** (`refresh/titles.rs`): a worker renames it through
   the daemon (`session_rename`, as the user) only while its name is the generated one
   (`auto_named`) or exactly the title the service last gave it (kept per session in
   `sensagent/status/titles.json`, 0600), so once the user renames it (`r`, `rakumux rename`,
   the API or MCP) automatic titles leave it alone; one asked for always applies, and the
   session counts as AI-titled again. A title another session of the folder has gets ` 2`,
   ` 3`, … (names are unique within a folder). A service answer without `title` renames
   nothing. Settings → Account signs in (a code, link and QR code); while a title is being made
   a spinner sits right after the session's name; Settings → Available on sign-in turns the
   account's AI session titles and Agent feed on and off (greyed out when signed out;
   `OwnerRequest::SetFeatures`, applied at once and saved to the account with
   `PATCH /api/v1/features`, as the website's Features page saves them), and `rakumux account
   status` lists them and each live session's AI description. Its stderr, static failure codes only, goes to
   `~/.rakumux/logs/status-service.log`. Clerk has no custom scopes, so production names
   `profile` as the AI status scope (`DeviceAuthConfig::with_status_scope`), matching the
   website's check. Clerk's access tokens last 24 hours: the service renews them with the
   refresh token once a fifth of the lifetime is left (`refresh/renewal.rs`), and a restore
   renews one that expired meanwhile. A client that finds a service from an older release
   asks it to stop (`Shutdown`) and starts its own; a newer one is used as it is. Overrides for development: `RAKUMUX_CLERK_ISSUER`,
   `RAKUMUX_CLERK_CLIENT_ID`, `RAKUMUX_CLERK_DEVICE_PAGE`, `RAKUMUX_CLOUD_ORIGIN`.
   **Agent feed** (`rakumux_sensagent::feed`, contract `contracts/feed/`): while the account's
   Feed switch (off by default, same features read) is on, the service has the daemon capture
   replies (`reply_capture`), takes them from its `replies` subscription (and `reply_list` after
   reconnecting or starting late), leaves out private sessions, masks keys, credential
   assignments and tokens, and uploads them with the session's name, folder name and git
   branch (`POST /api/v1/feed/entries`, batches of up to 50,
   idempotent on the reply id). It reads the account's feed back (`GET /api/v1/feed?after=SEQ`;
   entries carry their turn's `prompt`, and every page the account's prompts still waiting for
   answers, `open`, kept whole and passed to TUIs when they change) every 10 s while a TUI shows it (an owner `feed` request in the last 30 s; one marked
   `background`, from a TUI that only keeps the feed to count what's new, doesn't count), else
   every 2 minutes. Cache (the newest 500) and upload queue are memory only; the TUI asks the
   owner for the entries changed since its revision (`OwnerRequest::Feed`). Signing out or turning the
   switch off empties both. **Whole chats** (`feed/chats.rs`, `ChatSync`): under the same
   switch, each live, non-private session's agent conversation is read from the daemon
   (`session_transcript`) after where it was last uploaded, at most every 5 s while it changes
   (every second while the replies read asks for 1 s: rakumux.com open, a reply waiting here, or
   the half minute after reply activity; `ReplySync::watched`),
   its texts cleaned (escape sequences and controls removed, keys, credential assignments and
   tokens masked) and uploaded as messages (`POST /api/v1/feed/messages`, ≤ 100 and ~2 MiB a
   request); where each session's chat is up to is kept in `sensagent/status/chats.json`
   (0600), so a restart carries on. Entries carry their turn's last `position`. The TUI reads an
   entry's whole turn with `OwnerRequest::FeedTurn {entry, after?}` (`GET /api/v1/feed/turn`),
   each part cut to what a reply read whole shows and the answer within one owner message (`next`
   continues).
   **Replies and threads** (`feed/replies.rs`, `ReplySync`): the user
   writes back to any entry with a conversation id, on rakumux.com or in a TUI (`r` in the
   feed: `OwnerRequest::FeedReply {entry, text, id}` → `POST /api/v1/feed/replies`, ≤ 8 KiB, no
   controls; `id` is the reply's ULID, made by the TUI and sent again with the same text after a
   failure, and rakumux.com answers a repeated id with the reply it stored, so one stored before
   its answer was lost isn't stored, or typed in, twice). Replies being typed in and those
   written in a TUI have a worker of their own (`rakumux-status-reply`), apart from the two that
   fetch files, make AI titles and upload chats, so a TUI's write doesn't wait behind them past
   the 30 s it waits for its answer. Under the same switch the service reads the account's replies as they change
   (`GET /api/v1/feed/replies?after=SEQ&machine=NAME`, as soon as the answer's `poll_ms` says:
   every second while a reply this machine hasn't read yet waits for it or the user has the
   website open, and for
   30 s after the account's last reply activity (a reply or session message written anywhere, or
   one reported delivered or failed), then 2, 4 and 8 s as the quiet time doubles, up to 15 s
   after 4 minutes; within 1–60 s; every 5 s from a rakumux.com that doesn't say; while its
   realtime connection is open, when poked, and otherwise once a minute unless that pace is 1 s:
   **pokes**, `feed/poke.rs`: `GET /api/v1/feed/realtime` gives a `wss://` URL and a token for
   this machine, a thread of its own (`rakumux-status-poke`) holds the WebSocket (rustls,
   tungstenite; the token as its bearer, a `ping` every 30 s, gone when silent 90 s, made again
   after 12 h or when closed, backing off 1 s to a minute; a cloud answering 404 offers none and
   is asked again in half an hour), and a `poke` makes `ReplySync` read at once) and types each
   pending one addressed to
   this machine into the entry's session with `session_submit` (a bracketed paste, then
   Enter; the same for Claude Code, Codex and OpenCode, which have no API for a running
   interactive session), only while it's live, not private, shows the entry's conversation
   (`agent_turn.conversation_id`) and its agent waits (`idle`/`done`; `working`, `blocked` or an
   unknown chat waits, the cloud expires a reply after an hour), not while someone typed
   in that session in the last 10 s (`session_submit`'s `quiet_ms`, so it isn't added to what
   they're writing; tried again 2 s later), and only while the program its view showed in the
   foreground still is (`expect_foreground`, read again by the daemon; refused when its agent
   just exited to the shell, and looked at again 2 s later). A session takes its replies and session messages
   one at a time, in rakumux.com's order (`seq`; a later one waits behind an earlier one that
   waits; one written in this machine's TUI waits for a read that reaches it, made at once, at
   most 5 s, so one written before it on rakumux.com and not read here yet goes first): each is
   `delivered` once its chat shows it as a new prompt (a new `agent_turn` whose
   `user_prompt`'s letters and digits begin the reply's, or hold its first 32: typed after a
   draft on the same line; without a `user_prompt`, from an older daemon, any new turn in its
   chat), or, for an agent whose turns the daemon doesn't follow, once seen `working`/`blocked`
   (a view still showing it waiting proves nothing, as the lifecycle lags); with no new prompt
   within 30 s, or another one, it fails `not_taken` (its Enter lost, it went into a menu, or a
   draft on a line of its own went first: it may still be in the input box); the next goes in
   once the agent waits again (after being seen busy, or 5 s on). Typed in but its session
   ended or chat changed first, it fails.
   What's typed in survives a restart of the service (`feed/replies/kept.rs`,
   `sensagent/status/replies.json`, 0600): the ids it typed in or decided (marked before the
   daemon is asked, so a reply goes in at most once), those handed to the daemon and not yet
   reported (reported `delivered` on the next start), and the reports rakumux.com hasn't taken
   (kept while the switch is off too). The agent's answer to a reply is its reply that
   finished after the reply was typed in, and by the end of that reply's turn: so replies
   typed in one after another each get theirs, and one whose turn ended without an answer
   (interrupted, an API error) never takes a later one.
   It reports `delivered` or `failed` with why (`session_gone`, `conversation_changed`,
   `private`, `not_accepted`, `refused`, `attachment_unavailable`, `not_taken`;
   `POST /api/v1/feed/replies/status`). **Files attached** on rakumux.com (`attachments`, at
   most 5 of 4 MiB each; `feed/uploads.rs`): once a reply could otherwise be typed in, the
   service fetches its files one at a time (`GET /api/v1/feed/replies/attachments/{id}`, the
   body read as bytes up to its declared `size`, 60 s a file), each once, into
   `uploads/<reply id>/` under the name the reply gives (re-checked; `name (2).ext` for one
   taken ignoring case; directories 0700, files 0600, created fresh, never through a symlink),
   then types the text, a blank line and each file's absolute path on its own line. A 404 or a
   body of another length fails the reply `attachment_unavailable` (shown as "its files
   couldn't be fetched"), typing nothing; a network error, 429 or 5xx keeps it pending and
   tries again with backoff; the text never goes in without its files. A reply that won't be
   typed in has its files removed; typed in, they're kept 7 days (cleaned at the service's
   start and daily). Replies with files reach this machine only because its session list says
   `capabilities: ["reply_attachments"]`. A reply written
   on rakumux.com or another machine is typed only when this machine's
   `sessions.remote_replies` is on (Settings → Replies from rakumux.com, local, off by default,
   read every 10 s); one written in this machine's TUI always is: one whose id this machine
   made (`ReplySync::wrote`, kept in `replies.json`), never one that only names it in `from`,
   which any machine of the account can say. **This machine** goes by a name and an id it keeps
   (`feed/machine.rs`, `sensagent/machine.json`, 0600): the hostname when the file was made
   (a hostname that changes later leaves it), and a ULID sent with every feed request
   (`X-Rakumux-Machine-Id`). The first of the account's machines to send its session list under
   a name holds it; rakumux.com refuses another id using it (`machine_taken`), and that machine
   takes the name with a few characters of its id after it (`studio-6m8b`), at most every 20 s.
   Replies are read only once rakumux.com took a session list under the name
   (`SessionSync::confirmed`), and TUIs take the name from the service (`FeedPage.machine`).
   A **session message** (written
   on rakumux.com, or in a TUI on a prompt still waiting for its answer in a new chat, to a
   session with no answer to reply to: `OwnerRequest::SessionMessage {machine, session, text, id}`
   → `POST /api/v1/feed/replies` with `session` in place of `entry`; a reply without `entry` or
   `thread`, read with `session_messages=1` so older machines never get one) is typed once an agent runs
   there and waits, whatever chat it shows, never into a session without an agent; it never
   reaches TUIs, and the agent's next reply answers it. The agent's next reply in that
   session and conversation is uploaded with `reply_to`, and the cloud links it (`response`,
   `thread`). Pages to TUIs carry the replies (`FeedPage.replies`), and a reply read whole in
   the TUI's feed shows its thread under its turn.
   **Session list** (`feed/sessions.rs`, `SessionSync`): under the same switch the service
   uploads the machine's whole list of sessions (`POST /api/v1/feed/sessions`, at most 200, never
   private or dead ones): what the TUI's Status, Folders and App tabs group by (name, folder
   name, branch, agent, or without a named one the `program` the App tab files it under (its
   `AppKey`, never a shell), `status`, `lifecycle`, activity `phase`, the conversation shown,
   `pinned`, `last_active_at`), when it changes (at most every 3 s) and every 60 s meanwhile, so
   rakumux.com's Status, Folders and App pages list the account's sessions, show a session's
   conversation from the uploaded chats, and tell a machine that's away. Each session's 8 most
   recent places go too, as names only (`PlaceState`: a path in the session's folder, `.` for
   the folder itself, else its last component; branch, worktree, `edited`, `last_at`), and
   only once the cloud's answer said it takes them (`sessions-stored` `accepts: ["places"]`); a
   list that would pass the contract's 256 KiB goes without them.

## File structure

Agent-specific hooks, detection profiles, resume code and the OpenCode plugin/installer live
under `integrations/{open-code,claude-code,codex}/`. Shared engines live in `integrations/`.
See [integration boundaries and protocol](integrations/README.md). New integrations go there,
not in UI handlers, API/MCP transport or control policy. Optional version-1 `Report.activity`
is self-only (`session.activity` capability). The daemon validates, timestamps and expires
activity, and drops a busy hook state that the agent's own screen has shown idle since the
last hook (a turn that ended without one). The TUI animates busy markers locally without
network calls.

```
rakumux.sh                 # launcher (entry point)
build.sh                     # local build
release.sh                   # binary-only GitHub release entry point (--targets selects platforms)
api/                         # rakumux-api (external HTTP API), see api/README.md
  openapi.yaml               # the API contract (OpenAPI 3.1)
  src/                       # main, server, instance (flock), discovery (api.json), remote
                             # (remote.json, Tailscale addresses), routes, guard (Host/Origin/
                             # bearer, identity, remote rules), query, sessions, streams (WebSocket
                             # attach and events), folders, daemon (clients per identity), dto,
                             # error, detach, probe, log, time
  tests/                     # api_e2e (real binary + real daemon, remote listener on loopback,
                             # WebSocket clients), openapi_sync
mcp/                         # rakumux-mcp (MCP server for agents), see mcp/README.md
  src/                       # main, server (rmcp adapter), tools, client (API, discovery, identity),
                             # launch (rakumux-api --detach), render, untrusted, instructions
  tests/                     # mcp_stdio (stub API), full_stack (mcp → api → ptyd)
shell/                       # launcher, build and hook scripts (see shell/AGENTS.md)
install/                     # npm, Homebrew, release scripts (see install/AGENTS.md)
landing/                     # marketing site: standalone Next.js static export for Vercel (landing/README.md)
auth/AGENTS.md               # deferred auth design notes
tests/                       # manual test instructions and remaining shell tests
tui/
  Cargo.toml                 # workspace: ".", crates/vt, crates/session-core, crates/ptyd, ../api, ../mcp
  crates/
    session-core/src/
      lib.rs                 # PROTOCOL_VERSION, env variable names
      model.rs               # ScopeRoot, SessionView, SessionStatus, Lifecycle, Creator, AgentInfo,
                             # Reported, LaunchSpec, ResumeSpec, TerminalView
      ids.rs                 # SessionId (ULID), PaneId (standalone terminal), TerminalId, Token
      api.rs                 # Hello/Auth, Request, Response, errors, stream commands/events, events
      frame.rs               # [u32 len][u8 kind][payload] codec
      paths.rs               # Layout (~/.rakumux, run dir, socket), Registration, find_ptyd_binary
      remote.rs              # RemoteConfig (remote.json), pairing token, is_tailscale (the ranges)
      tailscale.rs           # this machine's Tailscale addresses (ranges + Tailscale's interface, or
                             # `tailscale status`), MagicDNS name
      client.rs              # SessionApi trait, Client, AttachStream, EventStream
    vt/src/
      lib.rs                 # Emulator (passive / responding), query responder, Modes, Event
      input.rs               # encode_key, encode_paste, encode_mouse
    vt/examples/             # spike.rs (fidelity), throughput.rs
    ptyd/src/
      main.rs, server.rs     # `rakumux-ptyd serve | --version`; accept loop, hello, dispatch, streams
      registration.rs        # single-instance claim, socket, service.json
      control_api.rs         # Control trait (server ↔ control)
      control/               # ControlService: registry, scope, identity, permissions, waits, audit,
                             # replies (reply capture's memory-only ring), places (where sessions
                             # worked)
      host.rs                # TerminalHost trait (control ↔ terminals), SpawnSpec, HostEvent
      terminal/              # Host: PTY actors, emulators, replay ring, attach fan-out, adopt/handoff
      handoff.rs             # restart in place: HandoffState across exec, target check
                             # detection/resume modules are mounted from integrations/
      persist.rs             # ScrollbackStore: sessions/<id>/{scrollback.ansi, meta.json}
      procinfo/              # foreground process, cwd, RSS (Linux /proc, macOS libproc); the process
                             # table, the machine's counters, TCP traffic per socket (tcp.rs, Linux)
      resources.rs           # resource_usage: the daemon's, clients' and sessions' processes
      log.rs                 # ~/.rakumux/logs/ptyd.log
  src/
    main.rs                  # cli::dispatch, else the TUI event loop
    cli/                     # new, ls, attach, rename, kill, rm, daemon, remote, account, session (JSON)
    account.rs               # the account service's clients: AccountAccess (the TUI's port), LocalAccount
                             # (starts `rakumux status-service`), `status-service` itself
    remote_access.rs         # phone access: remote.json on/off/rotate, (re)starting rakumux-api, pairing
                             # (URLs, token, rakumux://connect link), autostart; PhoneAccess (the TUI's port)
    qr.rs                    # QR codes drawn with ▀ (pairing: `rakumux remote pair`, the pairing dialog)
    app.rs, app/             # TUI state, folders and the folder filter, handlers, session pipelines, config
                             # (see tui/src/app/AGENTS.md)
    ui.rs, components/       # rendering: session list, detail, Folders modal, dialogs, footer (session,
                             # messages, mode label), welcome tour, phone access (offer, pairing dialog with QR code)
    terminal_view/           # live terminal pane widget, key/mouse/paste encoding
    layout/                  # Master detail / Grid / Tabs geometry, tab bar, session cycling
    help/                    # Settings modal (General, Keys: actions table) and Help modal (docs, license)
    naming/                  # AI session naming (Anthropic / OpenAI over HTTPS), not while AI titles are on
    auth/                    # Clerk auth, deferred (feature flag off)
    escape_time.rs           # keys whose escape sequence arrives in pieces (phones over SSH): a lone
                             # Esc waits for the rest of an arrow, Home/End, PgUp/PgDn, F1-F4 or a focus
                             # report: 25 ms on a local terminal and a desktop's over SSH (a key is one
                             # write); a phone over SSH (SSH_CONNECTION, SSH_CLIENT or SSH_TTY, and under
                             # layout::PHONE_BELOW columns) 300 ms in session mode and dialogs, 800 ms in
                             # Rakumux mode (the list and the feed)
    updater.rs, version.rs, theme.rs, icons.rs (plain / emoji / Nerd Font marks)
```

## Session model

`session_core::model::SessionView` is what every client sees: `id` (ULID, opaque), `name` (unique
within the scope), `scope`, `is_self`, `cwd`, `status`, `exit_code`, `lifecycle`, `agent`,
`reported`, `created_by`, `parent_id`, `depth`, `children`, `launch`, `resume`, `created_at`,
`last_active_at`, `pinned`, `git` (`GitInfo {repo_root, branch, detached_at, worktree, repo}` of
the current cwd; `repo`: the `origin` remote URL's last path component without `.git`, from the
common git dir's `config`, else the main working tree's folder name (a bare repository's own,
`x.git` → `x`); a submodule names itself), `agent_output` (a live Claude Code, Codex or OpenCode session's latest reasoning,
tool call or reply as one line, from its conversation log or database: `integrations/output.rs`),
`user_prompt` (what the user last typed to that agent, read the same way), `agent_turn` (`{id,
messages, conversation_id?}`: that agent's current turn, the user's last prompt and every message
since, whose content `session_turn` returns; the conversation tells chats apart for their one
automatic AI title), `places` (below), and `terminal` (pid, foreground process, title, size,
`controlled`, `alternate_screen`) while live. Folders are `ProjectView {path, name, sessions, live,
last_active_at, added_at, git}` on the wire.

- **Places** (capability `session.places`; `integrations/places.rs`, `control/places.rs`):
  where the session has changed files, most recent first, at most 32: `SessionPlace {path, git?,
  last_at, edited}`. A place is a git working tree, a repository's main checkout or a linked
  worktree (`path` = its `repo_root`, `git` its branch, or a detached HEAD's commit, linked
  worktree and repository name when last worked in; one kept before names gets it when touched), where its agent changed files (`edited`, always true): written
  or edited with its file tools (Claude Code's `Write`, `Edit`, `MultiEdit`, `NotebookEdit`;
  OpenCode's `write`, `edit`, `multiedit`) or patched (`apply_patch` however Codex calls it,
  OpenCode's `patch`). Working trees it only worked in (started in, `cd`'d into, searched, read
  from) are kept in memory and the registry, never listed, so one becomes a place, at its latest
  touch's time, once files there change: a session started in a main checkout that made a
  worktree and changed files only there lists that worktree alone. A known limit: other shell
  commands' writes (`sed -i`, a redirect, a script) aren't seen. A directory outside any
  repository is never a place, and those an earlier daemon kept are dropped as the registry is
  read (and from it at the session's next save). The daemon reads them from its agent's
  conversation, from its start (512 KiB a second) and then as it grows: Claude Code's entries'
  `cwd` and its tools' file paths, search roots, `EnterWorktree` and Bash `cd`/`git -C`; Codex's
  turns' `cwd`, shell calls' `workdir` and commands, and `apply_patch`'s files; OpenCode's tools'
  paths and commands. Its agent's working-directory reports and its terminal's directories count
  as worked in. Times are the conversation's own, so reading it again changes nothing, and a
  working tree's moves on a minute at a time; each session keeps 32 of each kind (changed, only
  worked in). Working trees in the agents' own data directories
  (`~/.claude`, `~/.codex`, OpenCode's but its worktrees, Claude Code's `/tmp/claude-<uid>`) and
  in `/proc`, `/sys`, `/dev` are never places. Kept in the registry, so ended sessions keep
  theirs; only paths are read. They stay on this machine (not in `rakumux-api`), except their
  names in the feed's session list (below).
- **Status**: `live` (terminal running), `exited` (process ended; final screen kept; Enter
  restarts), `dead` (record without terminal, e.g. after a reboot or daemon restart; saved
  scrollback shown; Enter revives). A session whose program the user ends from inside it in
  the TUI (`exit`, Ctrl-D, quitting the agent, while that window controls it in session mode) is
  deleted by that TUI right away; one that ends in the background stays `exited`.
- **Lifecycle**: what the program (usually an agent) is doing: `working`, `blocked` (needs the user),
  `done` (finished a turn, not yet looked at), `idle`, `unknown`, `none` (no agent). Detected by the
  daemon from the screen and foreground process; agents can add a self-report.
- **Revive** starts a new terminal for a dead or exited session (same id, cwd and launch spec) and
  types its resume command (startup commands, a known agent's resume form, or one the agent
  reported). The daemon revives the sessions that were running by itself when it starts after an
  unplanned end (see "Recovery").

### Recovery

What survives what (`ptyd/src/handoff.rs` and `integrations/recovery.rs`):

| Event | Running programs | How sessions come back |
|---|---|---|
| Restart in place (`rakumux daemon restart`, `rakumux --new-server`, the TUI's restart offer, SIGUSR2) | keep running (same pids) | nothing to do: the new image adopts the PTYs, screens, history, tokens and bindings; clients reconnect, a TUI in session mode goes back in |
| Crash, `kill -9`, reboot, SIGTERM | end | the next daemon start revives every session listed in `~/.rakumux/running.json`, with its resume command |
| `rakumux daemon stop` (`daemon_shutdown`) | end | not revived (the user ended them); Enter revives one |

The resume command (`SessionView.resume`, `ResumeSpec`) is kept current while a session runs: every
few seconds, or when the foreground process changes, the control layer asks `resume::plan` about the
PTY's foreground process. A known agent gives `KnownAgent` (run on revive): Claude Code
(`~/.claude/sessions/<pid>.json` → `claude <flags> --resume <id>`), Codex (the rollout file the
process has open, else the newest `cli` thread of the folder in `~/.codex/state_<n>.sqlite` →
`codex resume <flags> <id>`) and OpenCode (the conversation its plugin bound, else the newest
top-level session of the folder in `opencode.db` → `opencode <flags> -s <id>`, or `opencode2 …`
when V2 was started under that name); each keeps the
flags that shape an interactive session (permissions, model, …). Any other program gives
`LastCommand`: its command line is typed at the prompt on revive but not run. A shell back at its
prompt for 15 s clears it. An agent's own report (`ReportedByAgent`) wins while an agent runs.
Startup commands always run on revive.

## Protocol (v1)

Defined in `session-core/src/{api.rs, frame.rs}`.

- Unix socket `/tmp/rakumux-<uid>/ptyd-<hash>.sock`; peers with another UID are rejected.
- Frames: `[u32 BE length][u8 kind][payload]`, max 8 MiB. Kind 0 = JSON control message; kind 1 =
  raw terminal bytes (output frames start with a `u64` offset).
- First message `hello { protocol, client, version, auth }` with `auth` = user token (from
  `service.json`), session id + token (from the session's environment), or `delegated` (user token +
  agent kind + conversation id: acts as the running session that bound that conversation; capability
  `auth.delegated`). Then strictly alternating request/response, or a stream after `attach` /
  `subscribe`.
- Requests (`op`): `ping`, `session_list`, `session_get`, `session_create`, `session_rename`,
  `session_kill`, `session_delete`, `session_revive`, `session_report`, `session_read`,
  `session_wait`, `session_send`, `session_submit`, `session_touch`, `session_pin`,
  `session_bind`, `project_list`, `project_add`, `project_remove`, `attach`, `subscribe`,
  `daemon_status`, `daemon_shutdown`, `daemon_restart` (user only, capability `daemon.restart`:
  `{binary?}`; the daemon checks that `binary handoff-versions` reads its state, answers `ok`,
  closes every connection and execs it). `session_list` and `subscribe` take `scope` and/or `scopes`
  (user callers get the union), or `all` (user callers: every scope, including ones that get
  their first session after a `subscribe`; `scope`/`scopes` are then ignored). Capabilities
  `projects`, `session.pin`, `session.git`, `auth.delegated`, `sessions.all` and `daemon.restart`
  advertise the additions. `session_turn {id}` (user only, capability `session.turn`) returns a
  live agent's current turn: `TurnContent {id, prompt?, messages, total}`, the newest 100
  messages, each one line of at most 600 characters, for AI titles. `session_submit {id, text,
  quiet_ms?, expect_foreground?}` (paste, then Enter in a write of its own 500 ms later, nothing
  written between them: Claude Code drops an Enter that comes with a paste ending in `.png`/`.jpg`/`.gif`/
  `.webp` while it checks for an image, leaving the text unsent): with `quiet_ms` (capability
  `session.submit.quiet`) it's refused with `session_in_use`, unaudited, while the session's
  controller typed within that time; with `expect_foreground` (a process name, capability
  `session.submit.expect`) it's refused with `foreground_changed`, unaudited, unless that
  program is in the foreground as the daemon reads it again then; a daemon without them submits
  anyway. With `expect_foreground` the Enter is never written once another process group took
  the foreground after the text (`pty_io::write_inputs`): text meant for an agent that just
  exited stays unrun at its shell's prompt (without it, the text is type-ahead, as typing is).
  The daemon asks the terminal without its state lock.
- `session_import {source}` (user only, capability `session.import`) restores an archive directory
  containing a schema-3 `registry.sqlite3`, saved `sessions/` files and optional `history/` logs.
  The daemon preserves ids and metadata, refuses existing ids or names, stages files privately,
  and inserts the records in one transaction. It publishes `created` events but starts no programs.
  Stop the source daemon before reviving imported sessions to avoid duplicate agent conversations.
- `project_relocate {from,to}` (user only, capability `projects.relocate`) updates saved folder
  and cwd prefixes after a directory move, in one transaction. Existing sessions and their
  programs stay running; an unrelated existing source folder is refused. Reconnect clients
  afterwards (an in-place daemon restart preserves programs and refreshes authenticated scopes).
- Attach stream: checkpoint (base64 screen state) + offset, then output frames and stream events
  (`resized`, `role_changed`, `title`, `cwd`, `bell`, `clipboard`, `exited`, `lagged`); the client
  sends input frames and `resize` / `promote` / `demote` / `detach`.
- Standalone terminal panes (user only, capability `terminal.pane`): `terminal_open {id, cwd,
  env, cols, rows}` starts a login shell and opens a stream; `terminal_attach {id, role, cols,
  rows}` reconnects an existing pane after an in-place daemon restart, never creates one.
  The first response is `terminal_attached {id, cwd?, pid?, role, cols, rows, checkpoint, offset}`,
  not a `SessionView`. `id` is an opaque `PaneId`, not a session id. No registry/session files,
  identity environment, session events, `running.json` entry or crash recovery are created.
  Hiding keeps the owning stream open; shell exit or closing that stream releases the PTY.
  `checkpoint` requests a fresh `resized` checkpoint without resizing or taking control.
  Handoff state version 2 preserves pane PTYs; the daemon still reads version 1 for upgrades,
  but refuses a restart target that cannot read version 2 before affecting any programs.
  A pane with no reconnected owning window is reaped after a 30-second handoff grace period.
- Self-only `Report.working_directory` (capability `session.working_directory`) is
  `{agent, conversation_id?, path}`. The daemon validates an absolute existing directory and
  keeps the last report for that agent process. `SessionView.git` uses it while that agent
  runs; `SessionView.cwd` and scope retain their terminal/folder meanings. A shell or a new
  agent process uses the terminal cwd until it reports its own context. The normal two-second
  git poll follows the selected context and publishes updates when HEAD changes. Agent
  adapters report documented root context, never inferred tool paths or a different
  conversation's newest worktree. Foreground cwd polling continues without terminal output;
  old OSC 7 reports lose priority when their process or actual process cwd changes.
- Event stream: `EventEnvelope { scope, session_id, at, event }` with `created`, `updated`,
  `removed`, `exited`, `lifecycle`, `reported`, `activity`, and `reply` (below).
- Agent replies (user only, capability `session.replies`; `control/replies.rs`,
  `integrations/output.rs`): `reply_capture {enabled}` turns capture on or off (off at every
  daemon start and restart; off forgets what was kept). While it's on, a live Claude Code or
  Codex session whose lifecycle turns `Done` after it was seen working has its reply read from
  the conversation log the daemon already follows for `agent_output` (Claude Code: the main
  conversation's text after its last tool call; Codex: `task_complete`'s
  `last_agent_message`), within 10 s, never twice and never for a private session:
  `AgentReply {id, session_id, agent, conversation_id?, text (≤ 32 KiB), truncated,
  finished_at}`. Replies are kept in daemon memory only (the last 200, 2 MiB of text, an
  hour), listed by `reply_list {after?}` and delivered as `reply` events only to
  subscriptions with `replies: true`. The audit log records turning capture on or off, never
  reply text; session callers, delegated callers and `rakumux-api` never see replies. OpenCode
  replies are its turn's last text block. A reply's optional `position` is its turn's last
  message's in the conversation (below).
- Agent conversations (user only, capability `session.transcript`;
  `integrations/transcript.rs`): `session_transcript {id, after?, max_bytes?}` reads a live
  session's agent conversation on demand, nothing kept in memory, from the log or database the
  daemon already follows: `TranscriptPage {agent, conversation_id, messages, cursor, more}`.
  A message is `{id, position, role (user | assistant | tool), at?, parts}`; parts are
  `text`, `reasoning`, `tool_call {call_id?, name, input}` and `tool_result {call_id?, output,
  error?}`, each cut at 64 KiB (`truncated`). Claude Code and Codex: one message per log line at
  its byte offset (the cursor is the next offset; prompts without the agents' insertions,
  subagents' and meta entries left out). OpenCode: rows changed since a `(time_updated, id)`
  watermark, OpenCode 2 one message per reply block (`seq × 1024 + block`), 1.x one per piece.
  About 512 KiB of log is read per request (at most 2 MiB); `more` says ask again. A cursor of
  another conversation reads the current one from its start.
- Resource use (user only, capability `resources`; `resources.rs`, `procinfo/`):
  `resource_usage` answers a `ResourceSample {at_ms, machine, daemon, clients, sessions}` read
  when asked, nothing kept between requests: the machine's counters (`MachineUsage`: CPUs, busy
  and total CPU ms, memory total and available, bytes in and out on every interface but
  loopback), the daemon's process, each connected client's process once however many connections
  it holds (`ClientUsage {client, connections, process}`: the hello's client name and the peer's
  pid, `SO_PEERCRED` / `LOCAL_PEERPID`; one running inside a session is listed under it instead),
  and each live session's processes (`SessionUsage {id, processes}`: the program its terminal
  started and all its descendants from the process table, depth first, each naming its `parent`,
  at most 500). A `ProcessUsage` is `{pid, parent?, name, command (≤ 200 chars), cpu_ms,
  rss_bytes, net_rx_bytes?, net_tx_bytes?}`, all cumulative: clients divide two samples'
  differences by the time between them. Network per process is Linux only: the bytes each TCP
  socket it holds received and had acknowledged (`tcp_info` over `NETLINK_SOCK_DIAG`, which needs
  no privilege, `procinfo/tcp.rs`), sockets found through `/proc/<pid>/fd`, connections to
  loopback left out, a socket two processes share counted for the first listed. macOS gives
  CPU and memory per process (libproc, Mach time converted with the timebase) and the machine's
  counters (`host_statistics`, `getifaddrs`' 32-bit counters).
- **Compatibility**: new fields optional; enums carry an `Unknown` fallback; an op never changes
  meaning (add a new op); bump `PROTOCOL_VERSION` only for incompatible changes.

### Permissions

| Caller | How it authenticates | May |
|---|---|---|
| User (TUI, CLI outside any session) | token from `service.json` | everything |
| Session (any process inside a session, e.g. an agent) | `RAKUMUX_SESSION_ID` + `RAKUMUX_SESSION_TOKEN` | list/get/read/wait/subscribe within its own folder (naming another, or `all`, → `out_of_scope`); create sessions in its own folder, within `control::Limits` (→ `limit_exceeded`); report, touch, rename and pin **itself**; bind its agent's conversations to itself (`session_bind`); everything else → `permission_denied` (agent control of other sessions is planned for 0.7.0) |
| Delegated (the API for an agent conversation, e.g. OpenCode 2.x via `rakumux-mcp`) | user token + agent kind + conversation id | exactly what the session that bound the conversation may do; `unauthenticated` when no running session bound it |

`control::Limits` apply only to sessions created by sessions: at most 4 running per creating session,
depth 2 (a session the user created is depth 0), 8 running per folder and 10 creates per creating
session per minute. `rakumux new` inside a session needs `--detach`, since sessions can't attach.

Conversation bindings (`session_bind`) live in daemon memory: the newest binding of a conversation
wins, and a session's bindings end when it exits. OpenCode 2.x runs MCP servers in a service shared by
every conversation, so the Rakumux plugin for its TUI (`integrations/open-code/rakumux-session/`,
installed with `rakumux-mcp opencode-plugin --install`) binds the conversations it shows.

Mutating requests and denials are written to the audit log. Tokens give attribution and prevent
accidents; they don't stop a hostile process running as the same user.

## Data layout and schema

```text
~/.rakumux/                    ($RAKUMUX_HOME overrides)
  rakumux.sqlite3              registry, daemon only: sessions_v2, audit_log
  sessions/<id>/scrollback.ansi  saved scrollback (ANSI rows, oldest first)
  sessions/<id>/meta.json        SessionMeta {cwd, title, foreground_process, exit_code, updated_at}
  history/<id>.log               command history written by shell/rakumux-command-hook.sh
  logs/ptyd.log                  daemon log (1 MiB, rotated; level RAKUMUX_PTYD_LOG)
  logs/api.log                   log of a detached rakumux-api (RAKUMUX_API_LOG=debug: every request)
  logs/tui-input.log             terminal events read and handled, with the app's state (session-mode keys
                                 not logged): the last 500 written on quit and on `kill -USR1 <tui pid>`,
                                 every one live with `--debug`; for terminal apps that send keys unusually
  api.json                       ApiInfo {url, token, pid, version, remote_urls?} of the running
                                 rakumux-api, 0600; remote_urls: what its remote listener bound
                                 (only while one runs)
  api.lock                       flock held by the running rakumux-api (one per home)
  remote.json                    RemoteConfig {enabled, port (7337), token (26 chars a-z2-7),
                                 listen: ["ADDR:PORT"]}, 0600: remote access, written by
                                 `rakumux remote`, read by rakumux-api at start
  config.json                    Config (below)
  config.json.lock               serializes private configuration writes
  running.json                   {"sessions": [ids]}: sessions running now, revived by the next start
                                 after a crash or reboot; removed by `daemon stop`
  update_state.json              UpdateState {last_check, latest_version, pending_upgrade, install_method}
  session.json                   auth SessionRecord {session_id, token, email} (Clerk; deferred, unrelated to Rakumux sessions)
  sensagent/                     the account service (`rakumux status-service`): credentials/
                                 (the sign-in, 0600), status/ (descriptions and sharing consent,
                                 SQLite; titles.json: the title it last gave each session;
                                 chats.json; replies.json: replies written, typed in or
                                 decided here and reports not yet taken), machine.json (this
                                 machine's feed name and id), its registration and private socket
  uploads/<reply id>/            0700: files attached to feed replies (0600), fetched by the
                                 account service and typed to the agent as paths; kept 7 days
~/.rakumux/run/                ($RAKUMUX_HOME/run), 0700: the same from every login
  service.json                   Registration {instance_id, pid, protocol, version, socket, token}, 0600
  service.lock                   flock held by the running daemon (kept across a restart in place)
```

- **Registry** (`ptyd/src/control/registry.rs`): `sessions_v2(id, scope_root, name, cwd,
  launch_json, resume_json, created_by_json, parent_id, depth, agent_json, reported_json, exit_code,
  created_at, last_active_at, ended_at)`, index `(scope_root, last_active_at DESC)`;
  `audit_log(id, ts, scope_root, caller, method, target_session_id, decision, detail)`, pruned to
  30 days; `projects(path, added_at, last_used_at)` and a `pinned` column on `sessions_v2`;
  `session_places(session_id, path, git_json, last_at, edited)`, created when missing (a session's
  places, replaced together, deleted with it).
  `PRAGMA user_version = 3` (schema 2 is migrated in place, `projects` backfilled from session
  scopes); the 0.5.x `sessions` table is dropped on first open. Live
  state (terminal ids, session tokens) is in daemon memory only.
- **Config** (`tui/src/app/config.rs`): `Config {anthropic_api_key, openai_api_key,
  ai_primary_provider, openai_model}` plus `sessions {rakumux_key ("ctrl+\\"; an older
  `prefix_key` other than "ctrl+b" still counts), cycle_keys (["alt+up", "alt+down"]),
  cycle_keys_in_session (false), icons ("plain" | "emoji" | "nerd":
  the folder and branch marks), welcome_seen (false: the welcome tour shows on startup until
  its "Don't show this on startup" box is ticked), phone_offer_seen (false: "Use Rakumux from your
  phone?" was asked; it's asked once, when Tailscale runs here and `remote.json` doesn't exist yet),
  remote_replies (false: replies written to the feed on rakumux.com or other machines are typed
  into this machine's sessions; Settings → Replies from rakumux.com),
  sidebar_width (dragged divider),
  feed_width (the docked feed pane's width, set by dragging the line left of it; left out until
  dragged), stacked_list_percent (the list's share of the height on narrow screens, set by dragging the
  divider under it, so the split follows a phone keyboard coming and going;
  left out until dragged), session_order ({folder path: [session ids]}: the list order set by
  dragging or `J`/`K`; left out when empty), section_order ({"app:<name>" | "status:<status>":
  [session ids]}: the same under the App and Status tabs, each its own; left out when empty)}`. The folder filter isn't saved; the
  `selected_projects` of 0.6 previews is ignored and dropped on the next save.
- **Environment** injected into every session: `RAKUMUX_SESSION_ID`, `RAKUMUX_SESSION_TOKEN`,
  `RAKUMUX_SOCKET`, `RAKUMUX_BIN`, `TERM=xterm-256color`, `COLORTERM=truecolor`. Other variables:
  `RAKUMUX_HOME` (isolated data dir; tests), `RAKUMUX_PTYD_BIN` (daemon binary),
  `RAKUMUX_PTYD_LOG` (log level), `RAKUMUX_API_BIN` (the `rakumux-api` binary
  `rakumux-mcp`, `rakumux remote` and the TUI start), `RAKUMUX_API_LOG`, `GW_TUI_BIN` / `GW_PTYD_BIN` (launcher overrides),
  `ANTHROPIC_API_KEY` / `OPENAI_API_KEY` (AI naming fallbacks), `CLAUDE_CONFIG_DIR`, `CODEX_HOME`,
  `XDG_DATA_HOME` (where the daemon looks for agents' conversations, `resume.rs`). Sessions the daemon
  revives by itself get the daemon's environment (the one the client that started it had).

## Call stacks

### Launching the TUI

1. `rakumux.sh` (input: CLI args) resolves both binaries and execs `tui --ptyd-path <path>`.
2. `tui/src/main.rs` → `cli::dispatch` returns `None`; with `--new-server`, `cli::new_server`
   replaces the running daemon first (in place; an older one is stopped after asking) or starts
   one → TUI setup (raw mode, alternate screen) and first paint.
3. In the background: `Client::connect(ClientConfig { client_name: "tui", spawn: true, … })`. It
   connects to the daemon registered in `~/.rakumux/run` (or, when none answers there, one an
   earlier build registered under `$XDG_RUNTIME_DIR/rakumux`: `Layout::legacy`). Only if no daemon
   answers does it spawn `rakumux-ptyd serve` detached, wait up to 2 s for `service.json` and the
   socket, and complete the hello.
4. `add_project(launch folder)`, `projects()`, then `subscribe { all: true }` and
   `list_all(include_dead)` (capability `sessions.all`); a daemon without it gets
   `subscribe(scopes)` + `list_in` of every folder `projects()` returned, and one without
   `projects` `list(scope)` of the launch folder. The list keeps every folder's sessions and the
   folder filter narrows them locally (no resubscribe). The most recent session is selected and
   attached as observer (`attach`); `TerminalView::reset` applies the checkpoint, then output frames
   are fed as they arrive. Every resync (reconnect, `Ctrl-r`) does the same.
5. The event loop (`tokio::select!` over terminal input, the attach stream, the event stream and
   timers) updates `App` state; `ui.rs` + `components/` + `terminal_view` + `layout` + `help`
   render it.

### Interacting with a session

1. List focus: keys go to the app handlers, which call `SessionApi` (create, rename, delete, revive,
   …) and update state; `Option`/`Alt`+`↑`/`↓` cycles sessions (in session mode too when
   `sessions.cycle_keys_in_session` is on).
2. Enter sends `StreamCommand::Promote`; the TUI becomes controller and resizes the PTY to its pane;
   other attached clients get `role_changed`.
3. Session mode: keys pass through `terminal_view::key_to_bytes` (mode-aware) to
   `AttachSender::input`, except the Rakumux key (`Ctrl-\`), which demotes and switches to
   Rakumux mode (and the cycle keys when `cycle_keys_in_session` is on).

### A CLI command

`rakumux ls` → `rakumux.sh` → `tui` → `cli::dispatch` → `Client::connect` (user auth, spawn on
demand) → `SessionApi::list(ScopeRoot::resolve(cwd))` → text or JSON on stdout; exit code 0/1/2.

### Inside the daemon

1. `server` accepts a connection (thread per connection), checks the peer UID, reads `hello` →
   `Control::authenticate` → `Caller` (user or session).
2. Requests → `Control::handle` → registry + policy + `TerminalHost` calls → `Response`.
3. `attach` → `Control::prepare_attach` (permission, live terminal) → `TerminalHost::attach` →
   `attached` + stream. `subscribe` → `Control::subscribe` → `EventEnvelope` frames.
4. Per terminal: reader thread → actor (responding emulator; query replies go back through the
   writer thread) → fan-out to attachments; `HostEvent`s → control thread → `detect::detect`,
   lifecycle, events, scrollback persistence.

### An agent inside a session

`rakumux session report --state done --text …` → `$RAKUMUX_BIN` → `tui` → `cli` →
`Client::connect` (`AuthMode::Auto` picks session auth from the environment and uses
`RAKUMUX_SOCKET`) → `session_report` → control layer checks "self only", stores `reported`,
writes the audit entry and publishes a `reported` event → every TUI watching the scope updates.

### An agent through MCP

Agent → `rakumux-mcp` (stdio JSON-RPC, `tools/call read_session`) → `tools` resolves the
`session` argument (id, else name) → `client` reads `api.json` (health-checks the pid, or runs
`rakumux-api --detach`) → `GET /v1/sessions/{id}/content` with the bearer token and, when the
agent runs in a session, `X-Rakumux-Session-Id`/`-Token` → `rakumux-api` checks `Host`,
`Origin` and the token → `daemon` picks the client for that identity (`spawn_blocking`) →
`SessionApi::read` → daemon policy and folder (scope) checks → `ReadResult` → JSON →
`rakumux-mcp` sanitizes, caps and wraps the text in `<rakumux-output nonce=…>` → tool result
(structured content + text).

### A phone through remote access

`rakumux remote on` (`tui/src/cli/remote.rs`), or the one-time offer in
the TUI (`app/phone_access.rs`, through `remote_access::PhoneAccess` off the UI thread) →
`remote_access::enable` → `remote.json` `enabled` (new token if none) → SIGTERM to `api.json`'s pid
if that process is `rakumux-api`, wait → `rakumux-api --detach` → `server::serve` →
`remote::plan` (`session_core::tailscale::addresses`: `getifaddrs`, the Tailscale ranges on
Tailscale's interface; or `listen`) → binds the remote listener next to 127.0.0.1 → `api.json`
`remote_urls` → the URLs (MagicDNS name first, from `tailscale status --json`), the token and
`rakumux://connect?url=…&token=…`, shown as a QR code (`qr.rs`) by `rakumux remote pair` and
the TUI's pairing dialog; the app scans it. (The TUI does the `--detach` at startup when remote
access is on and no API runs; `remote_access::pairing` restarts an API that has no remote listener
once Tailscale has an address.) The app → `GET /v1/sessions?all=true` with `Authorization: Bearer <remote
token>` → `routes::handle(Listener::Remote)` → `guard::check_remote_bearer` +
`refuse_identity` → `Identity::User` → `SessionApi::list_all`. Opening a session →
`GET /v1/sessions/{id}/attach?role=controller&cols=&rows=` → `SessionApi::attach` (errors are JSON
before the upgrade) → 101 → `streams::run_attach`: a thread moves the blocking `AttachStream` onto
a channel; `attached` (checkpoint) first, output as binary frames, input and `resize`/`promote`/
`demote`/`fit` back as `StreamCommand`s, a ping every 30 s; closing the socket detaches.
`GET /v1/events` → `SessionApi::subscribe {all: true}` → `EventMessage`s.

### Restart in place

`rakumux daemon restart` → `SessionApi::restart_daemon(binary)` → server: user only,
`handoff::check_target` (`binary handoff-versions`), stop accepting, answer `ok` →
`Control::handoff` (tokens, spawn environments, bindings; registry closed) → `TerminalHost::handoff`
(each actor stops its reader, takes in what it read, and returns an inheritable PTY descriptor, its
screen checkpoint and history) → `Claim::hand_off` (socket and lock descriptors) →
`handoff::write_state` (unlinked 0600 file) → `execv(binary, serve --handoff-fd N)` → the new image
`registration::reclaim` (same instance id and token), `Host::adopt` (emulators restored, waiters
reap children that exited meanwhile), `ControlService::open_after_handoff` → serve. Clients
reconnect; `wait_for_restart` in the CLI and the TUI's reconnect (250 ms, then backing off) find the
new image because the socket stays bound throughout.

### Revive after a reboot or a crash

The daemon starts with every stored session `dead`, then revives those `running.json` lists
(`Control::revive_interrupted`, audited as `rakumux-ptyd`) with their resume commands. Others stay
dead: selecting one shows its saved scrollback (`session_read`); Enter → `SessionApi::revive` → the
control layer spawns a terminal with the same id, cwd and launch spec, a fresh environment snapshot
and token, and the resume command as startup input → the TUI attaches.

### AI naming

Signed in with the account's AI session titles on, the account service names sessions (component
10) and the TUI's own naming waits (`App::titles_on`). Otherwise, when a session with a generated
name first goes `idle` or `done`, the TUI reads `~/.rakumux/history/<id>.log` and the visible
screen (`session_read`), calls `naming::suggest_session_name` (the API keys in `config.json`),
and renames the session if a name comes back.

### Updates

`rakumux.sh` runs `shell/gwr/check-updates.sh` silently in the background (at most daily);
`U` (Shift+U) and Settings → Update use `UpdateService` off the UI thread. Binary
installs fetch the official GitHub release, verify SHA-256 and binary versions, reject unsafe
archive paths/links and custom install files, and atomically exchange staged/installed folders
(`renameat2` on Linux, `renamex_np` on macOS). Unwritable or unsupported installs fail without
changing the old bundle; source builds are not overwritten. Version probes use an isolated
environment/home. The user confirms installation; cached `UpdateState` is availability, never
startup authorization. Success reloads only the UI through the new launcher, stripping
`--new-server`. The daemon/API and all sessions stay running; Update has no stop/revive
fallback. Legacy npm/Homebrew installs use their package manager. On a protocol mismatch the TUI
asks before restarting it, and when a compatible daemon from an older build lacks features the TUI
shows (capabilities `session.git`, `projects`, `session.pin`, `sessions.all`, `terminal.pane`,
`session.working_directory`, `session.agent_output`, `session.user_prompt`, `session.turn`,
`session.replies`, `session.transcript`, `session.places`, `resources`, read with
`SessionApi::daemon_capabilities`) it says what's missing
and offers a restart: in place when the daemon has `daemon.restart` (sessions keep running;
`y`/Enter), otherwise by stopping it (`y` only, since running sessions end).

### Auth (deferred)

`tui/src/auth` (`AuthService`, `ClerkApi`, `FileStore`) is compiled but disabled
(`FeatureFlags { auth: false }`); see `auth/AGENTS.md`.

## Glossary

- **Session**: a folder + a terminal; durable record in the registry, live terminal in the daemon.
- **Folder** (scope, scope root; `project` on the wire): the directory a session was started in;
  the TUI shows all folders, grouped, and the folder filter narrows them; agents see only their
  own.
- **Folder filter**: the Folders tab's filter (`▤ Folders ▾`, `▤ web ▾`, `▤ 3 folders ▾`)
  and the Folders modal it opens; local to one window and not saved.
- **List tabs** (`app/grouping.rs`, `Grouping`): the session list's top row, Status, Folders
  and App: three ways of grouping the sessions. They decide the list's headers (per status,
  per folder, per app) and the one filter that applies; each keeps its own filter while another
  is active; none is saved, and every start (a restart or a UI reload) opens Status.
- **Feed** (`app/feed`, `f`): what agents said, across sessions and machines, beside the
  sessions: a pane on the right in Master detail and Grid, a tab in Tabs, the screen on a phone.
  It takes Rakumux mode's keys from the list while focused. Hidden at every start; never opens
  by itself.
- **Pin**: a user mark that lists a session in the pinned section, above every folder, app and
  status.
- **Session order**: where the user put each session in the list (drag, `J`/`K`); a TUI setting,
  not part of the registry.
- **Git context**: a session's branch, linked worktree and repository name, read from `.git` files
  (never `git`).
- **AI title**: a session's name when the account service gave it (once per agent chat, or when
  asked with `s`/↻); a name the user gave is never replaced automatically.
- **Place**: a git working tree (main checkout or linked worktree) where a session's agent
  changed files, as its conversation shows; listed under the session's row (`w`).
- **Reply** (agent reply): what an agent said when it finished a turn, captured by the daemon
  only while the status service has reply capture on, for the **feed**: the replies of the
  account's machines, kept on rakumux.com for 30 days while the account's Feed switch is on.
- **Chat** (transcript): an agent conversation's messages in order (prompts, text,
  reasoning, tool calls, tools' output), uploaded under the same switch; a reply read whole in
  the feed shows its turn from it.
- **Reply written back** (`FeedReply`): what the user wrote back to a feed entry, typed into
  the entry's session on its machine once its agent waits; with the entries that answer them
  they make a **thread**.
- **Daemon** (`rakumux-ptyd`): the single per-user server owning every session.
- **Client**: anything connected to the daemon (TUI, CLI call, agent, `rakumux-api`).
- **External API**: `rakumux-api`, HTTP on 127.0.0.1 over the daemon; found through `api.json`.
- **Remote access** (phone access in the TUI): the API's remote listener (Tailscale addresses, port
  7337) for the phone app, paired by scanning a QR code of the `rakumux://connect` link;
  opt-in with `rakumux remote on` or the TUI's one-time offer (`remote.json`), admitted by the
  remote token, always the user.
- **MCP server**: `rakumux-mcp`, the tools coding agents call; a client of the external API.
- **User caller / session caller**: a client authenticated with the daemon's user token, or with a
  session's own id + token (from inside that session).
- **Controller / observer**: the one attachment that sends input / any other read-only attachment.
  The terminal size follows the window being used (see "Terminal size across windows").
- **Checkpoint**: bytes that bring a fresh emulator to the current screen; sent on attach and
  resize.
- **Status**: live / exited / dead. **Lifecycle**: working / blocked / done / idle / unknown / none.
- **Report**: what a session says about itself (`session_report`): state, text, result, agent
  identity, resume command.
- **Revive**: a new terminal for a dead or exited session; **resume spec**: the command it runs
  (`known_agent`, `reported_by_agent`, `startup_command`) or types (`last_command`).
- **Restart in place**: the daemon execs a new binary in the same process and keeps every running
  program (`handoff`).
- **Recovery**: reviving, at the next start, the sessions that were running when the daemon ended
  without being stopped (`running.json`).
- **Registry**: the daemon's SQLite database. **Registration**: `service.json`, how clients find
  and authenticate to the daemon.
- **Rakumux key**: `Ctrl-\` by default; the one key a session never receives. It switches from
  session mode (keys go to the program) to Rakumux mode (keys drive Rakumux; the session is
  previewed). `rakumux attach` uses it as its prefix.
- **Layout**: Master detail (`sidebar`, default: list left, detail right; on narrow windows the
  list stacked above the detail in Rakumux mode), Grid (`grid`: paged read-only live terminal
  cards), or Tabs (`tabs`, tab bar on top). Not saved: every start opens Master detail. Phones
  offer Master detail (the list, then a session under tabs) and Tabs (the tab bar over the
  session), not Grid. The feed docks on the right in Master detail and Grid and is a tab in Tabs.
- **Top**: what the machine, Rakumux's own processes and every running session's processes
  use now (CPU, memory, network), from `resource_usage`; live only, never recorded.
- **Contracts**: the crate types and traits that define the architecture.

## Using Rakumux

| Command | Purpose |
|---|---|
| `rakumux` | TUI for the current folder |
| `rakumux new [--name N] [--cwd P] [--cmd C]... [--detach] [-- ARGV...]` | Create a session and attach |
| `rakumux ls [--json] [--dead] [--folder DIR]... \| --all` | Sessions of this folder, of the given folders, or of every folder (`--dead` includes dead ones; `--project` is a hidden alias of `--folder`; `--json` gives each session `folder` next to `scope`) |
| `rakumux attach <name\|id>` | Full-screen attach (prefix `Ctrl-\`: `d` detaches, `>`/`<` switch session, `n` new session) |
| `rakumux rename [<name\|id>] <new name>` | Rename (defaults to the current session) |
| `rakumux pin <name\|id>` / `rakumux unpin <name\|id>` | Pin (into the pinned section at the top of the list) / unpin |
| `rakumux kill <name\|id> [--tree]` / `rakumux rm <name\|id>` | Terminate / delete |
| `rakumux folders [--json]` / `rakumux folders add\|rm <dir>` | List folders / add one / forget one without sessions (`projects` is a hidden alias; `--json` prints `{"folders": [...], "projects": [...]}`, the same list twice) |
| `rakumux --new-server` | Replace the running daemon with this Rakumux's (in place: sessions keep running), or start one; then the TUI |
| `rakumux daemon start\|status\|stop\|restart\|logs` | Daemon management: `start` only when none runs; `restart` in place (an older daemon is stopped after asking); `stop` ends every session (not revived) |
| `rakumux session list\|get\|read\|wait\|report` | JSON commands for agents and scripts |
| `rakumux account login \| logout \| status [--json]` | The Rakumux account for AI session titles and the agent feed (also Settings → Account): `login` prints a link, a code and (on a terminal) a QR code and waits for approval in any browser; `status` shows who is signed in, whether AI session titles are on (rakumux.com/features) and each live session's AI description (`--json`: `ai_session_titles`, and `ai_session_summaries` as before) |
| `rakumux remote on [--port N] \| off \| pair \| status [--json] \| token [--rotate]` | Remote access for the phone app over Tailscale (the TUI offers it once): `on` saves it in `remote.json` (stays on), restarts the API and prints the URLs, the token, an `rakumux://connect` link and (on a terminal) its QR code; `pair` shows them again; `off` turns it off; `token --rotate` makes a new token (re-pair the app) |
| `rakumux-api [--port N] [--detach] [--remote-listen ADDR:PORT]...` | External HTTP API on 127.0.0.1, plus the remote listener when remote access is on (`api/README.md`) |
| `rakumux-mcp` | MCP server for agents, e.g. `claude mcp add --scope user rakumux -- rakumux-mcp` (`mcp/README.md`) |

TUI keys come in two modes. **Session mode**: everything goes to the program except `Ctrl-\`,
which switches to **Rakumux mode** (and `Option`/`Alt`+`↑/↓` when `cycle_keys_in_session` is
on). Rakumux mode (also where the TUI starts): `↑/↓`, `j/k`, `Option`/`Alt`+`↑/↓` select (the
session preview fades by 30% in Rakumux mode and 45% under a dialog or for saved output,
while the list never fades; the tabs, a header and + New session keep the last session in view,
one the filters show), `←`/`→` switch between the tabs, Status, Folders and App, wherever the
selection is (`h`/`l` on the tabs row), `f` the feed (shows it with the keys, gives it the keys,
hides it; below),
`gg`/`G`, `Ctrl-d`/`Ctrl-u`, `{`/`}` (previous/next folder, app or status) (a Ctrl or Alt
that no shortcut uses is ignored, so Ctrl-↑ from a phone's key bar moves and Alt-q quits), `Enter`/`i`/`l` go in (revive/restart; on a
folder header, a new session there), `Esc` (after cancelling a pending key or the filter) or
`Ctrl-\` back to the session you came from (with none, as after a start or a UI reload, into the
running session in view), `Tab` last session, `1`-`9` go into that session,
`n` new, `r` rename, `s` a new AI title for the session, `w` where the session has worked (`W`
every session's), `y` copy a place's path (below), `p` pin, `J`/`K` move the session down/up, `x` close, `d`/`dd` delete, `z` full
screen, `b` hide Master detail's list beside the session and show it again (below), `P` the Folders modal (Enter on the tabs: the active tab's), `/` filter, `Ctrl-y`/`Ctrl-e`/
PgUp/PgDn scroll the preview, `\` send `Ctrl-\` to the session, `t` Top (below), `?` Help, `,`/`o` Settings, `a` sign in (signed in: the account dialog; in the sign-in dialog `c` copies the link to the clipboard over OSC 52), `U` Update, `R`
ask to restart only this UI (`y`/Enter confirms, `n`/Esc cancels; reload the launcher and latest
build; daemon and sessions stay up), `S` ask to restart the server (in place with this build when
it can: `y`/Enter, sessions keep running; otherwise `y` only, since that ends them; also Settings →
Restart server, which shows the running server's version), `q`
quit. There is no header: the panes start at the top row, and the list's first row (Sidebar) is
its tabs, `◔ Status ▾  ▤ Folders    ◇ App` (Status is active at every start; every tab keeps
room for the active one's `▾`, and Folders shows its branch active or not, so switching tabs
moves nothing): the active one is bold, underlined and names its filter (`▤ web  ⎇ main ▾`,
`◇ claude ▾`, `◔ 2 statuses ▾`); short rows narrow the gaps, then drop the `▾` and its room
from every tab (`◔ Status  ▤ Folders  ◇ App`: names from 28 columns of list, the `▾` from 34),
then show the inactive tabs as their marks. The tab bar (Tabs) and Grid's header show the
active tab's filter at the left (`◔ Status ▾`); only Master detail shows the three tabs.
The session tab bar (Tabs, and a session under tabs on a phone or from Grid) holds the sessions
the active tab's filters show, in the Folders tab's order whatever the active tab
(`SessionList::tab_order`, `ShownAs::Tabs`: pinned first, then folder by folder in the order the
user set), so a tab keeps its place and number while its session works or is read; stepping
(cycle keys, `↑`/`↓`, `1`-`9`), clicks, drags and `J`/`K` there follow that order (`App::shown_as`).
Tabs that don't fit scroll to keep the selected one in view, holding still while it moves among
those shown (`layout::render_tabs_from`, `App::tab_first`), and the ends say how many are hidden
(`‹3`, `5›`; a click goes to the nearest one).
**The feed** (`f`, the footer's Feed button, or Settings; `app/feed/pane.rs`) is hidden at every
start and never opens by itself; the shown/hidden state is this window's, never saved. Master
detail and Grid dock it on the right (`layout::split_with`, `Screen::feed`): list (or Grid's
cards) | session | a one-column line (`Screen::feed_divider`, dragged to resize, saved as
`sessions.feed_width`) | the feed, `layout::default_feed_width` (44 columns, a quarter of wide
screens, at most 60) and at least `FEED_MIN_COLS` (30). The session narrows and takes the new
size at once, as a dragged divider gives it (`sync_terminal_size`); it docks only while the session
keeps `FEED_SESSION_MIN_COLS` (60) beside it (and Master detail its list or rail: hiding the list
makes room). Narrower windows from 80 columns show the feed in the session's place in Rakumux mode
only (beside or under the list; Grid's cards give way only while it has the keys) and session mode
keeps the session's full width. Tabs gives it the Feed tab, first in the tab bar before a `│`,
unnumbered, never moved, with the `●N` count; selected, the body under the bar shows the cards
and, from 100 columns, the selected reply beside them (`layout::feed_tab_columns`). On a phone it
fills the screen while it has the keys. It has Rakumux mode's keys of its own (`App::feed_has_keys`):
`f` shows it with them, gives them to it while the list has them, and hides it while it has them;
a click gives them to the side it lands on; `Esc` (after a pending key) gives them back to the list,
hiding it where it would leave the screen (a phone, narrow Grid); session mode gives them back too.
Its band (`≡ Feed`, as tall as the list's tabs) and the selected card's border are in the accent
while it has the keys. With them `↑`/`↓`/`j`/`k` go through the cards, `Enter` (or a second click,
a tap on a phone) reads the selected reply whole in the cards' place (a `‹` on the band, or `Esc`,
goes back; `↑`/`↓` scroll it; `Ctrl-d`/`Ctrl-u`, PgUp/PgDn page it), and `Enter` there (or beside
the cards) goes into its session when it runs here; `←`/`→` and the keys that act on the list's
session do nothing. Grid cards stay read-only observers: nothing in the feed sends
them Promote, Resize, Fit or input. It lists the account's machines' turns, newest first, each a card
(`components/feed.rs`, `app::feed::Card`): on its top border the session's name, then quieter
its agent and branch (the folder when no branch is known), and at the right its age and status
(`done`, `sending`, `working`, or the newest reply written back to it: `reply waiting`, `reply
typed in`, `answered`, `reply not sent`; only the mark when short of room); inside, the
prompt on a bar, then the start of the answer, its Markdown drawn
(`components/markdown.rs`, pulldown-cmark). A prompt
still waiting for its answer (rakumux.com's `open` turns, from the uploaded chats) is a card of
its own, `working`, until the entry that answers it takes the card over. A reply read whole is
its turn, its Markdown drawn, then its thread when someone wrote back; `r` (or a click on its
`↩ Reply` button) writes back to it, on a session's newest card only (`FeedStore::reply_to`), as
rakumux.com requires (an entry answering a reply is marked `↳`); on a `working` card, to the
session's newest entry when that's in the prompt's chat, else to the session itself (a session
message). The footer,
always one row at the bottom (in zoom too), is a full-width bar: orange with dark text in Rakumux mode, muted blue (Tokyo Night's `bg_visual`)
while a program takes the keys. At the left `session: ● <name>` with the status mark its list
row has (in Rakumux mode the selected session, else the one kept in view, or the selected card's
while the feed has the keys; the one taking the keys in session mode; none for the terminal
panel), since the detail has no title row: the session's terminal fills it from its top row;
then messages and the daemon connection while it isn't up (`⠋ connecting`, `✕ daemon
unavailable`). In the centre the mode label (`RAKUMUX MODE`, or a quiet `SESSION MODE`/`TERMINAL
MODE`), centred while it clears both sides, else between them. On the right buttons
(a darker shade of the orange bar; dark chips on the blue one): in Rakumux mode `f ≡ Feed`
(with `●N`, `●99+` beyond 99, for the feed entries that finished during this run and arrived
since the feed was last on screen: memory only, `FeedStore::unseen`, counted while the account's
Feed switch is on; it does what `f` does), `? Help` and `, Settings`, then the
Rakumux account at the bottom right: `a Sign in` while signed out (a tap asks for a sign-in code
at once), `a ✓ Signed in` while signed in (a tap opens the account dialog); `Ctrl-\` alone in
session mode (the label names the mode). To count while the feed is hidden the TUI asks the account service for
it every 10 s as a `background` read (every 2 s while it shows). When the row is short the
session gives way first (to its name alone, then nothing), then the message is cut, then
buttons go from the right (the account, Settings, Help, then the Feed's, which carries the
count: a 40-column phone keeps it and Help, Settings from 48); the label never goes, its short
word (`RAKUMUX`) under 14 columns.
Help and Settings list every key; Settings runs them. A welcome tour
(`components/welcome.rs`, `AppMode::Welcome`) opens on startup until its "Don't show this on
startup" box (on every card, Space) is ticked; `t` in Help replays it. Drag the divider to resize the
list, and the line left of the feed to resize it.
Beside the session (Master detail, 100 columns or more) the list's last row is `« Hide list`
(`Hit::HideList`): it, `b` or Settings hides the list (`App::toggle_list`, `PaneSizes::list_hidden`)
and the session takes its columns, leaving a `layout::RAIL_COLS` (2) wide rail at the left edge, a
sliver of the list pane with `»` at its bottom and the divider's line (`Hit::ShowList`: a click
shows the list again, as `b` does). Keys still move through the sessions while it's hidden. The session on screen takes the new pane size at once (`sync_terminal_size`, as a dragged
divider does). Stacked and phone screens, Grid and Tabs show no button and ignore it (`b` says so);
the state is this window's, never saved, and comes back when the window widens again.
When Tailscale runs on the machine and phone access was never set up (no `remote.json`), Rakumux
asks once, in Rakumux mode with no other dialog open (so after the tour), "Use Rakumux from your
phone?" (`AppMode::PhoneOffer`; `sessions.phone_offer_seen`): `y` turns it on and opens the pairing
dialog (`AppMode::Pairing`: the QR code of the `rakumux://connect` link, the URL and the token;
`x` turns phone access off), `n` leaves it off. After that, `rakumux remote on|off|pair`
changes it; Settings has no phone or AI naming rows (API keys come from `config.json` or
`ANTHROPIC_API_KEY`/`OPENAI_API_KEY`).

Phones (under `layout::PHONE_BELOW` = 80 columns; phone terminals
are about 40–60 columns in portrait) offer Master detail and Tabs, not Grid (a chosen Grid shows
as Master detail there, and `v` skips it). Master detail shows one thing at a time: Rakumux mode is
the session list alone, full screen (the feed, while it has the keys); a tap on a session (or
`Enter`) goes into it, and session mode
gives it the screen under a row of tabs, one per session (no folder filter; a tap on
a tab switches session and stays in session mode). Tabs keeps that row of tabs over the selected
session in Rakumux mode too. `Ctrl-\` (or a tap on SESSION MODE) goes back to Rakumux mode.
Moving through Master detail's list on a phone doesn't resize anything: no session is on screen
there.

Narrow windows (Sidebar from 80 to `layout::SINGLE_PANE_BELOW` = 100 columns): in Rakumux mode
the list is stacked on top, a one-row divider under it, and the selected session's detail (its
faded preview) below, so switching sessions shows them. The list gets 40% of the rows above
the footer, at least `LIST_MIN_ROWS` (10: the tabs, a folder's header and two sessions) and leaving the detail
`PREVIEW_MIN_ROWS` (4); dragging the divider sets that share (`sessions.stacked_list_percent`).
Below `STACK_MIN_ROWS` (15 rows above the footer, a 16-row screen) Rakumux mode shows the list
alone. Session mode fills everything above the footer. Taps arrive as clicks, so the footer is a
tap target too (`Hit::ModeBadge`, `Hit::FooterKey`, recorded by `components/footer.rs`): the
SESSION MODE label and the `Ctrl-\` button switch to Rakumux mode like `Ctrl-\`, the RAKUMUX
MODE label does what `Esc` does (back to the session you came from; while the feed has the keys, its
`Esc`), `f ≡ Feed` does what `f` does, and `? Help` and
`, Settings` open them. Dialogs are at least
min(screen width − 2, 48) columns (`components/utils.rs::popup_width`), wrap their key lines and
grow to fit their text; the welcome tour tightens its spacing when it doesn't fit, and Settings
puts values under their labels when both don't fit.

**Terminal size across windows.** A session is one terminal with one size, so every window shows
the program drawn for that size. The size follows the window being used (like tmux's `window-size
latest`): each window's own layout comes from its own width, and when its user does something (a
key, a click or a tap, the window coming to the front: focus reporting, `EnableFocusChange`), the
session it shows takes that window's session-mode pane size (`App::claim_terminal_size`: `Resize`
as controller, `Fit` as observer). The daemon ignores an observer's `Fit` only while the controller
typed in the last 5 s (`CONTROLLER_TYPING`), so whoever is typing keeps their size. Nobody fits
back by itself, so two windows never resize a session back and forth.

The list (`app/session_list.rs`) holds every folder's sessions; under the Folders tab each folder
shown has a header row (name, branch, live/total; selectable), a lone folder too. Inside a folder,
sessions whose git context is a linked worktree other than the folder's own working tree
(`grouping::linked_worktree`: one inside the repo, like `worktrees/feature`, or elsewhere) go
under a header for that worktree (`ListRow::WorktreeHeader`: its path in the folder, its branch,
live/total; selectable, Enter starts a new session there when it's inside the folder), after the
folder's own sessions and ordered by path. The section follows the agent's working directory, so
a session moves between them as its agent changes worktree; moves stay within one. The
list never reorders by itself: activity doesn't move a session or a group. The launch folder's
group comes first, then the others by folder name, then path. Sessions follow the order the user
set, and sessions without a place go last, oldest first, so a new session appears just above
"+ New session". All layouts share it (a row of session tabs always in this order).
Headers' names (a folder's, an app's, a status's, `pinned`) and their rows' text start in one
column; the two left of it are the gutter, where a status's or the pinned header's mark sits and
a row's marks go: its pin toggle, the places' `▸`/`▾`, the copy mark, and
`+ New session`'s `+`. With AI session titles on, a live session's name (its title) keeps two
columns after it: a spinner while a new title is being made, else the ↻ under the pointer
(`Hit::TitleRefresh`), which makes one; the row has no line for a description or the user's last
message. A row under a worktree's header starts a step further in, its text where
that header's words are and its gutter under its mark (`components/session_list.rs::nesting`);
ready's read-period headers (mark and words) and their rows (text, gutter and places) start one
column further in than ready's header and a row directly under any header (`READ_NEST`). A
dragged row's bar is just left of its gutter.
**Pinned sessions** sit in a section of their own at the top of the list, above every folder,
app and status, under every tab: a `★ pinned` header (`ListRow::PinnedHeader`, the gold pin,
`pinned` and how many; selectable, Enter does nothing), then each pinned session the active
tab's filter shows; the groups below leave them out, and an app or status left with no other
session drops its header. The section has an order of its own, the same under every tab
(`sessions.section_order` under `pinned`): the order the user gave it, then the rest folder by
folder. In the list they show no other mark, while the tab bar and Grid show ★ after their
name (`layout::PIN_MARK`). In terminals that report pointer movement (desktop terminals, also
over SSH; not phones), the session row under the pointer shows its pin toggle in the gutter
just left of its name: `☆` to pin it,
the gold `★` to unpin it; a click there (`Hit::PinToggle`) does that without selecting the row,
and `p` still pins the selected one. Dragging a session row (Sidebar) or tab (Tabs) moves it live (a blue bar marks the row while
held); `J`/`K` move it one place. Moves stay in the session's part of the list: the pinned
section (saved under `pinned`), or its folder (`sessions.session_order`). A click without movement still just selects.
The first load still selects the most recently used session.

The App and Status tabs group the same list differently (`app/grouping.rs`). App: a header per
app (`AppKey::of`: the session's `agent.kind`, else its foreground program unless a shell, else
`launch.argv[0]` unless a shell, else `shell`; an ended session keeps its agent), agents first,
then other programs, then `shell`, each by name, with live/total. Status: a header per status
(`StatusGroup::of`, like the rows' status marks: waiting for you, unread (a finished turn not yet
looked at), working, waiting for response (the `WaitingForResponse` activity phase), running
(live, nothing reported), exited, dead, then ready (idle: a reply that's been read)), in that order, with a session count. Each app and status has an order of its
own (`sessions.section_order`, by `Section::key`: `app:claude`, `status:working`): the order the
user gave them there, then the rest folder by folder (launch folder first); pinned sessions are
in the pinned section above, not here. Dragging and `J`/`K` reorder a session within its header across folders, never into
another header, and never change the folders' order. Ready is the exception
(`StatusGroup::by_last_read`): its sessions are listed by when each was last read
(`last_active_at`, which going in sets, as it turns an unread reply ready), most recent first,
under a sub-header per period (`grouping::ReadPeriod`, `ListRow::ReadPeriodHeader`: `◷ last hour`,
last day, older, the clock from `Icons::clock`, tinted like its sessions' marks; with a session
count; selectable, Enter opens the Status modal on ready; it and its rows a column in from
ready's), so reading one moves it to the top.
A pinned ready session is in the pinned section instead; unpinning puts one back in its period.
The periods are measured from
the list's clock (`SessionList::set_clock`, every tick), so sessions move down them as time
passes. Nothing there can be moved (`J`/`K` say so). A ready session's mark (`○`, wherever it's
shown) is amber like an unread reply's `●` but quieter (`format::read_period_style`: the amber
faded 30% when read within the hour, 50% within the day, then muted), and ready's header has the
last hour's. A row's details line has no status words
(its mark on the right says it): the app, subagents and report; an ended session still
says `exited · ⏎ restart` or `dead · ⏎ revive`. Under it, the row's places
(`components/session_list.rs::place_content`, `app/places.rs`). First the folder the session
works in now (`PlaceRef::Current`: its git context's working tree, else its cwd; its path in the
session's folder, the folder's name, or `~/…`), one line in the row's text column: its path, then
its repository's name (`format::repo_label`: not when its folder has that name) and branch
(`focal-md ⎇ paper-links`), even before it changed anything
there; short of room the path is cut at its start first (down to half the line), then the
repository's name (to 6 columns, then gone), then the branch at its end, never wrapped
(`fit_place`). When it has other places, `▸` sits in the gutter left
of it, with no count; `w`, `W` or a click
or tap on it (`Hit::PlacesToggle`) opens them under it (`▾`; closed by default, per window, not
saved; `app::places::listed_places`, never the current one): each one, most recent first, a pair
of lines two columns further in: its path, then its repository's name, branch and its age at the
right; a muted rule
(`─`) above each. No place has a mark or bar left of it. The list is the daemon's places, so a checkout the session only started in or
read from isn't among them. Paths are cut at their
start when narrow, the same on phones. `↓`/`↑` (`j`/`k`) step through the selected session's
open places pair by pair as if each were a row under it (`App::step`;
`SessionList::selected_place`, dropped by any other selection), and `y` copies the path of the
one they're on, else of the folder the session works in now, over OSC 52 (`App::copy_place`).
The place under the pointer (on any of its lines, not over the `▸`), and the one the keys are
on, shows `⧉` in its gutter (the current one's just left of its `▸`) and its path brighter (bold
for the keys). A click or tap on any of their lines copies its path (`Hit::Place`) without
selecting the row. A session taller than the list (its places open) shows the part around the
place the keys are on. A status
change moves a session to another header, but the selected session stays shown when it stops
matching the filter (an answered question), until something else is selected. Their modals
(`AppMode::Sections`, `components/section_picker.rs`) work like the Folders modal: All, then each
app or status with counts (Status lists every status); typing narrows, `Tab` (and `Space` while
nothing is typed) tick several, `Enter` shows them, `Esc` closes. A click on a tab activates it,
on the active one opens its modal; `P` opens the Folders modal from any tab and a folder picked
there makes the Folders tab active. New sessions still go in `App::current_folder`; a folder filter
only counts while the Folders tab is active.

New session (`n`, "+ New session", Enter on a folder header) creates immediately with a generated name,
selects the new session and enters session mode at its login-shell prompt, without a naming modal.
A single click on "+ New session" creates and enters it on desktop and phones; it never just selects
the action row. The
session goes in the current folder (`App::current_folder`: the selected session's or header's
folder, retained on the folder filter and "+ New session" rows; a single-folder filter takes
precedence; without folder context, the launch folder). A session made in a folder the filter
hides adds that folder to the filter. Other folders, commands and startup commands are CLI-only:
`rakumux new --cwd DIR --cmd C -- ARGV`.

**Grid.** Settings → Layout or `v` in Rakumux mode cycles Master detail, Grid and
Tabs (phones skip Grid). Grid fills the body above the one-row footer with up to six terminal
cards; a phone shows Master detail instead. Arrows or `h`/`j`/`k`/`l` select cards, PgUp/PgDn change page, Home/End go to the ends;
`P` and `/` keep filtering folders and text. Enter or a tap opens a session full screen under
tabs; `Ctrl-\` returns to Grid. `n` or the last + New session card creates immediately.
Only visible cards attach, always as observers, and are cancelled offscreen, on activation,
layout change, disconnect or shutdown. Previews use passive, zero-scrollback mirrors, bounded
mailboxes and checked generations; saved output is read with a cap. Browsing never sends
Promote, Resize, Fit, terminal input, Touch, clipboard, bell or terminal-query replies, even
through the hidden detail path. Cards crop/pan rather than resizing terminals. Default config
and wire spelling stays `sidebar`, now labelled Master detail; `grid` and `tabs` persist too.

**Top.** `t` in Rakumux mode or Settings → Top (`app/top.rs`, `components/top.rs`,
`AppMode::Top`) shows what this machine and Rakumux use now, read with `SessionApi::resources`
off the UI thread about every second while it's open (the first two samples half a second apart),
nothing recorded: a line for the machine (CPU of every CPU together, memory used of total, bytes
in and out a second; the bars go, then the parts wrap, when narrow), then rows: this window, the
server (`rakumux-ptyd`), the other windows, then the other clients (`rakumux-api`, the account
service, CLI calls), a blank line, and each session (name, app and folder) with its processes as
a tree under it when it runs more than one (each with its arguments, quieter), and their
`Total` pinned at the bottom. Columns: pid, CPU (a percentage of one CPU, as `top`; orange from
50%, pink from 100%), resident memory, and bytes received and sent a second over its TCP
connections (`—` where unknown); narrower, the pid goes, then received and sent merge, then
network goes. Rates are the last two samples' differences (a pid whose name changed gets none).
`↑`/`↓` (`j`/`k`), PgUp/PgDn and Home/End select; `←`/`→` (`h`/`l`) and Space fold and unfold a
session's processes; `s` sorts sessions by memory (the default), CPU, network or name; Enter
goes into the selected row's session; Esc (`q`, `t`) closes. A daemon without `resources` gets
"restart it (S)".

**Terminal panel.** `T` in Rakumux mode or Settings → Terminal panel opens a
temporary daemon-owned login shell under the main detail, without a session record or list entry.
It starts in the selected session's cwd, otherwise the current folder. `Focus::Terminal` routes keys,
paste, mouse and cursor only to that attachment, with a distinct TERMINAL MODE footer label.
The Rakumux key demotes it and returns to Rakumux mode; Esc returns to it, and T hides it.
The hide button keeps the stream and shell running. Exit/EOF closes the pane and releases its
terminal automatically; there is no session record to delete. Closing/reloading this UI ends
the temporary pane shell, never any Rakumux sessions. Drag its
divider to resize the split. The selected agent remains observed while the shell takes input;
only the focused terminal claims size, and scrolling an unfocused shell cannot resize the
agent. Short windows show the focused pane alone. Grid browsing keeps its grid and hides the
panel; focusing the shell restores the selected detail above it. Each window keeps one pane,
reusing it after hiding. All work uses SessionApi off the UI thread, with stale-result checks,
user-only daemon policy, passive emulators, bounded history and input-log privacy. In-place
daemon restarts preserve pane PTYs. Reconnects attach as observers until the user refocuses;
they never create replacement shells implicitly. An older daemon reports that an in-place
restart is needed; no fallback creates a normal or hidden session.

Settings is one scrolling page without tabs (`help::ModalTab::Settings`) under a search field:
typing narrows it to the settings and actions whose section or name has every word typed
(`Backspace` deletes, `Esc` clears, then closes; `Space` changes the selected row while nothing
is typed), and the dialog keeps its size meanwhile. The settings come first (Account, where you
sign in, then Available on sign-in, then Appearance, Keyboard and App), then under a `Keys`
rule every action and its keys, which `Enter` runs; `↑`/`↓` select among the rows shown; `→`
shows the selected row's note in a popover over the page (`←`, `Esc`, moving or `Enter` close
it), so the rows never shift; the wheel moves the selection. A setting that can't be changed
now (Available on sign-in, signed out) is greyed out. Labels sit on the left and values and keys are
pinned to the right. It's sized from its rows: centred, its widest row plus 20 columns (48–100),
capped by screen width with a side margin, and as tall as its rows, up to 80% of the screen's
height, scrolling only as far as the selected row needs. Help retains its wider reading area
and tabs. Settings and the account dialog keep two columns of padding inside their border and a
row above and below (`components::dialogs::padding`: no columns on a phone, no rows under 20
rows). Dialogs stay above the one-row footer so the mode bar remains visible on short screens.

Embedded terminals (the detail, Grid cards, the terminal panel) draw a program's default colours
in the outer terminal's own (`terminal_view::DefaultColors::NATIVE`), not the Rakumux theme;
colours a program sets itself are kept. The daemon still answers OSC 10/11 colour queries with
Tokyo Night's (`vt::Colors`).

The Folders modal (`P`, Enter on or a click on the active Folders tab; `AppMode::Folders`,
`app/folders.rs`) lists All folders (ticked when there's no filter), then every folder that has
sessions (and the filter's), with name, path, branch and live/total; typing filters the rows.
`Enter` on a folder shows it alone, on All folders every folder; `Tab`/`Space` tick several, and
then `Enter` shows the ticked folders plus the one under the cursor (`Tab`/`Space` on All folders
clears the ticks). `Esc` closes without changes. A typed folder path (starting with `/`, `~` or
`.`, or with a `/` in it) is offered as `+ Add folder` (or `+ Create folder`, confirmed with `y`)
with the folders that complete it listed below (`app/path_complete.rs`: folders only,
dot-folders once the name starts with `.`, capped reads); `↑`/`↓` choose one, Tab (or `→` at the
end of the query) completes the path with it, and Enter adds it (`add_project`) and shows it
alone, ready for new sessions. Forgetting a folder is CLI-only (`rakumux folders rm`).

## Shell wrapper integration

A program can't change its parent shell's directory. When the TUI quits with `c` and a session
selected, it prints `cd "<session cwd>"` on stdout after restoring the terminal
(`RunOutcome::SwitchDirectory`, `main.rs`). No wrapper ships yet: the TUI draws on stdout, so a
shell function that captures it (`$(rakumux)`) would hide the UI; a working wrapper needs the
directory written elsewhere (for example a file named by a flag, not implemented). The daemon is
always spawned with its stdio detached, so capturing a client's output never waits on the daemon.

## Local builds

- Build: `./build.sh`, or `cd tui && cargo build --workspace --release` (binaries in
  `tui/target/release/`: `tui`, `rakumux-ptyd`, `rakumux-api`, `rakumux-mcp`).
- Run a checkout: `./rakumux.sh`, or `shell/rakumux-local.sh` (often aliased as
  `rakumux-local`): default root `$HOME/repos/rakumux` (override with `RAKUMUX_LOCAL_ROOT` or
  `--root`), `--worktree <name>` runs `<root>/worktrees/<name>/rakumux.sh`, and arguments after
  `--` pass through (`rakumux-local -- --debug`).
- Run fully isolated from your real sessions:
  `RAKUMUX_HOME=$(mktemp -d) tui/target/debug/tui --ptyd-path tui/target/debug/rakumux-ptyd`.
  `RAKUMUX_HOME` gives a separate registry, run dir, socket and therefore a separate daemon.

## Development rules

- After each fix, run only its targeted regression test(s), adding one if missing
  (`cd tui && cargo test -p <crate> <test_filter> --locked`). Confirm they pass; do not rerun the full suite per fix.
- Run the full checks only when ready to finalize or release: from `tui/`,
  `cargo build --workspace --locked`, `cargo test --workspace --locked --no-fail-fast`,
  `cargo fmt --all --check`, plus relevant shell/website checks. No new warnings.
- Tests never touch the real `~/.rakumux`: set `RAKUMUX_HOME` to a temp dir.
- Doc comments on public items, as in the contract files. Explicit errors; no `unwrap()`/`expect()`
  on daemon request paths (tests may).
- Program the TUI and CLI against `SessionApi`, and test them with in-memory fakes
  (`AttachStream::pair`, `EventStream::from_receiver`). Test the control layer with a fake
  `TerminalHost`, and the server with a fake `Control`.
- Only the daemon answers terminal queries; client emulators are always `vt::Emulator::passive`.
- Contract changes are additive only (new optional field, new method with a default, new variant).
- Never run `git`, tmux or shell services from the product; git context comes from reading `.git`
  files only.

## Contributor documentation

- `README.md`: installation, usage, and project links.
- `CONTRIBUTING.md`: development workflow and verification.
- `SECURITY.md`: private vulnerability reporting and trust boundaries.
- `PUBLISHING.md`: source-publication boundaries and release checklist.
- `api/README.md` and `api/openapi.yaml`: HTTP and WebSocket contracts.
- `mcp/README.md`: coding-agent tools and identity.
- `install/AGENTS.md`: packaging and release commands.
