# Rakumux

A terminal multiplexer for coding agents. Rakumux keeps all your terminal sessions, in every
folder and worktree, in one fast TUI: a session list on the left, the live terminal on the right.
Sessions run in a small background daemon (`rakumux-ptyd`), so they keep running when you close
the TUI, and Rakumux shows which agent is working, blocked or done. Built for running Claude Code,
Codex and OpenCode side by side.

楽 (*raku*) means easy, and read *gaku* it means music: tmux, made easy.

**Status:** pre-release. 0.1.0 isn't published yet; build it from source in the meantime.

## Build from source

Needs a Rust toolchain (stable) and bash. Linux and macOS.

```bash
./build.sh                      # or: cd tui && cargo build --workspace --release
./rakumux.sh                    # run this checkout
```

The binaries land in `tui/target/release/`: `tui` (the `rakumux` TUI and CLI), `rakumux-ptyd`
(the session daemon), `rakumux-api` (local HTTP API) and `rakumux-mcp` (MCP server for agents).

## Usage

```bash
rakumux                   # Open the TUI
rakumux new --name dev    # Create a session and attach to it
rakumux ls                # List sessions in this folder
rakumux attach dev        # Full-screen attach (Ctrl-\ d to detach)
rakumux --help            # All commands
```

`AGENTS.md` describes the architecture, `api/README.md` the HTTP API and `mcp/README.md` the MCP
server.

## Licence

Rakumux is going to be open source; the licence is still being chosen. Until it's added, see
`LICENSE`.

Website: <https://rakumux.com>
