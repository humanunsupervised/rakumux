//! The socket server end to end: real socket in a temp `RAKUMUX_HOME`, the
//! real terminal host with `bash --norc --noprofile`, and a fake `Control`.

mod runtime_support;

use ptyd::host::{TerminalHost, TerminalState};
use runtime_support::*;
use session_core::api::{
    CallerInfo, ErrorCode, EventEnvelope, Request, Response, Role, SessionEvent, StreamCommand,
    StreamEvent, SubscribeRequest,
};
use session_core::model::ScopeRoot;
use session_core::paths::Registration;
use std::sync::atomic::Ordering;
use std::time::Duration;

#[test]
fn hello_authenticates_users_and_rejects_bad_tokens() {
    let daemon = Daemon::start();
    let mut client = TestClient::connect(&daemon.socket);
    match client.hello(&daemon.token) {
        Response::Hello(ok) => {
            assert_eq!(ok.protocol, session_core::PROTOCOL_VERSION);
            assert_eq!(ok.caller, CallerInfo::User);
            assert!(ok.capabilities.iter().any(|c| c == "attach"));
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(client.request(&Request::Ping), Response::Pong);

    let mut bad = TestClient::connect(&daemon.socket);
    match bad.hello("not-the-token") {
        Response::Error { error } => assert_eq!(error.code, ErrorCode::Unauthenticated),
        other => panic!("{other:?}"),
    }
    assert!(bad.is_closed());

    let mut eager = TestClient::connect(&daemon.socket);
    match eager.request(&Request::Ping) {
        Response::Error { error } => assert_eq!(error.code, ErrorCode::Unauthenticated),
        other => panic!("{other:?}"),
    }
    assert!(eager.is_closed());
}

#[test]
fn create_with_startup_command_then_submit_and_read() {
    let daemon = Daemon::start();
    let mut client = daemon.client();
    let session = client.create(80, 24, &["echo STARTUP_$((20+1))"]);
    client.wait_for_text(&session.id, "STARTUP_21");
    client.submit(&session.id, "echo hi");
    let screen = client.wait_for_text(&session.id, "\nhi\n");
    assert!(screen.contains("$ echo hi"), "{screen}");
}

#[test]
fn controller_and_observer_with_promotion() {
    let daemon = Daemon::start();
    let mut client = daemon.client();
    let session = client.create(80, 24, &[]);
    client.wait_for_text(&session.id, "$");
    let tid = daemon.control.terminal(&session.id).unwrap();

    let mut a = daemon
        .client()
        .attach(&session.id, Role::Controller, 80, 24);
    let mut b = daemon.client().attach(&session.id, Role::Observer, 100, 30);
    assert_eq!(a.attached.role, Role::Controller);
    assert_eq!(b.attached.role, Role::Observer);
    // Observers render at the controller's size.
    assert_eq!((b.attached.cols, b.attached.rows), (80, 24));
    assert!(daemon.host.info(tid).unwrap().controlled);

    // Observer input is dropped; controller input reaches the shell.
    b.input(b"echo FROM_B_$((1+1))\r");
    a.input(b"echo FROM_A_$((1+1))\r");
    a.wait_for_screen("FROM_A_2");
    b.wait_for_screen("FROM_A_2");
    assert!(!a.mirror.text().contains("FROM_B"), "{}", a.mirror.text());

    // The observer takes over at its own size.
    b.command(&StreamCommand::Promote {
        cols: 100,
        rows: 30,
    });
    b.wait_for_event("b promoted", |e| {
        *e == StreamEvent::RoleChanged {
            role: Role::Controller,
        }
    });
    a.wait_for_event("a demoted", |e| {
        *e == StreamEvent::RoleChanged {
            role: Role::Observer,
        }
    });
    let is_resize = |e: &StreamEvent| {
        matches!(
            e,
            StreamEvent::Resized {
                cols: 100,
                rows: 30,
                ..
            }
        )
    };
    a.wait_for_event("resize to 100x30 (a)", is_resize);
    b.wait_for_event("resize to 100x30 (b)", is_resize);
    let info = daemon.host.info(tid).unwrap();
    assert_eq!((info.cols, info.rows), (100, 30));

    a.input(b"echo NOW_A_$((1+1))\r");
    b.input(b"echo NOW_B_$((1+1))\r");
    b.wait_for_screen("NOW_B_2");
    a.wait_for_screen("NOW_B_2");
    assert!(!b.mirror.text().contains("NOW_A"), "{}", b.mirror.text());
    // Both mirrors agree with each other and with the daemon.
    a.drain(Duration::from_millis(300));
    b.drain(Duration::from_millis(300));
    assert_eq!(a.mirror.text(), b.mirror.text());
    assert_eq!(a.offset, b.offset);

    // Demoting leaves the terminal without a controller.
    b.command(&StreamCommand::Demote);
    b.wait_for_event("b demoted", |e| {
        *e == StreamEvent::RoleChanged {
            role: Role::Observer,
        }
    });
    assert!(wait_until(TIMEOUT, || !daemon
        .host
        .info(tid)
        .unwrap()
        .controlled));
}

#[test]
fn controller_resize_is_broadcast_with_a_checkpoint() {
    let daemon = Daemon::start();
    let mut client = daemon.client();
    let session = client.create(80, 24, &[]);
    client.wait_for_text(&session.id, "$");
    let mut controller = daemon
        .client()
        .attach(&session.id, Role::Controller, 80, 24);
    let mut observer = daemon.client().attach(&session.id, Role::Observer, 80, 24);

    // Observers can't resize.
    observer.command(&StreamCommand::Resize { cols: 50, rows: 10 });
    controller.command(&StreamCommand::Resize { cols: 90, rows: 20 });
    for client in [&mut controller, &mut observer] {
        let event = client.wait_for_event("resize", |e| matches!(e, StreamEvent::Resized { .. }));
        let StreamEvent::Resized {
            cols, rows, offset, ..
        } = event
        else {
            unreachable!()
        };
        assert_eq!((cols, rows), (90, 20));
        assert_eq!(client.mirror.size(), (20, 90));
        assert_eq!(client.offset, offset);
    }
    controller.input(b"stty size\r");
    controller.wait_for_screen("20 90");
    observer.wait_for_screen("20 90");
    assert!(!observer
        .events
        .iter()
        .any(|e| matches!(e, StreamEvent::Resized { cols: 50, .. })));
}

#[test]
fn client_disconnect_leaves_the_terminal_running() {
    let daemon = Daemon::start();
    let mut client = daemon.client();
    let session = client.create(80, 24, &[]);
    client.wait_for_text(&session.id, "$");
    let tid = daemon.control.terminal(&session.id).unwrap();

    let mut attached = daemon
        .client()
        .attach(&session.id, Role::Controller, 80, 24);
    attached.input(b"echo BEFORE_$((2+3))\r");
    attached.wait_for_screen("BEFORE_5");
    drop(attached);

    assert!(wait_until(TIMEOUT, || !daemon
        .host
        .info(tid)
        .unwrap()
        .controlled));
    assert_eq!(daemon.host.info(tid).unwrap().state, TerminalState::Running);
    client.submit(&session.id, "echo AFTER_$((2+4))");
    client.wait_for_text(&session.id, "AFTER_6");

    // Reattaching works and shows the earlier output.
    let mut again = daemon.client().attach(&session.id, Role::Observer, 80, 24);
    again.wait_for_screen("AFTER_6");
    assert!(again.mirror.text().contains("BEFORE_5"));
}

#[test]
fn reattach_checkpoint_equals_the_screen() {
    let daemon = Daemon::start();
    let mut client = daemon.client();
    let session = client.create(60, 12, &[]);
    client.wait_for_text(&session.id, "$");
    let tid = daemon.control.terminal(&session.id).unwrap();

    let mut first = daemon
        .client()
        .attach(&session.id, Role::Controller, 60, 12);
    first.input(
        b"printf '\\e[1;32mgreen\\e[0m plain \\e[7minverse\\e[0m\\n'; \
          printf '\\xe4\\xb8\\xad\\xe6\\x96\\x87 wide\\n'; echo DONE_$((7*6))\r",
    );
    first.wait_for_screen("DONE_42");
    first.drain(Duration::from_millis(400));
    first.command(&StreamCommand::Detach);
    assert!(first.wait_closed());

    let mut second = daemon.client().attach(&session.id, Role::Observer, 60, 12);
    assert_eq!(second.offset, first.offset);
    assert_eq!(second.mirror.text(), first.mirror.text());
    assert_eq!(second.screen_rows(), first.screen_rows());
    assert_eq!(
        second.mirror.screen().cursor_position(),
        first.mirror.screen().cursor_position()
    );
    // The daemon's own view of the screen matches the checkpoint, including
    // the soft-wrapped command line.
    let screen = visible_text(&*daemon.host, tid);
    assert_eq!(second.screen_rows(), screen);
    assert!(screen.contains("pri\nntf"), "{screen}");
    // Attributes survive the checkpoint.
    let row = (0..12)
        .find(|r| {
            second
                .mirror
                .screen()
                .cell(*r, 0)
                .is_some_and(|c| c.contents() == "g" && c.bold())
        })
        .expect("green row");
    let cell = second.mirror.screen().cell(row, 0).unwrap();
    assert_eq!(cell.fgcolor(), vt::vt100::Color::Idx(2));
    assert!(second.mirror.screen().cell(row, 12).unwrap().inverse());
}

#[test]
fn process_exit_ends_attach_streams_with_the_exit_code() {
    let daemon = Daemon::start();
    let mut client = daemon.client();
    let session = client.create(80, 24, &[]);
    client.wait_for_text(&session.id, "$");
    let mut observer = daemon.client().attach(&session.id, Role::Observer, 80, 24);
    client.submit(&session.id, "exit 3");
    observer.wait_for_event("exit", |e| *e == StreamEvent::Exited { code: Some(3) });
    assert!(observer.wait_closed());

    // Killing through the API: SIGHUP ends the shell.
    let other = client.create(80, 24, &[]);
    client.wait_for_text(&other.id, "$");
    let tid = daemon.control.terminal(&other.id).unwrap();
    match client.request(&Request::SessionKill {
        id: other.id.clone(),
        tree: false,
    }) {
        Response::Killed { killed } => assert_eq!(killed, vec![other.id.clone()]),
        response => panic!("{response:?}"),
    }
    let state = wait_for_state(&*daemon.host, tid, Duration::from_secs(5));
    assert!(matches!(state, TerminalState::Exited { .. }), "{state:?}");
}

#[test]
fn subscribe_streams_control_events() {
    let daemon = Daemon::start();
    let mut client = daemon.client();
    match client.request(&Request::Subscribe(SubscribeRequest::default())) {
        Response::Subscribed => {}
        other => panic!("{other:?}"),
    }
    let envelope = EventEnvelope {
        scope: ScopeRoot::from_canonical(daemon.home.work.clone()),
        session_id: session_core::ids::SessionId::from_string("S1"),
        at: 1,
        event: SessionEvent::Activity,
    };
    daemon.control.publish(envelope.clone());
    let received: EventEnvelope =
        session_core::frame::read_control(&mut client.stream).expect("event");
    assert_eq!(received, envelope);
}

#[test]
fn daemon_status_and_shutdown() {
    let mut daemon = Daemon::start();
    let mut client = daemon.client();
    let session = client.create(80, 24, &[]);
    client.wait_for_text(&session.id, "$");
    let tid = daemon.control.terminal(&session.id).unwrap();
    let pid = daemon.host.info(tid).unwrap().pid.unwrap();

    let status = match client.request(&Request::DaemonStatus) {
        Response::Status(status) => status,
        other => panic!("{other:?}"),
    };
    assert_eq!(status.pid, std::process::id());
    assert_eq!(status.sessions, 1);
    assert_eq!(status.live_terminals, 1);
    assert!(status.clients >= 1);
    assert_eq!(status.protocol, session_core::PROTOCOL_VERSION);

    let registration = Registration::load(&daemon.home.layout).unwrap();
    assert_eq!(registration.socket, daemon.socket);

    assert_eq!(client.request(&Request::DaemonShutdown), Response::Ok);
    assert!(wait_until(TIMEOUT, || !daemon.is_serving()));
    assert!(daemon.stop());
    assert!(daemon.control.shutdown_called.load(Ordering::SeqCst));
    assert!(!daemon.socket.exists());
    assert!(!daemon.home.layout.registration_file().exists());
    assert!(daemon.host.list().is_empty());
    assert!(wait_until(TIMEOUT, || !pid_alive(pid)));
}
