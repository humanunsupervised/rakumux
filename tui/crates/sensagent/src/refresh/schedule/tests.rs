use super::*;

fn session(number: u8) -> SessionId {
    SessionId::from_string(format!("synthetic-scheduled-session-{number}"))
}

fn stamp(revision: u64) -> SourceStamp {
    SourceStamp {
        terminal_generation: 1,
        source_revision: revision,
    }
}

fn candidate(number: u8, revision: u64, dirty: bool) -> Candidate {
    Candidate {
        session: session(number),
        stamp: stamp(revision),
        dirty,
    }
}

fn secs(seconds: u64) -> Duration {
    Duration::from_secs(seconds)
}

fn plan(scheduler: &mut RefreshScheduler, now: u64, candidates: Vec<Candidate>) -> Vec<(u8, bool)> {
    scheduler
        .plan(secs(now), candidates, &HashSet::new())
        .into_iter()
        .map(|due| {
            let number = due
                .session
                .as_str()
                .trim_start_matches("synthetic-scheduled-session-")
                .parse()
                .unwrap();
            (number, due.advance)
        })
        .collect()
}

/// Start and finish one attempt for session `number` at `revision`.
fn attempt(
    scheduler: &mut RefreshScheduler,
    number: u8,
    revision: u64,
    now: u64,
    outcome: Outcome,
) {
    scheduler.started(&session(number), stamp(revision), false, secs(now));
    scheduler.finished(&session(number), stamp(revision), outcome, secs(now));
}

#[test]
fn new_sessions_are_due_once_and_settled_stamps_are_skipped() {
    let mut scheduler = RefreshScheduler::new();
    assert_eq!(
        plan(&mut scheduler, 0, vec![candidate(1, 1, false)]),
        [(1, false)]
    );
    attempt(&mut scheduler, 1, 1, 0, Outcome::Done);
    for now in [1, 60, 600] {
        assert!(plan(&mut scheduler, now, vec![candidate(1, 1, false)]).is_empty());
    }
    scheduler.settle(&session(2), stamp(4), false);
    assert!(plan(&mut scheduler, 0, vec![candidate(2, 4, false)]).is_empty());
    assert_eq!(
        plan(&mut scheduler, 0, vec![candidate(2, 5, false)]),
        [(2, false)]
    );
}

#[test]
fn a_changed_stamp_waits_for_the_session_interval() {
    let mut scheduler = RefreshScheduler::new();
    attempt(&mut scheduler, 1, 1, 10, Outcome::Done);
    assert!(plan(&mut scheduler, 69, vec![candidate(1, 2, false)]).is_empty());
    assert_eq!(
        plan(&mut scheduler, 70, vec![candidate(1, 2, false)]),
        [(1, false)]
    );
}

#[test]
fn busy_output_is_refreshed_after_a_quiet_period_or_one_interval() {
    let mut scheduler = RefreshScheduler::new();
    attempt(&mut scheduler, 1, 1, 0, Outcome::Done);
    // Continuous output from t=1: no quiet period, so it waits a whole interval.
    for now in 1..=60 {
        scheduler.output(&session(1), secs(now));
        assert!(
            plan(&mut scheduler, now, vec![candidate(1, 1, true)]).is_empty(),
            "{now}"
        );
    }
    scheduler.output(&session(1), secs(61));
    assert_eq!(
        plan(&mut scheduler, 61, vec![candidate(1, 1, true)]),
        [(1, true)]
    );

    // Output that stops is refreshed once quiet, but never inside the interval.
    let mut scheduler = RefreshScheduler::new();
    attempt(&mut scheduler, 1, 1, 0, Outcome::Done);
    scheduler.output(&session(1), secs(10));
    assert!(plan(&mut scheduler, 59, vec![candidate(1, 1, true)]).is_empty());
    assert_eq!(
        plan(&mut scheduler, 60, vec![candidate(1, 1, true)]),
        [(1, true)]
    );
    scheduler.output(&session(1), secs(100));
    scheduler.advanced(&session(1));
    scheduler.output(&session(1), secs(120));
    assert!(plan(&mut scheduler, 124, vec![candidate(1, 1, true)]).is_empty());
    assert_eq!(
        plan(&mut scheduler, 125, vec![candidate(1, 1, true)]),
        [(1, true)]
    );
}

