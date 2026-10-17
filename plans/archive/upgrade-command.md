# Upgrade Command Plan

## Goal
Add a new CLI command `upgrade` so users can run `orchestra upgrade` to detect install type (Homebrew vs npm) and check for available updates to the app.

## Assumptions
- The CLI entry point that routes commands already exists (e.g., `orchestra` wrapper or CLI script).
- Install types to detect are Homebrew and npm; if neither is detected, we should report an actionable message.

## Proposed Behavior
- `orchestra upgrade`:
  - Detect install source:
    - Homebrew: confirm `brew` is installed and `orchestra` is a Homebrew formula or cask.
    - npm: confirm `npm` or `pnpm` is installed and `orchestra` was installed globally.
  - Report detected install type.
  - Check for updates:
    - Homebrew: `brew update` (optional) then `brew outdated` for the formula/cask.
    - npm: `npm outdated -g` (or `pnpm outdated -g` if pnpm detected) and check the `orchestra` entry.
  - If update is available, print the command to upgrade.
  - If not, confirm it is up to date.

## Implementation Steps
1. Locate the CLI command router and determine how new subcommands are added.
2. Add `upgrade` subcommand wiring with help text and examples.
3. Implement install-type detection helpers:
   - Homebrew detection:
     - `command -v brew`
     - `brew list --formula` or `brew list --cask` contains `orchestra` (choose based on existing packaging).
   - npm detection:
     - `command -v npm` (or `pnpm`)
     - `npm ls -g --depth=0` contains `orchestra` (and/or `pnpm list -g --depth=0`).
4. Implement update checks:
   - Homebrew: parse `brew outdated` output for `orchestra`.
   - npm/pnpm: parse `outdated -g` output for `orchestra`.
5. Add user-facing output:
   - Clear detection message.
   - Upgrade instructions for each install type.
   - Fallback guidance if no install type is found.
6. Add tests or manual test instructions (if repo has CLI tests):
   - Mock `brew`/`npm` command presence and outputs.

## Edge Cases
- Both Homebrew and npm installs present: prefer the one matching `which orchestra` path or show a prompt/choice.
- Homebrew without network access: allow skipping `brew update` and just check `brew outdated`.
- npm installed via nvm: global path detection should still work.
- `orchestra` installed as a symlink or wrapper script: ensure detection still resolves install type.

## Files to Touch (expected)
- CLI command router (entry point).
- New helper module for install detection and update checks (if applicable).
- Tests or docs for the new command.

## Open Questions
- Is `orchestra` packaged as a Homebrew formula or cask in this repo?
- Do we need to support additional install methods (e.g., direct binary download) now or later?
