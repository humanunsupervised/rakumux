//! Replies written back to feed entries (`contracts/feed/` `reply`): while the
//! account's Feed switch is on, the status service reads the account's
//! replies as they change (every few seconds), keeps them for TUIs' threads,
//! and types the pending ones addressed to this machine into their sessions
//! (`session_submit`: a paste, then Enter, the same for every agent), then
//! says so. It delivers only to the entry's own session, while it runs, isn't
//! private, still shows the entry's conversation and its agent waits for
//! input; a busy or blocked agent is waited for (the cloud expires a reply
//! after an hour). A session's replies go in one at a time, oldest first, and
//! each is reported delivered only once its agent is seen to take it (see
//! `Delivery`); the next waits for the agent to wait again. A session message
//! (written on rakumux.com to a session with no answer to reply to) goes in
//! the same queue: it is typed once an agent runs there and waits,
//! whatever chat it shows; never into a session without an agent, which is
//! waited for. Session messages never reach TUIs. A reply written on rakumux.com or another machine is
//! typed only when this machine's user turned that on in Settings; one
//! written in this machine's TUI always is. The agent's next reply in that
//! session is uploaded as the reply's answer (`reply_to`).
//!
//! A reply with files attached (`attachments`) has them fetched once it is
//! otherwise ready to be typed in, one at a time and each only once, saved
//! under `uploads/<reply id>/` (see `uploads`); then its text is typed with a
//! blank line and each file's absolute path on a line of its own. A file that
//! rakumux.com no longer has or that isn't what the reply said fails the reply
//! (`attachment_unavailable`) without typing anything; a request that fails
//! for now is tried again, and the text never goes in without its files. The
//! files of a reply that won't be typed in are removed. Driven by the owner
//! thread with its own clock.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use session_core::api::{AgentReply, ErrorCode};
use session_core::ids::SessionId;
use session_core::model::{Lifecycle, SessionStatus, SessionView};

use super::cache::{FeedReply, ReplyAttachment, ReplyFailure, ReplyStatus};
use super::job::FeedResult;
use super::wire::{self, FeedFailure, FeedRequest, RepliesRead, SessionMessage};

/// How often the account's replies are read while the feed is on, unless
/// rakumux.com's answer says otherwise (`poll_ms`: every second while a reply
/// waits for this machine or the website is open, and for 30 s after the
/// account's last reply activity, then 2, 4 and 8 s as the quiet time
/// doubles, up to 15 s after 4 minutes).
const READ_EVERY: Duration = Duration::from_secs(5);
/// rakumux.com asks this often while someone has it open, a reply waits here,
/// or for 30 s after the account's last reply activity.
const WATCHED_POLL: Duration = Duration::from_secs(1);
/// The first and longest waits after a failed request.
const RETRY_MIN: Duration = Duration::from_secs(5);
const RETRY_MAX: Duration = Duration::from_secs(300);
/// How long the "replies from elsewhere" setting is trusted before it's read again.
const SETTING_EVERY: Duration = Duration::from_secs(10);
/// Replies remembered as handled here, so a stale pending one isn't typed twice.
const MAX_HANDLED: usize = 1_000;
/// Nothing is typed into a session someone typed in this recently, so a reply
/// isn't added to what they're writing (`session_submit`'s `quiet_ms`).
pub(crate) const TYPING_QUIET: Duration = Duration::from_secs(10);
/// After the daemon refused because someone was typing, try that session again this much later.
pub(crate) const TYPING_HOLD: Duration = Duration::from_secs(2);
/// A reply typed in is delivered once its agent is seen to take it (working,
/// asking something, or a new prompt in its chat); this long after it was
/// typed it counts as delivered all the same, so its session's next replies
/// never wait on it for ever.
pub(crate) const TAKE_WITHIN: Duration = Duration::from_secs(30);
/// A reply taken without its agent being seen busy (its lifecycle can lag a
/// new prompt) holds its session's next one this long before the agent's
/// waiting is believed.
pub(crate) const SETTLE: Duration = Duration::from_secs(5);

/// Whether this machine takes replies written on rakumux.com or other machines.
pub type AcceptRemote = Arc<dyn Fn() -> bool + Send + Sync>;

/// What became of a reply a TUI wrote.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReplyWriteStatus {
    /// Stored on rakumux.com, for its machine to type into the session.
    Sent,
    SignedOut,
    /// The account's Feed switch is off.
    Off,
    /// The text says nothing, is too long, or the entry can't be replied to.
    Invalid,
    /// rakumux.com can't be reached, or asked for fewer requests.
    Unavailable,
    #[serde(other)]
    Unknown,
}

