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
//! A reply counts as written here only when this machine made its id (`from`
//! is what the writer said, and any machine of the account can say anything).
//! Which replies were written here, typed in or decided, and the reports not
//! yet taken, are kept across a restart (`kept`), so a reply is typed at most
//! once whatever happens to the service.
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

mod kept;
use kept::Kept;

/// How often the account's replies are read while the feed is on, unless
/// rakumux.com's answer says otherwise (`poll_ms`: every second while a reply
/// waits for this machine or the website is open, and for 30 s after the
/// account's last reply activity, then 2, 4 and 8 s as the quiet time
/// doubles, up to 15 s after 4 minutes).
const READ_EVERY: Duration = Duration::from_secs(5);
/// rakumux.com asks this often while someone has it open, a reply waits here,
/// or for 30 s after the account's last reply activity.
const WATCHED_POLL: Duration = Duration::from_secs(1);
/// While pokes come (`set_pushed`), replies are read when poked, as often as
/// rakumux.com asks while it asks every second (someone watching), and
/// otherwise this often, in case a poke was lost.
const PUSHED_READ_EVERY: Duration = Duration::from_secs(60);
/// The first and longest waits after a failed request.
const RETRY_MIN: Duration = Duration::from_secs(5);
const RETRY_MAX: Duration = Duration::from_secs(300);
/// The longest wait a refusal for too many requests holds a kind of request
/// back, whatever rakumux.com asks: its quotas count fixed windows, and it
/// asks to wait until the window ends (up to an hour, a day for daily ones),
/// while a refused request isn't counted. Pending replies expire in an hour.
const RATE_LIMIT_MAX: Duration = Duration::from_secs(60);
/// How long the "replies from elsewhere" setting is trusted before it's read again.
const SETTING_EVERY: Duration = Duration::from_secs(10);
/// A reply typed in is answered by its agent's next reply in its turn, which
/// the daemon captures within 10 s of the turn ending: one whose turn ended
/// this long ago without an answer has none, and is forgotten.
const ANSWER_WAIT: Duration = Duration::from_secs(30);
/// An answer's turn ended within this of when its reply's turn was seen to
/// end (the daemon notes the end first; this service hears of it after).
const ANSWER_SLACK_MS: i64 = 1_000;
/// Nothing is typed into a session someone typed in this recently, so a reply
/// isn't added to what they're writing (`session_submit`'s `quiet_ms`).
pub(crate) const TYPING_QUIET: Duration = Duration::from_secs(10);
/// After the daemon refused because someone was typing, try that session again this much later.
pub(crate) const TYPING_HOLD: Duration = Duration::from_secs(2);
/// A reply typed in is delivered once its agent's chat shows it as a new
/// prompt; one whose chat shows no new prompt this long after it was typed
/// failed (`not_taken`: its Enter was lost, or it went into a menu), so its
/// session's next replies never wait on it for ever. An agent whose turns
/// the daemon doesn't follow takes it once seen busy, or after this all the
/// same.
pub(crate) const TAKE_WITHIN: Duration = Duration::from_secs(30);
/// A new prompt is a reply's when its letters and digits begin the reply's,
/// or hold the reply's first this many (the reply typed after a draft on
/// the same line).
const PROMPT_HEAD: usize = 32;
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
        // Another machine held this one's name: it takes another, and a try
        // again goes through.
        Err(
            FeedFailure::RateLimited(_) | FeedFailure::Unavailable(_) | FeedFailure::MachineTaken,
        ) => ReplyWriteStatus::Unavailable,
    }
}

/// What to do next for replies.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ReplyWork {
    /// Ask rakumux.com (a read, or a status report).
    Request(FeedRequest),
    /// Type `text` into `session`, while `foreground` is the program there.
    Submit {
        reply: String,
        session: SessionId,
        text: String,
        foreground: Option<String>,
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
            FeedFailure::RateLimited(Some(delay)) => delay.min(RATE_LIMIT_MAX),
            FeedFailure::Unavailable(Some(delay)) => delay.min(RETRY_MAX),
            _ => self.wait,
        };
        self.wait = self.wait.saturating_mul(2).min(RETRY_MAX);
        self.at = now + wait;
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
    /// The first line of the user's last prompt to its agent (`user_prompt`).
    prompt: Option<String>,
    /// The program in its foreground: a reply is typed only while it's still
    /// there (`session_submit`'s `expect_foreground`).
    foreground: Option<String>,
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
            prompt: view.user_prompt.clone(),
            foreground: view
                .terminal
                .as_ref()
                .and_then(|terminal| terminal.foreground_process.clone()),
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
}

impl Pending {
    fn reply(reply: &FeedReply) -> Self {
        Self {
            seq: reply.seq,
            session_id: reply.session_id.clone(),
            target: Target::Chat(reply.conversation_id.clone()),
            text: reply.text.clone(),
            attachments: reply.attachments.clone(),
        }
    }

