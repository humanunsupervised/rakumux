//! Hook activity uses existing self-report permission checks and event fan-out.
mod control_common;
use control_common::*;
use ptyd::control_api::Control;
use session_core::activity::{ActivityEvent, ActivityPhase, ActivityUpdate, ACTIVITY_VERSION};
use session_core::api::{ErrorCode, Report, Request, Response, SessionEvent, SubscribeRequest};
use session_core::model::{Lifecycle, SessionStatus};

fn update(agent: &str, event: ActivityEvent) -> Report {
    Report {
        activity: Some(ActivityUpdate {
            version: ACTIVITY_VERSION,
            agent: agent.into(),
            conversation_id: "root".into(),
            completion_id: None,
            event,
        }),
        ..Default::default()
    }
}

#[test]
fn activity_hooks_update_all_clients_without_replacing_semantic_reports() {
    let harness = Harness::new();
    let session = harness.create(Some("agent"));
    let caller = harness.session_caller(&session.id);
    let subscribe = || {
        harness
            .control
            .subscribe(
                &harness.user,
                &SubscribeRequest {
                    all: true,
                    ..Default::default()
                },
            )
            .unwrap()
    };
    let first = subscribe();
    let second = subscribe();
    harness.handle(
        &caller,
        Request::SessionReport(Report {
            text: Some("keep this report".into()),
            ..Default::default()
        }),
    );
    for agent in ["claude", "codex", "opencode"] {
        assert_eq!(
            harness.handle(
                &caller,
                Request::SessionReport(update(
                    agent,
                    ActivityEvent::Phase {
                        phase: ActivityPhase::RunningTool
                    }
                ))
            ),
            Response::Ok
        );
        let view = harness.get(&session.id);
        assert_eq!(view.lifecycle, Lifecycle::Working);
        assert_eq!(view.activity.unwrap().phase, ActivityPhase::RunningTool);
        assert_eq!(view.agent.unwrap().kind, agent);
        assert_eq!(
            view.reported.unwrap().text.as_deref(),
            Some("keep this report")
        );
    }
    for receiver in [first, second] {
        let found = receiver.try_iter().any(|event| matches!(event.event, SessionEvent::Updated { session } if session.activity.is_some()));
        assert!(found, "each subscriber receives normalized activity");
    }
    harness.handle(
        &caller,
        Request::SessionReport(update(
            "opencode",
            ActivityEvent::Subagent {
                id: "child".into(),
                running: true,
            },
        )),
    );
    assert_eq!(
        harness.get(&session.id).activity.unwrap().active_subagents,
        1
    );
    harness.handle(
        &caller,
        Request::SessionReport(update(
            "opencode",
            ActivityEvent::Phase {
                phase: ActivityPhase::NeedsInput,
            },
        )),
    );
    assert_eq!(harness.get(&session.id).lifecycle, Lifecycle::Blocked);
    harness.handle(
        &caller,
        Request::SessionReport(update(
            "opencode",
            ActivityEvent::Phase {
                phase: ActivityPhase::Done,
            },
        )),
    );
    assert_eq!(harness.get(&session.id).lifecycle, Lifecycle::Done);
    harness.handle(
        &caller,
        Request::SessionTouch {
            id: session.id.clone(),
        },
    );
    assert_eq!(harness.get(&session.id).lifecycle, Lifecycle::Idle);
    harness.host.exit(&session.id, Some(0));
    eventually("exit clears activity", || {
        let view = harness.get(&session.id);
        (view.status == SessionStatus::Exited && view.activity.is_none()).then_some(())
    });
}

#[test]
fn invalid_activity_is_rejected_before_changing_identity_or_state() {
    let harness = Harness::new();
    let session = harness.create(Some("agent"));
    let caller = harness.session_caller(&session.id);
    let mut report = update(
        "codex",
        ActivityEvent::Phase {
            phase: ActivityPhase::Working,
        },
    );
    report.activity.as_mut().unwrap().version = 99;
    assert_eq!(
        error(harness.handle(&caller, Request::SessionReport(report))).code,
        ErrorCode::InvalidArgument
    );
    assert!(harness.get(&session.id).activity.is_none());
    let denied = harness.handle(
        &harness.user,
        Request::SessionReport(update(
            "codex",
            ActivityEvent::Phase {
                phase: ActivityPhase::Working,
            },
        )),
    );
    assert_eq!(
        error(denied).code,
        ErrorCode::InvalidArgument,
        "a user has no self session to report"
    );
    harness.host.exit(&session.id, Some(0));
    eventually("exit recorded", || {
        (harness.get(&session.id).status == SessionStatus::Exited).then_some(())
    });
    assert_eq!(
        error(harness.handle(
            &caller,
            Request::SessionReport(update(
                "codex",
                ActivityEvent::Phase {
                    phase: ActivityPhase::Working
                }
            ))
        ))
        .code,
        ErrorCode::SessionNotLive
    );
}

