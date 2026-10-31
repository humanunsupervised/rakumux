use std::collections::{HashMap, HashSet, VecDeque};
use std::fmt;
use std::time::Duration;

use session_core::ids::SessionId;
use session_core::model::AgentTurn;

use crate::source::Candidate;

/// The agent's messages after which a session's one automatic summary is made,
/// in the chat it shows then, when its turn hasn't finished sooner, unless the
/// turn settings say otherwise (`TurnSettings::every`).
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

/// A session to summarise now: asked for, or its one automatic summary.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Due {
    pub(crate) session: SessionId,
    /// The chat (agent conversation) it shows: see [`conversation`]; `""`
    /// for a program whose turns the daemon doesn't follow.
    pub(crate) chat: String,
    /// The session's automatic summary, not only one asked for.
    pub(crate) auto: bool,
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
    /// The running job is the session's automatic summary.
    auto: bool,
    /// The chat the running job is of.
    chat: String,
    /// The session had its one automatic summary, or one asked for was made:
    /// it gets no automatic one again.
    summarized: bool,
}

/// When to summarise which session: once in its life, in the first chat whose
/// agent reaches its [`AUTO_STEPS`]th output or finishes sooner, and whenever
/// the user asks (a summary asked for that's made counts as that one too);
/// never on a timer. Error backoff, a rate-limit pause and a global start
/// budget still apply. Pure state over an injected monotonic `now`; the dispatch
/// coordinator's two slots bound concurrency.
#[derive(Default)]
pub(crate) struct RefreshScheduler {
    sessions: HashMap<SessionId, Timing>,
    starts: VecDeque<Duration>,
    paused_until: Option<Duration>,
    unauthorized: bool,
    /// The messages after which a session's automatic summary is made.
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

    /// Make sessions' automatic summaries after `messages` of their agent's
    /// (at least one).
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
    /// or `s`), rather than its automatic one.
    pub fn asked(&self, session: &SessionId) -> bool {
        self.sessions
            .get(session)
            .is_some_and(|timing| timing.asked)
    }

    /// `session` has had its summary already (made before this service
    /// started): it gets no automatic one.
    pub fn summarized(&mut self, session: &SessionId) {
        self.sessions.entry(session.clone()).or_default().summarized = true;
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
            let ready = candidate
                .turn
                .as_ref()
                .is_some_and(|turn| turn.messages >= self.auto_at || candidate.finished);
            let auto = ready && !timing.summarized;
            if !timing.requested && !auto {
                continue;
            }
            if timing.retry_at.is_some_and(|at| now < at) {
                continue;
            }
            due.push(Due {
                chat: candidate
                    .turn
                    .as_ref()
                    .map(conversation)
                    .unwrap_or_default(),
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
        timing.auto = due.auto;
        timing.chat = due.chat.clone();
        self.starts.push_back(now);
    }

    /// The policy declined `due` without a job (excluded, no context, …): the
    /// request is answered, and an automatic summary isn't tried again.
    /// Returns the chat of the session's automatic summary that settles, if any.
    pub fn settle(&mut self, due: &Due) -> Option<String> {
        let timing = self.sessions.entry(due.session.clone()).or_default();
        timing.requested = false;
        if !due.auto {
            return None;
        }
        timing.summarized = true;
        Some(due.chat.clone())
    }

    /// An attempt ended. A session removed meanwhile is not tracked again, but a
    /// rate limit or refusal still applies to every session. Returns the chat
    /// of the session's one summary when it settles now: one made (automatic or
    /// asked for), or an automatic one not to be tried again.
    pub fn finished(
        &mut self,
        session: &SessionId,
        outcome: Outcome,
        now: Duration,
    ) -> Option<String> {
        match outcome {
            Outcome::Failed(RefreshFailure::Unauthorized) => self.unauthorized = true,
            Outcome::RateLimited(after) => {
                let until = now.saturating_add(after.unwrap_or_default().max(ERROR_BACKOFF));
                self.paused_until = Some(self.paused_until.map_or(until, |at| at.max(until)));
            }
            _ => {}
        }
        let timing = self.sessions.get_mut(session)?;
        let auto = std::mem::take(&mut timing.auto);
        let chat = std::mem::take(&mut timing.chat);
        let asked = std::mem::take(&mut timing.asked);
        match outcome {
            Outcome::Done => {
                timing.failure = None;
                timing.failures = 0;
                // Automatic or asked for, it's the session's summary: no
                // later chat gets an automatic one.
                timing.summarized = true;
                return Some(chat);
            }
            Outcome::Interrupted => {
                timing.retry_at = Some(now.saturating_add(INTERRUPTED_RETRY));
                // Asked for and not made (the user typed the next prompt
                // meanwhile, say): still asked for, of what the session shows then.
                timing.requested |= asked;
            }
            Outcome::Failed(RefreshFailure::Rejected) => {
                // Can't be sent: not tried again unless asked.
                timing.failure = Some(RefreshFailure::Rejected);
                timing.summarized |= auto;
                return auto.then_some(chat);
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
        None
    }

    /// Sharing stopped and its text was dropped (turned off, signed out): every
    /// session gets its automatic summary again once sharing resumes.
    pub fn forget_summaries(&mut self) {
        for timing in self.sessions.values_mut() {
            timing.summarized = false;
        }
    }

    /// The daemon deleted `session`.
    pub fn forget(&mut self, session: &SessionId) {
        self.sessions.remove(session);
    }

    /// Account or consent changed: failures and a refusal are reconsidered.
    /// Which sessions had their summary, error backoff, the start budget and a
    /// rate-limit pause remain.
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
