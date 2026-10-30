// RAKUMUX_PLUGIN=amp-session RAKUMUX_PLUGIN_VERSION=1
// Runs in Amp's plugin runtime, inside the Rakumux session (its identity is in
// the environment). The installer puts activity.mjs before this to make one module.
//
// - Binds the thread Amp shows to the session (`rakumux session bind`) and reports
//   its state (`rakumux session hook --agent amp`): ids and phases, no text.
// - Keeps a private copy of each thread it sees in its cache directory
//   (`threads/<thread>.jsonl`, owner only, removed after 30 days), which only the
//   Rakumux daemon on this machine reads: Amp keeps its threads on ampcode.com.
// - Never handles `tool.call`, so it can't allow, reject or change a tool call.
import { execFile } from "node:child_process";
import { appendFileSync, chmodSync, existsSync, mkdirSync, readFileSync, readdirSync, statSync, unlinkSync } from "node:fs";
import { join } from "node:path";
import { fileURLToPath } from "node:url";

export const description = "Rakumux: reports what Amp does to the Rakumux session it runs in, and keeps a private copy of its threads on this machine for Rakumux.";

const TICK_MS = 1_000;
const SYNC_MS = 2_000;
const HEARTBEAT_MS = 10_000;
const RETRY_MS = 10_000;
const KEEP_MS = 30 * 24 * 60 * 60 * 1000;
const PRUNE_MS = 24 * 60 * 60 * 1000;
const PAGE = 20;
const MAX_PAGES = 100;

