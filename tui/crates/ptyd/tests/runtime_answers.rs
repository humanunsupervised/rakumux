//! `session_answer` end to end with the real agents: GitHub Copilot CLI
//! (offline, `COPILOT_OFFLINE`, against a model through its own provider
//! support) and Droid (`FACTORY_AIRGAP_ENABLED`, a custom model), each in a
//! session of the real daemon in a temp home, against a scripted model this
//! test serves on 127.0.0.1: a prompt `ASK <name>` makes it call the agent's
//! question tool with the questions scripted under that name, and a tool's
//! result makes it say `Got: <result>`. The daemon answers each dialog key
//! by key, and the answers come back as the tool's result in the
//! conversation, which `session_transcript` reads with the call's id (what
//! rakumux.com answers by). No account, network or real home.
//!
//! Ignored by default: it needs `copilot` and `droid` on `PATH` and the
//! workspace's `tui` binary, and takes a minute or two. An agent that isn't
//! installed is skipped.
//!
//! ```text
//! cargo build --workspace
//! cargo test -p rakumux-ptyd --test runtime_answers -- --ignored --nocapture
//! ```
//!
//! `COPILOT_CACHE_HOME` is passed on when set, so Copilot's native package
//! (about 240 MB) isn't unpacked again each run.
#![cfg(target_os = "linux")]

mod runtime_support;

use runtime_support::{wait_until, TestHome};
use serde_json::{json, Value};
use session_core::api::{
    CreateSession, QuestionAnswer, ReadRequest, ReadSource, TextFormat, TranscriptPart,
};
use session_core::client::{AuthMode, Client, ClientConfig, SessionApi};
use session_core::ids::SessionId;
use session_core::model::{Lifecycle, ScopeRoot};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

const EXE: &str = env!("CARGO_BIN_EXE_rakumux-ptyd");
const SLOW: Duration = Duration::from_secs(45);

// ------------------------------------------------------------ the model

/// The question tool call scripted for `ASK <name>`: its tool and its
/// arguments as the model writes them (JSON text in the order asked, which
/// the agent keeps: Copilot's dialog shows a form's fields so).
fn scripted(name: &str) -> Option<(&'static str, &'static str)> {
    Some(match name {
        "copilot-colour" => (
            "ask_user",
            r#"{"message": "Which colour should the button be?", "requestedSchema": {"type": "object", "properties": {"colour": {"type": "string", "title": "Colour", "enum": ["red", "green", "blue"]}}, "required": ["colour"]}}"#,
        ),
        "copilot-form" => (
            "ask_user",
            r#"{"message": "A few choices for the release.", "requestedSchema": {"type": "object", "properties": {"colour": {"type": "string", "title": "Colour", "enum": ["red", "green", "blue"]}, "fruits": {"type": "array", "title": "Fruits", "items": {"type": "string", "enum": ["apple", "banana", "cherry"]}}, "ship": {"type": "boolean", "title": "Ship today"}, "note": {"type": "string", "title": "Note"}}, "required": ["colour", "fruits", "ship"]}}"#,
        ),
        "droid-three" => (
            "AskUser",
            r#"{"questionnaire": "1. [question] Which colour should the button be?\n[topic] Colour\n[option] Red\n[option] Green\n[option] Blue\n\n2. [question] Which fruits do you like? (multi)\n[topic] Fruits\n[option] Apple\n[option] Banana\n[option] Cherry\n\n3. [question] Which pet would you keep?\n[topic] Pet\n[option] Cat\n[option] Dog\n"}"#,
        ),
        _ => return None,
    })
}

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

