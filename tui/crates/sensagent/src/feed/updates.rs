//! Releases and updates asked of this machine on rakumux.com (`contracts/feed/`
//! `release`, `machineUpdate`). Every replies read may say the newest stable
//! release rakumux.com knows of (`release`), which the service passes to TUIs
//! with their feed pages so they offer it at once. While the feed is on and
//! this machine says `remote_update` (it has an [`Updater`]), the read brings
//! the update asked of it (`updates`, at most one): the service reports it
//! `installing`, has the updater install that release and restart the server
//! and the local API in place (sessions keep running), reports `updated` or
//! `failed` with why, and once that's reported hands over to the new
//! account service.
//!
//! Each update is installed at most once: its id is kept as handled before the
//! updater starts, with the reports rakumux.com hasn't taken yet, across a
//! restart of the service (`updates.json`, 0600). Driven by the owner thread
//! with its own clock.

use std::collections::{BTreeMap, VecDeque};
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use super::wire::{self, FeedFailure, FeedRequest};
use crate::cloud::FeedHttpReply;
use crate::service::UpdateFailure;

/// What this machine says with its session list when it installs releases
/// asked of it on rakumux.com (`capabilities`).
pub(crate) const REMOTE_UPDATE: &str = "remote_update";
/// The first and longest waits after a failed report.
const RETRY_MIN: Duration = Duration::from_secs(5);
const RETRY_MAX: Duration = Duration::from_secs(120);
/// Ids are kept this long: rakumux.com expires an update 15 minutes on.
const KEEP_MS: i64 = 6 * 60 * 60 * 1000;
/// Reports not taken this long after they were decided are dropped.
const REPORT_KEEP_MS: i64 = 60 * 60 * 1000;
/// At most this many ids, the oldest dropped first.
const MAX_IDS: usize = 64;

/// `version` is a stable release's (`major.minor.patch`, digits only): the
/// contract's `rakumuxVersion`.
pub(crate) fn is_version(version: &str) -> bool {
    let parts: Vec<&str> = version.split('.').collect();
    parts.len() == 3
        && parts.iter().all(|part| {
            !part.is_empty()
                && part.len() <= 6
                && part.bytes().all(|byte| byte.is_ascii_digit())
                && (part.len() == 1 || !part.starts_with('0'))
        })
}

/// The replies read's `release`: its version, when it fits the contract.
pub(crate) fn release(value: Option<serde_json::Value>) -> Option<String> {
    #[derive(Deserialize)]
    struct Release {
        version: String,
    }
    let release: Release = serde_json::from_value(value?).ok()?;
    is_version(&release.version).then_some(release.version)
}

/// An update asked of this machine, as read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct MachineUpdateAsk {
    pub id: String,
    pub version: String,
}

/// The replies read's `updates`: the first one within the contract.
pub(crate) fn asks(value: Option<serde_json::Value>) -> Vec<MachineUpdateAsk> {
    #[derive(Deserialize)]
    struct Wire {
        id: String,
        version: String,
    }
    let Some(serde_json::Value::Array(rows)) = value else {
        return Vec::new();
    };
    rows.into_iter()
        .take(1)
        .filter_map(|row| serde_json::from_value::<Wire>(row).ok())
        .filter(|ask| crate::feed::is_entry_id(&ask.id) && is_version(&ask.version))
        .map(|ask| MachineUpdateAsk {
            id: ask.id,
            version: ask.version,
        })
        .collect()
}

/// What became of an update, as reported (contract `update-status`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub(crate) enum Outcome {
    Installing,
    Updated,
    Failed { failure: UpdateFailure },
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
    Some(FeedRequest::UpdateStatus { body })
}

/// The answer to a report: the stored update (changed or not).
pub(crate) fn reported(reply: &FeedHttpReply) -> Result<(), FeedFailure> {
    #[derive(Deserialize)]
    struct Answer {
        version: u8,
        update: Stored,
    }
    #[derive(Deserialize)]
    struct Stored {
        id: String,
    }
    if reply.status != 200 {
        return Err(wire::failure(reply));
    }
    let answer: Answer = serde_json::from_slice(&reply.body).map_err(|_| FeedFailure::Invalid)?;
    if answer.version != 1 || !crate::feed::is_entry_id(&answer.update.id) {
        return Err(FeedFailure::Invalid);
    }
    Ok(())
}

/// What to do next for updates.
#[derive(Debug, PartialEq)]
pub(crate) enum UpdateWork {
    /// Install `version` (and restart the server and API in place) for update `id`.
    Install { id: String, version: String },
    /// Report an update to rakumux.com.
    Report(FeedRequest),
    /// The update is installed and reported: start the new account service in
    /// this one's place.
    HandOver,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Report {
    id: String,
    outcome: Outcome,
    /// When it was decided (Unix ms).
    at: i64,
}

#[derive(Default, Serialize, Deserialize)]
struct KeptFile {
    version: u8,
    /// Updates handled here, by id: when (Unix ms).
    #[serde(default)]
    handled: BTreeMap<String, i64>,
    /// Reports not yet taken by rakumux.com, oldest first.
    #[serde(default)]
    reports: VecDeque<Report>,
}

/// Updates' state on the owner thread.
pub(crate) struct UpdateSync {
    machine: String,
    /// This machine installs releases asked of it (it has an updater).
    able: bool,
    enabled: bool,
    asked: Option<MachineUpdateAsk>,
    /// The update being installed.
    installing: Option<String>,
    /// Installed: hand over once its `updated` report is taken (or dropped).
    hand_over: Option<String>,
    path: Option<PathBuf>,
    kept: KeptFile,
    reporting: bool,
    retry_at: Duration,
    retry: Duration,
}

impl UpdateSync {
    /// Updates for `machine`; `able` when it has an updater; `kept` is the file
    /// that keeps which were handled across a restart.
    pub fn new(machine: String, able: bool, kept: Option<PathBuf>) -> Self {
        let file = kept
            .as_ref()
            .and_then(|path| std::fs::read(path).ok())
            .and_then(|bytes| serde_json::from_slice::<KeptFile>(&bytes).ok())
            .filter(|file| file.version == 1)
            .unwrap_or_default();
        Self {
            machine,
            able,
            enabled: false,
            asked: None,
            installing: None,
            hand_over: None,
            path: kept,
            kept: file,
            reporting: false,
            retry_at: Duration::ZERO,
            retry: RETRY_MIN,
        }
    }

