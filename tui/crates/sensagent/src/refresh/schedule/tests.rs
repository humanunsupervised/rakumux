use super::*;
use crate::status::SourceStamp;

fn session(number: u8) -> SessionId {
    SessionId::from_string(format!("synthetic-scheduled-session-{number}"))
}

fn turn(conversation: &str, messages: u32) -> Option<AgentTurn> {
    Some(AgentTurn {
        id: 1,
        messages,
        conversation_id: Some(conversation.into()),
    })
}

fn candidate(number: u8, turn: Option<AgentTurn>, finished: bool) -> Candidate {
    Candidate {
        follows_turn: turn.is_some(),
        session: session(number),
        stamp: SourceStamp {
            terminal_generation: 1,
            source_revision: 1,
        },
        dirty: false,
        finished,
        turn,
    }
}

fn secs(seconds: u64) -> Duration {
    Duration::from_secs(seconds)
}

fn plan(scheduler: &mut RefreshScheduler, now: Duration, candidates: Vec<Candidate>) -> Vec<Due> {
    scheduler.plan(now, candidates, &HashSet::new())
}

fn auto(number: u8, conversation: &str) -> Due {
    Due {
        session: session(number),
        chat: conversation.into(),
        auto: true,
    }
}

#[test]
fn a_session_is_summarised_once_at_its_first_chats_fifth_output() {
    let mut scheduler = RefreshScheduler::new();
    assert!(plan(&mut scheduler, secs(0), vec![candidate(1, None, false)]).is_empty());
    assert!(plan(
        &mut scheduler,
        secs(0),
        vec![candidate(1, turn("c1", 4), false)]
    )
    .is_empty());
    let due = plan(
        &mut scheduler,
        secs(1),
        vec![candidate(1, turn("c1", 5), false)],
    );
    assert_eq!(due, [auto(1, "c1")]);
    scheduler.started(&due[0], secs(1));
    assert_eq!(
        scheduler
            .finished(&session(1), Outcome::Done, secs(2))
            .as_deref(),
        Some("c1")
    );
    // Never again: not the same chat, nor a new chat in the same session.
    for later in [turn("c1", 40), turn("c2", 5), turn("c3", 9)] {
        assert!(plan(&mut scheduler, secs(3), vec![candidate(1, later, true)]).is_empty());
    }
    // Another session has its own.
    assert_eq!(
        plan(
            &mut scheduler,
            secs(3),
            vec![candidate(2, turn("c4", 5), false)]
        ),
        [auto(2, "c4")]
    );
    // Sharing stopped and resumed: due again.
    scheduler.forget_summaries();
    assert_eq!(
        plan(
            &mut scheduler,
            secs(4),
            vec![candidate(1, turn("c2", 5), false)]
        ),
        [auto(1, "c2")]
    );
}

#[test]
fn a_summary_asked_for_and_made_is_the_sessions_one() {
    let mut scheduler = RefreshScheduler::new();
    // Asked for before its chat's fifth output: not the automatic one, but
    // made, it counts as the session's.
    scheduler.request(&session(1));
    let due = plan(
        &mut scheduler,
        secs(0),
        vec![candidate(1, turn("c1", 2), false)],
    );
    assert_eq!(
        due,
        [Due {
            session: session(1),
            chat: "c1".into(),
            auto: false,
        }]
    );
    scheduler.started(&due[0], secs(0));
    assert_eq!(
        scheduler
            .finished(&session(1), Outcome::Done, secs(1))
            .as_deref(),
        Some("c1")
    );
    for later in [turn("c1", 5), turn("c2", 5)] {
        assert!(plan(&mut scheduler, secs(2), vec![candidate(1, later, true)]).is_empty());
    }

    // Of a program whose turns the daemon doesn't follow (from its screen):
    // its agent, started later, gets none either.
    scheduler.request(&session(2));
    let due = plan(&mut scheduler, secs(0), vec![candidate(2, None, false)]);
    scheduler.started(&due[0], secs(0));
    assert_eq!(
        scheduler
            .finished(&session(2), Outcome::Done, secs(1))
            .as_deref(),
        Some("")
    );
    assert!(plan(
        &mut scheduler,
        secs(2),
        vec![candidate(2, turn("c2", 5), false)]
    )
    .is_empty());

    // One asked for that couldn't be sent isn't: the automatic one still comes.
    scheduler.request(&session(3));
    let due = plan(
        &mut scheduler,
        secs(0),
        vec![candidate(3, turn("c3", 1), false)],
    );
    scheduler.started(&due[0], secs(0));
    assert_eq!(
        scheduler.finished(
            &session(3),
            Outcome::Failed(RefreshFailure::Rejected),
            secs(1)
        ),
        None
    );
    assert_eq!(
        plan(
            &mut scheduler,
            secs(2),
            vec![candidate(3, turn("c3", 5), false)]
        ),
        [auto(3, "c3")]
    );
}

