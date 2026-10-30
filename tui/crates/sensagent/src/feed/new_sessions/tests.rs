use super::*;
use crate::source::fake::live_view;

const NOW: Duration = Duration::from_secs(100);
const ASK: &str = "01K8A2B3C4D5E6F7G8H9J0K1M2";
const OTHER: &str = "01K8A2B3C4D5E6F7G8H9J0K1M3";
const STARTED: &str = "01K8A2B3C4D5E6F7G8H9J0K1M4";

fn view(id: &str, folder: &str) -> SessionView {
    live_view(&SessionId::from_string(id), folder, 1)
}

fn ask(id: &str, folder: &str, agent: Option<Agent>) -> NewSessionAsk {
    NewSessionAsk {
        id: id.into(),
        machine: "studio".into(),
        folder: folder.into(),
        folder_session: None,
        agent,
    }
}

/// Enabled, with `views` followed.
fn on(views: &[SessionView]) -> NewSessionSync {
    let mut sync = NewSessionSync::new("studio".into(), None);
    sync.set_enabled(true, NOW);
    sync.connected(views);
    sync
}

fn body(work: Option<NewSessionWork>) -> serde_json::Value {
    match work {
        Some(NewSessionWork::Report(FeedRequest::NewSessionStatus { body })) => {
            serde_json::from_slice(&body).unwrap()
        }
        other => panic!("expected a report, got {other:?}"),
    }
}

fn created(name: &str) -> SessionView {
    let mut view = view(STARTED, "/home/me/rakumux");
    view.name = name.into();
    view
}

#[test]
fn a_request_starts_a_login_shell_in_the_listed_folder_with_the_agent_typed_in() {
    let mut sync = on(&[view("01K89ZQ4D2F6H8J0K2M4N6P8R0", "/home/me/rakumux")]);
    sync.read(true, vec![ask(ASK, "rakumux", Some(Agent::ClaudeCode))]);
    let Some(NewSessionWork::Create { id, request }) = sync.next_work(NOW) else {
        panic!("expected a create");
    };
    assert_eq!(id, ASK);
    let folder = ScopeRoot::from_canonical("/home/me/rakumux".into());
    assert_eq!(request.scope.as_ref(), Some(&folder));
    assert_eq!(request.cwd.as_deref(), Some(folder.path()));
    // A login shell with a generated name; the agent's own command typed in.
    assert_eq!(request.argv, None);
    assert_eq!(request.name, None);
    assert_eq!(request.startup_cmds, vec!["claude".to_string()]);
    assert!(request.env.is_empty());
    assert_eq!((request.machine, request.prefill), (None, None));
    // One at a time: nothing else until the daemon answers.
    assert_eq!(sync.next_work(NOW), None);

    sync.created(ASK, Ok(created("brave-otter")));
    let report = body(sync.next_work(NOW));
    assert_eq!(
        report,
        serde_json::json!({
            "version": 1,
            "id": ASK,
            "machine": "studio",
            "status": "created",
            "session": {"id": STARTED, "name": "brave-otter"},
        })
    );
    // One report out at a time.
    assert_eq!(sync.next_work(NOW), None);
    sync.reported(true, Ok(()), NOW);
    assert_eq!(sync.next_work(NOW), None);
}

#[test]
fn each_agent_runs_its_own_command_and_none_leaves_the_shell_alone() {
    for (agent, command) in [
        (Some(Agent::Codex), Some("codex")),
        (Some(Agent::OpenCode), Some("opencode")),
        (None, None),
    ] {
        let mut sync = on(&[view("01K89ZQ4D2F6H8J0K2M4N6P8R0", "/home/me/web")]);
        sync.read(true, vec![ask(ASK, "web", agent)]);
        let Some(NewSessionWork::Create { request, .. }) = sync.next_work(NOW) else {
            panic!("expected a create");
        };
        let expected: Vec<String> = command.into_iter().map(Into::into).collect();
        assert_eq!(request.startup_cmds, expected);
        assert_eq!(request.argv, None);
    }
}

