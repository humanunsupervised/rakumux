use std::ffi::CStr;
use std::fs::{self, File, Metadata, OpenOptions};
use std::io::{self, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::fs::{DirBuilderExt, FileExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use rusqlite::config::DbConfig;
use rusqlite::limits::Limit;
use rusqlite::types::ValueRef;
use rusqlite::{params, Connection, OpenFlags, Row, Transaction, TransactionBehavior};
use serde::{Deserialize, Serialize};
use session_core::ids::SessionId;

use super::model::validate_session;
use super::{
    CachedStatus, ConsentPolicy, ConsentRevision, ConsentSnapshot, StatusStore, StatusStoreError,
    MAX_CACHE_ENTRIES,
};

const DATABASE_NAME: &CStr = c"status.sqlite3";
const LOCK_NAME: &CStr = c"status.lock";
const JOURNAL_NAME: &CStr = c"status.sqlite3-journal";
const SIDECARS: [&str; 3] = [
    "status.sqlite3-journal",
    "status.sqlite3-wal",
    "status.sqlite3-shm",
];
const SCHEMA_VERSION: i64 = 1;
const MAX_CONSENT_BYTES: usize = 2 * 1024 * 1024;
const MAX_CACHE_BYTES: usize = 64 * 1024;
// A full consent BLOB plus SQLite record headers and integer columns must fit.
const MAX_SQLITE_RECORD_BYTES: i32 = 2 * 1024 * 1024 + 4096;
const MAX_PREFLIGHT_FILE_BYTES: u64 = 64 * 1024 * 1024;
const BUSY_TIMEOUT: Duration = Duration::from_millis(250);

const CONSENT_SCHEMA: &str = "CREATE TABLE status_consent (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    revision INTEGER NOT NULL CHECK (revision > 0),
    policy_json BLOB NOT NULL CHECK (typeof(policy_json) = 'blob'
        AND length(policy_json) BETWEEN 1 AND 2097152)
)";
const CACHE_SCHEMA: &str = "CREATE TABLE status_cache (
    session_id TEXT PRIMARY KEY NOT NULL CHECK (typeof(session_id) = 'text'
        AND octet_length(session_id) BETWEEN 1 AND 256),
    captured_at INTEGER NOT NULL CHECK (captured_at >= 0),
    entry_json BLOB NOT NULL CHECK (typeof(entry_json) = 'blob'
        AND length(entry_json) BETWEEN 1 AND 65536)
) WITHOUT ROWID";

/// Explicit, unencrypted Unix store for bounded local consent and advisory status.
///
/// The dedicated directory must have an existing trusted parent. Existing paths
/// must be owned, private and unlinked elsewhere; unsafe paths are never chmodded.
/// One nonblocking file lock is held until the SQLite connection closes. This is
/// not a sandbox against hostile same-UID path replacement or database modification.
/// No environment, credential file or daemon registry is consulted.
pub struct SqliteStatusStore {
    // Declaration order closes SQLite before releasing the lifetime owner lock.
    connection: Connection,
    storage: PrivateStorage,
}

impl SqliteStatusStore {
    /// Open only this explicitly selected dedicated directory, creating it as 0700.
    /// Uses DELETE journals, FULL synchronization and a bounded 250 ms SQLite wait.
    /// Revisions and capture timestamps must fit SQLite's nonnegative signed integers.
    /// Existing snapshots are validated/recovered on private scratch copies first.
    /// Future/invalid snapshot refusal preserves source database/journal bytes and
    /// existence. Each source database/journal is limited to 64 MiB. Scratch copies
    /// are streamed in 8 KiB chunks and normally removed; process crashes can leave
    /// private scratch folders. Later I/O failures can follow supported source recovery.
    /// An existing rollback journal without its database is refused, never initialized away.
    /// A write error must be reconciled by reloading; post-commit I/O failure does
    /// not promise that the commit was rolled back.
    pub fn open(directory: impl AsRef<Path>) -> Result<Self, StatusStoreError> {
        let storage = PrivateStorage::open(directory.as_ref())?;
        preflight_snapshot(&storage)?;
        // Only an already validated supported snapshot may undergo source recovery.
        let connection = open_connection(&storage.path)?;
        let mut store = Self {
            connection,
            storage,
        };
        store.initialize()?;
        Ok(store)
    }

