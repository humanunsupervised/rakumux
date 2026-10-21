//! Session operations: create, list, rename, kill, delete, revive, report,
//! read, and registry behaviour across daemon restarts.

mod control_common;

use control_common::*;
use ptyd::control_api::Control;
use session_core::api::{
    AttachRequest, ErrorCode, ReadRequest, ReadSource, Report, Request, Response, ReviveSession,
    Role, TextFormat,
};
use session_core::ids::SessionId;
use session_core::model::{
    AgentInfo, Creator, Lifecycle, ReportResult, ResumeSource, ScopeRoot, SessionStatus,
};
use std::path::PathBuf;
use std::time::Duration;

fn revive(h: &Harness, id: &SessionId, resume: bool) -> Response {
    h.handle(
        &h.user,
        Request::SessionRevive(ReviveSession {
            id: id.clone(),
            env: vec![("PATH".into(), "/bin".into())],
            cols: 100,
            rows: 30,
            resume,
        }),
    )
}

fn wait_status(h: &Harness, id: &SessionId, status: SessionStatus) {
    eventually(&format!("{id} to become {status:?}"), || {
        (h.get(id).status == status).then_some(())
    });
}

#[test]
fn create_builds_argv_env_cwd_and_startup_input() {
    let h = Harness::new();
    let mut request = h.create_request();
    request.cwd = Some(PathBuf::from("sub"));
    request.startup_cmds = vec!["echo hi".into(), "ls -la".into()];
    request.env = vec![
        ("PATH".into(), "/client/bin".into()),
        ("SHELL".into(), "/bin/zsh".into()),
        ("TMUX".into(), "/tmp/tmux".into()),
        ("TMUX_PANE".into(), "%3".into()),
        ("CLAUDECODE".into(), "1".into()),
        ("CLAUDE_CODE_SESSION_ID".into(), "c".into()),
        ("CODEX_THREAD_ID".into(), "t".into()),
        ("RAKUMUX_SESSION_ID".into(), "PARENT".into()),
        ("RAKUMUX_SESSION_TOKEN".into(), "parent-token".into()),
        ("TERM".into(), "screen".into()),
    ];
    request.cols = 0;
    request.rows = 0;
    let view = session(h.handle(&h.user, Request::SessionCreate(request)));

    let spec = h.host.spec_of(&view.id);
    assert_eq!(spec.session_id, view.id);
    assert_eq!(spec.argv, ["/bin/zsh", "-l"]);
    assert_eq!(spec.cwd, h.scope.path().join("sub"));
    assert_eq!(spec.startup_input, b"echo hi\rls -la\r".to_vec());
    assert_eq!(
        (spec.cols, spec.rows),
        (120, 40),
        "0x0 means the default size"
    );
    let env = |name: &str| env_value(&spec.env, name);
    assert_eq!(env("PATH").as_deref(), Some("/client/bin"));
    assert_eq!(env("RAKUMUX_SESSION_ID").as_deref(), Some(view.id.as_str()));
    assert_eq!(env("RAKUMUX_SESSION_TOKEN").unwrap().len(), 64);
    assert_eq!(
        env("RAKUMUX_SOCKET").as_deref(),
        Some("/tmp/rakumux-test/ptyd.sock")
    );
    assert_eq!(env("RAKUMUX_BIN").as_deref(), Some(RAKUMUX_BIN));
    assert_eq!(env("TERM").as_deref(), Some("xterm-256color"));
    assert_eq!(env("COLORTERM").as_deref(), Some("truecolor"));
    for stripped in [
        "TMUX",
        "TMUX_PANE",
        "CLAUDECODE",
        "CLAUDE_CODE_SESSION_ID",
        "CODEX_THREAD_ID",
    ] {
        assert_eq!(env(stripped), None, "{stripped} must be stripped");
    }

    assert_eq!(view.status, SessionStatus::Live);
    assert_eq!(view.scope, h.scope);
    assert_eq!(view.cwd, h.scope.path().join("sub"));
    assert_eq!(view.created_by, Creator::User);
    assert_eq!(view.launch.argv, None, "login shell is recorded as None");
    assert_eq!(view.launch.startup_cmds.len(), 2);
    assert_eq!(view.name.split('-').count(), 2, "generated two-word name");
    assert_eq!(view.lifecycle, Lifecycle::None);
    assert_eq!(view.terminal.as_ref().unwrap().cols, 120);

    // An explicit command is spawned as given.
    let mut request = h.create_request();
    request.argv = Some(vec!["htop".into(), "-d".into(), "5".into()]);
    let view = session(h.handle(&h.user, Request::SessionCreate(request)));
    assert_eq!(h.host.spec_of(&view.id).argv, ["htop", "-d", "5"]);
    assert_eq!(view.launch.argv.unwrap(), ["htop", "-d", "5"]);
}

