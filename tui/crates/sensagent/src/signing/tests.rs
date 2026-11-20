use super::pairing::{
    proposed_fingerprint, tag, Pairing, PairingFailure, PairingOutcome, PairingState, Proposal,
    ProposedDevice, OPEN_MS,
};
use super::testing::Device;
use super::*;
use ring::signature::KeyPair;
use serde_json::json;

const MACHINE: &str = "01K8A2B3C4D5E6F7G8H9J0K1M2";
const REQUEST: &str = "01K8B2B3C4D5E6F7G8H9J0K1M2";
const SESSION: &str = "01K89ZQ4D2F6H8J0K2M4N6P8R0";
const NOW: i64 = 1_763_630_000_000;

fn delete_command() -> serde_json::Value {
    json!({
        "v": 1, "kind": "delete_session", "machine": MACHINE, "id": REQUEST,
        "session": SESSION, "at": NOW, "expires": NOW + 10 * 60 * 1000,
    })
}

fn expected() -> Expected<'static> {
    Expected {
        kind: "delete_session",
        id: REQUEST,
        fields: vec![("session", json!(SESSION))],
    }
}

fn guard_with(devices: &[&Device]) -> Guard {
    let mut kept = PairedDevices::load(None);
    for device in devices {
        assert!(kept.add(device.paired()));
    }
    Guard::new(SIGNER_ORIGIN, kept).unwrap()
}

#[test]
fn a_command_signed_by_a_paired_passkey_or_phone_is_carried_out() {
    for format in [KeyFormat::Webauthn, KeyFormat::P256] {
        let device = Device::new(format);
        let mut guard = guard_with(&[&device]);
        let signed = device.sign(&delete_command(), SIGNER_ORIGIN);
        assert_eq!(
            guard.check(Some(&signed), &expected(), MACHINE, NOW + 1_000),
            Ok(()),
            "{format:?}"
        );
        assert_eq!(guard.devices.list()[0].last_used_at, Some(NOW + 1_000));
    }
}

#[test]
fn unsigned_commands_are_carried_out_only_until_a_device_is_paired() {
    let mut guard = guard_with(&[]);
    assert_eq!(guard.check(None, &expected(), MACHINE, NOW), Ok(()));
    let device = Device::new(KeyFormat::Webauthn);
    guard.devices.add(device.paired());
    assert_eq!(
        guard.check(None, &expected(), MACHINE, NOW),
        Err(Refusal::Unsigned)
    );
}

#[test]
fn a_device_not_paired_here_is_refused() {
    let paired = Device::new(KeyFormat::Webauthn);
    let stranger = Device::new(KeyFormat::Webauthn);
    let mut guard = guard_with(&[&paired]);
    let signed = stranger.sign(&delete_command(), SIGNER_ORIGIN);
    assert_eq!(
        guard.check(Some(&signed), &expected(), MACHINE, NOW),
        Err(Refusal::UnknownDevice)
    );
    // Nor a stranger's signature under a paired device's id.
    let mut forged = signed;
    forged.device = paired.key_id.clone();
    assert_eq!(
        guard.check(Some(&forged), &expected(), MACHINE, NOW),
        Err(Refusal::BadSignature)
    );
}

#[test]
fn what_was_asked_must_be_what_was_signed() {
    let device = Device::new(KeyFormat::Webauthn);
    let mut guard = guard_with(&[&device]);
    let signed = device.sign(&delete_command(), SIGNER_ORIGIN);
    let refused = |guard: &mut Guard, expected: Expected<'_>, machine: &str| {
        guard.check(Some(&signed), &expected, machine, NOW)
    };
    // Another session, request, kind or machine than the one signed.
    let other_session = Expected {
        fields: vec![("session", json!("01K89ZQ4D2F6H8J0K2M4N6P8R9"))],
        ..expected()
    };
    assert_eq!(
        refused(&mut guard, other_session, MACHINE),
        Err(Refusal::BadSignature)
    );
    let other_id = Expected {
        id: "01K8C2B3C4D5E6F7G8H9J0K1M2",
        ..expected()
    };
    assert_eq!(
        refused(&mut guard, other_id, MACHINE),
        Err(Refusal::BadSignature)
    );
    let other_kind = Expected {
        kind: "rename_session",
        ..expected()
    };
    assert_eq!(
        refused(&mut guard, other_kind, MACHINE),
        Err(Refusal::BadSignature)
    );
    assert_eq!(
        refused(&mut guard, expected(), "01K8A2B3C4D5E6F7G8H9J0K1M9"),
        Err(Refusal::BadSignature)
    );
    // A command changed after it was signed.
    let mut changed = signed.clone();
    let mut command = delete_command();
    command["session"] = json!("01K89ZQ4D2F6H8J0K2M4N6P8R9");
    changed.command = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&command).unwrap());
    let other = Expected {
        fields: vec![("session", json!("01K89ZQ4D2F6H8J0K2M4N6P8R9"))],
        ..expected()
    };
    assert_eq!(
        guard.check(Some(&changed), &other, MACHINE, NOW),
        Err(Refusal::BadSignature)
    );
}

