//! A reply typed into Claude Code while it works: Claude Code queues it and
//! logs it, once it takes it at its next step, as a `queued_command`
//! attachment. The daemon shows that as a new turn whose prompt is the reply,
//! while the agent still works (how the account service sees a reply typed
//! mid-turn delivered), and the turn's reply after it is what's captured as
//! its answer.
//!
//! A turn that ends with the next prompt, queued while it worked, before its
//! session is seen waiting keeps its own reply; one seen finishing is
//! captured once.
//!
//! Its own test binary: it points `CLAUDE_CONFIG_DIR` at a folder of its own
//! (one for every test here, each with its own pid and conversation), and
//! nothing else in this process reads the environment.
#![cfg(target_os = "linux")]

mod control_common;

use control_common::*;
use ptyd::control_api::Control;
use ptyd::detect::{DetectInput, Detection};
use ptyd::host::ProcessInfo;
use serde_json::{json, Value};
use session_core::api::{
    AgentReply, EventEnvelope, Request, Response, SessionEvent, SubscribeRequest,
};
use session_core::model::Lifecycle;
use std::fs::File;
use std::io::Write;
use std::time::{Duration, Instant};

const CONVERSATION: &str = "5f1c2a90-3b7e-4d1a-9c2e-6a8b0d4f1e23";
/// Claude Code's pid as the stand-in shows it: only its `sessions/` file is read.
const PID: u32 = 4242;

/// Like `marker_detector`, for a session running Claude Code.
fn claude_detector(input: &DetectInput<'_>) -> Detection {
    Detection {
        agent: Some("claude".into()),
        ..marker_detector(input)
    }
}

fn entry(log: &mut File, entry: Value) {
    writeln!(log, "{entry}").unwrap();
}

fn assistant(uuid: &str, content: Value) -> Value {
    json!({"type": "assistant", "uuid": uuid, "timestamp": "2026-10-30T10:00:05.000Z",
        "message": {"role": "assistant", "content": content}})
}

/// The Claude Code config folder every test here shares (`CLAUDE_CONFIG_DIR`).
fn claude_config() -> &'static std::path::Path {
    static CONFIG: std::sync::OnceLock<tempfile::TempDir> = std::sync::OnceLock::new();
    CONFIG
        .get_or_init(|| {
            let config = tempfile::tempdir().unwrap();
            std::env::set_var("CLAUDE_CONFIG_DIR", config.path());
            config
        })
        .path()
}

/// Claude Code `pid`'s conversation's log, as Claude Code keeps it.
fn conversation_log(pid: u32, conversation: &str) -> File {
    let config = claude_config();
    std::fs::create_dir_all(config.join("sessions")).unwrap();
    std::fs::write(
        config.join("sessions").join(format!("{pid}.json")),
        json!({"sessionId": conversation, "pid": pid}).to_string(),
    )
    .unwrap();
    let project = config.join("projects").join("-home-me-project");
    std::fs::create_dir_all(&project).unwrap();
    File::create(project.join(format!("{conversation}.jsonl"))).unwrap()
}

