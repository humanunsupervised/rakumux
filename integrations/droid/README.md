# Droid

Factory's CLI (`droid`, checked with 1.0.0). Without anything installed, Rakumux recognises
Droid (`droid`, its npm shim under node, or the `⛬ ` title), reads its screen for working,
blocked and idle, follows its conversation for the session's lines, turn, replies, whole chat
and places, and resumes it on revive. Its hooks add exact activity and the conversation id.

## Hooks

```bash
rakumux session hook-config --agent droid
```

Save the printed events in `~/.factory/hooks.json` (the user's hooks, keyed by event; with
`$FACTORY_HOME_OVERRIDE`, there). If the file exists, append Rakumux's handlers to its event
arrays; do not replace other hooks. Restart Droid inside a Rakumux session: it reads hooks when it
starts. `/hooks` shows them; `hooksDisabled` or `--no-hooks` turns them off.

The command is `"${RAKUMUX_BIN:-rakumux}" session hook --agent droid`. It reads Droid's JSON on
stdin and answers `{"suppressOutput":true}`: Droid adds any other stdout to the conversation as
context (`{}` included) and shows the hook's row in its chat. It forwards metadata only, never
prompts, tool input or output.

| Droid hook | Activity |
|---|---|
| `UserPromptSubmit`, `PostToolUse` | working |
| `PreToolUse` | running tool; `AskUser`, `ExitSpecMode`, `ProposeWorkstream`, `ProposeMission`: needs input |
| `Notification` `permission_prompt`, `elicitation_dialog` | needs input |
| `Notification` `idle_prompt` (fired, instead of `Stop`, when the user stops a turn) | idle |
| `Stop` | done, its `message_id` the completion |
| `SessionStart` (startup, resume, clear), `SessionEnd` | idle |
| `SessionStart` with `source: compact` | nothing: compaction neither starts nor ends a turn |
| `SessionStart` with `calling_session_id` | a Task subagent starting, counted under its caller |

A Task subagent is a Droid session of its own, and its hooks carry its own `session_id`. Its
`SessionStart` announces it under the conversation that called it; the daemon then takes its
other hooks as its heartbeat and its `Stop` as its end, never as the session's phase,
conversation or directory. `SubagentStop` names no subagent and `PreCompact` changes nothing, so
neither is installed. Before Droid has made a session (it does at the first prompt) its hooks say
`session_id: "unknown"`: they report nothing. The root hook's `cwd` reports Droid's working
directory, which follows `/cwd` and `droid -w` worktrees.

## Conversation

Droid keeps each conversation in `~/.factory/sessions/<folder key>/<id>.jsonl`: a first line that
sums the session up, then one message per line in Anthropic's content blocks, its hook rows
among them. The conversation followed is the one the hooks report, else the folder's newest
top-level one (a subagent's names its caller). Droid rewrites the first line in place when it
names the session (after the first message, after the first edit, `/rename`) and when `/cwd`
moves it, which moves every byte after it: offsets and positions count from that line's end, and
a read it changed under is done again, so nothing is read twice or skipped and a reply keeps its
position. A rewind or a compaction starts another session.

- `agent_output`: the agent's latest thinking (or a GPT model's reasoning summary), tool call
  (`Execute: <summary>`, `Edit: <file>`) or text. `user_prompt`: what the user typed, without
  Droid's reminders. Tool results, hook rows, notices for the model or the user only, and other
  agents' messages are neither.
- The reply for the feed: the text after the turn's last tool call, keyed by its message's id.
- `session_transcript`: one message per line; tool results are the `tool` role's.
- Places: the summary's `cwd` and `lastCwd`; `Create`, `Edit`, `MultiEdit` and `ApplyPatch`
  change files; `Read`, `LS`, `Grep`, `Glob` and `Execute`'s `cd`/`git -C` work in a directory.
  `~/.factory` is Droid's own, never a place, but its worktrees (`droid -w`) are.
- Replies written back are typed in (a bracketed paste, then Enter) and show up as the next
  prompt.
