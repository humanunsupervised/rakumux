//! Grok Build's conversations as the daemon follows them: which one a process
//! shows (`active_sessions.json`), its lines, turn and reply, the whole
//! conversation and where it worked. A binary of its own: it points
//! `GROK_HOME` at a synthetic home for its one test.

use ptyd::agent_output::places::PlaceScan;
use ptyd::agent_output::Follower;
use ptyd::host::ProcessInfo;
use serde_json::{json, Value};
use session_core::api::{TranscriptPart, TranscriptRole, MAX_REPLY_BYTES};
use std::io::Write;
use std::path::{Path, PathBuf};

const A: &str = "01a18b49-8533-7252-8d65-46555f07d0c5";
const B: &str = "01a18b4d-4043-74e1-920c-7f824c6b0d82";
const PID: u32 = 438_459;

/// A line of session `session`'s `updates.jsonl`.
fn line(session: &str, n: u32, update: Value, prompt: Option<&str>) -> String {
    let mut meta = json!({"eventId": format!("{session}-{n}"),
        "agentTimestampMs": 1_793_338_236_000_i64 + i64::from(n)});
    if let Some(prompt) = prompt {
        meta["promptId"] = json!(prompt);
    }
    let entry = json!({"timestamp": 1_793_338_236, "method": "session/update",
        "params": {"sessionId": session, "update": update, "_meta": meta}});
    format!("{entry}\n")
}

fn chunk(kind: &str, text: &str) -> Value {
    json!({"sessionUpdate": kind, "content": {"type": "text", "text": text}})
}

fn append(log: &Path, lines: &[String]) {
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log)
        .unwrap();
    for line in lines {
        file.write_all(line.as_bytes()).unwrap();
    }
}

fn session_log(home: &Path, id: &str) -> PathBuf {
    let dir = home.join("sessions/%2Fhome%2Fme%2Fproj").join(id);
    std::fs::create_dir_all(&dir).unwrap();
    dir.join("updates.jsonl")
}

