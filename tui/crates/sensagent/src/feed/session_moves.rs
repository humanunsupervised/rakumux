//! Sessions moved between workspaces from rakumux.com (`contracts/feed/`
//! `sessionMove`): while the account's feed is on, the replies read brings
//! the requests pending for this machine (the cloud sends them only because
//! its session list says `workspaces`), and the status service moves each
//! session through the daemon as the user: into the workspace of that name
//! (made when the machine has none), or out of any. It then reports it
//! `moved`, or `failed` with why.
//!
//! A request names a session by its id, and only one this machine lists to
//! rakumux.com is moved (live or exited, never private or another
//! machine's); any other fails `not_listed`. Each request is carried out at
//! most once: its id is kept as handled before the daemon is asked, and with
//! the reports rakumux.com hasn't taken yet, across a restart of the
//! service. Driven by the owner thread with its own clock.

use std::collections::HashSet;
use std::path::PathBuf;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use session_core::api::ErrorCode;
use session_core::client::SessionApi;
use session_core::ids::SessionId;
use session_core::model::SessionView;

use super::new_sessions::kept::Kept;
use super::new_sessions::listed_here;
use super::wire::{self, FeedFailure, FeedRequest};
use crate::cloud::FeedHttpReply;
use crate::signing::{Expected, Refusal, SharedGuard, Signature, Signed};

/// What this machine says with its session list when it lists its
/// workspaces and moves sessions asked of it on rakumux.com between them
/// (`capabilities`), so the cloud sends it those.
pub(crate) const WORKSPACES: &str = "workspaces";
/// The most requests one read carries (contract).
const MAX_ASKED: usize = 20;
/// The longest workspace name, in characters (contract `workspaceName`).
const MAX_NAME_CHARS: usize = 64;
/// The first and longest waits after a failed report.
const RETRY_MIN: Duration = Duration::from_secs(5);
const RETRY_MAX: Duration = Duration::from_secs(300);
/// Reports rakumux.com asks to hold back wait at most this long: a request
/// expires 10 minutes after it was written.
const RATE_LIMIT_MAX: Duration = Duration::from_secs(60);

/// A move asked of this machine, as read: pending, within the contract.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct MoveAsk {
    pub id: String,
    pub machine: String,
    pub session: SessionId,
    /// The workspace to move it to; `None` to take it out of any.
    pub workspace: Option<String>,
    /// What a paired device signed for it.
    pub signed: Signature,
}

#[derive(Deserialize)]
struct WireAsk {
    id: String,
    machine: String,
    session: String,
    #[serde(default)]
    workspace: Option<String>,
    status: String,
    #[serde(default)]
    signed: Option<serde_json::Value>,
}

/// Whether `name` is a workspace name as the contract and the daemon take
/// it: one line, trimmed, 1 to 64 characters, no controls.
fn is_name(name: &str) -> bool {
    let count = name.chars().count();
    name.trim() == name
        && (1..=MAX_NAME_CHARS).contains(&count)
        && !name.chars().any(char::is_control)
}

impl WireAsk {
    fn checked(self) -> Option<MoveAsk> {
        let valid = self.status == "pending"
            && crate::feed::is_entry_id(&self.id)
            && crate::feed::is_entry_id(&self.session)
            && self.workspace.as_deref().is_none_or(is_name);
        valid.then(|| MoveAsk {
            id: self.id,
            machine: self.machine,
            session: SessionId::from_string(self.session),
            workspace: self.workspace,
            signed: Signed::of(self.signed),
        })
    }
}

/// The replies read's `session_moves`: those within the contract, at most
/// [`MAX_ASKED`]; a row off the contract is left out on its own, never the
/// read (it stays pending, and expires).
pub(crate) fn asks(value: Option<serde_json::Value>) -> Vec<MoveAsk> {
    let Some(serde_json::Value::Array(rows)) = value else {
        return Vec::new();
    };
    rows.into_iter()
        .take(MAX_ASKED)
        .filter_map(|row| serde_json::from_value::<WireAsk>(row).ok()?.checked())
        .collect()
}

/// Why this machine didn't move a session (contract `sessionMoveFailure`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum MoveFailure {
    /// This machine doesn't list that session (gone already, or private).
    NotListed,
    /// The daemon couldn't move it.
    Refused,
    /// Its signature (contract `signatureFailure`).
    Unsigned,
    UnknownDevice,
    BadSignature,
    Expired,
}

