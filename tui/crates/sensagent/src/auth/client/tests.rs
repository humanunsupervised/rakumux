use super::*;
use crate::auth::tests::{config, device_json, grant, grant_json, verified_now as verified};
use crate::auth::{AuthService, PollCompletion};
use serde_json::{json, Value};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

struct RecordedRequest {
    endpoint: Url,
    form: Option<Vec<(String, String)>>,
    bearer: Option<String>,
}

struct FakeTransport {
    replies: Mutex<VecDeque<Result<HttpReply, AuthError>>>,
    recorded: Arc<Mutex<Vec<RecordedRequest>>>,
}

impl HttpTransport for FakeTransport {
    fn request(
        &self,
        endpoint: &Url,
        form: Option<&[(&str, &str)]>,
        bearer: Option<&SecretValue>,
    ) -> Result<HttpReply, AuthError> {
        self.recorded.lock().unwrap().push(RecordedRequest {
            endpoint: endpoint.clone(),
            form: form.map(|pairs| {
                pairs
                    .iter()
                    .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
                    .collect()
            }),
            bearer: bearer.map(|token| token.expose_secret().to_owned()),
        });
        self.replies
            .lock()
            .unwrap()
            .pop_front()
            .expect("unexpected request")
    }
}

fn metadata() -> Value {
    json!({
        "issuer": "https://clerk.example.com",
        "device_authorization_endpoint": "https://clerk.example.com/oauth/device_authorization",
        "token_endpoint": "https://clerk.example.com/oauth/token",
        "grant_types_supported": [DEVICE_GRANT_TYPE, "refresh_token"]
    })
}

fn reply(status: u16, body: Value) -> Result<HttpReply, AuthError> {
    Ok(HttpReply {
        receipt: TokenReceipt::now(),
        status,
        content_type: Some("application/json; charset=utf-8".into()),
        retry_after: None,
        body: serde_json::to_vec(&body).unwrap(),
    })
}

type Record = Arc<Mutex<Vec<RecordedRequest>>>;

fn transport(replies: Vec<Result<HttpReply, AuthError>>) -> (Box<dyn HttpTransport>, Record) {
    let recorded = Arc::new(Mutex::new(Vec::new()));
    (
        Box::new(FakeTransport {
            replies: Mutex::new(replies.into()),
            recorded: recorded.clone(),
        }),
        recorded,
    )
}

fn client(replies: Vec<Result<HttpReply, AuthError>>) -> (ClerkDeviceClient, Record) {
    let (transport, recorded) = transport(replies);
    (
        ClerkDeviceClient::with_transport(config(), transport).unwrap(),
        recorded,
    )
}

fn poll_request() -> PollRequest {
    let response: DeviceResponse = serde_json::from_value(device_json()).unwrap();
    let mut service = AuthService::default();
    let attempt = service.start().unwrap();
    service.complete_start(attempt, response.validate(&config()), Duration::ZERO);
    service.next_poll(Duration::from_secs(5)).unwrap()
}

#[test]
fn token_grants_keep_the_transport_receipt_through_account_verification() {
    let receipt = TokenReceipt {
        wall: std::time::UNIX_EPOCH + Duration::from_secs(1000),
        monotonic: crate::auth::tests::clock_origin(),
    };
    let mut response = reply(200, grant_json()).unwrap();
    response.receipt = receipt;
    let (client, _) = client(vec![
        reply(200, account_metadata()),
        Ok(response),
        reply(200, account_json("user_one")),
    ]);
    let PollOutcome::Granted(grant) = client.poll(&poll_request()) else {
        panic!("expected grant");
    };
    let actual = grant.receipt.unwrap();
    assert_eq!(actual.wall, receipt.wall);
    assert_eq!(actual.monotonic, receipt.monotonic);
    let verified = client.verify_account(grant).unwrap();
    let actual = verified.grant.receipt.unwrap();
    assert_eq!(actual.wall, receipt.wall);
    assert_eq!(actual.monotonic, receipt.monotonic);
}

