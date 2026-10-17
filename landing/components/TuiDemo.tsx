"use client";

// A small, working copy of Orchestra's screen. It plays by itself until the
// visitor focuses it, then takes the real keys: j/k or arrows to move (the
// session previews on the right), Enter to go in, Ctrl-\ to come back out.

import { useCallback, useEffect, useRef, useState } from "react";

type State = "working" | "waiting" | "done" | "running" | "idle";
type Seg = [text: string, tone?: string];
type Row = Seg[];

interface Session {
  name: string;
  program: string;
  branch: string;
  state: State;
  age: string;
  screen: Row[];
  prompt: string;
}

const SESSIONS: Session[] = [
  {
    name: "auth-refactor",
    program: "claude",
    branch: "feat/auth",
    state: "working",
    age: "2m",
    prompt: "›",
    screen: [
      [["› ", "muted"], ["Move the session checks into middleware and add tests", "bright"]],
      [],
      [["● ", "green"], ["Read ", "dim"], ["src/middleware/auth.ts"]],
      [["● ", "green"], ["Edit ", "dim"], ["src/middleware/auth.ts "], ["+42 ", "green"], ["−18", "red"]],
      [["● ", "green"], ["Run ", "dim"], ["npm test -- auth"]],
      [["  ✓ 23 passed ", "green"], ["(1.8s)", "muted"]],
      [],
      [["⠹ ", "orange"], ["Updating docs/auth.md…", "dim"]],
    ],
  },
  {
    name: "flaky-tests",
    program: "codex",
    branch: "fix/flaky-tests",
    state: "waiting",
    age: "9m",
    prompt: "›",
    screen: [
      [["Ran ", "dim"], ["pnpm vitest --run "], ["3 failed", "red"]],
      [],
      [["All three share a timer race in "], ["queue.spec.ts", "cyan"], [". Proposed fix:"]],
      [["  - await flush()", "red"]],
      [["  + await vi.runAllTimersAsync()", "green"]],
      [],
      [["Apply this change? ", "yellow"], ["[y/n]", "bright"]],
    ],
  },
  {
    name: "api-docs",
    program: "opencode",
    branch: "docs/pagination",
    state: "done",
    age: "1h",
    prompt: ">",
    screen: [
      [["Updated 4 files", "bright"]],
      [["  README.md  docs/api.md  docs/cli.md  CHANGELOG.md", "dim"]],
      [],
      [["Documented the new "], ["cursor", "cyan"], [" and "], ["limit", "cyan"], [" params."]],
      [],
      [["✓ Done. ", "green"], ["Ready for review.", "dim"]],
    ],
  },
  {
    name: "dev-server",
    program: "npm",
    branch: "main",
    state: "running",
    age: "3h",
    prompt: "",
    screen: [
      [["> my-app@0.4.2 dev", "muted"]],
      [["> next dev", "muted"]],
      [],
      [["  ▲ ", "bright"], ["Local:   "], ["http://localhost:3000", "cyan"]],
      [[" ✓ ", "green"], ["Ready in 1.2s"]],
      [[" GET ", "dim"], ["/ "], ["200", "green"], [" in 41ms", "muted"]],
      [[" GET ", "dim"], ["/api/session "], ["200", "green"], [" in 12ms", "muted"]],
    ],
  },
  {
    name: "shell",
    program: "zsh",
    branch: "main",
    state: "idle",
    age: "1d",
    prompt: "$",
    screen: [
      [["~/code/my-app ", "blue"], ["main ", "purple"], ["$ ", "muted"], ["git log --oneline -3"]],
      [["a41c9e2 ", "yellow"], ["Add pagination to /api/items"]],
      [["7d03b18 ", "yellow"], ["Fix flaky queue test"]],
      [["c5e2f40 ", "yellow"], ["Bump next to 16.3"]],
    ],
  },
];

const STATE_LABEL: Record<State, string> = {
  working: "working",
  waiting: "waiting for you",
  done: "done",
  running: "",
  idle: "",
};

const STATE_DOT: Record<State, [string, string]> = {
  working: ["●", "green"],
  waiting: ["◐", "yellow"],
  done: ["●", "blue"],
  running: ["●", "dim"],
  idle: ["○", "muted"],
};

type Mode = "orchestra" | "session";

interface Step {
  key: string;
  run: (s: DemoState) => DemoState;
}

interface DemoState {
  mode: Mode;
  selected: number;
  cameFrom: number | null;
  typed: Record<number, string>;
  sent: Record<number, string[]>;
}

const START: DemoState = {
  mode: "orchestra",
  selected: 0,
  cameFrom: null,
  typed: {},
  sent: {},
};

function select(s: DemoState, index: number): DemoState {
  const next = Math.max(0, Math.min(SESSIONS.length - 1, index));
  if (next === s.selected) return s;
  return { ...s, selected: next };
}

function goIn(s: DemoState): DemoState {
  return { ...s, mode: "session", cameFrom: s.selected };
}

