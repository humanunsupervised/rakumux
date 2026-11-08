//! The session history: deleting a session keeps it (record, saved output,
//! places) for 90 days, out of every list and free of its name, and
//! `session_restore` brings it back dead in its folder.

mod control_common;

use control_common::*;
use ptyd::control_api::Control;
use ptyd::persist::{ScrollbackStore, SessionMeta};
use session_core::api::{
    ErrorCode, EventKind, HistoryPage, ReadRequest, ReadSource, Request, Response, ReviveSession,
    SessionEvent, SubscribeRequest, TextFormat,
};
use session_core::ids::SessionId;
use session_core::model::SessionStatus;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;
use std::time::Duration;

const DAY_MS: i64 = 24 * 60 * 60 * 1000;

fn delete(h: &Harness, id: &SessionId) {
    assert_eq!(
        h.handle(&h.user, Request::SessionDelete { id: id.clone() }),
        Response::Ok
    );
}

fn history_page(
    h: &Harness,
    query: Option<&str>,
    limit: Option<u32>,
    before: Option<&SessionId>,
) -> Response {
    h.handle(
        &h.user,
        Request::SessionHistory {
            query: query.map(str::to_owned),
            limit,
            before: before.cloned(),
        },
    )
}

fn history(h: &Harness, query: Option<&str>) -> HistoryPage {
    match history_page(h, query, None, None) {
        Response::History(page) => page,
        other => panic!("expected the session history, got {other:?}"),
    }
}

fn names(page: &HistoryPage) -> Vec<String> {
    page.sessions.iter().map(|s| s.name.clone()).collect()
}

fn restore(h: &Harness, id: &SessionId) -> Response {
    h.handle(&h.user, Request::SessionRestore { id: id.clone() })
}

fn save_output(h: &Harness, id: &SessionId, text: &[u8]) {
    ScrollbackStore::new(h.layout.clone())
        .save(
            id,
            text,
            &SessionMeta {
                updated_at: 1,
                ..SessionMeta::default()
            },
        )
        .unwrap();
}

fn count(h: &Harness, sql: &str, id: &SessionId) -> i64 {
    h.db()
        .query_row(sql, [id.as_str()], |row| row.get(0))
        .unwrap()
}

#[test]
fn a_deleted_session_leaves_every_list_and_frees_its_name_but_is_kept() {
    let h = Harness::new();
    let s = h.create(Some("scratch"));
    let terminal = h.host.terminal_of(&s.id);
    save_output(&h, &s.id, b"saved output\r\n");
    h.db()
        .execute(
            "INSERT INTO session_places (session_id, path, git_json, last_at, edited) \
             VALUES (?1, '/repo', '{\"repo_root\":\"/repo\",\"branch\":\"main\"}', 1, 1)",
            [s.id.as_str()],
        )
        .unwrap();
    let events = h
        .control
        .subscribe(
            &h.user,
            &SubscribeRequest {
                all: true,
                kinds: vec![EventKind::Removed],
                ..Default::default()
            },
        )
        .unwrap();

    delete(&h, &s.id);

    let fake = h.host.terminal(terminal);
    assert!(fake.persisted && fake.terminated && fake.forgotten);
    let removed = events.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(removed.session_id, s.id);
    assert_eq!(removed.event, SessionEvent::Removed);
    assert_eq!(
        error(h.handle(&h.user, Request::SessionGet { id: s.id.clone() })).code,
        ErrorCode::NotFound
    );
    assert!(h.list(true).is_empty());
    assert!(ScrollbackStore::new(h.layout.clone())
        .load(&s.id)
        .unwrap()
        .is_some());
    assert_eq!(
        count(
            &h,
            "SELECT COUNT(*) FROM session_places WHERE session_id = ?1",
            &s.id
        ),
        1
    );
    // Its folder can be forgotten: it holds no session now.
    assert_eq!(
        h.handle(
            &h.user,
            Request::ProjectRemove {
                path: h.scope.clone()
            }
        ),
        Response::Ok
    );

    let page = history(&h, None);
    assert_eq!(names(&page), ["scratch"]);
    let entry = &page.sessions[0];
    assert_eq!(entry.id, s.id);
    assert_eq!(entry.scope, h.scope);
    assert!(entry.deleted_at >= s.created_at);
    assert!(!page.more);

    // Its name is free, and the next start doesn't bring it back.
    h.create(Some("scratch"));
    assert_eq!(h.list(true).len(), 1);
}

#[test]
fn restore_brings_a_session_back_dead_renamed_when_its_name_was_taken() {
    let h = Harness::new();
    let s = h.create(Some("scratch"));
    save_output(&h, &s.id, b"before it was deleted\r\n");
    delete(&h, &s.id);
    let taken = h.create(Some("scratch"));
    let events = h
        .control
        .subscribe(
            &h.user,
            &SubscribeRequest {
                all: true,
                kinds: vec![EventKind::Created],
                ..Default::default()
            },
        )
        .unwrap();

    let restored = session(restore(&h, &s.id));

    assert_eq!(restored.id, s.id);
    assert_eq!(restored.name, "scratch 2");
    assert_eq!(restored.status, SessionStatus::Dead);
    assert_eq!(restored.scope, h.scope);
    let created = events.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(created.session_id, s.id);
    assert!(history(&h, None).sessions.is_empty());
    let mut listed: Vec<_> = h.list(true).into_iter().map(|s| s.name).collect();
    listed.sort();
    assert_eq!(listed, ["scratch", "scratch 2"]);
    let _ = taken;
    // Its saved output reads as a dead session's, and it revives.
    let read = h.handle(
        &h.user,
        Request::SessionRead(ReadRequest {
            id: s.id.clone(),
            source: ReadSource::Recent,
            lines: Some(10),
            format: TextFormat::Text,
        }),
    );
    match read {
        Response::Read(result) => assert!(result.text.contains("before it was deleted")),
        other => panic!("expected saved output, got {other:?}"),
    }
    let revived = session(h.handle(
        &h.user,
        Request::SessionRevive(ReviveSession {
            id: s.id.clone(),
            env: Vec::new(),
            cols: 80,
            rows: 24,
            resume: true,
        }),
    ));
    assert_eq!(revived.status, SessionStatus::Live);
    // Restoring twice: it's no longer in the history.
    assert_eq!(error(restore(&h, &s.id)).code, ErrorCode::InvalidArgument);
    assert_eq!(
        error(restore(&h, &SessionId::from_string("01NOPE"))).code,
        ErrorCode::NotFound
    );
    assert!(h
        .audit()
        .iter()
        .any(|row| row.method == "session.restore" && row.decision == "allowed"));
}

