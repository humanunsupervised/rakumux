//! Signed commands (contract `contracts/feed/`, "Signed commands"): what
//! rakumux.com asks of this machine (a reply or session message typed in, an
//! answer or pick given, a session started, deleted or renamed, an update)
//! is carried out only when a device the user paired with this machine signed
//! it, so whoever can write to rakumux.com, its database or its deploys can't
//! make this machine act.
//!
//! A device is a browser's passkey for the signer site ([`SIGNER_ORIGIN`],
//! built in; `RAKUMUX_SIGNER_ORIGIN` for development), whose signature is a
//! WebAuthn assertion, or the phone app's key in the phone's secure chip,
//! which signs the command itself (ECDSA P-256, SHA-256). Devices are paired
//! at this machine ([`pairing`]) and kept in `sensagent/paired-devices.json`
//! (0600). Each command is its exact JSON bytes, signed whole: it names its
//! kind, this machine's id, its own id (the request's, so one is carried out
//! at most once by the kept ids each kind already has), when it was signed and
//! when it expires (at most an hour later), and what it asks; the machine
//! compares what it would act on with what was signed.
//!
//! Until a device is paired here, commands without a signature are carried
//! out as before; once one is, every command needs one (Pause aside, which
//! the heartbeats' actions keep unsigned).

pub mod log;
pub mod pairing;

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use ring::digest::{digest, SHA256};
use ring::signature::{UnparsedPublicKey, ECDSA_P256_SHA256_ASN1};
use serde::{Deserialize, Serialize};

/// The signer site, built into releases: the only page whose passkeys sign
/// for this machine.
pub const SIGNER_ORIGIN: &str = "https://rakumux-sign.com";
/// Overrides [`SIGNER_ORIGIN`] for development (a local signer site).
pub const SIGNER_ORIGIN_ENV: &str = "RAKUMUX_SIGNER_ORIGIN";
/// What this machine says with its session list: it takes signed commands
/// (`capabilities`).
pub const SIGNED_COMMANDS: &str = "signed_commands";
/// The most devices one machine keeps.
pub const MAX_DEVICES: usize = 32;
/// The longest a command stays good after it was signed.
pub const MAX_LIFETIME_MS: i64 = 60 * 60 * 1000;
/// How far ahead of this machine's clock a signing device's may be.
pub const CLOCK_SKEW_MS: i64 = 5 * 60 * 1000;
/// The longest key id taken (WebAuthn's credential ids are at most 1023 bytes;
/// the platforms' are far shorter).
const MAX_KEY_ID_BYTES: usize = 512;
/// The longest command taken (a reply's text is at most 8 KiB).
const MAX_COMMAND_BYTES: usize = 64 * 1024;
/// The longest device name taken.
pub const MAX_NAME_CHARS: usize = 64;

/// How a device signs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KeyFormat {
    /// A passkey for the signer site: a WebAuthn assertion over the
    /// command's SHA-256 as its challenge.
    Webauthn,
    /// A key that signs the command's bytes itself (ECDSA P-256 over
    /// SHA-256): the phone app's.
    P256,
}

/// A device paired with this machine.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairedDevice {
    /// The key's id: a passkey's credential id, base64url.
    pub key_id: String,
    pub format: KeyFormat,
    /// Its public key, an uncompressed P-256 point, base64url.
    pub public_key: String,
    /// What the user calls it ("Chrome on MacBook"), one line.
    pub name: String,
    /// When it was paired (ms since the epoch).
    pub paired_at: i64,
    /// When it last signed a command carried out here.
    #[serde(default)]
    pub last_used_at: Option<i64>,
}

impl PairedDevice {
    /// The short fingerprint both sides show while pairing (`ab12 · cd34 ·
    /// 9f0e`): the first six bytes of its public key's SHA-256.
    pub fn fingerprint(&self) -> String {
        URL_SAFE_NO_PAD
            .decode(&self.public_key)
            .map(|point| fingerprint(&point))
            .unwrap_or_default()
    }
}

/// The short fingerprint of a public key (an uncompressed point).
pub fn fingerprint(point: &[u8]) -> String {
    let hash = digest(&SHA256, point);
    hash.as_ref()[..6]
        .chunks(2)
        .map(|pair| format!("{:02x}{:02x}", pair[0], pair[1]))
        .collect::<Vec<_>>()
        .join(" · ")
}

