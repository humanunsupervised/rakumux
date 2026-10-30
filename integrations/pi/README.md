# Pi

Run `pi` in a Rakumux session. With nothing installed, Pi 1.1 or later reports its state to the
terminal (OSC 7501), and the daemon reads its conversation file. For finer state and the exact
conversation, install the Rakumux extension, then restart Pi (or `/reload` in it):

```bash
rakumux-mcp pi-plugin --install   # writes ~/.pi/agent/extensions/rakumux-session.ts
```

`PI_CODING_AGENT_DIR` is respected; `--dir DIR` names another agent directory.
`rakumux-mcp pi-plugin` prints the extension instead. No Pi setting is changed. For Rakumux's
MCP tools in Pi, see [`mcp/README.md`](../../mcp/README.md#pi).

## What works

| | Without the extension | With it |
|---|---|---|
| Detection | process `pi` (or `node …/@earendil-works/pi-coding-agent/…`), title `π - …`, the screen | same |
| Lifecycle | OSC 7501: working, blocked (an extension's dialog, sign-in), done, idle | activity v1: waiting for the model, working, running a tool, needs input (an extension's dialog), done with the reply's entry id, idle; outranks OSC 7501 |
| Conversation | the folder's newest file | the one Pi shows, bound to the session at every start, `/new`, `/resume` and `/fork` |
| Git context | the terminal's directory | the conversation's folder |

Either way the daemon reads the conversation file
(`<agent dir>/sessions/--<folder>--/<time>_<id>.jsonl`, or `PI_CODING_AGENT_SESSION_DIR` or the
`sessionDir` setting): the latest output line and prompt, the current turn, the reply for the
feed (the turn's final response, keyed by its entry `id`), the whole conversation on demand, and
places (`write` and `edit`, also from codemode scripts; `read`, `grep`, `find`, `ls` and `bash`
`cd`/`git -C` count as worked in). A feed reply is typed in with a bracketed paste and Enter
while Pi waits; it shows up as the next prompt. On revive Pi resumes with
`pi --session <file>`, from any folder.

The extension runs only in Pi's own TUI (never `pi -p`, `--mode json` or `rpc`, such as a
subagent extension's children) inside a Rakumux session. It sends metadata only: never a prompt,
a tool's arguments or output, or a message's text. It calls `rakumux session hook --agent pi`
and `rakumux session bind --agent pi` (`$RAKUMUX_BIN`), one at a time, on change and every 10 s.

## Limits

- Pi hides its arguments (`process.title`), so a revive keeps only what the conversation keeps
  (model, thinking level); other launch flags (`--tools`, `-e`, `--system-prompt`, …) are lost,
  and a `--session-dir` on Pi's command line can't be seen.
- Without the extension, two Pis in one folder show one conversation, and a new Pi shows its
  folder's previous one until its first prompt.
- Custom `--session-id`s with a `.` aren't followed.
- The whole conversation includes branches left with `/tree`, in the order they were written.
- Shell commands' writes (`sed -i`, a redirect) aren't places, as for the other agents.
- A reply typed in that starts with `!` or `/` is a shell or slash command in Pi, as typed.
- Checked on Linux with Pi 1.4.0; macOS isn't verified.

## Files

- `detection.rs`, `environment.rs`, `presentation.rs`: screen profile (Pi 0.75 and 1.4),
  inherited markers (`PI_CODING_AGENT`, `AI_AGENT`, `PI_SESSION_*`, `PI_PROVIDER`, `PI_MODEL`,
  `PI_REASONING_LEVEL`), colour.
- `output.rs`, `transcript.rs`, `places.rs`, `recovery.rs`: the conversation file.
- `hooks.rs`: the extension's snapshots, validated.
- `rakumux-session/`: the extension (`activity.mjs`, a pure projection of Pi's events, and
  `runtime.mjs`); `install.rs`: its installer, compiled into `rakumux-mcp`.
- `activity.test.mjs`, `extension_tests.rs`: the projection, and the extension against a fake Pi
  and a fake `rakumux`.
- `e2e_tests.rs`: the real `pi` in a throwaway daemon, offline, against a fake local model:

```bash
cd tui && cargo build --workspace --locked
cargo test -p rakumux-ptyd --test pi_e2e --locked -- --ignored
```

It needs Pi 1.1 or later and Node on `PATH`, no account or network, and never touches `~/.pi`.

Reference: <https://github.com/earendil-works/pi/tree/main/packages/coding-agent/docs>
(`extensions.md`, `session-format.md`, `terminal-setup.md`).
