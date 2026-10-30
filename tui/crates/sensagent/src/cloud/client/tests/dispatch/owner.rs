//! The owner's socket in front of a running refresher: the real server and client,
//! a fake daemon and fake HTTP, a temporary Rakumux home.

use std::io::Write;
use std::os::unix::net::UnixStream;
use std::sync::mpsc::Sender;

use serde::Serialize;
use session_core::paths::Layout;

use super::refresh::tracked_view;
use super::refresh::{eventually, Background, Event};
use super::*;
use crate::owner::protocol::{
    read_message, write_message, FolderChoice, Hello, LineState, OwnerRequest, OwnerResponse,
    SignInReason, SignInView, MAX_MESSAGE, MAX_SESSIONS, PROTOCOL,
};
use crate::owner::{
    OwnerClaim, OwnerClient, OwnerError, OwnerErrorCode, OwnerLayout, OwnerRegistration,
    OwnerServer,
};
use crate::refresh::{RefreshClient, SignInFailure, SignInPrompt, SignInState};

struct Served {
    background: Background,
    _events: Vec<Sender<Event>>,
    _home: tempfile::TempDir,
    layout: OwnerLayout,
    claim: OwnerClaim,
    server: OwnerServer,
}

impl Served {
    fn start() -> Self {
        Self::serving(|_, claim, client| {
            OwnerServer::start(claim, client, "synthetic-version").unwrap()
        })
    }

    /// Sign-outs revoke the removed sign-in with `revoker` (as the service
    /// does with Clerk).
    fn revoking(revoker: Arc<dyn crate::auth::GrantRevoker>) -> Self {
        Self::serving(move |account, claim, client| {
            OwnerServer::start_revoking(
                claim,
                client,
                "synthetic-version",
                Arc::new(move || account.revoke_signed_out(revoker.as_ref())),
            )
            .unwrap()
        })
    }

    fn serving(
        start: impl FnOnce(
            Arc<AccountService<FileCredentialStore, FakeClock>>,
            &mut OwnerClaim,
            RefreshClient,
        ) -> OwnerServer,
    ) -> Self {
        let (background, events) = Background::start(&[tracked_view(2)], 1);
        let home = tempfile::Builder::new()
            .prefix("synthetic-owner-home-")
            .tempdir()
            .unwrap();
        let layout = OwnerLayout::new(&Layout::with_home(home.path().to_path_buf()));
        let mut claim = OwnerClaim::claim(&layout, "synthetic-version").unwrap();
        let server = start(
            background.account.clone(),
            &mut claim,
            background.client.clone(),
        );
        Self {
            background,
            _events: events,
            _home: home,
            layout,
            claim,
            server,
        }
    }

    fn client(&self) -> OwnerClient {
        OwnerClient::connect(&self.layout, "synthetic-test").unwrap()
    }

    /// A hello-less connection, for protocol tests.
    fn raw(&self) -> UnixStream {
        let stream = UnixStream::connect(self.layout.socket()).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        stream
    }

    fn hello(&self, protocol: u32, token: &str) -> (UnixStream, OwnerResponse) {
        let mut stream = self.raw();
        let hello = Hello {
            protocol,
            client: "synthetic-raw".into(),
            token: token.into(),
        };
        write_message(&mut stream, &hello).unwrap();
        let response = read_message(&mut stream).unwrap().unwrap();
        (stream, response)
    }

    fn token(&self) -> String {
        OwnerRegistration::load(&self.layout).unwrap().token
    }
}

fn ask<T: Serialize>(stream: &mut UnixStream, request: &T) -> OwnerResponse {
    write_message(stream, request).unwrap();
    read_message(stream).unwrap().unwrap()
}

/// The consent folders on the wire.
fn choices(include_output: bool) -> Vec<FolderChoice> {
    folders(include_output)
        .iter()
        .map(FolderChoice::from)
        .collect()
}

fn refused(code: OwnerErrorCode) -> OwnerResponse {
    OwnerResponse::Error { code }
}

