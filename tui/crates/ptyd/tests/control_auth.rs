//! Identity, scope confinement, the stage-A permission matrix, sessions
//! creating sessions (and its limits) and audit.

mod control_common;

use control_common::*;
use ptyd::control_api::Control;
use session_core::api::CreateSession;
use session_core::api::{
    AttachRequest, Auth, CallerInfo, ErrorCode, ReadRequest, ReadSource, Report, Request,
    ReviveSession, Role, SendRequest, SubscribeRequest, TextFormat, WaitRequest, WaitUntil,
};
use session_core::ids::SessionId;
use session_core::model::{Creator, Lifecycle};

/// Error code of a session hello that must fail.
fn bad(h: &Harness, id: &SessionId, token: &str) -> ErrorCode {
    h.control
        .authenticate(&hello(Auth::Session {
            id: id.clone(),
            token: token.into(),
        }))
        .unwrap_err()
        .code
}

#[test]
fn user_auth_requires_the_registration_token() {
    let h = Harness::new();
    let caller = h
        .control
        .authenticate(&hello(Auth::User {
            token: h.user_token.as_str().into(),
        }))
        .unwrap();
    assert!(caller.is_user());
    assert_eq!(caller.client, "test");

    let err = h
        .control
        .authenticate(&hello(Auth::User {
            token: "not-the-token".into(),
        }))
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::Unauthenticated);
}

#[test]
fn session_auth_uses_the_injected_token_and_dies_with_the_session() {
    let mut h = Harness::new();
    let s = h.create(Some("worker"));
    let caller = h.session_caller(&s.id);
    assert_eq!(
        caller.info,
        CallerInfo::Session {
            id: s.id.clone(),
            scope: h.scope.clone()
        }
    );

    assert_eq!(bad(&h, &s.id, "wrong"), ErrorCode::Unauthenticated);
    let other = h.create(Some("other"));
    assert_eq!(
        bad(&h, &other.id, &h.token_of(&s.id)),
        ErrorCode::Unauthenticated,
        "a token only authenticates its own session"
    );
    assert_eq!(
        bad(&h, &SessionId::from_string("NOPE"), &h.token_of(&s.id)),
        ErrorCode::Unauthenticated
    );

    // Revive issues a new token; the old one stops working.
    let old_token = h.token_of(&s.id);
    h.host.exit(&s.id, Some(0));
    eventually("exit processed", || {
        (h.get(&s.id).status == session_core::model::SessionStatus::Exited).then_some(())
    });
    session(h.handle(
        &h.user,
        Request::SessionRevive(ReviveSession {
            id: s.id.clone(),
            env: vec![],
            cols: 80,
            rows: 24,
            resume: false,
        }),
    ));
    assert_eq!(bad(&h, &s.id, &old_token), ErrorCode::Unauthenticated);
    h.session_caller(&s.id);

    // Tokens are memory-only: a restarted daemon accepts none of them.
    let token = h.token_of(&s.id);
    h.restart();
    assert_eq!(bad(&h, &s.id, &token), ErrorCode::Unauthenticated);

    // Deleting a session revokes its token.
    let t = h.create(Some("doomed"));
    let token = h.token_of(&t.id);
    h.handle(&h.user, Request::SessionDelete { id: t.id.clone() });
    assert_eq!(bad(&h, &t.id, &token), ErrorCode::Unauthenticated);
}

#[test]
fn protocol_mismatch_is_reported_before_credentials() {
    let h = Harness::new();
    let mut hello = hello(Auth::User {
        token: h.user_token.as_str().into(),
    });
    hello.protocol = session_core::PROTOCOL_VERSION + 1;
    let err = h.control.authenticate(&hello).unwrap_err();
    assert_eq!(err.code, ErrorCode::ProtocolMismatch);
    assert_eq!(
        err.details.unwrap()["daemon_protocol"],
        session_core::PROTOCOL_VERSION
    );
}

