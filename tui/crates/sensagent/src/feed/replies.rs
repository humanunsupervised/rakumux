//! Replies written back to feed entries (`contracts/feed/` `reply`): while the
//! account's Feed switch is on, the status service reads the account's
//! replies as they change (every few seconds), keeps them for TUIs' threads,
//! and types the pending ones addressed to this machine into their sessions
//! (`session_submit`: a paste, then Enter, the same for every agent), then
//! says so. It delivers only to the entry's own session, while it runs, isn't
//! private, still shows the entry's conversation and its agent waits for
//! input; a busy or blocked agent is waited for (the cloud expires a reply
//! after an hour). A reply written on rakumux.com or another machine is
//! typed only when this machine's user turned that on in Settings; one
//! written in this machine's TUI always is. The agent's next reply in that
//! session is uploaded as the reply's answer (`reply_to`). Driven by the owner
//! thread with its own clock.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use session_core::api::{AgentReply, ErrorCode};
use session_core::ids::SessionId;
use session_core::model::{Lifecycle, SessionStatus, SessionView};

use super::cache::{FeedReply, ReplyFailure, ReplyStatus};
use super::job::FeedResult;
use super::wire::{self, FeedFailure, FeedRequest, RepliesRead};

/// How often the account's replies are read while the feed is on, unless
/// rakumux.com's answer says otherwise (`poll_ms`: every second while a reply
/// waits for this machine or the website is open, less often otherwise).
const READ_EVERY: Duration = Duration::from_secs(5);
/// The first and longest waits after a failed request.
const RETRY_MIN: Duration = Duration::from_secs(5);
const RETRY_MAX: Duration = Duration::from_secs(300);
/// How long the "replies from elsewhere" setting is trusted before it's read again.
const SETTING_EVERY: Duration = Duration::from_secs(10);
/// Replies remembered as handled here, so a stale pending one isn't typed twice.
const MAX_HANDLED: usize = 1_000;

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
        let status = match &outcome {
            Ok(_) => ReplyWriteStatus::Sent,
            Err(FeedFailure::Unauthorized) => ReplyWriteStatus::SignedOut,
            Err(FeedFailure::Off) => ReplyWriteStatus::Off,
            Err(FeedFailure::Invalid) => ReplyWriteStatus::Invalid,
            Err(FeedFailure::RateLimited(_) | FeedFailure::Unavailable(_)) => {
                ReplyWriteStatus::Unavailable
            }
        };
        Self {
            status,
            reply: outcome.ok(),
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
}

/// The request out to rakumux.com.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Out {
    Read,
    Report,
}

/// What a session shows, for deciding whether a reply can be typed into it.
struct Seen {
    live: bool,
    private: bool,
    lifecycle: Lifecycle,
    conversation: Option<String>,
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
        }
    }
}

