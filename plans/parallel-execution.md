# Parallel Execution of the Sessions Refactor

Status: active · Date: 2026-10-13 · Branch: `sessions-refactor` · Contracts commit: `71b0ff3`

The refactor (`plans/sessions-refactor.md`, `plans/agent-control.md`) is split into work packages
that run in parallel, each in its own git worktree, against **contracts fixed in code** at
`71b0ff3`. A central coordinator (the main session) owns the contracts, reviews each package,
merges them in order and runs end-to-end tests.

## Decisions taken after rev 6 of the refactor plan (binding for all packages)

1. **Single server, many clients.** Exactly one `orchestra-ptyd` per user owns every session's
   terminal and the session registry. Any number of TUIs (and CLI calls, agents) attach at the same
   time. Several TUIs may watch the same session; at most one is controller.
2. **The daemon owns the session API** (agent-control Option A): registry (SQLite), scope
   confinement, identity tokens, permissions, waits, lifecycle, audit. TUI and CLI are thin clients
   of `session_core::client::SessionApi`.
3. **No git anywhere in the product.** No worktrees, branches, git status, merge helpers, env-copy,
   or `orchestra-cli` (it was a worktree tool). A session is a folder + a terminal.
4. **Scope = the canonical folder Orchestra is launched from** (`ScopeRoot::resolve(cwd)`); no
   `git rev-parse`. A session's cwd must be inside its scope at creation.
5. **No shell services layer.** With git and tmux gone, `services.sh`, `server/`, `api/` only served
   AI naming; AI naming moves to Rust (`tui/src/naming`) and the shell layer is deleted. `jq`,
   `python3`, `git` and `tmux` stop being runtime dependencies.
6. **Agent control stage A ships in 0.6.0**: session identity env vars, `session.report/read/wait`
   for session callers. Session callers may only read within scope and mutate themselves (rename
   self, report self). Create/send/submit/kill on other sessions by agents is stage B (0.7.0).
7. **Help is a modal** (`?` from the list, `prefix ?` from a session) covering the whole new flow:
   sessions and scope, the master-detail screen, layouts, every shortcut (generated from the active
   key bindings, so a changed prefix shows correctly), observer vs controller, dead/exited sessions
   and revive, the single server with several TUIs, the `orchestra` CLI and agent commands, AI
   naming, and the license.
8. **`Option`/`Alt` + `↑`/`↓` cycles sessions** (previous/next in list order) from anywhere: list
   focus, session focus (intercepted, never forwarded to the program) and both layouts. Works on
   terminals that send Alt-modified arrows (`CSI 1;3A`/`CSI 1;3B`: iTerm2, Ghostty, kitty, WezTerm,
   Alacritty; Terminal.app needs "Use Option as Meta key"). `prefix >`/`prefix <` stay as the
   fallback. Configurable.
9. **Two session layouts**, chosen from a settings menu and saved in `~/.orchestra/config.json`
   (`sessions.layout`): **Sidebar** (default, session list on the left, detail on the right) and
   **Tabs** (a tab bar of sessions across the top; the selected tab's session detail fills the
   rest). Switching tabs (`Option`+`↑`/`↓`, `prefix >`/`<`, or clicking a tab) shows that session's
   detail view. The setting applies live.

## Contracts (do not change without the coordinator)

| Contract | File | Used by |
|---|---|---|
| Session model, wire protocol, errors, stream messages | `tui/crates/session-core/src/{model,api}.rs` | everyone |
| Frame codec | `tui/crates/session-core/src/frame.rs` | A1, B1 |
| Filesystem layout, registration file, binary lookup | `tui/crates/session-core/src/paths.rs` | A1, A2, A3, B1, C1, S1 |
| `SessionApi`, `AttachStream`, `EventStream` | `tui/crates/session-core/src/client.rs` (types) | B1 implements `Client`; C1 consumes |
| Terminal host boundary | `tui/crates/ptyd/src/host.rs` | A1 implements; A2 consumes |
| Control boundary | `tui/crates/ptyd/src/control_api.rs` | A2 implements; A1 (server) consumes |
| Support module signatures | `tui/crates/ptyd/src/{persist.rs,procinfo/mod.rs,detect.rs}` | A3 implements; A1, A2 consume |
| Terminal view API | `tui/src/terminal_view/mod.rs` | C2 implements; C1 consumes |
| CLI dispatch hook | `tui/src/cli/mod.rs` `dispatch` | B1 implements; `main.rs` calls it |
| AI naming API | `tui/src/naming/mod.rs` | S1 implements; C1 consumes |

If a package truly needs a contract change, make the **smallest additive** change (new optional
field, new method with a default, new variant) and list it under "Contract changes" in the final
report. Never rename or remove contract items.

## Rules for every package

- Work only in your worktree. Rust: `export PATH=$HOME/.cargo/bin:$PATH`; build and test from
  `tui/` (`cargo build --workspace`, `cargo test -p <crate>`). Keep `cargo fmt --all --check` clean
  and add no new warnings in files you own.
- **Edit only files you own** (table below). You may add new files inside owned directories.
- Tests never touch the real `~/.orchestra`: set `ORCHESTRA_HOME` to a temp dir.
- Don't install software system-wide, don't publish, don't push. `cargo install` into `~/.cargo` is
  allowed only where a package says so.
- Match the surrounding code: rustfmt, doc comments on public items like the contracts, no
  `unwrap()`/`expect()` on daemon request paths (tests may), explicit errors over silent fallbacks.
