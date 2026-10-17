# Client/Server Stage 1 Reorganization Plan

## Goal
Prepare Orchestra for a client/server architecture by reorganizing source ownership into top-level `client/` and `server/` folders without introducing a daemon, network protocol, preflight/autoconnect behavior, or new client/server interface yet.

Stage 1 is a code ownership refactor only:
- Server owns git/worktree operations, tmux/session operations, SQLite session registry code, auth/config/persistence services, and services dispatch implementation.
- Client owns the TUI, keyboard/input handling, rendering, launchers, user-facing CLI command wrappers, and shell hooks.
- Cloud/remote-VM work is excluded from this client/server refactor and parked as a top-level `cloud/` feature slice.
- Existing calls remain direct function/module/script calls.
- Root-level entrypoints stay as compatibility shims so current installs, tests, wrappers, and tmux bindings keep working.

## Current Architecture Audit

### Runtime Flow Today
1. `orchestra.sh` launches `tui/target/.../tui` with `--services-path ./services.sh`.
2. `tui/src/main.rs` owns terminal setup, CLI flags, update checks, and TUI event loop.
3. `tui/src/app.rs` owns large application state and delegates to `tui/src/app/*` pipelines.
4. `tui/src/api.rs` shells out to `services.sh` and exposes typed async methods.
5. `services.sh` dispatches JSON-ish commands to `api/git.sh`, `api/tmux.sh`, `server/services/*`, and Git service handlers.
6. `api/git.sh` executes git/worktree operations.
7. `api/tmux.sh` executes tmux operations and also owns SQLite registry, tmux status UI, tmux key bindings, command hook injection, and AI session naming.
8. `orchestra-cli.sh` is a user-facing shell CLI that sources git/tmux APIs and command scripts.

### Main Boundary Problems
- `api/tmux.sh` is the largest server-side hotspot. It mixes tmux primitives, SQLite registry, workspace session cycling, status bar rendering, key bindings, hook injection, env/config loading, and AI naming.
- `tui/src/app.rs` is mixed. It stores UI state, launches async tasks, calls cloud/auth/sync services, invokes services operations, reads/writes files, and updates dialogs.
- SQLite session registry logic exists in two places: Rust `tui/src/app/session_registry.rs` via `rusqlite`, and shell `api/tmux.sh` via embedded Python `sqlite3`.
- `tui/src/api.rs` mixes DTO definitions with the shell services command adapter.
- Build/install/test scripts assume the current flat root layout: `tui/`, `orchestra-cli.sh`, `orchestra.sh`, `services.sh`, `api/`, and `shell/`.

## Target Source Layout

```text
client/
  tui/                         # current Rust TUI crate, moved from tui/
  shell/
    orchestra-cli.sh                         # user-facing CLI implementation
    orchestra.sh                        # user-facing TUI launcher implementation
    gwr/                          # current shell/gwr/* launcher modules
    gw/commands/                  # current shell/git command wrappers and env/tmux CLI commands
    orchestra-command-hook.sh      # sourced into user/tmux shells

cloud/                            # parked feature slice, excluded from stage-1 client/server split
  mod.rs
  auth.rs
  config.rs
  do_api.rs
  models.rs
  remote_worktree_api.rs
  ssh_config.rs
  sync.rs
  sync_builders.rs
  vm_machines.rs
  vm_service.rs
  vm_state.rs
  worktree_meta.rs
  cloud-init.yaml

server/
  services/
    dispatch.sh                   # implementation moved from services.sh
    json.sh                       # JSON response helpers
    git/                          # git operations plus git/worktree/merge JSON service handlers
    session/                      # tmux/session JSON wrappers
    ai.sh                         # AI naming service wrappers
  session/
    tmux_api.sh                   # tmux primitives from api/tmux.sh
    registry_sqlite.sh            # all shell SQLite registry functions from api/tmux.sh
    status_ui.sh                  # tmux status/footer/menu/keybinding helpers from api/tmux.sh
    ai_name.sh                    # AI session naming and AI config loading from api/tmux.sh
    hook_injection.sh             # command hook and services path injection helpers from api/tmux.sh
  env/
    copy.sh                       # services/server env-copy operation wrapper if kept in shell
  core/                           # Rust server-side library crate for direct calls in stage 1
    Cargo.toml
    src/
      lib.rs
      session_registry.rs         # current tui/src/app/session_registry.rs
      config/
      auth/
      updater.rs

api/
  git.sh                          # compatibility shim to server/services/git/api.sh
  tmux.sh                         # compatibility shim/aggregator to server/session/*.sh

orchestra-cli.sh                             # compatibility shim to client/shell/orchestra-cli.sh
orchestra.sh                            # compatibility shim to client/shell/orchestra.sh
services.sh                      # compatibility shim to server/services/dispatch.sh
```

