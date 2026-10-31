use std::collections::{HashMap, HashSet};
use std::fmt;

use session_core::activity::ActivityPhase;
use session_core::api::{EventEnvelope, SessionEvent};
use session_core::ids::SessionId;
use session_core::model::{AgentTurn, Lifecycle, Reported, ScopeRoot, SessionStatus, SessionView};
use thiserror::Error;

use super::capture::{report_text, CaptureError, CaptureExpectation, SessionCapture};
use crate::dispatch::StatusCaptureJob;
use crate::status::model::{validate_folder, validate_session};
use crate::status::{ContextAvailability, SourceStamp, StatusSource, MAX_CACHE_ENTRIES};

/// Static tracker failures; never session names, folders or terminal text.
#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum SourceError {
    /// The status policy's bound. Sessions are never silently forgotten to make room.
    #[error("the local AI-status source limit was reached")]
    Capacity,
    /// Generations and revisions never wrap.
    #[error("local AI-status source revisions are exhausted")]
    Exhausted,
    /// A tracked session's new folder or id fails the status policy's checks.
    #[error("invalid local session observation for AI status")]
    Invalid,
}

/// What the coordinator applies to its status policy after one daemon observation.
#[derive(Clone, Debug, PartialEq, Eq)]
#[must_use = "apply the update to the status policy"]
pub(crate) enum SourceUpdate {
    /// Nothing that fences status changed (name, title, controller, size, cwd, …).
    Unchanged,
    /// A newer observation of a tracked session.
    Observe(StatusSource),
    /// The daemon deleted a tracked session.
    Remove(SessionId),
}

/// A tracked live, non-private session the refresh scheduler may consider.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct Candidate {
    pub(crate) session: SessionId,
    pub(crate) stamp: SourceStamp,
    /// Output or activity changed since `stamp`.
    pub(crate) dirty: bool,
    /// The agent just finished a turn (native lifecycle `Done`).
    pub(crate) finished: bool,
    /// The daemon follows the agent's turn: its stamps advance only at the turn's
    /// next prompt (see [`Facts::same`]).
    pub(crate) follows_turn: bool,
    /// The followed turn: its chat and how many messages the agent wrote.
    pub(crate) turn: Option<AgentTurn>,
}

/// AI summaries of agents' turns (`sessions.ai_summary` in the TUI's
/// `config.json`). Without a followed turn (another program, an older daemon),
/// terminal output is summarized, when asked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TurnSettings {
    /// A session's one automatic summary is made once its agent has written
    /// this many messages, or when its turn finishes sooner.
    pub every: u32,
    /// The newest messages a working turn's summary reads, after the prompt.
    pub recent: u32,
}

impl Default for TurnSettings {
    fn default() -> Self {
        Self {
            every: 5,
            recent: 10,
        }
    }
}

impl TurnSettings {
    /// At least one message each; at most what the daemon keeps of a turn.
    pub fn bounded(self) -> Self {
        Self {
            every: self.every.clamp(1, 100),
            recent: self.recent.clamp(1, 100),
        }
    }
}

/// Daemon facts that supersede a source revision at once: incarnation, liveness,
/// folder, privacy, the session's own report, and what its text is about: the
/// followed turn (its prompt), else the native lifecycle. Compare them with
/// [`Facts::same`].
#[derive(Clone)]
pub(super) struct Facts {
    pub(super) scope: ScopeRoot,
    pub(super) live: bool,
    pub(super) pid: Option<u32>,
    pub(super) private: bool,
    pub(super) lifecycle: Lifecycle,
    pub(super) reported: Option<Reported>,
    /// The agent's turn the daemon follows (`SessionView::agent_turn`).
    pub(super) turn: Option<AgentTurn>,
}

/// What a session is doing, as far as its summaries go.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Progress {
    /// No followed turn: the native lifecycle.
    Lifecycle(Lifecycle),
    /// A followed turn, and how far its agent got.
    Turn { id: u64, state: TurnState },
}

