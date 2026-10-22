use session_core::activity::AgentActivity;
use session_core::model::{GitInfo, ReportResult};

use super::*;
use crate::source::fake::live_view;
use crate::source::SessionCapture;

const FOLDER: &str = "/synthetic/tracker-private-folder";
const MOVED: &str = "/synthetic/tracker-relocated-folder";

fn id(number: u32) -> SessionId {
    SessionId::from_string(format!("synthetic-tracked-session-{number}"))
}

fn view(number: u32) -> SessionView {
    live_view(&id(number), FOLDER, 4000 + number)
}

fn envelope(number: u32, event: SessionEvent) -> EventEnvelope {
    EventEnvelope {
        scope: ScopeRoot::from_canonical(FOLDER.into()),
        session_id: id(number),
        at: 1,
        event,
    }
}

fn stamp(terminal_generation: u64, source_revision: u64) -> SourceStamp {
    SourceStamp {
        terminal_generation,
        source_revision,
    }
}

fn report(text: &str) -> Reported {
    Reported {
        state: None,
        text: Some(text.into()),
        result: None,
        at: 10,
    }
}

fn observed(update: Result<SourceUpdate, SourceError>) -> StatusSource {
    match update.unwrap() {
        SourceUpdate::Observe(source) => source,
        other => panic!("expected an observation, got {other:?}"),
    }
}

fn unchanged(update: Result<SourceUpdate, SourceError>) {
    assert_eq!(update.unwrap(), SourceUpdate::Unchanged);
}

#[test]
fn a_first_live_view_gets_a_fresh_stamp_and_may_have_output() {
    let mut tracker = SourceTracker::new();
    let source = observed(tracker.apply_view(&view(1)));
    assert_eq!(
        source,
        StatusSource {
            session_id: id(1),
            folder: ScopeRoot::from_canonical(FOLDER.into()),
            stamp: stamp(1, 1),
            private: false,
            live: true,
            context: ContextAvailability {
                reported_text: false,
                recent_output: true,
            },
        }
    );
    assert_eq!(observed(tracker.apply_view(&view(2))).stamp, stamp(2, 1));
    unchanged(tracker.apply_view(&view(1)));
    assert!(!tracker.is_dirty(&id(1)));
}

#[test]
fn sessions_first_seen_without_a_terminal_are_not_tracked() {
    let mut tracker = SourceTracker::new();
    for status in [
        SessionStatus::Exited,
        SessionStatus::Dead,
        SessionStatus::Unknown,
    ] {
        let mut stopped = view(1);
        stopped.status = status;
        unchanged(tracker.apply_view(&stopped));
    }
    let blocked = SessionEvent::Lifecycle {
        lifecycle: Lifecycle::Blocked,
    };
    unchanged(tracker.apply_event(&envelope(1, blocked)));
    unchanged(tracker.apply_event(&envelope(1, SessionEvent::Activity)));
    assert!(!tracker.is_dirty(&id(1)));
    assert_eq!(
        format!("{tracker:?}"),
        "SourceTracker { sessions: 0, retired: 0 }"
    );
    assert_eq!(observed(tracker.apply_view(&view(1))).stamp, stamp(1, 1));
}

#[test]
fn presentation_changes_and_mismatched_events_do_not_advance_a_stamp() {
    let mut tracker = SourceTracker::new();
    observed(tracker.apply_view(&view(1)));
    let mut changed = view(1);
    changed.name = "synthetic-renamed".into();
    changed.cwd = "/synthetic/elsewhere".into();
    changed.last_active_at = 99;
    changed.pinned = true;
    changed.auto_named = true;
    changed.description = Some("synthetic description".into());
    changed
        .labels
        .insert("role".into(), "synthetic-review".into());
    changed.git = Some(GitInfo::default());
    let terminal = changed.terminal.as_mut().unwrap();
    terminal.title = "synthetic-new-title".into();
    terminal.foreground_process = Some("synthetic-other".into());
    terminal.cols = 120;
    terminal.rows = 40;
    terminal.controlled = true;
    terminal.alternate_screen = true;
    unchanged(tracker.apply_view(&changed));
    unchanged(tracker.apply_event(&envelope(1, SessionEvent::Updated { session: changed })));
    unchanged(tracker.apply_event(&envelope(1, SessionEvent::Unknown)));

    let mut blocked = view(1);
    blocked.lifecycle = Lifecycle::Blocked;
    let foreign = SessionEvent::Updated {
        session: blocked.clone(),
    };
    unchanged(tracker.apply_event(&envelope(2, foreign)));
    assert!(!tracker.is_dirty(&id(1)));
    let matching = SessionEvent::Updated { session: blocked };
    assert_eq!(
        observed(tracker.apply_event(&envelope(1, matching))).stamp,
        stamp(1, 2)
    );
}

