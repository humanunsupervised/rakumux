# Rakumux: Terminal Sessions for Coding Agents

Rakumux is a terminal-first manager for long-running terminal sessions, built for running coding
agents (Claude Code, Codex, OpenCode, Pi, …) side by side. A **session** is a folder plus a
terminal. One per-user daemon, **`rakumux-ptyd`**, owns every session; the **`rakumux`** TUI and
CLI are clients of it, and so is **`rakumux-api`**, a local HTTP API that the **`rakumux-mcp`**
server uses to let coding agents drive sessions. Source: `humanunsupervised/rakumux`;
website: <https://rakumux.com>. The core is licensed under Apache-2.0 (`LICENSE`).
The mobile app is maintained separately in `humanunsupervised/rakumux-mobile` and is not included
in this repository's license grant. Public preparation and release checks: `PUBLISHING.md`.

## Architectural requirements

1. **One server, many clients.** Exactly one `rakumux-ptyd` per user owns every session's
   terminal and the session registry. Any number of TUIs, CLI calls and agents connect at the same
   time. Several TUIs may watch the same session; at most one is its controller. No client is
   special and no client may assume it is the only one; closing every TUI leaves all sessions
   running.
2. **No git operations.** No worktree creation, git status, merge helpers or env-copy, and no
   `rakumux-cli`; the product never runs `git`. The one exception is read-only **git context**:
   the daemon reads `.git` files to show a session's branch and linked worktree
   (`ptyd/src/git.rs`). Users run git inside a session like any other program.
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
   command (`ptyd/src/resume.rs`; see "Recovery" below).
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

## File structure

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
      ids.rs                 # SessionId (ULID), TerminalId, Token
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
      control/               # ControlService: registry, scope, identity, permissions, waits, audit
      host.rs                # TerminalHost trait (control ↔ terminals), SpawnSpec, HostEvent
      terminal/              # Host: PTY actors, emulators, replay ring, attach fan-out, adopt/handoff
      handoff.rs             # restart in place: HandoffState across exec, target check
      resume.rs              # resume plans: Claude Code, Codex, OpenCode conversations; last command
      persist.rs             # ScrollbackStore: sessions/<id>/{scrollback.ansi, meta.json}
      procinfo/              # foreground process, cwd, RSS (Linux /proc, macOS libproc)
      detect.rs              # agent and lifecycle detection from screen + process
      log.rs                 # ~/.rakumux/logs/ptyd.log
  src/
    main.rs                  # cli::dispatch, else the TUI event loop
    cli/                     # new, ls, attach, rename, kill, rm, daemon, remote, session (JSON)
    remote_access.rs         # phone access: remote.json on/off/rotate, (re)starting rakumux-api, pairing
                             # (URLs, token, rakumux://connect link), autostart; PhoneAccess (the TUI's port)
    qr.rs                    # QR codes drawn with ▀ (pairing: `rakumux remote pair`, the pairing dialog)
    app.rs, app/             # TUI state, folders and the folder filter, handlers, session pipelines, config
                             # (see tui/src/app/AGENTS.md)
    ui.rs, components/       # rendering: session list, detail, Folders modal, dialogs, footer (mode badge,
                             # messages), welcome tour, phone access (offer, pairing dialog with QR code)
    terminal_view/           # live terminal pane widget, key/mouse/paste encoding
    layout/                  # Sidebar / Tabs geometry, tab bar, session cycling
    help/                    # Settings modal (General, Keys: actions table) and Help modal (docs, license)
    naming/                  # AI session naming (Anthropic / OpenAI over HTTPS)
    auth/                    # Clerk auth, deferred (feature flag off)
    escape_time.rs           # keys whose escape sequence arrives in pieces (phones over SSH): a lone
                             # Esc waits for the rest of an arrow, Home/End, PgUp/PgDn, F1-F4 or a focus
                             # report: 300 ms in session mode, 800 ms in Rakumux mode
    updater.rs, version.rs, theme.rs, icons.rs (plain / emoji / Nerd Font marks)
