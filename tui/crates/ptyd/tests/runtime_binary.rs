//! The `orchestra-ptyd` binary's command line.

mod runtime_support;

use runtime_support::TestHome;
use std::process::Command;

const EXE: &str = env!("CARGO_BIN_EXE_orchestra-ptyd");

#[test]
fn version_and_usage() {
    let out = Command::new(EXE).arg("--version").output().unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert!(stdout.starts_with("orchestra-ptyd "), "{stdout}");
    assert!(stdout.contains(&format!("protocol {}", session_core::PROTOCOL_VERSION)));

    for args in [&["bogus"][..], &[][..], &["serve", "extra"][..]] {
        let out = Command::new(EXE).args(args).output().unwrap();
        assert_eq!(out.status.code(), Some(2), "{args:?}");
        assert!(String::from_utf8_lossy(&out.stderr).contains("usage"));
    }
}

#[test]
fn serve_exits_when_another_daemon_holds_the_slot() {
    let home = TestHome::new();
    let running = ptyd::registration::claim(&home.layout, "test").unwrap();
    let out = Command::new(EXE)
        .arg("serve")
        .env(session_core::env::HOME, &home.layout.home)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains(&format!("running (pid {})", std::process::id())),
        "{stderr}"
    );
    // The running daemon's files are untouched; the log says why it stopped.
    assert!(running.registration.socket.exists());
    let log = std::fs::read_to_string(home.layout.log_file()).unwrap();
    assert!(log.contains("not starting"), "{log}");
}
