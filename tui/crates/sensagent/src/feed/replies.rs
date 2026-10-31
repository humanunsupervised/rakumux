//! Replies written back to feed entries (`contracts/feed/` `reply`): while the
//! account's feed is on, the status service reads the account's
//! replies as they change (every few seconds), keeps them for TUIs' threads,
//! and types the pending ones addressed to this machine into their sessions
//! (`session_submit`: a paste, then Enter, the same for every agent), then
//! says so. It delivers only to the entry's own session, while it runs, isn't
//! private, still shows the entry's conversation and its agent waits for
//! input, or works when it's one that takes what's typed during a turn
//! ([`QUEUES_INPUT`]: typed **mid-turn**) and the daemon refuses to type
//! into an agent that asks something (`session.submit.blocked`); a blocked
//! agent (asking something), one of the others that works, or any that
//! works while the daemon lacks that guard, is waited for (the cloud
//! expires a reply after an hour). A session's replies go in one at a time,
//! oldest first, and each is reported delivered only once its agent is seen
//! to take it (see `Delivery`). The next waits for the agent to wait again,
//! or, while an agent of [`QUEUES_INPUT`] whose turns the daemon follows
//! works, only for that take: an agent that never stops gets each in turn,
//! as it would from someone typing at its prompt. A
//! session message (written on rakumux.com to a session with no answer to
//! reply to) goes in the same queue: it is typed once an agent runs there
//! and takes input, whatever chat it shows; never into a session without an
//! agent, which is waited for. Session messages never reach TUIs. A reply
//! written on rakumux.com, another machine or this machine's TUI is typed
//! alike; one written in this machine's TUI waits for a read that reaches it
//! (5 s at most), so one written before it elsewhere, to the same session and
//! not read here yet, goes in first. The agent's next reply in that session
//! is uploaded as the reply's answer (`reply_to`).
//!
//! Which replies were typed in or decided, and the reports not yet taken, are
//! kept across a restart (`kept`), so a reply is typed at most once whatever
//! happens to the service; one still going in, with the session it went into,
//! so a restarted service follows it as before and types nothing else there
//! meanwhile.
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
use std::time::Duration;

use serde::{Deserialize, Serialize};

use session_core::api::{AgentReply, ErrorCode};
use session_core::ids::SessionId;
use session_core::model::{Lifecycle, SessionStatus, SessionView};

use super::cache::{FeedReply, ReplyAttachment, ReplyFailure, ReplyStatus};
use super::job::FeedResult;
use super::wire::{self, FeedFailure, FeedRequest, QuestionReply, RepliesRead, SessionMessage};

mod kept;
use kept::{Kept, TypedInto};

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
/// session's next replies never wait on it for ever. One typed mid-turn is
/// taken at the agent's next step, minutes on maybe, or once the turn ends:
/// it fails only once its agent has waited this long without taking it. An
/// agent whose turns the daemon doesn't follow takes it once seen busy, or
/// after this all the same.
pub(crate) const TAKE_WITHIN: Duration = Duration::from_secs(30);
/// A new prompt is a reply's when its letters and digits begin the reply's,
/// or hold the reply's first this many (the reply typed after a draft on
/// the same line).
const PROMPT_HEAD: usize = 32;
/// A reply taken without its agent being seen busy (its lifecycle can lag a
/// new prompt) holds its session's next one this long before the agent's
/// waiting is believed.
pub(crate) const SETTLE: Duration = Duration::from_secs(5);
/// A reply written in this machine's TUI waits for a read that reaches it
/// (one normally back within a second) at most this long, then goes in all
/// the same: reads failing, or slow behind other work, don't hold it back.
const ORDER_WAIT: Duration = Duration::from_secs(5);
/// An answer to an agent's question goes to the daemon while its session's
/// agent asks something (`blocked`); one whose session has shown it asking
/// nothing this long goes all the same, and the daemon says whether the
/// question is still open (it types nothing unless it shows it): so an
/// answer to a question answered in the terminal is reported soon, not left
/// pending for the hour.
const ANSWER_GONE_AFTER: Duration = Duration::from_secs(10);

