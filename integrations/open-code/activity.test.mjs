import assert from "node:assert/strict";
import { test } from "node:test";
import { snapshot } from "./rakumux-session/activity.mjs";

function context() {
  return {
    ui: { router: { current: () => ({ type: "session", sessionID: "root" }) } },
    data: { session: {
      get: (id) => ({ id }), root: () => "root", family: () => ["root", "child", "finished"],
      status: (id) => id === "finished" ? undefined : { type: "running" },
      permission: { list: () => [] }, form: { list: () => [] },
      message: { list: () => [{ type: "user", text: "do not forward secrets" }] },
    } },
  };
}

test("reports explicit model wait and counts only active descendants", () => {
  const value = snapshot(context());
  assert.equal(value.update.phase, "waiting_for_response");
  assert.deepEqual(value.update.subagents, ["child"]);
  assert.ok(!JSON.stringify(value.update).includes("secrets"));
});

test("permissions/forms take priority over busy, then tool execution and idle", () => {
  const ctx = context();
  ctx.data.session.permission.list = (id) => id === "child" ? [{}] : [];
  assert.equal(snapshot(ctx).update.phase, "needs_input");
  ctx.data.session.permission.list = () => [];
  ctx.data.session.form.list = () => [{ state: { status: "pending" } }];
  assert.equal(snapshot(ctx).update.phase, "needs_input");
  ctx.data.session.form.list = () => [];
  ctx.data.session.message.list = () => [{ type: "assistant", content: [{ type: "tool", state: { status: "running" } }] }];
  assert.equal(snapshot(ctx).update.phase, "running_tool");
  ctx.data.session.status = () => undefined;
  assert.equal(snapshot(ctx).update.phase, "idle");
  assert.deepEqual(snapshot(ctx).update.subagents, []);
});

test("successful execution markers identify completed replies without reading their text", () => {
  const ctx = context();
  ctx.data.session.status = () => undefined;
  let completion = { id: "completion-1", type: "idle", outcome: "succeeded" };
  ctx.data.session.message.list = () => [
    { type: "assistant", content: [{ type: "text", text: "private reply" }] }, completion,
  ];
  const first = snapshot(ctx).update;
  assert.equal(first.phase, "done");
  assert.equal(first.completion_id, "completion-1");
  assert.deepEqual(snapshot(ctx).update, first, "heartbeats keep the same completion identity");
  completion = { ...completion, id: "completion-2" };
  assert.equal(snapshot(ctx).update.completion_id, "completion-2");
  assert.ok(!JSON.stringify(first).includes("private reply"));
  ctx.data.session.permission.list = () => [{}];
  assert.equal(snapshot(ctx).update.phase, "needs_input");
});

test("interrupted, failed and queued executions are not completed replies", () => {
  const ctx = context();
  ctx.data.session.status = () => undefined;
  for (const outcome of ["failed", "interrupted"]) {
    ctx.data.session.message.list = () => [{ id: "end", type: "idle", outcome }];
    assert.equal(snapshot(ctx).update.phase, "idle");
    assert.equal(snapshot(ctx).update.completion_id, undefined);
  }
  ctx.data.session.message.list = () => [{ type: "user" }];
  assert.equal(snapshot(ctx).update.phase, "idle");
});

test("active descendants take precedence over an already completed root reply", () => {
  const ctx = context();
  ctx.data.session.status = (id) => id === "child" ? { type: "running" } : undefined;
  ctx.data.session.message.list = () => [{ id: "end", type: "idle", outcome: "succeeded" }];
  assert.equal(snapshot(ctx).update.phase, "working");
  assert.deepEqual(snapshot(ctx).update.subagents, ["child"]);
});
