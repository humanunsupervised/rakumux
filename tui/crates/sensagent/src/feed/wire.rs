//! The feed's wire messages (`contracts/feed/`): upload bodies built here,
//! pages and acceptances read back within the contract's bounds.

use std::collections::VecDeque;
use std::sync::LazyLock;
use std::time::Duration;

use regex::Regex;
use serde::{Deserialize, Serialize};
use session_core::api::{TranscriptMessage, TranscriptPart, TranscriptRole};

use super::cache::{
    FeedEntry, FeedPrompt, FeedReply, OpenTurn, ReplyAttachment, ReplyFailure, ReplyStatus,
};
use super::chats::{ChatName, UploadMessage};
use crate::cloud::FeedHttpReply;

/// Entries per upload (contract).
pub(crate) const MAX_UPLOAD_ENTRIES: usize = 50;
/// Upload bodies stay under this, well within the contract's 1 MiB.
pub(crate) const UPLOAD_BUDGET: usize = 768 * 1024;
/// Entries asked for per read (the contract allows 200).
pub(crate) const PAGE_LIMIT: u32 = 100;
/// The longest reply text, in UTF-8 bytes (contract).
pub(crate) const MAX_TEXT_BYTES: usize = 32 * 1024;
/// Messages per chat upload (contract).
pub(crate) const MAX_UPLOAD_MESSAGES: usize = 100;
/// Chat upload bodies stay under this, well within the contract's 4 MiB.
pub(crate) const CHAT_BUDGET: usize = 2 * 1024 * 1024;
/// Messages per turn read (contract).
pub(crate) const MAX_TURN_MESSAGES: usize = 200;
/// Replies asked for per read (the contract allows 200).
pub(crate) const REPLIES_LIMIT: u32 = 100;
/// The longest reply text, in UTF-8 bytes (contract).
pub(crate) const MAX_PROMPT_BYTES: usize = 8 * 1024;
/// The most of a turn's prompt a page carries (`turnPrompt`), in UTF-8 bytes.
const MAX_PROMPT_TEXT: usize = 4096;
/// Files attached to one reply, and the largest (contract).
pub(crate) const MAX_ATTACHMENTS: usize = 5;
pub(crate) const MAX_ATTACHMENT_BYTES: u64 = 4 * 1024 * 1024;
/// What this machine handles beyond the contract's first version, said with
/// its session list when it can save replies' files (`capabilities`).
pub(crate) const REPLY_ATTACHMENTS: &str = "reply_attachments";
/// Said with every session list: this machine sends what it uses while
/// rakumux.com asks, for rtop there (`resources-upload`).
pub(crate) const RESOURCES: &str = "resources";
/// Said with the session list while the daemon answers agents' questions
/// (`session.answer`): rakumux.com takes answers to them for this machine.
pub(crate) const QUESTION_ANSWERS: &str = "question_answers";
/// Questions answered at once, options chosen per question, and the longest
/// question, option and free answer an answer carries (contract).
const MAX_ANSWERS: usize = 8;
const MAX_CHOICES: usize = 16;
const MAX_QUESTION_BYTES: usize = 4096;
const MAX_CHOICE_BYTES: usize = 800;
const MAX_ANSWER_TEXT_BYTES: usize = 8192;
/// The largest resources upload the contract takes (`x-maxBodyBytes`).
const MAX_RESOURCES_BODY_BYTES: usize = 65_536;

/// Why a feed request failed. Static: never reply text, URLs or tokens.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FeedFailure {
    /// The sign-in was refused; renewing it or signing in again helps.
    Unauthorized,
    /// The account's feed is off.
    Off,
    /// Too many requests; try again after the delay, when given.
    RateLimited(Option<Duration>),
    /// The feed can't be reached or is down.
    Unavailable(Option<Duration>),
    /// The request or the answer broke the contract.
    Invalid,
    /// Another machine of the account holds this machine's name: it takes
    /// another (`machine_taken`).
    MachineTaken,
}

/// One request to the feed, ready to send.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum FeedRequest {
    /// Upload these entries (their ids, in order), as `body`.
    Upload { ids: Vec<String>, body: Vec<u8> },
    /// Read a page: the newest (`after` absent) or the entries after a seq.
    Read { after: Option<u64> },
    /// Upload a chat's messages, as `body`.
    Messages { body: Vec<u8> },
    /// Read the turn an entry finished, past `after` when given.
    Turn { entry: String, after: Option<u64> },
    /// Store a reply written here, as `body`.
    ReplyUpload { body: Vec<u8> },
    /// Read the account's replies changed after a seq, as `machine`; with
    /// `answers`, the answers to agents' questions too.
    Replies {
        after: u64,
        machine: String,
        answers: bool,
    },
    /// Say whether a reply was typed into its session, as `body`.
    ReplyStatus { body: Vec<u8> },
    /// Upload this machine's whole session list, as `body`.
    Sessions { body: Vec<u8> },
    /// Upload what this machine uses now (rtop), as `body`.
    Resources { body: Vec<u8> },
    /// Fetch a file attached to a reply: `size` bytes, as the reply says.
    Attachment { id: String, size: u64 },
    /// Ask where to connect to be poked, and a token for it.
    Realtime,
    /// Enrol another machine of the account (`enrolment`), as `body`.
    MachineEnrol { body: Vec<u8> },
    /// Revoke an enrolled machine's credential.
    MachineRevoke { machine: String },
    /// Say whether a new session asked for on rakumux.com was started, as `body`.
    NewSessionStatus { body: Vec<u8> },
}

impl FeedRequest {
    /// `GET /api/v1/feed`'s query for a read.
    pub fn query(after: Option<u64>) -> String {
        match after {
            Some(after) => format!("after={after}&limit={PAGE_LIMIT}"),
            None => format!("limit={PAGE_LIMIT}"),
        }
    }

    /// `GET /api/v1/feed/replies`' query, naming the asking machine so the
    /// answer's `poll_ms` knows whether a reply waits for it, and asking for
    /// session messages too (replies to a session itself, without an entry),
    /// and with `answers` for answers to agents' questions (while the daemon
    /// gives them).
    pub fn replies_query(after: u64, machine: &str, answers: bool) -> String {
        format!(
            "after={after}&limit={REPLIES_LIMIT}&machine={}&session_messages=1{}",
            percent_encoded(machine),
            if answers { "&question_answers=1" } else { "" }
        )
    }

    /// `GET /api/v1/feed/turn`'s query.
    pub fn turn_query(entry: &str, after: Option<u64>) -> String {
        match after {
            Some(after) => format!("entry={entry}&after={after}"),
            None => format!("entry={entry}"),
        }
    }
}

/// `text` for a URL's query: unreserved characters as they are, every other
/// byte as `%XX`.
fn percent_encoded(text: &str) -> String {
    let mut encoded = String::with_capacity(text.len());
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            encoded.push(char::from(byte));
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}

#[derive(Serialize)]
struct Upload<'a> {
    version: u8,
    machine: &'a str,
    entries: Vec<UploadEntry<'a>>,
}

#[derive(Serialize)]
struct UploadEntry<'a> {
    id: &'a str,
    session: UploadSession<'a>,
    agent: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    conversation_id: Option<&'a str>,
    text: &'a str,
    truncated: bool,
    finished_at: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    position: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reply_to: Option<&'a str>,
}

#[derive(Serialize)]
struct UploadSession<'a> {
    id: &'a str,
    name: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    folder: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    branch: Option<&'a str>,
}

fn upload_entry(entry: &FeedEntry) -> UploadEntry<'_> {
    UploadEntry {
        id: &entry.id,
        session: UploadSession {
            id: &entry.session_id,
            name: &entry.session_name,
            folder: entry.folder.as_deref(),
            branch: entry.branch.as_deref(),
        },
        agent: &entry.agent,
        conversation_id: entry.conversation_id.as_deref(),
        text: &entry.text,
        truncated: entry.truncated,
        finished_at: entry.finished_at,
        position: entry.position,
        reply_to: entry.reply_to.as_deref(),
    }
}

/// An upload of the first of `entries` that fit one request, oldest first.
pub(crate) fn upload(machine: &str, entries: &[&FeedEntry]) -> Option<FeedRequest> {
    let mut chosen = Vec::new();
    let mut size = 256 + machine.len();
    for entry in entries.iter().take(MAX_UPLOAD_ENTRIES) {
        let one = serde_json::to_vec(&upload_entry(entry)).ok()?.len() + 1;
        if !chosen.is_empty() && size + one > UPLOAD_BUDGET {
            break;
        }
        size += one;
        chosen.push(*entry);
    }
    if chosen.is_empty() {
        return None;
    }
    let body = serde_json::to_vec(&Upload {
        version: 1,
        machine,
        entries: chosen.iter().map(|entry| upload_entry(entry)).collect(),
    })
    .ok()?;
    Some(FeedRequest::Upload {
        ids: chosen.iter().map(|entry| entry.id.clone()).collect(),
        body,
    })
}

#[derive(Serialize)]
struct ChatUpload<'a> {
    version: u8,
    machine: &'a str,
    chat: UploadChat<'a>,
    messages: Vec<&'a UploadMessage>,
}

#[derive(Serialize)]
struct UploadChat<'a> {
    agent: &'a str,
    conversation_id: &'a str,
    session: UploadSession<'a>,
}

/// An upload of the first of a chat's `messages` that fit one request, and
/// how many that is (at least one).
pub(crate) fn chat_upload(
    machine: &str,
    chat: &ChatName,
    messages: &VecDeque<UploadMessage>,
) -> Option<(FeedRequest, usize)> {
    let mut chosen = Vec::new();
    let mut size = 512 + machine.len() + chat.session_name.len();
    for message in messages.iter().take(MAX_UPLOAD_MESSAGES) {
        let one = serde_json::to_vec(message).ok()?.len() + 1;
        if !chosen.is_empty() && size + one > CHAT_BUDGET {
            break;
        }
        size += one;
        chosen.push(message);
    }
    if chosen.is_empty() {
        return None;
    }
    let count = chosen.len();
    let body = serde_json::to_vec(&ChatUpload {
        version: 1,
        machine,
        chat: UploadChat {
            agent: &chat.agent,
            conversation_id: &chat.conversation_id,
            session: UploadSession {
                id: &chat.session_id,
                name: &chat.session_name,
                folder: chat.folder.as_deref(),
                // A chat outlives its branch: the entries say each turn's.
                branch: None,
            },
        },
        messages: chosen,
    })
    .ok()?;
    Some((FeedRequest::Messages { body }, count))
}

