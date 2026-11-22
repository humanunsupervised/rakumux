//! Workspaces renamed or deleted from rakumux.com (`contracts/feed/`
//! `workspaceChange`): while the account's feed is on, the replies read
//! brings the changes pending for this machine (the cloud sends them only
//! because its session list says `workspaces.edit`), and the status service
//! carries each out through the daemon as the user: it renames the
//! workspace of that name, or deletes it (its sessions are then in none). It
//! then reports it `done`, or `failed` with why.
//!
//! A request names the workspace by its name: the one of exactly that name,
//! else of that name ignoring case; with none, it fails `not_found`. Each
//! request is carried out at most once: its id is kept as handled before
//! the daemon is asked, and with the reports rakumux.com hasn't taken yet,
//! across a restart of the service. Driven by the owner thread with its own
//! clock.

use std::path::PathBuf;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use session_core::api::ErrorCode;
use session_core::client::SessionApi;

use super::new_sessions::kept::Kept;
use super::wire::{self, FeedFailure, FeedRequest};
use crate::cloud::FeedHttpReply;
use crate::signing::{Expected, Refusal, SharedGuard, Signature, Signed};

/// What this machine says with its session list, beside `workspaces`, when
/// it renames and deletes the workspaces asked of it on rakumux.com.
pub(crate) const WORKSPACES_EDIT: &str = "workspaces.edit";
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

/// What's asked of a workspace.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ChangeAction {
    /// Give it this name.
    Rename(String),
    /// Forget it; its sessions are then in none.
    Delete,
}

impl ChangeAction {
    fn word(&self) -> &'static str {
        match self {
            ChangeAction::Rename(_) => "rename",
            ChangeAction::Delete => "delete",
        }
    }

    fn name(&self) -> serde_json::Value {
        match self {
            ChangeAction::Rename(name) => name.as_str().into(),
            ChangeAction::Delete => serde_json::Value::Null,
        }
    }
}

/// A change asked of this machine, as read: pending, within the contract.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ChangeAsk {
    pub id: String,
    pub machine: String,
    /// The workspace's name now.
    pub workspace: String,
    pub action: ChangeAction,
    /// What a paired device signed for it.
    pub signed: Signature,
}

#[derive(Deserialize)]
struct WireAsk {
    id: String,
    machine: String,
    workspace: String,
    action: String,
    #[serde(default)]
    name: Option<String>,
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
    fn checked(self) -> Option<ChangeAsk> {
        let action = match (self.action.as_str(), self.name) {
            ("rename", Some(name)) if is_name(&name) => ChangeAction::Rename(name),
            ("delete", None) => ChangeAction::Delete,
            _ => return None,
        };
        let valid = self.status == "pending"
            && crate::feed::is_entry_id(&self.id)
            && is_name(&self.workspace);
        valid.then(|| ChangeAsk {
            id: self.id,
            machine: self.machine,
            workspace: self.workspace,
            action,
            signed: Signed::of(self.signed),
        })
    }
}

/// The replies read's `workspace_changes`: those within the contract, at
/// most [`MAX_ASKED`]; a row off the contract is left out on its own, never
/// the read (it stays pending, and expires).
pub(crate) fn asks(value: Option<serde_json::Value>) -> Vec<ChangeAsk> {
    let Some(serde_json::Value::Array(rows)) = value else {
        return Vec::new();
    };
    rows.into_iter()
        .take(MAX_ASKED)
        .filter_map(|row| serde_json::from_value::<WireAsk>(row).ok()?.checked())
        .collect()
}

/// Why this machine didn't change a workspace (contract
/// `workspaceChangeFailure`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ChangeFailure {
    /// It has no workspace of that name.
    NotFound,
    /// Another of its workspaces has the new name.
    NameTaken,
    /// The daemon couldn't.
    Refused,
    /// Its signature (contract `signatureFailure`).
    Unsigned,
    UnknownDevice,
    BadSignature,
    Expired,
}