/// The agents a reply is typed into while they work (`working`, never
/// `blocked`), by the daemon's names for them (`SessionView::agent`'s
/// `kind`): their input box stays open during a turn, Enter there queues
/// the text (or steers it in) for the agent's next step, or the next turn
/// when this one ends first, and their chat shows it as a new prompt once
/// the agent takes it, which is how its delivery is seen (see `Delivery`).
/// Only while the daemon refuses to type into an agent that asks something
/// (capability `session.submit.blocked`, see [`ReplySync::connected`]): a
/// prompt can open between the view and the keystrokes, and without that
/// guard it takes the text and its Enter as its answer. Claude Code logs it
/// as a `queued_command` attachment, Codex as a `user_message`, and
/// OpenCode as a user message (1.x shows it `QUEUED`; 2.x steers it in, or
/// parks it with the turn's other queued prompts). Pi
/// steers it in once the response and tool calls under way are done, a
/// user message in the same run; GitHub Copilot holds it while its model
/// answers and steers it in between tool steps (a shell command running
/// then goes on in the background); Grok Build and Kimi Code queue it and
/// log it as the next turn's prompt as soon as the turn ends (each checked
/// end to end with the real CLI, offline). Every other agent gets replies
/// only while it waits.
///
/// An agent goes here once its integration reads what's typed during a
/// turn as a prompt of its chat, checked with the real agent. Droid and Amp
/// are said to queue or steer it too, and Cursor to hold it for a second
/// Enter; their end-to-end checks run stand-ins, which can't show it.
const QUEUES_INPUT: &[&str] = &[
    "claude",
    "claude-code",
    "codex",
    "opencode",
    "opencode2",
    "pi",
    "copilot",
    "grok",
    "kimi",
];

/// What became of a reply a TUI wrote.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReplyWriteStatus {
    /// Stored on rakumux.com, for its machine to type into the session.
    Sent,
    SignedOut,
    /// The account's feed is off.
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
    /// Give `answers` to the question `call_id` that `session`'s agent asks in
    /// its own dialog (`session_answer`).
    Answer {
        reply: String,
        session: SessionId,
        call_id: String,
        answers: Vec<session_core::api::QuestionAnswer>,
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
    /// Its agent takes what's typed while it works ([`QUEUES_INPUT`]).
    queues: bool,
}

