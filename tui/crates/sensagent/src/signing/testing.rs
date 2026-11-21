//! Devices that sign, for tests here and in the feed's syncs.

use std::cell::RefCell;
use std::rc::Rc;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use ring::digest::{digest, SHA256};
use ring::rand::SystemRandom;
use ring::signature::{EcdsaKeyPair, KeyPair, ECDSA_P256_SHA256_ASN1_SIGNING};
use serde_json::json;

use super::pairing::ProposedDevice;
use super::{Guard, KeyFormat, PairedDevice, PairedDevices, SharedGuard, Signed, SIGNER_ORIGIN};

/// A device that signs: a P-256 key and how it signs.
pub(crate) struct Device {
    pub key: EcdsaKeyPair,
    pub key_id: String,
    pub format: KeyFormat,
}

impl Device {
    pub(crate) fn new(format: KeyFormat) -> Self {
        let rng = SystemRandom::new();
        let pkcs8 = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, &rng).unwrap();
        let key = EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, pkcs8.as_ref(), &rng)
            .unwrap();
        let mut id = [0u8; 16];
        getrandom::getrandom(&mut id).unwrap();
        Self {
            key,
            key_id: URL_SAFE_NO_PAD.encode(id),
            format,
        }
    }

    pub(crate) fn proposed(&self, name: &str) -> ProposedDevice {
        ProposedDevice {
            key_id: self.key_id.clone(),
            format: self.format,
            public_key: URL_SAFE_NO_PAD.encode(self.key.public_key().as_ref()),
            name: name.into(),
        }
    }

    pub(crate) fn paired(&self) -> PairedDevice {
        PairedDevice {
            key_id: self.key_id.clone(),
            format: self.format,
            public_key: URL_SAFE_NO_PAD.encode(self.key.public_key().as_ref()),
            name: "Chrome on MacBook".into(),
            paired_at: 0,
            last_used_at: None,
        }
    }

    pub(crate) fn sign_bytes(&self, message: &[u8]) -> Vec<u8> {
        self.key
            .sign(&SystemRandom::new(), message)
            .unwrap()
            .as_ref()
            .to_vec()
    }

    /// `command` signed as this device signs, on the signer site `origin`.
    pub(crate) fn sign(&self, command: &serde_json::Value, origin: &str) -> Signed {
        let bytes = serde_json::to_vec(command).unwrap();
        match self.format {
            KeyFormat::P256 => Signed {
                device: self.key_id.clone(),
                command: URL_SAFE_NO_PAD.encode(&bytes),
                signature: URL_SAFE_NO_PAD.encode(self.sign_bytes(&bytes)),
                authenticator_data: None,
                client_data: None,
            },
            KeyFormat::Webauthn => {
                let rp_id = origin.trim_start_matches("https://");
                let mut authenticator = digest(&SHA256, rp_id.as_bytes()).as_ref().to_vec();
                authenticator.push(0x05); // user present and verified
                authenticator.extend_from_slice(&0u32.to_be_bytes());
                let client = serde_json::to_vec(&json!({
                    "type": "webauthn.get",
                    "challenge": URL_SAFE_NO_PAD.encode(digest(&SHA256, &bytes)),
                    "origin": origin,
                    "crossOrigin": true,
                    "topOrigin": "https://rakumux.com",
                }))
                .unwrap();
                let mut message = authenticator.clone();
                message.extend_from_slice(digest(&SHA256, &client).as_ref());
                Signed {
                    device: self.key_id.clone(),
                    command: URL_SAFE_NO_PAD.encode(&bytes),
                    signature: URL_SAFE_NO_PAD.encode(self.sign_bytes(&message)),
                    authenticator_data: Some(URL_SAFE_NO_PAD.encode(&authenticator)),
                    client_data: Some(URL_SAFE_NO_PAD.encode(&client)),
                }
            }
        }
    }
}

impl Device {
    /// `fields` as a command of `kind` for request `id` to machine `machine`,
    /// signed now, as rakumux.com passes it on (`signed`).
    pub(crate) fn signed_now(
        &self,
        kind: &str,
        machine: &str,
        id: &str,
        fields: serde_json::Value,
    ) -> serde_json::Value {
        let now = super::now_ms();
        let mut command = json!({
            "v": 1, "kind": kind, "machine": machine, "id": id,
            "at": now, "expires": now + 10 * 60 * 1000,
        });
        if let (Some(command), Some(fields)) = (command.as_object_mut(), fields.as_object()) {
            command.extend(fields.clone());
        }
        serde_json::to_value(self.sign(&command, SIGNER_ORIGIN)).unwrap()
    }
}

/// A guard for machine `machine` with `devices` paired (memory only).
pub(crate) fn guard(machine: &str, devices: &[&Device]) -> SharedGuard {
    let mut kept = PairedDevices::load(None);
    for device in devices {
        assert!(kept.add(device.paired()));
    }
    Rc::new(RefCell::new(
        Guard::new(SIGNER_ORIGIN, kept)
            .unwrap()
            .for_machine(machine),
    ))
}

/// The machine id the feed's sync tests sign for.
pub(crate) const TEST_MACHINE_ID: &str = "01K8C2B3C4D5E6F7G8H9J0K1M9";

/// The device paired in the feed's sync tests (the same one throughout).
pub(crate) fn test_device() -> &'static Device {
    static DEVICE: std::sync::OnceLock<Device> = std::sync::OnceLock::new();
    DEVICE.get_or_init(|| Device::new(KeyFormat::P256))
}

/// A guard for [`TEST_MACHINE_ID`] with [`test_device`] paired: every
/// command needs a signature, so the syncs' tests sign theirs.
pub(crate) fn paired() -> SharedGuard {
    guard(TEST_MACHINE_ID, &[test_device()])
}

/// `fields` as a command of `kind` for request `id`, signed now by
/// [`test_device`] for [`TEST_MACHINE_ID`], as rakumux.com passes it on.
pub(crate) fn signed_json(kind: &str, id: &str, fields: serde_json::Value) -> serde_json::Value {
    test_device().signed_now(kind, TEST_MACHINE_ID, id, fields)
}

/// [`signed_json`] as a sync reads it.
pub(crate) fn signed(kind: &str, id: &str, fields: serde_json::Value) -> super::Signature {
    Signed::of(Some(signed_json(kind, id, fields)))
}
