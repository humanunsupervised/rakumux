//! Heartbeats end to end: the real daemon binary in a temp home, a
//! heartbeat made over its socket, and a run asked for now. The run's login
//! shell types the heartbeat's command, which starts a stand-in Claude Code
//! (a script installed as `claude`: a moment starting, then Claude Code's
//! idle screen, recording its arguments and every byte typed) with the
//! prompt as its last argument, whatever is in it. A command that can't take
//! it there has it typed, only once that prompt shows, and sent. A session's
//! own first input (not sent) goes in the same way, without Enter.
#![cfg(target_os = "linux")]

mod runtime_support;

use runtime_support::{wait_until, TestHome};
use session_core::api::{CreateSession, FirstInput, ReviveSession};
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
/// Code's idle screen from a captured fixture; its arguments go to
/// `$STAND_IN_INPUT.args` (each ended by NUL), every byte typed afterwards to
/// `$STAND_IN_INPUT`. Only builtins run once it shows its prompt,
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
printf '%s\0' "$@" > "$STAND_IN_INPUT.args"
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

/// The arguments the stand-in was last started with.
fn arguments(input: &Path) -> Vec<String> {
    let text = std::fs::read_to_string(input.with_extension("args")).unwrap_or_default();
    let mut words: Vec<String> = text.split('\0').map(String::from).collect();
    words.pop();
    words
}

#[test]
fn a_run_is_its_command_with_the_prompt_as_the_last_argument() {
    let home = TestHome::new();
    let bin = home.dir.path().join("bin");
    let input: PathBuf = home.dir.path().join("typed");
    install_stand_in(&bin);
    let (_daemon, client) = start(&home, &bin, &input);
    let scope = ScopeRoot::from_canonical(std::fs::canonicalize(&home.work).unwrap());
    client.add_project(scope.path()).ok();

    // Nothing in it means anything to the shell, its line editor or Claude.
    let prompt = format!(
        "--model x\nDon't expand \"$HOME\", `date`, !! or \\n:\n\tfix it; then 'stop' & é\n{}",
        "a long task. ".repeat(600)
    );
    let heartbeat = client
        .heartbeat_create(HeartbeatSpec {
            scope: Some(scope.clone()),
            name: Some("deps".into()),
            fields: HeartbeatFields {
                agent: Some("claude".into()),
                command: Some(
                    "claude --dangerously-skip-permissions --allowedTools 'Bash(git *)' Edit"
                        .into(),
                ),
                prompt: Some(prompt.clone()),
                schedule: Some(Schedule::Daily { at: "09:00".into() }),
                ..Default::default()
            },
            enabled: true,
        })
        .unwrap();
    let run = client.heartbeat_run(&heartbeat.id).unwrap();
    let expected = [
        "--dangerously-skip-permissions",
        "--allowedTools",
        "Bash(git *)",
        "Edit",
        "--",
        &prompt,
    ];
    assert!(
        wait_until(SLOW, || arguments(&input).len() == expected.len()),
        "never started: {:?}",
        arguments(&input)
    );
    assert_eq!(arguments(&input), expected);
    assert!(wait_until(SLOW, || client
        .heartbeat(&heartbeat.id)
        .is_ok_and(|h| h
            .last_run
            .is_some_and(|r| r.outcome == RunOutcome::TaskGiven))));
    // Nothing is typed into the agent, its prompt showing.
    std::thread::sleep(Duration::from_secs(5));
    assert_eq!(typed(&input), "");

    // Brought back before its conversation is known, it's the same command.
    client.kill(&run.id, false).unwrap();
    std::fs::remove_file(input.with_extension("args")).unwrap();
    assert!(wait_until(SLOW, || client.get(&run.id).is_ok_and(|s| {
        s.status == session_core::model::SessionStatus::Exited
    })));
    client
        .revive(ReviveSession {
            id: run.id.clone(),
            env: Vec::new(),
            cols: 100,
            rows: 30,
            resume: true,
        })
        .unwrap();
    assert!(wait_until(SLOW, || arguments(&input).len() == expected.len()));
    assert_eq!(arguments(&input), expected);

    // The prompt is that run's alone.
    let other = client
        .create(CreateSession {
            scope: Some(scope),
            name: Some("other".into()),
            cwd: None,
            argv: None,
            startup_cmds: vec!["claude \"[${RAKUMUX_PROMPT-none}]\"".into()],
            env: vec![
                ("PATH".into(), format!("{}:/usr/bin:/bin", bin.display())),
                ("STAND_IN_INPUT".into(), input.display().to_string()),
                ("RAKUMUX_PROMPT".into(), prompt),
            ],
            cols: 100,
            rows: 30,
            machine: None,
            prefill: None,
            first_input: None,
            workspace: None,
        })
        .unwrap();
    assert!(
        wait_until(SLOW, || arguments(&input) == ["[none]"]),
        "{}: {:?}",
        other.id,
        arguments(&input)
    );
}

#[test]
fn a_run_whose_command_takes_no_prompt_has_it_typed_once_its_agents_prompt_shows() {
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
                command: Some("cd . && claude --permission-mode acceptEdits".into()),
                prompt: Some("update the dependencies".into()),
                schedule: Some(Schedule::Daily { at: "09:00".into() }),
                ..Default::default()
            },
            enabled: true,
        })
        .unwrap();
    assert!(heartbeat.enabled);
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
            workspace: None,
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

