//! Projects, pinning, multi-project and every-project (`all`) lists and
//! subscriptions, and the git context of sessions.

mod control_common;

use control_common::*;
use ptyd::control_api::{Caller, Control};
use ptyd::host::HostEvent;
use session_core::api::{
    Auth, ErrorCode, EventEnvelope, EventKind, Request, Response, SessionEvent, SubscribeRequest,
};
use session_core::ids::SessionId;
use session_core::model::{GitInfo, ProjectView, ScopeRoot, SessionStatus, SessionView};
use std::path::{Path, PathBuf};
use std::sync::mpsc::Receiver;
use std::time::Duration;

fn projects(h: &Harness, caller: &ptyd::control_api::Caller) -> Vec<ProjectView> {
    match h.handle(caller, Request::ProjectList) {
        Response::Projects { projects } => projects,
        other => panic!("expected projects, got {other:?}"),
    }
}

#[test]
fn relocating_a_moved_folder_preserves_live_sessions_and_updates_working_directories() {
    let h = Harness::new();
    let original = h.create(Some("original"));
    let other = h.create_in(&h.other_scope(), Some("other"));
    let nested_cwd = h.scope.path().join("sub");
    h.db()
        .execute(
            "UPDATE sessions_v2 SET cwd = ?2 WHERE id = ?1",
            [other.id.as_str(), nested_cwd.to_str().unwrap()],
        )
        .unwrap();
    let moved = h.root().join("renamed");
    std::fs::rename(h.scope.path(), &moved).unwrap();
    std::os::unix::fs::symlink(&moved, h.scope.path()).unwrap();
    let to = ScopeRoot::resolve(&moved).unwrap();
    assert_eq!(
        h.handle(
            &h.user,
            Request::ProjectRelocate {
                from: h.scope.clone(),
                to: to.clone()
            }
        ),
        Response::Ok
    );
    let relocated = h.get(&original.id);
    assert_eq!(relocated.scope, to);
    assert_eq!(relocated.cwd, moved);
    assert_eq!(relocated.terminal, original.terminal);
    assert_eq!(relocated.status, SessionStatus::Live);
    assert_eq!(h.get(&other.id).scope, h.other_scope());
    assert_eq!(h.get(&other.id).cwd, moved.join("sub"));
    let caller = h.session_caller(&original.id);
    assert!(
        matches!(caller.info, session_core::api::CallerInfo::Session { scope, .. } if scope == to)
    );
    let folders = projects(&h, &h.user);
    assert!(folders.iter().any(|folder| folder.path == to));
    assert!(!folders.iter().any(|folder| folder.path == h.scope));
    assert!(h
        .audit()
        .iter()
        .any(|entry| entry.method == "project.relocate" && entry.decision == "allowed"));
}

#[test]
fn relocation_is_user_only_and_cannot_replace_a_different_existing_folder() {
    let h = Harness::new();
    let session = h.create(None);
    let request = Request::ProjectRelocate {
        from: h.scope.clone(),
        to: h.other_scope(),
    };
    let caller = h.session_caller(&session.id);
    assert_eq!(
        error(h.handle(&caller, request.clone())).code,
        ErrorCode::PermissionDenied
    );
    assert_eq!(
        error(h.handle(&h.user, request)).code,
        ErrorCode::InvalidArgument
    );
    assert_eq!(h.get(&session.id).scope, h.scope);
    assert_eq!(h.get(&session.id).terminal, session.terminal);
}

fn add_project(h: &Harness, path: &Path) -> Response {
    h.handle(
        &h.user,
        Request::ProjectAdd {
            path: path.to_path_buf(),
        },
    )
}

fn project(response: Response) -> ProjectView {
    match response {
        Response::Project { project } => project,
        other => panic!("expected a project, got {other:?}"),
    }
}

fn list_in(h: &Harness, scope: Option<&ScopeRoot>, scopes: &[ScopeRoot]) -> Response {
    h.handle(
        &h.user,
        Request::SessionList {
            scope: scope.cloned(),
            scopes: scopes.to_vec(),
            include_dead: true,
            all: false,
        },
    )
}

/// `session_list` with `all`.
fn list_all(h: &Harness, caller: &Caller, include_dead: bool) -> Response {
    h.handle(
        caller,
        Request::SessionList {
            scope: None,
            scopes: Vec::new(),
            include_dead,
            all: true,
        },
    )
}

fn ids(sessions: &[SessionView]) -> Vec<SessionId> {
    sessions.iter().map(|s| s.id.clone()).collect()
}

fn sessions(response: Response) -> Vec<SessionView> {
    match response {
        Response::Sessions { sessions } => sessions,
        other => panic!("expected sessions, got {other:?}"),
    }
}