## Server-Side Move Candidates

### Shell Server Files
| Current path | Proposed path | Rationale |
| --- | --- | --- |
| `api/git.sh` | `server/services/git/api.sh` | Pure backend git/worktree API: repo roots, branch detection, worktree list/create/delete, branch delete, primary branch helpers. |
| `services.sh` | `server/services/dispatch.sh` | Current command dispatcher is the server command boundary. Keep root shim. |
| JSON helpers | `server/services/json.sh` | JSON response helpers belong to services/server infrastructure. |
| Tmux service wrappers | `server/services/session/tmux.sh` | JSON wrappers around tmux/session operations. |
| AI service wrappers | `server/services/ai.sh` | Server-side AI naming services. |
| `shell/git/*_api.sh` | `server/services/git/*.sh` | JSON wrappers around git/worktree/merge operations live with the git service capability. |
| `shell/git/worktree.sh`, `shell/git/repo.sh`, `shell/git/merge.sh` | `server/services/git/*.sh` or remove if stale | Aggregators for git service operations. Verify live use before moving because current source targets may be stale/mismatched. |
| `shell/env_copy.sh` | `server/env/copy.sh` | Backend file-copy operation used by services and CLI. If only binary discovery remains here, split discovery to client later. |

### `api/tmux.sh` Split Candidates
`api/tmux.sh` should not move as one final module. It should be split after adding a shim/aggregator because it owns unrelated responsibilities.

| Current responsibility | Proposed path | Notes |
| --- | --- | --- |
| Tmux primitives: availability, create, kill, attach, rename, list, preview, send keys | `server/session/tmux_api.sh` | Core server session lifecycle. |
| SQLite registry path/upsert/rename/remove/sync/query/cycle | `server/session/registry_sqlite.sh` | All shell SQLite functions move here. Includes `_tmux_session_registry_path`, `_tmux_registry_*`, `_tmux_workspace_cycle_target_cached`, and SQL-backed row queries. |
| Workspace session tabs, status line, menus, key bindings | `server/session/status_ui.sh` | Still server/session runtime logic, not TUI client rendering. It runs inside tmux. |
| AI API key loading and session name generation | `server/session/ai_name.sh` | Server service logic that inspects tmux panes and calls AI providers. |
| Command hook path resolution and tmux hook injection | `server/session/hook_injection.sh` | Depends on client hook path but is executed by server-side tmux functions. |

### Rust Server Files
These should move into `server/core` as a Rust library crate. The TUI can depend on this crate by path and continue using direct calls until a real server process exists.

| Current path | Proposed path | Rationale |
| --- | --- | --- |
| `tui/src/app/session_registry.rs` | `server/core/src/session_registry.rs` | Rust SQLite registry via `rusqlite`; must be server-owned. |
| `tui/src/app/config.rs` | `server/core/src/config/user_config.rs` | Persists `~/.orchestra/config.json`; server-side config/state. |
| `tui/src/app/repo_config.rs` | `server/core/src/config/repo_config.rs` | Persists `.orchestra/options.json`; repo state. |
| `tui/src/auth/*` | `server/core/src/auth/*` | Auth service, Clerk API client, and `~/.orchestra/session.json` persistence. |
| `tui/src/updater.rs` | `server/core/src/updater.rs` | Persists update state and executes install tooling. |

