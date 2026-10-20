//! Lifecycle of a live session: screen detection combined with self-reports.
//!
//! - A fresh self-report (`session.report` with a state) wins for
//!   [`REPORT_FRESH_MS`], then detection takes over again.
//! - `Working → Idle` becomes `Done` and stays `Done` until the session is
//!   touched (seen). A report of `done` has the same effect.
//! - `Unknown` (agent running, state unrecognisable) neither starts nor
//!   cancels a turn.

use super::activity::ActivityTracker;
use crate::detect::Detection;
use session_core::activity::{ActivityUpdate, AgentActivity};
use session_core::model::Lifecycle;

/// How long a reported state overrides detection.
pub(crate) const REPORT_FRESH_MS: i64 = 30_000;

#[derive(Debug, Clone)]
pub(crate) struct LifecycleTracker {
    detected: Lifecycle,
    agent: Option<String>,
    /// Last reported state (never `Done`; see `on_report`) and when.
    report: Option<(Lifecycle, i64)>,
    activity: Option<ActivityTracker>,
    /// A turn is in progress (Working seen since the last Idle).
    in_turn: bool,
    /// A turn finished and nobody has looked yet.
    done: bool,
    current: Lifecycle,
}

impl Default for LifecycleTracker {
    fn default() -> Self {
        Self {
            detected: Lifecycle::None,
            agent: None,
            report: None,
            activity: None,
            in_turn: false,
            done: false,
            current: Lifecycle::None,
        }
    }
}

impl LifecycleTracker {
    pub fn current(&self) -> Lifecycle {
        self.current
    }

    /// Agent kind from fresh hooks, otherwise screen/process detection.
    pub fn detected_agent(&self) -> Option<&str> {
        self.activity
            .as_ref()
            .map(|activity| activity.agent.as_str())
            .or(self.agent.as_deref())
    }

    pub fn activity(&self) -> Option<AgentActivity> {
        self.activity.as_ref().map(ActivityTracker::view)
    }

    pub fn activity_handoff(&self) -> Option<(ActivityUpdate, i64)> {
        self.activity.as_ref().map(ActivityTracker::handoff)
    }

    pub fn on_activity(&mut self, update: &ActivityUpdate, now: i64) {
        // This hook was received after any earlier self-report, even when
        // both arrived in the same millisecond.
        self.report = None;
        let activity = self
            .activity
            .get_or_insert_with(|| ActivityTracker::new(update, now));
        activity.apply(update, now);
        if activity.view().phase == session_core::activity::ActivityPhase::Done {
            self.done = true;
            self.in_turn = false;
        }
        self.recompute(now);
    }

    /// New detection result. Returns true when the lifecycle changed.
    pub fn on_detection(&mut self, detection: Detection, now_ms: i64) -> bool {
        if self
            .activity
            .as_ref()
            .is_some_and(|activity| detection.agent.as_ref() != Some(&activity.agent))
        {
            self.activity = None;
        }
        self.agent = detection.agent;
        self.detected = match detection.lifecycle {
            // Detection never reports Done; treat one as Idle defensively.
            Lifecycle::Done => Lifecycle::Idle,
            other => other,
        };
        self.recompute(now_ms)
    }

    /// The session reported `state` about itself.
    pub fn on_report(&mut self, state: Lifecycle, now_ms: i64) -> bool {
        let state = if state == Lifecycle::Done {
            self.done = true;
            self.in_turn = false;
            Lifecycle::Idle
        } else {
            state
        };
        self.report = Some((state, now_ms));
        self.recompute(now_ms)
    }

    /// The session was seen: `Done` turns into `Idle`.
    pub fn on_touch(&mut self, now_ms: i64) -> bool {
        self.done = false;
        self.recompute(now_ms)
    }

    /// Periodic re-evaluation (self-report and hook expiry).
    pub fn tick(&mut self, now_ms: i64) -> bool {
        let expired = self
            .activity
            .as_ref()
            .is_some_and(|activity| !activity.fresh(now_ms));
        if expired {
            self.activity = None;
        }
        self.recompute(now_ms) || expired
    }

    fn base(&self, now_ms: i64) -> Lifecycle {
        if let Some(activity) = self
            .activity
            .as_ref()
            .filter(|activity| activity.fresh(now_ms))
        {
            if let Some((state, received_ms)) = self.report {
                if received_ms >= activity.received_ms()
                    && now_ms.saturating_sub(received_ms) < REPORT_FRESH_MS
                {
                    return state;
                }
            }
            let view = activity.view();
            return if view.active_subagents > 0
                && view.phase != session_core::activity::ActivityPhase::NeedsInput
            {
                Lifecycle::Working
            } else {
                view.phase.lifecycle()
            };
        }
        match self.report {
            Some((state, at)) if now_ms.saturating_sub(at) < REPORT_FRESH_MS => state,
            _ => self.detected,
        }
    }

