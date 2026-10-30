# AI session-status wire contract, version 1

Canonical JSON Schema draft 2020-12 for `POST /api/v1/ai/session-status`.
This directory contains contracts and development-only conformance tests, not an
HTTP server, authentication implementation, model client or local runtime.

## Messages

- [Request](request.schema.json): `{version:1, request_id, context}`.
  `context.lifecycle` is required: `working`, `blocked`, `done`, `idle`, `unknown`
  or `none`. Optional `activity_phase` is `working`, `waiting_for_response`,
  `running_tool`, `needs_input`, `idle` or `done`.
- Optional `reported_text` and `recent_output` are plain strings capped at **2048**
  and **8192 UTF-8 bytes**, respectively. At least one must contain a Unicode
  letter, number, punctuation mark or symbol. An unused source may be empty.
  Lifecycle/activity alone, whitespace and combining marks alone are insufficient.
- [Success](success.schema.json): `{version:1, request_id, status_text, title?}`.
  `status_text` is one nonblank line of at most **140 Unicode codepoints**, not
  UTF-16 units, grapheme clusters, bytes or display columns. Text is advisory;
  it cannot change authoritative lifecycle.
- `title` is a short title for the session, in plain text: one line of **1–60
  codepoints** and at most **240 UTF-8 bytes**, with visible content, no
  whitespace at either end, no trailing period (`.`), and the same controls
  rejected as `status_text`. It is sent only when the request URL asks for it
  with the query `title=1` (`POST /api/v1/ai/session-status?title=1`): clients
  without it reject any field they don't know. A service refuses a query it
  doesn't know (`invalid_request`); one that was asked may still leave `title`
  out. The client names the session with it under its own rules (never over a
  name the user gave); the service never renames anything.
- [Error](error.schema.json): `{error:{code, message, retry_after_seconds?}}`.
  Each code has exactly one static message in the schema. Messages are additionally
  capped at 96 codepoints/UTF-8 bytes. Optional retry delays are integers **1–3600**,
  only for `rate_limited` and `upstream_unavailable`.

Every object rejects additional properties. No user/billing identity, credential,
local session ID, folder/path, database, model/provider selector, arbitrary prompt
or tool-execution fields are accepted. OAuth authorization belongs in the HTTP
Authorization header, never the JSON body. This package does not verify it.

[`common.schema.json`](common.schema.json) owns shared definitions. `request_id`
is 1–64 ASCII characters, starts with an alphanumeric character, and otherwise
allows alphanumerics, `.`, `_` and `-`. Generate a fresh public correlation value;
never reuse a local session ID, account ID, cache key or credential. Correlation
does not provide authorization, cloud persistence or idempotent inference.

## Mandatory validation boundaries

1. Accept UTF-8 JSON bytes, with at most **16384 bytes in the complete request
   body**, including whitespace and JSON escapes. Bound the HTTP stream before
   buffering or parsing; do not trust only Content-Length. Reject unsupported
   media/content encodings, malformed UTF-8/JSON, a byte-order mark and duplicate
   decoded member names. This contract uses uncompressed JSON.
2. Resolve all schema references locally by `$id`. Implement `maxUtf8Bytes` as an
   **assertion**: the decoded scalar string's UTF-8 encoding must be at most its
   nonnegative integer limit. A validator that silently ignores this custom
   keyword is not conformant. `maxLength` does **not** measure UTF-8 bytes.
3. Read the raw-body cap from request annotation `x-maxBodyBytes`. This annotation
   is enforced by the transport boundary, not by validating a parsed object or
   measuring `JSON.stringify(parsedBody)`. Escaping alone can exceed the cap even
   when every decoded field passes its own bound.
4. Apply the complete request/success/error schema without coercion, defaults,
   extra-field removal or Unicode normalization. Reject invalid data rather than
   silently modifying it. Do not echo validation/provider details into errors.
