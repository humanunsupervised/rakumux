use super::*;
use crate::status::{AccountKey, FolderConsent, SourceStamp, StatusText};
use session_core::model::ScopeRoot;
use std::os::unix::fs::{symlink, PermissionsExt};
use std::process::Command;
use std::sync::{Mutex, MutexGuard};
use std::time::Instant;

fn isolated_process_test() -> MutexGuard<'static, ()> {
    // A concurrent Command fork briefly inherits other tests' flock descriptors
    // until exec closes O_CLOEXEC files. Do not mistake that window for a leaked lock.
    static TEST_LOCK: Mutex<()> = Mutex::new(());
    TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner())
}

/// A temporary directory under the system's, canonical (macOS's is under the `/var`
/// symlink).
fn scratch() -> tempfile::TempDir {
    tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap()
}

fn setup() -> (tempfile::TempDir, SqliteStatusStore) {
    let directory = scratch();
    let store = SqliteStatusStore::open(directory.path().join("private-status")).unwrap();
    (directory, store)
}

fn account() -> AccountKey {
    AccountKey {
        issuer: "https://identity.example.test/".into(),
        client_id: "public-client".into(),
        subject: "synthetic-user".into(),
    }
}

fn folder(directory: &Path) -> ScopeRoot {
    ScopeRoot::resolve(directory).unwrap()
}

fn policy(directory: &Path) -> ConsentPolicy {
    ConsentPolicy::Enabled {
        account: account(),
        auth_revision: 7,
        folders: vec![FolderConsent {
            folder: folder(directory),
            include_terminal_output: false,
        }],
    }
}

fn enable(store: &mut SqliteStatusStore, directory: &Path) -> ConsentSnapshot {
    let current = store.load_consent().unwrap();
    store
        .compare_and_swap_consent(current.revision, &policy(directory))
        .unwrap()
}

fn entry(directory: &Path, revision: ConsentRevision) -> CachedStatus {
    CachedStatus {
        account: account(),
        auth_revision: 7,
        consent_revision: revision,
        session_id: SessionId::generate(),
        folder: folder(directory),
        stamp: SourceStamp {
            terminal_generation: 2,
            source_revision: 3,
        },
        request_id: "synthetic-request-1".into(),
        text: StatusText::try_from("Adding validation tests".to_owned()).unwrap(),
        captured_at: 100,
    }
}

fn seed_cache(store: &mut SqliteStatusStore, directory: &Path, count: usize) -> Vec<CachedStatus> {
    let snapshot = enable(store, directory);
    let mut entries = Vec::new();
    for index in 0..count {
        let mut cached = entry(directory, snapshot.revision);
        cached.captured_at = index as u64;
        store.save_cache(snapshot.revision, &cached).unwrap();
        entries.push(cached);
    }
    entries
}

fn write_private(path: &Path, bytes: &[u8]) {
    fs::write(path, bytes).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
}

fn open_error(path: &Path) -> StatusStoreError {
    match SqliteStatusStore::open(path) {
        Ok(_) => panic!("unsafe or invalid store unexpectedly opened"),
        Err(error) => error,
    }
}

fn assert_redacted(error: &StatusStoreError, path: &Path) {
    let diagnostic = format!("{error:?} {error}");
    assert!(!diagnostic.contains("PRIVATE"));
    assert!(!diagnostic.contains(path.to_str().unwrap()));
    assert!(!diagnostic.contains("SELECT"));
    assert!(!diagnostic.contains("status_consent"));
}

#[test]
fn missing_store_defaults_to_disabled_revision_zero_and_is_send() {
    let _isolation = isolated_process_test();
    fn require_send<T: Send>() {}
    require_send::<SqliteStatusStore>();
    let (_directory, mut store) = setup();
    assert_eq!(store.load_consent().unwrap(), ConsentSnapshot::default());
    assert_eq!(store.load_cache(&SessionId::generate()).unwrap(), None);
    store.remove_cache(&SessionId::generate()).unwrap();
}

#[test]
fn consent_cas_advances_even_disabled_and_stale_opt_out_cannot_resurrect() {
    let _isolation = isolated_process_test();
    let (directory, mut store) = setup();
    let tombstone = store
        .compare_and_swap_consent(ConsentRevision(0), &ConsentPolicy::Disabled)
        .unwrap();
    assert_eq!(tombstone.revision, ConsentRevision(1));
    assert_eq!(
        store.compare_and_swap_consent(ConsentRevision(0), &policy(directory.path())),
        Err(StatusStoreError::Conflict)
    );
    let enabled = enable(&mut store, directory.path());
    let cached = entry(directory.path(), enabled.revision);
    store.save_cache(enabled.revision, &cached).unwrap();
    let disabled = store
        .compare_and_swap_consent(enabled.revision, &ConsentPolicy::Disabled)
        .unwrap();
    assert_eq!(disabled.revision.0, enabled.revision.0 + 1);
    assert_eq!(
        store.compare_and_swap_consent(enabled.revision, &enabled.policy),
        Err(StatusStoreError::Conflict)
    );
    assert_eq!(
        store.save_cache(enabled.revision, &cached),
        Err(StatusStoreError::Conflict)
    );
    assert_eq!(store.load_consent().unwrap(), disabled);
    assert_eq!(store.load_cache(&cached.session_id).unwrap(), None);
}

#[test]
fn conflicting_consent_write_keeps_policy_and_all_cache() {
    let _isolation = isolated_process_test();
    let (directory, mut store) = setup();
    let entries = seed_cache(&mut store, directory.path(), 2);
    let before = store.load_consent().unwrap();
    assert_eq!(
        store.compare_and_swap_consent(ConsentRevision(0), &ConsentPolicy::Disabled),
        Err(StatusStoreError::Conflict)
    );
    assert_eq!(store.load_consent().unwrap(), before);
    for cached in entries {
        assert_eq!(store.load_cache(&cached.session_id).unwrap(), Some(cached));
    }
}

