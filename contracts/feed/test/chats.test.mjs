import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import { schemas, validators } from "../test-support/validation.mjs";

const fixtures = JSON.parse(readFileSync(new URL("../fixtures/chats.json", import.meta.url), "utf8"));
const feed = JSON.parse(readFileSync(new URL("../fixtures/messages.json", import.meta.url), "utf8"));
const base = fixtures.chatUpload.valid[0].value;

/** The first valid chat upload with one change: to the envelope, its chat, the chat's session or its first message. */
function variant({ patch, chat, session, message }) {
  const upload = structuredClone(base);
  Object.assign(upload, patch);
  Object.assign(upload.chat, chat);
  Object.assign(upload.chat.session, session);
  if (message) Object.assign(upload.messages[0], message);
  for (const key of Object.keys(upload.chat)) if (upload.chat[key] === null) delete upload.chat[key];
  return upload;
}

test("valid chat uploads pass", () => {
  for (const { note, value } of fixtures.chatUpload.valid) {
    assert.equal(validators.chatUpload(value), true, note);
  }
});

test("invalid chat uploads fail", () => {
  for (const fixture of fixtures.chatUpload.invalid) {
    assert.equal(validators.chatUpload(variant(fixture)), false, fixture.note);
  }
});

test("part text is bounded in UTF-8 bytes, not characters", () => {
  for (const { note, text, count, valid } of fixtures.chatUpload.bounds) {
    const parts = [{ type: "text", text: text.repeat(count) }];
    assert.equal(validators.chatUpload(variant({ message: { parts } })), valid, note);
    const output = [{ type: "tool_result", output: text.repeat(count) }];
    assert.equal(validators.chatUpload(variant({ message: { parts: output } })), valid, `${note} (output)`);
  }
});

test("at most 100 messages of 64 parts are uploaded at once", () => {
  const messages = (count) => Array.from({ length: count }, () => structuredClone(base.messages[0]));
  assert.equal(validators.chatUpload(variant({ patch: { messages: messages(100) } })), true);
  assert.equal(validators.chatUpload(variant({ patch: { messages: messages(101) } })), false);
  const parts = (count) => Array.from({ length: count }, () => ({ type: "text", text: "a" }));
  assert.equal(validators.chatUpload(variant({ message: { parts: parts(64) } })), true);
  assert.equal(validators.chatUpload(variant({ message: { parts: parts(65) } })), false);
});

for (const kind of ["chatAccepted", "turn"]) {
  test(`${kind} messages validate`, () => {
    for (const value of fixtures[kind].valid) assert.equal(validators[kind](value), true, JSON.stringify(value));
    for (const value of fixtures[kind].invalid) assert.equal(validators[kind](value), false, JSON.stringify(value));
  });
}

test("a reply names its turn's last position, which older machines leave out", () => {
  const entry = structuredClone(feed.upload.valid[0].value);
  assert.equal(validators.upload(entry), true);
  entry.entries[0].position = 2400;
  assert.equal(validators.upload(entry), true);
  entry.entries[0].position = -1;
  assert.equal(validators.upload(entry), false);
});

test("chat body caps are annotated for the transport", () => {
  assert.equal(schemas["chat-upload"]["x-maxBodyBytes"], 4194304);
  assert.equal(schemas.turn["x-maxBodyBytes"], 4194304);
});