/// The uncompressed P-256 point a device's public key gives, from its raw
/// form (65 bytes, `04…`) or its SubjectPublicKeyInfo (what a browser's
/// `getPublicKey()` and Android's Keystore give); `None` for anything else.
pub fn p256_point(bytes: &[u8]) -> Option<Vec<u8>> {
    /// The SubjectPublicKeyInfo prefix of an uncompressed P-256 key.
    const SPKI: [u8; 26] = [
        0x30, 0x59, 0x30, 0x13, 0x06, 0x07, 0x2a, 0x86, 0x48, 0xce, 0x3d, 0x02, 0x01, 0x06, 0x08,
        0x2a, 0x86, 0x48, 0xce, 0x3d, 0x03, 0x01, 0x07, 0x03, 0x42, 0x00,
    ];
    let point = bytes.strip_prefix(&SPKI[..]).unwrap_or(bytes);
    (point.len() == 65 && point[0] == 0x04).then(|| point.to_vec())
}

#[derive(Serialize, Deserialize)]
struct DevicesFile {
    version: u8,
    devices: Vec<PairedDevice>,
}

/// The devices paired with this machine, kept in a file (0600); memory only
/// without one.
pub struct PairedDevices {
    path: Option<PathBuf>,
    devices: Vec<PairedDevice>,
}

impl PairedDevices {
    /// The devices kept in `path` (none when it's missing or unreadable).
    pub fn load(path: Option<PathBuf>) -> Self {
        let devices = path
            .as_ref()
            .and_then(|path| std::fs::read(path).ok())
            .and_then(|bytes| serde_json::from_slice::<DevicesFile>(&bytes).ok())
            .filter(|file| file.version == 1)
            .map(|file| file.devices)
            .unwrap_or_default();
        Self { path, devices }
    }

    /// Read the file again: a CLI or another service may have changed it.
    pub fn reload(&mut self) {
        *self = Self::load(self.path.take());
    }

    pub fn list(&self) -> &[PairedDevice] {
        &self.devices
    }

    /// Whether commands need a signature here: once a device is paired.
    pub fn required(&self) -> bool {
        !self.devices.is_empty()
    }

    pub fn find(&self, key_id: &str) -> Option<&PairedDevice> {
        self.devices.iter().find(|device| device.key_id == key_id)
    }

    /// Pair `device` (in place of one with its key id); `false` when this
    /// machine already keeps [`MAX_DEVICES`] others.
    pub fn add(&mut self, device: PairedDevice) -> bool {
        self.devices.retain(|kept| kept.key_id != device.key_id);
        if self.devices.len() >= MAX_DEVICES {
            return false;
        }
        self.devices.push(device);
        self.save();
        true
    }

    /// Forget the device with `key_id`; whether there was one.
    pub fn remove(&mut self, key_id: &str) -> bool {
        let before = self.devices.len();
        self.devices.retain(|device| device.key_id != key_id);
        let removed = self.devices.len() != before;
        if removed {
            self.save();
        }
        removed
    }

    fn touch(&mut self, key_id: &str, now_ms: i64) {
        if let Some(device) = self.devices.iter_mut().find(|d| d.key_id == key_id) {
            device.last_used_at = Some(now_ms);
            self.save();
        }
    }

    fn save(&self) {
        let Some(path) = &self.path else {
            return;
        };
        let file = DevicesFile {
            version: 1,
            devices: self.devices.clone(),
        };
        let Ok(bytes) = serde_json::to_vec_pretty(&file) else {
            return;
        };
        if crate::feed::write_private(path, &bytes).is_err() {
            eprintln!("rakumux-status: saving the paired devices failed");
        }
    }
}

/// A command's signature as rakumux.com carries it (`signedCommand`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Signed {
    /// The signing device's key id.
    pub device: String,
    /// The command's exact JSON bytes, base64url.
    pub command: String,
    /// ECDSA P-256 signature (ASN.1 DER), base64url.
    pub signature: String,
    /// A passkey's authenticator data, base64url (WebAuthn only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authenticator_data: Option<String>,
    /// A passkey's client data JSON, base64url (WebAuthn only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_data: Option<String>,
}