/// What a summary and its title are made from, so what makes them out of date:
/// a followed turn, its prompt, whatever its agent writes about it and however
/// its lifecycle moves meanwhile, until the next prompt (or chat); without one,
/// the native lifecycle.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Subject<'a> {
    Lifecycle(Lifecycle),
    Turn { id: u64, chat: Option<&'a str> },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum TurnState {
    Working,
    /// Waiting for the user (lifecycle `blocked`).
    Blocked,
    /// The agent gave its final reply (lifecycle `done` or `idle`).
    Finished,
}

impl Facts {
    pub(super) fn of(view: &SessionView) -> Self {
        let live = view.status == SessionStatus::Live;
        Self {
            scope: view.scope.clone(),
            live,
            pid: view
                .terminal
                .as_ref()
                .and_then(|terminal| terminal.pid)
                .filter(|_| live),
            private: crate::shared_elsewhere(view),
            lifecycle: view.lifecycle,
            reported: view.reported.clone(),
            turn: view.agent_turn.clone().filter(|_| live),
        }
    }

    pub(super) fn follows_turn(&self) -> bool {
        self.turn.is_some()
    }

    pub(super) fn progress(&self) -> Progress {
        let Some(turn) = &self.turn else {
            return Progress::Lifecycle(self.lifecycle);
        };
        let state = match self.lifecycle {
            Lifecycle::Done | Lifecycle::Idle => TurnState::Finished,
            Lifecycle::Blocked => TurnState::Blocked,
            _ => TurnState::Working,
        };
        Progress::Turn { id: turn.id, state }
    }

    fn subject(&self) -> Subject<'_> {
        match &self.turn {
            Some(turn) => Subject::Turn {
                id: turn.id,
                chat: turn.conversation_id.as_deref(),
            },
            None => Subject::Lifecycle(self.lifecycle),
        }
    }

    /// Whether `other` describes the same source. A followed turn is one source
    /// from its prompt to the next: its messages and lifecycle don't change what
    /// a title made from it says, so a title asked for or made on the way still
    /// names the session when it arrives.
    pub(super) fn same(&self, other: &Facts) -> bool {
        self.scope == other.scope
            && self.live == other.live
            && self.pid == other.pid
            && self.private == other.private
            && self.reported == other.reported
            && self.subject() == other.subject()
    }
}

struct Tracked {
    facts: Facts,
    phase: Option<ActivityPhase>,
    stamp: SourceStamp,
    /// This generation's process, once the daemon reported it.
    incarnation: Option<u32>,
    /// The report may hold meaningful text; cleared by a no-context capture.
    reports: bool,
    /// The terminal may hold meaningful output; cleared by a no-context capture.
    output: bool,
    /// Output or activity phase changed since the current stamp.
    dirty: bool,
}

/// Ordered source stamps for the sessions one coordinator observes through SessionApi.
///
/// Owned by `StatusDispatchService`, which applies every returned update to its
/// policy, so stamps from two trackers can never mix. Liveness, folder, privacy,
/// lifecycle and report changes advance a stamp immediately, hiding older cached text.
/// New output and activity phases only mark a session dirty: `advance` folds them into
/// one revision when the owner's refresh budget allows, so text stays visible while a
/// terminal is busy. Stamps start again with each coordinator, whose required consent
/// confirmation clears older cached text.
#[derive(Default)]
pub(crate) struct SourceTracker {
    generation: u64,
    sessions: HashMap<SessionId, Tracked>,
    retired: HashSet<SessionId>,
    turns: TurnSettings,
}

impl SourceTracker {
    /// No sessions observed; no daemon request is made.
    pub fn new() -> Self {
        Self::default()
    }

    /// Summarize followed turns with these settings from now on.
    pub fn set_turn_settings(&mut self, settings: TurnSettings) {
        self.turns = settings.bounded();
    }

    /// Apply a complete view from `list`/`get` or a created/updated event. Sessions
    /// first seen without a terminal are not tracked: nothing can be captured from them.
    pub fn apply_view(&mut self, view: &SessionView) -> Result<SourceUpdate, SourceError> {
        let phase = view
            .activity
            .as_ref()
            .map(|activity| activity.phase)
            .filter(|_| view.status == SessionStatus::Live);
        self.apply(&view.id, Facts::of(view), phase)
    }

