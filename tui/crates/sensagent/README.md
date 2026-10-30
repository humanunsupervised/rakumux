# Sensagent local foundation

The first Rust implementation slice for the approved local-first architecture.
This library is independent of ptyd, terminal ownership and the TUI. It does not
enable login prompts, gate core features, create provider accounts or deploy services.

## Implemented

- Explicit public-client configuration: HTTPS issuer, public Client ID, exact
  trusted Account Portal device-verification URL and approved scopes.
- Advertised endpoint / Device Grant discovery; no guessed endpoint fallback.
- Blocking bounded HTTP with timeouts, disabled redirects and issuer-origin checks.
- Device request and OAuth access / refresh response validation, credential-safe
  Debug / errors, canonical URL and public-code checks.
- An I/O-free AuthService with monotonic deadlines, one in-flight poll, permanent
  slow_down interval growth, bounded transient backoff and numeric Retry-After.
- Attempt / poll generations protect cancellation, replacement, duplicate results
  and late token responses. Pending requests, grants and persisted credentials carry
  issuer, public client, requested scopes, verification page and explicit UserInfo pin.
  A differently configured client cannot transmit their private device code or bearer.
- Provider-grant completion is explicit, not a false "Signed in" state.
- Account verification uses an advertised, HTTPS issuer-pinned UserInfo endpoint,
  with documented OIDC metadata discovery when needed. No unverified ID-token decoding.
  A reviewed same-issuer UserInfo URL can be pinned explicitly from OAuth application
  settings; conflicting advertised metadata is an error, not a silent override.
- AccountService completes login only after identity validation and a conditional
  private-store commit. Online restoration must keep the same subject; email is
  not an ownership key.
- An explicit Unix private-file credential backend uses a dedicated 0700 directory,
  owned / singly linked regular 0600 files, a process-safe lock, bounded schema
  validation, atomic replacement and fsync. Symlinks and unsafe permissions are rejected.
- Revision fences and clear-state tombstones reject stale login / restore results
  after cancellation, sign-out or newer attempts. An interrupted replacement's
  previous account is preserved until a valid new commit.
- Login tickets and safe account views belong to one AccountService instance.
  Matching numeric revisions in another home or reopened service do not transfer ownership.
- Freshness is checked before and after persistence. A token that expires during
  a write cannot return a successful session; cleanup only clears that write's revision.
- The HTTP transport stamps paired wall/monotonic receipt before reading the token body.
  The grant retains that stamp through verification and persistence; each
  account view has its own monotonic deadline. Observed clock rollback is rejected.
- Monotonic time is sampled before wall time, both for new grants and restoration.
  Pausing between the reads cannot extend the token's remaining lifetime.
- An explicit AccountWebsite builds a fixed `/connect` URL containing only the public
  user code and original provider convenience link. Unsupported / missing / oversized
  links return a static error and leave the canonical Clerk fallback unchanged.
- [Local AI-status policy/store](src/status/README.md): explicit account/folder/data
  consent, revision-fenced SQLite cache, service-owned generation tickets, Unicode
  advisory text validation and a current-account gate. No network/TUI is wired.
- [Native cloud client/capture](src/cloud/README.md): job-selected bounded plain
  context, best-effort local redaction, strict canonical codec, explicit fixed-path
  HTTPS transport and cooperative cancellation. This is not an integrated dispatcher.
- [Account token-use intents](src/auth/account/access.rs): one-use, token-free staging
  bound to the issuing verified account/revision, with a 30-second monotonic limit.
  Consumption validates one fresh credential snapshot before lending its bearer.
- [Explicit dispatch coordinator](src/dispatch/README.md): fixed-target consent offers,
  bounded capture/preparation/commit/worker stages, cancellation and fenced completion.
  This is in-process control state, not a running per-home/IPC service.
