# Explicit in-process status dispatch

`StatusDispatchService` coordinates the existing account, consent/cache and native
cloud adapters. It is **not** an installed per-home service, IPC permission boundary,
TUI feature, scheduler or real terminal-capture adapter. Nothing starts automatically.
Hosted production OAuth/inference remain unsupported/disabled in the private repository.

## Account and destination authority

- The constructor owns one fixed `CloudAIClient` and an opaque instance identity.
  Workers receive that exact client, not a URL supplied with a job or response.
- `set_account_session` accepts only a current view from the injected AccountService.
  Adoption/replacement clears destination confirmation and cancels old work. Sign-in
  alone cannot resume sharing, including replacement with the same account view.
- `consent_offer` captures the exact account/credential revision, durable consent
  revision and canonical HTTPS origin. Trusted user controls must show those bindings
  and data-kind/folder selection before calling `enable`. Stale actions cannot retarget.
- Every new coordinator requires a new explicit offer/confirmation, even when the
  store already contains enabled consent for the same account and destination. The
  existing schema does not store endpoint authorization. This conservative rule
  prevents implicit reuse when a deployment origin/trust configuration changes.
- Standalone `AccessTokenLease` remains account-only. A committed dispatch binds it
  to this fixed client and exact policy job. It retains no bearer while queued;
  `run` rereads one current credential snapshot before borrowing its access token.
  A lease that waits 30 seconds must be reissued; credential expiry wins sooner.
- Exact credential revisions still fence consent. Stable authorization epochs,
  automatic refresh and remote grant revocation are not implemented.

The injected account/store/capture implementations are trusted local code. A future
IPC adapter must authenticate user-only control calls and keep token-use callbacks,
jobs and stores out of UI/session-caller APIs. The coordinator cannot turn a UI
boolean or arbitrary source snapshot into proof of daemon/provider authorization.

## Job lifecycle

```text
prepare → Capturing → build → Prepared → commit → Committed → run → finish
                └── stopped capture acknowledgement ──┘       └── abandon → finish
```

1. `prepare` checks explicit destination confirmation and current policy. Eligible
   work gets an opaque, non-cloneable capture handle with local session/folder,
   source stamp and allowed data kinds. No credential or raw text is included.
2. The future scoped SessionApi adapter reads only selected bounded plain-text
   windows. It must attest the exact session/folder and source revision, lifecycle
   and metadata. The numeric stamp passed to `build` is a consistency check, **not**
   cryptographic attestation or a replacement for that adapter's confinement checks.
3. `build` checks owner, phase and live policy before/after the bounded context builder.
   Reports-only jobs reject terminal output. The sealed prepared body has no public
   constructor/accessor and no token. Raw inputs/secrets are not retained by the service.
4. `commit` rechecks the complete job, current account, exact binding and account-use
   intent, then changes its phase under exclusive coordinator control. This is the
   **dispatch commitment** serialized with local opt-out/sign-out/source controls.
5. Move the worker to an I/O thread and call `run` promptly. It rechecks credentials,
   observes cancellation and calls the fixed native client at most once. No worker
   thread, retry or scheduling loop is created by this module.
6. Pass the opaque completion to the same coordinator's `finish`. Successful text is
   available only through a current-policy cache view; the completion has no text getter.
   Completion rechecks account/consent/source and the store's final insertion fence.

`build`/`commit` failures leave the capture handle available for stopped-work
acknowledgement. `acknowledge_capture` rejects committed workers; consume a stopped
worker with `abandon`, then `finish`. Dropping a handle/worker/completion does not
silently free its slot or prove another task stopped. A future runtime must handle
worker exits/panics explicitly; unknown work stays fenced rather than being replaced.

## Revocation and bounded work

- At most two unacknowledged jobs per coordinator, across every phase. Invalidated
  work keeps its slot until capture actually stops or a committed worker completes.
- Explicit consent/account changes cancel all old jobs before persistence. Failed
  opt-out/sign-out cannot leave this instance implicitly sharing under old consent.
- Trusted source updates, privacy tightening, non-live state and retirement cancel
  affected jobs. The runtime must call `recheck` on freshness events/ticks as well.
- Policy-read errors cancel all work and clear destination confirmation. Recovery
  requires a fresh explicit enable, not automatic resumption of previous settings.
- Owner drop cancels outstanding jobs. A queued worker then submits nothing. Already
  blocked I/O remains bounded by the native client's deadline; it is not interrupted.
- A request committed before revocation may already have been accepted remotely.
  Cancellation suppresses/discards locally but cannot retract upstream work or billing.
  There is no atomic transaction between a local opt-out and a remote HTTP syscall.
- Local bounds are not hosted spending quotas, durable cloud deduplication or a
  cross-restart limit on already accepted inference. Request IDs remain correlation.

Prepared context and HTTP results exist only in bounded worker memory. SQLite stores
only consent and validated advisory text/metadata. Debug/errors never print context,
credentials, account subjects, local paths, destinations or raw transport diagnostics.
Redaction is best effort; known secrets must be explicitly supplied by trusted capture
integration. Do not capture environments, credential files or complete histories.

## Remaining runtime gates

Per-home process ownership/lifecycle, authenticated IPC, real scoped SessionApi
observations/capture, metadata provenance, scheduling/backoff, TUI controls/rendering,
refresh/revocation, stable authorization epochs and live Clerk/TLS/model certification
remain unfinished. No daemon registry or PTY is opened here. Do not wire UI directly
to these low-level controls or treat an injected fake as production authentication.

Synthetic integration tests live under `cloud/client/tests/dispatch.rs` so they can
reuse the private HTTP transport seam without exposing a public transport bypass.
They use fake HTTP, explicit temporary credential files, fake clocks and a local
status-store fake. The 25 cases cover origin/account consent, owner isolation,
phases/slot retention, failed persistence, token-use expiry and late success/error
rejection while HTTP is blocked. Four crate-wide compile-fail checks cover sealed
account intents, prepared bodies and completions. Run from `tui/` with isolated `RAKUMUX_HOME`, inherited Rakumux
identity/socket variables unset, `TMPDIR=/tmp/opencode` and low-debug profiles:

```sh
cargo test -p rakumux-sensagent cloud::client::tests::dispatch --locked --offline
```

Independent source/regression review found no blockers. Final isolated workspace
build/tests, formatting and scoped crate Clippy pass, including 259 native unit
tests and four compile-fail checks. This is synthetic/local verification, not live
Clerk/TLS/model or installed shared-service certification. Linux is tested here;
macOS storage behavior remains a separate certification gate.
