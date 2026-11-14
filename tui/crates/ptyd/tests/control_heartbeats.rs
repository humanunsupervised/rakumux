//! Heartbeats in the control layer, on a fake host and a clock the tests
//! move: due runs start `<name> #<n>` sessions that type the command and
//! give the agent its task once its prompt shows; earlier runs that wait
//! end, busy ones skip a due time; ten runs stay listed; agents' heartbeats
//! wait for the user's approval.

mod control_common;

use control_common::*;
use session_core::api::{ApiError, CreateSession, ErrorCode, FirstInput, Request, Response};
use session_core::heartbeat::{
    Heartbeat, HeartbeatEdit, HeartbeatFields, HeartbeatSpec, RunOutcome, Schedule,
};
use session_core::ids::{HeartbeatId, SessionId};
use session_core::model::{Creator, SessionStatus, SessionView};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;

const MINUTE_MS: i64 = 60_000;

/// A harness on a clock the test moves forward with `skip`.
fn harness() -> (Harness, Arc<AtomicI64>) {
    let skip = Arc::new(AtomicI64::new(0));
    let clock = {
        let skip = skip.clone();
        Arc::new(move || now_ms() + skip.load(Ordering::SeqCst)) as ptyd::control::Clock
    };
    (Harness::with_clock(marker_detector, clock), skip)
}

fn heartbeat(response: Response) -> Heartbeat {
    match response {
        Response::Heartbeat { heartbeat } => heartbeat,
        other => panic!("expected a heartbeat, got {other:?}"),
    }
}

fn heartbeats(h: &Harness) -> Vec<Heartbeat> {
    match h.handle(&h.user, Request::HeartbeatList { scope: None }) {
        Response::Heartbeats { heartbeats } => heartbeats,
        other => panic!("expected heartbeats, got {other:?}"),
    }
}

fn get(h: &Harness, id: &HeartbeatId) -> Heartbeat {
    heartbeat(h.handle(&h.user, Request::HeartbeatGet { id: id.clone() }))
}

fn spec(h: &Harness, name: &str, schedule: Schedule) -> HeartbeatSpec {
    HeartbeatSpec {
        scope: Some(h.scope.clone()),
        name: Some(name.into()),
        fields: HeartbeatFields {
            command: Some("fakeagent --careful".into()),
            prompt: Some("update the dependencies".into()),
            schedule: Some(schedule),
            ..Default::default()
        },
        enabled: true,
    }
}

fn every(minutes: u32) -> Schedule {
    Schedule::Every { minutes }
}

fn runs_of(h: &Harness, id: &HeartbeatId) -> Vec<SessionView> {
    let mut runs: Vec<SessionView> = h
        .list(true)
        .into_iter()
        .filter(|s| s.heartbeat.as_ref().is_some_and(|r| &r.heartbeat == id))
        .collect();
    runs.sort_by_key(|s| s.heartbeat.as_ref().map(|r| r.run));
    runs
}

fn advance(skip: &AtomicI64, ms: i64) {
    skip.fetch_add(ms, Ordering::SeqCst);
}

/// Wait for run `n` of `id`.
fn run(h: &Harness, id: &HeartbeatId, n: u32) -> SessionView {
    eventually(&format!("run #{n}"), || {
        runs_of(h, id)
            .into_iter()
            .find(|s| s.heartbeat.as_ref().is_some_and(|r| r.run == n))
    })
}

fn denied(response: Response) -> ApiError {
    let error = error(response);
    assert_eq!(error.code, ErrorCode::PermissionDenied, "{error:?}");
    error
}

