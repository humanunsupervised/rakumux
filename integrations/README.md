# Agent integrations

```text
integrations/
  open-code/     # V2 CLI plugin, detection, recovery, installer, tests
  claude-code/   # Native command hooks, detection, recovery
  codex/         # Native command hooks, detection, recovery
  pi/            # Extension and installer, detection, recovery, end-to-end test
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
- [Pi](pi/README.md)

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

## Hook transport

`rakumux session hook --agent <agent> [--event <name>]` reads one hook's JSON on stdin and
always answers `{}` with exit 0. Agents: `claude`, `codex`, `opencode`, `copilot`,
`cursor-agent`, `pi`, `grok`, `kimi`, `droid`, `amp`; one without an adapter yet reports
nothing. `--event` names the hook's event for agents whose input doesn't (GitHub Copilot's
camelCase hooks); adapters read it as `hookEventName`.

Cursor, GitHub Copilot and Grok also run Claude Code's hooks (the user's or the repository's
`.claude/settings.json`). Fired there, `--agent claude` hooks report as that agent, never as
Claude Code: Cursor's input carries `cursor_version`, Grok's its own `hookEventName` next to
Claude's `hook_event_name` (or `GROK_HOOK_EVENT` is set), and GitHub Copilot sets
`COPILOT_CLI=1`. Sessions never inherit those two variables (`environment.rs`), so a daemon
started from inside one of those agents doesn't pass them on.

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

Phases: `working`, `waiting_for_response`, `running_tool`, `waiting_for_agents` (the agent's
turn paused until its background agents finish: working, with its screen waiting for input),
`needs_input`, `idle`, `done`.
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
while the parent itself is silent; a stopped subagent is never counted again. Agents whose
subagents run hooks under their own conversation id (Droid, Grok) need no adapter rule: an
update from the same agent whose conversation is a subagent the activity already counted
(running or stopped) is that subagent's heartbeat, in the daemon. A busy or waiting phase
keeps it running, `done` or `idle` stops it; it never changes the parent's phase, completion,
conversation, the session's agent identity or its working directory. While
hook-reported subagents run, activity stays fresh for 10 minutes without updates (one of
their tool calls can take minutes), and so does `waiting_for_agents`; snapshots keep the
120-second limit. The agent's own busy state (`working`, `waiting_for_response`,
`running_tool`) from command-hook deltas (not snapshots) that the same agent's screen has shown
idle since after the latest hook, for 5 seconds, is dropped with its subagents: that turn
ended without a hook (Claude Code's Esc fires no Stop). Waiting-for-input, waiting-for-agents
and finished phases, and subagents still running after their parent's turn, are never
second-guessed: the parent's screen waits for input while they work.
An agent whose hooks report no approval prompts (Cursor: `preToolUse` fires before the prompt
it may draw, and nothing after) sets `approvals_on_screen` in its detection profile: while its
screen shows a prompt (`Blocked`), that wins over its hooks' busy state. Claude Code, Codex and
OpenCode report their prompts in hooks (`needs_input`), so their hooks keep winning.
Compaction (`SessionStart` with `source: compact`, including idle compaction) neither starts
nor ends a turn.

The TUI animates busy states locally. Needs-input/done markers are static. Agent-internal
subagents are not Rakumux child sessions. These hook adapters forward metadata only, never
prompts, tool arguments, output or transcripts.

## Program status (OSC 7501)

The daemon's terminal answers the Program Status Protocol's support query (`OSC 7501;?`,
<https://www.superlogical.com/rex/docs/build/program-status>) with the same body, so programs that
speak it (Pi from 1.1, Kimi Code 2.x) report what they do, with nothing installed:
`state=idle|working|blocked|done|error|clear`, `kind` for `blocked`, `id` for a child record (Kimi
Code's subagents). The daemon keeps each session's records in memory (`control/program_status.rs`)
and never their texts (`msg`, `title`). Without fresh hook activity, and while detection names an
agent, they set the lifecycle: `working` and children working or blocked are working, `blocked`
waits for the user, `idle`, `done` and `error` are idle, and a `done` after anything else is a
finished reply. They show as activity too (`working`, `needs_input`, `done`, `idle`, or
`waiting_for_agents` while only children work), children counted as subagents. They rank above
the screen and below fresh hook activity and a newer self-report, never expire (the protocol has
no heartbeat), and end with the program's own `clear` or when detection names no agent. A restart
in place drops them until the program's next report; a program started before its daemon
answered the query never asked, and reports nothing until it's restarted.

## Latest output and prompt

`output.rs` gives each live Claude Code, Codex, OpenCode or Pi session two lines: what its agent
last said or did (`SessionView.agent_output`) and what the user last typed to it
(`SessionView.user_prompt`). The session list shows neither.
- **Where it comes from:** the daemon reads it from the agent's own conversation on this
  machine, at most once a second while the terminal shows output, and once more after it goes
  quiet (so does the resume command and places, at their own pace):
  - Claude Code: `<config dir>/projects/*/<conversation>.jsonl` (the newest copy), for the
    conversation in `sessions/<pid>.json`. When the process parked its conversation in a
    background job (`parkedJobId`), it's the job's (`jobs/<job>/state.json`). It's checked on
    every read, so `/clear`, resuming another conversation, or parking switch the transcript.
  - Codex: the `rollout-….jsonl` file the process has open.
  - OpenCode: `opencode.db`, read-only. OpenCode 2's newest replies in `session_message`, or
    1.x's newest assistant pieces in `part`, for the conversation the Rakumux plugin bound to
    the session (else the folder's newest, as resume does). Without the plugin, two OpenCodes
    in one folder show the same conversation's line.
  - Pi: `<agent dir>/sessions/--<folder>--/<time>_<id>.jsonl`, for the conversation the
    Rakumux extension bound to the session (else the folder's newest file, as Pi's
    `--continue` picks). Without the extension, two Pis in one folder show the same
    conversation's line, and a new Pi its folder's previous one until its first prompt.
- **What's taken:** the last entry the main conversation's assistant wrote (its reasoning, a
  tool call as `Tool: description` or a short path, or its reply), and the user's last typed
  message (a slash command as `/name args`; the agents' own insertions such as reminders,
  notifications and command output dropped), each cut to its first line: no control
  characters, at most 240 characters.
- **What isn't:** tool output and subagents' entries are never taken, and a log is read only
  from where it was last read, at most 256 KiB at a time. A follower new to a longer log reads
  its last 256 KiB, from the prompt of the turn they're in when that's within 8 MiB before
  them (found by looking only at what the user typed), so the turn keeps its prompt.
- **The turn:** the daemon also keeps the agent's current turn: the user's last prompt and
  every message the agent wrote since (reasoning, replies, tool calls), each flattened to one
  line of at most 600 characters, the newest 100. `SessionView.agent_turn` counts them and
  names the conversation (Claude Code's, or Codex's thread); `session_turn` (user callers
  only) returns them.
- **Where it goes:** the lines reach the daemon's clients like the rest of the session's view
  (the TUI, `rakumux-api` and so the phone app). The turn goes only to user clients that ask:
  the AI status service, which, while you're signed in and share the folder, makes one
  summary per conversation, once its agent has written 5 messages or when its turn finishes
  sooner (the prompt and the newest 10 messages; at a final reply every message that fits;
  `sessions.ai_summary` in `config.json`), and others only when asked (the ↻ on a
  description, or `s`).

## Replies for the feed

The same follower keeps the current turn's **reply** whole (`Reply`, at most 32 KiB): for
Claude Code, the main conversation's text after its last tool call (a prompt or a tool call
starts it over; one text block per log entry, keyed by the entry's `uuid`); for Codex, the
`task_complete` event's `last_agent_message` (keyed by its `turn_id`; a `user_message` or
`task_started` starts it over). The control layer takes it only while reply capture is on (the
status service turns it on for an account whose feed is on), when the session turns
`Done` after it was seen working, from a read of the log made after that; a key already
captured is never captured again. See the protocol section of `AGENTS.md` (`reply_capture`,
`reply_list`, `replies` subscriptions). The hook adapters still forward metadata only.
For OpenCode, the reply is the last text block the agent wrote since the newest prompt. For Pi,
its turn's final response: an assistant entry that stopped (`stop`, or cut at `length`) with no
tool to call, keyed by the entry's `id`; one that failed or was aborted is none. Each
reply also says where its turn ends in the conversation (`position`, as below).

## Whole conversations

`transcript.rs` reads a followed conversation whole, on demand (`session_transcript`), with
the mappings beside each agent's output parser (`<agent>/transcript.rs`): what the user typed
(the agents' own insertions taken out, as for the prompt line), the agent's text and visible
reasoning, its tool calls with their input, and the tools' output (the `tool` role), in order.

- **Claude Code:** one message per entry of the main conversation's transcript, at its byte
  offset, with its `uuid`. Subagents' entries, meta entries, compaction summaries and
  interruption notices are left out; redacted thinking says nothing.
- **Codex:** one message per rollout line, at its byte offset. The user's messages come from
  `user_message` events; replies, reasoning summaries (encrypted reasoning is left out), tool
  calls and their output from response items. A shell call's output is its `output`, failed
  when its exit code isn't 0.
- **OpenCode:** the conversation's rows changed since a watermark: each OpenCode 2 prompt, and
  each block of a reply (`seq × 1024 + block`; a tool block holds its call and, once it ran, its
  result), or each 1.x piece (`time_created × 1024 + its place`). Bookkeeping pieces (steps,
  snapshots, patches) and text OpenCode added are left out.
- **Pi:** one message per entry of its conversation file, at its byte offset, with the entry's
  `id`. A skill is shown as typed (`/skill:name args`); a `!` command the user ran is theirs, a
  `bash` call and its output. Pi's system prompt, extensions' context and summaries are left
  out; branches left with `/tree` stay, in the order they were written.

A log line longer than 8 MiB is passed over. Text is cut at 64 KiB per part and otherwise left
as written: the status service cleans it before it leaves the machine.

## Replies written back

A reply the user writes back to a feed entry goes into the agent the same way for every
integration: the status service types it into the session with `session_submit` (a bracketed
paste, then Enter), as if the user typed it. None of them has a supported way in from
outside: Claude Code and Codex take prompts only from their terminal (their headless modes
would run a second process on the same conversation), and OpenCode's server port belongs to
its own TUI; Pi's RPC mode is a process of its own. So nothing here is agent-specific beyond
what's already read: the conversation id
(`agent_turn.conversation_id`) the reply must still match, and the lifecycle that says the
agent waits for input, or works for one that takes what's typed during a turn and logs it as a
prompt once it does (`QUEUES_INPUT` in `tui/crates/sensagent/src/feed/replies.rs`, by the
daemon's agent names; add a new integration there once its chat shows such a prompt). The
agent's next captured reply answers it (`reply_to`).

Native input is capped at 1 MiB and never
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
cargo test -p rakumux-ptyd --lib pi:: --locked
cargo test -p rakumux-mcp --test pi_extension --locked
node --test '../integrations/pi/activity.test.mjs'
# Real Pi (1.1 or later) against a fake local model, offline; needs `cargo build --workspace`.
cargo test -p rakumux-ptyd --test pi_e2e --locked -- --ignored
```

Fixtures/fake clients do not replace real-agent or native macOS verification. Verify those
separately; Linux tests are not evidence of native macOS compatibility.