#[test]
fn fencing_facts_advance_the_revision_immediately() {
    let mut tracker = SourceTracker::new();
    observed(tracker.apply_view(&view(1)));
    let blocked = || SessionEvent::Lifecycle {
        lifecycle: Lifecycle::Blocked,
    };
    assert_eq!(
        observed(tracker.apply_event(&envelope(1, blocked()))).stamp,
        stamp(1, 2)
    );
    unchanged(tracker.apply_event(&envelope(1, blocked())));

    let reported = SessionEvent::Reported {
        reported: report("Waiting for synthetic review"),
    };
    let source = observed(tracker.apply_event(&envelope(1, reported)));
    assert_eq!(source.stamp, stamp(1, 3));
    assert!(source.context.reported_text);

    let blank = SessionEvent::Reported {
        reported: report(" \n\t "),
    };
    let source = observed(tracker.apply_event(&envelope(1, blank)));
    assert_eq!(source.stamp, stamp(1, 4));
    assert!(!source.context.reported_text);

    let summary = Reported {
        state: Some(Lifecycle::Done),
        text: None,
        result: Some(ReportResult {
            summary: "Synthetic tests pass".into(),
            files: vec!["/synthetic/private/file.rs".into()],
        }),
        at: 11,
    };
    let summarized = SessionEvent::Reported {
        reported: summary.clone(),
    };
    let source = observed(tracker.apply_event(&envelope(1, summarized)));
    assert_eq!(source.stamp, stamp(1, 5));
    assert!(source.context.reported_text);

    let mut private = view(1);
    private.lifecycle = Lifecycle::Blocked;
    private.reported = Some(summary);
    private.private = true;
    let source = observed(tracker.apply_view(&private));
    assert_eq!(source.stamp, stamp(1, 6));
    assert!(source.private);

    let mut moved = envelope(
        1,
        SessionEvent::Lifecycle {
            lifecycle: Lifecycle::Blocked,
        },
    );
    moved.scope = ScopeRoot::from_canonical(MOVED.into());
    let source = observed(tracker.apply_event(&moved));
    assert_eq!(source.stamp, stamp(1, 7));
    assert_eq!(source.folder.path(), std::path::Path::new(MOVED));

    moved.event = SessionEvent::Exited { code: Some(0) };
    let source = observed(tracker.apply_event(&moved));
    assert_eq!(source.stamp, stamp(1, 8));
    assert!(!source.live);
    assert!(!source.context.recent_output);
    unchanged(tracker.apply_event(&moved));
}

#[test]
fn a_new_terminal_incarnation_starts_a_new_generation() {
    let mut tracker = SourceTracker::new();
    observed(tracker.apply_view(&view(1)));
    observed(tracker.apply_view(&view(2)));
    let exited = SessionEvent::Exited { code: None };
    let ended = observed(tracker.apply_event(&envelope(1, exited)));
    assert_eq!(ended.stamp, stamp(1, 2));
    assert!(!ended.live);

    let revived = live_view(&id(1), FOLDER, 9001);
    let source = observed(tracker.apply_view(&revived));
    assert_eq!(source.stamp, stamp(3, 1));
    assert!(source.stamp > ended.stamp);
    assert!(source.live && source.context.recent_output);

    let replaced = live_view(&id(1), FOLDER, 9002);
    assert_eq!(observed(tracker.apply_view(&replaced)).stamp, stamp(4, 1));

    // A pid the daemon briefly lacks is a fact change, not a new terminal.
    let mut unknown = replaced.clone();
    unknown.terminal.as_mut().unwrap().pid = None;
    assert_eq!(observed(tracker.apply_view(&unknown)).stamp, stamp(4, 2));
    assert_eq!(observed(tracker.apply_view(&replaced)).stamp, stamp(4, 3));
    assert_eq!(observed(tracker.apply_view(&unknown)).stamp, stamp(4, 4));
    let other = live_view(&id(1), FOLDER, 9003);
    assert_eq!(observed(tracker.apply_view(&other)).stamp, stamp(5, 1));
}