    fn initialize(&mut self) -> Result<(), StatusStoreError> {
        self.storage.validate()?;
        let version = validate_snapshot_schema(&self.connection)?;
        // A future schema is refused before changing persistent journal settings.
        let journal: String = self
            .connection
            .query_row("PRAGMA journal_mode=DELETE", [], |row| row.get(0))
            .map_err(database_error)?;
        if journal != "delete" {
            return Err(StatusStoreError::Database);
        }
        self.connection
            .execute_batch("PRAGMA synchronous=FULL;")
            .map_err(database_error)?;
        if version == 0 {
            self.transaction(|transaction| {
                transaction
                    .execute_batch(CONSENT_SCHEMA)
                    .map_err(database_error)?;
                transaction
                    .execute_batch(CACHE_SCHEMA)
                    .map_err(database_error)?;
                transaction
                    .pragma_update(None, "user_version", SCHEMA_VERSION)
                    .map_err(database_error)?;
                Ok(())
            })?;
        }
        self.transaction(|transaction| validate_snapshot_records(transaction))
    }

    fn transaction<T>(
        &mut self,
        operation: impl FnOnce(&Transaction<'_>) -> Result<T, StatusStoreError>,
    ) -> Result<T, StatusStoreError> {
        self.storage.validate()?;
        let result = run_transaction(&mut self.connection, &self.storage, operation);
        // Check even failed operations; never hide newly unsafe SQLite sidecars.
        self.storage.validate()?;
        result
    }
}

impl StatusStore for SqliteStatusStore {
    fn load_consent(&mut self) -> Result<ConsentSnapshot, StatusStoreError> {
        self.transaction(|transaction| read_consent(transaction))
    }

    fn compare_and_swap_consent(
        &mut self,
        expected: ConsentRevision,
        policy: &ConsentPolicy,
    ) -> Result<ConsentSnapshot, StatusStoreError> {
        self.transaction(|transaction| {
            let current = read_consent(transaction)?;
            if current.revision != expected {
                return Err(StatusStoreError::Conflict);
            }
            policy.validate()?;
            let revision = expected
                .0
                .checked_add(1)
                .and_then(|revision| i64::try_from(revision).ok())
                .ok_or(StatusStoreError::RevisionExhausted)?;
            let bytes = encode_json(policy, MAX_CONSENT_BYTES)?;
            transaction.execute(
                "INSERT INTO status_consent (singleton, revision, policy_json) VALUES (1, ?1, ?2)
                 ON CONFLICT(singleton) DO UPDATE SET revision=excluded.revision,
                    policy_json=excluded.policy_json",
                params![revision, bytes],
            ).map_err(database_error)?;
            let kept = match policy.auth_revision() {
                Some(auth_revision) if policy.extends(&current.policy) => {
                    carried_cache(transaction, ConsentRevision(revision as u64), auth_revision)?
                }
                _ => Vec::new(),
            };
            transaction
                .execute("DELETE FROM status_cache", [])
                .map_err(database_error)?;
            for (session, captured_at, bytes) in kept {
                transaction
                    .execute(
                        "INSERT INTO status_cache (session_id, captured_at, entry_json)
                         VALUES (?1, ?2, ?3)",
                        params![session, captured_at, bytes],
                    )
                    .map_err(database_error)?;
            }
            Ok(ConsentSnapshot {
                revision: ConsentRevision(revision as u64),
                policy: policy.clone(),
            })
        })
    }