#[test]
fn create_validates_scope_cwd_argv_env_and_name() {
    let h = Harness::new();
    let code = |request| error(h.handle(&h.user, Request::SessionCreate(request))).code;

    let mut r = h.create_request();
    r.scope = None;
    assert_eq!(code(r), ErrorCode::InvalidArgument, "missing scope");

    let mut r = h.create_request();
    r.scope = Some(ScopeRoot::from_canonical(h.scope.path().join("../project")));
    assert_eq!(code(r), ErrorCode::InvalidArgument, "non-canonical scope");

    let mut r = h.create_request();
    r.scope = Some(ScopeRoot::from_canonical(h.root().join("missing")));
    assert_eq!(code(r), ErrorCode::InvalidArgument, "missing scope dir");

    let mut r = h.create_request();
    r.cwd = Some(PathBuf::from(".."));
    assert_eq!(code(r), ErrorCode::OutOfScope, "cwd escapes via ..");

    let mut r = h.create_request();
    r.cwd = Some(h.root().join("other"));
    assert_eq!(code(r), ErrorCode::OutOfScope, "absolute cwd outside");

    std::os::unix::fs::symlink(h.root().join("other"), h.scope.path().join("link")).unwrap();
    let mut r = h.create_request();
    r.cwd = Some(PathBuf::from("link"));
    assert_eq!(code(r), ErrorCode::OutOfScope, "symlink escaping the scope");

    let mut r = h.create_request();
    r.cwd = Some(PathBuf::from("nope"));
    assert_eq!(code(r), ErrorCode::InvalidArgument, "missing cwd");

    let mut r = h.create_request();
    r.argv = Some(vec![]);
    assert_eq!(code(r), ErrorCode::InvalidArgument, "empty argv");

    let mut r = h.create_request();
    r.argv = Some(vec!["".into()]);
    assert_eq!(code(r), ErrorCode::InvalidArgument, "empty program");

    let mut r = h.create_request();
    r.env.push(("A=B".into(), "1".into()));
    assert_eq!(code(r), ErrorCode::InvalidArgument, "bad env name");

    let mut r = h.create_request();
    r.name = Some("   ".into());
    assert_eq!(code(r), ErrorCode::InvalidArgument, "blank name");

    h.create(Some("taken"));
    let mut r = h.create_request();
    r.name = Some(" taken ".into());
    assert_eq!(code(r), ErrorCode::InvalidArgument, "duplicate name");

    h.host.fail_next_spawn("no such program");
    let mut r = h.create_request();
    r.name = Some("broken".into());
    let err = error(h.handle(&h.user, Request::SessionCreate(r)));
    assert_eq!(err.code, ErrorCode::InvalidArgument);
    assert!(err.message.contains("no such program"));
    assert_eq!(h.list(true).len(), 1, "a failed spawn leaves no record");
}