#[test]
fn a_passkey_must_have_signed_on_the_signer_site_with_the_user_verified() {
    let device = Device::new(KeyFormat::Webauthn);
    let mut guard = guard_with(&[&device]);
    // Made on rakumux.com itself: another origin and relying party.
    let elsewhere = device.sign(&delete_command(), "https://rakumux.com");
    assert_eq!(
        guard.check(Some(&elsewhere), &expected(), MACHINE, NOW),
        Err(Refusal::BadSignature)
    );
    // Present but not verified (no Touch ID): re-signed with UV cleared.
    let signed = device.sign(&delete_command(), SIGNER_ORIGIN);
    let mut authenticator = URL_SAFE_NO_PAD
        .decode(signed.authenticator_data.as_ref().unwrap())
        .unwrap();
    authenticator[32] = 0x01;
    let client = URL_SAFE_NO_PAD
        .decode(signed.client_data.as_ref().unwrap())
        .unwrap();
    let mut message = authenticator.clone();
    message.extend_from_slice(digest(&SHA256, &client).as_ref());
    let unverified = Signed {
        authenticator_data: Some(URL_SAFE_NO_PAD.encode(&authenticator)),
        signature: URL_SAFE_NO_PAD.encode(device.sign_bytes(&message)),
        ..signed.clone()
    };
    assert_eq!(
        guard.check(Some(&unverified), &expected(), MACHINE, NOW),
        Err(Refusal::BadSignature)
    );
    // A phone's plain signature under a passkey's id.
    let plain = Signed {
        authenticator_data: None,
        client_data: None,
        ..signed
    };
    assert_eq!(
        guard.check(Some(&plain), &expected(), MACHINE, NOW),
        Err(Refusal::BadSignature)
    );
}

#[test]
fn commands_hold_for_at_most_an_hour_from_when_they_were_signed() {
    let device = Device::new(KeyFormat::P256);
    let mut guard = guard_with(&[&device]);
    let signed = device.sign(&delete_command(), SIGNER_ORIGIN);
    let expires = NOW + 10 * 60 * 1000;
    assert_eq!(
        guard.check(Some(&signed), &expected(), MACHINE, expires),
        Err(Refusal::Expired)
    );
    // Signed by a clock far ahead of this machine's.
    assert_eq!(
        guard.check(Some(&signed), &expected(), MACHINE, NOW - CLOCK_SKEW_MS - 1),
        Err(Refusal::Expired)
    );
    let mut long = delete_command();
    long["expires"] = json!(NOW + MAX_LIFETIME_MS + 1);
    let signed = device.sign(&long, SIGNER_ORIGIN);
    assert_eq!(
        guard.check(Some(&signed), &expected(), MACHINE, NOW),
        Err(Refusal::Expired)
    );
}

#[test]
fn a_signature_off_the_contract_is_refused_never_taken_as_unsigned() {
    assert_eq!(Signed::of(None), Ok(None));
    assert_eq!(Signed::of(Some(json!(null))), Ok(None));
    assert!(Signed::of(Some(json!({ "device": "x" }))).is_err());
    assert!(Signed::of(Some(json!("signed"))).is_err());
}

#[test]
fn paired_devices_are_kept_in_a_private_file() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("paired-devices.json");
    let device = Device::new(KeyFormat::Webauthn);
    let mut devices = PairedDevices::load(Some(path.clone()));
    assert!(!devices.required());
    devices.add(device.paired());
    let mode = std::fs::metadata(&path).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o600);
    let again = PairedDevices::load(Some(path.clone()));
    assert_eq!(again.list(), &[device.paired()]);
    let mut again = again;
    assert!(again.remove(&device.key_id));
    assert!(!PairedDevices::load(Some(path)).required());
}