```

## Session model

`session_core::model::SessionView` is what every client sees: `id` (ULID, opaque), `name` (unique
within the scope), `scope`, `is_self`, `cwd`, `status`, `exit_code`, `lifecycle`, `agent`,
`reported`, `created_by`, `parent_id`, `depth`, `children`, `launch`, `resume`, `created_at`,
`last_active_at`, `pinned`, `git` (`GitInfo {repo_root, branch, detached_at, worktree}` of the
current cwd), and `terminal` (pid, foreground process, title, size, `controlled`,
`alternate_screen`) while live. Folders are `ProjectView {path, name, sessions, live,
last_active_at, added_at, git}` on the wire.

- **Status**: `live` (terminal running), `exited` (process ended; final screen kept; Enter
  restarts), `dead` (record without terminal, e.g. after a reboot or daemon restart; saved
  scrollback shown; Enter revives).
- **Lifecycle**: what the program (usually an agent) is doing: `working`, `blocked` (needs the user),
  `done` (finished a turn, not yet looked at), `idle`, `unknown`, `none` (no agent). Detected by the
  daemon from the screen and foreground process; agents can add a self-report.
- **Revive** starts a new terminal for a dead or exited session (same id, cwd and launch spec) and
  types its resume command (startup commands, a known agent's resume form, or one the agent
  reported). The daemon revives the sessions that were running by itself when it starts after an
  unplanned end (see "Recovery").

### Recovery

What survives what (`ptyd/src/handoff.rs` and `ptyd/src/resume.rs`):

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
top-level session of the folder in `opencode.db` → `opencode <flags> -s <id>`); each keeps the
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
  advertise the additions.
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
- Event stream: `EventEnvelope { scope, session_id, at, event }` with `created`, `updated`,
  `removed`, `exited`, `lifecycle`, `reported`, `activity`.
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
every conversation, so the Rakumux plugin for its TUI (`mcp/opencode/rakumux-session/tui.js`,
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
~/.rakumux/run/                ($RAKUMUX_HOME/run), 0700: the same from every login
  service.json                   Registration {instance_id, pid, protocol, version, socket, token}, 0600
  service.lock                   flock held by the running daemon (kept across a restart in place)
```

- **Registry** (`ptyd/src/control/registry.rs`): `sessions_v2(id, scope_root, name, cwd,
  launch_json, resume_json, created_by_json, parent_id, depth, agent_json, reported_json, exit_code,
  created_at, last_active_at, ended_at)`, index `(scope_root, last_active_at DESC)`;
  `audit_log(id, ts, scope_root, caller, method, target_session_id, decision, detail)`, pruned to
  30 days; `projects(path, added_at, last_used_at)` and a `pinned` column on `sessions_v2`.
  `PRAGMA user_version = 3` (schema 2 is migrated in place, `projects` backfilled from session
  scopes); the 0.5.x `sessions` table is dropped on first open. Live
  state (terminal ids, session tokens) is in daemon memory only.
- **Config** (`tui/src/app/config.rs`): `Config {anthropic_api_key, openai_api_key,
  ai_primary_provider, openai_model}` plus `sessions {rakumux_key ("ctrl+\\"; an older
  `prefix_key` other than "ctrl+b" still counts), cycle_keys (["alt+up", "alt+down"]),
  cycle_keys_in_session (false), layout ("sidebar" | "tabs"), icons ("plain" | "emoji" | "nerd":
  the folder and branch marks), welcome_seen (false: the welcome tour shows on startup until
  its "Don't show this on startup" box is ticked), phone_offer_seen (false: "Use Rakumux from your
  phone?" was asked; it's asked once, when Tailscale runs here and `remote.json` doesn't exist yet),
  sidebar_width (dragged divider),
  stacked_list_percent (the list's share of the height on narrow screens, set by dragging the
  divider under it, so the split follows a phone keyboard coming and going;
  left out until dragged), session_order ({folder path: [session ids]}: the list order set by
  dragging or `J`/`K`; left out when empty)}`. The folder filter isn't saved; the
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

`rakumux remote on` (`tui/src/cli/remote.rs`), or Settings → Phone access / the one-time offer in
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