impl From<Refusal> for ChangeFailure {
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
/// `workspace-change-status`, whose `status` tags it).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub(crate) enum Outcome {
    Done,
    Failed { failure: ChangeFailure },
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
    Some(FeedRequest::WorkspaceChangeStatus { body })
}

#[derive(Deserialize)]
struct StatusAnswer {
    version: u8,
    workspace_change: StoredId,
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
    if answer.version != 1 || !crate::feed::is_entry_id(&answer.workspace_change.id) {
        return Err(FeedFailure::Invalid);
    }
    Ok(())
}

/// What to do next for workspace changes.
#[derive(Debug, PartialEq)]
pub(crate) enum ChangeWork {
    /// Ask the daemon to rename or delete the workspace named `workspace`
    /// for request `id`.
    Change {
        id: String,
        workspace: String,
        action: ChangeAction,
    },
    /// Report a request to rakumux.com.
    Report(FeedRequest),
}

/// Workspace changes' state on the owner thread.
pub(crate) struct WorkspaceChangeSync {
    machine: String,
    enabled: bool,
    /// The requests pending for this machine, as last read, oldest first.
    asked: Vec<ChangeAsk>,
    /// The request the daemon is carrying out.
    changing: Option<String>,
    kept: Kept<Outcome>,
    /// A report is out.
    reporting: bool,
    retry_at: Duration,
    retry: Duration,
    /// Checks what's asked against the devices paired here.
    guard: SharedGuard,
}

