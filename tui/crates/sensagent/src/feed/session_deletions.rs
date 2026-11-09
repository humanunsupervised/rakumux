//! Sessions deleted from rakumux.com (`contracts/feed/` `sessionDeletion`):
//! while the account's feed is on, the replies read brings the requests
//! pending for this machine (the cloud sends them only because its session
//! list says `delete_sessions`), and the status service deletes each session
//! through the daemon as the user (`session_delete`, so into the session
//! history), then reports it `deleted`, or `failed` with why.
//!
//! A request names a session by its id, and only one this machine lists to
//! rakumux.com is deleted (live or exited, never private or another
//! machine's); any other fails `not_listed`. Each request is carried out at
//! most once: its id is kept as handled before the daemon is asked, and with
//! the reports rakumux.com hasn't taken yet, across a restart of the service.
//! Driven by the owner thread with its own clock.

use std::collections::HashSet;
use std::path::PathBuf;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use session_core::api::ErrorCode;
use session_core::ids::SessionId;
use session_core::model::SessionView;

use super::new_sessions::kept::Kept;
use super::new_sessions::listed_here;
use super::wire::{self, FeedFailure, FeedRequest};
use crate::cloud::FeedHttpReply;

/// What this machine says with its session list when it deletes sessions
/// asked of it on rakumux.com (`capabilities`), so the cloud sends it those.
pub(crate) const DELETE_SESSIONS: &str = "delete_sessions";
/// The most requests one read carries (contract).
const MAX_ASKED: usize = 20;
/// The first and longest waits after a failed report.
const RETRY_MIN: Duration = Duration::from_secs(5);
const RETRY_MAX: Duration = Duration::from_secs(300);
/// Reports rakumux.com asks to hold back wait at most this long: a request
/// expires 10 minutes after it was written.
const RATE_LIMIT_MAX: Duration = Duration::from_secs(60);

/// A deletion asked of this machine, as read: pending, within the contract.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DeletionAsk {
    pub id: String,
    pub machine: String,
    pub session: SessionId,
}

#[derive(Deserialize)]
struct WireAsk {
    id: String,
    machine: String,
    session: String,
    status: String,
}

impl WireAsk {
    fn checked(self) -> Option<DeletionAsk> {
        let valid = self.status == "pending"
            && crate::feed::is_entry_id(&self.id)
            && crate::feed::is_entry_id(&self.session);
        valid.then(|| DeletionAsk {
            id: self.id,
            machine: self.machine,
            session: SessionId::from_string(self.session),
        })
    }
}

/// The replies read's `session_deletions`: those within the contract, at most
/// [`MAX_ASKED`]; a row off the contract is left out on its own, never the
/// read (it stays pending, and expires).
pub(crate) fn asks(value: Option<serde_json::Value>) -> Vec<DeletionAsk> {
    let Some(serde_json::Value::Array(rows)) = value else {
        return Vec::new();
    };
    rows.into_iter()
        .take(MAX_ASKED)
        .filter_map(|row| serde_json::from_value::<WireAsk>(row).ok()?.checked())
        .collect()
}

/// Why this machine didn't delete a session (contract `sessionDeletionFailure`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum DeletionFailure {
    /// This machine doesn't list that session (gone already, or private).
    NotListed,
    /// The daemon couldn't delete it.
    Refused,
}

/// What became of a request, as reported (contract
/// `session-deletion-status`, whose `status` tags it).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub(crate) enum Outcome {
    Deleted,
    Failed { failure: DeletionFailure },
}

#[derive(Serialize)]
struct StatusBody<'a> {
    version: u8,
    id: &'a str,
    machine: &'a str,
    #[serde(flatten)]
    outcome: &'a Outcome,
}

/// This machine's report on request `id`.
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
    Some(FeedRequest::SessionDeletionStatus { body })
}

#[derive(Deserialize)]
struct StatusAnswer {
    version: u8,
    session_deletion: StoredId,
}

#[derive(Deserialize)]
struct StoredId {
    id: String,
}