#[test]
fn a_due_heartbeat_starts_a_run_that_gives_its_agent_the_task_once_its_prompt_shows() {
    let (h, skip) = harness();
    let created = heartbeat(h.handle(
        &h.user,
        Request::HeartbeatCreate(spec(&h, "deps", every(30))),
    ));
    assert!(created.enabled && !created.awaiting_approval);
    assert_eq!(
        created.agent, None,
        "fakeagent isn't an agent Rakumux names"
    );
    let due = created.next_run_at.expect("enabled: due");
    assert!((due * 1000 - now_ms() - 30 * MINUTE_MS).abs() < 5_000);
    assert!(runs_of(&h, &created.id).is_empty());

    advance(&skip, 30 * MINUTE_MS + 1_000);
    let first = run(&h, &created.id, 1);
    assert_eq!(first.name, "deps #1");
    assert_eq!(first.created_by, Creator::User);
    assert_eq!(first.launch.startup_cmds, ["fakeagent --careful"]);
    let started = get(&h, &created.id);
    assert_eq!(started.runs, 1);
    let last = started.last_run.clone().unwrap();
    assert_eq!((last.run, last.outcome), (1, RunOutcome::Started));
    assert_eq!(last.session_id.as_ref(), Some(&first.id));
    // The next one counts from now, not from the missed slot.
    let next = started.next_run_at.unwrap() * 1000;
    assert!((next - (now_ms() + skip.load(Ordering::SeqCst)) - 30 * MINUTE_MS).abs() < 5_000);

    // The shell, then the agent starting: nothing typed.
    h.host.show(&first.id, "$ fakeagent --careful");
    advance(&skip, 10_000);
    std::thread::sleep(std::time::Duration::from_millis(600));
    let terminal = h.host.terminal_of(&first.id);
    assert!(h.host.terminal(terminal).submits.is_empty());
    // At its prompt, quiet for a while: the task goes in, sent.
    h.host.show(&first.id, "fakeagent [idle]");
    std::thread::sleep(std::time::Duration::from_millis(600));
    assert!(
        h.host.terminal(terminal).submits.is_empty(),
        "the screen only just settled"
    );
    advance(&skip, 4_000);
    eventually("the task", || {
        (!h.host.terminal(terminal).submits.is_empty()).then_some(())
    });
    assert_eq!(
        h.host.terminal(terminal).submits,
        ["update the dependencies"]
    );
    eventually("the outcome", || {
        (get(&h, &created.id).last_run.unwrap().outcome == RunOutcome::TaskGiven).then_some(())
    });
    // Once only.
    advance(&skip, 10_000);
    std::thread::sleep(std::time::Duration::from_millis(600));
    assert_eq!(h.host.terminal(terminal).submits.len(), 1);
    let audit = h.audit();
    assert!(audit
        .iter()
        .any(|row| row.method == "heartbeat.run" && row.decision == "allowed"));
    assert!(!audit.iter().any(|row| row
        .detail
        .as_deref()
        .is_some_and(|d| d.contains("dependencies"))));
}

#[test]
fn an_agent_that_never_shows_its_prompt_gets_no_task() {
    let (h, skip) = harness();
    let created = heartbeat(h.handle(
        &h.user,
        Request::HeartbeatCreate(spec(&h, "deps", every(60))),
    ));
    let view = session(h.handle(
        &h.user,
        Request::HeartbeatRun {
            id: created.id.clone(),
        },
    ));
    assert_eq!(view.heartbeat.as_ref().map(|r| r.run), Some(1));
    h.host.show(&view.id, "$ fakeagent: command not found");
    advance(&skip, 121_000);
    eventually("given up", || {
        (get(&h, &created.id).last_run.unwrap().outcome == RunOutcome::NoAgent).then_some(())
    });
    let terminal = h.host.terminal_of(&view.id);
    assert!(h.host.terminal(terminal).submits.is_empty());
}

#[test]
fn earlier_runs_that_wait_end_and_busy_ones_skip_the_due_time() {
    let (h, skip) = harness();
    let created = heartbeat(h.handle(
        &h.user,
        Request::HeartbeatCreate(spec(&h, "deps", every(30))),
    ));
    advance(&skip, 30 * MINUTE_MS + 1_000);
    let first = run(&h, &created.id, 1);

    // At work at the next due time: skipped, nothing ended.
    h.host.show(&first.id, "fakeagent [working]");
    eventually("working", || {
        (h.get(&first.id).lifecycle == session_core::model::Lifecycle::Working).then_some(())
    });
    advance(&skip, 30 * MINUTE_MS + 1_000);
    eventually("skipped", || {
        let last = get(&h, &created.id).last_run.unwrap();
        (last.outcome == RunOutcome::Skipped).then_some(last)
    });
    assert_eq!(runs_of(&h, &created.id).len(), 1);
    assert!(!h.host.terminal(h.host.terminal_of(&first.id)).terminated);

    // Waiting at the next: it ends (its screen kept), and #2 starts.
    h.host.show(&first.id, "fakeagent [idle]");
    eventually("idle", || {
        (h.get(&first.id).lifecycle != session_core::model::Lifecycle::Working).then_some(())
    });
    advance(&skip, 30 * MINUTE_MS + 1_000);
    let second = run(&h, &created.id, 2);
    assert!(h.host.terminal(h.host.terminal_of(&first.id)).terminated);
    eventually("#1 ended", || {
        (h.get(&first.id).status == SessionStatus::Exited).then_some(())
    });
    assert_eq!(second.name, "deps #2");
    assert_eq!(get(&h, &created.id).runs, 2);

    // Asked for while #2 works: it starts beside it.
    h.host.show(&second.id, "fakeagent [working]");
    eventually("working", || {
        (h.get(&second.id).lifecycle == session_core::model::Lifecycle::Working).then_some(())
    });
    let third = session(h.handle(
        &h.user,
        Request::HeartbeatRun {
            id: created.id.clone(),
        },
    ));
    assert_eq!(third.name, "deps #3");
    assert!(!h.host.terminal(h.host.terminal_of(&second.id)).terminated);
}