    fn message(message: &SessionMessage) -> Self {
        Self {
            seq: message.seq,
            session_id: message.session_id.clone(),
            target: Target::Session,
            text: message.text.clone(),
            attachments: message.attachments.clone(),
        }
    }
}

/// A reply going into its session, and what its agent showed since: nothing
/// else is typed there until the agent is seen to take it and then waits
/// again. Taken means its chat shows a new prompt that is this reply (its
/// first line, [`Delivery::matches`]), or, for an agent whose turns the
/// daemon doesn't follow, that it works or asks something. Taken, it's
/// reported delivered; its session ended, its chat changed, another prompt
/// went in or none within [`TAKE_WITHIN`], failed. The agent's lifecycle can
/// lag what it does, so a view that still shows it waiting proves nothing.
struct Delivery {
    reply: String,
    target: Target,
    /// Its text as typed, letters and digits only ([`squash`]).
    sent: String,
    /// When it was handed to the daemon.
    since: Duration,
    /// The daemon typed it in.
    typed: bool,
    /// The agent's turn and chat just before: another turn after is a new prompt.
    turn: (Option<u64>, Option<String>),
    /// The agent was seen working or asking something since.
    busy: bool,
    /// Its chat was seen with a new prompt that is this reply since.
    took: bool,
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
        let prompted = self.prompted(seen);
        if self.took || prompted == Some(true) {
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
        if prompted == Some(false) {
            // Another prompt went in: this one wasn't taken as typed.
            return Taking::Failed(ReplyFailure::NotTaken);
        }
        // Its chat's turns say whether the agent took it; without them, its
        // being busy does.
        let turns = seen.turn.is_some() || self.turn.0.is_some();
        if !turns && self.busy {
            return Taking::Taken;
        }
        if now.saturating_sub(self.since) < TAKE_WITHIN {
            Taking::Waiting
        } else if turns {
            Taking::Failed(ReplyFailure::NotTaken)
        } else {
            Taking::Taken
        }
    }

    /// Whether `seen` shows a new prompt in its session's chat since this
    /// reply was typed, and whether it's this reply ([`Delivery::matches`]):
    /// `None` for no new prompt. Without a prompt to compare (an older
    /// daemon), a new turn in the same chat is this reply, and one in another
    /// chat no sign either way.
    fn prompted(&self, seen: &Seen) -> Option<bool> {
        let new =
            seen.turn.is_some() && (seen.turn, &seen.conversation) != (self.turn.0, &self.turn.1);
        if !new {
            return None;
        }
        match self.matches(seen.prompt.as_deref()) {
            Some(mine) => Some(mine),
            None => (self.turn.1.is_none() || seen.conversation == self.turn.1).then_some(true),
        }
    }

