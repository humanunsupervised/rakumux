# App Name Detection Hardening Plan

## Goal
Make auto-rename app prefixes deterministic and reliable by deriving the app name from the last user-started command in the tmux pane, while AI generates only the description.

## Scope
- In scope: app name detection, capture, normalization, and rename prefix wiring.
- Out of scope: changing AI description quality logic (except keeping it description-only).

## Current Pain Points
- App prefix can be missing if no reliable pane command history is available.
- Shell meta commands can become false app names (for example: `source`, `export`).
- Fallback parsing from terminal output can introduce noisy app labels.

## Target Behavior
1. App prefix is always sourced from the last captured command in the pane history store.
2. AI returns description only, never app prefix.
3. If no valid command exists, do not force any app prefix.
4. UI displays `App: Description` only when a valid known app prefix exists.

## Implementation Plan

### 1) Capture Layer (bash + zsh)
- File: `shell/orchestra-command-hook.sh`
- Keep pre-exec hooks (`DEBUG` for bash, `preexec` for zsh).
- Add a dedicated "resolved app" write path per pane:
  - `~/.orchestra/history/<pane_key>.last_app`
  - `~/.orchestra/history/<pane_key>.last_cmd`
- Update these files on every captured command after normalization.

### 2) App Resolver (single source of truth)
- Add a shared shell resolver function (in hook script or new helper module) that:
  - trims leading env assignments
  - strips wrappers: `sudo`, `env`, `command`, `nohup`, `time`
  - ignores shell/meta commands: `source`, `export`, `set`, `alias`, etc.
  - extracts executable token robustly
  - normalizes aliases/variants (`python3 -> python`, `docker-compose -> docker_compose`)
- Keep a documented allowlist/mapping for stable display labels.

### 3) Rename Pipeline Consumption
- File: `api/tmux.sh`
- Read app from `<pane_key>.last_app` first.
- Fallback order:
  1) `<pane_key>.last_app`
  2) parse last line from `<pane_key>.log`
  3) no prefix
- Remove any output-based app inference from the rename path.

### 4) AI Contract
- File: `api/tmux.sh` (OpenAI Responses + fallback prompts)
- Keep AI output as description-only JSON/text.
- Enforce post-processing to strip accidental prefixes from AI output.

### 5) UI Display Rules
- File: `tui/src/app/session_name.rs`
- Only render `App: Desc` when prefix is in known resolver mapping.
- Otherwise render plain description.

### 6) Observability
- Keep footer status with app source:
  - `Session renamed successfully (App source: <token>)`
- Add optional debug services command (future): `detect-app-source <session>`.

## Test Plan

### E2E (shell/tmux mocks)
- File: `tests/test-auto-rename-app-prefix.sh`
- Required cases:
  - `git status` -> `git_...`
  - `nvim file` -> `nvim_...`
  - `opencode ...` -> `opencode_...`
  - `source ~/.zshrc` -> no prefix
  - no history -> no prefix
  - wrappers: `sudo git status`, `env FOO=1 nvim file` -> expected app prefix

### Regression
- Ensure rename still succeeds with OpenAI and Anthropic providers.
- Ensure spinner lifecycle unchanged (spinner until rename result/timeout).

## Acceptance Criteria
- App prefix consistently matches last valid command app in pane.
- No `Terminal` or arbitrary words used as synthetic app labels.
- AI description remains provider-driven; app prefix remains system-driven.
- E2E tests pass and `./build.sh` passes.