/// The answer to a TUI's reply: the reply as stored, once it is.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplyWrite {
    pub status: ReplyWriteStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply: Option<FeedReply>,
}

impl ReplyWrite {
    pub(crate) fn of(outcome: Result<FeedReply, FeedFailure>) -> Self {
        Self {
            status: write_status(&outcome),
            reply: outcome.ok(),
        }
    }

    /// The answer to a TUI's session message: never a reply, as TUIs don't
    /// see session messages.
    pub(crate) fn of_message(outcome: Result<(), FeedFailure>) -> Self {
        Self {
            status: write_status(&outcome),
            reply: None,
        }
    }
}

fn write_status<T>(outcome: &Result<T, FeedFailure>) -> ReplyWriteStatus {
    match outcome {
        Ok(_) => ReplyWriteStatus::Sent,
        Err(FeedFailure::Unauthorized) => ReplyWriteStatus::SignedOut,
        Err(FeedFailure::Off) => ReplyWriteStatus::Off,
        Err(FeedFailure::Invalid) => ReplyWriteStatus::Invalid,
        Err(FeedFailure::RateLimited(_) | FeedFailure::Unavailable(_)) => {
            ReplyWriteStatus::Unavailable
        }
    }
}

/// What to do next for replies.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ReplyWork {
    /// Ask rakumux.com (a read, or a status report).
    Request(FeedRequest),
    /// Type `text` into `session`.
    Submit {
        reply: String,
        session: SessionId,
        text: String,
    },
    /// Fetch one of a reply's files from rakumux.com and save it.
    Fetch(Fetch),
    /// Remove a reply's saved files: it won't be typed in.
    Discard(String),
}

/// One file of a reply to fetch and save beside those saved already.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Fetch {
    pub reply: String,
    pub attachment: ReplyAttachment,
    /// The reply's first file: its directory is made afresh.
    pub first: bool,
    /// The names of its files saved already, which this one's must differ from.
    pub taken: Vec<String>,
}

impl Fetch {
    /// The download it takes.
    pub fn request(&self) -> FeedRequest {
        FeedRequest::Attachment {
            id: self.attachment.id.clone(),
            size: self.attachment.size,
        }
    }
}

/// What became of a [`Fetch`].
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Fetched {
    /// Saved here, at this path.
    Saved(PathBuf),
    /// rakumux.com doesn't have it (any more), or it wasn't what the reply said.
    Unavailable,
    /// It couldn't be saved here.
    Unsaved,
    /// The request failed: tried again later, unless the answer broke the contract.
    Failed(FeedFailure),
}

/// The request out to rakumux.com.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Out {
    Read,
    Report,
}

/// When one kind of request (reads, or status reports) may go again after
/// failures: once the wait rakumux.com asked for is over, else one doubling
/// from [`RETRY_MIN`] to [`RETRY_MAX`]. Reads and reports keep their own, as
/// rakumux.com limits them apart: a rate-limited read doesn't hold back the
/// report of a reply typed in meanwhile, before the reply expires.
#[derive(Clone, Copy, Debug)]
struct Backoff {
    /// Not before this.
    at: Duration,
    /// The wait after the next failure that names none.
    wait: Duration,
}

impl Backoff {
    fn new(now: Duration) -> Self {
        Self {
            at: now,
            wait: RETRY_MIN,
        }
    }

    fn ready(&self, now: Duration) -> bool {
        now >= self.at
    }

    fn succeeded(&mut self) {
        self.wait = RETRY_MIN;
    }

    fn failed(&mut self, failure: FeedFailure, now: Duration) {
        let wait = match failure {
            FeedFailure::RateLimited(Some(delay)) | FeedFailure::Unavailable(Some(delay)) => delay,
            _ => self.wait,
        };
        self.wait = self.wait.saturating_mul(2).min(RETRY_MAX);
        self.at = now + wait.min(Duration::from_secs(86_400));
    }
}

/// What a session shows, for deciding whether a reply can be typed into it.
struct Seen {
    live: bool,
    private: bool,
    lifecycle: Lifecycle,
    conversation: Option<String>,
    /// The agent's current turn (`agent_turn.id`): a new prompt makes a new one.
    turn: Option<u64>,
}

