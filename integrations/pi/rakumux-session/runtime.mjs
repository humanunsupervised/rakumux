// RAKUMUX_EXTENSION=pi-session RAKUMUX_EXTENSION_VERSION=1
// Rakumux's extension for Pi: what Pi does, its conversation and its folder,
// sent to the Rakumux session it runs in. Nothing outside one, nothing but
// Pi's own TUI (never `pi -p`, `--mode json` or `rpc` children).
import { execFile } from "node:child_process";
// The installer puts activity.mjs before this runtime to make one module.

export default function rakumuxSession(pi) {
  if (!process.env.RAKUMUX_SESSION_ID || !process.env.RAKUMUX_SESSION_TOKEN) return;
  const binary = process.env.RAKUMUX_BIN || "rakumux";
  let state = initial();
  let tui = false;
  let timer;
  let sending = false;
  let again = false;
  let previous = "";
  let sentAt = 0;
  let bound;
  let bindRetryAt = 0;

  function invoke(args, input) {
    return new Promise((resolve) => {
      const child = execFile(binary, args, { timeout: 1500, maxBuffer: 8192 }, (error) => resolve(!error));
      child.stdin?.on("error", () => {});
      child.stdin?.end(input ?? "");
    });
  }

  // One call at a time; whatever changed meanwhile is sent next.
  async function flush() {
    if (sending) { again = true; return; }
    sending = true;
    try {
      do {
        again = false;
        const update = snapshot(state);
        if (!update) break;
        const id = update.conversation_id;
        if (bound !== id && Date.now() >= bindRetryAt) {
          if (await invoke(["session", "bind", "--agent", "pi", id])) bound = id;
          else bindRetryAt = Date.now() + 10_000;
        }
        const input = JSON.stringify(update);
        if (input !== previous || Date.now() - sentAt >= 10_000) {
          if (await invoke(["session", "hook", "--agent", "pi"], input)) {
            previous = input;
            sentAt = Date.now();
          }
        }
      } while (again);
    } catch {
      // Rakumux's own detection is the fallback.
    } finally {
      sending = false;
    }
  }

  for (const name of EVENTS) {
    pi.on(name, (event, ctx) => {
      if (name === "session_start") tui = ctx?.mode === "tui";
      if (!tui) return;
      state = reduce(state, name, event, ctx);
      if (name === "session_start" && !timer) {
        // Activity expires without updates: the same snapshot every 10 s.
        timer = setInterval(() => void flush(), 10_000);
        timer.unref?.();
      }
      void flush();
    });
  }
  pi.on("session_shutdown", () => {
    clearInterval(timer);
    timer = undefined;
  });
}