/// One chat completion, streamed or not.
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
    let respond = |out: &mut TcpStream, kind: &str, text: &str| {
        let _ = write!(
            out,
            "HTTP/1.1 200 OK\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{text}",
            text.len()
        );
    };
    if get {
        let models = json!({"object": "list", "data": [{"id": "mock-model", "object": "model"}]});
        respond(&mut out, "application/json", &models.to_string());
        return;
    }
    let request: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let messages = request["messages"].as_array().cloned().unwrap_or_default();
    let last = messages.last().cloned().unwrap_or(Value::Null);
    let prompt = messages
        .iter()
        .rev()
        .find(|message| message["role"] == "user")
        .map(content)
        .unwrap_or_default();
    let ask = prompt
        .split_whitespace()
        .skip_while(|word| *word != "ASK")
        .nth(1)
        .and_then(scripted);
    // (text, or a tool call)
    let (text, call) = if last["role"] == "tool" {
        (Some(format!("Got: {}", content(&last))), None)
    } else if let Some((tool, arguments)) = ask {
        (None, Some((tool, arguments)))
    } else {
        (Some("Mock reply.".to_owned()), None)
    };
    // Every call its own id, as a model's are.
    static CALLS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(1);
    let call_id = format!(
        "call_{}_{}",
        std::process::id(),
        CALLS.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
    );
    if request["stream"] != json!(true) {
        let message = match (&text, &call) {
            (Some(text), _) => json!({"role": "assistant", "content": text}),
            (None, Some((tool, arguments))) => json!({"role": "assistant", "content": null,
                "tool_calls": [{"id": call_id, "type": "function",
                    "function": {"name": tool, "arguments": arguments}}]}),
            (None, None) => unreachable!(),
        };
        let finish = if call.is_some() { "tool_calls" } else { "stop" };
        let reply = json!({"id": "chatcmpl-mock", "object": "chat.completion", "created": 0,
            "model": "mock-model", "choices": [{"index": 0, "message": message,
                "finish_reason": finish}],
            "usage": {"prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15}});
        respond(&mut out, "application/json", &reply.to_string());
        return;
    }
    let _ = write!(
        out,
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\nConnection: close\r\n\r\n"
    );
    let mut send = |delta: Value, finish: Option<&str>| {
        let chunk = json!({"id": "chatcmpl-mock", "object": "chat.completion.chunk", "created": 0,
            "model": "mock-model", "choices": [{"index": 0, "delta": delta, "finish_reason": finish}],
            "usage": {"prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15}});
        let _ = write!(out, "data: {chunk}\n\n");
        let _ = out.flush();
    };
    match (text, call) {
        (Some(text), _) => {
            send(json!({"role": "assistant", "content": text}), None);
            send(json!({}), Some("stop"));
        }
        (None, Some((tool, arguments))) => {
            send(
                json!({"role": "assistant", "content": null, "tool_calls": [{"index": 0,
                    "id": call_id, "type": "function",
                    "function": {"name": tool, "arguments": arguments}}]}),
                None,
            );
            send(json!({}), Some("tool_calls"));
        }
        (None, None) => {}
    }
    let _ = write!(out, "data: [DONE]\n\n");
}

// ------------------------------------------------------------ the daemon

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

/// The real daemon in `home`, with `env` (where it finds the agent's logs).
fn daemon(home: &TestHome, rakumux: &Path, env: &[(&str, &Path)]) -> (Daemon, Client) {
    let mut command = Command::new(EXE);
    command
        .arg("serve")
        .env(session_core::env::HOME, &home.layout.home)
        .env(session_core::env::BIN, rakumux)
        .env("HOME", home.dir.path())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    for (name, value) in env {
        command.env(name, value);
    }
    let daemon = Daemon(command.spawn().unwrap());
    let client = loop {
        let connected = Client::connect(ClientConfig {
            layout: home.layout.clone(),
            client_name: "answers-e2e".into(),
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
    (daemon, client)
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

/// Wait until the screen shows `needle`.
fn wait_screen(client: &Client, id: &SessionId, needle: &str) {
    assert!(
        wait_until(SLOW, || screen(client, id).contains(needle)),
        "the screen never showed {needle:?}:\n{}",
        screen(client, id)
    );
}

/// The conversation's newest call of `tool`, by its id, and whether a result
/// with that id followed it, and what that result said.
fn call_and_result(
    client: &Client,
    id: &SessionId,
    tool: &str,
) -> Option<(String, Option<String>)> {
    let mut found: Option<(String, Option<String>)> = None;
    let mut cursor = None;
    loop {
        let page = client.transcript(id, cursor.as_ref(), None).ok()?;
        for part in page.messages.iter().flat_map(|message| &message.parts) {
            match part {
                TranscriptPart::ToolCall {
                    call_id: Some(call),
                    name,
                    ..
                } if name == tool => found = Some((call.clone(), None)),
                TranscriptPart::ToolResult {
                    call_id: Some(call),
                    output,
                    ..
                } => {
                    if let Some((asked, result)) = found.as_mut() {
                        if asked == call {
                            *result = Some(output.clone());
                        }
                    }
                }
                _ => {}
            }
        }
        if !page.more {
            return found;
        }
        cursor = Some(page.cursor);
    }
}

fn answer(question: &str, choices: &[&str], text: Option<&str>) -> QuestionAnswer {
    QuestionAnswer {
        question: question.into(),
        choices: choices.iter().map(|c| c.to_string()).collect(),
        text: text.map(String::from),
    }
}

/// Ask with `prompt`, wait for the dialog (blocked, `needle` on screen),
/// answer it through the daemon and return the tool's result.
fn ask_and_answer(
    client: &Client,
    id: &SessionId,
    tool: &str,
    prompt: &str,
    needle: &str,
    answers: Vec<QuestionAnswer>,
) -> String {
    let before = call_and_result(client, id, tool).map(|(call, _)| call);
    client.submit(id, prompt).unwrap();
    wait_screen(client, id, needle);
    let mut call = None;
    assert!(
        wait_until(SLOW, || {
            call = call_and_result(client, id, tool)
                .map(|(call, _)| call)
                .filter(|call| Some(call) != before.as_ref());
            call.is_some() && client.get(id).unwrap().lifecycle == Lifecycle::Blocked
        }),
        "no {tool} call, or not blocked:\n{}",
        screen(client, id)
    );
    let call = call.unwrap();
    // The daemon reads the log at most once a second.
    std::thread::sleep(Duration::from_millis(1500));
    client
        .answer(id, &call, &answers, Duration::from_secs(10))
        .unwrap_or_else(|error| panic!("{prompt}: {error:?}\n{}", screen(client, id)));
    let mut result = None;
    assert!(wait_until(SLOW, || {
        result = call_and_result(client, id, tool)
            .and_then(|(asked, result)| (asked == call).then_some(result).flatten());
        result.is_some()
    }));
    result.unwrap()
}

fn session(
    client: &Client,
    home: &TestHome,
    name: &str,
    command: String,
    env: Vec<(String, String)>,
) -> SessionId {
    let project = std::fs::canonicalize(&home.work).unwrap();
    client
        .create(CreateSession {
            scope: Some(ScopeRoot::resolve(&project).unwrap()),
            name: Some(name.into()),
            cwd: None,
            argv: Some(vec!["bash".into(), "--norc".into(), "--noprofile".into()]),
            startup_cmds: vec![command],
            env,
            cols: 120,
            rows: 40,
            machine: None,
            prefill: None,
        })
        .unwrap()
        .id
}

#[test]
#[ignore = "needs GitHub Copilot CLI on PATH and `cargo build --workspace`; about a minute"]
fn copilots_ask_user_is_answered_from_its_dialog() {
    let Some(copilot) = which("copilot") else {
        eprintln!("skipped: no `copilot` on PATH");
        return;
    };
    let rakumux = Path::new(EXE).with_file_name("tui");
    if !rakumux.is_file() {
        eprintln!("skipped: build the workspace first");
        return;
    }
    let home = TestHome::new();
    let copilot_home = home.dir.path().join("copilot-home");
    let cache = std::env::var_os("COPILOT_CACHE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.dir.path().join("copilot-cache"));
    let installed = Command::new(&rakumux)
        .args(["session", "hook-config", "--agent", "copilot", "--install"])
        .env("HOME", home.dir.path())
        .env("COPILOT_HOME", &copilot_home)
        .output()
        .unwrap();
    assert!(installed.status.success(), "{installed:?}");
    let port = start_model();
    let (_daemon, client) = daemon(&home, &rakumux, &[("COPILOT_HOME", &copilot_home)]);
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
    let id = session(
        &client,
        &home,
        "copilot",
        format!("{} --no-auto-update", copilot.display()),
        env,
    );
    wait_screen(&client, &id, "Do you trust the files in this folder?");
    client
        .send(session_core::api::SendRequest {
            id: id.clone(),
            keys: vec!["enter".into()],
            text: None,
        })
        .unwrap();
    wait_screen(&client, &id, "/ commands");

    // One choice, then its free answer typed under Other.
    let result = ask_and_answer(
        &client,
        &id,
        "ask_user",
        "ASK copilot-colour",
        "Copilot needs information.",
        vec![answer(
            "Which colour should the button be?",
            &["blue"],
            None,
        )],
    );
    assert_eq!(result, "User responded: blue");
    let result = ask_and_answer(
        &client,
        &id,
        "ask_user",
        "ASK copilot-colour again",
        "Copilot needs information.",
        vec![answer(
            "Which colour should the button be?",
            &[],
            Some("Purple, 2 shades"),
        )],
    );
    assert_eq!(result, "User responded: Purple, 2 shades");
    // A form of four fields, shown in the order asked, answered in the order
    // rakumux.com reads it (by field name, as the log is read).
    let result = ask_and_answer(
        &client,
        &id,
        "ask_user",
        "ASK copilot-form",
        "Copilot needs information.",
        vec![
            answer("Colour", &["green"], None),
            answer("Fruits", &["apple", "cherry"], Some("Mango")),
            answer("Note", &[], Some("Ship it on Friday")),
            answer("Ship today", &["No"], None),
        ],
    );
    assert_eq!(
        result,
        "User responded: colour=green, fruits=apple, cherry, Mango, ship=false, note=Ship it on Friday"
    );
}

#[test]
#[ignore = "needs Droid on PATH and `cargo build --workspace`; about a minute"]
fn droids_ask_user_is_answered_from_its_dialog() {
    let Some(droid) = which("droid") else {
        eprintln!("skipped: no `droid` on PATH");
        return;
    };
    let rakumux = Path::new(EXE).with_file_name("tui");
    if !rakumux.is_file() {
        eprintln!("skipped: build the workspace first");
        return;
    }
    let home = TestHome::new();
    let factory = home.dir.path().join("factory");
    let port = start_model();
    std::fs::create_dir_all(factory.join(".factory")).unwrap();
    std::fs::write(
        factory.join(".factory/settings.json"),
        json!({"customModels": [{"model": "mock-model", "displayName": "Mock",
            "baseUrl": format!("http://127.0.0.1:{port}/v1"), "apiKey": "not-a-key",
            "provider": "generic-chat-completion-api", "maxOutputTokens": 4000}]})
        .to_string(),
    )
    .unwrap();
    let hooks = Command::new(&rakumux)
        .args(["session", "hook-config", "--agent", "droid"])
        .output()
        .unwrap();
    assert!(hooks.status.success(), "{hooks:?}");
    std::fs::write(factory.join(".factory/hooks.json"), &hooks.stdout).unwrap();
    let (_daemon, client) = daemon(&home, &rakumux, &[("FACTORY_HOME_OVERRIDE", &factory)]);
    let mut env = home.env();
    env.extend(
        [
            ("FACTORY_HOME_OVERRIDE", factory.display().to_string()),
            ("FACTORY_AIRGAP_ENABLED", "true".into()),
            ("FACTORY_DROID_AUTO_UPDATE_ENABLED", "false".into()),
        ]
        .map(|(name, value)| (name.to_string(), value)),
    );
    let id = session(&client, &home, "droid", droid.display().to_string(), env);
    wait_screen(&client, &id, "Trust this folder?");
    client
        .send(session_core::api::SendRequest {
            id: id.clone(),
            keys: vec!["enter".into()],
            text: None,
        })
        .unwrap();
    wait_screen(&client, &id, "? for help");

    let result = ask_and_answer(
        &client,
        &id,
        "AskUser",
        "ASK droid-three",
        "ESC stop agent",
        vec![
            answer("Which colour should the button be?", &[], Some("Teal")),
            answer(
                "Which fruits do you like?",
                &["Apple", "Cherry"],
                Some("Kiwi"),
            ),
            answer("Which pet would you keep?", &["Dog"], None),
        ],
    );
    assert_eq!(
        result,
        "1. [question] Which colour should the button be?\n[answer] Teal\n\n\
         2. [question] Which fruits do you like?\n[answer] Apple, Cherry, Kiwi\n\n\
         3. [question] Which pet would you keep?\n[answer] Dog"
    );
}