When a session with a generated name first goes `idle` or `done`, the TUI reads
`~/.rakumux/history/<id>.log` and the visible screen (`session_read`), calls
`naming::suggest_session_name`, and renames the session if a name comes back.

### Updates

`rakumux.sh` runs `shell/gwr/check-updates.sh` silently in the background (at most daily);
`tui/src/updater.rs` keeps `UpdateState`, shows the upgrade dialog and runs `npm`/`brew` when the
user accepts, then restarts the TUI. Upgrading never stops the daemon; on a protocol mismatch the TUI
asks before restarting it, and when a compatible daemon from an older build lacks features the TUI
shows (capabilities `session.git`, `projects`, `session.pin`, `sessions.all`, read with
`SessionApi::daemon_capabilities`) it says what's missing and offers a restart: in place when the
daemon has `daemon.restart` (sessions keep running; `y`/Enter), otherwise by stopping it (`y` only,
since running sessions end).

### Auth (deferred)

`tui/src/auth` (`AuthService`, `ClerkApi`, `FileStore`) is compiled but disabled
(`FeatureFlags { auth: false }`); see `auth/AGENTS.md`.

## Glossary

- **Session**: a folder + a terminal; durable record in the registry, live terminal in the daemon.
- **Folder** (scope, scope root; `project` on the wire): the directory a session was started in;
  the TUI shows all folders, grouped, and the folder filter narrows them; agents see only their
  own.
- **Folder filter**: the TUI's top item (`▤ All folders ▾`, `▤ web ▾`, `▤ 3 folders ▾`) and the
  Folders modal it opens; local to one window and not saved.
- **Pin**: a user mark that lists a session first in its folder.
- **Session order**: where the user put each session in the list (drag, `J`/`K`); a TUI setting,
  not part of the registry.
- **Git context**: a session's branch and linked worktree, read from `.git` files (never `git`).
- **Daemon** (`rakumux-ptyd`): the single per-user server owning every session.
- **Client**: anything connected to the daemon (TUI, CLI call, agent, `rakumux-api`).
- **External API**: `rakumux-api`, HTTP on 127.0.0.1 over the daemon; found through `api.json`.
- **Remote access** (phone access in the TUI): the API's remote listener (Tailscale addresses, port
  7337) for the phone app, paired by scanning a QR code of the `rakumux://connect` link;
  opt-in with `rakumux remote on` or Settings → Phone access (`remote.json`), admitted by the
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
- **Layout**: Sidebar (list left, detail right; on narrow windows the list stacked above the
  detail in Rakumux mode) or Tabs (tab bar on top); `sessions.layout`. Phones have their own:
  the list, or a session under tabs.
- **Contracts**: the crate types and traits that define the architecture.

## Using Rakumux

| Command | Purpose |
|---|---|
| `rakumux` | TUI for the current folder |
| `rakumux new [--name N] [--cwd P] [--cmd C]... [--detach] [-- ARGV...]` | Create a session and attach |
| `rakumux ls [--json] [--dead] [--folder DIR]... \| --all` | Sessions of this folder, of the given folders, or of every folder (`--dead` includes dead ones; `--project` is a hidden alias of `--folder`; `--json` gives each session `folder` next to `scope`) |
| `rakumux attach <name\|id>` | Full-screen attach (prefix `d` detaches) |
| `rakumux rename [<name\|id>] <new name>` | Rename (defaults to the current session) |
| `rakumux pin <name\|id>` / `rakumux unpin <name\|id>` | Pin first in its folder / unpin |
| `rakumux kill <name\|id> [--tree]` / `rakumux rm <name\|id>` | Terminate / delete |
| `rakumux folders [--json]` / `rakumux folders add\|rm <dir>` | List folders / add one / forget one without sessions (`projects` is a hidden alias; `--json` prints `{"folders": [...], "projects": [...]}`, the same list twice) |
| `rakumux --new-server` | Replace the running daemon with this Rakumux's (in place: sessions keep running), or start one; then the TUI |
| `rakumux daemon start\|status\|stop\|restart\|logs` | Daemon management: `start` only when none runs; `restart` in place (an older daemon is stopped after asking); `stop` ends every session (not revived) |
| `rakumux session list\|get\|read\|wait\|report` | JSON commands for agents and scripts |
| `rakumux remote on [--port N] \| off \| pair \| status [--json] \| token [--rotate]` | Remote access for the phone app over Tailscale (also Settings → Phone access in the TUI): `on` saves it in `remote.json` (stays on), restarts the API and prints the URLs, the token, an `rakumux://connect` link and (on a terminal) its QR code; `pair` shows them again; `off` turns it off; `token --rotate` makes a new token (re-pair the app) |
| `rakumux-api [--port N] [--detach] [--remote-listen ADDR:PORT]...` | External HTTP API on 127.0.0.1, plus the remote listener when remote access is on (`api/README.md`) |
| `rakumux-mcp` | MCP server for agents, e.g. `claude mcp add --scope user rakumux -- rakumux-mcp` (`mcp/README.md`) |