    fn load_cache(
        &mut self,
        session: &SessionId,
    ) -> Result<Option<CachedStatus>, StatusStoreError> {
        validate_session(session)?;
        self.transaction(|transaction| {
            let snapshot = read_consent(transaction)?;
            // CASE in WHERE intentionally avoids a primary-key seek that could
            // materialize a malformed oversized index record before projection.
            // Scanning is bounded by the validated 256-row cache cap.
            let mut statement = transaction
                .prepare(
                    "SELECT CASE WHEN typeof(session_id)='text' AND octet_length(session_id) BETWEEN 1 AND 256
                        THEN session_id END, captured_at,
                  CASE WHEN typeof(entry_json)='blob' AND length(entry_json) BETWEEN 1 AND 65536
                  THEN entry_json END FROM status_cache
                  WHERE CASE WHEN typeof(session_id)='text' AND octet_length(session_id) BETWEEN 1 AND 256
                        THEN session_id END=?1",
                )
                .map_err(database_error)?;
            let mut rows = statement
                .query([session.as_str()])
                .map_err(database_error)?;
            let Some(row) = rows.next().map_err(database_error)? else {
                return Ok(None);
            };
            let entry = decode_cache(row)?;
            validate_cache_binding(&snapshot, &entry)?;
            Ok(Some(entry))
        })
    }

    fn save_cache(
        &mut self,
        expected: ConsentRevision,
        entry: &CachedStatus,
    ) -> Result<(), StatusStoreError> {
        self.transaction(|transaction| {
            let snapshot = read_consent(transaction)?;
            if snapshot.revision != expected
                || entry.consent_revision != expected
                || !snapshot.policy.permits_cache(entry)
            {
                return Err(StatusStoreError::Conflict);
            }
            entry.validate()?;
            let captured_at =
                i64::try_from(entry.captured_at).map_err(|_| StatusStoreError::InvalidData)?;
            let bytes = encode_json(entry, MAX_CACHE_BYTES)?;
            transaction.execute(
                "INSERT INTO status_cache (session_id, captured_at, entry_json) VALUES (?1, ?2, ?3)
                 ON CONFLICT(session_id) DO UPDATE SET captured_at=excluded.captured_at,
                    entry_json=excluded.entry_json",
                params![entry.session_id.as_str(), captured_at, bytes],
            ).map_err(database_error)?;
            // Equal timestamps are ordered by the opaque session key, never insertion order.
            transaction
                .execute(
                    "DELETE FROM status_cache WHERE session_id IN (
                    SELECT session_id FROM status_cache
                    ORDER BY captured_at DESC, session_id DESC LIMIT -1 OFFSET ?1)",
                    [MAX_CACHE_ENTRIES as i64],
                )
                .map_err(database_error)?;
            Ok(())
        })
    }

    fn remove_cache(&mut self, session: &SessionId) -> Result<(), StatusStoreError> {
        validate_session(session)?;
        self.transaction(|transaction| {
            transaction
                .execute(
                    "DELETE FROM status_cache WHERE session_id=?1",
                    [session.as_str()],
                )
                .map_err(database_error)?;
            Ok(())
        })
    }
}

fn open_connection(directory: &Path) -> Result<Connection, StatusStoreError> {
    let connection = Connection::open_with_flags(
        directory.join(DATABASE_NAME.to_string_lossy().as_ref()),
        OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_NO_MUTEX
            | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )
    .map_err(database_error)?;
    // This must precede every disk query/PRAGMA, including recovery and schema reads.
    // SQLITE_LIMIT_LENGTH also bounds complete on-disk records, not just JSON values.
    connection.set_limit(Limit::SQLITE_LIMIT_LENGTH, MAX_SQLITE_RECORD_BYTES);
    connection
        .busy_timeout(BUSY_TIMEOUT)
        .map_err(database_error)?;
    connection
        .set_db_config(DbConfig::SQLITE_DBCONFIG_DEFENSIVE, true)
        .map_err(database_error)?;
    connection
        .execute_batch("PRAGMA trusted_schema=OFF; PRAGMA temp_store=MEMORY;")
        .map_err(database_error)?;
    Ok(connection)
}

