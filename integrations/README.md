# Agent integrations

```text
integrations/
  open-code/     # V2 CLI plugin, detection, recovery, installer, tests
  claude-code/   # Native command hooks, detection, recovery
  codex/         # Native command hooks, detection, recovery
  detection.rs   # Shared process/screen engine and retained older profiles
  recovery.rs    # Shared resume dispatch and safe argument/file helpers
  hooks.rs       # Adapter dispatch and metadata-only normalization
  environment.rs / presentation.rs / mcp.rs  # Shared dispatch to provider metadata
```

Agent-specific runtime code, inherited environment markers and colours belong in its agent
folder. Rust `#[path]` modules mount
detection/recovery into the daemon, hook adapters into the CLI, and the OpenCode installer
into MCP. Existing public Rust module paths are unchanged; there are no new cross-layer
dependencies. Policy stays in `tui/crates/ptyd/src/control/`; wire types stay in
`tui/crates/session-core/src/activity.rs`. Clients never parse provider-specific events.
Older detection profiles remain supported; new hooks start with these three agents.

## Setup

- [OpenCode](open-code/README.md)
- [Claude Code](claude-code/README.md)
- [Codex](codex/README.md)

Hooks require a daemon advertising `session.activity`. They are opt-in and are not installed
automatically. No API key, hosted service or model request is involved.

## Active worktree and branch

With `session.working_directory`, the same self-only report can include
`working_directory: {agent, conversation_id?, path}`. The daemon validates and canonicalizes
the path, then reads that directory's `.git` files for the session's branch/worktree. It
retains the last report while the same agent process is running. No report changes the
session's folder permissions, terminal cwd or another session.

- OpenCode uses the displayed session's `location.directory` and `subpath`, not the plugin's
  original launch location. A worktree switch is sent even if its activity stays idle.
- Claude Code and Codex use the root hook input's `cwd`, never tool arguments or background
  subagent directories. Claude's `CwdChanged` hook updates context without changing activity.
- Without an active agent, git context follows the terminal cwd. The daemon polls quiet
  foreground processes and releases stale OSC 7 authority after a process/directory change.

Context survives an in-place daemon restart but is not a new session or a permission grant.
Agent UI reloads may be needed after installing integrations; Codex hooks must be reviewed
and trusted in `/hooks`. No adapter creates or switches a git worktree, runs `git`, or reads
prompt/transcript text to guess a directory. Without agent metadata, the terminal cwd is the
fallback; Rakumux does not guess from another conversation's most-recent directory.

## Common activity protocol, version 1

Integrations use the existing self-only `session_report` request's optional `activity` field:

```json
{
  "version": 1,
  "agent": "opencode",
  "conversation_id": "ses_example",
  "event": "snapshot",
  "phase": "running_tool",
  "subagents": ["ses_child"]
}
```

Events:

- `phase`: supplies `phase`. `idle`/`done` clear previous subagent counts.
- `subagent`: supplies `id` and `running`; repeated starts/stops are idempotent.
- `snapshot`: supplies `phase` and the complete set of active `subagents`.

An optional `completion_id` identifies the completed reply in `done` updates. Repeated
snapshots with that identifier refresh activity without making an opened reply unread again.
Integrations without reply identifiers use phase transitions; duplicate `done` updates do
not make a seen reply unread until another phase starts a new turn.

Phases: `working`, `waiting_for_response`, `running_tool`, `needs_input`, `idle`, `done`.
The daemon validates versions/events, bounds identifiers and active subagents (64), and
timestamps receipt. `SessionView.activity` carries phase/count to all clients through existing
`updated`/`lifecycle` events. Ordinary self-report text is untouched. Activity is ephemeral,
and a newer fresh explicit lifecycle report wins until a later hook update. Hook state is
carried through restart-in-place, cleared on detected agent changes/terminal exit, and expires after
120 seconds without updates. Screen/process detection remains the fallback. Command hooks
have no heartbeat: long quiet operations may fall back to detection before they finish.

Hooks fired inside a subagent (their input carries `agent_id`) never replace the parent's
phase. Their tool and permission hooks are that subagent's heartbeat (`subagent`, running):
a long foreground or background subagent stays counted, and its parent's activity fresh,
while the parent itself is silent; a stopped subagent is never counted again. While
hook-reported subagents run, activity stays fresh for 10 minutes without updates (one of
their tool calls can take minutes); snapshots keep the 120-second limit. A busy state
from command-hook deltas (not snapshots) that the same agent's screen has shown idle since
after the latest hook, for 5 seconds, is dropped: that turn ended without a hook (Claude
Code's Esc fires no Stop). Waiting-for-input and finished phases are never second-guessed.
Compaction (`SessionStart` with `source: compact`, including idle compaction) neither starts
nor ends a turn.

The TUI animates busy states locally. Needs-input/done markers are static. Agent-internal
subagents are not Rakumux child sessions. These adapters forward metadata only, never
prompts, tool arguments, output or transcripts. Native input is capped at 1 MiB and never
logged. Outside a Rakumux session hooks do nothing and never spawn a daemon. Errors emit a
generic diagnostic and empty JSON with success; activity cannot approve/deny/continue work.

## Session badges

Live agents have a right-side activity mark in the list, tabs and Grid. In the list it
follows the right-aligned age:

- Spinner: working, including active subagents.
- `○`: idle, with no unread completion.
- `●`: the latest turn finished and has not been opened (`Lifecycle::Done`).
- `◐`: waiting for input or permission, distinct from a completed reply.

Active subagents also show as a count after the status in the session's description, the
detail's title and Grid cards: `◈ 3` (🤖 or a Nerd Font robot with those icon sets). It's the
subagents only; the session's own agent is the row itself.

Opening the session marks a completed reply seen for every client. Passive list/Grid
previews never do. Replies arriving in an open, focused conversation at the bottom of its
history are marked seen; a background window, dialog or focused utility shell does not
acknowledge them. Terminal focus reporting is needed to distinguish background windows.
Read state is owned by the daemon, not inferred separately by each UI. Compatible daemon
handoff preserves acknowledgements; activity remains ephemeral across a crash or reboot.

## Verification

```bash
cd tui
cargo test -p rakumux-ptyd --test control_activity --locked
cargo test -p rakumux-ptyd detect:: --locked
cargo test -p rakumux-ptyd resume:: --locked
cargo test -p tui hook --locked
cargo test -p rakumux-mcp --test opencode_plugin --locked
node --test '../integrations/open-code/activity.test.mjs'
```

Fixtures/fake clients do not replace real-agent or native macOS verification. Verify those
separately; Linux tests are not evidence of native macOS compatibility.