#[derive(Deserialize)]
struct Stored {
    version: u8,
    accepted: u32,
}

/// That a chat upload was stored.
pub(crate) fn stored(reply: &FeedHttpReply) -> Result<u32, FeedFailure> {
    if reply.status != 200 {
        return Err(failure(reply));
    }
    let stored: Stored = serde_json::from_slice(&reply.body).map_err(|_| FeedFailure::Invalid)?;
    if stored.version != 1 {
        return Err(FeedFailure::Invalid);
    }
    Ok(stored.accepted)
}

#[derive(Serialize)]
struct SessionsUpload<'a> {
    version: u8,
    machine: &'a str,
    sessions: &'a [super::sessions::SessionState],
    at: i64,
    #[serde(skip_serializing_if = "<[_]>::is_empty")]
    capabilities: &'a [&'a str],
}

/// This machine's session list as `sessions-upload`, with what else it
/// handles (`capabilities`, left out when nothing); `None` only when it
/// can't be encoded.
pub(crate) fn sessions_upload(
    machine: &str,
    sessions: &[super::sessions::SessionState],
    at: i64,
    capabilities: &[&str],
) -> Option<FeedRequest> {
    let body = serde_json::to_vec(&SessionsUpload {
        version: 1,
        machine,
        sessions,
        at: at.max(1_577_836_800_000),
        capabilities,
    })
    .ok()?;
    Some(FeedRequest::Sessions { body })
}

#[derive(Deserialize)]
struct SessionsStoredWire {
    version: u8,
    stored: u32,
    #[serde(default)]
    accepts: Vec<String>,
}

/// What rakumux.com holds for this machine now, and what more it takes in
/// a session list and in chats (`sessions-stored`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SessionsStored {
    pub stored: u32,
    /// It takes each session's places (`accepts` has `places`).
    pub places: bool,
    /// It takes subagents' messages in chats (`accepts` has `subagents`).
    pub subagents: bool,
}

/// How many sessions rakumux.com holds for this machine now, and whether it
/// takes their places.
pub(crate) fn sessions_stored(reply: &FeedHttpReply) -> Result<SessionsStored, FeedFailure> {
    if reply.status != 200 {
        return Err(failure(reply));
    }
    let stored: SessionsStoredWire =
        serde_json::from_slice(&reply.body).map_err(|_| FeedFailure::Invalid)?;
    if stored.version != 1 || stored.stored > 200 {
        return Err(FeedFailure::Invalid);
    }
    Ok(SessionsStored {
        stored: stored.stored,
        places: stored.accepts.iter().any(|name| name == "places"),
        subagents: stored.accepts.iter().any(|name| name == "subagents"),
    })
}

/// What this machine uses now, as rtop on rakumux.com shows it
/// (`resources-upload`); `None` when it doesn't fit the contract's cap.
pub(crate) fn resources_upload(upload: &super::resources::Upload<'_>) -> Option<FeedRequest> {
    let body = serde_json::to_vec(upload).ok()?;
    (body.len() <= MAX_RESOURCES_BODY_BYTES).then_some(FeedRequest::Resources { body })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ResourcesStoredWire {
    version: u8,
    wanted: bool,
}

/// Whether rakumux.com still wants what this machine uses
/// (`resources-stored`): someone still has rtop open there.
pub(crate) fn resources_stored(reply: &FeedHttpReply) -> Result<bool, FeedFailure> {
    if reply.status != 200 {
        return Err(failure(reply));
    }
    let stored: ResourcesStoredWire =
        serde_json::from_slice(&reply.body).map_err(|_| FeedFailure::Invalid)?;
    if stored.version != 1 {
        return Err(FeedFailure::Invalid);
    }
    Ok(stored.wanted)
}

#[derive(Deserialize)]
struct Turn {
    version: u8,
    messages: Vec<TranscriptMessage>,
    more: bool,
}

/// A turn as read: its messages in order, and whether it goes on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TurnRead {
    pub messages: Vec<TranscriptMessage>,
    pub more: bool,
}

/// A turn's messages, stripped of anything a terminal would act on; parts of
/// kinds this version doesn't know are left out.
pub(crate) fn turn(reply: &FeedHttpReply) -> Result<TurnRead, FeedFailure> {
    if reply.status != 200 {
        return Err(failure(reply));
    }
    let turn: Turn = serde_json::from_slice(&reply.body).map_err(|_| FeedFailure::Invalid)?;
    if turn.version != 1 || turn.messages.len() > MAX_TURN_MESSAGES {
        return Err(FeedFailure::Invalid);
    }
    let messages = turn
        .messages
        .into_iter()
        .filter(|message| message.role != TranscriptRole::Unknown)
        .map(|message| TranscriptMessage {
            id: line(&message.id),
            parts: message
                .parts
                .into_iter()
                .filter_map(|part| match part {
                    TranscriptPart::Text { text, truncated } => Some(TranscriptPart::Text {
                        text: plain(&text),
                        truncated,
                    }),
                    TranscriptPart::Reasoning { text, truncated } => {
                        Some(TranscriptPart::Reasoning {
                            text: plain(&text),
                            truncated,
                        })
                    }
                    TranscriptPart::ToolCall {
                        call_id,
                        name,
                        input,
                        truncated,
                    } => Some(TranscriptPart::ToolCall {
                        call_id: call_id.map(|id| line(&id)),
                        name: line(&name),
                        input: plain(&input),
                        truncated,
                    }),
                    TranscriptPart::ToolResult {
                        call_id,
                        output,
                        error,
                        truncated,
                    } => Some(TranscriptPart::ToolResult {
                        call_id: call_id.map(|id| line(&id)),
                        output: plain(&output),
                        error,
                        truncated,
                    }),
                    TranscriptPart::Unknown => None,
                })
                .collect(),
            ..message
        })
        .collect();
    Ok(TurnRead {
        messages,
        more: turn.more,
    })
}

/// The contract's conversation id: 1–128 of letters, digits, `.`, `_`, `:`, `-`.
pub(crate) fn conversation_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id.chars().next().is_some_and(|c| c.is_ascii_alphanumeric())
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | ':' | '-'))
}

/// The contract's message id: as a conversation id.
pub(crate) fn message_id(id: &str) -> bool {
    conversation_id(id)
}

/// The contract's tool call id: as a conversation id, `|` too.
pub(crate) fn call_id(id: &str) -> bool {
    conversation_id(&id.replace('|', ":"))
}

/// The contract's subagent id (`subagentId`): as a conversation id, at most
/// 64 characters.
pub(crate) fn subagent_id(id: &str) -> bool {
    id.len() <= 64 && conversation_id(id)
}

/// The contract's attached file name (`fileName`): one visible line of at
/// most 128 characters and 255 UTF-8 bytes (what Linux and macOS take), never
/// a path (`/`, `\`), `.` or `..`.
pub(crate) fn file_name(name: &str) -> bool {
    let (Some(hidden), Some(visible)) = (LABEL_HIDDEN.as_ref(), LABEL_VISIBLE.as_ref()) else {
        return false;
    };
    !name.is_empty()
        && name.len() <= 255
        && name.chars().count() <= 128
        && !name.contains(['/', '\\'])
        && name != "."
        && name != ".."
        && !hidden.is_match(name)
        && visible.is_match(name)
}

/// The contract's media type: lowercase `type/subtype` without parameters.
fn media_type(text: &str) -> bool {
    let token = |part: &str| {
        part.bytes()
            .next()
            .is_some_and(|first| first.is_ascii_lowercase() || first.is_ascii_digit())
            && part.bytes().all(|byte| {
                byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"!#$&^_.+-".contains(&byte)
            })
    };
    text.len() <= 128
        && text
            .split_once('/')
            .is_some_and(|(kind, subtype)| token(kind) && token(subtype))
}

static LABEL_HIDDEN: LazyLock<Option<Regex>> = LazyLock::new(|| {
    Regex::new(r"[\p{Cc}\p{Cf}\p{Zl}\p{Zp}\p{Default_Ignorable_Code_Point}]").ok()
});
static LABEL_VISIBLE: LazyLock<Option<Regex>> =
    LazyLock::new(|| Regex::new(r"[\p{L}\p{N}\p{P}\p{S}]").ok());

/// `text` as the contract's one visible line of at most `characters`: what a
/// terminal would act on removed; `None` when nothing visible is left.
pub(crate) fn label(text: &str, characters: usize) -> Option<String> {
    let hidden = LABEL_HIDDEN.as_ref()?;
    let label: String = hidden
        .replace_all(text, "")
        .trim()
        .chars()
        .take(characters)
        .collect();
    LABEL_VISIBLE.as_ref()?.is_match(&label).then_some(label)
}

#[derive(Serialize)]
struct ReplyUpload<'a> {
    version: u8,
    id: &'a str,
    entry: &'a str,
    text: &'a str,
    from: &'a str,
}

/// The upload of a reply written here, `id` its new ULID; `None` when `text`
/// says nothing or is too long for the contract.
pub(crate) fn reply_upload(id: &str, entry: &str, text: &str, from: &str) -> Option<FeedRequest> {
    let text = crate::cloud::prompt_text(text)?;
    if text.len() > MAX_PROMPT_BYTES {
        return None;
    }
    let body = serde_json::to_vec(&ReplyUpload {
        version: 1,
        id,
        entry,
        text: &text,
        from,
    })
    .ok()?;
    Some(FeedRequest::ReplyUpload { body })
}

#[derive(Serialize)]
struct SessionMessageUpload<'a> {
    version: u8,
    id: &'a str,
    session: SessionTarget<'a>,
    text: &'a str,
    from: &'a str,
}

#[derive(Serialize)]
struct SessionTarget<'a> {
    machine: &'a str,
    id: &'a str,
}

