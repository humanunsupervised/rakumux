//! Pairing a device with this machine, at this machine: it shows a one-time
//! code (`ABCD-EFGH-JKMN`, 60 bits, five minutes, one try) and the signer
//! site's link holding it; the device (a browser on the signer site, or the
//! phone app scanning the link's QR code) makes its key and sends the public
//! key through rakumux.com with a tag, HMAC-SHA256 keyed with the code, over
//! the pairing's id and the key. Only the code's first group (the pairing's
//! id, which rakumux.com routes by) ever reaches rakumux.com, so it can't make
//! a tag for a key of its own: a wrong tag closes the pairing. The user then
//! confirms the device's name and fingerprint here before it's kept.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use ring::hmac;
use serde::{Deserialize, Serialize};

use super::{fingerprint, p256_point, KeyFormat, PairedDevice, PairedDevices, MAX_NAME_CHARS};

/// Crockford's base32: no I, L, O or U to misread.
const ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
/// Characters in a code; its first [`ID_CHARS`] are the pairing's id.
const CODE_CHARS: usize = 12;
const ID_CHARS: usize = 4;
/// How long a code can be used.
pub const OPEN_MS: i64 = 5 * 60 * 1000;
/// How long a device sent in time waits for the user's answer.
pub const CONFIRM_MS: i64 = 5 * 60 * 1000;
/// What a tag's message starts with.
const TAG_CONTEXT: &str = "rakumux-pair-v1";

/// A device rakumux.com brought for a pairing (`pairingProposal`).
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct Proposal {
    /// The proposal's id, which this machine's report names.
    pub id: String,
    /// The pairing it's for: the code's first group.
    pub pairing: String,
    pub device: ProposedDevice,
    /// HMAC-SHA256 keyed with the code over [`tag_message`], base64url.
    pub tag: String,
}

/// The replies read's `pairings`: those within the contract (one is sent
/// at most); a row off it is left out on its own.
pub fn proposals(value: Option<serde_json::Value>) -> Vec<Proposal> {
    let Some(serde_json::Value::Array(rows)) = value else {
        return Vec::new();
    };
    rows.into_iter()
        .take(1)
        .filter_map(|row| serde_json::from_value::<Proposal>(row).ok())
        .filter(|proposal| {
            crate::feed::is_entry_id(&proposal.id)
                && proposal.pairing.len() == ID_CHARS
                && proposal
                    .pairing
                    .bytes()
                    .all(|byte| ALPHABET.contains(&byte))
        })
        .collect()
}

/// The device a proposal names.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProposedDevice {
    pub key_id: String,
    pub format: KeyFormat,
    /// Its public key (raw point or SubjectPublicKeyInfo), base64url.
    pub public_key: String,
    pub name: String,
}

/// What the tag covers: the pairing's id and the device, one per line.
pub fn tag_message(pairing: &str, device: &ProposedDevice) -> String {
    let format = match device.format {
        KeyFormat::Webauthn => "webauthn",
        KeyFormat::P256 => "p256",
    };
    [
        TAG_CONTEXT,
        pairing,
        &device.key_id,
        format,
        &device.public_key,
        &device.name,
    ]
    .join("\n")
}

/// The tag a device sends with `code` (as typed or shown: case, dashes and
/// misread letters don't matter).
pub fn tag(code: &str, pairing: &str, device: &ProposedDevice) -> String {
    let key = hmac::Key::new(hmac::HMAC_SHA256, normalized(code).as_bytes());
    let tag = hmac::sign(&key, tag_message(pairing, device).as_bytes());
    URL_SAFE_NO_PAD.encode(tag.as_ref())
}

/// A code as made: upper case, no dashes, `O` read as `0` and `I`/`L` as `1`.
pub fn normalized(code: &str) -> String {
    code.chars()
        .filter(|c| !matches!(c, '-' | ' '))
        .map(|c| match c.to_ascii_uppercase() {
            'O' => '0',
            'I' | 'L' => '1',
            c => c,
        })
        .collect()
}

