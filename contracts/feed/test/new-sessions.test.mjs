import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import { schemas, validators } from "../test-support/validation.mjs";

const fixtures = JSON.parse(readFileSync(new URL("../fixtures/new-sessions.json", import.meta.url), "utf8"));

function check(kind, validate) {
  for (const value of fixtures[kind].valid) assert.equal(validate(value), true, JSON.stringify(value));
  for (const { note, value } of fixtures[kind].invalid) assert.equal(validate(value), false, note);
}

test("a new session names a folder and an agent, never a path or a command", () =>
  check("newSession", (value) => validators.newSession({ version: 1, new_session: value })));

test("a machine reports the session it started, or why it couldn't", () => check("status", validators.newSessionStatus));

test("the stored request comes back", () => check("answer", validators.newSession));

test("the replies read brings the new sessions pending for the reading machine", () => check("replies", validators.replies));

test("an older replies read without new sessions still validates", () => {
  assert.equal(validators.replies({ version: 1, replies: [], latest_seq: 0, more: false }), true);
});

test("a session list may say the machine takes new sessions", () => {
  const list = { version: 1, machine: "studio", sessions: [], at: 1761220000000, capabilities: ["reply_attachments", "new_sessions"] };
  assert.equal(validators.sessionsUpload(list), true);
});

test("new session body caps are annotated for the transport", () => {
  assert.equal(schemas["new-session-status"]["x-maxBodyBytes"], 4096);
});