#[test]
fn session_callers_are_confined_to_their_scope() {
    let h = Harness::new();
    let mine = h.create(Some("mine"));
    let neighbour = h.create(Some("neighbour"));
    let elsewhere = h.create_in(&h.other_scope(), Some("elsewhere"));
    let me = h.session_caller(&mine.id);

    // Own scope by default; another scope is refused.
    let listed = match h.handle(
        &me,
        Request::SessionList {
            scopes: Vec::new(),
            scope: None,
            include_dead: true,
            all: false,
        },
    ) {
        session_core::api::Response::Sessions { sessions } => sessions,
        other => panic!("{other:?}"),
    };
    let ids: Vec<_> = listed.iter().map(|s| s.id.clone()).collect();
    assert!(ids.contains(&mine.id) && ids.contains(&neighbour.id));
    assert!(!ids.contains(&elsewhere.id));
    assert!(listed.iter().find(|s| s.id == mine.id).unwrap().is_self);
    assert!(
        !listed
            .iter()
            .find(|s| s.id == neighbour.id)
            .unwrap()
            .is_self
    );

    let out_of_scope = [
        Request::SessionList {
            scopes: Vec::new(),
            scope: Some(h.other_scope()),
            include_dead: false,
            all: false,
        },
        Request::SessionGet {
            id: elsewhere.id.clone(),
        },
        Request::SessionRead(ReadRequest {
            id: elsewhere.id.clone(),
            source: ReadSource::Visible,
            lines: None,
            format: TextFormat::Text,
        }),
        Request::SessionWait(WaitRequest {
            id: elsewhere.id.clone(),
            until: WaitUntil::Exited,
            pattern: None,
            timeout_ms: 0,
        }),
    ];
    for request in out_of_scope {
        let err = error(h.handle(&me, request.clone()));
        assert_eq!(err.code, ErrorCode::OutOfScope, "{request:?}");
    }
    let err = h
        .control
        .subscribe(
            &me,
            &SubscribeRequest {
                scope: Some(h.other_scope()),
                ..Default::default()
            },
        )
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::OutOfScope);

    // User callers must name the scope; they may act across scopes by id.
    let err = error(h.handle(
        &h.user,
        Request::SessionList {
            scopes: Vec::new(),
            scope: None,
            include_dead: false,
            all: false,
        },
    ));
    assert_eq!(err.code, ErrorCode::InvalidArgument);
    assert_eq!(h.get(&elsewhere.id).name, "elsewhere");
}

#[test]
fn session_caller_permission_matrix() {
    let h = Harness::new();
    let me_view = h.create(Some("me"));
    let other = h.create(Some("other"));
    let me = h.session_caller(&me_view.id);
    let my_id = me_view.id.clone();

    let allowed = vec![
        Request::Ping,
        Request::SessionList {
            scopes: Vec::new(),
            scope: None,
            include_dead: false,
            all: false,
        },
        Request::SessionGet {
            id: other.id.clone(),
        },
        Request::SessionRead(ReadRequest {
            id: other.id.clone(),
            source: ReadSource::Visible,
            lines: None,
            format: TextFormat::Text,
        }),
        Request::SessionWait(WaitRequest {
            id: other.id.clone(),
            until: WaitUntil::Working,
            pattern: None,
            timeout_ms: 0,
        }),
        Request::SessionReport(Report {
            state: Some(Lifecycle::Working),
            ..Default::default()
        }),
        Request::SessionTouch { id: my_id.clone() },
        Request::SessionRename {
            id: my_id.clone(),
            name: "me renamed".into(),
        },
        Request::SessionPin {
            id: my_id.clone(),
            pinned: true,
        },
        Request::ProjectList,
    ];
    for request in allowed {
        let response = h.handle(&me, request.clone());
        assert!(
            !matches!(response, session_core::api::Response::Error { .. }),
            "{request:?} should be allowed, got {response:?}"
        );
    }
    assert!(h
        .control
        .subscribe(&me, &SubscribeRequest::default())
        .is_ok());

    let denied = vec![
        Request::SessionSend(SendRequest {
            id: other.id.clone(),
            keys: vec!["enter".into()],
            text: None,
        }),
        Request::SessionSend(SendRequest {
            id: my_id.clone(),
            keys: vec!["enter".into()],
            text: None,
        }),
        Request::SessionSubmit {
            id: other.id.clone(),
            text: "rm -rf /".into(),
        },
        Request::SessionKill {
            id: other.id.clone(),
            tree: false,
        },
        Request::SessionKill {
            id: my_id.clone(),
            tree: false,
        },
        Request::SessionDelete {
            id: other.id.clone(),
        },
        Request::SessionRevive(ReviveSession {
            id: other.id.clone(),
            env: vec![],
            cols: 80,
            rows: 24,
            resume: true,
        }),
        Request::SessionRename {
            id: other.id.clone(),
            name: "hijacked".into(),
        },
        Request::SessionTouch {
            id: other.id.clone(),
        },
        Request::SessionPin {
            id: other.id.clone(),
            pinned: true,
        },
        Request::ProjectAdd {
            path: h.root().join("other"),
        },
        Request::ProjectRemove {
            path: h.scope.clone(),
        },
        Request::SessionTurn { id: my_id.clone() },
        Request::SessionTurn {
            id: other.id.clone(),
        },
    ];
    for request in denied {
        let err = error(h.handle(&me, request.clone()));
        assert_eq!(err.code, ErrorCode::PermissionDenied, "{request:?}");
    }
    let err = error(h.handle(
        &me,
        Request::SessionKill {
            id: other.id.clone(),
            tree: false,
        },
    ));
    assert!(
        err.message.contains("later release"),
        "message explains stage B: {}",
        err.message
    );
    for id in [&my_id, &other.id] {
        let err = h
            .control
            .prepare_attach(
                &me,
                &AttachRequest {
                    id: id.clone(),
                    role: Role::Observer,
                    cols: 80,
                    rows: 24,
                },
            )
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::PermissionDenied);
    }

    // Nothing the denied calls targeted changed.
    assert_eq!(h.get(&other.id).name, "other");
    assert!(h
        .host
        .terminal(h.host.terminal_of(&other.id))
        .submits
        .is_empty());
    assert_eq!(h.get(&my_id).name, "me renamed");

    // Report is self-only by construction; users have no self.
    let err = error(h.handle(&h.user, Request::SessionReport(Report::default())));
    assert_eq!(err.code, ErrorCode::InvalidArgument);
}

