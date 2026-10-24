//! Waits, lifecycle (Done/Idle) and event subscriptions.

mod control_common;

use control_common::*;
use ptyd::control_api::{Caller, Control};
use session_core::api::{
    AttachRequest, ErrorCode, EventEnvelope, EventKind, ReadRequest, ReadSource, Report, Request,
    Response, Role, SessionEvent, SubscribeRequest, TextFormat, WaitRequest, WaitResult, WaitUntil,
};
use session_core::ids::SessionId;
use session_core::model::{Lifecycle, SessionStatus};
use std::sync::mpsc::Receiver;
use std::sync::Arc;
use std::time::{Duration, Instant};

fn wait(
    h: &Harness,
    caller: &Caller,
    id: &SessionId,
    until: WaitUntil,
    pattern: Option<&str>,
    timeout_ms: u64,
) -> WaitResult {
    match h.handle(
        caller,
        Request::SessionWait(WaitRequest {
            id: id.clone(),
            until,
            pattern: pattern.map(str::to_string),
            timeout_ms,
        }),
    ) {
        Response::Wait(result) => result,
        other => panic!("expected a wait result, got {other:?}"),
    }
}

fn lifecycle_of(h: &Harness, id: &SessionId) -> Lifecycle {
    h.get(id).lifecycle
}

fn settle(h: &Harness, id: &SessionId, lifecycle: Lifecycle) {
    eventually(&format!("{id} to become {lifecycle:?}"), || {
        (lifecycle_of(h, id) == lifecycle).then_some(())
    });
}

/// Run `f` on another thread after `delay` (to change state during a wait).
fn later(delay: Duration, f: impl FnOnce() + Send + 'static) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        std::thread::sleep(delay);
        f();
    })
}

fn drain(rx: &Receiver<EventEnvelope>) -> Vec<EventEnvelope> {
    // Events are published on the processing thread; give it a moment.
    let mut out = Vec::new();
    while let Ok(event) = rx.recv_timeout(Duration::from_millis(300)) {
        out.push(event);
    }
    out
}

fn kinds(events: &[EventEnvelope]) -> Vec<(String, &'static str)> {
    events
        .iter()
        .map(|e| {
            let kind = match &e.event {
                SessionEvent::Created { .. } => "created",
                SessionEvent::Updated { .. } => "updated",
                SessionEvent::Removed => "removed",
                SessionEvent::Exited { .. } => "exited",
                SessionEvent::Lifecycle { .. } => "lifecycle",
                SessionEvent::Reported { .. } => "reported",
                SessionEvent::Activity => "activity",
                SessionEvent::Reply { .. } => "reply",
                SessionEvent::Unknown => "unknown",
            };
            (e.session_id.to_string(), kind)
        })
        .collect()
}

#[test]
fn wait_for_output_matches_or_times_out() {
    let h = Harness::new();
    let s = h.create(Some("build"));
    let host = h.host.clone();
    let id = s.id.clone();
    let writer = later(Duration::from_millis(150), move || {
        host.show(&id, "$ make\nbuild ok\n$")
    });
    let started = Instant::now();
    let result = wait(
        &h,
        &h.user,
        &s.id,
        WaitUntil::Output,
        Some("build ok"),
        5000,
    );
    writer.join().unwrap();
    assert!(result.matched, "{result:?}");
    assert!(started.elapsed() < Duration::from_secs(3));
    assert_eq!(result.status, SessionStatus::Live);

    let started = Instant::now();
    let result = wait(&h, &h.user, &s.id, WaitUntil::Output, Some("failed"), 300);
    assert!(!result.matched);
    assert_eq!(result.reason, "timeout after 300 ms");
    assert!(started.elapsed() >= Duration::from_millis(300));

    let err = error(h.handle(
        &h.user,
        Request::SessionWait(WaitRequest {
            id: s.id.clone(),
            until: WaitUntil::Output,
            pattern: None,
            timeout_ms: 10,
        }),
    ));
    assert_eq!(err.code, ErrorCode::InvalidArgument);
}

#[test]
fn waits_end_when_the_session_exits() {
    let h = Harness::new();
    let s = h.create(Some("job"));
    let host = h.host.clone();
    let id = s.id.clone();
    let exiter = later(Duration::from_millis(150), move || host.exit(&id, Some(3)));
    let started = Instant::now();
    let result = wait(&h, &h.user, &s.id, WaitUntil::Idle, None, 10_000);
    exiter.join().unwrap();
    assert!(!result.matched);
    assert_eq!(result.reason, "session exited");
    assert_eq!(result.status, SessionStatus::Exited);
    assert!(started.elapsed() < Duration::from_secs(5));

    let result = wait(&h, &h.user, &s.id, WaitUntil::Exited, None, 0);
    assert!(result.matched);
    let result = wait(&h, &h.user, &s.id, WaitUntil::Settled, None, 0);
    assert!(result.matched, "an ended session is settled");
}