    fn recompute(&mut self, now_ms: i64) -> bool {
        let base = self.base(now_ms);
        match base {
            Lifecycle::Working => {
                self.in_turn = true;
                self.done = false;
            }
            Lifecycle::Blocked | Lifecycle::None => {
                self.in_turn = false;
                self.done = false;
            }
            Lifecycle::Idle => {
                if self.in_turn {
                    self.done = true;
                }
                self.in_turn = false;
            }
            Lifecycle::Unknown | Lifecycle::Done => {}
        }
        let next = if base == Lifecycle::Idle && self.done {
            Lifecycle::Done
        } else {
            base
        };
        let changed = next != self.current;
        self.current = next;
        changed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn detection(lifecycle: Lifecycle) -> Detection {
        Detection {
            agent: Some("claude".into()),
            lifecycle,
        }
    }

    #[test]
    fn working_then_idle_is_done_until_touched() {
        let mut t = LifecycleTracker::default();
        assert_eq!(t.current(), Lifecycle::None);
        assert!(t.on_detection(detection(Lifecycle::Idle), 0));
        assert_eq!(t.current(), Lifecycle::Idle);
        assert!(t.on_detection(detection(Lifecycle::Working), 1));
        assert!(t.on_detection(detection(Lifecycle::Idle), 2));
        assert_eq!(t.current(), Lifecycle::Done);
        assert!(!t.on_detection(detection(Lifecycle::Idle), 3));
        assert_eq!(t.current(), Lifecycle::Done);
        assert!(t.on_touch(4));
        assert_eq!(t.current(), Lifecycle::Idle);
        assert_eq!(t.detected_agent(), Some("claude"));
    }

    #[test]
    fn unknown_does_not_break_a_turn_and_blocked_cancels_it() {
        let mut t = LifecycleTracker::default();
        t.on_detection(detection(Lifecycle::Working), 0);
        t.on_detection(detection(Lifecycle::Unknown), 1);
        t.on_detection(detection(Lifecycle::Idle), 2);
        assert_eq!(t.current(), Lifecycle::Done);

        let mut t = LifecycleTracker::default();
        t.on_detection(detection(Lifecycle::Working), 0);
        t.on_detection(detection(Lifecycle::Blocked), 1);
        t.on_detection(detection(Lifecycle::Idle), 2);
        assert_eq!(t.current(), Lifecycle::Idle);
    }

    #[test]
    fn fresh_reports_win_then_expire() {
        let mut t = LifecycleTracker::default();
        t.on_detection(detection(Lifecycle::Idle), 0);
        assert!(t.on_report(Lifecycle::Working, 1_000));
        assert_eq!(t.current(), Lifecycle::Working);
        assert!(!t.on_detection(detection(Lifecycle::Idle), 2_000));
        assert_eq!(t.current(), Lifecycle::Working);
        // Report expired: detection says Idle after a Working turn.
        assert!(t.tick(1_000 + REPORT_FRESH_MS));
        assert_eq!(t.current(), Lifecycle::Done);
    }

    #[test]
    fn reported_done_is_cleared_by_touch_while_still_fresh() {
        let mut t = LifecycleTracker::default();
        t.on_report(Lifecycle::Working, 0);
        assert!(t.on_report(Lifecycle::Done, 10));
        assert_eq!(t.current(), Lifecycle::Done);
        assert!(t.on_touch(20));
        assert_eq!(t.current(), Lifecycle::Idle);
        assert!(!t.tick(30));
        assert_eq!(t.current(), Lifecycle::Idle);
    }

    #[test]
    fn activity_overrides_stale_screens_then_expires_or_clears_on_agent_exit() {
        use session_core::activity::{ActivityEvent, ActivityPhase, ACTIVITY_VERSION};
        let mut tracker = LifecycleTracker::default();
        tracker.on_detection(detection(Lifecycle::Idle), 0);
        let update = ActivityUpdate {
            version: ACTIVITY_VERSION,
            agent: "claude".into(),
            conversation_id: "root".into(),
            event: ActivityEvent::Phase {
                phase: ActivityPhase::Working,
            },
        };
        tracker.on_activity(&update, 1);
        tracker.on_detection(detection(Lifecycle::Idle), 2);
        assert_eq!(tracker.current(), Lifecycle::Working);
        assert!(tracker.activity().is_some());
        assert!(tracker.tick(super::super::activity::FRESH_MS + 1));
        assert!(tracker.activity().is_none());
        tracker.on_activity(&update, 200_000);
        tracker.on_detection(
            Detection {
                agent: None,
                lifecycle: Lifecycle::None,
            },
            200_001,
        );
        assert_eq!(tracker.current(), Lifecycle::None);
        assert!(tracker.activity().is_none());
    }

    #[test]
    fn a_newer_explicit_report_wins_until_a_later_hook_update() {
        use session_core::activity::{ActivityEvent, ActivityPhase, ACTIVITY_VERSION};
        let mut tracker = LifecycleTracker::default();
        let update = ActivityUpdate {
            version: ACTIVITY_VERSION,
            agent: "claude".into(),
            conversation_id: "root".into(),
            event: ActivityEvent::Phase {
                phase: ActivityPhase::Working,
            },
        };
        tracker.on_activity(&update, 1);
        tracker.on_report(Lifecycle::Done, 2);
        assert_eq!(tracker.current(), Lifecycle::Done);
        tracker.on_activity(&update, 2);
        assert_eq!(tracker.current(), Lifecycle::Working);
    }
}
