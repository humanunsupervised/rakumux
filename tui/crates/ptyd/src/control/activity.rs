//! Bounded, ephemeral hook state. Session permissions are enforced by `report`.
//!
//! Hooks go stale after [`FRESH_MS`] without one, except a question or a
//! prompt for permission (`NeedsInput`) in a conversation the daemon follows
//! in the agent's log: it waits silently for as long as the user takes, so
//! it lasts until that conversation moves on after it
//! ([`ActivityTracker::unanswered`]).

use session_core::activity::{
    ActivityEvent, ActivityPhase, ActivityUpdate, AgentActivity, ACTIVITY_VERSION,
};
use session_core::api::{ApiError, ErrorCode};
use std::collections::HashSet;

/// Hooks without a heartbeat eventually yield to screen/process detection.
pub(super) const FRESH_MS: i64 = 120_000;
/// While hook-reported subagents run, or the agent waits for its background agents,
/// their own hooks are the only heartbeat, and one tool call of theirs can take
/// minutes. A turn whose end was missed is still ended by the screen check in
/// `lifecycle`.
pub(super) const FRESH_WITH_SUBAGENTS_MS: i64 = 600_000;
const MAX_SUBAGENTS: usize = 64;
/// Subagents remembered per conversation, running or not, so their own hooks
/// are recognised as theirs.
const MAX_KNOWN_SUBAGENTS: usize = 256;

pub(super) fn validate(update: &ActivityUpdate) -> Result<(), ApiError> {
    let identifier = |value: &str| {
        !value.is_empty()
            && value.len() <= 256
            && value.chars().all(|character| !character.is_control())
    };
    let valid_event = match &update.event {
        ActivityEvent::Phase { phase } => *phase != ActivityPhase::Unknown,
        ActivityEvent::Subagent { id, .. } => identifier(id),
        ActivityEvent::Snapshot { phase, subagents } => {
            *phase != ActivityPhase::Unknown
                && subagents.len() <= MAX_SUBAGENTS
                && subagents.iter().all(|id| identifier(id))
        }
        ActivityEvent::Unknown => false,
    };
    if update.version != ACTIVITY_VERSION
        || !identifier(&update.agent)
        || !identifier(&update.conversation_id)
        || update
            .completion_id
            .as_deref()
            .is_some_and(|id| !identifier(id))
        || !valid_event
    {
        return Err(ApiError::new(
            ErrorCode::InvalidArgument,
            "invalid or unsupported activity update",
        ));
    }
    Ok(())
}

#[derive(Debug, Clone)]
pub(super) struct ActivityTracker {
    pub agent: String,
    conversation: String,
    completion_id: Option<String>,
    phase: ActivityPhase,
    subagents: HashSet<String>,
    stopped: HashSet<String>,
    /// Every subagent reported in this conversation, running or not: an
    /// update about one of their conversations is its heartbeat.
    known: HashSet<String>,
    received_ms: i64,
    /// When its conversation's latest update that asked the user something
    /// (`NeedsInput`) was received; `None` while it asks nothing.
    asked_ms: Option<i64>,
    /// The latest update was a delta (command hooks), which can miss the end of a turn;
    /// snapshots come from a live cache that keeps re-sending its state.
    delta: bool,
}

impl ActivityTracker {
    pub fn new(update: &ActivityUpdate, now: i64) -> Self {
        Self {
            agent: update.agent.clone(),
            conversation: update.conversation_id.clone(),
            completion_id: None,
            phase: ActivityPhase::Idle,
            subagents: HashSet::new(),
            stopped: HashSet::new(),
            known: HashSet::new(),
            received_ms: now,
            asked_ms: None,
            delta: false,
        }
    }

