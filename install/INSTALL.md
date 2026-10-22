# Installing Rakumux

Native binaries are available for macOS Intel, macOS Apple Silicon and Linux x64.
The installed app needs only bash: no git, tmux or other tools.

## Installer

```bash
curl -fsSL https://rakumux.com/install.sh | bash
```

The installer checks the archive against the release's `checksums.txt`, then runs its installer.
Use `PREFIX="$HOME/.local" bash` in place of `bash` for a user-local installation.
It automatically selects the package for your operating system and architecture.

Re-run the same installer to upgrade. npm and Homebrew publishing are not enabled; use the
binary installer or a release tarball.

## Release tarball

Download the package for your platform from
[GitHub Releases](https://github.com/humanunsupervised/rakumux/releases):

| Platform | Package | Availability |
|---|---|---|
| Linux x64 | `rakumux-linux-x64.tar.gz` | Published |
| macOS Apple Silicon | `rakumux-macos-arm64.tar.gz` | Published |
| macOS Intel | `rakumux-macos-intel.tar.gz` | Published |

```bash
curl -fL https://github.com/humanunsupervised/rakumux/releases/latest/download/rakumux-linux-x64.tar.gz | tar xz
cd rakumux-linux-x64
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

Press `Shift+U` in Rakumux mode (`Ctrl-\` first when working in a session), or open
Settings → Update. Rakumux checks the installed release channel. `Enter` confirms
installation and automatically restarts **only this UI**. `Esc` cancels before installation
starts. A failed download, checksum, package, version or permission check leaves the UI running.

Binary installs download the official native release and its published SHA-256 checksums over
HTTPS. All four binaries and launcher files are staged and checked, then the complete install
is swapped atomically. Running programs keep their old mapped images. Unknown/custom files
cause a refusal rather than being discarded. Settings and session data are not replaced.

Automatic binary updates need write permission on the install directory's parent. User-local
installs (`PREFIX="$HOME/.local"`) normally support this. Admin-owned installs need the installer
outside Rakumux with the original `PREFIX`; there is no hidden `sudo` prompt. Source builds are
never overwritten: rebuild the checkout, then press `R` to reload the UI.

Cached availability never authorizes installation on startup. The launcher `--update` option
is a check, not this in-app installation workflow. The daemon and API are not restarted by
Update. To apply a new daemon later, use `rakumux daemon restart` for a supported restart in
place. Decline any warning that it would end sessions. Update has no stop/revive fallback.

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