#[test]
fn list_orders_by_recency_and_hides_dead_unless_asked() {
    let mut h = Harness::new();
    let a = h.create(Some("a"));
    std::thread::sleep(Duration::from_millis(5));
    let b = h.create(Some("b"));
    std::thread::sleep(Duration::from_millis(5));
    let c = h.create(Some("c"));
    let names = |list: Vec<session_core::model::SessionView>| -> Vec<String> {
        list.into_iter().map(|s| s.name).collect()
    };
    assert_eq!(names(h.list(false)), ["c", "b", "a"]);

    std::thread::sleep(Duration::from_millis(5));
    assert_eq!(
        h.handle(&h.user, Request::SessionTouch { id: a.id.clone() }),
        Response::Ok
    );
    assert_eq!(names(h.list(false)), ["a", "c", "b"]);

    // Sessions of other scopes never show up.
    h.create_in(&h.other_scope(), Some("z"));
    assert_eq!(h.list(false).len(), 3);

    // Exited sessions stay listed; after a restart every session without an
    // exit is Dead and hidden unless include_dead.
    h.host.exit(&b.id, Some(1));
    h.wait_recorded_exit(&b.id);
    h.restart();
    assert_eq!(names(h.list(false)), ["b"]);
    let all = h.list(true);
    assert_eq!(names(all.clone()), ["a", "c", "b"]);
    let status: Vec<_> = all.iter().map(|s| (s.status, s.exit_code)).collect();
    assert_eq!(
        status,
        [
            (SessionStatus::Dead, None),
            (SessionStatus::Dead, None),
            (SessionStatus::Exited, Some(1))
        ]
    );
    assert!(all.iter().all(|s| s.terminal.is_none()));
    assert_eq!(h.control.session_count(), 4);
    let _ = c;
}

#[test]
fn rename_enforces_uniqueness_within_the_scope() {
    let h = Harness::new();
    let a = h.create(Some("alpha"));
    let b = h.create(Some("beta"));
    h.create_in(&h.other_scope(), Some("gamma"));
    let rename = |id: &SessionId, name: &str| {
        h.handle(
            &h.user,
            Request::SessionRename {
                id: id.clone(),
                name: name.into(),
            },
        )
    };

    let err = error(rename(&b.id, "alpha"));
    assert_eq!(err.code, ErrorCode::InvalidArgument);
    assert!(err.message.contains("already exists"));
    assert_eq!(session(rename(&b.id, "  gamma  ")).name, "gamma");
    assert_eq!(session(rename(&a.id, "alpha")).name, "alpha", "no-op");
    assert_eq!(error(rename(&a.id, "")).code, ErrorCode::InvalidArgument);
    assert_eq!(
        error(rename(&SessionId::from_string("NOPE"), "x")).code,
        ErrorCode::NotFound
    );
    assert_eq!(h.get(&b.id).name, "gamma");
}

