# Plan: Fix delete-worktree options (branch + tmux)

## Goal
- Deleting a worktree with options enabled reliably deletes the branch and all related tmux sessions.

## Findings / root causes
- `services.sh` dispatches `delete-branch-only` to `delete_branch_only_api`, but no such function exists. The function names and behaviors are swapped between `shell/git/delete_worktree_only_api.sh` and `shell/git/delete_branch_only_api.sh`.
- `api/tmux.sh:tmux_list_sessions_for_slug` only matches slug+hash prefixes when a worktree path is supplied. Local wrapper sessions created for Remote mode use `canonical_session_name` (no hash), so they are not listed or deleted.
- CLI tmux creation (`shell/tmux/new_session_command.sh`) uses the raw branch name (slashes intact), while the TUI uses a slugified branch (slashes replaced by `-`). This mismatch prevents listing/killing sessions created via the CLI when the TUI does deletion.

## Proposed approach
- Fix the services function naming and behavior for `delete-branch-only` and `delete-worktree-only`.
- Make tmux session listing tolerant of no-hash canonical session names when `worktree_path` is provided, or add a fallback in Rust when no sessions are found.
- Normalize slug usage across CLI and TUI tmux session creation.

## Implementation steps
1. Update `shell/git/delete_branch_only_api.sh` to define `delete_branch_only_api` and delete only the branch.
2. Update `shell/git/delete_worktree_only_api.sh` to define `delete_worktree_only_api` and delete only the worktree.
3. Add/adjust tests for delete options (branch + worktree) to confirm the correct services function is executed.
4. Update `api/tmux.sh` `tmux_list_sessions_for_slug` to also match canonical no-hash session names when `worktree_path` is supplied, or add a Rust fallback (`list_sessions`) when `list_sessions_for_worktree` returns empty.
5. Update `shell/tmux/new_session_command.sh` to slugify the branch (use `git_branch_to_slug`) before calling `tmux_ensure_session`, or teach session listing to accept both raw and slugified prefixes.

## Done when
- Deleting a worktree with “delete branch” removes the branch every time.
- Deleting with “delete tmux sessions” kills local sessions created by TUI and local wrapper sessions created for Remote mode.
- Session listing in the TUI includes local wrapper sessions for Remote mode (if intended by design).