impl Signed {
    /// A row's `signed`, read as any JSON: `Ok(None)` without one, `Err` for
    /// one off the contract (the command is then refused, never carried out
    /// as unsigned).
    pub fn of(value: Option<serde_json::Value>) -> Result<Option<Self>, ()> {
        match value {
            None | Some(serde_json::Value::Null) => Ok(None),
            Some(value) => serde_json::from_value(value).map(Some).map_err(|_| ()),
        }
    }
}

/// A request's `signed` as read: `Ok(None)` without one, `Err` for one off
/// the contract ([`Signed::of`]).
pub type Signature = Result<Option<Signed>, ()>;

/// Why a command wasn't carried out (each kind's failure, contract
/// `signatureFailure`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Refusal {
    /// It has no signature, and this machine has paired devices.
    Unsigned,
    /// Signed by a device not paired with this machine.
    UnknownDevice,
    /// The signature doesn't hold, or what was signed isn't what was asked.
    BadSignature,
    /// Signed over an hour ago, or later than this machine's clock allows.
    Expired,
}

/// What a command must say to be carried out: its kind, its id, and the
/// fields this machine acts on, each equal to what was signed (a field
/// expected as `null` must be absent or `null` there).
pub struct Expected<'a> {
    pub kind: &'a str,
    pub id: &'a str,
    pub fields: Vec<(&'a str, serde_json::Value)>,
}

/// A reply's (or session message's) signature as read, with what the
/// command must say: the reply's `session`, `entry`, `text`, `answer` and
/// `choice` as they came (never as cleaned for typing), and its files' names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReplySignature {
    pub signature: Signature,
    pub fields: Vec<(&'static str, serde_json::Value)>,
    pub names: Vec<String>,
}

/// The one [`Guard`] the owner thread's syncs share.
pub type SharedGuard = Rc<RefCell<Guard>>;

/// A guard for the built-in signer site with no device paired (memory only):
/// what a sync checks with until it's given the service's.
pub fn unpaired() -> SharedGuard {
    Rc::new(RefCell::new(Guard::from_env(PairedDevices::load(None))))
}

/// The signer site and the devices paired here: checks commands.
pub struct Guard {
    origin: String,
    rp_id: String,
    /// This machine's id (`X-Rakumux-Machine-Id`), which commands name.
    machine_id: String,
    pub devices: PairedDevices,
    /// The sessions started here for signed new-session requests, by request
    /// id, and whether their first prompt was taken (memory only).
    started: std::collections::HashMap<String, (String, bool)>,
    /// What became of each signed command (and each refused).
    log: log::SignedLog,
}

impl Guard {
    /// For the signer site at `origin` (`https://host`, or `http://` for a
    /// loopback one in development).
    pub fn new(origin: &str, devices: PairedDevices) -> Option<Self> {
        let host = origin.strip_prefix("https://").or_else(|| {
            origin
                .strip_prefix("http://")
                .filter(|host| host.starts_with("localhost") || host.starts_with("127.0.0.1"))
        })?;
        let rp_id = host.split(':').next()?.to_owned();
        (!rp_id.is_empty() && !host.contains('/')).then(|| Self {
            origin: origin.to_owned(),
            rp_id,
            machine_id: String::new(),
            devices,
            started: std::collections::HashMap::new(),
            log: log::SignedLog::default(),
        })
    }

    /// For the built-in signer site, or `RAKUMUX_SIGNER_ORIGIN`'s.
    pub fn from_env(devices: PairedDevices) -> Self {
        let origin = std::env::var(SIGNER_ORIGIN_ENV)
            .ok()
            .filter(|origin| Self::new(origin, PairedDevices::load(None)).is_some())
            .unwrap_or_else(|| SIGNER_ORIGIN.to_owned());
        match Self::new(&origin, devices) {
            Some(guard) => guard,
            // The built-in origin is always one `new` takes.
            None => unreachable!("the signer origin is https"),
        }
    }

    pub fn origin(&self) -> &str {
        &self.origin
    }

