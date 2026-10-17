//! Single-instance election and the registration file.

mod runtime_support;

use ptyd::registration::{claim, ClaimError};
use runtime_support::TestHome;
use session_core::paths::Registration;
use std::os::unix::fs::PermissionsExt;

fn mode(path: &std::path::Path) -> u32 {
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
}

#[test]
fn second_claim_fails_with_already_running() {
    let home = TestHome::new();
    let first = claim(&home.layout, "1.2.3").unwrap();
    match claim(&home.layout, "1.2.3") {
        Err(ClaimError::AlreadyRunning(Some(registration))) => {
            assert_eq!(registration.pid, std::process::id());
            assert_eq!(registration.instance_id, first.registration.instance_id);
        }
        Err(other) => panic!("expected AlreadyRunning, got {other:?}"),
        Ok(_) => panic!("expected AlreadyRunning, got a second claim"),
    }
    // The failed claim left the running daemon's files alone.
    assert!(first.registration.socket.exists());
    assert_eq!(
        Registration::load(&home.layout).unwrap(),
        first.registration
    );
}

#[test]
fn claim_writes_private_files_and_releases_them_on_drop() {
    let home = TestHome::new();
    let claimed = claim(&home.layout, "1.2.3").unwrap();
    let registration = Registration::load(&home.layout).unwrap();
    assert_eq!(registration, claimed.registration);
    assert_eq!(registration.version, "1.2.3");
    assert_eq!(registration.protocol, session_core::PROTOCOL_VERSION);
    assert_eq!(registration.token, claimed.user_token.as_str());
    assert_eq!(registration.token.len(), 64);
    assert_eq!(mode(&home.layout.registration_file()), 0o600);
    assert_eq!(mode(&home.layout.run_dir), 0o700);
    assert_eq!(mode(&registration.socket), 0o600);
    assert_eq!(mode(registration.socket.parent().unwrap()), 0o700);

    let socket = registration.socket.clone();
    drop(claimed);
    assert!(!socket.exists());
    assert!(!home.layout.registration_file().exists());
    // The slot is free again, with a new token.
    let again = claim(&home.layout, "1.2.3").unwrap();
    assert_ne!(again.registration.token, registration.token);
}

#[test]
fn a_new_daemon_takes_over_after_a_crash() {
    let home = TestHome::new();
    let stale = {
        let first = claim(&home.layout, "old").unwrap();
        let registration = first.registration.clone();
        drop(first);
        registration
    };
    // Leave behind what a crashed daemon would: a socket file and its
    // registration (the lock is released by the kernel when it dies).
    let listener = std::os::unix::net::UnixListener::bind(&stale.socket).unwrap();
    std::fs::write(
        home.layout.registration_file(),
        serde_json::to_vec(&stale).unwrap(),
    )
    .unwrap();
    drop(listener);
    assert!(stale.socket.exists());

    let fresh = claim(&home.layout, "new").unwrap();
    assert_ne!(fresh.registration.instance_id, stale.instance_id);
    assert_eq!(
        Registration::load(&home.layout).unwrap().version,
        "new".to_string()
    );
    // The new socket accepts connections.
    std::os::unix::net::UnixStream::connect(&fresh.registration.socket).unwrap();
}

#[test]
fn drop_leaves_files_that_another_daemon_replaced() {
    let home = TestHome::new();
    let claimed = claim(&home.layout, "1").unwrap();
    // Another instance rewrote the registration (e.g. after a forced
    // takeover); the old claim must not delete it.
    let mut other = claimed.registration.clone();
    other.instance_id = "someone-else".into();
    std::fs::write(
        home.layout.registration_file(),
        serde_json::to_vec(&other).unwrap(),
    )
    .unwrap();
    let socket = claimed.registration.socket.clone();
    std::fs::remove_file(&socket).unwrap();
    let replacement = std::os::unix::net::UnixListener::bind(&socket).unwrap();
    drop(claimed);
    assert!(home.layout.registration_file().exists());
    assert!(socket.exists());
    drop(replacement);
    std::fs::remove_file(&socket).unwrap();
}
