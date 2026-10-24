//! Versioned, agent-neutral activity reports. No prompts, tool arguments or output.

use serde::{Deserialize, Serialize};

/// The hook protocol version understood by this build.
pub const ACTIVITY_VERSION: u8 = 1;

/// A phase explicitly reported by an integration, not inferred from silence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActivityPhase {
    Working,
    WaitingForResponse,
    RunningTool,
    /// The agent's own turn is over but paused until its background agents
    /// (subagents, workflows) finish and wake it: still working, though its
    /// screen waits for input.
    WaitingForAgents,
    NeedsInput,
    Idle,
    Done,
    #[serde(other)]
    Unknown,
}

impl ActivityPhase {
    /// Existing lifecycle corresponding to this more detailed phase.
    pub fn lifecycle(self) -> crate::model::Lifecycle {
        use crate::model::Lifecycle;
        match self {
            Self::Working
            | Self::WaitingForResponse
            | Self::RunningTool
            | Self::WaitingForAgents => Lifecycle::Working,
            Self::NeedsInput => Lifecycle::Blocked,
            Self::Idle | Self::Done => Lifecycle::Idle,
            Self::Unknown => Lifecycle::Unknown,
        }
    }
}

/// One update from the integration running inside the reporting session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActivityUpdate {
    pub version: u8,
    pub agent: String,
    pub conversation_id: String,
    /// Opaque completed-reply identifier, when the integration supplies one.
    /// Repeated snapshots of this reply must not make it unread again.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completion_id: Option<String>,
    #[serde(flatten)]
    pub event: ActivityEvent,
}

/// Hooks can report deltas; integrations with a live cache can send snapshots.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum ActivityEvent {
    Phase {
        phase: ActivityPhase,
    },
    Subagent {
        id: String,
        running: bool,
    },
    Snapshot {
        phase: ActivityPhase,
        subagents: Vec<String>,
    },
    #[serde(other)]
    Unknown,
}

/// Daemon-owned projection shared by all clients. Expires when hooks go quiet.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentActivity {
    pub phase: ActivityPhase,
    pub active_subagents: u16,
    /// Unix seconds of the latest hook update received by the daemon.
    pub updated_at: i64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn activity_protocol_round_trips_and_tolerates_unknown_variants() {
        let update = ActivityUpdate {
            version: ACTIVITY_VERSION,
            agent: "codex".into(),
            conversation_id: "thread-1".into(),
            completion_id: None,
            event: ActivityEvent::Subagent {
                id: "child-1".into(),
                running: true,
            },
        };
        let json = serde_json::to_string(&update).unwrap();
        assert_eq!(
            serde_json::from_str::<ActivityUpdate>(&json).unwrap(),
            update
        );
        assert_eq!(
            serde_json::from_str::<ActivityPhase>("\"future_phase\"").unwrap(),
            ActivityPhase::Unknown
        );
        assert_eq!(
            serde_json::from_str::<ActivityEvent>("{\"event\":\"future_event\"}").unwrap(),
            ActivityEvent::Unknown
        );
    }

    #[test]
    fn completion_identity_is_optional_and_round_trips_without_changing_version_one() {
        let legacy = r#"{"version":1,"agent":"opencode","conversation_id":"root","event":"snapshot","phase":"done","subagents":[]}"#;
        let mut update: ActivityUpdate = serde_json::from_str(legacy).unwrap();
        assert!(update.completion_id.is_none());
        assert!(!serde_json::to_string(&update)
            .unwrap()
            .contains("completion_id"));
        update.completion_id = Some("reply-1".into());
        assert_eq!(
            serde_json::from_str::<ActivityUpdate>(&serde_json::to_string(&update).unwrap())
                .unwrap(),
            update
        );
    }
}