export default function rakumux(amp) {
  if (!process.env.RAKUMUX_SESSION_ID || !process.env.RAKUMUX_SESSION_TOKEN) return;
  if (amp.system?.executor?.kind !== "local" || !amp.system?.cacheDirectory) return;
  const binary = process.env.RAKUMUX_BIN || "rakumux";
  const workspace = workspacePath(amp);
  const dir = join(amp.system.cacheDirectory, "threads");
  const active = amp.activeThread ?? amp.experimental?.activeThread;
  const threads = new Map();
  let focus = null;
  let stateSubscription = null;
  let bound = null;
  let bindRetryAt = 0;
  let previous = "";
  let sentAt = 0;
  let syncedAt = 0;
  let prunedAt = 0;
  let sending = false;
  let queue = Promise.resolve();
  let closed = false;
  let warned = false;

  function warn() {
    if (!warned) amp.logger?.log?.("Rakumux: thread copy or report unavailable; using terminal detection.");
    warned = true;
  }

  function invoke(args, input) {
    return new Promise((resolve) => {
      const child = execFile(binary, args, { timeout: 1500, maxBuffer: 8192 }, (error) => resolve(!error));
      child.stdin?.on("error", () => {});
      child.stdin?.end(input ?? "");
    });
  }

  function thread(id) {
    let entry = threads.get(id);
    if (!entry) {
      entry = { id, state: undefined, turn: null, messages: [], idleSince: undefined, written: null, fresh: false };
      threads.set(id, entry);
    }
    return entry;
  }

  function file(id) {
    return join(dir, `${id}.jsonl`);
  }

  /** The ids the copy already holds, read once; whether it's new (history goes in without times). */
  function load(entry) {
    if (entry.written) return;
    entry.written = new Set();
    const path = file(entry.id);
    entry.fresh = !existsSync(path);
    if (entry.fresh) return;
    for (const line of readFileSync(path, "utf8").split("\n")) {
      try {
        const value = JSON.parse(line);
        if (value?.kind === "message") entry.written.add(String(value.id));
      } catch {}
    }
  }

  function append(entry, line) {
    mkdirSync(dir, { recursive: true, mode: 0o700 });
    const path = file(entry.id);
    const created = !existsSync(path);
    appendFileSync(path, JSON.stringify(line) + "\n", { mode: 0o600 });
    if (created) {
      chmodSync(dir, 0o700);
      chmodSync(path, 0o600);
    }
  }

  function files(block) {
    try {
      const uris = amp.helpers?.filesModifiedByToolCall?.({ toolUseID: block.id, tool: block.name, input: block.input ?? {} });
      if (!Array.isArray(uris)) return undefined;
      return uris.map((uri) => amp.helpers.filePathFromURI(uri)).filter((path) => typeof path === "string" && path.startsWith("/"));
    } catch { return undefined; }
  }

  /**
   * Copy `messages` (oldest first) the copy doesn't hold yet. History (what a
   * thread had before the plugin first saw it) has no time: it isn't new.
   */
  function write(entry, messages, { history = false } = {}) {
    load(entry);
    let live = !history;
    for (const message of messages) {
      if (entry.turn?.open && String(message?.id) === String(entry.turn.id)) live = true;
      if (!message || message.id === undefined || entry.written.has(String(message.id))) continue;
      const timestamp = live ? new Date().toISOString() : undefined;
      append(entry, messageLine(message, { timestamp, cwd: workspace, files }));
      entry.written.add(String(message.id));
    }
  }

  /** The thread's messages back to one the copy holds (all of a new copy's, within bounds). */
  async function recent(entry) {
    const handle = amp.threads.get(entry.id);
    const found = [];
    for (let page = 0; page < MAX_PAGES; page++) {
      const batch = await handle.messages({ full: true, from: "end", offset: page * PAGE, limit: PAGE });
      if (!Array.isArray(batch) || batch.length === 0) break;
      found.unshift(...batch);
      if (batch.length < PAGE || batch.some((message) => entry.written.has(String(message?.id)))) break;
    }
    return found;
  }

  /** Bring the copy up to date; while the turn runs, its last message may still grow. */
  function sync(entry, final) {
    const job = async () => {
      load(entry);
      const history = entry.fresh;
      const messages = await recent(entry);
      entry.messages = messages.slice(-PAGE);
      write(entry, final ? messages : messages.slice(0, -1), { history });
      entry.fresh = false;
    };
    queue = queue.then(job).catch(warn);
    return queue;
  }

  function onState(id, state) {
    const entry = thread(id);
    const was = entry.state;
    entry.state = state;
    if (state === "running") entry.idleSince = undefined;
    if (state === "idle" && entry.turn?.open) entry.idleSince ??= Date.now();
    if (state !== was && (state === "idle" || state === "error")) void sync(entry, true);
    void tick();
  }

  function setFocus(id) {
    if (!isThreadId(id) || id === focus) return;
    focus = id;
    stateSubscription?.unsubscribe?.();
    stateSubscription = null;
    const entry = thread(id);
    try {
      stateSubscription = amp.threads.get(id).state.subscribe({
        next: (state) => onState(id, state),
        error: () => {},
      });
    } catch { warn(); }
    void sync(entry, entry.state === "idle");
    void tick();
  }

  /** A thread an event names: the one Amp shows, unless it says it shows another. */
  function seen(id) {
    const shown = active?.current?.id;
    if (!shown || shown === id) setFocus(id);
  }

  async function tick() {
    if (closed || sending) return;
    sending = true;
    try {
      const now = Date.now();
      if (now - prunedAt >= PRUNE_MS) {
        prunedAt = now;
        prune(now);
      }
      if (!focus) return;
      const entry = thread(focus);
      if (bound !== focus && now >= bindRetryAt) {
        if (await invoke(["session", "bind", "--agent", "amp", focus])) bound = focus;
        else bindRetryAt = now + RETRY_MS;
      }
      const busy = entry.state === "running" || entry.state === "awaiting-approval" || entry.turn?.open;
      if (busy && now - syncedAt >= SYNC_MS) {
        syncedAt = now;
        void sync(entry, false);
      }
      if (entry.state === "idle" && entry.turn?.open) entry.idleSince ??= now;
      const input = JSON.stringify(snapshot({ thread: focus, state: entry.state, turn: entry.turn, messages: entry.messages, idleSince: entry.idleSince, now, workspace }));
      if (input !== previous || now - sentAt >= HEARTBEAT_MS) {
        if (await invoke(["session", "hook", "--agent", "amp"], input)) {
          previous = input;
          sentAt = now;
        }
      }
    } catch { warn(); } finally { sending = false; }
  }

  /** Copies not written to for 30 days go. */
  function prune(now) {
    try {
      for (const name of readdirSync(dir)) {
        if (!name.endsWith(".jsonl")) continue;
        const path = join(dir, name);
        if (now - statSync(path).mtimeMs > KEEP_MS) unlinkSync(path);
      }
    } catch {}
  }

  const subscription = active?.subscribe?.((value) => { if (value?.id) setFocus(value.id); });
  amp.on("session.start", (event) => { seen(event?.thread?.id); });
  amp.on("agent.start", (event) => {
    const id = event?.thread?.id;
    if (!isThreadId(id)) return {};
    seen(id);
    const entry = thread(id);
    entry.turn = { id: event.id, open: true };
    entry.idleSince = undefined;
    const prompt = { role: "user", id: event.id, content: [{ type: "text", text: String(event.message ?? "") }] };
    entry.messages = [...entry.messages, prompt].slice(-PAGE);
    queue = queue.then(() => write(entry, [prompt])).catch(warn);
    void tick();
    return {};
  });
  // Amp waits for this handler: the copy is written after it returns, and the
  // turn counts as ended (a finished reply) once its messages are in the copy.
  amp.on("agent.end", (event) => {
    const id = event?.thread?.id;
    if (!isThreadId(id)) return;
    const entry = thread(id);
    const messages = Array.isArray(event.messages) ? event.messages : [];
    queue = queue.then(() => {
      try {
        write(entry, messages);
        append(entry, turnEndLine(event, new Date().toISOString()));
      } finally {
        entry.turn = { id: event.id, open: false, status: event.status, completion: completionOf(messages) };
        entry.messages = messages.slice(-PAGE);
      }
    }).catch(warn).then(() => tick());
  });
  const timer = setInterval(() => void tick(), TICK_MS);
  amp.onDispose?.(() => {
    closed = true;
    clearInterval(timer);
    subscription?.unsubscribe?.();
    stateSubscription?.unsubscribe?.();
  });
  void tick();
}

function workspacePath(amp) {
  const root = amp.system?.workspaceRoot;
  if (!root) return undefined;
  try {
    const path = amp.helpers?.filePathFromURI?.(root) ?? root.fsPath ?? fileURLToPath(String(root));
    return typeof path === "string" && path.startsWith("/") ? path : undefined;
  } catch { return undefined; }
}
