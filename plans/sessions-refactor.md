# Sessions Refactor: One Session Daemon, Thin Clients, Master-Detail Layout

Status: in progress (rev 7) · Date: 2026-10-13 · Branch: `sessions-refactor` · Baseline: `origin/main` @

> **2026-10-13:** projects (several folders in one list), pinning, read-only git branch/worktree display and an external HTTP API + MCP were added by `plans/projects-api-mcp.md`, which overrides §4 (single-folder scope) and the "no git" rule where they conflict.
`62ca08a` · Contracts: `71b0ff3`

Companion plans:
- `plans/parallel-execution.md`: work packages, file ownership, rules and per-package "done when" for
  building this plan in parallel.
- `plans/agent-control.md` (rev 2): how coding agents inside sessions use the session API.
- `plans/agent-mcp.md` (in progress, written separately): the MCP adapter.

Rev 7 folds in decisions 1–9 taken after rev 6 (recorded in `parallel-execution.md`), agent-control
stage A, and the contracts fixed in code at `71b0ff3`. **The contract files are the source of truth**:
`tui/crates/session-core/src/*.rs`, `tui/crates/ptyd/src/{host.rs, control_api.rs}`,
`tui/crates/vt/src/lib.rs`. Where this plan and the code disagree, fix the plan.

## Decisions at a glance

| Topic | Decision | Section |
|---|---|---|
| Core concept | **Session** = a folder + a terminal: a durable record plus, while it runs, a live terminal. Sessions carry no git metadata. | §3 |
| Architecture | **One server, many clients.** Exactly one `rakumux-ptyd` per user owns every session's terminal and the session registry. Any number of TUIs, CLI calls and agents connect at the same time. Several TUIs may watch the same session; at most one is controller. | §5 |
| Session API owner | **The daemon** (agent-control Option A): registry (SQLite), scope confinement, identity tokens, permissions, waits, lifecycle, audit. TUI and CLI are thin clients of `session_core::client::SessionApi`. | §3.1, §5.14, §11 |
| Scope | The **canonical folder Rakumux is launched from** (`ScopeRoot::resolve(cwd)`), with no `git rev-parse`. A session's cwd must be inside its scope at creation. The list shows only the current scope. | §4 |
| Git | **None in the product.** No worktrees, branches, git status, merge helpers, env-copy or `rakumux-cli`. Users run git inside a session like any other program. | §10 |
| Shell layer | **Deleted.** `services.sh`, `server/` and `api/` only served git, tmux and AI naming; AI naming moves to Rust (`tui/src/naming`). `git`, `tmux`, `jq` and `python3` stop being runtime dependencies. | §10 |
| Runtime | `rakumux-ptyd` (`portable-pty` + `vt100` through `crates/vt`), detached, outlives every client. | §5 |
| Emulator | `vt100` 0.16.2. The phase 2 spike passed (2026-10-13); libghostty-vt is a follow-up. | §5.6 |
| tmux | **Removed outright**, with no tmux dependency. Running Rakumux inside tmux is unsupported. | §10 |
| Older tmux sessions | **Not supported.** 0.6.0 does not list, attach to, migrate, adopt or clean up sessions created by 0.5.x. They stay in tmux until the user kills them. | §2, §10 |
| Remote / cloud mode | **Deleted** (done in `728de9a`). | §10 |
| Default command | The user's **login shell**. | §5.7, §8 |
| Key model | As built: **two modes and one key**. Session mode passes every key to the program except the **Rakumux key, `Ctrl-\`** (configurable, `sessions.rakumux_key`), chosen because Claude Code, OpenCode, Pi and editors leave it alone (`Ctrl-b` is Claude Code's and OpenCode's "background" key, and Alt+↑/↓ are taken by OpenCode, Claude Code and Pi). Rakumux mode (the list focus) takes single keys: arrows and Vim keys, Enter/`i` in, Esc or `Ctrl-\` back to the session you came from, Tab last session, letters for actions. Originally planned: a `Ctrl-b` prefix. | §7 |
| Session cycling | **`Option`/`Alt` + `↑`/`↓`** selects the previous/next session in Rakumux mode, and inside sessions only when `sessions.cycle_keys_in_session` is on (Settings, General). Configurable (`sessions.cycle_keys`). | §7 |
| Layouts | **Sidebar** (default: list left, detail right) and **Tabs** (session tab bar on top, the selected session's detail below), chosen in the settings menu and saved as `sessions.layout`. Applies live. | §6 |
| Help | A **modal** (`?` from the list, `prefix ?` from a session) covering the whole flow, with shortcuts generated from the configured keys. | §6.6 |
| Survive reboot | Scrollback and metadata on disk; **revive** starts a new terminal for the same session and runs its resume command. | §5.8–5.9 |
| Agent control | **Stage A in 0.6.0**: session identity env vars; session callers may list/get/read/wait within their scope and report/touch/rename **themselves**. Agents creating or driving other sessions is stage B (0.7.0). | §5.14, `agent-control.md` |
| MCP | A thin adapter over the same session API (agent-control stage C); designed in `plans/agent-mcp.md` (in progress). | §18 |
| Release | One breaking release, **0.6.0**, on npm and Homebrew. | §2, §14 |
| Homebrew | **Kept and fixed**: single release script, wrapper matches npm, daemon binary added, no `depends_on`. | §13 |
| License | **Closed source** (the existing proprietary license). npm and Homebrew metadata are corrected to match; third-party notices ship with the binaries. Open-sourcing may be revisited later. | §13.4 |
| Deferred | **Auth** and **usage tracking** are untouched in this round and come next. | §18 |
| Execution | Parallel work packages in separate worktrees against contracts fixed in code; a coordinator merges in order and runs end-to-end tests. | §14 |

## Goals

1. Make the **session** Rakumux's primary concept: a persistent, resumable terminal in a folder, not
   tied to a worktree or branch.
2. Run every session in **one per-user daemon** that outlives all clients, so any number of TUIs and
   CLI calls can show and drive the same sessions at once.
3. Replace the worktree → sessions tree with a **master-detail screen**: the scope's sessions and the
   live, interactive session side by side (Sidebar) or as tabs (Tabs).
4. On launch the user can create a session, browse the scope's sessions, or go straight back into one
   (the most recent is selected by default).
5. Let coding agents inside sessions **see** their scope and **report** their own state
   (agent-control stage A).
6. **Shrink the product**: remove tmux, remote/cloud mode, every git feature, `rakumux-cli`,
   env-copy and the shell services layer.

## Non-goals

- **Any support for older tmux-based Rakumux sessions.** No listing, attaching, migration,
  adoption or cleanup, and no code that detects them. They keep running in tmux until the user kills
  them (`tmux ls`, `tmux kill-session`); the release notes say so.
- **Any git integration.** Rakumux never runs git. Users who want a worktree create it with git
  (inside the scope folder) and start a session there.
- Auth changes and usage tracking (deferred to the next round; §18).
- Showing sessions from other folders (future work).
- Agents creating, prompting or killing other sessions (agent-control stage B, 0.7.0).
- Windows support (Linux + macOS only).
- Live daemon upgrade with PTY handoff (designed for, not built; §5.10).
- Splits or multiple terminals per session (one terminal per session in v1; terminals are keyed by
  session id, so this can be added later).

---

## 1. Baseline and progress

### 1.1 What 0.5.x does

| Area | 0.5.x | Problem |
|---|---|---|
| Product focus | Git worktrees first: worktree tree, git status panel, merge/rebase/squash helpers, env-copy into new worktrees, `rakumux-cli` worktree commands | Git workflows belong to git and to the agents; they dominate the code and the UI. |
| Session identity | tmux name `rakumux__<slug>__<hash>__<date>__<time>__<name>` (`server/session/tmux_api.sh` `tmux_ensure_session`) | Branch slug baked into identity; renames rewrite the tmux name and need ~450 lines of prefix parsing in `tui/src/app/session_name.rs`. |
| Registry | SQLite `sessions` table (`worktree_path`, `branch`, `slug` all `NOT NULL`) written by Rust (`tui/src/app/session_registry.rs`) **and** shell (`_tmux_registry_*` via embedded `python3`) | A session can't exist without a worktree; two writers in two languages. |
| Backend | Rust TUI → `services.sh` → `server/services/dispatch.sh` → `api/git.sh`, `api/tmux.sh`; JSON built with `jq` | One process per call, four runtime dependencies (`git`, `tmux`, `jq`, `python3`), logic split across two languages. |
| Layout | `DisplayItem::{Worktree, Session, NewSession}` tree + preview + git status (`components/worktree_list.rs`, `app/navigation.rs`) | Sessions are children of worktrees. |
| Opening a session | `ServicesApi::attach_session` blocks while `tmux attach` owns the terminal, then forces a redraw | Session content isn't visible inside the app. |
| Preview | `capture-pane` polling (`app/session_preview.rs`) | Read-only, not live. |
| In-session UX | tmux status tabs + prefix bindings (`n` new, `l` list, `q` close, `r` rename, `<`/`>` cycle, `?`/`h` help) written into the user's global tmux key table | Mutates the user's tmux config. |
| Event loop | `main.rs` blocks on `crossterm::event::poll(timeout)` | Can't react immediately to streamed output. |
| `app.rs` | 5,456 lines at baseline; 4,043 after the cloud removal | To be split along the new seams (§12). |

### 1.2 Progress on `sessions-refactor`

| Commit | Step |
|---|---|
| `1e9bfa7` | Plan (revs 1–6); repo Homebrew formula synced with the tap (0.5.81). |
| `728de9a` | Remote/cloud mode removed (phase 1). |
| `0495398` | `plans/agent-control.md` rev 1. |
| `687b6d1` | `crates/vt` with query responder; emulator spike (phase 2): go with `vt100`. |
| `71b0ff3` | Contracts: `crates/session-core`, `crates/ptyd` skeleton, TUI module contracts (`terminal_view`, `cli`, `naming`). |
| `cf4bdf1`, `208b395` | `plans/parallel-execution.md` (decisions 1–9, work packages). |

The remaining work is in §14.

---

## 2. Usage data and what it means for this plan

There are no analytics, so this comes from public npm and GitHub numbers (collected 2026-10-13).

**npm `rakumux`** (the main install channel; binaries are bundled in the package):

| Window | Downloads |
|---|---|
| Last 12 months (2025-10-12 → 2026-10-11) | 6,036 |
| Last 30 days | 110 |
| Last 7 days | 7 (spread across 5 versions, including 0.5.19 and 0.5.42) |

- 66 versions published; latest is **0.5.81 (2026-07-16)**, with no publishes since. The repo's
  `install/npm/package.json` still says 0.5.80, so the next version must be above 0.5.81.
- **56%** of the year's downloads fall on a release day or the day after. Another **25%** come from
  non-release burst days, many of about 68 downloads, roughly one fetch per published version. That
  pattern fits a mirror or scanner re-syncing the whole package, not people.
- Upgrades are user-initiated (`tui/src/updater.rs` `perform_upgrade` runs `npm`/`brew` when the user
  accepts), so release-day spikes are not an auto-update fleet either.
- On quiet days the baseline is a **median of 2 downloads/day over the year and 1/day over the last
  30 days**. Downloads of very old versions also point to automated traffic.

**GitHub `humanunsupervised/rakumux`** (public release repo): 29 stars, 2 forks, one issue ever
(#1, "Brew install fails", opened 2026-03-08 by an external user, still open). Release-asset downloads
(Homebrew and manual installs): **180 in total across 93 releases**, 3 for v0.5.81.

**Interpretation.** Downloads can't be turned into a user count, but every signal points to a
small active user base, plausibly single digits to low tens and including the maintainer. At least
one external person has tried it (issue #1).

**Consequences for the plan:**

1. A **blanket replace is low-risk**: no migration tooling, no legacy adapter, no compatibility shims,
   no support for older tmux sessions, and no replacement for the removed git features.
2. Ship as **one breaking release, 0.6.0** (pre-1.0 semver allows breaking changes in a minor),
   published only when the whole refactor is done. Intermediate work merges to `sessions-refactor`
   and `main` but is not published.
3. **npm is the main channel**; Homebrew is kept and its formula is fixed (§13).
4. Release notes must cover:
   - older tmux sessions are not supported (they keep running in tmux and are no longer shown);
   - `tmux`, `git` and `jq` are no longer needed;
   - every git feature (worktree list, git status, merge helpers, env-copy) and `rakumux-cli` are
     gone; run git in a session instead;
   - sessions are listed per launch folder;
   - the command history hook changed; re-source `shell/rakumux-command-hook.sh`;
   - remote mode is gone;
   - leftover files that can be deleted by hand: `.rakumux/cloud.json`, `ssh.json`,
     `worktree.json` and `options.json` in repos, any DigitalOcean token in the OS keyring, and
     `~/.rakumux/history/` files named after tmux sessions.

---

## 3. Domain model

Defined in `tui/crates/session-core/src/{model.rs, ids.rs}` and shared by the daemon, the TUI and
the CLI. Summary (see the files for serde attributes and docs):

```rust
pub struct SessionId(String);   // ULID: 48-bit ms timestamp + 80 random bits, Crockford base32; sorts by creation
pub struct TerminalId(pub u64); // daemon-assigned PTY id; not stable across daemon restarts
pub struct Token(String);       // 256-bit hex secret (user token, session tokens); constant-time compare
pub struct ScopeRoot(PathBuf);  // canonical launch folder (§4)

