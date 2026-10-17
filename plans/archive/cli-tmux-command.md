# Plan: CLI Flags for New Tmux Session + Command

Goal: add CLI flags that let users specify a branch, request a new tmux session, and run a command automatically inside that session. One command should: resolve the worktree for the branch, create a new tmux session attached to that worktree, run the command, and attach the user to the session.

## Proposed CLI shape
- Add a new CLI path to `orchestra-cli.sh` that supports flags (recommended):
  - `orchestra-cli --branch <branch> --new-tmux --cmd "<command>"`
  - Aliases: `-b` for `--branch`, `-c` for `--cmd`.
- Keep existing subcommands intact; the flag-based entry runs before the `case` dispatch and exits after handling.

## Implementation steps
1) Add a new handler function in a shell module (recommended new file):
   - `shell/tmux/new_session_command.sh` containing something like `cmd_new_tmux_session_with_command()`.
   - Load this module from `orchestra-cli.sh` alongside the existing `shell/git/*.sh` commands.

2) Implement the flow in `cmd_new_tmux_session_with_command()`:
   - Validate `--branch` and `--cmd` are provided; if missing, print a concise usage line and return non‑zero.
   - Resolve or create the worktree for the branch:
     - If the branch exists: `git_branch_to_worktree_path` or `git_ensure_worktree_for_branch`.
     - If branch does not exist: decide behavior (recommended: error by default; optionally allow `--create-branch` later).
     - Copy env files using existing `copy_env_files` when creating a worktree.
   - Create a **new** tmux session (not reuse) using existing API:
     - `tmux_ensure_session` with `slug=branch_name`, `name` empty to trigger auto name, `worktree_path`.
   - Send the command to the session:
     - `tmux_send_keys "$session" "$cmd"`.
   - Attach/switch into the session:
     - `tmux_attach_session "$session"`.

3) Add flag parsing in `orchestra-cli.sh` before the `case` dispatch:
   - Parse `--branch/-b`, `--new-tmux`, `--cmd/-c`.
   - If `--new-tmux` is set, call the new handler and exit.
   - Keep all existing commands working unchanged.

4) Update usage/help text in `orchestra-cli.sh`:
   - Add the new flags and an example invocation.
   - Note that this flow requires tmux installed.

## Edge cases + decisions
- If the branch doesn’t exist: default to error; document in usage.
- If tmux isn’t available: print a clear error and exit 1 (use `tmux_available`).
- If the command is empty or only whitespace: treat as error (require explicit command string).
- Handle `--` to allow commands starting with `-` (optional enhancement).

## Files to touch
- `orchestra-cli.sh` (flag parsing + usage text + module load)
- `shell/tmux/new_session_command.sh` (new file)
- Possibly `shell/cli_usage.sh` if you want help text centralized later

## Quick manual checks
## E2E verification plan (outside this repo)
Run these steps from a separate git repo located one directory up from this repo (e.g. `../orchestra-e2e/`). This avoids polluting the orchestrator repo and validates the CLI path in a clean environment.

1) Create a temporary test repo and branch:
   - `mkdir -p ../orchestra-e2e && cd ../orchestra-e2e`
   - `git init`
   - `git commit --allow-empty -m "init"`
   - `git checkout -b e2e-cli-tmux`

2) Build the app locally from this repo:
   - `cd /Users/antoniodeperio/repos/orchestrator`
   - `./build.sh`

3) Launch the CLI flow using orchestra-local to point at the local build:
   - From the test repo directory:
     - `bash /Users/antoniodeperio/repos/orchestrator/shell/orchestra-local.sh --root /Users/antoniodeperio/repos/orchestrator -- --branch e2e-cli-tmux --new-tmux --cmd "echo e2e_ok"`
   - Expected: tmux session attaches, and the command runs in that session.

4) Validation checks:
   - In tmux, confirm `echo e2e_ok` output is visible.
   - Detach and confirm session exists: `tmux list-sessions`.

5) Negative case:
   - `bash /Users/antoniodeperio/repos/orchestrator/shell/orchestra-local.sh --root /Users/antoniodeperio/repos/orchestrator -- --branch missing-branch --new-tmux --cmd "echo hi"`
   - Expected: clear error about missing branch and non-zero exit.

## Existing quick manual checks
- `orchestra-cli --branch main --new-tmux --cmd "git status"` (creates tmux, runs command, attaches)
- `orchestra-cli --branch missing --new-tmux --cmd "echo hi"` (fails with clear message)
- `orchestra-cli ls` and other existing commands still work
