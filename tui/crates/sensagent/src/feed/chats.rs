//! Whole chats for the agent feed: while the account's Feed switch is on,
//! each live, non-private session's agent conversation is read from the
//! daemon (`session_transcript`) after where it was last uploaded, cleaned
//! (terminal controls removed, keys, credential assignments and tokens
//! masked) and uploaded to rakumux.com (`POST /api/v1/feed/messages`), one
//! request at a time; a changing chat is read at most every 5 s, every second
//! while replies are (`ReplySync::watched`). Where each session's chat is up
//! to is kept in the status directory, so a restarted service carries on
//! rather than starting over. Driven by the owner thread with its own clock.

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use session_core::api::{
    ErrorCode, TranscriptCursor, TranscriptMessage, TranscriptPage, TranscriptPart, TranscriptRole,
    MAX_PART_BYTES,
};
use session_core::ids::SessionId;
use session_core::model::{SessionStatus, SessionView};

use super::wire::{self, FeedFailure, FeedRequest};

/// How much of a chat's log is read at once.
pub(crate) const READ_BYTES: u32 = 512 * 1024;
/// A changing chat is read again at most this often…
const READ_EVERY: Duration = Duration::from_secs(5);
/// …or this often while replies are read every second (`ReplySync::watched`), so
/// rakumux.com's pages follow along.
const READ_EVERY_WATCHED: Duration = Duration::from_secs(1);
/// After a read failed, before that session is read again.
const READ_RETRY: Duration = Duration::from_secs(30);
/// The first and longest waits after a failed upload.
const RETRY_MIN: Duration = Duration::from_secs(5);
const RETRY_MAX: Duration = Duration::from_secs(300);
/// After the cloud says the switch is off, until it's tried again.
const OFF_WAIT: Duration = Duration::from_secs(120);
/// The longest wait the cloud can ask for (a day's quota).
const LONGEST_WAIT: Duration = Duration::from_secs(86_400);
/// The most text one message uploads, all its parts together.
const MESSAGE_TEXT: usize = 512 * 1024;
/// Sessions whose place is remembered, the most recently used kept.
const MAX_CURSORS: usize = 512;

/// What to do next for chats.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ChatWork {
    /// Read `session`'s chat from the daemon after `after`.
    Read {
        session: SessionId,
        after: Option<TranscriptCursor>,
    },
    /// Upload messages.
    Upload(FeedRequest),
}

/// The chat a session's messages belong to, as uploads name it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ChatName {
    pub agent: String,
    pub conversation_id: String,
    pub session_id: String,
    pub session_name: String,
    pub folder: Option<String>,
}

/// One message, cleaned and ready to upload.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct UploadMessage {
    pub id: String,
    pub position: u64,
    pub role: TranscriptRole,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub at: Option<i64>,
    pub parts: Vec<TranscriptPart>,
}

/// A session the daemon shows.
struct Followed {
    name: String,
    folder: Option<String>,
    private: bool,
    live: bool,
    /// It changed since its chat was last read.
    due: bool,
    /// Not read again before this.
    next: Duration,
}

/// Messages read and not yet all uploaded, with where the read stopped.
struct Pending {
    session: SessionId,
    chat: ChatName,
    messages: VecDeque<UploadMessage>,
    cursor: TranscriptCursor,
    more: bool,
    /// How many of `messages` the upload out now holds.
    sent: usize,
}

/// Chats' state on the owner thread.
pub(crate) struct ChatSync {
    machine: String,
    enabled: bool,
    /// The daemon reads chats (an older one doesn't, until it reconnects).
    supported: bool,
    sessions: HashMap<SessionId, Followed>,
    cursors: Cursors,
    /// Replies are read every second (`ReplySync::watched`): chats are read
    /// every [`READ_EVERY_WATCHED`].
    watched: bool,
    busy: bool,
    pending: Option<Pending>,
    retry_at: Duration,
    retry: Duration,
}