#[test]
fn user_clients_read_state_and_status_lines() {
    let served = Served::start();
    let client = served.client();
    assert_eq!(client.version(), "synthetic-version");
    let mut line = None;
    eventually(|| {
        let lines = client.statuses(&[session(2), session(9)]).unwrap();
        assert_eq!(lines[1].session, session(9));
        line = Some(lines[0].clone());
        lines[0].state == LineState::Ready
    });
    let line = line.unwrap();
    assert_eq!(line.text.as_deref(), Some(STATUS));
    assert!(line.captured_at.is_some());
    assert_eq!(line.failure, None);

    let state = client.state().unwrap();
    let account = state.account.unwrap();
    assert_eq!(account.subject, SUBJECT);
    assert_eq!(account.email.as_deref(), Some("person@example.com"));
    assert!(state.consent.enabled);
    assert_eq!(state.consent.folders, choices(true));
    assert!(state.connected);
    assert!(!state.rate_limited && !state.unauthorized);
}

#[test]
fn consent_offers_are_shown_then_used_once_by_their_own_connection() {
    let served = Served::start();
    let client = served.client();
    let other = served.client();
    let revision = client.state().unwrap().consent.revision;

    let offer = client.consent_offer().unwrap();
    assert_eq!(offer.origin, ORIGIN);
    assert_eq!(offer.account.subject, SUBJECT);
    assert_eq!(offer.account.email.as_deref(), Some("person@example.com"));
    assert_eq!(offer.consent_revision, revision);
    assert_eq!(offer.id.len(), 32);
    let stale = OwnerError::Refused(OwnerErrorCode::StaleOffer);
    assert_eq!(other.enable(&offer.id, choices(true)).unwrap_err(), stale);
    assert_eq!(
        client.enable("not-an-offer", choices(true)).unwrap_err(),
        stale
    );

    let consent = client.enable(&offer.id, choices(false)).unwrap();
    assert!(consent.enabled);
    assert_eq!(consent.revision, revision + 1);
    assert_eq!(consent.folders, choices(false));
    assert_eq!(client.enable(&offer.id, choices(true)).unwrap_err(), stale);

    // An offer for an older consent revision can't enable anything.
    let outdated = client.consent_offer().unwrap();
    let disabled = other.disable(consent.revision).unwrap();
    assert!(!disabled.enabled);
    assert_eq!(
        other.disable(consent.revision).unwrap_err(),
        OwnerError::Refused(OwnerErrorCode::Conflict)
    );
    assert_eq!(
        client.enable(&outdated.id, choices(true)).unwrap_err(),
        stale
    );
    assert!(!client.state().unwrap().consent.enabled);

    client.sign_out().unwrap();
    let state = other.state().unwrap();
    assert_eq!(state.account, None);
    let lines = other.statuses(&[session(2)]).unwrap();
    assert_eq!(lines[0].state, LineState::Disabled);
    assert_eq!(
        client.consent_offer().unwrap_err(),
        OwnerError::Refused(OwnerErrorCode::SignedOut)
    );
}

/// Records what it's asked to revoke and answers with `result`, as Clerk would.
struct Revoker {
    asked: Mutex<Vec<String>>,
    result: Result<(), crate::auth::AuthError>,
}

impl crate::auth::GrantRevoker for Revoker {
    fn revoke(
        &self,
        grant: &crate::auth::AuthorizationGrant,
    ) -> Result<(), crate::auth::AuthError> {
        self.asked
            .lock()
            .unwrap()
            .push(grant.access_token().expose_secret().to_owned());
        self.result.clone()
    }
}