#[test]
fn only_folders_this_machine_lists_and_never_by_path() {
    let mut private = view("01K89ZQ4D2F6H8J0K2M4N6P8R1", "/home/me/secret");
    private.private = true;
    let mut elsewhere = view("01K89ZQ4D2F6H8J0K2M4N6P8R2", "/home/me/sprite");
    elsewhere.machine = Some(session_core::model::MachineRef {
        id: session_core::ids::MachineId::from_string("01K89ZQ4D2F6H8J0K2M4N6P8R9"),
        name: "sprite".into(),
        scope: "/home/sprite/sprite".into(),
    });
    let mut dead = view("01K89ZQ4D2F6H8J0K2M4N6P8R3", "/home/me/old");
    dead.status = SessionStatus::Dead;
    let mut sync = on(&[private, elsewhere, dead]);
    // Each id is decided once: a fresh one per folder.
    let folders = [
        ("secret", "01K8A2B3C4D5E6F7G8H9J0K1A1"),
        ("sprite", "01K8A2B3C4D5E6F7G8H9J0K1A2"),
        ("old", "01K8A2B3C4D5E6F7G8H9J0K1A3"),
        ("nowhere", "01K8A2B3C4D5E6F7G8H9J0K1A4"),
    ];
    for (folder, id) in folders {
        sync.read(true, vec![ask(id, folder, None)]);
        let report = body(sync.next_work(NOW));
        assert_eq!(report["failure"], "folder_unknown", "{folder}");
        sync.reported(true, Ok(()), NOW);
    }
}

#[test]
fn a_folder_is_matched_by_its_name_exactly_as_listed() {
    // Folder names keep their spaces: the session list sends them as they are.
    let mut sync = on(&[view("01K89ZQ4D2F6H8J0K2M4N6P8R0", "/home/me/ notes")]);
    let page = serde_json::json!({
        "version": 1, "replies": [], "latest_seq": 0, "more": false,
        "new_sessions": [{"id": ASK, "machine": "studio", "folder": " notes", "status": "pending",
                          "created_at": 1761220000000_i64, "status_at": 1761220000000_i64}],
    });
    let reply = FeedHttpReply {
        status: 200,
        retry_after: None,
        body: serde_json::to_vec(&page).unwrap(),
    };
    sync.read(true, wire::replies(&reply).unwrap().new_sessions);
    let Some(NewSessionWork::Create { request, .. }) = sync.next_work(NOW) else {
        panic!("expected a create");
    };
    assert_eq!(
        request.scope,
        Some(ScopeRoot::from_canonical("/home/me/ notes".into()))
    );
    // Without the space it's another name.
    sync.read(true, vec![ask(OTHER, "notes", None)]);
    sync.created(ASK, Err(ErrorCode::NotFound));
    let reports: Vec<_> = std::iter::from_fn(|| {
        let report = body(sync.next_work(NOW));
        sync.reported(true, Ok(()), NOW);
        Some(report)
    })
    .take(2)
    .collect();
    assert_eq!(reports[1]["failure"], "folder_unknown");
}

#[test]
fn two_folders_of_one_name_need_the_session_that_says_which() {
    let a = view("01K89ZQ4D2F6H8J0K2M4N6P8R0", "/home/me/work/api");
    let b = view("01K89ZQ4D2F6H8J0K2M4N6P8R1", "/home/me/play/api");
    let mut sync = on(&[a, b.clone()]);
    sync.read(true, vec![ask(ASK, "api", None)]);
    assert_eq!(body(sync.next_work(NOW))["failure"], "folder_unknown");
    sync.reported(true, Ok(()), NOW);

    let mut named = ask(OTHER, "api", None);
    named.folder_session = Some(b.id.clone());
    sync.read(true, vec![named]);
    let Some(NewSessionWork::Create { request, .. }) = sync.next_work(NOW) else {
        panic!("expected a create");
    };
    assert_eq!(request.scope, Some(b.scope));

    // A session named in another folder says nothing about this one.
    let mut sync = on(&[view("01K89ZQ4D2F6H8J0K2M4N6P8R0", "/home/me/web")]);
    let mut wrong = ask(ASK, "web", None);
    wrong.folder_session = Some(SessionId::from_string("01K89ZQ4D2F6H8J0K2M4N6P8R5"));
    sync.read(true, vec![wrong]);
    assert!(matches!(
        sync.next_work(NOW),
        Some(NewSessionWork::Create { .. })
    ));
}

