"use client";

// A small, working copy of Rakumux's screen. It plays by itself until the
// visitor clicks or focuses it, then takes the real keys: j/k or arrows to
// move, ← → to switch between the Feed, Status, Folders and App tabs, Enter to
// go into a session, Ctrl-\ to come back out. Colours and marks are the TUI's
// (Tokyo Night); each agent's screen is drawn the way that agent draws it.

import { useCallback, useEffect, useLayoutEffect, useRef, useState, type ReactNode } from "react";

type Status = "waiting" | "unread" | "working" | "running" | "ready";
type Tab = "feed" | "status" | "folders" | "app";
type App = "claude" | "codex" | "opencode" | "node";

interface Session {
  id: string;
  name: string;
  app: App;
  folder: string;
  branch: string;
  age: string;
  pinned: boolean;
  status: Status;
}

const SESSIONS: Session[] = [
  { id: "auth", name: "auth-refactor", app: "claude", folder: "web", branch: "feat/auth", age: "2m", pinned: true, status: "working" },
  { id: "flaky", name: "flaky-tests", app: "codex", folder: "web", branch: "fix/flaky", age: "9m", pinned: false, status: "waiting" },
  { id: "docs", name: "api-docs", app: "opencode", folder: "api", branch: "docs/pagination", age: "1h", pinned: false, status: "unread" },
  { id: "review", name: "pr-review", app: "codex", folder: "api", branch: "feat/limits", age: "5m", pinned: false, status: "working" },
  { id: "dev", name: "dev-server", app: "node", folder: "web", branch: "main", age: "3h", pinned: false, status: "running" },
  { id: "orders", name: "migrations", app: "claude", folder: "api", branch: "main", age: "2h", pinned: false, status: "ready" },
];

const BY_ID = Object.fromEntries(SESSIONS.map((s) => [s.id, s])) as Record<string, Session>;

const TABS: { id: Tab; mark: string; label: string }[] = [
  { id: "feed", mark: "≡", label: "Feed" },
  { id: "status", mark: "◔", label: "Status" },
  { id: "folders", mark: "▤", label: "Folders" },
  { id: "app", mark: "◇", label: "App" },
];

// The Status tab's headers, in the TUI's order (app/grouping.rs).
const STATUS_ORDER: Status[] = ["waiting", "unread", "working", "running", "ready"];
const STATUS_LABEL: Record<Status, string> = {
  waiting: "waiting for you",
  unread: "unread",
  working: "working",
  running: "running",
  ready: "ready",
};
const FOLDER_BRANCH: Record<string, string> = { web: "main", api: "main" };
const APP_ORDER: App[] = ["claude", "codex", "opencode", "node"];

const SPINNER = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
const CLAUDE_STARS = ["✢", "✳", "✶", "✻", "✽", "✻", "✶", "✳"];

function Mark({ status, frame }: { status: Status; frame: number }) {
  switch (status) {
    case "waiting":
      return <span className="tn-orange">◐</span>;
    case "unread":
      return <span className="tn-amber">●</span>;
    case "working":
      return <span className="tn-blue">{SPINNER[frame % SPINNER.length]}</span>;
    case "running":
      return <span className="tn-green">●</span>;
    case "ready":
      return <span className="tn-ready">○</span>;
  }
}

// ------------------------------------------------------------------ feed

interface Card {
  id: string;
  session: string;
  age: string;
  prompt: string;
  /** The answer as Markdown-ish segments; none while the turn is still working. */
  answer: ReactNode | null;
  short: string;
}

const CARDS: Card[] = [
  {
    id: "c-auth",
    session: "auth",
    age: "2m",
    prompt: "Move the session checks into middleware and add tests",
    answer: null,
    short: "",
  },
  {
    id: "c-review",
    session: "review",
    age: "40m",
    prompt: "Check the limiter for off-by-one errors",
    short: "One: the window starts at limit + 1, so the first request after a reset is refused.",
    answer: (
      <>
        <p>
          One: the window starts at <code>limit + 1</code>, so the first request after a reset is refused.
        </p>
        <p>
          I fixed it in <code>limiter.ts</code> and added a test that sends <b>exactly the limit</b>.
        </p>
      </>
    ),
  },
  {
    id: "c-docs",
    session: "docs",
    age: "1h",
    prompt: "Document the new cursor and limit params",
    short: "Updated 4 files: README.md, docs/api.md, docs/cli.md and CHANGELOG.md.",
    answer: (
      <>
        <p>
          Updated <b>4 files</b>: <code>README.md</code>, <code>docs/api.md</code>, <code>docs/cli.md</code> and{" "}
          <code>CHANGELOG.md</code>.
        </p>
        <p>
          The new <code>cursor</code> and <code>limit</code> params are documented with examples.
        </p>
      </>
    ),
  },
  {
    id: "c-orders",
    session: "orders",
    age: "2h",
    prompt: "Add an index for the orders query",
    short: "Added orders_user_created_idx and a migration. The query now uses an index scan.",
    answer: (
      <>
        <p>
          Added <code>orders_user_created_idx</code> and a migration. The query plan now uses an index scan (
          <b>41 ms → 2 ms</b>).
        </p>
      </>
    ),
  },
];