    /// Apply metadata and return whether it announces a new completed reply.
    pub fn apply(&mut self, update: &ActivityUpdate, now: i64) -> bool {
        if self.is_subagent(update) {
            self.heartbeat(update, now);
            return false;
        }
        if !self.is_for(update) {
            *self = Self::new(update, now);
        }
        let completed = matches!(
            update.event,
            ActivityEvent::Phase {
                phase: ActivityPhase::Done
            } | ActivityEvent::Snapshot {
                phase: ActivityPhase::Done,
                ..
            }
        );
        let new_completion = completed
            && match &update.completion_id {
                Some(id) => self.completion_id.as_ref() != Some(id),
                None => self.phase != ActivityPhase::Done,
            };
        if completed || update.completion_id.is_some() {
            self.completion_id.clone_from(&update.completion_id);
        }
        match &update.event {
            ActivityEvent::Phase { phase } => {
                if matches!(phase, ActivityPhase::Done | ActivityPhase::Idle) {
                    self.subagents.clear();
                }
                if matches!(self.phase, ActivityPhase::Done | ActivityPhase::Idle) {
                    self.stopped.clear();
                }
                self.phase = *phase;
                self.asked_ms = (*phase == ActivityPhase::NeedsInput).then_some(now);
            }
            ActivityEvent::Snapshot { phase, subagents } => {
                self.phase = *phase;
                self.asked_ms = (*phase == ActivityPhase::NeedsInput).then_some(now);
                self.subagents = subagents.iter().cloned().collect();
                self.stopped.clear();
                for id in subagents {
                    self.remember(id);
                }
            }
            ActivityEvent::Subagent { id, running } => self.subagent(id, *running),
            ActivityEvent::Unknown => {}
        }
        self.received_ms = now;
        self.delta = !matches!(update.event, ActivityEvent::Snapshot { .. });
        new_completion
    }

    fn subagent(&mut self, id: &str, running: bool) {
        self.remember(id);
        if running {
            if !self.stopped.contains(id) && self.subagents.len() < MAX_SUBAGENTS {
                self.subagents.insert(id.to_owned());
            }
        } else {
            self.subagents.remove(id);
            if self.stopped.len() < MAX_SUBAGENTS {
                self.stopped.insert(id.to_owned());
            }
        }
    }

    fn remember(&mut self, id: &str) {
        if self.known.len() < MAX_KNOWN_SUBAGENTS {
            self.known.insert(id.to_owned());
        }
    }

    /// An update from a subagent's own conversation (Droid's and Grok's
    /// subagents run hooks under their own ids): that subagent's heartbeat,
    /// never the parent's phase, completion or conversation. A busy or
    /// waiting phase (or its own subagents) keeps it running; done or idle
    /// stops it.
    fn heartbeat(&mut self, update: &ActivityUpdate, now: i64) {
        let running = match &update.event {
            ActivityEvent::Phase { phase } | ActivityEvent::Snapshot { phase, .. } => {
                !matches!(phase, ActivityPhase::Done | ActivityPhase::Idle)
            }
            ActivityEvent::Subagent { .. } => true,
            ActivityEvent::Unknown => return,
        };
        self.subagent(&update.conversation_id, running);
        self.received_ms = now;
        self.delta = true;
    }

    /// Whether `update` comes from the conversation of a subagent this
    /// conversation reported (see [`Self::heartbeat`]).
    pub fn is_subagent(&self, update: &ActivityUpdate) -> bool {
        self.agent == update.agent
            && self.conversation != update.conversation_id
            && self.known.contains(&update.conversation_id)
    }

    /// Whether `update` belongs to this tracker: its conversation's, or one
    /// of its subagents'.
    pub fn accepts(&self, update: &ActivityUpdate) -> bool {
        self.is_for(update) || self.is_subagent(update)
    }

    /// Whether the latest update came from command hooks, which send no heartbeat and
    /// nothing when a turn is interrupted (Claude Code's Esc fires no Stop).
    pub fn from_deltas(&self) -> bool {
        self.delta
    }

    /// The root conversation its updates are about.
    pub fn conversation(&self) -> &str {
        &self.conversation
    }

    pub fn is_for(&self, update: &ActivityUpdate) -> bool {
        self.agent == update.agent && self.conversation == update.conversation_id
    }