/// The answer to a report: the stored request (changed or not).
pub(crate) fn reported(reply: &FeedHttpReply) -> Result<(), FeedFailure> {
    if reply.status != 200 {
        return Err(wire::failure(reply));
    }
    let answer: StatusAnswer =
        serde_json::from_slice(&reply.body).map_err(|_| FeedFailure::Invalid)?;
    if answer.version != 1 || !crate::feed::is_entry_id(&answer.session_deletion.id) {
        return Err(FeedFailure::Invalid);
    }
    Ok(())
}

/// What to do next for deletions.
#[derive(Debug, PartialEq)]
pub(crate) enum DeletionWork {
    /// Ask the daemon to delete `session` for request `id`.
    Delete { id: String, session: SessionId },
    /// Report a request to rakumux.com.
    Report(FeedRequest),
}

/// Deletions' state on the owner thread.
pub(crate) struct SessionDeletionSync {
    machine: String,
    enabled: bool,
    /// The sessions this machine lists to rakumux.com.
    listed: HashSet<SessionId>,
    /// The requests pending for this machine, as last read, oldest first.
    asked: Vec<DeletionAsk>,
    /// The request whose session the daemon is deleting.
    deleting: Option<String>,
    kept: Kept<Outcome>,
    /// A report is out.
    reporting: bool,
    retry_at: Duration,
    retry: Duration,
}

impl SessionDeletionSync {
    /// Deletions for `machine`; `kept` is the file that keeps which were
    /// handled across a restart (memory only without it).
    pub fn new(machine: String, kept: Option<PathBuf>) -> Self {
        Self {
            machine,
            enabled: false,
            listed: HashSet::new(),
            asked: Vec::new(),
            deleting: None,
            kept: Kept::load(kept),
            reporting: false,
            retry_at: Duration::ZERO,
            retry: RETRY_MIN,
        }
    }

    /// Follow the account: signed in with its feed on and this machine's
    /// name confirmed. Off, nothing is deleted; reports not yet taken are
    /// kept for when it's on again.
    pub fn set_enabled(&mut self, enabled: bool, now: Duration) {
        if enabled == self.enabled {
            return;
        }
        self.enabled = enabled;
        self.asked.clear();
        self.retry = RETRY_MIN;
        self.retry_at = now;
    }

    /// This machine goes by `machine` now: requests to the old name aren't its.
    pub fn rename(&mut self, machine: String) {
        self.machine = machine;
        self.asked.clear();
    }

    /// The daemon (re)connected: these are its sessions.
    pub fn connected(&mut self, views: &[SessionView]) {
        self.listed = views
            .iter()
            .filter(|view| listed_here(view))
            .map(|view| view.id.clone())
            .collect();
    }

    pub fn observe(&mut self, view: &SessionView) {
        if listed_here(view) {
            self.listed.insert(view.id.clone());
        } else {
            self.listed.remove(&view.id);
        }
    }

    pub fn forget(&mut self, session: &SessionId) {
        self.listed.remove(session);
    }

    /// A replies read brought `asks`, the requests pending for this machine
    /// now; `current` says whether it was made for the account still signed in.
    pub fn read(&mut self, current: bool, asks: Vec<DeletionAsk>) {
        if !current || !self.enabled {
            return;
        }
        self.asked = asks
            .into_iter()
            .filter(|ask| ask.machine == self.machine)
            .collect();
    }