#[test]
fn fresh_activity_survives_a_compatible_handoff_without_extending_its_age() {
    use ptyd::control::{ControlConfig, ControlService, Limits};
    let harness = Harness::new();
    let session = harness.create(Some("agent"));
    let caller = harness.session_caller(&session.id);
    harness.handle(
        &caller,
        Request::SessionReport(update(
            "opencode",
            ActivityEvent::Snapshot {
                phase: ActivityPhase::RunningTool,
                subagents: vec!["child".into()],
            },
        )),
    );
    let before = harness.get(&session.id).activity.unwrap();
    let handed = harness.control.handoff();
    assert_eq!(handed.activity.len(), 1);
    let restored = ControlService::open_after_handoff(
        ControlConfig {
            layout: harness.layout.clone(),
            user_token: harness.user_token.clone(),
            socket: "/tmp/rakumux-test/ptyd.sock".into(),
            rakumux_bin: Some(RAKUMUX_BIN.into()),
            version: "test".into(),
            limits: Limits::default(),
        },
        harness.host.clone(),
        handed,
    )
    .unwrap();
    let after = control_common::session(
        restored.handle(&harness.user, Request::SessionGet { id: session.id }),
    );
    assert_eq!(after.activity, Some(before));
    assert_eq!(after.lifecycle, Lifecycle::Working);
}

#[test]
fn completed_replies_stay_seen_across_snapshots_and_daemon_handoff() {
    use ptyd::control::{ControlConfig, ControlService, Limits};
    use session_core::api::{AttachRequest, Role};

    for seen in [false, true] {
        let harness = Harness::new();
        let session = harness.create(Some("agent"));
        let caller = harness.session_caller(&session.id);
        let mut report = update(
            "opencode",
            ActivityEvent::Snapshot {
                phase: ActivityPhase::Done,
                subagents: vec![],
            },
        );
        report.activity.as_mut().unwrap().completion_id = Some("reply-1".into());
        harness.handle(&caller, Request::SessionReport(report.clone()));
        harness
            .control
            .prepare_attach(
                &harness.user,
                &AttachRequest {
                    id: session.id.clone(),
                    role: Role::Observer,
                    cols: 80,
                    rows: 24,
                },
            )
            .unwrap();
        assert_eq!(harness.get(&session.id).lifecycle, Lifecycle::Done);
        if seen {
            harness.handle(
                &harness.user,
                Request::SessionTouch {
                    id: session.id.clone(),
                },
            );
        }
        let expected = if seen {
            Lifecycle::Idle
        } else {
            Lifecycle::Done
        };
        harness.handle(&caller, Request::SessionReport(report.clone()));
        assert_eq!(harness.get(&session.id).lifecycle, expected);

        let handed = harness.control.handoff();
        let restored = ControlService::open_after_handoff(
            ControlConfig {
                layout: harness.layout.clone(),
                user_token: harness.user_token.clone(),
                socket: "/tmp/rakumux-test/ptyd.sock".into(),
                rakumux_bin: Some(RAKUMUX_BIN.into()),
                version: "test".into(),
                limits: Limits::default(),
            },
            harness.host.clone(),
            handed,
        )
        .unwrap();
        let get = || {
            control_common::session(restored.handle(
                &harness.user,
                Request::SessionGet {
                    id: session.id.clone(),
                },
            ))
        };
        assert_eq!(get().lifecycle, expected);
        restored.handle(&caller, Request::SessionReport(report.clone()));
        assert_eq!(
            get().lifecycle,
            expected,
            "a heartbeat cannot resurrect a seen reply"
        );
        report.activity.as_mut().unwrap().completion_id = Some("reply-2".into());
        restored.handle(&caller, Request::SessionReport(report));
        assert_eq!(
            get().lifecycle,
            Lifecycle::Done,
            "a new reply becomes unread"
        );
    }
}

