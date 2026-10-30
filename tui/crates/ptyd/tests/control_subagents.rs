//! A Claude Code conversation read whole with its subagents
//! (`session_transcript` with `subagents`): each subagent's own transcript
//! comes after the main conversation's, marked with the call that started it
//! and its description, and one started in the background ends with its
//! notice as that call's result. Without `subagents`, the main conversation
//! alone, as before.
//!
//! Its own test binary: it points `CLAUDE_CONFIG_DIR` at a folder of its own,
//! and nothing else in this process reads the environment.
#![cfg(target_os = "linux")]

mod control_common;

use control_common::*;
use ptyd::detect::{DetectInput, Detection};
use ptyd::host::ProcessInfo;
use serde_json::{json, Value};
use session_core::api::{
    Request, Response, TranscriptPage, TranscriptPart, TranscriptRole, TranscriptSubagent,
};
use std::io::Write;

const CONVERSATION: &str = "5f1c2a90-3b7e-4d1a-9c2e-6a8b0d4f1e23";
const AGENT: &str = "a1b2c3d4e5f60718";
const PID: u32 = 4545;

fn claude_detector(input: &DetectInput<'_>) -> Detection {
    Detection {
        agent: Some("claude".into()),
        ..marker_detector(input)
    }
}

fn append(path: &std::path::Path, lines: &[Value]) {
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .unwrap();
    for line in lines {
        writeln!(file, "{line}").unwrap();
    }
}

fn entry(kind: &str, uuid: &str, content: Value) -> Value {
    json!({"type": kind, "uuid": uuid, "timestamp": "2026-10-30T10:00:00.000Z",
        "message": {"role": kind, "content": content}})
}

#[test]
fn a_conversation_is_read_with_its_subagents_marked_when_asked() {
    let config = tempfile::tempdir().unwrap();
    std::env::set_var("CLAUDE_CONFIG_DIR", config.path());
    std::fs::create_dir_all(config.path().join("sessions")).unwrap();
    std::fs::write(
        config.path().join("sessions").join(format!("{PID}.json")),
        json!({"sessionId": CONVERSATION, "pid": PID}).to_string(),
    )
    .unwrap();
    let project = config.path().join("projects").join("-home-me-project");
    let subagents = project.join(CONVERSATION).join("subagents");
    std::fs::create_dir_all(&subagents).unwrap();
    let main = project.join(format!("{CONVERSATION}.jsonl"));
    let mut ack = entry(
        "user",
        "m-ack",
        json!([{"type": "tool_result", "tool_use_id": "toolu_01AGENT",
            "content": "Async agent launched successfully."}]),
    );
    ack["toolUseResult"] = json!({"status": "async_launched"});
    append(
        &main,
        &[
            entry("user", "m-prompt", json!("Fix the flaky test")),
            entry(
                "assistant",
                "m-call",
                json!([{"type": "tool_use", "id": "toolu_01AGENT", "name": "Agent",
                    "input": {"description": "Find the flaky test", "prompt": "Look for it",
                        "run_in_background": true}}]),
            ),
            ack,
        ],
    );
    std::fs::write(
        subagents.join(format!("agent-{AGENT}.meta.json")),
        json!({"agentType": "Explore", "description": "Find the flaky test",
            "toolUseId": "toolu_01AGENT"})
        .to_string(),
    )
    .unwrap();
    let sidechain = |kind: &str, uuid: &str, content: Value| {
        let mut entry = entry(kind, uuid, content);
        entry["isSidechain"] = json!(true);
        entry["agentId"] = json!(AGENT);
        entry
    };
    append(
        &subagents.join(format!("agent-{AGENT}.jsonl")),
        &[
            sidechain("user", "s-prompt", json!("Look for it")),
            sidechain(
                "assistant",
                "s-answer",
                json!([{"type": "text", "text": "It's the map order."}]),
            ),
        ],
    );

    let h = Harness::with_detector(claude_detector);
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
    let read = |after, subagents| match h.handle(
        &h.user,
        Request::SessionTranscript {
            id: id.clone(),
            after,
            max_bytes: None,
            subagents,
        },
    ) {
        Response::Transcript { transcript } => Some(transcript),
        _ => None,
    };
    let mut frame = 0;
    let page: TranscriptPage = eventually("the chat to be readable", || {
        frame += 1;
        h.host.show(&id, &format!("thinking [working] {frame}"));
        read(None, true).filter(|page| !page.messages.is_empty())
    });
    let ids: Vec<&str> = page.messages.iter().map(|m| m.id.as_str()).collect();
    // The acknowledgement isn't the call's result: the subagent still works.
    assert_eq!(
        ids,
        [
            "m-prompt",
            "m-call",
            "a1b2c3d4e5f60718:s-prompt",
            "a1b2c3d4e5f60718:s-answer"
        ]
    );
    let marker = TranscriptSubagent {
        id: AGENT.into(),
        call_id: Some("toolu_01AGENT".into()),
        label: Some("Find the flaky test".into()),
    };
    assert_eq!(page.messages[2].subagent.as_ref(), Some(&marker));
    assert_eq!(page.messages[2].role, TranscriptRole::User);
    assert_eq!(page.cursor.subagents.len(), 1);

    // It ends: its notice is the call's result.
    append(
        &main,
        &[entry(
            "user",
            "m-notice",
            json!(format!(
                "<task-notification><task-id>{AGENT}</task-id>\
                 <tool-use-id>toolu_01AGENT</tool-use-id><status>completed</status>\
                 <summary>done</summary><result>It's the map order.</result></task-notification>"
            )),
        )],
    );
    let later = read(Some(page.cursor.clone()), true).unwrap();
    assert_eq!(later.messages.len(), 1);
    assert_eq!(later.messages[0].role, TranscriptRole::Tool);
    assert_eq!(
        later.messages[0].parts,
        [TranscriptPart::ToolResult {
            call_id: Some("toolu_01AGENT".into()),
            output: "It's the map order.".into(),
            error: false,
            truncated: false,
        }]
    );

    // Without subagents: the main conversation alone, as an older client reads it.
    let plain = read(None, false).unwrap();
    assert!(plain.messages.iter().all(|m| m.subagent.is_none()));
    assert_eq!(plain.messages.len(), 3);
}
