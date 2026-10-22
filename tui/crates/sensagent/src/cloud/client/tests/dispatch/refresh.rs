//! Scheduled refreshing over the dispatch coordinator: the synchronous refresher with
//! fake time, and the threaded runner against a fake daemon. Fake SessionApi and HTTP only.

use std::sync::mpsc::Sender;
use std::sync::Arc;

use session_core::api::{EventEnvelope, SessionEvent};
use session_core::client::SessionApi;
use session_core::model::{Reported, SessionView};

use super::source::{event, tracked_view};
use super::*;
use crate::cloud::RemoteErrorCode;
use crate::refresh::{
    AIStatus, RefreshClient, RefreshError, RefreshFailure, RefreshRunner, RefreshState,
    StatusRefresher, BUDGET_STARTS,
};
use crate::source::fake::FakeSessionApi;

type Refresher = StatusRefresher<MemoryStatusStore, FileCredentialStore, FakeClock>;

struct Harness {
    _directory: tempfile::TempDir,
    store: MemoryStatusStore,
    requests: Requests,
    refresher: Refresher,
}

impl Harness {
    fn enabled(mode: ReplyMode) -> Self {
        let mut fixture = Fixture::new(mode);
        fixture.enable(true);
        Self::from(fixture)
    }

    fn reports_only() -> Self {
        let mut fixture = Fixture::new(ReplyMode::Success);
        fixture.enable(false);
        Self::from(fixture)
    }

    fn from(fixture: Fixture) -> Self {
        let Fixture {
            directory,
            store,
            requests,
            owner,
            ..
        } = fixture;
        Self {
            _directory: directory,
            store,
            requests,
            refresher: StatusRefresher::new(owner),
        }
    }

    /// Follow `views` as a fresh daemon list.
    fn resync(&mut self, api: &FakeSessionApi, views: Vec<SessionView>, now: u64) {
        api.set_listed(views.clone());
        self.refresher.resync(&views, secs(now)).unwrap();
    }

    /// Tick at `now` and run every capture and request inline. Returns the captures.
    fn round(&mut self, api: &FakeSessionApi, now: u64) -> usize {
        let work = self.refresher.tick(secs(now));
        let started = work.len();
        for capture in work {
            if let Some(dispatch) = self.refresher.captured(capture.capture(api), secs(now)) {
                self.refresher.completed(dispatch.run(), secs(now));
            }
        }
        started
    }

    fn event(&mut self, envelope: EventEnvelope, now: u64) {
        self.refresher.apply_event(&envelope, secs(now)).unwrap();
    }

    fn status(&mut self, number: u8) -> AIStatus {
        self.refresher.status(&session(number)).unwrap()
    }

    fn calls(&self) -> usize {
        self.requests.lock().unwrap().len()
    }
}

fn secs(seconds: u64) -> Duration {
    Duration::from_secs(seconds)
}

pub(super) fn ready(status: &AIStatus) -> bool {
    matches!(&status.view, StatusView::Ready(entry) if entry.text.as_str() == STATUS)
}

fn api(views: &[SessionView]) -> FakeSessionApi {
    let api = FakeSessionApi::user(views[0].clone(), OUTPUT);
    api.set_listed(views.to_vec());
    api
}

fn blocked(number: u8) -> SessionView {
    let mut view = tracked_view(number);
    view.lifecycle = Lifecycle::Blocked;
    view
}

#[test]
fn refreshing_waits_for_the_daemon_list_and_confirmed_consent() {
    let api = api(&[tracked_view(2)]);
    let mut harness = Harness::from(Fixture::new(ReplyMode::Success));
    harness
        .refresher
        .apply_view(&tracked_view(2), secs(0))
        .unwrap();
    assert!(harness.refresher.tick(secs(0)).is_empty());
    harness.resync(&api, vec![tracked_view(2)], 0);
    assert_eq!(harness.round(&api, 0), 0);
    assert_eq!(harness.status(2).view, StatusView::Disabled);
    assert!(api.calls().is_empty());

    let offer = harness.refresher.consent_offer().unwrap();
    harness.refresher.enable(&offer, folders(true)).unwrap();
    assert_eq!(harness.round(&api, 0), 1);
    assert!(ready(&harness.status(2)));
    assert_eq!(harness.status(2).failure, None);
    assert_eq!(harness.calls(), 1);
    for now in [1, 60, 600] {
        assert_eq!(harness.round(&api, now), 0);
    }
    assert_eq!(harness.calls(), 1);
    assert_eq!(
        harness.refresher.state(secs(600)),
        RefreshState {
            connected: true,
            rate_limited: false,
            unauthorized: false,
        }
    );
}