#[test]
fn wait_until_reported_wakes_on_a_new_report() {
    let h = Arc::new(Harness::new());
    let s = h.create(Some("child"));
    let reporter = h.session_caller(&s.id);
    // A report from before the wait doesn't count.
    h.handle(
        &reporter,
        Request::SessionReport(Report {
            text: Some("starting".into()),
            ..Default::default()
        }),
    );
    let h2 = h.clone();
    let id = s.id.clone();
    let thread = later(Duration::from_millis(150), move || {
        let caller = h2.session_caller(&id);
        h2.handle(
            &caller,
            Request::SessionReport(Report {
                state: Some(Lifecycle::Done),
                text: Some("all tests pass".into()),
                ..Default::default()
            }),
        );
    });
    let result = wait(&h, &h.user, &s.id, WaitUntil::Reported, None, 5000);
    thread.join().unwrap();
    assert!(result.matched, "{result:?}");
    assert_eq!(
        result.reported.unwrap().text.as_deref(),
        Some("all tests pass")
    );
    assert_eq!(result.lifecycle, Lifecycle::Done);
}

#[test]
fn working_then_idle_is_done_until_touched_or_controlled() {
    let h = Harness::with_detector(marker_detector);
    let s = h.create(Some("agent"));
    assert_eq!(lifecycle_of(&h, &s.id), Lifecycle::None);

    h.host.show(&s.id, "> [idle]");
    settle(&h, &s.id, Lifecycle::Idle);
    assert_eq!(h.get(&s.id).agent.unwrap().kind, "fakeagent");

    h.host.show(&s.id, "thinking… [working]");
    let result = wait(&h, &h.user, &s.id, WaitUntil::Working, None, 2000);
    assert!(result.matched);

    let host = h.host.clone();
    let id = s.id.clone();
    let finisher = later(Duration::from_millis(100), move || {
        host.show(&id, "> [idle]")
    });
    let result = wait(&h, &h.user, &s.id, WaitUntil::Settled, None, 5000);
    finisher.join().unwrap();
    assert!(result.matched);
    assert_eq!(result.lifecycle, Lifecycle::Done);

    // Reading and waiting don't count as seeing it.
    let observer = h.create(Some("observer"));
    let agent = h.session_caller(&observer.id);
    h.handle(
        &agent,
        Request::SessionRead(ReadRequest {
            id: s.id.clone(),
            source: ReadSource::Visible,
            lines: None,
            format: TextFormat::Text,
        }),
    );
    wait(&h, &agent, &s.id, WaitUntil::Idle, None, 0);
    wait(&h, &h.user, &s.id, WaitUntil::Idle, None, 0);
    assert_eq!(lifecycle_of(&h, &s.id), Lifecycle::Done);
    // Further idle detections keep it Done.
    h.host.show(&s.id, "> [idle] ");
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(lifecycle_of(&h, &s.id), Lifecycle::Done);

    assert_eq!(
        h.handle(&h.user, Request::SessionTouch { id: s.id.clone() }),
        Response::Ok
    );
    assert_eq!(lifecycle_of(&h, &s.id), Lifecycle::Idle);

    // Observing doesn't mark it seen; taking control does.
    h.host.show(&s.id, "[working]");
    settle(&h, &s.id, Lifecycle::Working);
    h.host.show(&s.id, "[idle]");
    settle(&h, &s.id, Lifecycle::Done);
    let attach = |role| AttachRequest {
        id: s.id.clone(),
        role,
        cols: 80,
        rows: 24,
    };
    h.control
        .prepare_attach(&h.user, &attach(Role::Observer))
        .unwrap();
    assert_eq!(lifecycle_of(&h, &s.id), Lifecycle::Done);
    h.control
        .prepare_attach(&h.user, &attach(Role::Controller))
        .unwrap();
    assert_eq!(lifecycle_of(&h, &s.id), Lifecycle::Idle);

    // Blocked is not a finished turn.
    h.host.show(&s.id, "[working]");
    settle(&h, &s.id, Lifecycle::Working);
    h.host.show(&s.id, "Allow edit? [blocked]");
    let result = wait(&h, &h.user, &s.id, WaitUntil::Blocked, None, 2000);
    assert!(result.matched);
    h.host.show(&s.id, "[idle]");
    settle(&h, &s.id, Lifecycle::Idle);
}