impl Seen {
    fn of(view: &SessionView) -> Self {
        Self {
            live: view.status == SessionStatus::Live,
            private: view.private,
            lifecycle: view.lifecycle,
            conversation: view
                .agent_turn
                .as_ref()
                .and_then(|turn| turn.conversation_id.clone()),
            turn: view.agent_turn.as_ref().map(|turn| turn.id),
        }
    }

    /// Its agent waits for input.
    fn waits(&self) -> bool {
        matches!(self.lifecycle, Lifecycle::Idle | Lifecycle::Done)
    }

    /// Its agent works, or asks something.
    fn busy(&self) -> bool {
        matches!(self.lifecycle, Lifecycle::Working | Lifecycle::Blocked)
    }
}

/// Where a pending reply goes.
#[derive(Clone, Debug)]
enum Target {
    /// A reply to an entry: into the session while it shows that chat.
    Chat(String),
    /// A session message: into the session's agent, whatever chat it shows.
    Session,
}

/// A reply (or session message) pending for this machine.
#[derive(Clone, Debug)]
struct Pending {
    /// rakumux.com's order: a session's replies go in in it.
    seq: u64,
    session_id: String,
    target: Target,
    text: String,
    attachments: Vec<ReplyAttachment>,
    /// The machine it was written on; `None` on rakumux.com.
    from: Option<String>,
}

impl Pending {
    fn reply(reply: &FeedReply) -> Self {
        Self {
            seq: reply.seq,
            session_id: reply.session_id.clone(),
            target: Target::Chat(reply.conversation_id.clone()),
            text: reply.text.clone(),
            attachments: reply.attachments.clone(),
            from: reply.from.clone(),
        }
    }

    fn message(message: &SessionMessage) -> Self {
        Self {
            seq: message.seq,
            session_id: message.session_id.clone(),
            target: Target::Session,
            text: message.text.clone(),
            attachments: message.attachments.clone(),
            from: message.from.clone(),
        }
    }
}

/// A reply going into its session, and what its agent showed since: nothing
/// else is typed there until the agent is seen to take it (it works or asks
/// something, or its chat shows a new prompt) and then waits again. Seen
/// taken, it's reported delivered; its session ended or its chat changed
/// first, failed. The agent's lifecycle can lag what it does, so a view
/// that still shows it waiting proves nothing.
struct Delivery {
    reply: String,
    target: Target,
    /// When it was handed to the daemon.
    since: Duration,
    /// The daemon typed it in.
    typed: bool,
    /// The agent's turn and chat just before: another turn after is a new prompt.
    turn: (Option<u64>, Option<String>),
    /// The agent was seen working or asking something since.
    busy: bool,
    /// When it was seen taken (and reported delivered).
    taken: Option<Duration>,
}

/// What a typed-in reply's agent has shown of it.
enum Taking {
    Waiting,
    Taken,
    Failed(ReplyFailure),
}

impl Delivery {
    /// Typed in and not yet taken: what its session's latest view says.
    fn taking(&self, seen: Option<&Seen>, now: Duration) -> Taking {
        let Some(seen) = seen.filter(|seen| seen.live) else {
            return Taking::Failed(ReplyFailure::SessionGone);
        };
        if self.busy {
            return Taking::Taken;
        }
        if let Target::Chat(wanted) = &self.target {
            let changed = seen.lifecycle == Lifecycle::None
                || seen
                    .conversation
                    .as_ref()
                    .is_some_and(|shown| shown != wanted);
            if changed {
                return Taking::Failed(ReplyFailure::ConversationChanged);
            }
        }
        let new_prompt =
            seen.turn.is_some() && (seen.turn, &seen.conversation) != (self.turn.0, &self.turn.1);
        if new_prompt || now.saturating_sub(self.since) >= TAKE_WITHIN {
            Taking::Taken
        } else {
            Taking::Waiting
        }
    }

    /// Taken: whether its session takes the next reply now.
    fn done(&self, seen: Option<&Seen>, now: Duration) -> bool {
        let Some(seen) = seen.filter(|seen| seen.live) else {
            return true;
        };
        let settled = self
            .taken
            .is_some_and(|taken| now.saturating_sub(taken) >= SETTLE);
        seen.waits() && (self.busy || settled)
    }
}