fn names(sessions: &[SessionView]) -> Vec<String> {
    let mut names: Vec<String> = sessions.iter().map(|s| s.name.clone()).collect();
    names.sort();
    names
}

fn drain(rx: &Receiver<EventEnvelope>) -> Vec<EventEnvelope> {
    let mut out = Vec::new();
    while let Ok(event) = rx.recv_timeout(Duration::from_millis(300)) {
        out.push(event);
    }
    out
}

/// The latest `Updated` view of `id` among `events`.
fn updated(events: &[EventEnvelope], id: &SessionId) -> Option<SessionView> {
    events.iter().rev().find_map(|e| match &e.event {
        SessionEvent::Updated { session } if &session.id == id => Some(session.clone()),
        _ => None,
    })
}

/// Make `dir` a main worktree on `branch`.
fn git_repo(dir: &Path, branch: &str) {
    std::fs::create_dir_all(dir.join(".git/refs/heads")).unwrap();
    set_head(&dir.join(".git"), branch);
}

fn set_head(git_dir: &Path, branch: &str) {
    std::fs::write(git_dir.join("HEAD"), format!("ref: refs/heads/{branch}\n")).unwrap();
}

fn branch(name: &str, repo_root: &Path, worktree: Option<&str>) -> Option<GitInfo> {
    Some(GitInfo {
        repo_root: repo_root.to_path_buf(),
        branch: Some(name.into()),
        detached_at: None,
        worktree: worktree.map(str::to_string),
    })
}

#[test]
fn projects_are_added_by_sessions_and_explicitly_and_removed_when_empty() {
    let h = Harness::new();
    let a = h.create(Some("a"));
    h.create(Some("b"));
    let elsewhere = h.create_in(&h.other_scope(), Some("elsewhere"));
    h.host.exit(&a.id, Some(0));
    h.wait_recorded_exit(&a.id);

    let listed = projects(&h, &h.user);
    let mine = listed.iter().find(|p| p.path == h.scope).unwrap();
    assert_eq!(mine.name, "project");
    assert_eq!((mine.sessions, mine.live), (2, 1));
    assert!(mine.last_active_at.is_some());
    assert_eq!(mine.git, None);
    let other = listed.iter().find(|p| p.path == h.other_scope()).unwrap();
    assert_eq!((other.sessions, other.live), (1, 1));
    assert_eq!(listed.len(), 2);

    // Adding: canonicalized, idempotent, no sessions yet.
    let extra = h.root().join("extra");
    std::fs::create_dir_all(&extra).unwrap();
    let added = project(add_project(&h, &h.root().join("extra/../extra")));
    assert_eq!(added.path.path(), extra);
    assert_eq!(added.name, "extra");
    assert_eq!((added.sessions, added.live), (0, 0));
    assert_eq!(added.last_active_at, None);
    let again = project(add_project(&h, &extra));
    assert_eq!(again.added_at, added.added_at);
    assert_eq!(projects(&h, &h.user).len(), 3);

    std::fs::write(h.root().join("file"), "x").unwrap();
    for bad in [
        PathBuf::from("relative/dir"),
        h.root().join("missing"),
        h.root().join("file"),
    ] {
        let err = error(add_project(&h, &bad));
        assert_eq!(err.code, ErrorCode::InvalidArgument, "{}", bad.display());
    }

    // Removing: refused while sessions exist, unknown paths are not found.
    let remove =
        |path: &ScopeRoot| h.handle(&h.user, Request::ProjectRemove { path: path.clone() });
    let err = error(remove(&h.other_scope()));
    assert_eq!(err.code, ErrorCode::InvalidArgument);
    assert!(err.message.contains("1 session"), "{}", err.message);
    assert_eq!(remove(&added.path), Response::Ok);
    assert_eq!(
        error(remove(&added.path)).code,
        ErrorCode::NotFound,
        "already removed"
    );
    h.handle(
        &h.user,
        Request::SessionDelete {
            id: elsewhere.id.clone(),
        },
    );
    assert_eq!(remove(&h.other_scope()), Response::Ok);
    let left: Vec<_> = projects(&h, &h.user).into_iter().map(|p| p.path).collect();
    assert_eq!(left, vec![h.scope.clone()]);

    // Creating a session in a forgotten folder brings its project back.
    h.create_in(&h.other_scope(), Some("back"));
    assert_eq!(projects(&h, &h.user).len(), 2);

    let audited: Vec<_> = h
        .audit()
        .into_iter()
        .filter(|row| row.method.starts_with("project."))
        .map(|row| (row.method, row.decision))
        .collect();
    assert!(audited.contains(&("project.add".into(), "allowed".into())));
    assert!(audited.contains(&("project.add".into(), "error".into())));
    assert!(audited.contains(&("project.remove".into(), "allowed".into())));
    assert!(audited.contains(&("project.remove".into(), "error".into())));
}