fn preflight_snapshot(storage: &PrivateStorage) -> Result<(), StatusStoreError> {
    storage.validate()?;
    let database_length = bounded_file_length(&storage.database)?;
    let journal = open_existing_private_file(&storage.path.join("status.sqlite3-journal"))?;
    if let Some(journal) = &journal {
        bounded_file_length(journal)?;
    } else {
        // The documented SQLite header is safe to read without opening SQLite.
        // Never trust this header when a rollback journal can supersede it.
        validate_journalless_header(&storage.database, database_length)?;
        if database_length == 0 {
            return Ok(());
        }
    }

    let scratch = tempfile::Builder::new()
        .prefix(".status-preflight-")
        .permissions(fs::Permissions::from_mode(0o700))
        .tempdir_in(&storage.path)
        .map_err(io_error)?;
    let result = validate_scratch_snapshot(scratch.path(), storage, journal.as_ref());
    // Close all scratch descriptors before cleanup, including on refusal.
    let cleanup = scratch.close().map_err(io_error);
    storage.validate()?;
    if let Some(journal) = &journal {
        validate_named_file(&storage.path.join("status.sqlite3-journal"), journal, false)?;
    }
    result?;
    cleanup
}

fn validate_scratch_snapshot(
    directory: &Path,
    source: &PrivateStorage,
    journal: Option<&File>,
) -> Result<(), StatusStoreError> {
    let scratch = PrivateStorage::open(directory)?;
    copy_bounded_file(&source.database, &scratch.database)?;
    if let Some(journal) = journal {
        let target = open_private_file(&scratch.directory, &scratch.path, JOURNAL_NAME)?;
        copy_bounded_file(journal, &target)?;
    }
    scratch.validate()?;
    // SQLite, not a hand-written journal parser, determines the committed snapshot.
    // Recovery and any resulting journal deletion affect only these private copies.
    let connection = open_connection(&scratch.path)?;
    let version = validate_snapshot_schema(&connection)?;
    bounded_file_length(&scratch.database)?;
    if version == SCHEMA_VERSION {
        validate_snapshot_records(&connection)?;
    }
    scratch.validate()
}

fn validate_journalless_header(database: &File, length: u64) -> Result<(), StatusStoreError> {
    if length == 0 {
        return Ok(());
    }
    if length < 100 {
        return Err(StatusStoreError::InvalidData);
    }
    let mut header = [0u8; 100];
    database.read_exact_at(&mut header, 0).map_err(io_error)?;
    if &header[..16] != b"SQLite format 3\0" {
        return Err(StatusStoreError::InvalidData);
    }
    let version = u32::from_be_bytes([header[60], header[61], header[62], header[63]]);
    if version != 0 && i64::from(version) != SCHEMA_VERSION {
        return Err(StatusStoreError::UnsupportedSchema);
    }
    Ok(())
}

fn bounded_file_length(file: &File) -> Result<u64, StatusStoreError> {
    let length = file.metadata().map_err(io_error)?.len();
    if length > MAX_PREFLIGHT_FILE_BYTES {
        return Err(StatusStoreError::InvalidData);
    }
    Ok(length)
}

fn copy_bounded_file(source: &File, target: &File) -> Result<(), StatusStoreError> {
    let length = bounded_file_length(source)?;
    let mut buffer = [0u8; 8192];
    let mut offset = 0;
    while offset < length {
        let maximum = (length - offset).min(buffer.len() as u64) as usize;
        let count = source
            .read_at(&mut buffer[..maximum], offset)
            .map_err(io_error)?;
        if count == 0 {
            return Err(StatusStoreError::InvalidData);
        }
        target
            .write_all_at(&buffer[..count], offset)
            .map_err(io_error)?;
        offset += count as u64;
    }
    if source.metadata().map_err(io_error)?.len() != length {
        return Err(StatusStoreError::InvalidData);
    }
    Ok(())
}

