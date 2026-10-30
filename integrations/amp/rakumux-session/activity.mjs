// What the Rakumux plugin for Amp reports and keeps, as pure functions of
// Amp's documented plugin data (`@ampcode/plugin`: ThreadState, ThreadMessage).
// Snapshots carry ids and phases only; the thread copy carries the thread.

/** The most a block's text keeps, in characters: the transcript's part limit. */
export const MAX_TEXT = 64 * 1024;
/** How long an idle thread whose turn hasn't reported its end still counts as working. */
export const END_GRACE_MS = 3_000;
const MAX_SUBAGENTS = 64;

/** An Amp thread id that's safe as a file name and a conversation id. */
export function isThreadId(id) {
  return typeof id === "string" && /^T-[A-Za-z0-9_-]{1,126}$/.test(id);
}

/** A message id as the copy keeps it: Amp's own (numeric in legacy threads). */
export function messageId(id) {
  return typeof id === "number" && Number.isFinite(id) ? id : String(id);
}

function done(status) {
  return status === "done" || status === "error" || status === "cancelled" || status === "rejected-by-user";
}

/** Tool uses of `messages`' last assistant message that have no finished result yet. */
function pendingTools(messages) {
  const at = messages.findLastIndex((message) => message?.role === "assistant");
  if (at < 0) return [];
  const finished = new Set();
  for (const message of messages.slice(at + 1)) {
    for (const block of message?.content ?? []) {
      if (block?.type === "tool_result" && done(block.status)) finished.add(block.toolUseID);
    }
  }
  return (messages[at].content ?? []).filter((block) => block?.type === "tool_use" && !finished.has(block.id));
}

function typedPrompt(message) {
  return message?.role === "user" && (message.content ?? []).some((block) => block?.type === "text")
    && !(message.content ?? []).some((block) => block?.type === "tool_result");
}

function finalReply(message) {
  return message?.role === "assistant" && (message.content ?? []).some((block) => block?.type === "text" && block.text?.trim())
    && !(message.content ?? []).some((block) => block?.type === "tool_use");
}

/**
 * The activity snapshot of `thread`: `state` is Amp's (`idle`, `running`,
 * `awaiting-approval`, `error`), `turn` the latest turn the plugin saw
 * (`{id, open, status, completion}`), `messages` the newest messages read,
 * `idleSince` when an open turn's thread went idle. No message text is taken.
 */
export function snapshot({ thread, state, turn, messages = [], idleSince, now = Date.now(), workspace }) {
  const pending = pendingTools(messages);
  let phase = "idle";
  let completion;
  const busy = state === "running" || (state === undefined && turn?.open);
  if (state === "awaiting-approval") phase = "needs_input";
  else if (busy) {
    const latest = messages.at(-1);
    if (typedPrompt(latest)) phase = "waiting_for_response";
    else if (pending.length > 0) phase = "running_tool";
    else phase = "working";
  } else if (state === "error") phase = "idle";
  else if (turn && !turn.open) {
    if (turn.status === "done") {
      phase = "done";
      completion = turn.completion ?? String(turn.id);
    }
  } else if (turn?.open) {
    // Amp reports a turn's end (`agent.end`) around the time it goes idle.
    if (idleSince === undefined || now - idleSince < END_GRACE_MS) phase = "working";
    else if (finalReply(messages.at(-1))) {
      phase = "done";
      completion = String(messages.at(-1).id);
    }
  }
  const subagents = phase === "needs_input" || busy
    ? pending.filter((block) => block.name === "Task" && typeof block.id === "string" && block.id).map((block) => block.id).slice(0, MAX_SUBAGENTS)
    : [];
  return {
    version: 1, agent: "amp", conversation_id: thread, event: "snapshot", phase,
    ...(completion ? { completion_id: completion } : {}),
    subagents,
    ...(workspace ? { working_directory: { agent: "amp", conversation_id: thread, path: workspace } } : {}),
  };
}

/** The id of the turn's reply: its last assistant message's. */
export function completionOf(messages) {
  const last = (messages ?? []).findLast((message) => message?.role === "assistant");
  return last === undefined ? undefined : String(last.id);
}

/** `text` at most MAX_TEXT characters, and whether it was cut. */
export function cut(text) {
  return text.length > MAX_TEXT ? [text.slice(0, MAX_TEXT), true] : [text, false];
}

/** A tool's output as text: its text blocks, an image as `[image]`, anything else as JSON. */
export function outputText(output) {
  if (output === undefined || output === null) return "";
  if (typeof output === "string") return output;
  if (Array.isArray(output) && output.every((block) => block && typeof block.type === "string")) {
    return output.map((block) => block.type === "text" ? String(block.text ?? "") : `[${block.type}]`).join("\n");
  }
  try { return JSON.stringify(output) ?? ""; } catch { return ""; }
}

/** `value` with every string cut to MAX_TEXT, and whether any was. */
function capped(value) {
  let truncated = false;
  const walk = (value, depth) => {
    if (typeof value === "string") {
      const [text, cutHere] = cut(value);
      truncated ||= cutHere;
      return text;
    }
    if (depth > 16 || value === null || typeof value !== "object") return value;
    if (Array.isArray(value)) return value.map((item) => walk(item, depth + 1));
    return Object.fromEntries(Object.entries(value).map(([key, item]) => [key, walk(item, depth + 1)]));
  };
  return [walk(value, 0), truncated];
}

/** A block as the copy keeps it; `files(block)` names the files a tool call changes. */
function copyBlock(block, files) {
  const text = (key) => {
    const [value, truncated] = cut(String(block[key] ?? ""));
    return { type: block.type, [key]: value, ...(truncated ? { truncated } : {}) };
  };
  switch (block?.type) {
    case "text": return text("text");
    case "thinking": return text("thinking");
    case "tool_use": {
      const [input, truncated] = capped(block.input ?? {});
      const changed = files?.(block);
      return {
        type: "tool_use", id: String(block.id ?? ""), name: String(block.name ?? ""), input,
        ...(Array.isArray(changed) && changed.length > 0 ? { files: changed.slice(0, 256) } : {}),
        ...(truncated ? { truncated } : {}),
      };
    }
    case "tool_result": {
      const [output, truncated] = cut(outputText(block.output));
      return { type: "tool_result", toolUseID: String(block.toolUseID ?? ""), status: String(block.status ?? ""), output, ...(truncated ? { truncated } : {}) };
    }
    default: return undefined;
  }
}

/** The copy's line for a thread message (`ThreadMessage`). */
export function messageLine(message, { timestamp, cwd, files } = {}) {
  return {
    v: 1, kind: "message", id: messageId(message.id), role: String(message.role),
    ...(timestamp ? { timestamp } : {}),
    ...(cwd ? { cwd } : {}),
    content: (message.content ?? []).map((block) => copyBlock(block, files)).filter(Boolean),
  };
}

/** The copy's line for where a turn ended (`agent.end`). */
export function turnEndLine(event, timestamp) {
  return { v: 1, kind: "turn_end", id: messageId(event.id), status: String(event.status), ...(timestamp ? { timestamp } : {}) };
}