pub enum SessionStatus { Live, Exited, Dead, Unknown }           // does the terminal exist?
pub enum Lifecycle { Working, Blocked, Done, Idle, Unknown, None } // what the program (agent) is doing; None = no agent
pub enum Creator { User, Session { id: SessionId } }
pub struct AgentInfo { kind: String, agent_session_id: Option<String>, agent_session_path: Option<String> }
pub struct Reported { state: Option<Lifecycle>, text: Option<String>, result: Option<ReportResult>, at: i64 }
pub struct ReportResult { summary: String, files: Vec<String> }
pub struct LaunchSpec { argv: Option<Vec<String>>, startup_cmds: Vec<String> } // argv None → login shell
pub struct ResumeSpec { argv: Vec<String>, source: ResumeSource }
pub enum ResumeSource { StartupCommand, KnownAgent, ReportedByAgent, Unknown }

pub struct SessionView {        // what every client sees
    id, name, scope, is_self, cwd, status, exit_code, lifecycle, agent, reported,
    created_by, parent_id, depth, children, launch, resume, created_at, last_active_at,
    terminal: Option<TerminalView>, // pid, foreground_process, title, cols, rows, controlled, alternate_screen
}
```

- **Status**: `Live` (terminal running), `Exited` (process ended; final screen and scrollback kept),
  `Dead` (record exists, terminal gone after a reboot or daemon restart).
- **Lifecycle**: `Done` means "finished a turn and nobody has looked yet" (§5.13). Plain shells are
  `None`.
- Unknown enum values deserialize to `Unknown`, so an older client tolerates a newer daemon.
- `parent_id`, `depth`, `created_by`, `agent` and `reported` exist from 0.6.0 so agent-control
  stage B needs no breaking change.

### 3.1 Layers and boundaries

```
  TUI (tui/src/app, components,      CLI (tui/src/cli)       agents and scripts in sessions
       terminal_view, layout, help)                          (rakumux session …)
                 │                          │                          │
                 └──────── session_core::client::SessionApi ───────────┘
                                  │  Client: Unix socket, frames (§5.4)
  ┌─────────────────────────── rakumux-ptyd ────────────────────────────────┐
  │ server    accept loop, peer-UID check, hello, dispatch, attach/event      │
  │           streams                                                         │
  │    │  control_api::Control                                                │
  │ control   registry, scope, identity, permissions, waits, lifecycle,       │
  │           events, audit                                                   │
  │    │  host::TerminalHost                                                  │
  │ terminal  PTYs, processes, responding emulators, attach fan-out           │
  │ persist · procinfo · detect   (support modules)                           │
  └───────────────────────────────────────────────────────────────────────────┘
```

| Boundary | File | Between | Methods |
|---|---|---|---|
| `SessionApi` | `session-core/src/client.rs` | every client ↔ daemon | `caller`, `list`, `get`, `create`, `rename`, `kill`, `delete`, `revive`, `report`, `read`, `wait`, `send`, `submit`, `touch`, `status`, `shutdown`, `attach` (→ `AttachStream`), `subscribe` (→ `EventStream`) |
| `Control` | `ptyd/src/control_api.rs` | server ↔ control layer | `authenticate`, `handle`, `prepare_attach`, `subscribe`, `session_count`, `shutdown` |
| `TerminalHost` | `ptyd/src/host.rs` | control layer ↔ terminals | `spawn(SpawnSpec)`, `list`, `info`, `read`, `write`, `submit`, `send_keys`, `terminate`, `forget`, `attach` (→ `HostAttachment`), `subscribe` (→ `HostEvent`), `shutdown` |

- `SessionApi` is blocking and callable from any thread. `Client` implements it over the socket;
  tests use in-memory fakes. Streams deliver through channels (`try_recv`, `recv_timeout`) so a UI
  can poll them without blocking; `AttachStream::pair` and `EventStream::from_receiver` build fakes.
- The terminal host knows nothing about scopes, callers or policy; the control layer resolves argv,
  environment and startup input before calling `spawn`.
- **The UI only talks to `SessionApi`.** It never opens the socket itself, never reads the registry
  and never touches a PTY.

---

## 4. Scope: the launch folder

- **Scope root** = `ScopeRoot::resolve(cwd)`: the canonical folder (symlinks resolved) the TUI or CLI
  was started from. No git lookup of any kind.
- The list, `rakumux ls` and name resolution (`attach`, `rename`, `kill`, `rm`, `session …`) all
  work within the current scope. `<name|id>` resolves to an exact id first, then a unique name.
- Launching from a subfolder is a different scope with its own list (§17).
- A session's scope is fixed at creation. Its cwd must canonicalise inside the scope at creation
  (default: the scope root). Afterwards the program may `cd` anywhere; the session stays in its list.
- **User callers** (TUI, CLI outside sessions) pass `scope` explicitly on list, create and subscribe.
  **Session callers** (processes inside a session) always get their own session's scope, whatever
  they pass. Anything outside it is `out_of_scope`.
- The daemon is per user and serves every scope. A cross-folder view later is another filter over
  the same registry.

---

## 5. `rakumux-ptyd`: the session daemon

Modelled on `opencode-pty` (MIT, `~/repos/opencode-pty`, see `SPEC.md`) for threading and protocol,
and on Herdr (`~/repos/herdr`, Apache-2.0) for the detached lifecycle, persistence and agent resume.
The key difference from opencode-pty: **no single owner**; the daemon outlives every client.

### One server, many clients (architectural requirement)

- **Exactly one daemon per user.** `registration::claim` takes an exclusive `flock` on
  `service.lock`; a second instance fails with `ClaimError::AlreadyRunning`.
- **Clients are equal.** A TUI is one client among many. Closing every TUI leaves all sessions
  running. Two TUIs on the same scope show the same list, kept live by event subscriptions, and no
  client may assume it is the only one.
- **Attachments**: any number per terminal; at most one **controller** (owns input and terminal
  size), the rest are **observers**. `promote` demotes the previous controller, which receives
  `role_changed` and keeps watching.
- The CLI and agents connect for one request or one stream and disconnect.
- **All authority lives in the daemon**: registry, permissions, waits and audit, so policy is the
  same whichever client asks.

### 5.1 Packaging

`tui/` is a Cargo workspace (`members = [".", "crates/vt", "crates/session-core", "crates/ptyd"]`):

| Package | Lib / bin | Contents | Notable deps |
|---|---|---|---|
| `rakumux-session-core` | lib `session_core` | model, wire protocol, frame codec, ids, filesystem layout, client (`SessionApi`, `Client`, streams). No tokio, no ratatui. | serde, serde_json, base64, getrandom |
| `rakumux-vt` | lib `vt` | `Emulator` (vt100 wrapper, query responder, checkpoints, history rows), `input` encoders; `examples/{spike.rs, throughput.rs}` | vt100 0.16.2 |
| `rakumux-ptyd` | lib `ptyd`, bin `rakumux-ptyd` | server, control, terminal, persist, procinfo, detect, registration, log | portable-pty 0.9, rusqlite (bundled), libc |
| `tui` | bin `tui` (installed as `rakumux`) | TUI, CLI subcommands, AI naming, help, layouts, updater, auth (off) | ratatui 0.25, crossterm 0.27 (`event-stream`), tokio, reqwest, clap |

- A release ships two binaries, `tui` and `rakumux-ptyd`, plus `rakumux.sh` and the launcher
  helpers in `shell/`. The `env-copy` binary is gone.
- Daemon lookup (`paths::find_ptyd_binary`): explicit `--ptyd-path` → `$RAKUMUX_PTYD_BIN` → next to
  the current executable → `PATH`.
- `rakumux.sh` resolves the daemon like the `tui` binary (override `GW_PTYD_BIN`), passes
  `--ptyd-path`, and exports `RAKUMUX_BIN` (the launcher path) so the daemon can inject it into
  sessions.

### 5.2 Process lifecycle

Filesystem layout (`session_core::paths::Layout`):

```text
~/.rakumux/                    (or $RAKUMUX_HOME)
  rakumux.sqlite3              session registry (daemon only)
  sessions/<id>/scrollback.ansi  saved scrollback for dead sessions and revive
  sessions/<id>/meta.json
  history/<id>.log               command history (shell hook)
  logs/ptyd.log
$XDG_RUNTIME_DIR/rakumux/      (or ~/.rakumux/run; $RAKUMUX_HOME/run when set), mode 0700
  service.json                   registration: instance_id, pid, protocol, version, socket, token (0600)
  service.lock                   flock held by the running daemon