#[test]
fn output_and_activity_phases_wait_for_advance() {
    let mut tracker = SourceTracker::new();
    observed(tracker.apply_view(&view(1)));
    unchanged(tracker.advance(&id(1)));
    for _ in 0..5 {
        unchanged(tracker.apply_event(&envelope(1, SessionEvent::Activity)));
    }
    assert!(tracker.is_dirty(&id(1)));
    let source = observed(tracker.advance(&id(1)));
    assert_eq!(source.stamp, stamp(1, 2));
    assert!(source.context.recent_output);
    assert!(!tracker.is_dirty(&id(1)));
    unchanged(tracker.advance(&id(1)));

    let mut phased = view(1);
    phased.activity = Some(AgentActivity {
        phase: ActivityPhase::RunningTool,
        active_subagents: 0,
        updated_at: 5,
    });
    unchanged(tracker.apply_view(&phased));
    assert!(tracker.is_dirty(&id(1)));
    phased.activity.as_mut().unwrap().updated_at = 6;
    unchanged(tracker.apply_view(&phased));

    // An immediate change covers the pending output; nothing is left to advance.
    phased.lifecycle = Lifecycle::Done;
    assert_eq!(observed(tracker.apply_view(&phased)).stamp, stamp(1, 3));
    assert!(!tracker.is_dirty(&id(1)));
    unchanged(tracker.advance(&id(1)));
    unchanged(tracker.advance(&id(9)));
}

fn read(number: u32, stamp: SourceStamp, report: bool, output: bool) -> SessionCapture {
    SessionCapture::read(&id(number), stamp, report, output)
}

#[test]
fn no_context_hides_the_same_input_until_new_context_arrives() {
    let mut tracker = SourceTracker::new();
    let mut reported = view(1);
    reported.reported = Some(report("Synthetic report"));
    let source = observed(tracker.apply_view(&reported));
    assert_eq!(
        source.context,
        ContextAvailability {
            reported_text: true,
            recent_output: true,
        }
    );
    unchanged(tracker.no_context(&read(1, stamp(9, 9), true, true)));
    unchanged(tracker.no_context(&read(2, stamp(1, 1), true, true)));
    unchanged(tracker.no_context(&read(1, stamp(1, 1), false, false)));
    let source = observed(tracker.no_context(&read(1, stamp(1, 1), true, true)));
    assert_eq!(source.stamp, stamp(1, 2));
    assert_eq!(source.context, ContextAvailability::default());
    unchanged(tracker.no_context(&read(1, stamp(1, 2), true, true)));
    unchanged(tracker.advance(&id(1)));

    unchanged(tracker.apply_event(&envelope(1, SessionEvent::Activity)));
    let source = observed(tracker.advance(&id(1)));
    assert_eq!(source.stamp, stamp(1, 3));
    assert!(!source.context.reported_text && source.context.recent_output);

    observed(tracker.no_context(&read(1, stamp(1, 3), false, true)));
    let renewed = SessionEvent::Reported {
        reported: report("A new synthetic report"),
    };
    let source = observed(tracker.apply_event(&envelope(1, renewed)));
    assert_eq!(source.stamp, stamp(1, 5));
    assert!(source.context.reported_text && !source.context.recent_output);
}

#[test]
fn a_reports_only_capture_keeps_output_available_for_later_consent() {
    let mut tracker = SourceTracker::new();
    let mut reported = view(1);
    reported.reported = Some(report("[REDACTED-looking synthetic report]"));
    observed(tracker.apply_view(&reported));
    let source = observed(tracker.no_context(&read(1, stamp(1, 1), true, false)));
    assert_eq!(source.stamp, stamp(1, 2));
    assert_eq!(
        source.context,
        ContextAvailability {
            reported_text: false,
            recent_output: true,
        }
    );
    // Without a report, a capture that read nothing changes nothing.
    unchanged(tracker.no_context(&read(1, stamp(1, 2), false, false)));
}

