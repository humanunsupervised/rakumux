use std::fs;
use std::io::{Cursor, Write};
use std::os::unix::fs::{symlink, PermissionsExt};
use std::os::unix::net::UnixListener;

use session_core::paths::Layout;

use super::protocol::{
    read_message, write_message, ConsentView, Hello, LineState, OwnerRequest, OwnerResponse,
    MAX_MESSAGE, PROTOCOL,
};
use super::*;

fn home() -> (tempfile::TempDir, OwnerLayout) {
    let directory = tempfile::Builder::new()
        .prefix("synthetic-owner-")
        .tempdir()
        .unwrap();
    let layout = OwnerLayout::new(&Layout::with_home(directory.path().join("home")));
    (directory, layout)
}

fn mode(path: &std::path::Path) -> u32 {
    fs::symlink_metadata(path).unwrap().permissions().mode() & 0o777
}

#[test]
fn the_socket_sits_next_to_the_daemons_with_the_same_hash() {
    let (directory, layout) = home();
    let daemon = Layout::with_home(directory.path().join("home")).socket_path();
    assert_eq!(layout.socket().parent(), daemon.parent());
    let name = layout.socket().file_name().unwrap().to_str().unwrap();
    let hash = daemon
        .file_name()
        .unwrap()
        .to_str()
        .unwrap()
        .strip_prefix("ptyd-")
        .unwrap();
    assert_eq!(name, format!("sensagent-{hash}"));
    let root = directory.path().join("home/sensagent");
    assert_eq!(layout.root(), root);
    assert_eq!(layout.status_dir(), root.join("status"));
    assert_eq!(layout.credentials_dir(), root.join("credentials"));
    assert_eq!(
        layout.registration_file(),
        directory.path().join("home/run/sensagent.json")
    );
    assert_eq!(
        layout.lock_file(),
        directory.path().join("home/run/sensagent.lock")
    );
}

#[test]
fn prepare_creates_private_directories_and_never_loosens_its_own() {
    let (directory, layout) = home();
    layout.prepare().unwrap();
    for path in [
        directory.path().join("home"),
        directory.path().join("home/run"),
        layout.root(),
    ] {
        assert_eq!(mode(&path), 0o700, "{}", path.display());
    }
    layout.prepare().unwrap();

    fs::set_permissions(layout.root(), fs::Permissions::from_mode(0o750)).unwrap();
    assert_eq!(layout.prepare().unwrap_err(), OwnerError::Layout);
    assert_eq!(mode(&layout.root()), 0o750);

    fs::remove_dir(layout.root()).unwrap();
    let elsewhere = directory.path().join("elsewhere");
    fs::create_dir(&elsewhere).unwrap();
    fs::set_permissions(&elsewhere, fs::Permissions::from_mode(0o700)).unwrap();
    symlink(&elsewhere, layout.root()).unwrap();
    assert_eq!(layout.prepare().unwrap_err(), OwnerError::Layout);
}

#[test]
fn one_claim_per_home_whose_files_are_private_and_removed_on_drop() {
    let (_directory, layout) = home();
    let claim = OwnerClaim::claim(&layout, "1.2.3").unwrap();
    assert_eq!(mode(&layout.registration_file()), 0o600);
    assert_eq!(mode(layout.socket()), 0o600);
    assert_eq!(mode(&layout.lock_file()), 0o600);
    let loaded = OwnerRegistration::load(&layout).unwrap();
    assert_eq!(&loaded, claim.registration());
    assert_eq!(loaded.protocol, PROTOCOL);
    assert_eq!(loaded.version, "1.2.3");
    assert_eq!(loaded.socket, layout.socket());
    assert_eq!(loaded.token.len(), 64);
    let debug = format!("{claim:?}");
    assert!(!debug.contains(&loaded.token), "{debug}");

    assert_eq!(
        OwnerClaim::claim(&layout, "1.2.3").unwrap_err(),
        OwnerError::AlreadyRunning
    );
    drop(claim);
    assert!(!layout.registration_file().exists());
    assert!(!layout.socket().exists());
    assert_eq!(
        OwnerRegistration::load(&layout).unwrap_err(),
        OwnerError::NotRunning
    );
}

#[test]
fn files_left_by_a_dead_owner_are_replaced() {
    let (_directory, layout) = home();
    layout.prepare().unwrap();
    let stale = UnixListener::bind(layout.socket()).unwrap();
    drop(stale);
    fs::write(layout.registration_file(), b"{}").unwrap();
    let claim = OwnerClaim::claim(&layout, "1").unwrap();
    assert_eq!(
        &OwnerRegistration::load(&layout).unwrap(),
        claim.registration()
    );

    // A dropped claim leaves files another owner wrote alone.
    let other = layout.registration_file();
    fs::remove_file(&other).unwrap();
    fs::write(&other, b"{}").unwrap();
    fs::set_permissions(&other, fs::Permissions::from_mode(0o600)).unwrap();
    drop(claim);
    assert!(other.exists());
}