- [SessionApi source tracking and capture](src/source/README.md): the coordinator's own
  tracker derives ordered source stamps from daemon views/events, coalescing output into
  owner-advanced revisions; user-client capture reads bounded plain-text windows
  confirmed against the job's stamp.
- [Scheduled refreshing](src/refresh/README.md): a pure scheduler (one capture per
  session per minute, two jobs, 12 starts a minute, error backoff, rate-limit pause,
  unauthorized halt, coalesced output), a synchronous engine over the coordinator and
  an in-process runner with a daemon subscription, reconnects, two workers and one for
  replies being typed in and written in a TUI. It is
  started only by an embedding process; nothing is installed or wired to the TUI.
- [Per-home owner](src/owner/README.md): a proposed `$RAKUMUX_HOME/sensagent/` layout,
  a single-instance claim, and a versioned local socket protocol (state, status lines,
  one-use consent offers, disable, sign-out) with a server and client. Same-user peers
  with the registration token only. The `status_owner` developer example composes it
  with explicit configuration; nothing is installed or started by the TUI.

## Integration contract

1. Construct `DeviceAuthConfig` from reviewed public deployment configuration.
2. Call `AuthService::start()` before scheduling blocking discovery / start work.
3. Apply challenge results with `complete_start` and their original attempt ID.
4. Display only `VerificationPrompt`; never the private device code or credentials.
5. Use one `Instant` clock origin. Supply elapsed `Duration` to service methods.
6. Use `next_poll` to prepare work, run `ClerkDeviceClient::poll` off the UI thread /
   service lock, then call `complete_poll` with the exact poll ID.
7. Begin an AccountService login intent before provider work. Pass a granted token
   to `ClerkDeviceClient::verify_account`, then commit the verified result using the
   original LoginTicket. The grant already carries its transport receipt time.
   Only the successful AccountSession result is ready for explicit UI completion.
8. Cancel both the device attempt and account login intent when abandoning a flow;
   invalidate outstanding UI generations. A returned AccountSession carries a
   revision for rejecting stale async UI events; it is not a provider authorization token.
9. `DiscardedGrant` must never be stored. Dispose of it and revoke through a supported
   provider flow when integrated. `AccountService::restore` revalidates online before
   returning a session, and rechecks revision / expiry after verification and persistence.

The file backend is an explicitly selected, **unencrypted** fallback, not an OS
keychain. Its parent directory must already exist and be trusted. No path is read
from the real home or environment by this library. Local sign-out atomically writes
a no-credential tombstone; it is not secure media erasure. The removed grant is kept in
memory until `AccountService::revoke_signed_out` revokes its refresh token (and access
token, where possible) at Clerk's advertised revocation endpoint (RFC 7009); an enrolled
machine's tokens are never sent there.
CommitUncertain means rename happened but directory durability was not confirmed:
do not claim sign-in success or assume the old record remained; reconcile / reverify.

Tests inject fake HTTP transports; the production adapter uses reqwest / rustls.
No live Clerk instance or real TLS exchange is certified by those fake tests.
Raw provider error descriptions, response bodies and tokens are never diagnostic text.
Credentials are redacted, not promised to be memory-zeroized.

Clock observations and monotonic deadlines are process-local, not persisted. A
restarted service cannot detect a rollback that still leaves wall time after the
saved receipt timestamp.
Restoration therefore requires a reliable system clock and fresh online UserInfo
verification. The remaining wall lifetime is anchored to monotonic time before
restoration's network call, not after it. This local expiry check is never
provider-side authorization. A suspended process also depends on the platform's monotonic-clock behavior and a
correct wall clock on resume. Refresh and shared-owner integration remain pending.

## Account token-use intents

`AccountService::lease_access_token` prepares an opaque `AccessTokenLease` only for
its own current verified account view. It retains no bearer and expires within
30 monotonic seconds or at account expiry, whichever comes first. It is neither
cloneable nor serializable. It does not write credentials or advance their revision.

