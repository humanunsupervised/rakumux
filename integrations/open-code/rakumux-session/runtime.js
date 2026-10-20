// RAKUMUX_PLUGIN=opencode-session RAKUMUX_PLUGIN_VERSION=2
// Runs in the TUI, not OpenCode's shared service. Identity stays session-local.
import { execFile } from "node:child_process";
import { Plugin } from "@opencode/plugin/tui";
// The installer concatenates activity.mjs before this runtime to make one module.

export default Plugin.define({
  id: "rakumux.session",
  setup(context) {
    if (!process.env.RAKUMUX_SESSION_ID || !process.env.RAKUMUX_SESSION_TOKEN) return;
    const binary = process.env.RAKUMUX_BIN || "rakumux";
    let closed = false;
    let busy = false;
    let previous = "";
    let sentAt = 0;
    let bindRetryAt = 0;
    let warned = false;
    const bound = new Set();
    const roots = new Set();

    function invoke(args, input) {
      return new Promise((resolve) => {
        const child = execFile(binary, args, { timeout: 1500, maxBuffer: 8192 }, (error) => resolve(!error));
        child.stdin.on("error", () => {});
        child.stdin.end(input);
      });
    }

    async function tick() {
      if (closed || busy) return;
      busy = true;
      try {
        const state = snapshot(context);
        if (!state) return;
        roots.add(state.update.conversation_id);
        // Background descendants of previously visited roots need MCP identity too.
        const family = new Set(state.bindings);
        for (const root of roots) {
          for (const id of context.data.session.family(root) ?? []) family.add(id);
        }
        const missing = [...family].filter((id) => !bound.has(id)).slice(0, 64);
        if (missing.length && Date.now() >= bindRetryAt) {
          if (await invoke(["session", "bind", "--agent", "opencode", ...missing])) {
            for (const id of missing) bound.add(id);
          } else bindRetryAt = Date.now() + 10_000;
        }
        const input = JSON.stringify(state.update);
        if (input !== previous || Date.now() - sentAt >= 10_000) {
          if (await invoke(["session", "hook", "--agent", "opencode"], input)) {
            previous = input;
            sentAt = Date.now();
          }
        }
        warned = false;
      } catch {
        if (!warned) console.warn("Rakumux activity cache unavailable; using terminal detection.");
        warned = true;
      } finally { busy = false; }
    }

    const timer = setInterval(tick, 250);
    void tick();
    return () => { closed = true; clearInterval(timer); };
  },
});
