# Rakumux Auth & Billing — Technical Plan (Clerk)

Status: **deferred.** Auth is out of scope for the sessions refactor (0.6.0) and is the next round
after it (`plans/sessions-refactor.md` §18). The partial implementation in `tui/src/auth/` is
compiled but switched off (`FeatureFlags { auth: false }` in `tui/src/app/feature_flags.rs`), and no
key opens the auth menu.

This document proposes a full implementation for account auth and subscription management inside the `rakumux` TUI using Clerk. It prioritizes headless (SSH) terminals and supports Mac “open in browser” convenience. No backend changes are required for Phase 1.

Auth is independent of Rakumux's session architecture: it talks to Clerk directly from the `tui` binary and never goes through the `rakumux-ptyd` daemon. Note the two meanings of "session": an **auth session** is a Clerk sign-in; an **Rakumux session** is a folder + a terminal owned by the daemon. Code and files for auth say "auth" to keep them apart.

## Goals
- An Auth menu reachable from list focus (`s` proposed; free in the 0.6.0 key map)
- In-TUI auth menu: Sign in • Sign up • Manage subscription • Sign out • View account
- Headless-first: email code-based verification (no browser required)
- Optional web flow on macOS: open sign-in page
- Persist the auth session locally and gate paid features
- All auth code lives in `tui/src/auth/`, with minimal, isolated changes to the TUI


## UX flows

### Entry point
- Press `s` in list focus → show centered dialog with options:
  - Sign in
  - Sign up
  - Manage subscription
  - View account (if signed-in)
  - Sign out (if signed-in)
  - Close

### Sign in (primary: email code)
1. Prompt for email
2. Call Clerk to “prepare” email code and send code to inbox
3. Prompt for 6-8 digit code
4. Verify code → create Clerk session → store session token locally
5. Show success with user email and short user id; return to app

### Sign up (email + email code)
1. Prompt for email and (optional) password — password optional if instance allows passwordless
2. Create sign-up attempt → prepare email verification → code sent to inbox
3. Prompt for verification code → verify
4. On success, session is created; persist token and profile basics

### Sign in (optional mac convenience)
- Offer “Open web sign-in” (mac only): opens Clerk’s sign-in URL in browser (falls back to printing URL). Note: the TUI remains the source of truth; user should still pick email-code flow if they’re remote/headless.

### Manage subscription
- Opens an external URL to your customer portal (Stripe/own site). For headless: show URL and copy-to-clipboard hint. For mac: `open` the link.
- Short-term: points to a static portal URL requiring separate login in the browser.
- Long-term: deep-link using a short-lived portal/session URL minted by backend.

### View account / Sign out
- View account: display email, user id, session expiry, plan name (if available)
- Sign out: revoke local session (and optionally remote) and delete local credentials


## Architecture

### Module tree (`tui/src/auth/`, exists today)
- `mod.rs` — module exports
- `clerk_api.rs` — minimal Clerk Frontend API client (`reqwest`)
- `models.rs` — response/request structs for Clerk (`Client`, `SignInStart`, `SignUpStart`, `SessionToken`; only fields we use)
- `service.rs` — `AuthService`: high-level flows (sign-in, sign-up, sign-out); enabled only when `CLERK_FRONTEND_API` and `CLERK_PUBLISHABLE_KEY` are set
- `store.rs` — `FileStore` persistence of `SessionRecord { session_id, token, email }`
- `auth/AGENTS.md` (repo root) — this plan

### TUI integration
- `tui/src/app/types.rs`: `AppMode::{Auth, AuthEmail, AuthCode}` with `AuthMenu`, `AuthEmailDialog`, `AuthCodeDialog` (exist; bounced out of while the flag is off)
- `tui/src/components/auth.rs`: menu and dialogs (exist)
- When auth is picked up: bind the menu key in list focus, add it as a `KeyAction` row on the Settings modal's Keys tab (`tui/src/help`), and add it to the modal's General tab

### No daemon involvement
- All Clerk HTTP calls are made directly from the `tui` binary (using publishable/front-end endpoints). The session daemon, its protocol and its registry know nothing about accounts.


## Clerk integration (Frontend API only)

We will use the Frontend API (form-encoded). Core flows are supported without server secrets.

Instance configuration inputs (via `.env` / config):
- `CLERK_FRONTEND_API` — e.g. `example-abc-123.clerk.accounts.dev`
- `CLERK_PUBLISHABLE_KEY` — used for platform consistency (header)
- Optional: `AUTH_PORTAL_URL` — Manage subscription link

HTTP defaults:
- Base URL: `https://{CLERK_FRONTEND_API}`
- Headers: 
  - `Content-Type: application/x-www-form-urlencoded`
  - `Origin: https://rakumux.cli` (stable pseudo-origin)
  - `Clerk-Publishable-Key: {CLERK_PUBLISHABLE_KEY}` (helps with rate-limit/classification)

Client lifecycle (first call):
- POST `/v1/client` → capture `client.id` (keep in memory), and any cookies if Clerk returns them (we’ll mirror cookie jar in memory)