/// A reply's text as typed in: with files, a blank line, then each file's
/// absolute path on a line of its own (agents read files, images too, from
/// the paths in a prompt).
fn typed(text: &str, files: &[PathBuf]) -> String {
    if files.is_empty() {
        return text.to_owned();
    }
    let paths: Vec<String> = files
        .iter()
        .map(|path| path.display().to_string())
        .collect();
    format!("{}\n\n{}", text.trim_end(), paths.join("\n"))
}

/// A delivered reply waiting for the agent's answer, in `conversation` (any
/// of the session's chats when it's `None`).
struct Awaiting {
    reply: String,
    conversation: Option<String>,
    since_ms: i64,
}

/// Replies' state on the owner thread.
pub(crate) struct ReplySync {
    machine: String,
    enabled: bool,
    accept_remote: Option<AcceptRemote>,
    remote: bool,
    remote_read: Option<Duration>,
    sessions: HashMap<SessionId, Seen>,
    /// Where reads continue: the greatest reply seq read (the account's
    /// once a read said no more wait).
    cursor: u64,
    /// The read or report out, if one is.
    out: Option<Out>,
    next_read: Duration,
    /// How often rakumux.com asked to be read, at the last read.
    poll: Option<Duration>,
    /// Reads' and reports' waits after failures, apart.
    read_retry: Backoff,
    report_retry: Backoff,
    /// This machine's pending replies and session messages, by id, as last read.
    pending: HashMap<String, Pending>,
    /// Typed (or refused) here, to tell rakumux.com: the id and why it failed.
    reports: VecDeque<(String, Option<ReplyFailure>)>,
    /// The reply going into each session, until its agent took it and waits again.
    deliveries: HashMap<SessionId, Delivery>,
    handled: HashSet<String>,
    handled_order: VecDeque<String>,
    awaiting: HashMap<SessionId, Awaiting>,
    /// Sessions someone was typing in, by session id: nothing is typed there until then.
    held: HashMap<String, Duration>,
    /// Each pending reply's files saved so far, in its attachments' order.
    files: HashMap<String, Vec<PathBuf>>,
    /// The reply one of whose files is being fetched (one file at a time).
    fetching: Option<String>,
    /// Downloads' wait after failures, apart from reads' and reports'.
    fetch_retry: Backoff,
    /// Replies whose saved files are to be removed.
    discards: VecDeque<String>,
}

impl ReplySync {
    /// Replies for `machine`; `accept_remote` says whether replies written
    /// elsewhere are typed here (never without it).
    pub fn new(machine: String, accept_remote: Option<AcceptRemote>) -> Self {
        Self {
            machine,
            enabled: false,
            accept_remote,
            remote: false,
            remote_read: None,
            sessions: HashMap::new(),
            cursor: 0,
            out: None,
            next_read: Duration::ZERO,
            poll: None,
            read_retry: Backoff::new(Duration::ZERO),
            report_retry: Backoff::new(Duration::ZERO),
            pending: HashMap::new(),
            reports: VecDeque::new(),
            deliveries: HashMap::new(),
            handled: HashSet::new(),
            handled_order: VecDeque::new(),
            awaiting: HashMap::new(),
            held: HashMap::new(),
            files: HashMap::new(),
            fetching: None,
            fetch_retry: Backoff::new(Duration::ZERO),
            discards: VecDeque::new(),
        }
    }

    /// Follow the account: signed in with its Feed switch on. Off, replies are
    /// read again from the start next time.
    pub fn set_enabled(&mut self, enabled: bool, now: Duration) {
        if enabled == self.enabled {
            return;
        }
        self.enabled = enabled;
        self.cursor = 0;
        self.poll = None;
        self.pending.clear();
        self.reports.clear();
        self.awaiting.clear();
        // Nothing pending is typed in now: files saved for it go. A fetch
        // still out is removed when it's done.
        self.discards
            .extend(self.files.drain().map(|(reply, _)| reply));
        self.read_retry = Backoff::new(now);
        self.report_retry = Backoff::new(now);
        self.fetch_retry = Backoff::new(now);
        self.next_read = now;
    }

    /// rakumux.com asks for this machine's replies every second: someone has
    /// it open, a reply waits here, or it's the half minute after reply
    /// activity, so chats are worth uploading as they grow.
    pub fn watched(&self) -> bool {
        self.enabled && self.poll.is_some_and(|poll| poll <= WATCHED_POLL)
    }

    /// The daemon (re)connected: these are its sessions.
    pub fn connected(&mut self, views: &[SessionView]) {
        self.sessions.clear();
        for view in views {
            self.observe(view);
        }
    }

