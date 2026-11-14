//! Heartbeat actions from rakumux.com (`contracts/feed/` `heartbeatAction`):
//! while the account's feed is on, the replies read brings the actions
//! pending for this machine (the cloud sends them only because its session
//! list says `heartbeats`), and the status service carries out each through
//! the daemon as the user: `enable` (approving it) only while the heartbeat
//! is still the version the user read (`updated_at`), `pause`, or `run` (a
//! run now), then reports it `done` (with the run's session), or `failed`
//! with why.
//!
//! Each action is carried out at most once: its id is kept as handled before
//! the daemon is asked, and with the reports rakumux.com hasn't taken yet,
//! across a restart of the service. Driven by the owner thread with its own
//! clock.

use std::path::PathBuf;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use session_core::api::ErrorCode;
use session_core::client::SessionApi;
use session_core::ids::HeartbeatId;

use super::new_sessions::kept::Kept;
use super::wire::{self, FeedFailure, FeedRequest};
use crate::cloud::FeedHttpReply;

/// The most actions one read carries (contract).
const MAX_ASKED: usize = 20;
/// The first and longest waits after a failed report.
const RETRY_MIN: Duration = Duration::from_secs(5);
const RETRY_MAX: Duration = Duration::from_secs(300);
/// Reports rakumux.com asks to hold back wait at most this long: an action
/// expires 10 minutes after it was written.
const RATE_LIMIT_MAX: Duration = Duration::from_secs(60);

/// What is asked of a heartbeat.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Action {
    /// Approve and enable it, while it's the version `updated_at` (ms) names.
    Enable {
        updated_at: i64,
    },
    Pause,
    Run,
}

/// An action asked of this machine, as read: pending, within the contract.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ActionAsk {
    pub id: String,
    pub machine: String,
    pub heartbeat: String,
    pub action: Action,
}

#[derive(Deserialize)]
struct WireAsk {
    id: String,
    machine: String,
    heartbeat: String,
    action: String,
    #[serde(default)]
    updated_at: Option<i64>,
    status: String,
}

impl WireAsk {
    fn checked(self) -> Option<ActionAsk> {
        if self.status != "pending"
            || !crate::feed::is_entry_id(&self.id)
            || !crate::feed::is_entry_id(&self.heartbeat)
        {
            return None;
        }
        let action = match (self.action.as_str(), self.updated_at) {
            ("enable", Some(updated_at)) => Action::Enable { updated_at },
            ("pause", None) => Action::Pause,
            ("run", None) => Action::Run,
            _ => return None,
        };
        Some(ActionAsk {
            id: self.id,
            machine: self.machine,
            heartbeat: self.heartbeat,
            action,
        })
    }
}

/// The replies read's `heartbeat_actions`: those within the contract, at
/// most [`MAX_ASKED`]; a row off the contract is left out on its own.
pub(crate) fn asks(value: Option<serde_json::Value>) -> Vec<ActionAsk> {
    let Some(serde_json::Value::Array(rows)) = value else {
        return Vec::new();
    };
    rows.into_iter()
        .take(MAX_ASKED)
        .filter_map(|row| serde_json::from_value::<WireAsk>(row).ok()?.checked())
        .collect()
}

/// Why this machine didn't carry out an action (`heartbeatActionFailure`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ActionFailure {
    /// No such heartbeat here now.
    NotListed,
    /// It changed since the version approved: nothing enabled.
    Changed,
    /// No command, task or schedule to run with.
    Incomplete,
    /// The daemon refused.
    Refused,
}

/// What became of an action, as reported (`heartbeat-action-status`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub(crate) enum Outcome {
    Done {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session: Option<String>,
    },
    Failed {
        failure: ActionFailure,
    },
}

/// Carry out `action` on heartbeat `id` through the daemon, as the user:
/// the run's session for `run`.
pub(crate) fn carry_out(
    api: &dyn SessionApi,
    id: &str,
    action: Action,
) -> Result<Option<String>, ActionFailure> {
    let id = HeartbeatId::from_string(id);
    let failure = |code: ErrorCode| match code {
        ErrorCode::NotFound => ActionFailure::NotListed,
        ErrorCode::InvalidArgument => ActionFailure::Incomplete,
        _ => ActionFailure::Refused,
    };
    match action {
        Action::Enable { updated_at } => {
            let heartbeat = api.heartbeat(&id).map_err(|error| match error.code {
                ErrorCode::NotFound => ActionFailure::NotListed,
                _ => ActionFailure::Refused,
            })?;
            if heartbeat.updated_at.saturating_mul(1000) != updated_at {
                return Err(ActionFailure::Changed);
            }
            api.heartbeat_enable(&id, true)
                .map(|_| None)
                .map_err(|error| failure(error.code))
        }
        Action::Pause => api
            .heartbeat_enable(&id, false)
            .map(|_| None)
            .map_err(|error| failure(error.code)),
        Action::Run => api
            .heartbeat_run(&id)
            .map(|session| Some(session.id.as_str().to_owned()))
            .map_err(|error| match error.code {
                ErrorCode::NotFound => ActionFailure::NotListed,
                _ => ActionFailure::Refused,
            }),
    }
}