    /// Whether `prompt`, a new prompt's first line as the daemon reads it
    /// from the agent's chat (marks dropped, cut at 240 characters), is this
    /// reply: its letters and digits begin the reply's, or hold the reply's
    /// first [`PROMPT_HEAD`] (typed in after a draft on the same line, it
    /// still went in). `None` when there's nothing to compare.
    fn matches(&self, prompt: Option<&str>) -> Option<bool> {
        let line = squash(prompt?);
        if line.is_empty() || self.sent.is_empty() {
            return None;
        }
        let head: String = self.sent.chars().take(PROMPT_HEAD).collect();
        Some(self.sent.starts_with(&line) || line.contains(&head))
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

/// `text`'s letters and digits, lower case: what a reply and the prompt an
/// agent's chat shows for it have in common, whatever each did to spaces,
/// marks and lines.
fn squash(text: &str) -> String {
    text.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
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

/// A reply typed in, waiting for the agent's answer in `conversation` (any of
/// the session's chats when it's `None`). Its answer is the agent's reply that
/// finished after it was typed and, once its turn was seen to end, by then: so
/// a reply typed in behind it never takes its answer, and one whose turn ended
/// without an answer (interrupted, an error) never takes a later one.
struct Awaiting {
    reply: String,
    conversation: Option<String>,
    /// When it was typed in, on the wall clock (as answers' `finished_at`)
    /// and the owner thread's.
    since_ms: i64,
    since: Duration,
    /// When its turn was seen to end (its agent took it and waits again),
    /// on both clocks.
    ended: Option<(i64, Duration)>,
}

impl Awaiting {
    /// Its turn was seen to end at `now` on the owner's clock.
    fn end(&mut self, now: Duration) {
        if self.ended.is_none() {
            let since =
                i64::try_from(now.saturating_sub(self.since).as_millis()).unwrap_or(i64::MAX);
            self.ended = Some((self.since_ms.saturating_add(since), now));
        }
    }
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
    /// A realtime connection is open: rakumux.com pokes when to read.
    pushed: bool,
    /// Poked while a read was out: read again once it's back.
    reread: bool,
    /// Reads' and reports' waits after failures, apart.
    read_retry: Backoff,
    report_retry: Backoff,
    /// This machine's pending replies and session messages, by id, as last read.
    pending: HashMap<String, Pending>,
    /// The reply going into each session, until its agent took it and waits again.
    deliveries: HashMap<SessionId, Delivery>,
    /// Replies written, typed in or decided here, and reports not yet taken,
    /// kept across a restart.
    kept: Kept,
    /// Each session's replies typed in and not yet answered, oldest first.
    awaiting: HashMap<SessionId, VecDeque<Awaiting>>,
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
    /// elsewhere are typed here (never without it); `kept` is the file that
    /// keeps what's typed in across a restart (memory only without it).
    pub fn new(
        machine: String,
        accept_remote: Option<AcceptRemote>,
        kept: Option<PathBuf>,
    ) -> Self {
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
            pushed: false,
            reread: false,
            read_retry: Backoff::new(Duration::ZERO),
            report_retry: Backoff::new(Duration::ZERO),
            pending: HashMap::new(),
            deliveries: HashMap::new(),
            kept: Kept::load(kept),
            awaiting: HashMap::new(),
            held: HashMap::new(),
            files: HashMap::new(),
            fetching: None,
            fetch_retry: Backoff::new(Duration::ZERO),
            discards: VecDeque::new(),
        }
    }

    /// Follow the account: signed in with its Feed switch on. Off, replies are
    /// read again from the start next time; reports not yet taken are kept,
    /// and go once it's on again (one for another account is refused, and
    /// dropped).
    pub fn set_enabled(&mut self, enabled: bool, now: Duration) {
        if enabled == self.enabled {
            return;
        }
        self.enabled = enabled;
        self.cursor = 0;
        self.poll = None;
        self.reread = false;
        self.pending.clear();
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

    /// A realtime connection opened (`pushed`) or closed: while open, replies
    /// are read when poked; closed, at rakumux.com's pace again.
    pub fn set_pushed(&mut self, pushed: bool, now: Duration) {
        if !pushed && self.pushed {
            self.next_read = self.next_read.min(now + self.poll.unwrap_or(READ_EVERY));
        }
        self.pushed = pushed;
    }

    /// rakumux.com poked: read now, or again once the read out is back (it
    /// may have been answered before the change).
    pub fn poked(&mut self, now: Duration) {
        if self.out == Some(Out::Read) {
            self.reread = true;
        } else {
            self.next_read = now;
        }
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
            delivery.took |= delivery.prompted(&seen) == Some(true);
        }
        self.sessions.insert(view.id.clone(), seen);
    }

    pub fn forget(&mut self, session: &SessionId) {
        self.sessions.remove(session);
        self.awaiting.remove(session);
        self.held.remove(session.as_str());
    }

    /// rakumux.com named another machine as holding this one's name: from
    /// now on it goes by `machine`, and reads its replies from the start.
    pub fn rename(&mut self, machine: String, now: Duration) {
        self.machine = machine;
        self.cursor = 0;
        self.poll = None;
        self.pending.clear();
        self.next_read = now;
    }

    /// This machine made the id of a reply it's about to write: one read with
    /// it counts as written here (a reply's `from` is only what its writer
    /// said).
    pub fn wrote(&mut self, id: &str) {
        self.kept.wrote(id);
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
            while let Some(report) = self.kept.next_report() {
                match wire::reply_status(&report.id, &self.machine, report.failure) {
                    Some(request) => {
                        self.out = Some(Out::Report);
                        return Some(ReplyWork::Request(request));
                    }
                    // An id the contract can't carry: nothing to tell.
                    None => self.kept.reported(),
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
                !self.kept.handled(id)
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
            // Written here only when this machine made its id: `from` is
            // whatever its writer said.
            let elsewhere = !self.kept.is_own(id);
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
                let target = reply.target.clone();
                // Never typed again from now on, even if this service stops
                // before the daemon answers.
                self.kept.typing(&id);
                let seen = self.sessions.get(&session);
                let foreground = seen.and_then(|seen| seen.foreground.clone());
                let delivery = Delivery {
                    reply: id.clone(),
                    target,
                    sent: squash(&text),
                    since: now,
                    typed: false,
                    turn: (
                        seen.and_then(|seen| seen.turn),
                        seen.and_then(|seen| seen.conversation.clone()),
                    ),
                    busy: false,
                    took: false,
                    taken: None,
                };
                self.deliveries.insert(session.clone(), delivery);
                Some(ReplyWork::Submit {
                    reply: id,
                    session,
                    text,
                    foreground,
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
        // Replies whose turn ended (taken, and their agent waits again), and
        // those that failed after all, by session.
        let mut ended = Vec::new();
        let mut gone = Vec::new();
        let sessions = &self.sessions;
        self.deliveries.retain(|session, delivery| {
            let seen = sessions.get(session);
            if !delivery.typed {
                // The daemon hasn't answered yet.
                return true;
            }
            if delivery.taken.is_none() {
                match delivery.taking(seen, now) {
                    Taking::Waiting => return true,
                    Taking::Taken => {
                        delivery.taken = Some(now);
                        settled.push((delivery.reply.clone(), None));
                    }
                    Taking::Failed(failure) => {
                        settled.push((delivery.reply.clone(), Some(failure)));
                        gone.push((session.clone(), delivery.reply.clone()));
                        return false;
                    }
                }
            }
            let done = delivery.done(seen, now);
            if done {
                ended.push((session.clone(), delivery.reply.clone()));
            }
            !done
        });
        for (reply, failure) in settled {
            self.settle(&reply, failure, true);
        }
        for (session, reply) in ended {
            if let Some(awaiting) = self.awaiting.get_mut(&session) {
                for entry in awaiting.iter_mut().filter(|entry| entry.reply == reply) {
                    entry.end(now);
                }
            }
        }
        for (session, reply) in gone {
            if let Some(awaiting) = self.awaiting.get_mut(&session) {
                awaiting.retain(|entry| entry.reply != reply);
            }
        }
        // Answers come within seconds of their turn's end, or never.
        for awaiting in self.awaiting.values_mut() {
            awaiting.retain(|entry| {
                entry
                    .ended
                    .is_none_or(|(_, at)| now.saturating_sub(at) < ANSWER_WAIT)
            });
        }
        self.awaiting.retain(|_, awaiting| !awaiting.is_empty());
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

    /// Typed into its session (or the daemon refused it), at `now` on the
    /// owner's clock and `now_ms` on the wall clock. Typed in, it's
    /// delivered once its agent is seen to take it (see [`Delivery`]), and
    /// the agent's next reply in its turn answers it (see [`Awaiting`]).
    pub fn submitted(
        &mut self,
        reply: String,
        result: Result<(), ErrorCode>,
        now: Duration,
        now_ms: i64,
    ) {
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
                let awaiting = self.awaiting.entry(session).or_default();
                // Typed in only once the turns before ended.
                for earlier in awaiting.iter_mut() {
                    if earlier.ended.is_none() {
                        earlier.ended = Some((now_ms, now));
                    }
                }
                awaiting.push_back(Awaiting {
                    reply,
                    conversation,
                    since_ms: now_ms,
                    since: now,
                    ended: None,
                });
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
        self.hold_back(reply, now);
    }

    /// The daemon refused it because another program than the one seen has
    /// its session's foreground (its agent just exited, say): it stays
    /// pending, and that session is looked at again after [`TYPING_HOLD`],
    /// by when its view says what runs there.
    pub fn foreground_changed(&mut self, reply: String, now: Duration) {
        self.hold_back(reply, now);
    }

    fn hold_back(&mut self, reply: String, now: Duration) {
        if let Some(session) = self.submitting(&reply) {
            self.deliveries.remove(&session);
            self.kept.untyped(&reply);
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
        self.kept.decided(id, failure);
    }

    /// The reply `answer` answers: of those typed into its session in its
    /// conversation, the latest typed in before its turn finished, unless
    /// that one's turn had been seen to end before (see [`Awaiting`]). The
    /// replies typed in before it lost their answers.
    pub fn answered(&mut self, answer: &AgentReply) -> Option<String> {
        let awaiting = self.awaiting.get_mut(&answer.session_id)?;
        let at = awaiting.iter().rposition(|entry| {
            let same_chat = entry
                .conversation
                .as_deref()
                .is_none_or(|conversation| answer.conversation_id.as_deref() == Some(conversation));
            same_chat && entry.since_ms <= answer.finished_at
        })?;
        let entry = &awaiting[at];
        if entry
            .ended
            .is_some_and(|(ended_ms, _)| answer.finished_at > ended_ms + ANSWER_SLACK_MS)
        {
            return None;
        }
        let reply = entry.reply.clone();
        awaiting.drain(..=at);
        if awaiting.is_empty() {
            self.awaiting.remove(&answer.session_id);
        }
        Some(reply)
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
                let poll = read.poll.unwrap_or(READ_EVERY);
                self.next_read = if read.more || std::mem::take(&mut self.reread) {
                    now
                } else if self.pushed && poll > WATCHED_POLL {
                    now + PUSHED_READ_EVERY
                } else {
                    now + poll
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
        if mine && status == ReplyStatus::Pending && !self.kept.handled(id) {
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
                self.kept.reported();
                reply
            }
            // A report the cloud refuses is dropped, not retried forever.
            Err(FeedFailure::Invalid) => {
                self.kept.reported();
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
