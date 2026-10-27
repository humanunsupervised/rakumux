use std::collections::{HashMap, HashSet, VecDeque};
use std::fmt;
use std::time::Duration;

use session_core::ids::SessionId;
use session_core::model::AgentTurn;

use crate::source::Candidate;

/// The agent's messages after which a chat's one automatic summary is made, when
/// its turn hasn't finished sooner, unless the turn settings say otherwise
/// (`TurnSettings::every`).
pub const AUTO_STEPS: u32 = 5;
/// A session whose read or request failed waits this long before another attempt,
/// doubling with each further failure in a row up to `MAX_ERROR_BACKOFF`.
pub const ERROR_BACKOFF: Duration = Duration::from_secs(60);
/// The longest wait after repeated failures.
pub const MAX_ERROR_BACKOFF: Duration = Duration::from_secs(16 * 60);
/// A summary interrupted by a change of its session is tried again after this.
pub const INTERRUPTED_RETRY: Duration = Duration::from_secs(5);
/// Captures started across all sessions in any `BUDGET_WINDOW`.
pub const BUDGET_STARTS: usize = 12;
/// The window `BUDGET_STARTS` applies to.
pub const BUDGET_WINDOW: Duration = Duration::from_secs(60);
/// Jobs in flight at once: the dispatch coordinator's slot limit.
pub const MAX_RUNNING: usize = 2;

/// Why a session's last refresh produced no text. Static; never daemon or service text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RefreshFailure {
    /// The daemon refused or could not serve the read. Retried after the error backoff.
    Unreadable,
    /// This input can't be sent (for example an invisible character the redaction
    /// can't handle, or the service rejected it). Not retried until asked again.
    Rejected,
    /// The network or service failed. Retried after the error backoff.
    Unavailable,
    /// The service asked for fewer requests. Every session pauses.
    RateLimited,
    /// The service or local credentials refused the account. Every session stops until
    /// the account or consent changes.
    Unauthorized,
}

/// How one attempt ended, for the scheduler.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Outcome {
    /// Text was cached, or the capture held no meaningful context.
    Done,
    /// Canceled or changed before a result: tried again shortly.
    Interrupted,
    Failed(RefreshFailure),
    /// Rate limited, with the service's suggested delay.
    RateLimited(Option<Duration>),
}

/// A session to summarise now: asked for, or its chat's automatic summary
/// (the conversation it's for).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Due {
    pub(crate) session: SessionId,
    pub(crate) auto: Option<String>,
}

#[derive(Default)]
struct Timing {
    retry_at: Option<Duration>,
    failure: Option<RefreshFailure>,
    /// Unreadable or unavailable attempts in a row, for the error backoff.
    failures: u32,
    /// A summary was asked for (the refresh icon or `s`) and hasn't started.
    requested: bool,
    /// The running job was asked for: its title replaces any name.
    asked: bool,
    /// The running job is this conversation's automatic summary.
    auto: Option<String>,
    /// Conversations whose automatic summary was made: one per chat.
    summarized: HashSet<String>,
}

/// When to summarise which session: once per chat, at its agent's
/// [`AUTO_STEPS`]th output or when it finishes sooner, and whenever the user
/// asks; never on a timer. Error backoff, a rate-limit pause and a global start
/// budget still apply. Pure state over an injected monotonic `now`; the dispatch
/// coordinator's two slots bound concurrency.
#[derive(Default)]
pub(crate) struct RefreshScheduler {
    sessions: HashMap<SessionId, Timing>,
    starts: VecDeque<Duration>,
    paused_until: Option<Duration>,
    unauthorized: bool,
    /// The messages after which a chat's automatic summary is made.
    auto_at: u32,
}

/// The conversation a turn belongs to; one with no known id is one chat.
pub(crate) fn conversation(turn: &AgentTurn) -> String {
    turn.conversation_id.clone().unwrap_or_default()
}

impl RefreshScheduler {
    pub fn new() -> Self {
        Self {
            auto_at: AUTO_STEPS,
            ..Self::default()
        }
    }

    /// Make chats' automatic summaries after `messages` of their agent's (at
    /// least one).
    pub fn set_auto_at(&mut self, messages: u32) {
        self.auto_at = messages.max(1);
    }

    /// The user asked for a summary of `session`: it's due at once, even
    /// during an error backoff.
    pub fn request(&mut self, session: &SessionId) {
        let timing = self.sessions.entry(session.clone()).or_default();
        timing.requested = true;
        timing.retry_at = None;
    }

    /// Whether a summary was asked for and hasn't started.
    pub fn requested(&self, session: &SessionId) -> bool {
        self.sessions
            .get(session)
            .is_some_and(|timing| timing.requested)
    }

    /// Whether the job running for `session` was asked for (the refresh icon
    /// or `s`), rather than its chat's automatic one.
    pub fn asked(&self, session: &SessionId) -> bool {
        self.sessions
            .get(session)
            .is_some_and(|timing| timing.asked)
    }

    /// `conversation` of `session` has a summary already (one made before this
    /// service started): it gets no automatic one.
    pub fn summarized(&mut self, session: &SessionId, conversation: String) {
        let timing = self.sessions.entry(session.clone()).or_default();
        timing.summarized.insert(conversation);
    }

