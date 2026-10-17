//! Saved scrollback and metadata per session, for showing and reviving
//! sessions whose terminal is gone: `sessions/<id>/scrollback.ansi` and
//! `sessions/<id>/meta.json`. Writes are atomic (temp file + rename).
//!
//! Directories are created with mode 0700 and files with mode 0600: saved
//! scrollback can contain anything the session printed. Session ids are
//! validated before they become path components, so a hostile id can't
//! escape `sessions/`.

use serde::{Deserialize, Serialize};
use session_core::ids::SessionId;
use session_core::paths::Layout;
use std::fs::{self, DirBuilder, OpenOptions, Permissions};
use std::io::{self, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

const SCROLLBACK_FILE: &str = "scrollback.ansi";
const META_FILE: &str = "meta.json";
const DIR_MODE: u32 = 0o700;
const FILE_MODE: u32 = 0o600;
/// Longest id accepted as a directory name (ULIDs are 26 characters).
const MAX_ID_LEN: usize = 128;

/// Distinguishes temp files of concurrent saves within this process.
static TEMP_SEQ: AtomicU64 = AtomicU64::new(0);

/// What the daemon knew about a session's terminal when it last saved it.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct SessionMeta {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<PathBuf>,
    #[serde(default)]
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub foreground_process: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    /// Unix seconds.
    pub updated_at: i64,
}

/// A session's saved terminal output and metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SavedSession {
    /// ANSI-formatted rows separated by `\r\n`, oldest first.
    pub scrollback: Vec<u8>,
    pub meta: SessionMeta,
}

/// Per-session scrollback files under `Layout::session_dir`.
#[derive(Debug, Clone)]
pub struct ScrollbackStore {
    layout: Layout,
}

impl ScrollbackStore {
    pub fn new(layout: Layout) -> Self {
        Self { layout }
    }

    /// Replace the session's saved scrollback and metadata. Each file is
    /// written to a temp file, synced and renamed into place, so readers
    /// never see a partial file. The scrollback is written before the
    /// metadata.
    pub fn save(&self, id: &SessionId, scrollback: &[u8], meta: &SessionMeta) -> io::Result<()> {
        let dir = self.dir_for(id)?;
        let meta_json = serde_json::to_vec_pretty(meta)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        if let Some(sessions) = dir.parent() {
            ensure_private_dir(sessions)?;
        }
        ensure_private_dir(&dir)?;
        write_atomic(&dir.join(SCROLLBACK_FILE), scrollback)?;
        write_atomic(&dir.join(META_FILE), &meta_json)
    }

    /// The session's saved state, or `Ok(None)` when nothing was saved.
    ///
    /// A save interrupted between its two renames (daemon killed) leaves one
    /// file without the other; the missing half reads as empty scrollback or
    /// default metadata rather than discarding what was saved. A metadata
    /// file that isn't valid JSON is an `InvalidData` error.
    pub fn load(&self, id: &SessionId) -> io::Result<Option<SavedSession>> {
        let dir = self.dir_for(id)?;
        let scrollback = read_optional(&dir.join(SCROLLBACK_FILE))?;
        let meta = read_optional(&dir.join(META_FILE))?;
        if scrollback.is_none() && meta.is_none() {
            return Ok(None);
        }
        let meta = match meta {
            Some(raw) => serde_json::from_slice(&raw).map_err(|e| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("{}: {e}", dir.join(META_FILE).display()),
                )
            })?,
            None => SessionMeta::default(),
        };
        Ok(Some(SavedSession {
            scrollback: scrollback.unwrap_or_default(),
            meta,
        }))
    }

    /// Remove the session's directory. Missing is not an error.
    pub fn remove(&self, id: &SessionId) -> io::Result<()> {
        let dir = self.dir_for(id)?;
        match fs::remove_dir_all(&dir) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            other => other,
        }
    }

    fn dir_for(&self, id: &SessionId) -> io::Result<PathBuf> {
        validate_id(id)?;
        Ok(self.layout.session_dir(id))
    }
}

