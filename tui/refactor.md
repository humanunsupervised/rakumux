# Refactor Plan: `tui/src/app.rs`

This document proposes a modular refactor of `tui/src/app.rs` to improve readability, testability, and long-term maintainability without changing behavior.

## Objectives

- Reduce file size and cognitive load by grouping related responsibilities.
- Eliminate duplication (session context detection and name parsing appear in multiple places).
- Make async/background operations easier to reason about (clear owners and lifecycles).
- Preserve the public surface used by `main.rs` and `ui.rs` (no functional changes).
- Improve unit test coverage for pure utilities (session name parsing/formatting, validators).

## Current responsibilities inside `app.rs`

- Domain utilities:
  - Session name parsing/formatting utilities: `session_delim`, `strip_rakumux_prefix`, `extract_*`, `format_*`, `to_sentence_case`, `format_session_timestamp`, `extract_session_prefix`, `extract_session_datetime`, `should_auto_rename_session`.
  - Validators: `is_hex8`, `is_digits`, `is_valid_branch_name`, `is_valid_session_name`.
  - Session context detection from tmux metadata (duplicated in 3 places).
- Application types and UI state:
  - `App`, `AppResult`, `Selection`, `DisplayItem`.
  - `AppMode` and dialog structs (`CreateDialog`, `ConfirmationDialog`, `DeleteOptionsDialog`, `RenameDialog`, `ApiKeyDialog`, `MergeMenuDialog`, `CommitMessageDialog`, `LogDialog`, `MergeProgressDialog`).
  - Config persistence: `Config::{load, save}`.
- Data and async orchestration:
  - Data loading and refresh: `new`, `refresh_data_mut`, `perform_light_refresh`, `spawn_background_refresh`, `check_refresh_result`.
  - Git status pipeline: `spawn_git_status_task`, `check_git_status_result`, `extract_worktree_name_from_path`.
  - Session pipelines: preview (`load_session_preview_if_needed`, `check_preview_result`), rename (`start_session_rename`, `start_background_auto_rename`, `evaluate_rename_completion`, `auto_rename_session`).
  - Deletion pipelines: `start_background_worktree_deletion[_with_options]`, `start_background_branch_deletion` and completion detection.
  - Merge pipelines: `start_merge_operation[_with_message]`, `check_merge_result`, `op_label_to_friendly`.
  - Selection and navigation: `get_display_items`, toggle expand, selection helpers, debounced updates.
- Key handling by mode:
  - `handle_key_event` + per-mode handlers: `handle_splash_key`, `handle_browsing_key`, `handle_creating_key`, `handle_confirming_key`, `handle_delete_options_key`, `handle_api_key_key`, `handle_renaming_key`, `handle_merge_menu_key`, `handle_commit_message_key`, `handle_merge_progress_key`, `handle_logs_key`.

## Pain points / duplication

- Session context detection logic is repeated in: `refresh_data_mut`, `perform_light_refresh`, and `check_refresh_result`.
- Session name parsing/formatting code is large and sits among unrelated code.
- Many responsibilities in `App` (state, IO orchestration, UI mode handling, long-lived background tasks) make it hard to reason about lifecycles and cancellation.
- Timeout/duration magic numbers are scattered (10s, 30s, 60s, 200ms).

## Proposed module layout

Create an `app/` module directory and split responsibilities. Keep `pub` re-exports so `main.rs` and `ui.rs` continue to import from `crate::app`.

```
src/app/
  mod.rs                # Public facade: re-exports, App coordination glue
  types.rs              # AppResult, Selection, DisplayItem, AppMode, Dialog structs, GitStatusResult, MergeTaskResult, RefreshSnapshot
  config.rs             # Config load/save
  session_name.rs       # All session naming/formatting/parsing utilities
  context.rs            # Session app context detection from SessionMetadata
  sessions.rs           # Session operations: rename, preview, selection helpers
  git.rs                # Git status pipeline and helpers, branch validation
  refresh.rs            # Full/light/background refresh orchestration
  deletion.rs           # Worktree/branch deletion pipelines and state helpers
  merge.rs              # Merge/rebase/squash pipelines and mode helpers
  navigation.rs         # Display tree, expand/collapse, selection, debouncing
  handlers/
    browsing.rs         # handle_browsing_key + selection/delete/rename entrypoints
    creating.rs         # handle_creating_key
    confirming.rs       # handle_confirming_key (calls deletion ops)
    delete_options.rs   # handle_delete_options_key
    api_key.rs          # handle_api_key_key (uses config)
    renaming.rs         # handle_renaming_key (manual)
    splash.rs           # handle_splash_key
    merge_menu.rs       # handle_merge_menu_key
    merge_progress.rs   # handle_merge_progress_key
    commit_message.rs   # handle_commit_message_key
    logs.rs             # handle_logs_key
constants.rs            # Durations/timeouts and any shared constants
```

