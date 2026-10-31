//! An answer written on rakumux.com to the question Claude Code asks in its
//! `AskUserQuestion` dialog, end to end: the real daemon (`rakumux-ptyd
//! serve`), the real account service (`rakumux status-service`, signed in as
//! an enrolled machine), a local rakumux.com (`mid_turn/fake_cloud.rs`) and
//! the real Claude Code, asked to put two questions (one choice; several
//! choices and a free answer). The account service lists the session as
//! asking something, with `question_answers`, and uploads the chat with the
//! question's call; the test writes the answer for it as rakumux.com does;
//! the daemon works the dialog and Claude Code logs the question's result.
//!
//! Ignored by default: it needs `claude` on `PATH`, signed in, and talks to
//! its model (it writes one short conversation under `~/.claude`, as any run
//! of Claude Code does). Run it with
//! `cargo test -p tui --test question_answers -- --ignored --nocapture`.
//! Rakumux's own files are in a temp dir; `~/.rakumux` is never touched.
#![cfg(target_os = "linux")]

#[path = "mid_turn/certs.rs"]
mod certs;
#[path = "mid_turn/fake_cloud.rs"]
mod fake_cloud;

use fake_cloud::{ulid, FakeCloud, Machine};
use serde_json::{json, Value};
use session_core::api::{CreateSession, ReadRequest, ReadSource, SendRequest, TextFormat};
use session_core::client::{AuthMode, Client, ClientConfig, SessionApi};
use session_core::model::{Lifecycle, ScopeRoot, SessionView};
use session_core::paths::{Layout, Registration};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const TUI: &str = env!("CARGO_BIN_EXE_tui");
const MACHINE: &str = "e2e-answers";
/// The longest any step may take (Claude Code thinks before it asks).
const BOUND: Duration = Duration::from_secs(120);
const QUESTIONS: &str = r#"{"questions": [{"question": "Which colour do you prefer?", "header": "Colour", "multiSelect": false, "options": [{"label": "Red", "description": "Warm"}, {"label": "Green", "description": "Calm"}, {"label": "Blue", "description": "Cool"}]}, {"question": "Which fruits do you like?", "header": "Fruits", "multiSelect": true, "options": [{"label": "Apple", "description": "Crisp"}, {"label": "Banana", "description": "Soft"}, {"label": "Cherry", "description": "Small"}]}]}"#;

