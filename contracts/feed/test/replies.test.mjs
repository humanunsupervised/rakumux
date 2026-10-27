import assert from "node:assert/strict";
import { Buffer } from "node:buffer";
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

test("a session message has no entry or thread, and a reply to an entry has both", () => {
  const reply = structuredClone(fixtures.reply.valid[0].reply);
  const message = structuredClone(reply);
  delete message.entry;
  delete message.thread;
  delete message.response;
  assert.equal(validators.reply({ version: 1, reply: message }), true, "a session message");
  delete message.conversation_id;
  assert.equal(validators.reply({ version: 1, reply: message }), true, "the session said no chat");
  assert.equal(validators.replies({ version: 1, replies: [message], latest_seq: message.seq, more: false }), true);
  const halfThread = structuredClone(reply);
  delete halfThread.thread;
  assert.equal(validators.reply({ version: 1, reply: halfThread }), false, "an entry without its thread");
  const halfEntry = structuredClone(reply);
  delete halfEntry.entry;
  assert.equal(validators.reply({ version: 1, reply: halfEntry }), false, "a thread without its entry");
  const noChat = structuredClone(reply);
  delete noChat.conversation_id;
  assert.equal(validators.reply({ version: 1, reply: noChat }), false, "a reply to an entry names its chat");
});

/** The fixtures' pending reply with files, with other attachments. */
function withAttachments(attachments) {
  const reply = structuredClone(fixtures.reply.valid.find((value) => value.reply.attachments).reply);
  return validators.reply({ version: 1, reply: { ...reply, attachments } });
}

test("attachments validate", () => {
  for (const value of fixtures.attachments.valid) assert.equal(withAttachments([value]), true, value.name);
  for (const { note, patch } of fixtures.attachments.invalid) {
    const value = { ...structuredClone(fixtures.attachments.valid[0]), ...patch };
    for (const key of Object.keys(value)) if (value[key] === null) delete value[key];
    assert.equal(withAttachments([value]), false, note);
  }
});

test("a reply has one to five attachments, or none", () => {
  const files = fixtures.attachments.valid;
  assert.equal(files.length >= 6, true);
  assert.equal(withAttachments(files.slice(0, 5)), true);
  assert.equal(withAttachments(files.slice(0, 6)), false);
  assert.equal(withAttachments([]), false, "an empty list is left out instead");
});

test("file names are bounded in characters and UTF-8 bytes, media types in characters", () => {
  const named = (name) => withAttachments([{ ...fixtures.attachments.valid[0], name }]);
  assert.equal(named("a".repeat(128)), true);
  assert.equal(named("a".repeat(129)), false);
  assert.equal(named("a" + "é".repeat(127)), true, "255 bytes");
  assert.equal(named("é".repeat(128)), false, "128 characters but 256 bytes");
  assert.equal(named("😀".repeat(63)), true, "252 bytes");
  assert.equal(named("😀".repeat(64)), false, "256 bytes");
  const typed = (media_type) => withAttachments([{ ...fixtures.attachments.valid[0], media_type }]);
  assert.equal(typed("a/" + "b".repeat(126)), true);
  assert.equal(typed("a/" + "b".repeat(127)), false);
});

test("a reply's attachments add at most a few KiB to a replies page", () => {
  // Every name and media type at its longest, and as long as possible in JSON (quotes escape).
  const longest = fixtures.attachments.valid.slice(0, 5).map((value) => ({
    ...value,
    name: '"'.repeat(86) + "😀".repeat(42),
    media_type: "application/" + "x".repeat(116),
    size: 4194304,
  }));
  assert.equal(withAttachments(longest), true);
  assert.equal(Buffer.byteLength(JSON.stringify(longest), "utf8") <= 4096, true);
  // So a reply stays far within the page's 2 MiB, which the cloud fills reply by reply.
  const reply = structuredClone(fixtures.reply.valid.find((value) => value.reply.attachments).reply);
  Object.assign(reply, { text: "a" + '"'.repeat(8191), attachments: longest });
  assert.equal(validators.reply({ version: 1, reply }), true);
  assert.equal(Buffer.byteLength(JSON.stringify(reply), "utf8") < 32768, true);
});

test("reply body caps are annotated for the transport", () => {
  assert.equal(schemas["reply-upload"]["x-maxBodyBytes"], 65536);
  assert.equal(schemas.replies["x-maxBodyBytes"], 2097152);
});
