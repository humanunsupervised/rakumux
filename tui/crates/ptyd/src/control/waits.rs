//! `session.wait` conditions. The service re-evaluates the condition each
//! time session state changes (host events, reports, touches) and on a short
//! poll interval, until it matches, becomes impossible, or times out.

use session_core::api::WaitUntil;
use session_core::model::{Lifecycle, SessionStatus};
use std::time::Duration;

/// Re-check interval even without notifications (screen text for `Output`
/// changes without a state change).
pub(crate) const POLL: Duration = Duration::from_millis(250);

/// Longest wait accepted; longer timeouts are capped.
pub(crate) const MAX_WAIT: Duration = Duration::from_secs(24 * 60 * 60);

/// What a wait looks at.
#[derive(Debug, Clone)]
pub(crate) struct Observed<'a> {
    pub status: SessionStatus,
    pub lifecycle: Lifecycle,
    /// Reports received so far (in this daemon's lifetime).
    pub report_seq: u64,
    /// Visible screen text; only fetched for `Output`.
    pub screen: Option<&'a str>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Outcome {
    Matched(String),
    /// The condition can no longer become true (the session ended).
    Impossible(String),
    Pending,
}

pub(crate) fn evaluate(
    until: WaitUntil,
    pattern: Option<&str>,
    start_report_seq: u64,
    observed: &Observed<'_>,
) -> Outcome {
    let live = observed.status == SessionStatus::Live;
    let lifecycle = observed.lifecycle;
    let matched = match until {
        WaitUntil::Settled => {
            if !live {
                return Outcome::Matched(ended(observed.status));
            }
            matches!(
                lifecycle,
                Lifecycle::Idle | Lifecycle::Done | Lifecycle::Blocked
            )
        }
        WaitUntil::Idle => matches!(lifecycle, Lifecycle::Idle | Lifecycle::Done),
        WaitUntil::Blocked => lifecycle == Lifecycle::Blocked,
        WaitUntil::Working => lifecycle == Lifecycle::Working,
        WaitUntil::Exited => {
            if !live {
                return Outcome::Matched(ended(observed.status));
            }
            false
        }
        WaitUntil::Reported => {
            if observed.report_seq > start_report_seq {
                return Outcome::Matched("session reported".into());
            }
            false
        }
        WaitUntil::Output => match (pattern, observed.screen) {
            (Some(pattern), Some(screen)) if screen.contains(pattern) => {
                return Outcome::Matched("output matched".into());
            }
            _ => false,
        },
    };
    if matched {
        return Outcome::Matched(format!("session is {}", lifecycle_name(lifecycle)));
    }
    if !live {
        return Outcome::Impossible(ended(observed.status));
    }
    Outcome::Pending
}

fn ended(status: SessionStatus) -> String {
    match status {
        SessionStatus::Exited => "session exited".into(),
        SessionStatus::Dead => "session is dead".into(),
        _ => "session is not running".into(),
    }
}

pub(crate) fn lifecycle_name(lifecycle: Lifecycle) -> &'static str {
    match lifecycle {
        Lifecycle::Working => "working",
        Lifecycle::Blocked => "blocked",
        Lifecycle::Done => "done",
        Lifecycle::Idle => "idle",
        Lifecycle::Unknown => "unknown",
        Lifecycle::None => "none",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn obs(status: SessionStatus, lifecycle: Lifecycle) -> Observed<'static> {
        Observed {
            status,
            lifecycle,
            report_seq: 0,
            screen: None,
        }
    }

    #[test]
    fn lifecycle_conditions() {
        let live = SessionStatus::Live;
        assert!(matches!(
            evaluate(WaitUntil::Idle, None, 0, &obs(live, Lifecycle::Done)),
            Outcome::Matched(_)
        ));
        assert_eq!(
            evaluate(WaitUntil::Idle, None, 0, &obs(live, Lifecycle::Working)),
            Outcome::Pending
        );
        assert!(matches!(
            evaluate(WaitUntil::Settled, None, 0, &obs(live, Lifecycle::Blocked)),
            Outcome::Matched(_)
        ));
        assert_eq!(
            evaluate(WaitUntil::Settled, None, 0, &obs(live, Lifecycle::None)),
            Outcome::Pending
        );
    }

    #[test]
    fn exit_ends_or_satisfies_waits() {
        let exited = SessionStatus::Exited;
        assert_eq!(
            evaluate(WaitUntil::Exited, None, 0, &obs(exited, Lifecycle::None)),
            Outcome::Matched("session exited".into())
        );
        assert!(matches!(
            evaluate(WaitUntil::Settled, None, 0, &obs(exited, Lifecycle::None)),
            Outcome::Matched(_)
        ));
        assert_eq!(
            evaluate(WaitUntil::Working, None, 0, &obs(exited, Lifecycle::None)),
            Outcome::Impossible("session exited".into())
        );
    }

    #[test]
    fn reports_and_output() {
        let mut o = obs(SessionStatus::Live, Lifecycle::None);
        assert_eq!(evaluate(WaitUntil::Reported, None, 0, &o), Outcome::Pending);
        o.report_seq = 1;
        assert!(matches!(
            evaluate(WaitUntil::Reported, None, 0, &o),
            Outcome::Matched(_)
        ));
        o.screen = Some("$ make\nok: 3 passed\n");
        assert!(matches!(
            evaluate(WaitUntil::Output, Some("3 passed"), 0, &o),
            Outcome::Matched(_)
        ));
        assert_eq!(
            evaluate(WaitUntil::Output, Some("failed"), 0, &o),
            Outcome::Pending
        );
    }
}
