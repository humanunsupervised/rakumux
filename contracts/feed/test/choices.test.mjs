import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import { validators } from "../test-support/validation.mjs";

const fixtures = JSON.parse(readFileSync(new URL("../fixtures/choices.json", import.meta.url), "utf8"));
const list = (session) => validators.sessionsUpload({ version: 1, machine: "studio", at: 1761220000000, sessions: [session] });
const valid = (reply) => validators.reply({ version: 1, reply });

test("a session list says the prompt with numbered options a session's agent shows", () => {
  const session = structuredClone(fixtures.session);
  assert.equal(list(session), true);
  const { title: _title, detail: _detail, ...bare } = session.choice;
  assert.equal(list({ ...session, choice: bare }), true, "a title and detail are optional");
  assert.equal(list({ ...session, choice: { ...bare, detail: "1  one\n2 -two\n2 +2" } }), true, "a detail on lines");
  const off = [
    ["a key off the contract", { ...bare, key: "9F3C1A2B4D5E6F70" }],
    ["a short key", { ...bare, key: "9f3c" }],
    ["no options", { ...bare, options: [] }],
    ["an option numbered 0", { ...bare, options: [{ number: 0, label: "Yes" }] }],
    ["an option numbered 10", { ...bare, options: [{ number: 10, label: "Yes" }] }],
    ["ten options", { ...bare, options: Array.from({ length: 10 }, (_, n) => ({ number: (n % 9) + 1, label: "Yes" })) }],
    ["a label on two lines", { ...bare, options: [{ number: 1, label: "Yes\nplease" }] }],
    ["a detail too long", { ...bare, detail: "a".repeat(2001) }],
    ["a terminal control in the detail", { ...bare, detail: "a\u001b[2Jb" }],
    ["no question", { key: bare.key, options: bare.options }],
  ];
  for (const [note, choice] of off) {
    assert.equal(list({ ...session, choice }), false, note);
  }
});

test("a pick is a session message naming the prompt and its option", () => {
  const reply = structuredClone(fixtures.reply);
  assert.equal(valid(reply), true);
  assert.equal(validators.replies({ version: 1, replies: [reply], latest_seq: reply.seq, more: false }), true);
  assert.equal(valid({ ...structuredClone(reply), status: "failed", failure: "question_gone" }), true, "its prompt gone");
  const { conversation_id: _chat, ...chatless } = structuredClone(reply);
  assert.equal(valid(chatless), true, "a pick needs no chat");
  assert.equal(valid({ ...structuredClone(reply), entry: "01K8A0RZ0V3X5Q2N9T7W4Y6M8B", thread: "01K8A0RZ0V3X5Q2N9T7W4Y6M8B" }), false, "never to an entry");
  assert.equal(valid({ ...structuredClone(reply), response: "01K8A1Z9Y8X7W6V5T4S3R2Q1P0" }), false, "no reply answers it");
  const answer = { call_id: "toolu_1", answers: [{ question: "Q?", choices: ["A"] }] };
  assert.equal(valid({ ...structuredClone(reply), answer }), false, "never an answer too");
  for (const choice of [{ key: reply.choice.key }, { option: 1 }, { key: reply.choice.key, option: 0 }, { ...reply.choice, label: "Yes" }]) {
    assert.equal(valid({ ...structuredClone(reply), choice }), false, JSON.stringify(choice));
  }
});

test("a cloud says it takes choices", () => {
  assert.equal(validators.sessionsStored({ version: 1, stored: 1, accepts: ["places", "output", "choices"] }), true);
});
