//! `session_submit`'s `refuse_if_blocked`, with the daemon's own detection
//! on screens captured from the agents: nothing is typed into an agent that
//! asks something (a permission prompt, a question, a menu), an Enter is
//! held back from one that came to ask something after the paste, nothing
//! is typed into an agent whose screen shows no prompt yet (still starting,
//! though its hooks say idle), and Claude Code, Codex and OpenCode still
//! take a reply, Enter and all, while they wait or work.
mod control_common;
use control_common::*;
use session_core::activity::{ActivityEvent, ActivityPhase, ActivityUpdate, ACTIVITY_VERSION};
use session_core::api::{ErrorCode, Report, Request, Response};
use session_core::ids::SessionId;
use session_core::model::Lifecycle;

/// An agent's screens (`tests/fixtures/detect/`): waiting, working, asking.
struct Agent {
    name: &'static str,
    idle: &'static str,
    working: &'static str,
    blocked: &'static str,
}

const AGENTS: &[Agent] = &[
    Agent {
        name: "claude",
        idle: "claude_idle_prompt",
        working: "claude_working_model_turn",
        blocked: "claude_blocked_bash_permission",
    },
    Agent {
        name: "codex",
        idle: "codex_idle_prompt",
        working: "codex_working_status",
        blocked: "codex_blocked_command_approval",
    },
    Agent {
        name: "opencode",
        idle: "opencode_idle_home",
        working: "opencode_working_session",
        blocked: "opencode_blocked_permission",
    },
];

/// As the account service submits a reply: quiet, expecting the program it
/// saw, refused while its agent asks something.
fn reply(id: &SessionId, text: &str, foreground: &str) -> Request {
    Request::SessionSubmit {
        id: id.clone(),
        text: text.into(),
        quiet_ms: Some(10_000),
        expect_foreground: Some(foreground.into()),
        refuse_if_blocked: true,
    }
}

/// Show `fixture` and wait until the daemon's lifecycle for the session is
/// what it shows (a turn's end is `done`).
fn shows(h: &Harness, id: &SessionId, fixture: &str) {
    let expected = h.host.show_fixture(id, fixture);
    eventually(fixture, || {
        let now = h.get(id).lifecycle;
        let seen = now == expected || (expected == Lifecycle::Idle && now == Lifecycle::Done);
        seen.then_some(())
    });
}

/// The program in the session's foreground, as its view says.
fn foreground(h: &Harness, id: &SessionId) -> String {
    h.get(id)
        .terminal
        .and_then(|terminal| terminal.foreground_process)
        .expect("a foreground process")
}

#[test]
fn claude_code_codex_and_opencode_take_a_reply_enter_and_all_waiting_or_working() {
    for agent in AGENTS {
        let h = Harness::new();
        let s = h.create(Some(agent.name));
        let terminal = h.host.terminal_of(&s.id);
        shows(&h, &s.id, agent.idle);
        let program = foreground(&h, &s.id);
        assert_eq!(h.get(&s.id).agent.unwrap().kind, agent.name);
        assert_eq!(
            h.handle(&h.user, reply(&s.id, "run the tests", &program)),
            Response::Ok,
            "{}: waiting",
            agent.name
        );
        h.host.enter_due(&s.id);
        // Mid-turn, for the agents that queue what's typed meanwhile.
        shows(&h, &s.id, agent.working);
        assert_eq!(
            h.handle(&h.user, reply(&s.id, "and the docs", &program)),
            Response::Ok,
            "{}: working",
            agent.name
        );
        h.host.enter_due(&s.id);
        let typed = h.host.terminal(terminal);
        assert_eq!(
            typed.submits,
            ["run the tests", "and the docs"],
            "{}",
            agent.name
        );
        assert_eq!(
            typed.enters,
            [true, true],
            "{}: the Enters went",
            agent.name
        );
    }
}

#[test]
fn nothing_is_typed_while_the_agent_asks_something_and_tries_are_not_audited() {
    for agent in AGENTS {
        let h = Harness::new();
        let s = h.create(Some(agent.name));
        let terminal = h.host.terminal_of(&s.id);
        shows(&h, &s.id, agent.blocked);
        let program = foreground(&h, &s.id);
        let audited = h.audit().len();
        let refused = error(h.handle(&h.user, reply(&s.id, "1", &program)));
        assert_eq!(refused.code, ErrorCode::SessionBlocked, "{}", agent.name);
        assert!(
            h.host.terminal(terminal).submits.is_empty(),
            "{}",
            agent.name
        );
        assert_eq!(
            h.audit().len(),
            audited,
            "{}: each try isn't audited",
            agent.name
        );

        // Its screen asks again before the daemon looked: read now, refused.
        shows(&h, &s.id, agent.idle);
        assert_eq!(h.host.put_fixture(&s.id, agent.blocked), Lifecycle::Blocked);
        assert_ne!(h.get(&s.id).lifecycle, Lifecycle::Blocked);
        let refused = error(h.handle(&h.user, reply(&s.id, "1", &program)));
        assert_eq!(
            refused.code,
            ErrorCode::SessionBlocked,
            "{}: screen",
            agent.name
        );
        assert!(
            h.host.terminal(terminal).submits.is_empty(),
            "{}",
            agent.name
        );

        // Back at its prompt, the reply goes in.
        shows(&h, &s.id, agent.idle);
        assert_eq!(
            h.handle(&h.user, reply(&s.id, "go on", &program)),
            Response::Ok
        );
        h.host.enter_due(&s.id);
        assert_eq!(h.host.terminal(terminal).enters, [true], "{}", agent.name);
    }
}