    /// For the machine `machine_id`: the id commands must name.
    pub fn for_machine(mut self, machine_id: &str) -> Self {
        self.machine_id = machine_id.to_owned();
        self
    }

    /// Keep what becomes of signed commands in `path` ([`log`]).
    pub fn with_log(mut self, path: Option<PathBuf>) -> Self {
        self.log = log::SignedLog::new(path);
        self
    }

    pub fn machine_id(&self) -> &str {
        &self.machine_id
    }

    /// The daemon started `session` for the new-session request `id`.
    pub fn started(&mut self, id: &str, session: &str) {
        if self.started.len() >= 256 {
            self.started.clear();
        }
        self.started
            .insert(id.to_owned(), (session.to_owned(), false));
    }

    /// Whether a session message into `session` with `text` is the first
    /// prompt signed with the new-session request that started it (its
    /// `signed`, kind `new_session`): taken once, at `now_ms`.
    pub fn first_prompt(
        &mut self,
        signed: &Signed,
        session: &str,
        text: &str,
        now_ms: i64,
    ) -> Result<(), Refusal> {
        let device = self
            .devices
            .find(&signed.device)
            .ok_or(Refusal::UnknownDevice)?;
        let command = self.verified(signed, device)?;
        let id = command
            .get("id")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let expected = Expected {
            kind: "new_session",
            id: &id,
            fields: vec![("prompt", text.into())],
        };
        said(&command, &expected, &self.machine_id, now_ms)?;
        match self.started.get_mut(&id) {
            Some((started, taken)) if started == session && !*taken => {
                *taken = true;
                let key_id = signed.device.clone();
                self.devices.touch(&key_id, now_ms);
                Ok(())
            }
            _ => Err(Refusal::BadSignature),
        }
    }

    /// Whether reply `id` may be typed in at `now_ms`: signed as a `reply`
    /// saying what it says (or, as a new session's first prompt, signed with
    /// that session's request), or unsigned while no device is paired or
    /// when this machine wrote it itself (`own`). Its files' SHA-256s (hex)
    /// as signed, to check each against what's downloaded; none unsigned.
    pub fn reply(
        &mut self,
        id: &str,
        signature: &ReplySignature,
        own: bool,
        now_ms: i64,
    ) -> Result<Vec<String>, Refusal> {
        let result = self.reply_checked(id, signature, own, now_ms);
        let signed = signature.signature.as_ref().ok().and_then(Option::as_ref);
        let session = signature
            .fields
            .iter()
            .find(|(name, _)| *name == "session")
            .and_then(|(_, value)| value.as_str());
        if signed.is_some() || result.is_err() {
            let outcome = result.as_ref().map(|_| ()).map_err(|refusal| *refusal);
            self.record("reply", id, signed, session, outcome, now_ms);
        }
        result
    }

    fn reply_checked(
        &mut self,
        id: &str,
        signature: &ReplySignature,
        own: bool,
        now_ms: i64,
    ) -> Result<Vec<String>, Refusal> {
        let signed = match &signature.signature {
            Err(()) => return Err(Refusal::BadSignature),
            Ok(None) if own || !self.devices.required() => return Ok(Vec::new()),
            Ok(None) => return Err(Refusal::Unsigned),
            Ok(Some(signed)) => signed,
        };
        let device = self
            .devices
            .find(&signed.device)
            .ok_or(Refusal::UnknownDevice)?;
        let command = self.verified(signed, device)?;
        let field = |key: &str| {
            signature
                .fields
                .iter()
                .find(|(name, _)| *name == key)
                .map(|(_, value)| value.clone())
                .unwrap_or_default()
        };
        if command.get("kind").and_then(serde_json::Value::as_str) == Some("new_session") {
            let (session, text) = (field("session"), field("text"));
            let plain = field("entry").is_null()
                && field("answer").is_null()
                && field("choice").is_null()
                && signature.names.is_empty();
            if !plain {
                return Err(Refusal::BadSignature);
            }
            let (Some(session), Some(text)) = (session.as_str(), text.as_str()) else {
                return Err(Refusal::BadSignature);
            };
            self.first_prompt(signed, session, text, now_ms)?;
            return Ok(Vec::new());
        }
        let expected = Expected {
            kind: "reply",
            id,
            fields: signature.fields.clone(),
        };
        said(&command, &expected, &self.machine_id, now_ms)?;
        // Its files, by name in order, each with its SHA-256.
        let files: Vec<(&str, &str)> = match command.get("attachments") {
            None | Some(serde_json::Value::Null) => Vec::new(),
            Some(serde_json::Value::Array(files)) => files
                .iter()
                .map(|file| {
                    let text = |key| file.get(key).and_then(serde_json::Value::as_str);
                    text("name").zip(text("sha256"))
                })
                .collect::<Option<_>>()
                .ok_or(Refusal::BadSignature)?,
            Some(_) => return Err(Refusal::BadSignature),
        };
        let same = files.len() == signature.names.len()
            && files
                .iter()
                .zip(&signature.names)
                .all(|((name, hash), named)| {
                    name == named
                        && hash.len() == 64
                        && hash
                            .bytes()
                            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                });
        if !same {
            return Err(Refusal::BadSignature);
        }
        let hashes = files.iter().map(|(_, hash)| (*hash).to_owned()).collect();
        self.devices.touch(&signed.device.clone(), now_ms);
        Ok(hashes)
    }