#[test]
fn discovers_advertised_endpoints_and_sends_a_public_client_request() {
    let (client, recorded) = client(vec![reply(200, metadata()), reply(200, device_json())]);
    let authorization = client.start().unwrap();
    assert_eq!(authorization.prompt().user_code(), "BCDF-GHJK");
    let requests = recorded.lock().unwrap();
    assert_eq!(
        requests[0].endpoint.as_str(),
        "https://clerk.example.com/.well-known/oauth-authorization-server"
    );
    assert!(requests[0].form.is_none());
    assert_eq!(
        requests[1].endpoint.as_str(),
        "https://clerk.example.com/oauth/device_authorization"
    );
    let form = requests[1].form.as_ref().unwrap();
    assert_eq!(form.len(), 2);
    assert!(form.contains(&("client_id".into(), "public-client".into())));
    assert!(form.contains(&(
        "scope".into(),
        "openid offline_access ai:session-status".into()
    )));
    assert!(!form.iter().any(|(key, _)| key == "client_secret"));
}

#[test]
fn missing_capability_or_endpoint_never_falls_back_to_a_guessed_route() {
    for field in ["device_authorization_endpoint", "grant_types_supported"] {
        let mut body = metadata();
        body.as_object_mut().unwrap().remove(field);
        let (transport, recorded) = transport(vec![reply(200, body)]);
        let error = ClerkDeviceClient::with_transport(config(), transport)
            .err()
            .unwrap();
        assert_eq!(error, AuthError::UnsupportedDeviceGrant);
        assert_eq!(recorded.lock().unwrap().len(), 1);
    }
}

#[test]
fn discovery_rejects_cross_origin_insecure_or_credential_bearing_endpoints() {
    for (field, value) in [
        ("issuer", "https://other.example.com"),
        ("issuer", "https://clerk.example.com/nested"),
        ("token_endpoint", "https://other.example.com/token"),
        (
            "device_authorization_endpoint",
            "http://clerk.example.com/device",
        ),
        (
            "token_endpoint",
            "https://user:password@clerk.example.com/token",
        ),
        (
            "token_endpoint",
            "https://clerk.example.com/token?secret=unexpected",
        ),
    ] {
        let mut body = metadata();
        body[field] = json!(value);
        let (transport, recorded) = transport(vec![reply(200, body)]);
        assert_eq!(
            ClerkDeviceClient::with_transport(config(), transport)
                .err()
                .unwrap(),
            AuthError::UntrustedEndpoint
        );
        assert_eq!(recorded.lock().unwrap().len(), 1);
    }
}

#[test]
fn poll_sends_device_code_only_to_the_discovered_token_endpoint() {
    let (client, recorded) = client(vec![reply(200, metadata()), reply(200, grant_json())]);
    let request = poll_request();
    match client.poll(&request) {
        PollOutcome::Granted(grant) => {
            assert_eq!(grant.access_token().expose_secret(), "PRIVATE-ACCESS-TOKEN")
        }
        other => panic!("unexpected poll outcome: {other:?}"),
    }
    let recorded = recorded.lock().unwrap();
    assert_eq!(
        recorded[1].endpoint.as_str(),
        "https://clerk.example.com/oauth/token"
    );
    let form = recorded[1].form.as_ref().unwrap();
    assert_eq!(form.len(), 3);
    assert!(form.contains(&("grant_type".into(), DEVICE_GRANT_TYPE.into())));
    assert!(form.contains(&("device_code".into(), "PRIVATE-DEVICE-CODE".into())));
    assert!(!form.iter().any(|(key, _)| key == "client_secret"));
}

