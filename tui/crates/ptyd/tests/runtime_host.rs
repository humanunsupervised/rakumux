//! `terminal::Host` against real `bash --norc --noprofile` terminals.

mod runtime_support;

use ptyd::host::{HostError, HostEvent, TerminalHost, TerminalState};
use runtime_support::*;
use session_core::api::{ReadSource, Role, StreamEvent, TextFormat};
use session_core::client::AttachEvent;
use std::time::{Duration, Instant};

#[test]
fn spawn_submit_read_sees_the_output() {
    let home = TestHome::new();
    let host = home.host();
    let info = host.spawn(home.bash_spec(80, 24, b"")).unwrap();
    assert_eq!(info.state, TerminalState::Running);
    assert_eq!((info.cols, info.rows), (80, 24));
    assert!(info.pid.is_some());
    wait_for_prompt(&*host, info.id);

    host.submit(info.id, "echo hi").unwrap();
    let screen = wait_for_screen(&*host, info.id, "\nhi\n");
    assert!(screen.contains("$ echo hi"), "{screen}");

    let listed = host.list();
    assert_eq!(listed.len(), 1);
    assert!(listed[0].output_offset > 0);
    assert!(listed[0].last_output_ms.is_some());
    host.shutdown();
}

#[test]
fn startup_input_is_typed_once_the_shell_is_up() {
    let home = TestHome::new();
    let host = home.host();
    let info = host
        .spawn(home.bash_spec(80, 24, b"echo STARTUP_$((40+2))\r"))
        .unwrap();
    // The computed value proves the shell ran it (not just echoed input).
    wait_for_screen(&*host, info.id, "STARTUP_42");
    host.shutdown();
}

#[test]
fn terminal_query_replies_reach_the_program() {
    let home = TestHome::new();
    let host = home.host();
    let info = host.spawn(home.bash_spec(80, 24, b"")).unwrap();
    wait_for_prompt(&*host, info.id);
    // Without the daemon's responder `read` would wait for the reply forever.
    host.submit(
        info.id,
        r#"printf '\e[6n'; read -s -d R pos; echo "POS=${pos#*[}=END""#,
    )
    .unwrap();
    let screen = wait_for_screen(&*host, info.id, "=END");
    let line = screen.lines().find(|l| l.starts_with("POS=")).unwrap();
    let (row, col) = line["POS=".len()..line.len() - "=END".len()]
        .split_once(';')
        .unwrap();
    assert!(row.parse::<u16>().unwrap() >= 1, "{line}");
    assert_eq!(col.parse::<u16>().unwrap(), 1, "{line}");
    host.shutdown();
}

#[test]
fn exit_code_is_reported_and_exited_terminals_stay_readable() {
    let home = TestHome::new();
    let host = home.host();
    let events = host.subscribe();
    let info = host.spawn(home.bash_spec(80, 24, b"")).unwrap();
    wait_for_prompt(&*host, info.id);
    host.submit(info.id, "echo bye; exit 7").unwrap();

    let deadline = Instant::now() + TIMEOUT;
    let code = loop {
        match events.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
            Ok(HostEvent::Exited { id, code, .. }) if id == info.id => break code,
            Ok(_) => {}
            Err(e) => panic!("no exit event: {e}"),
        }
    };
    assert_eq!(code, Some(7));
    let now = host.info(info.id).unwrap();
    assert_eq!(now.state, TerminalState::Exited { code: Some(7) });
    assert!(visible_text(&*host, info.id).contains("bye"));
    assert_eq!(host.write(info.id, b"x"), Err(HostError::NotRunning));
    assert_eq!(host.submit(info.id, "x"), Err(HostError::NotRunning));
    assert_eq!(host.terminate(info.id), Err(HostError::NotRunning));
    assert!(matches!(
        host.attach(info.id, Role::Observer, 80, 24),
        Err(HostError::NotRunning)
    ));

    host.forget(info.id);
    assert!(host.info(info.id).is_none());
    assert!(host.list().is_empty());
    host.shutdown();
}

