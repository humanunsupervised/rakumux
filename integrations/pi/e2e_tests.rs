//! End to end with the real Pi CLI (`pi` on `PATH`, 1.1 or later) in a
//! throwaway daemon, offline: Pi's agent directory and home in a temp dir,
//! its model a fake OpenAI-compatible server in this test, the extension
//! installed with the real `rakumux-mcp pi-plugin --install` and reporting
//! through the real `rakumux session hook`. Ignored by default (it needs Pi,
//! Node and the workspace's binaries, and takes about a minute):
//!
//! ```bash
//! cd tui && cargo build --workspace --locked
//! cargo test -p rakumux-ptyd --test pi_e2e --locked -- --ignored --nocapture
//! ```

use session_core::api::{
    CreateSession, ReadRequest, ReadSource, ReviveSession, SendRequest, TextFormat, TranscriptPart,
    TranscriptRole,
};
use session_core::client::{AuthMode, Client, ClientConfig, SessionApi};
use session_core::ids::SessionId;
use session_core::model::{Lifecycle, ResumeSource, ScopeRoot, SessionStatus, SessionView};
use session_core::paths::{Layout, Registration};
use std::collections::BTreeSet;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const PTYD: &str = env!("CARGO_BIN_EXE_rakumux-ptyd");
/// Pi starts in a few seconds; a turn against the fake model takes less.
const WAIT: Duration = Duration::from_secs(45);
const COLS: u16 = 100;
const ROWS: u16 = 30;
/// An extension that asks before a `bash` call whose command says GATED, as
/// a permission gate does.
const GATE: &str = r#"
export default function (pi) {
  pi.on("tool_call", async (event, ctx) => {
    if (event.toolName !== "bash" || !String(event.input.command).includes("GATED")) return undefined;
    const ok = await ctx.ui.confirm("Allow bash?", String(event.input.command));
    return ok ? undefined : { block: true, reason: "Blocked by user" };
  });
}
"#;

// -- The fake model -----------------------------------------------------------

/// A canned OpenAI-compatible `chat/completions` server, streaming. The newest
/// prompt scripts it: `SLOW <ms>` waits before each response (as a real model
/// takes its time, while Pi shows it working), `THINK` reasons first,
/// `WRITE <path>` calls `write`, `BASH <command>` calls `bash`, `MCP <tool>`
/// calls the Rakumux MCP server's tool; after a tool's result it says what it
/// did; anything else is echoed.
fn fake_model() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            std::thread::spawn(move || answer(stream));
        }
    });
    port
}

