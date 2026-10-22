use super::*;
use crate::auth::tests::verified;
use std::os::unix::fs::{symlink, PermissionsExt};

fn setup() -> (tempfile::TempDir, FileCredentialStore) {
    let directory = tempfile::tempdir().unwrap();
    let store = FileCredentialStore::new(directory.path().join("private-auth"));
    (directory, store)
}

fn credentials() -> SavedCredentials {
    SavedCredentials::from_verified(verified("user_one")).unwrap()
}

fn write_private(path: &std::path::Path, bytes: &[u8]) {
    fs::write(path, bytes).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
}

#[test]
fn missing_credentials_are_empty_and_saved_files_are_private_and_atomic() {
    let (_directory, store) = setup();
    let initial = store.load().unwrap();
    assert_eq!(initial.revision().value(), 0);
    assert!(initial.credentials().is_none());
    let revision = store
        .compare_and_swap(initial.revision(), Some(&credentials()))
        .unwrap();
    let loaded = store.load().unwrap();
    assert_eq!(loaded.revision(), revision);
    assert_eq!(
        loaded.credentials().unwrap().access_token().expose_secret(),
        "PRIVATE-ACCESS-TOKEN"
    );
    assert_eq!(
        fs::metadata(&store.directory).unwrap().mode() & 0o777,
        0o700
    );
    for name in ["credentials.json", "credentials.lock"] {
        assert_eq!(
            fs::metadata(store.directory.join(name)).unwrap().mode() & 0o777,
            0o600
        );
    }
    assert_eq!(
        fs::read_dir(&store.directory).unwrap().count(),
        2,
        "no temporary file left over"
    );
}

#[test]
fn clear_retains_a_tombstone_that_rejects_late_initial_writes() {
    let (_directory, store) = setup();
    let initial = store.load().unwrap();
    let cleared = store.compare_and_swap(initial.revision(), None).unwrap();
    assert!(cleared.value() > initial.revision().value());
    assert_eq!(
        store
            .compare_and_swap(initial.revision(), Some(&credentials()))
            .unwrap_err(),
        CredentialStoreError::Conflict
    );
    let loaded = store.load().unwrap();
    assert_eq!(loaded.revision(), cleared);
    assert!(loaded.credentials().is_none());
    let bytes = fs::read(store.directory.join("credentials.json")).unwrap();
    assert!(!String::from_utf8_lossy(&bytes).contains("PRIVATE-"));
}

#[test]
fn a_conflicting_write_preserves_the_current_credentials_byte_for_byte() {
    let (_directory, store) = setup();
    let initial = store.load().unwrap();
    store
        .compare_and_swap(initial.revision(), Some(&credentials()))
        .unwrap();
    let before = fs::read(store.directory.join("credentials.json")).unwrap();
    assert_eq!(
        store
            .compare_and_swap(initial.revision(), None)
            .unwrap_err(),
        CredentialStoreError::Conflict
    );
    assert_eq!(
        before,
        fs::read(store.directory.join("credentials.json")).unwrap()
    );
}

#[test]
fn unsafe_directory_file_and_lock_permissions_are_rejected_not_fixed() {
    for name in [None, Some("credentials.json"), Some("credentials.lock")] {
        let (_directory, store) = setup();
        let initial = store.load().unwrap();
        store
            .compare_and_swap(initial.revision(), Some(&credentials()))
            .unwrap();
        let target = name
            .map(|name| store.directory.join(name))
            .unwrap_or(store.directory.clone());
        let mode = if name.is_none() { 0o755 } else { 0o644 };
        fs::set_permissions(&target, fs::Permissions::from_mode(mode)).unwrap();
        assert!(matches!(
            store.load(),
            Err(CredentialStoreError::UnsafePath)
        ));
        assert_eq!(fs::metadata(&target).unwrap().mode() & 0o777, mode);
    }
}

#[test]
fn symlinked_directory_credentials_and_lock_never_reach_an_external_file() {
    let directory = tempfile::tempdir().unwrap();
    let external = directory.path().join("external");
    fs::create_dir(&external).unwrap();
    fs::set_permissions(&external, fs::Permissions::from_mode(0o700)).unwrap();
    let linked = directory.path().join("linked");
    symlink(&external, &linked).unwrap();
    assert!(matches!(
        FileCredentialStore::new(linked).load(),
        Err(CredentialStoreError::UnsafePath)
    ));
    assert_eq!(fs::read_dir(&external).unwrap().count(), 0);

    for name in ["credentials.json", "credentials.lock"] {
        let (_directory, store) = setup();
        store.load().unwrap();
        let outside = store.directory.parent().unwrap().join("outside");
        write_private(&outside, b"PRIVATE-OUTSIDE-CONTENT");
        let target = store.directory.join(name);
        if target.exists() {
            fs::remove_file(&target).unwrap();
        }
        symlink(&outside, target).unwrap();
        assert!(matches!(
            store.load(),
            Err(CredentialStoreError::UnsafePath)
        ));
        assert_eq!(fs::read(outside).unwrap(), b"PRIVATE-OUTSIDE-CONTENT");
    }
}