#[test]
fn a_long_sleep_gives_one_run_and_ten_runs_stay_listed() {
    let (h, skip) = harness();
    let created = heartbeat(h.handle(
        &h.user,
        Request::HeartbeatCreate(spec(&h, "deps", every(30))),
    ));
    // Five hours away: one run, not ten.
    advance(&skip, 5 * 60 * MINUTE_MS);
    run(&h, &created.id, 1);
    std::thread::sleep(std::time::Duration::from_millis(800));
    assert_eq!(runs_of(&h, &created.id).len(), 1);
    assert_eq!(get(&h, &created.id).runs, 1);

    // Eleven more, each ending the one before: the oldest ended ones go to
    // the session history, ten stay.
    for n in 2..=12 {
        let view = session(h.handle(
            &h.user,
            Request::HeartbeatRun {
                id: created.id.clone(),
            },
        ));
        assert_eq!(view.heartbeat.as_ref().unwrap().run, n);
    }
    eventually("ten listed", || {
        let runs = runs_of(&h, &created.id);
        (runs.len() <= 11).then_some(runs)
    });
    // #12 ended #11 as it started; the next run trims to ten.
    let thirteen = session(h.handle(
        &h.user,
        Request::HeartbeatRun {
            id: created.id.clone(),
        },
    ));
    let runs = runs_of(&h, &created.id);
    let numbers: Vec<u32> = runs
        .iter()
        .map(|s| s.heartbeat.as_ref().unwrap().run)
        .collect();
    assert!(numbers.len() <= RUNS_KEPT_PLUS_ONE, "{numbers:?}");
    assert!(numbers.contains(&13) && !numbers.contains(&1) && !numbers.contains(&2));
    assert_eq!(thirteen.name, "deps #13");
    let history = match h.handle(
        &h.user,
        Request::SessionHistory {
            query: Some("deps #1".into()),
            limit: None,
            before: None,
        },
    ) {
        Response::History(page) => page.sessions,
        other => panic!("{other:?}"),
    };
    assert!(history.iter().any(|s| s.name == "deps #1"));
}

/// Ten, plus the one the newest run ended as it started (trimmed next time).
const RUNS_KEPT_PLUS_ONE: usize = 11;