Sign in (email code):
1) Create or reuse client
2) POST `/v1/client/sign_ins`
   - body: `strategy=email_code&identifier={email}`
3) POST `/v1/client/sign_ins/{id}/prepare_first_factor`
   - body: `strategy=email_code`
   - Clerk sends code to email
4) POST `/v1/client/sign_ins/{id}/attempt_first_factor`
   - body: `strategy=email_code&code={XXXXXX}`
   - On success → `created_session_id`
5) POST `/v1/client/sessions/{session_id}/tokens`
   - get JWT session token and expiry

Sign up (email + code):
1) POST `/v1/client/sign_ups` with `email_address={email}` (and password if enabled)
2) POST `/v1/client/sign_ups/{id}/prepare_verification` with `strategy=email_code`
3) POST `/v1/client/sign_ups/{id}/attempt_verification` with `strategy=email_code&code=...`
4) Session created → same token step as sign-in

Notes:
- `email_link` is not reliable for headless because many instances require same-client verification and cross-device cookies; we’ll prefer `email_code` as canonical.
- For development instances, Clerk may require a `DevBrowser` token. We’ll detect failures and show a helpful message to switch to production instance settings.


## Persistence & security

Today `FileStore` writes `~/.rakumux/session.json` (`SessionRecord { session_id, token, email }`), without restricting permissions. Target, in priority order:
1) macOS Keychain (if available): service `rakumux`, account `clerk_session`
2) `~/.rakumux/auth.json` (`$RAKUMUX_HOME` respected), mode 0600. Renamed from `session.json` so it isn't confused with Rakumux sessions (`~/.rakumux/sessions/`).

Schema (auth.json):
```
{
  "clerk_frontend_api": "...",
  "session_id": "...",
  "session_token": "...",
  "user_id": "...",
  "email": "...",
  "expires_at": 1735689600
}
```
Rotation:
- On app start, if token near expiry, auto-refresh via `sessions/{id}/tokens`
- On sign out, delete local secret and optionally call `POST /v1/client/sessions/{session_id}/end`


## Paid features gating (Phase 1 lightweight)

- Add a single auth check helper: `auth::service::require_session()`
- If missing/expired → prompt to sign in (auth menu)
- For now, paid features are gated by a stub entitlement check that always returns free. Later, add a tiny backend to read subscription status (Stripe) and return entitlements using the Clerk session token (server-side verification).


## CLI/browser helpers

- Open URL: 
  - macOS: `open {url}`
  - Linux: `xdg-open {url}` if present; else print URL
- Copy helper for headless: print url and note `pbcopy`/`xclip` if installed


## Error handling
- Network errors: show concise message + “try again”
- Rate limiting: backoff and inform user
- Invalid code: allow 3 attempts, then restart flow
- Missing instance vars: show one-time setup dialog to collect `CLERK_FRONTEND_API` and `CLERK_PUBLISHABLE_KEY` and persist to `.env`


## Testing
- Unit: mock HTTP client for Clerk flows (happy path + common errors)
- Manual: end-to-end using a Clerk dev instance with email code
- TUI snapshot: ensure new dialogs render at minimum widths


## Phases

Phase 1 (no backend; 100% client-side)
- Email code sign-in/sign-up
- Local session storage & sign out
- Manage subscription → external portal URL only
- Bind `s` to the Auth menu; update the Settings modal (Keys and General tabs)

Phase 2 (nice-to-have)
- Device-like flow with short code via a tiny backend that issues one-time “CLI connect” tickets; web page shows code; terminal polls and exchanges for session
- Real “Manage subscription” deep link: backend mints Stripe portal session tied to Clerk user
- macOS Keychain integration by default


## Exact Clerk endpoints used
- POST `/v1/client`
- POST `/v1/client/sign_ins`
- POST `/v1/client/sign_ins/{id}/prepare_first_factor`
- POST `/v1/client/sign_ins/{id}/attempt_first_factor`
- POST `/v1/client/sign_ups`
- POST `/v1/client/sign_ups/{id}/prepare_verification`
- POST `/v1/client/sign_ups/{id}/attempt_verification`
- POST `/v1/client/sessions/{session_id}/tokens`
- POST `/v1/client/sessions/{session_id}/end` (optional sign out)


## Implementation checklist
- [x] Add `tui/src/auth/` module files (partial: Clerk client, models, service, file store)
- [ ] Wire `s` → Auth menu; update the Settings modal (Keys and General tabs)
- [ ] Implement email code sign-in/sign-up
- [ ] Persist session and expose `auth::service::current_user()`
- [ ] Add “Manage subscription” action (URL from config)
- [ ] Add tests and manual instructions


## Open questions
- Which production Clerk instance domain should ship as default?
- Where should the customer billing portal live (URL)?
- Do we want macOS Keychain dependency in Phase 1 or fallback-only?
- Should a signed-in account ever matter to the daemon (e.g. gating agent-control features), or stay a TUI-only concern?

This plan keeps Phase 1 fully client-side, works over SSH without a browser, and cleanly layers future device-code and billing portal integrations without refactors.
