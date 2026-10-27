import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import { schemas, validators } from "../test-support/validation.mjs";

const fixtures = JSON.parse(readFileSync(new URL("../fixtures/sessions.json", import.meta.url), "utf8"));

test("session lists validate", () => {
  for (const value of fixtures.upload.valid) assert.equal(validators.sessionsUpload(value), true, JSON.stringify(value));
  const base = fixtures.upload.valid[0];
  for (const { note, patch, session } of fixtures.upload.invalid) {
    const upload = { ...structuredClone(base), ...patch };
    if (session) Object.assign(upload.sessions[0], session);
    for (const target of [upload, upload.sessions[0]]) {
      for (const key of Object.keys(target)) if (target[key] === null) delete target[key];
    }
    assert.equal(validators.sessionsUpload(upload), false, note);
  }
});

test("a machine lists at most 200 sessions", () => {
  const base = fixtures.upload.valid[0];
  const one = base.sessions[1];
  const many = (count) => ({
    ...base,
    sessions: Array.from({ length: count }, (_, index) => ({ ...one, id: `01K89ZQ4D2F6H8J0K2M4N${String(index).padStart(5, "0")}` })),
  });
  assert.equal(validators.sessionsUpload(many(200)), true);
  assert.equal(validators.sessionsUpload(many(201)), false);
});

test("a session lists at most 8 places", () => {
  const base = fixtures.upload.valid[0];
  const place = base.sessions[0].places[3];
  const withPlaces = (count) => {
    const upload = structuredClone(base);
    upload.sessions[0].places = Array.from({ length: count }, (_, index) => ({ ...place, name: `p${index}` }));
    return upload;
  };
  assert.equal(validators.sessionsUpload(withPlaces(8)), true);
  assert.equal(validators.sessionsUpload(withPlaces(9)), false);
});

test("a machine says what it handles beyond the first version", () => {
  const base = fixtures.upload.valid[0];
  for (const capabilities of fixtures.capabilities.valid) {
    assert.equal(validators.sessionsUpload({ ...base, capabilities }), true, JSON.stringify(capabilities));
  }
  const many = (count) => Array.from({ length: count }, (_, index) => `capability_${index}`);
  assert.equal(validators.sessionsUpload({ ...base, capabilities: many(16) }), true);
  assert.equal(validators.sessionsUpload({ ...base, capabilities: many(17) }), false);
});

test("stored answers validate", () => {
  for (const value of fixtures.stored.valid) assert.equal(validators.sessionsStored(value), true, JSON.stringify(value));
  for (const value of fixtures.stored.invalid) assert.equal(validators.sessionsStored(value), false, JSON.stringify(value));
});

test("session list body caps are annotated for the transport", () => {
  assert.equal(schemas["sessions-upload"]["x-maxBodyBytes"], 262144);
});
