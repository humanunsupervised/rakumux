# Agent feed wire contract, version 1

Canonical JSON Schema draft 2020-12 for the feed routes of rakumux.com: what agents said when
they finished a turn, uploaded by each machine's Rakumux status service and read back by every
machine of the same account. This directory contains contracts and development-only
conformance tests, not an HTTP server, database or local runtime.

## Routes and messages

| Route | Request | Success |
|---|---|---|
| `POST /api/v1/feed/entries` | [upload](upload.schema.json) | [accepted](accepted.schema.json) |
| `GET /api/v1/feed?after=SEQ&limit=N` | query only | [page](page.schema.json), increasing seq |
| `GET /api/v1/feed?before=SEQ&limit=N`, `GET /api/v1/feed?limit=N` | query only | [page](page.schema.json), decreasing seq |

- **Upload:** `{version:1, machine, entries}` with 1–50 entries. Each entry is
  `{id, session:{id, name, folder?}, agent, conversation_id?, text, truncated, finished_at}`.
  `id` is the ULID the local daemon gave the reply; the cloud keeps the first upload of an
  id and answers a repeat with the same `seq`, so retries never duplicate. `folder` is a
  folder's last path component, never a path. `text` is at most **32768 UTF-8 bytes**;
  `truncated` says the reply was longer. `finished_at` is the machine's clock (Unix ms);
  the cloud may clamp it to its own receipt time.
- **Accepted:** every uploaded id with its `seq`, in upload order.
- **Page:** `{version:1, entries, latest_seq, more}`. `seq` is the account's arrival order, the
  only cursor: `after` syncs what arrived since, `before` pages back. `limit` is 1–200
  (default 50); the cloud may return fewer to keep the response small, with `more` true.
  `after` and `before` together are invalid. Each entry adds `seq`, `machine` and
  `received_at` to what was uploaded.
- **[Error](error.schema.json):** `{error:{code, message, retry_after_seconds?}}`, one static
  message per code: `invalid_request` (400, or 413 for an oversized body), `unauthenticated`
  (401), `forbidden` (403: the account's Feed switch is off), `rate_limited` (429) and
  `unavailable` (503). Retry delays are 1–86400 seconds, only for the last two.

Every object rejects additional properties. Authorization is the terminal's Clerk OAuth
access token in the `Authorization` header, never the body; browser cookies and `Origin`
requests are refused. The user is the verified token's subject, never a body field.

## Validation boundaries

1. Bound the raw body before parsing: the upload's `x-maxBodyBytes` (1 MiB) on the server,
   the page's (2 MiB) on the client. Reject malformed UTF-8 or JSON, a byte-order mark and
   duplicate member names.
2. Implement `maxUtf8Bytes` as an assertion on the decoded string's UTF-8 length.
   `maxLength` counts code points, not bytes.
3. Apply the schemas without coercion, defaults or removing extra fields.

## What is not in it

No prompts, tool calls, tool output, terminal output, transcripts, full paths, account ids
or credentials. Schemas reject such fields, not secrets inside reply text: the status
service sends replies only while the account's Feed switch is on, only from consented,
non-private sessions, and applies its best-effort redaction first.

## Conformance tests

From `contracts/feed/`, with Node.js 22 or newer:

```sh
npm ci --cache .npm-cache --ignore-scripts --no-audit --no-fund
npm test
```

AJV **8.20.0** is the sole development dependency, as in `contracts/session-status/`.
Tests use local schemas and the synthetic fixtures in [`fixtures/`](fixtures/) only.
