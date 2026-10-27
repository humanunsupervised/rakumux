# tui: the `rakumux` binary and the session daemon

This Cargo workspace builds Rakumux's four binaries:

- **`tui`** (installed as `rakumux`): the terminal UI and the CLI subcommands.
- **`rakumux-ptyd`**: the per-user session daemon that owns every session's terminal and the
  session registry.
- **`rakumux-api`**: the local HTTP API and optional remote listener.
- **`rakumux-mcp`**: the MCP server for coding agents.

A **session** is a folder plus a terminal. Exactly one daemon runs per user; any number of TUIs, CLI
calls and agents connect to it at once, and sessions keep running when every TUI is closed. The TUI
shows the sessions of every folder (narrowed with its folder filter); the CLI shows the folder it
was started from. Architecture: [`../AGENTS.md`](../AGENTS.md).

## Workspace

| Package | Path | Builds |
|---|---|---|
| `tui` | `.` (`src/`) | bin `tui`: TUI (`app`, `components`, `terminal_view`, `layout`, `help`), CLI (`cli`), AI naming (`naming`), updater |
| `rakumux-ptyd` | `crates/ptyd` | lib `ptyd` + bin `rakumux-ptyd`: socket server, session control, terminals |
| `rakumux-session-core` | `crates/session-core` | lib `session_core`: session model, wire protocol, frame codec, filesystem layout, client |
| `rakumux-vt` | `crates/vt` | lib `vt`: terminal emulation on `vt100`, query responder, key/mouse encoders |

`tui` depends on `session-core` and `vt`, never on `ptyd`; it reaches the daemon only through
`session_core::client::SessionApi`.

## Prerequisites

- Rust (stable) and a C compiler (bundled SQLite).
- Runtime: nothing else. Rakumux doesn't use git, tmux, jq or python3.
- Linux or macOS.

## Building

```bash
cargo build --workspace              # debug: target/debug/{tui,rakumux-ptyd}
cargo build --workspace --release    # release: target/release/{tui,rakumux-ptyd}
```

Plain `cargo build` in this directory builds only the `tui` package; use `--workspace` (or
`-p rakumux-ptyd`) to get the daemon too.

## Running

Through the launcher (finds both binaries, passes `--ptyd-path`):

```bash
./rakumux.sh                       # from the repo root: TUI for the current folder
./rakumux.sh ls                    # any CLI subcommand
```

Directly, isolated from your real sessions (separate registry, socket and daemon):

```bash
export RAKUMUX_HOME=$(mktemp -d)
target/debug/tui --ptyd-path target/debug/rakumux-ptyd
target/debug/tui --ptyd-path target/debug/rakumux-ptyd new --detach -- bash
target/debug/tui --ptyd-path target/debug/rakumux-ptyd ls --json
target/debug/tui daemon stop
```

The first client that needs the daemon starts it in the background (`rakumux-ptyd serve`, stdio
detached). `rakumux-ptyd serve` also runs it in the foreground for debugging; its log is
`$RAKUMUX_HOME/logs/ptyd.log` (level via `RAKUMUX_PTYD_LOG=debug`).

## CLI

| Command | Purpose |
|---|---|
| `rakumux` | TUI |
| `rakumux new [--name N] [--cwd P] [--cmd C]... [--detach] [-- ARGV...]` | Create a session, then attach |
| `rakumux ls [--json] [--dead] [--folder DIR]... \| --all` | Sessions of the current folder, of the given folders, or of every folder (`--dead` includes dead ones) |
| `rakumux attach <name\|id>` | Full-screen attach; `Ctrl-\ d` detaches |
| `rakumux rename [<name\|id>] <new name>` | Rename (defaults to the current session) |
| `rakumux pin <name\|id>`, `rakumux unpin <name\|id>` | Pin a session first in its folder / unpin it |
| `rakumux kill <name\|id> [--tree]`, `rakumux rm <name\|id>` | Terminate / delete |
| `rakumux folders [--json]`, `rakumux folders add\|rm <dir>` | List folders / add one / forget one without sessions |
| `rakumux --new-server` | Replace the running server (sessions keep running), or start one, then open the TUI |
| `rakumux daemon start\|status\|stop\|restart\|logs` | Daemon management: `restart` keeps sessions running; `stop` ends them |
| `rakumux session list\|get\|read\|wait\|report` | JSON commands for agents and scripts |