TUI keys come in two modes. **Session mode**: everything goes to the program except `Ctrl-\`,
which switches to **Rakumux mode** (and `Option`/`Alt`+`↑/↓` when `cycle_keys_in_session` is
on). Rakumux mode (also where the TUI starts): `↑/↓`, `j/k`, `Option`/`Alt`+`↑/↓` select (the
session preview fades lightly in Rakumux mode and more under a dialog or for saved output,
while the list never fades; the folder filter, a folder
header and + New session keep the last session in view, one the filters show), `gg`/`G`,
`Ctrl-d`/`Ctrl-u`, `{`/`}` (previous/next folder) (a Ctrl or Alt that no shortcut uses is
ignored, so Ctrl-↑ from a phone's key bar moves and Alt-q quits), `Enter`/`i`/`l` go in (revive/restart; on a
folder header, a new session there), `Esc` (after cancelling a pending key or the filter) or
`Ctrl-\` back to the session you came from, `Tab` last session, `1`-`9` go into that session,
`n` new, `r` rename, `p` pin, `J`/`K` move the session down/up, `x` close, `d`/`dd` delete, `z` full
screen, `P` (or Enter on the folder filter) the Folders modal, `/` filter, `Ctrl-y`/`Ctrl-e`/
PgUp/PgDn scroll the preview, `\` send `Ctrl-\` to the session, `?` Help, `,`/`o` Settings, `R`
ask to restart only this UI (`y`/Enter confirms, `n`/Esc cancels; reload the launcher and latest
build; daemon and sessions stay up), `q`
quit. There is no header: the panes start at the top row, and the folder filter (`▤ All folders
▾`, a folder's name and branch, or `▤ 3 folders ▾`) is the list's first row (Sidebar) or sits at
the left of the tab bar (Tabs). The footer, always one row at the bottom (in zoom too), starts with the mode badge
(orange RAKUMUX / quiet SESSION), then messages and the daemon connection while it isn't up
(`⠋ connecting`, `✕ daemon unavailable`), and on the right the keys for Help and Settings
(`? help  , settings`; in session mode `Ctrl-\ rakumux  Ctrl-\ ? help  Ctrl-\ , settings`); when
the row is short the message is cut first, then hints go from the right (Settings, then Help).
Help and Settings' Keys tab list every key; the Keys tab runs them. A welcome tour
(`components/welcome.rs`, `AppMode::Welcome`) opens on startup until its "Don't show this on
startup" box (on every card, Space) is ticked; `t` in Help replays it. Drag the divider to resize the list.
When Tailscale runs on the machine and phone access was never set up (no `remote.json`), Rakumux
asks once, in Rakumux mode with no other dialog open (so after the tour), "Use Rakumux from your
phone?" (`AppMode::PhoneOffer`; `sessions.phone_offer_seen`): `y` turns it on and opens the pairing
dialog (`AppMode::Pairing`: the QR code of the `rakumux://connect` link, the URL and the token;
`x` turns phone access off), `n` leaves it off. Settings' General tab has the same switch (Phone
access: Enter turns it on or shows the pairing dialog).