/// Why a pairing ended without a device (contract `pairingFailure`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PairingFailure {
    /// The tag wasn't made with this code: a typo, or a key not the user's.
    WrongCode,
    /// Nothing came within five minutes, or the user didn't answer.
    Expired,
    /// The user said no.
    Refused,
    /// This machine keeps as many devices as it takes.
    Full,
    /// Closed here before a device came (another pairing opened).
    Cancelled,
}

/// Where a pairing is up to.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum PairingState {
    /// Waiting for a device.
    Open,
    /// A device came with the right tag: waiting for the user's answer.
    Proposed {
        name: String,
        fingerprint: String,
    },
    Paired {
        name: String,
    },
    Failed {
        failure: PairingFailure,
    },
}

/// What this machine tells rakumux.com of a proposal (`pairingStatus`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum PairingOutcome {
    Paired,
    Failed { failure: PairingFailure },
}

/// One pairing at this machine.
pub struct Pairing {
    code: String,
    opened_ms: i64,
    state: PairingState,
    /// The proposal taken, with the device it would pair.
    proposal: Option<(String, PairedDevice, i64)>,
    /// The report for that proposal, once decided, until it's taken.
    report: Option<(String, PairingOutcome)>,
}

impl Pairing {
    /// A new pairing with a fresh code, open from `now_ms`.
    pub fn open(now_ms: i64) -> Self {
        let mut random = [0u8; CODE_CHARS];
        if getrandom::getrandom(&mut random).is_err() {
            // Never a guessable code: a pairing without one fails at once.
            return Self {
                code: String::new(),
                opened_ms: now_ms,
                state: PairingState::Failed {
                    failure: PairingFailure::Cancelled,
                },
                proposal: None,
                report: None,
            };
        }
        let code = random
            .iter()
            .map(|byte| ALPHABET[usize::from(byte & 31)] as char)
            .collect();
        Self {
            code,
            opened_ms: now_ms,
            state: PairingState::Open,
            proposal: None,
            report: None,
        }
    }

    /// The code as shown: `ABCD-EFGH-JKMN`.
    pub fn code(&self) -> String {
        self.code
            .as_bytes()
            .chunks(ID_CHARS)
            .map(|group| String::from_utf8_lossy(group).into_owned())
            .collect::<Vec<_>>()
            .join("-")
    }

    /// The pairing's id: the code's first group, all rakumux.com ever sees.
    pub fn id(&self) -> &str {
        &self.code[..ID_CHARS.min(self.code.len())]
    }

    /// The signer site's page for it, the code after `#` (never sent to a server).
    pub fn link(&self, origin: &str) -> String {
        format!("{origin}/pair#{}", self.code())
    }

    /// When the code stops working.
    pub fn expires_ms(&self) -> i64 {
        self.opened_ms + OPEN_MS
    }

    pub fn state(&self) -> &PairingState {
        &self.state
    }

    /// Whether it still waits for a device or an answer.
    pub fn live(&self) -> bool {
        matches!(
            self.state,
            PairingState::Open | PairingState::Proposed { .. }
        )
    }

    /// rakumux.com brought `proposal`: taken when it's for this pairing,
    /// sent in time, its tag made with this code and its device well formed;
    /// a wrong tag fails the pairing (one try). One for another pairing, or
    /// one after the first, changes nothing.
    pub fn propose(&mut self, proposal: &Proposal, now_ms: i64) {
        self.tick(now_ms);
        if self.state != PairingState::Open || proposal.pairing != self.id() {
            return;
        }
        let key = hmac::Key::new(hmac::HMAC_SHA256, self.code.as_bytes());
        let tag_holds = URL_SAFE_NO_PAD
            .decode(&proposal.tag)
            .map(|tag| {
                hmac::verify(
                    &key,
                    tag_message(&proposal.pairing, &proposal.device).as_bytes(),
                    &tag,
                )
                .is_ok()
            })
            .unwrap_or(false);
        let device = well_formed(&proposal.device, now_ms);
        match (tag_holds, device) {
            (true, Some(device)) => {
                self.state = PairingState::Proposed {
                    name: device.name.clone(),
                    fingerprint: device.fingerprint(),
                };
                self.proposal = Some((proposal.id.clone(), device, now_ms));
            }
            _ => self.fail(Some(&proposal.id), PairingFailure::WrongCode),
        }
    }

