# shell/ – Launcher, build and hook scripts

Rakumux's logic is Rust (`tui/`): the `rakumux-ptyd` daemon and the `rakumux` binary (TUI +
CLI). The scripts here only launch the binaries, set the terminal title, check for updates, build,
run a local checkout, and log shell command history for AI naming. There is no shell services
layer: no `services.sh`, no git or tmux helpers, no `rakumux-cli`, no env-copy, and the scripts
need neither `jq` nor `python3`. See `AGENTS.md` at the repo root for the architecture.

## Directory structure

```
shell/
├── gwr_load.sh                # loader sourced by rakumux.sh (gwr_load_core)
├── gwr_colors.sh              # ANSI colour variables for launcher output
├── gwr_logging.sh             # gwr_info / gwr_success / gwr_warn / gwr_error (to stderr)
├── gwr_binary.sh              # binary discovery + silent update-check scheduling
├── gwr_terminal.sh            # terminal title
├── gwr_usage.sh               # `rakumux --help` text
├── gwr/
│   └── check-updates.sh       # npm / Homebrew update check (`rakumux --check-updates`)
├── build/                     # build.sh modules
│   ├── load.sh                # loads the build modules
│   ├── dependencies.sh        # toolchain checks
│   ├── rust.sh                # cargo build of the workspace (tui, ptyd, api, mcp)
│   ├── install.sh             # optional global install
│   ├── logging.sh             # build output helpers
│   └── usage.sh               # build.sh --help
├── rakumux-local.sh         # run rakumux.sh from a local checkout
└── rakumux-command-hook.sh  # command history hook for AI session naming
```

## Launcher (`gwr_*`, used by `rakumux.sh`)

`rakumux.sh` sources `gwr_load.sh` and calls `gwr_load_core`, then:

1. `-h`/`--help` → `gwr_show_usage` (`gwr_usage.sh`); `--check-updates`/`--update` →
   `gwr/check-updates.sh`.
2. `gwr_find_binary` (`gwr_binary.sh`) finds the `tui` binary: `GW_TUI_BIN`, then `rakumux` or
   `tui` next to the launcher (packaged layouts), then the release and debug builds in the checkout,
   then `PATH`. The daemon binary is resolved the same way
   (`GW_PTYD_BIN` override) and passed as `--ptyd-path`; `RAKUMUX_BIN` is exported so the daemon
   can tell sessions where the launcher is.
3. `gwr_set_terminal_title` (`gwr_terminal.sh`) titles the terminal `rakumux: /<folder>` after the
   current folder.
4. `gwr_check_updates_silent` runs the update check in the background with stdio detached, at most
   once a day (`gwr_should_check_updates`, timestamp in `~/.cache/rakumux/last_update_check`).
5. `exec` the binary with every argument passed through; the binary decides between TUI and CLI
   subcommands.

Rakumux runs in any folder; the launcher performs no git or services checks.

### check-updates.sh

- `detect_installation_method`: npm, Homebrew or local checkout.
- `get_npm_latest_version`, `get_brew_latest_version`, `version_compare`, `check_updates`.
- Prints upgrade instructions for the detected method. The TUI's own updater
  (`tui/src/updater.rs`) handles the in-app upgrade dialog.

## Build (`build/`, used by `build.sh`)

`build.sh` sources `build/load.sh`, checks dependencies (Cargo and a C compiler for the bundled
SQLite), builds the Cargo workspace in `tui/`
(release or `--debug`), and optionally installs globally (`--install`). The workspace produces four
binaries: `tui` and `rakumux-ptyd` (both needed to run Rakumux), plus `rakumux-api` (the
external HTTP API, `api/`) and `rakumux-mcp` (the MCP server for agents, `mcp/`), which ship next
to them. Release packaging lives in `install/scripts/` (see `install/AGENTS.md`).

## rakumux-local.sh

Runs a local checkout's `rakumux.sh` (often aliased as `rakumux-local`):

- Default root `$HOME/repos/rakumux`; override with `RAKUMUX_LOCAL_ROOT` or `--root`.
- `--worktree <name>` runs `<root>/worktrees/<name>/rakumux.sh` (a second checkout of the
  Rakumux repo for development; Rakumux itself does not manage worktrees).
- Remaining arguments pass through (`rakumux-local -- --debug`).

## rakumux-command-hook.sh

Sourced from the user's shell profile (bash `DEBUG` trap / zsh `preexec`). Inside a Rakumux
session (`RAKUMUX_SESSION_ID` set) it appends each command to
`~/.rakumux/history/<session id>.log` (`$RAKUMUX_HOME` respected); outside a session it does
nothing. The TUI reads this file, together with the visible screen, to suggest a session name
(`tui/src/naming`). It also defines `rakumux_rename_session <name>`, which runs
`"${RAKUMUX_BIN:-rakumux}" rename "<name>"` for the current session.

## Conventions

- Bash, `# shellcheck shell=bash` in sourced modules; `set -euo pipefail` in entry scripts.
- Launcher log output goes to stderr (`gwr_*` logging), so stdout stays clean for the binary.
- Anything started in the background must detach stdin/stdout/stderr, or the documented shell
  wrapper that captures Rakumux's output can hang.
- Keep scripts small. New behaviour belongs in Rust unless it is about finding or launching the
  binaries.
- Check changed scripts with `bash -n` (and `shellcheck` where available).