- Commit on your worktree branch in logical commits, messages ending with
  `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.
- Final report (≤40 lines): branch name, what's done, tests run and results, contract changes,
  known gaps / TODOs for the coordinator.

## Work packages

| ID | Package | Owns |
|---|---|---|
| A1 | Daemon runtime | `tui/crates/ptyd/src/{terminal/**,server.rs,registration.rs,log.rs,main.rs}`, `tui/crates/ptyd/tests/runtime_*.rs`, additive changes to `tui/crates/vt/**` |
| A2 | Daemon control layer | `tui/crates/ptyd/src/control/**`, `tui/crates/ptyd/tests/control_*.rs` |
| A3 | Daemon support modules | `tui/crates/ptyd/src/{persist.rs,procinfo/**,detect.rs}`, `tui/crates/ptyd/tests/fixtures/**` |
| B1 | Client library + CLI | `tui/crates/session-core/src/client.rs` (implementation; public API fixed), `tui/crates/session-core/Cargo.toml` deps, `tui/src/cli/**` |
| C1 | TUI master-detail | `tui/src/**` except `cli/`, `naming/`, `terminal_view/`; `tui/Cargo.toml` deps of the `tui` package |
| C2 | Terminal view widget | `tui/src/terminal_view/**` |
| C3 | Help modal + session layouts | `tui/src/help/**`, `tui/src/layout/**` (new modules) |
| R1 | Release & distribution | `install/**`, `orchestra.sh`, `build.sh`, `shell/gwr_*.sh`, `shell/gwr/**`, `shell/build/**`, `shell/orchestra-local.sh` |
| S1 | Shell layer removal + AI naming port | `services.sh`, `server/**`, `api/**`, `orchestra-cli.sh`, `shell/**` not owned by R1, `tests/**`, `fix-hanging-and-redeploy.sh`, `config.env`, `tui/src/naming/**` |
| P1 | Plans & docs | `plans/sessions-refactor.md`, `plans/agent-control.md`, `AGENTS.md`, `tui/src/app/AGENTS.md`, `shell/AGENTS.md`, `auth/AGENTS.md`, `tui/README.md` |

Merge order (coordinator): A3 → A2 → A1 → B1 → C2 → C3 → C1 → S1 → R1 → P1, then end-to-end
tests, hardening and docs finalisation.

---

### A1 — Daemon runtime

Implement `terminal::Host` (`host::TerminalHost`), the socket server, registration and logging.

- **Per terminal**: spawn with `portable-pty` (argv, cwd, complete env from `SpawnSpec`, size). One
  blocking reader thread → bounded channel → one actor thread owning `vt::Emulator::responding`
  (colors and scrollback from `HostConfig`) and an output replay ring (absolute offsets,
  `replay_bytes`); one writer thread for all PTY input (query replies, client input, startup input)
  so writes stay ordered. Coalesce output before fan-out (flush at 8 KiB or 1 ms).
- **Startup input**: write `SpawnSpec.startup_input` once the program has produced its first output
  or after 500 ms, whichever is first.
- **Fan-out**: attachments get `AttachEvent::Output{offset,bytes}` and `StreamEvent`s in order.
  At most one controller; `promote` demotes the previous one (`RoleChanged`). Controller channel
  applies backpressure; an observer whose backlog stays full gets `Lagged` then a fresh checkpoint
  (or is closed). Title/cwd(OSC 7)/bell/clipboard become `StreamEvent`s (clipboard to the
  controller only) and `HostEvent`s.
- **Resize** (controller): PTY and emulator resized in one actor command; everyone gets
  `Resized{checkpoint, offset}`.
- **`HostEvent::Output`**: throttle to ≤1/s per terminal plus one trailing event 500 ms after output
  stops.
- **Foreground process**: `MasterPty::process_group_leader()` + `procinfo::process_info` (A3; stub
  returns `None` until merged), refreshed at most every 2 s when output happens.
- **read**: `Visible` / `Recent` / `RecentUnwrapped` (join soft-wrapped rows via
  `vt100::Screen::row_wrapped`), `lines` limit, `Text` or `Ansi`. Add what you need to
  `crates/vt` (e.g. formatted history rows), additively, with tests.
- **submit**: bracketed-paste-aware text (from the emulator's live mode) then `\r`, one write.
  **send_keys**: validate all names first; encode with `vt::input::encode_key` and current modes.
- **terminate**: SIGHUP the process group, SIGKILL after 3 s. Child-wait thread sets
  `Exited{code}`; persist scrollback (`persist::ScrollbackStore`, A3) on exit and every
  `persist_interval` while output flows; drop exited terminals after `exited_retention` unless
  `forget` came first.
- **Server** (`server::main_entry`, `run`): `orchestra-ptyd serve` (foreground) and `--version`.
  Init log; `registration::claim`; build `Host` and `control::ControlService::open` (A2; stubbed
  until merge — use a test `Control` fake in your tests); accept loop, one thread per connection.
  Reject peers with another UID (`SO_PEERCRED` / `getpeereid`). First frame must be `Hello` →
  `Control::authenticate` → `Response::Hello` (capabilities list) or `Error` + close. Then:
  `Attach` → `Control::prepare_attach` → `TerminalHost::attach` → `Response::Attached`
  (checkpoint base64) → stream (output frames via `frame::write_output`, `StreamEvent` control
  frames; client input data frames → `AttachControl::input`, `StreamCommand`s → resize/promote/
  demote/detach). `Subscribe` → `Control::subscribe` → `Subscribed` then `EventEnvelope` frames.
  `DaemonStatus` (pid, version, uptime, sessions via `Control::session_count`, live terminals,
  clients, rss via `procinfo::self_rss_bytes`) and `DaemonShutdown` (user callers only:
  `Control::shutdown`, `TerminalHost::shutdown`, release claim, exit 0) are handled by the server;
  everything else goes to `Control::handle`. SIGTERM/SIGINT → same graceful shutdown.
- **Registration** (`registration::claim`): dirs 0700, `flock` on `service.lock`, stale
  socket/registration cleanup, bind socket 0600, write `service.json` atomically 0600, remove both
  on drop only if still ours.
- **Log**: file logger per `log.rs` docs, size-rotated (1 MiB, keep 1).
- **Done when**: integration tests (`tests/runtime_*.rs`, temp `ORCHESTRA_HOME`, real socket, real
  `bash --norc --noprofile`, a fake `Control`) cover: spawn → submit `echo hi` → read sees it;
  two attachments controller/observer, promotion and `RoleChanged`; resize broadcast; client
  disconnect leaves the terminal running; reattach checkpoint equals screen; terminate + exit code;
  second `claim` fails with `AlreadyRunning`; non-user peer rejected (unit-level); startup input
  delivered; query replies reach the program (`printf '\e[6n'` round trip via `read -s -d R`).

### A2 — Daemon control layer

Implement `control::ControlService` (`control_api::Control`) against `host::TerminalHost` (use an
in-memory fake host in tests).

- **Registry** at `Layout::registry_db()`: WAL, `busy_timeout`, `PRAGMA user_version = 2`. On first
  open `DROP TABLE IF EXISTS sessions` (the 0.5.x tmux registry; no migration). Tables:
  `sessions_v2(id PK, scope_root, name, cwd, launch_json, resume_json, created_by_json, parent_id,
  depth, agent_json, reported_json, exit_code, created_at, last_active_at, ended_at)` with an index
  on `(scope_root, last_active_at DESC)`, and `audit_log(id, ts, scope_root, caller, method,
  target_session_id, decision, detail)` pruned to 30 days on open. Live state (terminal id, status
  Live) is in memory only.
- **Startup reconcile**: no terminals survive a daemon restart → every stored session is `Dead`
  (or `Exited` if it has an exit code).
- **Identity**: `Auth::User` must match `ControlConfig::user_token`. Per-session tokens are issued
  at spawn/revive and kept **in memory only** (they die with the daemon, like the processes).
  Protocol mismatch → `ProtocolMismatch`.
- **Scope**: user callers pass `scope` (required where the contract says); session callers always
  get their own session's scope. Anything outside → `OutOfScope`.
- **Create**: cwd defaults to the scope root and must canonicalise inside it; default name is a
  short readable generated name (e.g. two words), unique within the scope; argv = `LaunchSpec.argv`
  or the login shell (`SHELL` from the request env, else the daemon's, else `/bin/sh`) with `-l`.
  Child env = request env minus `TMUX`, `TMUX_PANE`, `ORCHESTRA_SESSION_*`, `CLAUDECODE`,
  `CLAUDE_CODE_SESSION_ID`, `CLAUDE_CODE_CHILD_SESSION`, `CLAUDE_CODE_MESSAGING_TOKEN`,
  `CODEX_THREAD_ID`, `OMPCODE`, plus `ORCHESTRA_SESSION_ID`, `ORCHESTRA_SESSION_TOKEN`,
  `ORCHESTRA_SOCKET`, `ORCHESTRA_BIN` (if known), `TERM=xterm-256color`, `COLORTERM=truecolor`.
  `startup_cmds` become `startup_input` (`cmd\r` each).
- **Ops**: every `Request` the server forwards (see `control_api.rs`): list (ordered by
  `last_active_at` desc, `include_dead`), get, create, rename (unique-in-scope check), kill
  (`tree` = descendants), delete (terminate, remove record, `ScrollbackStore::remove`), revive (Dead
  or Exited → spawn with same id; with `resume` and a stored `ResumeSpec`, append the shell-quoted
  resume argv + `\r` to startup input), report (self only; stores `Reported`, agent info, resume
  argv as `ResumeSource::ReportedByAgent`), read (live → host; otherwise saved scrollback via
  `ScrollbackStore::load`, converted to text by feeding a passive `vt::Emulator`), wait (blocks up
  to `timeout_ms`, wakes on state changes; `Output` = substring on the visible screen), send,
  submit, touch (updates `last_active_at`, turns `Done` into `Idle`), ping.
- **Permissions (stage A)**: user callers may do everything. Session callers: list/get/read/wait
  within scope, report/touch/rename on **self**; everything else (create, send, submit, kill,
  delete, revive, attach, rename others) → `PermissionDenied` with a message saying agent control
  of other sessions arrives in a later release. Audit every mutating op and every denial.
- **Lifecycle**: on `HostEvent::Output`/`Exited` run `detect::detect` (A3; stub returns no agent)
  on the visible screen + foreground process; combine with self-reports (a fresh report wins for 30
  s, then detection); `Working → Idle` becomes `Done` until the session is touched. Publish
  `SessionEvent::Lifecycle`.
- **Events**: `subscribe` filters by scope/ids/kinds; emit `Created`, `Updated` (any view change),
  `Removed`, `Exited`, `Lifecycle`, `Reported`, `Activity` (≤1/s/session).
- **Done when**: unit/integration tests (`tests/control_*.rs` or module tests) cover auth (user,
  session, bad token, protocol mismatch), scope confinement, create env/argv/cwd validation, list
  ordering, rename uniqueness, kill tree, delete, revive with resume, report, wait (match, timeout,
  exit), lifecycle `Done`/`Idle`, events filtering, the session-caller permission matrix, legacy
  table drop, audit entries.

### A3 — Daemon support modules

- **persist.rs**: implement `ScrollbackStore` (atomic temp+rename writes, 0600 files, dirs 0700,
  missing = `Ok(None)`), with tests.
- **procinfo**: Linux via `/proc/<pid>/{comm,cmdline,cwd}` and `/proc/self/statm`; macOS via
  `libc` `proc_pidpath`/`proc_pidinfo` (`PROC_PIDVNODEPATHINFO` for cwd) and `sysctl`
  `KERN_PROCARGS2` for the command line, behind `#[cfg(target_os = "macos")]`. Try
  `cargo check --target x86_64-apple-darwin -p orchestra-ptyd` (`rustup target add` is allowed); if
  the C toolchain makes that impossible, say so in the report. Name detection must see through
  wrappers: `node …/claude`, `bun …/opencode`, etc. report the agent name via `cmdline`.
- **detect.rs**: `detect` identifies the agent (`claude`, `codex`, `opencode`, `pi`, `gemini`,
  `aider`, `cursor-agent`, …) from the foreground process name/cmdline (fall back to title) and
  classifies `Working` / `Blocked` / `Idle` / `Unknown` from evidence on the **visible screen
  bottom** (status lines, interrupt hints, permission prompts). Study Herdr's manifests
  (`~/repos/herdr/src/detect/`) for which markers matter, but write your own code and data (Herdr is
  Apache-2.0; we don't copy code). Capture real idle screens as fixtures where possible by running
  agents through `tui/crates/vt/examples/spike.rs` with a temp `HOME` (no prompts that cost API
  calls); write working/blocked fixtures by hand from the evidence. Plain shells → `agent: None,
  lifecycle: None`.
- **Done when**: unit tests for all three modules pass on Linux; detection fixture tests per agent.

### B1 — Client library + CLI

- **`Client`** (session-core): `connect(config)`: resolve auth (`AuthMode::Auto`: session env vars
  → session auth and `ORCHESTRA_SOCKET` if set; else registration file → user auth and its socket).
  If connecting fails and `config.spawn`: find the daemon (`paths::find_ptyd_binary`), spawn
  `orchestra-ptyd serve` with **stdin/stdout/stderr on `/dev/null`** and `setsid()` in `pre_exec`
  (add `libc` to session-core), then wait up to 2 s for the registration file and socket. Hello
  handshake; keep one request connection behind a mutex, reconnect once on a broken pipe. Map every
  `SessionApi` method to its `Request`/`Response`; unexpected responses → `Internal`, `Error` →
  `ApiError`. `attach`/`subscribe` open a new connection and run reader/writer threads that feed
  `AttachStream::pair` / `EventStream::from_receiver`.
- **CLI** (`tui/src/cli`, `dispatch`): global options before the subcommand (`--ptyd-path`,
  `--services-path` ignored, `--debug`). Scope for user callers = `ScopeRoot::resolve(cwd)`;
  `<name|id>` resolves within scope (exact id, else unique name).
  - `orchestra new [--name N] [--cwd P] [--cmd C]... [--detach] [-- ARGV...]` → create (env snapshot
    = current process env, size = terminal size or 80×24), then attach unless `--detach`.
  - `orchestra ls [--json] [--all]` (`--all` includes dead).
  - `orchestra attach <name|id>`: full-screen raw client. Enter alternate screen + raw mode, attach
    as controller at the terminal's size, write the checkpoint and then output bytes **straight to
    stdout** (the user's terminal does the emulation), forward stdin bytes as input. Prefix `Ctrl-b`
    (0x02) then: `d` detach, `>`/`<` next/previous session in scope by recency (reattach), `n` new
    session in cwd, `Ctrl-b` literal. On SIGWINCH send `Resize`. Restore the terminal on exit/error.
  - `orchestra rename [<name|id>] <new name>` (defaults to `$ORCHESTRA_SESSION_ID`),
    `orchestra kill <name|id> [--tree]`, `orchestra rm <name|id>` (delete).
  - `orchestra daemon status|stop|restart|logs` (`logs` tails `Layout::log_file()`).
  - `orchestra session list|get|read|wait|report` — JSON output for agents and scripts (stage A).
    `report` flags: `--state`, `--text`, `--summary`, `--file` (repeatable), `--agent-kind`,
    `--agent-session-id`, `--resume -- ARGV...`.
  - Exit codes: 0 ok, 1 error (message on stderr), 2 usage.
- **Done when**: tests with an in-process fake daemon (a test server using `frame` over a temp
  socket) cover connect/hello/auth modes, request mapping, attach stream I/O, event stream, and
  that the spawn helper detaches stdio (spawning `sleep 5` through it from inside `$(…)` returns at
  once). CLI argument parsing tests. Real end-to-end runs happen after A1/A2 merge (coordinator).

### C1 — TUI master-detail

Rebuild the TUI around sessions per `sessions-refactor.md` §6–§8, with the decisions above (no git:
no Git tab, no merge menu, no worktree options). Program against
`Arc<dyn session_core::client::SessionApi>`; build an in-memory fake for tests.

- **Layout**: header (scope path), left session list, right detail with tabs Terminal and Info,
  footer. Single-pane below ~100 cols. Status glyphs, lifecycle, relative time, cwd relative to
  scope, `reported.text` when present.
- **Behaviour**: subscribe to scope events and keep the list live (multiple TUIs may run at once;
  never assume you're the only client). Selected session attaches as observer; Enter promotes to
  controller (pane size); prefix keys per §7 (`Ctrl-b` default, configurable); zoom; nesting guard
  (`ORCHESTRA_SESSION_ID` set → that session can't be attached); dead session → saved scrollback
  (via `read`) with "Enter to revive"; exited → "Enter to restart".
- **Dialogs**: new session (name, directory inside scope, optional command, startup commands),
  rename, delete confirmation, daemon-restart consent on protocol mismatch, help (rewrite: overview,
  keys, sessions concept, license), API keys / AI provider (keep), upgrade (keep).
- **Event loop**: `tokio::select!` over crossterm `EventStream`, the active attach stream and the
  event subscription (pump the blocking channels from a thread into tokio channels), timers; redraw
  at most ~60 fps, immediately on input.
- **AI naming**: when a session with an auto-generated name first goes `Idle`/`Done`, gather
  `NamingInput` (history file `Layout::history_file(id)` + `read` visible screen) and call
  `naming::suggest_session_name` (S1); rename if it returns a name.
- **Delete** everything tied to worktrees, git, tmux and the shell services: `api.rs`
  (`ServicesApi`), worktree list, git status panel, merge/deletion/creation pipelines, env-copy and
  repo config copy locations, `session_name.rs`, `session_registry.rs`, `session_preview.rs`,
  `context.rs`, the tree `navigation.rs`, `--new-tmux`/`--cmd`, the `env-copy` binary target and
  `src/bin/env_copy.rs`, `walkdir`/`strip-ansi-escapes` if unused. Keep config, theme, updater,
  upgrade dialog, auth (flag off, compiling).
- `main.rs`: accept `--ptyd-path`; build `ClientConfig` (`client_name: "tui"`, spawn on); connect in
  the background after first paint; show a clear error state if the daemon can't start.
- **Done when**: `cargo test -p tui` passes with tests for list ordering/filtering, key routing
  (prefix, promote/demote, zoom, nesting guard), dialogs, and ratatui `TestBackend` renders at
  80×24 and 160×48 for empty / live / dead states; no references to tmux, git, worktrees or
  `services.sh` remain in `tui/src` (outside `auth`).

### C2 — Terminal view widget

Implement `tui/src/terminal_view` per its docs: passive emulator mirroring, `reset` from checkpoint,
`feed`, local scrolling of retained rows, rendering cells into a ratatui `Buffer` (fg/bg for
default/indexed/RGB colours, bold, italic, underline, inverse, dim; wide characters spanning two
cells; cropping/padding when the pane and terminal sizes differ; a subtle marker when scrolled
back), cursor position when focused and visible, `key_to_bytes` (map crossterm `KeyEvent` to
`vt::input::encode_key`, including Ctrl/Alt/Shift, F-keys, BackTab), `mouse_to_bytes` (pane-relative
coordinates, `vt::input::encode_mouse`), `paste_to_bytes`. **Done when**: unit tests render known
byte streams into a `Buffer` and assert cells/styles, wide chars, cropping, cursor; key and mouse
mapping tests.

### C3 — Help modal + session layouts

Decisions 7–9. Two new self-contained modules that C1 wires in (C1 owns `main.rs`, `app/**`,
`ui.rs`, `components/**`, config and key routing; C3 must not edit those).

- **`tui/src/layout/mod.rs`**:
  - `SessionLayout` enum (`Sidebar` default, `Tabs`), serde `snake_case`, with `label()` and
    `next()`.
  - `LayoutAreas { list: Option<Rect>, tabs: Option<Rect>, detail: Rect }` and
    `fn split(area: Rect, layout: SessionLayout) -> LayoutAreas`: Sidebar = list column (max of
    28–34 cols or 25%) + detail; Tabs = one-row tab bar + detail; below ~100 cols Sidebar degrades to
    the single-pane mode from `sessions-refactor.md` §6 (caller passes which pane is active).
  - Tab bar widget: `TabItem { name, glyph, glyph_style, selected, attention }` and
    `fn render_tabs(area, buf, items: &[TabItem], selected: usize) -> Vec<(usize, Rect)>` returning
    hit areas for mouse clicks; numbering (`1`–`9`), ellipsis for long names, scrolling so the
    selected tab is always visible, overflow markers `‹ ›`.
  - `fn cycle(len: usize, current: usize, delta: i32) -> usize` (wraps).
- **`tui/src/help/mod.rs`**: the help modal.
  - `HelpState { tab, scroll }`, tabs: Overview, Keys, Sessions, Layouts, Agents & CLI, License.
  - `HelpContext { prefix: String, cycle_keys: String, layout: SessionLayout, version: String,
    in_session: bool }` so shortcut text reflects the user's configured keys and current layout.
  - `fn handle_key(&mut self, key: &KeyEvent) -> HelpAction` (`Close` on `Esc`/`q`/`?`; `Tab`/
    `←`/`→` switch tabs; `↑`/`↓`/`PgUp`/`PgDn`/`Home`/`End` scroll; number keys jump to tabs).
  - `fn render(f: &mut Frame, area: Rect, state: &HelpState, ctx: &HelpContext)`: centred modal
    (~80% × 80%, full screen below 80×24), tab strip, scrollable body, footer hint.
  - Content covers everything in decision 7, written plainly; the License tab keeps the existing
    proprietary text from `tui/src/components/help.rs` (copy the text, not the code). No git, tmux,
    worktree or remote-mode content.
- **Done when**: unit tests for `split` at several sizes, tab overflow/ellipsis/hit areas, `cycle`
  wrap-around, help key handling, and `TestBackend` renders of the modal at 80×24 and 160×48 that
  contain the configured prefix; rustfmt clean, no warnings in owned files.

C1's matching responsibilities: config `sessions.layout` + `sessions.cycle_keys`, a settings menu
entry to switch layout (applies live, saved), `Option`/`Alt`+`↑`/`↓` handling in every focus state
(before forwarding keys to a session), opening the help modal from `?` / `prefix ?`, and calling
`layout::split` / `layout::render_tabs` / `help::render`.

### R1 — Release & distribution

Per `sessions-refactor.md` §13 with the decisions above:
- `orchestra.sh`: drop `gwr_check_git_repo` and `gwr_check_services_script`; stop passing
  `--services-path`; pass `--ptyd-path` (resolved like the `tui` binary; `GW_PTYD_BIN` override)
  and export `ORCHESTRA_BIN` (path of the launcher). Keep the update check.
- `install/scripts/build-release.sh`: build and ship `orchestra-ptyd` (workspace member) next to
  `tui`; stop shipping `env-copy`, `orchestra-cli.sh`, `services.sh`, `server/`, `api/`; include
  `LICENSE` and `THIRD_PARTY_NOTICES`; build tarballs with `COPYFILE_DISABLE=1` and without macOS
  metadata. Generate `THIRD_PARTY_NOTICES` from `Cargo.lock` (`cargo install cargo-about` into
  `~/.cargo` is allowed; commit the template/config).
- npm (`install/npm`): `"license": "SEE LICENSE IN LICENSE"`, add `LICENSE` (same text as
  `install/scripts/setup-public-repo.sh` writes), `files` updated, drop the `orchestra-cli` bin and
  `bin/orchestra-cli.js`, copy `orchestra-ptyd` in `install.js`, description/keywords without tmux
  or git.
- Homebrew: one `install/scripts/release-homebrew.sh` replacing `update-homebrew-sha.sh`,
  `update-homebrew-formula.sh`, `update-homebrew-tap.sh` (render from a template with version + three
  SHA-256s; `brew audit --strict`, local-tap install and `brew test` steps documented but **not run
  by you**); formula: `license :cannot_represent`, install `orchestra-ptyd` into `libexec`, wrapper
  `exec orchestra.sh "$@"` for every argument, no `orchestra-cli`, no `env-copy`, no
  `depends_on` git/tmux/jq, `test do` checks `orchestra --version` and `orchestra-ptyd --version`.
  `full-release-automation.sh` calls the new script.
- `install/AGENTS.md`, `install/README.md`, `install/INSTALL.md`: match.
- **Done when**: `bash -n` on every changed script, `ruby -c` on the formula (if Ruby is present), a
  dry-run of the packaging steps that don't need macOS, and a local `npm pack` of `install/npm` that
  contains the expected files.

### S1 — Shell layer removal + AI naming port

- Port AI naming from `server/services/ai.sh` (`services_ai_generate_name_from_base64`,
  `ai_choose_provider`) and the config loaders in `server/services/json.sh` to
  `tui/src/naming/mod.rs`: same providers, models, prompt and output clean-up as the shell version,
  API keys from `crate::app::Config` with `ANTHROPIC_API_KEY` / `OPENAI_API_KEY` env fallbacks,
  using `reqwest` (already a dependency). Unit-test prompt building and output clean-up with no
  network.
- Delete `services.sh`, `server/`, `api/`, `orchestra-cli.sh`, `shell/cli_*.sh`, `shell/git/**`,
  `shell/env_copy*.sh`, `shell/tmux/**`, `shell/fix_hanging_*.sh` and `fix-hanging-and-redeploy.sh`
  if only used by removed parts, `config.env` if unused, and tests under `tests/` that exercise
  tmux, git worktrees, env-copy or the services layer. Keep anything `orchestra.sh` / R1's files
  still source (check before deleting; ask via the report if unsure).
- Rewrite `shell/orchestra-command-hook.sh`: history lines go to `~/.orchestra/history/<id>.log`
  (`$ORCHESTRA_HOME` respected) when `ORCHESTRA_SESSION_ID` is set; no tmux; `orchestra_rename_session`
  calls `"${ORCHESTRA_BIN:-orchestra}" rename "$*"`.
- Rewrite `tests/manual-test-instructions.md` for the session model.
- **Done when**: `cargo test -p tui naming` passes; `bash -n` on remaining scripts; `git grep -n
  tmux -- ':!plans'` only finds intentional mentions (docs about not supporting tmux).

### P1 — Plans & docs

- `plans/sessions-refactor.md` → rev 7: fold in the six decisions above, agent-control stage A in
  0.6.0, `plans/agent-control.md` §9 changes that still apply (drop git/worktree items), the Phase
  table re-mapped to these packages, and a pointer to this file. Remove git-related sections (Git
  tab, merge helpers, worktree creation, env-copy, `orchestra-cli`) rather than leaving them.
- `plans/agent-control.md` → rev 2: decisions taken (Option A; stage A permissions as above); no
  worktree in `session.create`.
- `AGENTS.md`: rewrite for the new architecture (single daemon + clients, crates, data layout,
  protocol, call stacks, glossary), remove the stray `aslkdaslks`; same for
  `tui/src/app/AGENTS.md`, `shell/AGENTS.md`, `auth/AGENTS.md` where they describe removed parts;
  `tui/README.md`.
- **Done when**: no doc describes tmux, worktrees, merge helpers, remote mode or the services layer
  as current behaviour.

---

## Integration log (coordinator)

| Merged | Package | Notes |
|---|---|---|
| this commit | TUI | Icons setting (`sessions.icons`, Settings ▸ General): Plain `▤`/`⎇`, Emoji 📁/🌿, Nerd Font; passed explicitly (no global), and `format::truncate`/`truncate_middle` now measure columns so 2-column emoji keep the list aligned (checked for every style at 80 and 160 columns). User request: Orchestra mode shows no keys: the which-key strip is gone and the footer shows only the mode and messages (session mode keeps `Ctrl-\ orchestra`). Markdown docs had `Ctrl-\\` (two backslashes) in code spans; fixed. 594 workspace tests. |
| `e2d3e39` | TUI | A click on the list from a session enters Orchestra mode (Esc goes back). |
| `552b599` | TUI | Two modes and one key (user decision after a survey of Claude Code 2.1, OpenCode 2.0, Pi and lazygit defaults: `Ctrl-b` is Claude Code's and OpenCode's "background" key, Alt+↑/↓ are taken by OpenCode, Claude Code and Pi). Session mode forwards everything but the Orchestra key `Ctrl-\` (`sessions.orchestra_key`; a customised old `prefix_key` still counts); Orchestra mode is the list focus with arrows plus Vim keys, Enter/`i` in, Esc (cancel first) or `Ctrl-\` back to `came_from`, Tab last session, `1`-`9`, `dd`, `z`, `\`; which-key strip after a pause; footer mode badge; Option+↑/↓ inside sessions only with `cycle_keys_in_session` (Settings, General). `orchestra attach` prefix moved to `Ctrl-\`. Fixed: own Promote/Demote answers were taken for another client's takeover on a fast round trip; the attach test's output race. 592 workspace tests. |
| `96eb17e` | TUI | Project icon `▤` (plain symbol) in the list and the tab bar. |
| `0a27f55` | TUI | The daemon announced its features but no client could read them: `SessionApi::daemon_capabilities` (default `None`, additive). An older daemon (no `session.git`/`projects`/`session.pin`, e.g. one started before git context) gets a restart offer (`y` only); verified against a daemon built from `b25d073^`. Subtext names the main worktree too (`⎇ main · worktree repo`). 588 tests. |
| `1df349e` | TUI | Help (`?`, `prefix ?`) and Settings (`prefix l`, `o`) split; Settings' Keys tab is a table of `KeyAction`s run with Enter; every prefix command works in the list; `prefix l` no longer goes back to the list (`prefix Tab`/`Esc` do). |
| `40f6563` | TUI | Project item (selectable top row, padded band) and project switcher modal (filter, switch, Tab ticks, add or create a typed folder after `y`, Ctrl-x forgets); compact three-row session cells; selected text white, no backgrounds. |
| `cc9337e` | Merges | Tokyo Night merged as `f809801` (agent `5110ba3`): Tokyo Night assumed unless the terminal clearly can't (no `COLORTERM` needed); 256-colour fallback navy, not grey; `panel` = background `#1a1b26`; `Palette::base()` painted on the root and dialogs; embedded-terminal default colours themed; OSC 10/11 answer Tokyo Night. Settings modal merged (agent `0c32111`) without conflicts. Also "+ New session" moved to the bottom of the list (`115a198`). 584 workspace tests. |
| `0c32111` | Settings modal | User request: the footer was too crowded. Help modal and the `o` settings menu merged into one **Settings** modal (`AppMode::Settings`, `help::SettingsState`/`SettingsTab`): tabs General (the old menu's rows; its API key, provider and model dialogs now return to the modal), Keys, Overview, Sessions, Layouts, Agents & CLI, License; `o` opens General, `?` and `prefix ?` open Keys; `o` also closes. The Keys tab now carries everything the footer listed (adds `Ctrl-r`, filter keys, `1-9`, Home/End). Footer is one row: messages plus `? settings` (`prefix ? settings` in a session, `⏎ keep  Esc clear` while filtering); panes gain the row. 577 workspace tests; checked headless at 140×40, 120×32 and 80×24. |
| next commit | OpenCode identity | Why OpenCode refused creates: OpenCode 2.x (the user's `opencode2`, 2.0.28) runs MCP servers in a shared background service, so calls carry no session env, only `_meta["ai.opencode/sessionID"]`; the MCP refused changes. Fix (`agent-mcp.md` N6, now in 0.6.0): `SessionBind` op + `Auth::Delegated` (capability `auth.delegated`, memory-only bindings, newest wins, cleared on exit); `orchestra session bind`; API agent headers; MCP acts for the tagged conversation; OpenCode 2.x TUI plugin + `orchestra-mcp opencode-plugin [--install]` (writes `<config>/plugins/orchestra-session/tui.js`; 2.x skips single-file entries and migrates `tui.json` only once, so no config editing). 567 workspace tests (Node runs the plugin against a fake API). Verified with real OpenCode 2.0.28 + shared service in an isolated home: plugin bound the conversation; the real MCP with that tag created sessions as the Orchestra session, even with a stale env identity. |
| next commit | Agent creates | User decision: sessions may create sessions in their own project (`policy::authorize` allows `Create`; naming another project or a `cwd` outside it → `out_of_scope`). `control::Limits` now enforced on those creates only: 4 running children per session, depth 2, 8 running agent-created per project, 10 creates per session per minute → `limit_exceeded` with `details {limit, max}` (audited as denied). API maps `limit_exceeded` to 429 (`openapi.yaml`, README). MCP instructions and `create_session` description updated. `orchestra new` inside a session needs `--detach`. 556 workspace tests; verified with the real MCP inside a session (4 creates, then `max_children_per_parent`; another project → `out_of_scope`). |
| merge of A (`6287358`) | A (API/MCP) | `api/` → `orchestra-api` (hyper 0.14 HTTP/1.1 on 127.0.0.1; `api.json` + `api.lock` per home; bearer token, loopback Host, no Origin/Sec-Fetch-Site; identity headers → session-auth daemon client; `openapi.yaml` kept in sync by tests; falls back per project on older daemons). `mcp/` → `orchestra-mcp` (`rmcp =3.5.1`, stdio; six tools; nonce-delimited untrusted output; starts `orchestra-api --detach`; OpenCode shared-service guard). Build, tarball, npm and Homebrew ship both. 552 workspace tests. Open: agents get `permission_denied` on create (stage A policy); OpenCode 2.x doesn't pass the session identity; Codex install line unverified. |
| merge of T (`255ed04`) | T (projects) | Project selector pill on the list title row (or the tab bar's left end), `P` drop-down with checkboxes, add (`a`, Tab completion) and forget (`x`); selection saved as `sessions.selected_projects` (launch folder always shown, not saved); grouped list with project headers (launch folder first, then activity; Tabs by oldest session); `p` pins (gold `★`), pinned first per group; subtext `⎇ branch · worktree · program · state · report`, no cwd; Project field in the new-session dialog; header and help updated; falls back to the launch folder on `unsupported`. 492 workspace tests; checked against the real daemon with four projects incl. a linked worktree. |
| merge of D (`85d0703`) | D (projects) | Registry schema 3 (`projects` table backfilled from session scopes, `pinned` column); `SessionList`/`Subscribe` unions for user callers (session callers naming another scope → `out_of_scope`); `ProjectList/Add/Remove` (add needs an absolute path; remove refuses while sessions exist); `SessionPin` (user or self); capabilities `projects`, `session.pin`, `session.git`; git context read from `.git` files with a per-session cache refreshed on cwd change and every 2 s. CLI: `projects [add\|rm]`, `pin`/`unpin`, `ls --project/--all` (dead sessions moved to `--dead`). 461 workspace tests. Open: no project events (clients re-query); reftable repos give no branch. |
| next commit | K (projects) | Contract for `plans/projects-api-mcp.md`: `GitInfo`, `ProjectView`, `SessionView.{pinned, git}`, `SessionList.scopes`, `SubscribeRequest.scopes`, `SessionPin`, `ProjectList/Add/Remove`, `Response::{Projects, Project}`, `SessionApi::{list_in, projects, add_project, remove_project, pin}` (default `unsupported`). Daemon answers `unsupported` until D lands. Packages D, T, A fan out from here. |
| next commit | UI polish | From the 2026-10-13 review: observers fit an uncontrolled terminal to their pane (`StreamCommand::Fit`); Tokyo Night list with vertically centred cells; header shows the folder name with its parent middle-truncated; list rows omit the scope root and login shells. **Info tab removed** (user decision): the detail pane is the terminal only, the controlling/observing mark moved to the detail border, and the status no longer says "live" (the CLI `ls` says "running"). Plans that mention the Info tab (`agent-control.md`, `agent-mcp.md`, §C1 here) are superseded on that point. |
| `557d9ec`, `4c5fae2` | contract | v1 additions from `agent-mcp.md` §6.1: `SessionView.{auto_named, description, notes, labels, private}`, `Hello.max_blocking_ms` (enforced on waits), `ErrorCode::{ApprovalTimeout, ApprovalUnavailable}`, capability and read/wait-not-seen docs. TUI AI naming now follows the daemon's `auto_named`. |
| (load, manual) | — | 20 sessions × 200k lines (~400 MB of output): daemon RSS ~120 MB (~6 MB/session, matches the 2,000-row budget), CPU settles to 0.3% idle, `ls` stays instant, saved scrollback 3.2 MB total. |
| merge of C1 | C1 | TUI rebuilt as a client of the daemon: Sidebar/Tabs layouts, live list from the event stream, observer/controller panes, prefix keys, zoom, nesting guard, Alt+↑/↓ cycling, help modal, settings menu (`o`) with live layout switch, new-session/rename/delete/daemon-restart dialogs, AI naming, reconnect with backoff. Removed `api.rs`, worktree/git/tmux/merge/env-copy code and the `env-copy`, `walkdir`, `strip-ansi-escapes`, `rusqlite` dependencies. 416 workspace tests. |
| (e2e, TUI) | — | Real TUI via `orchestra.sh` + real daemon, headless 140×40: create (named and auto-named), type into a controlled pane (PTY resized to the pane), Alt+↓ cycling across three sessions, settings → Tabs (saved to config), help modal, quit and relaunch with sessions intact, two TUIs on one daemon (typing in one mirrors live in the other), vim in a pane mirrored to the observer, zoom, daemon stop → both TUIs reconnect and show dead sessions, Enter revives. |
| `8a9642e`, `42e57a5`, next | fixes | Daemon exits when its registration file disappears (stray test daemons); e2e tests kill their daemon on failure; dead-code allowances removed, `publish = false`; racy procinfo test fixed; Homebrew/npm install detection uses the resolved binary path. |
| merge of A1 + `e0c45cd` | A1 | Terminal host (per-PTY actor, replay ring, fan-out, backpressure, persistence, 2×2 size clamp, no saves after `forget`), socket server (hello, attach/event streams, `unsupported` for unknown ops, peer-UID check, SIGTERM), registration, logging; daemon end-to-end tests enabled. 366 workspace tests pass. |
| (e2e, manual) | — | Real CLI + daemon in an isolated `ORCHESTRA_HOME`: spawn-on-demand (`ls` 29 ms), `new`, `ls`, `attach` (typing, `Ctrl-b d`), session identity inside sessions (`is_self`), stage-A denial of `kill` from an agent, `daemon stop` → dead sessions with saved scrollback, `session read` of dead sessions. |
| next commit | CLI fix | `orchestra attach` revives dead/exited sessions instead of refusing. |
| merge of A3 | A3 | `ScrollbackStore` (atomic, 0600/0700, id validation), procinfo for Linux (`/proc`) and macOS (libproc, `KERN_PROCARGS2`; compile-checked for both macOS targets outside the repo, not run on a Mac), agent detection for claude/codex/opencode/pi/gemini/qwen/aider/cursor-agent plus name-only agents, 51 screen fixtures (14 captured live). |
| merge of R1 | R1 | `orchestra.sh` without git/services checks (`--ptyd-path`, `ORCHESTRA_BIN`); tarballs and npm ship `orchestra-ptyd`, `LICENSE`, `THIRD_PARTY_NOTICES` (cargo-about), no macOS metadata; one `release-homebrew.sh` + template (`license :cannot_represent`, wrapper execs `orchestra.sh`); npm launcher no longer routes to `orchestra-cli`; workspace builds. |
| `e361f34` | B1 | Client (auth modes, detached spawn, dedicated connections for wait/attach/subscribe, reconnect) and CLI (`new`, `ls`, `attach`, `rename`, `kill`, `rm`, `daemon …`, `session …` JSON). 30 + 35 tests. Gaps: attach polls for resize (no signal dep); dropped `EventStream` closes on next event; TUI `--help` should list subcommands (C1). |
| `f05a201` | A2 fix | Revive with resume replaces only the startup/launch command that starts the same program; user touches no longer audited. |
| merge of A2 | A2 | Control layer: registry (with agent-mcp v1 columns), auth, stage-A policy, waits, lifecycle (Done/Idle), events, audit; 60 tests with a fake host. Delete race reported to A1 (don't persist forgotten terminals). |
| merge of C3 | C3 | Help modal (six tabs, shortcuts from configured keys) and Sidebar/Tabs layouts with tab bar and cycling; 43 tests. `split` takes a `PaneFocus` third argument (handles zoom). C1 sent the final API. |
| merge of P1 | P1 | `plans/sessions-refactor.md` rev 7, `plans/agent-control.md` rev 2, `AGENTS.md` and module docs rewritten. Found and sent to R1: npm launcher routes to `orchestra-cli`; build scripts don't build the workspace. Open questions in sessions-refactor §17. |
| merge of C2 | C2 | Terminal view widget (29 tests). Adds `with_scrollback`, `history_len`, `scroll_to_bottom`, `reset_to_text`, `mouse_bytes`. Found: `vt100` panics below 2×2 — A1 asked to clamp sizes in the daemon. Remove the module's `allow(dead_code)` once C1 merges. |
| `795cfcd` | S1 | AI naming in Rust; shell services layer, `orchestra-cli`, git/tmux shell modules and their tests deleted. Follow-up `e9032f8`: naming moved from the retired `claude-3-5-haiku-latest` to `claude-haiku-5-5` (low effort, `max_tokens` 1024, text block selected by type). |

Open integration items:
- The command-history hook (`shell/orchestra-command-hook.sh`) is no longer sourced automatically
  (tmux used to do it). Decide between an `orchestra integration install shell` command that adds a
  guarded `source` line to the user's shell rc, or documenting the line in help/install docs.
- 0.6.0 release notes are in `install/release-notes/0.6.0.md`; `create-github-release.sh` uses
  `install/release-notes/<version>.md` when it exists (commit subjects otherwise).
- Needs macOS/real brew (user): full `build-release.sh`, `release-homebrew.sh` audit/install/test
  from a temporary tap, fresh install + upgrade from 0.5.81, `orchestra.sh` under bash 3.2.