#[test]
fn public_keys_are_taken_raw_or_as_subject_public_key_info() {
    let device = Device::new(KeyFormat::Webauthn);
    let point = device.key.public_key().as_ref().to_vec();
    assert_eq!(p256_point(&point), Some(point.clone()));
    let mut spki = hex("3059301306072a8648ce3d020106082a8648ce3d030107034200");
    spki.extend_from_slice(&point);
    assert_eq!(p256_point(&spki), Some(point.clone()));
    assert_eq!(p256_point(&point[1..]), None);
}

fn hex(text: &str) -> Vec<u8> {
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&text[i..i + 2], 16).unwrap())
        .collect()
}

fn proposal(pairing: &Pairing, code: &str, device: &ProposedDevice) -> Proposal {
    Proposal {
        id: "01K8D2B3C4D5E6F7G8H9J0K1M2".into(),
        pairing: pairing.id().into(),
        device: device.clone(),
        tag: tag(code, pairing.id(), device),
    }
}

#[test]
fn a_device_with_the_code_is_paired_once_the_user_says_yes() {
    let mut pairing = Pairing::open(NOW);
    let code = pairing.code();
    assert_eq!(code.len(), 14);
    assert_eq!(&code[..4], pairing.id());
    assert_eq!(
        pairing.link(SIGNER_ORIGIN),
        format!("https://rakumux-sign.com/pair#{code}")
    );
    let device = Device::new(KeyFormat::Webauthn);
    let proposed = device.proposed("Chrome on MacBook");
    // Typed in lower case, with an O for a zero: the same code.
    let typed = code.to_lowercase().replace('0', "o");
    pairing.propose(&proposal(&pairing, &typed, &proposed), NOW + 1_000);
    assert_eq!(
        pairing.state(),
        &PairingState::Proposed {
            name: "Chrome on MacBook".into(),
            fingerprint: proposed_fingerprint(&proposed).unwrap(),
        }
    );
    let mut devices = PairedDevices::load(None);
    pairing.confirm(true, &mut devices, NOW + 2_000);
    assert_eq!(
        pairing.state(),
        &PairingState::Paired {
            name: "Chrome on MacBook".into()
        }
    );
    assert_eq!(devices.list().len(), 1);
    assert_eq!(devices.list()[0].key_id, device.key_id);
    assert_eq!(
        pairing.report().map(|(_, outcome)| outcome),
        Some(&PairingOutcome::Paired)
    );
    // And it signs for this machine from now on.
    let mut guard = Guard::new(SIGNER_ORIGIN, devices).unwrap();
    let signed = device.sign(&delete_command(), SIGNER_ORIGIN);
    assert_eq!(
        guard.check(Some(&signed), &expected(), MACHINE, NOW),
        Ok(())
    );
}

#[test]
fn a_key_without_the_code_closes_the_pairing_after_one_try() {
    let mut pairing = Pairing::open(NOW);
    let theirs = Device::new(KeyFormat::Webauthn).proposed("Not yours");
    // rakumux.com knows the pairing's id, not the rest of the code.
    let guess = format!("{}-0000-0000", pairing.id());
    pairing.propose(&proposal(&pairing, &guess, &theirs), NOW + 1_000);
    assert_eq!(
        pairing.state(),
        &PairingState::Failed {
            failure: PairingFailure::WrongCode
        }
    );
    // The right code after that is too late.
    let mine = Device::new(KeyFormat::Webauthn).proposed("Chrome");
    let code = pairing.code();
    pairing.propose(&proposal(&pairing, &code, &mine), NOW + 2_000);
    assert!(matches!(pairing.state(), PairingState::Failed { .. }));
}

#[test]
fn a_changed_key_or_name_fails_the_tag() {
    let mut pairing = Pairing::open(NOW);
    let code = pairing.code();
    let device = Device::new(KeyFormat::Webauthn).proposed("Chrome");
    let mut sent = proposal(&pairing, &code, &device);
    sent.device.public_key = Device::new(KeyFormat::Webauthn).proposed("x").public_key;
    pairing.propose(&sent, NOW);
    assert_eq!(
        pairing.state(),
        &PairingState::Failed {
            failure: PairingFailure::WrongCode
        }
    );
}

