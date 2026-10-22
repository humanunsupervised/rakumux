//! Daemon views, events and scoped capture driving the dispatch coordinator: fake
//! SessionApi and HTTP only.

use session_core::activity::AgentActivity;
use session_core::api::{
    ApiError, ErrorCode, EventEnvelope, ReadRequest, ReadSource, SessionEvent, TextFormat,
};
use session_core::model::{Reported, SessionStatus, SessionView};

use super::*;
use crate::source::fake::{live_view, FakeSessionApi};
use crate::source::{CaptureError, SessionCapture, SourceError, OUTPUT_LINES};

const PID: u32 = 4242;
const TERMINAL_SECRET: &str = "synthetic-terminal-secret";
const DIAGNOSTIC: &str = "synthetic-private-daemon-diagnostic";

pub(super) fn tracked_view(number: u8) -> SessionView {
    let mut view = live_view(&session(number), FOLDER, PID);
    view.reported = Some(Reported {
        state: None,
        text: Some(REPORT.into()),
        result: None,
        at: 3,
    });
    view.activity = Some(AgentActivity {
        phase: ActivityPhase::RunningTool,
        active_subagents: 0,
        updated_at: 3,
    });
    view
}

pub(super) fn event(number: u8, event: SessionEvent) -> EventEnvelope {
    EventEnvelope {
        scope: folder(),
        session_id: session(number),
        at: 1,
        event,
    }
}

/// An enabled owner that applied the daemon's view of session `number`.
fn tracked(include_output: bool, number: u8) -> Fixture {
    let mut fixture = Fixture::enabled(include_output);
    fixture.owner.apply_view(&tracked_view(number)).unwrap();
    fixture
}

fn capture_with(
    owner: &mut Owner,
    api: &FakeSessionApi,
    job: &StatusCaptureJob,
) -> Result<SessionCapture, DispatchError> {
    Ok(owner.expectation(job)?.capture(api, job)?)
}

fn build_captured(
    owner: &mut Owner,
    job: &StatusCaptureJob,
    captured: &SessionCapture,
) -> Result<PreparedStatusDispatch, DispatchError> {
    owner.build_capture(job, captured, &[])
}

fn sent_context(fixture: &Fixture) -> Value {
    let requests = fixture.requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    let value: Value = serde_json::from_slice(&requests[0].body).unwrap();
    value["context"].clone()
}

fn changed(error: DispatchError) {
    assert_eq!(error, DispatchError::Capture(CaptureError::Changed));
}

#[test]
fn tracked_capture_feeds_dispatch_and_later_output_is_coalesced() {
    let mut fixture = tracked(true, 7);
    let output =
        format!("{OUTPUT}\n\u{26a0}\u{fe0f} token={TERMINAL_SECRET}\n{FOLDER}/main.rs\n\n\n");
    let api = FakeSessionApi::user(tracked_view(7), &output);
    let job = capture(&mut fixture.owner, 7);
    let expectation = fixture.owner.expectation(&job).unwrap();
    assert_redacted(&format!("{expectation:?}"));
    let captured = thread::scope(|scope| {
        scope
            .spawn(|| expectation.capture(&api, &job))
            .join()
            .unwrap()
    })
    .unwrap();
    assert_redacted(&format!("{captured:?}"));
    let get = format!("get {}", session(7));
    assert_eq!(
        api.calls(),
        [get.clone(), format!("read {}", session(7)), get]
    );
    assert_eq!(
        api.reads(),
        [ReadRequest {
            id: session(7),
            source: ReadSource::RecentUnwrapped,
            lines: Some(OUTPUT_LINES),
            format: TextFormat::Text,
        }]
    );
    assert_eq!(captured.stamp(), job.source_stamp());
    let prepared = build_captured(&mut fixture.owner, &job, &captured).unwrap();
    let worker = fixture.owner.commit(prepared).unwrap();
    assert_eq!(
        fixture.owner.finish(worker.run()).unwrap(),
        ApplyResult::Applied
    );

    let context = sent_context(&fixture);
    assert_eq!(context["lifecycle"], "working");
    assert_eq!(context["activity_phase"], "running_tool");
    assert_eq!(context["reported_text"], REPORT);
    let sent = context["recent_output"].as_str().unwrap();
    assert!(sent.starts_with(OUTPUT));
    assert!(sent.contains("\u{26a0} [REDACTED]"));
    assert!(sent.ends_with("[PATH]"));
    let body = String::from_utf8_lossy(&fixture.requests.lock().unwrap()[0].body).into_owned();
    for private in [
        TERMINAL_SECRET,
        FOLDER,
        "synthetic-local-session-7",
        "synthetic-session-name",
        "synthetic-cwd",
        "synthetic-title",
        "synthetic-agent",
        "\u{fe0f}",
    ] {
        assert!(!body.contains(private), "leaked {private}");
    }
    let StatusView::Ready(entry) = fixture.owner.view(&session(7)).unwrap() else {
        panic!("expected cached synthetic status");
    };
    assert_eq!(entry.text.as_str(), STATUS);
    assert_eq!(entry.stamp, job.source_stamp());

    // Busy output keeps the advisory text visible until the owner advances it.
    for _ in 0..3 {
        fixture
            .owner
            .apply_event(&event(7, SessionEvent::Activity))
            .unwrap();
    }
    assert!(fixture.owner.is_dirty(&session(7)));
    assert!(matches!(
        fixture.owner.view(&session(7)).unwrap(),
        StatusView::Ready(_)
    ));
    assert!(fixture.owner.advance(&session(7)).unwrap());
    assert!(!fixture.owner.advance(&session(7)).unwrap());
    assert_eq!(fixture.owner.view(&session(7)).unwrap(), StatusView::Stale);
    let next = capture(&mut fixture.owner, 7);
    assert_eq!(next.source_stamp().source_revision, 2);
    fixture.owner.acknowledge_capture(&next).unwrap();
    assert_eq!(fixture.calls(), 1);
}