impl Seen {
    fn of(view: &SessionView) -> Self {
        Self {
            queues: view
                .agent
                .as_ref()
                .is_some_and(|agent| QUEUES_INPUT.contains(&agent.kind.as_str())),
            live: view.status == SessionStatus::Live,
            private: crate::shared_elsewhere(view),
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

    /// Its agent works on a turn and takes what's typed meanwhile: one of
    /// [`QUEUES_INPUT`], whose turns the daemon follows, so its chat shows
    /// the reply as a prompt once it takes it, while the daemon refuses to
    /// type into an agent that asks something (`guarded`). Never while it
    /// asks something (`blocked`: a permission prompt, a question or a menu
    /// would take the text as its answer), nor while what it does isn't
    /// known.
    fn works_taking_input(&self, guarded: bool) -> bool {
        guarded && self.queues && self.lifecycle == Lifecycle::Working && self.turn.is_some()
    }

    /// A reply can be typed in now: its agent waits for input, or works and
    /// takes what's typed meanwhile (`guarded`: see [`Seen::works_taking_input`]).
    fn takes_input(&self, guarded: bool) -> bool {
        self.waits() || self.works_taking_input(guarded)
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
    /// An answer to the question the session's agent asks in its own dialog,
    /// in this chat: given to the dialog by the daemon, never typed as text.
    Answer {
        conversation: String,
        answer: QuestionReply,
    },
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
    /// When this machine's TUI wrote it, on the owner's clock, until a read
    /// brings it.
    written: Option<Duration>,
}

impl Pending {
    fn reply(reply: &FeedReply) -> Self {
        Self {
            seq: reply.seq,
            session_id: reply.session_id.clone(),
            target: Target::Chat(reply.conversation_id.clone()),
            text: reply.text.clone(),
            attachments: reply.attachments.clone(),
            written: None,
        }
    }

    fn message(message: &SessionMessage) -> Self {
        let target = match (&message.answer, &message.conversation_id) {
            (Some(answer), Some(conversation)) => Target::Answer {
                conversation: conversation.clone(),
                answer: answer.clone(),
            },
            _ => Target::Session,
        };
        Self {
            seq: message.seq,
            session_id: message.session_id.clone(),
            target,
            text: message.text.clone(),
            attachments: message.attachments.clone(),
            written: None,
        }
    }

    /// Written in this machine's TUI, and no read has reached it yet (reads
    /// are past `cursor`): a reply written before it elsewhere, to the same
    /// session, may still be unread here. Waited for at most [`ORDER_WAIT`].
    fn unread(&self, cursor: u64, now: Duration) -> bool {
        self.written
            .is_some_and(|at| self.seq > cursor && now.saturating_sub(at) < ORDER_WAIT)
    }
}

/// A reply going into its session, and what its agent showed since: nothing
/// else is typed there until the agent is seen to take it and then waits
/// again, or, for an agent that takes what's typed while it works
/// ([`Seen::works_taking_input`], the daemon guarding), until it's seen to
/// take it and work on (`free`): the next goes in mid-turn, and this one is
/// followed until its agent waits again, which ends its turn for its answer
/// (see [`Awaiting`]). Taken means its chat shows a new prompt that is this
/// reply (its first line, [`Delivery::matches`]), or, for an agent whose
/// turns the daemon doesn't follow, that it works or asks something. Taken,
/// it's reported delivered; its session ended, its chat changed, another prompt
/// went in or none within [`TAKE_WITHIN`], failed. The agent's lifecycle can
/// lag what it does, so a view that still shows it waiting proves nothing.
/// Typed mid-turn, its agent was busy already: only its chat's new prompt
/// says it took it, and only being busy after that counts; it's not taken
/// once its agent has waited [`TAKE_WITHIN`] without taking it (the turn
/// ended, and what was queued didn't go in).
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
    /// The agent was seen working or asking something since (typed mid-turn:
    /// since it took it).
    busy: bool,
    /// Its chat was seen with a new prompt that is this reply since.
    took: bool,
    /// When it was seen taken (and reported delivered).
    taken: Option<Duration>,
    /// Typed while its agent worked ([`Seen::works_taking_input`]).
    mid_turn: bool,
    /// Typed mid-turn and not taken yet: since when its agent has been seen
    /// waiting (or gone), its turn over; seen busy again, `None`.
    waited: Option<Duration>,
    /// Its chat showed it taken and its agent works on, taking what's typed
    /// meanwhile: its session takes the next reply now.
    free: bool,
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
        // Typed mid-turn, it's taken at the agent's next step or once the
        // turn ends: given up on only once the agent has waited a while.
        let from = if self.mid_turn {
            self.waited
        } else {
            Some(self.since)
        };
        if from.is_none_or(|from| now.saturating_sub(from) < TAKE_WITHIN) {
            Taking::Waiting
        } else if turns {
            Taking::Failed(ReplyFailure::NotTaken)
        } else {
            Taking::Taken
        }
    }

    /// Typed mid-turn and not taken yet: note whether its agent's turn is
    /// over (it waits, or left) at `now`, as `seen` shows.
    fn follow_turn(&mut self, seen: Option<&Seen>, now: Duration) {
        if !self.mid_turn {
            return;
        }
        let Some(seen) = seen else {
            return;
        };
        if seen.busy() {
            self.waited = None;
        } else if seen.waits() || seen.lifecycle == Lifecycle::None {
            self.waited.get_or_insert(now);
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

    /// Taken: whether its agent waits again (or its session ended), its turn
    /// over, so it's no longer followed.
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
/// without an answer (interrupted, an error) never takes a later one. One
/// typed mid-turn is answered only once its agent took it, by a reply that
/// finished after that: the turn it was typed into may end before the agent
/// takes it (it was writing its last message, or it takes queued text only
/// then), and that turn's reply isn't its answer. Several taken within one
/// turn (each typed mid-turn once the one before was taken) share its one
/// answer as an answer's `reply_to` can: the last taken before it was said
/// has it, the others none (see [`ReplySync::answered`]).
struct Awaiting {
    reply: String,
    conversation: Option<String>,
    /// When it was typed in (typed mid-turn: when its agent took it), on the
    /// wall clock (as answers' `finished_at`) and the owner thread's.
    since_ms: i64,
    since: Duration,
    /// Typed mid-turn, and its agent hasn't taken it yet: nothing answers it.
    untaken: bool,
    /// When its turn was seen to end (its agent took it and waits again),
    /// on both clocks.
    ended: Option<(i64, Duration)>,
}

impl Awaiting {
    /// `now` on the owner's clock, on the wall clock.
    fn wall_ms(&self, now: Duration) -> i64 {
        let since = i64::try_from(now.saturating_sub(self.since).as_millis()).unwrap_or(i64::MAX);
        self.since_ms.saturating_add(since)
    }

    /// Typed mid-turn, its agent was seen to take it at `now` on the
    /// owner's clock: answers count from then.
    fn taken(&mut self, now: Duration) {
        if self.untaken {
            self.since_ms = self.wall_ms(now);
            self.since = now;
            self.untaken = false;
        }
    }

    /// Its turn was seen to end at `now` on the owner's clock.
    fn end(&mut self, now: Duration) {
        if self.ended.is_none() {
            self.ended = Some((self.wall_ms(now), now));
        }
    }
}

/// Replies' state on the owner thread.
pub(crate) struct ReplySync {
    machine: String,
    enabled: bool,
    sessions: HashMap<SessionId, Seen>,
    /// The daemon refuses to type into an agent that asks something
    /// (`session.submit.blocked`): replies go in mid-turn only then.
    guarded: bool,
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
    /// The replies going into each session, oldest first, each until its
    /// agent took it and waits again; only the newest can hold its session
    /// back (the others are `free`).
    deliveries: HashMap<SessionId, Vec<Delivery>>,
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
    /// Replies handed to the daemon before this service started, not yet
    /// decided, by id: their sessions take nothing else until a read brings
    /// each again (still pending) and it's followed as a [`Delivery`], or
    /// says it's no longer pending.
    restored: HashMap<String, TypedInto>,
    /// The daemon answers agents' questions in their own dialogs
    /// (`session.answer`): answers are read, and given, only then.
    answers: bool,
    /// The answer the daemon is giving now, and its session: one at a time,
    /// and its session takes no reply meanwhile.
    answering: Option<(String, SessionId)>,
    /// Since when each pending answer's session was seen asking nothing.
    answer_waits: HashMap<String, Duration>,
}

impl ReplySync {
    /// Replies for `machine`; `kept` is the file that keeps what's typed in
    /// across a restart (memory only without it).
    pub fn new(machine: String, kept: Option<PathBuf>) -> Self {
        let kept = Kept::load(kept);
        let restored = kept
            .in_flight()
            .map(|(id, into)| (id.clone(), into.clone()))
            .collect();
        Self {
            machine,
            enabled: false,
            sessions: HashMap::new(),
            guarded: false,
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
            kept,
            awaiting: HashMap::new(),
            held: HashMap::new(),
            files: HashMap::new(),
            fetching: None,
            fetch_retry: Backoff::new(Duration::ZERO),
            discards: VecDeque::new(),
            restored,
            answers: false,
            answering: None,
            answer_waits: HashMap::new(),
        }
    }

    /// Whether the daemon answers agents' questions in their own dialogs
    /// (capability `session.answer`, read at each connect): reads ask for
    /// answers only then, and an answer read while it does is given only
    /// while it still does. A daemon without it never gets one, which stays
    /// pending until rakumux.com expires it.
    pub fn set_answers(&mut self, answers: bool) {
        self.answers = answers;
    }

    /// Follow the account: signed in with its feed on. Off, replies are
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

    /// rakumux.com poked: read now.
    pub fn poked(&mut self, now: Duration) {
        self.read_now(now);
    }

    /// Read now, or again once the read out is back (it may have been
    /// answered before the change).
    fn read_now(&mut self, now: Duration) {
        if self.out == Some(Out::Read) {
            self.reread = true;
        } else {
            self.next_read = now;
        }
    }

    /// The daemon (re)connected: these are its sessions, and `guarded` says
    /// whether it refuses to type into an agent that asks something
    /// (capability `session.submit.blocked`, read at each connect, as a
    /// daemon restarted since may announce it). Replies go into an agent at
    /// work (mid-turn) only then: a permission prompt can open between its
    /// view and the keystrokes, and a daemon without the guard types the
    /// reply into it, then the Enter that picks its highlighted option.
    /// Without it, replies wait for the agent to wait, as before mid-turn.
    pub fn connected(&mut self, views: &[SessionView], guarded: bool) {
        self.guarded = guarded;
        self.sessions.clear();
        for view in views {
            self.observe(view);
        }
    }

    pub fn observe(&mut self, view: &SessionView) {
        let seen = Seen::of(view);
        for delivery in self.deliveries.get_mut(&view.id).into_iter().flatten() {
            delivery.took |= delivery.prompted(&seen) == Some(true);
            // Typed mid-turn, its agent was busy already: busy counts once it took it.
            if delivery.took || !delivery.mid_turn {
                delivery.busy |= seen.busy();
            }
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

    /// A reply written in this machine's TUI was stored: typed here when its
    /// session runs here, once a read reaches it (its `seq`), which goes at
    /// once, so a reply written before it on rakumux.com to the same session,
    /// not read here yet, goes in first (see [`Pending::unread`]).
    pub fn written(&mut self, reply: &FeedReply, now: Duration) {
        self.take(reply);
        self.wait_for_read(&reply.id, now);
    }

    /// The same for a session message written in this machine's TUI.
    pub fn written_message(&mut self, message: &SessionMessage, now: Duration) {
        self.take_message(message);
        self.wait_for_read(&message.id, now);
    }

    fn wait_for_read(&mut self, id: &str, now: Duration) {
        if let Some(pending) = self.pending.get_mut(id) {
            pending.written = Some(now);
            self.read_now(now);
        }
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
                answers: self.answers,
            }));
        }
        None
    }

    /// The first pending reply that can be decided now: typed, or failed.
    fn deliver(&mut self, now: Duration) -> Option<ReplyWork> {
        self.held.retain(|_, until| *until > now);
        self.resume_restored(now);
        self.follow_deliveries(now);
        if let Some(work) = self.answer_next(now) {
            return Some(work);
        }
        // A session a reply is going into (or whose reply's file is being
        // fetched) takes nothing else until its agent took it and waits
        // again or works on taking input, and one someone is typing in
        // nothing until they pause.
        let being_typed: HashSet<&str> = self
            .deliveries
            .iter()
            .filter(|(_, deliveries)| deliveries.iter().any(|delivery| !delivery.free))
            .map(|(session, _)| session.as_str())
            .chain(
                self.fetching
                    .iter()
                    .filter_map(|id| self.pending.get(id))
                    .map(|reply| reply.session_id.as_str()),
            )
            .chain(self.restored.values().map(|into| into.session.as_str()))
            .chain(self.answering.iter().map(|(_, session)| session.as_str()))
            .collect();
        let mut ids: Vec<(u64, &String)> = self
            .pending
            .iter()
            .filter(|(id, reply)| {
                // Answers go apart (`answer_next`): only while their agent
                // asks something, when nothing else goes in.
                !matches!(reply.target, Target::Answer { .. })
                    && !self.kept.handled(id)
                    && !being_typed.contains(reply.session_id.as_str())
                    && !self.held.contains_key(&reply.session_id)
                    && !reply.unread(self.cursor, now)
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
            let session = SessionId::from_string(reply.session_id.clone());
            let guarded = self.guarded;
            let takes = |seen: &Seen| seen.takes_input(guarded);
            let outcome = match self.sessions.get(&session) {
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
                        // Its agent waits for input, or works and takes what's
                        // typed meanwhile: type it.
                        Some(_) if takes(seen) => Ok(true),
                        // Busy, blocked on a question, or its chat not known yet: wait.
                        _ => Ok(false),
                    },
                    // Typed only to an agent that takes input; a session
                    // without one (a shell) is waited for, never typed into.
                    Target::Session => Ok(takes(seen)),
                    // Given apart (`answer_next`).
                    Target::Answer { .. } => Ok(false),
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
                let seen = self.sessions.get(&session);
                let turn = (
                    seen.and_then(|seen| seen.turn),
                    seen.and_then(|seen| seen.conversation.clone()),
                );
                let mid_turn = seen.is_some_and(|seen| seen.works_taking_input(self.guarded));
                // Never typed again from now on, even if this service stops
                // before the daemon answers; followed where it went after a
                // restart.
                self.kept.typing(
                    &id,
                    TypedInto {
                        session: session.as_str().to_owned(),
                        turn: turn.0,
                        conversation: turn.1.clone(),
                        mid_turn,
                    },
                );
                let foreground = seen.and_then(|seen| seen.foreground.clone());
                let delivery = Delivery {
                    reply: id.clone(),
                    target,
                    sent: squash(&text),
                    since: now,
                    typed: false,
                    turn,
                    busy: false,
                    took: false,
                    taken: None,
                    mid_turn,
                    waited: None,
                    free: false,
                };
                self.deliveries
                    .entry(session.clone())
                    .or_default()
                    .push(delivery);
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

    /// The first pending answer to an agent's question that can be decided
    /// now: given to the daemon while its session's agent asks something in
    /// that chat (or, asking nothing, after [`ANSWER_GONE_AFTER`], for the
    /// daemon to say whether the question is still open), or failed. One at a
    /// time; answers don't wait behind replies (which wait while the agent
    /// asks something), and replies don't wait for answers but while one is
    /// being given.
    fn answer_next(&mut self, now: Duration) -> Option<ReplyWork> {
        let pending = &self.pending;
        self.answer_waits.retain(|id, _| pending.contains_key(id));
        if !self.answers || self.answering.is_some() {
            return None;
        }
        let mut ids: Vec<(u64, String)> = self
            .pending
            .iter()
            .filter(|(id, reply)| {
                matches!(reply.target, Target::Answer { .. })
                    && !self.kept.handled(id)
                    && !self.held.contains_key(&reply.session_id)
            })
            .map(|(id, reply)| (reply.seq, id.clone()))
            .collect();
        ids.sort();
        for (_, id) in ids {
            let Some(reply) = self.pending.get(&id) else {
                continue;
            };
            let Target::Answer {
                conversation,
                answer,
            } = &reply.target
            else {
                continue;
            };
            let session = SessionId::from_string(reply.session_id.clone());
            let outcome = match self.sessions.get(&session) {
                None => Err(ReplyFailure::SessionGone),
                Some(seen) if !seen.live => Err(ReplyFailure::SessionGone),
                Some(seen) if seen.private => Err(ReplyFailure::Private),
                // No agent there any more: nothing asks.
                Some(seen) if seen.lifecycle == Lifecycle::None => {
                    Err(ReplyFailure::ConversationChanged)
                }
                Some(seen) => match &seen.conversation {
                    Some(shown) if shown != conversation => Err(ReplyFailure::ConversationChanged),
                    Some(_) if seen.lifecycle == Lifecycle::Blocked => Ok(true),
                    // Asking nothing (yet, or any more), or its chat not known yet.
                    _ => {
                        let since = *self.answer_waits.entry(id.clone()).or_insert(now);
                        Ok(now.saturating_sub(since) >= ANSWER_GONE_AFTER)
                    }
                },
            };
            match outcome {
                Ok(false) => continue,
                Ok(true) => {
                    let (call_id, answers) = (answer.call_id.clone(), answer.answers.clone());
                    self.answer_waits.remove(&id);
                    // Never given again, even if this service stops before
                    // the daemon answers.
                    self.kept.answering(&id);
                    self.answering = Some((id.clone(), session.clone()));
                    return Some(ReplyWork::Answer {
                        reply: id,
                        session,
                        call_id,
                        answers,
                    });
                }
                Err(failure) => {
                    self.answer_waits.remove(&id);
                    self.handle(&id, Some(failure));
                }
            }
        }
        None
    }

    /// The daemon's answer to giving `reply`'s answers to its agent's
    /// question: given (`delivered`), not given as nothing showed the
    /// question (`question_gone`), keys went in but the dialog didn't take
    /// them (`not_taken`), or refused for now while someone typed there
    /// (tried again after [`TYPING_HOLD`]).
    pub fn answer_done(&mut self, reply: String, result: Result<(), ErrorCode>, now: Duration) {
        let Some((id, session)) = self.answering.take_if(|(id, _)| *id == reply) else {
            return;
        };
        let failure = match result {
            Ok(()) => None,
            Err(ErrorCode::SessionInUse) => {
                self.kept.untyped(&id);
                self.held
                    .insert(session.as_str().to_owned(), now + TYPING_HOLD);
                return;
            }
            Err(ErrorCode::QuestionGone) => Some(ReplyFailure::QuestionGone),
            Err(ErrorCode::NotTaken) => Some(ReplyFailure::NotTaken),
            Err(ErrorCode::SessionNotLive | ErrorCode::NotFound) => Some(ReplyFailure::SessionGone),
            Err(_) => Some(ReplyFailure::Refused),
        };
        self.settle(&id, failure, failure.is_none());
    }

    /// Follow again each reply handed to the daemon before this service
    /// started that a read brought again, still pending: as it was typed in
    /// then, into the session, turn and chat it went into, so its agent's
    /// taking it (or not) decides it as before.
    fn resume_restored(&mut self, now: Duration) {
        let read: Vec<String> = self
            .restored
            .keys()
            .filter(|id| self.pending.contains_key(*id))
            .cloned()
            .collect();
        for id in read {
            let (Some(into), Some(reply)) = (self.restored.remove(&id), self.pending.get(&id))
            else {
                continue;
            };
            let session = SessionId::from_string(into.session);
            let mut delivery = Delivery {
                reply: id,
                target: reply.target.clone(),
                sent: squash(&reply.text),
                since: now,
                typed: true,
                turn: (into.turn, into.conversation),
                busy: false,
                took: false,
                taken: None,
                mid_turn: into.mid_turn,
                waited: None,
                free: false,
            };
            // What its session shows now, as if just observed.
            if let Some(seen) = self.sessions.get(&session) {
                delivery.took = delivery.prompted(seen) == Some(true);
                delivery.busy = (delivery.took || !delivery.mid_turn) && seen.busy();
            }
            self.deliveries.entry(session).or_default().push(delivery);
        }
    }

    /// Follow the replies typed in (see [`Delivery`]): one its agent took is
    /// reported delivered, one whose session ended or chat changed first
    /// failed, and a session whose agent took its reply and waits again, or
    /// works on taking what's typed meanwhile, takes the next.
    fn follow_deliveries(&mut self, now: Duration) {
        let mut settled = Vec::new();
        // Replies typed mid-turn that their agent took now, those whose turn
        // ended (taken, and their agent waits again), and those that failed
        // after all, by session.
        let mut took = Vec::new();
        let mut ended = Vec::new();
        let mut gone = Vec::new();
        let sessions = &self.sessions;
        let guarded = self.guarded;
        for (session, deliveries) in &mut self.deliveries {
            let seen = sessions.get(session);
            deliveries.retain_mut(|delivery| {
                if !delivery.typed {
                    // The daemon hasn't answered yet.
                    return true;
                }
                if delivery.taken.is_none() {
                    delivery.follow_turn(seen, now);
                    match delivery.taking(seen, now) {
                        Taking::Waiting => return true,
                        Taking::Taken => {
                            delivery.taken = Some(now);
                            settled.push((delivery.reply.clone(), None));
                            if delivery.mid_turn {
                                took.push((session.clone(), delivery.reply.clone()));
                            }
                        }
                        Taking::Failed(failure) => {
                            settled.push((delivery.reply.clone(), Some(failure)));
                            gone.push((session.clone(), delivery.reply.clone()));
                            return false;
                        }
                    }
                }
                if delivery.done(seen, now) {
                    ended.push((session.clone(), delivery.reply.clone()));
                    return false;
                }
                // Its chat showed it taken, and its agent works on and takes
                // what's typed meanwhile: the next needn't wait for the turn
                // to end (it may never, for an agent kept busy).
                delivery.free |=
                    delivery.took && seen.is_some_and(|seen| seen.works_taking_input(guarded));
                true
            });
        }
        self.deliveries
            .retain(|_, deliveries| !deliveries.is_empty());
        for (reply, failure) in settled {
            self.settle(&reply, failure, true);
        }
        for (session, reply) in took {
            if let Some(awaiting) = self.awaiting.get_mut(&session) {
                for entry in awaiting.iter_mut().filter(|entry| entry.reply == reply) {
                    entry.taken(now);
                }
            }
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
            .flatten()
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

    /// The session a reply handed to the daemon goes into, while the daemon
    /// hasn't answered.
    fn submitting(&self, reply: &str) -> Option<SessionId> {
        self.deliveries
            .iter()
            .find(|(_, deliveries)| {
                deliveries
                    .iter()
                    .any(|delivery| delivery.reply == reply && !delivery.typed)
            })
            .map(|(session, _)| session.clone())
    }

    /// Forget `reply`'s delivery into `session`: the daemon didn't type it.
    fn undeliver(&mut self, session: &SessionId, reply: &str) {
        if let Some(deliveries) = self.deliveries.get_mut(session) {
            deliveries.retain(|delivery| delivery.reply != reply);
            if deliveries.is_empty() {
                self.deliveries.remove(session);
            }
        }
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
                let Some(delivery) = self.deliveries.get_mut(&session).and_then(|deliveries| {
                    deliveries
                        .iter_mut()
                        .find(|delivery| delivery.reply == reply)
                }) else {
                    return;
                };
                delivery.typed = true;
                let untaken = delivery.mid_turn;
                // A reply is answered in its chat; a session message in the
                // chat the session shows now, or any when it doesn't say.
                let conversation = match &delivery.target {
                    Target::Chat(conversation) | Target::Answer { conversation, .. } => {
                        Some(conversation.clone())
                    }
                    Target::Session => self
                        .sessions
                        .get(&session)
                        .and_then(|seen| seen.conversation.clone()),
                };
                let awaiting = self.awaiting.entry(session).or_default();
                // Typed to a waiting agent: the turns before ended. Typed
                // mid-turn, the turn of the one before may still go on.
                if !untaken {
                    for earlier in awaiting.iter_mut() {
                        earlier.ended.get_or_insert((now_ms, now));
                    }
                }
                awaiting.push_back(Awaiting {
                    reply,
                    conversation,
                    since_ms: now_ms,
                    since: now,
                    untaken,
                    ended: None,
                });
            }
            Err(_) => {
                self.undeliver(&session, &reply);
                self.handle(&reply, Some(ReplyFailure::Refused));
            }
        }
    }

    /// The daemon's answer to typing `reply` in: typed in (`true`), refused
    /// for now (someone typing there, another program in its foreground, its
    /// agent asking something: tried again after [`TYPING_HOLD`]), or
    /// refused for good.
    pub fn daemon_answered(
        &mut self,
        reply: String,
        result: Result<(), ErrorCode>,
        now: Duration,
        now_ms: i64,
    ) -> bool {
        match result {
            Err(ErrorCode::SessionInUse) => self.typing(reply, now),
            Err(ErrorCode::ForegroundChanged) => self.foreground_changed(reply, now),
            Err(ErrorCode::SessionBlocked) => self.asking(reply, now),
            result => {
                let typed = result.is_ok();
                self.submitted(reply, result, now, now_ms);
                return typed;
            }
        }
        false
    }

    /// The daemon refused it because someone was typing in its session: it
    /// stays pending, and that session is tried again after [`TYPING_HOLD`].
    pub fn typing(&mut self, reply: String, now: Duration) {
        self.hold_back(reply, now);
    }

    /// The daemon refused it because its session's agent asks something (a
    /// permission prompt, a question or a menu, which would take it as its
    /// answer), though its view said otherwise: it stays pending, and that
    /// session is tried again after [`TYPING_HOLD`], once it takes input.
    pub fn asking(&mut self, reply: String, now: Duration) {
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
            self.undeliver(&session, &reply);
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
    /// conversation, the latest typed in (typed mid-turn: taken) before its
    /// turn finished, unless that one's turn had been seen to end before (see
    /// [`Awaiting`]). The replies typed in before it lost their answers: of
    /// several an agent took within one turn, the last one taken before the
    /// answer was said has it, so `reply_to` names one reply, never one taken
    /// after its answer was written.
    pub fn answered(&mut self, answer: &AgentReply) -> Option<String> {
        let awaiting = self.awaiting.get_mut(&answer.session_id)?;
        let at = awaiting.iter().rposition(|entry| {
            let same_chat = entry
                .conversation
                .as_deref()
                .is_none_or(|conversation| answer.conversation_id.as_deref() == Some(conversation));
            same_chat && !entry.untaken && entry.since_ms <= answer.finished_at
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
                if !read.more {
                    self.forget_restored();
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

    /// Reads caught up: a reply handed to the daemon before a restart that
    /// no read brought as pending isn't any more (expired), and its session
    /// no longer waits for it.
    fn forget_restored(&mut self) {
        let gone: Vec<String> = self
            .restored
            .keys()
            .filter(|id| !self.pending.contains_key(*id))
            .cloned()
            .collect();
        for id in gone {
            self.restored.remove(&id);
            self.kept.no_longer_pending(&id);
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
        // One handed to the daemon before a restart is pending again only to
        // be followed (`resume_restored`), never typed again.
        let typable = !self.kept.handled(id) || self.restored.contains_key(id);
        if mine && status == ReplyStatus::Pending && typable {
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
