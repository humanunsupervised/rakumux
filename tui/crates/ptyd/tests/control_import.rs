//! Importing archived sessions never replaces existing records or starts
//! programs. Source archives and unrelated live sessions stay untouched.

mod control_common;

use control_common::*;
use ptyd::control_api::Control;
use ptyd::persist::{ScrollbackStore, SessionMeta};
use session_core::api::{ErrorCode, Request, Response};
use session_core::model::SessionStatus;
use session_core::paths::Layout;
use std::fs;
use std::os::unix::fs::{symlink, PermissionsExt};

fn archive(h: &Harness) -> tempfile::TempDir {
    let archive = tempfile::tempdir().unwrap();
    let path = archive.path().join("registry.sqlite3");
    h.db()
        .execute("VACUUM INTO ?1", [path.to_str().unwrap()])
        .unwrap();
    archive
}

fn import(h: &Harness, archive: &tempfile::TempDir) -> Response {
    h.handle(
        &h.user,
        Request::SessionImport {
            source: archive.path().to_path_buf(),
        },
    )
}

#[test]
fn imports_ids_metadata_history_and_saved_output_without_starting_programs() {
    let source = Harness::new();
    let original = source.create(Some("original"));
    source
        .db()
        .execute(
            "UPDATE sessions_v2 SET notes = 'keep notes', pinned = 1 WHERE id = ?1",
            [original.id.as_str()],
        )
        .unwrap();
    let archive = archive(&source);
    let layout = Layout::with_home(archive.path().to_path_buf());
    ScrollbackStore::new(layout.clone())
        .save(
            &original.id,
            b"saved output\r\n",
            &SessionMeta {
                updated_at: 1,
                ..SessionMeta::default()
            },
        )
        .unwrap();
    fs::create_dir(archive.path().join("history")).unwrap();
    fs::write(layout.history_file(&original.id), b"cargo test\n").unwrap();
    let target = Harness::new();
    let existing = target.create(Some("existing"));
    let imported = match import(&target, &archive) {
        Response::Sessions { sessions } => sessions,
        response => panic!("import failed: {response:?}"),
    };
    assert_eq!(imported.len(), 1);
    assert_eq!(imported[0].id, original.id);
    assert_eq!(imported[0].scope, original.scope);
    assert_eq!(imported[0].status, SessionStatus::Dead);
    assert!(imported[0].pinned);
    assert_eq!(imported[0].notes.as_deref(), Some("keep notes"));
    assert_eq!(target.get(&existing.id).terminal, existing.terminal);
    assert_eq!(
        fs::read(target.layout.history_file(&original.id)).unwrap(),
        b"cargo test\n"
    );
    let saved = ScrollbackStore::new(target.layout.clone())
        .load(&original.id)
        .unwrap()
        .unwrap();
    assert_eq!(saved.scrollback, b"saved output\r\n");
    assert_eq!(
        fs::metadata(target.layout.session_dir(&original.id))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    assert_eq!(
        fs::metadata(target.layout.history_file(&original.id))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    assert_eq!(source.get(&original.id).status, SessionStatus::Live);
    assert!(target
        .audit()
        .iter()
        .any(|entry| entry.method == "session.import" && entry.decision == "allowed"));
    assert_eq!(
        error(import(&target, &archive)).code,
        ErrorCode::InvalidArgument
    );
    assert_eq!(target.get(&existing.id).status, SessionStatus::Live);
}

#[test]
fn restores_a_session_that_changed_directory_after_creation() {
    let source = Harness::new();
    let original = source.create(Some("moved"));
    let cwd = source.other_scope().path().to_path_buf();
    source
        .db()
        .execute(
            "UPDATE sessions_v2 SET cwd = ?2 WHERE id = ?1",
            [original.id.as_str(), cwd.to_str().unwrap()],
        )
        .unwrap();
    let archive = archive(&source);
    let target = Harness::new();
    assert!(matches!(
        import(&target, &archive),
        Response::Sessions { .. }
    ));
    let restored = target.get(&original.id);
    assert_eq!(restored.scope, original.scope);
    assert_eq!(restored.cwd, cwd);
}

#[test]
fn session_callers_cannot_import_or_read_an_archive() {
    let h = Harness::new();
    let session = h.create(None);
    let caller = h.session_caller(&session.id);
    let response = h.handle(
        &caller,
        Request::SessionImport {
            source: h.root().join("missing"),
        },
    );
    assert_eq!(error(response).code, ErrorCode::PermissionDenied);
    assert!(h
        .audit()
        .iter()
        .any(|entry| entry.method == "session.import" && entry.decision == "denied"));
}

#[test]
fn name_conflicts_refuse_the_whole_archive_without_overwriting() {
    let source = Harness::new();
    source.create(Some("conflict"));
    let second = source.create(Some("second"));
    let archive = archive(&source);
    let target = Harness::new();
    let existing = target.create_in(&source.scope, Some("conflict"));
    assert_eq!(
        error(import(&target, &archive)).code,
        ErrorCode::InvalidArgument
    );
    assert_eq!(target.get(&existing.id).status, SessionStatus::Live);
    assert!(!target.layout.session_dir(&second.id).exists());
    assert_eq!(target.control.session_count(), 1);
}

#[test]
fn invalid_archive_files_roll_back_only_new_files() {
    let source = Harness::new();
    let original = source.create(None);
    let archive = archive(&source);
    let session_dir = archive.path().join("sessions").join(original.id.as_str());
    fs::create_dir_all(&session_dir).unwrap();
    fs::write(session_dir.join("scrollback.ansi"), b"saved output").unwrap();
    let outside = tempfile::NamedTempFile::new().unwrap();
    symlink(outside.path(), session_dir.join("meta.json")).unwrap();
    let target = Harness::new();
    assert_eq!(
        error(import(&target, &archive)).code,
        ErrorCode::InvalidArgument
    );
    assert_eq!(target.control.session_count(), 0);
    assert!(!target.layout.session_dir(&original.id).exists());
    assert!(session_dir.join("scrollback.ansi").is_file());
    assert!(outside.path().exists());
}