#[test]
fn self_reports_drive_lifecycle_and_report_done() {
    let h = Harness::new();
    let s = h.create(Some("reporter"));
    let me = h.session_caller(&s.id);
    let report = |state| {
        h.handle(
            &me,
            Request::SessionReport(Report {
                state: Some(state),
                ..Default::default()
            }),
        )
    };
    report(Lifecycle::Working);
    assert_eq!(lifecycle_of(&h, &s.id), Lifecycle::Working);
    report(Lifecycle::Idle);
    assert_eq!(lifecycle_of(&h, &s.id), Lifecycle::Done);
    // A session may touch itself (e.g. the TUI is not the only viewer).
    h.handle(&me, Request::SessionTouch { id: s.id.clone() });
    assert_eq!(lifecycle_of(&h, &s.id), Lifecycle::Idle);
    report(Lifecycle::Done);
    assert_eq!(lifecycle_of(&h, &s.id), Lifecycle::Done);
}

#[test]
fn events_are_filtered_by_scope_ids_and_kinds() {
    let h = Harness::new();
    let first = h.create(Some("first"));
    let subscribe = |request: SubscribeRequest| h.control.subscribe(&h.user, &request).unwrap();
    let all = subscribe(SubscribeRequest {
        scope: Some(h.scope.clone()),
        ..Default::default()
    });
    let only_first = subscribe(SubscribeRequest {
        scopes: Vec::new(),
        scope: Some(h.scope.clone()),
        session_ids: vec![first.id.clone()],
        kinds: vec![],
        all: false,
        replies: false,
    });
    let exits = subscribe(SubscribeRequest {
        scopes: Vec::new(),
        scope: Some(h.scope.clone()),
        session_ids: vec![],
        kinds: vec![EventKind::Exited, EventKind::Removed],
        all: false,
        replies: false,
    });
    let other = subscribe(SubscribeRequest {
        scope: Some(h.other_scope()),
        ..Default::default()
    });
    let me = h.session_caller(&first.id);
    let mine = h
        .control
        .subscribe(&me, &SubscribeRequest::default())
        .unwrap();

    let second = h.create(Some("second"));
    let elsewhere = h.create_in(&h.other_scope(), Some("elsewhere"));
    h.handle(
        &h.user,
        Request::SessionRename {
            id: first.id.clone(),
            name: "first renamed".into(),
        },
    );
    h.host.exit(&second.id, Some(0));
    h.wait_recorded_exit(&second.id);
    h.host.show(&first.id, "output");
    h.handle(
        &h.user,
        Request::SessionDelete {
            id: second.id.clone(),
        },
    );

    let f = first.id.to_string();
    let s = second.id.to_string();
    let e = elsewhere.id.to_string();
    let all_events = drain(&all);
    let got = kinds(&all_events);
    for expected in [
        (s.clone(), "created"),
        (f.clone(), "updated"),
        (s.clone(), "exited"),
        (s.clone(), "updated"),
        (f.clone(), "activity"),
        (s.clone(), "removed"),
    ] {
        assert!(got.contains(&expected), "missing {expected:?} in {got:?}");
    }
    assert!(got.iter().all(|(id, _)| *id != e));
    // Order: created before exited before removed for the second session.
    let pos = |what: (String, &str)| got.iter().position(|g| *g == what).unwrap();
    assert!(pos((s.clone(), "created")) < pos((s.clone(), "exited")));
    assert!(pos((s.clone(), "exited")) < pos((s.clone(), "removed")));

    let got = kinds(&drain(&only_first));
    assert!(
        !got.is_empty() && got.iter().all(|(id, _)| *id == f),
        "{got:?}"
    );

    let got = kinds(&drain(&exits));
    assert_eq!(got, vec![(s.clone(), "exited"), (s.clone(), "removed")]);

    let got = kinds(&drain(&other));
    assert_eq!(got, vec![(e.clone(), "created")]);

    // A session subscriber sees its own scope, with is_self on its views.
    let mine_events = drain(&mine);
    assert!(mine_events.iter().all(|ev| ev.scope == h.scope));
    let renamed = mine_events
        .iter()
        .find_map(|ev| match &ev.event {
            SessionEvent::Updated { session } if session.id == first.id => Some(session.clone()),
            _ => None,
        })
        .expect("update of self");
    assert!(renamed.is_self);
    assert_eq!(renamed.name, "first renamed");
    let created_second = mine_events
        .iter()
        .find_map(|ev| match &ev.event {
            SessionEvent::Created { session } => Some(session.clone()),
            _ => None,
        })
        .unwrap();
    assert!(!created_second.is_self);

    // Ids must exist and be in the subscribed scope.
    let err = h
        .control
        .subscribe(
            &h.user,
            &SubscribeRequest {
                scopes: Vec::new(),
                scope: Some(h.scope.clone()),
                session_ids: vec![elsewhere.id.clone()],
                kinds: vec![],
                all: false,
                replies: false,
            },
        )
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::OutOfScope);
    let err = h
        .control
        .subscribe(&h.user, &SubscribeRequest::default())
        .unwrap_err();
    assert_eq!(
        err.code,
        ErrorCode::InvalidArgument,
        "users must name a scope"
    );
}

