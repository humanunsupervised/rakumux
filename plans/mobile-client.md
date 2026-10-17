# Mobile client and remote access (rev 1)

A React Native app (`mobile/`) that shows and drives Orchestra sessions from a phone, talking to
the Orchestra server over the network (normally Tailscale). Until now nothing in Orchestra listened
on the network: `orchestra-ptyd` has a Unix socket and `orchestra-api` binds 127.0.0.1 and refuses
any non-loopback `Host`. This plan adds an opt-in **remote mode** to `orchestra-api` and the
streaming endpoints a terminal client needs, and the app.

## 1. Decisions

1. **Opt-in, once.** `orchestra remote on` enables remote access; it stays on (saved) until
   `orchestra remote off`. Nothing is exposed by default.
2. **Where it listens.** The API keeps its loopback listener (MCP, scripts) and adds a remote
   listener on every local Tailscale address (IPv4 in `100.64.0.0/10`, IPv6 in
   `fd7a:115c:a1e0::/48`, found with `getifaddrs`), port 7337 by default. `--remote-listen
   ADDR:PORT` (repeatable, or `remote.json` `listen`) binds other addresses instead, for networks
   other than Tailscale. Without a Tailscale address and no `listen`, remote mode logs a warning and
   serves loopback only. Traffic over Tailscale is encrypted by WireGuard; HTTPS with a real
   certificate is available by putting `tailscale serve` in front (documented, not automated).
3. **Pairing token.** Remote requests need `Authorization: Bearer <remote token>`: a long-lived
   token in `~/.orchestra/remote.json` (0600), 26 characters of lowercase base32 (128 bits, easy
   to paste), constant-time compared, rotated with `orchestra remote token --rotate`. A remote
   caller is a **user caller** (the user's own phone): it may do what the TUI does. The session
   identity headers (`X-Orchestra-Session-*`, agent delegation) are refused on the remote listener.
   The remote listener doesn't check `Host` and allows an `Origin` header (React Native's WebSocket
   sends one); the token is what admits a request. The loopback listener keeps its rules.
4. **Streaming over WebSocket** (both listeners, same auth as other requests): a terminal attach and
   the session event stream. Plain JSON request/response stays as it is.
5. **The app** is Expo (managed workflow, TypeScript), runnable in Expo Go: a connect screen (server
   URL and token), the session list, and a session screen with the terminal drawn by xterm.js in a
   WebView. It follows the TUI's phone layout: the list alone, or a session full screen under a row
   of session tabs.

## 2. `remote.json` and the CLI

```json
{ "enabled": true, "port": 7337, "token": "<26 chars a-z2-7>", "listen": [] }
```

- `orchestra remote on [--port N]`: create the file (new token if none), `enabled: true`, then
  (re)start the API detached so it serves the remote listener; print the URLs, the token and a
  deep link `orchestra://connect?url=<url>&token=<token>` (tapping it on the phone opens the app,
  filled in). URLs: `http://<MagicDNS name>:<port>` when `tailscale status --json` (optional; never
  required) names this machine, then `http://<tailscale ip>:<port>`.
- `orchestra remote off`: `enabled: false`, restart the API without the remote listener.
- `orchestra remote status [--json]`: enabled, the URLs the running API listens on, the token's
  first 4 characters.
- `orchestra remote token [--rotate]`: print the token; `--rotate` makes a new one and restarts the
  API (paired apps must be updated).
- Restarting the API: SIGTERM to the pid in `api.json` when it is `orchestra-api` (as the TUI does
  for the daemon), wait for it to go, then `orchestra-api --detach`.
- `api.json` gains `remote_urls: [..]` (optional, additive): what the remote listener bound.
- The TUI, at startup, when `remote.json` is enabled and no API answers, starts `orchestra-api
  --detach` in the background, so remote access comes back after a reboot once Orchestra is opened
  on the server.

## 3. API additions (both listeners)

| Route | Purpose |
|---|---|
| `GET /v1/sessions?all=true` | Every folder's sessions (`SessionApi::list_all` when the daemon has `sessions.all`; else the existing union). `include_dead` as before. |
| `POST /v1/sessions/{id}/revive` | Revive a dead or exited session (`resume: true`); returns the session. |
| `POST /v1/sessions/{id}/kill` | End its program (`tree: false`); 204. |
| `DELETE /v1/sessions/{id}` | Delete the session; 204. |
| `GET /v1/events` (WebSocket) | Session events of every folder. |
| `GET /v1/sessions/{id}/attach?role=observer\|controller&cols=C&rows=R` (WebSocket) | The live terminal. |

WebSocket upgrade: standard handshake (`Upgrade: websocket`, `Sec-WebSocket-Key`/`-Accept`,
version 13), on the same port; errors before the upgrade are ordinary JSON error responses.

### `GET /v1/events`

Server → client, text frames, JSON:
- `{"type":"subscribed"}` once;
- `{"type":"event","folder":"/path","session_id":"01J…","at":1792…,"event":{…}}` with `event` one of
  `{"kind":"created","session":Session}`, `{"kind":"updated","session":Session}`,
  `{"kind":"removed"}`, `{"kind":"exited","code":0}`, `{"kind":"lifecycle","lifecycle":"working"}`,
  `{"kind":"reported","reported":{…}}`, `{"kind":"activity"}`; `Session` is the API's existing
  session JSON.
