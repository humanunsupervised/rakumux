# Auth Scope & Implementation Plan (spec_auth)

## Goals
- Add account management to the TUI with fast, headless-friendly auth.
- Primary sign-in method: email code (no external browser required).
- Optional browser links for subscription/account (macOS first).
- Clean layering so app logic never talks HTTP directly.

## Non‑Goals (Phase 1)
- No backend server; use Clerk Frontend API only.
- No OAuth/device-code flows (may add later).
- No deep billing integration beyond opening a portal URL.

## Architecture
- UI (TUI)
  - Account entry and dialogs (modal overlays).
  - Keybinding: `s` opens Account menu.
- Service Layer
  - `AuthService` orchestrates flows (sign-in, sign-out) and persistence.
  - Decides enabled/disabled from env.
- API Wrapper
  - `ClerkApi` encapsulates Clerk Frontend API calls.
  - All HTTP and serialization are contained here.
- Store
  - `FileStore` persists session (session_id, token, email) to `~/.orchestra/session.json`.
  - Future: optional macOS Keychain fallback.

## Files
- UI
  - `tui/src/components/auth.rs` — Account menu overlay.
  - `tui/src/components/dialogs.rs` — `draw_auth_email_dialog`, `draw_auth_code_dialog`.
  - `tui/src/components/footer.rs` — Footer help updates.
  - `tui/src/ui.rs` — Mounts overlays by AppMode.
  - `tui/src/app.rs` — AppMode additions, key handling, status messages.
- Auth core
  - `tui/src/auth/models.rs` — Minimal Clerk models (Client, SignInStart, SessionToken…).
  - `tui/src/auth/clerk_api.rs` — Clerk API wrapper (stubbed, ready for live HTTP).
  - `tui/src/auth/service.rs` — `AuthService` (env detection, flows, persistence calls).
  - `tui/src/auth/store.rs` — File-backed session store.
- Bootstrapping
  - `tui/src/main.rs` — `mod auth;` added.
  - `tui/Cargo.toml` — `reqwest` (json, rustls-tls).

## Environment
- `CLERK_FRONTEND_API` (e.g., `example-abc-123.clerk.accounts.dev`)
- `CLERK_PUBLISHABLE_KEY`
- `AUTH_PORTAL_URL` (subscription/account portal link)
- Example entries added to `.env.example`.

## UI/UX
- Account menu (press `s`):
  - Sign in → Email → Code → Success
  - Sign up → (stub, Phase 2)
  - Manage subscription → opens `AUTH_PORTAL_URL`
  - View account → opens `AUTH_PORTAL_URL`
  - Sign out → clears local session
- Dialogs
  - Email dialog: input + inline validation; Enter sends code.
  - Code dialog: input + inline validation; Enter completes sign-in.
- Footers updated per mode for minimal guidance.

## Flows
- Sign in (email_code)
  1) Create client (Clerk) if needed
  2) Start sign-in with email → Clerk sends code
  3) Attempt with code → receive session token
  4) Persist session (id + token + email)
- Sign out
  - Clear local record; optionally call Clerk end-session when live.
- Manage subscription / View account
  - macOS: `open $AUTH_PORTAL_URL`
  - Future: add Linux `xdg-open`/Windows support.

## Clerk Endpoints (planned)
- `POST /v1/client`
- `POST /v1/client/sign_ins`
- `POST /v1/client/sign_ins/{id}/prepare_first_factor` (email_code)
- `POST /v1/client/sign_ins/{id}/attempt_first_factor` (email_code)
- `POST /v1/client/sign_ups` (Phase 2)
- `POST /v1/client/sign_ups/{id}/prepare_verification` (email_code)
- `POST /v1/client/sign_ups/{id}/attempt_verification` (email_code)
- `POST /v1/client/sessions/{session_id}/tokens`
- `POST /v1/client/sessions/{session_id}/end`

## Error Handling
- Input validation in dialogs (basic email/empty checks).
- Service returns user-friendly messages; App sets `status_message`.
- Timeouts on HTTP (15s); convert network/Clerk errors to concise UI strings.

## Security
- Store only minimal data (session id, token, email).
- File permissions via default umask; future: enforce 0600 on Linux/mac.
- Future: Keychain (macOS) storage option.

## Testing
- Manual TUI: sign-in flow, cancel/esc, invalid inputs.
- Env gating: with/without Clerk env set.
- Subscription/account links: macOS open success/failure.
- Persistence: session file created/cleared.

## Phase Status
- Done (Phase 1 scaffolding)
  - UI overlays, keybinding, AppMode wiring.
  - Service + API + Store scaffolds; stubs return success.
  - Env variables and build integration (reqwest).
- Next (Phase 1 → live)
  - Implement real HTTP in `ClerkApi` using `reqwest`:
    - Form-encoded payloads where required; pass publishable key/Origin as needed.
    - Map Clerk JSON to minimal models; surface precise error messages.
  - Cross-platform open: `xdg-open` (Linux), `start` (Windows).
  - Basic retries/backoff for flaky networks.
  - Tests for service flows (mock `ClerkApi`).
- Phase 2 (follow-up)
  - Sign up (email_code) parity with sign-in.
  - Session refresh/renew tokens.
  - Keychain integration (macOS), stricter file perms on Linux.
  - Optional device-code/OAuth providers.

## Implementation Notes
- Auth is opt-in: `AuthService::new_from_env()` sets `enabled=false` if env missing; UI shows informative status.
- No changes to bash services; Rust TUI talks to Clerk directly.
- Keep TUI responsive: all HTTP lives in service; UI stays non-blocking with short status messages.

## How to Use (dev)
```sh
# Build
cargo build --release --manifest-path tui/Cargo.toml

# Run (via wrapper)
./orchestra.sh
# Press `s` to open Account menu
```

## Risks & Mitigations
- Network/API variance → timeouts + clear UI errors; retries where safe.
- Token handling → minimal storage + future secure store.
- Cross-platform open → add `xdg-open`/`start` fallbacks.