#[test]
fn output_during_capture_keeps_the_job_but_fencing_changes_cancel_it() {
    let mut fixture = tracked(true, 7);
    let api = FakeSessionApi::user(tracked_view(7), OUTPUT);
    let job = capture(&mut fixture.owner, 7);
    let expectation = fixture.owner.expectation(&job).unwrap();
    fixture
        .owner
        .apply_event(&event(7, SessionEvent::Activity))
        .unwrap();
    let captured = expectation.capture(&api, &job).unwrap();
    let prepared = build_captured(&mut fixture.owner, &job, &captured).unwrap();
    let completion = fixture.owner.commit(prepared).unwrap().abandon();
    assert_eq!(
        fixture.owner.finish(completion).unwrap(),
        ApplyResult::Ignored
    );
    assert!(fixture.owner.is_dirty(&session(7)));

    let job = capture(&mut fixture.owner, 7);
    let captured = capture_with(&mut fixture.owner, &api, &job).unwrap();
    let blocked = SessionEvent::Lifecycle {
        lifecycle: Lifecycle::Blocked,
    };
    fixture.owner.apply_event(&event(7, blocked)).unwrap();
    assert!(job.is_canceled());
    assert_eq!(
        build_captured(&mut fixture.owner, &job, &captured).unwrap_err(),
        DispatchError::Canceled
    );
    assert_eq!(
        fixture.owner.expectation(&job).unwrap_err(),
        DispatchError::Capture(CaptureError::Canceled)
    );
    assert_eq!(
        fixture.owner.acknowledge_capture(&job).unwrap(),
        ApplyResult::Applied
    );
    assert!(!fixture.owner.is_dirty(&session(7)));
    assert_eq!(fixture.calls(), 0);
    fixture.assert_no_cache();
}

#[test]
fn reports_only_jobs_never_read_terminal_output() {
    let mut fixture = tracked(false, 7);
    let api = FakeSessionApi::user(tracked_view(7), OUTPUT);
    let job = capture(&mut fixture.owner, 7);
    assert_eq!(job.input_selection(), InputSelection::ReportOnly);
    let captured = capture_with(&mut fixture.owner, &api, &job).unwrap();
    let get = format!("get {}", session(7));
    assert_eq!(api.calls(), [get.clone(), get]);
    assert!(api.reads().is_empty());
    assert_eq!(captured.input().reported_text, Some(REPORT));
    assert_eq!(captured.input().recent_output, None);
    let prepared = build_captured(&mut fixture.owner, &job, &captured).unwrap();
    let completion = fixture.owner.commit(prepared).unwrap().run();
    assert_eq!(
        fixture.owner.finish(completion).unwrap(),
        ApplyResult::Applied
    );
    let context = sent_context(&fixture);
    assert_eq!(context["reported_text"], REPORT);
    assert!(context.get("recent_output").is_none());
}