#[test]
fn signing_out_revokes_the_sign_in_and_says_whether_clerk_did() {
    use crate::auth::{AuthError, NetworkFailure};
    use crate::owner::protocol::RevocationView;
    for (result, expected) in [
        (Ok(()), RevocationView::Revoked),
        (
            Err(AuthError::Network(NetworkFailure::Connection)),
            RevocationView::Failed,
        ),
    ] {
        let revoker = Arc::new(Revoker {
            asked: Mutex::default(),
            result,
        });
        let served = Served::revoking(revoker.clone());
        let client = served.client();
        let other = served.client();
        assert!(client.state().unwrap().account.is_some());
        assert_eq!(client.sign_out().unwrap(), expected);
        // Signed out here whatever Clerk said.
        assert_eq!(other.state().unwrap().account, None);
        assert_eq!(revoker.asked.lock().unwrap().len(), 1);
        // Once: signing out again finds nothing to revoke.
        assert_eq!(client.sign_out().unwrap(), RevocationView::NotNeeded);
        assert_eq!(revoker.asked.lock().unwrap().len(), 1);
    }

    // An older client's sign_out is answered as it was, and revokes all the same.
    let revoker = Arc::new(Revoker {
        asked: Mutex::default(),
        result: Ok(()),
    });
    let served = Served::revoking(revoker.clone());
    let token = served.token();
    let (mut stream, welcome) = served.hello(PROTOCOL, &token);
    assert!(matches!(welcome, OwnerResponse::Welcome { .. }));
    assert_eq!(
        ask(&mut stream, &OwnerRequest::SignOut),
        OwnerResponse::Done
    );
    assert_eq!(revoker.asked.lock().unwrap().len(), 1);
}

#[test]
fn unauthenticated_and_malformed_clients_are_refused() {
    let served = Served::start();
    let token = served.token();

    let (mut stream, response) = served.hello(PROTOCOL, "synthetic-wrong-token");
    assert_eq!(response, refused(OwnerErrorCode::Unauthenticated));
    assert!(read_message::<_, OwnerResponse>(&mut stream)
        .unwrap()
        .is_none());
    let (_, response) = served.hello(PROTOCOL + 1, &token);
    assert_eq!(response, refused(OwnerErrorCode::ProtocolMismatch));
    let mut stream = served.raw();
    assert_eq!(
        ask(&mut stream, &OwnerRequest::State),
        refused(OwnerErrorCode::Unauthenticated)
    );

    let (mut stream, response) = served.hello(PROTOCOL, &token);
    assert!(matches!(response, OwnerResponse::Welcome { .. }));
    let unknown = serde_json::json!({"op": "launch_everything"});
    assert_eq!(
        ask(&mut stream, &unknown),
        refused(OwnerErrorCode::InvalidRequest)
    );
    let too_many = OwnerRequest::Statuses {
        sessions: (0..=MAX_SESSIONS).map(|_| session(2)).collect(),
    };
    assert_eq!(
        ask(&mut stream, &too_many),
        refused(OwnerErrorCode::InvalidRequest)
    );
    assert!(matches!(
        ask(&mut stream, &OwnerRequest::State),
        OwnerResponse::State { .. }
    ));
    // An oversized frame ends the connection before its body is read.
    stream
        .write_all(&((MAX_MESSAGE + 1) as u32).to_be_bytes())
        .unwrap();
    assert!(read_message::<_, OwnerResponse>(&mut stream).map_or(true, |end| end.is_none()));

    let ids: Vec<SessionId> = (0..=MAX_SESSIONS).map(|_| session(2)).collect();
    assert_eq!(
        served.client().statuses(&ids).unwrap_err(),
        OwnerError::TooManySessions
    );
}

#[test]
fn stopping_ends_clients_and_the_claim_removes_its_files() {
    let Served {
        background,
        _events,
        _home,
        layout,
        claim,
        server,
    } = Served::start();
    let client = OwnerClient::connect(&layout, "synthetic-test").unwrap();
    assert!(client.state().is_ok());
    let Background { runner, .. } = background;
    runner.stop();
    assert_eq!(
        client.state().unwrap_err(),
        OwnerError::Refused(OwnerErrorCode::Stopped)
    );

    server.stop();
    assert_eq!(client.state().unwrap_err(), OwnerError::Connection);
    assert_eq!(
        OwnerClient::connect(&layout, "synthetic-test").unwrap_err(),
        OwnerError::NotRunning
    );
    drop(claim);
    assert!(!layout.registration_file().exists());
    assert!(!layout.socket().exists());
    assert_eq!(
        OwnerClient::connect(&layout, "synthetic-test").unwrap_err(),
        OwnerError::NotRunning
    );
}