impl ChatSync {
    /// Chats for `machine`, remembering where they're up to in `cursors`
    /// (memory only without one).
    pub fn new(machine: String, cursors: Option<PathBuf>) -> Self {
        Self {
            machine,
            enabled: false,
            supported: true,
            sessions: HashMap::new(),
            cursors: Cursors::load(cursors),
            watched: false,
            busy: false,
            pending: None,
            retry_at: Duration::ZERO,
            retry: RETRY_MIN,
        }
    }

    /// Upload while signed in with the account's Feed switch on. Turning it
    /// off drops what wasn't uploaded; what was uploaded stays remembered.
    pub fn set_enabled(&mut self, enabled: bool, now: Duration) {
        if enabled == self.enabled {
            return;
        }
        self.enabled = enabled;
        self.pending = None;
        self.retry = RETRY_MIN;
        self.retry_at = now;
        if enabled {
            for session in self.sessions.values_mut() {
                session.due = true;
            }
        }
    }

    /// Whether replies are read every second (`ReplySync::watched`): then
    /// chats are too, a session waiting out the slower pace no longer than that.
    pub fn set_watched(&mut self, watched: bool, now: Duration) {
        if watched && !self.watched {
            for session in self.sessions.values_mut() {
                session.next = session.next.min(now + READ_EVERY_WATCHED);
            }
        }
        self.watched = watched;
    }

    /// The daemon (re)connected: these are its sessions, each read again.
    pub fn connected(&mut self, views: &[SessionView]) {
        self.supported = true;
        self.sessions.clear();
        for view in views {
            self.observe(view);
        }
    }

    /// A session appeared or changed: its chat is read again soon.
    pub fn observe(&mut self, view: &SessionView) {
        let live = view.status == SessionStatus::Live;
        let next = self
            .sessions
            .get(&view.id)
            .map_or(Duration::ZERO, |followed| followed.next);
        self.sessions.insert(
            view.id.clone(),
            Followed {
                name: view.name.clone(),
                folder: super::sync::folder_of(view),
                private: view.private,
                live,
                due: live && !view.private,
                next,
            },
        );
    }

    pub fn forget(&mut self, session: &SessionId) {
        self.sessions.remove(session);
        if self
            .pending
            .as_ref()
            .is_some_and(|pending| pending.session == *session)
        {
            self.pending = None;
        }
    }

    /// The next read or upload, if one is due: uploading what was read first.
    pub fn next_work(&mut self, now: Duration) -> Option<ChatWork> {
        if !self.enabled || self.busy || now < self.retry_at {
            return None;
        }
        if let Some(pending) = &mut self.pending {
            let Some((request, sent)) =
                wire::chat_upload(&self.machine, &pending.chat, &pending.messages)
            else {
                self.pending = None;
                return None;
            };
            pending.sent = sent;
            self.busy = true;
            return Some(ChatWork::Upload(request));
        }
        if !self.supported {
            return None;
        }
        let session = self
            .sessions
            .iter()
            .filter(|(_, followed)| {
                followed.due && followed.live && !followed.private && followed.next <= now
            })
            .min_by_key(|(_, followed)| followed.next)
            .map(|(id, _)| id.clone())?;
        if let Some(followed) = self.sessions.get_mut(&session) {
            followed.due = false;
            followed.next = now
                + if self.watched {
                    READ_EVERY_WATCHED
                } else {
                    READ_EVERY
                };
        }
        self.busy = true;
        Some(ChatWork::Read {
            after: self.cursors.get(&session),
            session,
        })
    }

