# Installing Rakumux

Rakumux runs on macOS (Apple Silicon or Intel) and Linux x64. It needs only bash: no git, tmux
or other tools.

**Pre-release:** build from source until Rakumux release assets and packages are available.

## Installer

```bash
curl -fsSL https://rakumux.com/install.sh | bash
```

The installer checks the archive against the release's `checksums.txt`, then runs its installer.
Use `PREFIX="$HOME/.local" bash` in place of `bash` for a user-local installation.

## npm

```bash
npm install -g rakumux
```

Requires Node.js 14 or newer. The package contains prebuilt binaries for every supported platform.

## Homebrew (macOS and Linux)

```bash
brew install humanunsupervised/rakumux/rakumux
```

Update with `brew upgrade rakumux`.

## Release tarball

Download the package for your platform from
[GitHub Releases](https://github.com/humanunsupervised/rakumux/releases):

| Platform | Package |
|---|---|
| macOS Apple Silicon | `rakumux-macos-arm64.tar.gz` |
| macOS Intel | `rakumux-macos-intel.tar.gz` |
| Linux x64 | `rakumux-linux-x64.tar.gz` |

```bash
curl -L https://github.com/humanunsupervised/rakumux/releases/latest/download/rakumux-macos-arm64.tar.gz | tar xz
cd rakumux-macos-arm64
./install.sh                          # /usr/local (asks for sudo if needed)
PREFIX="$HOME/.local" ./install.sh    # or any other prefix
```

`install.sh` puts the package in `$PREFIX/libexec/rakumux` and the `rakumux` and
`rakumux-local` commands in `$PREFIX/bin`. Checksums are in `checksums.txt` on the release.

## From source

```bash
./build.sh            # builds tui, rakumux-ptyd, rakumux-api and rakumux-mcp (release)
./rakumux.sh        # run from the checkout
```

`./build.sh --install` also installs the binaries into `~/.cargo/bin`; `shell/rakumux-local.sh`
(alias it as `rakumux-local`) runs a checkout from any folder.

## First run

```bash
cd ~/projects/my-app
rakumux                  # TUI for this folder
rakumux new --name dev   # or create a session from the command line
rakumux --help
```

Sessions run in the `rakumux-ptyd` daemon, which starts on demand and keeps your sessions
running after you quit the TUI. There is one per user on a machine: Rakumux connects to the
running one from any login (SSH, mosh, a phone) and starts it only when none runs.
`rakumux daemon start|status|stop|restart|logs` manages it; `rakumux --new-server` replaces it
before opening the TUI. A restart keeps sessions running. After a crash or a reboot, the sessions
that were running come back by themselves: Claude Code, Codex and OpenCode in the same
conversation, other programs with their command typed at the prompt.

## Coding agents (MCP)

Every install also ships `rakumux-mcp`, an MCP server that lets coding agents list, create, read
and rename your sessions, and `rakumux-api`, the local HTTP API it uses (started on demand,
127.0.0.1 only). For Claude Code:

```bash
claude mcp add --scope user rakumux -- rakumux-mcp
```

Codex (`~/.codex/config.toml`; forwarding the session variables lets an agent running inside a
Rakumux session act as that session):

```toml
[mcp_servers.rakumux]
command = "rakumux-mcp"
env_vars = ["RAKUMUX_SESSION_ID", "RAKUMUX_SESSION_TOKEN"]
```

OpenCode (`~/.config/opencode/opencode.json`):

```json
{ "mcp": { "rakumux": { "type": "local", "command": ["rakumux-mcp"], "enabled": true } } }
```

Terminal output the tools return is marked untrusted, and agents running inside a session can only
see their own folder and rename themselves.

## Upgrading

Update through the channel you installed from, then use `rakumux daemon restart` (or the TUI's
offer) to switch to the new daemon in place. Running sessions keep running when the daemon
supports restart-in-place handoff. The UI asks before a restart that would end sessions.

## Uninstall

```bash
npm uninstall -g rakumux
brew uninstall rakumux
sudo rm -rf /usr/local/libexec/rakumux /usr/local/bin/rakumux /usr/local/bin/rakumux-local  # install.sh
```

Your sessions and settings live in `~/.rakumux`; remove it to start fresh.

## Troubleshooting

| Message | Fix |
|---|---|
| `rakumux binary (tui) not found` | Reinstall; in a checkout run `./build.sh`. A stale `GW_TUI_BIN` in your environment is ignored when it doesn't point at an executable. |
| `rakumux-ptyd (session daemon) not found` | Reinstall; in a checkout run `./build.sh` (it builds every binary). |
| `rakumux: command not found` after npm install | Add npm's global bin directory (`npm prefix -g`/bin) to `PATH`. |
| Sessions misbehave | `rakumux daemon logs`, then `rakumux daemon restart` (sessions keep running). |

## License

Rakumux is licensed under Apache-2.0; see `LICENSE` in the package (Homebrew:
`$(brew --prefix)/share/doc/rakumux/LICENSE`). `THIRD_PARTY_NOTICES` lists the open-source
components it includes and their licenses.
