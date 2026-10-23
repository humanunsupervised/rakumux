use std::cmp::Reverse;
use std::collections::{HashMap, HashSet, VecDeque};
use std::fmt;
use std::time::Duration;

use session_core::ids::SessionId;

use crate::source::Candidate;
use crate::status::SourceStamp;

/// At most one capture per session in this interval: a normal refresh a minute.
pub const SESSION_INTERVAL: Duration = Duration::from_secs(60);
/// A session whose read or request failed waits this long before another attempt,
/// doubling with each further failure in a row up to `MAX_ERROR_BACKOFF`.
pub const ERROR_BACKOFF: Duration = Duration::from_secs(60);
/// The longest wait after repeated failures.
pub const MAX_ERROR_BACKOFF: Duration = Duration::from_secs(16 * 60);
/// A busy session is refreshed once its output pauses this long, or after
/// `SESSION_INTERVAL` of continuous output, whichever comes first.
pub const QUIET_PERIOD: Duration = Duration::from_secs(5);
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
    /// can't handle, or the service rejected it). Retried only once the session changes.
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
    /// Canceled or changed before a result: considered again after the interval.
    Interrupted,
    Failed(RefreshFailure),
    /// Rate limited, with the service's suggested delay.
    RateLimited(Option<Duration>),
}

/// A session to refresh now; advance its stamp first when output is pending.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct Due {
    pub(crate) session: SessionId,
    pub(crate) advance: bool,
}

#[derive(Default)]
struct Timing {
    last_start: Option<Duration>,
    retry_at: Option<Duration>,
    /// Attempted or decided at this stamp: nothing to do until it changes.
    settled: Option<SourceStamp>,
    /// The decision doesn't depend on output (an unselected or reports-only folder).
    sticky: bool,
    output_at: Option<Duration>,
    dirty_at: Option<Duration>,
    failure: Option<RefreshFailure>,
    /// Unreadable or unavailable attempts in a row, for the error backoff.
    failures: u32,
}

impl Timing {
    /// Consider `stamp` again: the attempt at it didn't decide anything.
    fn unsettle(&mut self, stamp: SourceStamp) {
        if self.settled == Some(stamp) {
            self.settled = None;
        }
    }
}

/// When to refresh which session: the per-session interval, error backoff, quiet
/// period, global start budget and pauses. Pure state over an injected monotonic
/// `now`; the dispatch coordinator's two slots bound concurrency.
///
/// Sessions whose stamp changed at once (lifecycle, report, privacy, new terminal)
/// come first, then sessions with pending output; within each, the one refreshed
/// longest ago. A busy terminal is refreshed at most once per interval, without
/// waiting for it to become quiet.
#[derive(Default)]
pub(crate) struct RefreshScheduler {
    sessions: HashMap<SessionId, Timing>,
    starts: VecDeque<Duration>,
    paused_until: Option<Duration>,
    unauthorized: bool,
}

impl RefreshScheduler {
    pub fn new() -> Self {
        Self::default()
    }

    /// New output or an activity change left `session` dirty at `now`.
    pub fn output(&mut self, session: &SessionId, now: Duration) {
        let timing = self.sessions.entry(session.clone()).or_default();
        timing.dirty_at.get_or_insert(now);
        timing.output_at = Some(now);
    }

    /// Sessions due now, most urgent first. Sessions in `running` are skipped.
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
            if !candidate.dirty {
                timing.output_at = None;
                timing.dirty_at = None;
            }
            let urgent = timing.settled != Some(candidate.stamp);
            if !urgent {
                let quiet = timing
                    .output_at
                    .is_none_or(|at| now >= at.saturating_add(QUIET_PERIOD));
                let waited = timing
                    .dirty_at
                    .is_none_or(|at| now >= at.saturating_add(SESSION_INTERVAL));
                if !candidate.dirty || timing.sticky || !(quiet || waited) {
                    continue;
                }
            }
            // A finished turn is described at once; backoff and the global budget
            // still apply.
            let immediate = urgent && candidate.finished;
            let waiting = timing.retry_at.is_some_and(|at| now < at)
                || (!immediate
                    && timing
                        .last_start
                        .is_some_and(|at| now < at.saturating_add(SESSION_INTERVAL)));
            if waiting {
                continue;
            }
            due.push((Reverse(urgent), timing.last_start, candidate));
        }
        due.sort_by(|(urgent, start, candidate), (other, other_start, next)| {
            (urgent, start, candidate.session.as_str()).cmp(&(
                other,
                other_start,
                next.session.as_str(),
            ))
        });
        due.into_iter()
            .map(|(_, _, candidate)| Due {
                session: candidate.session,
                advance: candidate.dirty,
            })
            .collect()
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

    /// Pending output was folded into a new stamp.
    pub fn advanced(&mut self, session: &SessionId) {
        if let Some(timing) = self.sessions.get_mut(session) {
            timing.output_at = None;
            timing.dirty_at = None;
        }
    }

    /// A capture of `stamp` started. A `sticky` job ignores later output (a
    /// reports-only job).
    pub fn started(
        &mut self,
        session: &SessionId,
        stamp: SourceStamp,
        sticky: bool,
        now: Duration,
    ) {
        let timing = self.sessions.entry(session.clone()).or_default();
        timing.last_start = Some(now);
        timing.settled = Some(stamp);
        timing.sticky = sticky;
        timing.output_at = None;
        timing.dirty_at = None;
        self.starts.push_back(now);
    }

    /// The policy declined `stamp` without a job (cached, no context, excluded, …).
    /// A `sticky` decision ignores later output until the stamp itself changes.
    pub fn settle(&mut self, session: &SessionId, stamp: SourceStamp, sticky: bool) {
        let timing = self.sessions.entry(session.clone()).or_default();
        timing.settled = Some(stamp);
        timing.sticky = sticky;
    }

    /// An attempt at `stamp` ended. A session removed meanwhile is not tracked again,
    /// but a rate limit or refusal still applies to every session.
    pub fn finished(
        &mut self,
        session: &SessionId,
        stamp: SourceStamp,
        outcome: Outcome,
        now: Duration,
    ) {
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
        match outcome {
            Outcome::Done => {
                timing.failure = None;
                timing.failures = 0;
            }
            Outcome::Interrupted => timing.unsettle(stamp),
            Outcome::Failed(RefreshFailure::Rejected) => {
                timing.failure = Some(RefreshFailure::Rejected);
                timing.settled = Some(stamp);
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
                timing.unsettle(stamp);
            }
            Outcome::Failed(failure) => {
                timing.failure = Some(failure);
                timing.unsettle(stamp);
            }
            Outcome::RateLimited(_) => {
                timing.failure = Some(RefreshFailure::RateLimited);
                timing.unsettle(stamp);
            }
        }
    }

    /// The daemon deleted `session`.
    pub fn forget(&mut self, session: &SessionId) {
        self.sessions.remove(session);
    }

    /// Account or consent changed: every decision and failure is reconsidered. Per-
    /// session intervals, error backoff, the start budget and a rate-limit pause remain.
    pub fn reset(&mut self) {
        self.unauthorized = false;
        for timing in self.sessions.values_mut() {
            timing.settled = None;
            timing.sticky = false;
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