#[test]
fn terminate_hangs_up_the_shell() {
    let home = TestHome::new();
    let host = home.host();
    let info = host.spawn(home.bash_spec(80, 24, b"")).unwrap();
    wait_for_prompt(&*host, info.id);
    let started = Instant::now();
    host.terminate(info.id).unwrap();
    let state = wait_for_state(&*host, info.id, Duration::from_secs(5));
    assert!(matches!(state, TerminalState::Exited { .. }), "{state:?}");
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "SIGHUP was enough"
    );
    assert!(!pid_alive(info.pid.unwrap()));
    host.shutdown();
}

#[test]
fn terminate_escalates_to_sigkill_when_sighup_is_ignored() {
    let home = TestHome::new();
    let host = home.host();
    let argv = vec![
        "bash".into(),
        "--norc".into(),
        "--noprofile".into(),
        "-c".into(),
        "trap '' HUP; echo READY; sleep 30; echo done".into(),
    ];
    let info = host.spawn(home.spec(argv, 80, 24, b"")).unwrap();
    wait_for_screen(&*host, info.id, "READY");
    let started = Instant::now();
    host.terminate(info.id).unwrap();
    std::thread::sleep(Duration::from_millis(1500));
    assert_eq!(host.info(info.id).unwrap().state, TerminalState::Running);
    let state = wait_for_state(&*host, info.id, Duration::from_secs(5));
    assert_eq!(state, TerminalState::Exited { code: None });
    let elapsed = started.elapsed();
    assert!(
        elapsed >= Duration::from_millis(2900) && elapsed < Duration::from_secs(5),
        "{elapsed:?}"
    );
    host.shutdown();
}

#[test]
fn send_keys_validates_everything_before_writing() {
    let home = TestHome::new();
    let host = home.host();
    let info = host.spawn(home.bash_spec(80, 24, b"")).unwrap();
    wait_for_prompt(&*host, info.id);

    let keys = |names: &[&str]| names.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    assert_eq!(
        host.send_keys(info.id, &keys(&["e", "bogus"]), None),
        Err(HostError::InvalidKey("bogus".into()))
    );
    assert!(matches!(
        host.send_keys(info.id, &[], Some("echo x\n")),
        Err(HostError::InvalidKey(_))
    ));
    host.send_keys(
        info.id,
        &keys(&["e", "c", "h", "o"]),
        Some(" KEYS_$((1+1))"),
    )
    .unwrap();
    host.send_keys(info.id, &keys(&["enter"]), None).unwrap();
    let screen = wait_for_screen(&*host, info.id, "KEYS_2");
    // Nothing from the rejected calls reached the shell.
    assert!(!screen.contains("echo x"), "{screen}");
    assert!(!screen.contains("ebogus"), "{screen}");

    // ctrl+c interrupts a running command.
    host.submit(info.id, "sleep 30; echo NOT_INTERRUPTED")
        .unwrap();
    std::thread::sleep(Duration::from_millis(300));
    host.send_keys(info.id, &keys(&["ctrl+c"]), None).unwrap();
    host.submit(info.id, "echo AFTER_$((2+2))").unwrap();
    let screen = wait_for_screen(&*host, info.id, "AFTER_4");
    assert!(!screen.contains("\nNOT_INTERRUPTED"), "{screen}");
    host.shutdown();
}

