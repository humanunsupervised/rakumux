//! The agent feed: what agents said when they finished a turn, captured by
//! the daemon while the account's Feed switch is on, uploaded to rakumux.com
//! and read back from every machine of the account (`contracts/feed/`); and
//! the whole chats those turns belong to, uploaded as they grow (`chats`) and
//! read back a turn at a time (`turn`).
//!
//! The cache and the upload queue are memory only, gone when the status
//! service stops; where each chat's upload is up to is kept in the status
//! directory. Private sessions are never uploaded, and every text is redacted
//! of keys, credential assignments and tokens first.

mod cache;
mod chats;
mod job;
mod replies;
mod sync;
mod turn;
mod wire;

pub use cache::{FeedEntry, FeedPage, FeedReply, FeedStatus, ReplyFailure, ReplyStatus};
pub(crate) use chats::{ChatSync, ChatWork, READ_BYTES};
pub(crate) use job::FeedResult;
pub use job::{FeedCompletion, FeedJob};
pub use replies::{AcceptRemote, ReplyWrite, ReplyWriteStatus};
pub(crate) use replies::{ReplySync, ReplyWork};
pub use sync::machine_name;
pub(crate) use sync::FeedSync;
pub use turn::{FeedTurn, FeedTurnStatus};
pub use wire::FeedFailure;

/// Whether `id` is a feed entry's: a ULID in Crockford base32 capitals.
pub fn is_entry_id(id: &str) -> bool {
    id.len() == 26
        && id
            .bytes()
            .all(|b| b.is_ascii_digit() || (b.is_ascii_uppercase() && !b"ILOU".contains(&b)))
}
pub use wire::TurnRead;
pub(crate) use wire::{reply_upload, FeedRequest};
