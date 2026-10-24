import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import { schemas, validators } from "../test-support/validation.mjs";

const fixtures = JSON.parse(readFileSync(new URL("../fixtures/replies.json", import.meta.url), "utf8"));
const feed = JSON.parse(readFileSync(new URL("../fixtures/messages.json", import.meta.url), "utf8"));

test("reply uploads validate", () => {
  const base = fixtures.replyUpload.valid[0];
  assert.equal(validators.replyUpload(base), true);
  for (const { note, patch } of fixtures.replyUpload.invalid) {
    const upload = { ...structuredClone(base), ...patch };
    for (const key of Object.keys(upload)) if (upload[key] === null) delete upload[key];
    assert.equal(validators.replyUpload(upload), false, note);
  }
});

test("reply text is bounded at 8192 UTF-8 bytes", () => {
  const base = fixtures.replyUpload.valid[0];
  assert.equal(validators.replyUpload({ ...base, text: "a".repeat(8192) }), true);
  assert.equal(validators.replyUpload({ ...base, text: "a".repeat(8193) }), false);
  assert.equal(validators.replyUpload({ ...base, text: "😀".repeat(2048) }), true);
  assert.equal(validators.replyUpload({ ...base, text: "😀".repeat(2049) }), false);
});

for (const kind of ["replyStatus", "reply", "replies"]) {
  test(`${kind} messages validate`, () => {
    for (const value of fixtures[kind].valid) assert.equal(validators[kind](value), true, JSON.stringify(value));
    for (const value of fixtures[kind].invalid) assert.equal(validators[kind](value), false, JSON.stringify(value));
  });
}

test("an entry names the reply it answers and its thread; uploads only the reply", () => {
  const upload = structuredClone(feed.upload.valid[0].value);
  upload.entries[0].reply_to = "01K8A1B2C3D4E5F6G7H8J9K0MN";
  assert.equal(validators.upload(upload), true);
  upload.entries[0].thread = "01K8A0RZ0V3X5Q2N9T7W4Y6M8B";
  assert.equal(validators.upload(upload), false, "the cloud works out the thread");
  const page = structuredClone(feed.page.valid.find((value) => value.entries.length > 0));
  Object.assign(page.entries[0], { reply_to: "01K8A1B2C3D4E5F6G7H8J9K0MN", thread: "01K8A0RZ0V3X5Q2N9T7W4Y6M8B" });
  assert.equal(validators.page(page), true);
});

test("reply body caps are annotated for the transport", () => {
  assert.equal(schemas["reply-upload"]["x-maxBodyBytes"], 65536);
  assert.equal(schemas.replies["x-maxBodyBytes"], 2097152);
});