    /// Apply one subscribed event. Events for sessions not yet tracked wait for a view.
    pub fn apply_event(&mut self, envelope: &EventEnvelope) -> Result<SourceUpdate, SourceError> {
        let id = &envelope.session_id;
        let event = &envelope.event;
        match event {
            SessionEvent::Created { session } | SessionEvent::Updated { session } => {
                if &session.id != id {
                    return Ok(SourceUpdate::Unchanged);
                }
                return self.apply_view(session);
            }
            SessionEvent::Removed => return self.remove(id),
            SessionEvent::Activity => {
                if let Some(tracked) = self.sessions.get_mut(id) {
                    tracked.dirty |= output_counts(&tracked.facts);
                }
                return Ok(SourceUpdate::Unchanged);
            }
            // Replies go to the feed, not the AI status source.
            SessionEvent::Reply { .. } | SessionEvent::Unknown => {
                return Ok(SourceUpdate::Unchanged)
            }
            SessionEvent::Exited { .. }
            | SessionEvent::Lifecycle { .. }
            | SessionEvent::Reported { .. } => {}
        }
        let Some(tracked) = self.sessions.get(id) else {
            return Ok(SourceUpdate::Unchanged);
        };
        let mut facts = tracked.facts.clone();
        let mut phase = tracked.phase;
        facts.scope = envelope.scope.clone();
        match event {
            SessionEvent::Exited { .. } => {
                facts.live = false;
                facts.pid = None;
                facts.turn = None;
                phase = None;
            }
            SessionEvent::Lifecycle { lifecycle } => facts.lifecycle = *lifecycle,
            SessionEvent::Reported { reported } => facts.reported = Some(reported.clone()),
            _ => {}
        }
        self.apply(id, facts, phase)
    }

    /// Rebuild after a lost or lagging event stream from a fresh `list`. Known live
    /// sessions are marked dirty first, since output events may have been missed;
    /// tracked live sessions that are no longer listed become not live. Only a
    /// `removed` event retires a session. Every view is attempted: the updates made
    /// are returned with the first error, and all of them must be applied.
    pub fn resync(&mut self, views: &[SessionView]) -> (Vec<SourceUpdate>, Option<SourceError>) {
        let listed: HashSet<&SessionId> = views.iter().map(|view| &view.id).collect();
        let missing: Vec<SessionId> = self
            .sessions
            .iter()
            .filter(|(id, tracked)| tracked.facts.live && !listed.contains(id))
            .map(|(id, _)| id.clone())
            .collect();
        let mut updates = Vec::new();
        let mut failure = None;
        for view in views {
            if let Some(tracked) = self.sessions.get_mut(&view.id) {
                tracked.dirty |= output_counts(&tracked.facts);
            }
            match self.apply_view(view) {
                Ok(SourceUpdate::Unchanged) => {}
                Ok(update) => updates.push(update),
                Err(error) => failure = failure.or(Some(error)),
            }
        }
        for id in missing {
            let Some(tracked) = self.sessions.get(&id) else {
                continue;
            };
            let mut facts = tracked.facts.clone();
            facts.live = false;
            facts.pid = None;
            facts.turn = None;
            match self.apply(&id, facts, None) {
                Ok(SourceUpdate::Unchanged) => {}
                Ok(update) => updates.push(update),
                Err(error) => failure = failure.or(Some(error)),
            }
        }
        (updates, failure)
    }

