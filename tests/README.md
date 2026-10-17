# Orchestra Test Scripts

Most testing lives in the Rust workspace (`tui/`): unit tests per crate and daemon integration
tests that run against a temporary `ORCHESTRA_HOME`.

```bash
cd tui
cargo test --workspace          # everything
cargo test -p tui naming        # AI session naming
```

This directory holds the shell-level and manual checks.

## Automated

- **`test-command-hook.sh`** - `shell/orchestra-command-hook.sh`: per-session history logging and
  `orchestra_rename_session`, in bash (and zsh when installed), against a temporary
  `ORCHESTRA_HOME`.

```bash
./tests/test-command-hook.sh
```

## Manual

- **`manual-test-instructions.md`** - Checklist for the TUI, the daemon, the CLI, AI naming and the
  shell wrapper.
- **`test-wrappers.sh`** - Checks that the `orchestra` shell wrapper is defined.
- **`test-tui-quick.sh`**, **`test-tui-keys.sh`** - Launch the TUI briefly from the repo root.
- **`test-terminal-direct.sh`** - Alternate-screen and colour check for the terminal itself.
- **`debug-tui-output.sh`** - Terminal capabilities when the TUI renders nothing.

Run the scripts from the repository root, e.g. `./tests/test-wrappers.sh`.

## Troubleshooting

**"command not found"**
- Run from the repository root.
- Build first: `./build.sh` (or `cargo build --workspace` in `tui/`).

**TUI scripts report "Failed to initialize input reader"**
- Expected when the TUI runs without a terminal (pipes, timeouts). Run `./orchestra.sh` directly
  for manual testing.