// ------------------------------------------------------------------ state

type Mode = "rakumux" | "session";

interface DemoState {
  mode: Mode;
  tab: Tab;
  selected: string;
  card: number;
  cameFrom: string | null;
  status: Record<string, Status>;
  typed: Record<string, string>;
  sent: Record<string, string[]>;
}

const START: DemoState = {
  mode: "rakumux",
  tab: "status",
  selected: "auth",
  card: 0,
  cameFrom: null,
  status: Object.fromEntries(SESSIONS.map((s) => [s.id, s.status])),
  typed: {},
  sent: {},
};

type Row =
  | { kind: "header"; key: string; label: ReactNode; count: string }
  | { kind: "session"; id: string }
  | { kind: "new" };

function live(ids: string[]): string {
  return `${ids.length}/${ids.length} live`;
}

function sessionsIn(s: DemoState, test: (x: Session) => boolean): string[] {
  return SESSIONS.filter((x) => !x.pinned && test(x)).map((x) => x.id);
}

/** The list's rows under a tab: the pinned section, then a header per status,
 * folder or app, as the TUI groups them. */
function rows(s: DemoState): Row[] {
  const out: Row[] = [];
  const pinned = SESSIONS.filter((x) => x.pinned).map((x) => x.id);
  out.push({
    kind: "header",
    key: "pinned",
    label: (
      <>
        <span className="tn-amber">★</span> pinned
      </>
    ),
    count: `${pinned.length} session${pinned.length === 1 ? "" : "s"}`,
  });
  pinned.forEach((id) => out.push({ kind: "session", id }));
  if (s.tab === "status") {
    for (const status of STATUS_ORDER) {
      const ids = sessionsIn(s, (x) => s.status[x.id] === status);
      if (!ids.length) continue;
      out.push({
        kind: "header",
        key: status,
        label: (
          <>
            <Mark status={status} frame={0} /> {STATUS_LABEL[status]}
          </>
        ),
        count: `${ids.length} session${ids.length === 1 ? "" : "s"}`,
      });
      ids.forEach((id) => out.push({ kind: "session", id }));
    }
  } else if (s.tab === "folders") {
    for (const folder of ["web", "api"]) {
      const ids = sessionsIn(s, (x) => x.folder === folder);
      const all = SESSIONS.filter((x) => x.folder === folder).map((x) => x.id);
      out.push({
        kind: "header",
        key: folder,
        label: (
          <>
            {folder} <span className="tn-muted">⎇ {FOLDER_BRANCH[folder]}</span>
          </>
        ),
        count: live(all),
      });
      ids.forEach((id) => out.push({ kind: "session", id }));
    }
    out.push({ kind: "new" });
  } else if (s.tab === "app") {
    for (const app of APP_ORDER) {
      const ids = sessionsIn(s, (x) => x.app === app);
      const all = SESSIONS.filter((x) => x.app === app).map((x) => x.id);
      if (!ids.length) continue;
      out.push({ kind: "header", key: app, label: <>{app}</>, count: live(all) });
      ids.forEach((id) => out.push({ kind: "session", id }));
    }
  }
  return out;
}

function order(s: DemoState): string[] {
  return rows(s).flatMap((r) => (r.kind === "session" ? [r.id] : []));
}

function move(s: DemoState, delta: number | "top" | "bottom"): DemoState {
  if (s.tab === "feed") {
    const last = CARDS.length - 1;
    const card = delta === "top" ? 0 : delta === "bottom" ? last : Math.max(0, Math.min(last, s.card + delta));
    return { ...s, card, selected: CARDS[card].session };
  }
  const ids = order(s);
  const at = Math.max(0, ids.indexOf(s.selected));
  const next = delta === "top" ? 0 : delta === "bottom" ? ids.length - 1 : Math.max(0, Math.min(ids.length - 1, at + delta));
  return { ...s, selected: ids[next] };
}

