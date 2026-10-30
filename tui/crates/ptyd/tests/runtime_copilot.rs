//! GitHub Copilot CLI end to end: the real `copilot` (npm or native, from
//! `PATH`) in a session of the real daemon, offline (`COPILOT_OFFLINE`)
//! against a scripted model this test serves on 127.0.0.1 (Copilot's own
//! provider support, no account), with Rakumux's hooks installed by `rakumux
//! session hook-config --agent copilot --install`. Everything lives in a temp
//! home. Ignored by default: it needs Copilot CLI and the workspace's `tui`
//! binary, and takes a minute.
//!
//! ```text
//! cargo build --workspace
//! cargo test -p rakumux-ptyd --test runtime_copilot -- --ignored --nocapture
//! ```
//!
//! `COPILOT_CACHE_HOME` is passed on when set, so the native binary's
//! package (about 240 MB) isn't unpacked again each run.

mod runtime_support;

use runtime_support::{wait_until, TestHome};
use serde_json::{json, Value};
use session_core::api::{
    CreateSession, ErrorCode, ReadRequest, ReadSource, ReviveSession, SendRequest, TextFormat,
    TranscriptPart, TranscriptRole,
};
use session_core::client::{AuthMode, Client, ClientConfig, SessionApi};
use session_core::ids::SessionId;
use session_core::model::{Lifecycle, ResumeSource, ScopeRoot, SessionStatus, SessionView};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

const EXE: &str = env!("CARGO_BIN_EXE_rakumux-ptyd");
const SLOW: Duration = Duration::from_secs(45);
/// A turn the scripted model holds a while, and replies typed during turns.
const HOLD: &str = "slow 12 hold the turn while a reply comes";
const MIDTURN: &str = "MIDTURN ping 4242 typed while it works";
const STEER: &str = "MIDTURN pong 777 typed while a tool runs";

// ------------------------------------------------------------ the model

/// Serve the scripted model; returns its port. What the last prompt says
/// decides: `create <path>` and `run <command>` call those tools, `slow N`
/// waits N seconds first; after a tool's result it answers.
fn start_model() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            std::thread::spawn(move || serve(stream));
        }
    });
    port
}