/tmp/rakumux-<uid>/ptyd-<hash>.sock   short path for the sun_path limit; hash of the run dir
```

- **Spawn on demand.** When connecting fails and `ClientConfig.spawn` is set (user clients), the
  client finds the daemon binary and starts `rakumux-ptyd serve` with **stdin, stdout and stderr
  on `/dev/null`** and `setsid()`, then waits up to 2 s for the registration file and socket. Stdio
  must not be inherited: the documented shell wrapper runs Rakumux inside `$(…)`, and an inherited
  stdout would hang the wrapper until the daemon exits. Session callers never spawn a daemon.
- **First paint stays fast.** The TUI draws its frame immediately and connects in the background;
  the list fills in when the connection is up. If the daemon can't start, the TUI shows a clear
  error state.
- **Startup** (`registration::claim`): create dirs 0700, take the `flock`, remove a stale
  socket/registration, bind the socket 0600, write `service.json` atomically 0600. Both files are
  removed on exit only if they are still ours. Then `ControlService::open` opens and migrates the
  registry and reconciles it: no terminal survives a daemon start, so every stored session is
  `Dead` (or `Exited` if it has an exit code).
- **Stays alive with zero clients.** Exits on `daemon_shutdown` (user callers only) or SIGTERM/SIGINT:
  the control layer flushes (scrollback, registry), the host terminates every terminal, the claim is
  released, exit 0.
- Daemon death closes PTY masters, so children get SIGHUP and exit. Recovery is via revive (§5.9).

### 5.3 Threading model

Per terminal, following opencode-pty's invariants:

- One blocking reader thread → bounded channel → one **actor** thread that owns the responding
  `vt::Emulator` and the output replay ring.
- One writer thread for all PTY input (query replies, client input, startup input), so writes stay
  ordered.
- Bounded channels; one terminal can't block another or the service.
- Output coalesced before fan-out (flush at 8 KiB or 1 ms).
- PTY resize + emulator resize is a single actor command.
- Child exit and PTY EOF are separate events; a child-wait thread records `Exited { code }`.
- Each actor runs under `catch_unwind`; a panic marks that terminal `Failed`, not the daemon.

Server: one thread per connection; `Control::handle` may block (e.g. `session_wait`) on that
connection's thread. Control layer: one thread consumes `HostEvent`s (detection, lifecycle, events,
persistence).

### 5.4 Protocol (v1)

`session_core::PROTOCOL_VERSION = 1`; types in `session-core/src/api.rs`, bytes in `frame.rs`.

**Framing**: `[u32 BE length][u8 kind][payload]`, where length counts the kind byte and the payload;
max frame 8 MiB.
- Kind 0, **control**: UTF-8 JSON (`Request`, `Response`, `StreamCommand`, `StreamEvent`,
  `EventEnvelope`).
- Kind 1, **data**: raw terminal bytes. Daemon → client output frames start with a `u64 BE` output
  offset; client → daemon input frames are just the bytes.

**Connection flow**:
1. The daemon rejects peers with another UID (`SO_PEERCRED` / `getpeereid`).
2. The client sends `hello { protocol, client, version, auth }`. `auth` is `{ kind: "user", token }`
   (token from `service.json`) or `{ kind: "session", id, token }` (from `RAKUMUX_SESSION_ID` and
   `RAKUMUX_SESSION_TOKEN`). The daemon answers `hello { protocol, version, capabilities, caller }`
   or an `error` (`unauthenticated`, `protocol_mismatch`) and closes.
3. Request/response: one control frame each way, strictly in turn.
4. `attach` and `subscribe` turn the connection into a stream for its remaining lifetime; clients
   open another connection for other requests.

**Requests** (`Request`, tagged by `op`):

| op | Fields | Response |
|---|---|---|
| `hello` | protocol, client, version, auth | `hello` |
| `ping` | | `pong` |
| `session_list` | scope (required for user callers), include_dead | `sessions`, ordered by `last_active_at` desc |
| `session_get` | id | `session` |
| `session_create` | scope, name?, cwd? (inside scope; default scope root), argv? (default login shell), startup_cmds, env, cols, rows | `session` |
| `session_rename` | id, name (unique in scope) | `session` |
| `session_kill` | id, tree (also descendants) | `killed { killed: [ids] }`; records stay (`Exited`) |
| `session_delete` | id | `ok`; terminates, removes the record and saved scrollback |
| `session_revive` | id, env, cols, rows, resume (default true) | `session` |
| `session_report` | state?, text?, result?, agent?, resume? (**self only**) | `ok` |
| `session_read` | id, source (`visible` / `recent` / `recent_unwrapped`), lines?, format (`text` / `ansi`) | `read { text, rows, cols, cursor, truncated, untrusted: true }` |
| `session_wait` | id, until (`settled` / `idle` / `blocked` / `working` / `exited` / `reported` / `output`), pattern? (substring), timeout_ms | `wait { matched, status, lifecycle, reported?, reason }` |
| `session_send` | id, keys (named; all validated before any byte is written), text? | `ok` |
| `session_submit` | id, text (bracketed-paste aware, then Enter, one write) | `ok` |
| `session_touch` | id | `ok` (updates `last_active_at`; `done` → `idle`) |
| `attach` | id, role (`controller` / `observer`), cols, rows | `attached { session, role, cols, rows, checkpoint, offset }`, then the stream |
| `subscribe` | scope, session_ids, kinds | `subscribed`, then `EventEnvelope` frames |
| `daemon_status` | | `status { pid, version, protocol, uptime_secs, sessions, live_terminals, clients, rss_bytes? }` |
| `daemon_shutdown` | (user callers only) | `ok`, then the daemon exits |

**Errors**: `error { error: { code, message, details? } }`. Codes: `unauthenticated`,
`protocol_mismatch`, `not_found`, `out_of_scope`, `permission_denied`, `session_not_live`, `timeout`,
`invalid_argument`, `daemon_unavailable`, `unsupported`, `internal`; reserved for agent-control
stage B: `approval_required`, `approval_denied`, `limit_exceeded`, `session_blocked`,
`session_in_use`, `prompt_stalled`. Unknown codes deserialize to `unknown`.

**Attach stream**:
- Client → daemon: input as data frames (controller only); `StreamCommand` control frames
  `resize { cols, rows }` (controller only), `promote { cols, rows }`, `demote`, `detach`.
- Daemon → client: output data frames tagged with offsets; `StreamEvent` control frames
  `resized { cols, rows, checkpoint, offset }`, `role_changed { role }`, `title`, `cwd`, `bell`,
  `clipboard { selection, data }` (controller only), `exited { code }` (the stream ends),
  `lagged { offset }`.

**Event stream**: `EventEnvelope { scope, session_id, at (Unix ms), event }`, where `event` is
`created { session }`, `updated { session }` (any view change: name, status, cwd, title,
controller, …), `removed`, `exited { code }`, `lifecycle { lifecycle }`, `reported { reported }` or
`activity` (new output, at most once per second per session).

**Compatibility rules** (adapted from Herdr's AGENTS.md): new fields are optional; enums have an
`Unknown` fallback; never change an existing op's meaning (add a new op instead); `capabilities` in
the hello lets clients disable actions an older daemon lacks; fixture tests freeze v1 (phase 9).

### 5.5 Output, checkpoint, replay, memory

- The daemon's emulator holds the authoritative state. `attach` returns a **checkpoint**
  (`vt::Emulator::checkpoint`: vt100's `Screen::state_formatted()` plus the alternate-screen switch,
  so contents, cursor, input modes and attributes) and the output offset it corresponds to; output
  frames continue from there. A fresh client emulator processes the checkpoint to reach the identical
  screen. Clients never replay megabytes.
- Each terminal keeps a raw output ring (`HostConfig.replay_bytes`, default 256 KiB) with absolute
  offsets. The controller's channel applies backpressure; an observer whose backlog stays full gets
  `lagged` and then a fresh checkpoint (or is closed).
- **Resize** (controller only): PTY and emulator resize in one actor command; every attachment gets
  `resized` with a new checkpoint.
- **Memory budget**: a `vt100` cell is 32 bytes (static assert in its `cell.rs`), so 10,000 rows at
  120 cols ≈ 38 MB per session. Default 2,000 rows in memory (`HostConfig.scrollback_rows`, ≈ 7.7 MB
  at 120 cols); older history lives only in the saved scrollback. Client emulators keep no
  scrollback (`TerminalView::new`); history beyond the screen is fetched with
  `session_read { source: recent, format: ansi }`.
- Observers render at the controller's size, cropped or padded to their pane.

### 5.6 Emulator and terminal queries

`crates/vt` wraps `vt100` 0.16.2 (pure Rust, no extra toolchain):
- `Emulator::responding(rows, cols, scrollback, colors)` for the daemon and `Emulator::passive(…)`
  for clients; `feed(bytes) → Output { replies, events }`; `resize`, `size`, `screen`, `modes`,
  `text`, `checkpoint`, `history_len`, `history_rows`.
- `Modes { application_cursor, application_keypad, bracketed_paste, alternate_screen, hide_cursor,
  mouse_mode, mouse_encoding }`; `Event::{Title, Cwd, Clipboard, Bell}`.
- `vt::input::{encode_key, encode_paste, encode_mouse}`: mode-aware encoders (application cursor
  and keypad, bracketed paste, SGR/X10 mouse).

`vt100` does **not** answer terminal queries, so the responding emulator includes a **query
responder** built on its callbacks (`unhandled_csi`, `unhandled_osc`):

- Answered: DSR status (`CSI 5 n`), cursor position (`CSI 6 n`, `CSI ? 6 n`), primary and secondary
  device attributes, XTVERSION, text-area size (`CSI 18 t`), DECRQM for the private modes it tracks,
  and OSC 10/11 default colours (`HostConfig.colors`).
- Not answered (programs fall back): `CSI 14 t` (pixel size), OSC 4 palette, XTGETTCAP and the kitty
  keyboard query `CSI ? u`.
- **Only the daemon answers.** Client emulators are passive, or every attached client would inject
  duplicate replies.
- OSC 7 (cwd) and OSC 0/2 (title) update terminal metadata and are published as events. OSC 52
  (clipboard) goes to the current controller only, which writes it to the outer terminal.

**Phase 2 spike result (2026-10-13): go with `vt100`.** Each program ran in a PTY through
`crates/vt` (`examples/spike.rs`) and was compared with tmux rendering the same scripted session
(`TERM=xterm-256color` in both):

| Program | Result | Queries it sent (all answered or safely ignored) |
|---|---|---|
| bash (colour, CJK wide chars) | identical | none |
| vim (edit, arrows, `:set number`, `:vsplit`, resize 24×80 → 30×100) | identical | `CSI 6n`, `CSI >c`, `CSI ?12$p`, `DCS zz`, `OSC 10/11 ?` |
| nvim `--clean` (jump, `:vsplit`) | identical | DECRQM 2026/2027/2031/2048/69, `CSI ?u`, DA1, XTGETTCAP, `OSC 11 ?` |
| less (page, search) | identical | none |
| htop | identical layout (only live CPU/uptime values differ) | none |
| Claude Code (startup screen) | identical | `CSI >0q`, `CSI ?2026$p`, `CSI ?u`, DA1, `OSC 7501 ?` |
| OpenCode (startup screen) | identical except its random placeholder hint | `CSI 14t`, `CSI 6n`, DECRQM ×7, `CSI ?u`, DA1, XTGETTCAP, `OSC 4/10/11 ?` |

Parse throughput (release, 120×40, `examples/throughput.rs`): ~200 MB/s for plain text, ~140 MB/s
for colour-heavy output. libghostty-vt (used by Herdr and opencode-pty) stays a phase 12 evaluation
for higher fidelity (kitty keyboard protocol, graphics).

### 5.7 Environment

A daemon's own environment goes stale (`PATH`, `SSH_AUTH_SOCK`, `DISPLAY`, …), so `session_create`
and `session_revive` carry an **environment snapshot from the requesting client**. The child gets:

- the request env **minus** `TMUX`, `TMUX_PANE`, any `RAKUMUX_SESSION_*`, and the agent-internal
  variables `CLAUDECODE`, `CLAUDE_CODE_SESSION_ID`, `CLAUDE_CODE_CHILD_SESSION`,
  `CLAUDE_CODE_MESSAGING_TOKEN`, `CODEX_THREAD_ID`, `OMPCODE` (Herdr strips the same list,
  `herdr/src/pane.rs:187-196`);
- **plus** `RAKUMUX_SESSION_ID`, `RAKUMUX_SESSION_TOKEN`, `RAKUMUX_SOCKET`, `RAKUMUX_BIN`
  (when known), `TERM=xterm-256color`, `COLORTERM=truecolor` (names in `session_core::env`).

argv = `LaunchSpec.argv`, or the login shell: `SHELL` from the request env, else the daemon's, else
`/bin/sh`, with `-l`. `startup_cmds` become startup input (`cmd\r` each), written once the program
has produced its first output or after 500 ms, whichever comes first.

### 5.8 Persistence (surviving reboot / daemon loss)

- `persist::ScrollbackStore` keeps, per session, `~/.rakumux/sessions/<id>/scrollback.ansi`
  (ANSI-formatted rows separated by `\r\n`, oldest first) and `meta.json`
  (`SessionMeta { cwd, title, foreground_process, exit_code, updated_at }`). Writes are atomic (temp
  file + rename), files 0600, dirs 0700.
- Written every `HostConfig.persist_interval` (30 s) while output flows and when the process exits.
- Exited terminals stay in memory for `HostConfig.exited_retention` (10 minutes), then are forgotten.
- `session_read` on a session without a live terminal converts the saved scrollback to text through a
  passive emulator; the TUI shows it dimmed with "Enter to revive".
- Live cwd: OSC 7 when the shell emits it, otherwise `procinfo::cwd_of` on the foreground process
  (`/proc/<pid>/cwd` on Linux, `proc_pidinfo` on macOS). The control layer stores it in the
  registry and publishes `updated`.

### 5.9 Revive and agent resume

When a session is **Dead** or **Exited**, `session_revive`:

1. Starts a new terminal for the same session id in the recorded cwd, with the stored launch spec, a
   fresh environment snapshot and a fresh session token.
2. With `resume: true` and a stored `ResumeSpec`, appends the shell-quoted resume argv + `\r` to the
   startup input.
3. The TUI shows the saved scrollback dimmed above the new prompt (client side only; never fed to
   the PTY).

`ResumeSpec` sources:

1. **StartupCommand**: re-run the session's `startup_cmds`.
2. **KnownAgent**: when detection (§5.13) sees a known agent in the foreground, record its resume
   form (e.g. `claude --continue`). Verify the exact flags per agent at implementation time.
3. **ReportedByAgent**: an agent or its hook reports an exact resume command with
   `rakumux session report --resume -- <argv…>` (`session_report { resume }`), following Herdr's
   `agent_resume.rs` model.

### 5.10 Upgrades

- `hello` negotiates the protocol. A compatible older daemon keeps being used; clients disable
  actions whose capability is missing. On `protocol_mismatch` the TUI shows "Daemon update required:
  restarting ends N running sessions (they can be revived)" and restarts the daemon only with
  consent. `rakumux daemon restart` does the same from the CLI.
- Future: live handoff (PTY master fds over `SCM_RIGHTS` plus emulator checkpoints, like Herdr's
  `handoff_runtime.rs`). Keep per-terminal state serialisable so this stays possible.

### 5.11 Security

- Socket dir 0700, socket 0600, registration 0600. Peers with another UID are rejected
  (`SO_PEERCRED` on Linux, `getpeereid` on macOS).
- User clients authenticate with the 256-bit token from `service.json`. Session tokens are issued at
  spawn and revive and kept **in daemon memory only**: they die with the daemon, like the processes.
  Comparisons are constant time (`Token::matches`).
- **Threat model, stated plainly**: agents run as the same OS user. Any process can read
  `service.json`, read `/proc/<pid>/environ` or kill processes. Tokens and permissions give
  **attribution and prevent accidents** (wrong target, an agent poking a human's shell); they do not
  stop a deliberately hostile agent. Real isolation needs sandboxing and is out of scope
  (`agent-control.md` §5.4).

### 5.12 Observability

- Log to `~/.rakumux/logs/ptyd.log`, size-rotated (1 MiB, keep 1), level from `RAKUMUX_PTYD_LOG`
  (`error` | `warn` | `info` | `debug`; default `info`).
- `rakumux daemon status` prints pid, version, protocol, uptime, session count, live terminals,
  connected clients and RSS; `rakumux daemon logs` tails the log.

### 5.13 Agent state

- `detect::detect(DetectInput { screen, foreground, alternate_screen, title, ms_since_output })`
  returns `Detection { agent, lifecycle }`: which agent runs (`claude`, `codex`, `opencode`, `pi`,
  `gemini`, `aider`, `cursor-agent`, …, seen through wrappers such as `node …/claude` via the command
  line) and whether it is `Working`, `Blocked`, `Idle` or `Unknown`, from evidence at the bottom of the
  visible screen (status lines, interrupt hints, permission prompts). Plain shells → no agent,
  `Lifecycle::None`. Herdr's manifests (`src/detect/`) are a reference; the code and data are our own.
- Detection runs on `HostEvent::Output`, which the host throttles to at most one per second per
  terminal plus a final event 500 ms after output stops, so it sees the settled screen.
- The foreground process comes from `MasterPty::process_group_leader()` + `procinfo::process_info`,
  refreshed at most every 2 s while output happens.
- **Precedence** (`agent-control.md` §6.1): a fresh self-report wins for 30 s, then detection.
  Hooks only report identity; plugin reports (stage B) are authoritative when present.
- **`Done` vs `Idle`**: detection never returns `Done`. The control layer turns `Working → Idle` into
  `Done` until the session is touched (selected in a TUI, `session_touch`), then `Idle`.
- Lifecycle changes publish `lifecycle` events and drive the list's status glyph.

### 5.14 Control layer

`control::ControlService` implements `Control`. It owns:

- **Registry** (§11): the only reader and writer of `rakumux.sqlite3`.
- **Identity**: `Auth::User` must match the daemon's user token; `Auth::Session` must match that
  session's in-memory token. Protocol mismatch → `protocol_mismatch`.
- **Scope confinement** (§4).
- **Create**: validates cwd (inside scope), argv and env (§5.7); default name is a short readable
  generated name (two words), unique within the scope.
- **Ops**: every request the server forwards (§5.4). `kill { tree }` covers descendants; `delete`
  also removes saved scrollback; `revive` per §5.9; `report` stores `Reported`, agent info and a
  `ReportedByAgent` resume spec; `wait` blocks up to `timeout_ms` and wakes on state changes
  (`output` = substring on the visible screen); `touch` updates `last_active_at` and turns `Done` into
  `Idle`.
- **Permissions (agent-control stage A, 0.6.0)**:

  | Caller | May |
  |---|---|
  | User (TUI, CLI outside any session) | everything |
  | Session (process inside a session) | `session_list` / `get` / `read` / `wait` and `subscribe` within its own scope; `session_report`, `session_touch` and `session_rename` on **itself**. Everything else (create, send, submit, kill, delete, revive, attach, rename others) → `permission_denied`, with a message that agent control of other sessions arrives in a later release. |

- **Audit**: every mutating op and every denial goes to `audit_log` (time, scope, caller, method,
  target, decision, short detail; never terminal output). Pruned to 30 days.
- **Events**: `subscribe` filters by scope, session ids and kinds; `activity` is limited to one per
  second per session.
- `ControlConfig.limits` (`Limits { max_live_per_scope: 8, max_children_per_parent: 4, max_depth: 2 }`)
  exists for stage B and is not enforced on user callers.

---

## 6. Screen: master-detail in two layouts

Both layouts show the same things: the header (the scope path), the scope's sessions, the selected
session's detail (tabs **Terminal** and **Info**) and the footer. The layout is chosen in the settings
menu (§7), saved as `sessions.layout` in `~/.rakumux/config.json` and applied live. Geometry is
`layout::split(area, layout) -> LayoutAreas { list, tabs, detail }` (`tui/src/layout`).

> As built (0.6.x): the header is gone. The panes start at the top row; the project shows as the
> list's project item (Sidebar) or the selector at the left of the tab bar (Tabs); the mode badge,
> messages and the daemon connection state are in the one-row footer, which also stays in zoom.

### 6.1 Sidebar (default)

```
┌ RAKUMUX ───────────────────────────────────────── ~/repos/rakumux ┐
│ Sessions            │ ● api-refactor · claude · working         src/api  │
│ + New session       │┌─────────────────────────────────────────────────┐ │
│ ● api-refactor   2m ││ $ cargo test                                    │ │
│   working · src/api ││ running 42 tests ...                            │ │
│ ◐ docs-pass      1h ││ (live terminal: the session itself)             │ │
│   blocked           ││                                                 │ │
│ ✕ build          2h ││                                                 │ │
│   exited 1          ││                                                 │ │
│ ○ old-spike      3d │└─────────────────────────────────────────────────┘ │
│   dead · revive     │ [Terminal] [Info]                                  │
├─────────────────────┴────────────────────────────────────────────────────┤
│ ↑↓ select  ⌥↑↓ cycle  ⏎ focus  n new  r rename  d delete  ? help         │
└──────────────────────────────────────────────────────────────────────────┘
```

**Left pane: session list**, max(28–34 cols, 25%):

- Only sessions in the current scope, ordered by `last_active_at`; "+ New session" pinned at the top;
  `/` filters by name or cwd.
- Row: status glyph (live ● / blocked ◐ / exited ✕ / dead ○), name, relative last-active time.
  Dimmed second line: `reported.text` when present ("done: 3 findings"), otherwise the lifecycle
  word (working, done, idle) and the cwd relative to the scope root.

**Narrow terminals** (below ~100 cols): one pane at a time; Enter enters the session, `prefix Tab`
returns to the list.

### 6.2 Tabs

```
┌ RAKUMUX ───────────────────────────────────────── ~/repos/rakumux ┐
│ ‹ 1 ● api-refactor │ 2 ◐ docs-pass │ 3 ✕ build │ 4 ○ old-spike │ + ›     │
│ ● api-refactor · claude · working                               src/api  │
│┌────────────────────────────────────────────────────────────────────────┐│
││ $ cargo test                                                           ││
││ running 42 tests ...                                                   ││
││ (live terminal: the selected tab's session)                            ││
│└────────────────────────────────────────────────────────────────────────┘│
│ [Terminal] [Info]                                                        │
├──────────────────────────────────────────────────────────────────────────┤
│ ⌥↑↓ switch tab  ⏎ focus  n new  r rename  d delete  ? help               │
└──────────────────────────────────────────────────────────────────────────┘
```

- A one-row tab bar of the scope's sessions in list order replaces the list column, so the session
  gets the full width. Tabs are numbered `1`–`9`, show the status glyph, ellipsise long names, scroll
  so the selected tab stays visible, and show `‹ ›` when they overflow; a `+` tab creates a session.
- Switching tabs (`Option`/`Alt` + `↑`/`↓`, `prefix >`/`<`, or clicking a tab) shows that session's
  detail. The tab bar plays the list's role for focus: list-focus keys (§7) act on the selected tab.
- Rendering and hit areas: `layout::render_tabs` (`tui/src/layout`).

### 6.3 Detail pane and startup

Tabs of the detail pane:

- **Terminal** (default): the live session, rendered from its attach stream (§6.5).
- **Info**: cwd, scope, created time and creator, launch command, resume command, pid and foreground
  process, detected agent, reported state and summary.

Startup:

- No sessions in this scope → the detail pane shows a welcome with `n` to create.
- Sessions exist → the most recently active is selected and attached as observer; the list (or tab
  bar) has focus.
- Dead sessions → saved scrollback (dimmed) with "Enter to revive"; exited → final screen with exit
  code and "Enter to restart".
- Daemon unreachable → an error state explaining what failed, with retry.

### 6.4 Detail behaviour

- Selecting a session attaches as **observer** (live output, no input). Moving the selection
  detaches the previous stream; the process keeps running in the daemon.
- **Enter** focuses the pane and `promote`s to **controller**: keys go to the session and the PTY is
  resized to the pane.
- `prefix Tab` (or `prefix Esc`) returns focus to the list or tab bar and demotes to observer.
  (`prefix l` did until 0.6.0's Settings modal took it.)
- `prefix z` zooms: hides the list/tab bar and header so the session fills the screen, still in the
  TUI.
- If the focused session exits, focus returns to the list and the pane shows the exit code with
  "Enter to restart".
- **Nesting guard**: if `RAKUMUX_SESSION_ID` is set (Rakumux is running inside one of its own
  sessions), that session is marked "current" and can't be attached, to avoid an infinite mirror.

### 6.5 Rendering and input

- **Terminal view** (`tui/src/terminal_view`): `TerminalView` mirrors the session in a passive
  `vt::Emulator`; `reset(rows, cols, checkpoint)` on attach, `resized` and when showing saved
  scrollback; `feed(bytes)` for live output; `scroll(delta)` over rows the emulator holds; `render`
  draws cells straight into the ratatui `Buffer` (colours, bold, italic, underline, inverse, dim,
  wide characters spanning two cells, cropping/padding, a marker when scrolled back) and returns the
  cursor position when focused. Works on ratatui 0.25; **no upgrade**.
- **Input**: `key_to_bytes`, `mouse_to_bytes` (pane-relative, only when the program enabled mouse
  reporting) and `paste_to_bytes` (bracketed only when the program asked) build on the mode-aware
  `vt::input` encoders. Otherwise the mouse wheel scrolls local history; history beyond the screen
  comes from `session_read`.
- **Event loop**: `tokio::select!` over crossterm's `EventStream`, the active attach stream and the
  scope's event subscription (their blocking channels are pumped from a thread into tokio channels)
  and timers. Redraw at most ~60 fps, immediately on input.
- **Text selection**: the TUI captures the mouse, so native selection needs Shift+drag in most
  terminals (documented in help). In-app selection/copy mode is a follow-up (Herdr's `selection.rs`
  and `copy_mode.rs` are references).

### 6.6 Help modal

> As built (0.6.0): two modals share this frame. **Settings** (`,` or `o` in Rakumux mode) has
> **General** (the settings) and **Keys**, a two-column table of every action and its keys where
> Enter runs the selected action. **Help** (`?` in Rakumux mode) has **Overview**, **Sessions**,
> **Layouts**, **Agents & CLI** and **License**. There is no header: the footer's bottom-left badge
> shows the mode (orange RAKUMUX, quiet SESSION), then messages, and on the right the Help and
> Settings keys (after `Ctrl-\` in a session).

`?` in list focus and `prefix ?` in session focus open a centred modal (~80% × 80%, full screen below
80×24) with tabs **Overview**, **Keys**, **Sessions**, **Layouts**, **Agents & CLI** and **License**.
It covers the whole flow: sessions and scope, the master-detail screen and both layouts, every
shortcut (generated from the active key bindings, so a changed prefix or cycle key shows correctly),
observer vs controller, dead/exited sessions and revive, the single server with several TUIs, the
`rakumux` CLI and agent commands, AI naming, Shift+drag selection, and the existing proprietary
license text. No git, tmux, worktree or remote-mode content. Implemented in `tui/src/help`
(`HelpState`, `HelpContext`, `handle_key`, `render`); it replaces `components/help.rs`.

### 6.7 Several TUIs at once

- Each TUI subscribes to its scope (`subscribe { scope }`) and applies `created`, `updated`,
  `removed`, `lifecycle`, `reported` and `activity` to its list; after a reconnect it re-lists.
  Renames, deletes and new sessions from any client appear everywhere.
- Each TUI attaches to its selected session as observer. Enter in one TUI promotes it; the other
  receives `role_changed`, shows the session as "controlled elsewhere" and can take control back
  with Enter.
- The controller's pane size wins; observers crop or pad.

### 6.8 Full-screen attach outside the TUI

`rakumux attach <name|id>` is a thin raw-mode client: alternate screen, attach as controller at
the terminal's size, write the checkpoint and then output bytes **straight to stdout** (the user's
terminal does the emulation), forward stdin as input, send `resize` on SIGWINCH, restore the terminal
on exit. Prefix commands per §7. Works over SSH.

---

## 7. Keybindings

> **As built (0.6.0), superseding the tables below:** two modes. In session mode every key goes to
> the program except the Rakumux key `Ctrl-\` (`sessions.rakumux_key`), which demotes and
> switches to Rakumux mode (the list focus). Rakumux mode keys: `↑/↓`, `j/k`,
> `Option`/`Alt`+`↑/↓` select (preview follows); `gg`/`G`, `Home`/`End`; `Ctrl-d`/`Ctrl-u`; `{`/`}`
> previous/next project; Enter, `i`, `l`, `→` go in; Esc (after cancelling a pending `g`/`d` or the
> filter) or `Ctrl-\` back to the session Rakumux mode was entered from; Tab the session before;
> `1`-`9` into that session; `Ctrl-y`/`Ctrl-e`, PgUp/PgDn scroll the preview; `n` new, `r` rename,
> `p` pin, `x` close, `dd` delete, `z` full screen, `P` projects, `/` filter, `\` send `Ctrl-\` to
> the session, `?` Help, `,`/`o` Settings, `q` quit. `Option`/`Alt`+`↑/↓` work inside sessions only
> with `sessions.cycle_keys_in_session`. `rakumux attach` uses `Ctrl-\` as its prefix. Why: the
> planned `Ctrl-b` prefix is Claude Code's and OpenCode's "background" key and Alt+↑/↓ are OpenCode's,
> Claude Code's and Pi's; `Ctrl-\` is bound by none of them.

The prefix (default `Ctrl-b`, `sessions.prefix_key`) reuses the key letters of Rakumux's old tmux
bindings, so muscle memory carries over. Herdr uses the same default
(`src/config/keybinds.rs:14`). Rakumux is not meant to run inside tmux; if it is, tmux swallows
`Ctrl-b` (unsupported, documented in help).

**Everywhere in the TUI** (both layouts, list and session focus):

| Key | Action |
|---|---|
| `Option`/`Alt` + `↑` / `↓` | Previous / next session in list order (wraps). In session focus it is intercepted and never forwarded to the program. Needs a terminal that sends Alt-modified arrows (`CSI 1;3A` / `CSI 1;3B`: iTerm2, Ghostty, kitty, WezTerm, Alacritty; Terminal.app needs "Use Option as Meta key"). Configurable (`sessions.cycle_keys`). |

**List focus** (the list in Sidebar, the tab bar in Tabs):

| Key | Action |
|---|---|
| `j`/`k`, `↑`/`↓` | Select (in Tabs also `←`/`→`, and `1`–`9` jump to a tab) |
| `Enter`, `l`, `→` | Focus session · revive (dead) · restart (exited) |
| `n` | New session |
| `r` | Rename |
| `d` | Delete (confirm) |
| `/` | Filter |
| `Tab` | Next detail tab (Terminal ↔ Info) |
| `o` | Settings: layout (Sidebar / Tabs), API keys, AI provider; Keys tab (actions) |
| `?` | Help modal (§6.6) |
| `q` | Quit |
| `A`, `K` | Reserved for agent-control stage B (approval queue, kill tree) |

**Session focus (TUI) and `rakumux attach`**: everything goes to the session except the prefix and
the cycle keys.

| Keys | TUI | `rakumux attach` |
|---|---|---|
| `prefix Tab` / `prefix Esc` | Back to the list / tab bar | n/a |
| `prefix l` | Settings modal (both panes) | n/a |
| `prefix d` | Back to the list | Detach and exit |
| `prefix n` | New session | New session in the current directory |
| `prefix >` / `prefix <` | Next / previous session (fallback for the cycle keys) | Next / previous session in scope by recency (reattach) |
| `prefix r` | Rename | n/a |
| `prefix q` | Close session (confirm) | n/a |
| `prefix z` | Zoom toggle | n/a |
| `prefix ?` | Help modal (both panes) | n/a |
| `prefix prefix` | Send a literal `Ctrl-b` | Same |

---

## 8. New session flow

The new-session dialog (`n`, `prefix n`, the `+` tab, "+ New session"):

1. **Name**: optional. Blank → a generated two-word name, and an AI rename once there's meaningful
   output (§10).
2. **Directory**: defaults to the scope root; Tab completion; must be inside the scope (the daemon
   rejects anything else).
3. **Command**: empty = the user's login shell (default). Free text otherwise, split shell-style into
   `argv`.
4. **Startup commands**: optional, typed into the shell after it starts.

On submit, `create` runs in the background with a spinner, then the TUI selects the new session and
focuses it as controller. Reserved for agent-control stage B: an optional **Agent** field (kind +
initial prompt) using the same create path as agents.

> **Later change:** the dialog was cut down to the **Name** field alone. The session goes in the
> current project (the selected session's, else the first one shown, displayed under the name),
> starts in the project folder and runs the login shell. Directory, command and startup commands
> (and the project picker of `projects-api-mcp.md`) are left to the CLI:
> `rakumux new --cwd DIR --cmd C -- ARGV`.

---

## 9. Command line

All subcommands live in the `tui` binary (`tui/src/cli`, entry `cli::dispatch`); with no subcommand
the TUI starts. Global options may precede the subcommand: `--ptyd-path <PATH>`,
`--services-path <PATH>` (accepted and ignored, for older launchers) and `--debug`. Exit codes: 0 ok,
1 error (message on stderr), 2 usage. `rakumux.sh` handles `-h`/`--help` and
`--check-updates`/`--update` itself and passes every other argument through.

| Command | Purpose |
|---|---|
| `rakumux` | TUI for the current scope. |
| `rakumux new [--name N] [--cwd P] [--cmd C]... [--detach] [-- ARGV...]` | Create a session (env snapshot = current env; size = terminal size or 80×24), then attach unless `--detach`. |
| `rakumux ls [--json] [--all]` | Sessions in the current scope with status; `--all` includes dead ones. |
| `rakumux attach <name\|id>` | Full-screen attach (§6.8). |
| `rakumux rename [<name\|id>] <new name>` | Defaults to `$RAKUMUX_SESSION_ID`. |
| `rakumux kill <name\|id> [--tree]` | Terminate a session (and its descendants). |
| `rakumux rm <name\|id>` | Delete a session and its saved scrollback. |
| `rakumux daemon status\|stop\|restart\|logs` | Daemon management (§5.10, §5.12). |
| `rakumux session list\|get\|read\|wait\|report` | JSON for agents and scripts (agent-control stage A). `report` takes `--state`, `--text`, `--summary`, `--file` (repeatable), `--agent-kind`, `--agent-session-id`, `--resume -- ARGV...`. |

Later: the rest of `rakumux session …` and `rakumux integration …` (stage B), `rakumux mcp`
(stage C, `plans/agent-mcp.md`).

---

## 10. What's kept, what's deleted

### Kept and re-homed

| Feature | New home |
|---|---|
| AI session naming | `tui/src/naming`: `suggest_session_name(&Config, &NamingInput { history, screen })`, ported from `server/services/ai.sh` and `json.sh` with the same providers (Anthropic, OpenAI), models, prompt and output clean-up, over `reqwest`. Keys from `~/.rakumux/config.json`, falling back to `ANTHROPIC_API_KEY` / `OPENAI_API_KEY`. The TUI calls it when a session with a generated name first goes `Idle` or `Done`, with the session's history file and its visible screen (`session_read`). |
| Command history hook (`shell/rakumux-command-hook.sh`) | When `RAKUMUX_SESSION_ID` is set, logs commands to `~/.rakumux/history/<id>.log` (`$RAKUMUX_HOME` respected); no tmux; `rakumux_rename_session` runs `"${RAKUMUX_BIN:-rakumux}" rename "$*"`. |
| App detection (`app/context.rs`) | Daemon agent detection (`ptyd/src/detect.rs`, §5.13). |
| Help | The help modal (`tui/src/help`, §6.6) replaces `components/help.rs`. |
| API keys menu (Anthropic/OpenAI), AI provider, upgrade dialog, updater, config, theme | Kept; the API keys menu becomes part of the settings menu (§7). |
| Auth (`tui/src/auth`, flag `auth: false`) | Untouched beyond compiling (§18). |
| `SwitchDirectory` (`cd` on exit) | Kept: quitting with a session selected can print `cd "<cwd>"` for the shell wrapper (§17). |

### Deleted: git and worktrees

- TUI: `api.rs` (`ServicesApi`), the worktree tree (`components/worktree_list.rs`,
  `app/navigation.rs`, `DisplayItem`), the git status panel (`components/git_status.rs`,
  `app/git.rs`), merge/rebase/squash (`app/merge.rs` and its dialogs), worktree/branch deletion
  (`app/deletion.rs`), the branch-first create dialog, repo copy locations (`app/repo_config.rs`),
  `app/session_name.rs`, `app/session_registry.rs`, `app/session_preview.rs`,
  `components/session_preview.rs`, `app/context.rs`, and `--new-tmux`/`--cmd` in `main.rs`.
- env-copy: `src/bin/env_copy.rs` and the `env-copy` binary target, `shell/env_copy*.sh`,
  `GW_ENV_COPY_BIN`, and the repo file `.rakumux/options.json`.
- `rakumux-cli`: `rakumux-cli.sh`, `shell/cli_*.sh`, `shell/git/**`, npm's `bin/rakumux-cli.js`
  and `rakumux-cli` bin, Homebrew's `bin/rakumux-cli`.
- Launcher: `gwr_check_git_repo` (`shell/gwr_git.sh`); Rakumux runs in any folder.
- Cargo (`tui` package): `walkdir`, `strip-ansi-escapes` and `rusqlite` if no longer used.

### Deleted: the shell services layer

- `services.sh`, `server/` (services dispatch, JSON helpers, git/tmux/session/AI services,
  `server/session/tmux_api.sh`), `api/` (`git.sh`, `tmux.sh` shims).
- `gwr_check_services_script` (`shell/gwr_services.sh`), the build's services smoke test
  (`shell/build/services.sh`), the `--services-path` argument from `rakumux.sh`.
- `shell/fix_hanging_*.sh` and `fix-hanging-and-redeploy.sh` if only used by removed parts;
  `config.env` if unused.

### Deleted: tmux

- Everything tmux lived in the shell layer and the Rust pipelines above; in addition
  `shell/tmux/**`, tmux-specific tests (`test-workspace-session-cycle.sh`,
  `test-auto-rename-plain-name.sh`, `test-command-hook-*.sh`, `test_rename*.sh`, `test_ai_rename.sh`,
  `test_split*.sh`, `test_actual_split.sh`, `debug-sessions.sh`, `debug_parse.sh`), Homebrew's
  `depends_on "tmux"`, and tmux in npm/Homebrew descriptions, keywords, `shell/gwr_usage.sh` and the
  install scripts.
- Data: the 0.5.x `sessions` table in `~/.rakumux/rakumux.sqlite3` is dropped when the daemon
  first opens the registry; `~/.rakumux/history/` files named after tmux sessions are left alone.
- Older tmux sessions: nothing in 0.6.0 detects, lists, attaches to or kills them, and the tmux key
  bindings 0.5.x added to the user's tmux server stay until that server restarts.

### Deleted: remote / cloud mode (done, `728de9a`)

`cloud/`, `rakumux.vm.yaml`, the cloud tests, the TUI's cloud/sync/SSH dialogs and state,
`FeatureFlags.remote_mode`, and the Cargo dependencies `keyring`, `directories`, `serde_yaml` and
`mockito`. Users' `.rakumux/cloud.json`, `ssh.json`, `worktree.json` files and keyring entries are
not touched (release notes explain how to remove them).

---

## 11. Registry (SQLite)

The daemon is the **only** reader and writer of `~/.rakumux/rakumux.sqlite3`
(`Layout::registry_db()`). Settings: WAL, `busy_timeout`, `PRAGMA user_version = 2`. Live state
(terminal id, `Live` status, session tokens) is in memory only.

```sql
-- first open by 0.6.0: DROP TABLE IF EXISTS sessions;  (the 0.5.x tmux registry; no migration)
CREATE TABLE sessions_v2 (
  id TEXT PRIMARY KEY,
  scope_root TEXT NOT NULL,
  name TEXT NOT NULL,
  cwd TEXT NOT NULL,
  launch_json TEXT NOT NULL,
  resume_json TEXT,
  created_by_json TEXT NOT NULL,
  parent_id TEXT,
  depth INTEGER NOT NULL DEFAULT 0,
  agent_json TEXT,
  reported_json TEXT,
  exit_code INTEGER,
  created_at INTEGER NOT NULL,
  last_active_at INTEGER NOT NULL,
  ended_at INTEGER
);
CREATE INDEX idx_sessions_v2_scope ON sessions_v2(scope_root, last_active_at DESC);

CREATE TABLE audit_log (
  id INTEGER PRIMARY KEY,
  ts INTEGER NOT NULL,
  scope_root TEXT NOT NULL,
  caller TEXT NOT NULL,
  method TEXT NOT NULL,
  target_session_id TEXT,
  decision TEXT NOT NULL,
  detail TEXT
);   -- pruned to 30 days on open
```

**Reconcile** happens once, when the daemon starts: no terminal survives a daemon restart, so every
stored session becomes `Dead`, or `Exited` if it has an exit code. While the daemon runs, the
registry and live terminals change together, so there is nothing else to reconcile. Clients never
reconcile; they re-list after reconnecting.

---

## 12. Code layout after the refactor

```
tui/
  Cargo.toml                 # workspace + the `tui` package
  crates/
    session-core/src/        # lib.rs (PROTOCOL_VERSION, env names), model.rs, api.rs, frame.rs,
                             # ids.rs, paths.rs, client.rs
    vt/src/                  # lib.rs (Emulator, query responder), input.rs; examples/spike.rs, throughput.rs
    ptyd/src/                # main.rs, lib.rs, server.rs, registration.rs, log.rs,
                             # host.rs + terminal/ (TerminalHost), control_api.rs + control/ (Control),
                             # persist.rs, procinfo/, detect.rs
  src/
    main.rs                  # cli::dispatch, else the TUI
    cli/                     # new, ls, attach (raw client), rename, kill, rm, daemon, session
    naming/                  # AI session naming
    terminal_view/           # live pane widget + key/mouse/paste encoding
    layout/                  # Sidebar/Tabs geometry, tab bar, cycling
    help/                    # help modal
    app.rs, app/             # state slices, handlers by focus, session pipelines, config
    ui.rs, components/       # session list, detail, dialogs, footer, settings
    auth/                    # deferred; flag off
    updater.rs, version.rs, theme.rs
```

### Architecture rules (mirrored in `AGENTS.md`)

- One daemon, many clients. No client is special; no client assumes it is alone.
- The UI talks only to `SessionApi`. Domain types come from `session_core::model`; nothing in
  `tui/src` opens the socket, the registry or a PTY directly.
- In the daemon, `control` owns sessions and policy; `terminal` owns PTYs and knows nothing about
  scopes or callers; `server` owns connections and framing. They meet only at `Control` and
  `TerminalHost`.
- No git, no tmux, no shell services. Shell scripts only launch binaries, set the terminal title,
  check for updates and build.
- Contracts change only additively (new optional field, new method with a default, new variant) and
  only through the coordinator (`parallel-execution.md`).

### Splitting `app.rs`

- Move `App` fields into state slices: `sessions: SessionListState`, `detail: DetailState`,
  `focus: Focus`, plus layout, settings and help state.
- Delete: `sessions: HashMap<slug, …>`, `expanded_worktrees`, `renaming_sessions`,
  `rename_original_names`, `rename_start_times`, `session_previews`, `session_metadata`,
  `preview_*` polling fields, `tmux_available`, git status and merge state, `DisplayItem` and the
  tree building in `navigation.rs`.
- Handlers split by focus (list, session, dialogs) instead of `handlers/browsing.rs`.

### Config additions (`~/.rakumux/config.json`)

```json
"sessions": {
  "prefix_key": "ctrl+b",
  "cycle_keys": ["alt+up", "alt+down"],
  "layout": "sidebar"
}
```

`layout` is `sidebar` or `tabs` (`layout::SessionLayout`, snake_case). `cycle_keys` is previous, next.
The daemon uses the `HostConfig::new` defaults in 0.6.0 (2,000 rows in memory, 256 KiB replay ring,
10 minutes exited retention, 30 s persist interval); exposing them is a follow-up (§17).

---

## 13. Distribution: npm and Homebrew

### 13.1 npm

- Bundle `rakumux-ptyd` in `resources/prebuilt/<platform>/` next to `tui`; `install.js` copies it
  like `tui`. Drop `env-copy`.
- Drop `rakumux-cli.sh`, `services.sh`, `server/`, `api/`, `bin/rakumux-cli.js` and the
  `rakumux-cli` bin from the package.
- **`bin/rakumux.js` must run `rakumux` (`rakumux.sh`) for every argument.** Today it sends
  anything other than none / `--debug` / `-d` / `--help` / `-h` / `--version` to `rakumux-cli`
  (the same routing as Homebrew, §13.2), so `rakumux new`, `ls` and `attach` would break.
- Description and keywords without tmux, git or worktree.
- Fix the license metadata and ship `LICENSE` + `THIRD_PARTY_NOTICES` (§13.4).
- Note: `package.json` declares `cpu: arm64` for Linux, but `build-release.sh` only builds macOS
  (Intel, arm64) and Linux x64. Unchanged in this round.

### 13.2 Homebrew: what's wrong today

Checked against the live tap `humanunsupervised/homebrew-rakumux` (formula 0.5.81) and the v0.5.81
release tarballs:

- **Issue #1 ("No such file or directory - commands.sh", reported on 0.5.63)** was caused by the
  formula still listing `commands.sh` after it had left the release tarball. That line was removed in
  `7947b78` (2026-06-05). A simulated install of the 0.5.81 formula on Linux works: the tarball's
  SHA-256 matches, every installed path exists, the formula's own `test` passes and
  `rakumux --version` prints 0.5.81. It hasn't been confirmed with a real `brew install` on macOS,
  and the issue is still open.
- **The repo copy drifted**: it was synced with the tap's 0.5.81 in `1e9bfa7`, but three overlapping
  scripts still touch the formula (`update-homebrew-sha.sh`, `update-homebrew-formula.sh`,
  `update-homebrew-tap.sh`) and none commits the result back.
- **The wrappers route by argument.** Homebrew's `bin/rakumux` sends any arguments other than
  none / `--debug` / `-d` / `--help` / `-h` / `--version` to `rakumux-cli.sh`, with a temp-file `cd`
  capture that can't change the caller's directory because it runs in a subprocess. npm's
  `bin/rakumux.js` routes the same way. Every 0.6.0 subcommand (`rakumux new`, `ls`, `attach`, …)
  would break on both channels.
- **`license "Proprietary"`** fails `brew audit` ("contains non-standard SPDX licenses"), which is
  why `update-homebrew-sha.sh` already expects audit warnings. See §13.4.
- **Tarballs carry macOS metadata**: built on macOS with AppleDouble `._*` files (89 in the Linux
  tarball) and `com.apple.provenance` xattrs; GNU tar prints a warning for each.
- `depends_on "git"`, `depends_on "tmux" => :recommended` and `depends_on "jq"`, and the install lines
  for `rakumux-cli.sh`, `services.sh`, `env-copy`, `server` and `api/*.sh`, must go in 0.6.0.

### 13.3 Homebrew: fixes

**Before the refactor (phase 0):**

1. Run `brew install humanunsupervised/rakumux/rakumux` (0.5.81) on macOS arm64 and Intel. If it
   works, reply on issue #1 and close it (the maintainer posts this; it's public). **Open.**
2. Copy the tap's 0.5.81 formula back into the repo so the two match. **Done** (`1e9bfa7`).

**In the release (package R1; published in phase 10):**

1. Replace the three scripts with one, `install/scripts/release-homebrew.sh`: render the formula from
   a template (version + three SHA-256s), write it to the repo, run `brew audit --strict`, install
   from a local tap, run `brew test`, then push to the tap. The repo copy is the source of truth;
   `full-release-automation.sh` calls the new script.
2. Wrapper: `bin/rakumux` runs `exec rakumux.sh "$@"` for every argument, the same as npm. The
   `rakumux-cli` routing and the temp-file `cd` capture go.
3. Install `rakumux-ptyd` into `libexec` and export `GW_PTYD_BIN` alongside `GW_TUI_BIN`.
4. No `depends_on` at all (git, tmux and jq are no longer used); no `rakumux-cli`, `env-copy`,
   `services.sh`, `server` or `api` install lines; update `desc`.
5. `license :cannot_represent` (Homebrew's allowed symbol for licenses SPDX can't express), so
   `brew audit --strict` passes (§13.4).
6. Build tarballs with `COPYFILE_DISABLE=1` and without macOS metadata (`--no-mac-metadata` /
   `--no-xattrs`), so no `._*` files or xattrs ship; include `LICENSE` and `THIRD_PARTY_NOTICES`.
7. `test do`: `rakumux --version` matches `version` and `rakumux-ptyd --version` works. The test
   must not start a daemon.
8. Verify a fresh install and a `brew upgrade` from 0.5.81 on macOS arm64, macOS Intel and Linux x64
   (Linuxbrew).

### 13.4 License: closed source

Decision: Rakumux stays **closed source** under its existing proprietary license (copyright Antonio
de Perio and Human Unsupervised Pty Ltd). Open-sourcing may be revisited later (§18).

Where the license is declared today:

| Place | Says | Action |
|---|---|---|
| Public repo `humanunsupervised/rakumux` `LICENSE` | Proprietary | Source of truth; unchanged. |
| In-app Help → License tab | Proprietary | Text carried over into the help modal (§6.6). |
| `install/scripts/setup-public-repo.sh` (writes the public `LICENSE`) | Proprietary | Unchanged. |
| Homebrew formula | `"Proprietary"` (invalid for `brew audit`) | `license :cannot_represent` (§13.3). |
| npm `install/npm/package.json` | **`"MIT"`** (wrong) | `"license": "SEE LICENSE IN LICENSE"` (npm's form for non-SPDX licenses). |
| npm package contents | No `LICENSE` file | Add `install/npm/LICENSE` (same text as the public repo) and list it in `files`. |
| Release tarballs | No `LICENSE` file | Include `LICENSE`. |

Already-published npm versions (up to 0.5.81) keep their `MIT` metadata; npm can't edit published
versions, so 0.6.0 is the first to show the correct license.

**Third-party notices.** Closed-source binaries still have to carry the notices of the open-source
code they contain. The binaries link MIT/Apache crates (ratatui, tokio, reqwest, rusqlite, …) and
ship no notices today; the daemon adds `portable-pty` and `vt100` (both MIT). Generate
`THIRD_PARTY_NOTICES` from `Cargo.lock` at release time with `cargo about` (template and config
committed) and ship it in the npm package and tarballs.

**Reference projects.** Herdr (Apache-2.0) and opencode-pty (MIT) are design references. Implement
from the designs rather than copying code. If any code is copied, keep its copyright and license
notice in `THIRD_PARTY_NOTICES` (Apache-2.0 also requires carrying any `NOTICE` file and marking
modified files).

---

## 14. Phases and work packages

Nothing is published to npm or Homebrew until phase 10. Phases 4–8 run in parallel as work packages
in separate git worktrees against the contracts fixed in phase 3; `plans/parallel-execution.md` has
each package's file ownership, rules and "done when". Sizes are relative (S < M < L).

| Phase | Scope | Packages | Status / done when | Size |
|---|---|---|---|---|
| **0. Prep** | Branch `sessions-refactor`; baseline tests; Homebrew now-steps (§13.3) | n/a | Done, except the macOS `brew install` check and the reply on issue #1 (maintainer). | S |
| **1. Delete remote/cloud mode** | §10 | n/a | Done (`728de9a`). | S |
| **2. Emulator spike** | `crates/vt`; programs compared with tmux | n/a | Done (`687b6d1`): go with `vt100` (§5.6). | S |
| **3. Contracts** | Workspace; `session-core` (model, protocol, frame, ids, paths, client types); `ptyd` boundaries (`host.rs`, `control_api.rs`) and support-module signatures; TUI module contracts (`terminal_view`, `cli`, `naming`); decisions 1–9 | n/a | Done (`71b0ff3`; plans `cf4bdf1`, `208b395`). | M |
| **4. Daemon** | Terminal runtime, socket server, registration, log; control layer (registry, scope, identity, permissions, waits, lifecycle, events, audit); scrollback store, process info, agent detection | A1, A2, A3 | Per package. | L |
| **5. Clients** | Client library + CLI; terminal view widget; help modal + layouts; master-detail TUI on `SessionApi` (settings menu, `sessions.layout` / `cycle_keys`, `Option`+`↑`/`↓` in every focus state) | B1, C2, C3, C1 | Per package. | L |
| **6. Remove the shell layer** | Delete services/server/api/rakumux-cli/git/tmux/env-copy shell code and tests; AI naming in Rust; command hook on session ids | S1 | Per package. | M |
| **7. Release packaging** | Launcher, `build-release.sh`, npm package and wrapper, Homebrew script and formula, `LICENSE`, `THIRD_PARTY_NOTICES` | R1 | Per package; nothing published. | M |
| **8. Plans and docs** | This revision, `agent-control.md` rev 2, `AGENTS.md`, `tui/src/app/AGENTS.md`, `shell/AGENTS.md`, `auth/AGENTS.md`, `tui/README.md` | P1 | No doc describes tmux, worktrees, merge helpers, remote mode or the services layer as current behaviour. | S |
| **9. Integration + hardening** | Merge A3 → A2 → A1 → B1 → C2 → C3 → C1 → S1 → R1 → P1; end-to-end tests (§15); protocol fixture tests; crash/kill tests; upgrade-consent flow; 15+ session load test (observer fan-out, memory); macOS run (procinfo, clipboard/keychain access from daemon-spawned processes, `setsid` vs the Aqua bootstrap namespace); docs finalisation | coordinator | Create a session from the CLI, close the terminal, reattach elsewhere with the screen intact; two TUIs on one scope stay in sync and hand control back and forth; killing a client never kills a session; `kill -9` of the daemon → every session `Dead` → revive restores cwd (and the agent, where known); an agent in a session can list, read, wait and report, and is denied everything else; memory and CPU within budget at 15+ sessions. | M |
| **10. Release 0.6.0** | Version above npm's 0.5.81; release notes (§2); publish npm and Homebrew | maintainer | Fresh `npm i -g rakumux` and `brew install` work on macOS (arm64, x64) and Linux x64 without git, tmux or jq installed; `rakumux new/ls/attach` behave the same on both channels; upgrading from 0.5.81 (in-app updater, `brew upgrade`) works. | S |
| **11. Agent control stages B–C** | 0.7.0 / 0.7.x per `agent-control.md` §8; MCP per `plans/agent-mcp.md` (in progress) | later | n/a | n/a |
| **12. Follow-ups** | libghostty-vt evaluation; cross-folder view; live upgrade handoff; in-app selection/copy mode; multiple terminals per session; daemon tuning in config; Linux logind delay inhibitor to flush scrollback before shutdown | later | n/a | n/a |

---

## 15. Testing

Each package's "done when" in `parallel-execution.md` lists its tests; this is the overall picture.

- **Unit**
  - Codec round trips; unknown-field and unknown-enum tolerance; frozen v1 fixtures (phase 9).
  - Checkpoint equivalence: feed bytes to emulator A, apply A's checkpoint to a fresh emulator B,
    assert identical grid, cursor and modes.
  - Query responder: `CSI 6 n` → `CSI r;c R` at the right position; DA replies; no reply for
    unknown queries.
  - Key/mouse encoding across application-cursor, bracketed-paste and SGR-mouse modes;
    `TerminalView` renders known byte streams (styles, wide chars, cropping, cursor).
  - Scope: canonical resolution, symlinks, cwd outside scope rejected.
  - Env snapshot filtering; nesting guard; list ordering and filtering; key routing (prefix,
    promote/demote, zoom, `Option`+`↑`/`↓` in list and session focus and never forwarded).
  - Layouts: `layout::split` at several sizes for Sidebar and Tabs, tab overflow/ellipsis/hit areas,
    `cycle` wrap-around; switching layout applies live and is saved.
  - Help modal key handling; renders contain the configured prefix and cycle keys.
  - Persist, procinfo and detection fixtures per agent.
- **Daemon integration** (temp `RAKUMUX_HOME`, real socket, real `bash --norc --noprofile`)
  - spawn → submit `echo hi` → read sees it; startup input delivered; a query reply reaches the
    program (`printf '\e[6n'` round trip).
  - Two attachments: controller/observer, promotion and `role_changed`; resize broadcast.
  - Client disconnect leaves the terminal running; reattach checkpoint equals the screen.
  - terminate + exit code; second `claim` fails with `AlreadyRunning`; other-UID peer rejected.
- **Control layer**: auth (user, session, bad token, protocol mismatch); scope confinement; create
  env/argv/cwd validation; list ordering; rename uniqueness; kill tree; delete; revive with resume;
  report; wait (match, timeout, exit); `Done`/`Idle`; event filtering; the session-caller permission
  matrix; legacy table drop; audit entries (never containing output).
- **Client and CLI**: in-process fake daemon over a temp socket (hello/auth modes, request mapping,
  attach and event streams); the spawn helper detaches stdio (spawning from inside `$(…)` returns at
  once); argument parsing.
- **End to end** (coordinator, phase 9): spawn on demand from the TUI and the CLI; two TUIs on one
  scope; a session created by the CLI appears live in the TUI; `rakumux session …` from inside a
  session; `kill -9` the daemon → `Dead` → revive; stale lock/socket cleanup.
- **UI**: ratatui `TestBackend` renders at 80×24 and 160×48 for empty / live / exited / dead states,
  both focus states, both layouts, zoom and the help modal.
- **Release**: install from a locally packed npm tarball on a machine without git, tmux or jq;
  `brew audit --strict`, install from a local tap and `brew test` on macOS and Linuxbrew; upgrade
  from 0.5.81 on both channels; the tarballs contain no `._*` files; the npm package and tarballs
  contain `LICENSE` and `THIRD_PARTY_NOTICES`; `npm view rakumux license` shows
  `SEE LICENSE IN LICENSE`.
- **Manual** (`tests/manual-test-instructions.md`, rewritten for the session model): first run with
  no sessions; the spike's program list inside the pane; resize; prefix keys; `Option`+`↑`/`↓` in
  iTerm2/Ghostty and in Terminal.app with and without "Use Option as Meta key"; switch layouts in
  settings; help modal; quit and reopen (sessions still running); two TUIs side by side on one
  folder; two different folders show separate lists; reboot and revive; `rakumux attach` from a
  plain terminal and over SSH; the shell wrapper `cd`s correctly and never hangs; an agent in a
  session reports its state.

---

## 16. Risks and mitigations

| Risk | Mitigation |
|---|---|
| Emulator fidelity (`vt100` lacks kitty keyboard, graphics, some modes) | Spike passed for the target programs; the `crates/vt` wrapper keeps libghostty-vt swappable (phase 12). |
| Programs hang waiting on terminal query replies | Daemon-side query responder (§5.6); tests and the spike cover it. |
| Daemon spawn hangs the shell wrapper | Stdio on `/dev/null` + `setsid` (§5.2); a test spawns from inside `$(…)`. |
| The daemon is a single point of failure for every session and the registry | Per-terminal `catch_unwind`; control and terminal layers separated by `TerminalHost`; SQLite WAL; scrollback snapshots + revive make loss recoverable. |
| Memory growth with many sessions | 32 B/cell budget; 2,000-row default; client emulators without scrollback; load test in phase 9. |
| Stale environment in new sessions | Per-request env snapshot from the client (§5.7). |
| Daemon/TUI version skew | Protocol negotiation, capabilities, additive-only changes, consent-based restart (§5.10). |
| Several TUIs fight over a session | One controller per terminal; promotion demotes the other, which keeps watching; events keep every list in sync (§6.7). |
| `Option`+`↑`/`↓` is swallowed by some terminals, or a program needs it | `prefix >`/`<` fallback; `sessions.cycle_keys` to rebind; help explains the Terminal.app setting. |
| Agents abuse the session API (runaway fan-out, cross-session prompt injection) | Stage A only lets session callers read within scope and change themselves; `read` results are marked `untrusted`; stage B adds policy, limits, approvals (`agent-control.md` §5). |
| Same-user processes bypass tokens | Documented, not prevented (§5.11). |
| Existing users lose access to old tmux sessions and the git features through Rakumux | Accepted: small user base (§2); tmux sessions keep running; git still works in any session; release notes explain. |
| Parallel packages drift from each other | Contracts fixed in code (`71b0ff3`), additive-only changes through the coordinator, merge order and end-to-end tests (phase 9). |
| Homebrew formula drifts from releases again | One script renders, audits, tests and publishes it; the repo copy is the source of truth (§13.3). |
| Channel-specific behaviour (npm vs Homebrew wrappers) | Both wrappers run `rakumux.sh` for every argument; the release test runs the same commands on both. |
| Long unpublished branch drifts from `main` | Merge to `main` as phases finish; only publishing waits. |
| Mouse capture blocks native text selection | Document Shift+drag; in-app selection is a follow-up. |
| macOS specifics (process info; `setsid` vs the Aqua bootstrap namespace, see Herdr `src/platform/macos/bootstrap.rs`) | `procinfo` behind `cfg(target_os = "macos")`; clipboard/keychain checks from daemon-spawned processes in phase 9. |
| Two binaries to ship | Both built by `build-release.sh` and found by `find_ptyd_binary`; `rakumux daemon status` for support. |

---

## 17. Open questions

1. **Subfolder launches.** Launching from `~/repo/src` gives a separate, possibly empty list from
   `~/repo` (decision 4). Keep it that way, or show a hint when a parent folder has sessions?
2. **`cd` on exit.** With worktrees gone, is printing `cd "<session cwd>"` on quit (and the shell
   wrapper that evals it) still worth keeping?
3. **Daemon tuning.** Expose `scrollback_rows`, `replay_bytes`, `exited_retention` and
   `persist_interval` in `config.json` in 0.6.0, or wait until someone needs them?

Agent-control questions are in `agent-control.md` §10.

---

## 18. Deferred to the next round

Not part of 0.6.0:

- **Auth** (`tui/src/auth`, Clerk; `auth: false` in `feature_flags.rs`): left as is, kept compiling
  (it still uses `reqwest`). Design notes in `auth/AGENTS.md`.
- **Usage tracking**: there are no analytics today (§2). Whatever is chosen (e.g. an opt-in,
  anonymous daily ping) needs its own design, endpoint and privacy note.
- **Open-sourcing**: Rakumux stays closed source for now (§13.4). If that changes, the license
  table in §13.4 lists every place to update.
- **Agent control stages B–D** (0.7.0 and later, `agent-control.md` §8) and the **MCP adapter**
  (`plans/agent-mcp.md`, in progress).
- **Cross-folder view.**

---

## 19. Research references

Local clones: `~/repos/herdr`, `~/repos/opencode`, `~/repos/opencode-pty`.

**Herdr** (Apache-2.0, ~285k lines Rust): no tmux. `herdr server` runs detached via `setsid`
(`src/server/autodetect.rs:197`); one server/socket per named session (`src/session.rs`); PTYs via
`portable-pty`; screen state in vendored libghostty-vt; persists layout/cwd/history (`src/persist/`)
and relaunches agents with resume commands after reboot (`src/agent_resume.rs`); live upgrade hands
PTYs to the new server (`src/handoff_runtime.rs`); agent-state detection from snapshots
(`src/detect/`); `Ctrl-b` prefix by default. Its stated direction, "a server-owned runtime protocol
with the TUI as one client" (`herdr/AGENTS.md`), is the model for §5's single server.

**OpenCode**: a "session" is an LLM conversation in SQLite (`packages/core/src/session/sql.ts`).
Terminals come from **opencode-pty** (MIT, ~8k lines Rust): `portable-pty` + libghostty-vt,
authenticated framed socket protocol (v7), controller/observer attach with bounded replay,
snapshots, row reads, grouped per conversation via `group_id`
(`packages/core/src/persistent-pty/index.ts:169`). Single-owner model: the daemon dies with its owner
connection unless handed off. Fine for OpenCode's long-lived server, wrong for Rakumux, where the
daemon must outlive every client.

| # | Option | Verdict |
|---|---|---|
| 1 | Plain tmux | Rejected: nesting/prefix issues, user config leaks in, no structured output, nothing survives reboot. |
| 2 | tmux control mode | Rejected: still tmux-bound. |
| **3** | **Own PTY daemon** | **Chosen.** Full control of lifecycle, streaming, persistence and agent state; no tmux dependency. |
| 4 | Herdr as backend | Rejected: heavy dependency on a fast-moving product that overlaps Rakumux. |