function switchTab(s: DemoState, delta: number): DemoState {
  const at = TABS.findIndex((t) => t.id === s.tab);
  const tab = TABS[Math.max(0, Math.min(TABS.length - 1, at + delta))].id;
  if (tab === s.tab) return s;
  if (tab === "feed") {
    const card = Math.max(0, CARDS.findIndex((c) => c.session === s.selected));
    return { ...s, tab, card };
  }
  return { ...s, tab };
}

/** Going in: an unread reply is read now, so it turns ready. */
function goIn(s: DemoState, id = s.selected): DemoState {
  const status = s.status[id] === "unread" ? { ...s.status, [id]: "ready" as Status } : s.status;
  return { ...s, mode: "session", selected: id, cameFrom: id, status };
}

function toRakumux(s: DemoState): DemoState {
  return { ...s, mode: "rakumux", cameFrom: s.selected };
}

function typeInto(s: DemoState, text: string): DemoState {
  const current = s.typed[s.selected] ?? "";
  return { ...s, typed: { ...s.typed, [s.selected]: (current + text).slice(0, 56) } };
}

/** Enter in a session: the line goes to the program, and an agent that was
 * waiting for you gets back to work. */
function send(s: DemoState): DemoState {
  const line = (s.typed[s.selected] ?? "").trim();
  if (!line) return s;
  const sent = [...(s.sent[s.selected] ?? []), line].slice(-2);
  const app = BY_ID[s.selected].app;
  const status =
    app !== "node" && (s.status[s.selected] === "waiting" || s.status[s.selected] === "ready" || s.status[s.selected] === "unread")
      ? { ...s.status, [s.selected]: "working" as Status }
      : s.status;
  return { ...s, typed: { ...s.typed, [s.selected]: "" }, sent: { ...s.sent, [s.selected]: sent }, status };
}

/** One key: the next state, whether the demo used the key, and whether a
 * first `g` of `gg` is pending. Tab is left to the browser so keyboard users
 * can always leave the demo. */
function reduce(
  s: DemoState,
  key: string,
  ctrl: boolean,
  pendingG: boolean,
): { next: DemoState; handled: boolean; pendingG: boolean } {
  const done = (next: DemoState, handled = true, g = false) => ({ next, handled, pendingG: g });
  if (ctrl) {
    if (key !== "\\") return done(s, false);
    if (s.mode === "session") return done(toRakumux(s));
    return done(s.cameFrom ? goIn(s, s.cameFrom) : goIn(s));
  }
  if (s.mode === "session") {
    if (key === "Enter") return done(send(s));
    if (key === "Backspace") {
      const t = s.typed[s.selected] ?? "";
      return done({ ...s, typed: { ...s.typed, [s.selected]: t.slice(0, -1) } });
    }
    if (key.length === 1) return done(typeInto(s, key));
    return done(s, key === "Escape");
  }
  switch (key) {
    case "j":
    case "ArrowDown":
      return done(move(s, 1));
    case "k":
    case "ArrowUp":
      return done(move(s, -1));
    case "ArrowLeft":
      return done(switchTab(s, -1));
    case "ArrowRight":
      return done(switchTab(s, 1));
    case "G":
    case "End":
      return done(move(s, "bottom"));
    case "Home":
      return done(move(s, "top"));
    case "g":
      return pendingG ? done(move(s, "top")) : done(s, true, true);
    case "Enter":
    case "i":
    case "l":
      return done(goIn(s));
    case "Escape":
      return done(s.cameFrom ? goIn(s, s.cameFrom) : s);
    default: {
      const ids = s.tab === "feed" ? SESSIONS.map((x) => x.id) : order(s);
      if (/^[1-9]$/.test(key) && Number(key) <= ids.length) return done(goIn(s, ids[Number(key) - 1]));
      return done(s, false);
    }
  }
}

// What the demo does while nobody's touching it.
const AUTOPLAY: { key: string; ctrl?: boolean; label?: string }[] = [
  { key: "j" },
  { key: "Enter" },
  { key: "1" },
  { key: "Enter" },
  { key: "\\", ctrl: true, label: "Ctrl \\" },
  { key: "j" },
  { key: "ArrowRight", label: "→" },
  { key: "ArrowRight", label: "→" },
  { key: "ArrowLeft", label: "←" },
  { key: "ArrowLeft", label: "←" },
  { key: "ArrowLeft", label: "←" },
  { key: "j" },
  { key: "j" },
  { key: "ArrowRight", label: "→" },
];