function toOrchestra(s: DemoState): DemoState {
  return { ...s, mode: "orchestra", cameFrom: s.selected };
}

function typeInto(s: DemoState, text: string): DemoState {
  const current = s.typed[s.selected] ?? "";
  return { ...s, typed: { ...s.typed, [s.selected]: (current + text).slice(0, 48) } };
}

function send(s: DemoState): DemoState {
  const line = s.typed[s.selected] ?? "";
  const sent = [...(s.sent[s.selected] ?? []), line].slice(-3);
  return { ...s, typed: { ...s.typed, [s.selected]: "" }, sent: { ...s.sent, [s.selected]: sent } };
}

/** One key in the demo: the next state, whether the demo used the key, and
 * whether a first `g` of `gg` is pending. Tab is left to the browser so
 * keyboard users can always leave the demo. */
function reduce(
  s: DemoState,
  key: string,
  ctrl: boolean,
  pendingG: boolean,
): { next: DemoState; handled: boolean; pendingG: boolean } {
  const done = (next: DemoState, handled = true, g = false) => ({ next, handled, pendingG: g });
  if (ctrl) {
    if (key !== "\\") return done(s, false);
    if (s.mode === "session") return done(toOrchestra(s));
    return done(s.cameFrom !== null ? goIn({ ...s, selected: s.cameFrom }) : s);
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
      return done(select(s, s.selected + 1));
    case "k":
    case "ArrowUp":
      return done(select(s, s.selected - 1));
    case "G":
    case "End":
      return done(select(s, SESSIONS.length - 1));
    case "Home":
      return done(select(s, 0));
    case "g":
      return pendingG ? done(select(s, 0)) : done(s, true, true);
    case "Enter":
    case "i":
    case "l":
    case "ArrowRight":
      return done(goIn(s));
    case "Escape":
      return done(s.cameFrom !== null ? goIn({ ...s, selected: s.cameFrom }) : s);
    default:
      if (/^[1-9]$/.test(key) && Number(key) <= SESSIONS.length) {
        return done(goIn(select(s, Number(key) - 1)));
      }
      return done(s, false);
  }
}

// What the demo does while nobody's touching it.
const AUTOPLAY: Step[] = [
  { key: "j", run: (s) => select(s, s.selected + 1) },
  { key: "j", run: (s) => select(s, s.selected + 1) },
  { key: "j", run: (s) => select(s, s.selected + 1) },
  { key: "k", run: (s) => select(s, s.selected - 1) },
  { key: "k", run: (s) => select(s, s.selected - 1) },
  { key: "Enter", run: goIn },
  { key: "y", run: (s) => typeInto(s, "y") },
  { key: "Enter", run: send },
  { key: "Ctrl \\", run: toOrchestra },
  { key: "k", run: (s) => select(s, s.selected - 1) },
  { key: "G", run: (s) => select(s, SESSIONS.length - 1) },
  { key: "gg", run: (s) => select(s, 0) },
];

