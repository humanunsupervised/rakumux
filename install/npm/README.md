# Orchestra

Orchestra keeps all your terminal sessions, in every folder and worktree, in one fast TUI: a session
list on the left, the live terminal on the right. Sessions run in a small background daemon
(`orchestra-ptyd`), so they keep running when you close the TUI. Built for working with coding
agents like Claude Code, Codex and OpenCode in parallel.

## Install

```bash
npm install -g @humanu/orchestra
```

Prebuilt for macOS (Apple Silicon, Intel) and Linux x64. No git, tmux or other runtime
dependencies.

## Usage

```bash
orchestra                   # Open the TUI for the current folder
orchestra new --name dev    # Create a session and attach to it
orchestra ls                # List sessions in this folder
orchestra attach dev        # Full-screen attach (Ctrl-\ d to detach)
orchestra --help            # All commands
orchestra --check-updates   # Check for a newer release
```

## License

Orchestra is proprietary software; see `LICENSE`. `THIRD_PARTY_NOTICES` lists the open-source
components it includes and their licenses.

For release notes, visit https://github.com/humanunsupervised/orchestra