    /// The next thing to do, if anything is due: decide the requests read
    /// (delete one at a time; fail those that can't be), then report.
    pub fn next_work(&mut self, now: Duration) -> Option<DeletionWork> {
        if !self.enabled {
            return None;
        }
        if self.deleting.is_none() {
            let waiting: Vec<DeletionAsk> = self
                .asked
                .iter()
                .filter(|ask| !self.kept.handled(&ask.id))
                .cloned()
                .collect();
            for ask in waiting {
                if !self.listed.contains(&ask.session) {
                    self.kept.decided(
                        &ask.id,
                        Outcome::Failed {
                            failure: DeletionFailure::NotListed,
                        },
                    );
                    continue;
                }
                // Never asked of the daemon again from now on, even if this
                // service stops before it answers.
                self.kept.creating(&ask.id);
                self.deleting = Some(ask.id.clone());
                return Some(DeletionWork::Delete {
                    id: ask.id,
                    session: ask.session,
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
                    return Some(DeletionWork::Report(request));
                }
                // An id the contract can't carry: nothing to tell.
                None => self.kept.reported(),
            }
        }
        None
    }

    /// The daemon deleted the session for request `id` (or refused).
    pub fn deleted(&mut self, id: &str, result: Result<(), ErrorCode>) {
        if self.deleting.as_deref() == Some(id) {
            self.deleting = None;
        }
        let outcome = match result {
            Ok(()) => Outcome::Deleted,
            Err(ErrorCode::NotFound) => Outcome::Failed {
                failure: DeletionFailure::NotListed,
            },
            Err(_) => Outcome::Failed {
                failure: DeletionFailure::Refused,
            },
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
            // A report the cloud refuses (an expired request's, say) is
            // dropped, not retried forever.
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

    const REQUEST: &str = "01K8A2B3C4D5E6F7G8H9J0K1M2";
    const SESSION: &str = "01K89ZQ4D2F6H8J0K2M4N6P8R0";
    const NOW: Duration = Duration::from_secs(100);

    fn rows() -> Option<serde_json::Value> {
        Some(json!([{
            "id": REQUEST, "machine": "studio", "session": SESSION, "status": "pending",
            "created_at": 1, "status_at": 1,
        }]))
    }

    fn sync(listed: bool) -> SessionDeletionSync {
        let mut sync = SessionDeletionSync::new("studio".into(), None);
        sync.set_enabled(true, Duration::ZERO);
        if listed {
            sync.listed
                .insert(SessionId::from_string(SESSION.to_owned()));
        }
        sync.read(true, asks(rows()));
        sync
    }

    fn report_of(work: Option<DeletionWork>) -> serde_json::Value {
        match work {
            Some(DeletionWork::Report(FeedRequest::SessionDeletionStatus { body })) => {
                serde_json::from_slice(&body).expect("json")
            }
            other => panic!("expected a report, got {other:?}"),
        }
    }

    #[test]
    fn a_listed_session_is_deleted_once_and_reported() {
        let mut sync = sync(true);
        assert_eq!(
            sync.next_work(NOW),
            Some(DeletionWork::Delete {
                id: REQUEST.into(),
                session: SessionId::from_string(SESSION.to_owned()),
            })
        );
        // Read again while the daemon works: never asked twice.
        sync.read(true, asks(rows()));
        assert_eq!(sync.next_work(NOW), None);
        sync.deleted(REQUEST, Ok(()));
        let report = report_of(sync.next_work(NOW));
        assert_eq!(report["status"], "deleted");
        assert_eq!(report["id"], REQUEST);
        assert_eq!(report["machine"], "studio");
        sync.reported(true, Ok(()), NOW);
        sync.read(true, asks(rows()));
        assert_eq!(sync.next_work(NOW), None);
    }

    #[test]
    fn a_session_not_listed_is_never_asked_of_the_daemon() {
        let report = report_of(sync(false).next_work(NOW));
        assert_eq!(report["status"], "failed");
        assert_eq!(report["failure"], "not_listed");
    }

    #[test]
    fn the_daemon_refusing_is_reported() {
        let mut sync = sync(true);
        sync.next_work(NOW);
        sync.deleted(REQUEST, Err(ErrorCode::Internal));
        assert_eq!(report_of(sync.next_work(NOW))["failure"], "refused");
    }

    #[test]
    fn another_machine_and_rows_off_the_contract_are_left_out() {
        let mut sync = SessionDeletionSync::new("laptop".into(), None);
        sync.set_enabled(true, Duration::ZERO);
        sync.read(true, asks(rows()));
        assert_eq!(sync.next_work(NOW), None);
        let bad =
            json!([{ "id": "nope", "machine": "studio", "session": SESSION, "status": "pending" }]);
        assert!(asks(Some(bad)).is_empty());
    }

    #[test]
    fn a_retried_report_waits() {
        let mut sync = sync(false);
        report_of(sync.next_work(NOW));
        sync.reported(true, Err(FeedFailure::Unavailable(None)), NOW);
        assert_eq!(sync.next_work(NOW), None);
        report_of(sync.next_work(NOW + RETRY_MIN));
    }
}