#[test]
#[ignore = "needs a signed-in Claude Code on PATH: run with --ignored"]
fn an_answer_written_on_rakumux_com_answers_claude_codes_question() {
    let started = Instant::now();
    let dir = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    let layout = Layout::with_home(root.join("rakumux"));
    let work = root.join("work");
    std::fs::create_dir_all(&work).unwrap();
    let certs = certs::TestCerts::new();
    let ca = root.join("ca.pem");
    std::fs::write(&ca, &certs.ca_pem).unwrap();
    let id = ulid();
    let secret = format!("rkm_{}", format!("{}{}", ulid(), ulid()).get(..43).unwrap());
    let sensagent = layout.home.join("sensagent");
    std::fs::create_dir_all(&sensagent).unwrap();
    std::fs::set_permissions(&sensagent, std::fs::Permissions::from_mode(0o700)).unwrap();
    private(
        &sensagent.join("machine-credential.json"),
        &json!({"version": 1, "machine_id": id, "secret": secret,
            "account": {"subject": "user_e2e", "email": "e2e@example.com",
                "email_verified": true}}),
    );
    private(
        &sensagent.join("machine.json"),
        &json!({"version": 1, "id": id, "base": MACHINE, "renames": 0}),
    );
    let cloud = FakeCloud::start(
        &certs,
        Machine {
            name: MACHINE.into(),
            id,
            secret,
        },
    );
    let wait = |what: &str, check: &mut dyn FnMut() -> bool| {
        let deadline = Instant::now() + BOUND;
        while !check() {
            assert!(
                Instant::now() < deadline,
                "waited {BOUND:?} for {what}\n{}",
                diagnostics(&layout, &root, &cloud)
            );
            std::thread::sleep(Duration::from_millis(200));
        }
    };

    // The daemon, with this user's own HOME: Claude Code's sign-in and its
    // conversations are there.
    let daemon = Running(
        command(&ptyd_binary(), &layout)
            .arg("serve")
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let pid = daemon.0.id();
    wait("the daemon to register", &mut || {
        Registration::load(&layout).is_ok_and(|r| r.pid == pid && r.socket.exists())
    });
    let client = Client::connect(ClientConfig {
        layout: layout.clone(),
        client_name: "e2e".into(),
        version: "test".into(),
        auth: AuthMode::User,
        spawn: false,
        ptyd_path: None,
    })
    .unwrap();
    let prompt = format!(
        "This is a test of a dialog. Call the AskUserQuestion tool exactly once, right now, \
         with exactly this input JSON, unchanged. Do nothing else and read no files. Once it is \
         answered, reply with one line repeating the answers exactly as you received them, and \
         stop.\n\n{QUESTIONS}"
    );
    let env: Vec<(String, String)> = ["PATH", "HOME", "SHELL", "LANG"]
        .iter()
        .filter_map(|name| Some((name.to_string(), std::env::var(name).ok()?)))
        .collect();
    let session = client
        .create(CreateSession {
            scope: Some(ScopeRoot::resolve(&work).unwrap()),
            name: Some("questions".into()),
            cwd: None,
            argv: Some(vec!["claude".into(), prompt]),
            startup_cmds: Vec::new(),
            env,
            cols: 100,
            rows: 40,
            machine: None,
            prefill: None,
        })
        .unwrap();
    // A folder Claude Code doesn't know yet: trusted, as the user would.
    let screen = || {
        client
            .read(ReadRequest {
                id: session.id.clone(),
                source: ReadSource::Visible,
                lines: None,
                format: TextFormat::Text,
            })
            .map(|read| read.text)
            .unwrap_or_default()
    };
    let key = |name: &str| {
        client
            .send(SendRequest {
                id: session.id.clone(),
                keys: vec![name.into()],
                text: None,
            })
            .unwrap();
        std::thread::sleep(Duration::from_millis(700));
    };
    let mut asking: Option<SessionView> = None;
    let mut looks = 0;
    wait("Claude Code to ask its question", &mut || {
        let shown = screen();
        looks += 1;
        if looks % 50 == 0 {
            eprintln!("--- the session's screen:\n{shown}");
        }
        if shown.contains("trust this folder") {
            if shown
                .lines()
                .any(|line| line.trim_start().starts_with('❯') && line.contains("No, exit"))
            {
                key("down");
            }
            if shown
                .lines()
                .any(|line| line.trim_start().starts_with('❯') && line.contains("Yes, I trust"))
            {
                key("enter");
            }
            return false;
        }
        asking = client.get(&session.id).ok();
        asking.as_ref().is_some_and(|view| {
            view.lifecycle == Lifecycle::Blocked && shown.contains("Which colour do you prefer?")
        })
    });
    let conversation = asking
        .as_ref()
        .and_then(|view| view.agent_turn.as_ref()?.conversation_id.clone())
        .expect("the daemon follows Claude Code's conversation");

    let _service = Running(
        command(Path::new(TUI), &layout)
            .arg("status-service")
            .env("RAKUMUX_CLOUD_ORIGIN", &cloud.origin)
            .env("RAKUMUX_CLOUD_CA", &ca)
            .env("RAKUMUX_CLERK_ISSUER", &cloud.origin)
            .env(
                "RAKUMUX_CLERK_DEVICE_PAGE",
                format!("{}/device", cloud.origin),
            )
            .stderr(std::fs::File::create(root.join("status-service.log")).unwrap())
            .spawn()
            .unwrap(),
    );
    // rakumux.com sees the session asking, from a machine that takes answers,
    // and the question's call in its chat: the picker is answerable.
    let mut call: Option<(String, Value)> = None;
    wait(
        "the session listed asking, and the question in its chat",
        &mut || {
            let state = cloud.state();
            let listed = state.says("question_answers")
                && state.sessions.as_ref().is_some_and(|list| {
                    list["sessions"].as_array().is_some_and(|sessions| {
                        sessions.iter().any(|listed| {
                            listed["id"] == session.id.as_str()
                                && listed["lifecycle"] == "blocked"
                                && listed["conversation_id"] == conversation.as_str()
                        })
                    })
                });
            call = state.messages.iter().rev().find_map(|message| {
                message["parts"].as_array()?.iter().find_map(|part| {
                    (part["type"] == "tool_call" && part["name"] == "AskUserQuestion").then(|| {
                        let input = match &part["input"] {
                            Value::String(text) => {
                                serde_json::from_str(text).unwrap_or(Value::Null)
                            }
                            other => other.clone(),
                        };
                        (
                            part["call_id"].as_str().unwrap_or_default().to_owned(),
                            input,
                        )
                    })
                })
            });
            listed && call.is_some()
        },
    );
    let (call_id, input) = call.unwrap();
    let questions = input["questions"].as_array().unwrap();
    assert_eq!(questions.len(), 2, "{input}");
    let label =
        |question: usize, option: usize| questions[question]["options"][option]["label"].clone();
    let written = fake_cloud::now_ms();
    let reply = cloud.write_answer(
        (session.id.as_str(), session.name.as_str()),
        &conversation,
        &call_id,
        json!([
            {"question": questions[0]["question"], "choices": [label(0, 2)]},
            {"question": questions[1]["question"], "choices": [label(1, 0), label(1, 2)],
             "text": "Mango"},
        ]),
        "Blue; Apple, Cherry and Mango",
    );

    wait("the answer reported", &mut || {
        !cloud.state().reports_for(&reply).is_empty()
    });
    let reports = cloud.state().reports_for(&reply);
    assert_eq!(
        (reports[0].status.as_str(), reports[0].failure.as_deref()),
        ("delivered", None),
        "{}",
        diagnostics(&layout, &root, &cloud)
    );
    assert!(reports.len() == 1 && reports[0].at >= written);
    // Claude Code's own log has the question's result, with the answers given.
    let result = question_result(&conversation, &call_id).expect("the question's result");
    assert_eq!(
        result,
        json!({
            "Which colour do you prefer?": "Blue",
            "Which fruits do you like?": "Apple, Cherry, Mango",
        }),
        "{}",
        diagnostics(&layout, &root, &cloud)
    );
    eprintln!(
        "an answer from rakumux.com, end to end: {:?}",
        started.elapsed()
    );
    let _ = client.kill(&session.id, false);
}

/// The answers Claude Code logged for question call `call_id` of
/// `conversation` (its `toolUseResult`), under `~/.claude`.
fn question_result(conversation: &str, call_id: &str) -> Option<Value> {
    let config = std::env::var("CLAUDE_CONFIG_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(std::env::var("HOME").unwrap()).join(".claude"));
    let name = format!("{conversation}.jsonl");
    let log = std::fs::read_dir(config.join("projects"))
        .ok()?
        .filter_map(Result::ok)
        .map(|entry| entry.path().join(&name))
        .find(|path| path.is_file())?;
    std::fs::read_to_string(log)
        .ok()?
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .find(|entry| {
            entry["message"]["content"]
                .as_array()
                .is_some_and(|blocks| {
                    blocks.iter().any(|block| {
                        block["type"] == "tool_result" && block["tool_use_id"] == call_id
                    })
                })
        })
        .map(|entry| entry["toolUseResult"]["answers"].clone())
}

/// A command for this test's Rakumux home, never the session identity of
/// whatever runs the test.
fn command(program: &Path, layout: &Layout) -> Command {
    let mut command = Command::new(program);
    for name in [
        session_core::env::SESSION_ID,
        session_core::env::SESSION_TOKEN,
        session_core::env::SOCKET,
        session_core::env::BIN,
        "XDG_RUNTIME_DIR",
        "RAKUMUX_CLOUD_ORIGIN",
        "RAKUMUX_CLOUD_CA",
    ] {
        command.env_remove(name);
    }
    command
        .env(session_core::env::HOME, &layout.home)
        .stdin(Stdio::null())
        .stdout(Stdio::null());
    command
}

/// What each part did, for a failure's message.
fn diagnostics(layout: &Layout, root: &Path, cloud: &FakeCloud) -> String {
    let read = |path: PathBuf| std::fs::read_to_string(path).unwrap_or_default();
    let tail = |text: String| {
        let all: Vec<&str> = text.lines().collect();
        all[all.len().saturating_sub(20)..].join("\n")
    };
    let state = cloud.state();
    format!(
        "--- reports: {:?}\n--- requests:\n{}\n--- status-service log:\n{}\n--- daemon log:\n{}",
        state.reports,
        tail(state.log.join("\n")),
        tail(read(root.join("status-service.log"))),
        tail(read(layout.home.join("logs/ptyd.log"))),
    )
}

struct Running(Child);

impl Drop for Running {
    fn drop(&mut self) {
        // SAFETY: kill(2) on our own child's pid.
        unsafe {
            libc::kill(self.0.id() as libc::pid_t, libc::SIGTERM);
        }
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if let Ok(Some(_)) = self.0.try_wait() {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn private(path: &Path, value: &Value) {
    std::fs::write(path, value.to_string()).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
}

/// The `rakumux-ptyd` of this build, next to the `tui` under test.
fn ptyd_binary() -> PathBuf {
    let path = Path::new(TUI).parent().unwrap().join("rakumux-ptyd");
    assert!(
        path.is_file(),
        "build the workspace first: {}",
        path.display()
    );
    path
}
