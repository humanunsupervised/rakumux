//! A restart in place carries an agent's session over with nothing new on its
//! screen (Claude Code waiting for a prompt draws nothing): the new daemon
//! still follows its conversation. It read a session's screen and log only on
//! output, so after `S` an idle session had no turn, no latest line and no chat
//! (`session_transcript`: "no agent conversation is known") until someone typed
//! in it, and rakumux.com showed it without a conversation (2026-10-31).
//!
//! Its own test binary: it points `CLAUDE_CONFIG_DIR` at a folder of its own,
//! and nothing else in this process reads the environment.
#![cfg(target_os = "linux")]

mod control_common;

use control_common::*;
use ptyd::control::{ControlConfig, ControlService, Limits};
use ptyd::control_api::Control;
use ptyd::detect::{DetectInput, Detection};
use ptyd::host::ProcessInfo;
use serde_json::json;
use session_core::api::{Request, Response};
use std::io::Write;
use std::time::{Duration, Instant};

const CONVERSATION: &str = "4b7e2f90-1a3c-4d5e-8f60-7a8b9c0d1e2f";
/// Claude Code's pid as the stand-in shows it: only its `sessions/` file is read.
const PID: u32 = 4747;

fn claude_detector(input: &DetectInput<'_>) -> Detection {
    Detection {
        agent: Some("claude".into()),
        ..marker_detector(input)
    }
}

#[test]
fn a_quiet_session_carried_over_by_a_restart_in_place_is_still_followed() {
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
    let mut log = std::fs::File::create(project.join(format!("{CONVERSATION}.jsonl"))).unwrap();
    for entry in [
        json!({"type": "user", "uuid": "u1", "timestamp": "2026-10-30T13:07:25.416Z",
            "sessionId": CONVERSATION, "message": {"role": "user", "content": "continue"}}),
        json!({"type": "assistant", "uuid": "a1", "timestamp": "2026-10-30T13:11:32.545Z",
            "sessionId": CONVERSATION, "message": {"role": "assistant",
                "content": [{"type": "text", "text": "Both searches are ported."}]}}),
    ] {
        writeln!(log, "{entry}").unwrap();
    }

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
    eventually("the conversation followed before the restart", || {
        h.host.show(&id, "> [idle]");
        (h.get(&id).user_prompt.as_deref() == Some("continue")).then_some(())
    });

    // `S`: the next image adopts the terminal. Claude Code, waiting for a
    // prompt, writes nothing more.
    let restored = ControlService::open_after_handoff(
        ControlConfig {
            layout: h.layout.clone(),
            user_token: h.user_token.clone(),
            socket: "/tmp/rakumux-test/ptyd.sock".into(),
            rakumux_bin: Some(RAKUMUX_BIN.into()),
            version: "test".into(),
            limits: Limits::default(),
        },
        h.host.clone(),
        h.control.handoff(),
    )
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    let view = loop {
        let view = session(restored.handle(&h.user, Request::SessionGet { id: id.clone() }));
        if view.user_prompt.is_some() || Instant::now() > deadline {
            break view;
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    assert_eq!(view.user_prompt.as_deref(), Some("continue"));
    assert_eq!(
        view.agent_output.as_deref(),
        Some("Both searches are ported.")
    );
    assert_eq!(
        view.agent_turn
            .and_then(|turn| turn.conversation_id)
            .as_deref(),
        Some(CONVERSATION)
    );
    match restored.handle(
        &h.user,
        Request::SessionTranscript {
            id,
            after: None,
            max_bytes: None,
            subagents: false,
        },
    ) {
        Response::Transcript { transcript } => {
            assert_eq!(transcript.conversation_id, CONVERSATION);
            assert_eq!(transcript.messages.len(), 2);
        }
        other => panic!("no chat after the restart: {other:?}"),
    }
}
