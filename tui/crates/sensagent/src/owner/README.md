# Per-home AI-status owner

One owner per Rakumux home runs the [refresher](../refresh/README.md) and serves user
clients (the TUI, later) over a private Unix socket. This module provides its file
layout, single-instance claim, protocol, server and client. It does **not** install a
binary, spawn a process, configure a provider or enable sign-in or inference: the
[developer example](../../examples/status_owner.rs) composes the parts with explicit
configuration.

## Layout (proposed, for review)

```text
$RAKUMUX_HOME/
  sensagent/                 0700, the owner's alone (never chmodded once created)
    status/                  consent and cached status (status.sqlite3, status.lock)
    credentials/             sign-in credentials (unencrypted private files)
  run/                       shared with rakumux-ptyd, 0700
    sensagent.json           registration: pid, socket, token, protocol (0600)
    sensagent.lock           flock held by the running owner
/tmp/rakumux-<uid>/sensagent-<hash>.sock
```

- The socket sits next to the daemon's and reuses its hash of the runtime directory,
  so an isolated `RAKUMUX_HOME` gets its own owner. Nothing here reads the daemon's
  registry, registration or socket.
- `OwnerClaim::claim` prepares the layout, takes the lock without waiting (a second
  owner gets `AlreadyRunning`), replaces a dead owner's socket and registration, binds
  the socket (0600) and writes the registration atomically (0600). Dropping the claim
  removes only files that are still its own.
- The architecture's root-level `sensagent.sqlite3` is replaced by the store's own
  dedicated directory, which it already requires. Backups, migrations and macOS
  certification remain open.
- Directories are created with a plain `mkdir` and checked afterwards, so something
  planted at their path is refused rather than followed. The registration's temporary
  file is created fresh, never reused.
- The socket hash uses the runtime directory as spelled, like the daemon's: clients
  must name the home the same way (a symlinked spelling is refused, not followed).

## Protocol

`session_core::frame` control frames carrying JSON, each at most 256 KiB, with the
length checked before the body is read. The client sends a hello with the protocol
version and the registration's token; then requests and responses alternate.

| Request | Answer |
|---|---|
| `state` | account (subject, email), consent (revision, folders), whether the daemon is followed, rate limited or the account refused |
| `statuses {sessions}` | one line per session, in order (at most 256): state, advisory text and capture time when ready, last refresh failure |
| `consent_offer` | origin, account and consent revision to show, with a one-use id |
| `enable {offer, folders}` | the new consent; the offer must be this connection's latest |
| `disable {revision}` | the new consent |
| `sign_out` | done; sharing stops, local credentials are removed, then the sign-in is revoked at Clerk (best effort) |
| `sign_out_and_revoke` | the same, answered `signed_out {revocation}`: `revoked`, `not_needed` (nothing to revoke, or an enrolled machine's) or `failed` (signed out here all the same) |

Errors are static codes: `unauthenticated`, `protocol_mismatch`, `invalid_request`,
`stale_offer`, `conflict` (a `disable` for an older consent revision), `signed_out`,
`unavailable`, `stopped`, `busy` (too many clients). New fields are optional and
answers carry an `unknown` fallback, as in the daemon's protocol; folder entries use
their own wire type (`FolderChoice`), since the stored consent refuses unknown fields.

After any failed request the client closes its connection, so a late answer can't be
read as the next request's; later calls fail with `connection` until the caller
reconnects.

An offer lives only in the connection that asked for it, so another window can't
confirm it; a newer offer, sign-out or any consent or account change makes it stale.
Every new owner process needs consent confirmed again (the coordinator's rule), so
a restarted owner shows `consent_required` until a user confirms.

## Who may connect

The server accepts peers of the same uid that present the registration's token, like
the daemon's user clients. Session callers get no token in their environment, but a
same-user process can read the registration file: this is attribution and accident
prevention, not a sandbox against a hostile local process (the daemon makes the same
trade-off). The client checks that the registration is this user's private file,
names this layout's socket and that the process listening there runs as this user,
before sending the token.

Each connection gets a thread; past 32 at once a client is told `busy`. A hello must
be complete within 5 s however slowly it arrives (at most 4 KiB). Requests go to the
refresher's owner thread, so they are answered in order. `stop` (or drop) closes every
connection, under the same lock that registers new ones, and wakes the accept loop
through a socket pair, so it works even after the socket file is gone. Connection
threads are not joined; each ends once its socket closes.

## Credential lock

The owner reads credentials from several threads (status checks on the owner thread,
requests on workers). Reads now share the credential store's lock (records are only
replaced by rename), replacements take it alone, and either waits up to 250 ms before
`Busy`. Before, a status poll that coincided with a request could fail the account
check, which correctly but needlessly stopped sharing until consent was confirmed
again. A replacement whose disk sync stalls for longer still fails closed that way.

## Remaining gates

Sign-in over the socket (device grant start, progress, cancel), an installed binary and
who starts it, TUI controls and the third session-list line, pushing status changes
instead of polling, automatic credential refresh, and live Clerk/TLS/model
certification.

## Tests

`owner/tests.rs` covers the layout, claim, registration checks and framing with
temporary homes. `cloud/client/tests/dispatch/owner.rs` runs the real server and
client in front of a running refresher with a fake daemon and fake HTTP: state and
status lines, one-use offers, refused tokens, protocols and malformed or oversized
requests, and shutdown. Sockets go in `/tmp/rakumux-<uid>/` with a per-test hash, as
the daemon's tests do.

```sh
cargo test -p rakumux-sensagent owner --locked --offline
```