/// A delivered reply waiting for the agent's answer.
struct Awaiting {
    reply: String,
    conversation: String,
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
    /// The account's newest reply seq read.
    cursor: u64,
    /// The read or report out, if one is.
    out: Option<Out>,
    next_read: Duration,
    retry_at: Duration,
    retry: Duration,
    /// This machine's pending replies, by id, as last read.
    pending: HashMap<String, FeedReply>,
    /// Typed (or refused) here, to tell rakumux.com: the id and why it failed.
    reports: VecDeque<(String, Option<ReplyFailure>)>,
    /// Being typed now.
    submitting: HashSet<String>,
    handled: HashSet<String>,
    handled_order: VecDeque<String>,
    awaiting: HashMap<SessionId, Awaiting>,
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
            retry_at: Duration::ZERO,
            retry: RETRY_MIN,
            pending: HashMap::new(),
            reports: VecDeque::new(),
            submitting: HashSet::new(),
            handled: HashSet::new(),
            handled_order: VecDeque::new(),
            awaiting: HashMap::new(),
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
        self.pending.clear();
        self.reports.clear();
        self.awaiting.clear();
        self.retry = RETRY_MIN;
        self.retry_at = now;
        self.next_read = now;
    }

    /// The daemon (re)connected: these are its sessions.
    pub fn connected(&mut self, views: &[SessionView]) {
        self.sessions = views
            .iter()
            .map(|view| (view.id.clone(), Seen::of(view)))
            .collect();
    }

    pub fn observe(&mut self, view: &SessionView) {
        self.sessions.insert(view.id.clone(), Seen::of(view));
    }

    pub fn forget(&mut self, session: &SessionId) {
        self.sessions.remove(session);
        self.awaiting.remove(session);
    }

    /// A reply written in this machine's TUI was stored: typed here at once
    /// when its session runs here.
    pub fn written(&mut self, reply: &FeedReply) {
        self.take(reply);
    }

    /// The next thing to do, if anything is due: type what can be (deciding
    /// what can't), report it, then read.
    pub fn next_work(&mut self, now: Duration) -> Option<ReplyWork> {
        if !self.enabled {
            return None;
        }
        if let Some(work) = self.deliver(now) {
            return Some(work);
        }
        let free = self.out.is_none() && now >= self.retry_at;
        while free {
            let Some((id, failure)) = self.reports.front() else {
                break;
            };
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
        if free && now >= self.next_read {
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
        let mut ids: Vec<&String> = self
            .pending
            .keys()
            .filter(|id| !self.submitting.contains(*id) && !self.handled.contains(*id))
            .collect();
        ids.sort();
        let mut decided = None;
        for id in ids {
            let reply = &self.pending[id];
            let elsewhere = reply.from.as_deref() != Some(self.machine.as_str());
            let session = SessionId::from_string(reply.session_id.clone());
            let outcome = match self.sessions.get(&session) {
                _ if elsewhere && !remote => Err(ReplyFailure::NotAccepted),
                None => Err(ReplyFailure::SessionGone),
                Some(seen) if !seen.live => Err(ReplyFailure::SessionGone),
                Some(seen) if seen.private => Err(ReplyFailure::Private),
                // No agent there any more: not that chat.
                Some(seen) if seen.lifecycle == Lifecycle::None => {
                    Err(ReplyFailure::ConversationChanged)
                }
                Some(seen) => match &seen.conversation {
                    Some(conversation) if *conversation != reply.conversation_id => {
                        Err(ReplyFailure::ConversationChanged)
                    }
                    // Its agent waits for input: type it.
                    Some(_) if matches!(seen.lifecycle, Lifecycle::Idle | Lifecycle::Done) => {
                        Ok(true)
                    }
                    // Busy, blocked on a question, or its chat not known yet: wait.
                    _ => Ok(false),
                },
            };
            match outcome {
                Ok(false) => continue,
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
                let reply = self.pending.get(&id)?;
                self.submitting.insert(id.clone());
                Some(ReplyWork::Submit {
                    reply: id,
                    session,
                    text: reply.text.clone(),
                })
            }
            None => {
                self.handle(&id, failure);
                // Decided without typing: look again (and report) at once.
                self.deliver(now)
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

    /// Typed into its session (or the daemon refused it): rakumux.com hears
    /// next, and the agent's next reply there answers it.
    pub fn submitted(&mut self, reply: String, result: Result<(), ErrorCode>, now_ms: i64) {
        self.submitting.remove(&reply);
        let Some(pending) = self.pending.get(&reply) else {
            return;
        };
        match result {
            Ok(()) => {
                let session = SessionId::from_string(pending.session_id.clone());
                // Its agent has input now: nothing else is typed there until
                // the session says it waits again.
                if let Some(seen) = self.sessions.get_mut(&session) {
                    seen.lifecycle = Lifecycle::Working;
                }
                self.awaiting.insert(
                    session,
                    Awaiting {
                        reply: reply.clone(),
                        conversation: pending.conversation_id.clone(),
                        since_ms: now_ms,
                    },
                );
                self.handle(&reply, None);
            }
            Err(_) => self.handle(&reply, Some(ReplyFailure::Refused)),
        }
    }

    fn handle(&mut self, id: &str, failure: Option<ReplyFailure>) {
        self.pending.remove(id);
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
        let same_chat = answer.conversation_id.as_deref() == Some(awaiting.conversation.as_str());
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
            (Some(Out::Report), Ok(FeedResult::Reply(reply))) => {
                self.reported(current, Ok(reply), now).into_iter().collect()
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
                self.retry = RETRY_MIN;
                for reply in &read.replies {
                    self.cursor = self.cursor.max(reply.seq);
                    self.take(reply);
                }
                self.cursor = self.cursor.max(read.latest_seq);
                self.next_read = if read.more {
                    now
                } else {
                    now + read.poll.unwrap_or(READ_EVERY)
                };
                read.replies
            }
            Err(failure) => {
                self.fail(failure, now);
                Vec::new()
            }
        }
    }

    /// Hold `reply` for delivery when it's pending here, else drop it from
    /// what's pending.
    fn take(&mut self, reply: &FeedReply) {
        let mine = reply.machine == self.machine;
        if mine && reply.status == ReplyStatus::Pending && !self.handled.contains(&reply.id) {
            self.pending.insert(reply.id.clone(), reply.clone());
        } else {
            self.pending.remove(&reply.id);
        }
    }

    /// A status report finished. Returns the stored reply, for TUIs.
    fn reported(
        &mut self,
        current: bool,
        outcome: Result<FeedReply, FeedFailure>,
        now: Duration,
    ) -> Option<FeedReply> {
        self.out = None;
        if !current || !self.enabled {
            return None;
        }
        match outcome {
            Ok(reply) => {
                self.retry = RETRY_MIN;
                self.reports.pop_front();
                Some(reply)
            }
            // A report the cloud refuses is dropped, not retried forever.
            Err(FeedFailure::Invalid) => {
                self.reports.pop_front();
                None
            }
            Err(failure) => {
                self.fail(failure, now);
                None
            }
        }
    }

    fn fail(&mut self, failure: FeedFailure, now: Duration) {
        let wait = match failure {
            FeedFailure::RateLimited(Some(delay)) | FeedFailure::Unavailable(Some(delay)) => delay,
            _ => self.retry,
        };
        self.retry = self.retry.saturating_mul(2).min(RETRY_MAX);
        self.retry_at = now + wait.min(Duration::from_secs(86_400));
    }
}

#[cfg(test)]
mod tests;
