//! An answer written on rakumux.com to the question OpenCode (1.x as
//! `opencode`, 2 as `opencode2`), Grok Build or Kimi Code asks in its own
//! dialog, end to end: the real daemon (`rakumux-ptyd serve`), the real
//! account service (`rakumux status-service`, signed in as an enrolled
//! machine), a local rakumux.com (`mid_turn/fake_cloud.rs`) and the real
//! agent, in a home of its own, against a scripted model on 127.0.0.1
//! (`answers/mock_model.rs`) that calls the agent's own question tool with two
//! questions (one choice; several choices and a free answer). The account
//! service lists the session as asking something, with `question_answers`
//! and the agent's `question_answers.<agent>`, and uploads the chat with the
//! question's call; the test writes the answer for it as rakumux.com does;
//! the daemon works the dialog by number and the agent logs the result.
//!
//! Ignored by default: each needs its agent installed (`opencode`,
//! `opencode2`, `grok` on `PATH`; `kimi` there or `~/.kimi-code/bin/kimi`;
//! `RAKUMUX_E2E_<AGENT>` names another binary). No sign-in, no account:
//! the agent's home is a temp dir. Run them with
//! `cargo test -p tui --test question_answers_numbered -- --ignored --nocapture --test-threads 1`.
//! Rakumux's own files are in a temp dir; `~/.rakumux` is never touched.
#![cfg(target_os = "linux")]

#[path = "mid_turn/certs.rs"]
mod certs;
#[path = "mid_turn/fake_cloud.rs"]
mod fake_cloud;
#[path = "answers/mock_model.rs"]
mod mock_model;

use fake_cloud::{ulid, FakeCloud, Machine};
use mock_model::{MockModel, COLOUR, FRUITS};
use serde_json::{json, Value};
use session_core::api::{
    CreateSession, ReadRequest, ReadSource, SendRequest, TextFormat, TranscriptPart,
};
use session_core::client::{AuthMode, Client, ClientConfig, SessionApi};
use session_core::model::{Lifecycle, ScopeRoot};
use session_core::paths::{Layout, Registration};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const TUI: &str = env!("CARGO_BIN_EXE_tui");
const MACHINE: &str = "e2e-answers";
const BOUND: Duration = Duration::from_secs(90);

/// An agent under test: how it's started, and how rakumux.com names it.
struct Agent {
    /// Its command, as typed in the session.
    command: &'static str,
    /// The environment variable naming another binary.
    env: &'static str,
    /// The feed's name, and its question tool there.
    feed: &'static str,
    tool: &'static str,
}

#[test]
#[ignore = "needs OpenCode 1.x on PATH: run with --ignored"]
fn an_answer_written_on_rakumux_com_answers_opencodes_question() {
    answers(&Agent {
        command: "opencode",
        env: "RAKUMUX_E2E_OPENCODE",
        feed: "opencode",
        tool: "question",
    });
}

#[test]
#[ignore = "needs OpenCode 2 on PATH as opencode2: run with --ignored"]
fn an_answer_written_on_rakumux_com_answers_opencode_2s_question() {
    answers(&Agent {
        command: "opencode2",
        env: "RAKUMUX_E2E_OPENCODE2",
        feed: "opencode",
        tool: "question",
    });
}

#[test]
#[ignore = "needs Grok Build on PATH: run with --ignored"]
fn an_answer_written_on_rakumux_com_answers_grok_builds_question() {
    answers(&Agent {
        command: "grok",
        env: "RAKUMUX_E2E_GROK",
        feed: "grok",
        tool: "ask_user_question",
    });
}

#[test]
#[ignore = "needs Kimi Code installed: run with --ignored"]
fn an_answer_written_on_rakumux_com_answers_kimi_codes_question() {
    answers(&Agent {
        command: "kimi",
        env: "RAKUMUX_E2E_KIMI",
        feed: "kimi",
        tool: "AskUserQuestion",
    });
}

/// The agent's binary: its `RAKUMUX_E2E_<AGENT>`, else on `PATH` (Kimi Code:
/// else where its installer puts it).
fn binary(agent: &Agent) -> PathBuf {
    if let Some(path) = std::env::var_os(agent.env) {
        return PathBuf::from(path);
    }
    let on_path = std::env::var_os("PATH").and_then(|path| {
        std::env::split_paths(&path)
            .map(|dir| dir.join(agent.command))
            .find(|candidate| candidate.is_file())
    });
    let installed = std::env::var_os("HOME")
        .filter(|_| agent.command == "kimi")
        .map(|home| PathBuf::from(home).join(".kimi-code/bin/kimi"))
        .filter(|path| path.is_file());
    on_path.or(installed).unwrap_or_else(|| {
        panic!(
            "{} isn't installed: put it on PATH or name it with {}",
            agent.command, agent.env
        )
    })
}

