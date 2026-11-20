use super::*;
use crate::signing::pairing::{tag, PairingOutcome};
use crate::signing::testing::{guard, Device};

const MACHINE_ID: &str = "01K8C2B3C4D5E6F7G8H9J0K1M2";
const PROPOSAL: &str = "01K8D2B3C4D5E6F7G8H9J0K1M2";
const NOW: Duration = Duration::from_secs(10);

fn body(request: Option<FeedRequest>) -> serde_json::Value {
    match request {
        Some(FeedRequest::PairingOpen { body } | FeedRequest::PairingStatus { body }) => {
            serde_json::from_slice(&body).unwrap()
        }
        other => panic!("expected a pairing request, got {other:?}"),
    }
}

/// Opened at rakumux.com, a device sent with the code's tag is shown, paired
/// once the user says yes, reported, and listed with the session list.
#[test]
fn a_device_sent_with_the_code_is_paired_once_the_user_says_yes() {
    let now_ms = crate::signing::now_ms();
    let mut sync = PairingSync::new("studio".into(), guard(MACHINE_ID, &[]));
    sync.set_enabled(true);
    let view = sync.act(PairingAction::Start, now_ms).unwrap();
    assert!(!view.opened);
    assert!(view.link.ends_with(&format!("/pair#{}", view.code)));
    let open = body(sync.next_work(NOW, now_ms));
    let pairing = view.code[..4].to_owned();
    assert_eq!(open["pairing"], pairing.as_str(), "only the first group");
    assert!(!open.to_string().contains(&view.code[5..9]));
    assert_eq!(sync.next_work(NOW, now_ms), None, "one request out");
    sync.opened(true, Ok(false), NOW, now_ms);
    assert!(sync.waiting());
    assert!(sync.view(now_ms).unwrap().opened);

    let phone = Device::new(KeyFormat::P256);
    let device = phone.proposed("iPhone");
    let proposal = Proposal {
        id: PROPOSAL.into(),
        pairing: pairing.clone(),
        tag: tag(&view.code.to_lowercase(), &pairing, &device),
        device,
    };
    sync.read(true, &[proposal], now_ms);
    assert!(matches!(
        sync.view(now_ms).unwrap().state,
        PairingState::Proposed { ref name, .. } if name == "iPhone"
    ));
    assert!(!sync.waiting());
    assert!(
        sync.listed().devices.is_empty(),
        "not before the user says so"
    );
    sync.act(PairingAction::Answer { yes: true }, now_ms);
    let listed = sync.listed();
    assert_eq!(listed.machine_id, MACHINE_ID);
    assert_eq!(listed.devices.len(), 1);
    assert_eq!(listed.devices[0].name, "iPhone");
    let report = body(sync.next_work(NOW, now_ms));
    assert_eq!(report["status"], "paired");
    assert_eq!(report["proposal"], PROPOSAL);
    assert!(report.get("failure").is_none());
    sync.reported(true, Ok(()), NOW);
    assert_eq!(sync.next_work(NOW, now_ms), None);
    assert_eq!(sync.devices().len(), 1);
    assert!(sync.remove(&phone.key_id));
    assert!(sync.listed().devices.is_empty());
}

/// A proposal whose tag wasn't made with the code ends the pairing.
#[test]
fn a_wrong_tag_ends_the_pairing_and_is_reported() {
    let now_ms = crate::signing::now_ms();
    let mut sync = PairingSync::new("studio".into(), guard(MACHINE_ID, &[]));
    sync.set_enabled(true);
    let view = sync.act(PairingAction::Start, now_ms).unwrap();
    sync.next_work(NOW, now_ms);
    sync.opened(true, Ok(false), NOW, now_ms);
    let pairing = view.code[..4].to_owned();
    let device = Device::new(KeyFormat::Webauthn).proposed("Chrome");
    let proposal = Proposal {
        id: PROPOSAL.into(),
        pairing: pairing.clone(),
        tag: tag(&format!("{pairing}00000000"), &pairing, &device),
        device,
    };
    sync.read(true, &[proposal], now_ms);
    assert_eq!(
        sync.view(now_ms).unwrap().state,
        PairingState::Failed {
            failure: PairingFailure::WrongCode
        }
    );
    let report = body(sync.next_work(NOW, now_ms));
    assert_eq!(report["status"], "failed");
    assert_eq!(report["failure"], "wrong_code");
    let _ = PairingOutcome::Paired;
}

/// A code another machine's pairing holds is replaced by a new one.
#[test]
fn a_taken_code_is_replaced() {
    let now_ms = crate::signing::now_ms();
    let mut sync = PairingSync::new("studio".into(), guard(MACHINE_ID, &[]));
    sync.set_enabled(true);
    let first = sync.act(PairingAction::Start, now_ms).unwrap().code;
    sync.next_work(NOW, now_ms);
    sync.opened(true, Ok(true), NOW, now_ms);
    let second = sync.view(now_ms).unwrap();
    assert_ne!(second.code, first);
    assert!(!second.opened);
    let open = body(sync.next_work(NOW, now_ms));
    assert_eq!(open["pairing"], &second.code[..4]);
}

#[test]
fn opening_answers_are_read() {
    let reply = |status, body: &str| FeedHttpReply {
        status,
        retry_after: None,
        body: body.as_bytes().to_vec(),
    };
    assert_eq!(
        opened(&reply(
            200,
            r#"{"version":1,"pairing":"K7QM","expires_at":1}"#
        )),
        Ok(false)
    );
    assert_eq!(
        opened(&reply(
            409,
            r#"{"error":{"code":"pairing_taken","message":"Another pairing uses this code."}}"#
        )),
        Ok(true)
    );
    assert_eq!(
        opened(&reply(
            409,
            r#"{"error":{"code":"machine_taken","message":"Another machine uses this name."}}"#
        )),
        Err(FeedFailure::MachineTaken)
    );
}