    pub fn observe(&mut self, view: &SessionView) {
        let seen = Seen::of(view);
        if let Some(delivery) = self.deliveries.get_mut(&view.id) {
            delivery.busy |= seen.busy();
        }
        self.sessions.insert(view.id.clone(), seen);
    }

    pub fn forget(&mut self, session: &SessionId) {
        self.sessions.remove(session);
        self.awaiting.remove(session);
        self.held.remove(session.as_str());
    }

    /// A reply written in this machine's TUI was stored: typed here at once
    /// when its session runs here.
    pub fn written(&mut self, reply: &FeedReply) {
        self.take(reply);
    }

    /// The same for a session message written in this machine's TUI.
    pub fn written_message(&mut self, message: &SessionMessage) {
        self.take_message(message);
    }

    /// The next thing to do, if anything is due: remove files no longer
    /// needed, type what can be (fetching its files first, deciding what
    /// can't), report it, then read. One request is out at a time; reads and
    /// reports each wait out their own failures, and downloads (one at a
    /// time, apart from them) theirs.
    pub fn next_work(&mut self, now: Duration) -> Option<ReplyWork> {
        // Local housekeeping, whatever the account.
        if let Some(reply) = self.discards.pop_front() {
            return Some(ReplyWork::Discard(reply));
        }
        if !self.enabled {
            return None;
        }
        if let Some(work) = self.deliver(now) {
            return Some(work);
        }
        if self.out.is_some() {
            return None;
        }
        if self.report_retry.ready(now) {
            while let Some((id, failure)) = self.reports.front() {
                match wire::reply_status(id, &self.machine, *failure) {
                    Some(request) => {
                        self.out = Some(Out::Report);
                        return Some(ReplyWork::Request(request));
                    }
                    // An id the contract can't carry: nothing to tell.
                    None => {
                        self.reports.pop_front();
                    }
                }
            }
        }
        if self.read_retry.ready(now) && now >= self.next_read {
            self.out = Some(Out::Read);
            return Some(ReplyWork::Request(FeedRequest::Replies {
                after: self.cursor,
                machine: self.machine.clone(),
            }));
        }
        None
    }