    /// A read finished. Its messages are uploaded next; with none, its place
    /// is remembered at once.
    pub fn read_done(
        &mut self,
        session: SessionId,
        result: Result<TranscriptPage, ErrorCode>,
        now: Duration,
    ) {
        self.busy = false;
        let page = match result {
            Ok(page) => page,
            // An older daemon: not asked again until it reconnects.
            Err(ErrorCode::Unsupported | ErrorCode::Unknown) => {
                self.supported = false;
                return;
            }
            // No agent there, or not running: read again when it changes.
            Err(ErrorCode::NotFound | ErrorCode::SessionNotLive) => return,
            Err(_) => {
                if let Some(followed) = self.sessions.get_mut(&session) {
                    followed.due = true;
                    followed.next = now + READ_RETRY;
                }
                return;
            }
        };
        let Some(followed) = self.sessions.get_mut(&session) else {
            return;
        };
        if !self.enabled || followed.private {
            return;
        }
        if page.more {
            followed.due = true;
            followed.next = now;
        }
        let chat = ChatName {
            agent: page.agent,
            conversation_id: page.conversation_id,
            session_id: session.as_str().to_owned(),
            session_name: followed.name.clone(),
            folder: followed.folder.clone(),
        };
        let messages: VecDeque<UploadMessage> = page
            .messages
            .into_iter()
            .filter_map(upload_message)
            .collect();
        if !wire::conversation_id(&chat.conversation_id) || !known_agent(&chat.agent) {
            return;
        }
        if messages.is_empty() {
            self.cursors.set(&session, page.cursor);
            return;
        }
        self.pending = Some(Pending {
            session,
            chat,
            messages,
            cursor: page.cursor,
            more: page.more,
            sent: 0,
        });
    }

    /// An upload finished; `current` says whether it was made for the account
    /// still signed in.
    pub fn upload_done(&mut self, current: bool, outcome: Result<(), FeedFailure>, now: Duration) {
        self.busy = false;
        if !current || !self.enabled {
            self.pending = None;
            return;
        }
        let Some(pending) = &mut self.pending else {
            return;
        };
        match outcome {
            Ok(()) => self.retry = RETRY_MIN,
            // The cloud refused these: they're passed over, not retried forever.
            Err(FeedFailure::Invalid) => {
                eprintln!("rakumux-status: the feed refused a chat upload; it was passed over");
            }
            Err(failure) => {
                let wait = match failure {
                    FeedFailure::RateLimited(Some(delay))
                    | FeedFailure::Unavailable(Some(delay)) => delay,
                    FeedFailure::Off => OFF_WAIT,
                    _ => self.retry,
                };
                self.retry = self.retry.saturating_mul(2).min(RETRY_MAX);
                self.retry_at = now + wait.min(LONGEST_WAIT);
                return;
            }
        }
        let sent = pending.sent.min(pending.messages.len());
        pending.messages.drain(..sent);
        pending.sent = 0;
        if pending.messages.is_empty() {
            let Some(done) = self.pending.take() else {
                return;
            };
            self.cursors.set(&done.session, done.cursor);
            if let Some(followed) = self.sessions.get_mut(&done.session) {
                if done.more {
                    followed.due = true;
                    followed.next = now;
                }
            }
        }
    }
}

fn known_agent(agent: &str) -> bool {
    matches!(agent, "claude-code" | "codex" | "opencode")
}