#[test]
fn a_pairing_expires_and_the_user_can_refuse() {
    let mut pairing = Pairing::open(NOW);
    pairing.tick(NOW + OPEN_MS);
    assert_eq!(
        pairing.state(),
        &PairingState::Failed {
            failure: PairingFailure::Expired
        }
    );
    let mut pairing = Pairing::open(NOW);
    let code = pairing.code();
    let device = Device::new(KeyFormat::P256).proposed("Pixel 9");
    pairing.propose(&proposal(&pairing, &code, &device), NOW);
    let mut devices = PairedDevices::load(None);
    pairing.confirm(false, &mut devices, NOW);
    assert!(devices.list().is_empty());
    assert_eq!(
        pairing.report().map(|(_, outcome)| outcome),
        Some(&PairingOutcome::Failed {
            failure: PairingFailure::Refused
        })
    );
}

#[test]
fn a_proposal_for_another_pairing_changes_nothing() {
    let mut pairing = Pairing::open(NOW);
    let code = pairing.code();
    let device = Device::new(KeyFormat::Webauthn).proposed("Chrome");
    let mut sent = proposal(&pairing, &code, &device);
    sent.pairing = if pairing.id() == "ZZZZ" {
        "YYYY"
    } else {
        "ZZZZ"
    }
    .into();
    pairing.propose(&sent, NOW);
    assert_eq!(pairing.state(), &PairingState::Open);
}

fn reply_signature(signed: Option<Signed>, names: &[&str]) -> ReplySignature {
    ReplySignature {
        signature: Ok(signed),
        fields: vec![
            ("session", json!(SESSION)),
            ("entry", json!(REQUEST)),
            ("text", json!("look at these")),
            ("answer", serde_json::Value::Null),
            ("choice", serde_json::Value::Null),
        ],
        names: names.iter().map(|name| (*name).to_owned()).collect(),
    }
}

/// A reply's files are signed by name and SHA-256, in order: the hashes come
/// back for the downloads to be checked against; another name or count is
/// refused. Unsigned, a reply goes in only while no device is paired, or when
/// this machine wrote it.
#[test]
fn a_replys_files_are_signed_by_name_and_hash() {
    let device = Device::new(KeyFormat::P256);
    let mut guard = guard_with(&[&device]).for_machine(MACHINE);
    let hash = "ab".repeat(32);
    let command = |files: serde_json::Value| {
        json!({
            "v": 1, "kind": "reply", "machine": MACHINE, "id": REQUEST,
            "at": NOW, "expires": NOW + 60_000, "session": SESSION, "entry": REQUEST,
            "text": "look at these", "attachments": files,
        })
    };
    let signed = device.sign(
        &command(json!([{"name": "a.png", "sha256": hash}])),
        SIGNER_ORIGIN,
    );
    assert_eq!(
        guard.reply(
            REQUEST,
            &reply_signature(Some(signed.clone()), &["a.png"]),
            false,
            NOW
        ),
        Ok(vec![hash.clone()])
    );
    assert_eq!(
        guard.reply(
            REQUEST,
            &reply_signature(Some(signed), &["b.png"]),
            false,
            NOW
        ),
        Err(Refusal::BadSignature)
    );
    let none = device.sign(&command(serde_json::Value::Null), SIGNER_ORIGIN);
    assert_eq!(
        guard.reply(
            REQUEST,
            &reply_signature(Some(none), &["a.png"]),
            false,
            NOW
        ),
        Err(Refusal::BadSignature),
        "a file the signer didn't attach"
    );
    assert_eq!(
        guard.reply(REQUEST, &reply_signature(None, &[]), false, NOW),
        Err(Refusal::Unsigned)
    );
    assert_eq!(
        guard.reply(REQUEST, &reply_signature(None, &[]), true, NOW),
        Ok(Vec::new()),
        "written in this machine's TUI"
    );
    let mut unpaired = guard_with(&[]).for_machine(MACHINE);
    assert_eq!(
        unpaired.reply(REQUEST, &reply_signature(None, &["a.png"]), false, NOW),
        Ok(Vec::new())
    );
}