    /// Whether `expected`, asked of this machine with `signature`, may be
    /// carried out at `now_ms` ([`Guard::check`]); one off the contract never.
    pub fn allows(
        &mut self,
        signature: &Signature,
        expected: &Expected<'_>,
        now_ms: i64,
    ) -> Result<(), Refusal> {
        let machine_id = self.machine_id.clone();
        let (signed, result) = match signature {
            Err(()) => (None, Err(Refusal::BadSignature)),
            Ok(signed) => (
                signed.as_ref(),
                self.check(signed.as_ref(), expected, &machine_id, now_ms),
            ),
        };
        if signed.is_some() || result.is_err() {
            let session = expected
                .fields
                .iter()
                .find(|(name, _)| *name == "session")
                .and_then(|(_, value)| value.as_str());
            self.record(expected.kind, expected.id, signed, session, result, now_ms);
        }
        result
    }

    /// Write what became of command `id` to the log.
    fn record(
        &mut self,
        kind: &str,
        id: &str,
        signed: Option<&Signed>,
        session: Option<&str>,
        outcome: Result<(), Refusal>,
        now_ms: i64,
    ) {
        let name = signed
            .and_then(|signed| self.devices.find(&signed.device))
            .map(|device| device.name.clone());
        self.log.record(log::LogEntry {
            at: now_ms,
            kind: kind.to_owned(),
            id: id.to_owned(),
            outcome: match outcome {
                Ok(()) => log::Outcome::Accepted,
                Err(refusal) => refusal.into(),
            },
            device: signed.map(|signed| signed.device.clone()),
            name,
            session: session.map(str::to_owned),
        });
    }

    /// Whether `expected`, asked of the machine `machine_id`, may be carried
    /// out at `now_ms`: signed by a device paired here, or unsigned while none
    /// is. A command carried out marks its device used.
    pub fn check(
        &mut self,
        signed: Option<&Signed>,
        expected: &Expected<'_>,
        machine_id: &str,
        now_ms: i64,
    ) -> Result<(), Refusal> {
        let Some(signed) = signed else {
            return if self.devices.required() {
                Err(Refusal::Unsigned)
            } else {
                Ok(())
            };
        };
        let device = self
            .devices
            .find(&signed.device)
            .ok_or(Refusal::UnknownDevice)?;
        let command = self.verified(signed, device)?;
        said(&command, expected, machine_id, now_ms)?;
        let key_id = device.key_id.clone();
        self.devices.touch(&key_id, now_ms);
        Ok(())
    }

