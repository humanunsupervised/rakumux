//! Bounded, ephemeral hook state. Session permissions are enforced by `report`.

use session_core::activity::{
    ActivityEvent, ActivityPhase, ActivityUpdate, AgentActivity, ACTIVITY_VERSION,
};
use session_core::api::{ApiError, ErrorCode};
use std::collections::HashSet;

/// Hooks without a heartbeat eventually yield to screen/process detection.
pub(super) const FRESH_MS: i64 = 120_000;
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
    phase: ActivityPhase,
    subagents: HashSet<String>,
    stopped: HashSet<String>,
    received_ms: i64,
}

impl ActivityTracker {
    pub fn new(update: &ActivityUpdate, now: i64) -> Self {
        Self {
            agent: update.agent.clone(),
            conversation: update.conversation_id.clone(),
            phase: ActivityPhase::Idle,
            subagents: HashSet::new(),
            stopped: HashSet::new(),
            received_ms: now,
        }
    }

    pub fn apply(&mut self, update: &ActivityUpdate, now: i64) {
        if self.agent != update.agent || self.conversation != update.conversation_id {
            *self = Self::new(update, now);
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
    }

    pub fn fresh(&self, now: i64) -> bool {
        now.saturating_sub(self.received_ms) < FRESH_MS
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
}
