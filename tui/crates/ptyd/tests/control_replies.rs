//! Reply capture: a finished turn's reply, read from the agent's own log,
//! reaches only user subscriptions that ask for replies, and only while
//! capture is on.
#![cfg(target_os = "linux")]

mod control_common;

use control_common::*;
use ptyd::control_api::Control;
use ptyd::detect::{DetectInput, Detection};
use ptyd::host::ProcessInfo;
use session_core::api::{
    AgentReply, ErrorCode, EventEnvelope, Request, Response, SessionEvent, SubscribeRequest,
    TranscriptPart, TranscriptRole,
};
use session_core::ids::SessionId;
use session_core::model::Lifecycle;
use std::fs::File;
use std::io::Write;
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

const THREAD: &str = "019a1b2c-3d4e-7f60-8a9b-0c1d2e3f4a5b";

/// The daemon finds a Codex log among the files this process has open, so
/// tests that hold one take turns.
static OPEN_LOG: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn one_log_at_a_time() -> std::sync::MutexGuard<'static, ()> {
    OPEN_LOG
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Like `marker_detector`, for a session running Codex.
fn codex_detector(input: &DetectInput<'_>) -> Detection {
    Detection {
        agent: Some("codex".into()),
        ..marker_detector(input)
    }
}

/// A Codex rollout file, held open by this process as Codex holds its own.
fn rollout(h: &Harness) -> File {
    let directory = h.dir.path().join("codex");
    std::fs::create_dir_all(&directory).unwrap();
    let path = directory.join(format!("rollout-2026-10-23T10-00-00-{THREAD}.jsonl"));
    let mut file = File::create(path).unwrap();
    let meta =
        serde_json::json!({"type": "session_meta", "payload": {"id": THREAD, "source": "cli"}});
    writeln!(file, "{meta}").unwrap();
    file
}

fn event(file: &mut File, payload: serde_json::Value) {
    writeln!(
        file,
        "{}",
        serde_json::json!({"type": "event_msg", "payload": payload})
    )
    .unwrap();
}

/// Start a session running "Codex" (this process) with its log open.
fn codex_session(h: &Harness) -> (SessionId, File) {
    let session = h.create(Some("codex"));
    let file = rollout(h);
    h.host.set_foreground(
        &session.id,
        ProcessInfo {
            pid: std::process::id(),
            name: "codex".into(),
            cmdline: vec!["codex".into()],
            exe: None,
        },
    );
    (session.id, file)
}

fn settle(h: &Harness, id: &SessionId, lifecycle: Lifecycle) {
    eventually(&format!("{id} to become {lifecycle:?}"), || {
        (h.get(id).lifecycle == lifecycle).then_some(())
    });
}

/// One turn: working, the reply in the log, then finished.
fn turn(h: &Harness, id: &SessionId, file: &mut File, turn: &str, reply: &str) {
    h.host.show(id, "thinking [working]");
    settle(h, id, Lifecycle::Working);
    event(
        file,
        serde_json::json!({"type": "user_message", "message": "go"}),
    );
    event(
        file,
        serde_json::json!({"type": "task_complete", "turn_id": turn, "last_agent_message": reply}),
    );
    h.host.show(id, "> [idle]");
    settle(h, id, Lifecycle::Done);
}

fn replies_in(events: &Receiver<EventEnvelope>, wait: Duration) -> Vec<AgentReply> {
    let deadline = Instant::now() + wait;
    let mut replies = Vec::new();
    while let Some(left) = deadline.checked_duration_since(Instant::now()) {
        match events.recv_timeout(left) {
            Ok(EventEnvelope {
                event: SessionEvent::Reply { reply },
                ..
            }) => replies.push(reply),
            Ok(_) => {}
            Err(_) => break,
        }
    }
    replies
}

fn subscribe(h: &Harness, replies: bool) -> Receiver<EventEnvelope> {
    h.control
        .subscribe(
            &h.user,
            &SubscribeRequest {
                all: true,
                replies,
                ..Default::default()
            },
        )
        .unwrap()
}