#[test]
fn clean_sessions_forget_old_output_timing() {
    let mut scheduler = RefreshScheduler::new();
    attempt(&mut scheduler, 1, 1, 0, Outcome::Done);
    scheduler.output(&session(1), secs(1));
    // A fact change folded the output into a new revision, which was then settled.
    assert_eq!(
        plan(&mut scheduler, 61, vec![candidate(1, 2, false)]),
        [(1, false)]
    );
    scheduler.settle(&session(1), stamp(2), false);
    scheduler.output(&session(1), secs(100));
    assert!(plan(&mut scheduler, 102, vec![candidate(1, 2, true)]).is_empty());
    assert_eq!(
        plan(&mut scheduler, 105, vec![candidate(1, 2, true)]),
        [(1, true)]
    );
}

#[test]
fn urgent_sessions_lead_then_the_longest_unrefreshed() {
    let mut scheduler = RefreshScheduler::new();
    attempt(&mut scheduler, 1, 1, 0, Outcome::Done);
    attempt(&mut scheduler, 2, 1, 5, Outcome::Done);
    attempt(&mut scheduler, 3, 1, 1, Outcome::Done);
    attempt(&mut scheduler, 4, 1, 2, Outcome::Done);
    let candidates = vec![
        candidate(1, 1, true),
        candidate(2, 2, false),
        candidate(3, 1, true),
        candidate(4, 2, true),
        candidate(5, 1, false),
    ];
    assert_eq!(
        plan(&mut scheduler, 200, candidates),
        [(5, false), (4, true), (2, false), (1, true), (3, true)]
    );
    let running = HashSet::from([session(5), session(4)]);
    let due = scheduler.plan(
        secs(200),
        vec![candidate(4, 2, true), candidate(5, 1, false)],
        &running,
    );
    assert!(due.is_empty());
}

#[test]
fn the_global_budget_limits_starts_per_window() {
    let mut scheduler = RefreshScheduler::new();
    for number in 0..BUDGET_STARTS as u8 {
        assert!(scheduler.has_budget(secs(10)));
        scheduler.started(&session(number), stamp(1), false, secs(10));
    }
    assert!(!scheduler.has_budget(secs(10)));
    assert!(!scheduler.has_budget(secs(69)));
    assert!(scheduler.has_budget(secs(70)));
}

#[test]
fn failures_back_off_and_rejected_input_waits_for_new_output() {
    let mut scheduler = RefreshScheduler::new();
    for (number, failure) in [
        (1, RefreshFailure::Unreadable),
        (4, RefreshFailure::Unavailable),
    ] {
        attempt(&mut scheduler, number, 1, 0, Outcome::Failed(failure));
        assert_eq!(scheduler.failure(&session(number)), Some(failure));
        assert!(plan(&mut scheduler, 59, vec![candidate(number, 1, false)]).is_empty());
        assert_eq!(
            plan(&mut scheduler, 60, vec![candidate(number, 1, false)]),
            [(number, false)]
        );
    }

    attempt(
        &mut scheduler,
        2,
        1,
        0,
        Outcome::Failed(RefreshFailure::Rejected),
    );
    assert_eq!(
        scheduler.failure(&session(2)),
        Some(RefreshFailure::Rejected)
    );
    assert!(plan(&mut scheduler, 600, vec![candidate(2, 1, false)]).is_empty());
    scheduler.output(&session(2), secs(600));
    assert_eq!(
        plan(&mut scheduler, 605, vec![candidate(2, 1, true)]),
        [(2, true)]
    );
    attempt(&mut scheduler, 2, 2, 605, Outcome::Done);
    assert_eq!(scheduler.failure(&session(2)), None);

    attempt(&mut scheduler, 3, 1, 0, Outcome::Interrupted);
    assert_eq!(scheduler.failure(&session(3)), None);
    assert!(plan(&mut scheduler, 59, vec![candidate(3, 1, false)]).is_empty());
    assert_eq!(
        plan(&mut scheduler, 60, vec![candidate(3, 1, false)]),
        [(3, false)]
    );
}

#[test]
fn rate_limits_pause_every_session_for_at_least_the_backoff() {
    let mut scheduler = RefreshScheduler::new();
    attempt(
        &mut scheduler,
        1,
        1,
        0,
        Outcome::RateLimited(Some(secs(300))),
    );
    assert!(scheduler.rate_limited(secs(299)));
    assert!(plan(&mut scheduler, 299, vec![candidate(2, 1, false)]).is_empty());
    assert!(!scheduler.rate_limited(secs(300)));
    assert_eq!(
        plan(
            &mut scheduler,
            300,
            vec![candidate(1, 1, false), candidate(2, 1, false)]
        ),
        [(2, false), (1, false)]
    );

    let mut scheduler = RefreshScheduler::new();
    attempt(&mut scheduler, 1, 1, 0, Outcome::RateLimited(Some(secs(1))));
    assert!(plan(&mut scheduler, 59, vec![candidate(2, 1, false)]).is_empty());
    assert_eq!(
        plan(&mut scheduler, 60, vec![candidate(2, 1, false)]),
        [(2, false)]
    );
    // A reset never lifts the service's pause.
    attempt(&mut scheduler, 1, 1, 100, Outcome::RateLimited(None));
    scheduler.reset();
    assert!(plan(&mut scheduler, 159, vec![candidate(2, 1, false)]).is_empty());
}

