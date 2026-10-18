# Rakumux

Rakumux keeps all your terminal sessions, in every folder and worktree, in one fast TUI: a session
list on the left, the live terminal on the right. Sessions run in a small background daemon
(`rakumux-ptyd`), so they keep running when you close the TUI. Built for working with coding
agents like Claude Code, Codex and OpenCode in parallel.

## Install

```bash
npm install -g rakumux
```

Prebuilt for macOS (Apple Silicon, Intel) and Linux x64. No git, tmux or other runtime
dependencies.

## Usage

```bash
rakumux                   # Open the TUI for the current folder
rakumux new --name dev    # Create a session and attach to it
rakumux ls                # List sessions in this folder
rakumux attach dev        # Full-screen attach (Ctrl-\ d to detach)
rakumux --help            # All commands
rakumux --check-updates   # Check for a newer release
```

## License

Rakumux is licensed under Apache-2.0; see `LICENSE`. `THIRD_PARTY_NOTICES` lists the open-source
components it includes and their licenses.

For release notes, visit https://github.com/humanunsupervised/rakumux