/// The agent's home under `root`, its configuration pointing at `model`:
/// the environment its session gets, and what the daemon needs to find its
/// conversations.
fn home(
    agent: &Agent,
    root: &Path,
    model: &MockModel,
) -> (Vec<(String, String)>, Vec<(String, String)>) {
    let home = root.join("home");
    let bin = root.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    std::os::unix::fs::symlink(binary(agent), bin.join(agent.command)).unwrap();
    let mut session = vec![
        ("HOME".to_owned(), home.display().to_string()),
        (
            "PATH".to_owned(),
            format!("{}:/usr/bin:/bin", bin.display()),
        ),
        ("SHELL".to_owned(), "/bin/bash".to_owned()),
        ("LANG".to_owned(), "C.UTF-8".to_owned()),
        ("TERM".to_owned(), "xterm-256color".to_owned()),
    ];
    let mut daemon = Vec::new();
    let mut set = |name: &str, value: String, both: bool| {
        session.push((name.to_owned(), value.clone()));
        if both {
            daemon.push((name.to_owned(), value));
        }
    };
    match agent.feed {
        "opencode" => {
            let config = home.join(".config/opencode");
            std::fs::create_dir_all(&config).unwrap();
            std::fs::write(
                config.join("opencode.json"),
                json!({"$schema": "https://opencode.ai/config.json", "autoupdate": false,
                    "share": "disabled", "model": "mock/mock-model",
                    "provider": {"mock": {"npm": "@ai-sdk/openai-compatible", "name": "Mock",
                        "options": {"baseURL": model.base_url, "apiKey": "sk-mock"},
                        "models": {"mock-model": {"name": "Mock Model", "tool_call": true}}}}})
                .to_string(),
            )
            .unwrap();
            set(
                "XDG_CONFIG_HOME",
                home.join(".config").display().to_string(),
                false,
            );
            set(
                "XDG_DATA_HOME",
                home.join(".local/share").display().to_string(),
                true,
            );
            set(
                "XDG_CACHE_HOME",
                home.join(".cache").display().to_string(),
                false,
            );
            set(
                "XDG_STATE_HOME",
                home.join(".local/state").display().to_string(),
                false,
            );
            set("OPENCODE_DISABLE_AUTOUPDATE", "1".into(), false);
            set("OPENCODE_DISABLE_MODELS_FETCH", "1".into(), false);
        }
        "grok" => {
            let grok = home.join(".grok");
            std::fs::create_dir_all(&grok).unwrap();
            let url = &model.base_url;
            let origin = url.trim_end_matches("/v1");
            std::fs::write(
                grok.join("config.toml"),
                format!(
                    "[cli]\nauto_update = false\n\n[features]\ntelemetry = \"off\"\n\
                     remote_fetch = false\n\n[diagnostics]\nerror_reporting = false\n\n\
                     [models]\ndefault = \"mock\"\n\n[model.mock]\nmodel = \"mock-model\"\n\
                     base_url = \"{url}\"\nname = \"Mock\"\napi_key = \"mock-key\"\n\
                     context_window = 256000\n\n[auth]\nauth_provider_command = \"echo mock-token\"\n\n\
                     [endpoints]\nmodels_base_url = \"{url}\"\nxai_api_base_url = \"{url}\"\n\
                     cli_chat_proxy_base_url = \"{origin}\"\n"
                ),
            )
            .unwrap();
            set("GROK_HOME", grok.display().to_string(), true);
            set("GROK_DISABLE_AUTOUPDATER", "1".into(), false);
            set("DISABLE_ERROR_REPORTING", "1".into(), false);
        }
        _ => {
            set(
                "KIMI_CODE_HOME",
                root.join("kimi").display().to_string(),
                true,
            );
            set(
                "KIMI_CODE_CACHE_DIR",
                root.join("kimi-cache").display().to_string(),
                false,
            );
            set("KIMI_DISABLE_TELEMETRY", "1".into(), false);
            set("KIMI_CODE_NO_AUTO_UPDATE", "1".into(), false);
            set("KIMI_MODEL_PROVIDER_TYPE", "openai".into(), false);
            set("KIMI_MODEL_NAME", "mock-model".into(), false);
            set("KIMI_MODEL_API_KEY", "sk-mock".into(), false);
            set("KIMI_MODEL_BASE_URL", model.base_url.clone(), false);
        }
    }
    (session, daemon)
}