export default function TuiDemo() {
  const [state, setState] = useState<DemoState>(START);
  const stateRef = useRef<DemoState>(START);
  stateRef.current = state;
  const [pressed, setPressed] = useState<{ key: string; id: number } | null>(null);
  const [live, setLive] = useState(false); // the visitor has taken over
  const [focused, setFocused] = useState(false);
  const pendingG = useRef(false);
  const pressId = useRef(0);
  const frame = useRef<HTMLDivElement>(null);

  const flash = useCallback((key: string) => {
    pressId.current += 1;
    setPressed({ key, id: pressId.current });
  }, []);

  // Autoplay, unless the visitor prefers less motion or has taken over.
  useEffect(() => {
    if (live) return;
    if (window.matchMedia("(prefers-reduced-motion: reduce)").matches) return;
    let step = 0;
    const timer = window.setInterval(() => {
      const { key, run } = AUTOPLAY[step % AUTOPLAY.length];
      step += 1;
      setState((s) => {
        const next = run(s);
        // Loop back to a clean screen.
        return step % AUTOPLAY.length === 0 ? { ...START, selected: next.selected } : next;
      });
      flash(key);
    }, 1500);
    return () => window.clearInterval(timer);
  }, [live, flash]);

  const takeOver = useCallback(() => {
    if (!live) {
      setLive(true);
      setState((s) => ({ ...START, selected: s.selected }));
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
    const label = ctrl ? "Ctrl \\" : e.key === " " ? "Space" : e.key.length === 1 ? e.key : e.key.replace("Arrow", "");
    if (press(e.key, label, ctrl)) e.preventDefault();
  };

  const current = SESSIONS[state.selected];
  const typed = state.typed[state.selected] ?? "";
  const sent = state.sent[state.selected] ?? [];
  const inSession = state.mode === "session";

  return (
    <div className="demo">
      <div
        ref={frame}
        className={`window ${focused ? "is-focused" : ""}`}
        tabIndex={0}
        role="application"
        aria-label="Interactive Orchestra demo. Use j and k to move, Enter to go into a session, Control backslash to switch back to Orchestra mode."
        onKeyDown={onKeyDown}
        onFocus={() => setFocused(true)}
        onBlur={() => setFocused(false)}
        onMouseDown={takeOver}
      >
        <div className="window-bar" aria-hidden="true">
          <span className="dot" />
          <span className="dot" />
          <span className="dot" />
          <span className="window-title">~/code/my-app — orchestra</span>
        </div>

        <div className="tui" aria-hidden="true">
          <div className="tui-body">
            <div className="tui-list">
              <div className="tui-folders">
                <span>
                  <span className="t-dim">▤ </span>
                  <span className="t-bright">All folders</span>
                </span>
                <span className="t-muted">▾</span>
              </div>
              {SESSIONS.map((s, i) => {
                const isSelected = i === state.selected;
                const [dot, dotTone] = STATE_DOT[s.state];
                const label = STATE_LABEL[s.state];
                return (
                  <button
                    type="button"
                    key={s.name}
                    tabIndex={-1}
                    className={`tui-item ${isSelected ? "is-selected" : ""}`}
                    onClick={() => {
                      takeOver();
                      setState((st) => (st.mode === "session" ? { ...select(st, i), mode: "orchestra" } : select(st, i)));
                    }}
                  >
                    <span className="tui-item-line">
                      <span className={`t-${dotTone}`}>{dot}</span>
                      <span className="tui-item-name">{s.name}</span>
                      <span className="tui-item-age">{s.age}</span>
                    </span>
                    <span className="tui-item-sub">
                      ⎇ {s.branch} · {s.program}
                      {label ? ` · ${label}` : ""}
                    </span>
                  </button>
                );
              })}
              <div className="tui-new t-muted">+ New session</div>
            </div>

            <div className={`tui-pane ${inSession ? "is-live" : ""}`}>
              <div className="tui-pane-title">
                <span className={`t-${STATE_DOT[current.state][1]}`}>{STATE_DOT[current.state][0]}</span>{" "}
                <span className="t-bright">{current.name}</span>
                <span className="t-muted">
                  {" "}
                  · {current.program}
                  {STATE_LABEL[current.state] ? ` · ${STATE_LABEL[current.state]}` : ""}
                </span>
              </div>
              <div className="tui-screen">
                {current.screen.map((row, r) => (
                  <div className="tui-row" key={r}>
                    {row.length === 0 ? " " : row.map(([text, tone], c) => (
                      <span key={c} className={tone ? `t-${tone}` : undefined}>
                        {text}
                      </span>
                    ))}
                  </div>
                ))}
                {sent.map((line, r) => (
                  <div className="tui-row" key={`sent-${r}`}>
                    <span className="t-muted">{current.prompt || "$"} </span>
                    <span className="t-bright">{line}</span>
                  </div>
                ))}
                {current.prompt !== "" || inSession ? (
                  <div className="tui-row tui-input">
                    <span className="t-muted">{current.prompt || "$"} </span>
                    <span className="t-bright">{typed}</span>
                    {inSession && <span className="cursor" />}
                  </div>
                ) : null}
              </div>
            </div>
          </div>

          <div className="tui-footer">
            <span className={`badge ${inSession ? "badge-session" : "badge-orchestra"}`}>
              {inSession ? " SESSION " : " ORCHESTRA "}
            </span>
            <span className="t-muted tui-footer-msg">
              {inSession ? `Typing into ${current.name}` : live ? "" : "Demo playing: click or tab here to drive it"}
            </span>
            <span className="tui-spacer" />
            {inSession ? (
              <span className="tui-footer-keys">
                <b>Ctrl-\</b> orchestra
              </span>
            ) : (
              <span className="tui-footer-keys">
                <b>?</b> help <b>,</b> settings
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
                You&apos;re in a session: keys go to <b>{current.program}</b>. Press <kbd>Ctrl</kbd> <kbd>\</kbd> to get back
                out.
              </>
            ) : (
              <>
                <kbd>j</kbd> <kbd>k</kbd> move, <kbd>gg</kbd> <kbd>G</kbd> top and bottom, <kbd>Enter</kbd> go in,{" "}
                <kbd>1</kbd>–<kbd>5</kbd> jump.
              </>
            )
          ) : (
            <>Click the demo to drive it with your keyboard.</>
          )}
        </p>
        <div className="demo-buttons" role="group" aria-label="Demo keys">
          <button type="button" onClick={() => { press("k"); frame.current?.focus(); }} disabled={inSession} aria-label="Up">
            ↑
          </button>
          <button type="button" onClick={() => { press("j"); frame.current?.focus(); }} disabled={inSession} aria-label="Down">
            ↓
          </button>
          <button type="button" onClick={() => { press("Enter"); frame.current?.focus(); }} disabled={inSession}>
            Enter
          </button>
          <button
            type="button"
            className="is-accent"
            onClick={() => {
              press("\\", "Ctrl \\", true);
              frame.current?.focus();
            }}
          >
            Ctrl \
          </button>
        </div>
      </div>
    </div>
  );
}