#[test]
fn changed_or_unreadable_sessions_are_refused_without_http() {
    let mut fixture = tracked(true, 7);
    let mut private = tracked_view(7);
    private.private = true;
    let mut blocked = tracked_view(7);
    blocked.lifecycle = Lifecycle::Blocked;
    let mut reported = tracked_view(7);
    reported.reported.as_mut().unwrap().at = 4;
    let mut replaced = tracked_view(7);
    replaced.terminal.as_mut().unwrap().pid = Some(PID + 1);
    let mut exited = tracked_view(7);
    exited.status = SessionStatus::Exited;
    let mut moved = tracked_view(7);
    moved.scope = ScopeRoot::from_canonical("/synthetic/relocated-folder".into());
    let error = |code| Err(ApiError::new(code, DIAGNOSTIC));
    let cases = vec![
        (vec![Ok(blocked)], CaptureError::Changed, 1),
        (vec![Ok(reported)], CaptureError::Changed, 1),
        (vec![Ok(moved)], CaptureError::Changed, 1),
        (vec![Ok(tracked_view(8))], CaptureError::Changed, 1),
        (
            vec![Ok(tracked_view(7)), Ok(private)],
            CaptureError::Changed,
            3,
        ),
        (
            vec![Ok(tracked_view(7)), Ok(replaced)],
            CaptureError::Changed,
            3,
        ),
        (
            vec![Ok(tracked_view(7)), Ok(exited)],
            CaptureError::Changed,
            3,
        ),
        (vec![error(ErrorCode::NotFound)], CaptureError::Changed, 1),
        (
            vec![error(ErrorCode::OutOfScope)],
            CaptureError::NotPermitted,
            1,
        ),
        (
            vec![error(ErrorCode::Internal)],
            CaptureError::Unavailable,
            1,
        ),
        (
            vec![Ok(tracked_view(7)), error(ErrorCode::Internal)],
            CaptureError::Unavailable,
            3,
        ),
    ];
    for (views, expected, calls) in cases {
        let api = FakeSessionApi::user(tracked_view(7), OUTPUT);
        api.script(views);
        let job = capture(&mut fixture.owner, 7);
        let failure = capture_with(&mut fixture.owner, &api, &job).unwrap_err();
        assert_eq!(failure, DispatchError::Capture(expected));
        assert_eq!(api.calls().len(), calls);
        assert!(!format!("{failure} {failure:?}").contains(DIAGNOSTIC));
        assert_eq!(
            fixture.owner.acknowledge_capture(&job).unwrap(),
            ApplyResult::Applied
        );
    }

    let api = FakeSessionApi::user(tracked_view(7), OUTPUT);
    api.set_output(Err(ApiError::new(ErrorCode::Internal, DIAGNOSTIC)));
    let job = capture(&mut fixture.owner, 7);
    let expectation = fixture.owner.expectation(&job).unwrap();
    assert_eq!(
        expectation.capture(&api, &job).unwrap_err(),
        CaptureError::Unavailable
    );
    assert_eq!(api.calls().len(), 2);
    let caller = FakeSessionApi::session_caller(tracked_view(7));
    assert_eq!(
        expectation.capture(&caller, &job).unwrap_err(),
        CaptureError::NotPermitted
    );
    assert!(caller.calls().is_empty());
    fixture.owner.acknowledge_capture(&job).unwrap();
    assert_eq!(fixture.calls(), 0);
    fixture.assert_no_cache();
}

