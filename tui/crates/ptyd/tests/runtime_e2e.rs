//! End to end through the real binary: `rakumux-ptyd serve` with the real
//! control layer and terminal host in a temp `RAKUMUX_HOME`, driven over
//! its socket.
//!
//! Needs the real `control::ControlService` (work package A2); on a branch
//! where it is still a stub these tests are ignored.

mod runtime_support;

use runtime_support::*;
use session_core::api::{
    Auth, CreateSession, Hello, ReadRequest, ReadSource, Request, Response, Role, TextFormat,
};
use session_core::model::{ScopeRoot, SessionStatus, SessionView};
use session_core::paths::Registration;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

const EXE: &str = env!("CARGO_BIN_EXE_rakumux-ptyd");

fn pane_client(home: &TestHome) -> session_core::client::Client {
    use session_core::client::{AuthMode, Client, ClientConfig};
    Client::connect(ClientConfig {
        layout: home.layout.clone(),
        client_name: "pane-test".into(),
        version: "test".into(),
        auth: AuthMode::User,
        spawn: false,
        ptyd_path: None,
    })
    .unwrap()
}

fn pane_request(home: &TestHome) -> session_core::api::OpenTerminal {
    let mut env = home.env();
    env.push(("SHELL".into(), "/bin/bash".into()));
    session_core::api::OpenTerminal {
        id: session_core::ids::PaneId::generate(),
        cwd: home.work.clone(),
        env,
        cols: 80,
        rows: 12,
    }
}

#[test]
fn terminal_pane_exit_cleans_up_without_creating_or_deleting_a_session() {
    use session_core::api::{ErrorCode, StreamCommand, StreamEvent, TerminalAttachRequest};
    use session_core::client::{AttachEvent, SessionApi};
    let home = TestHome::new();
    let (mut daemon, registration) = start_daemon(&home, "rakumux");
    let mut user = TestClient::connect(&registration.socket);
    user.hello(&registration.token);
    let agent = create(&mut user, &home, &[]);
    let before = session(&mut user, &agent.id).terminal.unwrap();
    let client = pane_client(&home);
    let mut request = pane_request(&home);
    request.env.extend([
        ("RAKUMUX_SESSION_ID".into(), "parent-id".into()),
        ("RAKUMUX_SESSION_TOKEN".into(), "parent-token".into()),
        ("RAKUMUX_SOCKET".into(), "/invalid/parent.sock".into()),
        ("CLAUDECODE".into(), "1".into()),
    ]);
    let pane = request.id.clone();
    let stream = client.open_terminal(request).unwrap();
    let pid = stream.attached.pid.unwrap();
    assert_eq!(
        stream.attached.cwd,
        Some(std::fs::canonicalize(&home.work).unwrap())
    );
    assert_eq!(client.status().unwrap().sessions, 1);
    assert_eq!(client.list_all(true).unwrap().len(), 1);
    assert!(
        matches!(client.attach_terminal(TerminalAttachRequest { id: pane.clone(), role: Role::Observer, cols: 80, rows: 12 }), Err(error) if error.code == ErrorCode::PermissionDenied)
    );
    assert!(
        pid_alive(pid),
        "a refused second attach must not close the owning pane"
    );
    let marker = home.work.join("pane-environment");
    let line = format!("printf '%s' \"${{RAKUMUX_SESSION_ID-unset}}:${{RAKUMUX_SESSION_TOKEN-unset}}:${{RAKUMUX_SOCKET-unset}}:${{CLAUDECODE-unset}}\" > '{}'\r", marker.display());
    stream.sender().input(line.into_bytes()).unwrap();
    assert!(wait_until(TIMEOUT, || std::fs::read_to_string(&marker)
        .is_ok_and(|text| text == "unset:unset:unset:unset")));

    // Resync is passive: a fresh checkpoint does not resize either terminal.
    stream.sender().command(StreamCommand::Demote).unwrap();
    stream.sender().command(StreamCommand::Checkpoint).unwrap();
    let mut checkpoint = false;
    assert!(wait_until(TIMEOUT, || {
        if let Some(AttachEvent::Event(StreamEvent::Resized { cols, rows, .. })) =
            stream.recv_timeout(Duration::from_millis(20))
        {
            assert_eq!((cols, rows), (80, 12));
            checkpoint = true;
        }
        checkpoint
    }));
    stream
        .sender()
        .command(StreamCommand::Promote { cols: 80, rows: 12 })
        .unwrap();
    stream.sender().input(b"exit 7\r".to_vec()).unwrap();
    assert!(wait_until(TIMEOUT, || matches!(
        stream.recv_timeout(Duration::from_millis(20)),
        Some(AttachEvent::Event(StreamEvent::Exited { code: Some(7) }))
    )));
    assert!(wait_until(TIMEOUT, || !pid_alive(pid)));
    assert!(
        matches!(client.attach_terminal(TerminalAttachRequest { id: pane, role: Role::Observer, cols: 80, rows: 12 }), Err(error) if error.code == ErrorCode::NotFound)
    );
    let after = session(&mut user, &agent.id);
    assert_eq!(after.status, SessionStatus::Live);
    let after = after.terminal.unwrap();
    assert_eq!(
        (after.pid, after.cols, after.rows),
        (before.pid, before.cols, before.rows)
    );
    assert_eq!(client.status().unwrap().sessions, 1);
    if let Ok(entries) = std::fs::read_dir(home.layout.home.join("sessions")) {
        assert!(entries
            .map(Result::unwrap)
            .all(|entry| entry.file_name() == agent.id.as_str()));
    }
    client.shutdown().unwrap();
    assert_eq!(wait_exit(&mut daemon), Some(0));
}

