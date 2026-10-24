//! The agent feed: what agents said when they finished a turn, captured by
//! the daemon while the account's Feed switch is on, uploaded to rakumux.com
//! and read back from every machine of the account (`contracts/feed/`).
//!
//! Memory only: the cache and the upload queue are gone when the status
//! service stops. Private sessions are never uploaded, and reply text is
//! redacted of keys, credential assignments and tokens first.

mod cache;
mod job;
mod sync;
mod wire;

pub use cache::{FeedEntry, FeedPage, FeedStatus};
pub use job::{FeedCompletion, FeedJob};
pub use sync::machine_name;
pub(crate) use sync::FeedSync;
pub use wire::FeedFailure;
pub(crate) use wire::FeedRequest;