#[test]
fn busy_terminal_keeps_its_text_until_one_refresh_per_interval() {
    let api = api(&[tracked_view(2)]);
    let mut harness = Harness::enabled(ReplyMode::Success);
    harness.resync(&api, vec![tracked_view(2)], 0);
    assert_eq!(harness.round(&api, 0), 1);
    let first = harness.refresher.source_stamp(&session(2)).unwrap();
    for now in 1..=60 {
        harness.event(event(2, SessionEvent::Activity), now);
        assert_eq!(harness.round(&api, now), 0, "{now}");
        assert!(ready(&harness.status(2)), "{now}");
    }
    harness.event(event(2, SessionEvent::Activity), 61);
    assert_eq!(harness.round(&api, 61), 1);
    assert!(harness.refresher.source_stamp(&session(2)).unwrap() > first);
    assert!(ready(&harness.status(2)));
    assert_eq!(harness.calls(), 2);

    // Output that stops is refreshed once quiet, within the interval rule.
    harness.event(event(2, SessionEvent::Activity), 100);
    assert_eq!(harness.round(&api, 120), 0);
    assert_eq!(harness.round(&api, 121), 1);
    assert_eq!(harness.calls(), 3);
}

#[test]
fn lifecycle_changes_hide_text_at_once_and_refresh_after_the_interval() {
    let api = api(&[tracked_view(2)]);
    let mut harness = Harness::enabled(ReplyMode::Success);
    harness.resync(&api, vec![tracked_view(2)], 0);
    assert_eq!(harness.round(&api, 0), 1);
    api.set_listed(vec![blocked(2)]);
    harness.event(
        event(
            2,
            SessionEvent::Lifecycle {
                lifecycle: Lifecycle::Blocked,
            },
        ),
        10,
    );
    assert_eq!(harness.status(2).view, StatusView::Stale);
    assert_eq!(harness.round(&api, 59), 0);
    assert_eq!(harness.round(&api, 60), 1);
    assert!(ready(&harness.status(2)));
    assert_eq!(sent_lifecycles(&harness), ["working", "blocked"]);
}

fn sent_lifecycles(harness: &Harness) -> Vec<String> {
    harness
        .requests
        .lock()
        .unwrap()
        .iter()
        .map(|request| {
            let value: Value = serde_json::from_slice(&request.body).unwrap();
            value["context"]["lifecycle"].as_str().unwrap().to_owned()
        })
        .collect()
}

#[test]
fn two_jobs_and_the_start_budget_bound_a_backfill() {
    let views: Vec<SessionView> = (2..18).map(tracked_view).collect();
    let api = api(&views);
    let mut harness = Harness::enabled(ReplyMode::Success);
    harness.resync(&api, views, 0);

    let work = harness.refresher.tick(secs(0));
    assert_eq!(work.len(), 2);
    assert!(harness.refresher.tick(secs(0)).is_empty());
    for capture in work {
        let dispatch = harness
            .refresher
            .captured(capture.capture(&api), secs(0))
            .unwrap();
        harness.refresher.completed(dispatch.run(), secs(0));
    }
    let mut started = 2;
    loop {
        let round = harness.round(&api, 0);
        assert!(round <= 2);
        if round == 0 {
            break;
        }
        started += round;
    }
    assert_eq!(started, BUDGET_STARTS);
    assert_eq!(harness.calls(), BUDGET_STARTS);
    assert_eq!(harness.round(&api, 59), 0);
    assert_eq!(harness.round(&api, 60), 2);
    let missing = (2..18)
        .filter(|number| !ready(&harness.status(*number)))
        .count();
    assert_eq!(missing, 16 - BUDGET_STARTS - 2);
}

