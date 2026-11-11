//! Sessions renamed from rakumux.com (`contracts/feed/` `sessionRename`):
//! while the account's feed is on, the replies read brings the requests
//! pending for this machine (the cloud sends them only because its session
//! list says `rename_sessions`), and the status service renames each session
//! through the daemon as the user (`session_rename`, so automatic titles leave
//! it alone from then on), then reports it `renamed`, or `failed` with why.
//!
//! A request names a session by its id, and only one this machine lists to
//! rakumux.com is renamed (live or exited, never private or another
//! machine's); any other fails `not_listed`. Names are unique within a
//! folder: one another session there has fails `name_taken`. Each request is
//! carried out at most once: its id is kept as handled before the daemon is
//! asked, and with the reports rakumux.com hasn't taken yet, across a restart
//! of the service. Driven by the owner thread with its own clock.

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

/// What this machine says with its session list when it renames sessions
/// asked of it on rakumux.com (`capabilities`), so the cloud sends it those.
pub(crate) const RENAME_SESSIONS: &str = "rename_sessions";
/// The most requests one read carries (contract).
const MAX_ASKED: usize = 20;
/// The longest name, in characters (contract `sessionName`, the daemon's).
const MAX_NAME_CHARS: usize = 64;
/// The first and longest waits after a failed report.
const RETRY_MIN: Duration = Duration::from_secs(5);
const RETRY_MAX: Duration = Duration::from_secs(300);
/// Reports rakumux.com asks to hold back wait at most this long: a request
/// expires 10 minutes after it was written.
const RATE_LIMIT_MAX: Duration = Duration::from_secs(60);

/// A rename asked of this machine, as read: pending, within the contract.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RenameAsk {
    pub id: String,
    pub machine: String,
    pub session: SessionId,
    pub name: String,
}

#[derive(Deserialize)]
struct WireAsk {
    id: String,
    machine: String,
    session: String,
    name: String,
    status: String,
}

/// Whether `name` is one the daemon takes as it is: one line, trimmed,
/// 1 to 64 characters, no controls (`control/names.rs`'s rule).
fn is_name(name: &str) -> bool {
    let count = name.chars().count();
    name.trim() == name
        && (1..=MAX_NAME_CHARS).contains(&count)
        && !name.chars().any(char::is_control)
}

impl WireAsk {
    fn checked(self) -> Option<RenameAsk> {
        let valid = self.status == "pending"
            && crate::feed::is_entry_id(&self.id)
            && crate::feed::is_entry_id(&self.session)
            && is_name(&self.name);
        valid.then(|| RenameAsk {
            id: self.id,
            machine: self.machine,
            session: SessionId::from_string(self.session),
            name: self.name,
        })
    }
}

/// The replies read's `session_renames`: those within the contract, at most
/// [`MAX_ASKED`]; a row off the contract is left out on its own, never the
/// read (it stays pending, and expires).
pub(crate) fn asks(value: Option<serde_json::Value>) -> Vec<RenameAsk> {
    let Some(serde_json::Value::Array(rows)) = value else {
        return Vec::new();
    };
    rows.into_iter()
        .take(MAX_ASKED)
        .filter_map(|row| serde_json::from_value::<WireAsk>(row).ok()?.checked())
        .collect()
}

/// Why this machine didn't rename a session (contract `sessionRenameFailure`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RenameFailure {
    /// This machine doesn't list that session (gone already, or private).
    NotListed,
    /// Another session of its folder has that name.
    NameTaken,
    /// The daemon couldn't rename it.
    Refused,
}

/// What became of a request, as reported (contract
/// `session-rename-status`, whose `status` tags it).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub(crate) enum Outcome {
    Renamed,
    Failed { failure: RenameFailure },
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
    Some(FeedRequest::SessionRenameStatus { body })
}

#[derive(Deserialize)]
struct StatusAnswer {
    version: u8,
    session_rename: StoredId,
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
    if answer.version != 1 || !crate::feed::is_entry_id(&answer.session_rename.id) {
        return Err(FeedFailure::Invalid);
    }
    Ok(())
}

/// What to do next for renames.
#[derive(Debug, PartialEq)]
pub(crate) enum RenameWork {
    /// Ask the daemon to name `session` `name` for request `id`.
    Rename {
        id: String,
        session: SessionId,
        name: String,
    },
    /// Report a request to rakumux.com.
    Report(FeedRequest),
}

/// Renames' state on the owner thread.
pub(crate) struct SessionRenameSync {
    machine: String,
    enabled: bool,
    /// The sessions this machine lists to rakumux.com.
    listed: HashSet<SessionId>,
    /// The requests pending for this machine, as last read, oldest first.
    asked: Vec<RenameAsk>,
    /// The request whose session the daemon is renaming.
    renaming: Option<String>,
    kept: Kept<Outcome>,
    /// A report is out.
    reporting: bool,
    retry_at: Duration,
    retry: Duration,
}

impl SessionRenameSync {
    /// Renames for `machine`; `kept` is the file that keeps which were
    /// handled across a restart (memory only without it).
    pub fn new(machine: String, kept: Option<PathBuf>) -> Self {
        Self {
            machine,
            enabled: false,
            listed: HashSet::new(),
            asked: Vec::new(),
            renaming: None,
            kept: Kept::load(kept),
            reporting: false,
            retry_at: Duration::ZERO,
            retry: RETRY_MIN,
        }
    }