fn serve(stream: TcpStream) {
    let Ok(mut out) = stream.try_clone() else {
        return;
    };
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    if reader.read_line(&mut line).is_err() {
        return;
    }
    let get = line.starts_with("GET ");
    let mut length = 0;
    loop {
        let mut header = String::new();
        if reader.read_line(&mut header).unwrap_or(0) == 0 || header == "\r\n" {
            break;
        }
        if let Some((name, value)) = header.split_once(':') {
            if name.eq_ignore_ascii_case("content-length") {
                length = value.trim().parse().unwrap_or(0);
            }
        }
    }
    let mut body = vec![0; length];
    if reader.read_exact(&mut body).is_err() {
        return;
    }
    if get {
        let models = json!({"object": "list", "data": [{"id": "mock-model", "object": "model"}]});
        let text = models.to_string();
        let _ = write!(
            out,
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{text}",
            text.len()
        );
        return;
    }
    let request: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let messages = request["messages"].as_array().cloned().unwrap_or_default();
    let tools: Vec<&str> = request["tools"]
        .as_array()
        .map(|tools| {
            tools
                .iter()
                .filter_map(|tool| tool["function"]["name"].as_str())
                .collect()
        })
        .unwrap_or_default();
    let last_role = messages
        .last()
        .and_then(|message| message["role"].as_str())
        .unwrap_or("");
    let prompt = messages
        .iter()
        .rev()
        .find(|message| message["role"] == "user")
        .map(content)
        .unwrap_or_default();
    let typed = typed(&prompt);
    let words: Vec<&str> = typed.split_whitespace().collect();
    let after = |word: &str| {
        let at = words.iter().position(|w| *w == word)?;
        (at + 1 < words.len()).then(|| words[at + 1..].join(" "))
    };
    if last_role == "user" {
        if let Some(seconds) = after("slow").and_then(|rest| rest.split(' ').next()?.parse().ok()) {
            std::thread::sleep(Duration::from_secs(seconds));
        }
    }
    let _ = write!(
        out,
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\nConnection: close\r\n\r\n"
    );
    let mut send = |delta: Value, finish: Option<&str>| {
        let chunk = json!({"id": "chatcmpl-mock", "object": "chat.completion.chunk", "created": 0,
            "model": "mock-model", "choices": [{"index": 0, "delta": delta, "finish_reason": finish}]});
        let _ = write!(out, "data: {chunk}\n\n");
        let _ = out.flush();
    };
    let call = |send: &mut dyn FnMut(Value, Option<&str>), name: &str, arguments: Value| {
        send(
            json!({"role": "assistant", "content": null, "tool_calls": [{"index": 0,
                "id": format!("call_{}", std::process::id()), "type": "function",
                "function": {"name": name, "arguments": arguments.to_string()}}]}),
            None,
        );
        send(json!({}), Some("tool_calls"));
    };
    if last_role == "tool" {
        // A model takes a moment: Copilot shows its turn working meanwhile.
        std::thread::sleep(Duration::from_millis(1500));
        send(
            json!({"role": "assistant", "content": "Done: the tool ran."}),
            None,
        );
        send(json!({}), Some("stop"));
    } else if let Some(path) = after("create").filter(|_| tools.contains(&"create")) {
        call(
            &mut send,
            "create",
            json!({"path": path, "file_text": "made by the scripted model\n"}),
        );
    } else if let Some(command) = after("run").filter(|_| tools.contains(&"bash")) {
        call(
            &mut send,
            "bash",
            json!({"command": command, "description": "Run it", "mode": "sync", "initial_wait": 10}),
        );
    } else {
        let echo = words.iter().take(6).copied().collect::<Vec<_>>().join(" ");
        send(json!({"role": "assistant", "content": ""}), None);
        send(json!({"content": format!("Mock reply: {echo}")}), None);
        send(json!({}), Some("stop"));
    }
    let _ = write!(out, "data: [DONE]\n\n");
}