#[test]
fn unauthorized_stops_everything_until_reset() {
    let mut scheduler = RefreshScheduler::new();
    attempt(
        &mut scheduler,
        1,
        1,
        0,
        Outcome::Failed(RefreshFailure::Unauthorized),
    );
    assert!(scheduler.unauthorized());
    assert!(plan(&mut scheduler, 3600, vec![candidate(2, 1, false)]).is_empty());
    scheduler.reset();
    assert!(!scheduler.unauthorized());
    assert_eq!(scheduler.failure(&session(1)), None);
    assert_eq!(
        plan(
            &mut scheduler,
            3600,
            vec![candidate(1, 1, false), candidate(2, 1, false)]
        ),
        [(2, false), (1, false)]
    );
}

#[test]
fn sticky_decisions_ignore_output_until_the_stamp_or_consent_changes() {
    let mut scheduler = RefreshScheduler::new();
    scheduler.settle(&session(1), stamp(1), true);
    scheduler.output(&session(1), secs(0));
    assert!(plan(&mut scheduler, 600, vec![candidate(1, 1, true)]).is_empty());
    assert_eq!(
        plan(&mut scheduler, 600, vec![candidate(1, 2, true)]),
        [(1, true)]
    );
    scheduler.reset();
    assert_eq!(
        plan(&mut scheduler, 600, vec![candidate(1, 1, true)]),
        [(1, true)]
    );
}

#[test]
fn reset_keeps_intervals_and_forget_drops_a_session() {
    let mut scheduler = RefreshScheduler::new();
    attempt(&mut scheduler, 1, 1, 0, Outcome::Done);
    scheduler.reset();
    assert!(plan(&mut scheduler, 59, vec![candidate(1, 1, false)]).is_empty());
    assert_eq!(
        plan(&mut scheduler, 60, vec![candidate(1, 1, false)]),
        [(1, false)]
    );
    scheduler.forget(&session(1));
    assert_eq!(
        plan(&mut scheduler, 1, vec![candidate(1, 1, false)]),
        [(1, false)]
    );
    assert_eq!(
        format!("{scheduler:?}"),
        "RefreshScheduler { sessions: 1, starts: 1, unauthorized: false }"
    );
}

#[test]
fn repeated_failures_double_the_backoff_up_to_a_limit() {
    let mut scheduler = RefreshScheduler::new();
    let mut now = 0;
    for wait in [60, 120, 240, 480, 960, 960] {
        attempt(
            &mut scheduler,
            1,
            1,
            now,
            Outcome::Failed(RefreshFailure::Unavailable),
        );
        assert!(plan(&mut scheduler, now + wait - 1, vec![candidate(1, 1, false)]).is_empty());
        assert_eq!(
            plan(&mut scheduler, now + wait, vec![candidate(1, 1, false)]),
            [(1, false)]
        );
        now += wait;
    }
    attempt(&mut scheduler, 1, 1, now, Outcome::Done);
    attempt(
        &mut scheduler,
        1,
        2,
        now + 60,
        Outcome::Failed(RefreshFailure::Unreadable),
    );
    assert_eq!(
        plan(&mut scheduler, now + 120, vec![candidate(1, 2, false)]),
        [(1, false)]
    );
}

#[test]
fn reports_only_jobs_ignore_output_until_the_stamp_changes() {
    let mut scheduler = RefreshScheduler::new();
    scheduler.started(&session(1), stamp(1), true, secs(0));
    scheduler.finished(&session(1), stamp(1), Outcome::Done, secs(0));
    scheduler.output(&session(1), secs(1));
    assert!(plan(&mut scheduler, 600, vec![candidate(1, 1, true)]).is_empty());
    assert_eq!(
        plan(&mut scheduler, 600, vec![candidate(1, 2, true)]),
        [(1, true)]
    );
}

#[test]
fn a_session_removed_during_its_attempt_is_not_tracked_again() {
    let mut scheduler = RefreshScheduler::new();
    scheduler.started(&session(1), stamp(1), false, secs(0));
    scheduler.forget(&session(1));
    scheduler.finished(&session(1), stamp(1), Outcome::RateLimited(None), secs(0));
    assert!(scheduler.rate_limited(secs(59)));
    assert_eq!(
        format!("{scheduler:?}"),
        "RefreshScheduler { sessions: 0, starts: 1, unauthorized: false }"
    );
}
