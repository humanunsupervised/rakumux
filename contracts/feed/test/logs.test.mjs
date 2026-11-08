import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import { validators } from "../test-support/validation.mjs";

const fixtures = JSON.parse(readFileSync(new URL("../fixtures/logs.json", import.meta.url), "utf8"));

/** The first valid upload with one change to the envelope or its first event; a null member is removed. */
function variant({ patch, event }) {
  const upload = structuredClone(fixtures.upload.valid[0]);
  Object.assign(upload, patch);
  if (event) Object.assign(upload.events[0], event);
  for (const key of Object.keys(upload)) if (upload[key] === null) delete upload[key];
  return upload;
}

test("log uploads validate", () => {
  for (const value of fixtures.upload.valid) assert.equal(validators.logsUpload(value), true, JSON.stringify(value));
  for (const fixture of fixtures.upload.invalid) assert.equal(validators.logsUpload(variant(fixture)), false, fixture.note);
});

test("a batch holds at most 100 events", () => {
  const upload = structuredClone(fixtures.upload.valid[0]);
  upload.events = Array.from({ length: 101 }, () => upload.events[0]);
  assert.equal(validators.logsUpload(upload), false);
});

test("log answers validate", () => {
  for (const value of fixtures.stored.valid) assert.equal(validators.logsStored(value), true);
  for (const value of fixtures.stored.invalid) assert.equal(validators.logsStored(value), false);
});