#[test]
fn the_daemon_refusing_fails_refused_and_requests_go_one_at_a_time() {
    let mut sync = on(&[view("01K89ZQ4D2F6H8J0K2M4N6P8R0", "/home/me/web")]);
    sync.read(true, vec![ask(ASK, "web", None), ask(OTHER, "web", None)]);
    let Some(NewSessionWork::Create { id, .. }) = sync.next_work(NOW) else {
        panic!("expected a create");
    };
    assert_eq!(id, ASK);
    sync.created(ASK, Err(ErrorCode::NotFound));
    // The next starts while the first's report goes.
    let Some(NewSessionWork::Create { id, .. }) = sync.next_work(NOW) else {
        panic!("expected the second create");
    };
    assert_eq!(id, OTHER);
    let report = body(sync.next_work(NOW));
    assert_eq!(
        (report["id"].as_str(), report["failure"].as_str()),
        (Some(ASK), Some("refused"))
    );
}

#[test]
fn a_request_is_started_at_most_once_even_across_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("new-sessions.json");
    let views = [view("01K89ZQ4D2F6H8J0K2M4N6P8R0", "/home/me/web")];
    let mut sync = NewSessionSync::new("studio".into(), Some(path.clone()));
    sync.set_enabled(true, NOW);
    sync.connected(&views);
    sync.read(true, vec![ask(ASK, "web", None)]);
    assert!(matches!(
        sync.next_work(NOW),
        Some(NewSessionWork::Create { .. })
    ));
    // The service stops before the daemon answers: the request is still
    // pending on rakumux.com, and read again it's never started again (nor
    // reported: it expires there).
    drop(sync);
    let mut sync = NewSessionSync::new("studio".into(), Some(path.clone()));
    sync.set_enabled(true, NOW);
    sync.connected(&views);
    sync.read(true, vec![ask(ASK, "web", None)]);
    assert_eq!(sync.next_work(NOW), None);

    // A report not yet taken goes after a restart.
    sync.read(true, vec![ask(OTHER, "web", None)]);
    assert!(matches!(
        sync.next_work(NOW),
        Some(NewSessionWork::Create { .. })
    ));
    sync.created(OTHER, Ok(created("calm-heron")));
    drop(sync);
    let mut sync = NewSessionSync::new("studio".into(), Some(path.clone()));
    sync.set_enabled(true, NOW);
    sync.connected(&views);
    sync.read(true, vec![ask(OTHER, "web", None)]);
    let report = body(sync.next_work(NOW));
    assert_eq!(report["session"]["name"], "calm-heron");
    use std::os::unix::fs::PermissionsExt;
    let mode = std::fs::metadata(&path).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o600);
}

#[test]
fn reports_back_off_and_a_refused_one_is_dropped() {
    let mut sync = on(&[]);
    sync.read(true, vec![ask(ASK, "web", None), ask(OTHER, "web", None)]);
    assert!(matches!(
        sync.next_work(NOW),
        Some(NewSessionWork::Report(_))
    ));
    sync.reported(true, Err(FeedFailure::Unavailable(None)), NOW);
    assert_eq!(sync.next_work(NOW), None);
    let later = NOW + RETRY_MIN;
    assert_eq!(body(sync.next_work(later))["id"], ASK);
    // Refused for good (the request expired): the next one goes.
    sync.reported(true, Err(FeedFailure::Invalid), later);
    assert_eq!(body(sync.next_work(later))["id"], OTHER);
    // A report for an account no longer signed in stays for later.
    sync.reported(false, Ok(()), later);
    assert_eq!(body(sync.next_work(later))["id"], OTHER);
}

#[test]
fn only_this_machines_requests_and_only_while_enabled() {
    let mut sync = on(&[view("01K89ZQ4D2F6H8J0K2M4N6P8R0", "/home/me/web")]);
    let mut theirs = ask(ASK, "web", None);
    theirs.machine = "laptop".into();
    sync.read(true, vec![theirs]);
    assert_eq!(sync.next_work(NOW), None);
    // A read for an account no longer signed in brings nothing.
    sync.read(false, vec![ask(ASK, "web", None)]);
    assert_eq!(sync.next_work(NOW), None);
    // Off, nothing is started even when read.
    sync.set_enabled(false, NOW);
    sync.read(true, vec![ask(ASK, "web", None)]);
    assert_eq!(sync.next_work(NOW), None);
    // Renamed, requests to the old name aren't this machine's.
    sync.set_enabled(true, NOW);
    sync.rename("studio-6m8b".into());
    sync.read(true, vec![ask(ASK, "web", None)]);
    assert_eq!(sync.next_work(NOW), None);
}

