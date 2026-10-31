# Claude Code

```bash
rakumux session hook-config --agent claude
```

Merge the printed `hooks` entries into `~/.claude/settings.json` (or the directory selected by
`CLAUDE_CONFIG_DIR`). Append handlers to existing event arrays; do not replace other hooks
or settings. Restart Claude Code inside a Rakumux session.

The command is `"${RAKUMUX_BIN:-rakumux}" session hook --agent claude`. It reads native JSON
on stdin. Prompt submission reports **working**, not a confirmed network request. Tool hooks
report **running tool**, approval/question/MCP-elicitation hooks **needs input**, and Stop
**done**. A Stop whose `background_tasks` lists running subagents or workflows ("Waiting for
N background agents to finish") reports **waiting for agents**, which is working until they
wake the conversation again, however idle its screen looks meanwhile.
`StopFailure` (an API error ended the turn) and the `idle_prompt` notification (a minute after
a finished or Esc-interrupted turn) report **idle**, keeping an unread reply unread; a
compaction `SessionStart` changes nothing. Subagent start/stop uses actual `agent_id` values;
a subagent's tool and permission hooks keep it counted without replacing the parent's phase.
Silence is never interpreted as API waiting. Remove only Rakumux handlers to disable.

An Esc interrupt fires no hook. The daemon ends such a turn when Claude's own screen (its
✳ title or idle prompt) has looked idle for 5 seconds after the last hook.

The root hook's `cwd` also reports Claude's active worktree. `CwdChanged` updates it without
changing lifecycle status. The daemon needs `session.working_directory`; child hooks cannot
replace the root's worktree. No worktree-creation hook or git operation is installed.

A question it asks in its `AskUserQuestion` dialog can be answered from rakumux.com
(`questions.rs`, the daemon's `session_answer`): the daemon checks that the conversation's newest
`AskUserQuestion` call without a result is the one answered and that the screen shows its dialog,
then works the dialog: `down`/`up` to an option and `enter` for one choice; `space` on each option
of a `multiSelect` question and `enter` on `Next` (`Submit` on the last); the free answer typed on
its `Type something.` row; then, for several questions or a `multiSelect` one, `enter` on the
review's `1. Submit answers` once it lists the answers given. It never chooses `Chat about this`
and stops at any screen it doesn't expect. Checked against Claude Code 2.1.314, whose screens
are `fixtures/questions/`.

- `hooks.rs` / `hooks.json`: native adapter and configuration.
- `detection.rs`: screen fallback.
- `recovery.rs`: conversation lookup and safe resume flags.
- `questions.rs`: its questions in the log, and its dialog worked from the screen.

Reference: <https://code.claude.com/docs/en/hooks>.