`consume_access_token` takes that intent by value and rereads the credential store.
The same snapshot must match the issuing owner, subject, revision, complete provider
configuration/scopes and wall/monotonic freshness before its bearer reaches the
trusted callback. Stale or signed-out intents return `None`; expiry/storage/clock
errors suppress the callback. A foreign owner cannot even trigger a store read.

This is an account-only staging primitive, **not a standalone feature/destination grant**.
The separate dispatch coordinator checks feature scope, consent/source/account binding
and fixed cloud origin, and serializes its commitment with local revocation. Account
or external store mutations are not locked out by the callback API. Trusted code
can copy a borrowed token; it must not export it through UI/IPC or retain it. No
claim is made about stopping accepted upstream work or billing. Credential revisions
still require consent reconfirmation; stable authorization epochs are not added here.

## Not implemented yet

- OS-keychain backends. (Renewal, revoking a signed-out grant at Clerk and the shared
  credential-owner service are implemented: `refresh/`, `auth/account.rs`, `owner/`.)
- TUI continuation integration and real browser/provider round trips. Private `humanunsupervised/rakumux-cloud`
  owns guarded account pages; [public landing](../../../landing/README.md) remains static.
  Real Clerk configuration and end-to-end login remain pending.
- TUI account / consent controls and browser opening / copy UX.
- An installed owner process and who starts it, sign-in over the owner's socket and
  third-line rendering. The owner is a library plus a developer example.
  The low-level native client, context builder and local library/store are
  implemented; the private hosted API remains disabled for real authorization/inference.
- Stripe account management, full local supervisor or native agent-worker control.

Those are subsequent slices. The older disabled TUI email-code auth is not activated
or silently substituted for Device Grant. No client secret is distributed.

## Verify

From `tui/`:

```sh
cargo test -p rakumux-sensagent auth:: --locked
cargo test -p rakumux-sensagent cloud::client::tests::dispatch --locked --offline
cargo test -p rakumux-sensagent source --locked --offline
cargo test -p rakumux-sensagent refresh --locked --offline
cargo test -p rakumux-sensagent owner --locked --offline
```

Tests use fake provider transports / clocks and private temporary directories,
never real accounts, network requests or `~/.rakumux` data. File tests exercise the
native Linux backend here; macOS behavior and real Clerk metadata / TLS / identity
responses still need separate verification.

## Developer status-owner example

```sh
cargo run -p rakumux-sensagent --example status_owner -- --help
```

Runs the per-home owner for `$RAKUMUX_HOME` (set it to a temporary directory) with
explicit public sign-in and AI-service configuration, restoring credentials the
device-login example saved in `$RAKUMUX_HOME/sensagent/credentials`. Other commands
query it and confirm or stop sharing, showing the account, destination and consent
revision first. It needs a running `rakumux-ptyd` for that home and is not installed.

## Developer device-login example

```sh
cargo run -p rakumux-sensagent --example device_login -- --help
```

The optional example exercises the canonical provider flow, account verification
and private storage with explicit public issuer / client / verification-page
configuration and a chosen private directory. It creates no Clerk resources and
is not installed as another product binary or connected to TUI startup. Only run
a real attempt against an approved, already configured test client. No OAuth
credentials are printed.

`--website-origin HTTPS_ORIGIN` optionally prints the branded account link. Supply
only a reviewed website origin for the same Clerk environment. The native helper
does not infer a deployment domain, approve the grant, open a browser, enable AI,
or claim the real account flow was tested. It preserves the 1–128 byte ASCII code
without normalization and caps the provider URL at 2048 bytes / connect URL at 4096.
The direct Clerk URL and code are printed first and remain available on every failure.

For full workspace checks inside a running Rakumux session, also clear inherited
`RAKUMUX_SOCKET`, `RAKUMUX_SESSION_ID` and `RAKUMUX_SESSION_TOKEN`, and set
`RAKUMUX_HOME` to a temporary directory. A temporary home alone does not override
an explicit inherited socket in existing session-authentication tests.