#[test]
fn agents_heartbeats_wait_for_the_users_approval() {
    let (h, _skip) = harness();
    let agent = h.create(Some("agent"));
    let caller = h.session_caller(&agent.id);

    // Made by a session, in its own folder when it names none: paused.
    let mut asked = spec(&h, "nightly", Schedule::Daily { at: "02:00".into() });
    asked.scope = None;
    let made = heartbeat(h.handle(&caller, Request::HeartbeatCreate(asked)));
    assert_eq!(made.scope, h.scope);
    assert!(!made.enabled && made.awaiting_approval && made.next_run_at.is_none());
    assert_eq!(
        made.created_by,
        Creator::Session {
            id: agent.id.clone()
        }
    );
    // A session sees every folder's, and lists and reads them.
    match h.handle(&caller, Request::HeartbeatList { scope: None }) {
        Response::Heartbeats { heartbeats } => assert_eq!(heartbeats.len(), 1),
        other => panic!("{other:?}"),
    }
    // It can't approve, run or delete one.
    denied(h.handle(
        &caller,
        Request::HeartbeatEnable {
            id: made.id.clone(),
            enabled: true,
        },
    ));
    denied(h.handle(
        &caller,
        Request::HeartbeatRun {
            id: made.id.clone(),
        },
    ));
    denied(h.handle(
        &caller,
        Request::HeartbeatDelete {
            id: made.id.clone(),
        },
    ));

    // The user approves: it runs on its schedule.
    let approved = heartbeat(h.handle(
        &h.user,
        Request::HeartbeatEnable {
            id: made.id.clone(),
            enabled: true,
        },
    ));
    assert!(approved.enabled && !approved.awaiting_approval && approved.next_run_at.is_some());

    // Renaming it doesn't change what it runs: still enabled.
    let edit = |name: Option<&str>, fields: HeartbeatFields| {
        Request::HeartbeatUpdate(HeartbeatEdit {
            id: made.id.clone(),
            name: name.map(String::from),
            fields,
        })
    };
    let renamed = heartbeat(h.handle(&caller, edit(Some("nightly build"), Default::default())));
    assert!(renamed.enabled && renamed.name == "nightly build");
    // Changing its prompt does: paused again until approved.
    let changed = heartbeat(h.handle(
        &caller,
        edit(
            None,
            HeartbeatFields {
                prompt: Some("and push to main".into()),
                ..Default::default()
            },
        ),
    ));
    assert!(!changed.enabled && changed.awaiting_approval && changed.next_run_at.is_none());

    // Through rakumux-mcp the user's caller says by_agent: the same.
    let approved = heartbeat(h.handle(
        &h.user,
        Request::HeartbeatEnable {
            id: made.id.clone(),
            enabled: true,
        },
    ));
    assert!(approved.enabled);
    let by_agent = heartbeat(h.handle(
        &h.user,
        edit(
            None,
            HeartbeatFields {
                schedule: Some(every(60)),
                by_agent: true,
                ..Default::default()
            },
        ),
    ));
    assert!(!by_agent.enabled && by_agent.awaiting_approval);
    let mut mcp = spec(&h, "from mcp", every(60));
    mcp.fields.by_agent = true;
    let made = heartbeat(h.handle(&h.user, Request::HeartbeatCreate(mcp)));
    assert!(!made.enabled && made.awaiting_approval);
    // The user's own edits keep it enabled.
    let approved = heartbeat(h.handle(
        &h.user,
        Request::HeartbeatEnable {
            id: made.id.clone(),
            enabled: true,
        },
    ));
    let kept = heartbeat(h.handle(
        &h.user,
        Request::HeartbeatUpdate(HeartbeatEdit {
            id: approved.id.clone(),
            name: None,
            fields: HeartbeatFields {
                prompt: Some("something else".into()),
                ..Default::default()
            },
        }),
    ));
    assert!(kept.enabled && !kept.awaiting_approval);
    let audit = h.audit();
    assert!(audit
        .iter()
        .any(|row| row.method == "heartbeat.enable" && row.decision == "denied"));
}

#[test]
fn a_heartbeat_needs_a_prompt_and_a_schedule_before_it_runs() {
    let (h, _skip) = harness();
    let empty = heartbeat(h.handle(
        &h.user,
        Request::HeartbeatCreate(HeartbeatSpec {
            scope: Some(h.scope.clone()),
            ..Default::default()
        }),
    ));
    assert_eq!(empty.name, "Heartbeat");
    assert!(!empty.enabled);
    // Nothing ran in the folder yet: the default agent.
    assert_eq!(empty.command, "claude");
    assert_eq!(empty.agent.as_deref(), Some("claude"));
    let refused = error(h.handle(
        &h.user,
        Request::HeartbeatEnable {
            id: empty.id.clone(),
            enabled: true,
        },
    ));
    assert_eq!(refused.code, ErrorCode::InvalidArgument);
    assert!(
        refused.message.contains("a prompt, a schedule"),
        "{}",
        refused.message
    );
    let second = heartbeat(h.handle(
        &h.user,
        Request::HeartbeatCreate(HeartbeatSpec {
            scope: Some(h.scope.clone()),
            ..Default::default()
        }),
    ));
    assert_eq!(second.name, "Heartbeat 2");
    // Names are unique within a folder; schedules and prompts are checked.
    let taken = error(h.handle(
        &h.user,
        Request::HeartbeatCreate(spec(&h, "Heartbeat", every(30))),
    ));
    assert_eq!(taken.code, ErrorCode::InvalidArgument);
    let fast = error(h.handle(
        &h.user,
        Request::HeartbeatCreate(spec(&h, "fast", every(1))),
    ));
    assert!(fast.message.contains("every 5 minutes"), "{}", fast.message);
    let mut bad = spec(&h, "bad", every(30));
    bad.fields.command = Some("claude\nrm -rf /".into());
    assert_eq!(
        error(h.handle(&h.user, Request::HeartbeatCreate(bad))).code,
        ErrorCode::InvalidArgument
    );
    // Deleted: gone from the list.
    assert_eq!(
        h.handle(
            &h.user,
            Request::HeartbeatDelete {
                id: second.id.clone()
            }
        ),
        Response::Ok
    );
    assert_eq!(heartbeats(&h).len(), 1);
}