#[test]
fn foreign_revoked_or_removed_jobs_make_no_daemon_requests() {
    let mut fixture = tracked(true, 7);
    let api = FakeSessionApi::user(tracked_view(7), OUTPUT);
    let job = capture(&mut fixture.owner, 7);
    let expectation = fixture.owner.expectation(&job).unwrap();
    fixture.owner.apply_view(&tracked_view(8)).unwrap();
    let other = capture(&mut fixture.owner, 8);
    changed(DispatchError::Capture(
        expectation.capture(&api, &other).unwrap_err(),
    ));
    fixture.owner.acknowledge_capture(&other).unwrap();

    let mut foreign = StatusDispatchService::new(
        fixture.account.clone(),
        fixture.store.clone(),
        cloud(ORIGIN, fixture.requests.clone(), ReplyMode::Success),
    );
    assert_eq!(
        foreign.expectation(&job).unwrap_err(),
        DispatchError::ForeignOwner
    );
    assert_eq!(
        foreign.apply_view(&tracked_view(7)),
        Ok(()),
        "a foreign coordinator tracks its own sources"
    );

    let revision = fixture.owner.consent().unwrap().revision;
    fixture.owner.disable(revision).unwrap();
    assert!(job.is_canceled());
    assert_eq!(
        expectation.capture(&api, &job).unwrap_err(),
        CaptureError::Canceled
    );
    assert_eq!(
        fixture.owner.expectation(&job).unwrap_err(),
        DispatchError::Capture(CaptureError::Canceled)
    );
    fixture.owner.acknowledge_capture(&job).unwrap();

    fixture.enable(true);
    let job = capture(&mut fixture.owner, 7);
    fixture
        .owner
        .apply_event(&event(7, SessionEvent::Removed))
        .unwrap();
    assert!(job.is_canceled());
    fixture.owner.acknowledge_capture(&job).unwrap();
    fixture.owner.apply_view(&tracked_view(7)).unwrap();
    assert_skipped(&mut fixture.owner, 7, StatusView::Missing);
    // Removing a session the owner never tracked changes nothing.
    fixture
        .owner
        .apply_event(&event(9, SessionEvent::Removed))
        .unwrap();
    fixture.owner.apply_view(&tracked_view(9)).unwrap();
    let job = capture(&mut fixture.owner, 9);
    fixture.owner.acknowledge_capture(&job).unwrap();
    assert!(api.calls().is_empty());
    assert_eq!(fixture.calls(), 0);
}

#[test]
fn a_capture_without_context_is_recorded_instead_of_retried() {
    let mut fixture = Fixture::enabled(true);
    let view = live_view(&session(7), FOLDER, PID);
    fixture.owner.apply_view(&view).unwrap();
    let api = FakeSessionApi::user(view, " \n\u{fe0f}\n\n");
    let job = capture(&mut fixture.owner, 7);
    let captured = capture_with(&mut fixture.owner, &api, &job).unwrap();
    assert_eq!(
        build_captured(&mut fixture.owner, &job, &captured).unwrap_err(),
        DispatchError::Cloud(CloudAIError::NoContext)
    );
    fixture.owner.acknowledge_capture(&job).unwrap();
    assert_eq!(
        fixture.owner.view(&session(7)).unwrap(),
        StatusView::NoContext
    );
    assert_skipped(&mut fixture.owner, 7, StatusView::NoContext);

    fixture
        .owner
        .apply_event(&event(7, SessionEvent::Activity))
        .unwrap();
    assert_skipped(&mut fixture.owner, 7, StatusView::NoContext);
    assert!(fixture.owner.advance(&session(7)).unwrap());
    let job = capture(&mut fixture.owner, 7);
    fixture.owner.acknowledge_capture(&job).unwrap();
    assert_eq!(fixture.calls(), 0);
}

#[test]
fn reports_only_no_context_leaves_output_for_a_later_output_consent() {
    let mut fixture = Fixture::enabled(false);
    let mut view = live_view(&session(7), FOLDER, PID);
    view.reported = Some(Reported {
        state: None,
        text: Some("token=synthetic-redaction-only".into()),
        result: None,
        at: 3,
    });
    fixture.owner.apply_view(&view).unwrap();
    let api = FakeSessionApi::user(view, OUTPUT);
    let job = capture(&mut fixture.owner, 7);
    let captured = capture_with(&mut fixture.owner, &api, &job).unwrap();
    assert_eq!(
        build_captured(&mut fixture.owner, &job, &captured).unwrap_err(),
        DispatchError::Cloud(CloudAIError::NoContext)
    );
    fixture.owner.acknowledge_capture(&job).unwrap();
    assert_skipped(&mut fixture.owner, 7, StatusView::NoContext);

    fixture.enable(true);
    let job = capture(&mut fixture.owner, 7);
    assert_eq!(job.input_selection(), InputSelection::ReportAndOutput);
    let captured = capture_with(&mut fixture.owner, &api, &job).unwrap();
    assert_eq!(captured.input().reported_text, None);
    assert_eq!(captured.input().recent_output, Some(OUTPUT));
    fixture.owner.acknowledge_capture(&job).unwrap();
}