#[derive(Serialize)]
struct StatusBody<'a> {
    version: u8,
    id: &'a str,
    machine: &'a str,
    #[serde(flatten)]
    outcome: &'a Outcome,
}

fn status_request(id: &str, machine: &str, outcome: &Outcome) -> Option<FeedRequest> {
    if !crate::feed::is_entry_id(id) {
        return None;
    }
    let body = serde_json::to_vec(&StatusBody {
        version: 1,
        id,
        machine,
        outcome,
    })
    .ok()?;
    Some(FeedRequest::HeartbeatActionStatus { body })
}

#[derive(Deserialize)]
struct StatusAnswer {
    version: u8,
    heartbeat_action: StoredId,
}

#[derive(Deserialize)]
struct StoredId {
    id: String,
}

/// The answer to a report: the stored action (changed or not).
pub(crate) fn reported(reply: &FeedHttpReply) -> Result<(), FeedFailure> {
    if reply.status != 200 {
        return Err(wire::failure(reply));
    }
    let answer: StatusAnswer =
        serde_json::from_slice(&reply.body).map_err(|_| FeedFailure::Invalid)?;
    if answer.version != 1 || !crate::feed::is_entry_id(&answer.heartbeat_action.id) {
        return Err(FeedFailure::Invalid);
    }
    Ok(())
}

/// What to do next for heartbeat actions.
#[derive(Debug, PartialEq)]
pub(crate) enum ActionWork {
    /// Carry out action `id` on `heartbeat`.
    Act {
        id: String,
        heartbeat: String,
        action: Action,
    },
    /// Report an action to rakumux.com.
    Report(FeedRequest),
}

/// Heartbeat actions' state on the owner thread.
pub(crate) struct HeartbeatActionSync {
    machine: String,
    enabled: bool,
    asked: Vec<ActionAsk>,
    /// The action the daemon is carrying out.
    acting: Option<String>,
    kept: Kept<Outcome>,
    reporting: bool,
    retry_at: Duration,
    retry: Duration,
}

impl HeartbeatActionSync {
    /// Actions for `machine`; `kept` is the file that keeps which were
    /// handled across a restart (memory only without it).
    pub fn new(machine: String, kept: Option<PathBuf>) -> Self {
        Self {
            machine,
            enabled: false,
            asked: Vec::new(),
            acting: None,
            kept: Kept::load(kept),
            reporting: false,
            retry_at: Duration::ZERO,
            retry: RETRY_MIN,
        }
    }

    /// Follow the account: signed in with its feed on and this machine's
    /// name confirmed.
    pub fn set_enabled(&mut self, enabled: bool, now: Duration) {
        if enabled == self.enabled {
            return;
        }
        self.enabled = enabled;
        self.asked.clear();
        self.retry = RETRY_MIN;
        self.retry_at = now;
    }

    pub fn rename(&mut self, machine: String) {
        self.machine = machine;
        self.asked.clear();
    }

    /// A replies read brought `asks`, the actions pending for this machine.
    pub fn read(&mut self, current: bool, asks: Vec<ActionAsk>) {
        if !current || !self.enabled {
            return;
        }
        self.asked = asks
            .into_iter()
            .filter(|ask| ask.machine == self.machine)
            .collect();
    }

    /// The next thing to do: carry out one action at a time, then report.
    pub fn next_work(&mut self, now: Duration) -> Option<ActionWork> {
        if !self.enabled {
            return None;
        }
        if self.acting.is_none() {
            if let Some(ask) = self
                .asked
                .iter()
                .find(|ask| !self.kept.handled(&ask.id))
                .cloned()
            {
                // Never asked of the daemon again, even if this service
                // stops before it answers.
                self.kept.creating(&ask.id);
                self.acting = Some(ask.id.clone());
                return Some(ActionWork::Act {
                    id: ask.id,
                    heartbeat: ask.heartbeat,
                    action: ask.action,
                });
            }
        }
        if self.reporting || now < self.retry_at {
            return None;
        }
        while let Some(report) = self.kept.next_report() {
            match status_request(&report.id, &self.machine, &report.outcome) {
                Some(request) => {
                    self.reporting = true;
                    return Some(ActionWork::Report(request));
                }
                None => self.kept.reported(),
            }
        }
        None
    }