#[test]
fn touching_and_creating_mark_the_project_used() {
    let h = Harness::new();
    let extra = h.root().join("extra");
    std::fs::create_dir_all(&extra).unwrap();
    project(add_project(&h, &extra));
    std::thread::sleep(Duration::from_millis(5));
    let s = h.create_in(&h.other_scope(), Some("s"));
    // Most recently used first.
    let order: Vec<_> = projects(&h, &h.user).into_iter().map(|p| p.name).collect();
    assert_eq!(order, ["other", "extra"]);
    std::thread::sleep(Duration::from_millis(5));
    h.create(Some("t"));
    let order: Vec<_> = projects(&h, &h.user).into_iter().map(|p| p.name).collect();
    assert_eq!(order, ["project", "other", "extra"]);
    std::thread::sleep(Duration::from_millis(5));
    h.handle(&h.user, Request::SessionTouch { id: s.id.clone() });
    let order: Vec<_> = projects(&h, &h.user).into_iter().map(|p| p.name).collect();
    assert_eq!(order, ["other", "project", "extra"]);
}

#[test]
fn users_list_and_watch_several_projects() {
    let h = Harness::new();
    let third = ScopeRoot::from_canonical(h.root().join("third"));
    std::fs::create_dir_all(third.path()).unwrap();
    let watch = h
        .control
        .subscribe(
            &h.user,
            &SubscribeRequest {
                scope: Some(h.scope.clone()),
                scopes: vec![h.other_scope(), h.scope.clone()],
                ..Default::default()
            },
        )
        .unwrap();
    let a = h.create(Some("a"));
    let b = h.create_in(&h.other_scope(), Some("b"));
    let c = h.create_in(&third, Some("c"));

    let both = [h.scope.clone(), h.other_scope()];
    assert_eq!(names(&sessions(list_in(&h, None, &both))), ["a", "b"]);
    assert_eq!(
        names(&sessions(list_in(&h, Some(&h.scope), &[h.other_scope()]))),
        ["a", "b"]
    );
    assert_eq!(
        names(&sessions(list_in(
            &h,
            Some(&h.scope),
            std::slice::from_ref(&h.scope)
        ))),
        ["a"]
    );
    assert_eq!(
        names(&sessions(list_in(&h, None, std::slice::from_ref(&third)))),
        ["c"]
    );
    assert_eq!(
        error(list_in(&h, None, &[])).code,
        ErrorCode::InvalidArgument,
        "users must name a project"
    );

    let got: Vec<SessionId> = drain(&watch)
        .into_iter()
        .filter(|e| matches!(e.event, SessionEvent::Created { .. }))
        .map(|e| e.session_id)
        .collect();
    assert_eq!(got, vec![a.id.clone(), b.id.clone()]);

    // Session ids must belong to one of the watched projects.
    let request = |ids: Vec<SessionId>| SubscribeRequest {
        scopes: both.to_vec(),
        session_ids: ids,
        ..Default::default()
    };
    assert!(h
        .control
        .subscribe(&h.user, &request(vec![a.id.clone(), b.id.clone()]))
        .is_ok());
    let err = h
        .control
        .subscribe(&h.user, &request(vec![c.id.clone()]))
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::OutOfScope);
}

#[test]
fn session_callers_stay_in_their_own_project() {
    let h = Harness::new();
    let mine = h.create(Some("mine"));
    h.create_in(&h.other_scope(), Some("elsewhere"));
    let extra = h.root().join("extra");
    std::fs::create_dir_all(&extra).unwrap();
    let me = h.session_caller(&mine.id);

    let seen: Vec<_> = projects(&h, &me).into_iter().map(|p| p.path).collect();
    assert_eq!(seen, vec![h.scope.clone()]);

    let denied = [
        Request::ProjectAdd {
            path: extra.clone(),
        },
        Request::ProjectRemove {
            path: h.other_scope(),
        },
    ];
    for request in denied {
        let err = error(h.handle(&me, request.clone()));
        assert_eq!(err.code, ErrorCode::PermissionDenied, "{request:?}");
    }
    assert_eq!(projects(&h, &h.user).len(), 2, "nothing was added");

    // Naming its own project is fine; any other one is out of scope.
    let own = h.handle(
        &me,
        Request::SessionList {
            scope: None,
            scopes: vec![h.scope.clone()],
            include_dead: true,
            all: false,
        },
    );
    assert_eq!(names(&sessions(own)), ["mine"]);
    for (scope, scopes) in [
        (None, vec![h.scope.clone(), h.other_scope()]),
        (Some(h.scope.clone()), vec![h.other_scope()]),
    ] {
        let err = error(h.handle(
            &me,
            Request::SessionList {
                scope,
                scopes,
                include_dead: true,
                all: false,
            },
        ));
        assert_eq!(err.code, ErrorCode::OutOfScope);
    }
    let err = h
        .control
        .subscribe(
            &me,
            &SubscribeRequest {
                scopes: vec![h.other_scope()],
                ..Default::default()
            },
        )
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::OutOfScope);

    let denials: Vec<_> = h
        .audit()
        .into_iter()
        .filter(|row| row.decision == "denied")
        .map(|row| row.method)
        .collect();
    for method in ["project.add", "project.remove", "session.list", "subscribe"] {
        assert!(denials.iter().any(|m| m == method), "{method}: {denials:?}");
    }
}