    /// Follow the account: signed in with its feed on and this machine's
    /// name confirmed. Off, nothing is renamed; reports not yet taken are
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
    pub fn read(&mut self, current: bool, asks: Vec<RenameAsk>) {
        if !current || !self.enabled {
            return;
        }
        self.asked = asks
            .into_iter()
            .filter(|ask| ask.machine == self.machine)
            .collect();
    }

    /// The next thing to do, if anything is due: decide the requests read
    /// (rename one at a time; fail those that can't be), then report.
    pub fn next_work(&mut self, now: Duration) -> Option<RenameWork> {
        if !self.enabled {
            return None;
        }
        if self.renaming.is_none() {
            let waiting: Vec<RenameAsk> = self
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
                            failure: RenameFailure::NotListed,
                        },
                    );
                    continue;
                }
                // Never asked of the daemon again from now on, even if this
                // service stops before it answers.
                self.kept.creating(&ask.id);
                self.renaming = Some(ask.id.clone());
                return Some(RenameWork::Rename {
                    id: ask.id,
                    session: ask.session,
                    name: ask.name,
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
                    return Some(RenameWork::Report(request));
                }
                // An id the contract can't carry: nothing to tell.
                None => self.kept.reported(),
            }
        }
        None
    }

    /// The daemon renamed the session for request `id` (or refused). The
    /// name was checked as the daemon checks it, so its `invalid_argument`
    /// is the name being taken in the session's folder.
    pub fn renamed(&mut self, id: &str, result: Result<(), ErrorCode>) {
        if self.renaming.as_deref() == Some(id) {
            self.renaming = None;
        }
        let outcome = match result {
            Ok(()) => Outcome::Renamed,
            Err(ErrorCode::NotFound) => Outcome::Failed {
                failure: RenameFailure::NotListed,
            },
            Err(ErrorCode::InvalidArgument) => Outcome::Failed {
                failure: RenameFailure::NameTaken,
            },
            Err(_) => Outcome::Failed {
                failure: RenameFailure::Refused,
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
            "id": REQUEST, "machine": "studio", "session": SESSION, "name": "fix login",
            "status": "pending", "created_at": 1, "status_at": 1,
        }]))
    }

    fn sync(listed: bool) -> SessionRenameSync {
        let mut sync = SessionRenameSync::new("studio".into(), None);
        sync.set_enabled(true, Duration::ZERO);
        if listed {
            sync.listed
                .insert(SessionId::from_string(SESSION.to_owned()));
        }
        sync.read(true, asks(rows()));
        sync
    }

    fn report_of(work: Option<RenameWork>) -> serde_json::Value {
        match work {
            Some(RenameWork::Report(FeedRequest::SessionRenameStatus { body })) => {
                serde_json::from_slice(&body).expect("json")
            }
            other => panic!("expected a report, got {other:?}"),
        }
    }

    #[test]
    fn a_listed_session_is_renamed_once_and_reported() {
        let mut sync = sync(true);
        assert_eq!(
            sync.next_work(NOW),
            Some(RenameWork::Rename {
                id: REQUEST.into(),
                session: SessionId::from_string(SESSION.to_owned()),
                name: "fix login".into(),
            })
        );
        // Read again while the daemon works: never asked twice.
        sync.read(true, asks(rows()));
        assert_eq!(sync.next_work(NOW), None);
        sync.renamed(REQUEST, Ok(()));
        let report = report_of(sync.next_work(NOW));
        assert_eq!(report["status"], "renamed");
        assert_eq!(report["id"], REQUEST);
        assert_eq!(report["machine"], "studio");
        assert!(report.get("name").is_none());
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
    fn a_name_the_folder_has_and_the_daemon_refusing_are_reported() {
        let mut taken = sync(true);
        taken.next_work(NOW);
        taken.renamed(REQUEST, Err(ErrorCode::InvalidArgument));
        assert_eq!(report_of(taken.next_work(NOW))["failure"], "name_taken");
        let mut refused = sync(true);
        refused.next_work(NOW);
        refused.renamed(REQUEST, Err(ErrorCode::Internal));
        assert_eq!(report_of(refused.next_work(NOW))["failure"], "refused");
    }

    #[test]
    fn another_machine_and_rows_off_the_contract_are_left_out() {
        let mut sync = SessionRenameSync::new("laptop".into(), None);
        sync.set_enabled(true, Duration::ZERO);
        sync.read(true, asks(rows()));
        assert_eq!(sync.next_work(NOW), None);
        let row = |id: &str, name: &str| json!([{ "id": id, "machine": "studio", "session": SESSION, "name": name, "status": "pending" }]);
        assert!(asks(Some(row("nope", "fix login"))).is_empty());
        for name in [
            "",
            " fix",
            "fix ",
            "fix\nlogin",
            "a\u{1b}[31m",
            &"x".repeat(65),
        ] {
            assert!(asks(Some(row(REQUEST, name))).is_empty(), "{name:?}");
        }
        assert_eq!(asks(Some(row(REQUEST, &"é".repeat(64)))).len(), 1);
        let nameless = json!([{ "id": REQUEST, "machine": "studio", "session": SESSION, "status": "pending" }]);
        assert!(asks(Some(nameless)).is_empty());
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
