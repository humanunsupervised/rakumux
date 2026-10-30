//! Cursor's agent CLI end to end through the real daemon (`rakumux-ptyd
//! serve` in a temp `RAKUMUX_HOME`), with a stand-in for Cursor: a script
//! named `cursor-agent` that keeps its chat as Cursor 2026.10.22 does (its
//! `store.db` held open under `<config>/chats/<md5 of the folder>/<chat>/`,
//! its transcript under `<data>/projects/<folder slug>/agent-transcripts/`,
//! written whole after each finished turn, without that turn's end), shows
//! Cursor's busy and idle input box, and takes a bracketed paste. No account,
//! network or real Cursor; the real CLI's own screens are checked by the
//! detection fixtures, and its live behaviour by the checks in
//! `integrations/cursor-agent/README.md`.
//!
//! Mounted by `tui/crates/ptyd/tests/cursor_agent_e2e.rs`.

use super::runtime_support::*;
use session_core::api::{
    CreateSession, ReadRequest, ReadSource, Request, Response, ReviveSession, TextFormat,
    TranscriptRole,
};
use session_core::model::{Lifecycle, ResumeSource, ScopeRoot, SessionStatus, SessionView};
use session_core::paths::Registration;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

const EXE: &str = env!("CARGO_BIN_EXE_rakumux-ptyd");
const CHAT: &str = "0f8a3c1e-7b2d-4e5f-9a6b-1c2d3e4f5a6b";

/// What the stand-in does with each prompt: Cursor's transcript lines, its
/// screens, and a file written in the folder (`notes/<n>.txt`).
const STAND_IN: &str = r#"#!/bin/sh
# A stand-in for Cursor's agent CLI (see e2e.rs).
set -u
id=0f8a3c1e-7b2d-4e5f-9a6b-1c2d3e4f5a6b
prev=
for arg in "$@"; do
  [ "$prev" = "--resume" ] && id=$arg
  prev=$arg
