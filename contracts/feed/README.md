# Agent feed wire contract, version 1

Canonical JSON Schema draft 2020-12 for the feed routes of rakumux.com: what agents said when
they finished a turn, and the whole chats those turns belong to (the user's prompts, the agents'
text, reasoning and tool calls, and the tools' output), uploaded by each machine's Rakumux status
service and read back by every machine of the same account. This directory contains contracts and development-only
conformance tests, not an HTTP server, database or local runtime.

## Routes and messages

| Route | Request | Success |
|---|---|---|
| `POST /api/v1/feed/entries` | [upload](upload.schema.json) | [accepted](accepted.schema.json) |
| `GET /api/v1/feed?after=SEQ&limit=N` | query only | [page](page.schema.json), increasing seq |
| `GET /api/v1/feed?before=SEQ&limit=N`, `GET /api/v1/feed?limit=N` | query only | [page](page.schema.json), decreasing seq |
| `POST /api/v1/feed/messages` | [chat-upload](chat-upload.schema.json) | [chat-accepted](chat-accepted.schema.json) |
| `GET /api/v1/feed/turn?entry=ID[&after=POSITION]` | query only | [turn](turn.schema.json), increasing position |
| `POST /api/v1/feed/replies` | [reply-upload](reply-upload.schema.json) | [reply](reply.schema.json) |
| `GET /api/v1/feed/replies?after=SEQ&limit=N` | query only | [replies](replies.schema.json), increasing seq |
| `POST /api/v1/feed/replies/status` | [reply-status](reply-status.schema.json) | [reply](reply.schema.json) |
| `POST /api/v1/feed/sessions` | [sessions-upload](sessions-upload.schema.json) | [sessions-stored](sessions-stored.schema.json) |

- **Upload:** `{version:1, machine, entries}` with 1–50 entries. Each entry is
  `{id, session:{id, name, folder?, branch?}, agent, conversation_id?, text, truncated,
  finished_at, position?}`; `position` is the turn's last message's in its chat (below).
  `id` is the ULID the local daemon gave the reply; the cloud keeps the first upload of an
  id and answers a repeat with the same `seq`, so retries never duplicate. `folder` is a
  folder's last path component, never a path; `branch` is the git branch the session's agent
  worked on when the turn finished (absent when detached or unknown; a chat upload leaves it
  out). `text` is at most **32768 UTF-8 bytes**;
  `truncated` says the reply was longer. `finished_at` is the machine's clock (Unix ms);
  the cloud may clamp it to its own receipt time.
- **Accepted:** every uploaded id with its `seq`, in upload order.
- **Page:** `{version:1, entries, latest_seq, more}`. `seq` is the account's arrival order, the
  only cursor: `after` syncs what arrived since, `before` pages back. `limit` is 1–200
  (default 50); the cloud may return fewer to keep the response small, with `more` true.
  `after` and `before` together are invalid. Each entry adds `seq`, `machine` and
  `received_at` to what was uploaded.
- **Chats:** a chat is one agent conversation on one machine: `(machine, agent,
  conversation_id)` for the account. `POST /api/v1/feed/messages` uploads 1–100 of its messages,
  `{version:1, machine, chat:{agent, conversation_id, session}, messages}`; each message is
  `{id, position, role, at?, parts}` with 1–64 parts, and uploading an id again replaces that
  message and its parts (OpenCode rewrites its messages as they stream). `role` is `user` (what
  the user typed), `assistant` (the agent's text, reasoning and tool calls) or `tool` (tools'
  output). A part is `text` or `reasoning` `{text}`, `tool_call` `{call_id?, name, input}` or
  `tool_result` `{call_id?, output, error?}`, each with `truncated?`; texts are at most **65536
  UTF-8 bytes**, plain text (tab, LF, CR), and a tool's input or output may be empty. Messages sort
  by `position`, then `id`: Claude Code's and Codex's are byte offsets into their logs, OpenCode's
  its own order times 1024 plus a block's index. The session is the one that last showed the chat.
- **Turn:** `GET /api/v1/feed/turn?entry=ID` gives the messages of the turn a feed entry finished:
  from the chat's last `user` message at or before the entry's `position` up to that position,
  increasing; `more` says it continues, and `after=POSITION` asks for the rest. An entry without
  `position` (an older machine) ends at its chat's last message at or before `finished_at`. No
  messages when the chat wasn't uploaded. At most 200 messages per answer.
- **Replies and threads:** the user can write back to any entry with a conversation id, from
  rakumux.com or a machine (`reply-upload`, `{version:1, id, entry, text, from}`; `text` at most
  **8192 UTF-8 bytes**). The cloud stores it `pending` for the entry's machine, session, agent
  and conversation, with its `thread`: the entry that started it (an entry that answers a reply
  belongs to that reply's thread). The machine reads the account's replies after a seq
  (`replies`; a reply takes a new seq whenever it changes, and pending ones older than an hour
  are `expired` first), types each pending one into its session when the session still shows
  that conversation and the agent waits for input, and reports `delivered` or `failed` with a
  `failure` code (`reply-status`; only the reply's own machine, only a pending reply). The
  agent's next finished reply there is uploaded with `reply_to`, and the cloud records it as
  the reply's `response`; page entries carry `reply_to` and `thread`. Machines take replies
  written elsewhere only when the user turned that on there (`not_accepted` otherwise).
- **Sessions:** a machine's whole list of sessions now (`sessions-upload`, `{version:1,
  machine, sessions, at}`, at most 200; `sessionState` in common), replacing what was stored for
  it: a session left out is gone. Sent when the list changes and every minute meanwhile, so a
  machine that stops sending is known to be away. Each session says what the TUI's Status and
  Folders tabs group by: its name, folder name and branch, agent, `status` (`live` or
  `exited`; dead sessions aren't listed), `lifecycle`, the agent's `phase` when it reports one,
  the `conversation_id` it shows, `pinned` and `last_active_at`. Private sessions are never
  listed. The answer says how many are stored (`sessions-stored`). Kept while the account's
  Feed switch is on.
- **[Error](error.schema.json):** `{error:{code, message, retry_after_seconds?}}`, one static
  message per code: `invalid_request` (400, or 413 for an oversized body), `unauthenticated`
  (401), `forbidden` (403: the account's Feed switch is off), `rate_limited` (429) and
  `unavailable` (503). Retry delays are 1–86400 seconds, only for the last two. A turn, reply
  upload or reply status naming an entry or reply the account doesn't have is
  `invalid_request` with 404; a reply to an entry without a conversation id is
  `invalid_request` (400), and one to an entry that isn't the newest of its session (its
  machine's and session's latest by `finished_at`, then `seq`) is `invalid_request` with 409:
  only a session's newest message can be answered. A repeated upload of a stored reply still
  answers with the stored reply.

Every object rejects additional properties. Authorization is the terminal's Clerk OAuth
access token in the `Authorization` header, never the body; browser cookies and `Origin`
requests are refused. The user is the verified token's subject, never a body field.

## Validation boundaries

1. Bound the raw body before parsing: the upload's `x-maxBodyBytes` (1 MiB) on the server,
   the page's (2 MiB) on the client, 4 MiB for a chat upload and a turn. Reject malformed UTF-8 or JSON, a byte-order mark and
   duplicate member names.
2. Implement `maxUtf8Bytes` as an assertion on the decoded string's UTF-8 length.
   `maxLength` counts code points, not bytes.
3. Apply the schemas without coercion, defaults or removing extra fields.

## What is not in it

No terminal output, full paths (beyond what the chat's own text holds), account ids or
credentials. Schemas reject such fields, not secrets inside the text: the status service sends
replies and chats only while the account's Feed switch is on, never a private session's, and
masks keys, credential assignments and tokens in every text first (best effort).

## Conformance tests

From `contracts/feed/`, with Node.js 22 or newer:

```sh
npm ci --cache .npm-cache --ignore-scripts --no-audit --no-fund
npm test
```

AJV **8.20.0** is the sole development dependency, as in `contracts/session-status/`.
Tests use local schemas and the synthetic fixtures in [`fixtures/`](fixtures/) only.