#[test]
fn a_request_cannot_be_polled_with_a_different_public_client() {
    let other_config = DeviceAuthConfig::new(
        "https://clerk.example.com",
        "another-client",
        "https://accounts.example.com/device",
        &["openid"],
    )
    .unwrap();
    let (transport, recorded) = transport(vec![reply(200, metadata())]);
    let client = ClerkDeviceClient::with_transport(other_config, transport).unwrap();
    assert!(matches!(
        client.poll(&poll_request()),
        PollOutcome::Rejected(AuthError::UntrustedEndpoint)
    ));
    assert_eq!(
        recorded.lock().unwrap().len(),
        1,
        "must not submit the private code"
    );
}

#[test]
fn oauth_poll_errors_have_explicit_pending_slow_down_denial_and_expiry() {
    for (code, expected) in [
        ("authorization_pending", "pending"),
        ("slow_down", "slow"),
        ("access_denied", "denied"),
        ("expired_token", "expired"),
        ("invalid_grant", "fatal"),
        ("invalid_client", "fatal"),
        ("unexpected", "fatal"),
    ] {
        let (client, _) = client(vec![
            reply(200, metadata()),
            reply(
                400,
                json!({"error": code, "error_description": "PRIVATE-PROVIDER-DESCRIPTION"}),
            ),
        ]);
        let result = client.poll(&poll_request());
        assert!(!format!("{result:?}").contains("PRIVATE-PROVIDER-DESCRIPTION"));
        assert!(matches!(
            (expected, result),
            ("pending", PollOutcome::Pending)
                | ("slow", PollOutcome::SlowDown)
                | ("denied", PollOutcome::Denied)
                | ("expired", PollOutcome::Expired)
                | ("fatal", PollOutcome::Rejected(_))
        ));
    }
}

#[test]
fn invalid_client_scope_and_grant_fail_with_safe_actionable_errors() {
    for (code, expected) in [
        ("invalid_client", AuthError::InvalidClient),
        ("invalid_scope", AuthError::InvalidScope),
        ("invalid_grant", AuthError::InvalidGrant),
    ] {
        let (client, _) = client(vec![
            reply(200, metadata()),
            reply(
                400,
                json!({
                    "error": code, "error_description": "PRIVATE-PROVIDER-DETAILS"
                }),
            ),
        ]);
        let error = client.start().unwrap_err();
        assert_eq!(error, expected);
        assert!(!format!("{error:?}").contains("PRIVATE-PROVIDER-DETAILS"));
    }
}

#[test]
fn transient_transport_and_provider_errors_are_retryable_not_silent() {
    for response in [
        Err(AuthError::Network(NetworkFailure::Timeout)),
        Err(AuthError::ResponseRead),
        reply(503, json!({"error": "unavailable"})),
    ] {
        let (client, _) = client(vec![reply(200, metadata()), response]);
        assert!(matches!(
            client.poll(&poll_request()),
            PollOutcome::Retryable { .. }
        ));
    }
    let mut limited = reply(429, json!({})).unwrap();
    limited.retry_after = Some(Duration::from_secs(31));
    let (client, _) = client(vec![reply(200, metadata()), Ok(limited)]);
    assert!(
        matches!(client.poll(&poll_request()), PollOutcome::Retryable { retry_after: Some(delay), .. } if delay == Duration::from_secs(31))
    );
}

#[test]
fn redirects_and_unknown_http_failures_are_fatal() {
    let (transport, _) = transport(vec![reply(302, json!({"private": "DO-NOT-PRINT"}))]);
    assert_eq!(
        ClerkDeviceClient::with_transport(config(), transport)
            .err()
            .unwrap(),
        AuthError::HttpStatus(302)
    );
    for status in [301, 302, 307, 401, 404] {
        let (client, recorded) = client(vec![reply(200, metadata()), reply(status, json!({}))]);
        assert!(
            matches!(client.poll(&poll_request()), PollOutcome::Rejected(AuthError::HttpStatus(actual)) if actual == status)
        );
        assert_eq!(recorded.lock().unwrap().len(), 2);
    }
}