#[test]
fn rejected_input_is_not_captured_again_until_the_session_changes() {
    let mut view = tracked_view(2);
    view.reported = Some(Reported {
        state: None,
        text: Some(format!("{REPORT}\u{200b}")),
        result: None,
        at: 3,
    });
    let api = api(&[view.clone()]);
    let mut harness = Harness::enabled(ReplyMode::Success);
    harness.resync(&api, vec![view], 0);
    assert_eq!(harness.round(&api, 0), 1);
    assert_eq!(harness.calls(), 0);
    assert_eq!(harness.status(2).failure, Some(RefreshFailure::Rejected));
    assert_eq!(harness.round(&api, 600), 0);

    // New output allows another attempt; the report still can't be sent.
    harness.event(event(2, SessionEvent::Activity), 600);
    assert_eq!(harness.round(&api, 605), 1);
    assert_eq!(harness.calls(), 0);

    let reported = Reported {
        state: None,
        text: Some(REPORT.into()),
        result: None,
        at: 4,
    };
    let mut fixed = tracked_view(2);
    fixed.reported = Some(reported.clone());
    api.set_listed(vec![fixed]);
    harness.event(event(2, SessionEvent::Reported { reported }), 700);
    assert_eq!(harness.round(&api, 700), 1);
    assert_eq!(harness.calls(), 1);
    let status = harness.status(2);
    assert!(ready(&status));
    assert_eq!(status.failure, None);
}

#[test]
fn service_failures_back_off_for_one_session() {
    let api = api(&[tracked_view(2)]);
    let mut harness = Harness::enabled(ReplyMode::Remote(
        503,
        RemoteErrorCode::UpstreamUnavailable,
        None,
    ));
    harness.resync(&api, vec![tracked_view(2)], 0);
    assert_eq!(harness.round(&api, 0), 1);
    let status = harness.status(2);
    assert!(!ready(&status));
    assert_eq!(status.failure, Some(RefreshFailure::Unavailable));
    assert_eq!(harness.round(&api, 59), 0);
    assert_eq!(harness.round(&api, 60), 1);
    assert!(ready(&harness.status(2)));
    assert_eq!(harness.status(2).failure, None);
    assert_eq!(harness.calls(), 2);
}

#[test]
fn rate_limits_pause_every_session_including_a_capture_in_flight() {
    let api = api(&[tracked_view(2), tracked_view(3)]);
    let mut harness = Harness::enabled(ReplyMode::Remote(
        429,
        RemoteErrorCode::RateLimited,
        Some(120),
    ));
    harness.resync(&api, vec![tracked_view(2), tracked_view(3)], 0);
    // Session 3's capture finished after the pause began: it is not sent.
    assert_eq!(harness.round(&api, 0), 2);
    assert_eq!(harness.calls(), 1);
    assert_eq!(harness.status(2).failure, Some(RefreshFailure::RateLimited));
    assert_eq!(harness.status(3).failure, None);
    assert!(!ready(&harness.status(3)));
    assert!(harness.refresher.state(secs(1)).rate_limited);

    api.set_listed(vec![tracked_view(2), tracked_view(3), tracked_view(4)]);
    harness.event(
        event(
            4,
            SessionEvent::Created {
                session: tracked_view(4),
            },
        ),
        1,
    );
    assert_eq!(harness.round(&api, 119), 0);
    assert!(!harness.refresher.state(secs(120)).rate_limited);
    assert_eq!(harness.round(&api, 120), 2);
    assert_eq!(harness.round(&api, 120), 1);
    for number in [2, 3, 4] {
        assert!(ready(&harness.status(number)), "{number}");
    }
    assert_eq!(harness.calls(), 4);
}

#[test]
fn an_unauthorized_account_stops_refreshing_until_consent_is_confirmed_again() {
    let api = api(&[tracked_view(2), tracked_view(3)]);
    let mut harness = Harness::enabled(ReplyMode::Remote(
        401,
        RemoteErrorCode::Unauthenticated,
        None,
    ));
    harness.resync(&api, vec![tracked_view(2)], 0);
    assert_eq!(harness.round(&api, 0), 1);
    assert_eq!(
        harness.status(2).failure,
        Some(RefreshFailure::Unauthorized)
    );
    assert!(harness.refresher.state(secs(0)).unauthorized);
    harness.event(
        event(
            3,
            SessionEvent::Created {
                session: tracked_view(3),
            },
        ),
        1,
    );
    api.set_listed(vec![tracked_view(2), tracked_view(3)]);
    assert_eq!(harness.round(&api, 3600), 0);
    assert_eq!(harness.calls(), 1);

    let offer = harness.refresher.consent_offer().unwrap();
    harness.refresher.enable(&offer, folders(true)).unwrap();
    assert!(!harness.refresher.state(secs(3600)).unauthorized);
    assert_eq!(harness.round(&api, 3600), 2);
    assert!(ready(&harness.status(2)));
    assert!(ready(&harness.status(3)));
}