fn answers(agent: &Agent) {
    let started = Instant::now();
    let dir = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    let layout = Layout::with_home(root.join("rakumux"));
    let work = root.join("work");
    std::fs::create_dir_all(&work).unwrap();
    let model = MockModel::start();
    let (env, daemon_env) = home(agent, &root, &model);
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

    // The daemon, reading the agent's conversations where its home keeps them.
    let mut daemon = command(&ptyd_binary(), &layout);
    daemon.arg("serve").stderr(Stdio::null());
    for (name, value) in &daemon_env {
        daemon.env(name, value);
    }
    let daemon = Running(daemon.spawn().unwrap());
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
    let session = client
        .create(CreateSession {
            scope: Some(ScopeRoot::resolve(&work).unwrap()),
            name: Some("questions".into()),
            cwd: None,
            argv: None,
            startup_cmds: vec![agent.command.into()],
            env,
            cols: 100,
            rows: 40,
            machine: None,
            prefill: None,
        })
        .unwrap();
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
    let send = |keys: &[&str], text: Option<&str>| {
        client
            .send(SendRequest {
                id: session.id.clone(),
                keys: keys.iter().map(|key| key.to_string()).collect(),
                text: text.map(String::from),
            })
            .unwrap();
        std::thread::sleep(Duration::from_millis(500));
    };
    // Ready for a prompt (Kimi Code first asks to trust the folder).
    wait("the agent to start", &mut || {
        let shown = screen();
        if shown.contains("Trust this folder?") {
            send(&["enter"], None);
            return false;
        }
        client
            .get(&session.id)
            .is_ok_and(|view| view.agent.is_some() && view.lifecycle == Lifecycle::Idle)
    });
    std::thread::sleep(Duration::from_secs(1));
    send(&[], Some("ASK me two questions"));
    send(&["enter"], None);
    let mut conversation = None;
    wait("the agent to ask its question", &mut || {
        let view = client.get(&session.id).ok();
        conversation = view
            .as_ref()
            .and_then(|view| view.agent_turn.as_ref()?.conversation_id.clone());
        view.is_some_and(|view| view.lifecycle == Lifecycle::Blocked)
            && screen().contains(COLOUR)
            && conversation.is_some()
    });
    let conversation = conversation.unwrap();

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
    // rakumux.com sees the session asking, from a machine that answers this
    // agent's questions, and the question's call in its chat.
    let mut call: Option<String> = None;
    let own = format!("question_answers.{}", agent.feed);
    wait(
        "the session listed asking, and the question in its chat",
        &mut || {
            let state = cloud.state();
            let listed = state.says("question_answers")
                && state.says(&own)
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
                    (part["type"] == "tool_call" && part["name"] == agent.tool)
                        .then(|| part["call_id"].as_str().map(str::to_owned))?
                })
            });
            listed && call.is_some()
        },
    );
    let call_id = call.unwrap();
    let written = fake_cloud::now_ms();
    let reply = cloud.write_answer(
        (session.id.as_str(), session.name.as_str()),
        &conversation,
        &call_id,
        json!([
            {"question": COLOUR, "choices": ["Blue"]},
            {"question": FRUITS, "choices": ["Apple", "Cherry"], "text": "Mango"},
        ]),
        "Colour: Blue\nFruits: Apple, Cherry, Mango",
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
    // The agent's own record of the call has its result, with the answers given.
    let mut output = None;
    let mut after = None;
    while output.is_none() {
        let page = client
            .transcript(&session.id, after.as_ref(), None)
            .unwrap();
        output = page.messages.iter().find_map(|message| {
            message.parts.iter().find_map(|part| match part {
                TranscriptPart::ToolResult {
                    call_id: Some(id),
                    output,
                    ..
                } if *id == call_id => Some(output.clone()),
                _ => None,
            })
        });
        if !page.more {
            break;
        }
        after = Some(page.cursor);
    }
    let output = output.expect("the question's result in the agent's conversation");
    for said in ["Blue", "Apple", "Cherry", "Mango"] {
        assert!(output.contains(said), "{said} in {output}");
    }
    eprintln!(
        "an answer from rakumux.com to {}, end to end: {:?}",
        agent.command,
        started.elapsed()
    );
    let _ = client.kill(&session.id, false);
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
        "{} isn't built: run `cargo build --workspace` first",
        path.display()
    );
    path
}