#[test]
fn bounded_json_and_mime_validation_do_not_echo_provider_content() {
    for response in [
        HttpReply {
            receipt: TokenReceipt::now(),
            status: 200,
            content_type: Some("application/json".into()),
            retry_after: None,
            body: vec![b'x'; MAX_RESPONSE_BYTES + 1],
        },
        HttpReply {
            receipt: TokenReceipt::now(),
            status: 200,
            content_type: Some("text/html".into()),
            retry_after: None,
            body: b"PRIVATE-HTML-ERROR".to_vec(),
        },
        HttpReply {
            receipt: TokenReceipt::now(),
            status: 200,
            content_type: Some("application/json".into()),
            retry_after: None,
            body: b"PRIVATE-MALFORMED-JSON".to_vec(),
        },
    ] {
        let (transport, _) = transport(vec![Ok(response)]);
        let error = ClerkDeviceClient::with_transport(config(), transport)
            .err()
            .unwrap();
        assert!(!format!("{error:?}").contains("PRIVATE-"));
        assert!(!error.to_string().contains("PRIVATE-"));
    }
}

#[test]
fn client_and_service_can_drive_the_complete_provider_grant_without_app_sign_in() {
    let (client, recorded) = client(vec![
        reply(200, metadata()),
        reply(200, device_json()),
        reply(400, json!({"error": "authorization_pending"})),
        reply(400, json!({"error": "slow_down"})),
        reply(200, grant_json()),
    ]);
    let mut service = AuthService::default();
    let attempt = service.start().unwrap();
    service.complete_start(attempt, client.start(), Duration::ZERO);
    for now in [5, 10, 20] {
        let now = Duration::from_secs(now);
        let request = service.next_poll(now).unwrap();
        let outcome = client.poll(&request);
        let completion = service.complete_poll(request.id(), outcome, now);
        if now.as_secs() == 20 {
            assert!(matches!(completion, PollCompletion::Authorized(_)));
        }
    }
    assert!(matches!(
        service.status(Duration::from_secs(20)),
        crate::auth::AuthStatus::ProviderAuthorized(_)
    ));
    assert_eq!(recorded.lock().unwrap().len(), 5);
}

fn account_metadata() -> Value {
    let mut value = metadata();
    value["userinfo_endpoint"] = json!("https://clerk.example.com/oauth/userinfo");
    value
}

fn account_json(subject: &str) -> Value {
    json!({"sub": subject, "email": "person@example.com", "email_verified": true, "name": "Example Person"})
}

#[test]
fn account_verification_uses_the_exact_grant_as_bearer_and_returns_no_token_in_identity() {
    let (client, recorded) = client(vec![
        reply(200, account_metadata()),
        reply(200, account_json("user_one")),
    ]);
    let authorization = client.verify_account(grant()).unwrap();
    assert_eq!(authorization.account().subject(), "user_one");
    assert_eq!(authorization.account().email(), Some("person@example.com"));
    assert!(authorization.account().email_verified());
    let debug = format!("{authorization:?}");
    assert!(!debug.contains("PRIVATE-ACCESS-TOKEN"));
    assert!(!debug.contains("person@example.com"));
    let recorded = recorded.lock().unwrap();
    assert_eq!(
        recorded[1].endpoint.as_str(),
        "https://clerk.example.com/oauth/userinfo"
    );
    assert_eq!(recorded[1].bearer.as_deref(), Some("PRIVATE-ACCESS-TOKEN"));
    assert!(recorded[1].form.is_none());
    assert!(recorded[0].bearer.is_none());
}

#[test]
fn account_verification_can_discover_oidc_userinfo_without_guessing_an_endpoint() {
    let (client, recorded) = client(vec![
        reply(200, metadata()),
        reply(
            200,
            json!({"issuer": "https://clerk.example.com", "userinfo_endpoint": "https://clerk.example.com/account-info"}),
        ),
        reply(200, account_json("user_one")),
    ]);
    client.verify_account(grant()).unwrap();
    let recorded = recorded.lock().unwrap();
    assert_eq!(
        recorded[1].endpoint.path(),
        "/.well-known/openid-configuration"
    );
    assert!(recorded[1].bearer.is_none());
    assert_eq!(recorded[2].endpoint.path(), "/account-info");
    assert_eq!(recorded[2].bearer.as_deref(), Some("PRIVATE-ACCESS-TOKEN"));
}

