// Only use cached, documented OpenCode V2 data. Never read message text.
export function snapshot(context) {
  const route = context.ui.router.current();
  if (route.type !== "session") return;
  const sessions = context.data.session;
  const current = sessions.get(route.sessionID);
  if (!current) return;
  const root = sessions.root(current.id) ?? current.id;
  const family = [...new Set([root, ...(sessions.family(root) ?? [])])];
  const running = (id) => sessions.status(id)?.type === "running";
  const subagents = family.filter((id) => id !== root && running(id)).sort();
  const needsInput = family.some((id) =>
    (sessions.permission.list(id) ?? []).length > 0 ||
    (sessions.form.list(id, context.location) ?? []).some((form) => form.state.status === "pending"),
  );
  const messages = sessions.message.list(root) ?? [];
  const latest = messages.at(-1);
  let phase = "idle";
  if (needsInput) phase = "needs_input";
  else if (running(root)) {
    phase = "working";
    if (latest?.type === "user") phase = "waiting_for_response";
    if (latest?.type === "assistant" && latest.content.some((part) => part.type === "tool" && part.state.status === "running")) phase = "running_tool";
  } else if (subagents.length > 0) phase = "working";
  else if (latest?.type === "idle" && latest.outcome === "succeeded") phase = "done";
  return {
    bindings: family,
    update: {
      version: 1, agent: "opencode", conversation_id: root, event: "snapshot", phase,
      ...(phase === "done" ? { completion_id: latest.id } : {}),
      subagents: subagents.slice(0, 64),
    },
  };
}