    pub fn fresh(&self, now: i64) -> bool {
        let waiting = !self.subagents.is_empty() || self.phase == ActivityPhase::WaitingForAgents;
        let ttl = if self.delta && waiting {
            FRESH_WITH_SUBAGENTS_MS
        } else {
            FRESH_MS
        };
        now.saturating_sub(self.received_ms) < ttl
    }

    pub fn received_ms(&self) -> i64 {
        self.received_ms
    }

    /// Whether it asks the user something (`NeedsInput`) that `conversation`,
    /// as the agent's log shows it, hasn't moved past: nothing of it is dated
    /// after the update that asked (`moved_at`, by the conversation's clock,
    /// so a call logged after its own hook still precedes it). Answering
    /// writes the tool's result and dismissing the interruption, while the
    /// question itself sends no heartbeat. Only its own conversation counts.
    pub fn unanswered(&self, conversation: &str, moved_at: Option<i64>) -> bool {
        self.phase == ActivityPhase::NeedsInput
            && self.conversation == conversation
            && self
                .asked_ms
                .is_some_and(|asked| moved_at.is_none_or(|moved| moved <= asked))
    }

    pub fn handoff(&self) -> (ActivityUpdate, i64) {
        (
            ActivityUpdate {
                version: ACTIVITY_VERSION,
                agent: self.agent.clone(),
                conversation_id: self.conversation.clone(),
                completion_id: self.completion_id.clone(),
                event: ActivityEvent::Snapshot {
                    phase: self.phase,
                    subagents: self.subagents.iter().cloned().collect(),
                },
            },
            self.received_ms,
        )
    }