#[test]
fn joined_credentials_are_redacted_and_other_invisibles_are_rejected() {
    let mut fixture = tracked(true, 7);
    let token = format!("sk-ant-{}", "A1b2C3d4".repeat(4));
    let (left, right) = token.split_at(12);
    let api = FakeSessionApi::user(tracked_view(7), &format!("key {left}\u{200d}{right} ok"));
    let job = capture(&mut fixture.owner, 7);
    let captured = capture_with(&mut fixture.owner, &api, &job).unwrap();
    let prepared = build_captured(&mut fixture.owner, &job, &captured).unwrap();
    let completion = fixture.owner.commit(prepared).unwrap().run();
    fixture.owner.finish(completion).unwrap();
    assert_eq!(sent_context(&fixture)["recent_output"], "key [REDACTED] ok");

    // A zero-width space is not joined or split: the builder rejects the window,
    // and the owner's refresh policy must not retry the same stamp.
    let mut fixture = tracked(true, 7);
    let api = FakeSessionApi::user(tracked_view(7), "pass\u{200b}word=synthetic");
    let job = capture(&mut fixture.owner, 7);
    let captured = capture_with(&mut fixture.owner, &api, &job).unwrap();
    assert_eq!(
        build_captured(&mut fixture.owner, &job, &captured).unwrap_err(),
        DispatchError::Cloud(CloudAIError::InvalidRequest)
    );
    fixture.owner.acknowledge_capture(&job).unwrap();
    assert_eq!(fixture.calls(), 0);
}

#[test]
fn failed_source_updates_fail_closed_until_consent_is_confirmed_again() {
    let mut fixture = tracked(true, 7);
    fixture.store.0.lock().unwrap().fail_remove = true;
    let mut private = tracked_view(7);
    private.private = true;
    assert_eq!(
        fixture.owner.apply_view(&private).unwrap_err(),
        DispatchError::Policy(StatusError::Store(StatusStoreError::Database))
    );
    assert_skipped(&mut fixture.owner, 7, StatusView::ConsentRequired);
    fixture.store.0.lock().unwrap().fail_remove = false;
    fixture.enable(true);
    assert_eq!(
        fixture.owner.view(&session(7)).unwrap(),
        StatusView::Private
    );
    assert_skipped(&mut fixture.owner, 7, StatusView::Private);

    // An invalid view of a session never tracked can't leave anything unrecorded.
    let mut invalid = tracked_view(8);
    invalid.scope = ScopeRoot::from_canonical("relative/synthetic-folder".into());
    fixture.owner.apply_view(&invalid).unwrap();
    assert_skipped(&mut fixture.owner, 8, StatusView::Missing);
    // One of a tracked session might: sharing stops until confirmed again.
    fixture.owner.apply_view(&tracked_view(8)).unwrap();
    assert_eq!(
        fixture.owner.apply_view(&invalid).unwrap_err(),
        DispatchError::Source(SourceError::Invalid)
    );
    assert_skipped(&mut fixture.owner, 8, StatusView::ConsentRequired);
    fixture.enable(true);
    assert_skipped(&mut fixture.owner, 7, StatusView::Private);
    let job = capture(&mut fixture.owner, 8);
    fixture.owner.acknowledge_capture(&job).unwrap();
    assert_eq!(fixture.calls(), 0);
}

#[test]
fn another_jobs_capture_is_refused_and_records_nothing() {
    let mut fixture = Fixture::enabled(true);
    let view = live_view(&session(7), FOLDER, PID);
    fixture.owner.apply_view(&view).unwrap();
    let api = FakeSessionApi::user(view, " \n");
    let first = capture(&mut fixture.owner, 7);
    let empty = capture_with(&mut fixture.owner, &api, &first).unwrap();
    fixture.owner.acknowledge_capture(&first).unwrap();
    let second = capture(&mut fixture.owner, 7);
    assert_eq!(second.source_stamp(), empty.stamp());
    assert_eq!(
        fixture.owner.expectation(&first).unwrap_err(),
        DispatchError::Capture(CaptureError::Canceled)
    );
    assert_eq!(
        build_captured(&mut fixture.owner, &second, &empty).unwrap_err(),
        DispatchError::Policy(StatusError::InvalidSource)
    );
    assert!(second.is_canceled());
    fixture.owner.acknowledge_capture(&second).unwrap();
    // The empty capture was never recorded against the session.
    let job = capture(&mut fixture.owner, 7);
    assert_eq!(job.source_stamp(), empty.stamp());
    fixture.owner.acknowledge_capture(&job).unwrap();
    assert_eq!(fixture.calls(), 0);
}
