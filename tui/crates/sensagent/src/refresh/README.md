# Scheduled AI-status refreshing

This public module runs the [dispatch coordinator](../dispatch/README.md) for one
Rakumux home: it follows the daemon's sessions, decides when each one is refreshed, and
turns [scoped captures](../source/README.md) and requests into cached status. It is
**not** an installed per-home service, lock, IPC endpoint or TUI feature. Nothing here
chooses a home, opens a store or credential file, or accepts connections.

## Parts

- `RefreshScheduler` (crate-internal) is pure timing state over an injected monotonic
  `now`: per-session interval, error backoff, quiet period, global start budget, a
  rate-limit pause and an unauthorized halt.
- `StatusRefresher` is the synchronous engine. It owns the coordinator and its
  scheduler: `resync`/`apply_event`/`apply_view` feed daemon observations, `tick`
  returns the captures due now, `captured` builds and commits one, `completed` applies a
  finished request. Account and consent controls go through it, so every session is
  reconsidered after one. Tests drive it with fake time.
- `RefreshRunner` gives the engine threads: an owner thread (every coordinator call
  happens there; it ticks every second), a source thread following the daemon, two
  workers for captures and requests, and one for replies being typed in and replies and
  session messages written in a TUI, so those never wait behind file downloads, captures or
  chat uploads. `RefreshClient` sends controls and status queries
  to the owner thread and waits for its answer.

## Scheduling

Proposed defaults from the [feature plan](../../../../../plans/sensagent/sign-in-and-ai-status.md#generation-and-privacy-policy),
as constants:

| Rule | Value |
|---|---|
| Captures per session | one per `SESSION_INTERVAL` (60 s), whatever started it |
| Jobs in flight | `MAX_RUNNING` (2), the coordinator's slot limit |
| Captures started, all sessions | `BUDGET_STARTS` (12) per `BUDGET_WINDOW` (60 s) |
| After an unreadable session or a failed request | that session waits `ERROR_BACKOFF` (60 s), doubling with each failure in a row up to `MAX_ERROR_BACKOFF` (16 min) |
| Rate limited by the service | every session waits the suggested delay, at least 60 s |
| Account refused (401/403, expired credentials) | nothing refreshes until the account or consent changes |
| Input that can't be sent | not captured again until the session changes |

While a rate limit or refusal holds, a capture already in flight is acknowledged
instead of sent.

- **Unchanged input is skipped by stamp.** A session is captured only when its source
  stamp changed since it was last attempted or decided, or output is pending. A
  session the policy declined (cached text, no context, private, …) settles at that
  stamp. Output is ignored in unselected and reports-only folders, where it can't
  change the input, until the stamp itself changes. Elsewhere captured text is not
  compared: a redrawn spinner still counts as new output.
- **Busy terminals.** Output only marks a session dirty, and its cached text stays
  visible. It is refreshed once output pauses for `QUIET_PERIOD` (5 s), or after a whole
  interval of continuous output, and never inside the interval. The stamp is advanced
  only when the refresh starts.
- **Followed turns.** A session whose agent turn the daemon follows is refreshed at once
  at each of the turn's trigger points (see the source tracker), never for terminal
  output; backoff, the budget and the two slots still apply. The turn settings are read
  again every minute (`with_turn_settings`).
- **Order.** Sessions whose stamp changed at once (lifecycle such as blocked/done, the
  session's report, a new terminal, privacy or folder) come first, then sessions with
  pending output; within each, the one refreshed longest ago, new sessions first.
  Lifecycle changes hide old text at once; the new request still waits for the
  session's interval and the budget.
- **Backfill** after consent or startup is bounded by the budget and the two slots: 12
  sessions a minute at most.
- Consent and account controls (`enable`, `disable`, `set_account_session`,
  `sign_out`) clear every settled decision, failure and the unauthorized halt. Per-session
  intervals, backoff, the budget window and a rate-limit pause remain.

## Following the daemon

The runner needs a **user** SessionApi client (a session caller is refused at
`spawn`; capture refuses one too). It subscribes to every folder first, then lists
them, so no change falls between the two; an event that was already in the list may
be applied again, which captures' before-and-after checks catch. Until the first list
arrives, and after the stream ends, nothing is captured. On the end of a stream it
subscribes and lists again after 250 ms, doubling to 5 s. The daemon drops a
subscriber that falls behind; that ends the stream the same way.

A resync assumes output was missed: every live session is refreshed once its interval
allows. With many sessions this costs up to a minute of budget per 12 sessions after
each daemon restart; `last_active_at` can't narrow it, as it records use, not output.

After a capture finds the session `Changed`, the worker reads the session again and the
owner applies that view, so wrong facts (an event replayed after a list) can't block the
session forever. If an event or list for that session arrived while the job ran, the view
may be older than it and is dropped.

## Titles

Requests ask for a session title too (`?title=1`). When a finished request's text applies
and the service sent a title, `completed` returns a `Retitle` (`titles.rs`) that a worker
runs: it renames the session through the daemon (`session_rename`, as the user) only while
its name is the generated one (`auto_named`) or exactly the title this service last gave it
(`titles.json`, 0600), or when the user asked (`request`). A title another session of the
folder has gets ` 2`, ` 3`, …; one taken meanwhile is chosen again once. A service without
titles names nothing. A removed session's title is forgotten.

## Failures and threads

- `RefreshFailure` is static and per session, shown until a refresh succeeds:
  `Unreadable`, `Rejected`, `Unavailable`, `RateLimited` or `Unauthorized`. A capture
  that changed or was canceled, and a capture with no meaningful context, are not
  failures. `RefreshState` reports the connection, a rate-limit pause and the halt.
- A capture that panics, including its correcting read, comes back as unreadable. A
  request that panics is lost: the coordinator keeps its slot fenced (`running` keeps
  the session), as the plan requires for unknown work. Two lost requests stop
  refreshing until the runner restarts.
- `stop` (or dropping the runner) stops the owner thread and waits for it. The
  coordinator is dropped, which cancels outstanding jobs. The source thread and workers
  are not joined, since a daemon or service request can block until its timeout: the
  source thread ends at its next check, a worker in a request at its deadline,
  submitting nothing more.
- Known secrets for redaction are not selected yet (`build_capture` gets none).

## Remaining gates

The [owner](../owner/README.md) gives the refresher a per-home lock, layout and local
socket. Still open: sign-in over that socket, pushing status changes instead of polling
`statuses`, automatic credential refresh, TUI consent controls and third-line
rendering, and live Clerk/TLS/model certification.

## Tests

Scheduler tests are pure. Engine and runner tests in
`cloud/client/tests/dispatch/refresh.rs` use the real coordinator with the fake
SessionApi (`source/fake.rs`, now with `list_all` and queued event streams), the fake
HTTP transport (including canonical error replies), temporary credential files and an
in-memory store. Engine tests use fake time; runner tests use real threads and wait at
most ten seconds for each condition.

```sh
cargo test -p rakumux-sensagent refresh --locked --offline
```

Run from `tui/` with an isolated `RAKUMUX_HOME`, inherited Rakumux identity/socket
variables unset and the crate README's low-debug flags. No daemon, terminal or network
is used.