/// The upload of a session message written here (`id` its new ULID) to
/// session `session` of `machine` itself: for a session with nothing to reply
/// to in the chat it shows. `None` when `text` says nothing or is too long, or
/// the session isn't one the contract can name.
pub(crate) fn session_message_upload(
    id: &str,
    machine: &str,
    session: &str,
    text: &str,
    from: &str,
) -> Option<FeedRequest> {
    if !crate::feed::is_entry_id(session) || !within(machine, 256) {
        return None;
    }
    let text = crate::cloud::prompt_text(text)?;
    if text.len() > MAX_PROMPT_BYTES {
        return None;
    }
    let body = serde_json::to_vec(&SessionMessageUpload {
        version: 1,
        id,
        session: SessionTarget {
            machine,
            id: session,
        },
        text: &text,
        from,
    })
    .ok()?;
    Some(FeedRequest::ReplyUpload { body })
}

#[derive(Serialize)]
struct ReplyStatusBody<'a> {
    version: u8,
    id: &'a str,
    machine: &'a str,
    status: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    failure: Option<ReplyFailure>,
}

/// A reply's machine saying it typed it into the session (`failure` absent)
/// or couldn't.
pub(crate) fn reply_status(
    id: &str,
    machine: &str,
    failure: Option<ReplyFailure>,
) -> Option<FeedRequest> {
    let body = serde_json::to_vec(&ReplyStatusBody {
        version: 1,
        id,
        machine,
        status: if failure.is_some() {
            "failed"
        } else {
            "delivered"
        },
        failure,
    })
    .ok()?;
    Some(FeedRequest::ReplyStatus { body })
}

#[derive(Deserialize)]
struct WireReply {
    seq: u64,
    id: String,
    /// Absent on a session message, with `thread`.
    #[serde(default)]
    entry: Option<String>,
    #[serde(default)]
    thread: Option<String>,
    machine: String,
    session: PageSession,
    agent: String,
    #[serde(default)]
    conversation_id: Option<String>,
    text: String,
    /// Read as any JSON, so attachments off the contract leave out this row
    /// only (see [`attachments`]), never the page.
    #[serde(default)]
    attachments: Option<serde_json::Value>,
    #[serde(default)]
    from: Option<String>,
    /// Read as any JSON, so an answer off the contract leaves out this row
    /// only (see [`question_answer`]).
    #[serde(default)]
    answer: Option<serde_json::Value>,
    status: ReplyStatus,
    #[serde(default)]
    failure: Option<ReplyFailure>,
    #[serde(default)]
    response: Option<String>,
    created_at: i64,
    status_at: i64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WireAnswer {
    call_id: String,
    answers: Vec<WireQuestionAnswer>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WireQuestionAnswer {
    question: String,
    #[serde(default)]
    choices: Vec<String>,
    #[serde(default)]
    text: Option<String>,
}

/// What a session message answers instead of saying something
/// (`contracts/feed/` `questionAnswer`): the question an agent asks in its
/// own dialog, by its tool call, and the answer to each of its questions,
/// for the daemon to give that dialog (`session_answer`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct QuestionReply {
    pub call_id: String,
    pub answers: Vec<session_core::api::QuestionAnswer>,
}

/// A reply's answer to an agent's question, within the contract's bounds;
/// `Err` when it isn't, and the row is then left out like any off the
/// contract: an answer is given whole or not at all.
fn question_answer(value: Option<serde_json::Value>) -> Result<Option<QuestionReply>, ()> {
    let Some(value) = value else {
        return Ok(None);
    };
    let answer: WireAnswer = serde_json::from_value(value).map_err(|_| ())?;
    let text = |text: &str, bytes: usize| within(text, bytes) && plain(text) == text;
    let fits = call_id(&answer.call_id)
        && (1..=MAX_ANSWERS).contains(&answer.answers.len())
        && answer.answers.iter().all(|given| {
            text(&given.question, MAX_QUESTION_BYTES)
                && given.choices.len() <= MAX_CHOICES
                && given
                    .choices
                    .iter()
                    .all(|choice| text(choice, MAX_CHOICE_BYTES) && line(choice) == *choice)
                && given
                    .text
                    .as_deref()
                    .is_none_or(|said| text(said, MAX_ANSWER_TEXT_BYTES) && line(said) == said)
                && (!given.choices.is_empty() || given.text.is_some())
        });
    if !fits {
        return Err(());
    }
    Ok(Some(QuestionReply {
        call_id: answer.call_id,
        answers: answer
            .answers
            .into_iter()
            .map(|given| session_core::api::QuestionAnswer {
                question: given.question,
                choices: given.choices,
                text: given.text,
            })
            .collect(),
    }))
}

#[derive(Deserialize)]
struct WireAttachment {
    id: String,
    name: String,
    media_type: String,
    size: u64,
}

/// A reply's attached files, each within the contract's bounds; `None` when
/// they aren't, and the reply is then left out like any row off the contract:
/// its text is never typed without its files.
fn attachments(value: Option<serde_json::Value>) -> Option<Vec<ReplyAttachment>> {
    let Some(value) = value else {
        return Some(Vec::new());
    };
    let files: Vec<WireAttachment> = serde_json::from_value(value).ok()?;
    if files.len() > MAX_ATTACHMENTS {
        return None;
    }
    files
        .into_iter()
        .map(|file| {
            let fits = crate::feed::is_entry_id(&file.id)
                && file_name(&file.name)
                && media_type(&file.media_type)
                && (1..=MAX_ATTACHMENT_BYTES).contains(&file.size);
            fits.then_some(ReplyAttachment {
                id: file.id,
                name: file.name,
                media_type: file.media_type,
                size: file.size,
            })
        })
        .collect()
}

/// A session message: what the user wrote on rakumux.com to a session that
/// had no answer to reply to (`contracts/feed/` `reply` without `entry`). It
/// is typed into the session like a reply, but never shown in TUIs' threads.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SessionMessage {
    pub seq: u64,
    pub id: String,
    pub machine: String,
    pub session_id: String,
    /// The chat the session showed when it was written, when it said.
    pub conversation_id: Option<String>,
    pub text: String,
    /// Files the user attached, typed in as their paths after the text.
    pub attachments: Vec<ReplyAttachment>,
    pub from: Option<String>,
    pub status: ReplyStatus,
    /// An answer to the question its agent asks in its own dialog, given to
    /// that dialog in place of typing `text`; only with `conversation_id`.
    pub answer: Option<QuestionReply>,
}

/// A reply as read: one to an entry, or a session message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Received {
    Reply(FeedReply),
    Message(SessionMessage),
}

impl WireReply {
    /// Within the contract's bounds, stripped of anything a terminal would act on.
    fn checked(mut self) -> Option<Received> {
        let attachments = attachments(self.attachments.take())?;
        let answer = question_answer(self.answer.take()).ok()?;
        let ids = [&self.id, &self.session.id];
        let fits = self.seq > 0
            && ids.iter().all(|id| crate::feed::is_entry_id(id))
            && within(&self.machine, 256)
            && within(&self.session.name, 256)
            && self
                .session
                .folder
                .as_deref()
                .is_none_or(|f| within(f, 512))
            && within(&self.agent, 32)
            && self.conversation_id.as_deref().is_none_or(conversation_id)
            && within(&self.text, MAX_PROMPT_BYTES)
            && self.from.as_deref().is_none_or(|f| within(f, 256))
            && self
                .response
                .as_deref()
                .is_none_or(crate::feed::is_entry_id);
        if !fits {
            return None;
        }
        // An answer is to a session's chat, with no files.
        if answer.is_some()
            && (self.entry.is_some() || self.conversation_id.is_none() || !attachments.is_empty())
        {
            return None;
        }
        match (self.entry, self.thread, self.conversation_id) {
            (Some(entry), Some(thread), Some(conversation_id))
                if crate::feed::is_entry_id(&entry) && crate::feed::is_entry_id(&thread) =>
            {
                Some(Received::Reply(FeedReply {
                    seq: self.seq,
                    id: self.id,
                    entry,
                    thread,
                    machine: line(&self.machine),
                    session_id: self.session.id,
                    session_name: line(&self.session.name),
                    folder: self.session.folder.map(|folder| line(&folder)),
                    agent: line(&self.agent),
                    conversation_id,
                    text: plain(&self.text),
                    attachments,
                    from: self.from.map(|from| line(&from)),
                    status: self.status,
                    failure: self.failure,
                    response: self.response,
                    created_at: self.created_at,
                    status_at: self.status_at,
                }))
            }
            (None, None, conversation_id) => Some(Received::Message(SessionMessage {
                seq: self.seq,
                id: self.id,
                machine: line(&self.machine),
                session_id: self.session.id,
                conversation_id,
                text: plain(&self.text),
                attachments,
                from: self.from.map(|from| line(&from)),
                status: self.status,
                answer,
            })),
            // Half a reply: outside the contract.
            _ => None,
        }
    }
}

#[derive(Deserialize)]
struct ReplyAnswer {
    version: u8,
    reply: WireReply,
}

/// The stored reply (or session message) a reply upload or status change
/// answers with.
pub(crate) fn reply(reply: &FeedHttpReply) -> Result<Received, FeedFailure> {
    if reply.status != 200 {
        return Err(failure(reply));
    }
    let answer: ReplyAnswer =
        serde_json::from_slice(&reply.body).map_err(|_| FeedFailure::Invalid)?;
    if answer.version != 1 {
        return Err(FeedFailure::Invalid);
    }
    answer.reply.checked().ok_or(FeedFailure::Invalid)
}

#[derive(Deserialize)]
struct WireReplies {
    version: u8,
    replies: Vec<WireReply>,
    latest_seq: u64,
    more: bool,
    #[serde(default)]
    poll_ms: Option<u64>,
    /// Someone has rtop open on rakumux.com (present only then).
    #[serde(default)]
    resources_wanted: bool,
    /// New sessions asked of this machine (`new_sessions`), read row by row.
    #[serde(default)]
    new_sessions: Option<serde_json::Value>,
}

