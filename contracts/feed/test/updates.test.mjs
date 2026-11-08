import assert from "node:assert/strict";
import { test } from "node:test";
import { schemas, validators } from "../test-support/validation.mjs";

const ID = "01K8A2B3C4D5E6F7G8H9J0K1M2";
const read = (extra) => ({ version: 1, replies: [], latest_seq: 0, more: false, ...extra });

test("the replies read may say the newest release, a version and nothing else", () => {
  assert.equal(validators.replies(read({ release: { version: "0.1.49" } })), true);
  assert.equal(validators.replies(read({ release: { version: "0.1.49-rc.1" } })), false, "a prerelease");
  assert.equal(validators.replies(read({ release: { version: "v0.1.49" } })), false, "the tag, not the version");
  assert.equal(validators.replies(read({ release: { version: "0.1.49", notes: "…" } })), false, "notes go to the web, not machines");
  assert.equal(validators.replies(read({})), true, "an older rakumux.com says none");
});

test("the replies read brings a machine at most one update, an id and a version", () => {
  assert.equal(validators.replies(read({ updates: [{ id: ID, version: "0.1.49" }] })), true);
  assert.equal(validators.replies(read({ updates: [] })), true);
  assert.equal(validators.replies(read({ updates: [{ id: ID, version: "0.1.49" }, { id: "01K8A2B3C4D5E6F7G8H9J0K1M3", version: "0.1.49" }] })), false);
  assert.equal(validators.replies(read({ updates: [{ id: ID, version: "0.1.49", command: "curl" }] })), false, "never a command");
  assert.equal(validators.replies(read({ updates: [{ id: "x", version: "0.1.49" }] })), false);
});

test("a session list may say the machine's version and that it updates when asked", () => {
  const list = { version: 1, machine: "studio", sessions: [], at: 1761220000000, capabilities: ["new_sessions", "remote_update"], rakumux_version: "0.1.49" };
  assert.equal(validators.sessionsUpload(list), true);
  assert.equal(validators.sessionsUpload({ ...list, rakumux_version: "0.1" }), false);
  assert.match(schemas["sessions-upload"].properties.capabilities.description, /`remote_update`/);
});

test("a machine reports installing, updated, or failed with why", () => {
  const status = (extra) => ({ version: 1, id: ID, machine: "studio", ...extra });
  assert.equal(validators.updateStatus(status({ status: "installing" })), true);
  assert.equal(validators.updateStatus(status({ status: "updated" })), true);
  for (const failure of ["unsupported", "install_failed", "restart_failed"]) {
    assert.equal(validators.updateStatus(status({ status: "failed", failure })), true, failure);
  }
  assert.equal(validators.updateStatus(status({ status: "failed" })), false, "a failure says why");
  assert.equal(validators.updateStatus(status({ status: "updated", failure: "install_failed" })), false);
  assert.equal(validators.updateStatus(status({ status: "pending" })), false, "the machine never says pending");
  assert.equal(schemas["update-status"]["x-maxBodyBytes"], 4096);
});

test("the stored update comes back", () => {
  const update = { id: ID, machine: "studio", version: "0.1.49", status: "pending", created_at: 1761220000000, status_at: 1761220000000 };
  for (const status of ["pending", "installing", "updated", "expired"]) {
    assert.equal(validators.machineUpdate({ version: 1, update: { ...update, status } }), true, status);
  }
  assert.equal(validators.machineUpdate({ version: 1, update: { ...update, status: "failed", failure: "restart_failed" } }), true);
  assert.equal(validators.machineUpdate({ version: 1, update: { ...update, status: "failed" } }), false);
  assert.equal(validators.machineUpdate({ version: 1, update: { ...update, failure: "unsupported" } }), false);
});