    /// Follow the account: signed in with its feed on and this machine's name
    /// confirmed. Off, nothing is installed; reports wait for it to be on again.
    pub fn set_enabled(&mut self, enabled: bool, now: Duration) {
        if enabled == self.enabled {
            return;
        }
        self.enabled = enabled;
        self.asked = None;
        self.retry = RETRY_MIN;
        self.retry_at = now;
    }

    /// This machine goes by `machine` now.
    pub fn rename(&mut self, machine: String) {
        self.machine = machine;
        self.asked = None;
    }

    /// A replies read brought `asks`; `current` says whether it was made for
    /// the account still signed in.
    pub fn read(&mut self, current: bool, asks: Vec<MachineUpdateAsk>) {
        if current && self.enabled && self.able {
            self.asked = asks.into_iter().next();
        }
    }

    /// The next thing to do, if anything is due.
    pub fn next_work(&mut self, now: Duration) -> Option<UpdateWork> {
        if !self.enabled {
            return None;
        }
        if self.installing.is_none() && self.hand_over.is_none() {
            if let Some(ask) = self.asked.take().filter(|ask| !self.handled(&ask.id)) {
                // Never installed again from now on, whatever happens meanwhile.
                self.decided(&ask.id, Outcome::Installing);
                self.installing = Some(ask.id.clone());
                return Some(UpdateWork::Install {
                    id: ask.id,
                    version: ask.version,
                });
            }
        }
        if self.reporting || now < self.retry_at {
            return None;
        }
        while let Some(report) = self.kept.reports.front().cloned() {
            match status_request(&report.id, &self.machine, &report.outcome) {
                Some(request) => {
                    self.reporting = true;
                    return Some(UpdateWork::Report(request));
                }
                None => self.taken(),
            }
        }
        if self.hand_over.take().is_some() {
            return Some(UpdateWork::HandOver);
        }
        None
    }

    fn handled(&self, id: &str) -> bool {
        self.kept.handled.contains_key(id)
    }

    /// The updater finished update `id`.
    pub fn installed(&mut self, id: &str, result: Result<(), UpdateFailure>) {
        if self.installing.as_deref() == Some(id) {
            self.installing = None;
        }
        match result {
            Ok(()) => {
                self.decided(id, Outcome::Updated);
                self.hand_over = Some(id.to_owned());
            }
            Err(failure) => self.decided(id, Outcome::Failed { failure }),
        }
    }

    /// A report finished; `current` says whether it was made for the account
    /// still signed in.
    pub fn reported(&mut self, current: bool, outcome: Result<(), FeedFailure>, now: Duration) {
        self.reporting = false;
        if !current {
            return;
        }
        match outcome {
            Ok(()) | Err(FeedFailure::Invalid) => {
                self.retry = RETRY_MIN;
                self.taken();
            }
            Err(failure) => {
                let wait = match failure {
                    FeedFailure::RateLimited(Some(delay))
                    | FeedFailure::Unavailable(Some(delay)) => delay.min(RETRY_MAX),
                    _ => self.retry,
                };
                self.retry = self.retry.saturating_mul(2).min(RETRY_MAX);
                self.retry_at = now + wait;
            }
        }
    }

    fn decided(&mut self, id: &str, outcome: Outcome) {
        let now = now_ms();
        self.kept.handled.insert(id.to_owned(), now);
        self.kept.reports.push_back(Report {
            id: id.to_owned(),
            outcome,
            at: now,
        });
        self.save();
    }

    fn taken(&mut self) {
        self.kept.reports.pop_front();
        self.save();
    }

    fn save(&mut self) {
        let now = now_ms();
        let ids = &mut self.kept.handled;
        ids.retain(|_, at| now.saturating_sub(*at) < KEEP_MS);
        while ids.len() > MAX_IDS {
            let Some(oldest) = ids
                .iter()
                .min_by_key(|(_, at)| **at)
                .map(|(id, _)| id.clone())
            else {
                break;
            };
            ids.remove(&oldest);
        }
        self.kept
            .reports
            .retain(|report| now.saturating_sub(report.at) < REPORT_KEEP_MS);
        let Some(path) = &self.path else {
            return;
        };
        self.kept.version = 1;
        let Ok(bytes) = serde_json::to_vec(&self.kept) else {
            return;
        };
        if crate::feed::write_private(path, &bytes).is_err() {
            eprintln!("rakumux-status: saving which updates were installed failed");
        }
    }
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| {
            i64::try_from(since.as_millis()).unwrap_or(i64::MAX)
        })
}

#[cfg(test)]
mod tests;