#[test]
fn kill_tree_terminates_descendants_only() {
    let h = Harness::new();
    let parent = h.create(Some("parent"));
    let unrelated = h.create(Some("unrelated"));
    // Stage A has no API for session-created children: seed them as rows.
    let db = h.db();
    let scope = path_str(h.scope.path());
    let seed = |id: &str, name: &str, parent: &str, depth: i64| {
        db.execute(
            "INSERT INTO sessions_v2 (id, scope_root, name, cwd, launch_json, created_by_json, \
             parent_id, depth, created_at, last_active_at) \
             VALUES (?1, ?2, ?3, ?2, '{}', ?4, ?5, ?6, 1, 1)",
            rusqlite::params![
                id,
                scope,
                name,
                format!(r#"{{"kind":"session","id":"{parent}"}}"#),
                parent,
                depth
            ],
        )
        .unwrap();
    };
    seed("01CHILD0000000000000000000", "child", parent.id.as_str(), 1);
    seed(
        "01GRANDCHILD00000000000000",
        "grandchild",
        "01CHILD0000000000000000000",
        2,
    );
    let child = SessionId::from_string("01CHILD0000000000000000000");
    let grandchild = SessionId::from_string("01GRANDCHILD00000000000000");
    for id in [&child, &grandchild] {
        assert_eq!(h.get(id).status, SessionStatus::Dead);
        session(revive(&h, id, false));
    }
    let view = h.get(&parent.id);
    assert_eq!(view.children, vec![child.clone()]);
    assert_eq!(
        h.get(&grandchild).created_by,
        Creator::Session { id: child.clone() }
    );

    let killed = match h.handle(
        &h.user,
        Request::SessionKill {
            id: parent.id.clone(),
            tree: true,
        },
    ) {
        Response::Killed { killed } => killed,
        other => panic!("{other:?}"),
    };
    assert_eq!(
        killed,
        vec![parent.id.clone(), child.clone(), grandchild.clone()]
    );
    for id in [&parent.id, &child, &grandchild] {
        wait_status(&h, id, SessionStatus::Exited);
    }
    assert_eq!(h.get(&unrelated.id).status, SessionStatus::Live);
    assert!(h.get(&parent.id).terminal.is_none(), "records stay");

    let err = error(h.handle(
        &h.user,
        Request::SessionKill {
            id: parent.id.clone(),
            tree: false,
        },
    ));
    assert_eq!(err.code, ErrorCode::SessionNotLive);
}

#[test]
fn delete_terminates_and_removes_record_token_and_history() {
    let h = Harness::new();
    let s = h.create(Some("scratch"));
    let keep = h.create(Some("keep"));
    let terminal = h.host.terminal_of(&s.id);
    let history = h.layout.history_file(&s.id);
    std::fs::create_dir_all(history.parent().unwrap()).unwrap();
    std::fs::write(&history, "ls\n").unwrap();

    assert_eq!(
        h.handle(&h.user, Request::SessionDelete { id: s.id.clone() }),
        Response::Ok
    );
    let fake = h.host.terminal(terminal);
    assert!(fake.terminated && fake.forgotten);
    assert!(!history.exists());
    assert_eq!(
        error(h.handle(&h.user, Request::SessionGet { id: s.id.clone() })).code,
        ErrorCode::NotFound
    );
    let names: Vec<_> = h.list(true).into_iter().map(|s| s.name).collect();
    assert_eq!(names, ["keep"]);
    // The name is free again.
    h.create(Some("scratch"));
    assert_eq!(
        error(h.handle(&h.user, Request::SessionDelete { id: s.id.clone() })).code,
        ErrorCode::NotFound
    );
    let _ = keep;
}

#[test]
fn revive_restarts_with_the_same_id_and_optional_resume() {
    let mut h = Harness::new();
    let mut request = h.create_request();
    request.name = Some("agent".into());
    request.cwd = Some(PathBuf::from("sub"));
    request.startup_cmds = vec!["source .venv/bin/activate".into()];
    let s = session(h.handle(&h.user, Request::SessionCreate(request)));
    let me = h.session_caller(&s.id);
    assert_eq!(
        h.handle(
            &me,
            Request::SessionReport(Report {
                resume: Some(vec!["claude".into(), "--resume".into(), "abc 1".into()]),
                ..Default::default()
            }),
        ),
        Response::Ok
    );

    let err = error(revive(&h, &s.id, true));
    assert_eq!(
        err.code,
        ErrorCode::InvalidArgument,
        "live sessions can't be revived"
    );

    h.host.exit(&s.id, Some(0));
    wait_status(&h, &s.id, SessionStatus::Exited);
    let old_terminal = h.host.terminal_of(&s.id);
    let view = session(revive(&h, &s.id, true));
    assert_eq!(view.id, s.id);
    assert_eq!(view.status, SessionStatus::Live);
    assert_eq!(view.exit_code, None);
    let spec = h.host.spec_of(&s.id);
    assert_ne!(h.host.terminal_of(&s.id), old_terminal);
    assert!(h.host.terminal(old_terminal).forgotten);
    assert_eq!(spec.cwd, h.scope.path().join("sub"));
    // No SHELL in the revive snapshot: the daemon's login shell.
    assert_eq!(spec.argv.len(), 2);
    assert_eq!(spec.argv[1], "-l");
    assert_eq!(
        String::from_utf8(spec.startup_input.clone()).unwrap(),
        "source .venv/bin/activate\rclaude --resume 'abc 1'\r"
    );
    assert_eq!((spec.cols, spec.rows), (100, 30));

    // Without resume only the startup commands run; works after a restart.
    h.restart();
    assert_eq!(h.get(&s.id).status, SessionStatus::Dead);
    session(revive(&h, &s.id, false));
    assert_eq!(
        h.host.spec_of(&s.id).startup_input,
        b"source .venv/bin/activate\r".to_vec()
    );
    h.session_caller(&s.id);

    // A vanished working directory is an explicit error.
    h.host.exit(&s.id, None);
    wait_status(&h, &s.id, SessionStatus::Exited);
    std::fs::remove_dir(h.scope.path().join("sub")).unwrap();
    let err = error(revive(&h, &s.id, false));
    assert_eq!(err.code, ErrorCode::InvalidArgument);
    assert!(err.message.contains("no longer exists"));
}

#[test]
fn report_stores_state_result_agent_identity_and_resume() {
    let h = Harness::new();
    let s = h.create(Some("worker"));
    let me = h.session_caller(&s.id);
    let report = Report {
        working_directory: None,
        activity: None,
        state: Some(Lifecycle::Working),
        text: Some("refactoring the parser".into()),
        result: Some(ReportResult {
            summary: "3 findings".into(),
            files: vec!["src/parser.rs".into()],
        }),
        agent: Some(AgentInfo {
            kind: "claude".into(),
            agent_session_id: Some("c0ffee".into()),
            agent_session_path: None,
        }),
        resume: Some(vec!["claude".into(), "--resume".into(), "c0ffee".into()]),
    };
    assert_eq!(h.handle(&me, Request::SessionReport(report)), Response::Ok);
    let view = h.get(&s.id);
    let reported = view.reported.unwrap();
    assert_eq!(reported.state, Some(Lifecycle::Working));
    assert_eq!(reported.text.as_deref(), Some("refactoring the parser"));
    assert_eq!(reported.result.unwrap().files, ["src/parser.rs"]);
    assert_eq!(view.lifecycle, Lifecycle::Working, "a fresh report wins");
    let agent = view.agent.unwrap();
    assert_eq!(agent.kind, "claude");
    assert_eq!(agent.agent_session_id.as_deref(), Some("c0ffee"));
    let resume = view.resume.unwrap();
    assert_eq!(resume.source, ResumeSource::ReportedByAgent);
    assert_eq!(resume.argv, ["claude", "--resume", "c0ffee"]);

    // The agent's own session id is indexed for lookups.
    let (kind, id): (String, String) = h
        .db()
        .query_row(
            "SELECT agent_kind, agent_session_id FROM sessions_v2 WHERE id = ?1",
            [s.id.as_str()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!((kind.as_str(), id.as_str()), ("claude", "c0ffee"));

    // Identity-only reports keep the last semantic report.
    h.handle(
        &me,
        Request::SessionReport(Report {
            agent: Some(AgentInfo {
                kind: "claude".into(),
                agent_session_id: Some("d00d".into()),
                agent_session_path: None,
            }),
            ..Default::default()
        }),
    );
    let view = h.get(&s.id);
    assert_eq!(
        view.reported.unwrap().text.as_deref(),
        Some("refactoring the parser")
    );
    assert_eq!(
        view.agent.unwrap().agent_session_id.as_deref(),
        Some("d00d")
    );

    let invalid = [
        Report {
            state: Some(Lifecycle::Unknown),
            ..Default::default()
        },
        Report {
            text: Some("x".repeat(5000)),
            ..Default::default()
        },
        Report {
            resume: Some(vec![]),
            ..Default::default()
        },
    ];
    for report in invalid {
        let err = error(h.handle(&me, Request::SessionReport(report)));
        assert_eq!(err.code, ErrorCode::InvalidArgument);
    }
}

#[test]
fn read_uses_the_live_screen_then_saved_output() {
    let mut h = Harness::new();
    let s = h.create(Some("shell"));
    h.host.show(&s.id, "$ echo hi\nhi\n$");
    let read = |h: &Harness| match h.handle(
        &h.user,
        Request::SessionRead(ReadRequest {
            id: s.id.clone(),
            source: ReadSource::Visible,
            lines: None,
            format: TextFormat::Text,
        }),
    ) {
        Response::Read(result) => result,
        other => panic!("{other:?}"),
    };
    let live = read(&h);
    assert_eq!(live.text, "$ echo hi\nhi\n$");
    assert!(live.untrusted, "terminal output is always marked untrusted");

    h.restart();
    let dead = read(&h);
    assert!(dead.untrusted);
    assert_eq!(dead.rows, 0, "nothing saved (persist stub) reads as empty");
}

#[test]
fn attach_requires_a_running_terminal() {
    let h = Harness::new();
    let s = h.create(Some("shell"));
    let request = AttachRequest {
        id: s.id.clone(),
        role: Role::Observer,
        cols: 80,
        rows: 24,
    };
    let (terminal, view) = h.control.prepare_attach(&h.user, &request).unwrap();
    assert_eq!(terminal, h.host.terminal_of(&s.id));
    assert_eq!(view.id, s.id);
    h.host.exit(&s.id, Some(0));
    wait_status(&h, &s.id, SessionStatus::Exited);
    let err = h.control.prepare_attach(&h.user, &request).unwrap_err();
    assert_eq!(err.code, ErrorCode::SessionNotLive);
}

#[test]
fn legacy_sessions_table_is_dropped_on_first_open() {
    let dir = tempfile::tempdir().unwrap();
    let layout = session_core::paths::Layout::with_home(dir.path().join("home"));
    std::fs::create_dir_all(&layout.home).unwrap();
    {
        let conn = rusqlite::Connection::open(layout.registry_db()).unwrap();
        conn.execute_batch(
            "CREATE TABLE sessions (id INTEGER PRIMARY KEY AUTOINCREMENT, tmux_name TEXT NOT NULL \
             UNIQUE, repo_root TEXT NOT NULL); \
             INSERT INTO sessions (tmux_name, repo_root) VALUES ('repo-main', '/repo');",
        )
        .unwrap();
    }
    let token = session_core::ids::Token::generate();
    let control = open(&layout, &token, FakeHost::new(), ptyd::detect::detect);
    let conn = rusqlite::Connection::open(layout.registry_db()).unwrap();
    let tables: Vec<String> = conn
        .prepare("SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert!(!tables.contains(&"sessions".to_string()), "{tables:?}");
    assert!(tables.contains(&"sessions_v2".to_string()));
    assert!(tables.contains(&"audit_log".to_string()));
    assert!(tables.contains(&"projects".to_string()));
    let version: i64 = conn
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(version, 3);
    let journal: String = conn
        .query_row("PRAGMA journal_mode", [], |r| r.get(0))
        .unwrap();
    assert_eq!(journal, "wal");
    use std::os::unix::fs::PermissionsExt;
    let mode = std::fs::metadata(layout.registry_db())
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o600);
    assert_eq!(control.session_count(), 0);
}

#[test]
fn revive_replaces_an_agent_launch_command_with_its_resume_command() {
    let mut h = Harness::new();
    let mut request = h.create_request();
    request.argv = Some(vec!["/usr/local/bin/claude".into()]);
    let s = session(h.handle(&h.user, Request::SessionCreate(request)));
    let me = h.session_caller(&s.id);
    assert_eq!(
        h.handle(
            &me,
            Request::SessionReport(Report {
                resume: Some(vec!["claude".into(), "--resume".into(), "abc".into()]),
                ..Default::default()
            }),
        ),
        Response::Ok
    );
    h.host.exit(&s.id, Some(0));
    wait_status(&h, &s.id, SessionStatus::Exited);

    session(revive(&h, &s.id, true));
    let spec = h.host.spec_of(&s.id);
    // The agent itself was the launch command: a login shell runs the resume.
    assert_eq!(spec.argv.len(), 2);
    assert_eq!(spec.argv[1], "-l");
    assert_eq!(spec.startup_input, b"claude --resume abc\r".to_vec());

    // Without resume the original launch command runs again.
    h.host.exit(&s.id, Some(0));
    wait_status(&h, &s.id, SessionStatus::Exited);
    session(revive(&h, &s.id, false));
    assert_eq!(
        h.host.spec_of(&s.id).argv,
        vec!["/usr/local/bin/claude".to_string()]
    );
    let _ = &mut h;
}