// ------------------------------------------------------------------ agents

interface ScreenProps {
  session: Session;
  status: Status;
  typed: string;
  sent: string[];
  active: boolean;
  frame: number;
}

const Cursor = ({ on }: { on: boolean }) => (on ? <span className="cursor" /> : null);

function ClaudeCode({ session, status, typed, sent, active, frame }: ScreenProps) {
  const busy = status === "working";
  const star = CLAUDE_STARS[frame % CLAUDE_STARS.length];
  return (
    <div className="ag ag-claude">
      <div className="ag-main">
        {session.id === "auth" ? (
          <>
            <div className="cc-you">❯ Move the session checks into middleware and add tests</div>
            <div className="cc-line"><span className="cc-text">●</span> I&apos;ll move the checks into one middleware and cover it with tests.</div>
            <div className="cc-line"><span className="cc-ok">●</span> <b>Read</b>(src/middleware/auth.ts)</div>
            <div className="cc-out">└ Read 84 lines</div>
            <div className="cc-line"><span className="cc-ok">●</span> <b>Update</b>(src/middleware/auth.ts)</div>
            <div className="cc-out">└ Updated <span className="cc-code">src/middleware/auth.ts</span> with 42 additions and 18 removals</div>
            <div className="cc-line"><span className="cc-ok">●</span> <b>Bash</b>(npm test -- auth)</div>
            <div className="cc-out">└ ✓ 23 passed (1.8s)</div>
          </>
        ) : (
          <>
            <div className="cc-you">❯ Add an index for the orders query</div>
            <div className="cc-line"><span className="cc-ok">●</span> <b>Write</b>(db/migrations/0042_orders_index.sql)</div>
            <div className="cc-out">└ Wrote 6 lines to <span className="cc-code">db/migrations/0042_orders_index.sql</span></div>
            <div className="cc-line">
              <span className="cc-text">●</span> Added <span className="cc-code">orders_user_created_idx</span> and a migration. The query plan now
              uses an index scan (<b>41 ms → 2 ms</b>).
            </div>
            {!sent.length && <div className="cc-dim">✻ Worked for 1m 12s</div>}
          </>
        )}
        {sent.map((line, i) => (
          <div className="cc-you" key={i}>❯ {line}</div>
        ))}
        {busy && (
          <div className="cc-busy">
            <span>{star}</span> {sent.length ? "Thinking…" : "Updating docs…"} <span className="cc-dim">(esc to interrupt)</span>
          </div>
        )}
      </div>
      <div className="ag-bottom">
        <div className="cc-rule" />
        <div className="cc-input">
          ❯ {typed}
          <Cursor on={active} />
        </div>
        <div className="cc-rule" />
        <div className="cc-mode">
          ▸▸ accept edits on <span className="cc-dim">(shift+tab to cycle)</span>
        </div>
      </div>
    </div>
  );
}

function Codex({ session, status, typed, sent, active, frame }: ScreenProps) {
  const asking = status === "waiting";
  const busy = status === "working";
  return (
    <div className="ag ag-codex">
      <div className="ag-main">
        <div className="cx-head">
          <span className="cx-logo">&gt;_</span> <b>OpenAI Codex</b>
        </div>
        <div className="cx-dim cx-indent">~/code/{session.folder}</div>
        {session.id === "flaky" ? (
          <>
            <div className="cx-you">› Why do the queue tests fail on CI?</div>
            <div><span className="cx-ok">•</span> <b>Ran</b> pnpm vitest --run queue</div>
            <div className="cx-dim">  └ 3 failed · 41 passed</div>
            <div>
              • All three share a timer race in <span className="cx-code">queue.spec.ts</span>. One line fixes it:
            </div>
            <div className="cx-del">  - await flush()</div>
            <div className="cx-add">  + await vi.runAllTimersAsync()</div>
            {asking && (
              <div className="cx-ask">
                <div><b>Would you like to make the following edits?</b></div>
                <div className="cx-pick">› 1. Yes, proceed</div>
                <div>  2. Yes, and don&apos;t ask again for these files</div>
                <div>  3. No, and tell Codex what to do differently <span className="cx-dim">esc</span></div>
              </div>
            )}
          </>
        ) : (
          <>
            <div className="cx-you">› Review the rate limit change in this branch</div>
            <div>• <b>Explored</b></div>
            <div className="cx-dim">  └ Read limiter.ts, limiter.test.ts</div>
            <div className="cx-dim">    Search retryAfter</div>
          </>
        )}
        {sent.map((line, i) => (
          <div className="cx-you" key={i}>› {line}</div>
        ))}
        {busy && (
          <div>
            <span className={frame % 2 ? "cx-dim" : ""}>•</span> <b className="cx-shimmer">Working</b>{" "}
            <span className="cx-dim">({14 + Math.floor(frame / 8)}s • esc to interrupt)</span>
          </div>
        )}
      </div>
      <div className="ag-bottom">
        <div className="cx-input">
          <b>›</b> {typed ? typed : !active ? <span className="cx-dim">Ask Codex to do anything</span> : null}
          <Cursor on={active} />
        </div>
        <div className="cx-foot">
          <span className="cx-model">GPT-6.1-Sol default</span> <span className="cx-sep">·</span>{" "}
          <span className="cx-path">~/code/{session.folder}</span>
        </div>
      </div>
    </div>
  );
}

