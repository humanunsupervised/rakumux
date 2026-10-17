//! Lifecycle of a live session: screen detection combined with self-reports.
//!
//! - A fresh self-report (`session.report` with a state) wins for
//!   [`REPORT_FRESH_MS`], then detection takes over again.
//! - `Working → Idle` becomes `Done` and stays `Done` until the session is
//!   touched (seen). A report of `done` has the same effect.
//! - `Unknown` (agent running, state unrecognisable) neither starts nor
//!   cancels a turn.

use crate::detect::Detection;
use session_core::model::Lifecycle;

/// How long a reported state overrides detection.
pub(crate) const REPORT_FRESH_MS: i64 = 30_000;

#[derive(Debug, Clone)]
pub(crate) struct LifecycleTracker {
    detected: Lifecycle,
    agent: Option<String>,
    /// Last reported state (never `Done`; see `on_report`) and when.
    report: Option<(Lifecycle, i64)>,
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

    /// Agent kind found by screen/process detection.
    pub fn detected_agent(&self) -> Option<&str> {
        self.agent.as_deref()
    }

    /// New detection result. Returns true when the lifecycle changed.
    pub fn on_detection(&mut self, detection: Detection, now_ms: i64) -> bool {
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

    /// Periodic re-evaluation (report expiry).
    pub fn tick(&mut self, now_ms: i64) -> bool {
        self.recompute(now_ms)
    }

    fn base(&self, now_ms: i64) -> Lifecycle {
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
}
