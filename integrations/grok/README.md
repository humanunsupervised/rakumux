# Grok Build

xAI's coding agent CLI, `grok` (checked with 1.0.70). Not the community `grok-cli`.

```bash
rakumux session hook-config --agent grok --install
```

That writes `~/.grok/hooks/rakumux.json` (or `$GROK_HOME/hooks/rakumux.json`), a file of
Rakumux's own: Grok loads every `*.json` in its hooks folder, so nothing is merged and other
hooks stay as they are. Without `--install` it prints the configuration. Grok runs your hooks
only in folders you trusted (its "Do you trust the contents of this directory?" question, or
`/hooks-trust`), so it asks in each new folder. Restart Grok inside a Rakumux session. Remove
the file to disable.

The command is `"${RAKUMUX_BIN:-rakumux}" session hook --agent grok`. Prompt submission and
finished tools report **working**, a tool starting **running tool**, its question tool and
permission prompts **needs input**, and `Stop` **done**, with its `promptId` as the reply's
id. A `Stop` whose `backgroundTasks` lists running subagents or workflows reports **waiting
for agents**; the turn Grok starts when they finish is reported as any other. `StopFailure`,
`StopCancelled` and the `idle_prompt` notification report **idle**; the `Stop` Grok fires for
each session at exit reports nothing.

Subagents run in sessions of their own. `SubagentStart` (in the parent) counts one, by the
id its own session's hooks then carry; those hooks keep it counted, and its `SubagentStop`
or `SessionEnd` ends it. They never change the parent's phase or working directory.

Grok also runs Claude Code's hooks (`~/.claude/settings.json`, a repository's
`.claude/settings.json`). Those report as Grok too, so with Claude Code's hooks installed
Grok needs nothing more; installing both sends the same reports twice, harmlessly.
`[compat.claude] hooks = false` in `~/.grok/config.toml` turns that off.

Without hooks, Grok reports what it does through OSC 7501 (`state=working|blocked|done`,
`kind=question`, a child record per running subagent), which the daemon reads with nothing
installed; screen detection covers the rest. Hooks add the conversation's id (which session
of a process is on screen, and the reply's id) and Grok's working directory.

An interrupt while it waits for its model (Ctrl+C) fires no hook: the daemon ends the turn
when Grok's screen has looked idle for 5 seconds after the last one, or on its OSC 7501
report.

## What Rakumux reads

- **Which conversation:** `$GROK_HOME/active_sessions.json` lists the sessions each Grok
  process holds (`/new`, forks and the Agent Dashboard add more). The one on screen is the
  one its hooks named, else the one whose log changed last.
- **Lines, turn, reply and whole conversation:** that session's
  `sessions/<folder>/<id>/updates.jsonl`, read as it grows: what you typed (prompts Grok
  wakes itself with left out), its thinking, replies and tool calls, and tools' results.
- **Places:** the files its finished edits changed, its reads and searches, where its shell
  calls ran and what they `cd`'d into, and the same in each finished subagent's own log.
- **Resume:** `grok <flags> --resume <id>`, keeping permissions, model, effort, sandbox,
  agent, rules, tools and allow/deny rules; dropping a prompt, `-p`, `-c`, `--worktree` and
  the like. Started as `agent`, it resumes as `grok`.
- **Replies written back** are typed in as a bracketed paste and Enter, which Grok takes as
  one prompt (`expect_foreground: grok`); a draft left in its input box gets the reply
  appended.

## MCP

```bash
grok mcp add rakumux -- rakumux-mcp
```

Grok starts MCP servers with its own environment, so `rakumux-mcp` gets both
`RAKUMUX_SESSION_ID` and `RAKUMUX_SESSION_TOKEN` and acts as the session. A server added to
Claude Code (`~/.claude.json`, `.mcp.json`) is loaded by Grok too.

## Limits

- `Ctrl+\` opens Grok's Agent Dashboard, but it's Rakumux's key: type `/dashboard`, or press
  `\` in Rakumux mode to send it.
- Started as `agent` (Grok installs that name, and so does Cursor's CLI), Grok is recognised
  by the file it runs.
- Switching sessions in the Agent Dashboard without typing keeps the earlier session's lines
  until something is written there, or its hooks name it.
- A subagent's edits count once it finished; while it runs, only the parent's do.
- Leader mode (`--leader`) and Grok's daemon route were not tested.

## Checking it

Unit tests use synthetic fixtures of Grok's formats; `tui/crates/ptyd/tests/grok_follower.rs`
follows a synthetic home through a session switch. Against the real CLI, without a sign-in:
give Grok an isolated `HOME` whose `~/.grok/config.toml` names a fake OpenAI-compatible model
on 127.0.0.1 (`[model.fake] base_url = "http://127.0.0.1:PORT/v1"`, `[models] default =
"fake"`, `[auth] auth_provider_command = "echo fake"`, `[features] telemetry = "off"`), run
it in a throwaway daemon (`RAKUMUX_HOME` in a temp folder) in a network namespace with only
loopback, and drive it through the daemon's socket.

Reference: <https://docs.x.ai/build/overview>, <https://docs.x.ai/build/features/hooks>.
