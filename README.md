# Rakumux

**A terminal multiplexer for coding agents.**

[rakumux.com](https://rakumux.com) · [Docs](https://rakumux.com/docs) · [Releases](https://github.com/humanunsupervised/rakumux/releases) · [Pricing](https://rakumux.com/pricing)

楽 (*raku*) means easy, and read *gaku* it means music: tmux, made easy.

![Rakumux: the session list and a live Claude Code session](assets/rakumux-screenshot.png)

Run Claude Code, Codex and OpenCode side by side, on your own subscriptions, along with any other
program. Rakumux shows which agent is working, which needs you and which has finished. Your
sessions keep running when you close the window, lose SSH or reboot.

- **Every session in one list.** Group it by status, folder or app, with live previews. Pin,
  reorder and filter your sessions.
- **Your agent's native keys.** In a session every key goes to the program. A single key,
  `Ctrl-\`, switches to Rakumux.
- **Sessions outlive the UI.** One background server owns every terminal. After a crash or a
  reboot, agents resume their conversation.
- **Answer from anywhere** (optional free account). Read your agents' replies on rakumux.com in
  any browser, including your phone's, and write back. No VPN or open port.
- **Scriptable.** A CLI, a local HTTP API and an MCP server, so agents can drive sessions too.

Free and open source (Apache-2.0), for macOS and Linux. No tmux, git or VPN required.

## Install

```bash
curl -fsSL https://rakumux.com/install.sh | bash
```

The installer:

- picks the build for your machine (macOS Apple Silicon, macOS Intel or Linux x64);
- checks the download against the release's SHA-256 checksums;
- installs into `/usr/local`, asking for `sudo` if it needs to.

To install into your home directory without `sudo`, run the following, then make sure
`~/.local/bin` is on your `PATH`:

```bash
curl -fsSL https://rakumux.com/install.sh | PREFIX="$HOME/.local" bash
```

This gives you three commands:

- `rakumux`: the app and the CLI;
- `rakumux-mcp`: the MCP server for agents;
- `rakumux-api`: the local HTTP API.

The session server, `rakumux-ptyd`, is installed alongside them. Rakumux needs nothing else at
runtime besides bash.

**Update:** press `U` in Rakumux mode, or re-run the installer. Running sessions stay up.

For release tarballs, uninstalling and troubleshooting, see the
[installation guide](install/INSTALL.md).

## Quick start

```bash
cd ~/code/my-app
rakumux
```

1. Press `n` to start a session. You're in its shell, so run `claude`, `codex`, `opencode` or
   anything else.
2. Press `Ctrl-\` to switch to Rakumux mode. The footer turns orange.
3. Use `↑`/`↓` to pick a session, `Enter` to go back in, and `n` to start another.
4. Press `q` to quit. Your sessions keep running, and `rakumux` brings them back.

### Two modes, one key

| Mode | Footer | Keys go to | Switch to it |
|---|---|---|---|
| Session | blue | the program, so your agent's shortcuts all work | `Enter` on a session |
| Rakumux | orange | Rakumux | `Ctrl-\` |

Rakumux claims only one key, so it never overrides your agent's shortcuts. To send `Ctrl-\`
itself to the program, press `\` in Rakumux mode. To use another key, set
`sessions.rakumux_key` in `~/.rakumux/config.json`.

### Rakumux mode keys

| Key | Action |
|---|---|
| `↑` `↓` or `j` `k` | Select a session |
| `Enter` | Go into it (also restarts or revives an ended one) |
| `Esc` | Back to the session you came from |
| `Tab` / `1`–`9` | Last session / that session |
| `n` | New session in the current folder |
| `←` `→` | Switch tabs: Feed, Status, Folders, App |
| `P` / `/` | Filter by folder / by text |
| `p` | Pin or unpin |
| `J` `K` | Move down or up (or drag the session) |
| `r` | Rename |
| `x` / `d` | Close (end its program) / delete |
| `v` | Layout: Master detail, Grid, Tabs |
| `T` | Terminal panel: a scratch shell under the session |
| `z` | Full screen |
| `?` / `,` | Help / Settings (every key, searchable) |
| `U` | Update |
| `q` | Quit (sessions keep running) |

The mouse works too: click to select, drag to reorder, and drag the divider to resize.

## How sessions work

A session is a folder and a terminal. `n` starts one in the current folder, and from the CLI
`rakumux new --cwd DIR` starts one anywhere.

### The list

Tabs across the top of the list group your sessions:

- **Status**: waiting for you, unread, working, waiting for response, running, exited, dead, then
  ready (read, newest first).
- **Folders**: by folder, with each folder's git branch. Sessions in another linked worktree go
  under it.
- **App**: by agent or program.
- **Feed**: your agents' replies, newest first (needs an account).

Pinned sessions sit above every group.

### Status

Rakumux reads each agent's status from its screen. The [activity hooks](#coding-agents) make it
exact.

An ended session keeps its last screen:

- **exited**: its program ended. `Enter` restarts it.
- **dead**: its terminal is gone, for example after `rakumux daemon stop`. `Enter` revives it,
  resuming the agent's conversation.

### The server

One server per user, `rakumux-ptyd`, owns every terminal. It starts when first needed and keeps
running with no window open. Any number of windows can connect at once, from any login: SSH, mosh
or a phone.

- **Restart:** `rakumux daemon restart` swaps in a new version without stopping your programs.
- **Recovery:** after a crash or a reboot, the sessions that were running come back by
  themselves. Claude Code, Codex and OpenCode return in the same conversation. Other programs
  have their command typed at the prompt.

## Command line

```bash
rakumux new --name dev                          # create a session and attach
rakumux new --detach --name web -- npm run dev  # start a program in the background
rakumux ls --all                                # sessions of every folder
rakumux attach dev                              # full screen; Ctrl-\ then d detaches
rakumux rename dev api
rakumux pin dev
rakumux kill dev
rakumux rm dev
rakumux daemon status                           # also start, stop, restart, logs
rakumux --help
```

`rakumux ls` lists only the current folder's sessions; `--all` lists every folder's.

For scripts and agents, `rakumux session list|get|read|wait|report` print JSON.

## Coding agents

Rakumux runs any agent as it is. Two extras are optional.

### Activity hooks

The hooks report exactly when an agent is working, needs input or is done. They also report its
subagents and the worktree it's working in.

```bash
rakumux session hook-config --agent claude  # merge the printed hooks into ~/.claude/settings.json
rakumux session hook-config --agent codex   # merge into ~/.codex/hooks.json, then trust them in /hooks
rakumux-mcp opencode-plugin --install       # OpenCode
```

For details, see the [agent integrations guide](integrations/README.md).

### MCP server

The MCP server lets an agent list, create, read and rename sessions in its own folder.

```bash
claude mcp add --scope user rakumux -- rakumux-mcp
```

For Codex, OpenCode and session identity, see the [MCP guide](mcp/README.md).

## Rakumux account (optional)

Everything above works without an account.

**Sign in** with `a` in Rakumux mode, or `rakumux account login`. Then turn features on under
**Settings → Available on sign-in**.

**AI session summaries** add a short description under each agent session's name. Press `s` to
refresh one.

**Agent feed** collects your agents' replies and chats from every signed-in machine:

- in Rakumux's Feed tab;
- at [rakumux.com/feed](https://rakumux.com/feed), with your sessions by status, folder and app.

**Writing back:** you can answer an agent from the web. Rakumux types your message into the
session once the agent is waiting for you. Each machine takes replies written elsewhere only with
**Replies from rakumux.com** turned on in its Settings.

**Privacy:** your machine only ever calls out to rakumux.com. Nothing calls in. Before uploading,
Rakumux masks text that looks like a key, a credential or a token.

The free plan covers all of this. See [pricing](https://rakumux.com/pricing) and the
[privacy policy](https://rakumux.com/privacy).

## From your phone

- **Browser:** open rakumux.com in your phone's browser for the feed, your sessions and writing
  back.
- **SSH:** the app fits phone-width terminals. Tap a session to go in. `Ctrl-\`, or a tap on the
  `SESSION` badge, goes back to the list.
- **iPhone and Android apps (coming soon):** the iPhone app connects over
  [Tailscale](https://tailscale.com). Run `rakumux remote on`, then `rakumux remote pair` to show
  its QR code.

## Configuration and data

- **Data:** sessions, saved output, logs and `config.json` live in `~/.rakumux`.
- **Separate instance:** set `RAKUMUX_HOME` to another directory to run a fully separate
  instance, with its own server and sessions.
- **Settings** (`,`): layout, icons, session-switching keys, your account and its features.
- **Session names:** Rakumux names sessions with AI if you provide your own Anthropic or OpenAI
  key, through `ANTHROPIC_API_KEY`, `OPENAI_API_KEY` or `config.json`.
- **Logs:** `rakumux daemon logs` shows the server's log.

## Build from source

You need stable Rust, a C compiler and bash. SQLite is bundled.

```bash
git clone https://github.com/humanunsupervised/rakumux.git
cd rakumux
./build.sh       # release build of all four binaries
./rakumux.sh     # run from the checkout
```

To try it without touching your real sessions:

```bash
RAKUMUX_HOME=$(mktemp -d) ./rakumux.sh
```

After rebuilding:

- press `R` to reload the UI;
- press `S` to restart the server in place. Your sessions keep running.

To run the tests:

```bash
cd tui
cargo test --workspace --locked
```

Read [CONTRIBUTING.md](CONTRIBUTING.md) for the full checks, and [AGENTS.md](AGENTS.md) for the
architecture.

## More

- [Installation guide](install/INSTALL.md): tarballs, upgrading, uninstalling, troubleshooting
- [Agent integrations](integrations/README.md): hooks, status, worktrees
- [MCP server](mcp/README.md): tools for coding agents
- [HTTP API](api/README.md): local automation and remote access
- [Security](SECURITY.md): reporting a vulnerability privately

## License

Copyright 2024–2026 Antonio de Perio and Human Unsupervised Pty Ltd.

Rakumux is licensed under [Apache-2.0](LICENSE). Dependencies keep their own licenses, and binary
releases include `THIRD_PARTY_NOTICES`. The license grants no rights to Rakumux trademarks.

The mobile app is maintained separately in `humanunsupervised/rakumux-mobile`. Its source is not
part of this repository or its license grant.
