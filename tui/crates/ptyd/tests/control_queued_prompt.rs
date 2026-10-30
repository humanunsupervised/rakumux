//! A reply typed into Claude Code while it works: Claude Code queues it and
//! logs it, once it takes it at its next step, as a `queued_command`
//! attachment. The daemon shows that as a new turn whose prompt is the reply,
//! while the agent still works (how the account service sees a reply typed
//! mid-turn delivered), and the turn's reply after it is what's captured as
//! its answer.
//!
//! Its own test binary: it points `CLAUDE_CONFIG_DIR` at a folder of its own,
//! and nothing else in this process reads the environment meanwhile.
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

#[test]
fn a_prompt_queued_mid_turn_is_a_new_turn_with_that_prompt_and_its_reply_answers_it() {
    let config = tempfile::tempdir().unwrap();
    std::env::set_var("CLAUDE_CONFIG_DIR", config.path());
    std::fs::create_dir_all(config.path().join("sessions")).unwrap();
    std::fs::write(
        config.path().join("sessions").join(format!("{PID}.json")),
        json!({"sessionId": CONVERSATION, "pid": PID}).to_string(),
    )
    .unwrap();
    let project = config.path().join("projects").join("-home-me-project");
    std::fs::create_dir_all(&project).unwrap();
    let mut log = File::create(project.join(format!("{CONVERSATION}.jsonl"))).unwrap();

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