#[test]
fn disabled_wrong_account_auth_revision_folder_and_consent_revision_deny_cache() {
    let _isolation = isolated_process_test();
    let (directory, mut store) = setup();
    let mut cached = entry(directory.path(), ConsentRevision(0));
    assert_eq!(
        store.save_cache(ConsentRevision(0), &cached),
        Err(StatusStoreError::Conflict)
    );
    let enabled = enable(&mut store, directory.path());
    cached.consent_revision = enabled.revision;
    let mut invalid_entries = Vec::new();
    let mut wrong = cached.clone();
    wrong.account.subject = "another-user".into();
    invalid_entries.push(wrong);
    let mut wrong = cached.clone();
    wrong.account.client_id = "another-client".into();
    invalid_entries.push(wrong);
    let mut wrong = cached.clone();
    wrong.account.issuer = "https://another.example.test/".into();
    invalid_entries.push(wrong);
    let mut wrong = cached.clone();
    wrong.auth_revision += 1;
    invalid_entries.push(wrong);
    let other_folder = scratch();
    let mut wrong = cached.clone();
    wrong.folder = folder(other_folder.path());
    invalid_entries.push(wrong);
    let mut wrong = cached.clone();
    wrong.consent_revision.0 += 1;
    invalid_entries.push(wrong);
    for wrong in invalid_entries {
        assert_eq!(
            store.save_cache(enabled.revision, &wrong),
            Err(StatusStoreError::Conflict)
        );
        assert_eq!(store.load_cache(&cached.session_id).unwrap(), None);
    }
    assert_eq!(
        store.save_cache(ConsentRevision(0), &cached),
        Err(StatusStoreError::Conflict)
    );
    store.save_cache(enabled.revision, &cached).unwrap();
}

#[test]
fn every_policy_mutation_clears_the_entire_cache_atomically() {
    let _isolation = isolated_process_test();
    let (directory, mut store) = setup();
    let other_folder = scratch();
    let mut enabled = policy(directory.path());
    if let ConsentPolicy::Enabled { folders, .. } = &mut enabled {
        folders.push(FolderConsent {
            folder: folder(other_folder.path()),
            include_terminal_output: false,
        });
    }
    let snapshot = store
        .compare_and_swap_consent(ConsentRevision(0), &enabled)
        .unwrap();
    let first = entry(directory.path(), snapshot.revision);
    let second = entry(other_folder.path(), snapshot.revision);
    for cached in [&first, &second] {
        store.save_cache(snapshot.revision, cached).unwrap();
    }
    if let ConsentPolicy::Enabled { folders, .. } = &mut enabled {
        folders[0].include_terminal_output = true;
    }
    let changed = store
        .compare_and_swap_consent(snapshot.revision, &enabled)
        .unwrap();
    for cached in [&first, &second] {
        assert_eq!(store.load_cache(&cached.session_id).unwrap(), None);
        assert_eq!(
            store.save_cache(snapshot.revision, cached),
            Err(StatusStoreError::Conflict)
        );
    }
    let mut fresh = first.clone();
    fresh.consent_revision = changed.revision;
    store.save_cache(changed.revision, &fresh).unwrap();
    // Reconfirming an identical policy is still a mutation and a new generation.
    store
        .compare_and_swap_consent(changed.revision, &enabled)
        .unwrap();
    assert_eq!(store.load_cache(&fresh.session_id).unwrap(), None);
}

#[test]
fn reopen_persists_consent_cache_upsert_and_removal() {
    let _isolation = isolated_process_test();
    let (directory, mut store) = setup();
    let entries = seed_cache(&mut store, directory.path(), 2);
    let snapshot = store.load_consent().unwrap();
    let mut updated = entries[0].clone();
    updated.captured_at += 20;
    updated.text = StatusText::try_from("Reviewing validation tests".to_owned()).unwrap();
    store.save_cache(snapshot.revision, &updated).unwrap();
    store.remove_cache(&entries[1].session_id).unwrap();
    let path = store.storage.path.clone();
    drop(store);
    let mut reopened = SqliteStatusStore::open(path).unwrap();
    assert_eq!(reopened.load_consent().unwrap(), snapshot);
    assert_eq!(
        reopened.load_cache(&updated.session_id).unwrap(),
        Some(updated)
    );
    assert_eq!(reopened.load_cache(&entries[1].session_id).unwrap(), None);
}

#[test]
fn owner_lock_is_nonblocking_and_released_on_drop() {
    let _isolation = isolated_process_test();
    let (_directory, store) = setup();
    let path = store.storage.path.clone();
    let start = Instant::now();
    assert_eq!(open_error(&path), StatusStoreError::Busy);
    assert!(start.elapsed() < Duration::from_secs(2));
    drop(store);
    assert!(SqliteStatusStore::open(path).is_ok());
}

#[test]
fn sqlite_external_transaction_wait_is_bounded_and_reported() {
    let _isolation = isolated_process_test();
    let (_directory, mut store) = setup();
    let external = Connection::open(store.storage.path.join("status.sqlite3")).unwrap();
    external.execute_batch("BEGIN IMMEDIATE").unwrap();
    let start = Instant::now();
    assert_eq!(store.load_consent(), Err(StatusStoreError::Busy));
    assert!(start.elapsed() < Duration::from_secs(2));
    external.execute_batch("ROLLBACK").unwrap();
    assert_eq!(store.load_consent().unwrap(), ConsentSnapshot::default());
}