done
banner="CURSOR_STARTED $*"
hash=$(printf %s "$PWD" | md5sum | cut -c1-32)
chat="$CURSOR_CONFIG_DIR/chats/$hash/$id"
mkdir -p "$chat"
printf '{"schemaVersion":1,"createdAtMs":1,"hasConversation":true}' > "$chat/meta.json"
: >> "$chat/store.db"
exec 3<"$chat/store.db"
slug=$(printf %s "$PWD" | sed 's/[^A-Za-z0-9][^A-Za-z0-9]*/-/g; s/^-//; s/-$//')
dir="$CURSOR_DATA_DIR/projects/$slug/agent-transcripts/$id"
mkdir -p "$dir"
t="$dir/$id.jsonl"
touch "$t"
# Cursor redraws its screen: the busy line goes when the turn ends.
idle() { printf '\033[2J\033[H%s\n ┌──────────────────┐\n │ → Add a follow-up │\n └──────────────────┘\n' "$banner"; }
printf '\033[?2004h'
idle
esc=$(printf '\033')
open="${esc}[200~"
close="${esc}[201~"
n=0
while IFS= read -r line; do
  prompt=${line#*"$open"}
  prompt=${prompt%"$close"*}
  [ -z "$prompt" ] && continue
  n=$((n + 1))
  # After a finished turn Cursor writes the file whole, without its end.
  if tail -n 1 "$t" | grep -q turn_ended; then
    grep -v turn_ended "$t" > "$t.new"
    cat "$t.new" > "$t"
    rm -f "$t.new"
  fi
  printf '{"role":"user","message":{"content":[{"type":"text","text":"<user_query>\\n%s\\n</user_query>"}]}}\n' "$prompt" >> "$t"
  i=0
  while [ $i -lt 8 ]; do
    printf '  ⠰⠳ Generating…\n  ctrl+c to stop\n'
    sleep 0.3
    i=$((i + 1))
  done
  mkdir -p notes
  echo "$prompt" > "notes/$n.txt"
  printf '{"role":"assistant","message":{"content":[{"type":"text","text":"I will write it down."},{"type":"tool_use","name":"Write","input":{"path":"%s/notes/%s.txt","contents":"x"}}]}}\n' "$PWD" "$n" >> "$t"
  printf '{"role":"assistant","message":{"content":[{"type":"text","text":"Done: %s\\n\\n[REDACTED]"}]}}\n' "$prompt" >> "$t"
  printf '{"type":"turn_ended","status":"success"}\n' >> "$t"
  idle
  sleep 1.2
  idle
done
"#;

/// Kills the daemon if the test fails before shutting it down.
struct Daemon(Child);

impl Drop for Daemon {
    fn drop(&mut self) {
        if let Ok(None) = self.0.try_wait() {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}

fn start_daemon(home: &TestHome, env: &[(String, String)]) -> (Daemon, Registration) {
    let child = Command::new(EXE)
        .arg("serve")
        .envs(env.iter().cloned())
        .env(session_core::env::HOME, &home.layout.home)
        .env(session_core::env::BIN, "rakumux")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn rakumux-ptyd");
    let mut registration = None;
    assert!(
        wait_until(TIMEOUT, || {
            registration = Registration::load(&home.layout).ok();
            registration
                .as_ref()
                .is_some_and(|r| r.pid == child.id() && r.socket.exists())
        }),
        "daemon never registered"
    );
    (Daemon(child), registration.unwrap())
}

fn view(client: &mut TestClient, id: &session_core::ids::SessionId) -> SessionView {
    match client.request(&Request::SessionGet { id: id.clone() }) {
        Response::Session { session } => session,
        other => panic!("get failed: {other:?}"),
    }
}

/// The session's view once `check` holds.
fn wait_view(
    client: &mut TestClient,
    id: &session_core::ids::SessionId,
    what: &str,
    check: impl Fn(&SessionView) -> bool,
) -> SessionView {
    let mut last = None;
    let found = wait_until(Duration::from_secs(20), || {
        let current = view(client, id);
        let done = check(&current);
        last = Some(current);
        done
    });
    assert!(found, "never {what}: {last:#?}");
    last.unwrap()
}

fn wait_screen(client: &mut TestClient, id: &session_core::ids::SessionId, needle: &str) {
    let mut screen = String::new();
    let found = wait_until(Duration::from_secs(20), || {
        screen = match client.request(&Request::SessionRead(ReadRequest {
            id: id.clone(),
            source: ReadSource::RecentUnwrapped,
            lines: None,
            format: TextFormat::Text,
        })) {
            Response::Read(result) => result.text,
            other => panic!("read failed: {other:?}"),
        };
        screen.contains(needle)
    });
    assert!(found, "never saw {needle:?}; screen:\n{screen}");
}

fn submit(client: &mut TestClient, id: &session_core::ids::SessionId, text: &str) {
    // As the account service types a reply in: only into Cursor.
    let response = client.request(&Request::SessionSubmit {
        id: id.clone(),
        text: text.into(),
        quiet_ms: Some(10_000),
        expect_foreground: Some("cursor-agent".into()),
    });
    assert_eq!(response, Response::Ok, "submit {text:?}");
}

fn transcript(
    client: &mut TestClient,
    id: &session_core::ids::SessionId,
    after: Option<session_core::api::TranscriptCursor>,
) -> session_core::api::TranscriptPage {
    match client.request(&Request::SessionTranscript {
        id: id.clone(),
        after,
        max_bytes: None,
    }) {
        Response::Transcript { transcript } => transcript,
        other => panic!("transcript failed: {other:?}"),
    }
}

fn replies(client: &mut TestClient) -> Vec<session_core::api::AgentReply> {
    match client.request(&Request::ReplyList { after: None }) {
        Response::Replies { replies, .. } => replies,
        other => panic!("replies failed: {other:?}"),
    }
}

#[test]
fn cursors_chat_reaches_the_session_feed_places_and_revive() {
    let home = TestHome::new();
    // The folder is a repository, so where the agent writes is a place.
    std::fs::create_dir_all(home.work.join(".git/refs/heads")).unwrap();
    std::fs::write(home.work.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
    let bin = home.dir.path().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let script = bin.join("cursor-agent");
    std::fs::write(&script, STAND_IN).unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let cursor_home = |name: &str| home.dir.path().join(name).display().to_string();
    let path = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    // Cursor's directories are the daemon's, and the session's.
    let cursor_env = vec![
        (
            "CURSOR_CONFIG_DIR".to_string(),
            cursor_home("cursor-config"),
        ),
        ("CURSOR_DATA_DIR".to_string(), cursor_home("cursor-data")),
        ("PATH".to_string(), path.clone()),
    ];
    let mut env = home.env();
    env.retain(|(name, _)| name != "PATH");
    env.extend(cursor_env.clone());
    let (mut daemon, registration) = start_daemon(&home, &cursor_env);
    let mut client = TestClient::connect(&registration.socket);
    assert!(matches!(
        client.hello(&registration.token),
        Response::Hello(_)
    ));
    assert_eq!(
        client.request(&Request::ReplyCapture { enabled: true }),
        Response::Ok
    );
    let session = match client.request(&Request::SessionCreate(CreateSession {
        scope: Some(ScopeRoot::resolve(&home.work).unwrap()),
        name: Some("cursor".into()),
        cwd: None,
        argv: Some(bash_argv()),
        startup_cmds: Vec::new(),
        env: env.clone(),
        cols: 100,
        rows: 30,
        machine: None,
        prefill: None,
    })) {
        Response::Session { session } => session,
        other => panic!("create failed: {other:?}"),
    };
    let id = session.id.clone();
    wait_screen(&mut client, &id, "$");
    client.submit(&id, "cursor-agent --model gpt-5 --yolo");
    wait_screen(&mut client, &id, "CURSOR_STARTED");
    wait_view(&mut client, &id, "Cursor at rest", |view| {
        view.agent
            .as_ref()
            .is_some_and(|a| a.kind == "cursor-agent")
            && view.lifecycle == Lifecycle::Idle
    });

    // A prompt typed in as a reply is: a paste, then Enter.
    submit(&mut client, &id, "first prompt");
    wait_view(&mut client, &id, "working", |view| {
        view.lifecycle == Lifecycle::Working
    });
    let done = wait_view(&mut client, &id, "done with its reply read", |view| {
        view.lifecycle == Lifecycle::Done
            && view.agent_output.as_deref() == Some("Done: first prompt")
    });
    assert_eq!(done.user_prompt.as_deref(), Some("first prompt"));
    let turn = done.agent_turn.clone().expect("the turn");
    assert_eq!(turn.conversation_id.as_deref(), Some(CHAT));
    assert_eq!(turn.messages, 3, "two texts and a tool call");
    assert!(done.last_message_at.is_some(), "the transcript's time");
    match client.request(&Request::SessionTurn { id: id.clone() }) {
        Response::Turn { turn: content } => {
            assert_eq!(content.id, turn.id);
            assert_eq!(content.prompt.as_deref(), Some("first prompt"));
            assert_eq!(
                content.messages,
                [
                    "I will write it down.",
                    "Write: notes/1.txt",
                    "Done: first prompt"
                ]
            );
        }
        other => panic!("turn failed: {other:?}"),
    }

    // Its reply, for the feed: the text after its last tool call, without
    // the redacted reasoning Cursor joins to it.
    let mut captured = Vec::new();
    assert!(
        wait_until(Duration::from_secs(15), || {
            captured = replies(&mut client);
            !captured.is_empty()
        }),
        "no reply captured"
    );
    assert_eq!(captured[0].agent, "cursor-agent");
    assert_eq!(captured[0].conversation_id.as_deref(), Some(CHAT));
    assert_eq!(captured[0].text, "Done: first prompt");
    assert!(captured[0].position.is_some());

    // The whole chat.
    let page = transcript(&mut client, &id, None);
    assert_eq!(page.agent, "cursor-agent");
    assert_eq!(page.conversation_id, CHAT);
    let roles: Vec<TranscriptRole> = page.messages.iter().map(|m| m.role).collect();
    assert_eq!(
        roles,
        [
            TranscriptRole::User,
            TranscriptRole::Assistant,
            TranscriptRole::Assistant
        ]
    );
    assert_eq!(
        captured[0].position,
        page.messages.last().map(|m| m.position)
    );

    // Where it worked: the folder it wrote a file in.
    let repo = std::fs::canonicalize(&home.work).unwrap();
    wait_view(&mut client, &id, "a place", |view| {
        view.places
            .iter()
            .any(|place| place.path == repo && place.edited)
    });

    // How it comes back.
    let resume = wait_view(&mut client, &id, "resumable", |view| {
        view.resume
            .as_ref()
            .is_some_and(|resume| resume.source == ResumeSource::KnownAgent)
    })
    .resume
    .unwrap();
    assert_eq!(
        resume.argv,
        [
            "cursor-agent",
            "--model",
            "gpt-5",
            "--yolo",
            "--resume",
            CHAT
        ]
    );

    // A reply written back: typed in, it shows as the chat's next prompt
    // (Cursor writes the file whole for it, dropping the last turn's end).
    submit(&mut client, &id, "second prompt");
    let next = wait_view(&mut client, &id, "the reply as a new turn", |view| {
        view.user_prompt.as_deref() == Some("second prompt")
            && view.agent_turn.as_ref().is_some_and(|t| t.id != turn.id)
    });
    assert_eq!(
        next.agent_turn.unwrap().conversation_id.as_deref(),
        Some(CHAT)
    );
    wait_view(&mut client, &id, "done again", |view| {
        view.agent_output.as_deref() == Some("Done: second prompt")
    });
    assert!(
        wait_until(Duration::from_secs(15), || replies(&mut client).len() == 2),
        "no second reply"
    );
    assert_eq!(replies(&mut client)[1].text, "Done: second prompt");
    // Read on from the first page's cursor: the second turn, its prompt too.
    let more = transcript(&mut client, &id, Some(page.cursor.clone()));
    assert_eq!(more.messages.len(), 3, "{:#?}", more.messages);
    assert_eq!(more.messages[0].role, TranscriptRole::User);

    // Killed, then revived: the chat is resumed, flags and all.
    assert!(matches!(
        client.request(&Request::SessionKill {
            id: id.clone(),
            tree: false
        }),
        Response::Killed { .. }
    ));
    wait_view(&mut client, &id, "ended", |view| {
        view.status != SessionStatus::Live
    });
    match client.request(&Request::SessionRevive(ReviveSession {
        id: id.clone(),
        env,
        cols: 100,
        rows: 30,
        resume: true,
    })) {
        Response::Session { .. } => {}
        other => panic!("revive failed: {other:?}"),
    }
    wait_screen(
        &mut client,
        &id,
        &format!("CURSOR_STARTED --model gpt-5 --yolo --resume {CHAT}"),
    );

    assert_eq!(client.request(&Request::DaemonShutdown), Response::Ok);
    assert!(wait_until(TIMEOUT, || daemon
        .0
        .try_wait()
        .unwrap()
        .is_some()));
}
