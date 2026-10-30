import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import { schemas, validators } from "../test-support/validation.mjs";

const fixtures = JSON.parse(readFileSync(new URL("../fixtures/resources.json", import.meta.url), "utf8"));
const base = fixtures.upload.valid[0];

/**
 * The first valid upload with one change: to the envelope (`patch`), the host, the first session,
 * the first session's usage or the first role; a null member is removed.
 */
function variant({ patch, host, session, usage, role }) {
  const upload = structuredClone(base);
  Object.assign(upload, patch);
  if (host) Object.assign(upload.host, host);
  if (session) Object.assign(upload.sessions[0], session);
  if (usage) Object.assign(upload.sessions[0].usage, usage);
  if (role) Object.assign(upload.rakumux[0], role);
  for (const target of [upload, upload.host, upload.sessions[0], upload.sessions[0].usage, upload.rakumux[0]]) {
    for (const key of Object.keys(target ?? {})) if (target[key] === null) delete target[key];
  }
  return upload;
}

test("resource uploads validate", () => {
  for (const value of fixtures.upload.valid) assert.equal(validators.resourcesUpload(value), true, JSON.stringify(value));
  for (const fixture of fixtures.upload.invalid) {
    assert.equal(validators.resourcesUpload(variant(fixture)), false, fixture.note);
  }
});

test("a machine reports at most 200 sessions and 8 roles", () => {
  const session = base.sessions[0];
  const sessions = (count) => ({
    ...base,
    sessions: Array.from({ length: count }, (_, index) => ({ ...session, id: `01K89ZQ4D2F6H8J0K2M4N${String(index).padStart(5, "0")}` })),
  });
  assert.equal(validators.resourcesUpload(sessions(200)), true);
  assert.equal(validators.resourcesUpload(sessions(201)), false);
  const roles = (count) => ({ ...base, rakumux: Array.from({ length: count }, () => base.rakumux[0]) });
  assert.equal(validators.resourcesUpload(roles(8)), true);
  assert.equal(validators.resourcesUpload(roles(9)), false);
});

test("200 sessions fit the body cap", () => {
  const session = {
    id: "01K89ZQ4D2F6H8J0K2M4N6P8R2",
    processes: 500,
    usage: { cpu_percent: 409600, memory_bytes: 9007199254740991, net_rx_bytes_per_s: 9007199254740991, net_tx_bytes_per_s: 9007199254740991 },
  };
  const upload = { ...base, sessions: Array.from({ length: 200 }, () => session) };
  assert.ok(Buffer.byteLength(JSON.stringify(upload)) < schemas["resources-upload"]["x-maxBodyBytes"]);
});

test("stored answers validate", () => {
  for (const value of fixtures.stored.valid) assert.equal(validators.resourcesStored(value), true, JSON.stringify(value));
  for (const value of fixtures.stored.invalid) assert.equal(validators.resourcesStored(value), false, JSON.stringify(value));
});

test("a replies read may ask for resources", () => {
  for (const value of fixtures.replies.valid) assert.equal(validators.replies(value), true, JSON.stringify(value));
  for (const value of fixtures.replies.invalid) assert.equal(validators.replies(value), false, JSON.stringify(value));
});

test("resource body caps are annotated for the transport", () => {
  assert.equal(schemas["resources-upload"]["x-maxBodyBytes"], 65536);
  assert.equal(schemas["resources-stored"]["x-maxBodyBytes"], 1024);
});