fn validate_snapshot_schema(connection: &Connection) -> Result<i64, StatusStoreError> {
    let version: i64 = connection
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .map_err(database_error)?;
    match version {
        0 => {
            let has_schema: bool = connection
                .query_row("SELECT EXISTS (SELECT 1 FROM sqlite_schema)", [], |row| {
                    row.get(0)
                })
                .map_err(database_error)?;
            if has_schema {
                return Err(StatusStoreError::UnsupportedSchema);
            }
        }
        SCHEMA_VERSION => validate_schema(connection)?,
        _ => return Err(StatusStoreError::UnsupportedSchema),
    }
    Ok(version)
}

fn validate_snapshot_records(connection: &Connection) -> Result<(), StatusStoreError> {
    let snapshot = read_consent(connection)?;
    let mut statement = connection.prepare(
        "SELECT CASE WHEN typeof(session_id)='text' AND octet_length(session_id) BETWEEN 1 AND 256
            THEN session_id END, captured_at,
         CASE WHEN typeof(entry_json)='blob' AND length(entry_json) BETWEEN 1 AND 65536
            THEN entry_json END FROM status_cache LIMIT 257",
    ).map_err(database_error)?;
    let mut rows = statement.query([]).map_err(database_error)?;
    let mut count = 0;
    while let Some(row) = rows.next().map_err(database_error)? {
        count += 1;
        if count > MAX_CACHE_ENTRIES {
            return Err(StatusStoreError::InvalidData);
        }
        let entry = decode_cache(row)?;
        validate_cache_binding(&snapshot, &entry)?;
    }
    Ok(())
}

