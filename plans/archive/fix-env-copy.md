# Plan: Fix subdirectory .env copy in new worktrees

## Goal
- Ensure new worktree creation copies all `.env` and `.env.*` files from nested monorepo subdirectories into the same relative locations in the new worktree.

## Findings
- Worktree creation uses `shell/git/create_worktree.sh`, which calls `copy_env_files` from `shell/env/*`.
- `copy_env_files` loads `copy_locations` from `.orchestra/options.json` (default `/`) and for `/` performs a recursive `find` but prunes `.git` and `worktrees/`.
- CLI entrypoints pass `source_root="$(git_current_path)"`. If invoked from a subdirectory, only that subtree is scanned; the shared repo root scan prunes `worktrees/`, so `.env` files in sibling subdirectories of the current worktree are skipped.
- TUI services creation (`shell/git/create_worktree_api.sh`) already uses `git_repo_root`, so the issue primarily affects CLI flows and any shell commands run from a nested directory.

## Root cause
- The copy source is the current directory, not the worktree root, so recursive scanning misses `.env` files outside that subtree.

## Fix plan (shell)
1. Update CLI call sites to use `git_repo_root` for `source_root`:
   - `shell/git/create_worktree.sh`
   - `shell/git/checkout_worktree.sh`
   - `shell/tmux/new_session_command.sh`
2. Optional hardening: normalize `source_root` inside `shell/env/copy_env_core.sh` by deriving the worktree root via `git -C "$source_root" rev-parse --show-toplevel` when available.
3. Tests:
   - Add a test that runs env copy from a subdirectory and asserts sibling `.env` files are copied.
   - Extend `tests/test-copy-env-recursive.sh` or add a new focused test for CLI worktree creation from subdir.

## Rust option (longer term)
- Implement a native Rust copy in `tui` mirroring `copy_env_files` behavior (recursive `/` with pruning of `.git`, `worktrees/`, and target root).
- Wire TUI worktree creation to the Rust copy or add a services flag to switch to the native implementation.

## Verification
- From a nested directory, run `orchestra-cli create <branch>` and confirm `subdir1/.env` and `subdir2/.env` are copied into the new worktree.
- Run the TUI flow (`orchestra`) to confirm no regression and consistent behavior.