- Questions answered from rakumux.com (`questions.rs`, the daemon's `session_answer`): `AskUser`
  asks with a questionnaire (`1. [question] … (multi)`, `[topic]`, `[option]` lines), answered
  `[question] …` / `[answer] …`. The daemon checks that the newest `AskUser` call without a result
  is the one answered and that the screen shows its dialog (under `Ask User`: the question, its
  numbered options, `Or type your own answer...`), then works it question by question. Its cursor
  is only drawn bold, so the screen is read with its styles. An option's number picks it, or
  ticks its box for a `(multi)` question (the cursor stays); the free answer is typed on its row
  (`down` to it); a `(multi)` question then takes `down` to `Submit` (`Next` before the last) and
  `enter`. The last question's answer gives them all; there's no review. Its English screens
  only (Droid can show others). Checked against Droid 1.0.0 with a custom model; its screens are
  `fixtures/questions/` (`.ansi`, with their styles).

## Resume

`droid <kept flags> --resume <id>`, keeping `--auto`, `--settings`, `--append-system-prompt`,
`--append-system-prompt-file`, `--cwd` and `--disable-builtin-skills` (Droid restores the model
and autonomy it saved for the session). A prompt, `--resume`, `--last`, `--fork`, `--use-spec`
and `-w`/`--worktree` are dropped: resuming never makes a worktree. `droid exec`, `droid daemon`,
`droid mcp` and its other commands are typed on revive, never run. New sessions never inherit
`FACTORY_SESSION_ID`, which Droid gives the commands it runs.

## Limits

- Without the hooks, two Droids in one folder show the same conversation (the folder's newest).
- A subagent's permission prompt is its heartbeat, not the session's: the screen shows it.
- The Droid sandbox (opt-in) runs hooks in bubblewrap or Seatbelt, which may keep them from the
  daemon's socket; detection then does the work.
- `cloudSessionSync` (on by default) mirrors Droid's sessions to Factory's web app: that's Droid's,
  not Rakumux's.
- Its session format is internal (`version: 2`) and Droid updates itself; unknown lines and fields
  are passed over.

## Files

- `detection.rs`: screen and process profile. `hooks.rs` / `hooks.json`: adapter and config.
- `recovery.rs`: conversation lookup and resume flags. `output.rs`: the session file, its first
  line and the lines. `transcript.rs`, `places.rs`: whole chat and places.
- `environment.rs`, `presentation.rs`: `FACTORY_SESSION_ID`, the label colour.
- `questions.rs`, `fixtures/questions/`: `AskUser`'s calls and results, and its dialog.
- `e2e/`: an offline end-to-end check with a stand-in Droid (below).

## Verification

```bash
cd tui
cargo test -p rakumux-ptyd droid --locked
cargo test -p rakumux-ptyd detect:: --locked
cargo test -p tui droid --locked
cargo build --workspace --locked && python3 -I ../integrations/droid/e2e/run.py
```

`e2e/run.py` starts a throwaway daemon (`RAKUMUX_HOME`, `HOME` and `FACTORY_HOME_OVERRIDE` in a
temporary directory) and runs `e2e/fake_droid.py` as `droid`: it writes Droid's session file
(rewriting its first line), runs the hooks this command prints as Droid does, and draws Droid's
screens. It checks the lifecycle, `agent_output`, `user_prompt`, `agent_turn`, `session_turn`,
`session_transcript`, reply capture, places, a counted subagent, a reply typed in as a new turn,
the resume command and revival after a crash. No network, account or real Droid. Real model
turns need a Factory sign-in (`droid` → Login, or `FACTORY_API_KEY`), or airgap mode with a custom
model: `FACTORY_AIRGAP_ENABLED=true` and a `customModels` entry in `.factory/settings.json` (the
`generic-chat-completion-api` provider at a local model). `tui/crates/ptyd/tests/runtime_answers.rs`
(ignored) runs the real `droid` so against a scripted model and answers its `AskUser` dialog
through `session_answer`; `--test control_answer_agents` covers the daemon's side with its
screens.

References: <https://docs.factory.com/harness/hooks>, <https://docs.factory.com/droid-cli/cli-reference>,
<https://docs.factory.com/harness/mcp>.
