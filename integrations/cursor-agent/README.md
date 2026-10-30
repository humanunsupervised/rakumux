# Cursor (agent CLI)

Cursor's agent CLI, installed with `curl https://cursor.com/install -fsS | bash` as both `agent`
and `cursor-agent`. Checked against 2026.10.22-27dcf5f on Linux; its own screens and live
behaviour need a signed-in Cursor account (see "Live checks").

## Setup

Without anything installed, Rakumux detects Cursor, follows its chat (latest line, prompt,
turn, replies for the feed, the whole chat, places) and resumes it on revive. Its hooks add
the lifecycle as Cursor sees it:

```bash
rakumux session hook-config --agent cursor-agent
```

Merge the printed `hooks` entries into `~/.cursor/hooks.json` (Cursor's own format,
`"version": 1`): append to existing step arrays; do not replace other hooks. Restart the agent
inside a Rakumux session. It has to be the user's (or a trusted project's) `hooks.json`:
Cursor's CLI fires `beforeSubmitPrompt` and `stop` only when one of those defines them. Claude
Code's hooks, which Cursor also runs, report as Cursor's (their input has `cursor_version`), but
on their own they never see a prompt or a finished turn. Remove only the Rakumux entries to
disable.

The command is `"${RAKUMUX_BIN:-rakumux}" session hook --agent cursor-agent`; it always
answers `{}` (a permission hook's no-opinion). Prompt submission and tool results report
**working**, `preToolUse` **running tool** (its `AskQuestion` **needs input**), `stop` with
`completed` **done**, an aborted or failed `stop`, `sessionStart` and `sessionEnd` **idle**,
and `subagentStart` / `subagentStop` count subagents by `subagent_id` under the parent's chat.
A subagent's own tool hooks (they name the parent's Task call) are left out: they don't say
which subagent they are. `preCompact` changes nothing. Cursor has no hook for an approval or
a question it draws: its profile sets `approvals_on_screen`, so a prompt on its screen
(`Run this command?`, `Write to this file?`, its trust and sign-in screens, the answer and
decision prompts in its input box) wins over a busy hook. Only metadata is sent: never the
prompt, tool input or output, `user_email` or `transcript_path`.

The working directory is the hook's single `workspace_roots` entry (a `-w` worktree's own),
never a tool's `cwd`.

## What Rakumux reads

- **Process.** The launcher runs a bundled Node (`exec -a agent …/cursor-agent/versions/<v>/node
  … index.js`), which Linux names `MainThread`; detection goes by that install path, never the
  bare name `agent` (Grok installs one too). `expect_foreground` for replies typed in is that
  name (`MainThread` on Linux, `agent` or `cursor-agent` on macOS).
- **Chat.** `<config>/chats/<md5 of the folder>/<chat>/store.db`, which the process keeps open
  once the chat has a message (Linux reads it from `/proc`), else the chat its hooks name;
  where open files can't be read (macOS) without hooks, the folder's newest chat by its
  `meta.json` (two Cursors in one folder then show the same chat). `<config>` is
  `CURSOR_CONFIG_DIR`, else `$XDG_CONFIG_HOME/cursor`, else `~/.cursor`.
- **Transcript.** `<data>/projects/<folder slug>/agent-transcripts/<chat>/<chat>.jsonl`
  (`<data>`: `CURSOR_DATA_DIR`, else `~/.cursor`; another project's when the workspace is
  elsewhere). Lines are `{"role":"user"|"assistant","message":{"content":[…]}}` with `text` and
  `tool_use` blocks, and `{"type":"turn_ended"}`. The prompt is the user line's text, inside
  its `<user_query>`. Cursor writes the file whole on a process's first write, after each
  finished turn (dropping that turn's end) and on a rewind or compaction: readers never move
  past a final `turn_ended` line, and start over when where they stopped no longer ends a line.
  The directories come from the daemon's environment, as `CLAUDE_CONFIG_DIR` does.
- **Resume.** `cursor-agent <flags> --resume <chat>`, keeping `--model`, `--mode`, `--plan`,
  `-f`/`--force`/`--yolo`, `--auto-review`, `--approve-mcps`, `--trust`, `--sandbox`,
  `--workspace`, `-e`/`--endpoint`, `--add-dir` and `--plugin-dir`; never `--api-key` or
  `-H`/`--header`, a prompt, `--print` or `-w`. Commands that aren't a chat (`agent login`,
  `agent mcp list`, …) are typed back on revive, not run.
- **Places.** Its tool calls: `Write`, `StrReplace`, `Delete`, `EditNotebook` (and their older
  names) change files; `Read`, `Grep`, `Glob`, `LS`, `SemanticSearch` and `Shell`
  (`working_directory`, `cd`, `git -C`) are worked in. Paths are relative to the workspace.
- **Replies typed in.** A bracketed paste, then Enter: it shows as the chat's next user line.

## Limits

- The transcript has no tool output, no ids or times (a message's time is the file's), and an
  assistant line's text ends with its reasoning when the model gave some: a reply, a turn's
  messages and the chat can carry that reasoning after the answer (`[REDACTED]` reasoning is
  left out). Cursor's chat store (`store.db`) has them apart; reading it is the follow-up if
  live checks show reasoning in replies.
- Without hooks, a subagent isn't counted, and the lifecycle is the screen's.
- Cursor's IDE agents write transcripts in the same `projects/` tree; Rakumux reads one only by
  its chat id, never by recency.

## Live checks (a signed-in Cursor: `agent login`, or `CURSOR_API_KEY`)

1. Idle, working, approval (`Run this command?`), trust, `AskQuestion` and mode-switch screens
   match `detection.rs`.
2. A prompt that reads, edits a file and runs a command: the transcript's tool names and input
   keys (`places.rs`), the `<user_query>` wrapper, whether reasoning ends the reply text, the
   `turn_ended` line; `/proc/<pid>/fd` holds `chats/…/store.db`.
3. With the printed `hooks.json`: payloads, `preToolUse` before the approval, subagent ids, and
   `RAKUMUX_SESSION_*` reaching the hook.
4. `/clear`, `/resume`, `/fork`, `--resume <chat>` on revive, and a `-w` chat's workspace.
5. A reply from the feed typed in becomes the next turn (`user_prompt`).
6. `rakumux-mcp` from `~/.cursor/mcp.json` (see `mcp/README.md`).

The stand-in test (`e2e.rs`, run by `cargo test -p rakumux-ptyd --test cursor_agent_e2e`)
covers the daemon's side with Cursor's formats.

- `detection.rs`: screens and process; `hooks.rs` / `hooks.json`: adapter and configuration.
- `output.rs`, `transcript.rs`, `places.rs`: the chat, read from its transcript.
- `recovery.rs`: chat lookup and resume flags; `environment.rs`, `presentation.rs`.

References: <https://cursor.com/docs/cli/overview>, <https://cursor.com/docs/hooks>,
<https://cursor.com/docs/cli/reference/parameters>.
