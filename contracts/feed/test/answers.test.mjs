import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import { validators } from "../test-support/validation.mjs";

const fixtures = JSON.parse(readFileSync(new URL("../fixtures/answers.json", import.meta.url), "utf8"));
const valid = (reply) => validators.reply({ version: 1, reply });

test("an answer to an agent's question is a session message in its chat", () => {
  const reply = structuredClone(fixtures.reply);
  assert.equal(valid(reply), true);
  assert.equal(validators.replies({ version: 1, replies: [reply], latest_seq: reply.seq, more: false }), true);
  const answered = { ...structuredClone(reply), status: "failed", failure: "question_gone", status_at: reply.status_at + 1 };
  assert.equal(valid(answered), true, "its question gone");
  const { conversation_id: _chat, ...chatless } = structuredClone(reply);
  assert.equal(valid(chatless), false, "an answer names the chat it answers in");
  assert.equal(valid({ ...structuredClone(reply), entry: "01K8A0RZ0V3X5Q2N9T7W4Y6M8B", thread: "01K8A0RZ0V3X5Q2N9T7W4Y6M8B" }), false, "never to an entry");
  const files = [{ id: "01K8A2F1L3E5S7A9T1T2A3C4H5", name: "a.png", media_type: "image/png", size: 10 }];
  assert.equal(valid({ ...structuredClone(reply), attachments: files }), false, "no files");
  assert.equal(valid({ ...structuredClone(reply), response: "01K8A1Z9Y8X7W6V5T4S3R2Q1P0" }), false, "no reply answers it");
});

test("answers off the contract fail", () => {
  for (const { note, answer } of fixtures.invalidAnswers) {
    assert.equal(valid({ ...structuredClone(fixtures.reply), answer }), false, note);
  }
});

test("an answer gives at most 8 questions' answers, each at most 16 choices", () => {
  const with_ = (answers) => valid({ ...structuredClone(fixtures.reply), answer: { call_id: "toolu_1", answers } });
  const one = (n) => ({ question: `Question ${n}?`, choices: ["A"] });
  assert.equal(with_(Array.from({ length: 8 }, (_, n) => one(n))), true);
  assert.equal(with_(Array.from({ length: 9 }, (_, n) => one(n))), false);
  const labels = (count) => Array.from({ length: count }, (_, n) => `Option ${n}`);
  assert.equal(with_([{ question: "Q?", choices: labels(16) }]), true);
  assert.equal(with_([{ question: "Q?", choices: labels(17) }]), false);
  assert.equal(with_([{ question: "Q?", text: "a".repeat(2000) }]), true);
  assert.equal(with_([{ question: "Q?", text: "a".repeat(2001) }]), false);
  assert.equal(with_([{ question: "Q?\nPick one.", choices: ["A"] }]), true, "a question on two lines");
  assert.equal(with_([{ question: "é".repeat(2048), choices: ["A"] }]), true, "4096 bytes");
  assert.equal(with_([{ question: "é".repeat(2049), choices: ["A"] }]), false, "4098 bytes");
});

test("its machine reports a question gone", () => {
  assert.equal(validators.replyStatus(fixtures.replyStatus), true);
  assert.equal(validators.replyStatus({ ...fixtures.replyStatus, failure: "question_closed" }), false);
});