### Excluded Cloud Feature Slice
The previous remote VM/cloud work is intentionally omitted from the client/server refactor for now.

| Current path | New path | Stage-1 handling |
| --- | --- | --- |
| `tui/src/cloud/*` | `cloud/*` | Move as a top-level feature slice. Keep `tui` compiling through a path module import, but do not move this code into `client/` or `server/` yet. |

## Client-Side Move Candidates

### Shell Client Files
| Current path | Proposed path | Rationale |
| --- | --- | --- |
| `orchestra.sh` | `client/shell/orchestra.sh` | User-facing TUI launcher. Root file remains shim. |
| `shell/gwr_load.sh` and `shell/gwr_*.sh` | `client/shell/gwr/*.sh` | Launcher helpers: binary discovery, git/services checks, logging, usage, terminal title. |
| `orchestra-cli.sh` | `client/shell/orchestra-cli.sh` | User-facing CLI router. Root file remains shim. |
| `shell/cli_load.sh`, `shell/cli_info.sh`, `shell/cli_err.sh`, `shell/cli_debug.sh`, `shell/cli_have_cmd.sh` | `client/shell/cli/*.sh` | CLI loader/logging helpers. |
| `shell/git/create_worktree.sh` | `client/shell/cli/commands/create_worktree.sh` | CLI parsing/presentation wrapper around server git API. |
| `shell/git/delete_worktree.sh` | `client/shell/cli/commands/delete_worktree.sh` | User-facing command wrapper. |
| `shell/git/list_worktrees.sh` | `client/shell/cli/commands/list_worktrees.sh` | CLI presentation. |
| `shell/git/checkout_worktree.sh` | `client/shell/cli/commands/checkout_worktree.sh` | Emits `cd` for shell wrapper; client behavior. |
| `shell/git/status.sh` | `client/shell/cli/commands/status.sh` | CLI presentation wrapper. |
| `shell/tmux/new_session_command.sh` | `client/shell/cli/commands/new_session.sh` | User CLI path for creating sessions; calls server/session API. |
| `shell/env_copy_command.sh` | `client/shell/cli/commands/env_copy.sh` | User CLI wrapper; server env copy does file work. |
| `shell/orchestra-command-hook.sh` | `client/shell/orchestra-command-hook.sh` | Sourced into user shells/tmux panes; client-side hook. |
| `shell/git/gwr_worktree_title.sh` | `client/shell/gwr/worktree_title.sh` | Terminal/tmux title helper for client launch/session presentation. |
| `shell/orchestra-local.sh` | `client/shell/orchestra-local.sh` | Local developer launcher. Keep root/package compatibility if publicly referenced. |
| `shell/fix_hanging_*.sh` | `client/shell/debug/fix_hanging/*.sh` | Debug/diagnostic scripts, not server business logic. Move only after confirming current usage. |

### Rust Client Files
Move the current `tui/` crate to `client/tui/`. Within that crate, the following modules remain client-owned.

| Current path | Proposed path under `client/tui/src` | Rationale |
| --- | --- | --- |
| `ui.rs` | `ui.rs` or `client/ui.rs` | Ratatui layout orchestration. |
| `theme.rs` | `theme.rs` or `client/theme.rs` | UI palette/style definitions. |
| `components/*` | `components/*` or `client/components/*` | TUI rendering and dialogs. |
| `app/handlers/*` | `app/handlers/*` or `client/app/handlers/*` | Keyboard/controller layer. |
| `app/navigation.rs` | `app/navigation.rs` or `client/app/navigation.rs` | Display tree and selection mapping. |
| `app/session_name.rs` | `app/session_name.rs` or `client/app/session_name.rs` | Display-name parsing/formatting helpers. |
| `app/context.rs` | `app/context.rs` or `client/app/context.rs` | Converts session metadata to displayed context. |
| `app/constants.rs` | `app/constants.rs` or `client/app/constants.rs` | UI timings/constants. |
| `app/feature_flags.rs` | `app/feature_flags.rs` or `client/app/feature_flags.rs` | Client feature gates. |
| `components/help.rs` | `components/help.rs` | Client documentation renderer. |