#[test]
fn users_list_every_project() {
    let mut h = Harness::new();
    let third = ScopeRoot::from_canonical(h.root().join("third"));
    std::fs::create_dir_all(third.path()).unwrap();
    h.create_in(&third, Some("old"));
    h.restart();
    let a = h.create(Some("a"));
    h.create(Some("a2"));
    h.create_in(&h.other_scope(), Some("b"));
    h.host.exit(&a.id, Some(0));
    h.wait_recorded_exit(&a.id);

    let every = sessions(list_all(&h, &h.user, true));
    assert_eq!(names(&every), ["a", "a2", "b", "old"]);
    // In the order a list of those projects returns.
    let named = [h.scope.clone(), h.other_scope(), third.clone()];
    assert_eq!(ids(&every), ids(&sessions(list_in(&h, None, &named))));
    let status = |name: &str| every.iter().find(|s| s.name == name).unwrap().status;
    assert_eq!(status("old"), SessionStatus::Dead);
    assert_eq!(status("a"), SessionStatus::Exited);
    assert_eq!(status("b"), SessionStatus::Live);

    // Without dead sessions; exited ones stay.
    let alive = sessions(list_all(&h, &h.user, false));
    assert_eq!(names(&alive), ["a", "a2", "b"]);

    // `scope` and `scopes` are ignored with `all`.
    let ignored = sessions(h.handle(
        &h.user,
        Request::SessionList {
            scope: Some(h.scope.clone()),
            scopes: vec![third.clone()],
            include_dead: true,
            all: true,
        },
    ));
    assert_eq!(ids(&ignored), ids(&every));
    // Reads aren't audited.
    assert!(h.audit().iter().all(|row| row.method != "session.list"));
}

#[test]
fn users_watch_every_project_including_new_ones() {
    let h = Harness::new();
    let a = h.create(Some("a"));
    let watch = h
        .control
        .subscribe(
            &h.user,
            &SubscribeRequest {
                all: true,
                ..Default::default()
            },
        )
        .unwrap();
    // `scope` is ignored with `all`; kinds still filter.
    let creations = h
        .control
        .subscribe(
            &h.user,
            &SubscribeRequest {
                scope: Some(h.scope.clone()),
                kinds: vec![EventKind::Created],
                all: true,
                ..Default::default()
            },
        )
        .unwrap();
    h.host.exit(&a.id, Some(0));
    let b = h.create_in(&h.other_scope(), Some("b"));
    // A project that didn't exist when the subscriptions started.
    let later = ScopeRoot::from_canonical(h.root().join("later"));
    std::fs::create_dir_all(later.path()).unwrap();
    let c = h.create_in(&later, Some("c"));

    let events = drain(&watch);
    assert!(
        events.iter().any(|e| e.session_id == a.id
            && e.scope == h.scope
            && matches!(e.event, SessionEvent::Exited { .. })),
        "{events:?}"
    );
    let created: Vec<_> = events
        .iter()
        .filter(|e| matches!(e.event, SessionEvent::Created { .. }))
        .map(|e| (e.scope.clone(), e.session_id.clone()))
        .collect();
    assert_eq!(
        created,
        vec![
            (h.other_scope(), b.id.clone()),
            (later.clone(), c.id.clone())
        ]
    );
    let got: Vec<_> = drain(&creations)
        .into_iter()
        .map(|e| e.session_id)
        .collect();
    assert_eq!(got, vec![b.id.clone(), c.id.clone()]);

    // Session ids may belong to any project; unknown ones aren't found.
    let request = |ids: Vec<SessionId>| SubscribeRequest {
        session_ids: ids,
        all: true,
        ..Default::default()
    };
    let one = h
        .control
        .subscribe(&h.user, &request(vec![c.id.clone()]))
        .unwrap();
    for id in [&b.id, &c.id] {
        let pin = Request::SessionPin {
            id: id.clone(),
            pinned: true,
        };
        assert!(session(h.handle(&h.user, pin)).pinned);
    }
    let got: Vec<_> = drain(&one).into_iter().map(|e| e.session_id).collect();
    assert!(
        !got.is_empty() && got.iter().all(|id| *id == c.id),
        "{got:?}"
    );
    let err = h
        .control
        .subscribe(&h.user, &request(vec![SessionId::from_string("NOPE")]))
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::NotFound);
}