5. Compare a schema-valid success's `request_id` **exactly**, case-sensitively,
   with the outstanding request. A standalone JSON Schema cannot compare two
   messages. [`validateSuccessForRequest`](test-support/validation.mjs) demonstrates
   this separate boundary; correlation fixtures intentionally pass both schemas
   individually while failing the pair check.

Output rejects Unicode categories `Cc`, `Cf`, `Cs`, `Zl`, `Zp` and the
`Default_Ignorable_Code_Point` property. This covers C0/C1/DEL terminal controls,
ANSI/OSC escapes, bidi controls, zero-width/format controls, lone surrogates,
variation selectors and line/paragraph separators. Ordinary right-to-left letters
and visible combining-accent sequences are allowed. Joiner/variation-selector
emoji sequences are intentionally rejected. Input rejects the same terminal,
format/default-ignorable and surrogate controls, but permits tab, LF and CR for
plain multiline context. Strip terminal formatting locally before selection.
Regex adapters must support the schemas' Unicode property escapes; do not weaken
patterns when porting to Rust. Run these same fixtures against each implementation.

The static error codes are `invalid_request`, `unauthenticated`, `forbidden`,
`rate_limited`, `upstream_unavailable` and `invalid_output`. Suggested HTTP statuses
are 400 (or 413 for oversized input), 401, 403, 429, 503 and 502, respectively;
HTTP behavior is an integration responsibility, not validated by JSON Schema. A 401
with the header `X-Rakumux-Signed-Out` (`signed_out` or `deleted`) says the sign-in
is over (all the account's terminals were signed out, or the account was deleted):
sign out and revoke it rather than renewing it (see the feed contract's "Ending a
sign-in").

## Conformance tests

From `contracts/session-status/`, with Node.js 22 or newer:

```sh
npm ci --cache .npm-cache --ignore-scripts --no-audit --no-fund
npm test
```

AJV **8.20.0** is pinned as the sole direct development dependency. Tests use
Node's built-in runner, local schemas and synthetic fixtures only: no provider,
Clerk, network, clock, environment credentials or real session/database access.
The lockfile pins transitive dependencies. Installation requires npm registry
access; test execution does not. Do not add this package to the installed product.

[`fixtures/`](fixtures/) includes valid/invalid messages, malformed raw bodies,
credential/extra-field rejection, Unicode/output controls, errors and correlation.
`boundaries.json` is a compact fixture recipe: set the selected `field` to
`text.repeat(count) + suffix` on the first valid request/success/error fixture
(request fields go inside `context`; error fields go inside `error`).
`body-bounds.json` pads the first valid request with ASCII spaces to
`pad_to_bytes`, or sets `recent_output` to `a` repeated `output_count` times and
replaces literal `a` characters in its JSON with `\u0061` for `escape_ascii`.
`bodies.json` supplies literal body strings or byte arrays. All recipes are
expanded deterministically in [`test/contract.test.mjs`](test/contract.test.mjs).

## Integration limits

- Schemas reject prohibited **fields**, not secrets or paths embedded in free text.
  The local owner must enforce consent/private-session exclusion and best-effort
  redaction before selecting context. Syntactic meaningfulness is not proof that
  prose is useful or that a public ID is genuinely ephemeral.
- Keep cache/session/source identifiers, timestamps and account/consent/terminal
  generations local. Reject stale results separately even when correlation matches.
  Keep SQLite/cache ownership local; this contract needs no cloud application DB.
- Authenticate Clerk OAuth access tokens and enforce feature scope, bounded
  inference, timeouts and spending/abuse policy outside these schemas. Treat
  selected context as untrusted data, not instructions. Never execute returned
  text or enable sharing simply because a user signed in.
- No real authentication, HTTP handling, redaction, inference, retention policy,
  Rust validator or deployment is implemented or verified here. Core terminal
  features remain account-free. This narrow endpoint grants no terminal access.