/// A message's text: a string, or its text parts.
fn content(message: &Value) -> String {
    match &message["content"] {
        Value::String(text) => text.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter_map(|part| part["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

/// What was typed, without Copilot's own `<tag>…</tag>` additions.
fn typed(prompt: &str) -> String {
    let mut text = prompt.to_string();
    while let Some(open) = text.find('<') {
        let Some(name_end) = text[open + 1..].find('>') else {
            break;
        };
        let name = text[open + 1..open + 1 + name_end].to_string();
        if name.starts_with('/') || name.contains(' ') {
            break;
        }
        let close = format!("</{name}>");
        let Some(end) = text.find(&close) else {
            break;
        };
        text.replace_range(open..end + close.len(), "");
    }
    text.trim().to_string()
}

// ------------------------------------------------------------ the session

struct Daemon(std::process::Child);

impl Drop for Daemon {
    fn drop(&mut self) {
        // SAFETY: SIGTERM to our own child.
        unsafe { libc::kill(self.0.id() as i32, libc::SIGTERM) };
        let _ = self.0.wait();
    }
}

fn which(program: &str) -> Option<PathBuf> {
    std::env::var_os("PATH").and_then(|path| {
        std::env::split_paths(&path)
            .map(|dir| dir.join(program))
            .find(|candidate| candidate.is_file())
    })
}

/// Save the screen as `<name>.txt` under `$COPILOT_E2E_SCREENS`, when set
/// (what a session in Rakumux shows, for detection fixtures).
fn capture(client: &Client, id: &SessionId, name: &str) {
    if let Some(dir) = std::env::var_os("COPILOT_E2E_SCREENS") {
        let _ = std::fs::write(
            Path::new(&dir).join(format!("{name}.txt")),
            screen(client, id),
        );
    }
}

fn screen(client: &Client, id: &SessionId) -> String {
    client
        .read(ReadRequest {
            id: id.clone(),
            source: ReadSource::Visible,
            lines: None,
            format: TextFormat::Text,
        })
        .map(|read| read.text)
        .unwrap_or_default()
}

/// The session's whole conversation, a message a line: its role and its
/// text, tool calls' names or tools' output.
fn chat(client: &Client, id: &SessionId) -> Vec<(TranscriptRole, String)> {
    let mut messages = Vec::new();
    let mut cursor = None;
    loop {
        let page = client.transcript(id, cursor.as_ref(), None).unwrap();
        messages.extend(page.messages.into_iter().map(|message| {
            let text = message
                .parts
                .iter()
                .map(|part| match part {
                    TranscriptPart::Text { text, .. } | TranscriptPart::Reasoning { text, .. } => {
                        text.as_str()
                    }
                    TranscriptPart::ToolCall { name, .. } => name.as_str(),
                    TranscriptPart::ToolResult { output, .. } => output.as_str(),
                    TranscriptPart::Unknown => "",
                })
                .collect::<Vec<_>>()
                .join("\n");
            (message.role, text)
        }));
        if !page.more {
            return messages;
        }
        cursor = Some(page.cursor);
    }
}

/// Wait until `check` holds for the session; shows its screen when it
/// doesn't.
fn wait_view(
    client: &Client,
    id: &SessionId,
    what: &str,
    check: impl Fn(&SessionView) -> bool,
) -> SessionView {
    let mut view = None;
    let found = wait_until(SLOW, || {
        view = client.get(id).ok();
        view.as_ref().is_some_and(&check)
    });
    assert!(
        found,
        "{what} never happened; view {:#?}\nscreen:\n{}",
        view.map(|view| (
            view.lifecycle,
            view.activity,
            view.user_prompt,
            view.agent_output,
            view.resume
        )),
        screen(client, id)
    );
    client.get(id).unwrap()
}

fn keys(client: &Client, id: &SessionId, keys: &[&str]) {
    client
        .send(SendRequest {
            id: id.clone(),
            keys: keys.iter().map(|key| key.to_string()).collect(),
            text: None,
        })
        .unwrap();
}

/// Answer the folder trust dialog (shown on every start in a folder that
/// wasn't remembered), seen as blocked.
fn trust(client: &Client, id: &SessionId) {
    wait_view(client, id, "the trust dialog", |view| {
        view.lifecycle == Lifecycle::Blocked
    });
    assert!(wait_until(SLOW, || screen(client, id)
        .contains("Do you trust the files in this folder?")));
    capture(client, id, "blocked_trust_folder");
    keys(client, id, &["enter"]);
    wait_view(client, id, "the prompt after trusting", |view| {
        view.lifecycle == Lifecycle::Idle
    });
}

#[test]
#[ignore = "needs GitHub Copilot CLI on PATH and `cargo build --workspace`; about a minute"]
fn copilot_in_a_session_end_to_end() {
    let Some(copilot) = which("copilot") else {
        eprintln!("skipped: no `copilot` on PATH");
        return;
    };
    let rakumux = Path::new(EXE).with_file_name("tui");
    if !rakumux.is_file() {
        eprintln!(
            "skipped: build the workspace first ({} is missing)",
            rakumux.display()
        );
        return;
    }
    let home = TestHome::new();
    let copilot_home = home.dir.path().join("copilot-home");
    let cache = std::env::var_os("COPILOT_CACHE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.dir.path().join("copilot-cache"));
    // A repository to change files in (`.git` files are all the daemon reads).
    let project = std::fs::canonicalize(&home.work).unwrap();
    std::fs::create_dir_all(project.join(".git/refs/heads")).unwrap();
    std::fs::write(project.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
    let port = start_model();

    // Rakumux's hooks, as a user installs them.
    let installed = Command::new(&rakumux)
        .args(["session", "hook-config", "--agent", "copilot", "--install"])
        .env("HOME", home.dir.path())
        .env("COPILOT_HOME", &copilot_home)
        .output()
        .unwrap();
    assert!(installed.status.success(), "{installed:?}");
    let hooks = copilot_home.join("hooks/rakumux.json");
    assert!(hooks.is_file());

    let child = Command::new(EXE)
        .arg("serve")
        .env(session_core::env::HOME, &home.layout.home)
        .env(session_core::env::BIN, &rakumux)
        .env("COPILOT_HOME", &copilot_home)
        .env("RAKUMUX_PTYD_LOG", "debug")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let _daemon = Daemon(child);
    let client = loop {
        let connected = Client::connect(ClientConfig {
            layout: home.layout.clone(),
            client_name: "copilot-e2e".into(),
            version: "test".into(),
            auth: AuthMode::User,
            spawn: false,
            ptyd_path: None,
        });
        if let Ok(client) = connected {
            break client;
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    client.set_reply_capture(true).unwrap();

    let mut env = home.env();
    env.extend(
        [
            ("COPILOT_HOME", copilot_home.display().to_string()),
            ("COPILOT_CACHE_HOME", cache.display().to_string()),
            ("COPILOT_OFFLINE", "true".into()),
            ("COPILOT_AUTO_UPDATE", "false".into()),
            (
                "COPILOT_PROVIDER_BASE_URL",
                format!("http://127.0.0.1:{port}/v1"),
            ),
            ("COPILOT_PROVIDER_TYPE", "openai".into()),
            ("COPILOT_PROVIDER_API_KEY", "not-a-key".into()),
            ("COPILOT_MODEL", "mock-model".into()),
            ("COPILOT_PROVIDER_MODEL_ID", "gpt-4.1".into()),
            ("COPILOT_PROVIDER_MAX_PROMPT_TOKENS", "100000".into()),
            ("COPILOT_PROVIDER_MAX_OUTPUT_TOKENS", "4000".into()),
        ]
        .map(|(name, value)| (name.to_string(), value)),
    );
    let session = client
        .create(CreateSession {
            scope: Some(ScopeRoot::resolve(&project).unwrap()),
            name: Some("copilot".into()),
            cwd: None,
            argv: Some(vec!["bash".into(), "--norc".into(), "--noprofile".into()]),
            startup_cmds: vec![format!("{} --no-auto-update", copilot.display())],
            env: env.clone(),
            cols: 120,
            rows: 40,
            machine: None,
            prefill: None,
        })
        .unwrap();
    let id = session.id.clone();

    // Detected, blocked on folder trust, then idle at its prompt.
    let view = wait_view(&client, &id, "Copilot detected", |view| {
        view.agent
            .as_ref()
            .is_some_and(|agent| agent.kind == "copilot")
            || view
                .terminal
                .as_ref()
                .and_then(|terminal| terminal.foreground_process.as_deref())
                .is_some_and(|name| name != "bash")
    });
    eprintln!(
        "foreground: {:?}",
        view.terminal.and_then(|t| t.foreground_process)
    );
    trust(&client, &id);

    // A turn: working while the model thinks, then a finished reply.
    client
        .submit(&id, "slow 4 hello from the end to end test")
        .unwrap();
    let working = wait_view(&client, &id, "working", |view| {
        view.lifecycle == Lifecycle::Working
    });
    eprintln!("working activity: {:?}", working.activity);
    capture(&client, &id, "working");
    let done = wait_view(&client, &id, "a finished turn", |view| {
        view.lifecycle == Lifecycle::Done
            && view.agent_output.as_deref() == Some("Mock reply: slow 4 hello from the end")
    });
    assert_eq!(
        done.user_prompt.as_deref(),
        Some("slow 4 hello from the end to end test")
    );
    let turn = done.agent_turn.clone().unwrap();
    let conversation = turn.conversation_id.clone().unwrap();
    assert_eq!(conversation.len(), 36, "{conversation}");
    assert_eq!(
        done.agent.as_ref().unwrap().agent_session_id.as_deref(),
        Some(conversation.as_str()),
        "the hooks named the same conversation"
    );
    let content = client.turn(&id).unwrap();
    assert_eq!(
        content.prompt.as_deref(),
        Some("slow 4 hello from the end to end test")
    );
    assert_eq!(content.messages, ["Mock reply: slow 4 hello from the end"]);
    capture(&client, &id, "idle_after_turn");
    client
        .send(SendRequest {
            id: id.clone(),
            keys: Vec::new(),
            text: Some("half a prompt".into()),
        })
        .unwrap();
    std::thread::sleep(Duration::from_millis(500));
    capture(&client, &id, "idle_typing");
    keys(&client, &id, &["esc"]);
    std::thread::sleep(Duration::from_millis(300));
    capture(&client, &id, "idle_esc_once");
    keys(&client, &id, &["esc"]);
    std::thread::sleep(Duration::from_millis(300));
    // Read: idle.
    client.touch(&id).unwrap();
    wait_view(&client, &id, "idle once read", |view| {
        view.lifecycle == Lifecycle::Idle
    });

    // The reply, captured for the feed.
    let reply = {
        let mut found = None;
        assert!(wait_until(SLOW, || {
            found = client.replies(None).unwrap().0.into_iter().next();
            found.is_some()
        }));
        found.unwrap()
    };
    assert_eq!(reply.agent, "copilot");
    assert_eq!(
        reply.conversation_id.as_deref(),
        Some(conversation.as_str())
    );
    assert_eq!(reply.text, "Mock reply: slow 4 hello from the end");
    assert!(reply.position.is_some());

    // A file changed: blocked on its approval, then a place.
    let notes = project.join("notes.txt");
    client
        .submit(&id, &format!("please create {}", notes.display()))
        .unwrap();
    let blocked = wait_view(&client, &id, "the create approval", |view| {
        view.lifecycle == Lifecycle::Blocked
    });
    eprintln!("approval activity: {:?}", blocked.activity);
    // Its hook comes before the dialog is drawn.
    assert!(wait_until(SLOW, || screen(&client, &id)
        .contains("Do you want to create")));
    capture(&client, &id, "blocked_create_file");
    keys(&client, &id, &["enter"]);
    wait_view(&client, &id, "the file made and a place", |view| {
        view.lifecycle == Lifecycle::Done
            && view.agent_output.as_deref() == Some("Done: the tool ran.")
            && view
                .places
                .iter()
                .any(|place| place.path == project && place.edited)
    });
    assert!(notes.is_file());

    // The whole conversation.
    let page = client.transcript(&id, None, None).unwrap();
    assert_eq!(page.agent, "copilot");
    assert_eq!(page.conversation_id, conversation);
    let roles: Vec<TranscriptRole> = page.messages.iter().map(|m| m.role).collect();
    assert_eq!(
        roles,
        [
            TranscriptRole::User,
            TranscriptRole::Assistant,
            TranscriptRole::User,
            TranscriptRole::Assistant,
            TranscriptRole::Tool,
            TranscriptRole::Assistant,
        ],
        "{:#?}",
        page.messages
    );
    assert!(page.messages.iter().all(|message| message.id.len() == 36));

    // A reply written back, typed in as the status service does.
    client.touch(&id).unwrap();
    let foreground = client
        .get(&id)
        .unwrap()
        .terminal
        .and_then(|terminal| terminal.foreground_process)
        .unwrap();
    let before = client.get(&id).unwrap().agent_turn.unwrap().id;
    client
        .submit_unless_blocked(
            &id,
            "a reply written back from the feed",
            Duration::from_secs(1),
            Some(&foreground),
        )
        .unwrap();
    let answered = wait_view(&client, &id, "the reply typed in as a new turn", |view| {
        view.user_prompt.as_deref() == Some("a reply written back from the feed")
            && view
                .agent_turn
                .as_ref()
                .is_some_and(|turn| turn.id != before)
            && view.lifecycle == Lifecycle::Done
    });
    assert_eq!(
        answered.agent_turn.unwrap().conversation_id.as_deref(),
        Some(conversation.as_str())
    );

    // A reply typed while a turn works, as the status service types one
    // mid-turn: Copilot holds it (`pending`) while the model answers, then
    // takes it as the next turn's prompt; the turn it was typed into isn't
    // broken off.
    client.touch(&id).unwrap();
    wait_view(&client, &id, "idle once read", |view| {
        view.lifecycle == Lifecycle::Idle
    });
    let before = client.get(&id).unwrap().agent_turn.unwrap().id;
    client.submit(&id, HOLD).unwrap();
    let holding = wait_view(&client, &id, "the long turn working", |view| {
        view.lifecycle == Lifecycle::Working
            && view.user_prompt.as_deref() == Some(HOLD)
            && view
                .agent_turn
                .as_ref()
                .is_some_and(|turn| turn.id != before)
    });
    let holding_turn = holding.agent_turn.unwrap();
    let typed_at = std::time::Instant::now();
    client
        .submit_unless_blocked(&id, MIDTURN, Duration::from_secs(1), Some(&foreground))
        .unwrap();
    // Held while the turn works: it goes on, and the reply waits.
    assert!(!wait_until(Duration::from_secs(2), || {
        client.get(&id).is_ok_and(|view| {
            matches!(view.lifecycle, Lifecycle::Done | Lifecycle::Idle)
                || view
                    .agent_turn
                    .as_ref()
                    .is_some_and(|turn| turn.id != holding_turn.id)
        })
    }));
    let taken = wait_view(&client, &id, "the reply taken as a new turn", |view| {
        view.user_prompt.as_deref() == Some(MIDTURN)
            && view
                .agent_turn
                .as_ref()
                .is_some_and(|turn| turn.id != holding_turn.id)
    });
    let taken_turn = taken.agent_turn.clone().unwrap();
    eprintln!(
        "mid-turn reply: typed into turn {} (working), taken as turn {} after {} ms, {:?}",
        holding_turn.id,
        taken_turn.id,
        typed_at.elapsed().as_millis(),
        taken.lifecycle
    );
    assert_eq!(taken_turn.conversation_id, holding_turn.conversation_id);
    assert_eq!(
        taken_turn.conversation_id.as_deref(),
        Some(conversation.as_str())
    );
    wait_view(&client, &id, "the reply answered", |view| {
        view.lifecycle == Lifecycle::Done
            && view.agent_output.as_deref() == Some("Mock reply: MIDTURN ping 4242 typed while it")
            && view.agent_turn.as_ref().map(|turn| turn.id) == Some(taken_turn.id)
    });
    // Queued: the long turn finished with its own answer, then the reply
    // went in once, as a prompt of its own, and was answered.
    let messages = chat(&client, &id);
    let hold_at = messages
        .iter()
        .rposition(|message| *message == (TranscriptRole::User, HOLD.to_string()))
        .unwrap();
    assert_eq!(
        messages[hold_at + 1..],
        [
            (
                TranscriptRole::Assistant,
                "Mock reply: slow 12 hold the turn while".to_string()
            ),
            (TranscriptRole::User, MIDTURN.to_string()),
            (
                TranscriptRole::Assistant,
                "Mock reply: MIDTURN ping 4242 typed while it".to_string()
            ),
        ],
        "{messages:#?}"
    );
    // Not left in the input box.
    assert!(screen(&client, &id).matches(MIDTURN).count() <= 1);
    // Nor taken again: no other turn starts, and the chat stays as it is.
    assert!(!wait_until(Duration::from_secs(3), || {
        client.get(&id).is_ok_and(|view| {
            view.agent_turn
                .as_ref()
                .is_some_and(|turn| turn.id != taken_turn.id)
        })
    }));
    assert_eq!(chat(&client, &id), messages);
    let answer = {
        let mut found = None;
        assert!(wait_until(SLOW, || {
            found = client
                .replies(None)
                .unwrap()
                .0
                .into_iter()
                .find(|reply| reply.text == "Mock reply: MIDTURN ping 4242 typed while it");
            found.is_some()
        }));
        found.unwrap()
    };
    assert_eq!(
        answer.conversation_id.as_deref(),
        Some(conversation.as_str())
    );
    eprintln!(
        "replies captured: {:?}",
        client
            .replies(None)
            .unwrap()
            .0
            .iter()
            .map(|reply| reply.text.as_str())
            .collect::<Vec<_>>()
    );

    // Typed while a shell command runs (`sleep` allowed here, so there's no
    // approval and the turn is seen working): Copilot moves the command to
    // the background and steers the reply in at once, as a prompt of its
    // own; the command finishes all the same and the agent carries on.
    client.touch(&id).unwrap();
    wait_view(&client, &id, "idle once read", |view| {
        view.lifecycle == Lifecycle::Idle
    });
    client.submit(&id, "please run sleep 1").unwrap();
    wait_view(&client, &id, "the command's approval", |view| {
        view.lifecycle == Lifecycle::Blocked
    });
    assert!(
        wait_until(SLOW, || screen(&client, &id)
            .contains("don't ask again for `sleep`")),
        "{}",
        screen(&client, &id)
    );
    // A reply now would answer the approval: refused, nothing typed.
    let refused = client
        .submit_unless_blocked(&id, "1", Duration::from_secs(1), Some(&foreground))
        .unwrap_err();
    assert_eq!(refused.code, ErrorCode::SessionBlocked, "{refused}");
    keys(&client, &id, &["down"]);
    std::thread::sleep(Duration::from_millis(300));
    keys(&client, &id, &["enter"]);
    wait_view(&client, &id, "the short command run", |view| {
        view.lifecycle == Lifecycle::Done
            && view.agent_output.as_deref() == Some("Done: the tool ran.")
    });
    client.touch(&id).unwrap();
    wait_view(&client, &id, "idle once read", |view| {
        view.lifecycle == Lifecycle::Idle
    });
    client.submit(&id, "please run sleep 12").unwrap();
    let running = wait_view(&client, &id, "the long command running", |view| {
        view.lifecycle == Lifecycle::Working
            && view.agent_output.as_deref() == Some("Shell: sleep 12")
    });
    let running_turn = running.agent_turn.unwrap();
    let typed_at = std::time::Instant::now();
    client
        .submit_unless_blocked(&id, STEER, Duration::from_secs(1), Some(&foreground))
        .unwrap();
    let steered = wait_view(&client, &id, "the reply steered in", |view| {
        view.user_prompt.as_deref() == Some(STEER)
            && view
                .agent_turn
                .as_ref()
                .is_some_and(|turn| turn.id != running_turn.id)
    });
    let steered_turn = steered.agent_turn.clone().unwrap();
    eprintln!(
        "steered reply: typed into turn {} (working), taken as turn {} after {} ms, {:?}",
        running_turn.id,
        steered_turn.id,
        typed_at.elapsed().as_millis(),
        steered.lifecycle
    );
    assert_eq!(
        steered_turn.conversation_id.as_deref(),
        Some(conversation.as_str())
    );
    // Taken before the command finished: steered, not queued.
    assert!(typed_at.elapsed() < Duration::from_secs(10));
    wait_view(&client, &id, "the steered reply answered", |view| {
        view.agent_turn.as_ref().map(|turn| turn.id) == Some(steered_turn.id)
            && view.agent_output.as_deref() == Some("Mock reply: MIDTURN pong 777 typed while a")
    });
    wait_view(
        &client,
        &id,
        "the command finished and the agent on",
        |view| {
            view.lifecycle == Lifecycle::Done
                && view.agent_output.as_deref() == Some("Done: the tool ran.")
        },
    );
    let messages = chat(&client, &id);
    let run_at = messages
        .iter()
        .rposition(|message| *message == (TranscriptRole::User, "please run sleep 12".to_string()))
        .unwrap();
    let after_run = &messages[run_at + 1..];
    eprintln!("after the command: {after_run:#?}");
    assert_eq!(
        after_run
            .iter()
            .filter(|message| **message == (TranscriptRole::User, STEER.to_string()))
            .count(),
        1,
        "{after_run:#?}"
    );
    assert!(
        after_run
            .iter()
            .any(|(role, text)| *role == TranscriptRole::Assistant
                && text == "Mock reply: MIDTURN pong 777 typed while a"),
        "{after_run:#?}"
    );
    assert!(
        after_run
            .iter()
            .any(|(role, text)| *role == TranscriptRole::Tool
                && text.contains("completed with exit code 0")),
        "the command ran to its end: {after_run:#?}"
    );
    assert!(screen(&client, &id).matches(STEER).count() <= 1);

    // A second chat from the sidebar, working while the first is shown again:
    // its hooks aren't this session's (the second's finished turn doesn't
    // make the first look done).
    client.touch(&id).unwrap();
    wait_view(&client, &id, "idle once read", |view| {
        view.lifecycle == Lifecycle::Idle
    });
    keys(&client, &id, &["left"]);
    std::thread::sleep(Duration::from_millis(700));
    keys(&client, &id, &["left"]);
    std::thread::sleep(Duration::from_millis(700));
    client
        .send(SendRequest {
            id: id.clone(),
            keys: Vec::new(),
            text: Some("n".into()),
        })
        .unwrap();
    // Followed at the next output: the prompt typed into it.
    std::thread::sleep(Duration::from_millis(1500));
    client
        .submit(&id, "slow 8 hello from the second chat")
        .unwrap();
    let second = wait_view(&client, &id, "the second chat working", |view| {
        view.lifecycle == Lifecycle::Working
            && view
                .agent_turn
                .as_ref()
                .and_then(|turn| turn.conversation_id.as_deref())
                .is_some_and(|chat| chat != conversation)
    })
    .agent_turn
    .unwrap()
    .conversation_id
    .unwrap();
    keys(&client, &id, &["left"]);
    std::thread::sleep(Duration::from_millis(700));
    keys(&client, &id, &["down"]);
    std::thread::sleep(Duration::from_millis(500));
    keys(&client, &id, &["enter"]);
    wait_view(&client, &id, "the first chat shown again", |view| {
        view.agent_turn
            .as_ref()
            .and_then(|turn| turn.conversation_id.as_deref())
            == Some(conversation.as_str())
    });
    let second_log = copilot_home.join(format!("session-state/{second}/events.jsonl"));
    assert!(
        wait_until(SLOW, || std::fs::read_to_string(&second_log).is_ok_and(
            |log| log.contains("Mock reply: slow 8 hello from the second")
        )),
        "the second chat's turn never finished"
    );
    // Its agentStop hook has come and gone.
    std::thread::sleep(Duration::from_secs(3));
    let shown = wait_view(&client, &id, "the first chat idle", |view| {
        view.lifecycle == Lifecycle::Idle
    });
    assert_eq!(
        shown.agent_turn.unwrap().conversation_id.as_deref(),
        Some(conversation.as_str()),
        "the session's conversation is the one on screen"
    );
    std::thread::sleep(Duration::from_secs(2));
    let view = client.get(&id).unwrap();
    assert_eq!(view.lifecycle, Lifecycle::Idle);
    assert!(view.activity.is_none(), "{:?}", view.activity);
    keys(&client, &id, &["right"]);

    // Killed, then revived: the same conversation, resumed.
    let resume = wait_view(&client, &id, "its resume command", |view| {
        view.resume
            .as_ref()
            .is_some_and(|resume| resume.source == ResumeSource::KnownAgent)
    })
    .resume
    .unwrap();
    assert_eq!(
        resume.argv,
        [
            "copilot",
            "--no-auto-update",
            &format!("--resume={conversation}")
        ]
    );
    client.kill(&id, true).unwrap();
    wait_view(&client, &id, "the session ended", |view| {
        view.status != SessionStatus::Live
    });
    client
        .revive(ReviveSession {
            id: id.clone(),
            env,
            cols: 120,
            rows: 40,
            resume: true,
        })
        .unwrap();
    trust(&client, &id);
    // The log is read while the terminal shows output: a still screen is read
    // again once something is typed.
    let is_resumed = |view: &SessionView| {
        view.agent_turn
            .as_ref()
            .and_then(|turn| turn.conversation_id.as_deref())
            == Some(conversation.as_str())
    };
    for _ in 0..10 {
        if client.get(&id).is_ok_and(|view| is_resumed(&view)) {
            break;
        }
        client
            .send(SendRequest {
                id: id.clone(),
                keys: Vec::new(),
                text: Some("x".into()),
            })
            .unwrap();
        std::thread::sleep(Duration::from_millis(1200));
        keys(&client, &id, &["backspace"]);
        std::thread::sleep(Duration::from_millis(1200));
    }
    let resumed = wait_view(&client, &id, "the conversation resumed", is_resumed);
    assert_eq!(resumed.user_prompt.as_deref(), Some(STEER));
}
