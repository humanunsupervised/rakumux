# orchestra-api

Orchestra's external API: HTTP/1.1 + JSON on **127.0.0.1**, exposing a subset of the session
operations of the running Orchestra daemon (`orchestra-ptyd`), plus WebSocket streams for a live
terminal and for session events. It is a thin adapter over `session_core::client`: the daemon
still decides everything (folder confinement, permissions, audit). The contract is
[`openapi.yaml`](openapi.yaml) (OpenAPI 3.1).

`orchestra-mcp` (see `../mcp`) is its main local client; scripts can use it directly. With
[remote access](#remote-access) on, it also serves the Orchestra app on your phone over Tailscale.

## Running

```bash
orchestra-api                      # foreground, any free port; Ctrl-C stops it
orchestra-api --port 7420          # a fixed port (still 127.0.0.1 only)
orchestra-api --detach             # background; returns once it answers /v1/health
orchestra-api --ptyd-path <bin>    # daemon binary to start when none runs
orchestra-api --remote-listen 127.0.0.1:7400   # remote listener here (remote access must be on)
```

- One API per Orchestra home: a second `orchestra-api` exits with "another orchestra-api is
  running"; `--detach` with a healthy API already running just prints its URL.
- The API starts the daemon on the first user request when none runs (like the CLI), looking for
  `orchestra-ptyd` in `--ptyd-path`, `$ORCHESTRA_PTYD_BIN`, next to `orchestra-api`, then `PATH`.
  It never stops the daemon; stopping the API leaves every session running.
- `SIGINT`/`SIGTERM` stop it cleanly: requests in flight finish and `api.json` is removed.
- A detached API logs to `<home>/logs/api.log` (rotated to `api.log.1` above 1 MiB at start).
  `ORCHESTRA_API_LOG=debug` logs every request (never tokens).
- `ORCHESTRA_HOME` selects an isolated Orchestra home (tests); the default is `~/.orchestra`.

## Discovery: `api.json`

While it runs, the API writes `<ORCHESTRA_HOME or ~/.orchestra>/api.json`, mode 0600, atomically:

```json
{ "url": "http://127.0.0.1:39123", "token": "<64 hex chars>", "pid": 4242, "version": "0.6.0",
  "remote_urls": ["http://100.88.1.2:7337"] }
```

- The token is 32 random bytes, new on every start.
- `remote_urls` is there only while a remote listener runs: what it bound.
- A file whose `pid` is gone is stale: clients ignore it and the next API replaces it (the running
  API holds a `flock` on `<home>/api.lock`, so a file found by a new API is always stale).
- If the file is deleted while the API runs, the API writes it again within 5 s.

## Security

- **Bearer token** on every route except `GET /v1/health`: `Authorization: Bearer <token>`,
  compared in constant time. Unknown routes answer 401 before 404, so they reveal nothing.
- **Host check**: `Host` must be `127.0.0.1`, `localhost` or `[::1]` (any port), otherwise 403.
  This defeats DNS rebinding.
- **No browsers**: a request with an `Origin` or `Sec-Fetch-Site` header gets 403, and the API
  never sends CORS headers.
- Bodies must be `Content-Type: application/json`, at most 64 KiB; unknown body fields and query
  parameters are rejected (400).

The token keeps other local users and web pages out. Like the daemon's own tokens it does not stop
a hostile process running as the same user (which can read `api.json`).

These rules are the loopback listener's. The remote listener has its own; see
[Remote access](#remote-access).

## Identity

Without identity headers the API acts as **the user**: every folder, every operation the daemon
allows users. With both

```
X-Orchestra-Session-Id: $ORCHESTRA_SESSION_ID
X-Orchestra-Session-Token: $ORCHESTRA_SESSION_TOKEN
```

(the variables Orchestra sets in every session) the API opens a daemon client **as that session**,
so the daemon's agent policy applies: the session is confined to its own folder, may read and
list anything in it, may create sessions in it, and may rename only itself (controlling other
sessions is planned for 0.7.0). Creates are limited: at most 4 running sessions per creating
session, nesting 2 deep, 8 running agent-created sessions per folder and 10 creates a minute;
beyond that the answer is 429 `limit_exceeded`, with the limit in `details`. Sending only one of the
two headers is a 400.

For agents whose tool calls arrive without the session's environment (OpenCode 2.x runs MCP servers
in a service shared by every conversation), send `X-Orchestra-Agent: opencode` and
`X-Orchestra-Agent-Conversation: <conversation id>` instead: the API acts as the running session that
bound that conversation with `orchestra session bind` (the Orchestra plugin for OpenCode does this).
No such session is a 401 with `details: {"agent_conversation": "unbound"}`; a daemon without the
`auth.delegated` capability, a 501 with `"unsupported"`. Agent and session headers together are a
400. A session client never
starts the daemon.

## Endpoints

A session's **folder** is the canonical directory it was started in (the daemon's protocol calls it
`scope`, and its folder operations `project_*`). Wherever a `folder` is accepted it may be an
absolute directory (`~/` expanded) or the name of a known folder.