    pub fn view(&self) -> AgentActivity {
        AgentActivity {
            phase: self.phase,
            active_subagents: self.subagents.len() as u16,
            updated_at: self.received_ms / 1000,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn update(event: ActivityEvent) -> ActivityUpdate {
        ActivityUpdate {
            version: ACTIVITY_VERSION,
            agent: "claude".into(),
            conversation_id: "turn".into(),
            completion_id: None,
            event,
        }
    }

    #[test]
    fn subagents_are_idempotent_bounded_and_reset_between_conversations() {
        let mut tracker = ActivityTracker::new(
            &update(ActivityEvent::Phase {
                phase: ActivityPhase::Working,
            }),
            0,
        );
        tracker.apply(
            &update(ActivityEvent::Phase {
                phase: ActivityPhase::Working,
            }),
            0,
        );
        for _ in 0..2 {
            tracker.apply(
                &update(ActivityEvent::Subagent {
                    id: "a".into(),
                    running: true,
                }),
                1,
            );
        }
        assert_eq!(tracker.view().active_subagents, 1);
        tracker.apply(
            &update(ActivityEvent::Subagent {
                id: "a".into(),
                running: false,
            }),
            2,
        );
        tracker.apply(
            &update(ActivityEvent::Subagent {
                id: "a".into(),
                running: true,
            }),
            3,
        );
        assert_eq!(
            tracker.view().active_subagents,
            0,
            "a delayed duplicate start cannot resurrect a finished subagent"
        );
        for id in 0..100 {
            tracker.apply(
                &update(ActivityEvent::Subagent {
                    id: id.to_string(),
                    running: true,
                }),
                4,
            );
        }
        assert_eq!(tracker.view().active_subagents, 64);
        let mut next = update(ActivityEvent::Phase {
            phase: ActivityPhase::Idle,
        });
        next.conversation_id = "other".into();
        tracker.apply(&next, 5);
        assert_eq!(tracker.view().active_subagents, 0);
        assert!(!tracker.fresh(5 + FRESH_MS));
    }

    #[test]
    fn a_known_subagents_own_hooks_are_its_heartbeat_never_the_parents_phase() {
        let phase = |phase| ActivityEvent::Phase { phase };
        let from = |conversation: &str, event| ActivityUpdate {
            conversation_id: conversation.into(),
            ..update(event)
        };
        let mut tracker = ActivityTracker::new(&update(phase(ActivityPhase::Working)), 0);
        tracker.apply(&update(phase(ActivityPhase::RunningTool)), 0);
        // Not known yet: another conversation, as before.
        assert!(!tracker.is_subagent(&from("child", phase(ActivityPhase::Working))));
        tracker.apply(
            &update(ActivityEvent::Subagent {
                id: "child".into(),
                running: true,
            }),
            1,
        );
        // The subagent's own hooks (Droid's and Grok's carry its id) keep it
        // counted and the parent fresh, and change nothing else.
        for (at, event) in [
            (FRESH_MS / 2, phase(ActivityPhase::RunningTool)),
            (FRESH_MS, phase(ActivityPhase::NeedsInput)),
            (FRESH_MS * 3 / 2, phase(ActivityPhase::Working)),
        ] {
            let heartbeat = from("child", event);
            assert!(tracker.accepts(&heartbeat) && !tracker.is_for(&heartbeat));
            assert!(!tracker.apply(&heartbeat, at));
            assert!(tracker.fresh(at + FRESH_MS));
            assert_eq!(tracker.view().phase, ActivityPhase::RunningTool);
            assert_eq!(tracker.view().active_subagents, 1);
            assert_eq!(tracker.conversation(), "turn");
        }
        // Its Stop is its own end, never a finished reply of the parent's.
        let mut done = from("child", phase(ActivityPhase::Done));
        done.completion_id = Some("child-reply".into());
        assert!(!tracker.apply(&done, FRESH_MS * 2));
        assert_eq!(tracker.view().active_subagents, 0);
        assert_eq!(tracker.view().phase, ActivityPhase::RunningTool);
        // Still recognised after the parent's turn ends (a background subagent).
        tracker.apply(&update(phase(ActivityPhase::Done)), FRESH_MS * 2 + 1);
        let late = from("child", phase(ActivityPhase::Working));
        assert!(tracker.is_subagent(&late));
        // Another agent's conversation of that id is no heartbeat.
        let mut other = late.clone();
        other.agent = "codex".into();
        assert!(!tracker.accepts(&other));
    }

    #[test]
    fn a_question_is_unanswered_until_its_own_conversation_moves_on_after_it() {
        let phase = |phase| update(ActivityEvent::Phase { phase });
        let mut tracker = ActivityTracker::new(&phase(ActivityPhase::NeedsInput), 0);
        tracker.apply(&phase(ActivityPhase::NeedsInput), 1_000);
        assert!(tracker.unanswered("turn", None));
        assert!(tracker.unanswered("turn", Some(1_000)), "logged with it");
        assert!(!tracker.unanswered("turn", Some(1_001)));
        assert!(!tracker.unanswered("other", None));
        // A live cache's snapshot asks as of when it came.
        tracker.apply(
            &update(ActivityEvent::Snapshot {
                phase: ActivityPhase::NeedsInput,
                subagents: vec![],
            }),
            2_000,
        );
        assert!(tracker.unanswered("turn", Some(1_500)));
        // Answered: nothing asks.
        tracker.apply(&phase(ActivityPhase::Working), 3_000);
        assert!(!tracker.unanswered("turn", None));
    }

    #[test]
    fn running_hook_subagents_stay_fresh_longer_than_snapshots_or_a_quiet_parent() {
        let child = update(ActivityEvent::Subagent {
            id: "child".into(),
            running: true,
        });
        let mut tracker = ActivityTracker::new(&child, 0);
        tracker.apply(&child, 0);
        assert!(tracker.fresh(FRESH_MS + 1), "a subagent's long tool call");
        assert!(!tracker.fresh(FRESH_WITH_SUBAGENTS_MS));
        tracker.apply(
            &update(ActivityEvent::Subagent {
                id: "child".into(),
                running: false,
            }),
            1,
        );
        assert!(!tracker.fresh(1 + FRESH_MS));
        // A live cache re-sends its state every few seconds: no extension.
        tracker.apply(
            &update(ActivityEvent::Snapshot {
                phase: ActivityPhase::Working,
                subagents: vec!["child".into()],
            }),
            2,
        );
        assert!(!tracker.fresh(2 + FRESH_MS));
    }
}
