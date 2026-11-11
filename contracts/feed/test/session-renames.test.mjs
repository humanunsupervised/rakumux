import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import { schemas, validators } from "../test-support/validation.mjs";

const fixtures = JSON.parse(readFileSync(new URL("../fixtures/session-renames.json", import.meta.url), "utf8"));

function check(kind, validate) {
  for (const value of fixtures[kind].valid) assert.equal(validate(value), true, JSON.stringify(value));
  for (const { note, value } of fixtures[kind].invalid) assert.equal(validate(value), false, note);
}

test("a rename names a session by its id and carries one line of name, never a path or a command", () =>
  check("rename", (value) => validators.sessionRename({ version: 1, session_rename: value })));

test("a machine reports the session renamed, or why it couldn't", () => check("status", validators.sessionRenameStatus));

test("a replies read carries at most 20 renames", () => {
  const pending = fixtures.rename.valid[0];
  const read = (count) => ({ version: 1, replies: [], latest_seq: 0, more: false, session_renames: Array.from({ length: count }, () => pending) });
  assert.equal(validators.replies(read(20)), true);
  assert.equal(validators.replies(read(21)), false);
});

test("a session list may say the machine renames sessions", () => {
  const list = { version: 1, machine: "studio", sessions: [], at: 1761220000000, capabilities: ["rename_sessions", "delete_sessions"] };
  assert.equal(validators.sessionsUpload(list), true);
});

test("rename body caps are annotated for the transport", () => {
  assert.equal(schemas["session-rename-status"]["x-maxBodyBytes"], 4096);
});