#[test]
fn terminal_pane_stream_close_ends_only_its_temporary_process() {
    use session_core::api::{ErrorCode, TerminalAttachRequest};
    use session_core::client::SessionApi;
    let home = TestHome::new();
    let (mut daemon, _) = start_daemon(&home, "rakumux");
    let client = pane_client(&home);
    let request = pane_request(&home);
    let id = request.id.clone();
    let stream = client.open_terminal(request).unwrap();
    let pid = stream.attached.pid.unwrap();
    stream.detach();
    drop(stream);
    assert!(wait_until(TIMEOUT, || !pid_alive(pid)));
    assert!(
        matches!(client.attach_terminal(TerminalAttachRequest { id, role: Role::Observer, cols: 80, rows: 12 }), Err(error) if error.code == ErrorCode::NotFound)
    );
    assert_eq!(client.status().unwrap().sessions, 0);
    assert!(!home.layout.home.join("sessions").exists());
    client.shutdown().unwrap();
    assert_eq!(wait_exit(&mut daemon), Some(0));
}

#[test]
fn terminal_pane_survives_restart_in_place_without_a_registry_record() {
    use session_core::api::{StreamCommand, TerminalAttachRequest};
    use session_core::client::{AttachEvent, SessionApi};
    let home = TestHome::new();
    let (mut daemon, registration) = start_daemon(&home, "rakumux");
    let client = pane_client(&home);
    let request = pane_request(&home);
    let id = request.id.clone();
    let stream = client.open_terminal(request).unwrap();
    let pid = stream.attached.pid;
    client
        .restart_daemon(Some(std::path::Path::new(EXE)))
        .unwrap();
    assert!(wait_until(TIMEOUT, || matches!(
        stream.recv_timeout(Duration::from_millis(20)),
        Some(AttachEvent::Closed(_))
    )));
    let mut user = reconnect(&registration.socket, &registration.token);
    let client = pane_client(&home);
    let restored = client
        .attach_terminal(TerminalAttachRequest {
            id,
            role: Role::Observer,
            cols: 80,
            rows: 12,
        })
        .unwrap();
    assert_eq!(restored.attached.pid, pid);
    assert_eq!(restored.attached.role, Role::Observer);
    assert_eq!(client.status().unwrap().pid, registration.pid);
    assert_eq!(client.status().unwrap().sessions, 0);
    assert!(client.list_all(true).unwrap().is_empty());
    let marker = home.work.join("pane-after-restart");
    restored
        .sender()
        .command(StreamCommand::Promote { cols: 80, rows: 12 })
        .unwrap();
    restored
        .sender()
        .input(format!("printf 'AFTER_%s' 42 > '{}'\r", marker.display()).into_bytes())
        .unwrap();
    assert!(wait_until(TIMEOUT, || std::fs::read_to_string(&marker)
        .is_ok_and(|text| text == "AFTER_42")));
    assert!(!home.layout.home.join("sessions").exists());
    assert_eq!(user.request(&Request::DaemonShutdown), Response::Ok);
    assert_eq!(wait_exit(&mut daemon), Some(0));
}

