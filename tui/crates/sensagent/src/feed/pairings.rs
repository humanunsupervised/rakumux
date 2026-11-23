//! Pairing devices with this machine (contract `contracts/feed/`, "Pairing";
//! [`crate::signing::pairing`]): a TUI or `rakumux devices pair` opens a
//! pairing here, which shows its code and link; the service opens it at
//! rakumux.com under the code's first group (`POST /api/v1/feed/pairings`,
//! another code when that one is taken), reads the replies read's
//! `pairings` every couple of seconds meanwhile, hands the device proposed
//! with the right tag to the user to confirm, keeps it once they do, and
//! reports what became of the proposal (`POST /api/v1/feed/pairings/status`).
//! Driven by the owner thread with its own clock; memory only.

use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::wire::{self, FeedFailure, FeedRequest};
use crate::cloud::FeedHttpReply;
use crate::signing::pairing::{Pairing, PairingFailure, PairingState, Proposal};
use crate::signing::{KeyFormat, SharedGuard};

/// The first and longest waits after a failed request.
const RETRY_MIN: Duration = Duration::from_secs(2);
const RETRY_MAX: Duration = Duration::from_secs(30);
/// How often replies are read while a pairing waits for its device.
pub(crate) const READ_EVERY: Duration = Duration::from_secs(2);

/// A pairing as a TUI or the CLI shows it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairingView {
    /// The code to type or scan (`ABCD-EFGH-JKMN`).
    pub code: String,
    /// The signer site's page for it, the code after `#`.
    pub link: String,
    /// When the code stops working (Unix ms).
    pub expires_at: i64,
    /// Where it is up to.
    pub state: PairingState,
    /// rakumux.com took it: a device can be sent for it now.
    pub opened: bool,
}

/// A device paired with this machine, as a TUI or the CLI lists it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceView {
    pub key_id: String,
    pub format: KeyFormat,
    pub name: String,
    pub fingerprint: String,
    pub paired_at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_used_at: Option<i64>,
}

/// What the user does with a pairing.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PairingAction {
    /// Open a new one (closing any other).
    Start,
    /// Where the current one is.
    Show,
    /// Pair the device proposed (`true`) or not.
    Answer { yes: bool },
    /// Close it.
    Cancel,
}

/// Where opening the pairing at rakumux.com is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Opening {
    Due,
    Out,
    Opened,
}

/// Pairings' state on the owner thread.
pub(crate) struct PairingSync {
    machine: String,
    enabled: bool,
    guard: SharedGuard,
    pairing: Option<Pairing>,
    opening: Opening,
    /// A report is out.
    reporting: bool,
    retry_at: Duration,
    retry: Duration,
}

#[derive(Serialize)]
struct OpenBody<'a> {
    version: u8,
    machine: &'a str,
    pairing: &'a str,
    expires_at: i64,
}

#[derive(Serialize)]
struct StatusBody<'a> {
    version: u8,
    machine: &'a str,
    pairing: &'a str,
    proposal: &'a str,
    #[serde(flatten)]
    outcome: &'a crate::signing::pairing::PairingOutcome,
}

impl PairingSync {
    pub fn new(machine: String, guard: SharedGuard) -> Self {
        Self {
            machine,
            enabled: false,
            guard,
            pairing: None,
            opening: Opening::Due,
            reporting: false,
            retry_at: Duration::ZERO,
            retry: RETRY_MIN,
        }
    }

    /// Follow the account: signed in with its feed on and this machine's
    /// name confirmed. Off, a pairing waits to be opened.
    pub fn set_enabled(&mut self, enabled: bool) {
        if enabled != self.enabled {
            self.enabled = enabled;
            if self.opening == Opening::Opened {
                self.opening = Opening::Due;
            }
        }
    }

    /// This machine goes by `machine` now: a pairing opened under the old
    /// name is opened again.
    pub fn rename(&mut self, machine: String) {
        self.machine = machine;
        self.opening = Opening::Due;
    }

