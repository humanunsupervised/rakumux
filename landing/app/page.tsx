import CopyCommand from "@/components/CopyCommand";
import InstallTabs from "@/components/InstallTabs";
import TuiDemo from "@/components/TuiDemo";

const RELEASES = "https://github.com/humanunsupervised/rakumux/releases";
const NPM = "https://www.npmjs.com/package/rakumux";

const REASONS = [
  {
    tag: "rakumux-ptyd",
    title: "Sessions outlive the window",
    body: "A background server owns every session. Quit Rakumux, close the terminal or drop your SSH connection and your agents keep working. After a reboot, Enter brings a session back, resuming the agent's conversation when it knows how.",
  },
  {
    tag: "j / k",
    title: "See every agent at once",
    body: "Move through the list and each session plays live beside it. Which agent is working, which is waiting for you, which is done: it's on the screen, not buried in a tab you forgot about.",
  },
  {
    tag: "Ctrl-\\",
    title: "One key, no collisions",
    body: "Claude Code, Codex, OpenCode, Neovim and your shell keep every key but one. Ctrl-\\ is the key nothing else wants, so Rakumux never fights your tools for a shortcut.",
  },
  {
    tag: "rakumux-mcp",
    title: "Agents can drive it too",
    body: "An MCP server lets agents list folders, start sessions in their own folder, read another session's screen and rename sessions. Fan out a refactor, then check on it.",
  },
  {
    tag: "⎇ feat/auth",
    title: "Every folder, every worktree",
    body: "Each session lives in a folder: a repository, a worktree or anything else. See every folder's sessions at once or filter to a few, order and pin the ones that matter, and see each session's git branch as it changes.",
  },
  {
    tag: "rakumux ls",
    title: "Scriptable all the way down",
    body: "Everything on screen is a command too: new, ls, attach, rename, pin, kill. JSON output for scripts, and a token-protected local HTTP API on 127.0.0.1.",
  },
];

const KEYS: { keys: string[]; action: string }[] = [
  { keys: ["Ctrl", "\\"], action: "Switch between a session and Rakumux" },
  { keys: ["j", "k"], action: "Move (the session previews live)" },
  { keys: ["Enter"], action: "Go into the selected session" },
  { keys: ["Esc"], action: "Back to the session you came from" },
  { keys: ["Tab"], action: "Last session" },
  { keys: ["1–9"], action: "Straight into a session" },
  { keys: ["gg", "G"], action: "Top and bottom" },
  { keys: ["Ctrl-d", "Ctrl-u"], action: "Half a page" },
  { keys: ["n"], action: "New session" },
  { keys: ["r"], action: "Rename" },
  { keys: ["p"], action: "Pin" },
  { keys: ["dd"], action: "Delete" },
  { keys: ["J", "K"], action: "Move a session down or up" },
  { keys: ["P"], action: "Filter by folder" },
  { keys: ["/"], action: "Filter" },
  { keys: ["z"], action: "Full screen" },
  { keys: ["?", ","], action: "Help and Settings" },
];

function Keys({ keys }: { keys: string[] }) {
  return (
    <span className="keys">
      {keys.map((k) => (
        <kbd key={k}>{k}</kbd>
      ))}
    </span>
  );
}