    /// The first pending reply that can be decided now: typed, or failed.
    fn deliver(&mut self, now: Duration) -> Option<ReplyWork> {
        let remote = self.accepts_remote(now);
        self.held.retain(|_, until| *until > now);
        self.follow_deliveries(now);
        // A session a reply is going into (or whose reply's file is being
        // fetched) takes nothing else until its agent took it and waits
        // again, and one someone is typing in nothing until they pause.
        let being_typed: HashSet<&str> = self
            .deliveries
            .keys()
            .map(SessionId::as_str)
            .chain(
                self.fetching
                    .iter()
                    .filter_map(|id| self.pending.get(id))
                    .map(|reply| reply.session_id.as_str()),
            )
            .collect();
        let mut ids: Vec<(u64, &String)> = self
            .pending
            .iter()
            .filter(|(id, reply)| {
                !self.handled.contains(*id)
                    && !being_typed.contains(reply.session_id.as_str())
                    && !self.held.contains_key(&reply.session_id)
            })
            .map(|(id, reply)| (reply.seq, id))
            .collect();
        // Oldest first: a session's replies go in in the order they were written.
        ids.sort();
        let fetch_ready = self.fetching.is_none() && self.fetch_retry.ready(now);
        // Sessions whose earlier reply waits (for its agent, or to fetch its
        // files): later ones wait behind it.
        let mut behind: HashSet<&str> = HashSet::new();
        let mut decided = None;
        for (_, id) in ids {
            let reply = &self.pending[id];
            if behind.contains(reply.session_id.as_str()) {
                continue;
            }
            let elsewhere = reply.from.as_deref() != Some(self.machine.as_str());
            let session = SessionId::from_string(reply.session_id.clone());
            let waits = Seen::waits;
            let outcome = match self.sessions.get(&session) {
                _ if elsewhere && !remote => Err(ReplyFailure::NotAccepted),
                None => Err(ReplyFailure::SessionGone),
                Some(seen) if !seen.live => Err(ReplyFailure::SessionGone),
                Some(seen) if seen.private => Err(ReplyFailure::Private),
                Some(seen) => match &reply.target {
                    // No agent there any more: not that chat.
                    Target::Chat(_) if seen.lifecycle == Lifecycle::None => {
                        Err(ReplyFailure::ConversationChanged)
                    }
                    Target::Chat(wanted) => match &seen.conversation {
                        Some(conversation) if conversation != wanted => {
                            Err(ReplyFailure::ConversationChanged)
                        }
                        // Its agent waits for input: type it.
                        Some(_) if waits(seen) => Ok(true),
                        // Busy, blocked on a question, or its chat not known yet: wait.
                        _ => Ok(false),
                    },
                    // Typed only to an agent that waits for input; a session
                    // without one (a shell) is waited for, never typed into.
                    Target::Session => Ok(waits(seen)),
                },
            };
            match outcome {
                Ok(false) => {
                    behind.insert(reply.session_id.as_str());
                    continue;
                }
                Ok(true) if !fetch_ready && self.unfetched(id).is_some() => {
                    behind.insert(reply.session_id.as_str());
                    continue;
                }
                Ok(true) => {
                    decided = Some((id.clone(), None, Some(session)));
                    break;
                }
                Err(failure) => {
                    decided = Some((id.clone(), Some(failure), None));
                    break;
                }
            }
        }
        let (id, failure, session) = decided?;
        match session {
            Some(session) => {
                if let Some(attachment) = self.unfetched(&id) {
                    let saved = self.files.get(&id).map_or(&[][..], Vec::as_slice);
                    let fetch = Fetch {
                        reply: id.clone(),
                        attachment: attachment.clone(),
                        first: saved.is_empty(),
                        taken: saved
                            .iter()
                            .filter_map(|path| Some(path.file_name()?.to_str()?.to_owned()))
                            .collect(),
                    };
                    self.fetching = Some(id);
                    return Some(ReplyWork::Fetch(fetch));
                }
                let reply = self.pending.get(&id)?;
                let saved = self.files.get(&id).map_or(&[][..], Vec::as_slice);
                let text = typed(&reply.text, saved);
                let seen = self.sessions.get(&session);
                let delivery = Delivery {
                    reply: id.clone(),
                    target: reply.target.clone(),
                    since: now,
                    typed: false,
                    turn: (
                        seen.and_then(|seen| seen.turn),
                        seen.and_then(|seen| seen.conversation.clone()),
                    ),
                    busy: false,
                    taken: None,
                };
                self.deliveries.insert(session.clone(), delivery);
                Some(ReplyWork::Submit {
                    reply: id,
                    session,
                    text,
                })
            }
            None => {
                self.handle(&id, failure);
                // Decided without typing: look again (and report) at once.
                self.deliver(now)
            }
        }
    }

    /// Follow the replies typed in (see [`Delivery`]): one its agent took is
    /// reported delivered, one whose session ended or chat changed first
    /// failed, and a session whose agent took its reply and waits again takes
    /// the next.
    fn follow_deliveries(&mut self, now: Duration) {
        let mut settled = Vec::new();
        let sessions = &self.sessions;
        self.deliveries.retain(|session, delivery| {
            let seen = sessions.get(session);
            if !delivery.typed {
                // The daemon hasn't answered yet.
                return true;
            }
            if delivery.taken.is_some() {
                return !delivery.done(seen, now);
            }
            match delivery.taking(seen, now) {
                Taking::Waiting => true,
                Taking::Taken => {
                    delivery.taken = Some(now);
                    settled.push((delivery.reply.clone(), None));
                    !delivery.done(seen, now)
                }
                Taking::Failed(failure) => {
                    settled.push((delivery.reply.clone(), Some(failure)));
                    false
                }
            }
        });
        for (reply, failure) in settled {
            self.settle(&reply, failure, true);
        }
    }

    /// A reply is going into its session now (or was, until it's decided).
    fn delivering(&self, id: &str) -> bool {
        self.deliveries
            .values()
            .any(|delivery| delivery.reply == id && delivery.taken.is_none())
    }

    /// The first of a pending reply's files not saved yet, if any.
    fn unfetched(&self, id: &str) -> Option<&ReplyAttachment> {
        let saved = self.files.get(id).map_or(0, Vec::len);
        self.pending.get(id)?.attachments.get(saved)
    }