#[test]
fn a_session_changed_during_capture_is_corrected_from_the_daemon() {
    let api = api(&[blocked(2)]);
    let mut harness = Harness::enabled(ReplyMode::Success);
    harness
        .refresher
        .resync(&[tracked_view(2)], secs(0))
        .unwrap();
    let working = harness.refresher.source_stamp(&session(2)).unwrap();
    assert_eq!(harness.round(&api, 0), 1);
    assert_eq!(harness.calls(), 0);
    assert!(api.reads().is_empty());
    // The fresh view was applied: a new revision, and no failure to show.
    assert!(harness.refresher.source_stamp(&session(2)).unwrap() > working);
    assert_eq!(
        harness.status(2),
        AIStatus {
            view: StatusView::Missing,
            failure: None,
        }
    );

    assert_eq!(harness.round(&api, 59), 0);
    assert_eq!(harness.round(&api, 60), 1);
    assert!(ready(&harness.status(2)));
    assert_eq!(sent_lifecycles(&harness), ["blocked"]);
}

#[test]
fn a_refusal_stops_the_other_capture_in_flight() {
    let api = api(&[tracked_view(2), tracked_view(3)]);
    let mut harness = Harness::enabled(ReplyMode::Remote(403, RemoteErrorCode::Forbidden, None));
    harness.resync(&api, vec![tracked_view(2), tracked_view(3)], 0);
    assert_eq!(harness.round(&api, 0), 2);
    assert_eq!(harness.calls(), 1);
    assert_eq!(
        harness.status(2).failure,
        Some(RefreshFailure::Unauthorized)
    );
    assert_eq!(harness.status(3).failure, None);
    assert!(!ready(&harness.status(3)));
    assert_eq!(harness.round(&api, 3600), 0);
}

#[test]
fn a_correction_older_than_newer_events_is_not_applied() {
    let api = api(&[blocked(2)]);
    let mut harness = Harness::enabled(ReplyMode::Success);
    harness
        .refresher
        .resync(&[tracked_view(2)], secs(0))
        .unwrap();
    let work = harness.refresher.tick(secs(0));
    let captured = work.into_iter().next().unwrap().capture(&api);
    // The capture read "blocked"; meanwhile the session finished its turn.
    harness.event(
        event(
            2,
            SessionEvent::Lifecycle {
                lifecycle: Lifecycle::Done,
            },
        ),
        1,
    );
    assert!(harness.refresher.captured(captured, secs(1)).is_none());
    let mut done = tracked_view(2);
    done.lifecycle = Lifecycle::Done;
    api.set_listed(vec![done]);
    assert_eq!(harness.round(&api, 60), 1);
    assert_eq!(sent_lifecycles(&harness), ["done"]);
}

#[test]
fn reports_only_folders_ignore_output_until_the_report_changes() {
    let api = api(&[tracked_view(2)]);
    let mut harness = Harness::reports_only();
    harness.resync(&api, vec![tracked_view(2)], 0);
    assert_eq!(harness.round(&api, 0), 1);
    for now in [1, 70, 200] {
        harness.event(event(2, SessionEvent::Activity), now);
        assert_eq!(harness.round(&api, now + 10), 0);
        assert!(ready(&harness.status(2)));
    }
    assert_eq!(harness.calls(), 1);
    assert!(api.reads().is_empty());

    let reported = Reported {
        state: None,
        text: Some(format!("{REPORT} again")),
        result: None,
        at: 4,
    };
    let mut view = tracked_view(2);
    view.reported = Some(reported.clone());
    api.set_listed(vec![view]);
    harness.event(event(2, SessionEvent::Reported { reported }), 300);
    assert_eq!(harness.round(&api, 300), 1);
    assert_eq!(harness.calls(), 2);
}