    /// Sessions due now, asked-for ones first. Sessions in `running` are skipped.
    pub fn plan(
        &mut self,
        now: Duration,
        candidates: Vec<Candidate>,
        running: &HashSet<SessionId>,
    ) -> Vec<Due> {
        if self.halted(now) {
            return Vec::new();
        }
        let mut due = Vec::new();
        for candidate in candidates {
            if running.contains(&candidate.session) {
                continue;
            }
            let timing = self.sessions.entry(candidate.session.clone()).or_default();
            let auto = candidate.turn.as_ref().and_then(|turn| {
                let conversation = conversation(turn);
                let ready = turn.messages >= self.auto_at || candidate.finished;
                (ready && !timing.summarized.contains(&conversation)).then_some(conversation)
            });
            if !timing.requested && auto.is_none() {
                continue;
            }
            if timing.retry_at.is_some_and(|at| now < at) {
                continue;
            }
            due.push(Due {
                session: candidate.session,
                auto,
            });
        }
        due.sort_by(|due, other| {
            (!self.requested(&due.session), due.session.as_str())
                .cmp(&(!self.requested(&other.session), other.session.as_str()))
        });
        due
    }

    /// Whether another capture may start within the global budget.
    pub fn has_budget(&mut self, now: Duration) -> bool {
        while self
            .starts
            .front()
            .is_some_and(|at| now >= at.saturating_add(BUDGET_WINDOW))
        {
            self.starts.pop_front();
        }
        self.starts.len() < BUDGET_STARTS
    }

    /// A summary of `due` started.
    pub fn started(&mut self, due: &Due, now: Duration) {
        let timing = self.sessions.entry(due.session.clone()).or_default();
        timing.asked = timing.requested;
        timing.requested = false;
        timing.auto = due.auto.clone();
        self.starts.push_back(now);
    }

    /// The policy declined `due` without a job (excluded, no context, …): the
    /// request is answered, and an automatic summary isn't tried again.
    pub fn settle(&mut self, due: &Due) {
        let timing = self.sessions.entry(due.session.clone()).or_default();
        timing.requested = false;
        if let Some(conversation) = &due.auto {
            timing.summarized.insert(conversation.clone());
        }
    }

    /// An attempt ended. A session removed meanwhile is not tracked again, but a
    /// rate limit or refusal still applies to every session.
    pub fn finished(&mut self, session: &SessionId, outcome: Outcome, now: Duration) {
        match outcome {
            Outcome::Failed(RefreshFailure::Unauthorized) => self.unauthorized = true,
            Outcome::RateLimited(after) => {
                let until = now.saturating_add(after.unwrap_or_default().max(ERROR_BACKOFF));
                self.paused_until = Some(self.paused_until.map_or(until, |at| at.max(until)));
            }
            _ => {}
        }
        let Some(timing) = self.sessions.get_mut(session) else {
            return;
        };
        let auto = timing.auto.take();
        timing.asked = false;
        match outcome {
            Outcome::Done => {
                timing.failure = None;
                timing.failures = 0;
                timing.summarized.extend(auto);
            }
            Outcome::Interrupted => {
                timing.retry_at = Some(now.saturating_add(INTERRUPTED_RETRY));
            }
            Outcome::Failed(RefreshFailure::Rejected) => {
                // Can't be sent: not tried again unless asked.
                timing.failure = Some(RefreshFailure::Rejected);
                timing.summarized.extend(auto);
            }
            Outcome::Failed(
                failure @ (RefreshFailure::Unreadable | RefreshFailure::Unavailable),
            ) => {
                timing.failure = Some(failure);
                timing.failures = timing.failures.saturating_add(1);
                let doublings = timing.failures.saturating_sub(1).min(4);
                let backoff = ERROR_BACKOFF
                    .saturating_mul(1 << doublings)
                    .min(MAX_ERROR_BACKOFF);
                timing.retry_at = Some(now.saturating_add(backoff));
            }
            Outcome::Failed(failure) => timing.failure = Some(failure),
            Outcome::RateLimited(_) => timing.failure = Some(RefreshFailure::RateLimited),
        }
    }

    /// Sharing stopped and its text was dropped (turned off, signed out): every
    /// chat gets its automatic summary again once sharing resumes.
    pub fn forget_summaries(&mut self) {
        for timing in self.sessions.values_mut() {
            timing.summarized.clear();
        }
    }

    /// The daemon deleted `session`.
    pub fn forget(&mut self, session: &SessionId) {
        self.sessions.remove(session);
    }

    /// Account or consent changed: failures and a refusal are reconsidered.
    /// Which chats had their automatic summary, error backoff, the start budget
    /// and a rate-limit pause remain.
    pub fn reset(&mut self) {
        self.unauthorized = false;
        for timing in self.sessions.values_mut() {
            timing.failure = None;
        }
    }

    pub fn failure(&self, session: &SessionId) -> Option<RefreshFailure> {
        self.sessions.get(session).and_then(|timing| timing.failure)
    }

    /// Nothing may be sent: the service's rate limit or an account refusal holds.
    pub fn halted(&self, now: Duration) -> bool {
        self.unauthorized || self.rate_limited(now)
    }

    pub fn rate_limited(&self, now: Duration) -> bool {
        self.paused_until.is_some_and(|until| now < until)
    }

    pub fn unauthorized(&self) -> bool {
        self.unauthorized
    }
}

impl fmt::Debug for RefreshScheduler {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RefreshScheduler")
            .field("sessions", &self.sessions.len())
            .field("starts", &self.starts.len())
            .field("unauthorized", &self.unauthorized)
            .finish()
    }
}

#[cfg(test)]
mod tests;