#[test]
fn hardlinked_records_and_nonregular_files_are_rejected() {
    let (_directory, store) = setup();
    let initial = store.load().unwrap();
    store
        .compare_and_swap(initial.revision(), Some(&credentials()))
        .unwrap();
    fs::hard_link(
        store.directory.join("credentials.json"),
        store.directory.parent().unwrap().join("hardlink"),
    )
    .unwrap();
    assert!(matches!(
        store.load(),
        Err(CredentialStoreError::UnsafePath)
    ));

    let (_directory, store) = setup();
    store.load().unwrap();
    fs::create_dir(store.directory.join("credentials.json")).unwrap();
    assert!(matches!(
        store.load(),
        Err(CredentialStoreError::UnsafePath)
    ));
}

#[test]
fn malformed_oversized_or_unsupported_records_never_echo_credentials() {
    for bytes in [
        b"PRIVATE-BROKEN-CREDENTIALS".to_vec(),
        vec![b'x'; MAX_BYTES + 1],
        br#"{"version":2,"revision":1,"credential":null}"#.to_vec(),
        br#"{"version":1,"revision":0,"credential":null}"#.to_vec(),
        br#"{"version":1,"revision":1,"credential":null,"unexpected":"PRIVATE"}"#.to_vec(),
    ] {
        let (_directory, store) = setup();
        store.load().unwrap();
        write_private(&store.directory.join("credentials.json"), &bytes);
        let error = store.load().unwrap_err();
        assert!(!format!("{error:?} {error}").contains("PRIVATE"));
    }
}

#[test]
fn cached_identity_and_scope_validation_rejects_tampered_records() {
    for (field, value) in [
        ("subject", serde_json::json!("bad\u{1b}[31m")),
        ("issuer", serde_json::json!("http://clerk.example.com")),
        ("access_token", serde_json::json!("secret\nvalue")),
        ("scopes", serde_json::json!(["openid", "openid"])),
        ("expires_at", serde_json::json!(0)),
    ] {
        let (_directory, store) = setup();
        store.load().unwrap();
        let mut document = serde_json::to_value(StoredDocument {
            version: 1,
            revision: 1,
            credential: Some(StoredCredential::from(&credentials())),
        })
        .unwrap();
        document["credential"][field] = value;
        write_private(
            &store.directory.join("credentials.json"),
            &serde_json::to_vec(&document).unwrap(),
        );
        assert!(matches!(
            store.load(),
            Err(CredentialStoreError::InvalidData)
        ));
    }
}

#[test]
fn another_owner_observes_revisions_and_lock_contention_is_explicit() {
    let (_directory, store) = setup();
    let other = FileCredentialStore::new(store.directory.clone());
    let initial = store.load().unwrap();
    other.compare_and_swap(initial.revision(), None).unwrap();
    assert_eq!(
        store
            .compare_and_swap(initial.revision(), Some(&credentials()))
            .unwrap_err(),
        CredentialStoreError::Conflict
    );
    let (_directory_fd, _lock) = store.locked_directory(true).unwrap();
    assert!(matches!(other.load(), Err(CredentialStoreError::Busy)));
}

#[test]
fn a_briefly_held_lock_is_waited_for_not_reported_busy() {
    let (_directory, store) = setup();
    let other = FileCredentialStore::new(store.directory.clone());
    let held = store.locked_directory(true).unwrap();
    let (released, release) = std::sync::mpsc::channel();
    let holder = std::thread::spawn(move || {
        release.recv().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        drop(held);
    });
    let started = std::time::Instant::now();
    released.send(()).unwrap();
    assert!(other.load().is_ok());
    assert!(started.elapsed() >= std::time::Duration::from_millis(20));
    holder.join().unwrap();
}

#[test]
fn reads_share_the_lock_and_replacements_wait_for_them() {
    let (_directory, store) = setup();
    let other = FileCredentialStore::new(store.directory.clone());
    let reading = store.locked_directory(false).unwrap();
    let started = std::time::Instant::now();
    assert!(other.load().is_ok());
    assert!(started.elapsed() < std::time::Duration::from_millis(200));
    let revision = other.load().unwrap().revision();
    assert_eq!(
        other.compare_and_swap(revision, None).unwrap_err(),
        CredentialStoreError::Busy
    );
    drop(reading);
    assert!(other.compare_and_swap(revision, None).is_ok());
}

#[test]
fn revision_overflow_is_an_error_and_leaves_the_record_unchanged() {
    let (_directory, store) = setup();
    store.load().unwrap();
    let bytes = serde_json::to_vec(&StoredDocument {
        version: 1,
        revision: u64::MAX,
        credential: None,
    })
    .unwrap();
    write_private(&store.directory.join("credentials.json"), &bytes);
    let snapshot = store.load().unwrap();
    assert_eq!(
        store
            .compare_and_swap(snapshot.revision(), None)
            .unwrap_err(),
        CredentialStoreError::RevisionExhausted
    );
    assert_eq!(
        fs::read(store.directory.join("credentials.json")).unwrap(),
        bytes
    );
}