    /// One of a reply's files was fetched (or wasn't); `current` says whether
    /// for the account still signed in.
    pub fn fetched(&mut self, current: bool, reply: String, fetched: Fetched, now: Duration) {
        if self.fetching.as_ref() == Some(&reply) {
            self.fetching = None;
        }
        if !current || !self.enabled || !self.pending.contains_key(&reply) {
            // Not for a reply typed in now: what's saved of it goes, or, while
            // it's still pending, is made afresh by its next fetch.
            self.files.remove(&reply);
            if !self.enabled || !self.pending.contains_key(&reply) {
                self.discards.push_back(reply);
            }
            return;
        }
        match fetched {
            Fetched::Saved(path) => {
                self.fetch_retry.succeeded();
                self.files.entry(reply).or_default().push(path);
            }
            Fetched::Failed(failure) if failure != FeedFailure::Invalid => {
                self.fetch_retry.failed(failure, now);
            }
            // Gone, not what the reply said, or not to be saved here: nothing is typed.
            Fetched::Unavailable | Fetched::Unsaved | Fetched::Failed(_) => {
                self.handle(&reply, Some(ReplyFailure::AttachmentUnavailable));
            }
        }
    }

    /// Whether replies written elsewhere are typed here, read again every
    /// [`SETTING_EVERY`].
    fn accepts_remote(&mut self, now: Duration) -> bool {
        let stale = self
            .remote_read
            .is_none_or(|at| now.saturating_sub(at) >= SETTING_EVERY);
        if stale {
            self.remote = self.accept_remote.as_ref().is_some_and(|read| read());
            self.remote_read = Some(now);
        }
        self.remote
    }

    /// The session a reply handed to the daemon goes into, while the daemon
    /// hasn't answered.
    fn submitting(&self, reply: &str) -> Option<SessionId> {
        self.deliveries
            .iter()
            .find(|(_, delivery)| delivery.reply == reply && !delivery.typed)
            .map(|(session, _)| session.clone())
    }

    /// Typed into its session (or the daemon refused it). Typed in, it's
    /// delivered once its agent is seen to take it (see [`Delivery`]), and
    /// the agent's next reply there answers it.
    pub fn submitted(&mut self, reply: String, result: Result<(), ErrorCode>, now_ms: i64) {
        let Some(session) = self.submitting(&reply) else {
            return;
        };
        match result {
            Ok(()) => {
                let Some(delivery) = self.deliveries.get_mut(&session) else {
                    return;
                };
                delivery.typed = true;
                // A reply is answered in its chat; a session message in the
                // chat the session shows now, or any when it doesn't say.
                let conversation = match &delivery.target {
                    Target::Chat(conversation) => Some(conversation.clone()),
                    Target::Session => self
                        .sessions
                        .get(&session)
                        .and_then(|seen| seen.conversation.clone()),
                };
                self.awaiting.insert(
                    session,
                    Awaiting {
                        reply,
                        conversation,
                        since_ms: now_ms,
                    },
                );
            }
            Err(_) => {
                self.deliveries.remove(&session);
                self.handle(&reply, Some(ReplyFailure::Refused));
            }
        }
    }

    /// The daemon refused it because someone was typing in its session: it
    /// stays pending, and that session is tried again after [`TYPING_HOLD`].
    pub fn typing(&mut self, reply: String, now: Duration) {
        if let Some(session) = self.submitting(&reply) {
            self.deliveries.remove(&session);
            self.held
                .insert(session.as_str().to_owned(), now + TYPING_HOLD);
        }
    }

    fn handle(&mut self, id: &str, failure: Option<ReplyFailure>) {
        self.settle(id, failure, false);
    }

    /// Decided: rakumux.com hears, and it's never typed (again). Its files
    /// stay for the agent once `typed_in`, else go when it failed.
    fn settle(&mut self, id: &str, failure: Option<ReplyFailure>, typed_in: bool) {
        let attached = self
            .pending
            .remove(id)
            .is_some_and(|reply| !reply.attachments.is_empty());
        self.files.remove(id);
        if attached && failure.is_some() && !typed_in {
            self.discards.push_back(id.to_owned());
        }
        self.reports.push_back((id.to_owned(), failure));
        if self.handled.insert(id.to_owned()) {
            self.handled_order.push_back(id.to_owned());
            while self.handled_order.len() > MAX_HANDLED {
                if let Some(old) = self.handled_order.pop_front() {
                    self.handled.remove(&old);
                }
            }
        }
    }

    /// The reply `answer` answers, when it's the agent's next one after a
    /// reply was typed into its session, in that conversation.
    pub fn answered(&mut self, answer: &AgentReply) -> Option<String> {
        let awaiting = self.awaiting.get(&answer.session_id)?;
        let same_chat = awaiting
            .conversation
            .as_deref()
            .is_none_or(|conversation| answer.conversation_id.as_deref() == Some(conversation));
        if !same_chat || answer.finished_at < awaiting.since_ms {
            return None;
        }
        self.awaiting
            .remove(&answer.session_id)
            .map(|awaiting| awaiting.reply)
    }