    /// The current observation of every tracked session.
    pub fn observations(&self) -> impl Iterator<Item = StatusSource> + '_ {
        self.sessions
            .iter()
            .map(|(id, tracked)| observation(id, tracked))
    }

    /// The folders of live, non-private sessions, sorted and without duplicates.
    pub fn live_folders(&self) -> Vec<ScopeRoot> {
        let mut folders: Vec<ScopeRoot> = self
            .sessions
            .values()
            .filter(|tracked| tracked.facts.live && !tracked.facts.private)
            .map(|tracked| tracked.facts.scope.clone())
            .collect();
        folders.sort_by(|a, b| a.path().cmp(b.path()));
        folders.dedup();
        folders
    }

    /// Live, non-private sessions: the only ones a refresh could capture.
    pub fn candidates(&self) -> impl Iterator<Item = Candidate> + '_ {
        self.sessions
            .iter()
            .filter(|(_, tracked)| tracked.facts.live && !tracked.facts.private)
            .map(|(id, tracked)| Candidate {
                session: id.clone(),
                stamp: tracked.stamp,
                dirty: tracked.dirty,
                finished: matches!(
                    tracked.facts.progress(),
                    Progress::Lifecycle(Lifecycle::Done)
                        | Progress::Turn {
                            state: TurnState::Finished,
                            ..
                        }
                ),
                follows_turn: tracked.facts.follows_turn(),
                turn: tracked.facts.turn.clone(),
            })
    }

    /// A tracked session's current stamp.
    #[cfg(test)]
    pub fn stamp(&self, session: &SessionId) -> Option<SourceStamp> {
        self.sessions.get(session).map(|tracked| tracked.stamp)
    }

    /// Whether output or activity changed since the current stamp.
    pub fn is_dirty(&self, session: &SessionId) -> bool {
        self.sessions
            .get(session)
            .is_some_and(|tracked| tracked.dirty)
    }

    /// Make the next `advance` fold a new revision even without new output: a
    /// summary was asked for, and the last one's text mustn't answer it.
    pub fn freshen(&mut self, session: &SessionId) {
        if let Some(tracked) = self.sessions.get_mut(session) {
            tracked.dirty |= tracked.facts.live;
        }
    }

    /// Fold coalesced output/phase changes into one new revision. Call only when the
    /// owner's refresh budget allows another generation for this session.
    pub fn advance(&mut self, session: &SessionId) -> Result<SourceUpdate, SourceError> {
        let Some(tracked) = self.sessions.get_mut(session) else {
            return Ok(SourceUpdate::Unchanged);
        };
        if !tracked.dirty || !tracked.facts.live {
            return Ok(SourceUpdate::Unchanged);
        }
        revise(tracked)?;
        tracked.output = true;
        tracked.dirty = false;
        Ok(SourceUpdate::Observe(observation(session, tracked)))
    }

    /// Record that a capture held no meaningful context in the sources it read, so the
    /// policy reports that instead of retrying the same input. Sources the capture did
    /// not read keep their availability. A new report, or output folded in by
    /// `advance`, makes the session eligible again.
    pub fn no_context(&mut self, capture: &SessionCapture) -> Result<SourceUpdate, SourceError> {
        let Some(tracked) = self.sessions.get_mut(&capture.session) else {
            return Ok(SourceUpdate::Unchanged);
        };
        let reports = tracked.reports && !capture.read_report;
        let output = tracked.output && !capture.read_output;
        if tracked.stamp != capture.stamp || (reports, output) == (tracked.reports, tracked.output)
        {
            return Ok(SourceUpdate::Unchanged);
        }
        revise(tracked)?;
        tracked.reports = reports;
        tracked.output = output;
        Ok(SourceUpdate::Observe(observation(
            &capture.session,
            tracked,
        )))
    }

    /// What a capture for this job must find. Fails without daemon requests when the
    /// job's stamp or folder is no longer current, or the session can't be captured.
    pub fn expectation(&self, job: &StatusCaptureJob) -> Result<CaptureExpectation, CaptureError> {
        if job.is_canceled() {
            return Err(CaptureError::Canceled);
        }
        let tracked = self
            .sessions
            .get(job.session_id())
            .ok_or(CaptureError::Changed)?;
        if tracked.stamp != job.source_stamp() || &tracked.facts.scope != job.folder() {
            return Err(CaptureError::Changed);
        }
        if !tracked.facts.live || tracked.facts.private {
            return Err(CaptureError::NotPermitted);
        }
        Ok(CaptureExpectation::new(
            job,
            tracked.facts.clone(),
            self.turns,
            tracked.reports,
            tracked.output,
        ))
    }

    fn apply(
        &mut self,
        id: &SessionId,
        facts: Facts,
        phase: Option<ActivityPhase>,
    ) -> Result<SourceUpdate, SourceError> {
        if self.retired.contains(id) {
            return Ok(SourceUpdate::Unchanged);
        }
        let invalid = validate_session(id).is_err() || validate_folder(&facts.scope).is_err();
        let Some(tracked) = self.sessions.get_mut(id) else {
            // Never observed by the policy, so nothing can be left unrecorded.
            if !facts.live || invalid {
                return Ok(SourceUpdate::Unchanged);
            }
            if self.sessions.len() + self.retired.len() >= MAX_CACHE_ENTRIES {
                return Err(SourceError::Capacity);
            }
            let tracked = Tracked {
                stamp: SourceStamp {
                    terminal_generation: next(&mut self.generation)?,
                    source_revision: 1,
                },
                incarnation: facts.pid,
                reports: has_report(&facts),
                output: true,
                dirty: false,
                phase,
                facts,
            };
            let update = SourceUpdate::Observe(observation(id, &tracked));
            self.sessions.insert(id.clone(), tracked);
            return Ok(update);
        };
        if invalid {
            return Err(SourceError::Invalid);
        }
        let restarted = facts.live
            && (!tracked.facts.live
                || matches!((tracked.incarnation, facts.pid), (Some(old), Some(new)) if old != new));
        if restarted {
            tracked.stamp = SourceStamp {
                terminal_generation: next(&mut self.generation)?,
                source_revision: 1,
            };
            tracked.incarnation = facts.pid;
            tracked.reports = has_report(&facts);
            tracked.output = true;
            tracked.dirty = false;
        } else if !facts.same(&tracked.facts) {
            revise(tracked)?;
            if facts.reported != tracked.facts.reported {
                tracked.reports = has_report(&facts);
            }
            if tracked.incarnation.is_none() {
                tracked.incarnation = facts.pid;
            }
            // The new revision is captured afresh, so it covers pending output too;
            // a new followed turn has a new prompt.
            let progressed = facts.follows_turn() && facts.subject() != tracked.facts.subject();
            tracked.output |= tracked.dirty || progressed;
            tracked.dirty = false;
        } else {
            if phase != tracked.phase {
                tracked.dirty |= output_counts(&facts);
            }
            // A followed turn's messages and lifecycle changes.
            tracked.facts = facts;
            tracked.phase = phase;
            return Ok(SourceUpdate::Unchanged);
        }
        tracked.facts = facts;
        tracked.phase = phase;
        Ok(SourceUpdate::Observe(observation(id, tracked)))
    }

    /// Retire a tracked session. One never tracked was never observed by the policy and
    /// takes no slot; the daemon never reuses its id.
    fn remove(&mut self, id: &SessionId) -> Result<SourceUpdate, SourceError> {
        if self.sessions.remove(id).is_none() {
            return Ok(SourceUpdate::Unchanged);
        }
        self.retired.insert(id.clone());
        Ok(SourceUpdate::Remove(id.clone()))
    }
}