#[test]
fn untrusted_or_missing_userinfo_never_receives_a_bearer_token() {
    for (info, expected) in [
        (
            json!({"issuer": "https://clerk.example.com", "userinfo_endpoint": "https://other.example.com/userinfo"}),
            AuthError::UntrustedEndpoint,
        ),
        (
            json!({"issuer": "https://other.example.com", "userinfo_endpoint": "https://clerk.example.com/userinfo"}),
            AuthError::UntrustedEndpoint,
        ),
        (
            json!({"issuer": "https://clerk.example.com"}),
            AuthError::UnsupportedAccountVerification,
        ),
    ] {
        let (client, recorded) = client(vec![reply(200, metadata()), reply(200, info)]);
        assert_eq!(client.verify_account(grant()).unwrap_err(), expected);
        assert!(recorded
            .lock()
            .unwrap()
            .iter()
            .all(|request| request.bearer.is_none()));
    }
    let mut invalid = account_metadata();
    invalid["userinfo_endpoint"] = json!("https://other.example.com/userinfo");
    let (transport, recorded) = transport(vec![reply(200, invalid)]);
    assert_eq!(
        ClerkDeviceClient::with_transport(config(), transport)
            .err()
            .unwrap(),
        AuthError::UntrustedEndpoint
    );
    assert_eq!(recorded.lock().unwrap().len(), 1);
}

#[test]
fn wrong_client_grants_and_wrong_scope_grants_cannot_start_account_verification() {
    let other_config = DeviceAuthConfig::new(
        "https://clerk.example.com",
        "other-client",
        "https://accounts.example.com/device",
        &["openid", "offline_access", "ai:session-status"],
    )
    .unwrap();
    let (transport, recorded) = transport(vec![reply(200, account_metadata())]);
    let other_client = ClerkDeviceClient::with_transport(other_config, transport).unwrap();
    assert_eq!(
        other_client.verify_account(grant()).unwrap_err(),
        AuthError::UntrustedEndpoint
    );
    assert_eq!(recorded.lock().unwrap().len(), 1);

    let (client, recorded) = client(vec![reply(200, account_metadata())]);
    let mut wrong_scope = grant();
    wrong_scope.scopes.retain(|scope| scope != "openid");
    assert_eq!(
        client.verify_account(wrong_scope).unwrap_err(),
        AuthError::InvalidScope
    );
    assert_eq!(recorded.lock().unwrap().len(), 1);
}

#[test]
fn malformed_or_credential_bearing_identity_is_not_accepted_or_logged() {
    for (field, value) in [
        ("sub", json!("")),
        ("sub", json!("PRIVATE-ACCESS-TOKEN")),
        ("email", json!("private\nemail@example.com")),
        ("email", Value::Null),
        ("name", json!("Example\u{202e}Person")),
    ] {
        let mut identity = account_json("user_one");
        identity[field] = value;
        let (client, _) = client(vec![reply(200, account_metadata()), reply(200, identity)]);
        let error = client.verify_account(grant()).unwrap_err();
        assert!(matches!(error, AuthError::InvalidResponse(_)));
        assert!(!format!("{error:?}").contains("PRIVATE-ACCESS-TOKEN"));
    }
}

#[test]
fn account_errors_and_redirects_never_count_as_verification() {
    for status in [302, 401, 403, 503] {
        let (client, recorded) = client(vec![
            reply(200, account_metadata()),
            reply(status, json!({"error": "PRIVATE-DETAILS"})),
        ]);
        assert_eq!(
            client.verify_account(grant()).unwrap_err(),
            AuthError::HttpStatus(status)
        );
        assert_eq!(recorded.lock().unwrap().len(), 2);
    }
}