Global options before the subcommand: `--ptyd-path <PATH>`, `--debug`. Exit codes: 0 ok, 1 error,
2 usage.

## TUI

```
                                  │ ● api-refactor · working
   ▤ Folders                   ▾  │ $ cargo test
                                  │ running 42 tests ...
                                  │
 rakumux       ⎇ main ── 1/1 live │
   ● api-refactor            2m   │
     claude · ⎇ main · working    │
                                  │
 wt-docs  ⎇ docs ─────── 1/2 live │
   ◐ docs-pass               1h   │
     ⎇ docs · waiting for you     │
                                  │
   ○ old-spike               3d   │
     ⎇ docs · dead · ⏎ revive     │
                                  │
   + New session                  │
 SESSION                                             Ctrl-\ Rakumux 
```

- **Layouts**: Sidebar (above, default) or Tabs (a tab bar of sessions on top, the selected session
  below), or Grid (live terminal cards; not on phones); switch with `v` or in Settings (`,` or
  `o` in Rakumux mode). Every start opens Master detail; the choice isn't saved.
- **Narrow screens and phones** (Sidebar, under 100 columns; e.g. Blink, Termius or a web
  terminal): in Rakumux mode the list sits on top and the selected session below it, so you see
  it while you pick one. The list takes 40% of the height (at least the folder filter and two
  sessions); drag the line between them up or down to change it (saved as
  `sessions.stacked_list_percent`). In session mode the session fills the screen. Under 16 rows only
  the list shows in Rakumux mode. Dialogs keep at least 48 columns (or the screen less two).
- **Selecting** a session shows it live (observer). **Enter** takes control: keys go to the program
  and the terminal is sized to the pane. Other TUIs on the same session keep watching. Ending
  the program from inside it (`exit`, Ctrl-D, quitting the agent) deletes the session; one that
  ends in the background stays, its last screen kept, and Enter restarts it. Programs keep
  your terminal's own colours.
