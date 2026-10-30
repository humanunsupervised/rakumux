import assert from "node:assert/strict";
import { test } from "node:test";
import { initial, reduce, snapshot } from "./rakumux-session/activity.mjs";

const ID = "01a18a95-6f2e-7c3b-9a1d-2cd72115b9d0";

function ctx(overrides = {}) {
  return {
    mode: "tui", cwd: "/launch", isIdle: () => true,
    sessionManager: { getSessionId: () => ID, getCwd: () => "/repo" },
    ...overrides,
  };
}

/** Run Pi's events (`[name, event]`) from a fresh start. */
function run(events, context = ctx()) {
  let state = reduce(initial(), "session_start", { reason: "startup" }, context);
  const phases = [];
  for (const [name, event] of events) {
    state = reduce(state, name, event ?? {}, context);
    phases.push(snapshot(state).phase);
  }
  return { state, phases, update: snapshot(state) };
}

const assistant = (stopReason, text = "private reply") => ({
  role: "assistant", stopReason, content: [{ type: "text", text }],
});

// The order Pi 1.4 emits for a prompt, a tool call and a reply (verified with
// an event-logging extension against an offline model).
const turnWithTool = [
  ["agent_start"], ["turn_start", { turnIndex: 0 }],
  ["message_start", { message: { role: "system", content: "" } }],
  ["message_start", { message: { role: "user", content: "secret prompt" } }],
  ["message_start", { message: { role: "assistant" } }],
  ["tool_execution_start", { toolCallId: "call_1", toolName: "bash", args: { command: "secret command" } }],
  ["tool_execution_end", { toolCallId: "call_1", toolName: "bash", result: "secret output", isError: false }],
  ["turn_end", { message: assistant("toolUse"), messageEntryId: "a1", toolResults: [] }],
  ["turn_start", { turnIndex: 1 }],
  ["message_start", { message: { role: "assistant" } }],
  ["turn_end", { message: assistant("stop"), messageEntryId: "a2", toolResults: [] }],
  ["agent_before_settle", { outcome: "completed" }],
  ["agent_settled", { aborted: false }],
];

test("a session starts idle, naming its conversation and folder", () => {
  const update = snapshot(reduce(initial(), "session_start", { reason: "startup" }, ctx()));
  assert.deepEqual(update, {
    version: 1, agent: "pi", conversation_id: ID, event: "snapshot", phase: "idle", subagents: [],
    working_directory: { agent: "pi", conversation_id: ID, path: "/repo" },
  });
  assert.equal(snapshot(initial()), undefined, "nothing before Pi names its conversation");
  const busy = reduce(initial(), "session_start", { reason: "reload" }, ctx({ isIdle: () => false }));
  assert.equal(snapshot(busy).phase, "working");
});

test("a turn waits for the model, works, runs tools and ends done with its reply's entry", () => {
  const { phases, update } = run(turnWithTool);
  assert.deepEqual(phases, [
    "waiting_for_response", "waiting_for_response", "waiting_for_response", "waiting_for_response", "working",
    "running_tool", "working", "working", "waiting_for_response", "working", "working",
    "working", "done",
  ]);
  assert.equal(update.completion_id, "a2");
  assert.ok(!JSON.stringify(update).includes("secret"));
  assert.ok(!JSON.stringify(update).includes("private"));
});

test("parallel tools run until the last ends; a codemode script's own calls don't count", () => {
  const { phases } = run([
    ["agent_start"],
    ["tool_execution_start", { toolCallId: "a" }],
    ["tool_execution_start", { toolCallId: "b" }],
    ["tool_execution_start", { toolCallId: "a/1", parentToolCallId: "a" }],
    ["tool_execution_end", { toolCallId: "a/1", parentToolCallId: "a" }],
    ["tool_execution_end", { toolCallId: "a" }],
    ["tool_execution_end", { toolCallId: "b" }],
  ]);
  assert.deepEqual(phases, [
    "waiting_for_response", "running_tool", "running_tool", "running_tool", "running_tool",
    "running_tool", "working",
  ]);
});

test("an extension's dialog waits for the user, then the turn goes on", () => {
  // A permission gate asks from its tool_call handler, inside the call.
  const { phases } = run([
    ["agent_start"],
    ["tool_execution_start", { toolCallId: "a" }],
    ["ui_prompt_start", { kind: "confirm", title: "Allow bash?" }],
    ["ui_prompt_end", { kind: "confirm" }],
    ["ui_prompt_end", { kind: "confirm" }],
    ["tool_execution_end", { toolCallId: "a" }],
  ]);
  assert.deepEqual(phases, ["waiting_for_response", "running_tool", "needs_input", "running_tool", "running_tool", "working"]);
});

test("stopped, failed or tool-only runs end idle, without a reply", () => {
  const settle = (aborted, outcome) => [["agent_before_settle", { outcome }], ["agent_settled", { aborted }]];
  const replied = [["agent_start"], ["turn_end", { message: assistant("stop"), messageEntryId: "a1" }]];
  assert.equal(run([...replied, ...settle(true, "aborted")]).update.phase, "idle", "Esc");
  assert.equal(run([...replied, ...settle(false, "error")]).update.phase, "idle", "an error");
  const failed = [["agent_start"], ["turn_end", { message: assistant("error"), messageEntryId: "a1" }]];
  assert.equal(run([...failed, ...settle(false, "completed")]).update.phase, "idle");
  const cut = [["agent_start"], ["turn_end", { message: assistant("length"), messageEntryId: "a9" }]];
  assert.equal(run([...cut, ...settle(false, "completed")]).update.completion_id, "a9");
  for (const { update } of [run([...replied, ...settle(true, "aborted")])]) {
    assert.equal(update.completion_id, undefined);
  }
});

test("a new run forgets the last reply until it has its own", () => {
  const { update, phases } = run([...turnWithTool, ["agent_start"], ["agent_settled", { aborted: true }]]);
  assert.equal(phases.at(-2), "waiting_for_response");
  assert.equal(update.phase, "idle");
  assert.equal(update.completion_id, undefined);
});

test("a new conversation (/new, /resume, /fork) starts over under its own id", () => {
  let state = run(turnWithTool).state;
  const other = ctx({ sessionManager: { getSessionId: () => "other-id", getCwd: () => "/elsewhere" } });
  state = reduce(state, "session_start", { reason: "new" }, other);
  const update = snapshot(state);
  assert.equal(update.conversation_id, "other-id");
  assert.equal(update.phase, "idle");
  assert.equal(update.working_directory.path, "/elsewhere");
});