## Mixed Modules To Split Later
Do not blindly move these whole files in stage 1 unless a compile-focused sub-step extracts only the server calls.

| Current path | Why mixed | Stage-1 handling |
| --- | --- | --- |
| `tui/src/app.rs` | UI state, input helpers, cloud/auth flows, services calls, filesystem/process calls. | Keep in client after moving crate; gradually replace backend imports with `server/core` calls. |
| `tui/src/main.rs` | Terminal bootstrap, CLI `--new-tmux`, update checks, TUI loop. | Keep in client; move update service import to `server/core::updater`. |
| `tui/src/api.rs` | DTOs plus services command adapter. | Split DTOs into server/shared models later. For stage 1, either keep a client adapter or move implementation into `server/core::services_api` and call it directly. |
| `tui/src/app/sessions.rs` | Client async state plus server session calls. | Keep orchestration in client; server owns tmux implementations. |
| `tui/src/app/sessions_remote.rs` | App state plus SSH/remote tmux operations. | Leave remote/cloud behavior out of this refactor. It may continue calling the parked top-level `cloud/` slice until a separate cloud refactor happens. |
| `tui/src/app/session_preview.rs` | Client preview polling plus local/remote preview fetch. | Keep polling in client; move fetchers server-side later. |
| `tui/src/app/git.rs` | Client polling state plus services git status calls. | Keep polling in client; server owns git status implementation. |
| `tui/src/app/deletion.rs` | Client deletion indicators plus worktree/session deletion. | Keep task orchestration in client; server owns delete operations and registry removal. |
| `tui/src/app/merge.rs` | Client dialog/log formatting plus merge/rebase/squash calls. | Keep UI state in client; server owns merge implementation. |
| `tui/src/app/refresh.rs` | Client refresh timers plus repo/session queries. | Keep timers in client; server owns query implementations. |
| `tui/src/components/cloud_dialog.rs` | Renders UI but also starts auth/VM tasks and writes config/commits. | Exclude from stage 1 except for path updates needed to compile against the parked `cloud/` slice. |
| `tui/src/app/worktree_sync_modes.rs` | UI mode state plus persistence implications. | Keep enum visible to client; persistence should move server-side. |

## Compatibility Shims Required
Stage 1 should keep these root paths working:
- `./orchestra.sh` -> delegates to `client/shell/orchestra.sh`.
- `./orchestra-cli.sh` -> delegates to `client/shell/orchestra-cli.sh`.
- `./services.sh` -> delegates to `server/services/dispatch.sh`.
- `./api/git.sh` -> sources `server/services/git/api.sh`.
- `./api/tmux.sh` -> sources/aggregates `server/session/*.sh`.
- `./shell/orchestra-command-hook.sh` -> delegates/sources `client/shell/orchestra-command-hook.sh` or remains as a shim.

Reason: wrappers, tmux key bindings, tests, package installers, and user scripts currently call these paths directly.

## Stage-1 Implementation Sequence

1. Add root folders and shims.
   - Create `client/` and `server/`.
   - Move implementation files behind root shims one layer at a time.
   - Verify public commands still use the same root paths.
   - Status: partially complete. `server/` exists, and `services.sh` plus `api/git.sh` are root compatibility shims.