#[test]
fn submit_strips_paste_markers_from_the_text() {
    let home = TestHome::new();
    let host = home.host();
    let info = host.spawn(home.bash_spec(80, 24, b"")).unwrap();
    wait_for_prompt(&*host, info.id);
    assert!(wait_until(TIMEOUT, || host
        .info(info.id)
        .unwrap()
        .bracketed_paste));
    // Bracketed paste on (readline): a text trying to close the paste early.
    host.submit(info.id, "echo SAFE_A\x1b[201~; echo SAFE_B\x1b[200~")
        .unwrap();
    wait_for_screen(&*host, info.id, "\nSAFE_A\nSAFE_B\n");

    // Bracketed paste off (cat): unstripped markers would show up as text.
    host.submit(info.id, "cat").unwrap();
    assert!(wait_until(TIMEOUT, || !host
        .info(info.id)
        .unwrap()
        .bracketed_paste));
    host.submit(info.id, "X\x1b[201~Y\x1b[200~Z").unwrap();
    let screen = wait_for_screen(&*host, info.id, "XYZ\nXYZ");
    assert!(
        !screen.contains("201~") && !screen.contains("200~"),
        "{screen}"
    );
    host.send_keys(info.id, &["ctrl+d".to_string()], None)
        .unwrap();
    host.shutdown();
}

#[test]
fn tiny_sizes_are_clamped_and_wide_chars_do_not_panic() {
    let home = TestHome::new();
    let host = home.host();
    let info = host.spawn(home.bash_spec(1, 1, b"")).unwrap();
    assert_eq!((info.cols, info.rows), (2, 2));
    let attachment = host.attach(info.id, Role::Controller, 0, 0).unwrap();
    assert_eq!((attachment.info.cols, attachment.info.rows), (2, 2));
    attachment.control.resize(1, 1).unwrap();
    host.submit(
        info.id,
        r"printf '\xe4\xb8\xad\xe6\x96\x87\xe4\xb8\xad\n'; echo WIDE_OK",
    )
    .unwrap();
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(host.info(info.id).unwrap().state, TerminalState::Running);
    attachment.control.resize(80, 24).unwrap();
    host.submit(info.id, "echo BACK_$((3*3))").unwrap();
    wait_for_screen(&*host, info.id, "BACK_9");
    assert_eq!((host.info(info.id).unwrap().cols), 80);
    drop(attachment);
    host.shutdown();
}

#[test]
fn title_cwd_and_bell_become_events() {
    let home = TestHome::new();
    let host = home.host();
    let info = host.spawn(home.bash_spec(80, 24, b"")).unwrap();
    wait_for_prompt(&*host, info.id);
    let attachment = host.attach(info.id, Role::Observer, 80, 24).unwrap();
    let events = host.subscribe();
    host.submit(
        info.id,
        r"printf '\e]2;my-title\a\e]7;file://host/tmp/some%%20dir\a\a'",
    )
    .unwrap();

    let mut seen = (false, false, false);
    let deadline = Instant::now() + TIMEOUT;
    while seen != (true, true, true) && Instant::now() < deadline {
        match events.recv_timeout(Duration::from_millis(100)) {
            Ok(HostEvent::Title { title, .. }) => seen.0 = title == "my-title",
            Ok(HostEvent::Cwd { path, .. }) => seen.1 |= path.to_str() == Some("/tmp/some dir"),
            Ok(HostEvent::Bell { .. }) => seen.2 = true,
            _ => {}
        }
    }
    assert_eq!(seen, (true, true, true));
    let now = host.info(info.id).unwrap();
    assert_eq!(now.title, "my-title");
    assert_eq!(now.cwd.unwrap().to_str(), Some("/tmp/some dir"));

    let mut stream_events = Vec::new();
    while let Ok(event) = attachment.events.recv_timeout(Duration::from_millis(300)) {
        if let AttachEvent::Event(e) = event {
            stream_events.push(e);
        }
    }
    assert!(stream_events.contains(&StreamEvent::Title {
        title: "my-title".into()
    }));
    assert!(stream_events.contains(&StreamEvent::Bell));
    assert!(stream_events.contains(&StreamEvent::Cwd {
        path: "/tmp/some dir".into()
    }));
    host.shutdown();
}