fn run_transaction<T>(
    connection: &mut Connection,
    storage: &PrivateStorage,
    operation: impl FnOnce(&Transaction<'_>) -> Result<T, StatusStoreError>,
) -> Result<T, StatusStoreError> {
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(database_error)?;
    let result = operation(&transaction).and_then(|value| {
        storage.validate()?;
        Ok(value)
    });
    match result {
        Ok(value) => {
            transaction.commit().map_err(database_error)?;
            // Make DELETE-journal removal durable as well as SQLite's FULL page writes.
            storage.directory.sync_all().map_err(io_error)?;
            Ok(value)
        }
        Err(error) => {
            transaction.rollback().map_err(database_error)?;
            Err(error)
        }
    }
}

fn validate_schema(connection: &Connection) -> Result<(), StatusStoreError> {
    let mut statement = connection
        .prepare("SELECT type, name, sql FROM sqlite_schema WHERE name NOT GLOB 'sqlite_*' LIMIT 3")
        .map_err(database_error)?;
    let mut rows = statement.query([]).map_err(database_error)?;
    let mut count = 0;
    while let Some(row) = rows.next().map_err(database_error)? {
        let kind = row.get_ref(0).map_err(|_| StatusStoreError::InvalidData)?;
        let name = row.get_ref(1).map_err(|_| StatusStoreError::InvalidData)?;
        let sql = row.get_ref(2).map_err(|_| StatusStoreError::InvalidData)?;
        let expected = match name {
            ValueRef::Text(b"status_consent") => CONSENT_SCHEMA,
            ValueRef::Text(b"status_cache") => CACHE_SCHEMA,
            _ => return Err(StatusStoreError::InvalidData),
        };
        if kind != ValueRef::Text(b"table") || sql != ValueRef::Text(expected.as_bytes()) {
            return Err(StatusStoreError::InvalidData);
        }
        count += 1;
    }
    if count != 2 {
        return Err(StatusStoreError::InvalidData);
    }
    Ok(())
}

fn read_consent(connection: &Connection) -> Result<ConsentSnapshot, StatusStoreError> {
    let mut statement = connection
        .prepare(
            "SELECT singleton, revision,
            CASE WHEN typeof(policy_json)='blob' AND length(policy_json) BETWEEN 1 AND 2097152
            THEN policy_json END FROM status_consent LIMIT 2",
        )
        .map_err(database_error)?;
    let mut rows = statement.query([]).map_err(database_error)?;
    let Some(row) = rows.next().map_err(database_error)? else {
        return Ok(ConsentSnapshot::default());
    };
    let singleton: i64 = row.get(0).map_err(|_| StatusStoreError::InvalidData)?;
    let revision: i64 = row.get(1).map_err(|_| StatusStoreError::InvalidData)?;
    if singleton != 1 || revision <= 0 {
        return Err(StatusStoreError::InvalidData);
    }
    let bytes = bounded_blob(row, 2, MAX_CONSENT_BYTES)?;
    let policy: ConsentPolicy =
        serde_json::from_slice(bytes).map_err(|_| StatusStoreError::InvalidData)?;
    if policy == ConsentPolicy::Disabled {
        // Serde's internally tagged unit variants can discard unknown fields even
        // with deny_unknown_fields. Keep the private disk boundary strict as well.
        let _: DisabledPolicyRecord =
            serde_json::from_slice(bytes).map_err(|_| StatusStoreError::InvalidData)?;
    }
    policy.validate()?;
    if rows.next().map_err(database_error)?.is_some() {
        return Err(StatusStoreError::InvalidData);
    }
    Ok(ConsentSnapshot {
        revision: ConsentRevision(revision as u64),
        policy,
    })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DisabledPolicyRecord {
    #[serde(rename = "state")]
    _state: String,
}

fn decode_cache(row: &Row<'_>) -> Result<CachedStatus, StatusStoreError> {
    let session = row.get_ref(0).map_err(|_| StatusStoreError::InvalidData)?;
    if !matches!(session, ValueRef::Text(bytes) if !bytes.is_empty() && bytes.len() <= 256) {
        return Err(StatusStoreError::InvalidData);
    }
    let captured_at: i64 = row.get(1).map_err(|_| StatusStoreError::InvalidData)?;
    let bytes = bounded_blob(row, 2, MAX_CACHE_BYTES)?;
    let entry: CachedStatus =
        serde_json::from_slice(bytes).map_err(|_| StatusStoreError::InvalidData)?;
    entry.validate()?;
    if session != ValueRef::Text(entry.session_id.as_str().as_bytes())
        || u64::try_from(captured_at).ok() != Some(entry.captured_at)
    {
        return Err(StatusStoreError::InvalidData);
    }
    Ok(entry)
}

/// Every valid cache row, moved to consent `revision` and credential
/// `auth_revision`, ready to insert again. Rows that fail to decode are left
/// out, as a later load would refuse them.
fn carried_cache(
    connection: &Connection,
    revision: ConsentRevision,
    auth_revision: u64,
) -> Result<Vec<(String, i64, Vec<u8>)>, StatusStoreError> {
    let mut statement = connection
        .prepare(
            "SELECT CASE WHEN typeof(session_id)='text' AND octet_length(session_id) BETWEEN 1 AND 256
                THEN session_id END, captured_at,
             CASE WHEN typeof(entry_json)='blob' AND length(entry_json) BETWEEN 1 AND 65536
             THEN entry_json END FROM status_cache LIMIT ?1",
        )
        .map_err(database_error)?;
    let mut rows = statement
        .query([MAX_CACHE_ENTRIES as i64])
        .map_err(database_error)?;
    let mut kept = Vec::new();
    while let Some(row) = rows.next().map_err(database_error)? {
        let Ok(mut entry) = decode_cache(row) else {
            continue;
        };
        entry.consent_revision = revision;
        entry.auth_revision = auth_revision;
        let captured_at =
            i64::try_from(entry.captured_at).map_err(|_| StatusStoreError::InvalidData)?;
        let bytes = encode_json(&entry, MAX_CACHE_BYTES)?;
        kept.push((entry.session_id.as_str().to_owned(), captured_at, bytes));
    }
    Ok(kept)
}

fn validate_cache_binding(
    snapshot: &ConsentSnapshot,
    entry: &CachedStatus,
) -> Result<(), StatusStoreError> {
    if snapshot.revision != entry.consent_revision || !snapshot.policy.permits_cache(entry) {
        return Err(StatusStoreError::InvalidData);
    }
    Ok(())
}

fn bounded_blob<'a>(
    row: &'a Row<'_>,
    index: usize,
    maximum: usize,
) -> Result<&'a [u8], StatusStoreError> {
    // SELECT's CASE/length guard prevents fetching oversized overflow-page blobs;
    // check the borrowed value again before allocating or decoding JSON in Rust.
    match row
        .get_ref(index)
        .map_err(|_| StatusStoreError::InvalidData)?
    {
        ValueRef::Blob(bytes) if !bytes.is_empty() && bytes.len() <= maximum => Ok(bytes),
        _ => Err(StatusStoreError::InvalidData),
    }
}