function OpenCode({ session, status, typed, sent, active, frame }: ScreenProps) {
  const busy = status === "working";
  return (
    <div className="ag ag-opencode">
      <div className="oc-tabs">
        <span className="oc-tab">Document pagination ×</span>
        <span className="oc-plus">+</span>
      </div>
      <div className="ag-main">
        <div className="oc-you">Document the new cursor and limit params</div>
        <div className="oc-muted">→ Explored: 3 reads, 1 search</div>
        <div className="oc-thought">+ Thought: Checking the API docs · 2 steps · 6.4s</div>
        <div>I&apos;ll add the params to the API reference, the CLI docs and the changelog.</div>
        <div className="oc-muted">→ Edited: 4 files</div>
        <div>
          Updated <span className="oc-code">README.md</span>, <span className="oc-code">docs/api.md</span>,{" "}
          <span className="oc-code">docs/cli.md</span> and <span className="oc-code">CHANGELOG.md</span>. The new{" "}
          <span className="oc-code">cursor</span> and <span className="oc-code">limit</span> params are documented with
          examples.
        </div>
        {sent.map((line, i) => (
          <div className="oc-you" key={i}>{line}</div>
        ))}
        {busy && <div className="oc-muted">{SPINNER[frame % SPINNER.length]} Thinking…</div>}
      </div>
      <div className="ag-bottom">
        <div className="oc-input">
          <div>
            {typed}
            <Cursor on={active} />
            {!typed && !active ? " " : null}
          </div>
          <div>
            <span className="oc-agent">Build</span> <span className="oc-muted">·</span> Claude Opus 5.5{" "}
            <span className="oc-muted">Anthropic</span>
          </div>
        </div>
        <div className="oc-status">
          <span>~/code/{session.folder}:{session.branch}</span>
          <span>
            12.4K (6%) <b>ctrl+p</b> commands
          </span>
        </div>
      </div>
    </div>
  );
}

function DevServer({ typed, active }: ScreenProps) {
  return (
    <div className="ag">
      <div className="ag-main">
        <div className="tn-muted">&gt; my-app@0.4.2 dev</div>
        <div className="tn-muted">&gt; next dev</div>
        <div> </div>
        <div>
          <b>  ▲ Next.js 16.3</b>
        </div>
        <div>
          {"  "}- Local:   <span className="tn-cyan">http://localhost:3000</span>
        </div>
        <div> </div>
        <div>
          <span className="tn-green"> ✓</span> Ready in 1.2s
        </div>
        <div>
          {" "}GET / <span className="tn-green">200</span> <span className="tn-muted">in 41ms</span>
        </div>
        <div>
          {" "}GET /api/session <span className="tn-green">200</span> <span className="tn-muted">in 12ms</span>
        </div>
        {(typed || active) && (
          <div>
            {typed}
            <Cursor on={active} />
          </div>
        )}
      </div>
    </div>
  );
}

function Program(props: ScreenProps) {
  switch (props.session.app) {
    case "claude":
      return <ClaudeCode {...props} />;
    case "codex":
      return <Codex {...props} />;
    case "opencode":
      return <OpenCode {...props} />;
    case "node":
      return <DevServer {...props} />;
  }
}

