import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import { schemas, validators } from "../test-support/validation.mjs";

const fixtures = JSON.parse(readFileSync(new URL("../fixtures/session-deletions.json", import.meta.url), "utf8"));

function check(kind, validate) {
  for (const value of fixtures[kind].valid) assert.equal(validate(value), true, JSON.stringify(value));
  for (const { note, value } of fixtures[kind].invalid) assert.equal(validate(value), false, note);
}

test("a deletion names a session by its id, never a path or a command", () =>
  check("deletion", (value) => validators.sessionDeletion({ version: 1, session_deletion: value })));

test("a machine reports the session deleted, or why it couldn't", () => check("status", validators.sessionDeletionStatus));

test("a replies read carries at most 20 deletions", () => {
  const pending = fixtures.deletion.valid[0];
  const read = (count) => ({ version: 1, replies: [], latest_seq: 0, more: false, session_deletions: Array.from({ length: count }, () => pending) });
  assert.equal(validators.replies(read(20)), true);
  assert.equal(validators.replies(read(21)), false);
});

test("a session list may say the machine deletes sessions", () => {
  const list = { version: 1, machine: "studio", sessions: [], at: 1761220000000, capabilities: ["delete_sessions"] };
  assert.equal(validators.sessionsUpload(list), true);
});

test("deletion body caps are annotated for the transport", () => {
  assert.equal(schemas["session-deletion-status"]["x-maxBodyBytes"], 4096);
});