#[test]
fn output_events_are_throttled_with_a_trailing_event() {
    let home = TestHome::new();
    let host = home.host();
    let info = host.spawn(home.bash_spec(80, 24, b"")).unwrap();
    wait_for_prompt(&*host, info.id);
    std::thread::sleep(Duration::from_millis(700));
    let events = host.subscribe();
    host.submit(info.id, "for i in $(seq 1 25); do echo $i; sleep 0.1; done")
        .unwrap();
    std::thread::sleep(Duration::from_millis(4500));
    let outputs = events
        .try_iter()
        .filter(|e| matches!(e, HostEvent::Output { .. }))
        .count();
    // ~2.5-3 s of output: one per second at most, plus the trailing one.
    assert!((2..=6).contains(&outputs), "{outputs} output events");
    host.shutdown();
}

#[test]
fn reads_cover_history_unwrapped_lines_and_ansi() {
    let home = TestHome::new();
    let host = home.host();
    let info = host.spawn(home.bash_spec(20, 5, b"")).unwrap();
    wait_for_prompt(&*host, info.id);
    host.submit(
        info.id,
        r"for i in 1 2 3 4 5 6 7 8; do echo L$i; done; printf '\e[31mRED\e[0m\n'",
    )
    .unwrap();
    wait_for_screen(&*host, info.id, "RED");
    let recent = host
        .read(info.id, ReadSource::Recent, None, TextFormat::Text)
        .unwrap();
    assert!(recent.text.contains("L1\nL2\n"), "{}", recent.text);
    assert!(recent.untrusted);
    let last = host
        .read(info.id, ReadSource::Recent, Some(2), TextFormat::Text)
        .unwrap();
    assert!(last.truncated);
    assert_eq!(last.text.lines().count(), 2);
    let ansi = host
        .read(info.id, ReadSource::Visible, None, TextFormat::Ansi)
        .unwrap();
    assert!(ansi.text.contains("\x1b[0;31mRED"), "{:?}", ansi.text);
    // The long command line soft-wraps at 20 columns; unwrapped joins it.
    let unwrapped = host
        .read(info.id, ReadSource::RecentUnwrapped, None, TextFormat::Text)
        .unwrap();
    assert!(
        unwrapped
            .text
            .contains("for i in 1 2 3 4 5 6 7 8; do echo L$i; done"),
        "{}",
        unwrapped.text
    );
    host.shutdown();
}

#[test]
fn scrollback_is_saved_while_output_flows_and_on_exit() {
    let home = TestHome::new();
    let host = home.host_with(|c| c.persist_interval = Duration::from_millis(50));
    let info = host.spawn(home.bash_spec(80, 24, b"")).unwrap();
    wait_for_prompt(&*host, info.id);
    host.submit(
        info.id,
        "for i in $(seq 1 10); do echo $i; sleep 0.05; done; echo LOOP_$((5+5))",
    )
    .unwrap();
    wait_for_screen(&*host, info.id, "LOOP_10");
    assert!(host.scrollback_saves() >= 2, "{}", host.scrollback_saves());
    // Idle terminals aren't saved over and over.
    std::thread::sleep(Duration::from_millis(300));
    let idle = host.scrollback_saves();
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(host.scrollback_saves(), idle);
    host.submit(info.id, "exit").unwrap();
    wait_for_state(&*host, info.id, TIMEOUT);
    assert!(wait_until(TIMEOUT, || host.scrollback_saves() > idle));
    host.shutdown();
}

