# Orchestrator Performance Analysis

## Completed milestones

- Native Rust TUI (orchestra) as the primary interface
  - Replaced fzf-driven shell UI with a compiled ratatui + crossterm app
  - Faster startup and consistently responsive interactions

- Non-blocking, async-first architecture in the TUI
  - Heavy operations (git status, tmux preview, AI rename, merge helpers) run in background tasks
  - UI never blocks; results flow back via channels and force redraws only when needed

- Adaptive event loop and targeted updates
  - Dynamic poll timeouts (short while work is pending; long when idle)
  - Debounced selection updates to avoid unnecessary refresh work
  - “Light refresh” path updates just sessions/metadata for the current worktree

- Work and data scoping for less I/O
  - Load tmux sessions only for expanded worktrees
  - Only compute session previews for the currently selected session
  - Cache and reuse session metadata to format display names with context

- Background refresh snapshots
  - Periodic, off-thread services calls aggregate repo/worktrees/sessions/metadata
  - Apply snapshots atomically; re-render once per snapshot

- Robust task cancellation on navigation
  - Abort in-flight tasks (git status, previews, renames, deletions) when the user navigates
  - Prevents stale work and reduces CPU churn during fast movement

- Git status moved off the hot path
  - Async load with a small status message, then a single UI update on completion
  - Clears/updates only when selection changes or a refresh is scheduled

- Tmux session listing optimizations
  - Hash + slug prefixes used to limit listings to the selected worktree
  - Sorting by last-attached/activity done in the tmux pipeline to return ready-to-display lists

- Auto-rename correctness and UX polish (this cycle)
  - Fixed tmux delimiter splitting to support multi-char delimiter reliably
  - Standardized on "__" delimiter and consistent orchestra prefix handling
  - Prefix-based rename tracking now matches tmux-format precisely
  - Spinner now appears immediately during AI rename and persists until completion
  - Removed “(auto-renaming…)” text; shows only the spinner for a cleaner, cheaper redraw

## Pending milestones

- Reduce services process spawn overhead
  - Consider a persistent services/daemon or batched endpoints for multi-call flows
  - Add combined endpoints (sessions + metadata) for the non-snapshot path

- UI render micro-optimizations
  - Precompute and cache display spans where possible to reduce per-frame allocations
  - Maintain a small LRU for formatted names/labels during frequent redraws

- Smarter refresh orchestration
  - Adaptive background refresh cadence (lengthen when idle, shorten on activity)
  - Diff-aware apply for snapshots to minimize UI work

- Large-scale stress validation
  - Validate with 100+ worktrees and hundreds of sessions
  - Record input latency and redraw cadence; ensure no regressions under load

- Lightweight telemetry (opt-in)
  - Measure timings for services calls and UI phases to guide future tuning

- Services/API batching and error resilience
  - Batch session-metadata calls where feasible; unify snapshot + regular flows
  - Harden error propagation and partial-result handling to avoid full refresh retries

## Notes

- The rename flow is fully non-blocking: UI returns instantly from tmux, then AI rename runs in the background with a spinner and completes without user intervention.
- All logic now assumes the "__" delimiter; legacy single-underscore parsing was removed to simplify parsing and reduce edge-case costs.