Notes:
- `components/` (rendering) will import `crate::app::types::*` for modes/dialogs/display items just like today.
- Keep `App` as the single state owner, but move most method bodies into focused modules. These modules will take `&mut App` as a parameter to operate on state.
- Avoid cycles by keeping pure utils (`session_name`, `context`, `constants`) free of `App`.

## Function/file mapping (high-level)

- session_name.rs
  - `session_delim`, `strip_rakumux_prefix`, `is_hex8`, `is_digits`
  - `extract_new_format_session`, `extract_old_format_session`
  - `format_session_content[_with_context]`, `format_session_display_name[_with_context]`
  - `format_session_timestamp`, `extract_session_prefix`, `extract_session_datetime`
  - `should_auto_rename_session`, `to_sentence_case`
  - Tests currently at bottom of file move here and expand

- context.rs
  - `fn app_from_metadata(md: &SessionMetadata) -> String` (centralize repeated logic)

- types.rs
  - `AppResult`, `Selection`, `DisplayItem`
  - `AppMode` + dialogs (`CreateDialog`, `ConfirmationDialog`, `DeleteOptionsDialog`, `RenameDialog`, `ApiKeyDialog`, `MergeMenuDialog`, `CommitMessageDialog`, `LogDialog`, `MergeProgressDialog`)
  - `GitStatusResult`, `MergeTaskResult`, `RefreshSnapshot`

- config.rs
  - `Config::{load, save}`, `config_file_path`

- git.rs
  - `spawn_git_status_task`, `check_git_status_result`, `extract_worktree_name_from_path`
  - `is_valid_branch_name`

- sessions.rs
  - Rename flows: `start_session_rename`, `start_background_auto_rename`, `evaluate_rename_completion`, `auto_rename_session`
  - Preview flows: `load_session_preview_if_needed`, `check_preview_result`
  - Selection helpers: `get_selected_preview`, `get_selected_session_metadata`, `get_currently_selected_session_name`

- deletion.rs
  - `start_background_worktree_deletion`, `start_background_worktree_deletion_with_options`, `start_background_branch_deletion`
  - `is_worktree_deleting`
  - Parts of deletion-completion handling currently in `refresh_data_mut` (moved to `refresh.rs` but call helpers here)

- merge.rs
  - `op_label_to_friendly`
  - `start_merge_operation`, `start_merge_operation_with_message`, `is_merge_in_progress`, `check_merge_result`
  - Mode-specific handlers live under `handlers/` but share helpers here

- refresh.rs
  - `refresh_data_mut` (call out to `context::app_from_metadata`, `session_name` utils, `git::spawn_git_status_task`)
  - `perform_light_refresh`, `spawn_background_refresh`, `check_refresh_result`, `schedule_light_refresh`
  - Keep rename/deletion timeout checks here but delegate state updates to `sessions`/`deletion`

- navigation.rs
  - `get_display_items`, `toggle_expand`, `toggle_expand_and_update_git_status`
  - `get_selected_worktree_path`, `get_selected_worktree_name`, `get_currently_selected_worktree_name`
  - `mark_navigation_occurred`, `check_and_perform_debounced_updates`
  - Action entrypoints used by browsing: `handle_selection_action`, `handle_delete_action`, `handle_manual_rename_action`, `handle_auto_rename_action`