impl WorkspaceChangeSync {
    /// Changes for `machine`; `kept` is the file that keeps which were
    /// handled across a restart (memory only without it).
    pub fn new(machine: String, kept: Option<PathBuf>) -> Self {
        Self {
            machine,
            enabled: false,
            asked: Vec::new(),
            changing: None,
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

    /// Follow the account: signed in with its feed on, this machine's name
    /// confirmed, and a daemon that renames workspaces. Off, nothing is
    /// changed; reports not yet taken are kept for when it's on again.
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

    /// A replies read brought `asks`, the requests pending for this machine
    /// now; `current` says whether it was made for the account still signed in.
    pub fn read(&mut self, current: bool, asks: Vec<ChangeAsk>) {
        if !current || !self.enabled {
            return;
        }
        self.asked = asks
            .into_iter()
            .filter(|ask| ask.machine == self.machine)
            .collect();
    }

    /// The next thing to do, if anything is due: decide the requests read
    /// (carry out one at a time; fail those whose signature doesn't hold),
    /// then report.
    pub fn next_work(&mut self, now: Duration) -> Option<ChangeWork> {
        if !self.enabled {
            return None;
        }
        if self.changing.is_none() {
            let waiting: Vec<ChangeAsk> = self
                .asked
                .iter()
                .filter(|ask| !self.kept.handled(&ask.id))
                .cloned()
                .collect();
            for ask in waiting {
                let expected = Expected {
                    kind: "workspace_change",
                    id: &ask.id,
                    fields: vec![
                        ("workspace", ask.workspace.as_str().into()),
                        ("action", ask.action.word().into()),
                        ("name", ask.action.name()),
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
                // Never asked of the daemon again from now on, even if this
                // service stops before it answers.
                self.kept.creating(&ask.id);
                self.changing = Some(ask.id.clone());
                return Some(ChangeWork::Change {
                    id: ask.id,
                    workspace: ask.workspace,
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
                    return Some(ChangeWork::Report(request));
                }
                // An id the contract can't carry: nothing to tell.
                None => self.kept.reported(),
            }
        }
        None
    }

    /// The daemon carried out request `id` (or couldn't).
    pub fn changed(&mut self, id: &str, result: Result<(), ChangeFailure>) {
        if self.changing.as_deref() == Some(id) {
            self.changing = None;
        }
        let outcome = match result {
            Ok(()) => Outcome::Done,
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

/// Rename or delete, through the daemon as the user, the workspace named
/// `workspace` (the one of exactly that name, else of that name ignoring
/// case).
pub(crate) fn carry_out(
    api: &dyn SessionApi,
    workspace: &str,
    action: &ChangeAction,
) -> Result<(), ChangeFailure> {
    let known = api.workspaces().map_err(|_| ChangeFailure::Refused)?;
    let found = known
        .iter()
        .find(|known| known.name == workspace)
        .or_else(|| {
            known
                .iter()
                .find(|known| known.name.to_lowercase() == workspace.to_lowercase())
        })
        .ok_or(ChangeFailure::NotFound)?;
    let result = match action {
        ChangeAction::Rename(name) => api.workspace_rename(&found.id, name).map(|_| ()),
        ChangeAction::Delete => api.workspace_delete(&found.id),
    };
    result.map_err(|error| match error.code {
        ErrorCode::NotFound => ChangeFailure::NotFound,
        ErrorCode::InvalidArgument => ChangeFailure::NameTaken,
        _ => ChangeFailure::Refused,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const REQUEST: &str = "01K8A2B3C4D5E6F7G8H9J0K1M2";
    const NOW: Duration = Duration::from_secs(100);

    fn row(
        workspace: &str,
        action: &str,
        name: Option<&str>,
        signed: serde_json::Value,
    ) -> serde_json::Value {
        let mut row = json!({
            "id": REQUEST, "machine": "studio", "workspace": workspace, "action": action,
            "status": "pending", "created_at": 1, "status_at": 1, "signed": signed,
        });
        if let Some(name) = name {
            row["name"] = name.into();
        }
        json!([row])
    }

    fn signed(workspace: &str, action: &str, name: Option<&str>) -> serde_json::Value {
        crate::signing::testing::signed_json(
            "workspace_change",
            REQUEST,
            json!({ "workspace": workspace, "action": action, "name": name }),
        )
    }

    fn sync(action: &str, name: Option<&str>) -> WorkspaceChangeSync {
        let mut sync = WorkspaceChangeSync::new("studio".into(), None)
            .signing(crate::signing::testing::paired());
        sync.set_enabled(true, Duration::ZERO);
        let signature = signed("Launch", action, name);
        sync.read(true, asks(Some(row("Launch", action, name, signature))));
        sync
    }

    fn report_of(work: Option<ChangeWork>) -> serde_json::Value {
        match work {
            Some(ChangeWork::Report(FeedRequest::WorkspaceChangeStatus { body })) => {
                serde_json::from_slice(&body).expect("json")
            }
            other => panic!("expected a report, got {other:?}"),
        }
    }

    #[test]
    fn only_the_change_a_paired_device_signed_is_carried_out() {
        use crate::signing::testing::{guard, Device};
        use crate::signing::KeyFormat;
        const MACHINE_ID: &str = "01K8C2B3C4D5E6F7G8H9J0K1M2";
        let phone = Device::new(KeyFormat::P256);
        let run = |action: &str, name: Option<&str>, signed: serde_json::Value| {
            let mut sync = WorkspaceChangeSync::new("studio".into(), None)
                .signing(guard(MACHINE_ID, &[&phone]));
            sync.set_enabled(true, Duration::ZERO);
            sync.read(true, asks(Some(row("Launch", action, name, signed))));
            sync.next_work(NOW)
        };
        let signed = |workspace: &str, action: &str, name: Option<&str>| {
            phone.signed_now(
                "workspace_change",
                MACHINE_ID,
                REQUEST,
                json!({ "workspace": workspace, "action": action, "name": name }),
            )
        };
        assert!(matches!(
            run(
                "rename",
                Some("Ship"),
                signed("Launch", "rename", Some("Ship"))
            ),
            Some(ChangeWork::Change { .. })
        ));
        assert!(matches!(
            run("delete", None, signed("Launch", "delete", None)),
            Some(ChangeWork::Change { .. })
        ));
        let refused = |work| report_of(work)["failure"].clone();
        // Signed for another name, another workspace or another action.
        assert_eq!(
            refused(run(
                "rename",
                Some("Ship"),
                signed("Launch", "rename", Some("Other"))
            )),
            "bad_signature"
        );
        assert_eq!(
            refused(run("delete", None, signed("Research", "delete", None))),
            "bad_signature"
        );
        assert_eq!(
            refused(run(
                "delete",
                None,
                signed("Launch", "rename", Some("Ship"))
            )),
            "bad_signature"
        );
        assert_eq!(
            refused(run("delete", None, serde_json::Value::Null)),
            "unsigned"
        );
    }

    #[test]
    fn a_change_is_carried_out_once_and_reported() {
        let mut sync = sync("rename", Some("Ship"));
        assert_eq!(
            sync.next_work(NOW),
            Some(ChangeWork::Change {
                id: REQUEST.into(),
                workspace: "Launch".into(),
                action: ChangeAction::Rename("Ship".into()),
            })
        );
        // Read again while the daemon works: never asked twice.
        let signature = signed("Launch", "rename", Some("Ship"));
        sync.read(
            true,
            asks(Some(row(
                "Launch",
                "rename",
                Some("Ship"),
                signature.clone(),
            ))),
        );
        assert_eq!(sync.next_work(NOW), None);
        sync.changed(REQUEST, Ok(()));
        let report = report_of(sync.next_work(NOW));
        assert_eq!(report["status"], "done");
        assert_eq!(report["id"], REQUEST);
        assert_eq!(report["machine"], "studio");
        sync.reported(true, Ok(()), NOW);
        sync.read(
            true,
            asks(Some(row("Launch", "rename", Some("Ship"), signature))),
        );
        assert_eq!(sync.next_work(NOW), None);
    }

    #[test]
    fn what_the_daemon_says_is_reported() {
        for (failure, said) in [
            (ChangeFailure::NotFound, "not_found"),
            (ChangeFailure::NameTaken, "name_taken"),
            (ChangeFailure::Refused, "refused"),
        ] {
            let mut sync = sync("delete", None);
            sync.next_work(NOW);
            sync.changed(REQUEST, Err(failure));
            let report = report_of(sync.next_work(NOW));
            assert_eq!(report["status"], "failed");
            assert_eq!(report["failure"], said);
        }
    }

    #[test]
    fn another_machine_and_rows_off_the_contract_are_left_out() {
        let mut sync = WorkspaceChangeSync::new("laptop".into(), None);
        sync.set_enabled(true, Duration::ZERO);
        sync.read(
            true,
            asks(Some(row(
                "Launch",
                "delete",
                None,
                signed("Launch", "delete", None),
            ))),
        );
        assert_eq!(sync.next_work(NOW), None);
        let none = serde_json::Value::Null;
        assert!(asks(Some(row("Launch", "rename", None, none.clone()))).is_empty());
        assert!(asks(Some(row("Launch", "delete", Some("x"), none.clone()))).is_empty());
        assert!(asks(Some(row("Launch", "merge", None, none.clone()))).is_empty());
        assert!(asks(Some(row("Launch ", "delete", None, none.clone()))).is_empty());
        assert!(asks(Some(row("Launch", "rename", Some("a\nb"), none.clone()))).is_empty());
        assert_eq!(
            asks(Some(row("Launch", "rename", Some("Ship"), none))).len(),
            1
        );
    }

    #[test]
    fn a_retried_report_waits() {
        let mut sync = sync("delete", None);
        sync.next_work(NOW);
        sync.changed(REQUEST, Err(ChangeFailure::NotFound));
        report_of(sync.next_work(NOW));
        sync.reported(true, Err(FeedFailure::Unavailable(None)), NOW);
        assert_eq!(sync.next_work(NOW), None);
        report_of(sync.next_work(NOW + RETRY_MIN));
    }
}