impl fmt::Debug for SourceTracker {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SourceTracker")
            .field("sessions", &self.sessions.len())
            .field("retired", &self.retired.len())
            .finish()
    }
}

fn next(generation: &mut u64) -> Result<u64, SourceError> {
    *generation = generation.checked_add(1).ok_or(SourceError::Exhausted)?;
    Ok(*generation)
}

fn revise(tracked: &mut Tracked) -> Result<(), SourceError> {
    tracked.stamp.source_revision = tracked
        .stamp
        .source_revision
        .checked_add(1)
        .ok_or(SourceError::Exhausted)?;
    Ok(())
}

/// Whether new terminal output or activity calls for a new summary: not while the
/// daemon follows the agent's turn, whose prompt and messages decide.
fn output_counts(facts: &Facts) -> bool {
    facts.live && !facts.follows_turn()
}

fn has_report(facts: &Facts) -> bool {
    facts.reported.as_ref().and_then(report_text).is_some()
}

fn observation(id: &SessionId, tracked: &Tracked) -> StatusSource {
    StatusSource {
        session_id: id.clone(),
        folder: tracked.facts.scope.clone(),
        stamp: tracked.stamp,
        private: tracked.facts.private,
        live: tracked.facts.live,
        context: ContextAvailability {
            reported_text: tracked.reports,
            recent_output: tracked.facts.live && tracked.output,
        },
    }
}

#[cfg(test)]
mod tests;