    /// The user's answer to the device proposed: kept in `devices` on yes.
    pub fn confirm(&mut self, yes: bool, devices: &mut PairedDevices, now_ms: i64) {
        self.tick(now_ms);
        if !matches!(self.state, PairingState::Proposed { .. }) {
            return;
        }
        let Some((id, device, _)) = self.proposal.take() else {
            return;
        };
        if !yes {
            return self.fail(Some(&id), PairingFailure::Refused);
        }
        let name = device.name.clone();
        if !devices.add(device) {
            return self.fail(Some(&id), PairingFailure::Full);
        }
        self.state = PairingState::Paired { name };
        self.report = Some((id, PairingOutcome::Paired));
    }

    /// Close it here, before a device was paired.
    pub fn cancel(&mut self) {
        if self.live() {
            let id = self.proposal.take().map(|(id, _, _)| id);
            self.fail(id.as_deref(), PairingFailure::Cancelled);
        }
    }

    /// Expire a code nobody used and a device the user didn't answer for.
    pub fn tick(&mut self, now_ms: i64) {
        match &self.state {
            PairingState::Open if now_ms >= self.expires_ms() => {
                self.fail(None, PairingFailure::Expired)
            }
            PairingState::Proposed { .. } => {
                let since = self.proposal.as_ref().map_or(0, |(_, _, at)| *at);
                if now_ms >= since + CONFIRM_MS {
                    let id = self.proposal.take().map(|(id, _, _)| id);
                    self.fail(id.as_deref(), PairingFailure::Expired);
                }
            }
            _ => {}
        }
    }

    /// The report rakumux.com hasn't taken: the proposal's id and what
    /// became of it.
    pub fn report(&self) -> Option<&(String, PairingOutcome)> {
        self.report.as_ref()
    }

    /// rakumux.com took the report.
    pub fn reported(&mut self) {
        self.report = None;
    }

    fn fail(&mut self, proposal: Option<&str>, failure: PairingFailure) {
        self.state = PairingState::Failed { failure };
        self.proposal = None;
        if let Some(id) = proposal {
            self.report = Some((id.to_owned(), PairingOutcome::Failed { failure }));
        }
    }
}

/// The device a proposal names, when its key and name fit: a key id of at
/// most 512 bytes, a P-256 public key, a name of one line.
fn well_formed(device: &ProposedDevice, now_ms: i64) -> Option<PairedDevice> {
    let key_id = URL_SAFE_NO_PAD.decode(&device.key_id).ok()?;
    let point = p256_point(&URL_SAFE_NO_PAD.decode(&device.public_key).ok()?)?;
    let name = device.name.trim();
    let fits = (1..=super::MAX_KEY_ID_BYTES).contains(&key_id.len())
        && !name.is_empty()
        && name.chars().count() <= MAX_NAME_CHARS
        && !name.chars().any(char::is_control);
    fits.then(|| PairedDevice {
        key_id: device.key_id.clone(),
        format: device.format,
        public_key: URL_SAFE_NO_PAD.encode(&point),
        name: name.to_owned(),
        paired_at: now_ms,
        last_used_at: None,
    })
}

/// The fingerprint a proposed device shows, for the signer page to agree with.
pub fn proposed_fingerprint(device: &ProposedDevice) -> Option<String> {
    let point = p256_point(&URL_SAFE_NO_PAD.decode(&device.public_key).ok()?)?;
    Some(fingerprint(&point))
}