#[test]
fn directory_database_lock_and_live_journal_are_private_without_wal() {
    let _isolation = isolated_process_test();
    let (directory, mut store) = setup();
    enable(&mut store, directory.path());
    assert_eq!(
        fs::metadata(&store.storage.path).unwrap().mode() & 0o7777,
        0o700
    );
    for name in ["status.sqlite3", "status.lock"] {
        let metadata = fs::metadata(store.storage.path.join(name)).unwrap();
        assert!(metadata.is_file());
        assert_eq!(metadata.nlink(), 1);
        assert_eq!(metadata.mode() & 0o7777, 0o600);
    }
    assert_eq!(
        store
            .connection
            .query_row("PRAGMA journal_mode", [], |row| row.get::<_, String>(0))
            .unwrap(),
        "delete"
    );
    assert_eq!(
        store
            .connection
            .query_row("PRAGMA synchronous", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        2
    );
    store
        .connection
        .execute_batch("BEGIN IMMEDIATE; UPDATE status_consent SET revision=revision+1")
        .unwrap();
    let journal = fs::metadata(store.storage.path.join("status.sqlite3-journal")).unwrap();
    assert_eq!(journal.mode() & 0o7777, 0o600);
    assert_eq!(journal.nlink(), 1);
    assert!(journal.is_file());
    store.storage.validate().unwrap();
    store.connection.execute_batch("ROLLBACK").unwrap();
    assert_eq!(fs::read_dir(&store.storage.path).unwrap().count(), 2);
}

#[test]
fn unsafe_existing_directory_and_file_modes_are_rejected_not_chmodded() {
    let _isolation = isolated_process_test();
    for name in [
        None,
        Some("status.sqlite3"),
        Some("status.lock"),
        Some("status.sqlite3-journal"),
        Some("status.sqlite3-wal"),
        Some("status.sqlite3-shm"),
    ] {
        let (_directory, store) = setup();
        let path = store.storage.path.clone();
        let target = name.map(|name| path.join(name)).unwrap_or(path.clone());
        drop(store);
        if name.is_some() && !target.exists() {
            write_private(&target, b"");
        }
        let mode = if name.is_none() { 0o755 } else { 0o640 };
        fs::set_permissions(&target, fs::Permissions::from_mode(mode)).unwrap();
        assert_eq!(open_error(&path), StatusStoreError::UnsafePath);
        assert_eq!(fs::metadata(target).unwrap().mode() & 0o7777, mode);
    }
}

#[test]
fn symlinked_directory_and_known_files_never_reach_an_external_target() {
    let _isolation = isolated_process_test();
    let directory = scratch();
    let external = directory.path().join("external");
    fs::DirBuilder::new().mode(0o700).create(&external).unwrap();
    let linked = directory.path().join("linked");
    symlink(&external, &linked).unwrap();
    assert_eq!(open_error(&linked), StatusStoreError::UnsafePath);
    assert_eq!(open_error(&linked.join("")), StatusStoreError::UnsafePath);
    assert_eq!(open_error(&linked.join(".")), StatusStoreError::UnsafePath);
    assert_eq!(fs::read_dir(&external).unwrap().count(), 0);
    for name in [
        "status.sqlite3",
        "status.lock",
        "status.sqlite3-journal",
        "status.sqlite3-wal",
        "status.sqlite3-shm",
    ] {
        let (directory, store) = setup();
        let path = store.storage.path.clone();
        drop(store);
        let outside = directory.path().join("PRIVATE-outside");
        write_private(&outside, b"PRIVATE-EXTERNAL-CONTENT");
        let target = path.join(name);
        if target.exists() {
            fs::remove_file(&target).unwrap();
        }
        symlink(&outside, target).unwrap();
        assert_eq!(open_error(&path), StatusStoreError::UnsafePath);
        assert_eq!(fs::read(&outside).unwrap(), b"PRIVATE-EXTERNAL-CONTENT");
        assert_eq!(fs::metadata(outside).unwrap().mode() & 0o7777, 0o600);
    }
}

#[test]
fn hardlinks_and_nonregular_database_lock_and_sidecars_are_rejected() {
    let _isolation = isolated_process_test();
    for name in [
        "status.sqlite3",
        "status.lock",
        "status.sqlite3-journal",
        "status.sqlite3-wal",
        "status.sqlite3-shm",
    ] {
        for hardlink in [true, false] {
            let (directory, store) = setup();
            let path = store.storage.path.clone();
            drop(store);
            let target = path.join(name);
            if hardlink {
                if !target.exists() {
                    write_private(&target, b"");
                }
                fs::hard_link(&target, directory.path().join("outside-link")).unwrap();
            } else {
                if target.exists() {
                    fs::remove_file(&target).unwrap();
                }
                fs::create_dir(&target).unwrap();
            }
            assert_eq!(open_error(&path), StatusStoreError::UnsafePath);
        }
    }
}

#[test]
fn live_operations_recheck_modes_links_identity_and_sidecars() {
    let _isolation = isolated_process_test();
    for name in ["status.sqlite3", "status.lock", "status.sqlite3-journal"] {
        let (_directory, mut store) = setup();
        let target = store.storage.path.join(name);
        if !target.exists() {
            write_private(&target, b"");
        }
        fs::set_permissions(&target, fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(store.load_consent(), Err(StatusStoreError::UnsafePath));
        assert_eq!(
            store.remove_cache(&SessionId::generate()),
            Err(StatusStoreError::UnsafePath)
        );
    }
    let (directory, mut store) = setup();
    fs::hard_link(
        store.storage.path.join("status.sqlite3"),
        directory.path().join("outside-link"),
    )
    .unwrap();
    assert_eq!(store.load_consent(), Err(StatusStoreError::UnsafePath));
    let (_directory, mut store) = setup();
    let database = store.storage.path.join("status.sqlite3");
    fs::rename(&database, store.storage.path.join("replaced")).unwrap();
    write_private(&database, b"");
    assert_eq!(store.load_consent(), Err(StatusStoreError::UnsafePath));
}

#[test]
fn private_foreign_wal_state_is_refused_without_opening_or_removing_it() {
    let _isolation = isolated_process_test();
    for name in ["status.sqlite3-wal", "status.sqlite3-shm"] {
        let (_directory, store) = setup();
        let path = store.storage.path.clone();
        drop(store);
        let target = path.join(name);
        write_private(&target, b"PRIVATE-FOREIGN-WAL");
        assert_eq!(open_error(&path), StatusStoreError::InvalidData);
        assert_eq!(fs::read(target).unwrap(), b"PRIVATE-FOREIGN-WAL");
    }
}

#[test]
fn missing_parent_is_not_created_and_existing_nondirectory_is_not_modified() {
    let _isolation = isolated_process_test();
    let directory = scratch();
    let missing = directory.path().join("missing").join("private-status");
    assert_eq!(
        open_error(&missing),
        StatusStoreError::Io(io::ErrorKind::NotFound)
    );
    assert!(!missing.parent().unwrap().exists());
    let file = directory.path().join("PRIVATE-not-a-directory");
    write_private(&file, b"PRIVATE-CONTENT");
    assert_eq!(open_error(&file), StatusStoreError::UnsafePath);
    assert_eq!(fs::read(file).unwrap(), b"PRIVATE-CONTENT");
}

#[test]
fn future_schema_is_refused_without_rewriting_database() {
    let _isolation = isolated_process_test();
    let (_directory, store) = setup();
    store
        .connection
        .pragma_update(None, "user_version", SCHEMA_VERSION + 1)
        .unwrap();
    let path = store.storage.path.clone();
    drop(store);
    let before = fs::read(path.join("status.sqlite3")).unwrap();
    let error = open_error(&path);
    assert_eq!(error, StatusStoreError::UnsupportedSchema);
    assert_redacted(&error, &path);
    assert_eq!(fs::read(path.join("status.sqlite3")).unwrap(), before);
}

fn assert_refusal_preserves_database_and_journal(path: &Path, expected: StatusStoreError) {
    let database = fs::read(path.join("status.sqlite3")).unwrap();
    let journal = fs::read(path.join("status.sqlite3-journal")).unwrap();
    assert_eq!(
        &journal[..8],
        &[0xd9, 0xd5, 0x05, 0xf9, 0x20, 0xa1, 0x63, 0xd7]
    );
    let error = open_error(path);
    assert_eq!(error, expected);
    assert_redacted(&error, path);
    // Compare bytes, not just the version, length or logical SQLite contents.
    assert!(fs::read(path.join("status.sqlite3")).unwrap() == database);
    assert!(fs::read(path.join("status.sqlite3-journal")).unwrap() == journal);
    assert_eq!(
        fs::read_dir(path).unwrap().count(),
        3,
        "scratch was removed"
    );
}

#[test]
fn preflight_future_schema_with_hot_journal_preserves_both_source_files() {
    let _isolation = isolated_process_test();
    let (directory, mut store) = setup();
    seed_cache(&mut store, directory.path(), 8);
    let path = store.storage.path.clone();
    drop(store);
    run_child(&path, "future-interrupt");
    let header = fs::read(path.join("status.sqlite3")).unwrap();
    assert_eq!(&header[60..64], &2u32.to_be_bytes());
    assert_refusal_preserves_database_and_journal(&path, StatusStoreError::UnsupportedSchema);
}

#[test]
fn preflight_committed_future_schema_behind_downgraded_header_preserves_source_files() {
    let _isolation = isolated_process_test();
    let (directory, mut store) = setup();
    seed_cache(&mut store, directory.path(), 8);
    let path = store.storage.path.clone();
    drop(store);
    run_child(&path, "future-downgrade-interrupt");
    let header = fs::read(path.join("status.sqlite3")).unwrap();
    assert_eq!(&header[60..64], &1u32.to_be_bytes());
    assert_refusal_preserves_database_and_journal(&path, StatusStoreError::UnsupportedSchema);
}

#[test]
fn preflight_uncommitted_future_header_recovers_the_supported_committed_snapshot() {
    let _isolation = isolated_process_test();
    let (directory, mut store) = setup();
    let entries = seed_cache(&mut store, directory.path(), 8);
    let snapshot = store.load_consent().unwrap();
    let path = store.storage.path.clone();
    drop(store);
    run_child(&path, "current-upgrade-interrupt");
    let header = fs::read(path.join("status.sqlite3")).unwrap();
    assert_eq!(&header[60..64], &2u32.to_be_bytes());
    let mut reopened = SqliteStatusStore::open(&path).unwrap();
    assert_eq!(reopened.load_consent().unwrap(), snapshot);
    for cached in entries {
        assert_eq!(
            reopened.load_cache(&cached.session_id).unwrap(),
            Some(cached)
        );
    }
    assert!(!path.join("status.sqlite3-journal").exists());
    assert_eq!(fs::read_dir(path).unwrap().count(), 2);
}

#[test]
fn preflight_invalid_committed_snapshot_preserves_database_and_hot_journal() {
    let _isolation = isolated_process_test();
    let (directory, mut store) = setup();
    seed_cache(&mut store, directory.path(), 8);
    let path = store.storage.path.clone();
    drop(store);
    run_child(&path, "invalid-interrupt");
    assert_refusal_preserves_database_and_journal(&path, StatusStoreError::InvalidData);
}

#[test]
fn preflight_database_and_journal_copies_have_an_explicit_hard_size_limit() {
    let _isolation = isolated_process_test();
    for name in ["status.sqlite3", "status.sqlite3-journal"] {
        let (_directory, store) = setup();
        let path = store.storage.path.clone();
        drop(store);
        let database_header = fs::read(path.join("status.sqlite3")).unwrap();
        let target = path.join(name);
        if !target.exists() {
            write_private(&target, b"PRIVATE-OVERSIZED-JOURNAL");
        }
        let file = OpenOptions::new().write(true).open(&target).unwrap();
        file.set_len(MAX_PREFLIGHT_FILE_BYTES + 1).unwrap();
        let error = open_error(&path);
        assert_eq!(error, StatusStoreError::InvalidData);
        assert_redacted(&error, &path);
        assert_eq!(
            fs::metadata(target).unwrap().len(),
            MAX_PREFLIGHT_FILE_BYTES + 1
        );
        let mut header = vec![0; database_header.len()];
        File::open(path.join("status.sqlite3"))
            .unwrap()
            .read_exact_at(&mut header, 0)
            .unwrap();
        assert!(header == database_header);
        assert_eq!(
            fs::read_dir(path).unwrap().count(),
            if name == "status.sqlite3" { 2 } else { 3 }
        );
    }
}

fn assert_orphan_journal_refusal(path: &Path, expected_journal: &[u8]) {
    let error = open_error(path);
    assert_eq!(error, StatusStoreError::InvalidData);
    assert_redacted(&error, path);
    assert_eq!(
        fs::symlink_metadata(path.join("status.sqlite3"))
            .unwrap_err()
            .kind(),
        io::ErrorKind::NotFound
    );
    let journal = File::open(path.join("status.sqlite3-journal")).unwrap();
    assert_eq!(
        journal.metadata().unwrap().len(),
        expected_journal.len() as u64
    );
    // Compare every byte in bounded chunks, including the oversized fixture, without
    // allocating a second full journal or printing its content on assertion failure.
    let mut actual = [0u8; 8192];
    for (index, expected) in expected_journal.chunks(actual.len()).enumerate() {
        journal
            .read_exact_at(&mut actual[..expected.len()], (index * 8192) as u64)
            .unwrap();
        assert!(&actual[..expected.len()] == expected);
    }
    assert_eq!(
        fs::read_dir(path).unwrap().count(),
        2,
        "only journal and owner lock remain"
    );
}

#[test]
fn orphan_oversized_journal_preserves_database_absence_and_journal_bytes() {
    let _isolation = isolated_process_test();
    let directory = scratch();
    let path = directory.path().join("private-status");
    fs::DirBuilder::new().mode(0o700).create(&path).unwrap();
    let journal_path = path.join("status.sqlite3-journal");
    write_private(&journal_path, b"PRIVATE-ORPHAN-JOURNAL");
    OpenOptions::new()
        .write(true)
        .open(&journal_path)
        .unwrap()
        .set_len(MAX_PREFLIGHT_FILE_BYTES + 1)
        .unwrap();
    let before = fs::read(&journal_path).unwrap();
    assert_orphan_journal_refusal(&path, &before);
}

#[test]
fn orphan_valid_hot_journal_preserves_database_absence_and_recovery_evidence() {
    let _isolation = isolated_process_test();
    let (directory, mut store) = setup();
    let entries = seed_cache(&mut store, directory.path(), 8);
    let snapshot = store.load_consent().unwrap();
    let path = store.storage.path.clone();
    drop(store);
    run_child(&path, "interrupt");
    let before = fs::read(path.join("status.sqlite3-journal")).unwrap();
    assert_eq!(
        &before[..8],
        &[0xd9, 0xd5, 0x05, 0xf9, 0x20, 0xa1, 0x63, 0xd7]
    );
    let moved_database = directory.path().join("saved-database");
    fs::rename(path.join("status.sqlite3"), &moved_database).unwrap();
    assert_orphan_journal_refusal(&path, &before);

    // Explicit test-fixture restoration proves the preserved journal really can
    // recover its original DB; the backend never guesses or performs this move.
    fs::rename(moved_database, path.join("status.sqlite3")).unwrap();
    let mut reopened = SqliteStatusStore::open(&path).unwrap();
    assert_eq!(reopened.load_consent().unwrap(), snapshot);
    for cached in entries {
        assert_eq!(
            reopened.load_cache(&cached.session_id).unwrap(),
            Some(cached)
        );
    }
}

#[test]
fn orphan_empty_and_stale_journals_are_never_initialized_away() {
    let _isolation = isolated_process_test();
    for bytes in [b"".as_slice(), b"PRIVATE-STALE-JOURNAL".as_slice()] {
        let directory = scratch();
        let path = directory.path().join("private-status");
        fs::DirBuilder::new().mode(0o700).create(&path).unwrap();
        write_private(&path.join("status.sqlite3-journal"), bytes);
        assert_orphan_journal_refusal(&path, bytes);
    }
}

#[test]
fn schema_zero_with_existing_tables_and_mutated_schema_one_are_refused() {
    let _isolation = isolated_process_test();
    let (_directory, store) = setup();
    store
        .connection
        .pragma_update(None, "user_version", 0)
        .unwrap();
    let path = store.storage.path.clone();
    drop(store);
    assert_eq!(open_error(&path), StatusStoreError::UnsupportedSchema);
    let (_directory, store) = setup();
    store
        .connection
        .execute_batch("CREATE TABLE PRIVATE_extra (value TEXT)")
        .unwrap();
    let path = store.storage.path.clone();
    drop(store);
    assert_eq!(open_error(&path), StatusStoreError::InvalidData);
}

#[test]
fn malformed_database_and_consent_are_static_redacted_errors() {
    let _isolation = isolated_process_test();
    let (_directory, store) = setup();
    let path = store.storage.path.clone();
    drop(store);
    write_private(&path.join("status.sqlite3"), b"PRIVATE-INVALID-SQLITE");
    let error = open_error(&path);
    assert_eq!(error, StatusStoreError::InvalidData);
    assert_redacted(&error, &path);
    for bytes in [
        b"PRIVATE-BROKEN-CONSENT".to_vec(),
        br#"{"state":"disabled","PRIVATE":"unexpected"}"#.to_vec(),
        br#"{"state":"enabled","account":{"issuer":"http://PRIVATE/","client_id":"client","subject":"user"},"auth_revision":1,"folders":[]}"#.to_vec(),
    ] {
        let (directory, mut store) = setup();
        enable(&mut store, directory.path());
        store.connection.execute("UPDATE status_consent SET policy_json=?1", [bytes]).unwrap();
        let error = store.load_consent().unwrap_err();
        assert_eq!(error, StatusStoreError::InvalidData);
        assert_redacted(&error, &store.storage.path);
        let path = store.storage.path.clone();
        drop(store);
        assert_eq!(open_error(&path), StatusStoreError::InvalidData);
    }
}

#[test]
fn oversized_and_wrong_sqlite_type_rows_are_rejected_before_json_decoding() {
    let _isolation = isolated_process_test();
    let (directory, mut store) = setup();
    let entries = seed_cache(&mut store, directory.path(), 1);
    store
        .connection
        .execute_batch("PRAGMA ignore_check_constraints=ON")
        .unwrap();
    store
        .connection
        .execute(
            "UPDATE status_consent SET policy_json=zeroblob(?1)",
            [MAX_CONSENT_BYTES as i64 + 1],
        )
        .unwrap();
    assert_eq!(store.load_consent(), Err(StatusStoreError::InvalidData));
    store
        .connection
        .execute(
            "UPDATE status_consent SET policy_json=?1",
            [encode_json(&policy(directory.path()), MAX_CONSENT_BYTES).unwrap()],
        )
        .unwrap();
    store
        .connection
        .execute(
            "UPDATE status_cache SET entry_json=zeroblob(?1)",
            [MAX_CACHE_BYTES as i64 + 1],
        )
        .unwrap();
    assert_eq!(
        store.load_cache(&entries[0].session_id),
        Err(StatusStoreError::InvalidData)
    );
    store
        .connection
        .execute(
            "UPDATE status_cache SET entry_json='PRIVATE-TEXT-NOT-BLOB'",
            [],
        )
        .unwrap();
    assert_eq!(
        store.load_cache(&entries[0].session_id),
        Err(StatusStoreError::InvalidData)
    );
    store
        .connection
        .execute(
            "UPDATE status_consent SET policy_json='PRIVATE-TEXT-NOT-BLOB'",
            [],
        )
        .unwrap();
    assert_eq!(store.load_consent(), Err(StatusStoreError::InvalidData));
}

#[test]
fn sqlite_value_limit_allows_full_consent_and_cache_values_with_record_overhead() {
    let _isolation = isolated_process_test();
    let (directory, mut store) = setup();
    let entries = seed_cache(&mut store, directory.path(), 1);
    assert_eq!(
        store.connection.limit(Limit::SQLITE_LIMIT_LENGTH),
        MAX_SQLITE_RECORD_BYTES
    );
    // These are intentionally not valid JSON: exercise the independent SQLite
    // record limit without weakening the stricter domain/JSON decoder bounds.
    store
        .connection
        .execute(
            "UPDATE status_consent SET policy_json=zeroblob(?1)",
            [MAX_CONSENT_BYTES as i64],
        )
        .unwrap();
    store
        .connection
        .execute(
            "UPDATE status_cache SET entry_json=zeroblob(?1)",
            [MAX_CACHE_BYTES as i64],
        )
        .unwrap();
    let consent_length: usize = store
        .connection
        .query_row(
            "SELECT length(policy_json) FROM status_consent",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let cache_length: usize = store
        .connection
        .query_row("SELECT length(entry_json) FROM status_cache", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(consent_length, MAX_CONSENT_BYTES);
    assert_eq!(cache_length, MAX_CACHE_BYTES);
    assert_eq!(store.load_consent(), Err(StatusStoreError::InvalidData));
    assert_eq!(
        store.load_cache(&entries[0].session_id),
        Err(StatusStoreError::InvalidData)
    );
}

#[test]
fn cache_schema_and_public_lookup_inputs_bound_session_id_bytes() {
    let _isolation = isolated_process_test();
    let (directory, mut store) = setup();
    let entries = seed_cache(&mut store, directory.path(), 1);
    let oversized = "x".repeat(257);
    assert!(store
        .connection
        .execute("UPDATE status_cache SET session_id=?1", [&oversized])
        .is_err());
    // 129 two-byte Unicode scalars must fail the byte bound, not pass a character bound.
    assert!(store
        .connection
        .execute("UPDATE status_cache SET session_id=?1", ["é".repeat(129)])
        .is_err());
    assert_eq!(
        store.load_cache(&SessionId::from_string(oversized.clone())),
        Err(StatusStoreError::InvalidData)
    );
    assert_eq!(
        store.remove_cache(&SessionId::from_string(oversized)),
        Err(StatusStoreError::InvalidData)
    );
    assert_eq!(
        store.load_cache(&entries[0].session_id).unwrap(),
        Some(entries[0].clone())
    );
}

#[test]
fn oversized_indexed_session_ids_are_guarded_before_large_sqlite_allocations() {
    let _isolation = isolated_process_test();
    // The smaller case fits SQLITE_LIMIT_LENGTH, so it tests octet_length's
    // no-materialization guard independently of the SQLite record-size ceiling.
    for size in [1024 * 1024, 32 * 1024 * 1024] {
        let (directory, mut store) = setup();
        seed_cache(&mut store, directory.path(), 1);
        let path = store.storage.path.clone();
        drop(store);
        let external = Connection::open(path.join("status.sqlite3")).unwrap();
        external
            .execute_batch("PRAGMA ignore_check_constraints=ON")
            .unwrap();
        external
            .execute(
                "UPDATE status_cache SET session_id=CAST('PRIVATE-INDEXED-ID' || zeroblob(?1) AS TEXT)",
                [size],
            )
            .unwrap();
        drop(external);
        run_child(
            &path,
            if size == 1024 * 1024 {
                "guarded-indexed-id"
            } else {
                "oversized-indexed-id"
            },
        );
    }
}

#[test]
fn malformed_cache_payload_binding_text_and_index_metadata_never_escape() {
    let _isolation = isolated_process_test();
    for tamper in 0..8 {
        let (directory, mut store) = setup();
        let entries = seed_cache(&mut store, directory.path(), 1);
        let mut value = serde_json::to_value(&entries[0]).unwrap();
        match tamper {
            0 => value["account"]["subject"] = serde_json::json!("PRIVATE-other-user"),
            1 => value["consent_revision"] = serde_json::json!(22),
            2 => value["stamp"]["source_revision"] = serde_json::json!(0),
            3 => value["text"] = serde_json::json!("PRIVATE\ninvalid text"),
            4 => value["PRIVATE-raw-output"] = serde_json::json!("forbidden"),
            5 => value["captured_at"] = serde_json::json!(999),
            6 => value["session_id"] = serde_json::json!(SessionId::generate()),
            _ => value["auth_revision"] = serde_json::json!(8),
        }
        store
            .connection
            .execute(
                "UPDATE status_cache SET entry_json=?1",
                [serde_json::to_vec(&value).unwrap()],
            )
            .unwrap();
        let error = store.load_cache(&entries[0].session_id).unwrap_err();
        assert_eq!(error, StatusStoreError::InvalidData);
        assert_redacted(&error, &store.storage.path);
        let path = store.storage.path.clone();
        drop(store);
        assert_eq!(open_error(&path), StatusStoreError::InvalidData);
    }
}

#[test]
fn invalid_cache_and_policy_inputs_leave_the_previous_records_unchanged() {
    let _isolation = isolated_process_test();
    let (directory, mut store) = setup();
    let entries = seed_cache(&mut store, directory.path(), 1);
    let snapshot = store.load_consent().unwrap();
    let mut invalid = entries[0].clone();
    invalid.stamp.terminal_generation = 0;
    assert_eq!(
        store.save_cache(snapshot.revision, &invalid),
        Err(StatusStoreError::InvalidData)
    );
    invalid = entries[0].clone();
    invalid.captured_at = u64::MAX;
    assert_eq!(
        store.save_cache(snapshot.revision, &invalid),
        Err(StatusStoreError::InvalidData)
    );
    invalid = entries[0].clone();
    invalid.session_id = SessionId::from_string("x".repeat(MAX_CACHE_BYTES));
    assert_eq!(
        store.save_cache(snapshot.revision, &invalid),
        Err(StatusStoreError::InvalidData)
    );
    let mut invalid_policy = snapshot.policy.clone();
    if let ConsentPolicy::Enabled { folders, .. } = &mut invalid_policy {
        folders.clear();
    }
    assert_eq!(
        store.compare_and_swap_consent(snapshot.revision, &invalid_policy),
        Err(StatusStoreError::InvalidData)
    );
    assert_eq!(store.load_consent().unwrap(), snapshot);
    assert_eq!(
        store.load_cache(&entries[0].session_id).unwrap(),
        Some(entries[0].clone())
    );
}

#[test]
fn revision_exhaustion_is_checked_without_wrapping_or_clearing_cache() {
    let _isolation = isolated_process_test();
    let (directory, mut store) = setup();
    let snapshot = enable(&mut store, directory.path());
    store
        .connection
        .execute("UPDATE status_consent SET revision=?1", [i64::MAX])
        .unwrap();
    let mut cached = entry(directory.path(), ConsentRevision(i64::MAX as u64));
    store.save_cache(cached.consent_revision, &cached).unwrap();
    assert_eq!(
        store.compare_and_swap_consent(cached.consent_revision, &snapshot.policy),
        Err(StatusStoreError::RevisionExhausted)
    );
    assert_eq!(
        store.load_consent().unwrap().revision,
        cached.consent_revision
    );
    assert_eq!(
        store.load_cache(&cached.session_id).unwrap(),
        Some(cached.clone())
    );
    cached.consent_revision = ConsentRevision(u64::MAX);
    assert_eq!(
        store.save_cache(cached.consent_revision, &cached),
        Err(StatusStoreError::Conflict)
    );
    assert_eq!(
        store.compare_and_swap_consent(ConsentRevision(u64::MAX), &ConsentPolicy::Disabled),
        Err(StatusStoreError::Conflict)
    );
}

#[test]
fn cache_eviction_is_bounded_oldest_first_and_deterministic_for_equal_times() {
    let _isolation = isolated_process_test();
    let (directory, mut store) = setup();
    let mut entries = seed_cache(&mut store, directory.path(), MAX_CACHE_ENTRIES + 2);
    assert_eq!(store.load_cache(&entries[0].session_id).unwrap(), None);
    assert_eq!(store.load_cache(&entries[1].session_id).unwrap(), None);
    for cached in &entries[2..] {
        assert_eq!(
            store.load_cache(&cached.session_id).unwrap(),
            Some(cached.clone())
        );
    }
    let count: i64 = store
        .connection
        .query_row("SELECT COUNT(*) FROM status_cache", [], |row| row.get(0))
        .unwrap();
    assert_eq!(count, MAX_CACHE_ENTRIES as i64);
    let snapshot = store.load_consent().unwrap();
    let snapshot = store
        .compare_and_swap_consent(snapshot.revision, &snapshot.policy)
        .unwrap();
    for cached in &mut entries {
        cached.consent_revision = snapshot.revision;
        cached.captured_at = 500;
    }
    entries.sort_by(|left, right| left.session_id.cmp(&right.session_id));
    // Insert in reverse order so insertion order cannot accidentally satisfy the tie-break.
    for cached in entries.iter().rev() {
        store.save_cache(snapshot.revision, cached).unwrap();
    }
    for (index, cached) in entries.iter().enumerate() {
        assert_eq!(
            store.load_cache(&cached.session_id).unwrap().is_some(),
            index >= 2
        );
    }
}

#[test]
fn failed_cache_clear_rolls_back_consent_revision_and_every_cache_row() {
    let _isolation = isolated_process_test();
    let (directory, mut store) = setup();
    let entries = seed_cache(&mut store, directory.path(), 2);
    let before = store.load_consent().unwrap();
    store
        .connection
        .execute_batch(
            "CREATE TRIGGER fail_cache_clear BEFORE DELETE ON status_cache
        BEGIN SELECT RAISE(ABORT, 'PRIVATE-SQL-ERROR-CONTENT'); END",
        )
        .unwrap();
    let error = store
        .compare_and_swap_consent(before.revision, &ConsentPolicy::Disabled)
        .unwrap_err();
    assert_eq!(error, StatusStoreError::Database);
    assert_redacted(&error, &store.storage.path);
    assert_eq!(store.load_consent().unwrap(), before);
    for cached in &entries {
        assert_eq!(
            store.load_cache(&cached.session_id).unwrap(),
            Some(cached.clone())
        );
    }
    store
        .connection
        .execute_batch("DROP TRIGGER fail_cache_clear")
        .unwrap();
    let path = store.storage.path.clone();
    drop(store);
    let mut reopened = SqliteStatusStore::open(path).unwrap();
    assert_eq!(reopened.load_consent().unwrap(), before);
    for cached in entries {
        assert_eq!(
            reopened.load_cache(&cached.session_id).unwrap(),
            Some(cached)
        );
    }
}

#[test]
fn failed_eviction_rolls_back_the_new_upsert_in_the_same_transaction() {
    let _isolation = isolated_process_test();
    let (directory, mut store) = setup();
    let entries = seed_cache(&mut store, directory.path(), MAX_CACHE_ENTRIES);
    let revision = store.load_consent().unwrap().revision;
    store
        .connection
        .execute_batch(
            "CREATE TRIGGER fail_eviction BEFORE DELETE ON status_cache
        BEGIN SELECT RAISE(ABORT, 'PRIVATE-EVICTION-ERROR'); END",
        )
        .unwrap();
    let mut incoming = entry(directory.path(), revision);
    incoming.captured_at = 1000;
    assert_eq!(
        store.save_cache(revision, &incoming),
        Err(StatusStoreError::Database)
    );
    assert_eq!(store.load_cache(&incoming.session_id).unwrap(), None);
    assert_eq!(
        store.load_cache(&entries[0].session_id).unwrap(),
        Some(entries[0].clone())
    );
    let count: i64 = store
        .connection
        .query_row("SELECT COUNT(*) FROM status_cache", [], |row| row.get(0))
        .unwrap();
    assert_eq!(count, MAX_CACHE_ENTRIES as i64);
}

fn run_child(path: &Path, action: &str) {
    let result = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "status::sqlite_store::tests::sqlite_store_process_helper",
            "--nocapture",
        ])
        .env("SENSAGENT_STATUS_TEST_DIRECTORY", path)
        .env("SENSAGENT_STATUS_TEST_ACTION", action)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "isolated SQLite helper failed: {}",
        String::from_utf8_lossy(&result.stderr)
    );
}

#[test]
fn lifetime_lock_excludes_a_separate_process() {
    let _isolation = isolated_process_test();
    let (_directory, store) = setup();
    run_child(&store.storage.path, "busy");
}

#[test]
fn interrupted_process_leaves_a_hot_journal_that_rolls_back_on_reopen() {
    let _isolation = isolated_process_test();
    let (directory, mut store) = setup();
    let entries = seed_cache(&mut store, directory.path(), 8);
    let snapshot = store.load_consent().unwrap();
    let path = store.storage.path.clone();
    drop(store);
    run_child(&path, "interrupt");
    let journal = path.join("status.sqlite3-journal");
    assert!(journal.is_file());
    // A synced journal header proves this was not merely a discarded dirty page cache.
    assert_eq!(
        &fs::read(&journal).unwrap()[..8],
        &[0xd9, 0xd5, 0x05, 0xf9, 0x20, 0xa1, 0x63, 0xd7]
    );
    let mut reopened = SqliteStatusStore::open(&path).unwrap();
    assert_eq!(reopened.load_consent().unwrap(), snapshot);
    for cached in entries {
        assert_eq!(
            reopened.load_cache(&cached.session_id).unwrap(),
            Some(cached)
        );
    }
    assert!(!journal.exists());
}

#[test]
fn sqlite_store_process_helper() {
    let _isolation = isolated_process_test();
    let Some(path) = std::env::var_os("SENSAGENT_STATUS_TEST_DIRECTORY") else {
        return;
    };
    let path = PathBuf::from(path);
    match std::env::var("SENSAGENT_STATUS_TEST_ACTION")
        .unwrap()
        .as_str()
    {
        "busy" => assert_eq!(open_error(&path), StatusStoreError::Busy),
        "interrupt" => {
            let store = SqliteStatusStore::open(&path).unwrap();
            store
                .connection
                .execute_batch(
                    "PRAGMA cache_size=1; BEGIN IMMEDIATE;
                UPDATE status_consent SET revision=revision+1;
                UPDATE status_cache SET entry_json=zeroblob(65536);",
                )
                .unwrap();
            // Exit without destructors, modeling a crash after spilling uncommitted pages.
            std::process::exit(0);
        }
        "future-interrupt"
        | "future-downgrade-interrupt"
        | "current-upgrade-interrupt"
        | "invalid-interrupt" => {
            let store = SqliteStatusStore::open(&path).unwrap();
            let action = std::env::var("SENSAGENT_STATUS_TEST_ACTION").unwrap();
            if action.starts_with("future-") {
                store
                    .connection
                    .pragma_update(None, "user_version", SCHEMA_VERSION + 1)
                    .unwrap();
            }
            if action == "invalid-interrupt" {
                store
                    .connection
                    .execute(
                        "UPDATE status_consent SET policy_json=?1",
                        [b"PRIVATE-BROKEN-COMMITTED-CONSENT".as_slice()],
                    )
                    .unwrap();
            }
            store
                .connection
                .execute_batch("PRAGMA cache_size=1; BEGIN IMMEDIATE")
                .unwrap();
            if action == "future-downgrade-interrupt" {
                store
                    .connection
                    .pragma_update(None, "user_version", SCHEMA_VERSION)
                    .unwrap();
            } else if action == "current-upgrade-interrupt" {
                store
                    .connection
                    .pragma_update(None, "user_version", SCHEMA_VERSION + 1)
                    .unwrap();
            }
            store
                .connection
                .execute_batch(
                    "UPDATE status_consent SET revision=revision+1;
                UPDATE status_cache SET entry_json=zeroblob(65536)",
                )
                .unwrap();
            store.connection.cache_flush().unwrap();
            let spilled_version = match action.as_str() {
                "future-downgrade-interrupt" => Some(1u32),
                "current-upgrade-interrupt" => Some(2u32),
                _ => None,
            };
            if let Some(version) = spilled_version {
                assert_eq!(
                    store
                        .connection
                        .query_row("PRAGMA user_version", [], |row| row.get::<_, u32>(0))
                        .unwrap(),
                    version
                );
                // Model an interrupted page-one spill. SQLite normally pins that
                // page until commit, so cache_flush alone does not spill its header.
                // Only the documented header field is written here; SQLite created
                // the actual rollback journal and will interpret/recover it itself.
                let database = OpenOptions::new()
                    .write(true)
                    .open(path.join("status.sqlite3"))
                    .unwrap();
                database.write_all_at(&version.to_be_bytes(), 60).unwrap();
                database.sync_all().unwrap();
            }
            std::process::exit(0);
        }
        "guarded-indexed-id" | "oversized-indexed-id" => {
            let mut current = 0;
            let mut largest = 0;
            // SAFETY: both outputs point to live i64 values. This child owns its
            // SQLite allocator counters; fixture construction ran in the parent.
            assert_eq!(
                unsafe {
                    rusqlite::ffi::sqlite3_status64(
                        rusqlite::ffi::SQLITE_STATUS_MALLOC_SIZE,
                        &mut current,
                        &mut largest,
                        1,
                    )
                },
                rusqlite::ffi::SQLITE_OK
            );
            let error = open_error(&path);
            assert_eq!(error, StatusStoreError::InvalidData);
            assert_redacted(&error, &path);
            // Deliberately bypass startup validation to exercise the lookup guard
            // too, as if an external same-UID writer inserted this malformed row.
            let storage = PrivateStorage::open(&path).unwrap();
            let connection = open_connection(&path).unwrap();
            assert_eq!(
                connection.limit(Limit::SQLITE_LIMIT_LENGTH),
                MAX_SQLITE_RECORD_BYTES
            );
            let mut store = SqliteStatusStore {
                connection,
                storage,
            };
            let result = store.load_cache(&SessionId::from_string("synthetic-lookup"));
            assert!(matches!(
                result,
                Ok(None) | Err(StatusStoreError::InvalidData)
            ));
            drop(store);
            // SAFETY: same valid output pointers; read only the synthetic child's counter.
            assert_eq!(
                unsafe {
                    rusqlite::ffi::sqlite3_status64(
                        rusqlite::ffi::SQLITE_STATUS_MALLOC_SIZE,
                        &mut current,
                        &mut largest,
                        0,
                    )
                },
                rusqlite::ffi::SQLITE_OK
            );
            let guarded =
                std::env::var("SENSAGENT_STATUS_TEST_ACTION").unwrap() == "guarded-indexed-id";
            let maximum = if guarded {
                512 * 1024
            } else {
                MAX_SQLITE_RECORD_BYTES as i64 + 4096
            };
            assert!(
                largest <= maximum,
                "indexed ID caused an oversized SQLite allocation"
            );
            assert!(largest > 0, "SQLite allocator instrumentation was inactive");
        }
        _ => panic!("unknown isolated SQLite test action"),
    }
}