#[test]
fn a_grok_conversation_is_followed_where_the_process_shows_it() {
    let home = tempfile::tempdir().unwrap();
    // The only test here: nothing else reads the environment meanwhile.
    std::env::set_var("GROK_HOME", home.path());
    let rows = json!([
        {"session_id": A, "pid": PID, "pid_start": 278276, "cwd": "/home/me/proj",
            "opened_at": "2026-10-30T05:30:13.720726878Z"},
        {"session_id": B, "pid": PID, "pid_start": 278276, "cwd": "/home/me/proj",
            "opened_at": "2026-10-30T05:34:18.211689838Z"},
    ]);
    std::fs::write(home.path().join("active_sessions.json"), rows.to_string()).unwrap();
    let process = ProcessInfo {
        pid: PID,
        name: "grok".into(),
        cmdline: vec!["grok".into()],
        exe: Some("/home/me/.grok/downloads/grok-linux-x86_64".into()),
    };
    // Nothing typed anywhere yet: no log, nothing to follow.
    assert!(Follower::locate(&process, "grok").is_none());

    let a = session_log(home.path(), A);
    let hook = json!({"sessionUpdate": "hook_execution", "event_name": "user_prompt_submit"});
    let write = json!({"sessionUpdate": "tool_call", "toolCallId": "call_1", "title": "write",
        "rawInput": {"file_path": "/home/me/proj/src/new.rs", "content": "fn main() {}\n"},
        "_meta": {"x.ai/tool": {"name": "write", "kind": "write", "label": "Write"}}});
    let written = json!({"sessionUpdate": "tool_call_update", "toolCallId": "call_1",
        "status": "completed", "content": [{"type": "diff", "path": "/home/me/proj/src/new.rs",
            "oldText": "", "newText": "fn main() {}\n"}],
        "rawOutput": {"type": "SearchReplace", "EditsApplied":
            {"tool_output_for_prompt": "The file /home/me/proj/src/new.rs has been created."}}});
    append(
        &a,
        &[
            line(A, 2, hook, None),
            line(A, 3, chunk("user_message_chunk", "Fix the parser"), None),
            line(
                A,
                4,
                chunk("agent_thought_chunk", "Looking at it."),
                Some("p1"),
            ),
            line(A, 5, write, Some("p1")),
            line(A, 6, written, Some("p1")),
        ],
    );
    let reply_at = std::fs::metadata(&a).unwrap().len();
    append(
        &a,
        &[
            line(
                A,
                7,
                chunk("agent_message_chunk", "Fixed: empty input parses."),
                Some("p1"),
            ),
            line(
                A,
                8,
                json!({"sessionUpdate": "turn_completed", "prompt_id": "p1",
                "stop_reason": "end_turn"}),
                None,
            ),
        ],
    );
    let mut follower = Follower::locate(&process, "grok").unwrap();
    assert_eq!(follower.agent(), "grok");
    assert!(follower.follow(None));
    assert_eq!(follower.conversation().as_deref(), Some(A));
    assert_eq!(follower.latest(), Some("Fixed: empty input parses."));
    assert_eq!(follower.prompt(), Some("Fix the parser"));
    let turn = follower.turn().clone();
    assert_eq!(turn.prompt.as_deref(), Some("Fix the parser"));
    assert_eq!(
        turn.messages,
        [
            "Looking at it.",
            "Write: src/new.rs",
            "Fixed: empty input parses."
        ]
    );
    let reply = follower.reply().unwrap().clone();
    assert_eq!(
        (reply.key.as_str(), reply.text.as_str()),
        ("p1", "Fixed: empty input parses.")
    );
    assert_eq!(reply.position, Some(reply_at));
    assert_eq!(follower.last_at(), Some(1_793_338_236_007));

    // A finished background subagent wakes it: Grok's prompt, not the user's,
    // so the turn goes on and its reply is the new one. A rewind's marker is
    // bookkeeping.
    append(
        &a,
        &[
            line(A, 9, chunk("user_message_chunk",
                "<system-reminder>\nWhile you were idle, 1 background subagent completed:\n</system-reminder>"), None),
            line(A, 10, chunk("agent_message_chunk", "Your helper found two files."),
                Some("subagent-completed-x")),
            line(A, 11, json!({"sessionUpdate": "rewind_marker", "target_prompt_index": 0}), None),
        ],
    );
    assert!(follower.follow(None));
    assert_eq!(follower.prompt(), Some("Fix the parser"));
    assert_eq!(follower.turn().id, turn.id);
    assert_eq!(follower.turn().total, 4);
    let reply = follower.reply().unwrap();
    assert_eq!(
        (reply.key.as_str(), reply.text.as_str()),
        ("subagent-completed-x", "Your helper found two files.")
    );

    // A long reply is cut.
    append(
        &a,
        &[
            line(A, 12, chunk("user_message_chunk", "Explain it all"), None),
            line(
                A,
                13,
                chunk("agent_message_chunk", &"Long reply line. ".repeat(3000)),
                Some("p2"),
            ),
        ],
    );
    assert!(follower.follow(None));
    let reply = follower.reply().unwrap();
    assert!(reply.truncated && reply.text.len() <= MAX_REPLY_BYTES);
    assert_eq!(reply.key, "p2");

    // `/new`, then a prompt there: the process shows that one now.
    let b = session_log(home.path(), B);
    append(
        &b,
        &[line(
            B,
            3,
            chunk("user_message_chunk", "Now the docs"),
            None,
        )],
    );
    assert!(follower.follow(None));
    assert_eq!(follower.conversation().as_deref(), Some(B));
    assert_eq!(follower.prompt(), Some("Now the docs"));
    assert_eq!(follower.turn().prompt.as_deref(), Some("Now the docs"));
    assert_eq!(follower.reply(), None);
    // Its hooks name the other one (the Agent Dashboard switched back to it).
    assert!(follower.follow(Some(A)));
    assert_eq!(follower.conversation().as_deref(), Some(A));
    assert_eq!(follower.prompt(), Some("Explain it all"));

    // The whole conversation.
    let source = follower.transcript_source().unwrap();
    assert_eq!(source.agent(), "grok");
    let page = source.read(None, None).unwrap();
    assert_eq!(page.conversation_id, A);
    assert!(!page.more);
    let roles: Vec<(TranscriptRole, &str)> = page
        .messages
        .iter()
        .map(|message| {
            let kind = match &message.parts[0] {
                TranscriptPart::Text { .. } => "text",
                TranscriptPart::Reasoning { .. } => "reasoning",
                TranscriptPart::ToolCall { .. } => "tool_call",
                TranscriptPart::ToolResult { .. } => "tool_result",
                _ => "other",
            };
            (message.role, kind)
        })
        .collect();
    assert_eq!(
        roles,
        [
            (TranscriptRole::User, "text"),
            (TranscriptRole::Assistant, "reasoning"),
            (TranscriptRole::Assistant, "tool_call"),
            (TranscriptRole::Tool, "tool_result"),
            (TranscriptRole::Assistant, "text"),
            (TranscriptRole::Assistant, "text"),
            (TranscriptRole::User, "text"),
            (TranscriptRole::Assistant, "text"),
        ]
    );
    assert_eq!(page.messages[0].id, format!("{A}-3"));
    let reply_message = &page.messages[4];
    assert_eq!(reply_message.position, reply_at);
    // Read on from its cursor: nothing new.
    assert!(source
        .read(Some(&page.cursor), None)
        .unwrap()
        .messages
        .is_empty());

    // Where it worked: the file it wrote.
    let mut scan = PlaceScan::new(source, Some("/home/me/proj".into()));
    let touches = scan.read(1024 * 1024);
    assert!(touches
        .iter()
        .any(|touch| touch.dir == Path::new("/home/me/proj/src") && touch.edited));
    std::env::remove_var("GROK_HOME");
}
