# Claude Code

```bash
rakumux session hook-config --agent claude
```

Merge the printed `hooks` entries into `~/.claude/settings.json` (or the directory selected by
`CLAUDE_CONFIG_DIR`). Append handlers to existing event arrays; do not replace other hooks
or settings. Restart Claude Code inside a Rakumux session.

The command is `"${RAKUMUX_BIN:-rakumux}" session hook --agent claude`. It reads native JSON
on stdin. Prompt submission reports **working**, not a confirmed network request. Tool hooks
report **running tool**, approval/question hooks **needs input**, and Stop **done**. Subagent
start/stop uses actual `agent_id` values. Child tool/stop hooks do not overwrite the parent
phase. Silence is never interpreted as API waiting. Remove only Rakumux handlers to disable.

The root hook's `cwd` also reports Claude's active worktree. `CwdChanged` updates it without
changing lifecycle status. The daemon needs `session.working_directory`; child hooks cannot
replace the root's worktree. No worktree-creation hook or git operation is installed.

- `hooks.rs` / `hooks.json`: native adapter and configuration.
- `detection.rs`: screen fallback.
- `recovery.rs`: conversation lookup and safe resume flags.

Reference: <https://code.claude.com/docs/en/hooks>.
