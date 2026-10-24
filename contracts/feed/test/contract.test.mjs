import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import { schemas, validators } from "../test-support/validation.mjs";

const fixtures = JSON.parse(readFileSync(new URL("../fixtures/messages.json", import.meta.url), "utf8"));
const base = fixtures.upload.valid[0].value;

/** The first valid upload with one change: to the envelope, its entry or the entry's session. */
function variant({ value, patch, entry, session }) {
  if (value) return value;
  const upload = structuredClone(base);
  Object.assign(upload, patch);
  Object.assign(upload.entries[0], entry);
  Object.assign(upload.entries[0].session, session);
  return upload;
}

test("valid uploads pass", () => {
  for (const { note, value } of fixtures.upload.valid) {
    assert.equal(validators.upload(value), true, note);
  }
});

test("invalid uploads fail", () => {
  for (const fixture of fixtures.upload.invalid) {
    assert.equal(validators.upload(variant(fixture)), false, fixture.note);
  }
});

test("reply text is bounded in UTF-8 bytes, not characters", () => {
  for (const { note, text, count, valid } of fixtures.upload.bounds) {
    assert.equal(validators.upload(variant({ entry: { text: text.repeat(count) } })), valid, note);
  }
});

test("at most 50 entries are uploaded at once", () => {
  const entries = (count) => ({ patch: { entries: Array.from({ length: count }, () => structuredClone(base.entries[0])) } });
  assert.equal(validators.upload(variant(entries(50))), true);
  assert.equal(validators.upload(variant(entries(51))), false);
});

for (const kind of ["accepted", "page", "error"]) {
  test(`${kind} messages validate`, () => {
    for (const value of fixtures[kind].valid) assert.equal(validators[kind](value), true, JSON.stringify(value));
    for (const value of fixtures[kind].invalid) assert.equal(validators[kind](value), false, JSON.stringify(value));
  });
}

test("body caps are annotated for the transport", () => {
  assert.equal(schemas.upload["x-maxBodyBytes"], 1048576);
  assert.equal(schemas.page["x-maxBodyBytes"], 2097152);
});