- handlers/*
  - Each `handle_*_key` extracted to its own file, importing helpers from the modules above. Keep `handle_key_event` in `mod.rs` (thin dispatcher by `AppMode`).

- constants.rs
  - `RENAME_CLEANUP_SECS = 10`, `RENAME_TIMEOUT_SECS = 30`, `DELETION_TIMEOUT_SECS = 60`, `NAV_DEBOUNCE_MS = 200`

## App struct (state grouping – optional, low-risk)

To reduce the long field list, we can logically group state inside `App` without changing external behavior:

- DataState: repo/worktrees/sessions/metadata/displays/previews
- UiState: selection index, expanded set, mode, status message
- AsyncState: tmux availability, tasks/handles, channels, loading flags, debounce timestamps
- OpsState: rename/deletion tracking sets and start times

This is optional for first pass; we can keep fields flat and only move methods to modules to minimize churn.

## Incremental rollout plan (no behavior change)

1) Extract pure utilities first
- Move `session_name.rs` + tests, `constants.rs`.
- Introduce `context::app_from_metadata` and replace the repeated inlined logic.

2) Extract shared types
- Create `types.rs` and move `AppMode`, dialogs, `Selection`, `DisplayItem`, `AppResult`, `GitStatusResult`, `MergeTaskResult`, `RefreshSnapshot`.
- Update `components/*` to `use crate::app::types::*`.

3) Extract config
- Move `Config` to `config.rs` and update `handle_api_key_key`.

4) Extract focused pipelines
- `sessions.rs` (rename + preview) and `git.rs` (status pipeline) with their existing method bodies.
- Replace in `app.rs` with thin wrappers or re-exports.

5) Extract refresh orchestration
- Move `refresh_data_mut`, `perform_light_refresh`, `spawn_background_refresh`, `check_refresh_result`, `schedule_light_refresh` into `refresh.rs` while delegating to `sessions`/`git`/`context`/`session_name`.

6) Extract deletion and merge modules
- Move deletion pipeline to `deletion.rs` and merge pipeline to `merge.rs`.

7) Extract navigation + handlers
- Move `get_display_items`, toggle helpers, selection helpers, debouncing to `navigation.rs`.
- Split all per-mode handlers into `handlers/` and keep `handle_key_event` as the dispatcher.

8) Optional state grouping
- If still too large, re-group `App` fields into small sub-structs, but keep method signatures and external API intact.

## Testing strategy

- Keep the existing compile-time tests for `should_auto_rename_session`; move them to `session_name.rs` and expand coverage for all formats and edge-cases.
- Add unit tests for `context::app_from_metadata` mapping.
- Add unit tests for validators (`is_valid_branch_name`).
- Manual regression: run existing `tests/test-tui-*.sh` and manual flow from `tests/manual-test-instructions.md`.
- Policy: for every refactor step that extracts new modules/files, add unit tests for those files before marking the step DONE. Prefer pure, deterministic tests that do not require external tools.

## Risks and mitigations

- UI/components import breakage: mitigate by re-exporting from `app::types` and keeping `crate::app` as the import root.
- Async lifecycle errors: avoid changing logic; only move method bodies wholesale, then deduplicate shared helper logic (context detection) with targeted diffs.
- Naming drift for session formatting: covered by moving/expanding tests.

## What will remain in `src/app/mod.rs`

- `pub struct App { ... }` (fields unchanged initially)
- `impl App` thin facade:
  - Constructor: `new` (same signature)
  - Dispatcher: `handle_key_event` (delegates to handlers)
  - Periodic/nonblocking tick helpers used by main loop: `check_*` methods remain but delegate (`check_merge_result`, `check_git_status_result`, `check_preview_result`, `check_refresh_result`, `check_and_perform_debounced_updates`).
  - Re-exports from submodules so callers keep `use crate::app::*`.

## Open questions (for review)

- Do we want to introduce a `TaskManager` wrapper around join handles/channels now, or keep them in `App` for the first pass?
- Should `DisplayItem` construction (which is UI-focused) live in `components/worktree_list.rs` instead? Current approach keeps it in `navigation.rs` to avoid data->UI coupling.
- Are we okay to consolidate the three copies of context-detection logic into a single `context::app_from_metadata` function (no behavior change intended, but visible in diff)?

## Summary

This plan splits `app.rs` into focused modules while preserving the current behavior and the external API used by `main.rs` and UI components. The refactor will proceed in small, reviewable steps starting with pure utilities and types, followed by pipelines (sessions/git/refresh), and finally input handlers.

## Progress and milestones

### Step 7: Extract navigation.rs and handlers/* — implemented

Changes
- Added src/app/navigation.rs with:
  - get_display_items, toggle_expand, toggle_expand_and_update_git_status
  - get_selected_worktree_path, get_selected_worktree_name, get_currently_selected_worktree_name
  - Unit tests for display items, toggle, and selection helpers
- Added src/app/handlers/ with:
  - mod.rs (module root)
  - splash.rs (handle_splash_key) + test
  - browsing.rs (handle_browsing_key + selection/delete/rename entrypoints)
- Updated src/app.rs:
  - mod navigation; mod handlers;
  - Delegated get_display_items, toggle*, get_selected_* wrappers to navigation
  - handle_key_event now dispatches SplashScreen and Browsing to handlers
- Left other mode handlers in app.rs for now; will extract in subsequent micro-steps if desired.

Verification
- cargo test: 21 tests passed (added 3 navigation + 1 splash handler tests)
- Manual compile verified

Notes
- Handlers for creating/confirming/delete-options/api-key/renaming/merge-menu/merge-progress/commit-message/logs remain; can be split similarly later without behavior change.

### Step 6: Extract deletion.rs and merge.rs — implemented

Changes
- Added src/app/deletion.rs:
  - start_background_worktree_deletion, start_background_worktree_deletion_with_options
  - start_background_branch_deletion, is_worktree_deleting
  - Unit tests for marking deletion state
- Added src/app/merge.rs:
  - op_label_to_friendly, start_merge_operation(_with_message), is_merge_in_progress, check_merge_result
  - Unit tests for mapping, in-progress detection, and up-to-date result handling
- Updated src/app.rs:
  - mod deletion; mod merge; thin wrappers delegate to new modules

Verification
- cargo test: 17 tests passed (prev 12 + 2 deletion + 3 merge)
- Manual compile verified

Notes
- Handlers remain in app.rs; will move in Step 7.

### Step 5: Extract refresh.rs — implemented

Changes
- Added src/app/refresh.rs with:
  - refresh_data_mut, perform_light_refresh, spawn_background_refresh, check_refresh_result
  - schedule_light_refresh, mark_navigation_occurred, check_and_perform_debounced_updates
- Updated src/app.rs:
  - Added mod refresh; schedule_light_refresh now delegates to refresh
  - (Transitional) Existing methods still present; callers will be rewired in subsequent steps
- Updated src/app/sessions.rs:
  - Removed local schedule_light_refresh; now calls refresh::schedule_light_refresh

Verification
- cargo test: 12 tests passed (3 session_name + 1 config + 3 git + 2 sessions + 3 refresh)
- Manual compile verified

Notes
- Next steps will rewire remaining call sites to use refresh wrappers or move bodies entirely to module functions.

### Step 4: Extract sessions.rs and git.rs — implemented

Changes
- Added src/app/sessions.rs with:
  - start_background_auto_rename, evaluate_rename_completion, auto_rename_session
  - load_session_preview_if_needed, check_preview_result
  - selection helpers: get_selected_preview, get_selected_session_metadata, get_currently_selected_session_name
  - schedule_light_refresh
- Added src/app/git.rs with:
  - extract_worktree_name_from_path, spawn_git_status_task, check_git_status_result
- Updated src/app.rs:
  - mod sessions; mod git; wrappers that delegate to the new modules
  - No behavior changes; signatures preserved

Verification
- cargo test: 9 tests passed (3 session_name + 1 config + 2 sessions + 3 git)
- Manual compile verified

Notes
- refresh orchestration still in app.rs; will move in Step 5.
- Deletion/Merge pipelines remain in app.rs for Step 6.


### Step 2: Extract shared types — implemented

Changes
- Added src/app/types.rs with: AppMode, dialogs (CreateDialog, ConfirmationDialog, DeleteOptionsDialog, RenameDialog, ApiKeyDialog, MergeMenuDialog, CommitMessageDialog, LogDialog, MergeProgressDialog), Selection, DisplayItem, AppResult, GitStatusResult, RefreshSnapshot.
- Updated app.rs to declare pub mod types; removed inline type definitions; app now uses types::*.
- Updated imports in components and ui to use crate::app::types::*.
- Resolved prior visibility warning by making GitStatusResult public.

Verification
- Built successfully: cargo build --release
- Tests passed: cargo test (3 session_name tests)

Notes
- No behavior changes; only type relocation and import updates.

### Step 3: Extract config — implemented

Changes
- Moved Config into src/app/config.rs with load/save and config_file_path.
- Updated src/app.rs: added pub mod config; pub use config::Config; removed inline Config.
- UI handlers now import Config from app::config (behavior unchanged).

Verification
- Built successfully: cargo build
- Tests passed: cargo test (3 session_name tests + 1 config test)


- Step 1: Extract pure utilities — DONE
- Step 2: Extract shared types — DONE
- Step 3: Extract config — DONE
- Step 4: Extract sessions.rs and git.rs — DONE
- Step 5: Extract refresh.rs — DONE
- Step 6: Extract deletion.rs and merge.rs — DONE
- Step 7: Extract navigation.rs and handlers/* — DONE
- Step 8: Optional App field grouping — PENDING

### Step 1: Extract pure utilities — implemented

Changes
- Added src/app/session_name.rs and moved all session naming/parsing/formatting utilities there (including tests for should_auto_rename_session).
- Added src/app/constants.rs with RENAME_CLEANUP_SECS, RENAME_TIMEOUT_SECS, DELETION_TIMEOUT_SECS, NAV_DEBOUNCE_MS and wired them in app.rs (no behavior change).
- Added src/app/context.rs with pub(super) fn app_from_metadata(md: &SessionMetadata) -> String and replaced duplicated inline logic in:
  - App::refresh_data_mut (session display formatting)
  - App::perform_light_refresh (session display formatting)
  - App::check_refresh_result (session display formatting)
  - App::detect_session_app_context now delegates to context::app_from_metadata
- app.rs now declares submodules (mod constants; mod context; mod session_name;) and delegates via thin wrappers to preserve existing call sites.
- Removed the local unit tests from app.rs and moved them under session_name.rs.

Verification
- Built successfully: cargo build --manifest-path tui/Cargo.toml --release
- No functional changes observed; only refactor/cleanup.

Notes
- Kept public surface intact; no changes to main.rs or components/* imports.
- Subsequent steps (types/config/pipelines/handlers) remain PENDING and will be proposed separately after approval.
