use session_core::ids::SessionId;
use std::io;
use thiserror::Error;

use super::{CachedStatus, ConsentPolicy, ConsentRevision, ConsentSnapshot};

/// Hard local cache bound, independent of any service-funded inference quota.
pub const MAX_CACHE_ENTRIES: usize = 256;

/// Credential/content/path-safe persistence failures.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum StatusStoreError {
    /// Another local owner/transaction holds the store.
    #[error("the local AI-status store is busy")]
    Busy,
    /// A newer consent decision invalidated this write.
    #[error("AI-status consent changed; reload the current decision")]
    Conflict,
    /// Existing storage has unsafe ownership, mode, links or type.
    #[error("the local AI-status store has unsafe permissions or file type")]
    UnsafePath,
    /// Stored data violated bounded domain invariants.
    #[error("invalid local AI-status data")]
    InvalidData,
    /// A newer database cannot be silently opened or rewritten.
    #[error("unsupported local AI-status database schema")]
    UnsupportedSchema,
    /// Persistent revisions never wrap.
    #[error("local AI-status revision exhausted")]
    RevisionExhausted,
    /// Filesystem diagnostics contain no user path or data.
    #[error("local AI-status I/O failure: {0:?}")]
    Io(io::ErrorKind),
    /// SQLite details may contain SQL/path/content and must not escape this category.
    #[error("local AI-status database operation failed")]
    Database,
}

/// Single local owner's durable boundary. No method receives credentials/raw context.
pub trait StatusStore: Send {
    /// Load and validate the current explicit consent decision; missing means disabled/rev0.
    fn load_consent(&mut self) -> Result<ConsentSnapshot, StatusStoreError>;
    /// Atomically advance only expected revision and replace policy. Clear ALL cache
    /// rows, unless the new policy `extends` the current one: then move every row to
    /// the new consent and credential revisions.
    fn compare_and_swap_consent(
        &mut self,
        expected: ConsentRevision,
        policy: &ConsentPolicy,
    ) -> Result<ConsentSnapshot, StatusStoreError>;
    /// Read one bounded validated cache row. Service must still check current auth/source.
    fn load_cache(&mut self, session: &SessionId)
        -> Result<Option<CachedStatus>, StatusStoreError>;
    /// Atomically check consent revision AND permits_cache, then upsert a validated row.
    /// Keep at most MAX_CACHE_ENTRIES, evicting oldest captured_at first when necessary.
    fn save_cache(
        &mut self,
        expected: ConsentRevision,
        entry: &CachedStatus,
    ) -> Result<(), StatusStoreError>;
    /// Remove only this session's advisory cache; never operate on its actual terminal.
    fn remove_cache(&mut self, session: &SessionId) -> Result<(), StatusStoreError>;
}
