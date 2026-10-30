//! What a terminal's program says it is doing through OSC 7501, the Program
//! Status Protocol (`vt::ProgramStatus`; Pi and Kimi Code speak it): the
//! program's own record and its children's (subagents), as lifecycle and
//! activity. Memory only: a restart in place drops them, and the program's
//! next report brings them back.

use session_core::activity::{ActivityPhase, AgentActivity};
use session_core::model::Lifecycle;
use std::collections::HashSet;
use vt::{ProgramState, ProgramStatus};

/// The most child records counted, as for hook subagents.
const MAX_CHILDREN: usize = 64;

#[derive(Debug, Clone, Default)]
pub(super) struct ProgramRecords {
    /// The program's own record; none until it reports one.
    root: Option<ProgramState>,
    /// Children whose record says they work or wait for the user.
    running: HashSet<String>,
    received_ms: i64,
}

impl ProgramRecords {
    /// Apply a report; returns whether it says a turn finished (the
    /// program's own `done` after any other state).
    pub fn apply(&mut self, status: &ProgramStatus, now: i64) -> bool {
        self.received_ms = now;
        match &status.id {
            Some(child) => {
                if matches!(status.state, ProgramState::Working | ProgramState::Blocked) {
                    if self.running.len() < MAX_CHILDREN {
                        self.running.insert(child.clone());
                    }
                } else {
                    self.running.remove(child);
                }
                false
            }
            None if status.state == ProgramState::Clear => {
                *self = Self::default();
                false
            }
            None => {
                let finished =
                    status.state == ProgramState::Done && self.root != Some(ProgramState::Done);
                self.root = Some(status.state);
                finished
            }
        }
    }

    /// Nothing is left to say (the program cleared its records).
    pub fn is_empty(&self) -> bool {
        self.root.is_none() && self.running.is_empty()
    }

    pub fn received_ms(&self) -> i64 {
        self.received_ms
    }

    /// The lifecycle the records say: blocked on the user, working (the
    /// program, or a child while the program itself waits), or idle.
    pub fn lifecycle(&self) -> Option<Lifecycle> {
        match self.root {
            Some(ProgramState::Blocked) => Some(Lifecycle::Blocked),
            Some(ProgramState::Working) => Some(Lifecycle::Working),
            _ if !self.running.is_empty() => Some(Lifecycle::Working),
            Some(_) => Some(Lifecycle::Idle),
            None => None,
        }
    }

    /// As activity: the program's phase and its children running.
    pub fn view(&self) -> AgentActivity {
        AgentActivity {
            phase: match self.root {
                Some(ProgramState::Working) => ActivityPhase::Working,
                Some(ProgramState::Blocked) => ActivityPhase::NeedsInput,
                _ if !self.running.is_empty() => ActivityPhase::WaitingForAgents,
                Some(ProgramState::Done) => ActivityPhase::Done,
                _ => ActivityPhase::Idle,
            },
            active_subagents: u16::try_from(self.running.len()).unwrap_or(u16::MAX),
            updated_at: self.received_ms / 1000,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report(state: ProgramState, id: Option<&str>) -> ProgramStatus {
        ProgramStatus {
            state,
            kind: None,
            id: id.map(String::from),
            app: Some("kimi-code".into()),
        }
    }

    #[test]
    fn the_programs_record_is_its_state_and_childrens_records_count_subagents() {
        use ProgramState::*;
        let mut records = ProgramRecords::default();
        assert_eq!(records.lifecycle(), None);
        assert!(!records.apply(&report(Idle, None), 1));
        assert_eq!(records.lifecycle(), Some(Lifecycle::Idle));
        records.apply(&report(Working, None), 2);
        assert_eq!(records.lifecycle(), Some(Lifecycle::Working));
        // Kimi Code's subagent: a child record of its own.
        records.apply(&report(Working, Some("agent-1")), 3);
        records.apply(&report(Blocked, Some("agent-2")), 4);
        assert_eq!(records.view().active_subagents, 2);
        records.apply(&report(Done, Some("agent-1")), 5);
        records.apply(&report(Clear, Some("agent-2")), 6);
        assert_eq!(records.view().active_subagents, 0);
        assert_eq!(records.view().updated_at, 0);
        // The program's own done finishes a turn, once.
        assert!(records.apply(&report(Done, None), 7_000));
        assert!(!records.apply(&report(Done, None), 7_001));
        assert_eq!(records.lifecycle(), Some(Lifecycle::Idle));
        assert_eq!(records.view().phase, ActivityPhase::Done);
        assert_eq!(records.view().updated_at, 7);
        // A background child working after the program's turn: still working.
        records.apply(&report(Working, Some("agent-3")), 8);
        assert_eq!(records.lifecycle(), Some(Lifecycle::Working));
        assert_eq!(records.view().phase, ActivityPhase::WaitingForAgents);
        // Blocked on the user beats a child working.
        records.apply(&report(Blocked, None), 9);
        assert_eq!(records.lifecycle(), Some(Lifecycle::Blocked));
        assert_eq!(records.view().phase, ActivityPhase::NeedsInput);
        // Error ends the turn like idle.
        records.apply(&report(Clear, Some("agent-3")), 10);
        records.apply(&report(Error, None), 11);
        assert_eq!(records.lifecycle(), Some(Lifecycle::Idle));
        // The program leaving clears everything.
        records.apply(&report(Working, Some("agent-4")), 12);
        records.apply(&report(Clear, None), 13);
        assert!(records.is_empty());
        assert_eq!(records.lifecycle(), None);
    }

    #[test]
    fn children_are_bounded() {
        let mut records = ProgramRecords::default();
        for id in 0..100 {
            records.apply(&report(ProgramState::Working, Some(&id.to_string())), 1);
        }
        assert_eq!(records.view().active_subagents, MAX_CHILDREN as u16);
    }
}