/// A create request from a session caller: no project (its own is used).
fn agent_create(h: &Harness, name: &str) -> CreateSession {
    CreateSession {
        scope: None,
        name: Some(name.into()),
        ..h.create_request()
    }
}

#[test]
fn sessions_create_sessions_in_their_own_project() {
    let h = Harness::new();
    let parent = h.create(Some("parent"));
    let me = h.session_caller(&parent.id);

    let child = session(h.handle(&me, Request::SessionCreate(agent_create(&h, "child"))));
    assert_eq!(child.scope, h.scope);
    assert_eq!(child.parent_id.as_ref(), Some(&parent.id));
    assert_eq!(
        child.created_by,
        Creator::Session {
            id: parent.id.clone()
        }
    );
    assert_eq!(child.depth, 1);
    // The child has its own identity, not the parent's.
    let spec = h.host.spec_of(&child.id);
    assert_eq!(
        env_value(&spec.env, "RAKUMUX_SESSION_ID").as_deref(),
        Some(child.id.as_str())
    );
    assert_ne!(
        env_value(&spec.env, "RAKUMUX_SESSION_TOKEN"),
        Some(h.token_of(&parent.id))
    );
    assert!(h.get(&parent.id).children.contains(&child.id));

    // Naming its own project is fine; another project or a working
    // directory outside it is not.
    let mut named = agent_create(&h, "named");
    named.scope = Some(h.scope.clone());
    session(h.handle(&me, Request::SessionCreate(named)));
    let mut elsewhere = agent_create(&h, "elsewhere");
    elsewhere.scope = Some(h.other_scope());
    let err = error(h.handle(&me, Request::SessionCreate(elsewhere)));
    assert_eq!(err.code, ErrorCode::OutOfScope);
    let mut outside = agent_create(&h, "outside");
    outside.cwd = Some(h.root().join("other"));
    let err = error(h.handle(&me, Request::SessionCreate(outside)));
    assert_eq!(err.code, ErrorCode::OutOfScope);
    let mut sub = agent_create(&h, "sub");
    sub.cwd = Some("sub".into());
    let sub = session(h.handle(&me, Request::SessionCreate(sub)));
    assert_eq!(sub.cwd, h.scope.path().join("sub"));
    assert!(h.list(false).iter().all(|s| s.scope == h.scope));
}

