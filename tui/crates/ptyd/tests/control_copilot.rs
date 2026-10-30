//! GitHub Copilot CLI in a session: its conversation is the one its process
//! shows, and hooks from conversations it holds open in its sessions sidebar
//! but doesn't show are left out. A test binary of its own: it sets
//! `COPILOT_HOME` for the daemon's follower.
mod control_common;
use control_common::*;
use session_core::activity::{ActivityEvent, ActivityPhase, ActivityUpdate, ACTIVITY_VERSION};
use session_core::api::{AgentWorkingDirectory, Report, Request, Response};
use session_core::model::ResumeSource;
use std::io::Write;
use std::path::Path;

/// On screen.
const SHOWN: &str = "bd0ffe88-2f24-4e0d-8b97-ead37412c634";
/// Open in the sidebar, not on screen.
const HIDDEN: &str = "0471e2d2-e37e-4a26-ab61-7af3f61b8611";
/// The native binary's pid: no process here has it.
const PID: u32 = 4_000_000_004;

const IDLE: &str = "● Done.\n\
    ────────────────────────────────────────\n❯\n────────────────────────────────────────\n\
    \u{20}← open sidebar · Interactive · Manual Approval · / commands · ? help\n";

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .unwrap();
    file.write_all(text.as_bytes()).unwrap();
}

fn hook(conversation: &str, phase: ActivityPhase, cwd: &Path) -> Request {
    Request::SessionReport(Report {
        activity: Some(ActivityUpdate {
            version: ACTIVITY_VERSION,
            agent: "copilot".into(),
            conversation_id: conversation.into(),
            completion_id: None,
            event: ActivityEvent::Phase { phase },
        }),
        working_directory: Some(AgentWorkingDirectory {
            agent: "copilot".into(),
            conversation_id: Some(conversation.into()),
            path: cwd.to_path_buf(),
        }),
        ..Report::default()
    })
}

#[test]
fn the_conversation_on_screen_is_followed_and_hidden_ones_are_left_out() {
    let home = tempfile::tempdir().unwrap();
    let copilot = home.path().join("copilot");
    // Set before the daemon starts; this binary runs this one test.
    std::env::set_var("COPILOT_HOME", &copilot);
    write(
        &copilot.join(format!("logs/process-1793338015057-{PID}.log")),
        &format!(
            "2026-10-30T05:26:55.083Z [INFO] Registering foreground session: {HIDDEN}\n\
             2026-10-30T05:31:13.587Z [INFO] Unregistering foreground session: {HIDDEN}\n\
             2026-10-30T05:31:13.589Z [INFO] Registering foreground session: {SHOWN}\n"
        ),
    );
    let event = |kind: &str, data: serde_json::Value| {
        format!(
            "{}\n",
            serde_json::json!({"type": kind, "data": data, "id": format!("e-{kind}"),
                "timestamp": "2026-10-30T05:31:20.000Z", "parentId": null})
        )
    };
    write(
        &copilot.join(format!("session-state/{SHOWN}/events.jsonl")),
        &(event("user.message", serde_json::json!({"content": "Write the docs"}))
            + &event(
                "assistant.message",
                serde_json::json!({"messageId": "m1", "content": "Writing them.", "toolRequests": []}),
            )),
    );

    let harness = Harness::new();
    let session = harness.create(Some("agent"));
    let caller = harness.session_caller(&session.id);
    harness.host.set_foreground(
        &session.id,
        ptyd::host::ProcessInfo {
            pid: PID,
            name: "MainThread".into(),
            cmdline: vec!["/opt/homebrew/bin/copilot".into(), "--yolo".into()],
            exe: None,
        },
    );
    // The log is read at most once a second, while the terminal shows output.
    let view = eventually("the shown conversation is followed", || {
        harness.host.show(&session.id, IDLE);
        let view = harness.get(&session.id);
        (view.user_prompt.as_deref() == Some("Write the docs")).then_some(view)
    });
    assert_eq!(view.agent_output.as_deref(), Some("Writing them."));
    assert_eq!(
        view.agent_turn.unwrap().conversation_id.as_deref(),
        Some(SHOWN)
    );
    let resume = eventually("its resume command", || {
        let resume = harness.get(&session.id).resume?;
        (resume.source == ResumeSource::KnownAgent).then_some(resume.argv)
    });
    assert_eq!(resume, ["copilot", "--yolo", &format!("--resume={SHOWN}")]);

    // A hidden conversation's hooks: accepted, and left out.
    let cwd = harness.scope.path().to_path_buf();
    assert_eq!(
        harness.handle(&caller, hook(HIDDEN, ActivityPhase::Working, &cwd)),
        Response::Ok
    );
    let view = harness.get(&session.id);
    assert!(view.activity.is_none(), "{:?}", view.activity);
    assert_ne!(
        view.agent
            .as_ref()
            .and_then(|agent| agent.agent_session_id.as_deref()),
        Some(HIDDEN)
    );
    // The one on screen drives the session.
    harness.handle(&caller, hook(SHOWN, ActivityPhase::RunningTool, &cwd));
    let view = harness.get(&session.id);
    assert_eq!(view.activity.unwrap().phase, ActivityPhase::RunningTool);
    assert_eq!(
        view.agent.unwrap().agent_session_id.as_deref(),
        Some(SHOWN)
    );
    harness.handle(&caller, hook(HIDDEN, ActivityPhase::Done, &cwd));
    assert_eq!(
        harness.get(&session.id).activity.unwrap().phase,
        ActivityPhase::RunningTool,
        "a hidden conversation's finished turn isn't this session's"
    );
}