#[test]
fn invalid_observations_are_never_tracked_and_fail_for_tracked_sessions() {
    let mut tracker = SourceTracker::new();
    let mut relative = view(1);
    relative.scope = ScopeRoot::from_canonical("relative/synthetic-folder".into());
    unchanged(tracker.apply_view(&relative));
    let mut controlled = live_view(&SessionId::from_string("synthetic\nsession"), FOLDER, 1);
    unchanged(tracker.apply_view(&controlled));
    controlled.status = SessionStatus::Dead;
    unchanged(tracker.apply_view(&controlled));

    let source = observed(tracker.apply_view(&view(1)));
    assert_eq!(
        tracker.apply_view(&relative).unwrap_err(),
        SourceError::Invalid
    );
    let mut moved = envelope(
        1,
        SessionEvent::Lifecycle {
            lifecycle: Lifecycle::Blocked,
        },
    );
    moved.scope = ScopeRoot::from_canonical("/synthetic/../escape".into());
    assert_eq!(
        tracker.apply_event(&moved).unwrap_err(),
        SourceError::Invalid
    );
    assert_eq!(tracker.observations().collect::<Vec<_>>(), [source]);
}

#[test]
fn removal_retires_tracked_sessions_and_ignores_unknown_ones() {
    let mut tracker = SourceTracker::new();
    observed(tracker.apply_view(&view(1)));
    assert_eq!(
        tracker
            .apply_event(&envelope(1, SessionEvent::Removed))
            .unwrap(),
        SourceUpdate::Remove(id(1))
    );
    unchanged(tracker.apply_event(&envelope(1, SessionEvent::Removed)));
    unchanged(tracker.apply_view(&view(1)));
    unchanged(tracker.apply_event(&envelope(1, SessionEvent::Activity)));
    unchanged(tracker.apply_event(&envelope(1, SessionEvent::Created { session: view(1) })));
    assert!(!tracker.is_dirty(&id(1)));

    // Deleting a dead or never-seen session takes no slot: the policy never saw it.
    unchanged(tracker.apply_event(&envelope(2, SessionEvent::Removed)));
    assert_eq!(
        format!("{tracker:?}"),
        "SourceTracker { sessions: 0, retired: 1 }"
    );
    assert_eq!(observed(tracker.apply_view(&view(2))).stamp, stamp(2, 1));
}

#[test]
fn tracking_is_bounded_like_the_status_policy() {
    let mut tracker = SourceTracker::new();
    for number in 0..MAX_CACHE_ENTRIES as u32 {
        observed(tracker.apply_view(&view(number)));
    }
    assert_eq!(
        tracker.apply_view(&view(20_000)).unwrap_err(),
        SourceError::Capacity
    );
    unchanged(tracker.apply_event(&envelope(20_001, SessionEvent::Removed)));
    let mut dead = view(20_002);
    dead.status = SessionStatus::Dead;
    unchanged(tracker.apply_view(&dead));
    assert_eq!(
        tracker
            .apply_event(&envelope(0, SessionEvent::Removed))
            .unwrap(),
        SourceUpdate::Remove(id(0))
    );
    // Retirement fences keep their slot; tracked sessions still update.
    assert_eq!(
        tracker.apply_view(&view(20_000)).unwrap_err(),
        SourceError::Capacity
    );
    let blocked = SessionEvent::Lifecycle {
        lifecycle: Lifecycle::Blocked,
    };
    observed(tracker.apply_event(&envelope(1, blocked)));
}

#[test]
fn counters_never_wrap_or_change_state_when_exhausted() {
    let mut tracker = SourceTracker::new();
    observed(tracker.apply_view(&view(1)));
    tracker
        .sessions
        .get_mut(&id(1))
        .unwrap()
        .stamp
        .source_revision = u64::MAX;
    unchanged(tracker.apply_event(&envelope(1, SessionEvent::Activity)));
    let blocked = SessionEvent::Lifecycle {
        lifecycle: Lifecycle::Blocked,
    };
    assert_eq!(
        tracker.apply_event(&envelope(1, blocked)).unwrap_err(),
        SourceError::Exhausted
    );
    assert_eq!(tracker.advance(&id(1)).unwrap_err(), SourceError::Exhausted);
    assert_eq!(
        tracker
            .no_context(&read(1, stamp(1, u64::MAX), true, true))
            .unwrap_err(),
        SourceError::Exhausted
    );
    let tracked = &tracker.sessions[&id(1)];
    assert!(tracked.dirty && tracked.output);
    assert_eq!(tracked.facts.lifecycle, Lifecycle::Working);

    tracker.generation = u64::MAX;
    assert_eq!(
        tracker.apply_view(&view(2)).unwrap_err(),
        SourceError::Exhausted
    );
    assert!(!tracker.sessions.contains_key(&id(2)));
}