#[test]
fn a_panicking_daemon_read_frees_its_slot_as_unreadable() {
    for call in [1, 2] {
        let api = api(&[tracked_view(2)]);
        let mut harness = Harness::enabled(ReplyMode::Success);
        if call == 2 {
            // The first read finds the session changed; the correcting read panics.
            api.set_listed(vec![blocked(2)]);
        }
        api.panic_on_get(call);
        harness
            .refresher
            .resync(&[tracked_view(2)], secs(0))
            .unwrap();
        assert_eq!(harness.round(&api, 0), 1);
        assert_eq!(harness.status(2).failure, Some(RefreshFailure::Unreadable));
        assert_eq!(harness.calls(), 0);
        api.set_listed(vec![tracked_view(2)]);
        assert_eq!(harness.round(&api, 60), 1);
        assert!(ready(&harness.status(2)), "{call}");
    }
}

#[test]
fn a_lost_event_stream_starts_nothing_until_the_next_list() {
    let api = api(&[tracked_view(2)]);
    let mut harness = Harness::enabled(ReplyMode::Success);
    harness.resync(&api, vec![tracked_view(2)], 0);
    harness.refresher.disconnected();
    assert!(!harness.refresher.state(secs(0)).connected);
    assert_eq!(harness.round(&api, 0), 0);
    harness.resync(&api, vec![tracked_view(2)], 1);
    assert_eq!(harness.round(&api, 1), 1);
}

#[test]
fn unselected_folders_are_not_advanced_for_their_output() {
    let mut view = tracked_view(2);
    view.scope = ScopeRoot::from_canonical("/synthetic/unselected-folder".into());
    let api = api(&[view.clone()]);
    let mut harness = Harness::enabled(ReplyMode::Success);
    harness.resync(&api, vec![view], 0);
    assert_eq!(harness.round(&api, 0), 0);
    assert_eq!(harness.status(2).view, StatusView::Excluded);
    let stamp = harness.refresher.source_stamp(&session(2));
    for now in [1, 70, 200] {
        harness.event(event(2, SessionEvent::Activity), now);
        assert_eq!(harness.round(&api, now + 10), 0);
    }
    assert_eq!(harness.refresher.source_stamp(&session(2)), stamp);
    assert!(api.calls().is_empty());
}

#[test]
fn consent_controls_reconsider_every_session() {
    let api = api(&[tracked_view(2)]);
    let mut harness = Harness::enabled(ReplyMode::Success);
    harness.resync(&api, vec![tracked_view(2)], 0);
    assert_eq!(harness.round(&api, 0), 1);
    let revision = harness.refresher.consent().unwrap().revision;
    harness.refresher.disable(revision).unwrap();
    assert_eq!(harness.status(2).view, StatusView::Disabled);
    assert_eq!(harness.round(&api, 600), 0);
    assert!(harness.store.0.lock().unwrap().cache.is_empty());

    let offer = harness.refresher.consent_offer().unwrap();
    harness.refresher.enable(&offer, folders(false)).unwrap();
    assert_eq!(harness.round(&api, 600), 1);
    assert!(ready(&harness.status(2)));
    assert_eq!(harness.calls(), 2);

    harness.refresher.sign_out().unwrap();
    assert_eq!(harness.status(2).view, StatusView::SignedOut);
    assert_eq!(harness.round(&api, 1200), 0);
}

#[test]
fn removed_sessions_are_forgotten_and_debug_output_is_redacted() {
    let api = api(&[tracked_view(2)]);
    let mut harness = Harness::enabled(ReplyMode::Success);
    harness.resync(&api, vec![tracked_view(2)], 0);
    let work = harness.refresher.tick(secs(0));
    assert_redacted(&format!("{:?}", work[0]));
    let captured = work.into_iter().next().unwrap().capture(&api);
    assert_redacted(&format!("{captured:?}"));
    let dispatch = harness.refresher.captured(captured, secs(0)).unwrap();
    assert_redacted(&format!("{dispatch:?}"));
    let completed = dispatch.run();
    assert_redacted(&format!("{completed:?}"));
    harness.refresher.completed(completed, secs(0));
    let debug = format!("{:?}", harness.refresher);
    assert!(!debug.contains("synthetic-local-session"), "{debug}");

    harness.event(event(2, SessionEvent::Removed), 1);
    assert_eq!(harness.refresher.source_stamp(&session(2)), None);
    assert_eq!(harness.round(&api, 600), 0);
    assert!(!ready(&harness.status(2)));
}

