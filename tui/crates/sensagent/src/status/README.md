# Local AI-status policy and private SQLite store

This public Rust module implements local consent, advisory-cache ownership and
stale-result fencing. It performs **no network, terminal, registry or TUI operations**.
It is not a running per-home owner, IPC permission boundary or refresh scheduler.
Hosted implementation stays in private `humanunsupervised/rakumux-cloud`.

## Ownership and data

- `AccountSessionGate` rechecks an owned, verified/persisted `AccountSession`
  through `AccountService`, including expiry/revision and `ai:session-status` scope.
  A disk account string or UI signed-in boolean is not sufficient.
- `AISessionStatusService` requires an explicitly injected store and trusted account
  gate. The future single local owner must authorize user-only consent mutations
  and obtain canonical session observations through `SessionApi`.
- `SqliteStatusStore::open(directory)` requires an explicit dedicated directory
  under an existing trusted parent. It reads no environment or default home. Its
  files are `status.sqlite3`, `status.lock` and SQLite's temporary rollback journal.
  The installed Rakumux-home layout remains unwired; this library does not open
  the daemon's `rakumux.sqlite3`.
- The database contains consent and bounded advisory text with local account,
  session, folder and source bindings. It receives no OAuth credentials, report
  bodies, raw terminal output, provider keys or full cloud request bodies.

## Consent and freshness

The initial policy is disabled. Explicit enable binds the verified issuer/client/
subject and **exact credential revision**, with 1–64 unique canonical folders.
Reports-only selection never permits terminal output as a fallback. No folder is
implicitly added, and private/non-live/no-context sources produce no job.

Consent mutations use compare-and-swap. Every successful change advances the
revision and clears all cache in one transaction. An old window cannot overwrite
a newer opt-out. Enable also requires the exact account binding shown in the
consent action, so an account switch cannot silently retarget an old UI intent.
Cache insertion atomically checks the same consent revision,
account, credential revision and selected folder.

Tickets belong to one service instance. Their fresh public 128-bit correlation is
unrelated to local account/session/cache identifiers. Terminal incarnation, source
revision, privacy, account, credential revision and consent are checked before
preparation/completion and cache exposure; auth/consent are rechecked after cache I/O.
Old source updates cannot replace newer ones. Inconsistent same-stamp updates
poison that observation; privacy tightening can exclude immediately.
`remove` permanently retires that session ID for the current owner, even when a
removed event arrives before its source. It is not an offscreen eviction operation.
Retirement fences count against the 256-observation bound and are never silently
dropped; safe producer-generation resynchronization remains future work.

The worker must call `is_current` again before capture/submission. The future owner
must drive the [dispatch coordinator](../dispatch/README.md), which serializes local
commitment with revocation and cancels revoked workers. This library
does not claim it can cancel an accepted remote request or prevent its billing.
Two unacknowledged jobs occupy slots even after invalidation; a slot is freed only
on completion or after a caller acknowledges that work stopped via `abandon`.

Cached status is never native lifecycle or an execution result. Stale/private/
excluded/account-mismatched views contain no old text. Source observations and
cache rows are capped at 256; these are local bounds, **not hosted spending quotas**.
`StatusText` enforces the canonical one-line/140-codepoint Unicode rules, with
redacted Debug and validated serde. Rust excludes surrogate scalars; malformed
JSON surrogate sequences reject before text construction. Full response-envelope,
body-size and correlation validation belongs to the separate native cloud codec.

## Conservative first-slice boundary

Any credential revision change, including the existing AccountService's restore
operation, requires explicit consent reconfirmation. This deliberately prevents
old preferences from silently resuming sharing after sign-out/re-authentication.
A reviewed stable authorization-epoch design is needed before preserving consent
across safe refresh/restore. Do not weaken the current binding to subject alone.

There is no automatic submission, capture loop, IPC, TUI rendering, per-minute
backoff or startup backfill in this module; [refresh](../refresh/README.md) schedules it. [Source tracking](../source/README.md)
derives ordered observations from SessionApi views and events. The separate native
[cloud adapters](../cloud/README.md) provide bounded context/redaction/HTTP helpers,
and the separate dispatch module provides in-process coordination, not a running
credential owner or authenticated IPC boundary. Context-
availability flags are trusted owner observations, not proof that text was redacted
or fits the wire contract. Session callers must not gain consent/cache privileges
when a future local API is added. Cached rows require a fresh matching source
observation; an old database alone never proves a current terminal incarnation.

## Storage guarantees and limits

- One nonblocking owner lock lives with the store. A second owner gets `Busy`;
  callers must not silently create an alternative writer/store as a fallback.
- Owned 0700 directory and singly linked regular 0600 files; unsafe ownership,
  modes, symlinks and hardlinks are rejected, not chmodded. Known sidecars are checked.
- Schema 1, DELETE journals, FULL synchronization and 250 ms busy timeout. Future
  schema versions are refused; corrupt data is not silently reset to enabled/default.
- Existing snapshots are preflighted on private scratch copies before SQLite can
  recover the source. Copies are streamed and capped at 64 MiB per database/journal
  file. Future/invalid refusal preserves source bytes/existence. Supported recovery
  can precede a later I/O error; that case still requires reconciliation.
  An existing orphan journal without its database is refused before any database
  creation; recovery state is never silently discarded to initialize a fresh store.
- Consent JSON is capped at 2 MiB, cache JSON at 64 KiB per row. Cache evicts oldest
  capture times deterministically, with at most 256 rows. Indexed row identity/time
  must match its validated JSON. SQLite numeric fields must fit signed integers.
  A 2 MiB + 4 KiB SQLite value/record limit is installed before disk queries;
  session IDs are bounded to 256 bytes and guarded before value materialization.
- Filesystem/SQLite/parser diagnostics are static and exclude paths/content. A
  post-commit I/O error requires reloading; it does not guarantee rollback.
- Storage is **unencrypted**. The parent is trusted and hostile same-UID replacement
  or tampering is not a promised security boundary. No live database-file sync or
  backup mechanism is provided. Linux is tested; macOS still needs certification.
  A process crash can leave a private scratch folder; it is not a user backup.

## Targeted checks

Run from `tui/`, with a temporary `RAKUMUX_HOME`, inherited Rakumux identity/socket
variables unset, `TMPDIR=/tmp/opencode`, and low-debug/nonincremental profiles:

```sh
cargo test -p rakumux-sensagent status::service --locked --offline
cargo test -p rakumux-sensagent status::text --locked --offline
cargo test -p rakumux-sensagent sqlite_store --locked --offline
cargo test -p rakumux-sensagent status_gate --locked --offline
```

All fixtures are synthetic. No actual account, cloud endpoint, user database or
terminal is needed. See the crate README for isolation and full-workspace checks.