#[test]
fn registrations_must_be_this_users_private_files() {
    let (directory, layout) = home();
    let claim = OwnerClaim::claim(&layout, "1").unwrap();
    let path = layout.registration_file();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
    assert_eq!(
        OwnerRegistration::load(&layout).unwrap_err(),
        OwnerError::Registration
    );
    assert_eq!(
        OwnerClient::connect(&layout, "test").unwrap_err(),
        OwnerError::Registration
    );

    let copy = directory.path().join("copy.json");
    fs::copy(&path, &copy).unwrap();
    fs::set_permissions(&copy, fs::Permissions::from_mode(0o600)).unwrap();
    fs::remove_file(&path).unwrap();
    symlink(&copy, &path).unwrap();
    assert_eq!(
        OwnerRegistration::load(&layout).unwrap_err(),
        OwnerError::Registration
    );
    drop(claim);
}

#[test]
fn clients_refuse_another_socket_and_report_a_missing_owner() {
    let (directory, layout) = home();
    let claim = OwnerClaim::claim(&layout, "1").unwrap();
    let mut registration = claim.registration().clone();
    registration.socket = directory.path().join("elsewhere.sock");
    let json = serde_json::to_vec(&registration).unwrap();
    let path = layout.registration_file();
    fs::write(&path, json).unwrap();
    assert_eq!(
        OwnerClient::connect(&layout, "test").unwrap_err(),
        OwnerError::Registration
    );

    // Registered, but nothing listens: the owner died.
    registration.socket = layout.socket().to_path_buf();
    fs::write(&path, serde_json::to_vec(&registration).unwrap()).unwrap();
    drop(claim);
    fs::write(&path, serde_json::to_vec(&registration).unwrap()).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    assert_eq!(
        OwnerClient::connect(&layout, "test").unwrap_err(),
        OwnerError::NotRunning
    );
}

#[test]
fn messages_are_bounded_before_their_body_is_read() {
    let hello = Hello {
        protocol: PROTOCOL,
        client: "tui".into(),
        token: "synthetic-private-token".into(),
    };
    assert!(!format!("{hello:?}").contains("synthetic-private-token"));
    let mut buffer = Vec::new();
    write_message(&mut buffer, &hello).unwrap();
    let read: Hello = read_message(&mut Cursor::new(buffer.clone()))
        .unwrap()
        .unwrap();
    assert_eq!(read, hello);
    assert!(read_message::<_, Hello>(&mut Cursor::new(Vec::new()))
        .unwrap()
        .is_none());
    assert!(
        read_message::<_, Hello>(&mut Cursor::new(buffer[..buffer.len() - 1].to_vec())).is_err()
    );

    let mut oversized = ((MAX_MESSAGE + 1) as u32).to_be_bytes().to_vec();
    oversized.push(0);
    assert!(read_message::<_, Hello>(&mut Cursor::new(oversized)).is_err());
    let mut data = 3u32.to_be_bytes().to_vec();
    data.extend_from_slice(&[1, b'{', b'}']);
    assert!(read_message::<_, Hello>(&mut Cursor::new(data)).is_err());

    let large = "x".repeat(MAX_MESSAGE);
    assert!(write_message(&mut Vec::new(), &large).is_err());
}

#[test]
fn newer_answers_degrade_to_unknown() {
    let response: OwnerResponse = serde_json::from_str(r#"{"type":"from_the_future"}"#).unwrap();
    assert_eq!(response, OwnerResponse::Unknown);
    let state: LineState = serde_json::from_str(r#""summarizing""#).unwrap();
    assert_eq!(state, LineState::Unknown);
    let code: OwnerErrorCode = serde_json::from_str(r#""too_busy""#).unwrap();
    assert_eq!(code, OwnerErrorCode::Unknown);
}

#[test]
fn folder_entries_accept_fields_a_newer_owner_adds() {
    let consent: ConsentView = serde_json::from_str(
        r#"{"revision":3,"enabled":true,"future":1,
            "folders":[{"folder":"/synthetic/folder","include_terminal_output":false,"label":"x"}]}"#,
    )
    .unwrap();
    assert_eq!(consent.folders.len(), 1);
    assert!(!consent.folders[0].include_terminal_output);
}

#[test]
fn a_failed_request_closes_the_client_so_late_answers_are_never_misread() {
    let (_directory, layout) = home();
    let mut claim = OwnerClaim::claim(&layout, "1").unwrap();
    let listener = claim.take_listener().unwrap();
    let owner = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let _: Hello = read_message(&mut stream).unwrap().unwrap();
        let welcome = OwnerResponse::Welcome {
            protocol: PROTOCOL,
            version: "1".into(),
        };
        write_message(&mut stream, &welcome).unwrap();
        let _: OwnerRequest = read_message(&mut stream).unwrap().unwrap();
        // A malformed answer, then the answer the client no longer waits for.
        let mut garbage = 4u32.to_be_bytes().to_vec();
        garbage.extend_from_slice(&[0, b'{', b'{', b'{']);
        stream.write_all(&garbage).unwrap();
        write_message(&mut stream, &OwnerResponse::Done).unwrap();
        stream
    });
    let client = OwnerClient::connect(&layout, "test").unwrap();
    assert_eq!(client.sign_out().unwrap_err(), OwnerError::Connection);
    let _stream = owner.join().unwrap();
    assert_eq!(client.sign_out().unwrap_err(), OwnerError::Connection);
    assert_eq!(client.state().unwrap_err(), OwnerError::Connection);
}