| Method and path | Purpose |
|---|---|
| `GET /v1/health` | API version, pid, whether the daemon answers and its capabilities. No auth; never starts the daemon. |
| `GET /v1/folders` | Folders the caller can see (users: all; sessions: their own): `{"folders": [...], "projects": [...]}`. A daemon without the `projects` capability answers 501 `unsupported`. |
| `GET /v1/sessions?folder=&name=&q=&include_dead=` | List sessions. `folder` (directory or folder name) may repeat; omitted → the caller's own folder (sessions) or every folder (users; a daemon without `projects` gets a 400 asking for `folder`, with `details.missing = "folder"`). `name` is exact, `q` a case-insensitive substring of the name. |
| `GET /v1/sessions/{id}` | One session. |
| `POST /v1/sessions` | Create: `{folder, name?, cwd?, command?: [argv], startup_commands?: [..], cols?, rows?}` → 201 + `Location`. |
| `GET /v1/sessions/{id}/content?source=screen\|scrollback&lines=&format=text\|ansi` | Read the terminal. `scrollback` defaults to the last 200 lines. Output is untrusted (`untrusted: true`). |
| `PATCH /v1/sessions/{id}` | `{name}` → rename. |
| `GET /v1/sessions?all=true` | Every folder's sessions (the daemon's `sessions.all`, else the union of its folders), most recently active first. Not with `folder`; session callers get 403. |
| `POST /v1/sessions/{id}/revive` | Revive a dead or exited session (its resume command runs) → 200 `{"session": {...}}`, the same envelope as the other session routes. Optional body `{cols?, rows?}`; a live session is a 400. |
| `POST /v1/sessions/{id}/kill` | End its program (not the sessions it created) → 204; not live → 409 `session_not_live`. |
| `DELETE /v1/sessions/{id}` | Delete the session → 204. |
| `GET /v1/events` (WebSocket) | Session events of every folder (a session caller: its own). |
| `GET /v1/sessions/{id}/attach?role=observer\|controller&cols=&rows=` (WebSocket) | The live terminal. `cols`/`rows` are required for a controller. |

Sessions come back as `{"session": {...}}` / `{"sessions": [...]}` with `folder` (the daemon's
`scope`), `status` (`live`/`exited`/`dead`), `lifecycle` (`working`/`blocked`/`done`/`idle`/
`unknown`/`none`), `command`, `git`, `terminal` and RFC 3339 times; see `Session` in the OpenAPI
document.

**Deprecated `project` names.** Earlier builds said "project"; those names still work and are
marked deprecated in the OpenAPI document:

- `GET /v1/projects` is an alias of `GET /v1/folders` (same handler, same answer).
- The `project` query parameter and body field are aliases of `folder`. `folder` wins; giving both
  with different values is a 400 `invalid_argument`.
- Responses carry `project` next to every `folder` and `projects` next to `folders`, with the same
  values.

Errors are `{"error": {"code", "message", "details"?}}`. Daemon codes map to statuses:
`not_found` 404, `invalid_argument` 400, `unauthenticated` 401, `out_of_scope`/`permission_denied`
403, `session_not_live`/`session_in_use` 409, `limit_exceeded` 429, `timeout` 504, `daemon_unavailable` 503,
`unsupported` 501, anything else 500. The API's own: `method_not_allowed` 405 (with `Allow`),
`payload_too_large` 413, `unsupported_media_type` 415, `upgrade_required` 426 (a WebSocket route
without the upgrade).

## Streams (WebSocket)

`GET /v1/events` and `GET /v1/sessions/{id}/attach` are WebSockets (RFC 6455, version 13) on the
same port as everything else, with the same checks (bearer token, identity headers on loopback).
The daemon stream is opened before the upgrade, so its errors are ordinary JSON answers: 404 for
an unknown session, 409 `session_not_live` for one that isn't running. The server pings every 30 s
and drops a client that didn't answer the previous ping; it closes the socket when the stream ends.

**`/v1/events`** (server → client, text):

- `{"type":"subscribed"}` once, first;
- `{"type":"event","folder":"/path","session_id":"01J…","at":<unix ms>,"event":{…}}`, `event` one
  of `{"kind":"created","session":Session}`, `{"kind":"updated","session":Session}`,
  `{"kind":"removed"}`, `{"kind":"exited","code":0}`, `{"kind":"lifecycle","lifecycle":"working"}`,
  `{"kind":"reported","reported":{…}}`, `{"kind":"activity"}` (`Session` as in the other answers).

**`/v1/sessions/{id}/attach`**:

- server → client: first `{"type":"attached","session":Session,"role":"observer"|"controller",
  "cols":C,"rows":R,"checkpoint":"<base64>","offset":N}` (feed the checkpoint to a fresh terminal of
  `cols`×`rows` to show the current screen); then **binary** frames of terminal output, in order
  from `offset`, and text events `{"type":"resized","cols","rows","checkpoint","offset"}` (reset the
  terminal to it), `{"type":"role_changed","role"}`, `{"type":"title","title"}`, `{"type":"bell"}`,
  `{"type":"exited","code"}`, `{"type":"lagged"}` (output was dropped: attach again);
- client → server: **binary** frames are input bytes (a controller's; an observer's are ignored);
  text `{"type":"resize","cols","rows"}` (controller), `{"type":"promote","cols","rows"}` (become
  controller at this size), `{"type":"demote"}`, `{"type":"fit","cols","rows"}` (observer: take this
  size unless the controller is typing). Closing the socket detaches.

## Remote access

Off by default: nothing listens on the network until you turn it on.

```bash
orchestra remote on [--port N]     # turn it on (it stays on) and print URL, token, link and QR code
orchestra remote pair              # the URL, token, link and QR code again
orchestra remote status [--json]   # on/off, the URLs the running API listens on, the token's start
orchestra remote token [--rotate]  # print the token; --rotate makes a new one (re-pair the app)
orchestra remote off               # turn it off
```

`orchestra remote on` writes `<home>/remote.json` (mode 0600) and restarts the API (`SIGTERM` to
the pid in `api.json` when that process is `orchestra-api`, then `orchestra-api --detach`):

```json
{ "enabled": true, "port": 7337, "token": "<26 chars a-z2-7>", "listen": [] }
```

It prints the URLs (`http://<MagicDNS name>:7337` when `tailscale status --json` names the machine,
then `http://<tailscale ip>:7337`), the token and a link, `orchestra://connect?url=…&token=…`, that
fills both in when tapped on the phone; on a terminal also the link as a QR code to scan with the
app (`orchestra remote pair` shows it again). The TUI has the same switch (Settings → Phone access)
and asks once, when Tailscale runs and remote access was never set up. When Orchestra starts and remote access is on but no API
runs (after a reboot), it starts `orchestra-api --detach` in the background.

- **Where it listens.** The API keeps its loopback listener and adds a remote listener on every
  Tailscale address of the machine at `port`: IPv4 in `100.64.0.0/10` or IPv6 in
  `fd7a:115c:a1e0::/48` (found with `getifaddrs`) **and** Tailscale's: on its own interface
  (`tailscale0` on Linux, any `tailscale*` name), or listed by `tailscale status` (macOS, where the
  interface is an anonymous `utun`). The ranges alone aren't enough: `100.64.0.0/10` is the
  carrier-grade NAT range some ISPs and hotspots hand out, and other VPNs (Cloudflare WARP) use it
  too; those addresses are never bound. `remote.json`'s `listen` (`["ADDR:PORT", …]`) or
  `orchestra-api --remote-listen ADDR:PORT` (repeatable) binds those addresses instead, for other
  networks or for `tailscale serve` (below). No Tailscale address and no `listen`: a warning in
  `logs/api.log`, loopback only. `api.json`'s `remote_urls` says what was bound.
- **Who gets in.** Every request except `GET /v1/health` needs `Authorization: Bearer <remote
  token>` (constant-time compare; the API token doesn't work there, nor the remote token on
  loopback). Health without the token says only "the daemon is not reachable" in `daemon.error`
  (the detailed reason names local paths). A remote caller is **the user**: it can do what the TUI
  does, including typing into any
  session. The identity headers (`X-Orchestra-Session-*`, `X-Orchestra-Agent*`) are refused (403).
  The remote listener doesn't check `Host` and allows `Origin` (React Native's WebSocket sends one);
  the token is what admits a request. Same routes as on loopback.
- **Keep the token secret.** Anyone on your tailnet with it can run commands as you. Rotate it
  (`orchestra remote token --rotate`) when a phone is lost; the API restarts and the old token
  stops working at once.
- **Encryption.** Traffic over Tailscale is encrypted by WireGuard, so plain `http://` to a
  Tailscale address is private to your tailnet. On any other network, put TLS in front.

### HTTPS with `tailscale serve`

For a real certificate (`https://<machine>.<tailnet>.ts.net`), let Tailscale terminate TLS and
proxy to the API (enable HTTPS certificates for the tailnet in the Tailscale admin console first).

**Proxy to the remote listener, never to the API's loopback port** (the `url` in `api.json`): the
loopback listener refuses a non-loopback `Host` and any `Origin`, and requests that come through
`tailscale serve` carry the `ts.net` name as `Host` (and React Native's WebSocket may send an
`Origin`). The remote listener checks neither, and wants the remote token.