#[test]
fn a_finished_turns_reply_reaches_only_subscriptions_that_ask() {
    let _log = one_log_at_a_time();
    let h = Harness::with_detector(codex_detector);
    let (id, mut file) = codex_session(&h);
    assert_eq!(
        h.handle(&h.user, Request::ReplyCapture { enabled: true }),
        Response::Ok
    );
    let replies = subscribe(&h, true);
    let plain = subscribe(&h, false);

    turn(&h, &id, &mut file, "t1", "Done: the tests pass.");
    let got = replies_in(&replies, Duration::from_secs(3));
    assert_eq!(got.len(), 1, "{got:?}");
    assert_eq!(got[0].session_id, id);
    assert_eq!(got[0].agent, "codex");
    assert_eq!(got[0].conversation_id.as_deref(), Some(THREAD));
    assert_eq!(got[0].text, "Done: the tests pass.");
    // Where the turn ends in the chat: its `task_complete` line.
    assert!(got[0].position.is_some_and(|position| position > 0));
    assert!(replies_in(&plain, Duration::from_millis(300)).is_empty());

    // Listed for a late reader, then nothing after it.
    match h.handle(&h.user, Request::ReplyList { after: None }) {
        Response::Replies { replies, capturing } => {
            assert!(capturing);
            assert_eq!(replies, got);
        }
        other => panic!("expected replies, got {other:?}"),
    }
    match h.handle(
        &h.user,
        Request::ReplyList {
            after: Some(got[0].id.clone()),
        },
    ) {
        Response::Replies { replies, .. } => assert!(replies.is_empty()),
        other => panic!("expected replies, got {other:?}"),
    }

    // The same reply is never captured twice; the next turn's is.
    h.host.show(&id, "> [idle] ");
    assert!(replies_in(&replies, Duration::from_millis(600)).is_empty());
    h.handle(&h.user, Request::SessionTouch { id: id.clone() });
    turn(&h, &id, &mut file, "t2", "Second answer.");
    let got = replies_in(&replies, Duration::from_secs(3));
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].text, "Second answer.");
}

#[test]
fn nothing_is_captured_while_capture_is_off() {
    let _log = one_log_at_a_time();
    let h = Harness::with_detector(codex_detector);
    let (id, mut file) = codex_session(&h);
    let replies = subscribe(&h, true);
    turn(&h, &id, &mut file, "t1", "Not kept.");
    assert!(replies_in(&replies, Duration::from_millis(1500)).is_empty());
    match h.handle(&h.user, Request::ReplyList { after: None }) {
        Response::Replies { replies, capturing } => {
            assert!(!capturing);
            assert!(replies.is_empty());
        }
        other => panic!("expected replies, got {other:?}"),
    }
}

#[test]
fn sessions_cannot_capture_list_or_subscribe_to_replies() {
    let h = Harness::with_detector(codex_detector);
    let session = h.create(Some("agent"));
    let agent = h.session_caller(&session.id);
    for request in [
        Request::ReplyCapture { enabled: true },
        Request::ReplyList { after: None },
    ] {
        match h.handle(&agent, request) {
            Response::Error { error } => assert_eq!(error.code, ErrorCode::PermissionDenied),
            other => panic!("expected a denial, got {other:?}"),
        }
    }
    let error = h
        .control
        .subscribe(
            &agent,
            &SubscribeRequest {
                replies: true,
                ..Default::default()
            },
        )
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::PermissionDenied);
}

