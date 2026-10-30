// What Pi's extension events say it does, as Rakumux activity v1. Metadata
// only: never a prompt, a tool's arguments or output, or a message's text.

/** Before Pi's first session_start. */
export function initial() {
  return { conversation: undefined, cwd: undefined, phase: "idle", tools: [], prompts: 0,
    finished: undefined, outcome: undefined, completion: undefined };
}

/** The state after Pi's event `name` (`event`, with its context `ctx`). */
export function reduce(state, name, event, ctx) {
  switch (name) {
    case "session_start": {
      const manager = ctx.sessionManager;
      return { ...initial(), conversation: manager.getSessionId(),
        cwd: manager.getCwd?.() ?? ctx.cwd, phase: ctx.isIdle() ? "idle" : "working" };
    }
    case "agent_start":
      return { ...state, phase: "waiting_for_response", tools: [], finished: undefined,
        outcome: undefined, completion: undefined };
    case "turn_start":
      return { ...state, phase: "waiting_for_response", tools: [] };
    case "message_start":
      return event.message?.role === "assistant" ? { ...state, phase: "working" } : state;
    case "tool_execution_start": {
      // A codemode script's own calls run inside its call.
      if (event.parentToolCallId) return state;
      return { ...state, phase: "running_tool", tools: [...state.tools, event.toolCallId] };
    }
    case "tool_execution_end": {
      if (event.parentToolCallId) return state;
      const tools = state.tools.filter((id) => id !== event.toolCallId);
      return { ...state, tools, phase: tools.length ? "running_tool" : "working" };
    }
    case "turn_end": {
      // A response that finished with no tool to call is the turn's reply.
      const stop = event.message?.role === "assistant" ? event.message.stopReason : undefined;
      return { ...state, finished: stop === "stop" || stop === "length" ? event.messageEntryId : undefined };
    }
    case "agent_before_settle":
      return { ...state, outcome: event.outcome };
    case "agent_settled": {
      // Retries, compaction and queued messages are over: done, unless it was
      // stopped or failed.
      const done = !event.aborted && state.outcome !== "error" && Boolean(state.finished);
      return { ...state, tools: [], phase: done ? "done" : "idle",
        completion: done ? state.finished : undefined };
    }
    case "ui_prompt_start":
      return { ...state, prompts: state.prompts + 1 };
    case "ui_prompt_end":
      return { ...state, prompts: Math.max(0, state.prompts - 1) };
    default:
      return state;
  }
}

/** The update to send for `state`, once Pi named its conversation. */
export function snapshot(state) {
  if (!state.conversation) return;
  // An extension's dialog waits for the user, whatever runs behind it.
  const phase = state.prompts > 0 ? "needs_input" : state.phase;
  return {
    version: 1, agent: "pi", conversation_id: state.conversation, event: "snapshot", phase,
    ...(phase === "done" ? { completion_id: state.completion } : {}),
    subagents: [],
    ...(state.cwd ? { working_directory: {
      agent: "pi", conversation_id: state.conversation, path: state.cwd,
    } } : {}),
  };
}

/** The events `reduce` reads. */
export const EVENTS = ["session_start", "agent_start", "turn_start", "message_start",
  "tool_execution_start", "tool_execution_end", "turn_end", "agent_before_settle",
  "agent_settled", "ui_prompt_start", "ui_prompt_end"];