/// The `LimitExceeded` details name the limit that was reached.
fn limit_of(h: &Harness, caller: &ptyd::control_api::Caller, name: &str) -> String {
    let err = error(h.handle(caller, Request::SessionCreate(agent_create(h, name))));
    assert_eq!(err.code, ErrorCode::LimitExceeded, "{err:?}");
    err.details.unwrap()["limit"].as_str().unwrap().to_string()
}

#[test]
fn sessions_that_sessions_create_are_limited() {
    let h = Harness::new();
    let first = h.create(Some("first"));
    let a = h.session_caller(&first.id);

    // Four running children per parent; one ending makes room again.
    let children: Vec<_> = (0..4)
        .map(|i| {
            session(h.handle(
                &a,
                Request::SessionCreate(agent_create(&h, &format!("a{i}"))),
            ))
        })
        .collect();
    assert_eq!(limit_of(&h, &a, "a4"), "max_children_per_parent");
    h.host.exit(&children[0].id, Some(0));
    h.wait_recorded_exit(&children[0].id);
    session(h.handle(&a, Request::SessionCreate(agent_create(&h, "a4"))));

    // Two levels deep: a child may create, a grandchild may not.
    let b = h.session_caller(&children[1].id);
    let grandchild = session(h.handle(&b, Request::SessionCreate(agent_create(&h, "b0"))));
    assert_eq!(grandchild.depth, 2);
    let c = h.session_caller(&grandchild.id);
    assert_eq!(limit_of(&h, &c, "c0"), "max_depth");

    // Eight running sessions created by sessions per project: 5 so far.
    let second = h.create(Some("second"));
    let d = h.session_caller(&second.id);
    for i in 0..3 {
        session(h.handle(
            &d,
            Request::SessionCreate(agent_create(&h, &format!("d{i}"))),
        ));
    }
    assert_eq!(limit_of(&h, &d, "d3"), "max_live_per_scope");
    // The user isn't limited.
    h.create(Some("by the user"));

    let rows = h.audit();
    let last = rows.last().unwrap();
    assert_eq!(
        (last.method.as_str(), last.decision.as_str()),
        ("session.create", "allowed")
    );
    let refused = &rows[rows.len() - 2];
    assert_eq!(refused.caller, format!("session:{}", second.id));
    assert_eq!(refused.decision, "denied");
}

#[test]
fn sessions_create_at_most_ten_sessions_a_minute() {
    let h = Harness::new();
    let parent = h.create(Some("parent"));
    let me = h.session_caller(&parent.id);
    for i in 0..10 {
        let child = session(h.handle(
            &me,
            Request::SessionCreate(agent_create(&h, &format!("c{i}"))),
        ));
        h.host.exit(&child.id, Some(0));
        h.wait_recorded_exit(&child.id);
    }
    assert_eq!(limit_of(&h, &me, "c10"), "max_creates_per_minute");
    // Another session has its own allowance.
    let other = h.create(Some("other"));
    let other = h.session_caller(&other.id);
    session(h.handle(&other, Request::SessionCreate(agent_create(&h, "o0"))));
}

fn bind(conversations: &[&str]) -> Request {
    Request::SessionBind {
        agent: "opencode".into(),
        conversations: conversations.iter().map(|c| c.to_string()).collect(),
    }
}

