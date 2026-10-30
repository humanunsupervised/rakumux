//! Bounded, ephemeral hook state. Session permissions are enforced by `report`.

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
    received_ms: i64,
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
            received_ms: now,
            delta: false,
        }
    }

    /// Apply metadata and return whether it announces a new completed reply.
    pub fn apply(&mut self, update: &ActivityUpdate, now: i64) -> bool {
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
            }
            ActivityEvent::Snapshot { phase, subagents } => {
                self.phase = *phase;
                self.subagents = subagents.iter().cloned().collect();
                self.stopped.clear();
            }
            ActivityEvent::Subagent { id, running } => {
                if *running {
                    if !self.stopped.contains(id) && self.subagents.len() < MAX_SUBAGENTS {
                        self.subagents.insert(id.clone());
                    }
                } else {
                    self.subagents.remove(id);
                    if self.stopped.len() < MAX_SUBAGENTS {
                        self.stopped.insert(id.clone());
                    }
                }
            }
            ActivityEvent::Unknown => {}
        }
        self.received_ms = now;
        self.delta = !matches!(update.event, ActivityEvent::Snapshot { .. });
        new_completion
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