#[test]
fn invalid_completion_identifiers_are_rejected_without_changing_activity() {
    let harness = Harness::new();
    let session = harness.create(Some("agent"));
    let caller = harness.session_caller(&session.id);
    for completion_id in [String::new(), "a".repeat(257), "bad\nidentifier".into()] {
        let mut report = update(
            "opencode",
            ActivityEvent::Phase {
                phase: ActivityPhase::Done,
            },
        );
        report.activity.as_mut().unwrap().completion_id = Some(completion_id);
        assert_eq!(
            error(harness.handle(&caller, Request::SessionReport(report))).code,
            ErrorCode::InvalidArgument
        );
        assert!(harness.get(&session.id).activity.is_none());
    }
}

#[test]
fn opencode_v2_started_as_opencode2_keeps_its_activity_through_terminal_output() {
    use session_core::model::ResumeSource;
    let harness = Harness::new();
    let session = harness.create(Some("agent"));
    let caller = harness.session_caller(&session.id);
    harness.handle(
        &caller,
        Request::SessionBind {
            agent: "opencode".into(),
            conversations: vec!["root".into()],
        },
    );
    harness.host.set_foreground(
        &session.id,
        ptyd::host::ProcessInfo {
            pid: 4242,
            name: "opencode2".into(),
            cmdline: vec!["opencode2".into()],
        },
    );
    harness.handle(
        &caller,
        Request::SessionReport(update(
            "opencode",
            ActivityEvent::Snapshot {
                phase: ActivityPhase::RunningTool,
                subagents: vec!["child".into()],
            },
        )),
    );
    // Output was detected as "no agent", which dropped the plugin's activity
    // (and its unread replies) every time OpenCode drew.
    harness.host.show(&session.id, "■■■■⬝⬝⬝⬝  esc interrupt");
    let view = eventually("output is detected as OpenCode", || {
        let view = harness.get(&session.id);
        (view.resume.as_ref()?.source == ResumeSource::KnownAgent).then_some(view)
    });
    assert_eq!(
        view.resume.unwrap().argv,
        vec!["opencode2".to_string(), "-s".into(), "root".into()]
    );
    let activity = view.activity.expect("the plugin's activity is kept");
    assert_eq!(activity.phase, ActivityPhase::RunningTool);
    assert_eq!(activity.active_subagents, 1);
    assert_eq!(view.lifecycle, Lifecycle::Working);
}

#[test]
fn an_esc_interrupted_claude_turn_ends_without_a_stop_hook() {
    const BUSY: &str =
        "⏺ Bash(cargo test)\n  ⎿  Running…\n\n✻ Bloviating… (12s · ↓ 1.2k tokens)\n\n\
        ────────────────────────────────────────\n❯\n────────────────────────────────────────\n\
        \u{20} ⏵⏵ bypass permissions on (shift+tab to cycle) · esc to interrupt\n";
    const INTERRUPTED: &str =
        "⏺ Bash(cargo test)\n  ⎿  Interrupted · What should Claude do instead?\n\n\
        ────────────────────────────────────────\n❯\n────────────────────────────────────────\n\
        \u{20} ⏵⏵ bypass permissions on (shift+tab to cycle)\n";
    let harness = Harness::new();
    let session = harness.create(Some("agent"));
    let caller = harness.session_caller(&session.id);
    harness.host.set_foreground(
        &session.id,
        ptyd::host::ProcessInfo {
            pid: 4242,
            name: "claude".into(),
            cmdline: vec!["claude".into()],
        },
    );
    harness.handle(
        &caller,
        Request::SessionReport(update(
            "claude",
            ActivityEvent::Phase {
                phase: ActivityPhase::RunningTool,
            },
        )),
    );
    harness.host.show(&session.id, BUSY);
    std::thread::sleep(std::time::Duration::from_millis(300));
    // Esc: Claude Code redraws its idle prompt; no Stop or other hook follows.
    harness.host.show(&session.id, INTERRUPTED);
    std::thread::sleep(std::time::Duration::from_millis(1_000));
    assert_eq!(
        harness.get(&session.id).lifecycle,
        Lifecycle::Working,
        "a Stop hook may still be on its way"
    );
    std::thread::sleep(std::time::Duration::from_millis(4_000));
    let view = eventually("the missed turn end", || {
        let view = harness.get(&session.id);
        view.activity.is_none().then_some(view)
    });
    assert_eq!(view.lifecycle, Lifecycle::Done);
}
