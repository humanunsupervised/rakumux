# Amp

Install the Rakumux plugin, then restart Amp inside Rakumux (or run `plugins: reload` from its
command palette):

```bash
rakumux-mcp amp-plugin --install
```

It writes `~/.config/amp/plugins/rakumux-session.js` (`XDG_CONFIG_HOME` respected; `--dir`
names Amp's config directory) and changes no Amp setting. `rakumux-mcp amp-plugin` prints the
same self-contained module. For MCP tools, see [mcp/README.md](../../mcp/README.md#amp).

## What the plugin does

Amp runs it as a system plugin in every Amp it starts. It does nothing outside a Rakumux session
(both `RAKUMUX_SESSION_*` variables) or when Amp runs remotely (an orb or a runner). Inside one:

- **Binds** the thread Amp shows (`amp.activeThread`) to the session (`rakumux session bind
  --agent amp T-…`), so the daemon follows and resumes that thread.
- **Reports** Amp's own thread state as activity snapshots (`rakumux session hook --agent amp`,
  on change and every 10 s): `running` is working (waiting for a response after the prompt,
  running a tool while a call has no result), `awaiting-approval` needs input, a turn Amp ended
  as `done` is a finished reply (its last assistant message's id as `completion_id`); a
  cancelled or failed turn, or `error`, is idle. Running `Task` calls count as subagents. Amp's
  workspace is the working directory. Ids and phases only.
- **Keeps a copy** of each thread it sees: Amp keeps threads on ampcode.com, not on this
  machine. One JSON line per message in Amp's plugin schema, plus where each turn ended, in
  `~/.cache/amp/plugin-cache/rakumux-session/threads/<thread>.jsonl` (`XDG_CACHE_HOME`
  respected; directory 0700, files 0600). Copies not written to for 30 days are removed. Only
  the Rakumux daemon on this machine reads them; what leaves the machine is what the agent feed
  sends for every agent, while you're signed in.
- **Never handles `tool.call`**, so it can't allow, reject or change a tool call. It handles
  `session.start`, `agent.start` (returns `{}`) and `agent.end` (returns nothing).

## What works

| | |
|---|---|
| Detection | `amp` (installer and npm builds), `@ampcode/cli` / `@sourcegraph/amp` under Node; approval dialogs and the signed-out log-in question block; a braille title spinner is work; its animation isn't |
| State | the plugin's snapshots, above the screen |
| Resume | `amp threads continue <thread> <flags>`: the bound thread, else the one the process continued, else `--last`; keeps `--dangerously-allow-all`, `-m/--mode`, `--settings-file`, `--mcp-config`, `--features`, `--fast`, `--[no-]ide`, `--[no-]notifications`, `--[no-]remote-control-terminal`, `--[no-]color`, `--log-*`. `amp --no-tui` (a runner) is a command |
| Latest line, prompt, turn, reply, whole chat | from the copy (`output.rs`, `transcript.rs`) |
| Places | each message's workspace; `edit_file`, `create_file`, `apply_patch`, `Read`, `Grep`, `glob`, `Bash` (`cmd`, `cwd`), `shell_command`; the files Amp itself names for a call (`filesModifiedByToolCall`, its `sed -i` too) |
| Replies written back | typed in (bracketed paste, Enter) while Amp waits; the copy shows them as the next prompt |
| Environment | new sessions don't inherit `AMP_THREAD_ID`, `AMP_CURRENT_THREAD_ID`, `AGENT_THREAD_ID`, `AI_AGENT` |

## Limits

- Without the plugin there is nothing to read: state comes from the screen and resume is
  `amp threads continue --last`.
- Apart from the log-in question, the screen markers come from Amp's own strings, not a
  captured screen (Amp needs a sign-in to draw one).
- History a thread had before the plugin first saw it is copied once (its newest 2000
  messages) without times; other messages carry the time the plugin copied them.
- Only the thread Amp shows is followed; threads running in the background are copied when
  their turns end but not reported.
- The copy follows Amp's documented plugin API (`amp plugins show-docs`); Amp updates itself
  often, so check it there after a release.

## Tests

- `activity.test.mjs` (Node): snapshots, the copy's lines (`fixtures/messages.json` gives
  exactly `fixtures/thread.jsonl`, the fixture the Rust readers are tested on), bounds.
- `plugin_tests.rs` (`cargo test -p rakumux-mcp --test amp_plugin`): the installed plugin under
  Node against a fake Amp plugin API and a fake `rakumux`: binding, reports, the copy's
  permissions and pruning, nothing outside a session or with a remote executor.
- Rust unit tests beside each module, and `control_activity`'s Amp test.
- End to end (not in CI: it needs Amp installed): a throwaway daemon, the real `rakumux` CLI
  and installer, and an offline stand-in for Amp's TUI that runs Amp's real plugin runtime
  (`plugin-runtime.ts` from Amp's binary, run by `BUN_BE_BUN=1 amp`) and plays scripted turns
  through its plugin RPC. It checks lifecycle (working, blocked, done), the latest line,
  prompt and turn, `session_turn`, `session_transcript`, reply capture, places, a reply typed
  in with `session_submit` becoming the next turn, and resume after a kill and a revive.

## Files

- `rakumux-session/`: the plugin (`activity.mjs`, pure; `runtime.js`).
- `install.rs`: installer compiled into `rakumux-mcp`.
- `detection.rs`, `recovery.rs`, `hooks.rs`, `output.rs`, `transcript.rs`, `places.rs`,
  `environment.rs`, `presentation.rs`: mounted by the shared dispatch in `integrations/`.

Reference: <https://ampcode.com/docs/customize/plugins>, <https://ampcode.com/docs/plugin-api>.