/// Every run is in its heartbeat's record, newest first, with where its
/// session is now: a run that couldn't start too, a deleted run's saved
/// output still readable, and all of it there after the daemon restarts.
#[test]
fn every_run_is_recorded_and_a_deleted_runs_output_stays_readable() {
    use session_core::api::TextFormat;
    use session_core::heartbeat::RunPlace;
    use session_core::model::SessionStatus;
    let home = TestHome::new();
    let bin = home.dir.path().join("bin");
    let input: PathBuf = home.dir.path().join("typed");
    install_stand_in(&bin);
    let (daemon, client) = start(&home, &bin, &input);
    let scope = ScopeRoot::from_canonical(std::fs::canonicalize(&home.work).unwrap());
    client.add_project(scope.path()).ok();
    let spec = |scope: &ScopeRoot, name: &str| HeartbeatSpec {
        scope: Some(scope.clone()),
        name: Some(name.into()),
        fields: HeartbeatFields {
            command: Some("claude".into()),
            prompt: Some("check the build".into()),
            schedule: Some(Schedule::Every { minutes: 60 }),
            ..Default::default()
        },
        enabled: true,
    };
    let heartbeat = client.heartbeat_create(spec(&scope, "build")).unwrap();
    let given = |run: &session_core::model::SessionView| {
        client
            .heartbeat_runs(&heartbeat.id, None, None)
            .is_ok_and(|page| {
                page.runs.iter().any(|r| {
                    r.run.session_id.as_ref() == Some(&run.id)
                        && r.run.outcome == RunOutcome::TaskGiven
                })
            })
    };
    let first = client.heartbeat_run(&heartbeat.id).unwrap();
    assert!(
        wait_until(SLOW, || given(&first)),
        "run #1 never had its task"
    );
    let second = client.heartbeat_run(&heartbeat.id).unwrap();
    assert!(
        wait_until(SLOW, || given(&second)),
        "run #2 never had its task"
    );
    client.kill(&first.id, false).unwrap();
    assert!(wait_until(SLOW, || client
        .get(&first.id)
        .is_ok_and(|s| s.status == SessionStatus::Exited)));

    let page = client.heartbeat_runs(&heartbeat.id, None, None).unwrap();
    let runs: Vec<(u32, Option<RunPlace>, Option<SessionStatus>)> = page
        .runs
        .iter()
        .map(|r| (r.run.run, r.place, r.status))
        .collect();
    assert_eq!(
        runs,
        [
            (2, Some(RunPlace::Listed), Some(SessionStatus::Live)),
            (1, Some(RunPlace::Listed), Some(SessionStatus::Exited)),
        ]
    );
    assert!(!page.more);
    // In pages.
    let newest = client.heartbeat_runs(&heartbeat.id, Some(1), None).unwrap();
    assert!(newest.more);
    let older = client
        .heartbeat_runs(&heartbeat.id, Some(1), Some(newest.runs[0].seq))
        .unwrap();
    assert_eq!(older.runs[0].run.session_id.as_ref(), Some(&first.id));

    // Deleted, run #1 is in the session history, its output readable.
    client.delete(&first.id).unwrap();
    let page = client.heartbeat_runs(&heartbeat.id, None, None).unwrap();
    assert_eq!(page.runs[1].place, Some(RunPlace::History));
    let saved = client
        .history_read(&first.id, Some(200), TextFormat::Text)
        .unwrap();
    assert!(
        saved.text.contains("starting Claude Code"),
        "saved output: {:?}",
        saved.text
    );
    // A listed session isn't read so.
    assert!(client
        .history_read(&second.id, None, TextFormat::Text)
        .is_err());

    // A run that couldn't start is recorded too.
    let gone = home.dir.path().join("gone");
    std::fs::create_dir_all(&gone).unwrap();
    let gone = ScopeRoot::from_canonical(std::fs::canonicalize(&gone).unwrap());
    client.add_project(gone.path()).ok();
    let orphan = client.heartbeat_create(spec(&gone, "orphan")).unwrap();
    std::fs::remove_dir(gone.path()).unwrap();
    assert!(client.heartbeat_run(&orphan.id).is_err());
    let failed = client.heartbeat_runs(&orphan.id, None, None).unwrap();
    assert_eq!(failed.runs.len(), 1);
    assert_eq!(failed.runs[0].run.outcome, RunOutcome::Failed);
    assert_eq!(failed.runs[0].run.session_id, None);
    assert!(failed.runs[0].run.detail.is_some());

    // The record outlives the daemon.
    drop(client);
    drop(daemon);
    let (_daemon, client) = start(&home, &bin, &input);
    let page = client.heartbeat_runs(&heartbeat.id, None, None).unwrap();
    let runs: Vec<(u32, RunOutcome, Option<RunPlace>)> = page
        .runs
        .iter()
        .map(|r| (r.run.run, r.run.outcome, r.place))
        .collect();
    assert_eq!(
        runs,
        [
            (2, RunOutcome::TaskGiven, Some(RunPlace::Listed)),
            (1, RunOutcome::TaskGiven, Some(RunPlace::History)),
        ]
    );
}
