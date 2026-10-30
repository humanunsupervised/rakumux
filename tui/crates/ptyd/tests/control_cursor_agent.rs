//! Cursor's agent CLI in the control layer, detected by the real detector:
//! its hooks' activity keeps the session's agent, an approval its screen
//! shows wins over a busy hook (its hooks have no approval step), and the
//! chat its hooks name reaches its resume command.
mod control_common;

use control_common::*;
use session_core::activity::{ActivityEvent, ActivityPhase, ActivityUpdate, ACTIVITY_VERSION};
use session_core::api::{AgentWorkingDirectory, Report, Request, Response};
use session_core::ids::SessionId;
use session_core::model::{Lifecycle, ResumeSource};

const CHAT: &str = "0f8a3c1e-7b2d-4e5f-9a6b-1c2d3e4f5a6b";
const VERSION: &str = "/home/me/.local/share/cursor-agent/versions/2026.10.22-27dcf5f";

/// What a Cursor hook reports (`rakumux session hook --agent cursor-agent`).
fn hook(phase: ActivityPhase, working_directory: Option<&std::path::Path>) -> Request {
    Request::SessionReport(Report {
        activity: Some(ActivityUpdate {
            version: ACTIVITY_VERSION,
            agent: "cursor-agent".into(),
            conversation_id: CHAT.into(),
            completion_id: None,
            event: ActivityEvent::Phase { phase },
        }),
        working_directory: working_directory.map(|path| AgentWorkingDirectory {
            agent: "cursor-agent".into(),
            conversation_id: Some(CHAT.into()),
            path: path.to_path_buf(),
        }),
        ..Report::default()
    })
}

/// Cursor started as `agent`, as Linux shows it.
fn run_cursor(harness: &Harness, id: &SessionId, pid: u32) {
    harness.host.set_foreground(
        id,
        ptyd::host::ProcessInfo {
            pid,
            name: "MainThread".into(),
            cmdline: vec![
                "/home/me/.local/bin/agent".into(),
                "--use-system-ca".into(),
                format!("{VERSION}/index.js"),
                "--model".into(),
                "gpt-5".into(),
            ],
            exe: Some(format!("{VERSION}/node").into()),
        },
    );
}

const IDLE: &str = " ┌────────────────────┐\n │ → Add a follow-up  │\n └────────────────────┘\n";
const APPROVAL: &str = "  Run this command?\n  Not in allowlist: rm\n  → Run (once) (y)\n    Skip & tell the agent what to do instead (esc or n)\n";

#[test]
fn cursors_hooks_drive_its_session_and_name_its_chat() {
    let harness = Harness::new();
    // Cursor's directories: this test's, never the real ones (the daemon
    // reads them from its environment).
    std::env::set_var("CURSOR_CONFIG_DIR", harness.dir.path().join("cursor"));
    std::env::set_var("CURSOR_DATA_DIR", harness.dir.path().join("cursor"));
    let session = harness.create(Some("agent"));
    let caller = harness.session_caller(&session.id);
    run_cursor(&harness, &session.id, 4242);
    harness.host.show(&session.id, IDLE);
    eventually("Cursor is detected", || {
        let view = harness.get(&session.id);
        (view.agent?.kind == "cursor-agent" && view.lifecycle == Lifecycle::Idle).then_some(())
    });

    // Its prompt hook, then its tool hook: working, with the agent kept.
    let root = harness.scope.path().to_path_buf();
    assert_eq!(
        harness.handle(&caller, hook(ActivityPhase::Working, Some(&root))),
        Response::Ok
    );
    assert_eq!(
        harness.handle(&caller, hook(ActivityPhase::RunningTool, Some(&root))),
        Response::Ok
    );
    let view = harness.get(&session.id);
    assert_eq!(view.lifecycle, Lifecycle::Working);
    assert_eq!(view.agent.unwrap().kind, "cursor-agent");
    assert_eq!(view.activity.unwrap().phase, ActivityPhase::RunningTool);

    // `preToolUse` fired, then Cursor drew its approval: the screen wins.
    harness.host.show(&session.id, APPROVAL);
    eventually("the approval is seen", || {
        (harness.get(&session.id).lifecycle == Lifecycle::Blocked).then_some(())
    });

    // The chat its hooks name is the one it resumes, with its flags (looked
    // for again now and then, and when another process shows).
    run_cursor(&harness, &session.id, 4243);
    harness.host.show(&session.id, APPROVAL);
    let resume = eventually("a resume command", || {
        let resume = harness.get(&session.id).resume?;
        (resume.source == ResumeSource::KnownAgent).then_some(resume.argv)
    });
    assert_eq!(
        resume,
        ["cursor-agent", "--model", "gpt-5", "--resume", CHAT]
    );

    // Approved, the turn goes on and finishes: an unread reply.
    harness.host.show(&session.id, IDLE);
    assert_eq!(
        harness.handle(&caller, hook(ActivityPhase::Working, None)),
        Response::Ok
    );
    assert_eq!(harness.get(&session.id).lifecycle, Lifecycle::Working);
    assert_eq!(
        harness.handle(&caller, hook(ActivityPhase::Done, None)),
        Response::Ok
    );
    assert_eq!(harness.get(&session.id).lifecycle, Lifecycle::Done);
}
