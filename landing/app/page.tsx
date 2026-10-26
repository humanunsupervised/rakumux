import CopyCommand from "@/components/CopyCommand";
import TuiDemo from "@/components/TuiDemo";

const GITHUB = "https://github.com/humanunsupervised/rakumux";
const RELEASES = `${GITHUB}/releases`;

const REASONS = [
  {
    tag: "claude · codex · opencode",
    title: "Bring your own subscription",
    body: "Your Claude Code, Codex and OpenCode plans, with every native feature. No extra model costs.",
  },
  {
    tag: "≡ Feed",
    title: "Never let an agent sit idle",
    body: "Every finished turn lands in your feed. Answer from any browser and it's typed straight in.",
  },
  {
    tag: "◔ Status",
    title: "Find the right session fast",
    body: "Group by status, folder or app. Pin what matters. The agent waiting on you is always on top.",
  },
  {
    tag: "ssh",
    title: "Built for remote",
    body: "Lives in your terminal. Run it over SSH, close your laptop, and your agents keep going.",
  },
  {
    tag: "rakumux.com",
    title: "Web and phone, no VPN",
    body: "Check in from your desktop or phone with nothing to set up. Tailscale optional. iPhone and Android apps soon.",
  },
  {
    tag: "rakumux-mcp",
    title: "Serious management tools",
    body: "Restart without stopping an agent. Sessions survive reboots. Agents launch agents over MCP.",
  },
];

