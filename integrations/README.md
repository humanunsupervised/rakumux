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

The TUI animates busy states locally. Needs-input/done markers are static. Agent-internal
subagents are not Rakumux child sessions. These adapters forward metadata only, never
prompts, tool arguments, output or transcripts. Native input is capped at 1 MiB and never
logged. Outside a Rakumux session hooks do nothing and never spawn a daemon. Errors emit a
generic diagnostic and empty JSON with success; activity cannot approve/deny/continue work.

## Session badges

Live agents have a right-side activity badge in the list, tabs and Grid. The list
uses a wider, padded badge immediately before the right-aligned age:

- Spinner: working, including active subagents.
- `○`: idle, with no unread completion.
- `●`: the latest turn finished and has not been opened (`Lifecycle::Done`).
- `◐`: waiting for input or permission, distinct from a completed reply.

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