Phones (under `layout::PHONE_BELOW` = 80 columns, whatever the layout setting; phone terminals
are about 40–60 columns in portrait) show one thing at a time: Rakumux mode is the session list
alone, full screen; a tap on a session (or `Enter`) goes into it, and session mode gives it the
screen under a row of tabs, one per session (no folder filter, no title row; a tap on a tab switches
session and stays in session mode). `Ctrl-\` (or a tap on the SESSION badge) goes back to the list.
Moving through the list on a phone doesn't resize anything: no session is on screen there.

Narrow windows (Sidebar from 80 to `layout::SINGLE_PANE_BELOW` = 100 columns): in Rakumux mode
the list is stacked on top, a one-row divider under it, and the selected session's detail (title
row and lightly faded preview) below, so switching sessions shows them. The list gets 40% of the rows above
the footer, at least `LIST_MIN_ROWS` (9: the folder filter and two sessions) and leaving the detail
`PREVIEW_MIN_ROWS` (4); dragging the divider sets that share (`sessions.stacked_list_percent`).
Below `STACK_MIN_ROWS` (14 rows above the footer, a 15-row screen) Rakumux mode shows the list
alone. Session mode fills everything above the footer. Taps arrive as clicks, so the footer is a
tap target too (`Hit::ModeBadge`, `Hit::FooterKey`, recorded by `components/footer.rs`): the
SESSION badge switches to Rakumux mode like `Ctrl-\`, the RAKUMUX badge goes back to the
session you came from like `Esc`, and each hint does what its keys do. Dialogs are at least
min(screen width − 2, 48) columns (`components/utils.rs::popup_width`), wrap their key lines and
grow to fit their text; the welcome tour tightens its spacing when it doesn't fit, and Settings'
General tab puts values under their labels when both don't fit.

**Terminal size across windows.** A session is one terminal with one size, so every window shows
the program drawn for that size. The size follows the window being used (like tmux's `window-size
latest`): each window's own layout comes from its own width, and when its user does something (a
key, a click or a tap, the window coming to the front: focus reporting, `EnableFocusChange`), the
session it shows takes that window's session-mode pane size (`App::claim_terminal_size`: `Resize`
as controller, `Fit` as observer). The daemon ignores an observer's `Fit` only while the controller
typed in the last 5 s (`CONTROLLER_TYPING`), so whoever is typing keeps their size. Nobody fits
back by itself, so two windows never resize a session back and forth.

The list (`app/session_list.rs`) holds every folder's sessions; with more than one folder shown
each has a header row (name, branch, live/total; selectable), and with one there are none. The
list never reorders by itself: activity doesn't move a session or a group. The launch folder's
group comes first, then the others by folder name, then path. In a folder, pinned sessions lead;
both they and the others follow the order the user set, and sessions without a place go last,
oldest first, so a new session appears just above "+ New session". Both layouts share it.
Dragging a session row (Sidebar) or tab (Tabs) moves it live (a blue bar marks the row while
held); `J`/`K` move it one place. Moves stay in the session's folder and its pinned or unpinned
part, and are saved as `sessions.session_order`. A click without movement still just selects.
The first load still selects the most recently used session.

New session (`n`, "+ New session", Enter on a folder header) creates immediately with a generated name,
selects the new session and enters session mode at its login-shell prompt, without a naming modal. The
session goes in the current folder (`App::current_folder`: the selected session's or header's
folder, retained on the folder filter and "+ New session" rows; a single-folder filter takes
precedence; without folder context, the launch folder). A session made in a folder the filter
hides adds that folder to the filter. Other folders, commands and startup commands are CLI-only:
`rakumux new --cwd DIR --cmd C -- ARGV`.

The Folders modal (`P`, Enter on or a click on the folder filter; `AppMode::Folders`,
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

A program can't change its parent shell's directory. When the TUI quits with a session selected it
can print `cd "<session cwd>"`; a shell function around `rakumux` evals that line. The wrapper is
documented at the top of `rakumux.sh`. The daemon is always spawned with its stdio detached, so a
wrapper that captures Rakumux's output never waits on the daemon.

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