#[test]
fn a_turns_chat_is_named_for_its_one_automatic_summary() {
    let _log = one_log_at_a_time();
    let h = Harness::with_detector(codex_detector);
    let (id, mut file) = codex_session(&h);
    h.host.show(&id, "thinking [working]");
    settle(&h, &id, Lifecycle::Working);
    event(
        &mut file,
        serde_json::json!({"type": "user_message", "message": "Fix the parser"}),
    );
    event(
        &mut file,
        serde_json::json!({"type": "agent_message", "message": "Looking at it."}),
    );
    // Every client sees the turn's counts and its chat, never its text. The log
    // is read at most once a second, while the agent's screen changes.
    let mut frame = 0;
    let conversation = eventually("the turn to be followed", || {
        frame += 1;
        h.host.show(&id, &format!("still [working] {frame}"));
        h.get(&id)
            .agent_turn
            .filter(|turn| turn.messages >= 1)
            .map(|turn| turn.conversation_id)
    });
    assert_eq!(conversation.as_deref(), Some(THREAD));
}

#[test]
fn a_running_agents_whole_chat_is_read_on_from_a_cursor() {
    let _log = one_log_at_a_time();
    let h = Harness::with_detector(codex_detector);
    let (id, mut file) = codex_session(&h);
    h.host.show(&id, "thinking [working]");
    settle(&h, &id, Lifecycle::Working);
    event(
        &mut file,
        serde_json::json!({"type": "user_message", "message": "Fix the parser"}),
    );
    let item = |file: &mut File, payload: serde_json::Value| {
        writeln!(
            file,
            "{}",
            serde_json::json!({"type": "response_item", "payload": payload})
        )
        .unwrap();
    };
    item(
        &mut file,
        serde_json::json!({"type": "function_call", "name": "exec_command", "call_id": "call_1",
            "arguments": "{\"cmd\":\"cargo test\"}"}),
    );
    item(
        &mut file,
        serde_json::json!({"type": "function_call_output", "call_id": "call_1", "output": "ok"}),
    );
    item(
        &mut file,
        serde_json::json!({"type": "message", "role": "assistant",
            "content": [{"type": "output_text", "text": "Fixed."}]}),
    );
    let read = |after| {
        h.handle(
            &h.user,
            Request::SessionTranscript {
                id: id.clone(),
                after,
                max_bytes: None,
            },
        )
    };
    // Once the daemon follows the agent, its chat is there to read.
    let mut frame = 0;
    let page = eventually("the chat to be readable", || {
        frame += 1;
        h.host.show(&id, &format!("still [working] {frame}"));
        match read(None) {
            Response::Transcript { transcript } => Some(transcript),
            _ => None,
        }
    });
    assert_eq!(page.agent, "codex");
    assert_eq!(page.conversation_id, THREAD);
    let roles: Vec<_> = page.messages.iter().map(|message| message.role).collect();
    assert_eq!(
        roles,
        [
            TranscriptRole::User,
            TranscriptRole::Assistant,
            TranscriptRole::Tool,
            TranscriptRole::Assistant
        ]
    );
    assert!(page
        .messages
        .windows(2)
        .all(|pair| pair[0].position < pair[1].position));
    assert_eq!(
        page.messages[3].parts,
        [TranscriptPart::Text {
            text: "Fixed.".into(),
            truncated: false
        }]
    );
    match read(Some(page.cursor.clone())) {
        Response::Transcript { transcript } => assert!(transcript.messages.is_empty()),
        other => panic!("expected a transcript, got {other:?}"),
    }
    // Sessions can't read chats; a session without an agent has none.
    let agent = h.session_caller(&id);
    let request = Request::SessionTranscript {
        id: id.clone(),
        after: None,
        max_bytes: None,
    };
    match h.handle(&agent, request) {
        Response::Error { error } => assert_eq!(error.code, ErrorCode::PermissionDenied),
        other => panic!("expected a denial, got {other:?}"),
    }
    let shell = h.create(Some("shell"));
    let request = Request::SessionTranscript {
        id: shell.id,
        after: None,
        max_bytes: None,
    };
    match h.handle(&h.user, request) {
        Response::Error { error } => assert_eq!(error.code, ErrorCode::NotFound),
        other => panic!("expected no chat, got {other:?}"),
    }
}
