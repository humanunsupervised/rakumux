import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import { schemas, validators } from "../test-support/validation.mjs";

const fixtures = JSON.parse(readFileSync(new URL("../fixtures/new-sessions.json", import.meta.url), "utf8"));

function check(kind, validate) {
  for (const value of fixtures[kind].valid) assert.equal(validate(value), true, JSON.stringify(value));
  for (const { note, value } of fixtures[kind].invalid) assert.equal(validate(value), false, note);
}

test("a new session names a folder and an agent, never a path or a command", () =>
  check("newSession", (value) => validators.newSession({ version: 1, new_session: value })));

test("a machine reports the session it started, or why it couldn't", () => check("status", validators.newSessionStatus));

test("the stored request comes back", () => check("answer", validators.newSession));

test("the replies read brings the new sessions pending for the reading machine", () => check("replies", validators.replies));

test("a replies read carries at most 20 new sessions", () => {
  const pending = fixtures.newSession.valid[1];
  const read = (count) => ({ version: 1, replies: [], latest_seq: 0, more: false, new_sessions: Array.from({ length: count }, () => pending) });
  assert.equal(validators.replies(read(20)), true);
  assert.equal(validators.replies(read(21)), false);
});

test("an older replies read without new sessions still validates", () => {
  assert.equal(validators.replies({ version: 1, replies: [], latest_seq: 0, more: false }), true);
});

test("a session list may say the machine takes new sessions", () => {
  const list = { version: 1, machine: "studio", sessions: [], at: 1761220000000, capabilities: ["reply_attachments", "new_sessions"] };
  assert.equal(validators.sessionsUpload(list), true);
});

test("a session list may say the machine starts every agent in new sessions", () => {
  const list = { version: 1, machine: "studio", sessions: [], at: 1761220000000, capabilities: ["new_sessions", "new_session_agents"] };
  assert.equal(validators.sessionsUpload(list), true);
  assert.match(schemas["sessions-upload"].properties.capabilities.description, /`new_session_agents`/);
  assert.match(schemas.common.$defs.newSession.properties.agent.description, /`new_session_agents`/);
});

test("a new session's arguments are 1 to 32, each one line of at most 1024 characters and bytes", () => {
  const asked = fixtures.newSession.valid.find((value) => value.args);
  const read = (args) => validators.newSession({ version: 1, new_session: { ...asked, args } });
  assert.equal(read(Array.from({ length: 32 }, () => "--x")), true);
  assert.equal(read(Array.from({ length: 33 }, () => "--x")), false);
  assert.equal(read(["a".repeat(1024)]), true);
  assert.equal(read(["a".repeat(1025)]), false);
  assert.equal(read(["é".repeat(512)]), true);
  assert.equal(read(["é".repeat(513)]), false);
  assert.equal(read([" two words "]), true);
});

test("a session list may say the machine starts agents with arguments", () => {
  const list = { version: 1, machine: "studio", sessions: [], at: 1761220000000, capabilities: ["new_sessions", "new_session_agents", "new_session_args"] };
  assert.equal(validators.sessionsUpload(list), true);
  assert.match(schemas["sessions-upload"].properties.capabilities.description, /`new_session_args`/);
  assert.match(schemas.common.$defs.newSession.properties.args.description, /`new_session_args`/);
});

test("agent-commands.json names each agent a new session starts, with its program and arguments", () => {
  const commands = JSON.parse(readFileSync(new URL("../agent-commands.json", import.meta.url), "utf8"));
  assert.equal(commands.version, 1);
  assert.deepEqual(commands.agents.map(({ agent }) => agent), schemas.common.$defs.agent.enum);
  for (const { agent, program, arguments: args } of commands.agents) {
    assert.match(program, /^[a-z][a-z-]*$/, agent);
    assert.ok(args.length > 0, agent);
    for (const argument of args) {
      assert.deepEqual(Object.keys(argument).filter((key) => !["flag", "value", "values", "about", "repeats"].includes(key)), [], agent);
      assert.match(argument.flag, /^-/, agent);
      assert.equal(validators.newSession({ version: 1, new_session: { ...fixtures.newSession.valid.find((value) => value.args), agent, args: [argument.flag] } }), true, argument.flag);
    }
  }
});

test("new session body caps are annotated for the transport", () => {
  assert.equal(schemas["new-session-status"]["x-maxBodyBytes"], 4096);
});