fn encode_json(value: &impl Serialize, maximum: usize) -> Result<Vec<u8>, StatusStoreError> {
    let mut writer = BoundedJson {
        bytes: Vec::new(),
        maximum,
    };
    serde_json::to_writer(&mut writer, value).map_err(|_| StatusStoreError::InvalidData)?;
    Ok(writer.bytes)
}

struct BoundedJson {
    bytes: Vec<u8>,
    maximum: usize,
}

impl Write for BoundedJson {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > self.maximum.saturating_sub(self.bytes.len()) {
            return Err(io::Error::from(io::ErrorKind::InvalidData));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

struct PrivateStorage {
    path: PathBuf,
    directory: File,
    lock: File,
    database: File,
}

impl PrivateStorage {
    fn open(path: &Path) -> Result<Self, StatusStoreError> {
        // Strip trailing separators/dots before O_NOFOLLOW; otherwise "link/"
        // can turn the selected symlink into an intermediate path component.
        let normalized: PathBuf = path.components().collect();
        if !matches!(
            normalized.components().next_back(),
            Some(Component::Normal(_))
        ) {
            return Err(StatusStoreError::UnsafePath);
        }
        let path = normalized.as_path();
        let created = match fs::DirBuilder::new().mode(0o700).create(path) {
            Ok(()) => true,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => false,
            Err(error) => return Err(io_error(error)),
        };
        let directory = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(path)
            .map_err(io_error)?;
        validate_directory(&directory.metadata().map_err(io_error)?)?;
        let path = fs::canonicalize(path).map_err(io_error)?;
        validate_named_file(&path, &directory, true)?;
        if created {
            let parent = path.parent().ok_or(StatusStoreError::UnsafePath)?;
            let parent = OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
                .open(parent)
                .map_err(io_error)?;
            parent.sync_all().map_err(io_error)?;
        }
        let lock = open_private_file(&directory, &path, LOCK_NAME)?;
        // SAFETY: this owned live descriptor is borrowed, never wrapped a second time.
        if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            let error = io::Error::last_os_error();
            return Err(if error.kind() == io::ErrorKind::WouldBlock {
                StatusStoreError::Busy
            } else {
                io_error(error)
            });
        }
        validate_sidecars(&path)?;
        reject_orphan_journal(&path)?;
        let database = open_private_file(&directory, &path, DATABASE_NAME)?;
        directory.sync_all().map_err(io_error)?;
        let storage = Self {
            path,
            directory,
            lock,
            database,
        };
        storage.validate()?;
        Ok(storage)
    }

