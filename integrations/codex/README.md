# Codex

Use a Codex version with the documented native hooks framework:

```bash
rakumux session hook-config --agent codex
```

Merge the printed `hooks` entries into `~/.codex/hooks.json` (or `$CODEX_HOME/hooks.json`).
Append handlers to existing event arrays; do not replace other hooks/config. Restart Codex
inside Rakumux, open `/hooks`, and review/trust the new handlers. Do not bypass hook-trust
or tool-approval controls. Local/managed policy can disable hooks.

The command is `"${RAKUMUX_BIN:-rakumux}" session hook --agent codex`. Prompt submission
reports **working**, not a confirmed network request. Native events cover tool execution,
permissions, completion, interruption and actual subagent IDs. A compaction `SessionStart`
changes nothing; hooks carrying `agent_id` only keep that subagent counted. Older versions without hooks
retain screen detection; legacy completion notifications cannot supply full phase/subagent
data. Remove only the Rakumux handlers to disable.

The root hook's `cwd` reports Codex's current/last-used worktree on native events. The daemon
needs `session.working_directory`. Child-agent directories and tool argument paths are not
used as the main thread's worktree; no transcript is parsed to guess one.

- `hooks.rs` / `hooks.json`: native adapter and configuration.
- `detection.rs`: screen fallback.
- `recovery.rs`: root-thread lookup and safe resume flags.

Reference: <https://developers.openai.com/codex/hooks>.