#[test]
fn a_prompt_queued_mid_turn_is_a_new_turn_with_that_prompt_and_its_reply_answers_it() {
    let mut log = conversation_log(PID, CONVERSATION);

    let h = Harness::with_detector(claude_detector);
    assert_eq!(
        h.handle(&h.user, Request::ReplyCapture { enabled: true }),
        Response::Ok
    );
    let events = h
        .control
        .subscribe(
            &h.user,
            &SubscribeRequest {
                all: true,
                replies: true,
                ..Default::default()
            },
        )
        .unwrap();
    let id = h.create(Some("claude")).id;
    h.host.set_foreground(
        &id,
        ProcessInfo {
            pid: PID,
            name: "claude".into(),
            cmdline: vec!["claude".into()],
            exe: None,
        },
    );

    // A turn at work: the prompt, then a tool call.
    entry(
        &mut log,
        json!({"type": "user", "uuid": "u1", "timestamp": "2026-10-30T10:00:00.000Z",
            "message": {"role": "user", "content": "Fix the parser"}}),
    );
    entry(
        &mut log,
        assistant(
            "a1",
            json!([{"type": "tool_use", "id": "t1", "name": "Bash",
                "input": {"command": "cargo test", "description": "Run the tests"}}]),
        ),
    );
    // The log is read at most once a second, while the agent's screen changes.
    let mut frame = 0;
    let mut working = |h: &Harness| {
        frame += 1;
        h.host.show(&id, &format!("thinking [working] {frame}"));
    };
    let first = eventually("the first turn", || {
        working(&h);
        let view = h.get(&id);
        (view.user_prompt.as_deref() == Some("Fix the parser")).then_some(view.agent_turn?)
    });
    assert_eq!(first.conversation_id.as_deref(), Some(CONVERSATION));

    // A reply typed meanwhile: Claude Code takes it after the tool call.
    entry(
        &mut log,
        json!({"type": "user", "uuid": "u2", "timestamp": "2026-10-30T10:00:20.000Z",
            "message": {"role": "user", "content": [
                {"type": "tool_result", "tool_use_id": "t1", "content": "ok"}]}}),
    );
    entry(
        &mut log,
        json!({"type": "attachment", "uuid": "q1", "timestamp": "2026-10-30T10:00:21.000Z",
            "attachment": {"type": "queued_command", "prompt": "Also check the web app",
                "commandMode": "prompt", "origin": {"kind": "human"}}}),
    );
    let taken = eventually("the queued prompt's turn", || {
        working(&h);
        let view = h.get(&id);
        (view.user_prompt.as_deref() == Some("Also check the web app")).then_some(view)
    });
    let turn = taken.agent_turn.expect("its turn");
    assert_ne!(turn.id, first.id, "a new turn");
    assert_eq!(turn.conversation_id.as_deref(), Some(CONVERSATION));
    assert_eq!(taken.lifecycle, Lifecycle::Working, "still at work");

    // The turn goes on and ends: what it said after the queued prompt is
    // the reply captured.
    entry(
        &mut log,
        assistant(
            "a2",
            json!([{"type": "text", "text": "Checked both: the parser and the web app."}]),
        ),
    );
    working(&h);
    std::thread::sleep(Duration::from_millis(1_100));
    working(&h);
    h.host.show(&id, "> [idle]");
    eventually("the turn to end", || {
        (h.get(&id).lifecycle == Lifecycle::Done).then_some(())
    });
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut replies: Vec<AgentReply> = Vec::new();
    while let Some(left) = deadline.checked_duration_since(Instant::now()) {
        match events.recv_timeout(left) {
            Ok(EventEnvelope {
                event: SessionEvent::Reply { reply },
                ..
            }) => {
                replies.push(reply);
                break;
            }
            Ok(_) => {}
            Err(_) => break,
        }
    }
    assert_eq!(replies.len(), 1, "{replies:?}");
    assert_eq!(replies[0].agent, "claude-code");
    assert_eq!(replies[0].conversation_id.as_deref(), Some(CONVERSATION));
    assert_eq!(replies[0].text, "Checked both: the parser and the web app.");
}

/// Now, as Claude Code stamps its log's entries (RFC 3339, UTC, in ms), and
/// as Unix milliseconds.
fn stamp() -> (String, i64) {
    let ms = now_ms();
    let (days, of_day) = (ms.div_euclid(86_400_000), ms.rem_euclid(86_400_000));
    // Civil from days (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    let text = format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{:03}Z",
        of_day / 3_600_000,
        of_day / 60_000 % 60,
        of_day / 1_000 % 60,
        of_day % 1_000
    );
    (text, ms)
}

/// The user's prompt, typed at Claude Code's prompt; returns when.
fn typed(log: &mut File, uuid: &str, text: &str) -> i64 {
    let (at, ms) = stamp();
    entry(
        log,
        json!({"type": "user", "uuid": uuid, "timestamp": at,
            "message": {"role": "user", "content": text}}),
    );
    ms
}

/// What the agent said; returns when.
fn said(log: &mut File, uuid: &str, text: &str) -> i64 {
    let (at, ms) = stamp();
    entry(
        log,
        json!({"type": "assistant", "uuid": uuid, "timestamp": at,
            "message": {"role": "assistant", "content": [{"type": "text", "text": text}]}}),
    );
    ms
}