2. Move shell server ownership first.
   - Move `api/git.sh` implementation to `server/services/git/api.sh`.
   - Move `services.sh` implementation to `server/services/dispatch.sh`.
   - Move services modules into `server/services/*`.
   - Keep `api/git.sh` and `services.sh` root shims.
   - Status: complete for git and services dispatch. `api/tmux.sh` is still in the root API area until the next split step.

3. Split `api/tmux.sh` behind an `api/tmux.sh` shim.
   - Start by moving the full implementation to `server/session/tmux_api.sh` if needed for a safe first commit.
   - Then split SQLite registry functions to `server/session/registry_sqlite.sh`.
   - Then split status UI/keybinding helpers, AI naming, and hook injection.
   - Keep `api/tmux.sh` as the compatibility aggregator while tests are updated.
   - Status: first safe move complete. The full implementation now lives at `server/session/tmux_api.sh`; internal responsibility split is still pending.

4. Move shell client ownership.
   - Move `orchestra.sh` implementation and `shell/gwr_*` modules under `client/shell`.
   - Move `orchestra-cli.sh` implementation and user-facing command wrappers under `client/shell/cli/commands`.
   - Keep root `orchestra-cli.sh` and `orchestra.sh` as shims.

5. Move Rust TUI crate.
   - Move `tui/` to `client/tui/`.
   - Update build scripts, install scripts, tests, and docs that reference `tui/target/...`.
   - Consider adding a root `Cargo.toml` workspace once `server/core` exists.

6. Create Rust `server/core` library crate.
   - Move `tui/src/app/session_registry.rs` first because it is the Rust SQLite registry.
   - Move config/auth/updater persistence modules next.
   - Do not move top-level `cloud/` into `server/core` during stage 1.
   - Update `client/tui` to call `orchestra_server_core::*` directly.
   - Do not introduce a server process yet.

7. Keep mixed Rust app pipelines in client for now.
   - They can call server services directly.
   - Later stages will replace direct calls with a client/server interface.

## Tests And Build Impact

### Tests Most Likely To Change
- `tests/test-workspace-session-cycle.sh`: sources `api/git.sh` and `api/tmux.sh`; calls private `_tmux_registry_*` helpers.
- `tests/test-auto-rename-plain-name.sh`: sources `api/tmux.sh`; calls AI naming and root services rename.
- `tests/test-command-hook-resolution.sh`: sources `api/tmux.sh`; checks services/hook path resolution.
- `tests/test-command-hook-auto-rename.sh`: copies/mocks `services.sh`; sensitive to root shim behavior.
- `tests/test-e2e-env-copy.sh`: uses root `orchestra-cli.sh` and `services.sh`.
- `tests/test-env-copy-rust.sh`: references `tui/target/release/env-copy`.
- `tests/test-tui-simple.sh` and `tests/test-tui-direct.sh`: run `tui/target/release/tui --services-path services.sh`.
- `tests/test-wrappers.sh`: calls `./orchestra-cli.sh` and `./orchestra.sh`.
- Ad-hoc scripts like `tests/test_git_status_accuracy.sh`, `tests/test_rename_api.sh`, `tests/test_rename_final.sh`, and `tests/test_async_deletion.sh` assume flat root paths.

### Reorg Regression Suite
There is no single canonical e2e harness today, but the repo has enough script-level coverage to use as a regression suite for the stage-1 reorg:
- `tests/test-workspace-session-cycle.sh`: tmux workspace sessions, SQLite registry, cycling/status integration.
- `tests/test-auto-rename-plain-name.sh`: AI rename services and tmux session rename path.
- `tests/test-command-hook-resolution.sh`: command hook services path resolution.
- `tests/test-command-hook-auto-rename.sh`: shell hook auto-rename integration.
- `tests/test-e2e-env-copy.sh`: end-to-end `orchestra-cli`/services/env-copy flow.
- `tests/test-create-env-subdir.sh`: CLI worktree creation plus env-copy path behavior.
- `tests/test-env-copy-rust.sh`: Rust `env-copy` binary behavior.
- `tests/test_git_status_direct.sh`: services git status contract.
- `tests/test-tui-simple.sh`: TUI binary starts with services path and help/debug smoke checks.
- `tests/test-wrappers.sh`: root `orchestra-cli.sh`/`orchestra.sh` compatibility.