impl From<Refusal> for MoveFailure {
    fn from(refusal: Refusal) -> Self {
        match refusal {
            Refusal::Unsigned => Self::Unsigned,
            Refusal::UnknownDevice => Self::UnknownDevice,
            Refusal::BadSignature => Self::BadSignature,
            Refusal::Expired => Self::Expired,
        }
    }
}

/// What became of a request, as reported (contract
/// `session-move-status`, whose `status` tags it).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub(crate) enum Outcome {
    Moved,
    Failed { failure: MoveFailure },
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
    Some(FeedRequest::SessionMoveStatus { body })
}

#[derive(Deserialize)]
struct StatusAnswer {
    version: u8,
    session_move: StoredId,
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
    if answer.version != 1 || !crate::feed::is_entry_id(&answer.session_move.id) {
        return Err(FeedFailure::Invalid);
    }
    Ok(())
}

/// What to do next for moves.
#[derive(Debug, PartialEq)]
pub(crate) enum MoveWork {
    /// Ask the daemon to put `session` in the workspace named `workspace`
    /// (made when there's none), or in none, for request `id`.
    Move {
        id: String,
        session: SessionId,
        workspace: Option<String>,
    },
    /// Report a request to rakumux.com.
    Report(FeedRequest),
}

/// Moves' state on the owner thread.
pub(crate) struct SessionMoveSync {
    machine: String,
    enabled: bool,
    /// The sessions this machine lists to rakumux.com.
    listed: HashSet<SessionId>,
    /// The requests pending for this machine, as last read, oldest first.
    asked: Vec<MoveAsk>,
    /// The request whose session the daemon is moving.
    moving: Option<String>,
    kept: Kept<Outcome>,
    /// A report is out.
    reporting: bool,
    retry_at: Duration,
    retry: Duration,
    /// Checks what's asked against the devices paired here.
    guard: SharedGuard,
}

impl SessionMoveSync {
    /// Moves for `machine`; `kept` is the file that keeps which were
    /// handled across a restart (memory only without it).
    pub fn new(machine: String, kept: Option<PathBuf>) -> Self {
        Self {
            machine,
            enabled: false,
            listed: HashSet::new(),
            asked: Vec::new(),
            moving: None,
            kept: Kept::load(kept),
            reporting: false,
            retry_at: Duration::ZERO,
            retry: RETRY_MIN,
            guard: crate::signing::unpaired(),
        }
    }

    /// Check requests with the service's `guard`.
    pub fn signing(mut self, guard: SharedGuard) -> Self {
        self.guard = guard;
        self
    }

    /// Follow the account: signed in with its feed on and this machine's
    /// name confirmed. Off, nothing is moved; reports not yet taken are
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
    pub fn read(&mut self, current: bool, asks: Vec<MoveAsk>) {
        if !current || !self.enabled {
            return;
        }
        self.asked = asks
            .into_iter()
            .filter(|ask| ask.machine == self.machine)
            .collect();
    }