#[test]
fn activity_is_throttled_and_reports_publish_events() {
    let h = Harness::with_detector(marker_detector);
    let s = h.create(Some("busy"));
    let rx = h
        .control
        .subscribe(
            &h.user,
            &SubscribeRequest {
                scopes: Vec::new(),
                scope: Some(h.scope.clone()),
                session_ids: vec![s.id.clone()],
                kinds: vec![
                    EventKind::Activity,
                    EventKind::Lifecycle,
                    EventKind::Reported,
                ],
                all: false,
                replies: false,
            },
        )
        .unwrap();
    for i in 0..5 {
        h.host.show(&s.id, &format!("line {i}"));
    }
    let got = kinds(&drain(&rx));
    assert_eq!(
        got.iter().filter(|(_, k)| *k == "activity").count(),
        1,
        "≤1 activity event per second: {got:?}"
    );

    h.host.show(&s.id, "[working]");
    settle(&h, &s.id, Lifecycle::Working);
    let me = h.session_caller(&s.id);
    h.handle(
        &me,
        Request::SessionReport(Report {
            state: Some(Lifecycle::Blocked),
            text: Some("need approval".into()),
            ..Default::default()
        }),
    );
    let events = drain(&rx);
    let lifecycles: Vec<Lifecycle> = events
        .iter()
        .filter_map(|e| match e.event {
            SessionEvent::Lifecycle { lifecycle } => Some(lifecycle),
            _ => None,
        })
        .collect();
    assert_eq!(lifecycles, vec![Lifecycle::Working, Lifecycle::Blocked]);
    assert!(events.iter().any(|e| matches!(
        &e.event,
        SessionEvent::Reported { reported } if reported.text.as_deref() == Some("need approval")
    )));
}

#[test]
fn shutdown_ends_waits_and_subscriptions() {
    let h = Arc::new(Harness::new());
    let s = h.create(Some("long"));
    let rx = h
        .control
        .subscribe(
            &h.user,
            &SubscribeRequest {
                scope: Some(h.scope.clone()),
                ..Default::default()
            },
        )
        .unwrap();
    let h2 = h.clone();
    let id = s.id.clone();
    let waiter = std::thread::spawn(move || {
        h2.handle(
            &h2.user,
            Request::SessionWait(WaitRequest {
                id,
                until: WaitUntil::Exited,
                pattern: None,
                timeout_ms: 60_000,
            }),
        )
    });
    std::thread::sleep(Duration::from_millis(100));
    let started = Instant::now();
    h.control.shutdown();
    let response = waiter.join().unwrap();
    assert!(started.elapsed() < Duration::from_secs(2));
    assert_eq!(error(response).code, ErrorCode::DaemonUnavailable);
    while rx.try_recv().is_ok() {}
    assert!(rx.recv_timeout(Duration::from_millis(100)).is_err());
    assert_eq!(
        error(h.handle(&h.user, Request::SessionGet { id: s.id.clone() })).code,
        ErrorCode::DaemonUnavailable
    );
}

#[test]
fn waits_stop_at_the_clients_blocking_limit() {
    let h = Harness::new();
    let s = h.create(Some("build"));
    let mut limited = h.user.clone();
    limited.max_blocking_ms = Some(200);
    let started = Instant::now();
    let result = wait(&h, &limited, &s.id, WaitUntil::Output, Some("never"), 5000);
    assert!(!result.matched);
    assert!(
        result.reason.contains("the client's limit"),
        "{}",
        result.reason
    );
    assert!(started.elapsed() < Duration::from_secs(2));

    // A shorter requested timeout than the limit reports a plain timeout.
    let result = wait(&h, &limited, &s.id, WaitUntil::Output, Some("never"), 100);
    assert_eq!(result.reason, "timeout after 100 ms");
}