    /// What the user did, at `now_ms`; the pairing as it is then.
    pub fn act(&mut self, action: PairingAction, now_ms: i64) -> Option<PairingView> {
        match action {
            PairingAction::Start => {
                if let Some(pairing) = &mut self.pairing {
                    pairing.cancel();
                }
                self.pairing = Some(Pairing::open(now_ms));
                self.opening = Opening::Due;
                self.retry = RETRY_MIN;
                self.retry_at = Duration::ZERO;
            }
            PairingAction::Show => {}
            PairingAction::Answer { yes } => {
                if let Some(pairing) = &mut self.pairing {
                    pairing.confirm(yes, &mut self.guard.borrow_mut().devices, now_ms);
                }
            }
            PairingAction::Cancel => {
                if let Some(pairing) = &mut self.pairing {
                    pairing.cancel();
                }
            }
        }
        self.view(now_ms)
    }

    /// The pairing as it is at `now_ms`.
    pub fn view(&mut self, now_ms: i64) -> Option<PairingView> {
        let origin = self.guard.borrow().origin().to_owned();
        let pairing = self.pairing.as_mut()?;
        pairing.tick(now_ms);
        Some(PairingView {
            code: pairing.code(),
            link: pairing.link(&origin),
            expires_at: pairing.expires_ms(),
            state: pairing.state().clone(),
            opened: self.opening == Opening::Opened && pairing.live(),
        })
    }

    /// Whether replies should be read often: a pairing waits for its device.
    pub fn waiting(&self) -> bool {
        self.enabled
            && self.opening == Opening::Opened
            && self
                .pairing
                .as_ref()
                .is_some_and(|pairing| *pairing.state() == PairingState::Open)
    }

    /// A replies read brought `proposals`; `current` says whether it was made
    /// for the account still signed in.
    pub fn read(&mut self, current: bool, proposals: &[Proposal], now_ms: i64) {
        if !current || !self.enabled {
            return;
        }
        if let Some(pairing) = &mut self.pairing {
            for proposal in proposals {
                pairing.propose(proposal, now_ms);
            }
        }
    }

    /// The request due now, if any: open the pairing, then report.
    pub fn next_work(&mut self, now: Duration, now_ms: i64) -> Option<FeedRequest> {
        if !self.enabled || now < self.retry_at {
            return None;
        }
        let pairing = self.pairing.as_mut()?;
        pairing.tick(now_ms);
        if self.opening == Opening::Due && *pairing.state() == PairingState::Open {
            let body = serde_json::to_vec(&OpenBody {
                version: 1,
                machine: &self.machine,
                pairing: pairing.id(),
                expires_at: pairing.expires_ms(),
            })
            .ok()?;
            self.opening = Opening::Out;
            return Some(FeedRequest::PairingOpen { body });
        }
        if self.reporting {
            return None;
        }
        let (proposal, outcome) = pairing.report()?;
        let body = serde_json::to_vec(&StatusBody {
            version: 1,
            machine: &self.machine,
            pairing: pairing.id(),
            proposal,
            outcome,
        })
        .ok()?;
        self.reporting = true;
        Some(FeedRequest::PairingStatus { body })
    }

    /// Opening finished: taken by another machine's pairing, a new code is
    /// opened instead.
    pub fn opened(
        &mut self,
        current: bool,
        outcome: Result<bool, FeedFailure>,
        now: Duration,
        now_ms: i64,
    ) {
        if self.opening == Opening::Out {
            self.opening = Opening::Due;
        }
        if !current || !self.enabled {
            return;
        }
        match outcome {
            Ok(false) => {
                self.opening = Opening::Opened;
                self.retry = RETRY_MIN;
            }
            Ok(true) => self.pairing = Some(Pairing::open(now_ms)),
            Err(failure) => self.back_off(failure, now),
        }
    }

