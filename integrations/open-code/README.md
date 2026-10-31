# OpenCode

Install the CLI-side plugin, then restart OpenCode V2 inside Rakumux:

```bash
rakumux-mcp opencode-plugin --install
```

The installer bundles the cache projection and runtime into one `tui.js` under
`~/.config/opencode/plugins/rakumux-session/` (`XDG_CONFIG_HOME` respected; `--dir` overrides
the config directory). It does not change OpenCode config. `rakumux-mcp opencode-plugin`
prints the same self-contained module instead.

The plugin runs in the TUI, where session credentials exist, not OpenCode's shared service.
It preserves conversation binding for MCP and reports the displayed root's cached execution,
tool phase, permissions/forms and active descendants. A queued prompt is not running work.
A running execution whose latest message is the submitted user message is **waiting for
response**, not proof of a particular network operation. The cache is polled every 250 ms;
snapshots are sent on change or every 10 seconds. No prompt/output text or model calls.

A cached successful `idle` execution marker reports **done**, using its opaque message ID
as `completion_id`. This catches completions even when the running phase fell between polls.
Repeated snapshots cannot make that reply unread after it is opened. Active descendants or
pending permissions/forms take priority over a root's completed marker. Failed/interrupted
execution markers report idle rather than claiming a successful completion.

The displayed session's `location.directory` and `subpath` also report its active worktree.
The original plugin location is not reused after a switch. A directory change is sent even
when activity is unchanged; the daemon needs `session.working_directory` to use it for the
branch/worktree label. Background descendants do not overwrite the displayed context.

The daemon recognises OpenCode V2 run as `opencode` or as `opencode2` (the second name its
`@opencode/cli` package installs); resuming keeps `opencode2`, since `opencode` may be a 1.x
install. An unrecognised process name would count as "no agent" and drop the plugin's activity.

Questions its `question` tool asks (1.x and 2) are answered from rakumux.com (`questions.rs`):
the newest `question` call is read from the database with its `status` (`completed` once
answered, `error` once dismissed), and `session_answer` works the dialog by number: an option's
digit, the number after the options (`Type your own answer`) and the text and Enter, Tab to leave a
page of boxes, then Enter on the review once it lists the answers given (one question taking one
choice has none).

- `rakumux-session/`: plugin and pure activity projection.
- `install.rs`: installer compiled into MCP.
- `detection.rs` / `recovery.rs`: terminal fallback and conversation resume.
- `activity.test.mjs` / `plugin_tests.rs`: cache/fake-plugin tests.

Reference: <https://opencode.ai/v2/docs/build/plugins/cli>.