/// Replies as read: those that fit the contract (replies to entries, and
/// session messages apart), the page's and the account's greatest seq,
/// whether more follow, and when rakumux.com says to read again (kept within
/// 1–60 s), when it does.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct RepliesRead {
    pub replies: Vec<FeedReply>,
    pub messages: Vec<SessionMessage>,
    /// The greatest seq of the page's rows, those left out as off the
    /// contract too, so a page of rows that don't fit can't stall reads.
    pub page_seq: u64,
    pub latest_seq: u64,
    pub more: bool,
    pub poll: Option<Duration>,
    /// Someone has rtop open on rakumux.com: send what this machine uses.
    pub resources_wanted: bool,
    /// The new sessions pending for this machine, when the read brought them.
    pub new_sessions: Vec<super::new_sessions::NewSessionAsk>,
}

impl RepliesRead {
    /// Where the next read continues: see [`read_to`].
    pub fn read_to(&self) -> u64 {
        read_to(self.page_seq, self.latest_seq, self.more)
    }
}

/// Where reads continue after a page: past every row on it and, once no more
/// wait, past the account's greatest seq. While more wait, rows between the
/// page's greatest seq and the account's are still to be read (a page holds
/// at most its limit), so the account's isn't taken then.
fn read_to(page_seq: u64, latest_seq: u64, more: bool) -> u64 {
    if more {
        page_seq
    } else {
        page_seq.max(latest_seq)
    }
}

/// A page of the account's changed replies.
pub(crate) fn replies(reply: &FeedHttpReply) -> Result<RepliesRead, FeedFailure> {
    if reply.status != 200 {
        return Err(failure(reply));
    }
    let page: WireReplies =
        serde_json::from_slice(&reply.body).map_err(|_| FeedFailure::Invalid)?;
    if page.version != 1 || page.replies.len() > 200 {
        return Err(FeedFailure::Invalid);
    }
    let page_seq = page
        .replies
        .iter()
        .map(|reply| reply.seq)
        .max()
        .unwrap_or(0);
    let mut replies = Vec::new();
    let mut messages = Vec::new();
    for received in page.replies.into_iter().filter_map(WireReply::checked) {
        match received {
            Received::Reply(reply) => replies.push(reply),
            Received::Message(message) => messages.push(message),
        }
    }
    Ok(RepliesRead {
        replies,
        messages,
        page_seq,
        latest_seq: page.latest_seq,
        more: page.more,
        poll: page
            .poll_ms
            .map(|ms| Duration::from_millis(ms.clamp(1_000, 60_000))),
        resources_wanted: page.resources_wanted,
        new_sessions: super::new_sessions::asks(page.new_sessions),
    })
}

/// Where this machine connects to be poked (`realtime.schema.json`), and the
/// token that admits it there once.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct RealtimeToken {
    /// `wss://…`; `ws://` only to this machine itself (a local worker).
    pub url: String,
    pub token: String,
}

impl std::fmt::Debug for RealtimeToken {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RealtimeToken")
            .field("url", &self.url)
            .field("token", &"[REDACTED]")
            .finish()
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WireRealtime {
    version: u8,
    url: String,
    token: String,
    #[allow(dead_code)]
    expires_at: u64,
}

/// The realtime answer, within the contract: a TLS WebSocket (or one on this
/// machine), and a token a header can carry. A cloud without pokes answers
/// 404, which, like any other refusal, is `Invalid` here.
pub(crate) fn realtime(reply: &FeedHttpReply) -> Result<RealtimeToken, FeedFailure> {
    if reply.status != 200 {
        return Err(failure(reply));
    }
    let answer: WireRealtime =
        serde_json::from_slice(&reply.body).map_err(|_| FeedFailure::Invalid)?;
    let url_fits = answer.url.len() <= 512 && realtime_url(&answer.url);
    let token_fits = (16..=1_024).contains(&answer.token.len())
        && answer
            .token
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'));
    if answer.version != 1 || !url_fits || !token_fits {
        return Err(FeedFailure::Invalid);
    }
    Ok(RealtimeToken {
        url: answer.url,
        token: answer.token,
    })
}

/// `wss://host[:port][/path]`, or `ws://` to a loopback host: no credentials,
/// query or fragment.
pub(crate) fn realtime_url(url: &str) -> bool {
    let (rest, local_only) = match url.strip_prefix("wss://") {
        Some(rest) => (rest, false),
        None => match url.strip_prefix("ws://") {
            Some(rest) => (rest, true),
            None => return false,
        },
    };
    let (authority, path) = rest.split_once('/').unwrap_or((rest, ""));
    let host = match authority.rsplit_once(':') {
        Some((host, port))
            if !port.is_empty() && port.len() <= 5 && port.bytes().all(|b| b.is_ascii_digit()) =>
        {
            host
        }
        Some(_) => return false,
        None => authority,
    };
    let host_fits = !host.is_empty()
        && host
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-'));
    let path_fits = path
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'~' | b'/' | b'-'));
    host_fits && path_fits && (!local_only || matches!(host, "127.0.0.1" | "localhost"))
}

/// What an attachment download brought back.
#[derive(PartialEq, Eq)]
pub(crate) enum Download {
    /// The file, exactly the size its reply said.
    File(Vec<u8>),
    /// Gone for good: rakumux.com doesn't have it (404: the reply isn't
    /// pending any more, or its file was deleted), or it isn't the size its
    /// reply said.
    Unavailable,
}

impl std::fmt::Debug for Download {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::File(bytes) => write!(formatter, "File({} bytes)", bytes.len()),
            Self::Unavailable => formatter.write_str("Unavailable"),
        }
    }
}

/// An attachment download's answer, its body taken as bytes and never
/// parsed: the file when it is exactly `size` bytes. Errors other than 404
/// are those of every feed route.
pub(crate) fn download(reply: FeedHttpReply, size: u64) -> Result<Download, FeedFailure> {
    match reply.status {
        200 if u64::try_from(reply.body.len()).ok() == Some(size) => Ok(Download::File(reply.body)),
        200 | 404 => Ok(Download::Unavailable),
        _ => Err(failure(&reply)),
    }
}

#[derive(Deserialize)]
struct Accepted {
    version: u8,
    accepted: Vec<AcceptedEntry>,
}

#[derive(Deserialize)]
struct AcceptedEntry {
    id: String,
    seq: u64,
}

#[derive(Deserialize)]
struct Page {
    version: u8,
    entries: Vec<PageEntry>,
    latest_seq: u64,
    more: bool,
    #[serde(default)]
    open: Option<Vec<WireOpen>>,
}

#[derive(Deserialize)]
struct WirePrompt {
    id: String,
    text: String,
    #[serde(default)]
    truncated: bool,
    #[serde(default)]
    at: Option<i64>,
}

impl WirePrompt {
    /// Within the contract's bounds, stripped of what a terminal would act on.
    fn checked(self) -> Option<FeedPrompt> {
        let fits = conversation_id(&self.id) && self.text.len() <= MAX_PROMPT_TEXT;
        fits.then(|| FeedPrompt {
            id: self.id,
            text: plain(&self.text),
            truncated: self.truncated,
            at: self.at,
        })
    }
}

#[derive(Deserialize)]
struct WireOpen {
    machine: String,
    session: PageSession,
    agent: String,
    conversation_id: String,
    prompt: WirePrompt,
}

impl WireOpen {
    fn checked(self) -> Option<OpenTurn> {
        let fits = within(&self.machine, 256)
            && within(&self.session.id, 64)
            && within(&self.session.name, 256)
            && self
                .session
                .folder
                .as_deref()
                .is_none_or(|f| within(f, 512))
            && within(&self.agent, 32)
            && conversation_id(&self.conversation_id);
        if !fits {
            return None;
        }
        Some(OpenTurn {
            machine: line(&self.machine),
            session_id: self.session.id,
            session_name: line(&self.session.name),
            folder: self.session.folder.map(|folder| line(&folder)),
            branch: self.session.branch.map(|branch| line(&branch)),
            agent: line(&self.agent),
            conversation_id: self.conversation_id,
            prompt: self.prompt.checked()?,
        })
    }
}

#[derive(Deserialize)]
struct PageEntry {
    seq: u64,
    id: String,
    machine: String,
    session: PageSession,
    agent: String,
    #[serde(default)]
    conversation_id: Option<String>,
    text: String,
    truncated: bool,
    finished_at: i64,
    #[serde(default)]
    reply_to: Option<String>,
    #[serde(default)]
    thread: Option<String>,
    #[serde(default)]
    prompt: Option<WirePrompt>,
}

#[derive(Deserialize)]
struct PageSession {
    id: String,
    name: String,
    #[serde(default)]
    folder: Option<String>,
    #[serde(default)]
    branch: Option<String>,
}

#[derive(Deserialize)]
struct ErrorBody {
    error: ErrorDetail,
}

#[derive(Deserialize)]
struct ErrorDetail {
    code: String,
    #[serde(default)]
    retry_after_seconds: Option<u64>,
}

/// The longest wait a failure can ask for (the contract's: a day).
const MAX_RETRY_AFTER: Duration = Duration::from_secs(86_400);

/// A page as read: its entries, the page's and the account's greatest seq
/// and whether more follow.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct PageRead {
    pub entries: Vec<FeedEntry>,
    /// The greatest seq of the page's rows, those left out as off the
    /// contract too, so a page of rows that don't fit can't stall reads.
    pub page_seq: u64,
    pub latest_seq: u64,
    pub more: bool,
    /// The account's open turns, when rakumux.com sent them.
    pub open: Option<Vec<OpenTurn>>,
}

impl PageRead {
    /// Where the next read continues: see [`read_to`].
    pub fn read_to(&self) -> u64 {
        read_to(self.page_seq, self.latest_seq, self.more)
    }
}

/// The seq of each uploaded id.
pub(crate) fn accepted(reply: &FeedHttpReply) -> Result<Vec<(String, u64)>, FeedFailure> {
    if reply.status != 200 {
        return Err(failure(reply));
    }
    let accepted: Accepted =
        serde_json::from_slice(&reply.body).map_err(|_| FeedFailure::Invalid)?;
    if accepted.version != 1 {
        return Err(FeedFailure::Invalid);
    }
    Ok(accepted
        .accepted
        .into_iter()
        .filter(|entry| entry.seq > 0)
        .map(|entry| (entry.id, entry.seq))
        .collect())
}