#[cfg(unix)]
#[test]
fn stored_sessions_require_online_reverification_and_same_subject() {
    use crate::auth::{AccountError, AccountService, CredentialStore, FileCredentialStore};
    let directory = tempfile::tempdir().unwrap();
    let store = FileCredentialStore::new(directory.path().join("private-auth"));
    let account = AccountService::new(config(), store.clone());
    let ticket = account.begin_login().unwrap();
    account.commit_login(&ticket, verified("user_one")).unwrap();
    let (client, _) = client(vec![
        reply(200, account_metadata()),
        reply(200, account_json("user_one")),
        reply(200, account_json("user_other")),
    ]);
    assert_eq!(
        account
            .restore(&client)
            .unwrap()
            .unwrap()
            .account()
            .subject(),
        "user_one"
    );
    assert!(matches!(
        account.restore(&client),
        Err(AccountError::AccountMismatch)
    ));
    assert_eq!(
        store
            .load()
            .unwrap()
            .credentials()
            .unwrap()
            .account()
            .subject(),
        "user_one"
    );
}

#[test]
fn explicitly_pinned_userinfo_is_used_without_guessing_or_extra_discovery() {
    let pinned = config()
        .with_userinfo_endpoint("https://clerk.example.com/configured-account-info")
        .unwrap();
    let (transport, recorded) = transport(vec![
        reply(200, metadata()),
        reply(200, account_json("user_one")),
    ]);
    let mut pinned_grant = grant();
    pinned_grant.provider = Box::new(ProviderIdentity::from(&pinned));
    let client = ClerkDeviceClient::with_transport(pinned, transport).unwrap();
    client.verify_account(pinned_grant).unwrap();
    let recorded = recorded.lock().unwrap();
    assert_eq!(recorded.len(), 2);
    assert_eq!(recorded[1].endpoint.path(), "/configured-account-info");
    assert_eq!(recorded[1].bearer.as_deref(), Some("PRIVATE-ACCESS-TOKEN"));
}

#[test]
fn userinfo_pin_and_metadata_conflicts_are_errors_not_overrides() {
    for endpoint in [
        "http://clerk.example.com/userinfo",
        "https://other.example.com/userinfo",
        "https://clerk.example.com/userinfo?token=unexpected",
    ] {
        assert!(config().with_userinfo_endpoint(endpoint).is_err());
    }
    let pinned = config()
        .with_userinfo_endpoint("https://clerk.example.com/configured-account-info")
        .unwrap();
    let (transport, recorded) = transport(vec![reply(200, account_metadata())]);
    assert_eq!(
        ClerkDeviceClient::with_transport(pinned, transport)
            .err()
            .unwrap(),
        AuthError::UntrustedEndpoint
    );
    assert_eq!(recorded.lock().unwrap().len(), 1);
}

#[test]
fn a_minimal_userinfo_subject_does_not_require_unrequested_profile_fields() {
    let (client, _) = client(vec![
        reply(200, account_metadata()),
        reply(200, json!({"sub": "user_minimal"})),
    ]);
    let verified = client.verify_account(grant()).unwrap();
    assert_eq!(verified.account().subject(), "user_minimal");
    assert!(verified.account().email().is_none());
    assert!(!verified.account().email_verified());
    assert!(verified.account().name().is_none());
}

#[cfg(unix)]
#[test]
fn unavailable_online_identity_verification_never_authenticates_a_cached_account() {
    use crate::auth::{AccountError, AccountService, CredentialStore, FileCredentialStore};
    for status in [401, 503] {
        let directory = tempfile::tempdir().unwrap();
        let store = FileCredentialStore::new(directory.path().join("private-auth"));
        let account = AccountService::new(config(), store.clone());
        let ticket = account.begin_login().unwrap();
        account.commit_login(&ticket, verified("user_one")).unwrap();
        let (client, _) = client(vec![
            reply(200, account_metadata()),
            reply(status, json!({"error":"PRIVATE-ERROR"})),
        ]);
        assert!(
            matches!(account.restore(&client), Err(AccountError::Provider(AuthError::HttpStatus(actual))) if actual == status)
        );
        assert!(
            store.load().unwrap().credentials().is_some(),
            "do not destroy the previous record on a transient provider failure"
        );
    }
}

