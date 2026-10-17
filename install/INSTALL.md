# Installing Orchestra

Orchestra runs on macOS (Apple Silicon or Intel) and Linux x64. It needs only bash: no git, tmux
or other tools.

## npm

```bash
npm install -g @humanu/orchestra
```

Requires Node.js 14 or newer. The package contains prebuilt binaries for every supported platform.

## Homebrew (macOS and Linux)

```bash
brew install humanunsupervised/orchestra/orchestra
```

Update with `brew upgrade orchestra`.

## Release tarball

Download the package for your platform from
[GitHub Releases](https://github.com/humanunsupervised/orchestra/releases):

| Platform | Package |
|---|---|
| macOS Apple Silicon | `orchestra-macos-arm64.tar.gz` |
| macOS Intel | `orchestra-macos-intel.tar.gz` |
| Linux x64 | `orchestra-linux-x64.tar.gz` |

```bash
curl -L https://github.com/humanunsupervised/orchestra/releases/latest/download/orchestra-macos-arm64.tar.gz | tar xz
cd orchestra-macos-arm64
./install.sh                          # /usr/local (asks for sudo if needed)
PREFIX="$HOME/.local" ./install.sh    # or any other prefix
```

`install.sh` puts the package in `$PREFIX/libexec/orchestra` and the `orchestra` and
`orchestra-local` commands in `$PREFIX/bin`. Checksums are in `checksums.txt` on the release.

## From source

```bash
./build.sh            # builds tui, orchestra-ptyd, orchestra-api and orchestra-mcp (release)
./orchestra.sh        # run from the checkout
```

`./build.sh --install` also installs the binaries into `~/.cargo/bin`; `shell/orchestra-local.sh`
(alias it as `orchestra-local`) runs a checkout from any folder.

## First run

```bash
cd ~/projects/my-app
orchestra                  # TUI for this folder
orchestra new --name dev   # or create a session from the command line
orchestra --help
```

Sessions run in the `orchestra-ptyd` daemon, which starts on demand and keeps your sessions
running after you quit the TUI. There is one per user on a machine: Orchestra connects to the
running one from any login (SSH, mosh, a phone) and starts it only when none runs.
`orchestra daemon start|status|stop|restart|logs` manages it; `orchestra --new-server` replaces it
before opening the TUI. A restart keeps sessions running. After a crash or a reboot, the sessions
that were running come back by themselves: Claude Code, Codex and OpenCode in the same
conversation, other programs with their command typed at the prompt.

## Coding agents (MCP)

Every install also ships `orchestra-mcp`, an MCP server that lets coding agents list, create, read
and rename your sessions, and `orchestra-api`, the local HTTP API it uses (started on demand,
127.0.0.1 only). For Claude Code:

```bash
claude mcp add --scope user orchestra -- orchestra-mcp
```

Codex (`~/.codex/config.toml`; forwarding the session variables lets an agent running inside an
Orchestra session act as that session):

```toml
[mcp_servers.orchestra]
command = "orchestra-mcp"
env_vars = ["ORCHESTRA_SESSION_ID", "ORCHESTRA_SESSION_TOKEN"]
```

OpenCode (`~/.config/opencode/opencode.json`):

```json
{ "mcp": { "orchestra": { "type": "local", "command": ["orchestra-mcp"], "enabled": true } } }
```

Terminal output the tools return is marked untrusted, and agents running inside a session can only
see their own folder and rename themselves.

## Upgrading from 0.5.x

0.6.0 replaces tmux and git worktrees with Orchestra's own sessions:

- **Remove the old shell functions.** Earlier versions asked you to add `orchestra()` and
  `orchestra-cli()` functions to `~/.bashrc` / `~/.zshrc` for directory switching. Delete them:
  `orchestra-cli` no longer exists, and the old `orchestra()` function sends subcommands such as
  `orchestra ls` to it. Then open a new shell (or `unset -f orchestra orchestra-cli`).
- tmux sessions from 0.5.x are not imported; they keep running in tmux until you close them.
- The `GW_ENV_COPY_BIN` and `GW_ORCHESTRATOR_ROOT` variables are no longer used.
- After a later upgrade, `orchestra daemon restart` (or the TUI's offer) switches to the new
  daemon in place: running sessions keep running. A daemon from a 0.6 preview build can't hand
  its sessions over; restarting it ends them once (Enter brings one back).

## Uninstall

```bash
npm uninstall -g @humanu/orchestra
brew uninstall orchestra
sudo rm -rf /usr/local/libexec/orchestra /usr/local/bin/orchestra /usr/local/bin/orchestra-local  # install.sh
```

Your sessions and settings live in `~/.orchestra`; remove it to start fresh.

## Troubleshooting

| Message | Fix |
|---|---|
| `orchestra binary (tui) not found` | Reinstall; in a checkout run `./build.sh`. A stale `GW_TUI_BIN` in your environment is ignored when it doesn't point at an executable. |
| `orchestra-ptyd (session daemon) not found` | Reinstall; in a checkout run `./build.sh` (it builds every binary). |
| `orchestra: command not found` after npm install | Add npm's global bin directory (`npm prefix -g`/bin) to `PATH`. |
| Sessions misbehave | `orchestra daemon logs`, then `orchestra daemon restart` (sessions keep running). |

## License

Orchestra is proprietary software; see `LICENSE` in the package (Homebrew:
`$(brew --prefix)/share/doc/orchestra/LICENSE`). `THIRD_PARTY_NOTICES` lists the open-source
components it includes and their licenses.
