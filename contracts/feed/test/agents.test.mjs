import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import { schemas, validators } from "../test-support/validation.mjs";

const read = (name) => JSON.parse(readFileSync(new URL(`../fixtures/${name}.json`, import.meta.url), "utf8"));
const agents = read("agents");
const reply = read("messages").upload.valid[0].value;
const chat = read("chats").chatUpload.valid[0].value;
const list = read("sessions").upload.valid[0];
const asked = read("new-sessions").newSession.valid[0];

/** Each place an agent is named, with `agent` set to `name`. */
const named = (name) => ({
  "a reply": [validators.upload, { ...structuredClone(reply), entries: [{ ...reply.entries[0], agent: name }] }],
  "a chat": [validators.chatUpload, { ...structuredClone(chat), chat: { ...chat.chat, agent: name } }],
  "a session list": [validators.sessionsUpload, { ...structuredClone(list), sessions: [{ ...list.sessions[0], agent: name }] }],
  "a new session": [validators.newSession, { version: 1, new_session: { ...asked, agent: name } }],
});

test("the agent list is the fixture's", () => {
  assert.deepEqual(schemas.common.$defs.agent.enum, agents.known);
});

test("every agent Rakumux names is taken wherever an agent is named", () => {
  for (const name of agents.known) {
    for (const [where, [validate, value]] of Object.entries(named(name))) {
      assert.equal(validate(value), true, `${name} in ${where}`);
    }
  }
});

test("other agents are refused, so a machine leaves out what they said", () => {
  for (const name of agents.unknown) {
    for (const [where, [validate, value]] of Object.entries(named(name))) {
      assert.equal(validate(value), false, `${JSON.stringify(name)} in ${where}`);
    }
  }
});