/// Each signed command's outcome is written to the log once, however often
/// it's checked: the device that signed it, the kind and the session, never
/// what a reply says; a command refused (unsigned here) is written too, and
/// one read back from the file comes out the same.
#[test]
fn the_log_keeps_what_became_of_each_signed_command_once() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(log::FILE_NAME);
    let device = Device::new(KeyFormat::P256);
    let mut guard = guard_with(&[&device])
        .for_machine(MACHINE)
        .with_log(Some(path.clone()));
    let signed: Signature = Ok(Some(device.sign(&delete_command(), SIGNER_ORIGIN)));
    for _ in 0..3 {
        assert_eq!(guard.allows(&signed, &expected(), NOW), Ok(()));
    }
    let reply = json!({
        "v": 1, "kind": "reply", "machine": MACHINE, "id": SESSION,
        "at": NOW, "expires": NOW + 60_000, "session": SESSION, "entry": REQUEST,
        "text": "a secret plan", "attachments": null,
    });
    let reply_signed = device.sign(&reply, SIGNER_ORIGIN);
    let mut signature = reply_signature(Some(reply_signed), &[]);
    signature.fields[2] = ("text", json!("a secret plan"));
    assert_eq!(guard.reply(SESSION, &signature, false, NOW), Ok(Vec::new()));
    assert_eq!(
        guard.reply(REQUEST, &reply_signature(None, &[]), false, NOW),
        Err(Refusal::Unsigned)
    );
    assert_eq!(
        guard.reply(
            "01K8C2B3C4D5E6F7G8H9J0K1M2",
            &reply_signature(None, &[]),
            true,
            NOW
        ),
        Ok(Vec::new()),
        "this machine's own reply isn't a signed command"
    );

    let entries = log::read(&path);
    let name = device.paired().name;
    assert_eq!(
        entries,
        vec![
            log::LogEntry {
                at: NOW,
                kind: "delete_session".into(),
                id: REQUEST.into(),
                outcome: log::Outcome::Accepted,
                device: Some(device.paired().key_id),
                name: Some(name.clone()),
                session: Some(SESSION.into()),
            },
            log::LogEntry {
                at: NOW,
                kind: "reply".into(),
                id: SESSION.into(),
                outcome: log::Outcome::Accepted,
                device: Some(device.paired().key_id),
                name: Some(name),
                session: Some(SESSION.into()),
            },
            log::LogEntry {
                at: NOW,
                kind: "reply".into(),
                id: REQUEST.into(),
                outcome: log::Outcome::Unsigned,
                device: None,
                name: None,
                session: Some(SESSION.into()),
            },
        ]
    );
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(!text.contains("secret"), "never what a reply says");
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
}

/// What a browser's key on the signer site signs (WebCrypto's ECDSA P-256,
/// its r‖s turned into DER), and what the phone app signs without its native
/// key (`@noble/curves` in Expo Go), each one key paired as `p256`, holds
/// here, whatever lengths r and s come out as; a signature over other bytes
/// doesn't. The fixtures are made by those clients' own code in rakumux-cloud
/// (`signer/tests/machine-fixture.mjs`, and the mobile app's).
#[test]
fn a_browser_or_app_keys_signature_holds() {
    let mut fixture: Vec<serde_json::Value> =
        serde_json::from_str(include_str!("webcrypto-fixture.json")).unwrap();
    let app: Vec<serde_json::Value> =
        serde_json::from_str(include_str!("noble-fixture.json")).unwrap();
    assert!(fixture.len() >= 3 && !app.is_empty());
    fixture.extend(app);
    for case in &fixture {
        let mut kept = PairedDevices::load(None);
        assert!(kept.add(PairedDevice {
            key_id: case["key_id"].as_str().unwrap().into(),
            format: KeyFormat::P256,
            public_key: case["public_key"].as_str().unwrap().into(),
            name: "Chrome on Mac".into(),
            paired_at: 0,
            last_used_at: None,
        }));
        let command = &case["command"];
        let mut guard = Guard::new(SIGNER_ORIGIN, kept)
            .unwrap()
            .for_machine(command["machine"].as_str().unwrap());
        let signed: Signed = serde_json::from_value(case["signed"].clone()).unwrap();
        let at = command["at"].as_i64().unwrap();
        let signature = ReplySignature {
            signature: Ok(Some(signed.clone())),
            fields: vec![
                ("session", command["session"].clone()),
                ("entry", command["entry"].clone()),
                ("text", command["text"].clone()),
                ("answer", serde_json::Value::Null),
                ("choice", serde_json::Value::Null),
            ],
            names: Vec::new(),
        };
        let id = command["id"].as_str().unwrap();
        assert_eq!(
            guard.reply(id, &signature, false, at),
            Ok(Vec::new()),
            "{}",
            case["case"]
        );
        let mut forged = signature.clone();
        forged.fields[2] = ("text", json!("something else"));
        assert_eq!(
            guard.reply(id, &forged, false, at),
            Err(Refusal::BadSignature),
            "{}",
            case["case"]
        );
    }
}
