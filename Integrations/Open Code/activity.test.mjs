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