#[test]
fn a_chat_that_finishes_sooner_is_summarised_when_it_finishes() {
    let mut scheduler = RefreshScheduler::new();
    assert!(plan(
        &mut scheduler,
        secs(0),
        vec![candidate(1, turn("c1", 2), false)]
    )
    .is_empty());
    assert_eq!(
        plan(
            &mut scheduler,
            secs(1),
            vec![candidate(1, turn("c1", 2), true)]
        ),
        [auto(1, "c1")]
    );
    // A finished session with no turn (not an agent the daemon reads) isn't.
    assert!(plan(&mut scheduler, secs(1), vec![candidate(2, None, true)]).is_empty());
}

#[test]
fn asking_summarises_any_session_at_once_and_first() {
    let mut scheduler = RefreshScheduler::new();
    scheduler.summarized(&session(1));
    scheduler.request(&session(2));
    assert!(scheduler.requested(&session(2)));
    let due = plan(
        &mut scheduler,
        secs(0),
        vec![
            candidate(1, turn("c9", 9), true),
            candidate(2, None, false),
            candidate(3, turn("c3", 5), false),
        ],
    );
    let sessions: Vec<_> = due.iter().map(|due| due.session.clone()).collect();
    assert_eq!(
        sessions,
        [session(2), session(3)],
        "asked first; session 1 already summarised, in any chat"
    );
    scheduler.started(&due[0], secs(0));
    assert!(!scheduler.requested(&session(2)));
    assert!(plan(&mut scheduler, secs(0), vec![candidate(2, None, false)]).is_empty());
}

#[test]
fn failures_back_off_and_a_request_skips_the_backoff() {
    let mut scheduler = RefreshScheduler::new();
    let due = plan(
        &mut scheduler,
        secs(0),
        vec![candidate(1, turn("c1", 5), false)],
    );
    scheduler.started(&due[0], secs(0));
    scheduler.finished(
        &session(1),
        Outcome::Failed(RefreshFailure::Unavailable),
        secs(1),
    );
    assert_eq!(
        scheduler.failure(&session(1)),
        Some(RefreshFailure::Unavailable)
    );
    assert!(plan(
        &mut scheduler,
        secs(60),
        vec![candidate(1, turn("c1", 5), false)]
    )
    .is_empty());
    assert_eq!(
        plan(
            &mut scheduler,
            secs(61),
            vec![candidate(1, turn("c1", 5), false)]
        ),
        [auto(1, "c1")],
        "the automatic summary is tried again after the backoff"
    );
    let due = plan(
        &mut scheduler,
        secs(61),
        vec![candidate(1, turn("c1", 5), false)],
    );
    scheduler.started(&due[0], secs(61));
    scheduler.finished(
        &session(1),
        Outcome::Failed(RefreshFailure::Unreadable),
        secs(62),
    );
    assert!(plan(
        &mut scheduler,
        secs(63),
        vec![candidate(1, turn("c1", 5), false)]
    )
    .is_empty());
    scheduler.request(&session(1));
    assert_eq!(
        plan(
            &mut scheduler,
            secs(63),
            vec![candidate(1, turn("c1", 5), false)]
        )
        .len(),
        1
    );
}

#[test]
fn a_rejected_or_declined_automatic_summary_is_not_retried() {
    let mut scheduler = RefreshScheduler::new();
    let due = plan(
        &mut scheduler,
        secs(0),
        vec![candidate(1, turn("c1", 5), false)],
    );
    scheduler.started(&due[0], secs(0));
    scheduler.finished(
        &session(1),
        Outcome::Failed(RefreshFailure::Rejected),
        secs(1),
    );
    assert!(plan(
        &mut scheduler,
        secs(999),
        vec![candidate(1, turn("c1", 5), false)]
    )
    .is_empty());

    let due = plan(
        &mut scheduler,
        secs(0),
        vec![candidate(2, turn("c2", 5), false)],
    );
    assert_eq!(scheduler.settle(&due[0]).as_deref(), Some("c2"));
    for later in [turn("c2", 5), turn("c5", 5)] {
        assert!(plan(&mut scheduler, secs(1), vec![candidate(2, later, false)]).is_empty());
    }
}