#[test]
fn forgotten_terminals_never_save_scrollback_again() {
    let home = TestHome::new();
    // Save every 20 ms while output flows, to make a race likely.
    let host = home.host_with(|c| c.persist_interval = Duration::from_millis(20));
    for round in 0..5 {
        let info = host.spawn(home.bash_spec(80, 24, b"")).unwrap();
        wait_for_prompt(&*host, info.id);
        let before = host.scrollback_saves();
        host.submit(info.id, "while :; do echo tick; sleep 0.005; done")
            .unwrap();
        assert!(wait_until(TIMEOUT, || host.scrollback_saves() > before));
        // What session delete does: terminate, forget, remove the files. On
        // odd rounds the shell is already exiting when forget arrives.
        if round % 2 == 1 {
            host.send_keys(info.id, &["ctrl+c".to_string()], None)
                .unwrap();
            host.submit(info.id, "exit").unwrap();
        }
        let _ = host.terminate(info.id);
        host.forget(info.id);
        let after_forget = host.scrollback_saves();
        let dir = home.layout.session_dir(&info.session_id);
        let _ = std::fs::remove_dir_all(&dir);

        std::thread::sleep(Duration::from_millis(600));
        assert_eq!(host.scrollback_saves(), after_forget, "round {round}");
        assert!(
            !dir.exists(),
            "round {round}: {} was recreated",
            dir.display()
        );
        assert!(wait_until(TIMEOUT, || !pid_alive(info.pid.unwrap())));
    }
    host.shutdown();
}

#[test]
fn forget_drops_and_terminates_a_running_terminal() {
    let home = TestHome::new();
    let host = home.host();
    let info = host.spawn(home.bash_spec(80, 24, b"")).unwrap();
    wait_for_prompt(&*host, info.id);
    let pid = info.pid.unwrap();
    host.forget(info.id);
    assert!(host.info(info.id).is_none());
    assert!(matches!(
        host.submit(info.id, "x"),
        Err(HostError::NotFound)
    ));
    assert!(wait_until(TIMEOUT, || !pid_alive(pid)));
    host.shutdown();
}

#[test]
fn replay_ring_returns_recent_raw_output() {
    let home = TestHome::new();
    let host = home.host();
    let info = host.spawn(home.bash_spec(80, 24, b"")).unwrap();
    wait_for_prompt(&*host, info.id);
    host.submit(info.id, "echo RING_$((5*5))").unwrap();
    wait_for_screen(&*host, info.id, "RING_25");
    let all = host.replay_since(info.id, 0).unwrap().unwrap();
    assert!(String::from_utf8_lossy(&all).contains("RING_25"));
    let end = host.info(info.id).unwrap().output_offset;
    assert_eq!(host.replay_since(info.id, end).unwrap(), Some(vec![]));
    assert_eq!(host.replay_since(info.id, end + 1).unwrap(), None);
    host.shutdown();
}

#[test]
fn spawn_rejects_bad_commands_and_directories() {
    let home = TestHome::new();
    let host = home.host();
    let mut spec = home.bash_spec(80, 24, b"");
    spec.cwd = home.work.join("missing");
    assert!(matches!(host.spawn(spec), Err(HostError::Spawn(m)) if m.contains("missing")));
    let spec = home.spec(vec!["definitely-not-a-program-xyz".into()], 80, 24, b"");
    assert!(matches!(host.spawn(spec), Err(HostError::Spawn(_))));
    let spec = home.spec(vec![], 80, 24, b"");
    assert!(matches!(host.spawn(spec), Err(HostError::Spawn(_))));
    assert!(host.list().is_empty());
    host.shutdown();
}

#[test]
fn shutdown_ends_every_terminal_and_refuses_new_ones() {
    let home = TestHome::new();
    let host = home.host();
    let a = host.spawn(home.bash_spec(80, 24, b"")).unwrap();
    let b = host.spawn(home.bash_spec(80, 24, b"")).unwrap();
    wait_for_prompt(&*host, a.id);
    wait_for_prompt(&*host, b.id);
    host.shutdown();
    assert!(host.list().is_empty());
    assert!(wait_until(TIMEOUT, || !pid_alive(a.pid.unwrap())
        && !pid_alive(b.pid.unwrap())));
    assert!(matches!(
        host.spawn(home.bash_spec(80, 24, b"")),
        Err(HostError::Spawn(_))
    ));
}