#[test]
fn terminal_pane_operations_refuse_session_callers_and_invalid_requests() {
    use session_core::api::{ErrorCode, TerminalAttachRequest};
    use session_core::client::{AuthMode, Client, SessionApi};
    let home = TestHome::new();
    let (mut daemon, registration) = start_daemon(&home, "rakumux");
    let mut user = TestClient::connect(&registration.socket);
    user.hello(&registration.token);
    let token_file = home.work.join("session-token");
    let session = create(
        &mut user,
        &home,
        &[&format!(
            "printf '%s' \"$RAKUMUX_SESSION_TOKEN\" > '{}'",
            token_file.display()
        )],
    );
    assert!(wait_until(TIMEOUT, || token_file.exists()));
    let client = pane_client(&home);
    let mut config = client.config().clone();
    config.auth = AuthMode::Session {
        id: session.id,
        token: std::fs::read_to_string(token_file).unwrap(),
    };
    let inside = Client::connect(config).unwrap();
    let request = pane_request(&home);
    let id = request.id.clone();
    assert!(
        matches!(inside.open_terminal(request.clone()), Err(error) if error.code == ErrorCode::PermissionDenied)
    );
    assert!(
        matches!(inside.attach_terminal(TerminalAttachRequest { id, role: Role::Observer, cols: 80, rows: 12 }), Err(error) if error.code == ErrorCode::PermissionDenied)
    );
    let mut invalid = request.clone();
    invalid.cwd = home.work.join("missing");
    assert!(
        matches!(client.open_terminal(invalid), Err(error) if error.code == ErrorCode::InvalidArgument)
    );
    let mut invalid = request.clone();
    invalid.env.push(("BAD=NAME".into(), "value".into()));
    assert!(
        matches!(client.open_terminal(invalid), Err(error) if error.code == ErrorCode::InvalidArgument)
    );
    let mut invalid = request;
    invalid.cols = u16::MAX;
    invalid.rows = u16::MAX;
    assert!(
        matches!(client.open_terminal(invalid), Err(error) if error.code == ErrorCode::InvalidArgument)
    );
    assert_eq!(client.status().unwrap().sessions, 1);
    client.shutdown().unwrap();
    assert_eq!(wait_exit(&mut daemon), Some(0));
}

/// Kills the daemon if a test fails before shutting it down.
struct Daemon(Child);

impl std::ops::Deref for Daemon {
    type Target = Child;
    fn deref(&self) -> &Child {
        &self.0
    }
}

