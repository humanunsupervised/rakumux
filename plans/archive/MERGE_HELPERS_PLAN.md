# Merge & Rebase Helpers (TUI "m" Menu) — Design Plan

Purpose: add fast, safe helpers to integrate changes between a non-primary worktree branch and the primary branch without replacing standard git usage. These helpers operate locally, surface conflicts/errors, and never push automatically.

## Goals
- One-key access: press `m` on a non-primary worktree to open helpers
- Four actions (v1):
  1. Merge from Primary → merge primary into the selected branch
  2. Rebase from Primary → rebase selected branch onto primary
  3. Merge Commit to Primary → merge selected branch into primary, create a merge commit
  4. Merge (Squash) to Primary → squash the selected branch into primary as a single commit
- Strict safety: block if dirty, capture and show git errors, pause on conflicts
- No pushes; user decides when/what to push

Note: The "merge into primary and close" variant is intentionally deferred from v1.

## Terminology
- Primary branch: detected from the repository’s default remote HEAD (usually origin/HEAD → origin/main). Fallback order if unknown: `main`, `master`, `trunk`.
- Selected branch/worktree: the non-primary worktree node under the cursor when pressing `m`.
- Remote: default `origin`.

## UX: TUI Flow
- Context: While viewing the worktree list, with a non-primary worktree selected
- Keybinding: `m`
- Dialog: centered modal list with the four options, arrow up/down + Enter to choose
- Busy state: after choosing, show a spinner + “Running <operation>…” and a scrollable log modal (open on error; on success, show brief success status in footer)
- Conflicts: stop, show error/logs, message e.g. “Conflicts detected. Fix them in: <path>. Use git status, resolve, then continue or abort as appropriate.”

## Primary Branch Detection
Add a helper that returns the primary branch name:
- Try: `git symbolic-ref refs/remotes/origin/HEAD` → parse `origin/<branch>`
- If missing: `git remote show origin` and parse “HEAD branch: <branch>”
- Fallback: first found among `main`, `master`, `trunk`
- Optional override later: env `ORCHESTRA_PRIMARY_BRANCH`

## Preconditions & Safety Checks
- Dirty trees block operations that modify them:
  - For (1) and (2): target worktree must be clean (`git status --porcelain` empty)
  - For (3) and (4): primary worktree (if used) must be clean
- If primary worktree does not exist:
  - For (1)(2): operate against remote-tracking ref `origin/<primary>` (after fetch)
  - For (3)(4): ensure/create a primary worktree (using existing ensure worktree helper) to perform the merge safely
- Always `git fetch origin --prune` before reading `origin/<primary>`
- Rebase will leave repo in rebase state on conflicts; we surface and stop

## Operations: Exact Steps
Notation: `WT` = selected worktree path; `BR` = selected branch; `PR` = primary branch; `PR_WT` = primary worktree path if exists/ensured.

Common first step (all):
- `git -C WT fetch origin --prune` (and possibly at repo root as well)
- Detect PR via helper

### 1) Merge from Primary → Merge PR into BR
- If WT dirty → error
- If primary worktree exists and clean: optionally update local PR:
  - `git -C PR_WT pull --ff-only` (surface error if not fast-forward)
- Else rely on remote-tracking:
  - `git -C WT fetch origin --prune`
- Perform merge into feature:
  - Prefer remote-tracking ref: `git -C WT merge origin/PR`
  - If origin/PR unavailable but PR local exists: `git -C WT merge PR`
- Outcomes:
  - Success: merged; show summary
  - Already up to date: show no-op
  - Conflicts: stop and surface logs; user must resolve in WT

### 2) Rebase from Primary → Rebase BR onto PR
- If WT dirty → error
- Fetch as above
- Perform rebase:
  - Prefer remote-tracking ref: `git -C WT rebase origin/PR`
  - Fallback to `git -C WT rebase PR` if needed
- Outcomes:
  - Success: rebased; show new tip
  - Already up to date: show no-op
  - Conflicts: stop; user must resolve with `git rebase --continue` or `--abort` inside WT

### 3) Merge Commit to Primary → Merge BR into PR (create merge commit)
- Ensure PR_WT exists (use existing ensure function); if created, it’s clean by definition
- If PR_WT dirty → error
- Update PR:
  - `git -C PR_WT fetch origin --prune`
  - `git -C PR_WT pull --ff-only` (surface error if not fast-forward)
- Merge BR into PR with merge commit (force a merge commit to make history explicit):
  - `git -C PR_WT merge --no-ff BR`
- Outcomes:
  - Success: merge commit created locally
  - Conflicts: stop and surface; user resolves in PR_WT
- Note: no push. User can push later manually.

### 4) Merge (Squash) to Primary → Squash BR into PR (single commit)
- Ensure PR_WT exists (use existing ensure function)
- If PR_WT dirty → error
- Update PR:
  - `git -C PR_WT fetch origin --prune`
  - `git -C PR_WT pull --ff-only`
- Squash merge changes from BR into PR working tree and index:
  - `git -C PR_WT merge --squash BR`
- If conflicts: stop and surface; user resolves in PR_WT
- If staged changes present, create a single commit:
  - `git -C PR_WT commit -m "Squash merge ${BR} into ${PR}"`
  - If nothing to commit (already integrated): treat as up_to_date
- Outcomes:
  - Success: one commit created on PR with combined changes
  - Conflicts: stop and surface; user resolves in PR_WT