```bash
orchestra remote on            # the remote listener on the Tailscale addresses, port 7337
tailscale serve --bg --https=443 http://<tailscale-ip>:7337
tailscale serve status
```

`tailscale serve` documents local targets (`localhost:3000`, `127.0.0.1`); if yours refuses a
Tailscale IP as the target, bind the remote listener on loopback instead (it is still the remote
listener, with its rules) and proxy to that:

```bash
# In ~/.orchestra/remote.json:  "listen": ["127.0.0.1:7337"]
#   (add "<tailscale-ip>:7337" too to keep the plain http:// tailnet URL working)
orchestra remote on
tailscale serve --bg --https=443 http://127.0.0.1:7337
```

WebSockets are proxied too. In the app, use `https://<machine>.<tailnet>.ts.net` and the same
token. `tailscale serve reset` undoes it. Use `serve`, never `funnel`: `funnel` publishes the port
on the public internet.

## curl

```bash
API=$(jq -r .url ~/.orchestra/api.json); TOKEN=$(jq -r .token ~/.orchestra/api.json)
auth=(-H "Authorization: Bearer $TOKEN")

curl -s "$API/v1/health"
curl -s "${auth[@]}" "$API/v1/folders"
curl -s "${auth[@]}" --get "$API/v1/sessions" --data-urlencode "folder=$PWD"
curl -s "${auth[@]}" --get "$API/v1/sessions" --data-urlencode "folder=$PWD" -d name=reviewer
curl -s "${auth[@]}" "$API/v1/sessions/01JAB…"

curl -s "${auth[@]}" -X POST "$API/v1/sessions" -H 'Content-Type: application/json' \
  -d "{\"folder\": \"$PWD\", \"name\": \"tests\", \"startup_commands\": [\"cargo test\"]}"

curl -s "${auth[@]}" "$API/v1/sessions/01JAB…/content?source=scrollback&lines=50" | jq -r .text

curl -s "${auth[@]}" -X PATCH "$API/v1/sessions/01JAB…" -H 'Content-Type: application/json' \
  -d '{"name": "unit tests"}'

# From inside an Orchestra session, act as that session:
curl -s "${auth[@]}" -H "X-Orchestra-Session-Id: $ORCHESTRA_SESSION_ID" \
  -H "X-Orchestra-Session-Token: $ORCHESTRA_SESSION_TOKEN" "$API/v1/sessions"

curl -s "${auth[@]}" "$API/v1/sessions?all=true"
curl -s "${auth[@]}" -X POST "$API/v1/sessions/01JAB…/kill"
curl -s "${auth[@]}" -X POST "$API/v1/sessions/01JAB…/revive"
curl -s "${auth[@]}" -X DELETE "$API/v1/sessions/01JAB…"

# Remote access, from another machine on the tailnet:
REMOTE=http://box.tail1234.ts.net:7337; RTOKEN=$(orchestra remote token)   # on the server
curl -s -H "Authorization: Bearer $RTOKEN" "$REMOTE/v1/sessions?all=true"

# WebSockets, e.g. with websocat:
websocat -H "Authorization: Bearer $TOKEN" "${API/http/ws}/v1/events"
```

(`jq` and `websocat` are only used by these examples; Orchestra itself doesn't need them.)

## Code

```
api/
  openapi.yaml        the contract (tests/openapi_sync.rs keeps it in step with the code)
  src/main.rs         flags; foreground serve or --detach
  src/server.rs       claim, bind 127.0.0.1 (and the remote listener), publish api.json, serve,
                      clean up
  src/remote.rs       remote access: remote.json, Tailscale addresses (getifaddrs), what to bind
  src/instance.rs     one API per home (flock on api.lock)
  src/discovery.rs    api.json (shared with orchestra-mcp)
  src/routes.rs       routing per listener, bodies, responses
  src/guard.rs        Host / Origin / bearer checks, identity headers; the remote listener's rules
  src/query.rs        query parameters
  src/streams.rs      WebSocket handshake, terminal attach and session events streams
  src/sessions.rs     list / get / create / rename / read / revive / kill / delete use cases
                      (against SessionApi)
  src/folders.rs      folders, and `folder` (alias `project`) argument resolution
  src/daemon.rs       daemon clients per identity (blocking; used via spawn_blocking)
  src/dto.rs          JSON shapes (shared with orchestra-mcp)
  src/error.rs        error envelope and status mapping
  src/detach.rs       --detach; src/probe.rs: blocking health probe
  tests/api_e2e.rs    the real binary against a real orchestra-ptyd in a temp ORCHESTRA_HOME
                      (remote listener on 127.0.0.1:0, WebSocket clients)
```

Build and test from `tui/` (the Cargo workspace root): `cargo test -p orchestra-api`.