fn resynced(
    tracker: &mut SourceTracker,
    views: &[SessionView],
) -> Vec<(SessionId, SourceStamp, bool)> {
    let (updates, failure) = tracker.resync(views);
    assert_eq!(failure, None);
    updates
        .into_iter()
        .map(|update| observed(Ok(update)))
        .map(|source| (source.session_id, source.stamp, source.live))
        .collect()
}

#[test]
fn resync_marks_missed_output_and_unlisted_sessions() {
    let mut tracker = SourceTracker::new();
    observed(tracker.apply_view(&view(1)));
    observed(tracker.apply_view(&view(2)));
    observed(tracker.no_context(&read(2, stamp(2, 1), false, true)));
    let mut blocked = view(2);
    blocked.lifecycle = Lifecycle::Blocked;
    assert_eq!(
        resynced(&mut tracker, &[view(1), blocked.clone(), view(3)]),
        [(id(2), stamp(2, 3), true), (id(3), stamp(3, 1), true)]
    );
    assert!(tracker.is_dirty(&id(1)));
    assert!(!tracker.is_dirty(&id(2)) && !tracker.is_dirty(&id(3)));
    // A changed session's new revision covers the output that may have been missed.
    assert!(tracker
        .observations()
        .all(|source| source.context.recent_output));

    assert_eq!(
        resynced(&mut tracker, &[blocked, view(3)]),
        [(id(1), stamp(1, 2), false)]
    );
    assert_eq!(resynced(&mut tracker, &[]).len(), 2);
    assert!(resynced(&mut tracker, &[]).is_empty());
}

#[test]
fn a_failed_resync_still_returns_every_update_it_made() {
    let mut tracker = SourceTracker::new();
    for number in 0..MAX_CACHE_ENTRIES as u32 {
        observed(tracker.apply_view(&view(number)));
    }
    let mut exited = view(0);
    exited.status = SessionStatus::Exited;
    let mut private = view(1);
    private.private = true;
    let (updates, failure) = tracker.resync(&[exited, view(9_000), private]);
    assert_eq!(failure, Some(SourceError::Capacity));
    let sources: Vec<StatusSource> = updates
        .into_iter()
        .map(|update| observed(Ok(update)))
        .collect();
    assert_eq!(sources.len(), MAX_CACHE_ENTRIES);
    assert!(sources
        .iter()
        .any(|source| source.session_id == id(0) && !source.live));
    assert!(sources
        .iter()
        .any(|source| source.session_id == id(1) && source.private));
    assert!(sources.iter().filter(|source| !source.live).count() == MAX_CACHE_ENTRIES - 1);
}

#[test]
fn daemon_event_sequences_advance_once_per_fencing_change() {
    let mut tracker = SourceTracker::new();
    observed(tracker.apply_view(&view(1)));
    let waiting = report("Waiting for synthetic input");
    let reported = SessionEvent::Reported {
        reported: waiting.clone(),
    };
    assert_eq!(
        observed(tracker.apply_event(&envelope(1, reported))).stamp,
        stamp(1, 2)
    );
    let mut updated = view(1);
    updated.reported = Some(waiting);
    unchanged(tracker.apply_event(&envelope(
        1,
        SessionEvent::Updated {
            session: updated.clone(),
        },
    )));
    updated.lifecycle = Lifecycle::Blocked;
    assert_eq!(
        observed(tracker.apply_event(&envelope(
            1,
            SessionEvent::Updated {
                session: updated.clone()
            }
        )))
        .stamp,
        stamp(1, 3)
    );
    let blocked = SessionEvent::Lifecycle {
        lifecycle: Lifecycle::Blocked,
    };
    unchanged(tracker.apply_event(&envelope(1, blocked)));

    let exited = SessionEvent::Exited { code: Some(0) };
    assert_eq!(
        observed(tracker.apply_event(&envelope(1, exited))).stamp,
        stamp(1, 4)
    );
    updated.status = SessionStatus::Exited;
    updated.lifecycle = Lifecycle::None;
    updated.terminal = None;
    let source =
        observed(tracker.apply_event(&envelope(1, SessionEvent::Updated { session: updated })));
    assert_eq!(source.stamp, stamp(1, 5));
    assert!(!source.live);
}
