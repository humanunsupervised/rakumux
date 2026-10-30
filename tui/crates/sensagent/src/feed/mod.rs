//! The agent feed: what agents said when they finished a turn, captured by
//! the daemon while the account's feed is on, uploaded to rakumux.com
//! and read back from every machine of the account (`contracts/feed/`); and
//! the whole chats those turns belong to, uploaded as they grow (`chats`) and
//! read back a turn at a time (`turn`); replies written back (`replies`), with
//! the files attached to them saved for the agent (`uploads`); the
//! machine's session list, for the website's Status and Folder (`sessions`);
//! while someone has rtop open there, what the machine uses (`resources`);
//! and the new sessions asked of it there (`new_sessions`).
//!
//! The cache and the upload queue are memory only, gone when the status
//! service stops; where each chat's upload is up to is kept in the status
//! directory. Private sessions are never uploaded, and every text is redacted
//! of keys, credential assignments and tokens first.

mod cache;
mod chats;
mod job;
mod machine;
mod new_sessions;
mod poke;
mod replies;
mod resources;
mod sessions;
mod sync;
mod turn;
mod uploads;
mod wire;

pub use cache::{
    FeedEntry, FeedPage, FeedPrompt, FeedReply, FeedStatus, OpenTurn, ReplyAttachment,
    ReplyFailure, ReplyStatus,
};
pub(crate) use chats::{write_private, ChatSync, ChatWork, READ_BYTES};
pub(crate) use job::FeedResult;
pub use job::{FeedCompletion, FeedJob};
pub(crate) use machine::MachineIdentity;
pub(crate) use new_sessions::{NewSessionSync, NewSessionWork, NEW_SESSIONS};
pub(crate) use poke::{listen, PokeEvent, PokeSync};
pub(crate) use replies::{Fetch, Fetched, ReplySync, ReplyWork, TYPING_QUIET};
pub use replies::{ReplyWrite, ReplyWriteStatus};
pub(crate) use resources::{ResourceSync, ResourceWork};
pub(crate) use sessions::SessionSync;
pub use sync::machine_name;
pub(crate) use sync::FeedSync;
pub use turn::{FeedTurn, FeedTurnStatus};
pub(crate) use uploads::{fetch, Uploads, CLEAN_EVERY};
pub use wire::FeedFailure;

/// Whether `id` is a feed entry's: a ULID in Crockford base32 capitals.
pub fn is_entry_id(id: &str) -> bool {
    id.len() == 26
        && id
            .bytes()
            .all(|b| b.is_ascii_digit() || (b.is_ascii_uppercase() && !b"ILOU".contains(&b)))
}
pub use wire::TurnRead;
pub(crate) use wire::{
    failure as refused, reply_upload, session_message_upload, FeedRequest, RealtimeToken,
    SessionMessage,
};