#[test]
fn user_clients_act_for_the_session_that_bound_a_conversation() {
    let h = Harness::new();
    let a = h.create(Some("a"));
    let b = h.create(Some("b"));
    let delegated = |token: &str, agent: &str, conversation: &str| {
        h.control.authenticate(&hello(Auth::Delegated {
            token: token.into(),
            agent: agent.into(),
            conversation: conversation.into(),
        }))
    };
    let user_token = h.user_token.as_str().to_owned();
    let as_conversation = |conversation: &str| delegated(&user_token, "opencode", conversation);

    let err = as_conversation("ses_1").unwrap_err();
    assert_eq!(err.code, ErrorCode::Unauthenticated);
    assert!(err.message.contains("ses_1"), "{}", err.message);

    let a_caller = h.session_caller(&a.id);
    let response = h.handle(&a_caller, bind(&["ses_1", "ses_2"]));
    assert!(
        matches!(response, session_core::api::Response::Ok),
        "{response:?}"
    );
    let caller = as_conversation("ses_1").unwrap();
    assert_eq!(caller.session_id(), Some(&a.id));
    // It is that session: creates land in its project, as its children.
    let child = session(h.handle(
        &caller,
        Request::SessionCreate(CreateSession {
            scope: None,
            ..h.create_request()
        }),
    ));
    assert_eq!(child.parent_id.as_ref(), Some(&a.id));
    // The user token is required, and the agent kind must match.
    assert_eq!(
        delegated("wrong", "opencode", "ses_1").unwrap_err().code,
        ErrorCode::Unauthenticated
    );
    assert_eq!(
        delegated(&user_token, "claude", "ses_1").unwrap_err().code,
        ErrorCode::Unauthenticated
    );

    // The newest binding wins.
    let b_caller = h.session_caller(&b.id);
    h.handle(&b_caller, bind(&["ses_1"]));
    assert_eq!(as_conversation("ses_1").unwrap().session_id(), Some(&b.id));
    assert_eq!(as_conversation("ses_2").unwrap().session_id(), Some(&a.id));

    // Only sessions bind, and only plausible ids.
    for (caller, request) in [
        (&h.user, bind(&["ses_9"])),
        (&a_caller, bind(&[])),
        (&a_caller, bind(&["has space"])),
        (
            &a_caller,
            Request::SessionBind {
                agent: "Open Code".into(),
                conversations: vec!["ses_9".into()],
            },
        ),
    ] {
        assert_eq!(
            error(h.handle(caller, request)).code,
            ErrorCode::InvalidArgument
        );
    }

    // Bindings end with their session; an older one takes over.
    h.host.exit(&b.id, Some(0));
    h.wait_recorded_exit(&b.id);
    assert_eq!(as_conversation("ses_1").unwrap().session_id(), Some(&a.id));
    h.host.exit(&a.id, Some(0));
    h.wait_recorded_exit(&a.id);
    assert_eq!(
        as_conversation("ses_2").unwrap_err().code,
        ErrorCode::Unauthenticated
    );

    let binds: Vec<_> = h
        .audit()
        .into_iter()
        .filter(|row| row.method == "session.bind")
        .map(|row| row.decision)
        .collect();
    assert_eq!(binds[..2], ["allowed".to_string(), "allowed".to_string()]);
}

#[test]
fn audit_records_mutations_and_denials_but_not_reads() {
    let h = Harness::new();
    let a = h.create(Some("alpha"));
    let b = h.create(Some("beta"));
    h.handle(
        &h.user,
        Request::SessionRename {
            id: a.id.clone(),
            name: "alpha two".into(),
        },
    );
    h.handle(
        &h.user,
        Request::SessionSubmit {
            id: b.id.clone(),
            text: format!("echo {}", "x".repeat(500)),
        },
    );
    h.handle(&h.user, Request::SessionGet { id: a.id.clone() });
    let agent = h.session_caller(&a.id);
    error(h.handle(
        &agent,
        Request::SessionKill {
            id: b.id.clone(),
            tree: false,
        },
    ));
    error(h.handle(
        &h.user,
        Request::SessionKill {
            id: SessionId::from_string("MISSING"),
            tree: false,
        },
    ));

    let rows = h.audit();
    let summary: Vec<_> = rows
        .iter()
        .map(|r| (r.caller.as_str(), r.method.as_str(), r.decision.as_str()))
        .collect();
    let agent_label = format!("session:{}", a.id);
    assert_eq!(
        summary,
        vec![
            ("user:test", "session.create", "allowed"),
            ("user:test", "session.create", "allowed"),
            ("user:test", "session.rename", "allowed"),
            ("user:test", "session.submit", "allowed"),
            (agent_label.as_str(), "session.kill", "denied"),
            ("user:test", "session.kill", "error"),
        ]
    );
    assert_eq!(rows[0].target.as_deref(), Some(a.id.as_str()));
    assert_eq!(
        rows[0].scope.as_deref(),
        Some(path_str(h.scope.path()).as_str())
    );
    assert_eq!(rows[2].detail.as_deref(), Some("alpha two"));
    let submitted = rows[3].detail.as_deref().unwrap();
    assert_eq!(
        submitted.chars().count(),
        201,
        "detail is clipped to 200 chars"
    );
    assert!(rows[4].detail.as_deref().unwrap().contains("later release"));
    assert_eq!(rows[5].target.as_deref(), Some("MISSING"));
}