/// A page's entries, each checked against the contract's bounds and stripped of
/// anything a terminal would act on; entries that don't fit are left out.
pub(crate) fn page(reply: &FeedHttpReply) -> Result<PageRead, FeedFailure> {
    if reply.status != 200 {
        return Err(failure(reply));
    }
    let page: Page = serde_json::from_slice(&reply.body).map_err(|_| FeedFailure::Invalid)?;
    if page.version != 1 || page.entries.len() > 200 {
        return Err(FeedFailure::Invalid);
    }
    let page_seq = page
        .entries
        .iter()
        .map(|entry| entry.seq)
        .max()
        .unwrap_or(0);
    let entries = page
        .entries
        .into_iter()
        .filter_map(|entry| {
            let fits = entry.seq > 0
                && within(&entry.id, 64)
                && within(&entry.machine, 256)
                && within(&entry.session.id, 64)
                && within(&entry.session.name, 256)
                && entry
                    .session
                    .folder
                    .as_deref()
                    .is_none_or(|f| within(f, 512))
                && within(&entry.agent, 32)
                && entry
                    .conversation_id
                    .as_deref()
                    .is_none_or(|c| within(c, 128))
                && entry.text.len() <= MAX_TEXT_BYTES;
            fits.then(|| FeedEntry {
                id: entry.id,
                seq: Some(entry.seq),
                machine: line(&entry.machine),
                session_id: entry.session.id,
                session_name: line(&entry.session.name),
                folder: entry.session.folder.map(|folder| line(&folder)),
                branch: entry.session.branch.map(|branch| line(&branch)),
                agent: line(&entry.agent),
                conversation_id: entry.conversation_id,
                text: plain(&entry.text),
                truncated: entry.truncated,
                finished_at: entry.finished_at,
                position: None,
                reply_to: entry.reply_to.filter(|id| crate::feed::is_entry_id(id)),
                thread: entry.thread.filter(|id| crate::feed::is_entry_id(id)),
                prompt: entry.prompt.and_then(WirePrompt::checked),
            })
        })
        .collect();
    let open = page.open.map(|open| {
        open.into_iter()
            .take(50)
            .filter_map(WireOpen::checked)
            .collect()
    });
    Ok(PageRead {
        entries,
        page_seq,
        latest_seq: page.latest_seq,
        more: page.more,
        open,
    })
}

/// What a refusal says, from its status and error code.
pub(crate) fn failure(reply: &FeedHttpReply) -> FeedFailure {
    // The code must agree with the status; anything else is a broken answer.
    let error = serde_json::from_slice::<ErrorBody>(&reply.body)
        .ok()
        .map(|body| body.error);
    let code = error.as_ref().map(|error| error.code.as_str());
    // The body's delay may be longer than a Retry-After header carries (a day's quota).
    let delay = error
        .as_ref()
        .and_then(|error| error.retry_after_seconds)
        .map(|seconds| Duration::from_secs(seconds).min(MAX_RETRY_AFTER))
        .max(reply.retry_after);
    match (reply.status, code) {
        (401, Some("unauthenticated")) => FeedFailure::Unauthorized,
        (403, Some("forbidden")) => FeedFailure::Off,
        (409, Some("machine_taken")) => FeedFailure::MachineTaken,
        (429, Some("rate_limited")) => FeedFailure::RateLimited(delay),
        (503, Some("unavailable")) => FeedFailure::Unavailable(delay),
        (500..=599, _) => FeedFailure::Unavailable(None),
        _ => FeedFailure::Invalid,
    }
}

fn within(text: &str, bytes: usize) -> bool {
    !text.is_empty() && text.len() <= bytes
}

/// Whether a terminal would act on `character` rather than show it: controls
/// (but tab and line feed), and bidi and zero-width format characters.
fn hidden(character: char) -> bool {
    (character.is_control() && character != '\n' && character != '\t')
        || matches!(character, '\u{200b}'..='\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2060}'..='\u{206f}' | '\u{feff}')
}

/// Text to show, without anything a terminal would act on.
fn plain(text: &str) -> String {
    text.chars()
        .filter(|character| !hidden(*character))
        .collect()
}

