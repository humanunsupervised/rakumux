//! Heartbeats end to end: the real daemon binary in a temp home, a
//! heartbeat made over its socket, and a run asked for now. The run's login
//! shell types the heartbeat's command, which starts a stand-in Claude Code
//! (a script installed as `claude`: a moment starting, then Claude Code's
//! idle screen, recording every byte typed); the daemon gives it the task
//! only once that prompt shows, and sends it. A session's own first input
//! (not sent) goes in the same way, without Enter.
#![cfg(target_os = "linux")]

mod runtime_support;

use runtime_support::{wait_until, TestHome};
use session_core::api::{CreateSession, FirstInput};
use session_core::client::{AuthMode, Client, ClientConfig, SessionApi};
use session_core::heartbeat::{HeartbeatFields, HeartbeatSpec, RunOutcome, Schedule};
use session_core::model::ScopeRoot;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

const EXE: &str = env!("CARGO_BIN_EXE_rakumux-ptyd");
const SLOW: Duration = Duration::from_secs(30);

struct Daemon(Child);

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// `claude`: two seconds starting (a plain line, then nothing), then Claude
/// Code's idle screen from a captured fixture; every byte typed afterwards
/// goes to `$STAND_IN_INPUT`. Only builtins run once it shows its prompt,
/// so it alone is the session's foreground.
fn install_stand_in(bin: &Path) {
    let fixture =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/detect/claude_idle_prompt.txt");
    let text = std::fs::read_to_string(fixture).unwrap();
    let (_, screen) = text.split_once("\n---\n").unwrap();
    std::fs::create_dir_all(bin).unwrap();
    std::fs::write(bin.join("idle.txt"), screen.replace('\n', "\r\n")).unwrap();
    let script = format!(
        r#"#!/bin/bash
set -u
stty raw -echo 2>/dev/null
printf 'starting Claude Code…\r\n'
read -r -t 2 -N 100000 _ignored || true
printf '\e]0;✳ Claude Code\a\e[?1049h\e[H'
IFS= read -r -d '' idle < "{dir}/idle.txt" || true
printf '%s' "$idle"
while IFS= read -r -s -N 1 c; do printf '%s' "$c" >> "$STAND_IN_INPUT"; done
"#,
        dir = bin.display()
    );
    let path = bin.join("claude");
    std::fs::write(&path, script).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

fn start(home: &TestHome, bin: &Path, input: &Path) -> (Daemon, Client) {
    let path = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let mut command = Command::new(EXE);
    command
        .arg("serve")
        .env(session_core::env::HOME, &home.layout.home)
        .env("HOME", home.dir.path())
        .env("PATH", path)
        .env("SHELL", "/bin/bash")
        .env("STAND_IN_INPUT", input)
        .env_remove(session_core::env::SESSION_ID)
        .env_remove(session_core::env::SESSION_TOKEN)
        .env_remove(session_core::env::SOCKET)
        .env_remove(session_core::env::BIN)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let daemon = Daemon(command.spawn().unwrap());
    let client = loop {
        let connected = Client::connect(ClientConfig {
            layout: home.layout.clone(),
            client_name: "heartbeats-e2e".into(),
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

fn typed(input: &Path) -> String {
    std::fs::read_to_string(input).unwrap_or_default()
}

#[test]
fn a_run_gives_a_real_agent_its_task_once_its_prompt_shows() {
    let home = TestHome::new();
    let bin = home.dir.path().join("bin");
    let input: PathBuf = home.dir.path().join("typed");
    install_stand_in(&bin);
    let (_daemon, client) = start(&home, &bin, &input);
    let work = std::fs::canonicalize(&home.work).unwrap();
    let scope = ScopeRoot::from_canonical(work);
    client.add_project(scope.path()).ok();

    let heartbeat = client
        .heartbeat_create(HeartbeatSpec {
            scope: Some(scope.clone()),
            name: Some("deps".into()),
            fields: HeartbeatFields {
                command: Some("claude --permission-mode acceptEdits".into()),
                prompt: Some("update the dependencies".into()),
                schedule: Some(Schedule::Daily { at: "09:00".into() }),
                ..Default::default()
            },
            enabled: true,
        })
        .unwrap();
    assert!(heartbeat.enabled);
    assert_eq!(heartbeat.agent.as_deref(), Some("claude"));
    assert!(heartbeat.next_run_at.is_some());

    let run = client.heartbeat_run(&heartbeat.id).unwrap();
    assert_eq!(run.name, "deps #1");
    assert_eq!(run.heartbeat.as_ref().map(|r| r.run), Some(1));
    // Starting: the task waits for the prompt.
    std::thread::sleep(Duration::from_millis(1500));
    assert_eq!(typed(&input), "", "typed before the prompt showed");
    assert!(
        wait_until(SLOW, || {
            let typed = typed(&input);
            // The Enter, as the stand-in's terminal reads it.
            typed.contains("update the dependencies\r")
                || typed.contains("update the dependencies\n")
        }),
        "the task never arrived; typed: {:?}",
        typed(&input)
    );
    // Exactly once, and the heartbeat says so.
    std::thread::sleep(Duration::from_millis(1500));
    assert_eq!(typed(&input).matches("update the dependencies").count(), 1);
    assert!(wait_until(SLOW, || client
        .heartbeat(&heartbeat.id)
        .is_ok_and(|h| h
            .last_run
            .is_some_and(|r| r.outcome == RunOutcome::TaskGiven))));
    let listed = client.heartbeats(None).unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].runs, 1);

    // A session's own first input, not sent: typed once the prompt shows,
    // no Enter.
    std::fs::remove_file(&input).ok();
    client
        .create(CreateSession {
            scope: Some(scope.clone()),
            name: Some("setup".into()),
            cwd: None,
            argv: None,
            startup_cmds: vec!["claude".into()],
            env: Vec::new(),
            cols: 100,
            rows: 30,
            machine: None,
            prefill: None,
            first_input: Some(FirstInput {
                text: "Heartbeat hb_X is empty".into(),
                submit: false,
            }),
        })
        .unwrap();
    assert!(
        wait_until(SLOW, || typed(&input).contains("Heartbeat hb_X is empty")),
        "never typed; typed: {:?}",
        typed(&input)
    );
    std::thread::sleep(Duration::from_millis(1500));
    assert_eq!(typed(&input), "Heartbeat hb_X is empty");
}