    /// The next thing to do, if anything is due: decide the requests read
    /// (move one at a time; fail those that can't be), then report.
    pub fn next_work(&mut self, now: Duration) -> Option<MoveWork> {
        if !self.enabled {
            return None;
        }
        if self.moving.is_none() {
            let waiting: Vec<MoveAsk> = self
                .asked
                .iter()
                .filter(|ask| !self.kept.handled(&ask.id))
                .cloned()
                .collect();
            for ask in waiting {
                let expected = Expected {
                    kind: "move_session",
                    id: &ask.id,
                    fields: vec![
                        ("session", ask.session.as_str().into()),
                        (
                            "workspace",
                            ask.workspace
                                .as_deref()
                                .map_or(serde_json::Value::Null, Into::into),
                        ),
                    ],
                };
                let allowed = self.guard.borrow_mut().allows(
                    &ask.signed,
                    &expected,
                    crate::signing::now_ms(),
                );
                if let Err(refusal) = allowed {
                    self.kept.decided(
                        &ask.id,
                        Outcome::Failed {
                            failure: refusal.into(),
                        },
                    );
                    continue;
                }
                if !self.listed.contains(&ask.session) {
                    self.kept.decided(
                        &ask.id,
                        Outcome::Failed {
                            failure: MoveFailure::NotListed,
                        },
                    );
                    continue;
                }
                // Never asked of the daemon again from now on, even if this
                // service stops before it answers.
                self.kept.creating(&ask.id);
                self.moving = Some(ask.id.clone());
                return Some(MoveWork::Move {
                    id: ask.id,
                    session: ask.session,
                    workspace: ask.workspace,
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
                    return Some(MoveWork::Report(request));
                }
                // An id the contract can't carry: nothing to tell.
                None => self.kept.reported(),
            }
        }
        None
    }

    /// The daemon moved the session for request `id` (or refused).
    pub fn moved(&mut self, id: &str, result: Result<(), ErrorCode>) {
        if self.moving.as_deref() == Some(id) {
            self.moving = None;
        }
        let outcome = match result {
            Ok(()) => Outcome::Moved,
            Err(ErrorCode::NotFound) => Outcome::Failed {
                failure: MoveFailure::NotListed,
            },
            Err(_) => Outcome::Failed {
                failure: MoveFailure::Refused,
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
                self.retry_at = now + crate::jitter::later(wait);
            }
        }
    }
}

/// Move `session` through the daemon as the user: into the workspace named
/// `workspace` (the one of exactly that name, else of that name ignoring
/// case, else a new one), or out of any. The daemon's error code when it
/// can't.
pub(crate) fn carry_out(
    api: &dyn SessionApi,
    session: &SessionId,
    workspace: Option<&str>,
) -> Result<(), ErrorCode> {
    let target = match workspace {
        None => None,
        Some(name) => {
            let known = api.workspaces().map_err(|error| error.code)?;
            let found = known
                .iter()
                .find(|known| known.name == name)
                .or_else(|| {
                    known
                        .iter()
                        .find(|known| known.name.to_lowercase() == name.to_lowercase())
                })
                .map(|known| known.id.clone());
            match found {
                Some(id) => Some(id),
                None => Some(api.workspace_create(name).map_err(|error| error.code)?.id),
            }
        }
    };
    api.set_workspace(session, target.as_ref())
        .map(|_| ())
        .map_err(|error| error.code)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const REQUEST: &str = "01K8A2B3C4D5E6F7G8H9J0K1M2";
    const SESSION: &str = "01K89ZQ4D2F6H8J0K2M4N6P8R0";
    const NOW: Duration = Duration::from_secs(100);

    fn row(workspace: Option<&str>, signed: serde_json::Value) -> serde_json::Value {
        let mut row = json!({
            "id": REQUEST, "machine": "studio", "session": SESSION,
            "status": "pending", "created_at": 1, "status_at": 1, "signed": signed,
        });
        if let Some(workspace) = workspace {
            row["workspace"] = workspace.into();
        }
        json!([row])
    }

    fn signed(workspace: Option<&str>) -> serde_json::Value {
        crate::signing::testing::signed_json(
            "move_session",
            REQUEST,
            json!({ "session": SESSION, "workspace": workspace }),
        )
    }

    fn sync(listed: bool, workspace: Option<&str>) -> SessionMoveSync {
        let mut sync =
            SessionMoveSync::new("studio".into(), None).signing(crate::signing::testing::paired());
        sync.set_enabled(true, Duration::ZERO);
        if listed {
            sync.listed
                .insert(SessionId::from_string(SESSION.to_owned()));
        }
        sync.read(true, asks(Some(row(workspace, signed(workspace)))));
        sync
    }

    fn report_of(work: Option<MoveWork>) -> serde_json::Value {
        match work {
            Some(MoveWork::Report(FeedRequest::SessionMoveStatus { body })) => {
                serde_json::from_slice(&body).expect("json")
            }
            other => panic!("expected a report, got {other:?}"),
        }
    }

    #[test]
    fn only_the_workspace_a_paired_device_signed_is_given() {
        use crate::signing::testing::{guard, Device};
        use crate::signing::KeyFormat;
        const MACHINE_ID: &str = "01K8C2B3C4D5E6F7G8H9J0K1M2";
        let phone = Device::new(KeyFormat::P256);
        let run = |workspace: Option<&str>, signed: serde_json::Value| {
            let mut sync =
                SessionMoveSync::new("studio".into(), None).signing(guard(MACHINE_ID, &[&phone]));
            sync.set_enabled(true, Duration::ZERO);
            sync.listed
                .insert(SessionId::from_string(SESSION.to_owned()));
            sync.read(true, asks(Some(row(workspace, signed))));
            sync.next_work(NOW)
        };
        let signed = |workspace: Option<&str>| {
            phone.signed_now(
                "move_session",
                MACHINE_ID,
                REQUEST,
                json!({ "session": SESSION, "workspace": workspace }),
            )
        };
        assert!(matches!(
            run(Some("Launch"), signed(Some("Launch"))),
            Some(MoveWork::Move { .. })
        ));
        assert!(matches!(
            run(None, signed(None)),
            Some(MoveWork::Move { .. })
        ));
        let refused = |work| report_of(work)["failure"].clone();
        // Signed for another workspace, or for none: not this move.
        assert_eq!(
            refused(run(Some("Launch"), signed(Some("Other")))),
            "bad_signature"
        );
        assert_eq!(refused(run(Some("Launch"), signed(None))), "bad_signature");
        assert_eq!(refused(run(None, signed(Some("Launch")))), "bad_signature");
        assert_eq!(
            refused(run(Some("Launch"), serde_json::Value::Null)),
            "unsigned"
        );
    }

    #[test]
    fn a_listed_session_is_moved_once_and_reported() {
        let mut sync = sync(true, Some("Launch"));
        assert_eq!(
            sync.next_work(NOW),
            Some(MoveWork::Move {
                id: REQUEST.into(),
                session: SessionId::from_string(SESSION.to_owned()),
                workspace: Some("Launch".into()),
            })
        );
        // Read again while the daemon works: never asked twice.
        sync.read(
            true,
            asks(Some(row(Some("Launch"), signed(Some("Launch"))))),
        );
        assert_eq!(sync.next_work(NOW), None);
        sync.moved(REQUEST, Ok(()));
        let report = report_of(sync.next_work(NOW));
        assert_eq!(report["status"], "moved");
        assert_eq!(report["id"], REQUEST);
        assert_eq!(report["machine"], "studio");
        sync.reported(true, Ok(()), NOW);
        sync.read(
            true,
            asks(Some(row(Some("Launch"), signed(Some("Launch"))))),
        );
        assert_eq!(sync.next_work(NOW), None);
    }

    #[test]
    fn out_of_any_workspace_is_a_move_too() {
        let mut sync = sync(true, None);
        assert_eq!(
            sync.next_work(NOW),
            Some(MoveWork::Move {
                id: REQUEST.into(),
                session: SessionId::from_string(SESSION.to_owned()),
                workspace: None,
            })
        );
    }

    #[test]
    fn a_session_not_listed_and_the_daemon_refusing_are_reported() {
        let report = report_of(sync(false, Some("Launch")).next_work(NOW));
        assert_eq!(report["status"], "failed");
        assert_eq!(report["failure"], "not_listed");
        let mut gone = sync(true, Some("Launch"));
        gone.next_work(NOW);
        gone.moved(REQUEST, Err(ErrorCode::NotFound));
        assert_eq!(report_of(gone.next_work(NOW))["failure"], "not_listed");
        let mut refused = sync(true, Some("Launch"));
        refused.next_work(NOW);
        refused.moved(REQUEST, Err(ErrorCode::LimitExceeded));
        assert_eq!(report_of(refused.next_work(NOW))["failure"], "refused");
    }

    #[test]
    fn another_machine_and_rows_off_the_contract_are_left_out() {
        let mut sync = SessionMoveSync::new("laptop".into(), None);
        sync.set_enabled(true, Duration::ZERO);
        sync.read(
            true,
            asks(Some(row(Some("Launch"), signed(Some("Launch"))))),
        );
        assert_eq!(sync.next_work(NOW), None);
        for name in [
            "",
            " fix",
            "fix ",
            "fix\nlogin",
            "a\u{1b}[31m",
            &"x".repeat(65),
        ] {
            assert!(
                asks(Some(row(Some(name), serde_json::Value::Null))).is_empty(),
                "{name:?}"
            );
        }
        assert_eq!(
            asks(Some(row(Some(&"é".repeat(64)), serde_json::Value::Null))).len(),
            1
        );
        let bad_id =
            json!([{ "id": "nope", "machine": "studio", "session": SESSION, "status": "pending" }]);
        assert!(asks(Some(bad_id)).is_empty());
    }

    #[test]
    fn a_retried_report_waits() {
        let mut sync = sync(false, Some("Launch"));
        report_of(sync.next_work(NOW));
        sync.reported(true, Err(FeedFailure::Unavailable(None)), NOW);
        assert_eq!(sync.next_work(NOW), None);
        report_of(sync.next_work(NOW + RETRY_MIN));
    }
}
