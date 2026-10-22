# Native status capture and cloud client

This public Rust module is the **native client**, not the hosted implementation.
The hosted API stays in private `humanunsupervised/rakumux-cloud/api/` and still
has unsupported production OAuth and disabled inference. No TUI, scheduler,
always-running owner, automatic submission or real provider test is enabled here.

## Owner integration contract

1. The future single local feature/credential owner authenticates user-only control
   requests and obtains session observations through scoped `SessionApi` operations.
2. `AISessionStatusService::prepare` creates a current account/consent/source-bound job.
   The owner reads only permitted bounded plain-text report/output windows.
3. Prefer `ContextBuilder::build_current`: it checks that exact live job before and
   after bounded selection/redaction. Foreign/revoked/obsolete jobs return `Canceled`;
   a policy-read failure returns static `PolicyUnavailable`.
4. Immediately before sending, the owner must recheck `is_current`, verify that the
   access token belongs to the same current account/revision, and serialize dispatch
   with opt-out/sign-out/privacy/source changes. A validated body is **not a lease**.
5. Run blocking `CloudAIClient::generate` off the UI/owner event thread. The owner
   propagates revocation through the job's cancellation flag and fences completion
   again using `AISessionStatusService::complete`. A finished worker acknowledges
   cancellation/abandon only after its local work actually stopped.

The separate [in-process dispatch coordinator](../dispatch/README.md) now implements
fixed-target offers, current-job commitment, cancellation and fenced completion.
It is not a running per-home service or authenticated IPC endpoint. Its sealed worker
binds the account-only `AccessTokenLease` to the fixed client and current policy job.
Low-level `ContextBuilder::build`, the validated-request codec and HTTP client do not supply
an IPC/consent permission boundary by themselves. Do not wire UI clients directly
to these adapters or treat typed/fake inputs as authentication proofs. Origin or
trust-configuration changes must require the future owner to revalidate consent;
the current consent store is not a dynamic endpoint-reconfiguration mechanism.

## Bounded context and best-effort hygiene

- Explicit inputs only; no home, environment, filesystem, terminal or network reads.
  Inputs must already be plain text, not ANSI/checkpoint streams or full environments.
- Reports-only jobs reject supplied terminal output, even an empty string. Unknown
  activity phase is omitted; native lifecycle is preserved, never changed by output.
- Inspect at most 64 KiB per source window and at most 32 explicitly supplied known
  secrets. Known credential values are borrowed, not printed or serialized.
- Reject prohibited controls/invisibles before matching. TAB/LF/CR and canonical
  Unicode input separators remain unchanged. No Unicode normalization/coercion.
- Mark original UTF-8 byte spans for known literal secrets, common credential
  assignments/bearers/token formats/private-key blocks, and common absolute paths.
  Secret masking dominates path masking. Render constant labels once, so overlapping
  rules/short secrets cannot destroy delimiters or repeatedly expand mask labels.
  Explicit known-secret matching covers overlapping occurrences in linear work per
  source/secret and merges spans before marking; repeated input cannot leak a tail
  fragment or turn span marking into quadratic work.
- Redact before retaining recent UTF-8 tails of at most 2048 report / 8192 output
  bytes. Do not leave partial replacement labels at the truncation boundary.
- Empty, blank, mark-only and redaction-only sources produce `NoContext`. This is
  syntactic signal checking, not proof that remaining prose usefully describes work.
- Only `version`, the job's fresh public correlation and allowed context fields enter
  the request. Local session/account/folder/cache IDs and generations stay local.
- Validate the complete encoded JSON body independently, with a 16384-byte cap.
  JSON escaping/whitespace counts; decoded field caps alone are insufficient.

**Redaction is best effort, not secret detection or a confidentiality guarantee.**
Unknown, partially captured, encoded or unusually formatted secrets/paths can evade
these patterns. A partial secret already cut by the upstream capture window cannot
be reconstructed reliably. Do not capture private sessions, environment dumps,
credential/configuration files, full histories or unnecessary paths in the first
place. User consent must explain destination/retention limits. Selected context is
untrusted data for the future private prompt, never tool-execution authority.

## Protocol and HTTP

- `ValidatedStatusRequest` seals bounded original UTF-8 JSON plus correlation. Strict
  object/enum/duplicate/optional-null checks implement the public canonical contract;
  `from_value` cannot recover duplicate members lost by a previous JSON parser.
- Success validates the whole versioned response, canonical `StatusText` and exact
  case-sensitive correlation. Server errors require the exact static schema message,
  permitted code/HTTP status and bounded retry delay. No body diagnostic is surfaced.
- Configuration is an explicit clean ASCII HTTPS root origin, with one fixed
  `/api/v1/ai/session-status` path. No default origin, discovery, model-supplied URL,
  environment configuration, proxy, redirect, retry or decompression is used.
- Bearer values are marked sensitive and the complete header is capped at 8192 bytes.
  JSON/UTF-8/identity response encoding is required; at most 16 KiB is read.
- Connect timeout is 5 seconds and configured total timeout is 15 seconds. Cancellation
  suppresses dispatch or discards late data/errors, but does **not** interrupt blocking
  I/O or prove that accepted upstream work or billing stopped.
  The outgoing request itself carries the total deadline through response-body reads.
  A fixed-length nonreusable body and disabled connection pool prevent the inspected
  reqwest/Hyper automatic replay paths; one fake send count alone is not that proof.
  Wrapped reqwest body timeouts retain their Timeout category without diagnostic echo.
- Retry delays are suggestions for the future owner's bounded policy. The client
  never retries automatically and does not establish trusted global spending quotas.
- Capture inputs, sealed requests and status responses redact context in Debug.
  Errors are static categories; transport URLs/bodies/tokens are not diagnostic text.

## Tests and remaining gates

Tests use synthetic sources, local owner fakes, shared public fixtures and injected
HTTP transports. They perform no real HTTP/TLS/auth/model calls or user-home reads.
Run targeted `cloud::context`, `cloud::wire` or `cloud::client` tests from `tui/` with
isolated `RAKUMUX_HOME`, inherited Rakumux identity/socket variables unset and the
crate README's low-debug flags.

Independent review closed overlapping-secret tail leakage, the blocking transport's
per-read timeout trap, implicit reusable-body/connection replay, and wrapped timeout
misclassification. Source inspection and offline request/error-shape regressions
support these guarantees; real TLS timing/protocol behavior remains uncertified.

Runtime owner/IPC permissions, the live [SessionApi capture](../source/README.md)
loop, refresh/revocation, worker/event-loop integration, scheduling/backoff, TUI
integration, approved provider configuration and live transport certification remain
unfinished. Do not enable production sharing or pretend the disabled hosted API now
generates output.