    /// The command `signed` holds, when `device`'s signature over it holds.
    fn verified(
        &self,
        signed: &Signed,
        device: &PairedDevice,
    ) -> Result<serde_json::Map<String, serde_json::Value>, Refusal> {
        let bad = |_| Refusal::BadSignature;
        if signed.device.len() > MAX_KEY_ID_BYTES * 2 {
            return Err(Refusal::BadSignature);
        }
        let command = URL_SAFE_NO_PAD.decode(&signed.command).map_err(bad)?;
        if command.len() > MAX_COMMAND_BYTES {
            return Err(Refusal::BadSignature);
        }
        let signature = URL_SAFE_NO_PAD.decode(&signed.signature).map_err(bad)?;
        let point = URL_SAFE_NO_PAD.decode(&device.public_key).map_err(bad)?;
        let key = UnparsedPublicKey::new(&ECDSA_P256_SHA256_ASN1, point);
        match device.format {
            KeyFormat::P256 => {
                if signed.authenticator_data.is_some() || signed.client_data.is_some() {
                    return Err(Refusal::BadSignature);
                }
                key.verify(&command, &signature)
                    .map_err(|_| Refusal::BadSignature)?;
            }
            KeyFormat::Webauthn => {
                let (Some(authenticator), Some(client)) =
                    (&signed.authenticator_data, &signed.client_data)
                else {
                    return Err(Refusal::BadSignature);
                };
                let authenticator = URL_SAFE_NO_PAD.decode(authenticator).map_err(bad)?;
                let client = URL_SAFE_NO_PAD.decode(client).map_err(bad)?;
                self.assertion(&command, &authenticator, &client)?;
                let mut message = authenticator;
                message.extend_from_slice(digest(&SHA256, &client).as_ref());
                key.verify(&message, &signature)
                    .map_err(|_| Refusal::BadSignature)?;
            }
        }
        match serde_json::from_slice(&command) {
            Ok(serde_json::Value::Object(map)) => Ok(map),
            _ => Err(Refusal::BadSignature),
        }
    }

    /// A passkey's assertion is for this command, made on the signer site,
    /// with the user present and verified (Touch ID, Face ID, a PIN).
    fn assertion(
        &self,
        command: &[u8],
        authenticator: &[u8],
        client: &[u8],
    ) -> Result<(), Refusal> {
        #[derive(Deserialize)]
        struct ClientData {
            #[serde(rename = "type")]
            kind: String,
            challenge: String,
            origin: String,
        }
        let client: ClientData =
            serde_json::from_slice(client).map_err(|_| Refusal::BadSignature)?;
        let challenge = URL_SAFE_NO_PAD.encode(digest(&SHA256, command));
        /// User present, user verified.
        const UP: u8 = 0x01;
        const UV: u8 = 0x04;
        let holds = client.kind == "webauthn.get"
            && client.challenge == challenge
            && client.origin == self.origin
            && authenticator.len() >= 37
            && authenticator[..32] == *digest(&SHA256, self.rp_id.as_bytes()).as_ref()
            && authenticator[32] & (UP | UV) == UP | UV;
        holds.then_some(()).ok_or(Refusal::BadSignature)
    }
}

/// The command says what's asked: version 1, `expected`'s kind and id, this
/// machine, `expected`'s fields as they are, and a time that holds now.
fn said(
    command: &serde_json::Map<String, serde_json::Value>,
    expected: &Expected<'_>,
    machine_id: &str,
    now_ms: i64,
) -> Result<(), Refusal> {
    let text = |key: &str| command.get(key).and_then(serde_json::Value::as_str);
    let holds = command.get("v").and_then(serde_json::Value::as_u64) == Some(1)
        && text("kind") == Some(expected.kind)
        && text("machine") == Some(machine_id)
        && text("id") == Some(expected.id)
        && expected
            .fields
            .iter()
            .all(|(key, value)| command.get(*key).unwrap_or(&serde_json::Value::Null) == value);
    if !holds {
        return Err(Refusal::BadSignature);
    }
    let time = |key: &str| command.get(key).and_then(serde_json::Value::as_i64);
    let (Some(at), Some(expires)) = (time("at"), time("expires")) else {
        return Err(Refusal::BadSignature);
    };
    let current = at <= now_ms + CLOCK_SKEW_MS
        && now_ms < expires
        && expires > at
        && expires - at <= MAX_LIFETIME_MS;
    current.then_some(()).ok_or(Refusal::Expired)
}

/// Milliseconds since the epoch, by this machine's clock.
pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|since| since.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
pub(crate) mod testing;
#[cfg(test)]
mod tests;
