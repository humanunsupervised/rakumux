# Kimi Code

Kimi Code CLI 2.x (`kimi`, MoonshotAI's TypeScript rewrite; the archived Python `kimi-cli` 1.x
is detected only). Most of it works with nothing installed; the Rakumux plugin adds its MCP
server and tells the session exactly which conversation Kimi shows.

## Setup

```bash
rakumux-mcp kimi-plugin --install
```

It installs the plugin in Kimi Code's home (`$KIMI_CODE_HOME`, else `~/.kimi-code`; `--dir`
overrides) as Kimi's own `/plugins install <dir>` would: `plugins/managed/rakumux/kimi.plugin.json`,
recorded in `plugins/installed.json` beside your other plugins. Restart Kimi Code (or run
`/plugins reload`). `rakumux-mcp kimi-plugin` prints the manifest instead: save it as
`kimi.plugin.json` in a folder and run `/plugins install <that folder>` to review it in Kimi
first. To remove it, `/plugins remove rakumux`.

The plugin holds:

- **Hooks** on `SessionStart`, `UserPromptSubmit` and `TurnStarted` running
  `"${RAKUMUX_BIN:-rakumux}" session hook --agent kimi`. They report the main agent's
  conversation (`session_id`) and folder (`cwd`) as the session's working directory, never a
  phase, a prompt or tool input. Outside Rakumux they do nothing.
- **MCP**: `rakumux-mcp` from `PATH` (Kimi refuses an absolute path in a plugin). It inherits
  Kimi's environment, `RAKUMUX_SESSION_ID` and `RAKUMUX_SESSION_TOKEN` included, so its tools act
  as the session. Kimi names them `mcp__plugin-rakumux_rakumux__<tool>`; in Always Ask mode each
  call asks ("Approve for this session" stops asking for that tool).

Kimi needs `kimi` on `PATH` for a revive to resume it: the official script installs
`~/.kimi-code/bin/kimi` and adds that folder to your shell's profile (unless
`KIMI_NO_MODIFY_PATH=1`).

## What works

- **Detection** (`detection.rs`): the native binary retitles itself `kimi-code`, cut to the room
  its arguments had (`kimi` started bare stays `kimi`, `kimi -c` shows as `kimi-co`), and wipes
  its arguments; its executable (`~/.kimi-code/bin/kimi`) names it whatever the title says. npm
  (`@moonshot-ai/kimi-code`) and the legacy Python CLI are matched by package and name. The
  screen covers a Kimi started before its terminal answered OSC 7501: approval, question and
  trust panels (read only between the two rules that replace the input box), the braille
  `Working…` line and the moon frames of a running tool, and the `context:` line when idle.
- **Lifecycle** with nothing installed: Kimi reports `idle`, `working`, `blocked`
  (`permission`, `question`, `auth`), `done` and its subagents (child records) through OSC 7501
  ([Program status](../README.md#program-status-osc-7501)). Its hooks never set a phase: a
  subagent's `Stop` carries the main agent's `session_id` and nothing else.
- **Conversation** (`recovery.rs`): the one the plugin's hooks named, else the one the process
  holds a lock on while a turn runs (`locks/sessions/<id>.lock`, its pid or a child's, as after a
  staged update), else the one followed, else the folder's newest that isn't archived
  (`sessions/wd_<slug>_<hash>/<id>/state.json`). Checked on every read, so `/new` and
  `/sessions` move it.
- **Resume**: `kimi -r <session id>`, as Kimi prints on exit. The conversation keeps its
  permission mode, plan mode, model and agent; `--add-dir`, `--skills-dir` and `--agent-file`
  are lost (Kimi wipes its arguments).
- **Lines, turn, reply, chat** (`output.rs`, `transcript.rs`) from the main agent's log,
  `sessions/…/<id>/agents/main/wire.jsonl`: prompts and steers whose origin is the user's (Kimi's
  reminders and hook results left out), reasoning, text and tool calls (`Bash: <description>`,
  `Write: src/app.rs`). The reply is the text after the last tool call of a turn that completed
  (a cancelled or failed turn has none), keyed by its part's `uuid`. The chat is one message per
  record at its byte offset; tool results are the `tool` role's. Subagents' logs are never read.
- **Places** (`places.rs`): Write and Edit's files, `file_history.tracked` (what the main agent
  or a subagent is about to change), Read's files, Grep and Glob's roots, and Bash's working
  directories and `cd`/`git -C`.
- **Replies written back** are pasted and entered (`session_submit`, expecting `kimi`,
  `kimi-code` or the cut title the session shows). Typed during a turn, Kimi queues it and logs
  it as a prompt when that turn ends.
- **Environment** (`environment.rs`): sessions never inherit `KIMI_PLUGIN_ROOT`, which Kimi sets
  for its plugins' hooks and MCP servers.

## Limits

- Kimi writes a step's text when the step ends, so `agent_output` doesn't move during one long
  streamed reply; the lifecycle does.
- Without the plugin, two Kimis idle in one folder may show each other's conversation until one
  runs a turn (its lock says which), and resume names the folder's newest.
- Only the daemon's `KIMI_CODE_HOME` (else `~/.kimi-code`) is read; a Kimi started with another
  home isn't followed.
- A reply starting with `!` runs a shell command in Kimi, and `/` a slash command: typed as
  written, as for every agent.
- `/undo` and compaction leave the log's earlier records; chats already uploaded keep them.

## Verification

```bash
cd tui
cargo test -p rakumux-ptyd detect:: --locked
cargo test -p rakumux-ptyd resume::kimi --locked
cargo test -p rakumux-ptyd agent_output:: --locked
cargo test -p rakumux-ptyd --test control_kimi --locked
cargo test -p tui hook --locked
cargo test -p rakumux-mcp kimi --locked
```

`control_kimi` runs Kimi Code's log format and screens through the daemon (lines, turn, reply,
chat, places, resume, a reply typed in, a cancelled turn, the lock and folder fallbacks).
Fixtures were captured from Kimi Code 2.4.0 against a local mock model.

The real CLI was checked end to end, offline (it needs the installed `kimi`, so it isn't in CI):
a throwaway daemon (`RAKUMUX_HOME`), an isolated `HOME` and `KIMI_CODE_HOME`, the plugin
installed with `rakumux-mcp kimi-plugin --install`, and an OpenAI-compatible mock model on
`127.0.0.1` (`KIMI_MODEL_PROVIDER_TYPE=openai`, `KIMI_MODEL_BASE_URL`, `KIMI_MODEL_NAME`,
`KIMI_MODEL_API_KEY`, `KIMI_MODEL_REASONING_KEY=reasoning_content`) scripted to think, reply,
write a file, run a command, start a subagent, ask a question and call Rakumux's MCP tools. It
showed the trust panel blocked; a command's approval blocked, then working, then done; a
subagent counted while it ran; `agent_output`, `user_prompt`, `agent_turn` and `session_turn`;
`session_transcript`; one reply captured per completed turn, at its turn's last message; the
checkout as an edited place; the hooks naming the conversation (`SessionStart`,
`UserPromptSubmit`, `TurnStarted`, a subagent's turn left out); `list_sessions` from inside Kimi
answering "this is you"; a reply typed in with `session_submit` showing up as a new turn; `/new`
followed and resumed; and after a kill, a revive running `kimi -r <id>` into the same chat.

References: <https://www.kimi.com/code/docs/en/kimi-code-cli/customization/plugins.html>,
`…/customization/hooks.html`, `…/customization/mcp.html`, `…/reference/kimi-command.html`.