#[test]
fn the_history_is_newest_first_searchable_and_paged() {
    let h = Harness::new();
    let alpha = h.create(Some("alpha"));
    let beta = h.create(Some("beta"));
    let gamma = h.create_in(&h.other_scope(), Some("gamma"));
    for id in [&alpha.id, &beta.id, &gamma.id] {
        delete(&h, id);
        std::thread::sleep(Duration::from_millis(3));
    }

    assert_eq!(names(&history(&h, None)), ["gamma", "beta", "alpha"]);
    assert_eq!(names(&history(&h, Some("ALP"))), ["alpha"]);
    // The folder's path matches too.
    assert_eq!(names(&history(&h, Some("other"))), ["gamma"]);
    assert!(history(&h, Some("nothing like it")).sessions.is_empty());

    let first = match history_page(&h, None, Some(2), None) {
        Response::History(page) => page,
        other => panic!("{other:?}"),
    };
    assert_eq!(names(&first), ["gamma", "beta"]);
    assert!(first.more);
    let rest = match history_page(&h, None, Some(2), Some(&first.sessions[1].id)) {
        Response::History(page) => page,
        other => panic!("{other:?}"),
    };
    assert_eq!(names(&rest), ["alpha"]);
    assert!(!rest.more);
    assert_eq!(
        error(history_page(
            &h,
            None,
            None,
            Some(&SessionId::from_string("01NOPE"))
        ))
        .code,
        ErrorCode::NotFound
    );
}

#[test]
fn session_callers_get_no_history_and_restore_nothing() {
    let h = Harness::new();
    let gone = h.create(Some("gone"));
    delete(&h, &gone.id);
    let s = h.create(Some("agent"));
    let caller = h.session_caller(&s.id);

    let listed = h.handle(
        &caller,
        Request::SessionHistory {
            query: None,
            limit: None,
            before: None,
        },
    );
    assert_eq!(error(listed).code, ErrorCode::PermissionDenied);
    let restored = h.handle(
        &caller,
        Request::SessionRestore {
            id: gone.id.clone(),
        },
    );
    assert_eq!(error(restored).code, ErrorCode::PermissionDenied);
    let denied: Vec<_> = h
        .audit()
        .into_iter()
        .filter(|row| row.decision == "denied")
        .map(|row| row.method)
        .collect();
    assert_eq!(denied, ["session.history", "session.restore"]);
    assert_eq!(names(&history(&h, None)), ["gone"]);
}

#[test]
fn a_deleted_session_is_never_revived_after_a_crash() {
    let mut h = Harness::new();
    let s = h.create(Some("docker"));
    delete(&h, &s.id);
    h.restart();
    std::fs::write(
        h.layout.home.join("running.json"),
        format!("{{\"sessions\": [\"{}\"]}}", s.id),
    )
    .unwrap();
    assert_eq!(h.control.revive_interrupted(), 0);
    assert!(h.list(true).is_empty());
    assert_eq!(names(&history(&h, None)), ["docker"]);
}

#[test]
fn sessions_leave_the_history_with_their_files_after_ninety_days() {
    let skip = Arc::new(AtomicI64::new(0));
    let clock = {
        let skip = skip.clone();
        Arc::new(move || now_ms() + skip.load(Ordering::SeqCst)) as ptyd::control::Clock
    };
    let h = Harness::with_clock(ptyd::detect::detect, clock);
    let old = h.create(Some("old"));
    save_output(&h, &old.id, b"old output\r\n");
    let history_file = h.layout.history_file(&old.id);
    std::fs::create_dir_all(history_file.parent().unwrap()).unwrap();
    std::fs::write(&history_file, "ls\n").unwrap();
    h.db()
        .execute(
            "INSERT INTO session_last_message (session_id, at) VALUES (?1, 1)",
            [old.id.as_str()],
        )
        .unwrap();
    delete(&h, &old.id);

    // A day later the tick purges, and keeps what's younger than 90 days.
    skip.store(89 * DAY_MS, Ordering::SeqCst);
    std::thread::sleep(Duration::from_millis(600));
    let recent = h.create(Some("recent"));
    delete(&h, &recent.id);
    assert_eq!(names(&history(&h, None)), ["recent", "old"]);

    skip.store(91 * DAY_MS, Ordering::SeqCst);
    // Its record goes first, then its files: wait for all of them.
    eventually("the old session purged with its files", || {
        let files_gone = ScrollbackStore::new(h.layout.clone())
            .load(&old.id)
            .unwrap()
            .is_none()
            && !history_file.exists();
        (names(&history(&h, None)) == ["recent"] && files_gone).then_some(())
    });
    assert_eq!(
        count(
            &h,
            "SELECT COUNT(*) FROM session_last_message WHERE session_id = ?1",
            &old.id
        ),
        0
    );
}