/// One line to show: `plain`, with line breaks and tabs as spaces.
fn line(text: &str) -> String {
    plain(text)
        .chars()
        .map(|character| {
            if character.is_whitespace() {
                ' '
            } else {
                character
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reply(status: u16, body: &str) -> FeedHttpReply {
        FeedHttpReply {
            status,
            retry_after: Some(Duration::from_secs(30)),
            body: body.as_bytes().to_vec(),
        }
    }

    fn entry(id: &str, text: &str) -> FeedEntry {
        FeedEntry {
            id: id.into(),
            seq: None,
            machine: "studio".into(),
            session_id: "01K89ZQ4D2F6H8J0K2M4N6P8R0".into(),
            session_name: "fix-tests".into(),
            folder: Some("rakumux".into()),
            branch: None,
            agent: "claude-code".into(),
            conversation_id: None,
            text: text.into(),
            truncated: false,
            finished_at: 1_761_220_000_000,
            position: None,
            reply_to: None,
            thread: None,
            prompt: None,
        }
    }

    #[test]
    fn a_turn_reads_back_plain_and_parts_of_unknown_kinds_are_left_out() {
        let body = r#"{"version":1,"entry":"01K8A0RZ0V3X5Q2N9T7W4Y6M8B","more":true,"messages":[
            {"id":"u1","position":0,"role":"user","at":1761220000000,"parts":[{"type":"text","text":"Fix it\u202e"}]},
            {"id":"a1","position":9,"role":"assistant","parts":[
                {"type":"tool_call","call_id":"c1","name":"Bash","input":"{}"},
                {"type":"image","url":"x"}]},
            {"id":"t1","position":12,"role":"tool","parts":[{"type":"tool_result","output":"\u001b[31mno","error":true}]}
        ]}"#;
        let read = turn(&reply(200, body)).unwrap();
        assert!(read.more);
        assert_eq!(read.messages.len(), 3);
        assert_eq!(
            read.messages[0].parts,
            [TranscriptPart::Text {
                text: "Fix it".into(),
                truncated: false
            }]
        );
        assert_eq!(read.messages[1].parts.len(), 1);
        assert_eq!(
            read.messages[2].parts,
            [TranscriptPart::ToolResult {
                call_id: None,
                output: "[31mno".into(),
                error: true,
                truncated: false
            }]
        );
        assert_eq!(
            turn(&reply(
                403,
                r#"{"error":{"code":"forbidden","message":"x"}}"#
            )),
            Err(FeedFailure::Off)
        );
        assert_eq!(
            turn(&reply(200, r#"{"version":2,"messages":[],"more":false}"#)),
            Err(FeedFailure::Invalid)
        );
        assert_eq!(stored(&reply(200, r#"{"version":1,"accepted":3}"#)), Ok(3));
    }

    #[test]
    fn a_chat_upload_names_its_chat_and_takes_what_fits() {
        let chat = ChatName {
            agent: "codex".into(),
            conversation_id: "019a1b2c-3d4e-7f60-8a9b-0c1d2e3f4a5b".into(),
            session_id: "01K89ZQ4D2F6H8J0K2M4N6P8R0".into(),
            session_name: "fix-tests".into(),
            folder: None,
        };
        let message = |position: u64, text: &str| UploadMessage {
            id: position.to_string(),
            position,
            role: TranscriptRole::Tool,
            at: None,
            parts: vec![TranscriptPart::ToolResult {
                call_id: Some("call_1".into()),
                output: text.into(),
                error: false,
                truncated: false,
            }],
            subagent: None,
        };
        let big = "z".repeat(600 * 1024);
        let messages: VecDeque<_> = (0..5).map(|n| message(n, &big)).collect();
        let (FeedRequest::Messages { body }, count) =
            chat_upload("studio", &chat, &messages).unwrap()
        else {
            panic!("expected a chat upload");
        };
        assert_eq!(count, 3, "2 MiB a request");
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["chat"]["conversation_id"], chat.conversation_id);
        assert!(json["chat"]["session"].get("folder").is_none());
        assert!(json["messages"][0].get("at").is_none());
        assert_eq!(json["messages"][0]["parts"][0]["type"], "tool_result");
        assert!(json["messages"][0]["parts"][0].get("error").is_none());
        assert_eq!(label(" Bash\u{200b} ", 128).as_deref(), Some("Bash"));
        assert_eq!(label("\u{7}", 128), None);
        assert!(call_id("call_9|fc_1") && !call_id("a b"));
    }

    #[test]
    fn an_upload_takes_what_fits_one_request() {
        let small = entry("A", "short");
        let Some(FeedRequest::Upload { ids, body }) = upload("studio", &[&small]) else {
            panic!("expected an upload");
        };
        assert_eq!(ids, ["A"]);
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["version"], 1);
        assert_eq!(json["machine"], "studio");
        assert_eq!(json["entries"][0]["session"]["folder"], "rakumux");
        assert!(json["entries"][0].get("conversation_id").is_none());

        let big: Vec<FeedEntry> = (0..40)
            .map(|i| entry(&format!("E{i}"), &"x".repeat(30 * 1024)))
            .collect();
        let refs: Vec<&FeedEntry> = big.iter().collect();
        let Some(FeedRequest::Upload { ids, body }) = upload("studio", &refs) else {
            panic!("expected an upload");
        };
        assert!(ids.len() < 40 && !ids.is_empty());
        assert!(body.len() <= UPLOAD_BUDGET);
        assert!(upload("studio", &[]).is_none());
    }

    #[test]
    fn a_page_is_read_and_its_text_made_safe_to_show() {
        let body = r#"{"version":1,"latest_seq":9,"more":false,"entries":[
            {"seq":9,"id":"01K8A0RZ0V3X5Q2N9T7W4Y6M8B","machine":"lab\nbox","session":{"id":"01K89ZQ4D2F6H8J0K2M4N6P8R0","name":"tests"},
             "agent":"codex","text":"done \u001b[31mred\u202e","truncated":false,"finished_at":1,"received_at":2},
            {"seq":0,"id":"bad","machine":"m","session":{"id":"s","name":"n"},"agent":"codex","text":"t","truncated":false,"finished_at":1,"received_at":2}
        ]}"#;
        let read = page(&reply(200, body)).unwrap();
        assert_eq!(read.latest_seq, 9);
        assert_eq!(read.entries.len(), 1, "a zero seq is left out");
        let entry = &read.entries[0];
        assert_eq!(entry.seq, Some(9));
        assert_eq!(entry.machine, "lab box");
        assert_eq!(entry.text, "done [31mred");
        assert_eq!(entry.folder, None);
    }

    #[test]
    fn errors_follow_the_contracts_codes() {
        let error = |status, code: &str| {
            page(&reply(
                status,
                &format!(r#"{{"error":{{"code":"{code}","message":"x"}}}}"#),
            ))
            .unwrap_err()
        };
        assert_eq!(error(401, "unauthenticated"), FeedFailure::Unauthorized);
        assert_eq!(error(403, "forbidden"), FeedFailure::Off);
        assert_eq!(
            error(429, "rate_limited"),
            FeedFailure::RateLimited(Some(Duration::from_secs(30)))
        );
        assert_eq!(
            error(503, "unavailable"),
            FeedFailure::Unavailable(Some(Duration::from_secs(30)))
        );
        assert_eq!(error(403, "rate_limited"), FeedFailure::Invalid);
        let daily = page(&reply(
            429,
            r#"{"error":{"code":"rate_limited","message":"x","retry_after_seconds":7200}}"#,
        ));
        assert_eq!(
            daily.unwrap_err(),
            FeedFailure::RateLimited(Some(Duration::from_secs(7200)))
        );
        assert_eq!(
            page(&reply(502, "<html>")).unwrap_err(),
            FeedFailure::Unavailable(None)
        );
        assert_eq!(
            accepted(&reply(
                200,
                r#"{"version":1,"accepted":[{"id":"A","seq":3}]}"#
            ))
            .unwrap(),
            [("A".to_owned(), 3)]
        );
    }

    const REPLY: &str = r#"{"seq":3,"id":"01K8A1B2C3D4E5F6G7H8J9K0MN","entry":"01K8A0RZ0V3X5Q2N9T7W4Y6M8B",
        "thread":"01K8A0RZ0V3X5Q2N9T7W4Y6M8B","machine":"studio",
        "session":{"id":"01K89ZQ4D2F6H8J0K2M4N6P8R0","name":"fix-tests","folder":"rakumux"},
        "agent":"claude-code","conversation_id":"5f1c2a90-3b7e-4d1a-9c2e-6a8b0d4f1e23",
        "text":"Now run the tests \u001b[2J too.","status":"delivered","response":"01K8A1Z9Y8X7W6V5T4S3R2Q1P0",
        "created_at":1761220100000,"status_at":1761220103000}"#;

    #[test]
    fn a_reply_upload_is_the_contracts_and_leaves_out_what_a_prompt_cant_hold() {
        let Some(FeedRequest::ReplyUpload { body }) = reply_upload(
            "01K8A1B2C3D4E5F6G7H8J9K0MN",
            "01K8A0RZ0V3X5Q2N9T7W4Y6M8B",
            "  Now run the tests.\nThen open a PR.  ",
            "laptop",
        ) else {
            panic!("expected an upload");
        };
        let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(
            body,
            serde_json::json!({"version": 1, "id": "01K8A1B2C3D4E5F6G7H8J9K0MN",
                "entry": "01K8A0RZ0V3X5Q2N9T7W4Y6M8B",
                "text": "Now run the tests.\nThen open a PR.", "from": "laptop"})
        );
        let refused = |text: &str| {
            reply_upload(
                "01K8A1B2C3D4E5F6G7H8J9K0MN",
                "01K8A0RZ0V3X5Q2N9T7W4Y6M8B",
                text,
                "laptop",
            )
        };
        assert_eq!(refused("   \n "), None);
        assert_eq!(refused("\u{200d}\u{fe0f}"), None);
        // What a prompt can't hold is left out: controls, bidi controls, and
        // emoji's joiners and variation selectors, so ✔️ goes as ✔.
        let text = |text: &str| match refused(text) {
            Some(FeedRequest::ReplyUpload { body }) => {
                serde_json::from_slice::<serde_json::Value>(&body).unwrap()["text"].clone()
            }
            other => panic!("expected an upload, got {other:?}"),
        };
        assert_eq!(text("run \u{1b}[2Jthis"), "run [2Jthis");
        assert_eq!(text("\u{202e}txt.exe"), "txt.exe");
        assert_eq!(
            text("Done \u{2714}\u{fe0f} \u{1f469}\u{200d}\u{1f4bb}"),
            "Done \u{2714} \u{1f469}\u{1f4bb}"
        );
        assert_eq!(refused(&"a".repeat(MAX_PROMPT_BYTES + 1)), None);
        assert!(refused(&"a".repeat(MAX_PROMPT_BYTES)).is_some());
    }

    #[test]
    fn a_session_message_upload_names_the_session_and_its_machine() {
        let upload = |machine: &str, session: &str, text: &str| {
            session_message_upload(
                "01K8A1B2C3D4E5F6G7H8J9K0MN",
                machine,
                session,
                text,
                "laptop",
            )
        };
        let Some(FeedRequest::ReplyUpload { body }) =
            upload("studio", "01K8A0RZ0V3X5Q2N9T7W4Y6M8B", "  Now the docs.  ")
        else {
            panic!("expected an upload");
        };
        let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(
            body,
            serde_json::json!({"version": 1, "id": "01K8A1B2C3D4E5F6G7H8J9K0MN",
                "session": {"machine": "studio", "id": "01K8A0RZ0V3X5Q2N9T7W4Y6M8B"},
                "text": "Now the docs.", "from": "laptop"})
        );
        assert_eq!(upload("studio", "01K8A0RZ0V3X5Q2N9T7W4Y6M8B", "  \n"), None);
        assert_eq!(upload("studio", "session-1", "Now the docs."), None);
        assert_eq!(
            upload("", "01K8A0RZ0V3X5Q2N9T7W4Y6M8B", "Now the docs."),
            None
        );
        assert_eq!(
            upload(
                "studio",
                "01K8A0RZ0V3X5Q2N9T7W4Y6M8B",
                &"a".repeat(MAX_PROMPT_BYTES + 1)
            ),
            None
        );
    }

    #[test]
    fn a_reply_status_says_delivered_or_why_not() {
        let body = |request: Option<FeedRequest>| match request {
            Some(FeedRequest::ReplyStatus { body }) => {
                serde_json::from_slice::<serde_json::Value>(&body).unwrap()
            }
            other => panic!("expected a status, got {other:?}"),
        };
        assert_eq!(
            body(reply_status("01K8A1B2C3D4E5F6G7H8J9K0MN", "studio", None)),
            serde_json::json!({"version": 1, "id": "01K8A1B2C3D4E5F6G7H8J9K0MN",
                "machine": "studio", "status": "delivered"})
        );
        assert_eq!(
            body(reply_status(
                "01K8A1B2C3D4E5F6G7H8J9K0MN",
                "studio",
                Some(ReplyFailure::ConversationChanged)
            ))["failure"],
            "conversation_changed"
        );
    }

    #[test]
    fn replies_read_back_cleaned_and_those_off_contract_are_left_out() {
        let answer = format!(r#"{{"version":1,"reply":{REPLY}}}"#);
        let Received::Reply(stored) = super::reply(&reply(200, &answer)).unwrap() else {
            panic!("a reply to an entry");
        };
        assert_eq!(stored.seq, 3);
        assert_eq!(stored.status, ReplyStatus::Delivered);
        assert_eq!(stored.text, "Now run the tests [2J too.");
        assert_eq!(stored.folder.as_deref(), Some("rakumux"));
        assert_eq!(
            stored.response.as_deref(),
            Some("01K8A1Z9Y8X7W6V5T4S3R2Q1P0")
        );

        let broken = REPLY.replace(r#""seq":3"#, r#""seq":0"#);
        let unknown = REPLY
            .replace(r#""delivered""#, r#""bounced""#)
            .replace(r#""seq":3"#, r#""seq":5"#);
        let page = format!(
            r#"{{"version":1,"replies":[{REPLY},{broken},{unknown}],"latest_seq":9,"more":true}}"#
        );
        let read = replies(&reply(200, &page)).unwrap();
        assert_eq!(read.latest_seq, 9);
        assert!(read.more);
        assert_eq!(
            read.replies
                .iter()
                .map(|reply| (reply.seq, reply.status))
                .collect::<Vec<_>>(),
            [(3, ReplyStatus::Delivered), (5, ReplyStatus::Unknown)]
        );
        assert_eq!(
            replies(&reply(
                403,
                r#"{"error":{"code":"forbidden","message":"x"}}"#
            ))
            .unwrap_err(),
            FeedFailure::Off
        );
        assert_eq!(
            super::reply(&reply(200, r#"{"version":2,"reply":{}}"#)).unwrap_err(),
            FeedFailure::Invalid
        );
    }

    #[test]
    fn pages_bring_entries_prompts_and_the_open_turns() {
        let body = r#"{"version":1,"latest_seq":3,"more":false,"entries":[
            {"seq":3,"id":"01K8A0RZ0V3X5Q2N9T7W4Y6M8B","machine":"studio",
             "session":{"id":"01K89ZQ4D2F6H8J0K2M4N6P8R0","name":"fix-tests","branch":"main"},
             "agent":"claude-code","conversation_id":"c1","text":"Fixed.","truncated":false,
             "finished_at":1761220000000,"received_at":1761220000400,
             "prompt":{"id":"m1","text":"Fix it\u202e","truncated":false,"at":1761219990000}}],
            "open":[
             {"machine":"studio","session":{"id":"01K89ZQ4D2F6H8J0K2M4N6P8R0","name":"fix-tests"},
              "agent":"claude-code","conversation_id":"c1",
              "prompt":{"id":"m2","text":"Now the docs","truncated":false,"at":1761220060000}},
             {"machine":"studio","session":{"id":"01K89ZQ4D2F6H8J0K2M4N6P8R0","name":"fix-tests"},
              "agent":"claude-code","conversation_id":"c1",
              "prompt":{"id":"bad id","text":"x","truncated":false}}]}"#;
        let read = page(&reply(200, body)).unwrap();
        let prompt = read.entries[0].prompt.as_ref().unwrap();
        assert_eq!((prompt.id.as_str(), prompt.text.as_str()), ("m1", "Fix it"));
        assert_eq!(read.entries[0].branch.as_deref(), Some("main"));
        let open = read.open.unwrap();
        assert_eq!(open.len(), 1, "an open turn off the contract is left out");
        assert_eq!(open[0].prompt.id, "m2");
        // An older rakumux.com says nothing about open turns.
        let older = page(&reply(
            200,
            r#"{"version":1,"entries":[],"latest_seq":0,"more":false}"#,
        ))
        .unwrap();
        assert_eq!(older.open, None);
    }

    #[test]
    fn session_messages_are_read_apart_and_half_replies_left_out() {
        let message = REPLY
            .replace(r#""entry":"01K8A0RZ0V3X5Q2N9T7W4Y6M8B","#, "")
            .replace(r#""thread":"01K8A0RZ0V3X5Q2N9T7W4Y6M8B","#, "")
            .replace(r#","response":"01K8A1Z9Y8X7W6V5T4S3R2Q1P0""#, "")
            .replace(r#""seq":3"#, r#""seq":4"#);
        assert!(
            !message.contains("entry") && !message.contains("thread"),
            "{message}"
        );
        let half = REPLY
            .replace(r#""thread":"01K8A0RZ0V3X5Q2N9T7W4Y6M8B","#, "")
            .replace(r#""seq":3"#, r#""seq":5"#);
        let page = format!(
            r#"{{"version":1,"replies":[{REPLY},{message},{half}],"latest_seq":5,"more":false}}"#
        );
        let read = replies(&reply(200, &page)).unwrap();
        assert_eq!(read.replies.iter().map(|r| r.seq).collect::<Vec<_>>(), [3]);
        assert_eq!(read.messages.len(), 1);
        let message = &read.messages[0];
        assert_eq!(message.seq, 4);
        assert_eq!(message.session_id, "01K89ZQ4D2F6H8J0K2M4N6P8R0");
        assert_eq!(message.text, "Now run the tests [2J too.");
        assert_eq!(read.latest_seq, 5);
    }

    #[test]
    fn answers_to_questions_are_read_whole_or_left_out() {
        let message = REPLY
            .replace(r#""entry":"01K8A0RZ0V3X5Q2N9T7W4Y6M8B","#, "")
            .replace(r#""thread":"01K8A0RZ0V3X5Q2N9T7W4Y6M8B","#, "")
            .replace(r#","response":"01K8A1Z9Y8X7W6V5T4S3R2Q1P0""#, "");
        let with = |seq: u64, answer: &str| {
            message
                .replace(r#""seq":3"#, &format!(r#""seq":{seq}"#))
                .replace(r#""status":"#, &format!(r#""answer":{answer},"status":"#))
        };
        let good = with(
            4,
            r#"{"call_id":"toolu_01A","answers":[
                {"question":"Which colour?\nPick one.","choices":["Red"]},
                {"question":"Fruits?","choices":["Apple","Cherry"],"text":"Mango"}]}"#,
        );
        let rows = [
            good,
            // No answers, a question answered with nothing, a free answer on
            // two lines, a choice with a terminal control, a member off the
            // contract: each row left out.
            with(5, r#"{"call_id":"toolu_01A","answers":[]}"#),
            with(
                6,
                r#"{"call_id":"toolu_01A","answers":[{"question":"Q?"}]}"#,
            ),
            with(
                7,
                r#"{"call_id":"toolu_01A","answers":[{"question":"Q?","text":"a\nb"}]}"#,
            ),
            with(
                8,
                r#"{"call_id":"toolu_01A","answers":[{"question":"Q?","choices":["\u001b[2J"]}]}"#,
            ),
            with(
                9,
                r#"{"call_id":"toolu_01A","answers":[{"question":"Q?","choices":["A"]}],"x":1}"#,
            ),
            with(
                10,
                r#"{"call_id":"no spaces","answers":[{"question":"Q?","choices":["A"]}]}"#,
            ),
        ];
        let page = format!(
            r#"{{"version":1,"replies":[{}],"latest_seq":10,"more":false}}"#,
            rows.join(",")
        );
        let read = replies(&reply(200, &page)).unwrap();
        assert_eq!(read.messages.len(), 1, "{:?}", read.messages);
        let answer = read.messages[0].answer.as_ref().unwrap();
        assert_eq!(answer.call_id, "toolu_01A");
        assert_eq!(
            answer.answers,
            [
                session_core::api::QuestionAnswer {
                    question: "Which colour?\nPick one.".into(),
                    choices: vec!["Red".into()],
                    text: None,
                },
                session_core::api::QuestionAnswer {
                    question: "Fruits?".into(),
                    choices: vec!["Apple".into(), "Cherry".into()],
                    text: Some("Mango".into()),
                },
            ]
        );
        // Every row read, those left out too.
        assert_eq!(read.read_to(), 10);
        // An answer to an entry, or without the chat it answers in: left out.
        let to_entry = REPLY.replace(
            r#""status":"#,
            r#""answer":{"call_id":"t1","answers":[{"question":"Q?","choices":["A"]}]},"status":"#,
        );
        let chatless = with(
            11,
            r#"{"call_id":"t1","answers":[{"question":"Q?","choices":["A"]}]}"#,
        )
        .replace(
            r#""conversation_id":"5f1c2a90-3b7e-4d1a-9c2e-6a8b0d4f1e23","#,
            "",
        );
        assert!(!chatless.contains("conversation_id"), "{chatless}");
        let page = format!(
            r#"{{"version":1,"replies":[{to_entry},{chatless}],"latest_seq":11,"more":false}}"#
        );
        let read = replies(&reply(200, &page)).unwrap();
        assert!(read.replies.is_empty() && read.messages.is_empty());
    }

    #[test]
    fn reads_go_past_rows_left_out_but_not_past_pages_still_to_read() {
        // Only rows off the contract: none kept, but the page was read.
        let off = REPLY
            .replace(r#""seq":3"#, r#""seq":42"#)
            .replace("01K8A1B2C3D4E5F6G7H8J9K0MN", "not an id");
        let body = |more: bool| {
            format!(r#"{{"version":1,"replies":[{off}],"latest_seq":300,"more":{more}}}"#)
        };
        let read = replies(&reply(200, &body(true))).unwrap();
        assert!(read.replies.is_empty() && read.messages.is_empty());
        assert_eq!((read.page_seq, read.latest_seq), (42, 300));
        // More wait: after this page; the account's newest is on a later one.
        assert_eq!(read.read_to(), 42);
        assert_eq!(replies(&reply(200, &body(false))).unwrap().read_to(), 300);

        // The feed's pages the same.
        let body = |more: bool| {
            format!(
                r#"{{"version":1,"latest_seq":300,"more":{more},"entries":[
                {{"seq":8,"id":"{long}","machine":"studio",
                 "session":{{"id":"01K89ZQ4D2F6H8J0K2M4N6P8R0","name":"fix-tests"}},
                 "agent":"claude-code","text":"Fixed.","truncated":false,
                 "finished_at":1761220000000}}]}}"#,
                long = "x".repeat(65)
            )
        };
        let read = page(&reply(200, &body(true))).unwrap();
        assert!(read.entries.is_empty());
        assert_eq!((read.page_seq, read.read_to()), (8, 8));
        assert_eq!(page(&reply(200, &body(false))).unwrap().read_to(), 300);
    }

    const FILE: &str = r#"{"id":"01K8A1C0FFEE0000000000000A","name":"screen shot.png",
        "media_type":"image/png","size":4194304}"#;

    /// `REPLY` with `attachments` (`None`: none), as seq `seq`.
    fn attached(seq: u64, attachments: Option<&str>) -> String {
        let with = attachments.map_or(String::new(), |files| format!(r#","attachments":{files}"#));
        REPLY
            .replace(r#""seq":3"#, &format!(r#""seq":{seq}"#))
            .replace(
                r#""status":"delivered""#,
                &format!(r#""status":"pending"{with}"#),
            )
    }

    #[test]
    fn replies_bring_their_files_and_rows_with_files_off_the_contract_are_left_out() {
        let two = format!(
            r#"[{FILE},{}]"#,
            FILE.replace("0A", "0B")
                .replace("screen shot.png", "notes.txt")
        );
        let message = attached(4, Some(&format!("[{FILE}]")))
            .replace(r#""entry":"01K8A0RZ0V3X5Q2N9T7W4Y6M8B","#, "")
            .replace(r#""thread":"01K8A0RZ0V3X5Q2N9T7W4Y6M8B","#, "");
        let six = format!("[{}]", vec![FILE; 6].join(","));
        let malformed = [
            r#""files""#.to_owned(),
            r#"[{"id":"01K8A1C0FFEE0000000000000A","name":"a.txt","media_type":"text/plain"}]"#
                .into(),
            six,
            FILE.replace("screen shot.png", "../shot.png"),
            FILE.replace("screen shot.png", "shot\\u0007.png"),
            FILE.replace("screen shot.png", ".."),
            FILE.replace("4194304", "4194305"),
            FILE.replace("4194304", "0"),
            FILE.replace("4194304", "-1"),
            FILE.replace("01K8A1C0FFEE0000000000000A", "file-1"),
            FILE.replace("image/png", "Image/PNG"),
            FILE.replace("image/png", "image/png; charset=x"),
        ];
        let mut rows = vec![attached(2, None), attached(3, Some(&two)), message];
        for (n, files) in malformed.iter().enumerate() {
            let files = if files.starts_with('[') || files.starts_with('"') {
                files.clone()
            } else {
                format!("[{files}]")
            };
            rows.push(attached(10 + n as u64, Some(&files)));
        }
        let page = format!(
            r#"{{"version":1,"replies":[{}],"latest_seq":30,"more":false}}"#,
            rows.join(",")
        );
        let read = replies(&reply(200, &page)).unwrap();
        assert_eq!(read.page_seq, 21, "rows left out are read past");
        assert_eq!(
            read.replies.iter().map(|r| r.seq).collect::<Vec<_>>(),
            [2, 3]
        );
        assert!(read.replies[0].attachments.is_empty(), "absent: none");
        let files = &read.replies[1].attachments;
        assert_eq!(files.len(), 2);
        assert_eq!(
            files[0],
            ReplyAttachment {
                id: "01K8A1C0FFEE0000000000000A".into(),
                name: "screen shot.png".into(),
                media_type: "image/png".into(),
                size: 4_194_304,
            }
        );
        assert_eq!(files[1].name, "notes.txt");
        assert_eq!(read.messages.len(), 1);
        assert_eq!(read.messages[0].attachments.len(), 1);
    }

    #[test]
    fn a_download_is_its_files_bytes_exactly_or_unavailable() {
        let bytes = |status: u16, body: Vec<u8>| FeedHttpReply {
            status,
            retry_after: None,
            body,
        };
        let whole = vec![7u8; 4_194_304];
        assert_eq!(
            download(bytes(200, whole.clone()), 4_194_304),
            Ok(Download::File(whole))
        );
        assert_eq!(
            download(bytes(200, vec![7; 9]), 10),
            Ok(Download::Unavailable),
            "short"
        );
        assert_eq!(
            download(bytes(200, vec![7; 11]), 10),
            Ok(Download::Unavailable),
            "long"
        );
        let gone = br#"{"error":{"code":"invalid_request","message":"x"}}"#.to_vec();
        assert_eq!(
            download(bytes(404, gone.clone()), 10),
            Ok(Download::Unavailable)
        );
        assert_eq!(download(bytes(400, gone), 10), Err(FeedFailure::Invalid));
        assert_eq!(
            download(
                bytes(
                    503,
                    br#"{"error":{"code":"unavailable","message":"x"}}"#.to_vec()
                ),
                10
            ),
            Err(FeedFailure::Unavailable(None))
        );
        assert_eq!(
            download(bytes(502, b"<html>".to_vec()), 10),
            Err(FeedFailure::Unavailable(None))
        );
        assert_eq!(
            download(
                bytes(
                    429,
                    br#"{"error":{"code":"rate_limited","message":"x"}}"#.to_vec()
                ),
                10
            ),
            Err(FeedFailure::RateLimited(None))
        );
        assert_eq!(
            download(
                bytes(
                    401,
                    br#"{"error":{"code":"unauthenticated","message":"x"}}"#.to_vec()
                ),
                10
            ),
            Err(FeedFailure::Unauthorized)
        );
    }

    #[test]
    fn the_session_list_says_what_else_the_machine_handles() {
        let body = |capabilities: &[&str]| match sessions_upload("studio", &[], 1, capabilities) {
            Some(FeedRequest::Sessions { body }) => {
                serde_json::from_slice::<serde_json::Value>(&body).unwrap()
            }
            other => panic!("expected a session list, got {other:?}"),
        };
        assert_eq!(
            body(&[REPLY_ATTACHMENTS])["capabilities"],
            serde_json::json!(["reply_attachments"])
        );
        assert!(body(&[]).get("capabilities").is_none());
    }

    #[test]
    fn replies_name_the_machine_and_say_when_to_read_again_within_bounds() {
        assert_eq!(
            FeedRequest::replies_query(7, "Studio Mac é", false),
            "after=7&limit=100&machine=Studio%20Mac%20%C3%A9&session_messages=1"
        );
        // Answers to agents' questions only while the daemon gives them.
        assert_eq!(
            FeedRequest::replies_query(7, "studio", true),
            "after=7&limit=100&machine=studio&session_messages=1&question_answers=1"
        );
        let paced = |poll: &str| {
            replies(&reply(
                200,
                &format!(r#"{{"version":1,"replies":[],"latest_seq":0,"more":false{poll}}}"#),
            ))
            .unwrap()
            .poll
        };
        assert_eq!(paced(r#","poll_ms":1000"#), Some(Duration::from_secs(1)));
        assert_eq!(paced(r#","poll_ms":15000"#), Some(Duration::from_secs(15)));
        assert_eq!(paced(r#","poll_ms":10"#), Some(Duration::from_secs(1)));
        assert_eq!(
            paced(r#","poll_ms":3600000"#),
            Some(Duration::from_secs(60))
        );
        assert_eq!(paced(""), None);
    }

    #[test]
    fn a_realtime_answer_is_a_tls_websocket_and_a_header_safe_token() {
        let token = "v1.eyJhY2N0IjoieCJ9.c2lnbmF0dXJl";
        let answer = |url: &str, token: &str| {
            format!(r#"{{"version":1,"url":"{url}","token":"{token}","expires_at":1761220300000}}"#)
        };
        let got = realtime(&reply(
            200,
            &answer("wss://realtime.rakumux.com/v1/connect", token),
        ))
        .unwrap();
        assert_eq!(got.url, "wss://realtime.rakumux.com/v1/connect");
        assert_eq!(got.token, token);
        assert!(!format!("{got:?}").contains(token), "never logged");
        // A worker on this machine, for development.
        assert!(realtime(&reply(
            200,
            &answer("ws://127.0.0.1:8787/v1/connect", token)
        ))
        .is_ok());
        for (url, token) in [
            ("ws://realtime.rakumux.com/v1/connect", token),
            ("wss://realtime.rakumux.com/v1/connect?token=x", token),
            ("wss://user@realtime.rakumux.com/v1/connect", token),
            ("https://realtime.rakumux.com/v1/connect", token),
            ("wss://realtime.rakumux.com/v1/connect", "short"),
            (
                "wss://realtime.rakumux.com/v1/connect",
                "v1.has a space.in-it-somewhere",
            ),
        ] {
            assert_eq!(
                realtime(&reply(200, &answer(url, token))),
                Err(FeedFailure::Invalid),
                "{url} {token}"
            );
        }
        assert_eq!(
            realtime(&reply(
                200,
                r#"{"version":1,"url":"wss://a.example/v1/connect","token":"v1.aaaaaaaaaaaaaaaa.b","expires_at":1,"user":"u"}"#
            )),
            Err(FeedFailure::Invalid),
            "more than the contract says"
        );
        // A cloud without pokes.
        let off = r#"{"error":{"code":"invalid_request","message":"Invalid feed request."}}"#;
        assert_eq!(realtime(&reply(404, off)), Err(FeedFailure::Invalid));
    }

    #[test]
    fn rakumux_com_says_whether_it_still_wants_resources() {
        assert_eq!(
            resources_stored(&reply(200, r#"{"version":1,"wanted":true}"#)),
            Ok(true)
        );
        assert_eq!(
            resources_stored(&reply(200, r#"{"version":1,"wanted":false}"#)),
            Ok(false)
        );
        for broken in [
            r#"{"version":2,"wanted":true}"#,
            r#"{"version":1}"#,
            r#"{"version":1,"wanted":"yes"}"#,
            r#"{"version":1,"wanted":true,"stored":1}"#,
        ] {
            assert_eq!(
                resources_stored(&reply(200, broken)),
                Err(FeedFailure::Invalid),
                "{broken}"
            );
        }
        let off = r#"{"error":{"code":"forbidden","message":"The feed is off."}}"#;
        assert_eq!(resources_stored(&reply(403, off)), Err(FeedFailure::Off));
    }

    #[test]
    fn a_replies_read_says_when_rtop_is_open_on_rakumux_com() {
        let read = |extra: &str| {
            replies(&reply(
                200,
                &format!(r#"{{"version":1,"replies":[],"latest_seq":0,"more":false{extra}}}"#),
            ))
            .unwrap()
            .resources_wanted
        };
        assert!(!read(""));
        assert!(!read(r#","poll_ms":1000"#));
        assert!(read(r#","poll_ms":1000,"resources_wanted":true"#));
    }

    #[test]
    fn a_resources_upload_bigger_than_the_contract_takes_isnt_made() {
        use session_core::api::{MachineUsage, ProcessUsage, ResourceSample, SessionUsage};
        let process = |pid: u32| ProcessUsage {
            pid,
            parent: None,
            name: "node".into(),
            command: String::new(),
            cpu_ms: 0,
            rss_bytes: u64::MAX,
            net_rx_bytes: Some(u64::MAX),
            net_tx_bytes: Some(u64::MAX),
        };
        let sample = |at_ms: i64| ResourceSample {
            at_ms,
            machine: MachineUsage {
                cpus: 4,
                ..Default::default()
            },
            daemon: process(1),
            clients: Vec::new(),
            sessions: (0..250)
                .map(|index| SessionUsage {
                    id: session_core::ids::SessionId::from_string(format!(
                        "01K89ZQ4D2F6H8J0K2M4N{index:05}"
                    )),
                    processes: vec![process(1000 + index)],
                })
                .collect(),
        };
        // 200 sessions at the largest numbers still fit.
        let mut resources = super::super::resources::ResourceSync::new();
        let now = Duration::from_secs(10);
        resources.set_enabled(true, now);
        resources.wanted(now);
        resources.next_work(now);
        resources.read_done(Ok(sample(1_761_800_000_000)), "studio", &|_| true, now);
        let later = now + Duration::from_secs(1);
        resources.next_work(later);
        resources.read_done(Ok(sample(1_761_800_001_000)), "studio", &|_| true, later);
        let Some(super::super::resources::ResourceWork::Upload(FeedRequest::Resources { body })) =
            resources.next_work(later)
        else {
            panic!("expected an upload");
        };
        assert!(body.len() <= MAX_RESOURCES_BODY_BYTES, "{}", body.len());
        let upload: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(upload["sessions"].as_array().unwrap().len(), 200);
        assert_eq!(
            upload["sessions"][0]["usage"]["memory_bytes"],
            9_007_199_254_740_991_u64
        );
    }
}