#[test]
fn session_callers_cannot_list_or_watch_every_project() {
    let h = Harness::new();
    let mine = h.create(Some("mine"));
    h.create_in(&h.other_scope(), Some("elsewhere"));
    let me = h.session_caller(&mine.id);
    // The API acting for a conversation `mine` bound is `mine` too.
    let bound = h.handle(
        &me,
        Request::SessionBind {
            agent: "opencode".into(),
            conversations: vec!["ses_1".into()],
        },
    );
    assert_eq!(bound, Response::Ok);
    let delegated = h
        .control
        .authenticate(&hello(Auth::Delegated {
            token: h.user_token.as_str().into(),
            agent: "opencode".into(),
            conversation: "ses_1".into(),
        }))
        .unwrap();
    assert_eq!(delegated.session_id(), Some(&mine.id));

    for caller in [&me, &delegated] {
        // Naming its own project doesn't make `all` acceptable.
        for scope in [None, Some(h.scope.clone())] {
            let err = error(h.handle(
                caller,
                Request::SessionList {
                    scope: scope.clone(),
                    scopes: Vec::new(),
                    include_dead: true,
                    all: true,
                },
            ));
            assert_eq!(err.code, ErrorCode::OutOfScope);
            let err = h
                .control
                .subscribe(
                    caller,
                    &SubscribeRequest {
                        scope,
                        all: true,
                        ..Default::default()
                    },
                )
                .unwrap_err();
            assert_eq!(err.code, ErrorCode::OutOfScope);
        }
    }
    // Their own project, as before.
    let own = h.handle(
        &me,
        Request::SessionList {
            scope: None,
            scopes: Vec::new(),
            include_dead: true,
            all: false,
        },
    );
    assert_eq!(names(&sessions(own)), ["mine"]);

    let label = format!("session:{}", mine.id);
    let scope = path_str(h.scope.path());
    let denials: Vec<_> = h
        .audit()
        .into_iter()
        .filter(|row| row.decision == "denied")
        .collect();
    for method in ["session.list", "subscribe"] {
        let rows: Vec<_> = denials.iter().filter(|row| row.method == method).collect();
        assert_eq!(rows.len(), 4, "{method}: {denials:?}");
        for row in rows {
            assert_eq!(row.caller, label);
            assert_eq!(row.scope.as_deref(), Some(scope.as_str()));
            assert!(row.detail.as_deref().unwrap().contains("own folder"));
        }
    }
}