#[test]
fn the_replies_read_brings_requests_within_the_contract_row_by_row() {
    let rows = serde_json::json!([
        {"id": ASK, "machine": "studio", "folder": "rakumux", "folder_session": "01K89ZQ4D2F6H8J0K2M4N6P8R0",
         "agent": "claude-code", "status": "pending", "created_at": 1761220000000_i64, "status_at": 1761220000000_i64},
        {"id": OTHER, "machine": "studio", "folder": "web", "status": "pending",
         "created_at": 1761220000000_i64, "status_at": 1761220000000_i64},
        // A path, an agent off the list, a command, not pending, an id that isn't one.
        {"id": STARTED, "machine": "studio", "folder": "/etc", "status": "pending", "created_at": 1, "status_at": 1},
        {"id": STARTED, "machine": "studio", "folder": "web", "agent": "bash", "status": "pending", "created_at": 1, "status_at": 1},
        {"id": STARTED, "machine": "studio", "folder": "..", "status": "pending", "created_at": 1, "status_at": 1},
        {"id": STARTED, "machine": "studio", "folder": "web", "status": "created", "created_at": 1, "status_at": 1},
        {"id": "x", "machine": "studio", "folder": "web", "status": "pending", "created_at": 1, "status_at": 1},
        {"id": STARTED, "machine": "studio", "folder": "we\u{1b}[2Jb", "status": "pending", "created_at": 1, "status_at": 1},
        "not a request"
    ]);
    let page = serde_json::json!({
        "version": 1, "replies": [], "latest_seq": 0, "more": false, "new_sessions": rows,
    });
    let reply = FeedHttpReply {
        status: 200,
        retry_after: None,
        body: serde_json::to_vec(&page).unwrap(),
    };
    let read = wire::replies(&reply).unwrap();
    assert_eq!(
        read.new_sessions,
        vec![
            NewSessionAsk {
                id: ASK.into(),
                machine: "studio".into(),
                folder: "rakumux".into(),
                folder_session: Some(SessionId::from_string("01K89ZQ4D2F6H8J0K2M4N6P8R0")),
                agent: Some(Agent::ClaudeCode),
            },
            ask(OTHER, "web", None),
        ]
    );
    // An older cloud's read has none.
    let older = FeedHttpReply {
        status: 200,
        retry_after: None,
        body: br#"{"version":1,"replies":[],"latest_seq":0,"more":false}"#.to_vec(),
    };
    assert!(wire::replies(&older).unwrap().new_sessions.is_empty());
}

#[test]
fn a_report_is_answered_with_the_stored_request() {
    let answer = |status: u16, body: &str| FeedHttpReply {
        status,
        retry_after: None,
        body: body.as_bytes().to_vec(),
    };
    let stored = format!(
        r#"{{"version":1,"new_session":{{"id":"{ASK}","machine":"studio","folder":"web","status":"failed","failure":"refused","created_at":1761220000000,"status_at":1761220000000}}}}"#
    );
    assert_eq!(reported(&answer(200, &stored)), Ok(()));
    assert_eq!(
        reported(&answer(200, r#"{"version":2,"new_session":{"id":"x"}}"#)),
        Err(FeedFailure::Invalid)
    );
    let missing = r#"{"error":{"code":"invalid_request","message":"Invalid request."}}"#;
    assert_eq!(reported(&answer(404, missing)), Err(FeedFailure::Invalid));
}

#[test]
fn the_session_list_says_this_machine_takes_new_sessions() {
    use super::super::sessions::SessionSync;
    let capabilities = |sessions: SessionSync| {
        let mut sessions = sessions;
        sessions.set_enabled(true, NOW);
        match sessions.next_request(NOW, 1_761_220_000_000) {
            Some(FeedRequest::Sessions { body }) => {
                serde_json::from_slice::<serde_json::Value>(&body).unwrap()["capabilities"].clone()
            }
            other => panic!("expected a session list, got {other:?}"),
        }
    };
    let both = SessionSync::new("studio".into(), true)
        .handling(NEW_SESSIONS)
        .handling(NEW_SESSIONS);
    assert_eq!(
        capabilities(both),
        serde_json::json!(["reply_attachments", "new_sessions"])
    );
    let alone = SessionSync::new("studio".into(), false).handling(NEW_SESSIONS);
    assert_eq!(capabilities(alone), serde_json::json!(["new_sessions"]));
}