/// A message as the feed uploads it: each text cleaned and cut, at most
/// [`MESSAGE_TEXT`] of text in all; `None` when nothing is left of it.
pub(crate) fn upload_message(message: TranscriptMessage) -> Option<UploadMessage> {
    const LATEST_MS: i64 = 4_102_444_800_000;
    const EARLIEST_MS: i64 = 1_577_836_800_000;
    if message.role == TranscriptRole::Unknown || message.position > (1 << 53) - 1 {
        return None;
    }
    let mut budget = MESSAGE_TEXT;
    let mut parts = Vec::new();
    for part in message.parts {
        if budget == 0 {
            break;
        }
        let mut clean = |text: &str, truncated: bool, empty_ok: bool| -> Option<(String, bool)> {
            let cleaned = crate::cloud::chat_text(text)?;
            let cleaned = if empty_ok {
                cleaned
            } else {
                cleaned.trim().to_owned()
            };
            if cleaned.is_empty() && !empty_ok {
                return None;
            }
            let limit = budget.min(MAX_PART_BYTES);
            let (cut, was_cut) = cut(cleaned, limit);
            budget -= cut.len();
            Some((cut, truncated || was_cut))
        };
        let part = match part {
            TranscriptPart::Text { text, truncated } => clean(&text, truncated, false)
                .map(|(text, truncated)| TranscriptPart::Text { text, truncated }),
            TranscriptPart::Reasoning { text, truncated } => clean(&text, truncated, false)
                .map(|(text, truncated)| TranscriptPart::Reasoning { text, truncated }),
            TranscriptPart::ToolCall {
                call_id,
                name,
                input,
                truncated,
            } => wire::label(&name, 128).and_then(|name| {
                let (input, truncated) = clean(&input, truncated, true)?;
                Some(TranscriptPart::ToolCall {
                    call_id: call_id.filter(|id| wire::call_id(id)),
                    name,
                    input,
                    truncated,
                })
            }),
            TranscriptPart::ToolResult {
                call_id,
                output,
                error,
                truncated,
            } => clean(&output, truncated, true).map(|(output, truncated)| {
                TranscriptPart::ToolResult {
                    call_id: call_id.filter(|id| wire::call_id(id)),
                    output,
                    error,
                    truncated,
                }
            }),
            TranscriptPart::Unknown => None,
        };
        parts.extend(part);
    }
    if parts.is_empty() || !wire::message_id(&message.id) {
        return None;
    }
    parts.truncate(64);
    Some(UploadMessage {
        id: message.id,
        position: message.position,
        role: message.role,
        at: message
            .at
            .filter(|at| (EARLIEST_MS..=LATEST_MS).contains(at)),
        parts,
    })
}

/// `text` at most `limit` bytes, cut at a character boundary.
fn cut(mut text: String, limit: usize) -> (String, bool) {
    if text.len() <= limit {
        return (text, false);
    }
    let mut end = limit;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text.truncate(end);
    (text, true)
}

/// Where each session's chat is up to, kept in a file (0600) when given one.
struct Cursors {
    path: Option<PathBuf>,
    entries: HashMap<String, Remembered>,
    /// Increases with each change, for keeping the most recently used.
    clock: u64,
}

#[derive(Clone, Serialize, Deserialize)]
struct Remembered {
    cursor: TranscriptCursor,
    used: u64,
}

#[derive(Serialize, Deserialize)]
struct CursorFile {
    version: u8,
    sessions: HashMap<String, Remembered>,
}

impl Cursors {
    fn load(path: Option<PathBuf>) -> Self {
        let entries: HashMap<String, Remembered> = path
            .as_ref()
            .and_then(|path| std::fs::read(path).ok())
            .and_then(|bytes| serde_json::from_slice::<CursorFile>(&bytes).ok())
            .filter(|file| file.version == 1)
            .map(|file| file.sessions)
            .unwrap_or_default();
        let clock = entries.values().map(|entry| entry.used).max().unwrap_or(0);
        Self {
            path,
            entries,
            clock,
        }
    }

    fn get(&self, session: &SessionId) -> Option<TranscriptCursor> {
        self.entries
            .get(session.as_str())
            .map(|entry| entry.cursor.clone())
    }

    fn set(&mut self, session: &SessionId, cursor: TranscriptCursor) {
        self.clock += 1;
        self.entries.insert(
            session.as_str().to_owned(),
            Remembered {
                cursor,
                used: self.clock,
            },
        );
        while self.entries.len() > MAX_CURSORS {
            let Some(oldest) = self
                .entries
                .iter()
                .min_by_key(|(_, entry)| entry.used)
                .map(|(id, _)| id.clone())
            else {
                break;
            };
            self.entries.remove(&oldest);
        }
        self.save();
    }

    fn save(&self) {
        let Some(path) = &self.path else {
            return;
        };
        let file = CursorFile {
            version: 1,
            sessions: self.entries.clone(),
        };
        let Ok(bytes) = serde_json::to_vec(&file) else {
            return;
        };
        if write_private(path, &bytes).is_err() {
            eprintln!("rakumux-status: saving where chats are up to failed");
        }
    }
}

/// Replace `path` with `bytes`, readable by its owner only.
pub(crate) fn write_private(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let staged = path.with_extension("json.tmp");
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&staged)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    std::fs::rename(&staged, path)
}

#[cfg(test)]
mod tests;