#[test]
fn a_slow_hello_is_cut_off_and_clients_past_the_limit_are_told_busy() {
    let served = Served::start();
    // A small hello sent a byte at a time stays under each read's timeout, but not
    // the whole hello's.
    let mut slow = served.raw();
    let started = Instant::now();
    slow.write_all(&101u32.to_be_bytes()).unwrap();
    slow.write_all(&[0]).unwrap();
    let mut closed = false;
    for _ in 0..40 {
        thread::sleep(Duration::from_millis(100));
        if slow.write_all(b" ").is_err() {
            closed = true;
            break;
        }
    }
    assert!(closed);
    assert!(started.elapsed() < Duration::from_secs(3));

    let held: Vec<UnixStream> = (0..32).map(|_| served.raw()).collect();
    let mut late = served.raw();
    assert_eq!(
        read_message::<_, OwnerResponse>(&mut late).unwrap(),
        Some(refused(OwnerErrorCode::Busy))
    );
    drop(held);
    eventually(|| OwnerClient::connect(&served.layout, "synthetic-test").is_ok());
}

#[test]
fn sign_in_is_idle_until_started_and_its_view_holds_only_public_values() {
    let served = Served::start();
    let client = served.client();
    assert_eq!(client.sign_in_status().unwrap(), SignInView::Idle);
    client.sign_in_cancel().unwrap();
    assert_eq!(client.sign_in_status().unwrap(), SignInView::Idle);

    let waiting = SignInView::from(&SignInState::Waiting(SignInPrompt {
        user_code: "WDJB-MJHT".into(),
        verification_uri: "https://accounts.example/device".into(),
        verification_uri_complete: None,
        connect_url: Some("https://cloud.example/connect?code=WDJB-MJHT".into()),
        expires_in_seconds: 600,
    }));
    assert_eq!(
        serde_json::to_value(&waiting).unwrap(),
        json!({
            "state": "waiting",
            "user_code": "WDJB-MJHT",
            "verification_uri": "https://accounts.example/device",
            "verification_uri_complete": null,
            "connect_url": "https://cloud.example/connect?code=WDJB-MJHT",
            "expires_in_seconds": 600,
        })
    );
    assert_eq!(
        SignInView::from(&SignInState::Failed(SignInFailure::Expired)),
        SignInView::Failed {
            reason: SignInReason::Expired
        }
    );
    // A newer service's states and reasons stay readable.
    let newer: SignInView = serde_json::from_value(json!({"state": "newer"})).unwrap();
    assert_eq!(newer, SignInView::Unknown);
    let reason: SignInView =
        serde_json::from_value(json!({"state": "failed", "reason": "newer"})).unwrap();
    assert_eq!(
        reason,
        SignInView::Failed {
            reason: SignInReason::Unknown
        }
    );
}

#[test]
fn switches_are_changed_for_the_signed_in_account_only() {
    let served = Served::start();
    let client = served.client();
    client.set_features(Some(false), None).unwrap();
    client.set_features(None, Some(true)).unwrap();
    assert!(!client.state().unwrap().features_unsaved);
    // Both absent is no change; the wire leaves them out.
    let mut stream = served.hello(PROTOCOL, &served.token()).0;
    let request = OwnerRequest::SetFeatures {
        ai_session_summaries: None,
        feed: None,
    };
    assert_eq!(
        serde_json::to_value(&request).unwrap(),
        serde_json::json!({"op": "set_features"})
    );
    assert_eq!(ask(&mut stream, &request), OwnerResponse::Done);
    client.sign_out().unwrap();
    assert_eq!(
        client.set_features(Some(true), None).unwrap_err(),
        OwnerError::Refused(OwnerErrorCode::SignedOut)
    );
}

#[test]
fn a_client_can_ask_the_service_to_stop() {
    let served = Served::start();
    assert!(!served.server.shutdown_requested());
    served.client().shutdown().unwrap();
    assert!(served.server.shutdown_requested());
}