/// A refresher running on its own threads against a fake daemon.
pub(super) struct Background {
    _directory: tempfile::TempDir,
    pub(super) requests: Requests,
    pub(super) runner: RefreshRunner,
    pub(super) client: RefreshClient,
    pub(super) api: Arc<FakeSessionApi>,
}

impl Background {
    /// Queue `streams` event streams, then start an enabled refresher.
    pub(super) fn start(views: &[SessionView], streams: usize) -> (Self, Vec<Sender<Event>>) {
        let api = Arc::new(api(views));
        let events = (0..streams).map(|_| api.stream()).collect();
        let Harness {
            _directory,
            requests,
            refresher,
            ..
        } = Harness::enabled(ReplyMode::Success);
        let session_api: Arc<dyn SessionApi> = api.clone();
        let runner = RefreshRunner::spawn(refresher, session_api).unwrap();
        let client = runner.client();
        let background = Self {
            _directory,
            requests,
            runner,
            client,
            api,
        };
        (background, events)
    }

    fn status(&self, number: u8) -> AIStatus {
        self.client
            .statuses(vec![session(number)])
            .unwrap()
            .remove(0)
    }

    fn connected(&self) -> bool {
        self.client.state().unwrap().connected
    }

    fn count(&self, call: &str) -> usize {
        self.api
            .calls()
            .iter()
            .filter(|recorded| *recorded == call)
            .count()
    }
}

pub(super) type Event = Result<EventEnvelope, String>;

/// Poll `check` for up to ten seconds.
pub(super) fn eventually(mut check: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !check() {
        assert!(Instant::now() < deadline, "timed out");
        thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn the_runner_follows_the_daemon_and_refreshes_in_the_background() {
    let (background, events) = Background::start(&[tracked_view(2)], 1);
    eventually(|| ready(&background.status(2)));
    assert_eq!(background.api.calls()[..2], ["subscribe", "list_all"]);
    assert_eq!(background.requests.lock().unwrap().len(), 1);
    assert!(background.connected());

    background.api.set_listed(vec![blocked(2)]);
    events[0]
        .send(Ok(event(
            2,
            SessionEvent::Lifecycle {
                lifecycle: Lifecycle::Blocked,
            },
        )))
        .unwrap();
    eventually(|| background.status(2).view == StatusView::Stale);

    let client = background.client.clone();
    let revision = client.consent().unwrap().revision;
    client.disable(revision).unwrap();
    assert_eq!(background.status(2).view, StatusView::Disabled);
    let offer = client.consent_offer().unwrap();
    client.enable(offer, folders(true)).unwrap();
    client.sign_out().unwrap();
    assert_eq!(background.status(2).view, StatusView::SignedOut);

    background.runner.stop();
    assert_eq!(client.state().unwrap_err(), RefreshError::Stopped);
    assert_eq!(client.consent().unwrap_err(), RefreshError::Stopped);
}

#[test]
fn the_runner_resubscribes_and_lists_again_after_the_stream_ends() {
    let (background, mut events) = Background::start(&[tracked_view(2)], 1);
    eventually(|| background.count("list_all") == 1 && background.connected());

    events.clear();
    eventually(|| !background.connected());
    // No stream is available: the runner keeps trying, with backoff.
    eventually(|| background.count("subscribe") >= 3);
    assert_eq!(background.count("list_all"), 1);
    let _second = background.api.stream();
    eventually(|| background.count("list_all") == 2 && background.connected());

    let client = background.client.clone();
    drop(background);
    assert_eq!(client.state().unwrap_err(), RefreshError::Stopped);
}

#[test]
fn the_runner_requires_a_user_client() {
    let harness = Harness::enabled(ReplyMode::Success);
    let api: Arc<dyn SessionApi> = Arc::new(FakeSessionApi::session_caller(tracked_view(2)));
    assert_eq!(
        RefreshRunner::spawn(harness.refresher, api).unwrap_err(),
        RefreshError::NotPermitted
    );
}