/// A session of Claude Code `pid` with capture on, and its replies' events.
fn claude_session(
    pid: u32,
) -> (
    Harness,
    session_core::ids::SessionId,
    std::sync::mpsc::Receiver<EventEnvelope>,
) {
    let h = Harness::with_detector(claude_detector);
    assert_eq!(
        h.handle(&h.user, Request::ReplyCapture { enabled: true }),
        Response::Ok
    );
    let events = h
        .control
        .subscribe(
            &h.user,
            &SubscribeRequest {
                all: true,
                replies: true,
                ..Default::default()
            },
        )
        .unwrap();
    let id = h.create(Some("claude")).id;
    h.host.set_foreground(
        &id,
        ProcessInfo {
            pid,
            name: "claude".into(),
            cmdline: vec!["claude".into()],
            exe: None,
        },
    );
    (h, id, events)
}

fn replies_in(
    events: &std::sync::mpsc::Receiver<EventEnvelope>,
    wait: Duration,
) -> Vec<AgentReply> {
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

/// Keep the screen at work (the log is read while it changes) until `f`.
fn working_until<T>(
    h: &Harness,
    id: &session_core::ids::SessionId,
    what: &str,
    mut f: impl FnMut(&session_core::model::SessionView) -> Option<T>,
) -> T {
    let mut frame = 0;
    eventually(what, || {
        frame += 1;
        h.host.show(id, &format!("thinking [working] {frame}"));
        f(&h.get(id))
    })
}

/// The screen waiting, until the session is `Done`.
fn finishes(h: &Harness, id: &session_core::ids::SessionId) {
    h.host.show(id, "> [idle]");
    eventually("the turn to end", || {
        (h.get(id).lifecycle == Lifecycle::Done).then_some(())
    });
}

#[test]
fn a_turn_its_queued_prompt_ended_before_it_was_seen_waiting_keeps_its_own_reply() {
    const PID: u32 = 4343;
    const CONVERSATION: &str = "7a2c4e60-1b3d-4f5a-8c9e-0d1f2a3b4c5d";
    let mut log = conversation_log(PID, CONVERSATION);
    let (h, id, events) = claude_session(PID);
    typed(&mut log, "u1", "Fix the parser");
    working_until(&h, &id, "the prompt read", |view| {
        (view.user_prompt.as_deref() == Some("Fix the parser")).then_some(())
    });
    let answered_at = said(&mut log, "a1", "Fixed: the parser skips blank lines.");
    working_until(&h, &id, "its answer read", |view| {
        (view.agent_output.as_deref() == Some("Fixed: the parser skips blank lines.")).then_some(())
    });
    // Its turn ends, and Claude Code starts on the prompt queued meanwhile
    // at once: the session is never seen waiting in between.
    let (at, _) = stamp();
    entry(
        &mut log,
        json!({"type": "attachment", "uuid": "q1", "timestamp": at,
            "attachment": {"type": "queued_command", "prompt": "MIDTURN ping 4242",
                "commandMode": "prompt", "origin": {"kind": "human"}}}),
    );
    working_until(&h, &id, "the queued prompt's turn", |view| {
        (view.user_prompt.as_deref() == Some("MIDTURN ping 4242")).then_some(())
    });
    let held = replies_in(&events, Duration::from_secs(2));
    assert_eq!(held.len(), 1, "{held:?}");
    assert_eq!(held[0].text, "Fixed: the parser skips blank lines.");
    assert_eq!(held[0].conversation_id.as_deref(), Some(CONVERSATION));
    // Finished when it was said: before the queued prompt was taken, so it
    // isn't that prompt's answer.
    assert_eq!(held[0].finished_at, answered_at);
    assert_eq!(h.get(&id).lifecycle, Lifecycle::Working);

    // The queued prompt's turn, seen to end: its reply is its answer.
    said(&mut log, "a2", "pong 4242");
    working_until(&h, &id, "the answer read", |view| {
        (view.agent_output.as_deref() == Some("pong 4242")).then_some(())
    });
    finishes(&h, &id);
    let answer = replies_in(&events, Duration::from_secs(3));
    assert_eq!(answer.len(), 1, "{answer:?}");
    assert_eq!(answer[0].text, "pong 4242");
    assert!(answer[0].finished_at > held[0].finished_at);
}

#[test]
fn a_turn_seen_ending_is_captured_once_and_never_late() {
    const PID: u32 = 4344;
    const CONVERSATION: &str = "8b3d5f71-2c4e-4a6b-9d0f-1e2a3b4c5d6e";
    let mut log = conversation_log(PID, CONVERSATION);
    let (h, id, events) = claude_session(PID);
    let mut turn = |h: &Harness, n: u32, prompt: &str, reply: &str| {
        typed(&mut log, &format!("u{n}"), prompt);
        working_until(h, &id, "the prompt read", |view| {
            (view.user_prompt.as_deref() == Some(prompt)).then_some(())
        });
        said(&mut log, &format!("a{n}"), reply);
        working_until(h, &id, "the answer read", |view| {
            (view.agent_output.as_deref() == Some(reply)).then_some(())
        });
        finishes(h, &id);
        h.handle(&h.user, Request::SessionTouch { id: id.clone() });
    };
    // Each turn seen ending: its reply once, though the next prompt ends it
    // too.
    turn(&h, 1, "Fix the parser", "Fixed it.");
    turn(&h, 2, "Now the docs", "Docs written.");
    let texts = |replies: Vec<AgentReply>| -> Vec<String> {
        replies.into_iter().map(|reply| reply.text).collect()
    };
    assert_eq!(
        texts(replies_in(&events, Duration::from_secs(2))),
        ["Fixed it.", "Docs written."]
    );
    // Capture off as a turn ends: its reply isn't captured once capture is
    // back on and the next prompt ends that turn again.
    assert_eq!(
        h.handle(&h.user, Request::ReplyCapture { enabled: false }),
        Response::Ok
    );
    turn(&h, 3, "Then the tests", "Tests pass.");
    assert_eq!(
        h.handle(&h.user, Request::ReplyCapture { enabled: true }),
        Response::Ok
    );
    turn(&h, 4, "Ship it", "Shipped.");
    assert_eq!(
        texts(replies_in(&events, Duration::from_secs(2))),
        ["Shipped."]
    );
}

#[test]
fn a_turn_seen_ending_before_its_reply_reached_the_log_and_the_next_prompt_ended_it_is_the_due_one()
{
    const PID: u32 = 4345;
    const CONVERSATION: &str = "9c4e6a82-3d5f-4b7c-8e1a-2f3b4c5d6e7f";
    let mut log = conversation_log(PID, CONVERSATION);
    let (h, id, events) = claude_session(PID);
    typed(&mut log, "u1", "Fix the parser");
    working_until(&h, &id, "the prompt read", |view| {
        (view.user_prompt.as_deref() == Some("Fix the parser")).then_some(())
    });
    // Its screen waits before its last message reaches the log; by the
    // time that's read, the prompt queued meanwhile has begun the next turn.
    finishes(&h, &id);
    let seen_done = now_ms();
    std::thread::sleep(Duration::from_millis(20));
    said(&mut log, "a1", "Fixed it.");
    let (at, _) = stamp();
    entry(
        &mut log,
        json!({"type": "attachment", "uuid": "q1", "timestamp": at,
            "attachment": {"type": "queued_command", "prompt": "MIDTURN ping 4242",
                "commandMode": "prompt", "origin": {"kind": "human"}}}),
    );
    working_until(&h, &id, "the queued prompt's turn", |view| {
        (view.user_prompt.as_deref() == Some("MIDTURN ping 4242")).then_some(())
    });
    let held = replies_in(&events, Duration::from_secs(2));
    assert_eq!(held.len(), 1, "{held:?}");
    assert_eq!(held[0].text, "Fixed it.");
    assert!(held[0].finished_at <= seen_done, "as its end was seen");
    // Nothing more is due: the next turn's answer waits for its own end.
    said(&mut log, "a2", "pong 4242");
    working_until(&h, &id, "the answer read", |view| {
        (view.agent_output.as_deref() == Some("pong 4242")).then_some(())
    });
    assert!(replies_in(&events, Duration::from_millis(1_200)).is_empty());
    finishes(&h, &id);
    let answer = replies_in(&events, Duration::from_secs(3));
    assert_eq!(answer.len(), 1, "{answer:?}");
    assert_eq!(answer[0].text, "pong 4242");
    assert!(answer[0].finished_at > seen_done);
}