- The server sends a ping every 30 s; a client that doesn't answer is dropped.

### `GET /v1/sessions/{id}/attach`

Server → client:
- first, text: `{"type":"attached","session":Session,"role":"observer"|"controller","cols":C,
  "rows":R,"checkpoint":"<base64>","offset":N}`. Feeding the checkpoint to a fresh terminal of
  `cols`×`rows` shows the current screen;
- binary frames: terminal output bytes, in order, continuing from `offset`;
- text events: `{"type":"resized","cols","rows","checkpoint","offset"}` (reset the terminal to it),
  `{"type":"role_changed","role"}`, `{"type":"title","title"}`, `{"type":"bell"}`,
  `{"type":"exited","code"}`, `{"type":"lagged"}`; the server closes the socket when the stream
  ends (exit, delete, daemon gone).

Client → server:
- binary frames: input bytes (controller only; an observer's are ignored);
- text: `{"type":"resize","cols","rows"}` (controller), `{"type":"promote","cols","rows"}` (become
  controller at this size), `{"type":"demote"}`, `{"type":"fit","cols","rows"}` (observer: take
  this size unless someone is typing); closing the socket detaches.

Errors before the upgrade: 404 unknown session, 409 `session_not_live` for a dead one.

## 4. The app (`mobile/`)

- **Connect**: server URL (placeholder `http://<machine>.<tailnet>.ts.net:7337`) and token; Connect
  checks `GET /v1/health` and `GET /v1/sessions?all=true`, then saves both (expo-secure-store).
  The `orchestra://connect?url=…&token=…` link fills them in. Settings can forget the server.
- **Sessions**: grouped by folder (name, branch), each row its status glyph, name, branch, what its
  agent is doing (working, needs you, done, idle) and when it was last active; updated live from
  `/v1/events`, pull to refresh; `+ New session` (name, folder from `/v1/folders`). Tapping a live
  session opens it; a dead or exited one offers Revive.
- **Session**: a row of session tabs on top (tap to switch), the terminal below (xterm.js in a
  WebView, sized with its fit addon), attached as controller at that size. The keyboard types into
  it; a key bar above the keyboard has Esc, Tab, Ctrl (sticky), arrows, ^C and paste. Back (or
  swipe) returns to the list and detaches. A lost connection reconnects and attaches again.
- No Orchestra code is shared with Rust; the app has its own small client (`src/api/`) for the
  contract above, with unit tests against a mock server.

## 5. Work packages

| | Package | Files |
|---|---|---|
| A | Remote mode, remote.json, WebSocket attach and events, revive/kill/delete, `all=true`, `orchestra remote`, TUI autostart, OpenAPI, tests (api_e2e with a real daemon and real WebSocket clients) | `api/`, `tui/src/cli/`, `tui/src/main.rs` (autostart only), `api/README.md`, AGENTS.md sections |
| B | The Expo app, its API client, a mock server for development, tests, `mobile/README.md` | `mobile/` |

A and B run in parallel against this contract; then the coordinator runs the app's client against a
real remote API on an isolated `ORCHESTRA_HOME`.

## 6. Status

- rev 1 (2026-10-16): plan.
- B done (2026-10-16): `mobile/` (Expo SDK 58): connect, sessions, session screens; the client
  (`src/api`) for §3; `dev/mock-server.mjs`; Jest tests (the clients against the mock server).
  Not yet run against package A's server or on a phone.
- A done (2026-10-16): `remote.json` (`session_core::remote`), the remote listener and
  `--remote-listen` (`api/src/remote.rs`, `guard.rs`), `api.json` `remote_urls`, revive / kill /
  delete / `all=true`, WebSocket attach and events (`api/src/streams.rs`), `orchestra remote`
  (`tui/src/cli/remote.rs`), the TUI autostart, OpenAPI, `api/README.md` (Remote access, `tailscale
  serve`), end-to-end tests with a real daemon and tokio-tungstenite clients. Additions within the
  contract: an optional revive body `{cols?, rows?}`, 426 `upgrade_required` for a WebSocket route
  requested without the upgrade, 400 for `all=true` with `folder` and for a controller attach
  without `cols`/`rows`, and `role` defaults to `observer`. Revive answers `{"session": Session}`;
  reviving a live session is the daemon's 400 `invalid_argument` (the app's mock says 409).
  `tailscale serve` proxies to the remote listener (a Tailscale address, or the remote listener
  bound on 127.0.0.1 through `listen`), never to the loopback listener.
- Pairing (2026-10-17): the remote listener binds only addresses on Tailscale's own interface
  (`session_core::tailscale`; the `100.64.0.0/10` range alone also matches ISP carrier-grade NAT
  and other VPNs). The pairing link is shown as a QR code (`orchestra remote on`/`pair` on a
  terminal, the TUI's pairing dialog) and the app scans it. Phone access stays off by default; the
  TUI asks once when Tailscale runs and `remote.json` doesn't exist, and Settings → Phone access
  turns it on or off.