#[test]
fn cross_scope_polling_cannot_infer_scopes_from_another_clients_configuration() {
    let minimal = DeviceAuthConfig::new(
        "https://clerk.example.com",
        "public-client",
        "https://accounts.example.com/device",
        &["openid"],
    )
    .unwrap();
    let authorization = serde_json::from_value::<DeviceResponse>(device_json())
        .unwrap()
        .validate(&minimal)
        .unwrap();
    let mut service = AuthService::default();
    let attempt = service.start().unwrap();
    service.complete_start(attempt, Ok(authorization), Duration::ZERO);
    let request = service.next_poll(Duration::from_secs(5)).unwrap();
    let mut missing_scopes = grant_json();
    missing_scopes.as_object_mut().unwrap().remove("scope");
    let (client, recorded) = client(vec![
        reply(200, metadata()),
        reply(200, missing_scopes.clone()),
    ]);
    assert!(matches!(
        client.poll(&request),
        PollOutcome::Rejected(AuthError::UntrustedEndpoint)
    ));
    assert_eq!(
        recorded.lock().unwrap().len(),
        1,
        "do not transmit a foreign device code"
    );

    let (transport, _) = transport(vec![reply(200, metadata()), reply(200, missing_scopes)]);
    let original = ClerkDeviceClient::with_transport(minimal, transport).unwrap();
    let PollOutcome::Granted(grant) = original.poll(&request) else {
        panic!("expected original scopes");
    };
    assert_eq!(grant.scopes(), &["openid"]);
}

#[test]
fn differently_pinned_clients_reject_grants_before_bearer_transmission() {
    let original = config()
        .with_userinfo_endpoint("https://clerk.example.com/original-info")
        .unwrap();
    let other = config()
        .with_userinfo_endpoint("https://clerk.example.com/other-info")
        .unwrap();
    let mut original_grant = grant();
    original_grant.provider = Box::new(ProviderIdentity::from(&original));
    let (transport, recorded) = transport(vec![reply(200, metadata())]);
    let client = ClerkDeviceClient::with_transport(other, transport).unwrap();
    assert_eq!(
        client.verify_account(original_grant).unwrap_err(),
        AuthError::UntrustedEndpoint
    );
    assert_eq!(recorded.lock().unwrap().len(), 1);
    assert!(recorded
        .lock()
        .unwrap()
        .iter()
        .all(|request| request.bearer.is_none()));
}

#[cfg(unix)]
#[test]
fn restoring_with_a_differently_pinned_client_never_transmits_the_stored_bearer() {
    use crate::auth::{AccountError, AccountService, CredentialStore, FileCredentialStore};
    let directory = tempfile::tempdir().unwrap();
    let store = FileCredentialStore::new(directory.path().join("private-auth"));
    let original = config()
        .with_userinfo_endpoint("https://clerk.example.com/original-info")
        .unwrap();
    let account = AccountService::new(original.clone(), store.clone());
    let ticket = account.begin_login().unwrap();
    let mut proof = verified("user_one");
    proof.grant.provider = Box::new(ProviderIdentity::from(&original));
    account.commit_login(&ticket, proof).unwrap();
    let revision = store.load().unwrap().revision();
    let (transport, recorded) = transport(vec![reply(200, metadata())]);
    let other = config()
        .with_userinfo_endpoint("https://clerk.example.com/other-info")
        .unwrap();
    let client = ClerkDeviceClient::with_transport(other, transport).unwrap();
    assert!(matches!(
        account.restore(&client),
        Err(AccountError::Provider(AuthError::UntrustedEndpoint))
    ));
    assert_eq!(recorded.lock().unwrap().len(), 1);
    assert_eq!(store.load().unwrap().revision(), revision);
}
