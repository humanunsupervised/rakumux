import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import { schemas, validators } from "../test-support/validation.mjs";

const fixtures = JSON.parse(readFileSync(new URL("../fixtures/workspace-changes.json", import.meta.url), "utf8"));

function check(kind, validate) {
  for (const value of fixtures[kind].valid) assert.equal(validate(value), true, JSON.stringify(value));
  for (const { note, value } of fixtures[kind].invalid) assert.equal(validate(value), false, note);
}

test("a change renames a workspace to one line of name, or deletes it", () =>
  check("change", (value) => validators.workspaceChange({ version: 1, workspace_change: value })));

test("a machine reports the change done, or why it couldn't", () => check("status", validators.workspaceChangeStatus));

test("a replies read carries at most 20 workspace changes", () => {
  const pending = fixtures.change.valid[0];
  const read = (count) => ({ version: 1, replies: [], latest_seq: 0, more: false, workspace_changes: Array.from({ length: count }, () => pending) });
  assert.equal(validators.replies(read(20)), true);
  assert.equal(validators.replies(read(21)), false);
});

test("a session list may say the machine edits workspaces", () => {
  const list = { version: 1, machine: "studio", sessions: [], at: 1761220000000, capabilities: ["workspaces", "workspaces.edit"] };
  assert.equal(validators.sessionsUpload(list), true);
});

test("workspace change body caps are annotated for the transport", () => {
  assert.equal(schemas["workspace-change-status"]["x-maxBodyBytes"], 4096);
});