    /// A report finished.
    pub fn reported(&mut self, current: bool, outcome: Result<(), FeedFailure>, now: Duration) {
        self.reporting = false;
        if !current || !self.enabled {
            return;
        }
        match outcome {
            // Taken, or refused for good (expired meanwhile): nothing more to say.
            Ok(()) | Err(FeedFailure::Invalid) => {
                if let Some(pairing) = &mut self.pairing {
                    pairing.reported();
                }
            }
            Err(failure) => self.back_off(failure, now),
        }
    }

    fn back_off(&mut self, failure: FeedFailure, now: Duration) {
        let wait = match failure {
            FeedFailure::RateLimited(Some(delay)) | FeedFailure::Unavailable(Some(delay)) => {
                delay.min(RETRY_MAX)
            }
            _ => self.retry,
        };
        self.retry = self.retry.saturating_mul(2).min(RETRY_MAX);
        self.retry_at = now + crate::jitter::later(wait);
    }

    /// The devices paired with this machine.
    pub fn devices(&self) -> Vec<DeviceView> {
        self.guard
            .borrow()
            .devices
            .list()
            .iter()
            .map(|device| DeviceView {
                key_id: device.key_id.clone(),
                format: device.format,
                name: device.name.clone(),
                fingerprint: device.fingerprint(),
                paired_at: device.paired_at,
                last_used_at: device.last_used_at,
            })
            .collect()
    }

    /// Forget the device with `key_id`; whether there was one.
    pub fn remove(&mut self, key_id: &str) -> bool {
        self.guard.borrow_mut().devices.remove(key_id)
    }

    /// This machine's id and devices, for its session list.
    pub fn listed(&self) -> wire::ListedSigning {
        let guard = self.guard.borrow();
        wire::ListedSigning {
            machine_id: guard.machine_id().to_owned(),
            devices: guard
                .devices
                .list()
                .iter()
                .map(|device| wire::ListedDevice {
                    key_id: device.key_id.clone(),
                    format: device.format,
                    name: device.name.clone(),
                })
                .collect(),
        }
    }
}

/// The answer to opening a pairing: opened (`false`), or its id taken by
/// another machine's open pairing (`true`, `pairing_taken`).
pub(crate) fn opened(reply: &FeedHttpReply) -> Result<bool, FeedFailure> {
    #[derive(Deserialize)]
    struct Answer {
        version: u8,
    }
    #[derive(Deserialize)]
    struct Error {
        error: Code,
    }
    #[derive(Deserialize)]
    struct Code {
        code: String,
    }
    if reply.status == 409 {
        let taken = serde_json::from_slice::<Error>(&reply.body)
            .is_ok_and(|error| error.error.code == "pairing_taken");
        return if taken {
            Ok(true)
        } else {
            Err(wire::failure(reply))
        };
    }
    if reply.status != 200 {
        return Err(wire::failure(reply));
    }
    let answer: Answer = serde_json::from_slice(&reply.body).map_err(|_| FeedFailure::Invalid)?;
    if answer.version != 1 {
        return Err(FeedFailure::Invalid);
    }
    Ok(false)
}

/// The answer to a pairing's report: the proposal as stored.
pub(crate) fn reported(reply: &FeedHttpReply) -> Result<(), FeedFailure> {
    #[derive(Deserialize)]
    struct Answer {
        version: u8,
        id: String,
    }
    if reply.status != 200 {
        return Err(wire::failure(reply));
    }
    let answer: Answer = serde_json::from_slice(&reply.body).map_err(|_| FeedFailure::Invalid)?;
    if answer.version != 1 || !crate::feed::is_entry_id(&answer.id) {
        return Err(FeedFailure::Invalid);
    }
    Ok(())
}

/// Why a pairing failed, in a few words, for TUIs and the CLI.
pub fn failure_text(failure: PairingFailure) -> &'static str {
    match failure {
        PairingFailure::WrongCode => "the code didn't match",
        PairingFailure::Expired => "the code expired",
        PairingFailure::Refused => "not paired",
        PairingFailure::Full => "this machine has as many devices as it takes",
        PairingFailure::Cancelled => "cancelled",
    }
}

#[cfg(test)]
mod tests;
