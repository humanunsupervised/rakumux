# Plan: Rust-based env file copy for worktrees

## Goal
- Fully replace shell-based `.env*` copying with a Rust implementation used by both TUI and CLI flows, and remove the shell copy system once migration is complete.

## Current flow (summary)
- CLI: `orchestra-cli.sh` → `shell/git/*` → `copy_env_files` in `shell/env/copy_env_core.sh`.
- TUI: `tui/src/app.rs` → `ServicesApi.copy_env_files` → `services.sh` → `shell/env/*`.

## Proposed Rust implementation
### Design
- Add a Rust helper that implements the same semantics as `copy_env_files`:
  - Inputs: `source_root`, `target_root`, `repo_root` (optional), `copy_locations` (default `[/]`).
  - For `/`: recursively find files named `.env*` and copy to `target_root` preserving relative paths.
  - Exclude: `.git`, `worktrees`, and `target_root` (to avoid self-copying).
  - If both `source_root` and `repo_root` are provided, prefer the first base that yields any `.env*` matches (mirror shell behavior).
- Emit a clear result (e.g., JSON with `copied`, `failed`, `files[]`) and a non-zero exit code on fatal errors.

### Location
- Option A (recommended): new Rust binary `env-copy` under `tui/` (or a new `crates/env_copy` workspace) with minimal dependencies.
- Option B: module inside `tui` and a CLI subcommand (less ideal for CLI reuse).

## Integration plan
1. Implement Rust copy logic (module + unit tests).
2. TUI path:
   - Replace `api.copy_env_files` call in `tui/src/app.rs` with direct Rust copy call.
   - Continue using `RepoConfig::effective_copy_locations` for config parity.
3. CLI path:
   - Replace `copy_env_files` usage with a direct call to the Rust binary.
   - Update `build.sh` to build/install the Rust binary alongside `tui`.
4. Services path:
   - Route `copy-env-files` to the Rust binary to avoid shell copy logic.
5. Cleanup (full replacement):
   - Delete shell copy implementation:
     - `shell/env/copy_env_core.sh`
     - `shell/env/copy_env_load.sh`
     - `shell/env/copy_env_locations.sh`
     - `shell/env/copy_env_logging.sh`
     - `shell/env/copy_env_state.sh`
     - `shell/env/copy_env_constants.sh`
     - `shell/env/copy_env_command.sh`
     - `shell/env/copy_env_debug_parse.sh`
     - `server/services/copy_env.sh`
     - `copy_env.sh`
   - Remove shell wiring:
     - Drop `copy_env_init` sourcing in `orchestra-cli.sh` and `services.sh`.
     - Remove `copy-env` command wiring in `orchestra-cli.sh` (or re-point to Rust binary).
     - Remove `copy-env-files` services command routing if no longer used.
   - Remove/replace tests that target shell copy:
     - `tests/test-copy-env.sh`
     - `tests/test-copy-env-recursive.sh`
   - Update packaging scripts that include `copy_env.sh`:
     - `install/scripts/test-install.sh`
     - `install/scripts/publish-npm.sh`
     - `install/scripts/build-release.sh`

## Behavior compatibility checks
- Preserve `.orchestra/options.json` semantics (`copy_locations`, default `/`).
- Preserve `.env*` matching and pruning logic.
- Ensure relative paths remain identical to shell output.

## Tests
1. Unit tests for Rust helper:
   - Copies root + nested `.env*`.
   - Skips `.git/` and `worktrees/`.
   - Preserves relative paths.
2. Integration tests:
   - Update or add `tests/test-create-env-subdir.sh` to verify Rust path (when binary is present).
   - Add a focused test that exercises `env-copy` directly with a temp repo.
3. E2E tests (note):
   - Add an end-to-end test that creates a worktree via CLI and TUI and verifies nested `.env` files are copied in the new worktree.

## Rollout / verification
- Build via `build.sh` and confirm `env-copy` is produced.
- Run CLI flow from a subdirectory: `orchestra-cli create <branch>` → verify nested `.env` files copied.
- Run TUI flow: create worktree and verify same behavior.

## Risks / mitigations
- Divergence from shell semantics → mirror shell rules and add parity tests.
- Build/install gaps → ensure `build.sh` produces the binary and CLI/TUI depend on it directly.