#[test]
fn an_interrupted_summary_is_tried_again_shortly() {
    let mut scheduler = RefreshScheduler::new();
    let due = plan(
        &mut scheduler,
        secs(0),
        vec![candidate(1, turn("c1", 5), false)],
    );
    scheduler.started(&due[0], secs(0));
    scheduler.finished(&session(1), Outcome::Interrupted, secs(1));
    assert!(plan(
        &mut scheduler,
        secs(5),
        vec![candidate(1, turn("c1", 6), false)]
    )
    .is_empty());
    assert_eq!(
        plan(
            &mut scheduler,
            secs(6),
            vec![candidate(1, turn("c1", 6), false)]
        )
        .len(),
        1
    );
}

#[test]
fn an_interrupted_summary_asked_for_is_still_asked_for() {
    let mut scheduler = RefreshScheduler::new();
    scheduler.summarized(&session(1));
    scheduler.request(&session(1));
    let due = plan(
        &mut scheduler,
        secs(0),
        vec![candidate(1, turn("c1", 9), false)],
    );
    scheduler.started(&due[0], secs(0));
    assert!(scheduler.asked(&session(1)));
    // The next prompt came meanwhile, say: its summary is the one asked for.
    scheduler.finished(&session(1), Outcome::Interrupted, secs(1));
    assert!(scheduler.requested(&session(1)) && !scheduler.asked(&session(1)));
    let next = || vec![candidate(1, turn("c1", 0), false)];
    assert!(plan(&mut scheduler, secs(5), next()).is_empty());
    let due = plan(&mut scheduler, secs(6), next());
    assert_eq!(due.len(), 1);
    scheduler.started(&due[0], secs(6));
    scheduler.finished(&session(1), Outcome::Done, secs(7));
    assert!(!scheduler.requested(&session(1)));
    assert!(plan(&mut scheduler, secs(60), next()).is_empty());
}

#[test]
fn a_rate_limit_pauses_everything_and_a_refusal_stops_until_reset() {
    let mut scheduler = RefreshScheduler::new();
    scheduler.request(&session(1));
    let due = plan(&mut scheduler, secs(0), vec![candidate(1, None, false)]);
    scheduler.started(&due[0], secs(0));
    scheduler.finished(&session(1), Outcome::RateLimited(Some(secs(120))), secs(1));
    assert!(scheduler.rate_limited(secs(100)));
    scheduler.request(&session(1));
    assert!(plan(&mut scheduler, secs(100), vec![candidate(1, None, false)]).is_empty());
    assert_eq!(
        plan(&mut scheduler, secs(121), vec![candidate(1, None, false)]).len(),
        1
    );

    scheduler.finished(
        &session(1),
        Outcome::Failed(RefreshFailure::Unauthorized),
        secs(122),
    );
    assert!(scheduler.halted(secs(500)));
    scheduler.reset();
    assert!(!scheduler.halted(secs(500)));
}

#[test]
fn the_automatic_summary_follows_the_turn_settings() {
    let mut scheduler = RefreshScheduler::new();
    scheduler.set_auto_at(2);
    assert_eq!(
        plan(
            &mut scheduler,
            secs(0),
            vec![candidate(1, turn("c1", 2), false)]
        )
        .len(),
        1
    );
    scheduler.set_auto_at(0);
    assert_eq!(
        plan(
            &mut scheduler,
            secs(0),
            vec![candidate(2, turn("c2", 1), false)]
        )
        .len(),
        1
    );
}

#[test]
fn the_global_budget_bounds_starts_per_window() {
    let mut scheduler = RefreshScheduler::new();
    for number in 0..BUDGET_STARTS as u8 {
        assert!(scheduler.has_budget(secs(0)));
        scheduler.started(
            &Due {
                session: session(number),
                chat: String::new(),
                auto: false,
            },
            secs(0),
        );
    }
    assert!(!scheduler.has_budget(secs(59)));
    assert!(scheduler.has_budget(secs(60)));
}
