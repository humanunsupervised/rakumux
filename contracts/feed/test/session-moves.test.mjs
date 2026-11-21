import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import { schemas, validators } from "../test-support/validation.mjs";

const fixtures = JSON.parse(readFileSync(new URL("../fixtures/session-moves.json", import.meta.url), "utf8"));

function check(kind, validate) {
  for (const value of fixtures[kind].valid) assert.equal(validate(value), true, JSON.stringify(value));
  for (const { note, value } of fixtures[kind].invalid) assert.equal(validate(value), false, note);
}

test("a move names a session by its id and a workspace by one line of name, or none", () =>
  check("move", (value) => validators.sessionMove({ version: 1, session_move: value })));

test("a machine reports the session moved, or why it couldn't", () => check("status", validators.sessionMoveStatus));

test("a replies read carries at most 20 moves", () => {
  const pending = fixtures.move.valid[0];
  const read = (count) => ({ version: 1, replies: [], latest_seq: 0, more: false, session_moves: Array.from({ length: count }, () => pending) });
  assert.equal(validators.replies(read(20)), true);
  assert.equal(validators.replies(read(21)), false);
});

test("a session list names its workspaces and each session's", () => {
  const session = { id: "01K89ZQ4D2F6H8J0K2M4N6P8R0", name: "fix login", status: "live", lifecycle: "idle", pinned: false, last_active_at: 1761220000000 };
  const list = (workspaces, inSession) => ({
    version: 1, machine: "studio", at: 1761220000000, capabilities: ["workspaces"], workspaces,
    sessions: [{ ...session, ...(inSession === undefined ? {} : { workspace: inSession }) }],
  });
  assert.equal(validators.sessionsUpload(list(["Launch", "Research"], "Launch")), true);
  assert.equal(validators.sessionsUpload(list([], undefined)), true);
  assert.equal(validators.sessionsUpload(list(["Launch", "Launch"], "Launch")), false, "a name twice");
  assert.equal(validators.sessionsUpload(list(["Launch"], " Launch")), false, "untrimmed");
  assert.equal(validators.sessionsUpload(list(Array.from({ length: 101 }, (_, i) => `w${i}`), undefined)), false, "over 100");
  assert.equal(validators.sessionsStored({ version: 1, stored: 1, accepts: ["workspaces"] }), true);
});

test("move body caps are annotated for the transport", () => {
  assert.equal(schemas["session-move-status"]["x-maxBodyBytes"], 4096);
});