#[test]
fn a_new_heartbeat_starts_from_the_flags_the_folders_last_agent_ran_with() {
    let (h, _skip) = harness();
    let older = h.create(Some("older"));
    let newer = h.create(Some("newer"));
    let set_resume = |id: &SessionId, argv: &str, at: i64| {
        h.db()
            .execute(
                "UPDATE sessions_v2 SET resume_json = ?2, last_active_at = ?3 WHERE id = ?1",
                rusqlite::params![
                    id.as_str(),
                    format!(r#"{{"argv":{argv},"source":"known_agent"}}"#),
                    at
                ],
            )
            .unwrap();
    };
    set_resume(
        &older.id,
        r#"["codex","resume","--full-auto","019a"]"#,
        now_ms() - 60_000,
    );
    set_resume(
        &newer.id,
        r#"["claude","--permission-mode","acceptEdits","--resume","abc"]"#,
        now_ms(),
    );
    let made = |agent: Option<&str>| {
        heartbeat(h.handle(
            &h.user,
            Request::HeartbeatCreate(HeartbeatSpec {
                scope: Some(h.scope.clone()),
                fields: HeartbeatFields {
                    agent: agent.map(String::from),
                    ..Default::default()
                },
                ..Default::default()
            }),
        ))
    };
    let any = made(None);
    assert_eq!(any.command, "claude --permission-mode acceptEdits");
    assert_eq!(any.agent.as_deref(), Some("claude"));
    let codex = made(Some("codex"));
    assert_eq!(codex.command, "codex --full-auto");
    let kimi = made(Some("kimi"));
    assert_eq!(kimi.command, "kimi");
    // A command given is kept as written, its agent read from it.
    let mut given = spec(&h, "given", every(30));
    given.fields.command = Some("FOO=1 opencode2 -m big".into());
    let given = heartbeat(h.handle(&h.user, Request::HeartbeatCreate(given)));
    assert_eq!(given.command, "FOO=1 opencode2 -m big");
    assert_eq!(given.agent.as_deref(), Some("opencode"));
}

#[test]
fn heartbeats_and_runs_survive_a_restart_and_deleting_one_leaves_its_runs() {
    let (mut h, _skip) = harness();
    let created = heartbeat(h.handle(
        &h.user,
        Request::HeartbeatCreate(spec(&h, "deps", every(30))),
    ));
    let view = session(h.handle(
        &h.user,
        Request::HeartbeatRun {
            id: created.id.clone(),
        },
    ));
    // The run sets the next due time from its own second.
    let before = get(&h, &created.id);
    h.restart();
    assert_eq!(heartbeats(&h).len(), 1);
    let kept = get(&h, &created.id);
    assert_eq!(kept.runs, 1);
    assert_eq!(kept.next_run_at, before.next_run_at);
    assert_eq!(h.get(&view.id).heartbeat.as_ref().map(|r| r.run), Some(1));
    assert_eq!(
        h.handle(
            &h.user,
            Request::HeartbeatDelete {
                id: created.id.clone()
            }
        ),
        Response::Ok
    );
    assert!(h.get(&view.id).heartbeat.is_none());
    assert!(heartbeats(&h).is_empty());
}

#[test]
fn a_first_input_not_sent_is_typed_without_enter_and_never_into_the_shell() {
    let (h, skip) = harness();
    let request = CreateSession {
        startup_cmds: vec!["fakeagent".into()],
        first_input: Some(FirstInput {
            text: "Heartbeat hb_X is empty. Ask me what to run.".into(),
            submit: false,
        }),
        ..h.create_request()
    };
    let view = session(h.handle(&h.user, Request::SessionCreate(request)));
    let terminal = h.host.terminal_of(&view.id);
    h.host.show(&view.id, "$ ");
    advance(&skip, 5_000);
    std::thread::sleep(std::time::Duration::from_millis(600));
    assert!(h.host.terminal(terminal).keys.is_empty());
    h.host.show(&view.id, "fakeagent [idle]");
    std::thread::sleep(std::time::Duration::from_millis(400));
    advance(&skip, 4_000);
    eventually("typed", || {
        (!h.host.terminal(terminal).keys.is_empty()).then_some(())
    });
    let typed = h.host.terminal(terminal);
    assert_eq!(
        typed.keys,
        [(
            vec![],
            Some("Heartbeat hb_X is empty. Ask me what to run.".to_string())
        )]
    );
    assert!(typed.submits.is_empty());
    // A line break in text that isn't sent would run it.
    let request = CreateSession {
        first_input: Some(FirstInput {
            text: "one\ntwo".into(),
            submit: false,
        }),
        ..h.create_request()
    };
    assert_eq!(
        error(h.handle(&h.user, Request::SessionCreate(request))).code,
        ErrorCode::InvalidArgument
    );
}
