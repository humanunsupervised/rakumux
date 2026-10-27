//! Local AI-status consent, cache and stale-result policy. No network or TUI wiring.

mod account;
pub(crate) mod model;
mod service;
#[cfg(unix)]
mod sqlite_store;
mod store;
mod text;

pub use account::{AccountGate, AccountSessionGate};
pub use model::{
    AccountBinding, AccountKey, CachedStatus, ConsentPolicy, ConsentRevision, ConsentSnapshot,
    ContextAvailability, FolderConsent, InputSelection, SourceStamp, StatusSource,
};
pub use service::{
    AISessionStatusService, GenerationTicket, Preparation, StatusClock, StatusError, StatusView,
    SystemStatusClock,
};
#[cfg(unix)]
pub use sqlite_store::SqliteStatusStore;
pub use store::{StatusStore, StatusStoreError, MAX_CACHE_ENTRIES};
pub use text::{SessionTitle, StatusText, MAX_TITLE_BYTES, MAX_TITLE_CHARS};