    /// A read or report finished; `current` says whether it was made for the
    /// account still signed in. Returns the replies it brought, for TUIs.
    pub fn request_done(
        &mut self,
        current: bool,
        outcome: Result<FeedResult, FeedFailure>,
        now: Duration,
    ) -> Vec<FeedReply> {
        match (self.out, outcome) {
            (Some(Out::Read), Ok(FeedResult::Replies(read))) => {
                self.read_done(current, Ok(read), now)
            }
            (Some(Out::Report), Ok(FeedResult::Reply(reply))) => self
                .reported(current, Ok(Some(reply)), now)
                .into_iter()
                .collect(),
            // A session message's status: nothing for TUIs.
            (Some(Out::Report), Ok(FeedResult::Message(_))) => {
                self.reported(current, Ok(None), now);
                Vec::new()
            }
            (Some(Out::Read), Err(failure)) => self.read_done(current, Err(failure), now),
            (Some(Out::Report), Err(failure)) => {
                self.reported(current, Err(failure), now);
                Vec::new()
            }
            // Not what was asked: the contract was broken.
            (Some(Out::Read), Ok(_)) => self.read_done(current, Err(FeedFailure::Invalid), now),
            (Some(Out::Report), Ok(_)) => {
                self.reported(current, Err(FeedFailure::Invalid), now);
                Vec::new()
            }
            (None, _) => Vec::new(),
        }
    }

    /// A read finished. Returns the replies read, for TUIs' threads.
    fn read_done(
        &mut self,
        current: bool,
        outcome: Result<RepliesRead, FeedFailure>,
        now: Duration,
    ) -> Vec<FeedReply> {
        self.out = None;
        if !current || !self.enabled {
            return Vec::new();
        }
        match outcome {
            Ok(read) => {
                self.read_retry.succeeded();
                for reply in &read.replies {
                    self.take(reply);
                }
                for message in &read.messages {
                    self.take_message(message);
                }
                // Past this page only while more wait: the account's newest
                // seq is on a later page, after replies not read yet.
                self.cursor = self.cursor.max(read.read_to());
                self.poll = read.poll;
                self.next_read = if read.more {
                    now
                } else {
                    now + read.poll.unwrap_or(READ_EVERY)
                };
                read.replies
            }
            Err(failure) => {
                self.read_retry.failed(failure, now);
                Vec::new()
            }
        }
    }

    /// Hold `reply` for delivery when it's pending here, else drop it from
    /// what's pending.
    fn take(&mut self, reply: &FeedReply) {
        self.hold(&reply.id, &reply.machine, reply.status, || {
            Pending::reply(reply)
        });
    }

    /// The same for a session message.
    fn take_message(&mut self, message: &SessionMessage) {
        self.hold(&message.id, &message.machine, message.status, || {
            Pending::message(message)
        });
    }

    fn hold(
        &mut self,
        id: &str,
        machine: &str,
        status: ReplyStatus,
        pending: impl FnOnce() -> Pending,
    ) {
        let mine = machine == self.machine;
        if mine && status == ReplyStatus::Pending && !self.handled.contains(id) {
            self.pending.insert(id.to_owned(), pending());
        } else if self.pending.remove(id).is_some()
            && self.files.remove(id).is_some()
            && !self.delivering(id)
        {
            // No longer pending (expired, say) before it was typed in; one
            // typed in (or being typed in) keeps its files for the agent.
            self.discards.push_back(id.to_owned());
        }
    }

    /// A status report finished. Returns the stored reply, for TUIs (none for
    /// a session message).
    fn reported(
        &mut self,
        current: bool,
        outcome: Result<Option<FeedReply>, FeedFailure>,
        now: Duration,
    ) -> Option<FeedReply> {
        self.out = None;
        if !current || !self.enabled {
            return None;
        }
        match outcome {
            Ok(reply) => {
                self.report_retry.succeeded();
                self.reports.pop_front();
                reply
            }
            // A report the cloud refuses is dropped, not retried forever.
            Err(FeedFailure::Invalid) => {
                self.reports.pop_front();
                None
            }
            Err(failure) => {
                self.report_retry.failed(failure, now);
                None
            }
        }
    }
}

#[cfg(test)]
mod tests;