#[test]
fn requests_without_all_behave_as_before() {
    let h = Harness::new();
    h.create(Some("a"));
    h.create_in(&h.other_scope(), Some("b"));
    let scope = serde_json::to_string(&h.scope).unwrap();
    // As an older client sends them: no `all` field.
    let request: Request =
        serde_json::from_str(&format!(r#"{{"op":"session_list","scope":{scope}}}"#)).unwrap();
    assert_eq!(names(&sessions(h.handle(&h.user, request))), ["a"]);
    let request: Request = serde_json::from_str(r#"{"op":"session_list"}"#).unwrap();
    assert_eq!(
        error(h.handle(&h.user, request)).code,
        ErrorCode::InvalidArgument,
        "users still name a project"
    );
    let watch: SubscribeRequest = serde_json::from_str(&format!(r#"{{"scope":{scope}}}"#)).unwrap();
    let watch = h.control.subscribe(&h.user, &watch).unwrap();
    h.create_in(&h.other_scope(), Some("c"));
    let d = h.create(Some("d"));
    let created: Vec<_> = drain(&watch)
        .into_iter()
        .filter(|e| matches!(e.event, SessionEvent::Created { .. }))
        .map(|e| e.session_id)
        .collect();
    assert_eq!(created, vec![d.id]);
}

#[test]
fn pinning_persists_and_publishes_an_update() {
    let mut h = Harness::new();
    let s = h.create(Some("s"));
    let other = h.create(Some("other"));
    let watch = h
        .control
        .subscribe(
            &h.user,
            &SubscribeRequest {
                scope: Some(h.scope.clone()),
                ..Default::default()
            },
        )
        .unwrap();
    let pin = |caller: &ptyd::control_api::Caller, id: &SessionId, pinned: bool| {
        h.handle(
            caller,
            Request::SessionPin {
                id: id.clone(),
                pinned,
            },
        )
    };
    let view = session(pin(&h.user, &s.id, true));
    assert!(view.pinned);
    assert!(updated(&drain(&watch), &s.id).unwrap().pinned);
    // Pinning again changes nothing, so nothing is published.
    session(pin(&h.user, &s.id, true));
    assert!(updated(&drain(&watch), &s.id).is_none());
    assert!(h.list(true).iter().find(|v| v.id == s.id).unwrap().pinned);

    // A session may pin itself, not others.
    let me = h.session_caller(&other.id);
    assert!(session(pin(&me, &other.id, true)).pinned);
    assert_eq!(
        error(pin(&me, &s.id, false)).code,
        ErrorCode::PermissionDenied
    );
    assert_eq!(
        error(pin(&h.user, &SessionId::from_string("NOPE"), true)).code,
        ErrorCode::NotFound
    );

    h.restart();
    let after = h.get(&s.id);
    assert_eq!(after.status, SessionStatus::Dead);
    assert!(after.pinned, "pins survive a daemon restart");
    let view = session(h.handle(
        &h.user,
        Request::SessionPin {
            id: s.id.clone(),
            pinned: false,
        },
    ));
    assert!(!view.pinned);
    let methods: Vec<_> = h
        .audit()
        .into_iter()
        .filter(|row| row.method == "session.pin")
        .map(|row| (row.decision, row.detail))
        .collect();
    assert!(methods.contains(&("allowed".into(), Some("pin".into()))));
    assert!(methods.contains(&("allowed".into(), Some("unpin".into()))));
    assert!(methods.iter().any(|(decision, _)| decision == "denied"));
}

#[test]
fn git_context_follows_the_cwd_and_head() {
    let mut h = Harness::new();
    let repo = h.scope.path().to_path_buf();
    git_repo(&repo, "main");
    let s = h.create(Some("s"));
    assert_eq!(s.git, branch("main", &repo, None));
    let watch = h
        .control
        .subscribe(
            &h.user,
            &SubscribeRequest {
                scope: Some(h.scope.clone()),
                ..Default::default()
            },
        )
        .unwrap();

    // A checkout changes HEAD without a cwd change: the poll notices.
    set_head(&repo.join(".git"), "feature");
    let view = eventually("an update with the new branch", || {
        watch
            .recv_timeout(Duration::from_millis(100))
            .ok()
            .and_then(|e| updated(&[e], &s.id))
            .filter(|v| v.git == branch("feature", &repo, None))
    });
    assert_eq!(view.cwd, repo);

    // The cwd moves into a linked worktree.
    let admin = repo.join(".git/worktrees/review");
    std::fs::create_dir_all(&admin).unwrap();
    set_head(&admin, "review-fix");
    std::fs::write(admin.join("commondir"), "../..\n").unwrap();
    let checkout = h.root().join("review");
    std::fs::create_dir_all(checkout.join("src")).unwrap();
    std::fs::write(
        checkout.join(".git"),
        format!("gitdir: {}\n", admin.display()),
    )
    .unwrap();
    let cwd = |path: PathBuf| {
        h.host.emit(HostEvent::Cwd {
            id: h.host.terminal_of(&s.id),
            session_id: s.id.clone(),
            path,
        })
    };
    cwd(checkout.join("src"));
    let view = eventually("an update in the worktree", || {
        updated(&drain(&watch), &s.id)
    });
    assert_eq!(view.cwd, checkout.join("src"));
    assert_eq!(view.git, branch("review-fix", &checkout, Some("review")));

    // Out of any repository (the temp dir is not inside one).
    if h.root().ancestors().all(|a| !a.join(".git").exists()) {
        cwd(h.root().join("other"));
        let view = eventually("an update outside git", || updated(&drain(&watch), &s.id));
        assert_eq!(view.git, None);
    }

    // The project reports the git context of its own directory.
    let mine = projects(&h, &h.user)
        .into_iter()
        .find(|p| p.path == h.scope)
        .unwrap();
    assert_eq!(mine.git, branch("feature", &repo, None));

    // After a restart dead sessions use their stored cwd.
    cwd(checkout.clone());
    eventually("the cwd is stored", || {
        (h.get(&s.id).cwd == checkout).then_some(())
    });
    h.restart();
    let dead = h.get(&s.id);
    assert_eq!(dead.status, SessionStatus::Dead);
    assert_eq!(dead.git, branch("review-fix", &checkout, Some("review")));
    let listed = h.list(true);
    assert_eq!(listed[0].git, dead.git);
}

#[test]
fn agent_working_directory_follows_internal_worktree_switches_without_changing_scope_or_terminal_cwd(
) {
    use ptyd::host::ProcessInfo;
    use session_core::api::{AgentWorkingDirectory, Report};
    for agent in ["claude", "codex", "opencode"] {
        let h = Harness::new();
        let root = h.scope.path().to_path_buf();
        git_repo(&root, "main");
        let checkout = h.root().join("active-worktree");
        let admin = root.join(".git/worktrees/active");
        std::fs::create_dir_all(&admin).unwrap();
        set_head(&admin, "agent-feature");
        std::fs::write(admin.join("commondir"), "../..\n").unwrap();
        std::fs::create_dir_all(&checkout).unwrap();
        std::fs::write(
            checkout.join(".git"),
            format!("gitdir: {}\n", admin.display()),
        )
        .unwrap();
        let started = h.create(Some("agent"));
        h.host.set_foreground(
            &started.id,
            ProcessInfo {
                pid: 4242,
                name: agent.into(),
                cmdline: vec![agent.into()],
            },
        );
        let caller = h.session_caller(&started.id);
        assert_eq!(
            h.handle(
                &caller,
                Request::SessionReport(Report {
                    working_directory: Some(AgentWorkingDirectory {
                        agent: agent.into(),
                        conversation_id: Some("conversation".into()),
                        path: checkout.clone()
                    }),
                    ..Report::default()
                })
            ),
            Response::Ok
        );
        let active = h.get(&started.id);
        assert_eq!(
            active.git,
            branch("agent-feature", &checkout, Some("active")),
            "{agent}"
        );
        assert_eq!(active.scope, h.scope);
        let outside = h.handle(
            &caller,
            Request::SessionList {
                scope: Some(ScopeRoot::resolve(&checkout).unwrap()),
                scopes: Vec::new(),
                include_dead: true,
                all: false,
            },
        );
        assert!(
            matches!(outside, Response::Error { error } if error.code == ErrorCode::OutOfScope),
            "a logical working directory must not change the agent's authenticated folder"
        );
        assert_eq!(
            active.cwd, root,
            "the agent's logical worktree is not the terminal cwd"
        );
        assert_eq!(
            h.list(true)[0].git,
            active.git,
            "listing must not reset the git cache to the launch directory"
        );
        let watch = h
            .control
            .subscribe(
                &h.user,
                &SubscribeRequest {
                    scope: Some(h.scope.clone()),
                    ..Default::default()
                },
            )
            .unwrap();
        set_head(&admin, "renamed-agent-branch");
        let update = eventually("agent worktree HEAD change without another report", || {
            updated(&drain(&watch), &started.id).filter(|view| {
                view.git.as_ref().and_then(|git| git.branch.as_deref())
                    == Some("renamed-agent-branch")
            })
        });
        assert_eq!(
            update.git,
            branch("renamed-agent-branch", &checkout, Some("active"))
        );
        assert_eq!(
            h.get(&started.id).git,
            update.git,
            "last working context is retained while the agent waits"
        );
        h.host.set_foreground(
            &started.id,
            ProcessInfo {
                pid: 5000,
                name: "bash".into(),
                cmdline: vec!["bash".into(), "-l".into()],
            },
        );
        assert_eq!(
            h.get(&started.id).git,
            branch("main", &root, None),
            "a non-agent uses its terminal cwd"
        );
        h.host.set_foreground(
            &started.id,
            ProcessInfo {
                pid: 6000,
                name: agent.into(),
                cmdline: vec![agent.into()],
            },
        );
        assert_eq!(
            h.get(&started.id).git,
            branch("main", &root, None),
            "a new agent process cannot inherit an old process's worktree"
        );
    }
}

#[test]
fn agent_working_directory_survives_handoff_and_rejects_invalid_paths() {
    use ptyd::control::{ControlConfig, ControlService, Limits};
    use ptyd::host::ProcessInfo;
    use session_core::api::{AgentWorkingDirectory, Report};
    let mut h = Harness::new();
    let repo = h.scope.path().to_path_buf();
    git_repo(&repo, "main");
    let checkout = h.root().join("agent-work");
    git_repo(&checkout, "agent-branch");
    let started = h.create(Some("agent"));
    h.host.set_foreground(
        &started.id,
        ProcessInfo {
            pid: 4242,
            name: "codex".into(),
            cmdline: vec!["codex".into()],
        },
    );
    let caller = h.session_caller(&started.id);
    for invalid in [
        PathBuf::from("relative"),
        h.root().join("missing"),
        PathBuf::from(""),
    ] {
        let response = h.handle(
            &caller,
            Request::SessionReport(Report {
                working_directory: Some(AgentWorkingDirectory {
                    agent: "codex".into(),
                    conversation_id: Some("thread".into()),
                    path: invalid,
                }),
                ..Report::default()
            }),
        );
        assert!(
            matches!(response, Response::Error { error } if error.code == ErrorCode::InvalidArgument)
        );
    }
    assert_eq!(
        h.handle(
            &caller,
            Request::SessionReport(Report {
                working_directory: Some(AgentWorkingDirectory {
                    agent: "codex".into(),
                    conversation_id: Some("thread".into()),
                    path: checkout.clone(),
                }),
                ..Report::default()
            })
        ),
        Response::Ok
    );
    let handed = h.control.handoff();
    assert_eq!(handed.working_directories.len(), 1);
    h.control = ControlService::open_after_handoff(
        ControlConfig {
            layout: h.layout.clone(),
            user_token: h.user_token.clone(),
            socket: h.layout.socket_path(),
            rakumux_bin: None,
            version: "test".into(),
            limits: Limits::default(),
        },
        h.host.clone(),
        handed,
    )
    .unwrap();
    let restored = h.get(&started.id);
    assert_eq!(restored.git, branch("agent-branch", &checkout, None));
    assert_eq!(restored.cwd, repo);
    assert_eq!(restored.scope, h.scope);
}

#[test]
fn places_follow_where_a_session_works_and_outlive_its_terminal() {
    use ptyd::host::ProcessInfo;
    use session_core::api::{AgentWorkingDirectory, Report};
    let mut h = Harness::new();
    let root = h.scope.path().to_path_buf();
    git_repo(&root, "main");
    let other = h.root().join("other");
    git_repo(&other, "fix");
    std::fs::create_dir_all(other.join("src")).unwrap();
    let checkout = h.root().join("checkout");
    let admin = root.join(".git/worktrees/review");
    std::fs::create_dir_all(&admin).unwrap();
    set_head(&admin, "review-branch");
    std::fs::write(admin.join("commondir"), "../..\n").unwrap();
    std::fs::create_dir_all(&checkout).unwrap();
    std::fs::write(
        checkout.join(".git"),
        format!("gitdir: {}\n", admin.display()),
    )
    .unwrap();
    let places = |view: &SessionView| {
        let mut places: Vec<(PathBuf, Option<String>, Option<String>)> = view
            .places
            .iter()
            .map(|place| {
                let git = place.git.as_ref();
                (
                    place.path.clone(),
                    git.and_then(|git| git.branch.clone()),
                    git.and_then(|git| git.worktree.clone()),
                )
            })
            .collect();
        places.sort();
        places
    };

    // Started in its folder.
    let s = h.create(Some("agent"));
    assert_eq!(places(&s), [(root.clone(), Some("main".into()), None)]);

    // The terminal goes into another repository: its working tree, published.
    let watch = h
        .control
        .subscribe(
            &h.user,
            &SubscribeRequest {
                scope: Some(h.scope.clone()),
                ..Default::default()
            },
        )
        .unwrap();
    h.host.emit(HostEvent::Cwd {
        id: h.host.terminal_of(&s.id),
        session_id: s.id.clone(),
        path: other.join("src"),
    });
    let view = eventually("an update with the other repository", || {
        updated(&drain(&watch), &s.id).filter(|view| view.places.len() == 2)
    });
    assert_eq!(
        places(&view),
        [
            (other.clone(), Some("fix".into()), None),
            (root.clone(), Some("main".into()), None)
        ]
    );

    // The agent reports a linked worktree it works in.
    h.host.set_foreground(
        &s.id,
        ProcessInfo {
            pid: 4242,
            name: "claude".into(),
            cmdline: vec!["claude".into()],
        },
    );
    let caller = h.session_caller(&s.id);
    assert_eq!(
        h.handle(
            &caller,
            Request::SessionReport(Report {
                working_directory: Some(AgentWorkingDirectory {
                    agent: "claude".into(),
                    conversation_id: Some("conversation".into()),
                    path: checkout.clone(),
                }),
                ..Report::default()
            })
        ),
        Response::Ok
    );
    let expected = [
        (
            checkout.clone(),
            Some("review-branch".into()),
            Some("review".into()),
        ),
        (other.clone(), Some("fix".into()), None),
        (root.clone(), Some("main".into()), None),
    ];
    assert_eq!(places(&h.get(&s.id)), expected);

    // A dead session keeps them; deleting it removes them.
    h.restart();
    let dead = h.get(&s.id);
    assert_eq!(dead.status, SessionStatus::Dead);
    assert_eq!(places(&dead), expected);
    assert_eq!(
        h.handle(&h.user, Request::SessionDelete { id: s.id.clone() }),
        Response::Ok
    );
    let left: i64 = h
        .db()
        .query_row("SELECT COUNT(*) FROM session_places", [], |row| row.get(0))
        .unwrap();
    assert_eq!(left, 0);
}