export default function Home() {
  return (
    <>
      <header className="nav">
        <div className="container nav-inner">
          <a href="#top" className="brand" aria-label="Rakumux home">
            <span className="badge badge-rakumux">RAKUMUX</span>
          </a>
          <nav className="nav-links" aria-label="Sections">
            <a href="#why">Why</a>
            <a href="#modes">Modes</a>
            <a href="#agents">Agents</a>
            <a href="#keys">Keys</a>
            <a href="#install" className="nav-cta">
              Install
            </a>
          </nav>
        </div>
      </header>

      <main id="top">
        <section className="hero">
          <div className="container">
            <p className="eyebrow">
              <span className="pulse" aria-hidden="true" /> A terminal session manager for coding agents
            </p>
            <h1>
              Every coding agent.
              <br />
              <span className="accent">One terminal.</span>
            </h1>
            <p className="lede">
              Rakumux keeps Claude Code, Codex, OpenCode, your dev server and your shells running in the background,
              and puts them side by side, live, one key apart. Built in Rust, driven from the keyboard.
            </p>
            <div className="hero-actions">
              <CopyCommand command="npm install -g rakumux" />
              <a href="#install" className="link-arrow">
                Homebrew and downloads →
              </a>
            </div>
            <p className="hero-meta">macOS and Linux · no tmux, git or other tools needed</p>
          </div>
          <div className="container container-wide hero-demo">
            <TuiDemo />
          </div>
        </section>

        <section id="why" className="section">
          <div className="container">
            <p className="kicker">Why Rakumux</p>
            <h2>Stop losing agents in terminal tabs.</h2>
            <p className="section-lede">
              Running three agents, a dev server and a test watcher means a dozen tabs, and the one waiting for your
              answer is never the one you&apos;re looking at. Rakumux gives them one home.
            </p>
            <div className="grid">
              {REASONS.map((r) => (
                <article className="card" key={r.title}>
                  <code className="card-tag">{r.tag}</code>
                  <h3>{r.title}</h3>
                  <p>{r.body}</p>
                </article>
              ))}
            </div>
          </div>
        </section>

        <section id="modes" className="section section-alt">
          <div className="container split">
            <div>
              <p className="kicker">Two modes, one key</p>
              <h2>Your tools keep their keys.</h2>
              <p className="section-lede">
                Inside a session, every key goes to the program. Press <kbd>Ctrl</kbd> <kbd>\</kbd> and your keys drive
                Rakumux instead, with the Vim-style moves your fingers already know. The badge in the bottom-left
                corner always tells you which mode you&apos;re in.
              </p>
              <ul className="checks">
                <li>No prefix-key gymnastics: one key out, Enter or Esc back in.</li>
                <li>
                  <kbd>Option</kbd> <kbd>↑</kbd> <kbd>↓</kbd> cycle sessions, even from inside one if you turn it on.
                </li>
                <li>Settings has a Keys table that lists every action and runs the one you pick.</li>
              </ul>
            </div>
            <div className="modes" aria-label="The two modes">
              <div className="mode">
                <span className="badge badge-session">SESSION</span>
                <p>
                  <b>Work in the session.</b> Every key goes to Claude Code, Neovim, your shell. Rakumux stays out of
                  the way and blends into the background.
                </p>
              </div>
              <div className="mode-switch" aria-hidden="true">
                <span className="mode-arrow">
                  <Keys keys={["Ctrl", "\\"]} /> ↓
                </span>
                <span className="mode-arrow">
                  ↑ <Keys keys={["Enter"]} />
                </span>
              </div>
              <div className="mode mode-hot">
                <span className="badge badge-rakumux">RAKUMUX</span>
                <p>
                  <b>Drive Rakumux.</b> <kbd>j</kbd>/<kbd>k</kbd> to look around while sessions preview live,{" "}
                  <kbd>n</kbd> for a new one, <kbd>Esc</kbd> to go back where you were.
                </p>
              </div>
            </div>
          </div>
        </section>

        <section id="agents" className="section">
          <div className="container split split-reverse">
            <div>
              <p className="kicker">Built for agents</p>
              <h2>Let your agents start their own sessions.</h2>
              <p className="section-lede">
                Rakumux recognises common coding agents and shows whether each one is working, waiting for you or
                done. Its MCP server goes further: an agent running inside Rakumux can start sessions in its own
                folder, read another session&apos;s screen and report back.
              </p>
              <ul className="checks">
                <li>Works with Claude Code, Codex, OpenCode and any MCP client.</li>
                <li>Agents start sessions only in their own folder, a few at a time.</li>
                <li>The local API listens on 127.0.0.1 only and needs a token.</li>
              </ul>
            </div>
            <div className="terminal" aria-label="Terminal example">
              <div className="terminal-bar" aria-hidden="true">
                <span className="dot" />
                <span className="dot" />
                <span className="dot" />
              </div>
              <pre>
                <span className="t-muted"># Give Claude Code the Rakumux tools</span>
                {"\n"}
                <span className="t-orange">$</span> claude mcp add --scope user rakumux -- rakumux-mcp
                {"\n\n"}
                <span className="t-muted"># OpenCode 2.x: tool calls act as the session they run in</span>
                {"\n"}
                <span className="t-orange">$</span> rakumux-mcp opencode-plugin --install
                {"\n\n"}
                <span className="t-muted"># Or script it yourself</span>
                {"\n"}
                <span className="t-orange">$</span> rakumux new --name tests --detach -- npm test
                {"\n"}
                <span className="t-orange">$</span> rakumux ls
                {"\n"}
                <span className="t-dim">{"  "}NAME{"           "}STATUS{"   "}AGENT{"   "}GIT{"              "}ACTIVE</span>
                {"\n"}
                {"  "}auth-refactor{"  "}<span className="t-green">working</span>{"  "}claude{"  "}feat/auth{"        "}2m
                {"\n"}
                {"  "}flaky-tests{"    "}<span className="t-yellow">blocked</span>{"  "}codex{"   "}fix/flaky-tests{"  "}9m
                {"\n"}
                {"  "}tests{"          "}<span className="t-dim">running</span>{"  "}-{"       "}main{"             "}1m
              </pre>
            </div>
          </div>
        </section>

        <section id="keys" className="section section-alt">
          <div className="container">
            <p className="kicker">Keyboard first</p>
            <h2>The whole thing fits on one card.</h2>
            <p className="section-lede">
              These are Rakumux mode&apos;s keys. Forget one? <kbd>?</kbd> opens Help, and a one-minute tour shows on
              first launch.
            </p>
            <dl className="keygrid">
              {KEYS.map((k) => (
                <div className="keyrow" key={k.action}>
                  <dt>
                    <Keys keys={k.keys} />
                  </dt>
                  <dd>{k.action}</dd>
                </div>
              ))}
            </dl>
          </div>
        </section>

        <section id="install" className="section">
          <div className="container split">
            <div>
              <p className="kicker">Install</p>
              <h2>Up and running in a minute.</h2>
              <p className="section-lede">
                One package with everything in it: the app, its background server, the MCP server and the local API.
                Runs on macOS (Apple Silicon and Intel) and Linux x64, and needs nothing but bash.
              </p>
              <InstallTabs />
            </div>
            <div className="firstrun">
              <h3>Then, in any folder</h3>
              <ol className="steps">
                <li>
                  <span className="step-n">1</span>
                  <div>
                    <p>In a repository or any folder, open Rakumux.</p>
                    <CopyCommand command="rakumux" />
                  </div>
                </li>
                <li>
                  <span className="step-n">2</span>
                  <div>
                    <p>
                      Press <kbd>n</kbd>, give it a name (or don&apos;t), and run a command or an agent in its shell.
                    </p>
                  </div>
                </li>
                <li>
                  <span className="step-n">3</span>
                  <div>
                    <p>
                      Press <kbd>Ctrl</kbd> <kbd>\</kbd> to step out, <kbd>j</kbd> <kbd>k</kbd> to look around,{" "}
                      <kbd>Enter</kbd> to jump back in.
                    </p>
                  </div>
                </li>
              </ol>
            </div>
          </div>
        </section>

        <section className="cta">
          <div className="container cta-inner">
            <h2>
              Give your agents a <span className="accent">conductor</span>.
            </h2>
            <CopyCommand command="npm install -g rakumux" />
          </div>
        </section>
      </main>

      <footer className="footer">
        <div className="container footer-inner">
          <span className="badge badge-session">RAKUMUX</span>
          <span className="t-muted">© {new Date().getFullYear()} Human Unsupervised</span>
          <span className="tui-spacer" />
          <a href={RELEASES}>Releases</a>
          <a href={NPM}>npm</a>
          <a href="#install">Install</a>
        </div>
      </footer>
    </>
  );
}
