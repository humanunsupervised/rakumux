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
| `GET /api/v1/feed/replies?after=SEQ&limit=N&machine=NAME&session_messages=1` | query only | [replies](replies.schema.json), increasing seq |
| `POST /api/v1/feed/replies/status` | [reply-status](reply-status.schema.json) | [reply](reply.schema.json) |
| `GET /api/v1/feed/replies/attachments/{id}` | path only | the file's bytes, not JSON (below) |
| `POST /api/v1/feed/sessions` | [sessions-upload](sessions-upload.schema.json) | [sessions-stored](sessions-stored.schema.json) |
| `GET /api/v1/feed/realtime` | header only | [realtime](realtime.schema.json) |
| WebSocket at the realtime `url` | bearer `token` | [poke](poke.schema.json) messages |
| `POST /api/v1/machines/credentials` | [machine-enrol](machine-enrol.schema.json) | [machine-credential](machine-credential.schema.json) |
| `GET /api/v1/machines/credentials` | header only | [machine-credentials](machine-credentials.schema.json) |
| `DELETE /api/v1/machines/credentials/{machine_id}` | path only | 204, no body |
| `POST /api/v1/machines/token` | no body; bearer: the machine secret | [machine-token](machine-token.schema.json) |

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
  only cursor: `after` syncs what arrived since, `before` pages back. The account's seqs are
  given in the order their uploads commit, so a reader that continues past the greatest seq it
  read (and past `latest_seq` once `more` is false) never misses one. `limit` is 1–200
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
  (`replies`, naming itself with `machine`; a reply takes a new seq whenever it changes, and
  pending ones older than an hour are `expired` first) as often as the answer's `poll_ms` says
  (1000 while a reply that machine hasn't read yet waits for it, or the user has the website
  open, 15000 otherwise; within 1–60 s; a machine keeps its own pace when it's absent). An
  account's seqs are given in the order their changes commit, so a reader that continues past
  the greatest seq it read (and past `latest_seq` once `more` is false) never misses one. It
  types each pending one into its session when the session still shows
  that conversation and the agent waits for input, and reports `delivered` once the agent took
  it as a prompt or `failed` with a `failure` code (`reply-status`; only the reply's own machine,
  only a pending reply): `not_taken` when it was typed in but the agent's chat showed no new
  prompt within 30 seconds, or another one (its Enter lost, the text in a menu). The
  agent's next finished reply there is uploaded with `reply_to`, and the cloud records it as
  the reply's `response`; page entries carry `reply_to` and `thread`. Machines take replies
  written elsewhere only when the user turned that on there (`not_accepted` otherwise). `from`
  names the machine a reply was written on, for showing; a machine counts a reply as written
  there only when it made the reply's `id` itself, so another machine naming it in `from` can't
  get past that setting.
  Readers take a `failure` they don't know as a failure without a reason.
- **Session messages:** on rakumux.com or a machine the user can also write to a session that
  has no answer to reply to yet (a new chat, or one whose newest answer is in another chat): a
  machine sends `reply-upload` with `session: {machine, id}` in place of `entry`. It is
  stored as a reply without `entry` and `thread`, with the chat the session showed when it
  said one (`conversation_id`), and reaches only machines that read with
  `session_messages=1`, so an older machine never gets a reply it can't parse. The machine
  types it into the session once an agent runs there and waits for input, whatever chat it
  shows (never into a session without an agent: it waits for one), and reports it like a
  reply; the agent's next finished reply answers it (`reply_to`) and starts its own thread.
- **Attachments:** on rakumux.com the user can attach up to 5 files of at most **4 MiB**
  (4194304 bytes) each to a reply or session message, which still has its `text`. The reply
  lists them as `attachments`, `{id, name, media_type, size}` each (`attachment` in common):
  `name` is one visible line of at most 128 characters and 255 UTF-8 bytes, never a path, `.`
  or `..`, and two files may share it; `media_type` is lowercase `type/subtype` without
  parameters, informational only.
  The cloud keeps each file only while its reply is pending, and sends a reply with attachments
  only to a reader whose machine advertises `reply_attachments` in its session list (below),
  so an older machine never gets one it would type without its files. The reply's machine
  fetches every file before typing anything, saves it under a directory of its own for that
  reply, and types the text followed by each file's absolute path; when a file can't be fetched
  or its length isn't `size`, it types nothing and reports `failed` with
  `attachment_unavailable`. `attachments` stays on the reply after its files are gone, so
  threads can still name them.
- **Attachment download:** `GET /api/v1/feed/replies/attachments/{id}`, authorized like the
  other routes, answers 200 with the file as raw bytes: `Content-Type:
  application/octet-stream`, `Content-Length` equal to the attachment's `size`,
  `Content-Disposition: attachment`, `X-Content-Type-Options: nosniff` and `Cache-Control:
  no-store`. A machine names the file from the reply's `name`, never from the response's
  headers, and doesn't trust `media_type`. An id the account doesn't have, of a reply that is
  no longer pending, or whose file was deleted is `invalid_request` with 404; other errors are
  as for every route ([error](error.schema.json)).
- **Sessions:** a machine's whole list of sessions now (`sessions-upload`, `{version:1,
  machine, sessions, at}`, at most 200; `sessionState` in common), replacing what was stored for
  it: a session left out is gone. Sent when the list changes and every minute meanwhile, so a
  machine that stops sending is known to be away. Each session says what the TUI's Status,
  Folders and App tabs group by: its name, folder name and branch, agent (or, without one of
  the three, the `program` the App tab files it under: an agent Rakumux doesn't name, its
  foreground program while it runs, else the one it started with; never a shell), `status` (`live` or
  `exited`; dead sessions aren't listed), `lifecycle`, the agent's `phase` when it reports one,
  the `conversation_id` it shows, `pinned` and `last_active_at`. Private sessions are never
  listed. The answer says how many are stored (`sessions-stored`), and with `accepts` what more
  the cloud takes in a list: a machine sends those only once an answer named them. Known:
  **`places`**, the 8 places a session worked in most recently (`sessionPlace` in common: a git
  working tree where it changed files; no `branch` while its HEAD is detached), most recent first, each `{name, inside,
  branch?, worktree?, edited, last_at}`: `name` is its path in the session's folder when it's
  there (`inside`; `.` for the folder itself), else its last path component, never an absolute
  path. Kept while the account's Feed switch is on. The upload also says what the machine
  handles beyond the first version of this contract (`capabilities`, at most 16 unique names
  matching `^[a-z][a-z0-9_.]{0,31}$`; absent from older machines): `reply_attachments`, it
  fetches a reply's attachments and gives the agent their paths. The cloud keeps the latest list
  and ignores names it doesn't know.
- **Pokes:** instead of reading its replies every few seconds, a machine can be told when to.
  `GET /api/v1/feed/realtime` (the machine id header required, else `invalid_request`) gives a
  WebSocket `url` and a `token` that admits that machine of the account once, before
  `expires_at` (a few minutes); a cloud that offers no pokes answers `invalid_request` with
  404, and the machine reads at `poll_ms` as before and asks again much later. The machine
  connects with `Authorization: Bearer <token>` (never in the URL) and keeps the connection:
  it may send a text `ping` (answered `pong`) to keep it alive, at most every 30 s, and takes a
  connection silent for 90 s as gone. The server sends a `poke` when anything the account's
  machines read about replies changes (a reply or session message written, reported, expired
  or answered) and when the account's website comes into use; it carries nothing else. On a
  poke the machine reads its replies at once. While connected, it reads at `poll_ms` only
  while that is 1000 (someone is watching, or replies are busy), and otherwise once a minute in
  case a poke was lost. The server closes with 4000 when the same machine connects again, 4001
  when a connection is a day old (connect again, with a new token) and 1008 when it refuses;
  the machine reconnects with a new token, backing off from 1 s to a minute.
- **Machine id:** a machine sends its own id with every request, `X-Rakumux-Machine-Id`: a ULID
  it keeps, so two of the account's machines with one name are told apart. The first id to send
  a session list under a name holds that name. A request from another id that names it (`machine`
  in an upload, a reply status or the replies read) is `machine_taken` (409) and changes nothing;
  that machine then takes another name. A request without the header (an older machine) is never
  checked, and an id that isn't a ULID is `invalid_request`.
- **Enrolled machines:** a machine with no sign-in of its own (the user's Fly Sprite) gets one
  from a signed-in machine of the account. That machine enrols it (`machine-enrol`: the id and
  name it will use, and optionally a `wake_url`) with its Clerk token and gets its **machine
  secret** (`rkm_…`) once; the cloud keeps only its hash, one live secret per machine (enrolling
  it again revokes the earlier one), and `DELETE` revokes it. The enrolled machine trades the
  secret (`POST /api/v1/machines/token`, the secret as bearer, its id as
  `X-Rakumux-Machine-Id`) for an **access token** (`rkma.v1.…`) good for 15 minutes, and sends
  that on its other requests, with its id, which must be the one it was enrolled with. A
  revoked or unknown secret, or another id, is `unauthenticated`; at most 60 exchanges an hour
  per secret (`rate_limited`). An access token reaches what a machine does and nothing else:
  the features read, AI session titles, the uploads (entries, messages, sessions) as its own
  machine, and its own replies (the replies read with `machine` set to the name it holds, their
  status and attachments) and the realtime token; the feed and turn reads, writing replies,
  changing features and the credential routes are `forbidden`. When a reply or session message
  is written to an enrolled machine with a `wake_url`, the cloud sends `GET <wake_url>/v1/health`
  (no credential, at most every 10 s) to wake it: a paused Sprite reads nothing until something
  connects to it.
- **[Error](error.schema.json):** `{error:{code, message, retry_after_seconds?}}`, one static
  message per code: `invalid_request` (400, or 413 for an oversized body), `unauthenticated`
  (401), `forbidden` (403: the account's Feed switch is off), `machine_taken` (409: another
  machine holds the name, above), `rate_limited` (429) and
  `unavailable` (503). Retry delays are 1–86400 seconds, only for the last two. A turn, reply
  upload or reply status naming an entry or reply the account doesn't have is
  `invalid_request` with 404; a reply to an entry without a conversation id is
  `invalid_request` (400), and one to an entry that isn't the newest of its session (its
  machine's and session's latest by `finished_at`, then `seq`) is `invalid_request` with 409:
  only a session's newest message can be answered. A session message to a session its machine
  doesn't list is `invalid_request` with 404, and to one that runs no agent or has ended
  `invalid_request` with 409. A repeated upload of a stored reply still
  answers with the stored reply, whatever its entry or session says now: a writer that sends a
  reply again with the same `id` (its answer lost) stores it once. An attachment download whose file is gone is `invalid_request`
  with 404 (above).

Every object rejects additional properties. Authorization is the terminal's Clerk OAuth
access token, or an enrolled machine's access token (above), in the `Authorization` header,
never the body; browser cookies and `Origin` requests are refused. The user is the verified
token's subject, never a body field.

## Validation boundaries

1. Bound the raw body before parsing: the upload's `x-maxBodyBytes` (1 MiB) on the server,
   the page's (2 MiB) on the client, 4 MiB for a chat upload and a turn. Reject malformed UTF-8 or JSON, a byte-order mark and
   duplicate member names. Read an attachment download as bytes, never parsed: at most the
   attachment's `size` (4 MiB at most), and any other length is unavailable.
2. Implement `maxUtf8Bytes` as an assertion on the decoded string's UTF-8 length.
   `maxLength` counts code points, not bytes.
3. Apply the schemas without coercion, defaults or removing extra fields.

## What is not in it

No terminal output, full paths (beyond what the chat's own text holds), account ids or
credentials. Files attached to replies travel only through the attachment download, never in a
JSON body, and only from rakumux.com to the reply's machine. Schemas reject such fields, not
secrets inside the text: the status service sends replies and chats only while the account's
Feed switch is on, never a private session's, and masks keys, credential assignments and tokens
in every text first (best effort).

## Conformance tests

From `contracts/feed/`, with Node.js 22 or newer:

```sh
npm ci --cache .npm-cache --ignore-scripts --no-audit --no-fund
npm test
```

AJV **8.20.0** is the sole development dependency, as in `contracts/session-status/`.
Tests use local schemas and the synthetic fixtures in [`fixtures/`](fixtures/) only.