/// Ids are opaque, but they become directory names: allow only characters
/// that can't form a path separator or `..`.
fn validate_id(id: &SessionId) -> io::Result<()> {
    let value = id.as_str();
    let valid = !value.is_empty()
        && value.len() <= MAX_ID_LEN
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
    if valid {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("invalid session id {value:?}"),
        ))
    }
}

/// Create `dir` (and missing parents) with mode 0700 and make sure `dir`
/// itself is 0700 even if it already existed.
fn ensure_private_dir(dir: &Path) -> io::Result<()> {
    DirBuilder::new()
        .recursive(true)
        .mode(DIR_MODE)
        .create(dir)?;
    let meta = fs::metadata(dir)?;
    if meta.permissions().mode() & 0o7777 != DIR_MODE {
        fs::set_permissions(dir, Permissions::from_mode(DIR_MODE))?;
    }
    Ok(())
}

/// Write `bytes` to `path` via a synced temp file in the same directory and
/// a rename. The temp file is removed if anything fails.
pub(crate) fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "path has no file name"))?;
    let temp = path.with_file_name(format!(
        ".{name}.{}.{}.tmp",
        std::process::id(),
        TEMP_SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(FILE_MODE)
            .open(&temp)?;
        // The umask may have narrowed the mode; make it exact.
        file.set_permissions(Permissions::from_mode(FILE_MODE))?;
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(&temp, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

fn read_optional(path: &Path) -> io::Result<Option<Vec<u8>>> {
    match fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> (tempfile::TempDir, ScrollbackStore) {
        let home = tempfile::tempdir().unwrap();
        let layout = Layout::with_home(home.path().join("rakumux"));
        (home, ScrollbackStore::new(layout))
    }

    fn meta() -> SessionMeta {
        SessionMeta {
            cwd: Some("/work/project".into()),
            title: "✳ Claude Code".into(),
            foreground_process: Some("claude".into()),
            exit_code: Some(3),
            updated_at: 1_760_000_000,
        }
    }

    fn mode(path: &Path) -> u32 {
        fs::metadata(path).unwrap().permissions().mode() & 0o7777
    }

    #[test]
    fn save_then_load_round_trips() {
        let (_home, store) = store();
        let id = SessionId::generate();
        let scrollback = b"\x1b[1mhello\x1b[0m\r\nworld \xe2\x9c\xb3\r\n".to_vec();
        store.save(&id, &scrollback, &meta()).unwrap();
        let saved = store.load(&id).unwrap().unwrap();
        assert_eq!(saved.scrollback, scrollback);
        assert_eq!(saved.meta, meta());
    }

    #[test]
    fn missing_session_loads_as_none() {
        let (_home, store) = store();
        assert_eq!(store.load(&SessionId::generate()).unwrap(), None);
    }

    #[test]
    fn save_overwrites_previous_state() {
        let (_home, store) = store();
        let id = SessionId::generate();
        store.save(&id, b"first", &meta()).unwrap();
        let newer = SessionMeta {
            exit_code: None,
            updated_at: 1_760_000_100,
            ..meta()
        };
        store.save(&id, b"second", &newer).unwrap();
        let saved = store.load(&id).unwrap().unwrap();
        assert_eq!(saved.scrollback, b"second");
        assert_eq!(saved.meta, newer);
    }

    #[test]
    fn files_are_private_and_no_temp_files_remain() {
        let (_home, store) = store();
        let id = SessionId::generate();
        store.save(&id, b"data", &meta()).unwrap();
        store.save(&id, b"data again", &meta()).unwrap();
        let dir = store.layout.session_dir(&id);
        assert_eq!(mode(&dir), 0o700);
        assert_eq!(mode(dir.parent().unwrap()), 0o700);
        assert_eq!(mode(&dir.join(SCROLLBACK_FILE)), 0o600);
        assert_eq!(mode(&dir.join(META_FILE)), 0o600);
        let mut names: Vec<String> = fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert_eq!(
            names,
            vec![META_FILE.to_string(), SCROLLBACK_FILE.to_string()]
        );
    }

    #[test]
    fn existing_directories_are_tightened_to_0700() {
        let (_home, store) = store();
        let id = SessionId::generate();
        let dir = store.layout.session_dir(&id);
        fs::create_dir_all(&dir).unwrap();
        fs::set_permissions(&dir, Permissions::from_mode(0o755)).unwrap();
        store.save(&id, b"x", &meta()).unwrap();
        assert_eq!(mode(&dir), 0o700);
    }

    #[test]
    fn remove_deletes_the_directory_and_tolerates_missing() {
        let (_home, store) = store();
        let id = SessionId::generate();
        store.save(&id, b"data", &meta()).unwrap();
        store.remove(&id).unwrap();
        assert!(!store.layout.session_dir(&id).exists());
        assert_eq!(store.load(&id).unwrap(), None);
        store.remove(&id).unwrap();
        store.remove(&SessionId::generate()).unwrap();
    }

    #[test]
    fn partial_save_reads_the_half_that_exists() {
        let (_home, store) = store();
        let id = SessionId::generate();
        store.save(&id, b"rows", &meta()).unwrap();
        let dir = store.layout.session_dir(&id);

        fs::remove_file(dir.join(META_FILE)).unwrap();
        let saved = store.load(&id).unwrap().unwrap();
        assert_eq!(saved.scrollback, b"rows");
        assert_eq!(saved.meta, SessionMeta::default());

        store.save(&id, b"rows", &meta()).unwrap();
        fs::remove_file(dir.join(SCROLLBACK_FILE)).unwrap();
        let saved = store.load(&id).unwrap().unwrap();
        assert!(saved.scrollback.is_empty());
        assert_eq!(saved.meta, meta());
    }

    #[test]
    fn corrupt_metadata_is_an_error() {
        let (_home, store) = store();
        let id = SessionId::generate();
        store.save(&id, b"rows", &meta()).unwrap();
        fs::write(store.layout.session_dir(&id).join(META_FILE), b"{not json").unwrap();
        let err = store.load(&id).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn metadata_omits_absent_fields_and_tolerates_missing_ones() {
        let minimal = SessionMeta {
            updated_at: 5,
            ..SessionMeta::default()
        };
        let json = serde_json::to_string(&minimal).unwrap();
        assert_eq!(json, r#"{"title":"","updated_at":5}"#);
        let back: SessionMeta = serde_json::from_str(r#"{"updated_at":5}"#).unwrap();
        assert_eq!(back, minimal);
    }

    #[test]
    fn ids_that_could_escape_the_sessions_dir_are_rejected() {
        let (home, store) = store();
        for bad in [
            "",
            "..",
            ".",
            "../escape",
            "a/b",
            "a\\b",
            "nul\0byte",
            "über",
        ] {
            let id = SessionId::from_string(bad);
            let err = store.save(&id, b"x", &meta()).unwrap_err();
            assert_eq!(err.kind(), io::ErrorKind::InvalidInput, "{bad:?}");
            assert_eq!(
                store.load(&id).unwrap_err().kind(),
                io::ErrorKind::InvalidInput
            );
            assert_eq!(
                store.remove(&id).unwrap_err().kind(),
                io::ErrorKind::InvalidInput
            );
        }
        let long = SessionId::from_string("A".repeat(MAX_ID_LEN + 1));
        assert!(store.save(&long, b"x", &meta()).is_err());
        assert!(!home.path().join("escape").exists());
    }

    #[test]
    fn concurrent_saves_leave_one_complete_version() {
        let (_home, store) = store();
        let id = SessionId::generate();
        let threads: Vec<_> = (0..8)
            .map(|n| {
                let store = store.clone();
                let id = id.clone();
                std::thread::spawn(move || {
                    let body = vec![b'a' + n as u8; 64 * 1024];
                    for _ in 0..10 {
                        store.save(&id, &body, &meta()).unwrap();
                    }
                })
            })
            .collect();
        for t in threads {
            t.join().unwrap();
        }
        let saved = store.load(&id).unwrap().unwrap();
        assert_eq!(saved.scrollback.len(), 64 * 1024);
        assert!(saved.scrollback.iter().all(|b| *b == saved.scrollback[0]));
        assert_eq!(
            fs::read_dir(store.layout.session_dir(&id)).unwrap().count(),
            2
        );
    }
}