#[test]
fn an_agent_the_daemon_has_asking_something_is_refused_whatever_its_screen() {
    // Hooks, OSC 7501 or a report can say so before the screen does.
    let h = Harness::new();
    let s = h.create(Some("claude"));
    let terminal = h.host.terminal_of(&s.id);
    shows(&h, &s.id, "claude_idle_prompt");
    let program = foreground(&h, &s.id);
    let inside = h.session_caller(&s.id);
    let report = Report {
        state: Some(Lifecycle::Blocked),
        ..Report::default()
    };
    assert_eq!(
        h.handle(&inside, Request::SessionReport(report)),
        Response::Ok
    );
    assert_eq!(h.get(&s.id).lifecycle, Lifecycle::Blocked);
    let refused = error(h.handle(&h.user, reply(&s.id, "yes", &program)));
    assert_eq!(refused.code, ErrorCode::SessionBlocked);
    assert!(h.host.terminal(terminal).submits.is_empty());
    // Not asked to look, a submit goes in as before.
    let plain = Request::SessionSubmit {
        id: s.id.clone(),
        text: "yes".into(),
        quiet_ms: None,
        expect_foreground: None,
        refuse_if_blocked: false,
    };
    assert_eq!(h.handle(&h.user, plain), Response::Ok);
    assert_eq!(h.host.terminal(terminal).submits, ["yes"]);
}

#[test]
fn an_enter_is_held_back_from_an_agent_that_came_to_ask_something_after_the_paste() {
    for agent in AGENTS {
        let h = Harness::new();
        let s = h.create(Some(agent.name));
        let terminal = h.host.terminal_of(&s.id);
        shows(&h, &s.id, agent.working);
        let program = foreground(&h, &s.id);
        assert_eq!(
            h.handle(&h.user, reply(&s.id, "also update the docs", &program)),
            Response::Ok,
            "{}",
            agent.name
        );
        // A permission prompt opens during the pause: the Enter would
        // approve it. Held back, whether or not the daemon has looked yet.
        h.host.put_fixture(&s.id, agent.blocked);
        h.host.enter_due(&s.id);
        assert_eq!(h.host.terminal(terminal).enters, [false], "{}", agent.name);

        // Its prompt answered and the turn over, the next reply goes in.
        shows(&h, &s.id, agent.idle);
        assert_eq!(
            h.handle(&h.user, reply(&s.id, "thanks", &program)),
            Response::Ok
        );
        h.host.enter_due(&s.id);
        let typed = h.host.terminal(terminal);
        assert_eq!(
            typed.submits,
            ["also update the docs", "thanks"],
            "{}",
            agent.name
        );
        assert_eq!(typed.enters, [false, true], "{}", agent.name);
    }
}

#[test]
fn nothing_is_typed_into_an_agent_whose_screen_shows_no_prompt_yet() {
    // 2026-10-31: a session message from rakumux.com typed into Claude Code
    // as it started, its SessionStart hook having said idle before anything
    // was drawn. The paste went in, its Enter was lost.
    let h = Harness::new();
    let s = h.create(Some("claude"));
    let terminal = h.host.terminal_of(&s.id);
    shows(&h, &s.id, "claude_unknown_starting");
    let started = Report {
        activity: Some(ActivityUpdate {
            version: ACTIVITY_VERSION,
            agent: "claude".into(),
            conversation_id: "0d2a1c8e-5b7f-4e3a-9c6d-1f2e3a4b5c6d".into(),
            completion_id: None,
            event: ActivityEvent::Phase {
                phase: ActivityPhase::Idle,
            },
        }),
        ..Report::default()
    };
    let inside = h.session_caller(&s.id);
    assert_eq!(
        h.handle(&inside, Request::SessionReport(started)),
        Response::Ok
    );
    assert_eq!(h.get(&s.id).lifecycle, Lifecycle::Idle, "as its hook says");
    let program = foreground(&h, &s.id);
    let audited = h.audit().len();
    // Starting, then a first-run screen of its own: no prompt either time.
    for screen in ["claude_unknown_starting", "claude_unknown_onboarding"] {
        assert_eq!(h.host.put_fixture(&s.id, screen), Lifecycle::Unknown);
        let refused = error(h.handle(&h.user, reply(&s.id, "Start on the parser.", &program)));
        assert_eq!(refused.code, ErrorCode::SessionBlocked, "{screen}");
    }
    assert!(h.host.terminal(terminal).submits.is_empty());
    assert_eq!(h.audit().len(), audited, "each try isn't audited");
    // Not asked to look, a submit goes in as before.
    let plain = Request::SessionSubmit {
        id: s.id.clone(),
        text: "hello".into(),
        quiet_ms: None,
        expect_foreground: None,
        refuse_if_blocked: false,
    };
    assert_eq!(h.handle(&h.user, plain), Response::Ok);

    // Its prompt drawn, the message goes in, Enter and all.
    assert_eq!(
        h.host.put_fixture(&s.id, "claude_idle_prompt"),
        Lifecycle::Idle
    );
    assert_eq!(
        h.handle(&h.user, reply(&s.id, "Start on the parser.", &program)),
        Response::Ok
    );
    h.host.enter_due(&s.id);
    let typed = h.host.terminal(terminal);
    assert_eq!(typed.submits, ["hello", "Start on the parser."]);
    assert_eq!(typed.enters.last(), Some(&true));
}