/** A program's screen. When its output doesn't fit, the newest lines stay in
 * view, as in a terminal. */
function Screen(props: ScreenProps) {
  const frame = useRef<HTMLDivElement>(null);
  useLayoutEffect(() => {
    const main = frame.current?.querySelector<HTMLElement>(".ag-main");
    if (main) main.scrollTop = main.scrollHeight;
  });
  return (
    <div className="ag-frame" ref={frame}>
      <Program {...props} />
    </div>
  );
}

function FeedDetail({ card, status }: { card: Card; status: Status }) {
  const session = BY_ID[card.session];
  return (
    <div className="tf-detail">
      <div>
        <b className="tn-white">{session.name}</b>
        <span className="tn-muted"> · {session.folder} · {session.app} · on studio</span>
      </div>
      <div className="tn-muted">{card.answer ? `finished ${card.age} ago` : `asked ${card.age} ago`}</div>
      <div>
        <b className="tn-blue">Enter</b> <span className="tn-muted">goes into its session</span>
        {card.answer && (
          <>
            <span className="tn-muted"> · </span>
            <span className="tf-reply">↩ Reply  r</span>
          </>
        )}
      </div>
      <div className="tf-prompt tf-prompt-wide">{card.prompt}</div>
      {card.answer ? (
        <div className="tf-answer">{card.answer}</div>
      ) : (
        <div className="tf-answer">
          <Mark status={status} frame={0} /> <span className="tn-muted">working</span>
        </div>
      )}
    </div>
  );
}

// ------------------------------------------------------------------ demo

