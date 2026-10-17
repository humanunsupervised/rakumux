# AI Provider Preference Plan

## Goal
Allow users to enter both Anthropic and OpenAI API keys from the `o` options menu, store a primary AI provider preference, and use a fast/cheap OpenAI model for auto-rename when OpenAI is primary.

## Assumptions
- Config lives in `~/.orchestra/config.json` (current path in `tui/src/app/config.rs`).
- Default primary provider remains `anthropic` to preserve current behavior.
- Default OpenAI model: `gpt-4o-mini` (fast, cheap, capable).
- If primary provider is missing a key or errors, auto-rename falls back to the other configured provider.

## Proposed Behavior
- Press `o` to open the options menu.
- Users can enter:
  - Anthropic API key
  - OpenAI API key
  - Primary AI provider (Anthropic/OpenAI)
  - OpenAI model (only meaningful when OpenAI is primary)
- Auto-rename uses the configured primary provider and model.

## Config Schema
Add fields to `~/.orchestra/config.json`:
```json
{
  "anthropic_api_key": "sk-ant-...",
  "openai_api_key": "sk-...",
  "ai_primary_provider": "anthropic",
  "openai_model": "gpt-4o-mini"
}
```
- Backward compatible: missing fields default to existing behavior.

## UI/UX Changes (Options Menu)
- Add menu entries for:
  - OpenAI API Key
  - Primary AI Provider
  - OpenAI Model
- Provider selection uses a simple list dialog.
- OpenAI model uses a list dialog with curated options; include a default and optionally a “Custom…” input if needed later.

## Auto-Rename Provider Selection
Update `tmux_generate_ai_session_name` to choose provider/model based on config:
- If primary is OpenAI and key exists → call OpenAI with configured model.
- If primary is Anthropic and key exists → call Anthropic (current flow).
- If primary missing key or request fails → fallback to the other provider if configured.
- If no usable keys → return `missing_api_key`.

## Implementation Steps
1. **Config schema updates (Rust):**
   - Extend `tui/src/app/config.rs` with `openai_api_key`, `ai_primary_provider`, `openai_model`.
   - Update defaults + config tests.
2. **Types + menu state (Rust):**
   - Update `tui/src/app/types.rs` with `ApiProvider::OpenAI` and new dialog identifiers for provider/model selection.
3. **Options menu wiring (Rust):**
   - Update `tui/src/app/handlers/browsing.rs` to add menu items and open the correct dialogs.
   - Update `tui/src/app.rs` to save OpenAI key, primary provider, and model to config.
4. **Dialog rendering (Rust):**
   - Update `tui/src/components/dialogs.rs` to render new dialogs and labels.
5. **Services config loaders (Shell):**
   - Add `load_openai_api_key`, `load_ai_primary_provider`, `load_openai_model` in `server/services/json.sh` or a focused service config module.
6. **Auto-rename provider selection (Shell/API):**
   - Update `api/tmux.sh` to:
     - Load provider/model settings.
     - Implement OpenAI API call helper (Responses or Chat Completions).
     - Select provider + fallback logic.
   - Update `server/services/ai.sh` to use the provider-aware `tmux_generate_ai_session_name` and handle `missing_api_key` consistently.
7. **Help text:**
   - Update `shell/gwr_usage.sh` to describe “Configure AI keys” instead of only Anthropic.

## Edge Cases
- Primary set to OpenAI but no OpenAI key → fallback to Anthropic if configured.
- Invalid OpenAI model → handle API error and fallback if possible.
- Empty or invalid keys → show “missing_api_key” message, no rename.
- Ensure no API keys are logged or echoed to stdout.

## Files to Touch (expected)
- `tui/src/app/config.rs`
- `tui/src/app/types.rs`
- `tui/src/app/handlers/browsing.rs`
- `tui/src/app.rs`
- `tui/src/components/dialogs.rs`
- `server/services/json.sh`
- `server/services/ai.sh`
- `api/tmux.sh`
- `shell/gwr_usage.sh`

## Testing / Verification
- Run existing Rust tests: `cd tui && cargo test`.
- Manual test:
  - Set Anthropic key, set OpenAI key, switch primary provider, run auto-rename.
  - Verify fallback behavior when primary key is missing.
  - Confirm `~/.orchestra/config.json` has new fields and round-trips correctly.