    fn validate(&self) -> Result<(), StatusStoreError> {
        validate_named_file(&self.path, &self.directory, true)?;
        validate_named_file(&self.path.join("status.lock"), &self.lock, false)?;
        validate_named_file(&self.path.join("status.sqlite3"), &self.database, false)?;
        validate_sidecars(&self.path)
    }
}

fn validate_directory(metadata: &Metadata) -> Result<(), StatusStoreError> {
    // SAFETY: geteuid has no preconditions or pointers.
    let owner = unsafe { libc::geteuid() };
    if !metadata.is_dir() || metadata.uid() != owner || metadata.mode() & 0o7777 != 0o700 {
        return Err(StatusStoreError::UnsafePath);
    }
    Ok(())
}

fn validate_regular_file(metadata: &Metadata) -> Result<(), StatusStoreError> {
    // SAFETY: geteuid has no preconditions or pointers.
    let owner = unsafe { libc::geteuid() };
    if !metadata.is_file()
        || metadata.uid() != owner
        || metadata.nlink() != 1
        || metadata.mode() & 0o7777 != 0o600
    {
        return Err(StatusStoreError::UnsafePath);
    }
    Ok(())
}

fn validate_named_file(path: &Path, file: &File, directory: bool) -> Result<(), StatusStoreError> {
    let named = fs::symlink_metadata(path).map_err(io_error)?;
    let opened = file.metadata().map_err(io_error)?;
    if directory {
        validate_directory(&named)?;
        validate_directory(&opened)?;
    } else {
        validate_regular_file(&named)?;
        validate_regular_file(&opened)?;
    }
    if named.dev() != opened.dev() || named.ino() != opened.ino() {
        return Err(StatusStoreError::UnsafePath);
    }
    Ok(())
}

fn validate_sidecars(directory: &Path) -> Result<(), StatusStoreError> {
    for name in SIDECARS {
        match fs::symlink_metadata(directory.join(name)) {
            Ok(metadata) => {
                validate_regular_file(&metadata)?;
                // This backend never enables WAL. Do not let SQLite open foreign WAL state.
                if name != "status.sqlite3-journal" {
                    return Err(StatusStoreError::InvalidData);
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(io_error(error)),
        }
    }
    Ok(())
}

fn reject_orphan_journal(directory: &Path) -> Result<(), StatusStoreError> {
    match fs::symlink_metadata(directory.join("status.sqlite3")) {
        Ok(metadata) => return validate_regular_file(&metadata),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(io_error(error)),
    }
    // Called under the lifetime owner lock, before O_CREAT can change DB absence.
    // Any journal may be recovery evidence; do not infer whether it is hot or stale.
    match fs::symlink_metadata(directory.join("status.sqlite3-journal")) {
        Ok(metadata) => {
            validate_regular_file(&metadata)?;
            Err(StatusStoreError::InvalidData)
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(io_error(error)),
    }
}

fn open_private_file(directory: &File, path: &Path, name: &CStr) -> Result<File, StatusStoreError> {
    let path = path.join(name.to_string_lossy().as_ref());
    match fs::symlink_metadata(&path) {
        Ok(metadata) => validate_regular_file(&metadata)?,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(io_error(error)),
    }
    // SAFETY: name is a fixed NUL-terminated basename, directory is a live owned FD,
    // and openat returns a newly owned descriptor. c_uint is the promoted mode type.
    let descriptor = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDWR | libc::O_CREAT | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK,
            0o600 as libc::c_uint,
        )
    };
    if descriptor < 0 {
        return Err(io_error(io::Error::last_os_error()));
    }
    // SAFETY: this is the only ownership transfer of the fresh descriptor.
    let file = unsafe { File::from_raw_fd(descriptor) };
    validate_named_file(&path, &file, false)?;
    Ok(file)
}

fn open_existing_private_file(path: &Path) -> Result<Option<File>, StatusStoreError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => validate_regular_file(&metadata)?,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(io_error(error)),
    }
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
        .open(path)
        .map_err(io_error)?;
    validate_named_file(path, &file, false)?;
    Ok(Some(file))
}

fn io_error(error: io::Error) -> StatusStoreError {
    if matches!(
        error.raw_os_error(),
        Some(libc::ELOOP | libc::ENOTDIR | libc::EISDIR)
    ) {
        StatusStoreError::UnsafePath
    } else {
        StatusStoreError::Io(error.kind())
    }
}

fn database_error(error: rusqlite::Error) -> StatusStoreError {
    match error.sqlite_error_code() {
        Some(rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked) => {
            StatusStoreError::Busy
        }
        Some(
            rusqlite::ErrorCode::DatabaseCorrupt
            | rusqlite::ErrorCode::NotADatabase
            | rusqlite::ErrorCode::TooBig,
        ) => StatusStoreError::InvalidData,
        _ => StatusStoreError::Database,
    }
}

#[cfg(test)]
mod tests;
