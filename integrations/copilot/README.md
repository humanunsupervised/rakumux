# GitHub Copilot CLI

Rakumux recognises GitHub Copilot CLI (`copilot`, npm `@github/copilot`, Homebrew
`copilot-cli` or GitHub's install script) in any session. Its hooks make the session list
exact; everything else works with nothing installed.

## Setup

```bash
rakumux session hook-config --agent copilot --install
```

This writes one file of Rakumux's own, `~/.copilot/hooks/rakumux.json` (`$COPILOT_HOME/hooks/`),
which Copilot loads beside your own hook files; nothing else is changed, and running it again
replaces only that file. Restart Copilot to load it. Without `--install` the file is printed
instead. To remove it, delete that file.

Every command in it is `"${RAKUMUX_BIN:-rakumux}" session hook --agent copilot --event <event>
|| true`. Copilot denies a tool when its `preToolUse` hook fails, so the `|| true` keeps tools
working on a machine without `rakumux` (or with an older one); keep it if you copy the file
elsewhere. Outside a Rakumux session the hooks report nothing.

To let Copilot drive sessions through MCP, see [mcp/README.md](../../mcp/README.md#github-copilot-cli).

## What works

- **Detection.** The npm loader (`node …/bin/copilot`), the native binary (on Linux its process
  is named `MainThread`; it's found by its path) and the title `GitHub Copilot`. Its approval and
  folder-trust dialogs and `ask_user` questions are blocked, `esc esc interrupt`/`… edit prompt`
  is working, its input box idle. Inside Rakumux, Copilot draws its input box with half-block bars
  rather than rules; both are recognised.
- **State from hooks.** A prompt is working, a tool call running a tool, a permission prompt or
  `ask_user` needs input, `agentStop` done, a model error idle. Subagents are counted by their id
  (`subagentStart`/`subagentStop`), and their own tool hooks keep them counted. The root's `cwd`
  is where it works (it follows `/cd` at the next hook). A repository's Claude Code hooks
  (`.claude/settings.json`), which Copilot also runs, report as Copilot.
- **The conversation on screen.** One Copilot can hold several conversations open (`/clear`,
  `/new`, its sessions sidebar). The one on screen is the one its process log last registered
  (`~/.copilot/logs/process-<ms>-<pid>.log`, `Registering foreground session: <id>`); without that
  log (`--log-level none`), the one it holds open (`session-state/<id>/inuse.<pid>.lock`), its
  hooks', then `--resume`. Hooks from a conversation it holds open but doesn't show are left out,
  and switching away from one takes its turn in progress and unread reply along.
- **Resume.** `copilot <flags> --resume=<id>`, keeping the flags that shape a session
  (permissions, model, tools, MCP, `--add-dir`, …) and dropping a prompt, `--continue`, `-w`,
  `-C` and the rest. Copilot goes back to the conversation's own folder. A folder it wasn't told
  to remember asks for trust again.
- **Latest line, prompt, turn and reply.** From
  `~/.copilot/session-state/<id>/events.jsonl`: the agent's reasoning, tool calls
  (`Shell: cargo test`, `Edit: src/app.rs`) and messages; what you typed (`user.message` without
  a `source`; Copilot's own injections, subagents' tasks and autopilot continuations aren't); the
  reply is the text after its last tool call, keyed by `messageId`.
- **Whole conversation.** One message per event, with the event's id: your prompts, the
  assistant's reasoning, text and tool calls, and the tools' results (`tool`). Subagents, hook
  records and system prompts are left out; reasoning logged both as an event and on its message
  counts once.
- **Places.** The session's folder as it starts, resumes and `/cd`s, its tools' paths and shell
  commands, and the files its tools changed (`fileEdits`, what was written, not what was asked).
- **Replies written back** are typed in with a bracketed paste, which Copilot takes as one prompt
  (a long one shows as `[Paste #1 - N lines]` and is kept whole), and confirmed by the next turn's
  prompt.
- **New sessions.** Rakumux keeps Copilot's markers (`COPILOT_CLI`, `COPILOT_AGENT_SESSION_ID`, …)
  out of new sessions, and from a session started by Copilot's shell tool its git settings too
  (`GIT_EDITOR=true`, no credential prompts, its `GIT_CONFIG_*` entries).

## Limits

- Copilot rewrites its event log when a turn is taken back (`/undo`, or Esc Esc on a prompt it
  hasn't answered): it's read again from the start, and a taken-back prompt already sent to the
  feed stays there.
- The process log, the `inuse` locks and `open-sessions-state.json` aren't documented; the event
  log is (its JSON Schema ships with the package). Copilot updates itself almost daily; if they
  change, the session falls back to its hooks and its screen.
- Copilot's sandbox may keep hooks from reaching the daemon's socket; the screen still works.
- `agentStop` comes while background agents still run (Copilot has no `waiting for agents`
  state), and an Esc Esc interrupt fires no hook: the screen ends that turn.
- `COPILOT_HOME` is read from the daemon's environment, as `CLAUDE_CONFIG_DIR` and `CODEX_HOME`
  are; `--log-dir` is followed when it's absolute.

## Files

- `detection.rs`: screen and process profile.
- `hooks.rs` / `hooks.json`: hook adapter and Rakumux's hook file.
- `recovery.rs`: which conversation is on screen, and resume flags.
- `output.rs`, `transcript.rs`, `places.rs`: the event log.
- `environment.rs`, `presentation.rs`: markers kept out of new sessions, and its colour.

## Tests

```bash
cd tui
cargo test -p rakumux-ptyd copilot --locked              # detection, resume, output, places, transcript
cargo test -p rakumux-ptyd --test control_copilot --locked
cargo test -p tui copilot --locked                        # hooks and the installer
cargo test -p rakumux-sensagent copilot --locked          # a reply written back
```

The end-to-end test runs the real `copilot` in a session of the real daemon, offline
(`COPILOT_OFFLINE`) against a scripted model it serves itself, with no account: detection,
folder trust, working/needs input/done/idle from the hooks, the line, prompt, turn and reply,
places, the whole conversation, a reply typed in with `session_submit`, a second chat from the
sidebar finishing in the background without marking the session done, and resume after a kill.

```bash
cargo build --workspace
cargo test -p rakumux-ptyd --test runtime_copilot --locked -- --ignored --nocapture
```

Set `COPILOT_CACHE_HOME` to reuse Copilot's unpacked package between runs (about 240 MB), and
`COPILOT_E2E_SCREENS=<dir>` to save the screens it sees.

References: <https://docs.github.com/en/copilot/reference/hooks-reference>,
<https://docs.github.com/en/copilot/reference/copilot-cli-reference/cli-command-reference>,
<https://docs.github.com/en/copilot/reference/copilot-cli-reference/cli-config-dir-reference>.
