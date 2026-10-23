# SessionApi source tracking and scoped capture

This public module turns what the daemon already publishes into the ordered source
stamps the [status policy](../status/README.md) needs, and reads the bounded plain-text
windows a [dispatch job](../dispatch/README.md) permits. It is **not** a running owner,
scheduler, IPC endpoint or TUI feature. Nothing here subscribes, polls or captures by
itself; the [refresher](../refresh/README.md) drives it.

## Source tracking

Each `StatusDispatchService` owns one source tracker, and daemon observations reach its
policy only through it, so stamps from two trackers can never mix. The owner feeds it:

- `apply_view(&SessionView)` for the initial `list` and any `get`;
- `apply_event(&EventEnvelope)` for every subscribed event;
- `resync(&[SessionView])` with a fresh `list` after a lost or lagging event stream.

Each change is applied to the policy at once and cancels affected jobs.

- **New generation** when a session becomes live or its terminal pid changes (revive,
  restart). A pid the daemon briefly omits is a fact change, not a new terminal.
- **Immediate revision** when liveness, folder (`project_relocate`), privacy, native
  lifecycle or the session's own report changes. Older cached text is hidden at once.
- **Followed turns.** While the daemon follows a session's agent turn
  (`SessionView::agent_turn`), its lifecycle and messages count only through the turn's
  trigger point (`TurnSettings`): a new prompt, every `every` (5) messages while working,
  blocked, and the final reply (lifecycle done or idle; then every message, so a reply read
  after the lifecycle changed counts). Each is an immediate revision; output and activity
  phases mark nothing dirty.
- **Coalesced revision** for output (`activity` events, throttled by the daemon) and
  activity-phase changes. They only mark the session dirty (`is_dirty`); the owner
  calls `advance` when its refresh budget allows another generation. Cached text stays
  visible while a terminal is busy, and capture is not cancelled by every line.
- **Unchanged**: names, titles, sizes, controllers, cwd, git context, pins, labels and
  activity timestamps. They are presentation, not status input.
- When `build_capture` finds no meaningful context in a capture's windows, it records
  that against the job's stamp. Only the sources that capture read are marked empty, so
  a reports-only capture leaves output available if output sharing is enabled later.
  The policy then shows `NoContext` instead of retrying the same input; a new report or
  advanced output makes the session eligible again.
- `Removed` retires a tracked session for the coordinator's lifetime. Removing a dead
  or never-seen session changes nothing and takes no slot: the policy never saw it.
- `resync` marks known live sessions dirty, since output events may have been missed,
  and makes tracked live sessions that are no longer listed not live. It attempts
  every view and applies every update it made before returning the first error.
- Sessions first seen without a terminal are not tracked. Folder and session id get the
  policy's own checks: an invalid view of an untracked session is not tracked, and one
  of a tracked session is refused and, like exhausted counters, stops sharing until
  consent is confirmed again. Tracked and retired sessions share
  the policy's 256 bound; a new session over it is refused without stopping sharing.
- Confirming consent also re-applies every tracked observation, so a policy update that
  failed earlier (for example, a cache deletion) is restored before sharing resumes.

Stamps start again with each coordinator. That is safe because every new coordinator
requires explicit consent confirmation, and confirming consent clears the cache.

## Scoped capture

1. On the owner thread, `owner.expectation(&job)` checks the job's owner, exact stamp
   and folder without daemon requests. Canceled, stale, private or non-live jobs stop.
2. Off the owner thread, `expectation.capture(&api, &job)` uses a **user** SessionApi
   client (a session caller is refused before any request). It reads the session with
   `get`, requires the expected folder, liveness, pid, privacy, lifecycle and report,
   reads `RecentUnwrapped` text (200 rows) only for report-and-output jobs, and reads
   the session again. For a followed turn it reads `session_turn` instead: the prompt and
   the newest `recent` (10) messages while working, every message that fits once finished,
   within 6 KiB so the builder's tail selection keeps the prompt. Any difference, including a saved-scrollback fallback after the
   terminal ended, is `Changed`. Cancellation is checked around every request.
3. Back on the owner thread, pass the capture to `build_capture` with the same job. A
   capture carries its job's local correlation, so another job's capture is refused.
   The coordinator rechecks the job, so a fencing event applied during capture still
   cancels it. On any failure, acknowledge the capture. After `Changed`, refresh the
   session (`get`, then `apply_view`) or wait for its events.

Output written between the reads stays under the job's stamp and marks the session
dirty for a later revision. The status is advisory; fencing facts are reconfirmed.

### Windows

- Report text is the session's `text` and result `summary`; result file lists are never
  read. Terminal text is the daemon's plain `Text` format, never ANSI or checkpoints.
- U+200D, U+FE0E and U+FE0F (emoji joiners and presentation selectors, common in agent
  TUIs) are deleted. In the builder's Unicode patterns they are word characters, so
  deleting them only joins neighbouring text and can only add redaction matches. Every
  other invisible or control character is left for the builder to reject: deleting
  a zero-width space could remove a word boundary a token pattern needs.
- Each window keeps a tail of at most the builder's 64 KiB inspection bound, starting at
  a whole line where one fits.
- A private key whose `BEGIN` line fell outside the window is dropped through its `END`
  line, since the builder's key pattern needs `BEGIN`. Whole lines of 60 or more base64
  characters, optionally after a line number (`cat -n`, editors, `grep -n`), are masked,
  covering key bodies whose markers are both off screen (a pager or `sed` in the middle
  of a key). Shorter fragments, such as a key's last line or one redrawn across a
  terminal under 60 columns wide, can still pass. Bare hex digests are masked too.
- Surrounding blank rows are trimmed; empty windows are omitted.

Redaction stays best effort (see the [native client](../cloud/README.md)). A window can
still cut or miss other secrets; private sessions and unconsented folders are never
read. `SessionCapture` and `CaptureExpectation` Debug output contains no text or bindings,
and `CaptureError` is static: daemon messages are never surfaced.

## Known limits and remaining gates

- A window the builder rejects (`InvalidRequest`, for example a zero-width space, or
  `CaptureTooLarge`) is not recorded against its stamp. The
  [refresh scheduler](../refresh/README.md) settles that stamp instead and shows
  `Rejected` until the session changes.
- Exited sessions stay tracked and retired sessions keep their fence for the coordinator's
  lifetime, both within the policy's 256 bound. A long-running owner needs a reviewed
  compaction, or a restart with consent confirmation, before that bound is reached.
- A missed exit and revive that reuses the same pid, seen only through `resync`, keeps the
  old generation; lifecycle and report checks narrow but do not close that case.
- The subscription loop, reconnects and scheduling are in [refresh](../refresh/README.md).
  Still to build: the per-home owner process and lock, authenticated IPC, known-secret
  selection, TUI controls and third-line rendering. Live Clerk, TLS and model
  certification remain separate.

## Tests

Tracker and window tests are pure. Integration tests under
`cloud/client/tests/dispatch/source.rs` drive the coordinator, its tracker and capture
with a fake SessionApi (`source/fake.rs`), the existing fake HTTP transport, temporary
credential files and an in-memory status store:

```sh
cargo test -p rakumux-sensagent source --locked --offline
```

Run from `tui/` with an isolated `RAKUMUX_HOME`, inherited Rakumux identity/socket
variables unset and the crate README's low-debug flags. No daemon, terminal or network
is used.