    /// The daemon carried out action `id` (or didn't).
    pub fn acted(&mut self, id: &str, result: Result<Option<String>, ActionFailure>) {
        if self.acting.as_deref() == Some(id) {
            self.acting = None;
        }
        let outcome = match result {
            Ok(session) => Outcome::Done { session },
            Err(failure) => Outcome::Failed { failure },
        };
        self.kept.decided(id, outcome);
    }

    /// A report finished; `current` says whether it was made for the account
    /// still signed in.
    pub fn reported(&mut self, current: bool, outcome: Result<(), FeedFailure>, now: Duration) {
        self.reporting = false;
        if !current || !self.enabled {
            return;
        }
        match outcome {
            Ok(()) => {
                self.retry = RETRY_MIN;
                self.kept.reported();
            }
            Err(FeedFailure::Invalid) => self.kept.reported(),
            Err(failure) => {
                let wait = match failure {
                    FeedFailure::RateLimited(Some(delay)) => delay.min(RATE_LIMIT_MAX),
                    FeedFailure::Unavailable(Some(delay)) => delay.min(RETRY_MAX),
                    _ => self.retry,
                };
                self.retry = self.retry.saturating_mul(2).min(RETRY_MAX);
                self.retry_at = now + wait;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const ACTION: &str = "01K8A2B3C4D5E6F7G8H9J0K1M3";
    const HEARTBEAT: &str = "01K8A2B3C4D5E6F7G8H9J0K1M2";
    const NOW: Duration = Duration::from_secs(100);

    fn row(action: &str, updated_at: Option<i64>) -> serde_json::Value {
        let mut row = json!({
            "id": ACTION, "machine": "studio", "heartbeat": HEARTBEAT, "action": action,
            "status": "pending", "created_at": 1, "status_at": 1,
        });
        if let Some(at) = updated_at {
            row["updated_at"] = json!(at);
        }
        row
    }

    fn report_of(work: Option<ActionWork>) -> serde_json::Value {
        match work {
            Some(ActionWork::Report(FeedRequest::HeartbeatActionStatus { body })) => {
                serde_json::from_slice(&body).expect("json")
            }
            other => panic!("expected a report, got {other:?}"),
        }
    }

    #[test]
    fn an_action_is_carried_out_once_and_reported() {
        let mut sync = HeartbeatActionSync::new("studio".into(), None);
        sync.set_enabled(true, Duration::ZERO);
        sync.read(true, asks(Some(json!([row("run", None)]))));
        assert_eq!(
            sync.next_work(NOW),
            Some(ActionWork::Act {
                id: ACTION.into(),
                heartbeat: HEARTBEAT.into(),
                action: Action::Run,
            })
        );
        sync.read(true, asks(Some(json!([row("run", None)]))));
        assert_eq!(sync.next_work(NOW), None);
        sync.acted(ACTION, Ok(Some("01K89ZQ4D2F6H8J0K2M4N6P8R0".into())));
        let report = report_of(sync.next_work(NOW));
        assert_eq!(report["status"], "done");
        assert_eq!(report["session"], "01K89ZQ4D2F6H8J0K2M4N6P8R0");
        sync.reported(true, Ok(()), NOW);
        sync.read(true, asks(Some(json!([row("run", None)]))));
        assert_eq!(sync.next_work(NOW), None);
    }

    #[test]
    fn a_failure_says_why() {
        let mut sync = HeartbeatActionSync::new("studio".into(), None);
        sync.set_enabled(true, Duration::ZERO);
        sync.read(
            true,
            asks(Some(json!([row("enable", Some(1_761_220_000_000))]))),
        );
        sync.next_work(NOW);
        sync.acted(ACTION, Err(ActionFailure::Changed));
        let report = report_of(sync.next_work(NOW));
        assert_eq!(report["status"], "failed");
        assert_eq!(report["failure"], "changed");
        assert!(report.get("session").is_none());
    }

    #[test]
    fn rows_off_the_contract_are_left_out() {
        assert!(asks(Some(json!([row("enable", None)]))).is_empty());
        assert!(asks(Some(json!([row("pause", Some(1))]))).is_empty());
        assert!(asks(Some(json!([row("delete", None)]))).is_empty());
        assert_eq!(asks(Some(json!([row("pause", None)]))).len(), 1);
        let mut sync = HeartbeatActionSync::new("laptop".into(), None);
        sync.set_enabled(true, Duration::ZERO);
        sync.read(true, asks(Some(json!([row("pause", None)]))));
        assert_eq!(sync.next_work(NOW), None);
    }
}
