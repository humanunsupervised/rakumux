// Rakumux plugin for OpenCode 2.x's TUI, loaded from
// <OpenCode config>/plugins/rakumux-session/tui.js.
// Installed by `rakumux-mcp opencode-plugin --install`; reinstalling
// overwrites this file.
// RAKUMUX_PLUGIN=opencode-session RAKUMUX_PLUGIN_VERSION=1
//
// OpenCode 2.x runs MCP servers in a background service shared by every
// conversation, so the Rakumux MCP server can't see which Rakumux session
// a tool call comes from; each call only names its OpenCode conversation.
// This plugin runs in the TUI, which does run inside the Rakumux session: it
// tells Rakumux which conversations this TUI shows (and their subagents),
// with `rakumux session bind`, and the MCP server then acts as this session
// for them. Outside a Rakumux session it does nothing.

import { execFile } from "node:child_process";

const AGENT = "opencode";
const POLL_MS = 250;
const RETRY_MS = 10_000;
const BATCH = 64;

function setup(api) {
  if (!process.env.RAKUMUX_SESSION_ID || !process.env.RAKUMUX_SESSION_TOKEN) return;
  const bin = process.env.RAKUMUX_BIN || "rakumux";
  // Conversations claimed (or being claimed) for this session.
  const claimed = new Set();
  const queue = new Set();
  let running = false;
  let retryAt = 0;
  let lastRoot;
  let disposed = false;

  function flush() {
    if (running || disposed || queue.size === 0 || Date.now() < retryAt) return;
    const ids = [...queue].slice(0, BATCH);
    for (const id of ids) queue.delete(id);
    running = true;
    execFile(bin, ["session", "bind", "--agent", AGENT, ...ids], { timeout: 5_000 }, (error) => {
      running = false;
      if (error) {
        // Rakumux missing or the daemon away: try again later.
        for (const id of ids) queue.add(id);
        retryAt = Date.now() + RETRY_MS;
        return;
      }
      flush();
    });
  }

  function claim(ids, again) {
    for (const id of ids) {
      if (typeof id !== "string" || !id) continue;
      if (claimed.has(id) && !again) continue;
      claimed.add(id);
      queue.add(id);
    }
    flush();
  }

  function rootOf(id) {
    try {
      return api.data.session.root(id) || id;
    } catch {
      return id;
    }
  }

  function familyOf(id) {
    try {
      return api.data.session.family(id) || [];
    } catch {
      return [];
    }
  }

  function sync() {
    if (disposed) return;
    let route;
    try {
      route = api.ui.router.current();
    } catch {
      return;
    }
    if (route?.type === "session" && typeof route.sessionID === "string") {
      const root = rootOf(route.sessionID);
      // Opening a conversation claims it again, so when it's open in
      // several Rakumux sessions the one used last wins.
      claim([root, route.sessionID, ...familyOf(root)], root !== lastRoot);
      lastRoot = root;
    }
    flush();
  }

  const unsubscribe = api.data.listen(({ details: event }) => {
    if (event?.type !== "session.created") return;
    const id = event.data?.sessionID;
    const parent = event.data?.parentID;
    // A subagent of a claimed conversation calls tools for this session too.
    if (typeof id === "string" && (claimed.has(parent) || claimed.has(rootOf(id)))) claim([id], false);
  });
  const poll = setInterval(sync, POLL_MS);
  poll.unref?.();
  sync();
  return () => {
    disposed = true;
    clearInterval(poll);
    unsubscribe?.();
  };
}

// OpenCode 1.x doesn't need this: it starts MCP servers inside each TUI,
// with the session's environment, so they already act as the session.
export default {
  id: "rakumux.session-binding",
  setup,
};
