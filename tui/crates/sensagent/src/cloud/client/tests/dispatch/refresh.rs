//! Scheduled refreshing over the dispatch coordinator: the synchronous refresher with
//! fake time, and the threaded runner against a fake daemon. Fake SessionApi and HTTP only.

use std::sync::mpsc::Sender;
use std::sync::Arc;

use session_core::api::TurnContent;
use session_core::api::{EventEnvelope, SessionEvent};
use session_core::client::SessionApi;
use session_core::model::{AgentTurn, Reported, SessionView};

use super::source::{event, tracked_view as plain_view};
use super::*;
use crate::cloud::{FeatureChange, RemoteErrorCode};
use crate::refresh::{
    AIStatus, RefreshClient, RefreshError, RefreshFailure, RefreshRunner, RefreshState,
    StatusRefresher, BUDGET_STARTS,
};
use crate::source::fake::FakeSessionApi;

type Refresher = StatusRefresher<MemoryStatusStore, FileCredentialStore, FakeClock>;

struct Harness {
    _directory: tempfile::TempDir,
    account: Arc<AccountService<FileCredentialStore, FakeClock>>,
    store: MemoryStatusStore,
    requests: Requests,
    summaries: Arc<AtomicBool>,
    saves: Arc<AtomicBool>,
    title: Arc<Mutex<Option<String>>>,
    refuse_features: Arc<Mutex<Option<Option<&'static str>>>>,
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
            account,
            store,
            requests,
            summaries,
            saves,
            title,
            refuse_features,
            owner,
            ..
        } = fixture;
        Self {
            _directory: directory,
            account,
            store,
            requests,
            summaries,
            saves,
            title,
            refuse_features,
            refresher: StatusRefresher::new(owner),
        }
    }

    /// Remember titles and titled chats in `directory`, as the installed
    /// service does in its status directory.
    fn titling_in(self, directory: &std::path::Path) -> Self {
        let refresher = self
            .refresher
            .with_titles(directory.join("titles.json"))
            .with_titled_chats(directory.join("titled-chats.json"));
        Self { refresher, ..self }
    }

    /// Let the account's AI session summaries switch decide sharing.
    fn following(self) -> Self {
        Self {
            refresher: self.refresher.following_account_switch(),
            ..self
        }
    }

    /// Read the account's switches inline when a read is due; whether one ran.
    fn read_features(&mut self, now: u64) -> bool {
        let Some(job) = self.refresher.features_job(secs(now)) else {
            return false;
        };
        self.refresher.features_read(job.run(), secs(now));
        true
    }

    fn shared(&mut self) -> Option<Vec<FolderConsent>> {
        match self.refresher.consent().unwrap().policy {
            ConsentPolicy::Enabled { folders, .. } => Some(folders),
            ConsentPolicy::Disabled => None,
        }
    }

    /// Follow `views` as a fresh daemon list.
    fn resync(&mut self, api: &FakeSessionApi, views: Vec<SessionView>, now: u64) {
        api.set_listed(views.clone());
        self.refresher.resync(&views, secs(now)).unwrap();
    }

    /// The session title the fake service sends with its text from now on.
    fn send_title(&self, title: Option<&str>) {
        *self.title.lock().unwrap() = title.map(Into::into);
    }

    /// Tick at `now` and run every capture and request inline. Returns the captures.
    fn round(&mut self, api: &FakeSessionApi, now: u64) -> usize {
        let work = self.refresher.tick(secs(now));
        let started = work.len();
        for capture in work {
            if let Some(dispatch) = self.refresher.captured(capture.capture(api), secs(now)) {
                let completed = dispatch.run();
                if let Some(retitle) = self.refresher.completed(completed, secs(now)) {
                    self.refresher.retitled(retitle.apply(api));
                }
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

/// An agent chat at its fifth message: due for its session's one automatic
/// summary.
pub(super) fn tracked_view(number: u8) -> SessionView {
    let mut view = plain_view(number);
    view.agent_turn = Some(AgentTurn {
        id: 1,
        messages: 5,
        conversation_id: Some(format!("synthetic-conversation-{number}")),
    });
    view
}

/// The turn the fake daemon reads for every session.
fn turn(messages: u32) -> TurnContent {
    TurnContent {
        id: 1,
        prompt: Some("Add a regression test for quoted commas".into()),
        messages: (1..=messages).map(|n| format!("Step {n}")).collect(),
        total: messages,
    }
}

fn api(views: &[SessionView]) -> FakeSessionApi {
    let api = FakeSessionApi::user(views[0].clone(), OUTPUT);
    api.set_listed(views.to_vec());
    api.set_turn(turn(5));
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
            summaries: None,
            feed: None,
            features_unsaved: false,
        }
    );
}

#[test]
fn a_terminal_rakumux_com_signed_out_signs_out_and_revokes_its_sign_in() {
    let revoked = Arc::new(Mutex::new(Vec::new()));
    let mut harness = Harness::from(Fixture::new(ReplyMode::Success)).following();
    harness.refresher = {
        let revoked = revoked.clone();
        harness
            .refresher
            .revoking_with(move |pending| revoked.lock().unwrap().push(format!("{pending:?}")))
    };
    assert!(harness.read_features(0));
    assert!(harness.refresher.account().is_some());

    // A plain refusal (an expired token, say) changes nothing: renewing helps.
    *harness.refuse_features.lock().unwrap() = Some(None);
    assert!(harness.read_features(120));
    assert!(harness.refresher.account().is_some());
    assert!(revoked.lock().unwrap().is_empty());

    // "Sign out all terminals" on rakumux.com: signed out here, the sign-in
    // handed over to be revoked at Clerk, once.
    *harness.refuse_features.lock().unwrap() = Some(Some("signed_out"));
    assert!(harness.read_features(180));
    assert!(harness.refresher.account().is_none());
    assert!(harness.account.take_signed_out().is_none());
    let revoked = revoked.lock().unwrap().clone();
    assert_eq!(revoked.len(), 1);
    // Its secrets are never shown or logged.
    assert!(!revoked[0].contains("PRIVATE"), "{}", revoked[0]);
    let state = harness.refresher.state(secs(180));
    assert_eq!((state.summaries, state.feed), (None, None));
    // Signed out: nothing more is read.
    assert!(!harness.read_features(600));
}

#[test]
fn only_a_refresher_following_the_switch_reads_it() {
    let mut harness = Harness::from(Fixture::new(ReplyMode::Success));
    assert!(!harness.read_features(0));
    assert_eq!(harness.refresher.state(secs(0)).summaries, None);
}

#[test]
fn the_account_switch_shares_live_folders_and_stops_when_turned_off() {
    let mut private = tracked_view(3);
    private.scope = ScopeRoot::from_canonical("/synthetic/private-folder".into());
    private.private = true;
    let views = vec![tracked_view(2), private];
    let api = api(&views);
    let mut harness = Harness::from(Fixture::new(ReplyMode::Success)).following();
    harness.resync(&api, views, 0);
    // Nothing is shared before the account's switch is known.
    assert_eq!(harness.round(&api, 0), 0);
    assert_eq!(harness.shared(), None);

    assert!(harness.read_features(0));
    assert_eq!(harness.refresher.state(secs(0)).summaries, Some(true));
    assert_eq!(harness.round(&api, 0), 1);
    assert_eq!(harness.shared(), Some(folders(true)));
    assert!(ready(&harness.status(2)));
    assert_eq!(harness.calls(), 1);
    // Read again only every two minutes.
    assert!(!harness.read_features(119));

    harness.summaries.store(false, Ordering::SeqCst);
    assert!(harness.read_features(120));
    assert_eq!(harness.refresher.state(secs(120)).summaries, Some(false));
    assert_eq!(harness.round(&api, 120), 0);
    assert_eq!(harness.shared(), None);
    assert_eq!(harness.status(2).view, StatusView::Disabled);

    harness.summaries.store(true, Ordering::SeqCst);
    assert!(harness.read_features(240));
    harness.round(&api, 240);
    assert_eq!(harness.shared(), Some(folders(true)));
}

#[test]
fn turning_a_switch_off_here_stops_sharing_at_once_and_saves_it() {
    let api = api(&[tracked_view(2)]);
    let mut harness = Harness::from(Fixture::new(ReplyMode::Success)).following();
    harness.resync(&api, vec![tracked_view(2)], 0);
    assert!(harness.read_features(0));
    harness.round(&api, 0);
    assert_eq!(harness.shared(), Some(folders(true)));

    let off = FeatureChange {
        ai_session_summaries: Some(false),
        feed: None,
    };
    harness.refresher.change_features(off, secs(10)).unwrap();
    // Shown changed before it's saved, and sharing follows.
    let state = harness.refresher.state(secs(10));
    assert_eq!((state.summaries, state.feed), (Some(false), Some(false)));
    harness.round(&api, 10);
    assert_eq!(harness.shared(), None);
    assert!(harness.summaries.load(Ordering::SeqCst));
    // Saved at once rather than at the next two-minute read.
    assert!(harness.read_features(10));
    assert!(!harness.summaries.load(Ordering::SeqCst));
    assert_eq!(harness.refresher.state(secs(10)).summaries, Some(false));
    assert!(!harness.read_features(129));
    assert!(harness.read_features(130));
    assert_eq!(harness.refresher.state(secs(130)).summaries, Some(false));
}

#[test]
fn a_switch_that_fails_to_save_goes_back_to_the_accounts() {
    let api = api(&[tracked_view(2)]);
    let mut harness = Harness::from(Fixture::new(ReplyMode::Success)).following();
    harness.resync(&api, vec![tracked_view(2)], 0);
    assert!(harness.read_features(0));
    harness.saves.store(false, Ordering::SeqCst);
    let feed = FeatureChange {
        ai_session_summaries: None,
        feed: Some(true),
    };
    harness.refresher.change_features(feed, secs(5)).unwrap();
    assert_eq!(harness.refresher.state(secs(5)).feed, Some(true));
    assert!(harness.read_features(5));
    let state = harness.refresher.state(secs(5));
    assert_eq!(state.feed, Some(false));
    assert!(state.features_unsaved);
    // Asking again clears the failure; a save that works keeps it.
    harness.saves.store(true, Ordering::SeqCst);
    harness.refresher.change_features(feed, secs(6)).unwrap();
    assert!(!harness.refresher.state(secs(6)).features_unsaved);
    assert!(harness.read_features(6));
    assert_eq!(harness.refresher.state(secs(6)).feed, Some(true));
}

#[test]
fn switches_change_only_while_signed_in() {
    let mut harness = Harness::from(Fixture::new(ReplyMode::Success)).following();
    harness.refresher.sign_out().unwrap();
    let change = FeatureChange {
        ai_session_summaries: Some(false),
        feed: None,
    };
    assert_eq!(
        harness.refresher.change_features(change, secs(0)),
        Err(DispatchError::Policy(StatusError::SignedOut))
    );
}

#[test]
fn a_switch_read_for_an_earlier_sign_in_is_not_applied() {
    let api = api(&[tracked_view(2)]);
    let mut harness = Harness::from(Fixture::new(ReplyMode::Success)).following();
    harness.resync(&api, vec![tracked_view(2)], 0);
    let job = harness.refresher.features_job(secs(0)).unwrap();
    let other = login(&harness.account, "synthetic-other-subject");
    harness.refresher.set_account_session(Some(other)).unwrap();
    harness.refresher.features_read(job.run(), secs(0));
    assert_eq!(harness.refresher.state(secs(0)).summaries, None);
    assert_eq!(harness.round(&api, 0), 0);
    assert_eq!(harness.shared(), None);
    // The new account's switch is read at once.
    assert!(harness.read_features(0));
    harness.round(&api, 0);
    assert_eq!(harness.shared(), Some(folders(true)));
}

#[test]
fn output_and_time_never_refresh_and_asking_does() {
    let api = api(&[tracked_view(2)]);
    let mut harness = Harness::enabled(ReplyMode::Success);
    harness.resync(&api, vec![tracked_view(2)], 0);
    assert_eq!(harness.round(&api, 0), 1);
    for now in [1, 61, 600, 3600] {
        harness.event(event(2, SessionEvent::Activity), now);
        assert_eq!(harness.round(&api, now), 0, "{now}");
        assert!(ready(&harness.status(2)), "{now}");
    }
    assert_eq!(harness.calls(), 1);
    harness.refresher.request(&session(2));
    assert!(harness.status(2).refreshing);
    assert_eq!(harness.round(&api, 3601), 1);
    let status = harness.status(2);
    assert!(ready(&status) && !status.refreshing);
    assert_eq!(harness.calls(), 2);
}

#[test]
fn lifecycle_changes_keep_the_text_until_a_summary_is_asked_for() {
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
    assert!(ready(&harness.status(2)));
    assert_eq!(harness.round(&api, 600), 0);
    harness.refresher.request(&session(2));
    assert_eq!(harness.round(&api, 600), 1);
    assert!(ready(&harness.status(2)));
    assert_eq!(sent_lifecycles(&harness), ["working", "blocked"]);
}

#[test]
fn an_agent_turn_is_summarised_from_its_prompt_and_messages_not_the_screen() {
    let api = api(&[tracked_view(2)]);
    let mut harness = Harness::enabled(ReplyMode::Success);
    harness.resync(&api, vec![tracked_view(2)], 0);
    assert_eq!(harness.round(&api, 0), 1);
    assert!(ready(&harness.status(2)));
    assert!(
        api.reads().is_empty(),
        "the screen isn't read for an agent's turn"
    );
    let request = harness.requests.lock().unwrap()[0].body.clone();
    let value: Value = serde_json::from_slice(&request).unwrap();
    let output = value["context"]["recent_output"].as_str().unwrap();
    assert!(
        output.contains("Add a regression test for quoted commas"),
        "{output}"
    );
    assert!(output.contains("Step 5"), "{output}");
}

#[test]
fn a_chat_finished_before_its_fifth_output_is_summarised_then() {
    let mut early = tracked_view(2);
    early.agent_turn = Some(AgentTurn {
        id: 1,
        messages: 2,
        conversation_id: Some("synthetic-short-chat".into()),
    });
    let api = api(&[early.clone()]);
    api.set_turn(turn(2));
    let mut harness = Harness::enabled(ReplyMode::Success);
    harness.resync(&api, vec![early.clone()], 0);
    assert_eq!(harness.round(&api, 0), 0);
    let mut done = early;
    done.lifecycle = Lifecycle::Done;
    api.set_listed(vec![done.clone()]);
    harness.event(event(2, SessionEvent::Updated { session: done }), 1);
    assert_eq!(harness.round(&api, 1), 1);
    assert!(ready(&harness.status(2)));
    assert_eq!(sent_lifecycles(&harness), ["done"]);
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
fn rejected_input_is_not_tried_again_until_asked() {
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
    harness.event(event(2, SessionEvent::Activity), 600);
    assert_eq!(harness.round(&api, 605), 0);

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
    assert_eq!(harness.round(&api, 700), 0);
    harness.refresher.request(&session(2));
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

/// A program whose turns the daemon doesn't follow: its lifecycle is what its
/// summary is about.
fn unfollowed(number: u8, lifecycle: Lifecycle) -> SessionView {
    let mut view = plain_view(number);
    view.lifecycle = lifecycle;
    view
}

#[test]
fn a_session_changed_during_capture_is_corrected_from_the_daemon() {
    let api = api(&[unfollowed(2, Lifecycle::Blocked)]);
    let mut harness = Harness::enabled(ReplyMode::Success);
    harness
        .refresher
        .resync(&[unfollowed(2, Lifecycle::Working)], secs(0))
        .unwrap();
    let working = harness.refresher.source_stamp(&session(2)).unwrap();
    harness.refresher.request(&session(2));
    assert_eq!(harness.round(&api, 0), 1);
    assert_eq!(harness.calls(), 0);
    assert!(api.reads().is_empty());
    // The fresh view was applied: a new revision, and no failure to show. It's
    // still asked for: tried again shortly, with the corrected view.
    assert!(harness.refresher.source_stamp(&session(2)).unwrap() > working);
    assert_eq!(
        harness.status(2),
        AIStatus {
            view: StatusView::Missing,
            failure: None,
            refreshing: true,
        }
    );
    assert_eq!(harness.round(&api, 4), 0);
    assert_eq!(harness.round(&api, 5), 1);
    assert!(ready(&harness.status(2)));
    assert_eq!(sent_lifecycles(&harness), ["blocked"]);
}

#[test]
fn a_turn_changed_during_capture_is_read_again_at_its_prompt() {
    let mut next = tracked_view(2);
    next.agent_turn = Some(AgentTurn {
        id: 2,
        messages: 5,
        conversation_id: Some("synthetic-conversation-2".into()),
    });
    let api = api(&[next]);
    api.set_turn(prompted(
        2,
        "Write the release notes",
        &["Reading the log"; 5],
    ));
    let mut harness = Harness::enabled(ReplyMode::Success);
    harness
        .refresher
        .resync(&[tracked_view(2)], secs(0))
        .unwrap();
    // Planned for the first prompt; the daemon is on the next one.
    assert_eq!(harness.round(&api, 0), 1);
    assert_eq!(harness.calls(), 0);
    assert_eq!(harness.round(&api, 5), 1);
    assert!(ready(&harness.status(2)));
    assert!(
        sent_outputs(&harness)[0].starts_with("The user's last prompt:\nWrite the release notes\n")
    );
}

#[test]
fn a_followed_turn_moving_on_during_capture_is_still_summarised() {
    // The capture finds the agent waiting for the user and more messages
    // than planned for: the same prompt, so the same summary and title.
    let mut waiting = blocked(2);
    waiting.agent_turn = Some(AgentTurn {
        id: 1,
        messages: 7,
        conversation_id: Some("synthetic-conversation-2".into()),
    });
    let api = api(&[waiting]);
    let mut harness = Harness::enabled(ReplyMode::Success);
    harness
        .refresher
        .resync(&[tracked_view(2)], secs(0))
        .unwrap();
    assert_eq!(harness.round(&api, 0), 1);
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
    let api = api(&[unfollowed(2, Lifecycle::Blocked)]);
    let mut harness = Harness::enabled(ReplyMode::Success);
    harness
        .refresher
        .resync(&[unfollowed(2, Lifecycle::Working)], secs(0))
        .unwrap();
    harness.refresher.request(&session(2));
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
    api.set_listed(vec![unfollowed(2, Lifecycle::Done)]);
    assert_eq!(harness.round(&api, 60), 1);
    assert_eq!(sent_lifecycles(&harness), ["done"]);
}

#[test]
fn reports_only_folders_send_reports_and_never_output() {
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
    assert_eq!(harness.round(&api, 300), 0);
    harness.refresher.request(&session(2));
    assert_eq!(harness.round(&api, 300), 1);
    assert_eq!(harness.calls(), 2);
    assert!(
        api.reads().is_empty(),
        "reports only: the screen is never read"
    );
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

/// An agent session whose name Rakumux generated.
fn generated(number: u8) -> SessionView {
    let mut view = tracked_view(number);
    view.name = format!("brisk-otter-{number}");
    view.auto_named = true;
    view
}

/// The session `api` holds now, in its agent's `chat`th chat (at its fifth
/// message), listed and followed by `harness` from `now`.
fn next_chat(harness: &mut Harness, api: &FakeSessionApi, number: u8, chat: u64, now: u64) {
    let mut view = api.get(&session(number)).unwrap();
    view.agent_turn = Some(AgentTurn {
        id: chat,
        messages: 5,
        conversation_id: Some(format!("synthetic-chat-{chat}")),
    });
    api.set_listed(vec![view.clone()]);
    api.set_turn(TurnContent {
        id: chat,
        ..turn(5)
    });
    harness.event(event(number, SessionEvent::Updated { session: view }), now);
}

fn name(api: &FakeSessionApi, number: u8) -> String {
    api.get(&session(number)).unwrap().name
}

fn renamed(api: &FakeSessionApi) -> bool {
    api.calls().iter().any(|call| call.starts_with("rename "))
}

#[test]
fn a_session_is_titled_once_and_its_next_chats_are_not() {
    let api = api(&[generated(2)]);
    let mut harness = Harness::enabled(ReplyMode::Success);
    harness.send_title(Some("Fix the CSV parser"));
    harness.resync(&api, vec![generated(2)], 0);
    assert_eq!(harness.round(&api, 0), 1);
    assert!(ready(&harness.status(2)));
    assert_eq!(name(&api, 2), "Fix the CSV parser");
    assert_eq!(
        harness.refresher.title_of(&session(2)).as_deref(),
        Some("Fix the CSV parser")
    );

    // The name is still the service's own, but the session had its title:
    // its next chats get none, however long they run.
    harness.send_title(Some("Write the release notes"));
    for (chat, now) in [(2, 1), (3, 601)] {
        next_chat(&mut harness, &api, 2, chat, now);
        for now in [now, now + 599] {
            assert_eq!(harness.round(&api, now), 0, "chat {chat} at {now}");
        }
    }
    assert_eq!(name(&api, 2), "Fix the CSV parser");
    assert_eq!(harness.calls(), 1);
    // A title asked for still applies.
    harness.refresher.request(&session(2));
    assert_eq!(harness.round(&api, 2000), 1);
    assert_eq!(name(&api, 2), "Write the release notes");
}

#[test]
fn a_name_the_user_gave_stays_until_a_title_is_asked_for() {
    let api = api(&[generated(2)]);
    let mut harness = Harness::enabled(ReplyMode::Success);
    harness.send_title(Some("Fix the CSV parser"));
    harness.resync(&api, vec![generated(2)], 0);
    harness.round(&api, 0);
    // The user renames it (`r`, `rakumux rename`, the API).
    api.rename(&session(2), "csv work").unwrap();

    harness.send_title(Some("Write the release notes"));
    next_chat(&mut harness, &api, 2, 2, 1);
    assert_eq!(harness.round(&api, 1), 0);
    assert_eq!(name(&api, 2), "csv work");

    // Asking (`s`, the ↻) always applies, and the name is the service's again.
    harness.send_title(Some("Release notes"));
    harness.refresher.request(&session(2));
    assert_eq!(harness.round(&api, 2), 1);
    assert_eq!(name(&api, 2), "Release notes");
    // Still no automatic title after it.
    harness.send_title(Some("Ship the release"));
    next_chat(&mut harness, &api, 2, 3, 3);
    assert_eq!(harness.round(&api, 3), 0);
    assert_eq!(name(&api, 2), "Release notes");
}

/// The session `api` holds now, its agent on turn `turn` of the same chat
/// with `messages` written and `lifecycle`, listed and followed by `harness`
/// from `now`.
fn on_turn(
    harness: &mut Harness,
    api: &FakeSessionApi,
    number: u8,
    (turn, messages, lifecycle): (u64, u32, Lifecycle),
    now: u64,
) {
    let mut view = api.get(&session(number)).unwrap();
    view.lifecycle = lifecycle;
    view.agent_turn = Some(AgentTurn {
        id: turn,
        messages,
        conversation_id: Some(format!("synthetic-conversation-{number}")),
    });
    api.set_listed(vec![view.clone()]);
    harness.event(event(number, SessionEvent::Updated { session: view }), now);
}

/// The daemon's turn `id`: the user's `prompt` and the agent's `messages`.
fn prompted(id: u64, prompt: &str, messages: &[&str]) -> TurnContent {
    TurnContent {
        id,
        prompt: Some(prompt.into()),
        messages: messages.iter().map(|message| (*message).into()).collect(),
        total: messages.len() as u32,
    }
}

fn sent_outputs(harness: &Harness) -> Vec<String> {
    harness
        .requests
        .lock()
        .unwrap()
        .iter()
        .map(|request| {
            let value: Value = serde_json::from_slice(&request.body).unwrap();
            value["context"]["recent_output"]
                .as_str()
                .unwrap_or_default()
                .to_owned()
        })
        .collect()
}

#[test]
fn a_title_asked_for_a_prompt_without_an_answer_is_the_prompts_once_the_agent_starts() {
    let api = api(&[generated(2)]);
    let mut harness = Harness::enabled(ReplyMode::Success);
    harness.send_title(Some("Fix the CSV parser"));
    harness.resync(&api, vec![generated(2)], 0);
    assert_eq!(harness.round(&api, 0), 1);

    // A new prompt in the same chat, not answered yet (the daemon still sees
    // the agent idle), and the user asks for a title.
    api.set_turn(prompted(2, "Write the release notes", &[]));
    on_turn(&mut harness, &api, 2, (2, 0, Lifecycle::Idle), 1);
    harness.send_title(Some("Write the release notes"));
    harness.refresher.request(&session(2));
    let work = harness.refresher.tick(secs(1));
    assert_eq!(work.len(), 1);
    let captured = work.into_iter().next().unwrap().capture(&api);
    let dispatch = harness.refresher.captured(captured, secs(1)).unwrap();
    // The agent starts on it while the title is being made.
    on_turn(&mut harness, &api, 2, (2, 6, Lifecycle::Working), 2);
    let retitle = harness
        .refresher
        .completed(dispatch.run(), secs(2))
        .expect("the prompt's title");
    harness.refresher.retitled(retitle.apply(&api));

    assert_eq!(name(&api, 2), "Write the release notes");
    assert_eq!(
        sent_outputs(&harness)[1],
        "The user's last prompt:\nWrite the release notes\n\n\
         The agent hasn't written anything since."
    );
    assert!(!harness.status(2).refreshing);
    assert_eq!(harness.round(&api, 60), 0);
}

#[test]
fn a_title_asked_for_follows_a_prompt_typed_while_it_was_made() {
    let api = api(&[generated(2)]);
    let mut harness = Harness::enabled(ReplyMode::Success);
    harness.send_title(Some("Fix the CSV parser"));
    harness.resync(&api, vec![generated(2)], 0);
    assert_eq!(harness.round(&api, 0), 1);

    harness.send_title(Some("Quoted comma tests"));
    harness.refresher.request(&session(2));
    let work = harness.refresher.tick(secs(3));
    let captured = work.into_iter().next().unwrap().capture(&api);
    let dispatch = harness.refresher.captured(captured, secs(3)).unwrap();
    // The user types the next prompt meanwhile: the title made from the turn
    // before it doesn't name the session, and it's asked for again.
    api.set_turn(prompted(2, "Write the release notes", &[]));
    on_turn(&mut harness, &api, 2, (2, 0, Lifecycle::Working), 3);
    assert!(harness
        .refresher
        .completed(dispatch.run(), secs(3))
        .is_none());
    assert_eq!(name(&api, 2), "Fix the CSV parser");
    assert!(harness.status(2).refreshing);

    harness.send_title(Some("Write the release notes"));
    assert_eq!(harness.round(&api, 8), 1);
    assert_eq!(name(&api, 2), "Write the release notes");
    let sent = sent_outputs(&harness);
    assert_eq!(
        sent.last().map(String::as_str),
        Some(
            "The user's last prompt:\nWrite the release notes\n\n\
             The agent hasn't written anything since."
        )
    );
    assert!(!harness.status(2).refreshing);
}

#[test]
fn a_restarted_service_doesnt_title_a_session_with_its_automatic_title_again() {
    let kept = tempfile::tempdir().unwrap();
    let path = kept.path().join("titled-chats.json");
    let api = api(&[generated(2)]);
    let mut harness = Harness::enabled(ReplyMode::Success).titling_in(kept.path());
    harness.send_title(Some("Fix the CSV parser"));
    harness.resync(&api, vec![generated(2)], 0);
    assert_eq!(harness.round(&api, 0), 1);
    assert_eq!(name(&api, 2), "Fix the CSV parser");
    let mode = std::fs::metadata(&path).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o600);

    // The service restarts, and the daemon restarted in place: its follower
    // numbers the chat's turns afresh; the chat is the same.
    let mut restarted = Harness::enabled(ReplyMode::Success).titling_in(kept.path());
    restarted.send_title(Some("Parse quoted commas"));
    let mut view = api.get(&session(2)).unwrap();
    view.lifecycle = Lifecycle::Done;
    view.agent_turn = Some(AgentTurn {
        id: 1,
        messages: 9,
        conversation_id: Some("synthetic-conversation-2".into()),
    });
    restarted.resync(&api, vec![view], 0);
    for now in [0, 60, 600] {
        assert_eq!(restarted.round(&api, now), 0, "{now}");
    }
    assert_eq!(name(&api, 2), "Fix the CSV parser");
    // Nor its next chat; a title asked for always applies.
    next_chat(&mut restarted, &api, 2, 3, 601);
    assert_eq!(restarted.round(&api, 601), 0);
    assert_eq!(name(&api, 2), "Fix the CSV parser");
    restarted.send_title(Some("Quoted comma tests"));
    restarted.refresher.request(&session(2));
    assert_eq!(restarted.round(&api, 602), 1);
    assert_eq!(name(&api, 2), "Quoted comma tests");
}

#[test]
fn a_title_asked_for_first_is_the_sessions_one_after_a_restart_too() {
    let kept = tempfile::tempdir().unwrap();
    // A shell: no agent's turn to follow, so its screen, only when asked.
    let mut shell = plain_view(2);
    shell.name = "brisk-otter-2".into();
    shell.auto_named = true;
    let api = api(&[shell.clone()]);
    let mut harness = Harness::enabled(ReplyMode::Success).titling_in(kept.path());
    harness.send_title(Some("Tail the server logs"));
    harness.resync(&api, vec![shell], 0);
    assert_eq!(harness.round(&api, 0), 0);
    harness.refresher.request(&session(2));
    assert_eq!(harness.round(&api, 1), 1);
    assert_eq!(name(&api, 2), "Tail the server logs");

    // The service restarts, then an agent started there reaches its fifth
    // message: the session had its title, so it gets no automatic one.
    let mut restarted = Harness::enabled(ReplyMode::Success).titling_in(kept.path());
    restarted.send_title(Some("Fix the CSV parser"));
    restarted.resync(&api, vec![api.get(&session(2)).unwrap()], 2);
    next_chat(&mut restarted, &api, 2, 1, 3);
    for now in [3, 60, 600] {
        assert_eq!(restarted.round(&api, now), 0, "{now}");
    }
    assert_eq!(name(&api, 2), "Tail the server logs");
}

#[test]
fn an_older_releases_titled_chats_count_for_their_session() {
    let kept = tempfile::tempdir().unwrap();
    let path = kept.path().join("titled-chats.json");
    // As a release that titled every chat wrote it: the chats it titled.
    let older = json!({
        "version": 1,
        "sessions": {
            session(2).as_str(): {"chats": ["synthetic-chat-1", "synthetic-chat-2"], "used": 2}
        }
    });
    fs::write(&path, serde_json::to_vec(&older).unwrap()).unwrap();
    // A chat it doesn't list, at its fifth message.
    let api = api(&[generated(2)]);
    let mut harness = Harness::enabled(ReplyMode::Success).titling_in(kept.path());
    harness.send_title(Some("Write the release notes"));
    harness.resync(&api, vec![generated(2)], 0);
    for now in [0, 60, 600] {
        assert_eq!(harness.round(&api, now), 0, "{now}");
    }
    assert!(!renamed(&api));

    // A title asked for adds its chat, in the format that release reads.
    harness.refresher.request(&session(2));
    assert_eq!(harness.round(&api, 601), 1);
    assert_eq!(name(&api, 2), "Write the release notes");
    let file: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    assert_eq!(file["version"], 1);
    assert_eq!(
        file["sessions"][session(2).as_str()]["chats"],
        json!([
            "synthetic-chat-1",
            "synthetic-chat-2",
            "synthetic-conversation-2"
        ])
    );
}

#[test]
fn titled_chats_of_sessions_gone_or_of_sharing_turned_off_are_forgotten() {
    let kept = tempfile::tempdir().unwrap();
    let titled_again = |api: &FakeSessionApi| {
        let mut harness = Harness::enabled(ReplyMode::Success).titling_in(kept.path());
        harness.resync(api, vec![api.get(&session(2)).unwrap()], 0);
        harness.round(api, 0) == 1
    };
    let api = api(&[generated(2)]);
    let mut harness = Harness::enabled(ReplyMode::Success).titling_in(kept.path());
    harness.resync(&api, vec![generated(2)], 0);
    assert_eq!(harness.round(&api, 0), 1);
    assert!(!titled_again(&api));

    // The daemon doesn't have the session any more (deleted while the
    // service wasn't running): forgotten at the next list.
    harness.refresher.keep_only([&session(3)]);
    assert!(titled_again(&api));
    // Deleted while it ran.
    let mut harness = Harness::enabled(ReplyMode::Success).titling_in(kept.path());
    harness.resync(&api, vec![generated(2)], 0);
    assert!(!titled_again(&api));
    harness.event(event(2, SessionEvent::Removed), 1);
    assert!(titled_again(&api));
    // Sharing turned off: every session is titled again once it's back on.
    let mut harness = Harness::enabled(ReplyMode::Success).titling_in(kept.path());
    harness.resync(&api, vec![generated(2)], 0);
    let revision = harness.refresher.consent().unwrap().revision;
    harness.refresher.disable(revision).unwrap();
    assert!(titled_again(&api));
}

#[test]
fn a_title_another_session_of_the_folder_has_gets_a_number() {
    let mut other = tracked_view(3);
    other.name = "Fix the CSV parser".into();
    let views = vec![generated(2), other];
    let api = api(&views);
    let mut harness = Harness::enabled(ReplyMode::Success);
    harness.send_title(Some("Fix the CSV parser"));
    harness.resync(&api, views, 0);
    harness.round(&api, 0);
    assert_eq!(name(&api, 2), "Fix the CSV parser 2");
    assert_eq!(name(&api, 3), "Fix the CSV parser");
    assert_eq!(
        harness.refresher.title_of(&session(2)).as_deref(),
        Some("Fix the CSV parser 2")
    );
}

#[test]
fn an_older_service_without_titles_names_nothing() {
    let api = api(&[generated(2)]);
    let mut harness = Harness::enabled(ReplyMode::Success);
    harness.resync(&api, vec![generated(2)], 0);
    assert_eq!(harness.round(&api, 0), 1);
    assert!(ready(&harness.status(2)));
    assert_eq!(name(&api, 2), "brisk-otter-2");
    assert!(!renamed(&api));
    assert_eq!(harness.refresher.title_of(&session(2)), None);
}

#[test]
fn a_revoked_request_names_nothing_and_a_removed_session_forgets_its_title() {
    let api = api(&[generated(2)]);
    let mut harness = Harness::enabled(ReplyMode::Success);
    harness.send_title(Some("Fix the CSV parser"));
    harness.resync(&api, vec![generated(2)], 0);
    let work = harness.refresher.tick(secs(0));
    let captured = work.into_iter().next().unwrap().capture(&api);
    let dispatch = harness.refresher.captured(captured, secs(0)).unwrap();
    let revision = harness.refresher.consent().unwrap().revision;
    harness.refresher.disable(revision).unwrap();
    assert!(harness
        .refresher
        .completed(dispatch.run(), secs(0))
        .is_none());
    assert!(!renamed(&api));

    let offer = harness.refresher.consent_offer().unwrap();
    harness.refresher.enable(&offer, folders(true)).unwrap();
    harness.round(&api, 10);
    assert_eq!(name(&api, 2), "Fix the CSV parser");
    harness.event(event(2, SessionEvent::Removed), 11);
    assert_eq!(harness.refresher.title_of(&session(2)), None);
}

/// A refresher running on its own threads against a fake daemon.
pub(super) struct Background {
    _directory: tempfile::TempDir,
    pub(super) account: Arc<AccountService<FileCredentialStore, FakeClock>>,
    pub(super) requests: Requests,
    pub(super) runner: RefreshRunner,
    pub(super) client: RefreshClient,
    pub(super) api: Arc<FakeSessionApi>,
    /// The session title the fake service sends; none by default.
    pub(super) title: Arc<Mutex<Option<String>>>,
}

impl Background {
    /// Queue `streams` event streams, then start an enabled refresher.
    pub(super) fn start(views: &[SessionView], streams: usize) -> (Self, Vec<Sender<Event>>) {
        let api = Arc::new(api(views));
        let events = (0..streams).map(|_| api.stream()).collect();
        let Harness {
            _directory,
            account,
            requests,
            refresher,
            title,
            ..
        } = Harness::enabled(ReplyMode::Success);
        let session_api: Arc<dyn SessionApi> = api.clone();
        let runner = RefreshRunner::spawn(refresher, session_api).unwrap();
        let client = runner.client();
        let background = Self {
            _directory,
            account,
            requests,
            runner,
            client,
            api,
            title,
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

    // A lifecycle change keeps the text; asking makes a new summary.
    background.api.set_listed(vec![blocked(2)]);
    events[0]
        .send(Ok(event(
            2,
            SessionEvent::Lifecycle {
                lifecycle: Lifecycle::Blocked,
            },
        )))
        .unwrap();
    assert!(ready(&background.status(2)));
    background.client.summarize(session(2)).unwrap();
    eventually(|| background.requests.lock().unwrap().len() == 2);
    eventually(|| ready(&background.status(2)) && !background.status(2).refreshing);

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
fn the_runner_names_a_session_after_the_title_asked_for() {
    let mut named = tracked_view(2);
    named.name = "csv work".into();
    let (background, _events) = Background::start(&[named], 1);
    eventually(|| ready(&background.status(2)));
    assert_eq!(background.api.get(&session(2)).unwrap().name, "csv work");

    *background.title.lock().unwrap() = Some("Fix the CSV parser".into());
    background.client.summarize(session(2)).unwrap();
    eventually(|| background.api.get(&session(2)).unwrap().name == "Fix the CSV parser");
    eventually(|| !background.status(2).refreshing);
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