const PLATFORMS: {
  tag: string;
  title: string;
  body: string;
  available: boolean;
  link?: { href: string; label: string };
}[] = [
  {
    tag: "macOS · Linux",
    title: "Terminal app",
    body: "The full app, free and open source. Run it on your machine or over SSH.",
    available: true,
    link: { href: "#install", label: "Install" },
  },
  {
    tag: "rakumux.com",
    title: "Web app",
    body: "Your feed and sessions in any browser, desktop or mobile. Free with an account.",
    available: true,
    link: { href: "https://rakumux.com/feed", label: "Open the web app" },
  },
  {
    tag: "iOS · Android",
    title: "iPhone and Android",
    body: "Native apps are on the way. Until then, the web app works on your phone.",
    available: false,
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
  { keys: ["d", "dd"], action: "Delete" },
  { keys: ["J", "K"], action: "Move a session down or up" },
  { keys: ["P"], action: "Filter by folder" },
  { keys: ["/"], action: "Filter" },
  { keys: ["z"], action: "Full screen" },
  { keys: ["?", ","], action: "Help and Settings" },
  { keys: ["R"], action: "Restart the UI (asks first; sessions keep running)" },
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

/** GitHub's mark, 20px, in the text colour. */
function GitHubMark() {
  return (
    <svg width="20" height="20" viewBox="0 0 24 24" fill="currentColor" aria-hidden="true" focusable="false">
      <path d="M12 .297C5.37.297 0 5.67 0 12.297c0 5.303 3.438 9.8 8.205 11.385.6.113.82-.258.82-.577 0-.285-.01-1.04-.015-2.04-3.338.724-4.043-1.61-4.043-1.61-.546-1.387-1.333-1.756-1.333-1.756-1.09-.745.083-.729.083-.729 1.205.084 1.838 1.236 1.838 1.236 1.07 1.835 2.809 1.305 3.495.998.108-.776.418-1.305.762-1.605-2.665-.305-5.467-1.334-5.467-5.93 0-1.31.469-2.38 1.236-3.22-.124-.303-.536-1.524.117-3.176 0 0 1.008-.322 3.301 1.23a11.52 11.52 0 0 1 3.003-.404c1.02.005 2.045.138 3.003.404 2.291-1.552 3.297-1.23 3.297-1.23.655 1.652.243 2.873.12 3.176.77.84 1.235 1.91 1.235 3.22 0 4.61-2.807 5.622-5.479 5.92.43.372.823 1.102.823 2.222 0 1.606-.015 2.898-.015 3.293 0 .322.216.694.825.576C20.565 22.092 24 17.597 24 12.297c0-6.627-5.373-12-12-12" />
    </svg>
  );
}

export default function Home() {
  return (
    <>
      <header className="nav">
        <div className="container nav-inner">
          <a href="#top" className="brand" aria-label="Rakumux home">
            <img src="/icon.svg" alt="" width={28} height={28} className="brand-icon" />
            <span className="brand-name">Rakumux</span>
          </a>
          <nav className="nav-links" aria-label="Rakumux">
            <a
              href={GITHUB}
              className="nav-github"
              aria-label="GitHub (opens in a new tab)"
              target="_blank"
              rel="noopener noreferrer"
            >
              <GitHubMark /> GitHub
            </a>
            <a href="https://rakumux.com/pricing">Pricing</a>
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
              <span className="pulse" aria-hidden="true" /> A terminal multiplexer for coding agents
            </p>
            <h1>
              Every coding agent.
              <br />
              <span className="accent">One terminal.</span>
            </h1>
            <p className="lede">
              Run Claude Code, Codex and OpenCode side by side, on your own subscriptions. See which one needs you.
              Answer it from anywhere.
            </p>
            <div className="hero-actions">
              <CopyCommand command="curl -fsSL https://rakumux.com/install.sh | bash" />
              <a href="#install" className="link-arrow">
                How it installs →
              </a>
            </div>
            <p className="hero-meta">
              <a href={GITHUB} target="_blank" rel="noopener noreferrer">
                Free and open source
              </a>{" "}
              · macOS + Linux · no tmux or VPN needed
            </p>
          </div>
          <div className="container container-wide hero-demo">
            <TuiDemo />
          </div>
        </section>

        <section id="why" className="section">
          <div className="container">
            <p className="kicker">Why Rakumux</p>
            <h2>A multiplexer for your agents.</h2>
            <p className="section-lede">
              Your agents, exactly as they are. Always working, always in reach.
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

        <section id="agent-feed" className="section section-alt">
          <div className="container split">
            <div>
              <p className="kicker">Agent feed</p>
              <h2>Keep every agent at 100%.</h2>
              <p className="section-lede">
                Every turn from every agent on every machine, in one feed. Reply from your desktop or phone, and
                Rakumux types it in the moment the agent is ready. No VPN, no open ports.
              </p>
              <ul className="checks">
                <li>Every session on every machine, live.</li>
                <li>Message any session, even a brand-new one.</li>
                <li>Tailscale optional. iPhone and Android apps coming soon.</li>
                <li>Off until you turn it on. Private sessions never leave your machine.</li>
              </ul>
            </div>
            <div className="feedmock" aria-label="The Agent feed on rakumux.com">
              <article className="feed-card">
                <header className="feed-card-top">
                  <h3 className="feed-card-title">
                    <span className="feed-card-name">auth-refactor</span> <span className="feed-card-sub">Claude Code · feat/auth</span>
                  </h3>
                  <p className="feed-card-meta">
                    <span>2m</span>
                    <span className="feed-card-status" data-status="done"><span className="feed-card-mark" aria-hidden="true">●</span> done</span>
                  </p>
                </header>
                <div className="feed-items">
                  <div className="feed-prompt">Move the session checks into middleware and add tests</div>
                  <div className="feed-text">
                    <p>
                      Moved the checks into a <code>requireSession()</code> middleware and added 23 tests. All of them
                      pass.
                    </p>
                  </div>
                  <div className="feedmock-reply" aria-hidden="true">
                    <span>Write back to Claude Code…</span>
                    <span className="feedmock-send">Send</span>
                  </div>
                </div>
              </article>
              <article className="feed-card">
                <header className="feed-card-top">
                  <h3 className="feed-card-title">
                    <span className="feed-card-name">flaky-tests</span> <span className="feed-card-sub">Codex · fix/flaky</span>
                  </h3>
                  <p className="feed-card-meta">
                    <span>9m</span>
                    <span className="feed-card-status" data-status="sent"><span className="feed-card-mark" aria-hidden="true">●</span> sent</span>
                  </p>
                </header>
                <div className="feed-items">
                  <div className="feed-prompt">Why do the queue tests fail on CI?</div>
                  <div className="feed-text">
                    <p>
                      All three share a timer race in <code>queue.spec.ts</code>. Swapping <code>flush()</code> for{" "}
                      <code>vi.runAllTimersAsync()</code> fixes it.
                    </p>
                  </div>
                  <div className="feed-reply-sent">
                    <div className="feed-prompt">Apply it and rerun the suite.</div>
                    <p className="feed-reply-status" data-tone="delivered">from your phone · Sent</p>
                  </div>
                </div>
              </article>
            </div>
          </div>
        </section>

        <section id="modes" className="section">
          <div className="container split">
            <div>
              <p className="kicker">Two modes, one key</p>
              <h2>Your agent&apos;s native keys.</h2>
              <p className="section-lede">
                Every key goes to your agent. Press <kbd>Ctrl</kbd> <kbd>\</kbd> to drive Rakumux, <kbd>Enter</kbd> to
                jump back.
              </p>
              <ul className="checks">
                <li>One key out, one key back. No prefix gymnastics.</li>
                <li>
                  <kbd>Option</kbd> <kbd>↑</kbd> <kbd>↓</kbd> to cycle sessions, even from inside one.
                </li>
              </ul>
            </div>
            <div className="modes" aria-label="The two modes">
              <div className="mode">
                <span className="badge badge-session">SESSION</span>
                <p>
                  <b>Work in the session.</b> Every key goes to your agent.
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
                  <b>Drive Rakumux.</b> <kbd>j</kbd>/<kbd>k</kbd> to look around, <kbd>n</kbd> for new, <kbd>Esc</kbd> to
                  go back.
                </p>
              </div>
            </div>
          </div>
        </section>

        <section id="agents" className="section section-alt">
          <div className="container split split-reverse">
            <div>
              <p className="kicker">Built for agents</p>
              <h2>Know when each agent needs you.</h2>
              <p className="section-lede">
                Working, waiting for you, or done: Rakumux tracks every agent, so you answer the right one first.
              </p>
              <ul className="checks">
                <li>Claude Code, Codex and OpenCode as they are, plus any other program.</li>
                <li>Agents start their own sessions over MCP.</li>
                <li>A local, token-protected API for your scripts.</li>
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

        <section id="keys" className="section">
          <div className="container">
            <p className="kicker">Key bindings</p>
            <h2>Use your coding agent natively.</h2>
            <p className="section-lede">
              Your agent keeps every shortcut. Rakumux&apos;s own keys fit on one card. <kbd>?</kbd> opens Help.
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

        <section id="platforms" className="section section-alt">
          <div className="container">
            <p className="kicker">Platforms</p>
            <h2>Everywhere you work.</h2>
            <div className="grid">
              {PLATFORMS.map((p) => (
                <article className="card" key={p.title}>
                  <div className="platform-head">
                    <code className="card-tag">{p.tag}</code>
                    <span className={p.available ? "platform-status is-available" : "platform-status"}>
                      {p.available ? "Available" : "Coming soon"}
                    </span>
                  </div>
                  <h3>{p.title}</h3>
                  <p>{p.body}</p>
                  {p.link && (
                    <a href={p.link.href} className="link-arrow platform-link">
                      {p.link.label} →
                    </a>
                  )}
                </article>
              ))}
            </div>
          </div>
        </section>

        <section id="install" className="section">
          <div className="container split">
            <div>
              <p className="kicker">Install</p>
              <h2>Free. Open source. Running in a minute.</h2>
              <p className="section-lede">
                The whole app is Apache-2.0 on GitHub. No account, no card. One command on macOS or Linux.
              </p>
              <div className="install">
                <CopyCommand command="curl -fsSL https://rakumux.com/install.sh | bash" />
                <p className="install-note">
                  Downloads the latest release from GitHub, verifies its checksum and installs to{" "}
                  <code>/usr/local</code>. Another prefix:{" "}
                  <code>curl -fsSL https://rakumux.com/install.sh | PREFIX=&quot;$HOME/.local&quot; bash</code>. Read the{" "}
                  <a href="/install.sh">script</a> first if you like.
                </p>
              </div>
              <a href={GITHUB} className="source-link" target="_blank" rel="noopener noreferrer">
                <GitHubMark /> Read the source on GitHub
              </a>
            </div>
            <div className="firstrun">
              <h3>Then, in any folder</h3>
              <ol className="steps">
                <li>
                  <span className="step-n">1</span>
                  <div>
                    <p>Open Rakumux.</p>
                    <CopyCommand command="rakumux" />
                  </div>
                </li>
                <li>
                  <span className="step-n">2</span>
                  <div>
                    <p>
                      Press <kbd>n</kbd> and start an agent: <code>claude</code>, <code>codex</code> or{" "}
                      <code>opencode</code>.
                    </p>
                  </div>
                </li>
                <li>
                  <span className="step-n">3</span>
                  <div>
                    <p>
                      <kbd>Ctrl</kbd> <kbd>\</kbd> to step out, <kbd>j</kbd> <kbd>k</kbd> to look around,{" "}
                      <kbd>Enter</kbd> to jump back.
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
            <CopyCommand command="curl -fsSL https://rakumux.com/install.sh | bash" />
          </div>
        </section>
      </main>

      <footer className="footer">
        <div className="container footer-inner">
          <span className="badge badge-session">RAKUMUX</span>
          <span className="t-muted">© {new Date().getFullYear()} Human Unsupervised</span>
          <span className="tui-spacer" />
          <a href={GITHUB} target="_blank" rel="noopener noreferrer">Source · Apache-2.0</a>
          <a href={RELEASES}>Releases</a>
          <a href="#install">Install</a>
        </div>
      </footer>
    </>
  );
}