impl std::ops::DerefMut for Daemon {
    fn deref_mut(&mut self) -> &mut Child {
        &mut self.0
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        if let Ok(None) = self.0.try_wait() {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}

fn start_daemon(home: &TestHome, rakumux_bin: &str) -> (Daemon, Registration) {
    start_daemon_with(home, rakumux_bin, &[])
}

fn start_daemon_with(
    home: &TestHome,
    rakumux_bin: &str,
    env: &[(&str, &std::path::Path)],
) -> (Daemon, Registration) {
    let child = Command::new(EXE)
        .arg("serve")
        .envs(env.iter().map(|(k, v)| (*k, *v)))
        .env(session_core::env::HOME, &home.layout.home)
        .env(session_core::env::BIN, rakumux_bin)
        .env("RAKUMUX_PTYD_LOG", "debug")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn rakumux-ptyd");
    let mut registration = None;
    assert!(
        wait_until(TIMEOUT, || {
            registration = Registration::load(&home.layout).ok();
            // A crashed daemon's registration stays until the next one claims.
            registration
                .as_ref()
                .is_some_and(|r| r.pid == child.id() && r.socket.exists())
        }),
        "daemon never registered"
    );
    (Daemon(child), registration.unwrap())
}

fn wait_exit(child: &mut Child) -> Option<i32> {
    let mut status = None;
    wait_until(TIMEOUT, || {
        status = child.try_wait().unwrap();
        status.is_some()
    });
    status.and_then(|s| s.code())
}

fn create(
    client: &mut TestClient,
    home: &TestHome,
    startup: &[&str],
) -> session_core::model::SessionView {
    let response = client.request(&Request::SessionCreate(CreateSession {
        scope: Some(ScopeRoot::resolve(&home.work).unwrap()),
        name: Some("e2e".into()),
        cwd: None,
        argv: Some(bash_argv()),
        startup_cmds: startup.iter().map(|s| s.to_string()).collect(),
        env: home.env(),
        cols: 80,
        rows: 24,
    }));
    match response {
        Response::Session { session } => session,
        other => panic!("create failed: {other:?}"),
    }
}

fn read(client: &mut TestClient, id: &session_core::ids::SessionId) -> String {
    match client.request(&Request::SessionRead(ReadRequest {
        id: id.clone(),
        source: ReadSource::RecentUnwrapped,
        lines: None,
        format: TextFormat::Text,
    })) {
        Response::Read(result) => result.text,
        other => panic!("read failed: {other:?}"),
    }
}

fn wait_read(client: &mut TestClient, id: &session_core::ids::SessionId, needle: &str) {
    let mut screen = String::new();
    let found = wait_until(TIMEOUT, || {
        screen = read(client, id);
        screen.contains(needle)
    });
    assert!(found, "never saw {needle:?}; screen:\n{screen}");
}

#[test]
fn serve_runs_sessions_with_the_real_control_layer() {
    let home = TestHome::new();
    let (mut daemon, registration) = start_daemon(&home, "/opt/rakumux/bin/rakumux");
    let mut client = TestClient::connect(&registration.socket);
    assert!(matches!(
        client.hello(&registration.token),
        Response::Hello(_)
    ));

    let session = create(&mut client, &home, &["echo E2E_$((1+2))"]);
    assert_eq!(session.status, SessionStatus::Live);
    wait_read(&mut client, &session.id, "E2E_3");

    // The control layer injects the session identity and daemon details.
    client.submit(
        &session.id,
        "echo \"ID=$RAKUMUX_SESSION_ID SOCK=$RAKUMUX_SOCKET BIN=$RAKUMUX_BIN TERM=$TERM\"",
    );
    let expected = format!(
        "ID={} SOCK={} BIN=/opt/rakumux/bin/rakumux TERM=xterm-256color",
        session.id,
        registration.socket.display()
    );
    wait_read(&mut client, &session.id, &expected);

    // Attach as controller through the real prepare_attach.
    let mut attached = TestClient::connect(&registration.socket);
    assert!(matches!(
        attached.hello(&registration.token),
        Response::Hello(_)
    ));
    let mut stream = attached.attach(&session.id, Role::Controller, 100, 30);
    stream.input(b"echo ATTACHED_$((2*2))\r");
    stream.wait_for_screen("ATTACHED_4");

    match client.request(&Request::DaemonStatus) {
        Response::Status(status) => {
            assert_eq!(status.sessions, 1);
            assert_eq!(status.live_terminals, 1);
            assert_eq!(status.pid, registration.pid);
        }
        other => panic!("{other:?}"),
    }

    // Delete: terminate, forget, remove saved files; nothing comes back.
    assert_eq!(
        client.request(&Request::SessionDelete {
            id: session.id.clone()
        }),
        Response::Ok
    );
    assert!(stream.wait_closed());
    let dir = home.layout.session_dir(&session.id);
    std::thread::sleep(Duration::from_millis(500));
    assert!(!dir.exists(), "{} recreated after delete", dir.display());

    assert_eq!(client.request(&Request::DaemonShutdown), Response::Ok);
    assert_eq!(wait_exit(&mut daemon), Some(0));
    assert!(!registration.socket.exists());
    assert!(!home.layout.registration_file().exists());
    let log = std::fs::read_to_string(home.layout.log_file()).unwrap();
    assert!(log.contains("rakumux-ptyd stopped"), "{log}");
}

#[test]
fn projects_pins_and_git_context_through_the_real_daemon() {
    let home = TestHome::new();
    let repo = home.work.join("repo");
    std::fs::create_dir_all(repo.join(".git/refs/heads")).unwrap();
    std::fs::write(repo.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
    let other = home.dir.path().join("other");
    std::fs::create_dir_all(&other).unwrap();
    let (mut daemon, registration) = start_daemon(&home, "rakumux");
    let mut client = TestClient::connect(&registration.socket);
    match client.hello(&registration.token) {
        Response::Hello(ok) => {
            for capability in ["projects", "session.pin", "session.git", "sessions.all"] {
                assert!(
                    ok.capabilities.iter().any(|c| c == capability),
                    "{capability}"
                );
            }
        }
        other => panic!("{other:?}"),
    }
    let session = create(&mut client, &home, &[]);
    wait_read(&mut client, &session.id, "$");
    let work = ScopeRoot::resolve(&home.work).unwrap();
    let other_scope = ScopeRoot::resolve(&other).unwrap();

    match client.request(&Request::ProjectAdd {
        path: other.clone(),
    }) {
        Response::Project { project } => {
            assert_eq!(project.path, other_scope);
            assert_eq!(project.sessions, 0);
        }
        other => panic!("{other:?}"),
    }
    let projects = match client.request(&Request::ProjectList) {
        Response::Projects { projects } => projects,
        other => panic!("{other:?}"),
    };
    let mine = projects.iter().find(|p| p.path == work).unwrap();
    assert_eq!((mine.sessions, mine.live), (1, 1));
    assert!(projects.iter().any(|p| p.path == other_scope));

    match client.request(&Request::SessionPin {
        id: session.id.clone(),
        pinned: true,
    }) {
        Response::Session { session } => assert!(session.pinned),
        other => panic!("{other:?}"),
    }
    match client.request(&Request::SessionList {
        scope: None,
        scopes: vec![work.clone(), other_scope.clone()],
        include_dead: false,
        all: false,
    }) {
        Response::Sessions { sessions } => {
            assert_eq!(sessions.len(), 1);
            assert!(sessions[0].pinned);
            assert_eq!(sessions[0].git, None);
        }
        other => panic!("{other:?}"),
    }
    match client.request(&Request::SessionList {
        scope: None,
        scopes: Vec::new(),
        include_dead: true,
        all: true,
    }) {
        Response::Sessions { sessions } => {
            assert_eq!(sessions.len(), 1);
            assert_eq!(sessions[0].id, session.id);
        }
        other => panic!("{other:?}"),
    }

    // The daemon follows the shell into the repository and reads its branch.
    client.submit(&session.id, "cd repo");
    let mut git = None;
    let found = wait_until(TIMEOUT, || {
        // Output makes the host look at the shell's cwd again.
        client.submit(&session.id, ":");
        std::thread::sleep(Duration::from_millis(250));
        git = match client.request(&Request::SessionGet {
            id: session.id.clone(),
        }) {
            Response::Session { session } => session.git,
            other => panic!("{other:?}"),
        };
        git.is_some()
    });
    assert!(found, "no git context after cd");
    let git = git.unwrap();
    assert_eq!(git.branch.as_deref(), Some("main"));
    assert_eq!(git.repo_root, std::fs::canonicalize(&repo).unwrap());

    assert_eq!(
        client.request(&Request::ProjectRemove { path: other_scope }),
        Response::Ok
    );
    assert_eq!(client.request(&Request::DaemonShutdown), Response::Ok);
    assert_eq!(wait_exit(&mut daemon), Some(0));
}

#[test]
fn sigterm_shuts_down_gracefully() {
    let home = TestHome::new();
    let (mut daemon, registration) = start_daemon(&home, "rakumux");
    let mut client = TestClient::connect(&registration.socket);
    assert!(matches!(
        client.hello(&registration.token),
        Response::Hello(_)
    ));
    let session = create(&mut client, &home, &[]);
    wait_read(&mut client, &session.id, "$");

    // SAFETY: signalling our own child.
    unsafe {
        libc::kill(daemon.id() as i32, libc::SIGTERM);
    }
    assert_eq!(wait_exit(&mut daemon), Some(0));
    assert!(!registration.socket.exists());
    assert!(!home.layout.registration_file().exists());
}

#[test]
fn exits_when_its_registration_file_is_removed() {
    let home = TestHome::new();
    let (mut daemon, _registration) = start_daemon(&home, "rakumux");
    std::fs::remove_file(home.layout.registration_file()).unwrap();
    let mut status = None;
    wait_until(Duration::from_secs(15), || {
        status = daemon.try_wait().unwrap();
        status.is_some()
    });
    assert_eq!(
        status.and_then(|s| s.code()),
        Some(0),
        "daemon kept running"
    );
}

fn session(client: &mut TestClient, id: &session_core::ids::SessionId) -> SessionView {
    match client.request(&Request::SessionGet { id: id.clone() }) {
        Response::Session { session } => session,
        other => panic!("get failed: {other:?}"),
    }
}

/// Connect and say hello once the restarted daemon answers.
fn reconnect(socket: &std::path::Path, token: &str) -> TestClient {
    let mut client = None;
    let answered = wait_until(TIMEOUT, || {
        let Ok(stream) = std::os::unix::net::UnixStream::connect(socket) else {
            return false;
        };
        stream.set_read_timeout(Some(TIMEOUT)).unwrap();
        let mut candidate = TestClient { stream };
        let hello = Request::Hello(Hello {
            max_blocking_ms: None,
            protocol: session_core::PROTOCOL_VERSION,
            client: "test".into(),
            version: "0".into(),
            auth: Auth::User {
                token: token.into(),
            },
        });
        if session_core::frame::write_control(&mut candidate.stream, &hello).is_err() {
            return false;
        }
        match session_core::frame::read_control::<_, Response>(&mut candidate.stream) {
            Ok(Response::Hello(_)) => {
                client = Some(candidate);
                true
            }
            _ => false,
        }
    });
    assert!(answered, "the restarted daemon never answered");
    client.unwrap()
}

#[test]
fn restart_in_place_keeps_sessions_running() {
    let home = TestHome::new();
    let (mut daemon, registration) = start_daemon(&home, "rakumux");
    let mut client = TestClient::connect(&registration.socket);
    match client.hello(&registration.token) {
        Response::Hello(ok) => assert!(ok
            .capabilities
            .iter()
            .any(|c| c == session_core::api::CAPABILITY_DAEMON_RESTART)),
        other => panic!("{other:?}"),
    }
    let token_file = home.work.join("token");
    let started = create(
        &mut client,
        &home,
        &[
            &format!("echo \"$RAKUMUX_SESSION_TOKEN\" > {}", token_file.display()),
            "seq 1 60; echo BEFORE_$((1+1))",
        ],
    );
    wait_read(&mut client, &started.id, "BEFORE_2");
    let pid = session(&mut client, &started.id).terminal.unwrap().pid;
    let observer = {
        let mut attach = TestClient::connect(&registration.socket);
        assert!(matches!(
            attach.hello(&registration.token),
            Response::Hello(_)
        ));
        attach.attach(&started.id, Role::Observer, 80, 24)
    };

    // A binary that can't take over is refused and nothing changes.
    match client.request(&Request::DaemonRestart {
        binary: Some("/bin/true".into()),
    }) {
        Response::Error { error } => {
            assert_eq!(error.code, session_core::api::ErrorCode::Unsupported)
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(
        session(&mut client, &started.id).status,
        SessionStatus::Live
    );

    assert_eq!(
        client.request(&Request::DaemonRestart {
            binary: Some(EXE.into()),
        }),
        Response::Ok
    );
    let mut observer = observer;
    assert!(observer.wait_closed(), "attach streams end on a restart");
    let mut client = reconnect(&registration.socket, &registration.token);

    // Same process, same session, same program, same screen and history.
    assert_eq!(
        daemon.try_wait().unwrap(),
        None,
        "the daemon process exited"
    );
    match client.request(&Request::DaemonStatus) {
        Response::Status(status) => {
            assert_eq!(status.pid, registration.pid);
            assert_eq!(status.live_terminals, 1);
        }
        other => panic!("{other:?}"),
    }
    let after = session(&mut client, &started.id);
    assert_eq!(after.status, SessionStatus::Live);
    assert_eq!(after.terminal.unwrap().pid, pid);
    let restored = read(&mut client, &started.id);
    assert!(restored.contains("BEFORE_2"), "{restored}");
    assert!(restored.contains("\n3\n"), "history lost:\n{restored}");
    assert_eq!(
        Registration::load(&home.layout).unwrap().instance_id,
        registration.instance_id
    );

    // Input and output work, and the program inside still authenticates.
    client.submit(&started.id, "echo AFTER_$((3+3))");
    wait_read(&mut client, &started.id, "AFTER_6");
    let token = std::fs::read_to_string(&token_file).unwrap();
    let mut inside = TestClient::connect(&registration.socket);
    let hello = inside.request(&Request::Hello(Hello {
        max_blocking_ms: None,
        protocol: session_core::PROTOCOL_VERSION,
        client: "agent".into(),
        version: "0".into(),
        auth: Auth::Session {
            id: started.id.clone(),
            token: token.trim().into(),
        },
    }));
    assert!(matches!(hello, Response::Hello(_)), "{hello:?}");

    // An attach works again, and the exit code still arrives.
    let mut attach = TestClient::connect(&registration.socket);
    assert!(matches!(
        attach.hello(&registration.token),
        Response::Hello(_)
    ));
    let mut stream = attach.attach(&started.id, Role::Controller, 80, 24);
    stream.wait_for_screen("AFTER_6");
    stream.input(b"exit 7\r");
    let exited = wait_until(TIMEOUT, || {
        session(&mut client, &started.id).status == SessionStatus::Exited
    });
    assert!(exited, "the exit never arrived");
    assert_eq!(session(&mut client, &started.id).exit_code, Some(7));

    assert_eq!(client.request(&Request::DaemonShutdown), Response::Ok);
    assert_eq!(wait_exit(&mut daemon), Some(0));
    let log = std::fs::read_to_string(home.layout.log_file()).unwrap();
    assert!(log.contains("restarted in place"), "{log}");
    assert!(log.contains("adopted 1 running terminal(s)"), "{log}");
}

/// The session's view once `check` holds.
fn wait_session(
    client: &mut TestClient,
    id: &session_core::ids::SessionId,
    what: &str,
    check: impl Fn(&SessionView) -> bool,
) -> SessionView {
    let mut last = None;
    let found = wait_until(Duration::from_secs(20), || {
        let view = session(client, id);
        let done = check(&view);
        last = Some(view);
        done
    });
    assert!(found, "never {what}: {last:?}");
    last.unwrap()
}

#[test]
fn sessions_come_back_after_a_crash_but_not_after_a_stop() {
    use session_core::model::ResumeSource;
    let home = TestHome::new();
    // A stand-in for Claude Code: it records its conversation the way
    // Claude Code does (`<config dir>/sessions/<pid>.json`) and waits.
    let bin = home.dir.path().join("bin");
    let claude_dir = home.dir.path().join("claude-config");
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::create_dir_all(claude_dir.join("sessions")).unwrap();
    let script = bin.join("claude");
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\necho \"{{\\\"pid\\\":$$,\\\"sessionId\\\":\\\"conv-123\\\"}}\" > {}/sessions/$$.json\necho CLAUDE_RUNNING \"$@\"\nwhile :; do sleep 1; done\n",
            claude_dir.display()
        ),
    )
    .unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    // Revived sessions get the daemon's environment: the stand-in comes
    // first on its PATH too, so no real `claude` ever starts.
    let path = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let path_value = std::path::PathBuf::from(&path);
    let daemon_env = [
        ("CLAUDE_CONFIG_DIR", claude_dir.as_path()),
        ("PATH", path_value.as_path()),
    ];
    let (mut daemon, registration) = start_daemon_with(&home, "rakumux", &daemon_env);
    let mut client = TestClient::connect(&registration.socket);
    assert!(matches!(
        client.hello(&registration.token),
        Response::Hello(_)
    ));
    let create_with_path = |client: &mut TestClient, name: &str| {
        let mut env = home.env();
        env.retain(|(k, _)| k != "PATH");
        env.push(("PATH".into(), path.clone()));
        match client.request(&Request::SessionCreate(CreateSession {
            scope: Some(ScopeRoot::resolve(&home.work).unwrap()),
            name: Some(name.into()),
            cwd: None,
            argv: Some(bash_argv()),
            startup_cmds: Vec::new(),
            env,
            cols: 100,
            rows: 24,
        })) {
            Response::Session { session } => session,
            other => panic!("create failed: {other:?}"),
        }
    };
    let agent = create_with_path(&mut client, "agent");
    let server = create_with_path(&mut client, "server");
    wait_read(&mut client, &agent.id, "$");
    wait_read(&mut client, &server.id, "$");
    client.submit(
        &agent.id,
        "claude --dangerously-skip-permissions --model opus 'fix the bug'",
    );
    client.submit(&server.id, "sleep 1000");
    wait_read(&mut client, &agent.id, "CLAUDE_RUNNING");
    let resumed = wait_session(&mut client, &agent.id, "resumable", |view| {
        view.resume
            .as_ref()
            .is_some_and(|r| r.source == ResumeSource::KnownAgent)
    })
    .resume
    .unwrap();
    assert_eq!(
        resumed.argv,
        [
            "claude",
            "--dangerously-skip-permissions",
            "--model",
            "opus",
            "--resume",
            "conv-123"
        ]
    );
    let last = wait_session(&mut client, &server.id, "remembered", |view| {
        view.resume
            .as_ref()
            .is_some_and(|r| r.source == ResumeSource::LastCommand)
    })
    .resume
    .unwrap();
    assert_eq!(last.argv, ["sleep", "1000"]);

    // A crash: the programs end with the daemon's PTYs.
    daemon.kill().unwrap();
    daemon.wait().unwrap();
    let (mut daemon, registration) = start_daemon_with(&home, "rakumux", &daemon_env);
    let mut client = TestClient::connect(&registration.socket);
    assert!(matches!(
        client.hello(&registration.token),
        Response::Hello(_)
    ));
    // Both come back: the agent in its conversation, the command typed at
    // the prompt but not run.
    wait_session(&mut client, &agent.id, "revived", |view| {
        view.status == SessionStatus::Live
    });
    wait_read(
        &mut client,
        &agent.id,
        "CLAUDE_RUNNING --dangerously-skip-permissions --model opus --resume conv-123",
    );
    wait_session(&mut client, &server.id, "revived", |view| {
        view.status == SessionStatus::Live
    });
    wait_read(&mut client, &server.id, "sleep 1000");
    std::thread::sleep(Duration::from_millis(700));
    let view = session(&mut client, &server.id);
    assert_eq!(
        view.terminal.unwrap().foreground_process.as_deref(),
        Some("bash"),
        "the last command ran"
    );

    // Stopping on purpose: nothing comes back.
    assert_eq!(client.request(&Request::DaemonShutdown), Response::Ok);
    assert_eq!(wait_exit(&mut daemon), Some(0));
    let (mut daemon, registration) = start_daemon_with(&home, "rakumux", &daemon_env);
    let mut client = TestClient::connect(&registration.socket);
    assert!(matches!(
        client.hello(&registration.token),
        Response::Hello(_)
    ));
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(session(&mut client, &agent.id).status, SessionStatus::Dead);
    assert_eq!(session(&mut client, &server.id).status, SessionStatus::Dead);
    assert_eq!(client.request(&Request::DaemonShutdown), Response::Ok);
    assert_eq!(wait_exit(&mut daemon), Some(0));
}