- Note: no push. User can push later manually.

## Error/Conflict Surfacing
- Capture stdout/stderr and exit codes for each step
- Return JSON with `ok: false`, `error_type`, `message`, and `logs` (array of lines)
- TUI shows a log modal with the accumulated logs and a concise next-steps hint per error type:
  - `dirty_worktree`: “Commit/stash or clean the worktree: <WT>”
  - `ff_only_failed`: “Primary cannot fast-forward. Resolve on primary, then retry.”
  - `merge_conflict` / `rebase_conflict`: “Resolve in <path>; then continue/abort as appropriate.”
  - `no_primary_detected`: “Could not detect primary; set ORCHESTRA_PRIMARY_BRANCH.”

## Services & API Changes

### New git.sh helpers
- `git_primary_branch()` → echoes primary branch name; non-zero if undetectable
- `git_is_worktree_clean <path>` → 0 if clean, 1 otherwise
- `git_pull_ff_only <path>` → safe update of currently checked out branch
- `git_merge_into <target_path> <ref>` → run `git -C target merge <ref>`
- `git_rebase_onto <target_path> <upstream_ref>` → `git -C target rebase <ref>`
- `git_squash_merge_into <primary_path> <branch>` → run `git -C primary_path merge --squash <branch>` and then commit if needed
- `git_ensure_primary_worktree` → wrapper around existing ensure for PR

### New services.sh endpoints (return JSON)
- `primary-branch` → `{ ok, primary }`
- `merge-from-primary --worktree <WT> --branch <BR>` → merges `origin/PR` into BR at WT
- `rebase-from-primary --worktree <WT> --branch <BR>` → rebases BR onto `origin/PR`
- `merge-into-primary --worktree <WT> --branch <BR>` → ensures PR_WT, pulls FF-only, merges BR with `--no-ff`
- `squash-into-primary --worktree <WT> --branch <BR>` → ensures PR_WT, pulls FF-only, runs `--squash` and commits

Shape (success):
```json
{
  "ok": true,
  "operation": "merge-from-primary",
  "primary": "main",
  "target_branch": "feature/foo",
  "target_path": "/abs/path",
  "logs": ["$ git fetch origin --prune", "Already up to date"],
  "result": "merged|rebased|squashed|up_to_date"
}
```
Shape (error):
```json
{
  "ok": false,
  "operation": "rebase-from-primary",
  "error_type": "rebase_conflict",
  "message": "Resolve conflicts and run 'git rebase --continue' in /abs/path",
  "logs": ["...full stderr/stdout lines..."]
}
```

## Rust TUI Changes (tui)

Files touched: `tui/src/app.rs`, `tui/src/components/dialogs.rs`, `tui/src/components/worktree_list.rs`, `tui/src/api.rs`, `tui/src/ui.rs`.

- Keybinding:
  - In worktree list navigation, when selection is a non-primary worktree, `m` opens the Merge Menu
- New App mode:
  - `AppMode::MergeMenu { worktree_slug, worktree_path, branch, selected_index }`
- Dialog UI:
  - New modal list component in `dialogs.rs` with 4 items (merge/rebase/merge-to-primary/squash-to-primary)
  - Reuses existing popup styling; size ~60% width, 10–14 lines tall
- Action handling:
  - On selection, spawn async task to call services endpoint
  - While running, set a Busy sub-mode with spinner and a hidden buffer of logs
  - On error: open a scrollable log modal; ESC closes modal, keeps user in place
  - On success: brief footer success and refresh data for all operations
- Primary detection in UI:
  - Fetch once per Menu open; if unavailable → show error and disable actions

## Edge Cases
- No remote `origin`: surface `no_remote` error, provide guidance to set remote
- Detached HEAD in WT: block and show error
- Primary cannot fast-forward during `pull --ff-only`: surface and stop
- Missing `origin/PR`: after fetch, if still missing, surface `no_primary_detected`
- Nothing to commit after `--squash`: treat as up_to_date
- Enormous histories: operations are local and should be quick; log output truncated to last N lines in modal if needed

## Testing Plan
- Unit-like shell tests (new):
  - Create temp repo, configure `origin`, create PR/BR, simulate changes
  - Verify each endpoint behavior: success, up-to-date, dirty-blocked, conflict paths, squash commit created
- TUI manual tests:
  - New docs in `tests/test-merge-helpers.md` with step-by-step scenarios
  - Keyboard: `m`, select each action, verify dialogs and logs

## Non-Goals / Out of Scope
- Automatic pushes to remote
- Auto conflict resolution or auto-continue rebase/merge
- Changing repository policies (fast-forward-only etc.)

## Implementation Order
1) `api/git.sh`: add helpers (primary detection, cleanliness, primitives)
2) `services.sh`: add endpoints with structured JSON + logs
3) `tui`: Merge Menu dialog + keybinding + async calls + log modal
4) Tests and docs

## User Guidance Text (surfaced on errors)
- Dirty WT: “Your worktree has uncommitted changes. Commit/stash or clean it, then retry.”
- Rebase conflict: “Resolve conflicts in WT, then `git rebase --continue` or `--abort`.”
- Merge conflict: “Resolve conflicts in WT/PR_WT, commit the merge, then refresh.”
- Primary FF failure: “Primary cannot fast-forward; fix on primary branch, then retry.”

---
This plan adds convenient, explicit helpers that respect git safety, make no remote changes, and integrate cleanly with the existing services and TUI architecture.