export default function TuiDemo() {
  const [state, setState] = useState<DemoState>(START);
  const stateRef = useRef<DemoState>(START);
  stateRef.current = state;
  const [pressed, setPressed] = useState<{ key: string; id: number } | null>(null);
  const [live, setLive] = useState(false); // the visitor has taken over
  const [focused, setFocused] = useState(false);
  const [frame, setFrame] = useState(0);
  const pendingG = useRef(false);
  const pressId = useRef(0);
  const frameEl = useRef<HTMLDivElement>(null);
  const listEl = useRef<HTMLDivElement>(null);

  const flash = useCallback((key: string) => {
    pressId.current += 1;
    setPressed({ key, id: pressId.current });
  }, []);

  // Busy marks spin, as in the TUI, unless the visitor prefers less motion.
  useEffect(() => {
    if (window.matchMedia("(prefers-reduced-motion: reduce)").matches) return;
    const timer = window.setInterval(() => setFrame((f) => f + 1), 120);
    return () => window.clearInterval(timer);
  }, []);

  // Autoplay, unless the visitor prefers less motion or has taken over.
  useEffect(() => {
    if (live) return;
    if (window.matchMedia("(prefers-reduced-motion: reduce)").matches) return;
    let step = 0;
    const timer = window.setInterval(() => {
      const { key, ctrl = false, label } = AUTOPLAY[step % AUTOPLAY.length];
      step += 1;
      const { next } = reduce(stateRef.current, key, ctrl, false);
      // Loop back to a clean screen.
      const after = step % AUTOPLAY.length === 0 ? START : next;
      stateRef.current = after;
      setState(after);
      flash(label ?? key);
    }, 1600);
    return () => window.clearInterval(timer);
  }, [live, flash]);

  const takeOver = useCallback(() => {
    if (!live) {
      setLive(true);
      stateRef.current = { ...START };
      setState(stateRef.current);
    }
  }, [live]);

  const press = useCallback(
    (key: string, label = key, ctrl = false): boolean => {
      takeOver();
      const { next, handled, pendingG: g } = reduce(stateRef.current, key, ctrl, pendingG.current);
      pendingG.current = g;
      stateRef.current = next;
      setState(next);
      if (handled) flash(label);
      return handled;
    },
    [takeOver, flash],
  );

  const onKeyDown = (e: React.KeyboardEvent) => {
    if (e.metaKey || e.altKey) return;
    const ctrl = e.ctrlKey;
    // Legacy terminals send Ctrl-\ as 0x1c; browsers report the key itself.
    if (ctrl && e.key !== "\\") return;
    const label = ctrl
      ? "Ctrl \\"
      : e.key === " "
        ? "Space"
        : e.key.length === 1
          ? e.key
          : e.key.replace("ArrowLeft", "←").replace("ArrowRight", "→").replace("ArrowUp", "↑").replace("ArrowDown", "↓");
    if (press(e.key, label, ctrl)) e.preventDefault();
  };

  const inSession = state.mode === "session";
  const current = BY_ID[state.selected];
  const listRows = rows(state);

  // Keep the selected row or card in view inside the list (never the page).
  useLayoutEffect(() => {
    const list = listEl.current;
    const row = list?.querySelector<HTMLElement>(".is-selected");
    if (!list || !row) return;
    const top = row.offsetTop - list.offsetTop;
    if (top < list.scrollTop) list.scrollTop = Math.max(0, top - 30);
    else if (top + row.offsetHeight > list.scrollTop + list.clientHeight) {
      list.scrollTop = top + row.offsetHeight - list.clientHeight + 8;
    }
  }, [state.selected, state.card, state.tab]);

  const select = (id: string) => {
    takeOver();
    const s = stateRef.current;
    const next = s.selected === id && s.mode === "rakumux" ? goIn(s, id) : { ...s, mode: "rakumux" as Mode, selected: id };
    stateRef.current = next;
    setState(next);
  };

  const selectCard = (index: number) => {
    takeOver();
    const s = stateRef.current;
    const next =
      s.card === index && s.mode === "rakumux"
        ? goIn(s, CARDS[index].session)
        : { ...s, mode: "rakumux" as Mode, card: index, selected: CARDS[index].session };
    stateRef.current = next;
    setState(next);
  };

  const showTab = (tab: Tab) => {
    takeOver();
    const s = stateRef.current;
    const at = TABS.findIndex((t) => t.id === s.tab);
    const to = TABS.findIndex((t) => t.id === tab);
    const next = { ...switchTab(s, to - at), mode: "rakumux" as Mode };
    stateRef.current = next;
    setState(next);
  };

  const showFeedDetail = state.tab === "feed" && !inSession;
  const footerSession = current;

  return (
    <div className="demo">
      <div
        ref={frameEl}
        className={`window ${focused ? "is-focused" : ""}`}
        tabIndex={0}
        role="application"
        aria-label="Interactive Rakumux demo. Use j and k to move, the left and right arrows to switch tabs, Enter to go into a session, Control backslash to switch back to Rakumux mode."
        onKeyDown={onKeyDown}
        onFocus={() => setFocused(true)}
        onBlur={() => setFocused(false)}
        onMouseDown={takeOver}
      >
        <div className="window-bar" aria-hidden="true">
          <span className="dot" />
          <span className="dot" />
          <span className="dot" />
          <span className="window-title">~/code/web — rakumux</span>
        </div>

        <div className="tui" aria-hidden="true">
          <div className="tui-body">
            <div className="tui-list">
              <div className="tui-tabs">
                {TABS.map((t) => {
                  const active = t.id === state.tab;
                  return (
                    <button
                      type="button"
                      tabIndex={-1}
                      key={t.id}
                      className={`tui-tab ${active ? "is-active" : ""}`}
                      onClick={() => showTab(t.id)}
                    >
                      {t.mark} {t.label}
                      <span className="tui-tab-caret">{active && t.id !== "feed" ? " ▾" : "  "}</span>
                    </button>
                  );
                })}
              </div>
              <div className="tui-rows" ref={listEl}>
                {state.tab === "feed"
                  ? CARDS.map((card, i) => {
                      const session = BY_ID[card.session];
                      const selected = i === state.card;
                      const status = state.status[card.session];
                      return (
                        <button
                          type="button"
                          tabIndex={-1}
                          key={card.id}
                          className={`tf-card ${selected ? "is-selected" : ""}`}
                          onClick={() => selectCard(i)}
                        >
                          <span className="tf-top">
                            <b className="tf-name">{session.name}</b>
                            <span className="tf-meta">
                              {session.app} · {session.branch}
                            </span>
                            <span className="tf-state">
                              {card.age} ·{" "}
                              {card.answer ? (
                                <span className="tn-green">● done</span>
                              ) : (
                                <span className="tn-blue">
                                  <Mark status={status === "working" ? "working" : "running"} frame={frame} /> working
                                </span>
                              )}
                            </span>
                          </span>
                          <span className="tf-prompt">{card.prompt}</span>
                          <span className="tf-short">{card.answer ? card.short : "…"}</span>
                        </button>
                      );
                    })
                  : listRows.map((row) => {
                      if (row.kind === "header") {
                        return (
                          <div className="tui-header" key={`h-${row.key}`}>
                            <span className="tui-header-label">{row.label}</span>
                            <span className="tui-rule" />
                            <span className="tui-header-count">{row.count}</span>
                          </div>
                        );
                      }
                      if (row.kind === "new") {
                        return (
                          <div className="tui-new" key="new">
                            + New session
                          </div>
                        );
                      }
                      const s = BY_ID[row.id];
                      const selected = row.id === state.selected;
                      return (
                        <button
                          type="button"
                          tabIndex={-1}
                          key={row.id}
                          className={`tui-item ${selected ? "is-selected" : ""}`}
                          onClick={() => select(row.id)}
                        >
                          <span className="tui-item-line">
                            <span className="tui-item-name">{s.name}</span>
                            <span className="tui-item-age">{s.age}</span>
                            <Mark status={state.status[row.id]} frame={frame} />
                          </span>
                          <span className="tui-item-sub">
                            <span className="tui-item-app">{s.app}</span> · ⎇ <span className="tui-item-branch">{s.branch}</span>
                          </span>
                        </button>
                      );
                    })}
              </div>
            </div>

            <div className={`tui-pane ${inSession ? "is-live" : showFeedDetail ? "" : "is-preview"}`}>
              {showFeedDetail ? (
                <FeedDetail card={CARDS[state.card]} status={state.status[CARDS[state.card].session]} />
              ) : (
                <Screen
                  session={current}
                  status={state.status[current.id]}
                  typed={state.typed[current.id] ?? ""}
                  sent={state.sent[current.id] ?? []}
                  active={inSession}
                  frame={frame}
                />
              )}
            </div>
          </div>

          <div className={`tui-footer ${inSession ? "is-session" : ""}`}>
            <span className="tui-footer-badge">{inSession ? "SESSION" : "RAKUMUX"}</span>
            <span className="tui-footer-session">
              session: <Mark status={state.status[footerSession.id]} frame={frame} /> <b>{footerSession.name}</b>
            </span>
            {inSession ? (
              <span className="tui-footer-keys">
                <span className="tui-chip">
                  <b>Ctrl-\</b> Rakumux mode
                </span>
              </span>
            ) : (
              <span className="tui-footer-keys">
                <span className="tui-chip">
                  <b>?</b> Help
                </span>
                <span className="tui-chip tui-chip-wide">
                  <b>,</b> Settings
                </span>
                <span className="tui-chip tui-chip-wide">
                  <b>a</b> ✓ Signed in
                </span>
              </span>
            )}
          </div>
        </div>

        {pressed && (
          <span key={pressed.id} className="keyflash" aria-hidden="true">
            {pressed.key}
          </span>
        )}
      </div>

      <div className="demo-controls">
        <p className="demo-hint">
          {focused ? (
            inSession ? (
              <>
                You&apos;re in a session: keys go to <b>{current.app === "node" ? "npm" : current.app}</b>. Press{" "}
                <kbd>Ctrl</kbd> <kbd>\</kbd> to get back out.
              </>
            ) : (
              <>
                <kbd>j</kbd> <kbd>k</kbd> move, <kbd>←</kbd> <kbd>→</kbd> tabs, <kbd>Enter</kbd> go in, <kbd>1</kbd>–
                <kbd>6</kbd> jump.
              </>
            )
          ) : (
            <>Click the demo to drive it with your keyboard.</>
          )}
        </p>
        <div className="demo-buttons" role="group" aria-label="Demo keys">
          <button type="button" onClick={() => { press("ArrowLeft", "←"); frameEl.current?.focus(); }} disabled={inSession} aria-label="Previous tab">
            ←
          </button>
          <button type="button" onClick={() => { press("k"); frameEl.current?.focus(); }} disabled={inSession} aria-label="Up">
            ↑
          </button>
          <button type="button" onClick={() => { press("j"); frameEl.current?.focus(); }} disabled={inSession} aria-label="Down">
            ↓
          </button>
          <button type="button" onClick={() => { press("ArrowRight", "→"); frameEl.current?.focus(); }} disabled={inSession} aria-label="Next tab">
            →
          </button>
          <button type="button" onClick={() => { press("Enter"); frameEl.current?.focus(); }} disabled={inSession}>
            Enter
          </button>
          <button
            type="button"
            className="is-accent"
            onClick={() => {
              press("\\", "Ctrl \\", true);
              frameEl.current?.focus();
            }}
          >
            Ctrl \
          </button>
        </div>
      </div>
    </div>
  );
}