fn answer(mut stream: TcpStream) {
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut head = String::new();
    let mut length = 0;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap_or(0) == 0 {
            return;
        }
        if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
            length = value.trim().parse().unwrap_or(0);
        }
        if line == "\r\n" {
            break;
        }
        head.push_str(&line);
    }
    let mut body = vec![0; length];
    if reader.read_exact(&mut body).is_err() {
        return;
    }
    if !head.contains("/chat/completions") {
        let _ = stream
            .write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
        return;
    }
    let request: serde_json::Value = serde_json::from_slice(&body).unwrap_or_default();
    let messages = request["messages"].as_array().cloned().unwrap_or_default();
    let text = |message: &serde_json::Value| match &message["content"] {
        serde_json::Value::String(text) => text.clone(),
        serde_json::Value::Array(parts) => parts
            .iter()
            .filter_map(|part| part["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    };
    let prompt = messages
        .iter()
        .rev()
        .find(|message| message["role"] == "user")
        .map(text)
        .unwrap_or_default();
    let after_tool = messages.last().is_some_and(|m| m["role"] == "tool");
    let word_after = |marker: &str| {
        let at = prompt.find(marker)? + marker.len();
        Some(prompt[at..].lines().next().unwrap_or("").trim().to_owned())
    };
    let _ = stream.write_all(
        b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\nConnection: close\r\n\r\n",
    );
    let send = |stream: &mut TcpStream, delta: serde_json::Value, finish: Option<&str>| {
        let chunk = serde_json::json!({"id": "chatcmpl-fake", "object": "chat.completion.chunk",
            "created": 0, "model": "fake-model",
            "choices": [{"index": 0, "delta": delta, "finish_reason": finish}]});
        let _ = stream.write_all(format!("data: {chunk}\n\n").as_bytes());
        let _ = stream.flush();
    };
    if let Some(ms) = word_after("SLOW ").and_then(|w| w.split_whitespace().next()?.parse().ok()) {
        std::thread::sleep(Duration::from_millis(ms));
    }
    send(&mut stream, serde_json::json!({"role": "assistant"}), None);
    let write = word_after("WRITE ").map(|w| w.split_whitespace().next().unwrap_or("").to_owned());
    let bash = word_after("BASH ");
    // Pi offers a server's `direct` tools to the model as `mcp__<server>__<tool>`.
    let mcp = word_after("MCP ").map(|tool| format!("mcp__rakumux__{tool}"));
    let offered = |name: &str| {
        request["tools"]
            .as_array()
            .is_some_and(|tools| tools.iter().any(|t| t["function"]["name"] == name))
    };
    if after_tool {
        let said = match (&write, &mcp) {
            (Some(path), _) => format!("Wrote {path}."),
            (None, Some(_)) => "Listed them.".to_owned(),
            (None, None) => "Ran it.".to_owned(),
        };
        send(&mut stream, serde_json::json!({"content": said}), None);
        send(&mut stream, serde_json::json!({}), Some("stop"));
    } else {
        if prompt.contains("THINK") {
            send(
                &mut stream,
                serde_json::json!({"reasoning_content": "Let me think about it."}),
                None,
            );
        }
        let call = match (&write, &bash, &mcp) {
            (Some(path), _, _) => Some((
                "write",
                serde_json::json!({"path": path, "content": "hello\n"}),
            )),
            (None, Some(command), _) => Some(("bash", serde_json::json!({"command": command}))),
            (None, None, Some(tool)) if offered(tool) => {
                Some((tool.as_str(), serde_json::json!({})))
            }
            _ => None,
        };
        match call {
            Some((name, arguments)) => {
                send(&mut stream, serde_json::json!({"content": "On it."}), None);
                send(
                    &mut stream,
                    serde_json::json!({"tool_calls": [{"index": 0, "id": "call_1",
                        "type": "function",
                        "function": {"name": name, "arguments": arguments.to_string()}}]}),
                    None,
                );
                send(&mut stream, serde_json::json!({}), Some("tool_calls"));
            }
            None => {
                let first = match &mcp {
                    Some(tool) => format!("{tool} isn't offered"),
                    None => prompt.lines().next().unwrap_or("").to_owned(),
                };
                send(
                    &mut stream,
                    serde_json::json!({"content": format!("Echo: {first}")}),
                    None,
                );
                send(&mut stream, serde_json::json!({}), Some("stop"));
            }
        }
    }
    let usage = serde_json::json!({"id": "chatcmpl-fake", "object": "chat.completion.chunk",
        "created": 0, "model": "fake-model", "choices": [],
        "usage": {"prompt_tokens": 20, "completion_tokens": 10, "total_tokens": 30}});
    let _ = stream.write_all(format!("data: {usage}\n\ndata: [DONE]\n\n").as_bytes());
    let _ = stream.shutdown(std::net::Shutdown::Both);
}

// -- The world: Pi, its agent directory, a daemon ---------------------------

struct Daemon(Child);

/// The `rakumux-api` that `rakumux-mcp` starts detached in this home, if it
/// did: stopped with the world.
struct Api(PathBuf);

impl Drop for Api {
    fn drop(&mut self) {
        let pid = std::fs::read(self.0.join("api.json"))
            .ok()
            .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
            .and_then(|info| info["pid"].as_i64());
        if let Some(pid) = pid.and_then(|pid| i32::try_from(pid).ok()) {
            // SAFETY: kill has no memory-safety preconditions.
            unsafe { libc::kill(pid, libc::SIGTERM) };
        }
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

struct World {
    // Dropped in order: the API an MCP server started, the client, then the
    // daemon (killing every session), then the files.
    _api: Api,
    client: Client,
    _daemon: Daemon,
    layout: Layout,
    base: tempfile::TempDir,
    agent: PathBuf,
    repo: PathBuf,
    env: Vec<(String, String)>,
}

/// The binary `name` built next to the daemon.
fn sibling(name: &str) -> PathBuf {
    Path::new(PTYD).with_file_name(name)
}

/// Pi's version, when `pi` runs. Even `--version` takes a lock in Pi's agent
/// directory, so it runs in `home` and `agent`, never the user's.
fn pi_version(home: &Path, agent: &Path) -> Option<(u32, u32)> {
    let out = Command::new("pi")
        .arg("--version")
        .current_dir(home)
        .env("HOME", home)
        .env("PI_CODING_AGENT_DIR", agent)
        .env("PI_OFFLINE", "1")
        .stdin(Stdio::null())
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let mut parts = text.trim().split('.');
    Some((parts.next()?.parse().ok()?, parts.next()?.parse().ok()?))
}

/// A daemon in a temp home with an isolated Pi; `None` (skipped) without
/// Pi 1.1 or the workspace's binaries.
fn world() -> Option<World> {
    let base = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(base.path()).unwrap();
    let home = root.join("home");
    let agent = root.join("agent");
    let repo = root.join("repo");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&agent).unwrap();
    match pi_version(&home, &agent) {
        Some(version) if version >= (1, 1) => {}
        other => {
            eprintln!("skipped: needs pi 1.1 or later on PATH (found {other:?})");
            return None;
        }
    }
    for binary in ["tui", "rakumux-mcp"] {
        assert!(
            sibling(binary).is_file(),
            "{binary} isn't built: run `cargo build --workspace` first"
        );
    }
    std::fs::create_dir_all(repo.join(".git")).unwrap();
    std::fs::write(repo.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
    std::fs::create_dir_all(agent.join("extensions")).unwrap();
    let port = fake_model();
    std::fs::write(
        agent.join("models.json"),
        serde_json::json!({"providers": {"fake": {
            "baseUrl": format!("http://127.0.0.1:{port}/v1"), "api": "openai-completions",
            "apiKey": "fake", "models": [{"id": "fake-model", "reasoning": true}]}}})
        .to_string(),
    )
    .unwrap();
    std::fs::write(
        agent.join("settings.json"),
        r#"{"defaultProvider": "fake", "defaultModel": "fake-model", "quietStartup": true}"#,
    )
    .unwrap();
    std::fs::write(root.join("gate.ts"), GATE).unwrap();
    let installed = Command::new(sibling("rakumux-mcp"))
        .args(["pi-plugin", "--install", "--dir"])
        .arg(&agent)
        .output()
        .unwrap();
    assert!(installed.status.success(), "{installed:?}");
    assert!(agent.join("extensions/rakumux-session.ts").is_file());
    // Rakumux's MCP server, as its README says to add it to Pi.
    let added = Command::new("pi")
        .args(["mcp", "add", "rakumux", "--exposure", "direct", "--"])
        .arg(sibling("rakumux-mcp"))
        .current_dir(&home)
        .env("HOME", &home)
        .env("PI_CODING_AGENT_DIR", &agent)
        .env("PI_OFFLINE", "1")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(added.status.success(), "{added:?}");

    let layout = Layout::with_home(root.join("rakumux"));
    let path = std::env::var("PATH").unwrap_or_default();
    let env: Vec<(String, String)> = vec![
        ("PATH".into(), path.clone()),
        ("HOME".into(), home.display().to_string()),
        ("PI_CODING_AGENT_DIR".into(), agent.display().to_string()),
        ("PI_OFFLINE".into(), "1".into()),
        ("PI_TELEMETRY".into(), "0".into()),
        ("RAKUMUX_HOME".into(), layout.home.display().to_string()),
        ("TERM".into(), "xterm-256color".into()),
        ("LANG".into(), "C.UTF-8".into()),
        ("PS1".into(), "$ ".into()),
    ];
    let child = Command::new(PTYD)
        .arg("serve")
        .env_clear()
        .envs(env.iter().map(|(k, v)| (k, v)))
        .env(session_core::env::BIN, sibling("tui"))
        .env("RAKUMUX_PTYD_LOG", "debug")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let daemon = Daemon(child);
    let deadline = Instant::now() + WAIT;
    while !Registration::load(&layout).is_ok_and(|r| r.pid == daemon.0.id() && r.socket.exists()) {
        assert!(Instant::now() < deadline, "the daemon never registered");
        std::thread::sleep(Duration::from_millis(50));
    }
    let client = Client::connect(ClientConfig {
        layout: layout.clone(),
        client_name: "pi-e2e".into(),
        version: "test".into(),
        auth: AuthMode::User,
        spawn: false,
        ptyd_path: None,
    })
    .unwrap();
    Some(World {
        _api: Api(layout.home.clone()),
        client,
        _daemon: daemon,
        layout,
        base,
        agent,
        repo,
        env,
    })
}

impl World {
    /// A bash session in the repository that runs `command` (Pi) at start.
    fn session(&self, command: &str) -> SessionId {
        self.client
            .create(CreateSession {
                scope: Some(ScopeRoot::resolve(&self.repo).unwrap()),
                name: None,
                cwd: Some(self.repo.clone()),
                argv: Some(vec!["bash".into(), "--norc".into(), "--noprofile".into()]),
                startup_cmds: vec![command.into()],
                env: self.env.clone(),
                cols: COLS,
                rows: ROWS,
                machine: None,
                prefill: None,
            })
            .unwrap()
            .id
    }

    /// Poll `id`'s view until `check` holds, noting every lifecycle and
    /// activity phase seen meanwhile.
    fn wait(
        &self,
        id: &SessionId,
        what: &str,
        seen: &mut Seen,
        check: impl Fn(&SessionView) -> bool,
    ) -> SessionView {
        let deadline = Instant::now() + WAIT;
        loop {
            let view = self.client.get(id).unwrap();
            seen.note(&view);
            if check(&view) {
                return view;
            }
            if Instant::now() >= deadline {
                let screen = self.screen(id);
                let log = std::fs::read_to_string(self.layout.home.join("logs/ptyd.log"))
                    .unwrap_or_default();
                let log: Vec<&str> = log.lines().rev().take(60).collect();
                let log: Vec<&str> = log.into_iter().rev().collect();
                panic!(
                    "never {what}; last view: {view:#?}\nscreen:\n{screen}\ndaemon log:\n{}",
                    log.join("\n")
                );
            }
            std::thread::sleep(Duration::from_millis(40));
        }
    }

    fn screen(&self, id: &SessionId) -> String {
        self.client
            .read(ReadRequest {
                id: id.clone(),
                source: ReadSource::Visible,
                lines: None,
                format: TextFormat::Text,
            })
            .map(|read| read.text)
            .unwrap_or_default()
    }

    /// Type a prompt as a feed reply is typed in: a paste, then Enter, only
    /// while Pi is in the foreground.
    fn submit(&self, id: &SessionId, text: &str) {
        self.client
            .submit_expecting(id, text, Duration::from_millis(0), Some("pi"))
            .unwrap();
    }

    /// The Pi conversation files under the agent directory.
    fn conversation_files(&self) -> Vec<PathBuf> {
        let mut files = Vec::new();
        for folder in std::fs::read_dir(self.agent.join("sessions"))
            .into_iter()
            .flatten()
            .flatten()
        {
            for file in std::fs::read_dir(folder.path())
                .into_iter()
                .flatten()
                .flatten()
            {
                files.push(file.path());
            }
        }
        files.sort();
        files
    }
}

/// What a session went through while it was watched.
#[derive(Default)]
struct Seen {
    lifecycles: BTreeSet<String>,
    phases: BTreeSet<String>,
}

impl Seen {
    fn note(&mut self, view: &SessionView) {
        self.lifecycles.insert(format!("{:?}", view.lifecycle));
        if let Some(activity) = &view.activity {
            self.phases.insert(format!("{:?}", activity.phase));
        }
    }
}

fn is_pi(view: &SessionView) -> bool {
    view.agent.as_ref().is_some_and(|agent| agent.kind == "pi")
        && view
            .terminal
            .as_ref()
            .and_then(|t| t.foreground_process.as_deref())
            == Some("pi")
}

fn conversation(view: &SessionView) -> Option<String> {
    view.agent_turn.as_ref()?.conversation_id.clone()
}

fn parts(page: &session_core::api::TranscriptPage) -> Vec<(TranscriptRole, String)> {
    page.messages
        .iter()
        .flat_map(|message| {
            message.parts.iter().map(|part| {
                let shown = match part {
                    TranscriptPart::Text { text, .. } => format!("text {text}"),
                    TranscriptPart::Reasoning { text, .. } => format!("reasoning {text}"),
                    TranscriptPart::ToolCall { name, .. } => format!("call {name}"),
                    TranscriptPart::ToolResult { error, .. } => format!("result error={error}"),
                    _ => "other".into(),
                };
                (message.role, shown)
            })
        })
        .collect()
}

#[test]
#[ignore = "needs pi 1.1+, node and `cargo build --workspace`; run with --ignored"]
fn pi_with_the_rakumux_extension() {
    let Some(world) = world() else {
        return;
    };
    let client = &world.client;
    client.set_reply_capture(true).unwrap();
    // Our extension is found in the agent directory; the gate is explicit.
    let gate = world.base.path().join("gate.ts");
    let id = world.session(&format!("pi -e {}", gate.display()));
    let mut seen = Seen::default();

    // Detected, and its extension reports at once.
    let view = world.wait(
        &id,
        "Pi running, its extension reporting",
        &mut seen,
        |view| is_pi(view) && view.activity.is_some() && view.lifecycle == Lifecycle::Idle,
    );
    assert_eq!(view.status, SessionStatus::Live);

    // A turn: it waits for the model, works, runs a tool and finishes.
    world.submit(&id, "SLOW 1500 THINK WRITE notes/a.txt");
    world.wait(&id, "the first turn done", &mut seen, |view| {
        view.lifecycle == Lifecycle::Done
    });
    assert!(seen.lifecycles.contains("Working"), "{:?}", seen.lifecycles);
    for phase in ["WaitingForResponse", "Done"] {
        assert!(seen.phases.contains(phase), "{phase}: {:?}", seen.phases);
    }
    assert!(world.repo.join("notes/a.txt").is_file(), "Pi ran the write");

    // Its conversation: one file, named by the id the turn carries.
    let files = world.conversation_files();
    assert_eq!(files.len(), 1, "{files:?}");
    let file = files[0].clone();
    let id_of_file = file
        .file_stem()
        .unwrap()
        .to_str()
        .unwrap()
        .split_once('_')
        .unwrap()
        .1
        .to_owned();
    let view = world.wait(&id, "the turn read from the log", &mut seen, |view| {
        view.agent_output.as_deref() == Some("Wrote notes/a.txt.")
    });
    assert_eq!(
        view.user_prompt.as_deref(),
        Some("SLOW 1500 THINK WRITE notes/a.txt")
    );
    assert_eq!(conversation(&view).as_deref(), Some(id_of_file.as_str()));
    let turn = client.turn(&id).unwrap();
    assert_eq!(
        turn.prompt.as_deref(),
        Some("SLOW 1500 THINK WRITE notes/a.txt")
    );
    assert_eq!(
        turn.messages,
        [
            "Let me think about it.",
            "On it.",
            "write: notes/a.txt",
            "Wrote notes/a.txt."
        ]
    );

    // Its reply, captured for the feed.
    let deadline = Instant::now() + WAIT;
    let reply = loop {
        let (replies, _) = client.replies(None).unwrap();
        if let Some(reply) = replies.into_iter().find(|r| r.session_id == id) {
            break reply;
        }
        assert!(Instant::now() < deadline, "no reply was captured");
        std::thread::sleep(Duration::from_millis(100));
    };
    assert_eq!(reply.agent, "pi");
    assert_eq!(reply.text, "Wrote notes/a.txt.");
    assert_eq!(reply.conversation_id.as_deref(), Some(id_of_file.as_str()));
    assert!(reply.position.is_some());

    // The whole conversation.
    let page = client.transcript(&id, None, None).unwrap();
    assert_eq!(page.agent, "pi");
    assert_eq!(page.conversation_id, id_of_file);
    assert_eq!(
        parts(&page),
        [
            (
                TranscriptRole::User,
                "text SLOW 1500 THINK WRITE notes/a.txt".into()
            ),
            (
                TranscriptRole::Assistant,
                "reasoning Let me think about it.".into()
            ),
            (TranscriptRole::Assistant, "text On it.".into()),
            (TranscriptRole::Assistant, "call write".into()),
            (TranscriptRole::Tool, "result error=false".into()),
            (TranscriptRole::Assistant, "text Wrote notes/a.txt.".into()),
        ]
    );

    // Where it changed files: the repository.
    let view = world.wait(&id, "the repository listed as a place", &mut seen, |view| {
        view.places
            .iter()
            .any(|place| place.path == world.repo && place.edited)
    });
    assert_eq!(view.places.len(), 1, "{:?}", view.places);

    // An extension's dialog waits for the user.
    world.submit(&id, "BASH echo GATED");
    world.wait(&id, "blocked on the dialog", &mut seen, |view| {
        view.lifecycle == Lifecycle::Blocked
    });
    assert!(seen.phases.contains("NeedsInput"), "{:?}", seen.phases);
    client
        .send(SendRequest {
            id: id.clone(),
            keys: vec!["enter".into()],
            text: None,
        })
        .unwrap();
    world.wait(&id, "the gated turn done", &mut seen, |view| {
        view.lifecycle == Lifecycle::Done && view.agent_output.as_deref() == Some("Ran it.")
    });

    // A reply written back, typed in: it shows up as the next prompt.
    let before = client.get(&id).unwrap().agent_turn.unwrap().id;
    world.submit(&id, "Thanks, that works");
    let view = world.wait(&id, "the reply typed in as a new turn", &mut seen, |view| {
        view.user_prompt.as_deref() == Some("Thanks, that works")
            && view
                .agent_turn
                .as_ref()
                .is_some_and(|turn| turn.id != before)
            && view.agent_output.as_deref() == Some("Echo: Thanks, that works")
    });
    assert_eq!(conversation(&view).as_deref(), Some(id_of_file.as_str()));

    // Rakumux's MCP server, run by Pi with the session's environment: it acts
    // as this session, which sees its own folder only.
    let elsewhere = world.base.path().join("elsewhere");
    std::fs::create_dir_all(&elsewhere).unwrap();
    let other = client
        .create(CreateSession {
            scope: Some(ScopeRoot::resolve(&elsewhere).unwrap()),
            name: Some("other-folder".into()),
            cwd: None,
            argv: Some(vec!["bash".into(), "--norc".into(), "--noprofile".into()]),
            startup_cmds: Vec::new(),
            env: world.env.clone(),
            cols: COLS,
            rows: ROWS,
            machine: None,
            prefill: None,
        })
        .unwrap();
    world.submit(&id, "MCP list_sessions");
    world.wait(&id, "the MCP tool called", &mut seen, |view| {
        view.agent_output.as_deref() == Some("Listed them.")
    });
    let page = client.transcript(&id, None, None).unwrap();
    let (listed, failed) = page
        .messages
        .iter()
        .rev()
        .flat_map(|message| &message.parts)
        .find_map(|part| match part {
            TranscriptPart::ToolResult { output, error, .. } => Some((output.clone(), *error)),
            _ => None,
        })
        .unwrap();
    assert!(!failed, "{listed}");
    let me = format!("({id})");
    let line = listed
        .lines()
        .find(|line| line.contains(&me))
        .unwrap_or_else(|| panic!("{listed}"));
    assert!(line.contains("this is you"), "{line}");
    assert!(!listed.contains(other.id.as_str()), "{listed}");
    client.kill(&other.id, true).unwrap();

    // Resumed after a kill: the same conversation, from its file.
    let view = world.wait(&id, "a resume command recorded", &mut seen, |view| {
        view.resume.as_ref().is_some_and(|resume| {
            resume.source == ResumeSource::KnownAgent
                && resume.argv == ["pi", "--session", file.to_str().unwrap()]
        })
    });
    let pid = view.terminal.unwrap().pid;
    client.kill(&id, true).unwrap();
    world.wait(&id, "the session ended", &mut seen, |view| {
        view.status != SessionStatus::Live
    });
    client
        .revive(ReviveSession {
            id: id.clone(),
            env: world.env.clone(),
            cols: COLS,
            rows: ROWS,
            resume: true,
        })
        .unwrap();
    world.wait(&id, "Pi back in its conversation", &mut seen, |view| {
        is_pi(view)
            && view.terminal.as_ref().is_some_and(|t| t.pid != pid)
            && conversation(view).as_deref() == Some(id_of_file.as_str())
    });
    world.submit(&id, "After the revive");
    world.wait(
        &id,
        "a turn in the resumed conversation",
        &mut seen,
        |view| view.agent_output.as_deref() == Some("Echo: After the revive"),
    );
    assert_eq!(
        world.conversation_files(),
        [file.clone()],
        "no new conversation"
    );
    let resumed = std::fs::read_to_string(&file).unwrap();
    assert!(resumed.contains("Thanks, that works") && resumed.contains("After the revive"));
}

#[test]
#[ignore = "needs pi 1.1+, node and `cargo build --workspace`; run with --ignored"]
fn pi_alone_reports_through_osc_7501() {
    let Some(world) = world() else {
        return;
    };
    let client = &world.client;
    client.set_reply_capture(true).unwrap();
    // No extension of ours: only the gate, loaded explicitly.
    let gate = world.base.path().join("gate.ts");
    let id = world.session(&format!("pi --no-extensions -e {}", gate.display()));
    let mut seen = Seen::default();
    world.wait(&id, "Pi running", &mut seen, |view| {
        is_pi(view) && view.lifecycle == Lifecycle::Idle
    });

    // Its own state: working, then a finished reply.
    world.submit(&id, "SLOW 1500 hello there");
    let view = world.wait(&id, "the turn done", &mut seen, |view| {
        view.lifecycle == Lifecycle::Done
            && view.agent_output.as_deref() == Some("Echo: SLOW 1500 hello there")
    });
    assert!(seen.lifecycles.contains("Working"), "{:?}", seen.lifecycles);
    // Unbound, the folder's newest conversation is followed.
    let files = world.conversation_files();
    assert_eq!(files.len(), 1, "{files:?}");
    let name = files[0].file_name().unwrap().to_str().unwrap().to_owned();
    let conversation_id = conversation(&view).expect("a conversation");
    assert!(
        name.ends_with(&format!("_{conversation_id}.jsonl")),
        "{name}"
    );
    let deadline = Instant::now() + WAIT;
    while !client
        .replies(None)
        .unwrap()
        .0
        .iter()
        .any(|reply| reply.text == "Echo: SLOW 1500 hello there")
    {
        assert!(Instant::now() < deadline, "no reply was captured");
        std::thread::sleep(Duration::from_millis(100));
    }

    // Blocked on a dialog, as Pi reports it.
    world.submit(&id, "BASH echo GATED");
    world.wait(&id, "blocked on the dialog", &mut seen, |view| {
        view.lifecycle == Lifecycle::Blocked
    });
    client
        .send(SendRequest {
            id: id.clone(),
            keys: vec!["enter".into()],
            text: None,
        })
        .unwrap();
    world.wait(&id, "the gated turn done", &mut seen, |view| {
        view.lifecycle == Lifecycle::Done && view.agent_output.as_deref() == Some("Ran it.")
    });

    // Resumable from the folder's newest file, which Pi wrote only at the
    // first prompt: found though the screen has been quiet since.
    world.wait(&id, "a resume command recorded", &mut seen, |view| {
        view.resume.as_ref().is_some_and(|resume| {
            resume.source == ResumeSource::KnownAgent
                && resume.argv == ["pi", "--session", files[0].to_str().unwrap()]
        })
    });
}