#[test]
fn a_stalled_observer_is_resynced_without_blocking_output() {
    let home = TestHome::new();
    let host = home.host();
    let info = host.spawn(home.bash_spec(80, 24, b"")).unwrap();
    wait_for_prompt(&*host, info.id);
    // An observer that doesn't read while ~2 MB of output flows.
    let observer = host.attach(info.id, Role::Observer, 80, 24).unwrap();
    host.submit(
        info.id,
        "yes FLOODLINE | head -n 200000; echo FLOOD_$((6*7))",
    )
    .unwrap();
    // The program isn't held back by the observer.
    wait_for_screen(&*host, info.id, "FLOOD_42");

    // Replaying the stream the way a client does ends at the same screen.
    let mut mirror = vt::Emulator::passive(observer.info.rows, observer.info.cols, 0);
    mirror.feed(&observer.checkpoint);
    let mut expected = observer.offset;
    let mut lagged = false;
    while let Ok(event) = observer.events.recv_timeout(Duration::from_millis(500)) {
        match event {
            AttachEvent::Output { offset, bytes } => {
                assert_eq!(offset, expected, "gap without a checkpoint");
                mirror.feed(&bytes);
                expected += bytes.len() as u64;
            }
            AttachEvent::Event(StreamEvent::Lagged { .. }) => lagged = true,
            AttachEvent::Event(StreamEvent::Resized {
                cols,
                rows,
                checkpoint,
                offset,
            }) => {
                assert!(lagged, "a lag marker precedes the fresh checkpoint");
                mirror = vt::Emulator::passive(rows, cols, 0);
                mirror.feed(&decode_checkpoint(&checkpoint));
                expected = offset;
            }
            _ => {}
        }
    }
    assert!(lagged, "the observer should have lagged");
    assert_eq!(expected, host.info(info.id).unwrap().output_offset);
    assert!(mirror.text().contains("FLOOD_42"), "{}", mirror.text());
    host.shutdown();
}

#[test]
fn observers_fit_the_terminal_unless_the_controller_is_typing() {
    let home = TestHome::new();
    let host = home.host();
    let info = host.spawn(home.bash_spec(80, 24, b"")).unwrap();
    let observer = host.attach(info.id, Role::Observer, 80, 24).unwrap();
    let wait_cols = |cols: u16, fit: &dyn Fn()| {
        let deadline = Instant::now() + Duration::from_secs(5);
        while host.info(info.id).unwrap().cols != cols && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
            fit();
        }
        host.info(info.id).unwrap().cols
    };

    // Nobody controls it: the observer's pane size wins.
    observer.control.fit(120, 40).unwrap();
    let now = host.info(info.id).unwrap();
    assert_eq!((now.cols, now.rows), (120, 40));

    // A controller that is typing keeps its size.
    let controller = host.attach(info.id, Role::Controller, 100, 30).unwrap();
    assert_eq!(host.info(info.id).unwrap().cols, 100);
    controller.control.input(b":\r").unwrap();
    observer.control.fit(60, 20).unwrap();
    let now = host.info(info.id).unwrap();
    assert_eq!((now.cols, now.rows), (100, 30));

    // A controller that hasn't typed (another window took over and its
    // user went elsewhere) gives way to the window being used.
    let quiet = host.attach(info.id, Role::Controller, 70, 22).unwrap();
    assert_eq!(host.info(info.id).unwrap().cols, 70);
    let fit = || observer.control.fit(90, 25).unwrap();
    assert_eq!(wait_cols(90, &fit), 90);

    // Without a controller, observers fit as before.
    quiet.control.demote();
    let fit = || observer.control.fit(110, 30).unwrap();
    assert_eq!(wait_cols(110, &fit), 110);
    drop(controller);
    drop(quiet);
    drop(observer);
    host.shutdown();
}