- **Session descriptions** show the app name first, colour-coded by app, then the branch and state.
- **Two modes.** In **session mode** everything you type goes to the program except `Ctrl-\`,
  which switches to **Rakumux mode**. There, `↑/↓`, `j/k` or `Option`/`Alt`+`↑/↓` select (the
  session shows live, in full colour; on the folder filter, a folder header or + New
  session the last session stays in view), `gg`/`G` top/bottom, `Ctrl-d`/`Ctrl-u` half a page,
  `{`/`}` previous/next folder, `←`/`→` switch the list's tabs (Status, Folders, App), `f` the
  Feed (what agents said, in the list's place) and back; `Enter` or
  `i` goes in (reviving a dead session, restarting an
  exited one; on a folder header, a new session there), `Esc` (once it has
  cancelled a pending key or the filter) or `Ctrl-\` goes back to the session you came from (or,
  after a start or a reload, into the session shown),
  `Tab` to the one before, `1`-`9` into that session. `n` new, `r` rename, `p` pin, `J`/`K` move
  the session down/up, `x` close, `d`/`dd` delete, `z` full screen, `P` folders, `/` filter, `\`
  sends `Ctrl-\` to the session, `?` Help, `,` or `o` Settings, `R` restart the UI, `S` restart the server, `q` quit. The footer is a
  full-width bar that shows the mode, in zoom too, with `session: <name>` in its centre:
  orange in Rakumux mode (with the `f ≡ Feed`, `? Help` and `, Settings` buttons on the right;
  `RAKUMUX FEED` while the Feed shows), muted blue in
  session mode (with a `Ctrl-\ Rakumux` button). Help and Settings list the keys, and
  Settings runs any of them.
- **Folders**: a session belongs to the folder it was started in (a repo, a worktree, any
  folder). The list shows every folder, live: sessions another window, the CLI or an agent start
  in any folder (a new worktree too) appear as they start. Each folder shown has a header row
  (name, branch, live/total), even when it's the only one. Inside a folder, sessions working in
  a linked git worktree (one inside the repo, like `worktrees/feature`, or elsewhere) get a
  header of their own for that worktree, and follow their agent between worktrees. Enter on a
  worktree's header starts a new session in it.
- **Order**: the list never reorders by itself; using a session doesn't move it. The folder you
  started in leads, then the others by name. A new session goes last in its folder, and pinned
  sessions come first. Drag a session (or a tab) up and down to move it, or press `J`/`K`; it
  stays in its folder, among the pinned sessions or the others. The order is saved as
  `sessions.session_order` in `~/.rakumux/config.json`. Under the Status and App tabs each
  status and app keeps an order of its own (`sessions.section_order`): moves there reorder a
  session within its group, across folders, without touching the folders' order.
- **New session** (`n`, or "+ New session"): asks only for a name (leave it blank and one is
  picked). The session goes in the selected session's or header's folder, shown under the name.
  Moving to "+ New session" or the folder filter keeps that folder. A single-folder filter
  takes precedence; without folder context, the launch folder is used. The session runs your
  login shell there. For another folder or a command, use
  `rakumux new --cwd DIR --cmd C -- ARGV`.
- **Restart the UI**: press `R` in Rakumux mode, or run Restart Rakumux from Settings.
  A confirmation explains that sessions stay running: `y`/Enter restarts, `n`/Esc cancels.
  This reloads the UI through its launcher, picking up the latest local build. The daemon and
  session programs keep running. `Ctrl-r` only reconnects or reloads the list.
- **Restart the server**: press `S` in Rakumux mode, or Restart server in Settings (it shows the
  running server's version next to this build's), or run `rakumux daemon restart`. After you
  confirm, the server restarts in place with this build: sessions and their programs keep
  running, and every window reconnects.
- **Folder filter** (`▤ Folders ▾` at the top; `P`, or Enter on it): the Folders modal lists
  All folders and every folder with sessions. Type to filter; `Enter` shows the folder under the
  cursor alone (All folders: every folder); `Tab`/`Space` tick several, then `Enter` shows them
  together. Type a folder path (`/`, `~`, `.`, or with a `/` in it) to add a folder and show it:
  the folders that complete it are listed as you type (dot-folders once you type the `.`).
  `↑`/`↓` choose one, `Tab` (or `→` at the end) completes the path with it and adds a `/` so its
  folders come next, and `Enter` adds it. A folder that doesn't exist is created after you
  confirm with `y`. The filter isn't saved: every start shows every folder.
- **Welcome tour**: a six-card carousel on first start (folders and sessions, the two modes,
  moving around, the main shortcuts, Help and Settings). Every card has "Don't show this on
  startup" (Space; `sessions.welcome_seen`), so you can opt out at any point; until it's ticked
  the tour shows on every start. `t` in Help replays it.
- **Icons** (Settings): the marks for folders and branches, Plain (`▤`, `⎇`, any font),
  Emoji (📁, 🌿) or Nerd Font icons, whichever your terminal shows. A TUI can't pick the
  terminal's font, so the choice is yours.
- **Option/Alt+↑/↓** go to the program inside a session unless Settings, "Option+↑/↓ in
  sessions" is on (OpenCode, Pi and Claude Code use them). Terminal.app needs "Use Option as Meta
  key".
- Mouse: click a session to show it, click the session pane to go in, drag a session to move
  it, drag the line beside (or under) the list to resize it. Click the mode badge: SESSION
  switches to Rakumux mode, RAKUMUX goes back to the session you came from. Click a footer
  button (`? Help`, `, Settings`, `Ctrl-\ Rakumux`) to do what it says. Phone terminals send taps
  as clicks, so you can get around without `Ctrl-\`. The wheel moves through Settings and
  scrolls Help. Text selection: Shift+drag (the TUI
  captures the mouse).

## Development

```bash
cargo test --workspace               # or: cargo test -p tui / -p rakumux-ptyd / ...
cargo fmt --all --check
cargo clippy --workspace
```

- Tests must not touch the real `~/.rakumux`: set `RAKUMUX_HOME` to a temp dir.
- Program UI and CLI code against `SessionApi` and test it with fakes (`AttachStream::pair`,
  `EventStream::from_receiver`); test the daemon's control layer with a fake `TerminalHost`.
- Emulator fidelity spike and throughput check:

  ```bash
  cargo run --release -p rakumux-vt --example spike -- 80 24 'w:500|t:ls\r|w:300' -- bash --norc
  cargo run --release -p rakumux-vt --example throughput -- some-output.log
  ```

- Where to change what: the TUI's state and keys in `src/app` (see `src/app/AGENTS.md`), rendering
  in `src/ui.rs` and `src/components`, the live pane in `src/terminal_view`, layouts in
  `src/layout`, the Settings and Help modals in `src/help`, CLI in `src/cli`, the wire protocol in
  `crates/session-core/src/api.rs` (additive changes only), daemon behaviour in `crates/ptyd`.
