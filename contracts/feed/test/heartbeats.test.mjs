import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import { schemas, validators } from "../test-support/validation.mjs";

const fixtures = JSON.parse(readFileSync(new URL("../fixtures/heartbeats.json", import.meta.url), "utf8"));

function check(kind, validate) {
  for (const value of fixtures[kind].valid) assert.equal(validate(value), true, JSON.stringify(value).slice(0, 300));
  for (const { note, value } of fixtures[kind].invalid) assert.equal(validate(value), false, note);
}

const upload = (heartbeats) => ({ version: 1, machine: "studio", heartbeats, at: 1761220000000 });

test("a heartbeat names its folder, never a path, and carries its command and task masked", () =>
  check("heartbeat", (value) => validators.heartbeatsUpload(upload([value]))));

test("a machine uploads at most 50 heartbeats, or none", () => {
  const one = fixtures.heartbeat.valid[0];
  assert.equal(validators.heartbeatsUpload(upload([])), true);
  assert.equal(validators.heartbeatsUpload(upload(Array.from({ length: 50 }, () => one))), true);
  assert.equal(validators.heartbeatsUpload(upload(Array.from({ length: 51 }, () => one))), false);
  assert.equal(validators.heartbeatsStored({ version: 1, stored: 50 }), true);
  assert.equal(validators.heartbeatsStored({ version: 1, stored: 51 }), false);
});

test("an action names a heartbeat by its id; enable carries the version approved", () =>
  check("action", (value) => validators.heartbeatAction({ version: 1, heartbeat_action: value })));

test("a machine reports an action done, or why it couldn't", () => check("status", validators.heartbeatActionStatus));

test("a replies read carries at most 20 heartbeat actions", () => {
  const pending = fixtures.action.valid[0];
  const read = (count) => ({ version: 1, replies: [], latest_seq: 0, more: false, heartbeat_actions: Array.from({ length: count }, () => pending) });
  assert.equal(validators.replies(read(20)), true);
  assert.equal(validators.replies(read(21)), false);
});

test("a session list marks a heartbeat's runs and says the machine runs heartbeats", () => {
  const session = { id: "01K89ZQ4D2F6H8J0K2M4N6P8R0", name: "nightly-deps #3", status: "live", lifecycle: "working", pinned: false, last_active_at: 1761220000000, heartbeat: { heartbeat: "01K8A2B3C4D5E6F7G8H9J0K1M2", run: 3 } };
  const list = { version: 1, machine: "studio", sessions: [session], at: 1761220000000, capabilities: ["heartbeats"] };
  assert.equal(validators.sessionsUpload(list), true);
  assert.equal(validators.sessionsUpload({ ...list, sessions: [{ ...session, heartbeat: { heartbeat: "01K8A2B3C4D5E6F7G8H9J0K1M2", run: 0 } }] }), false);
  assert.equal(validators.sessionsStored({ version: 1, stored: 1, accepts: ["heartbeats"] }), true);
});

test("heartbeat body caps are annotated for the transport", () => {
  assert.equal(schemas["heartbeats-upload"]["x-maxBodyBytes"], 1048576);
  assert.equal(schemas["heartbeat-action-status"]["x-maxBodyBytes"], 4096);
});
