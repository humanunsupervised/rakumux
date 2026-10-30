import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import {
  END_GRACE_MS, MAX_TEXT, completionOf, isThreadId, messageLine, outputText, snapshot, turnEndLine,
} from "./rakumux-session/activity.mjs";

const T = "T-5f0c1d2e-3a4b-4c5d-8e9f-0a1b2c3d4e5f";
const prompt = { role: "user", id: "m1", content: [{ type: "text", text: "do not forward secrets" }] };
const toolCall = (name, id = "toolu_1") => ({ role: "assistant", id: "m2", content: [{ type: "text", text: "Looking." }, { type: "tool_use", id, name, input: { cmd: "ls" } }] });
const reply = { role: "assistant", id: "m4", content: [{ type: "text", text: "private reply" }] };

test("Amp's thread state becomes the phase, without message text", () => {
  const base = { thread: T, workspace: "/repo" };
  assert.equal(snapshot({ ...base, state: "awaiting-approval" }).phase, "needs_input");
  assert.equal(snapshot({ ...base, state: "running", messages: [prompt] }).phase, "waiting_for_response");
  assert.equal(snapshot({ ...base, state: "running", messages: [prompt, toolCall("Bash")] }).phase, "running_tool");
  const ran = { role: "user", id: "m3", content: [{ type: "tool_result", toolUseID: "toolu_1", status: "done", output: "secret output" }] };
  assert.equal(snapshot({ ...base, state: "running", messages: [prompt, toolCall("Bash"), ran] }).phase, "working");
  assert.equal(snapshot({ ...base, state: "error" }).phase, "idle");
  assert.equal(snapshot({ ...base, state: "idle" }).phase, "idle");
  const update = snapshot({ ...base, state: "running", messages: [prompt, toolCall("Bash"), ran] });
  assert.deepEqual(update.working_directory, { agent: "amp", conversation_id: T, path: "/repo" });
  assert.equal(update.conversation_id, T);
  assert.ok(!JSON.stringify(update).includes("secret"));
  assert.ok(!("working_directory" in snapshot({ thread: T, state: "idle" })));
});

test("a turn Amp ended as done is a finished reply, the same one on every heartbeat", () => {
  const turn = { id: "m1", open: false, status: "done", completion: completionOf([prompt, reply]) };
  const first = snapshot({ thread: T, state: "idle", turn, messages: [prompt, reply] });
  assert.equal(first.phase, "done");
  assert.equal(first.completion_id, "m4");
  assert.deepEqual(snapshot({ thread: T, state: "idle", turn, messages: [prompt, reply] }), first);
  assert.ok(!JSON.stringify(first).includes("private reply"));
  // Cancelled or failed turns are no reply.
  for (const status of ["cancelled", "error"]) {
    const update = snapshot({ thread: T, state: "idle", turn: { ...turn, status } });
    assert.equal(update.phase, "idle");
    assert.equal(update.completion_id, undefined);
  }
  // A numeric (legacy) id is a string on the wire.
  assert.equal(completionOf([{ role: "assistant", id: 12, content: [] }]), "12");
});

test("an open turn whose thread went idle waits briefly for its end", () => {
  const turn = { id: "m1", open: true };
  const now = 1_000_000;
  assert.equal(snapshot({ thread: T, state: "idle", turn, idleSince: now - 100, now }).phase, "working");
  // Its end never came: a final reply on screen is done, anything else idle.
  const late = { thread: T, state: "idle", turn, idleSince: now - END_GRACE_MS, now };
  assert.equal(snapshot({ ...late, messages: [prompt, toolCall("Bash")] }).phase, "idle");
  const done = snapshot({ ...late, messages: [prompt, reply] });
  assert.equal(done.phase, "done");
  assert.equal(done.completion_id, "m4");
  // Without Amp's state, the turn's own events say it's working.
  assert.equal(snapshot({ thread: T, turn, messages: [prompt] }).phase, "waiting_for_response");
});

test("running Task subagents are counted while the turn works", () => {
  const tasks = { role: "assistant", id: "m2", content: [
    { type: "tool_use", id: "toolu_a", name: "Task", input: { description: "search" } },
    { type: "tool_use", id: "toolu_b", name: "Task", input: { description: "tests" } },
    { type: "tool_use", id: "toolu_c", name: "Read", input: { path: "/a" } },
  ] };
  const partial = { role: "user", id: "m3", content: [
    { type: "tool_result", toolUseID: "toolu_a", status: "done", output: "found" },
    { type: "tool_result", toolUseID: "toolu_b", status: "running" },
  ] };
  const update = snapshot({ thread: T, state: "running", messages: [prompt, tasks, partial] });
  assert.equal(update.phase, "running_tool");
  assert.deepEqual(update.subagents, ["toolu_b"]);
  assert.deepEqual(snapshot({ thread: T, state: "idle", messages: [prompt, tasks, partial] }).subagents, []);
});

test("only Amp thread ids are used", () => {
  assert.ok(isThreadId(T));
  for (const bad of ["", "T-", "ses_1", "T-1/../../x", "T-1 2", 7, null, `T-${"a".repeat(200)}`]) assert.ok(!isThreadId(bad), String(bad));
});

test("the copy's lines are what the daemon's readers are tested on", () => {
  const fixture = JSON.parse(readFileSync(new URL("./fixtures/messages.json", import.meta.url), "utf8"));
  const expected = readFileSync(new URL("./fixtures/thread.jsonl", import.meta.url), "utf8").trimEnd().split("\n");
  const files = (block) => fixture.files[block.id];
  const lines = fixture.events.map((event) => JSON.stringify(event.message
    ? messageLine(event.message, { timestamp: event.timestamp, cwd: fixture.cwd, files })
    : turnEndLine(event.end, event.timestamp)));
  assert.deepEqual(lines, expected);
});

test("long text, tool output and images are bounded", () => {
  const long = "é".repeat(MAX_TEXT + 10);
  const line = messageLine({ role: "assistant", id: "a", content: [
    { type: "text", text: long },
    { type: "tool_use", id: "t", name: "create_file", input: { path: "/w/x", content: long } },
    { type: "redacted", data: "x" },
  ] });
  assert.equal(line.content.length, 2);
  assert.equal(line.content[0].text.length, MAX_TEXT);
  assert.equal(line.content[0].truncated, true);
  assert.equal(line.content[1].input.content.length, MAX_TEXT);
  assert.equal(line.content[1].input.path, "/w/x");
  assert.equal(line.content[1].truncated, true);
  assert.ok(!("timestamp" in line), "history has no time");
  assert.equal(outputText([{ type: "text", text: "ok" }, { type: "image", mimeType: "image/png", data: "AAAA" }]), "ok\n[image]");
  assert.equal(outputText({ output: "ok", exitCode: 0 }), '{"output":"ok","exitCode":0}');
  assert.equal(outputText(undefined), "");
  const result = messageLine({ role: "user", id: 3, content: [{ type: "tool_result", toolUseID: "t", status: "done", output: long }] });
  assert.equal(result.id, 3);
  assert.equal(result.content[0].output.length, MAX_TEXT);
  assert.equal(result.content[0].truncated, true);
});