### Build/Install Files To Update
- `build.sh`
- `shell/build/rust.sh`
- `shell/build/install.sh`
- `shell/build/services.sh`
- `shell/build/dependencies.sh`
- `shell/build/load.sh`
- `shell/build/logging.sh`
- `shell/build/usage.sh`
- `install/scripts/build-release.sh`
- `install/scripts/publish-npm.sh`
- `install/scripts/test-install.sh`
- `install/npm/install.js`
- `install/npm/bin/*.js`
- `install/homebrew/Formula/orchestra.rb`

### Docs To Update After Reorg
- `AGENTS.md`
- `shell/AGENTS.md`
- `tui/src/app/AGENTS.md`
- `cloud/AGENTS.md`
- `plans/COMMANDS.md`
- `tests/README.md`
- `install/AGENTS.md`
- `install/README.md`
- `install/CLOSED-SOURCE-RELEASE.md`
- `install/npm/implementation.md`
- `install/notes.md`
- `tui/README.md`
- Existing `plans/*.md` that reference `tui/`, `api/`, `shell/`, `orchestra-cli.sh`, or `services.sh`.

### Verification Commands
Run after each reorg slice, not only at the end:
- `bash -n orchestra-cli.sh orchestra.sh services.sh api/git.sh api/tmux.sh`
- `cargo test --manifest-path client/tui/Cargo.toml`
- `cargo build --release --manifest-path client/tui/Cargo.toml`
- `tests/test-workspace-session-cycle.sh`
- `tests/test-auto-rename-plain-name.sh`
- `tests/test-command-hook-resolution.sh`
- `tests/test-command-hook-auto-rename.sh`
- `tests/test-wrappers.sh`
- `tests/test_git_status_direct.sh`
- `tests/test-e2e-env-copy.sh`

## Protocol Guidance For Later
For same-machine client/server communication, gRPC is not the best first step unless there is a clear need for generated multi-language clients, strict protobuf contracts, or streaming APIs from day one.

Preferred later path:
1. Define a Rust trait/API boundary first while calls are still in-process.
2. Add a local Unix domain socket server using JSON-RPC or simple HTTP-over-UDS once the boundary stabilizes.
3. Use a repo-scoped socket path under `$XDG_RUNTIME_DIR/orchestra/<repo-hash>.sock` with a lock/pid file and a handshake that returns repo root, server version, and API version.
4. Consider tonic/gRPC over Unix domain sockets only after the API stabilizes or if remote/multi-language clients become a real requirement.

For the requested startup behavior later, the client should preflight the repo-scoped socket, verify the running server owns the same shared git root, and connect if compatible. If no compatible server responds, the launcher starts one and then starts/connects the client.

## Non-Goals For Stage 1
- No background server daemon.
- No socket, HTTP, JSON-RPC, gRPC, or IPC implementation.
- No preflight/autoconnect behavior.
- No behavior changes to git, tmux, SQLite schema, cloud, auth, env copy, or update flows.
- No cleanup of duplicated SQLite implementations beyond moving both into server-owned locations.
- No cloud/remote-VM refactor beyond parking existing `tui/src/cloud/*` code under top-level `cloud/`.

## Open Questions
- Should `server/core` become the canonical implementation for session registry and eventually replace shell/Python SQLite code, or should shell keep registry ownership until the daemon exists?
- Should release artifacts preserve the current flat package layout even after source tree reorg? This is safest for Homebrew/npm compatibility.
- Should a small top-level `shared/` crate/folder be introduced for DTOs, or should `client/tui` import DTOs from `server/core` during stage 1?
- Should `env-copy` stay built from the TUI crate under `client/tui`, or move to a server utility crate/binary?
