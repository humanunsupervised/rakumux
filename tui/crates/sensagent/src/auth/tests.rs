use super::model::{DeviceResponse, TokenReceipt, TokenResponse};
use super::*;
use serde_json::{json, Value};
use std::time::Duration;

pub(crate) fn clock_origin() -> std::time::Instant {
    static ORIGIN: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    *ORIGIN.get_or_init(std::time::Instant::now)
}

pub(crate) fn config() -> DeviceAuthConfig {
    DeviceAuthConfig::new(
        "https://clerk.example.com",
        "public-client",
        "https://accounts.example.com/device",
        &["openid", "offline_access", "ai:session-status"],
    )
    .unwrap()
}

pub(super) fn device_json() -> Value {
    json!({
        "device_code": "PRIVATE-DEVICE-CODE",
        "user_code": "BCDF-GHJK",
        "verification_uri": "https://accounts.example.com/device",
        "verification_uri_complete": "https://accounts.example.com/device?user_code=BCDF-GHJK",
        "expires_in": 600,
        "interval": 5
    })
}

pub(super) fn grant_json() -> Value {
    json!({
        "access_token": "PRIVATE-ACCESS-TOKEN",
        "refresh_token": "PRIVATE-REFRESH-TOKEN",
        "id_token": "NOT-AN-ACCESS-TOKEN",
        "token_type": "Bearer",
        "expires_in": 3600,
        "scope": "openid offline_access ai:session-status"
    })
}

fn device(value: Value) -> DeviceAuthorization {
    serde_json::from_value::<DeviceResponse>(value)
        .unwrap()
        .validate(&config())
        .unwrap()
}

pub(super) fn grant() -> AuthorizationGrant {
    serde_json::from_value::<TokenResponse>(grant_json())
        .unwrap()
        .validate(&config(), TokenReceipt::now())
        .unwrap()
}

pub(crate) fn verified(subject: &str) -> VerifiedAuthorization {
    let mut grant = grant();
    grant.receipt = Some(super::model::TokenReceipt {
        wall: std::time::UNIX_EPOCH + Duration::from_secs(1000),
        monotonic: clock_origin(),
    });
    VerifiedAuthorization {
        identity: AccountIdentity {
            subject: subject.into(),
            email: Some("person@example.com".into()),
            email_verified: true,
            name: Some("Example Person".into()),
        }
        .validate()
        .unwrap(),
        grant,
    }
}

pub(super) fn verified_now(subject: &str) -> VerifiedAuthorization {
    let mut proof = verified(subject);
    proof.grant.receipt = Some(super::model::TokenReceipt {
        wall: std::time::SystemTime::now(),
        monotonic: std::time::Instant::now(),
    });
    proof
}

fn pending() -> AuthService {
    let mut service = AuthService::default();
    let attempt = service.start().unwrap();
    assert_eq!(
        service.complete_start(attempt, Ok(device(device_json())), seconds(0)),
        ApplyResult::Applied
    );
    service
}

fn seconds(value: u64) -> Duration {
    Duration::from_secs(value)
}

fn next_deadline(service: &mut AuthService, now: u64) -> u64 {
    match service.status(seconds(now)) {
        AuthStatus::AwaitingApproval { next_poll_at, .. } => next_poll_at.as_secs(),
        other => panic!("expected pending status, got {other:?}"),
    }
}

#[test]
fn public_configuration_rejects_unsafe_urls_and_scopes() {
    for issuer in [
        "http://clerk.example.com",
        "https://user:password@clerk.example.com",
        "https://clerk.example.com?secret=x",
        "https://clerk.example.com/#x",
        "https://clerk.example.com/nested",
    ] {
        assert!(DeviceAuthConfig::new(
            issuer,
            "client",
            "https://accounts.example.com/device",
            &["openid"]
        )
        .is_err());
    }
    for scopes in [
        vec![],
        vec!["openid", "openid"],
        vec!["two scopes"],
        vec!["bad\nscope"],
    ] {
        assert!(DeviceAuthConfig::new(
            "https://clerk.example.com",
            "client",
            "https://accounts.example.com/device",
            &scopes
        )
        .is_err());
    }
    assert!(DeviceAuthConfig::new(
        "https://clerk.example.com",
        "",
        "https://accounts.example.com/device",
        &["openid"]
    )
    .is_err());
    assert!(DeviceAuthConfig::new(
        "https://clerk.example.com",
        "client",
        "http://accounts.example.com/device",
        &["openid"]
    )
    .is_err());
}

#[test]
fn the_ai_status_scope_defaults_and_must_be_requested() {
    assert_eq!(config().status_scope(), AI_STATUS_SCOPE);
    let profile = DeviceAuthConfig::new(
        "https://clerk.example.com",
        "public-client",
        "https://accounts.example.com/device",
        &["openid", "profile", "offline_access"],
    )
    .unwrap();
    assert!(profile.clone().with_status_scope("email").is_err());
    assert_eq!(
        profile.with_status_scope("profile").unwrap().status_scope(),
        "profile"
    );
}

#[test]
fn device_response_preserves_provider_timings_and_safe_prompt() {
    let mut value = device_json();
    value["interval"] = json!(13);
    value["expires_in"] = json!(83);
    let authorization = device(value);
    assert_eq!(authorization.interval(), seconds(13));
    assert_eq!(authorization.expires_in(), seconds(83));
    assert_eq!(authorization.prompt().user_code(), "BCDF-GHJK");
    assert!(authorization.prompt().verification_uri_complete().is_some());
    let output = format!("{authorization:?}");
    assert!(!output.contains("PRIVATE-DEVICE-CODE"));
    assert!(output.contains("[REDACTED]"));
}

#[test]
fn missing_interval_uses_only_the_rfc_default() {
    let mut value = device_json();
    value.as_object_mut().unwrap().remove("interval");
    assert_eq!(device(value).interval(), seconds(5));
}

#[test]
fn invalid_device_payloads_fail_without_credential_leakage() {
    for (field, invalid) in [
        ("expires_in", json!(0)),
        ("interval", json!(0)),
        ("device_code", json!("")),
        ("device_code", json!("credential\nheader")),
        ("user_code", json!("fake\u{1b}[31m")),
        ("user_code", json!("")),
        ("verification_uri", json!("https://evil.example/device")),
        (
            "verification_uri_complete",
            json!("https://accounts.example.com/device?user_code=OTHER"),
        ),
        (
            "verification_uri_complete",
            json!("https://accounts.example.com/device?user_code=BCDF-GHJK&token=secret"),
        ),
        (
            "verification_uri_complete",
            json!("https://accounts.example.com/device?user_code=BCDF-GHJK&user_code=BCDF-GHJK"),
        ),
        (
            "verification_uri_complete",
            json!("https://accounts.example.com/device?user_code=BCDF-GHJK#fragment"),
        ),
        (
            "verification_uri_complete",
            json!("https://accounts.example.com/other?user_code=BCDF-GHJK"),
        ),
    ] {
        let mut value = device_json();
        value[field] = invalid;
        let result = serde_json::from_value::<DeviceResponse>(value)
            .map_err(|_| AuthError::InvalidResponse("malformed OAuth JSON"))
            .and_then(|response| response.validate(&config()));
        assert!(result.is_err(), "field {field} must fail");
        assert!(!format!("{result:?}").contains("PRIVATE-DEVICE-CODE"));
    }
}

#[test]
fn tokens_are_redacted_and_id_tokens_are_not_api_credentials() {
    let grant = grant();
    assert_eq!(grant.access_token().expose_secret(), "PRIVATE-ACCESS-TOKEN");
    assert_eq!(
        grant.refresh_token().unwrap().expose_secret(),
        "PRIVATE-REFRESH-TOKEN"
    );
    assert_eq!(grant.expires_in(), seconds(3600));
    let output = format!("{grant:?}");
    for secret in [
        "PRIVATE-ACCESS-TOKEN",
        "PRIVATE-REFRESH-TOKEN",
        "NOT-AN-ACCESS-TOKEN",
    ] {
        assert!(!output.contains(secret));
    }
    let id_only = json!({"id_token": "ID-ONLY", "token_type": "Bearer", "expires_in": 10});
    assert!(serde_json::from_value::<TokenResponse>(id_only).is_err());
}

#[test]
fn a_provider_cannot_expose_a_private_device_code_as_the_public_code() {
    let mut value = device_json();
    value["user_code"] = value["device_code"].clone();
    value
        .as_object_mut()
        .unwrap()
        .remove("verification_uri_complete");
    let response: DeviceResponse = serde_json::from_value(value).unwrap();
    assert!(response.validate(&config()).is_err());
}

#[test]
fn unexpected_token_scopes_cannot_escalate_access_or_leak_credentials_in_debug() {
    let mut value = grant_json();
    value["scope"] = json!("openid offline_access ai:session-status PRIVATE-ACCESS-TOKEN");
    let response: TokenResponse = serde_json::from_value(value).unwrap();
    let error = response
        .validate(&config(), TokenReceipt::now())
        .unwrap_err();
    assert_eq!(
        error,
        AuthError::InvalidResponse("unexpected OAuth scope was granted")
    );
    assert!(!format!("{error:?}").contains("PRIVATE-ACCESS-TOKEN"));
}

#[test]
fn tokens_require_bearer_expiry_refresh_and_requested_scopes() {
    for (field, value) in [
        ("token_type", json!("Basic")),
        ("expires_in", json!(0)),
        ("refresh_token", Value::Null),
        ("scope", json!("openid offline_access")),
    ] {
        let mut response = grant_json();
        response[field] = value;
        let result = serde_json::from_value::<TokenResponse>(response)
            .unwrap()
            .validate(&config(), TokenReceipt::now());
        assert!(result.is_err(), "field {field} must fail");
    }
}

#[test]
fn first_poll_waits_and_only_one_request_can_be_in_flight() {
    let mut service = pending();
    assert!(service.next_poll(seconds(4)).is_none());
    let request = service.next_poll(seconds(5)).unwrap();
    assert!(service.next_poll(seconds(100)).is_none());
    assert!(!format!("{request:?}").contains("PRIVATE-DEVICE-CODE"));
    assert!(matches!(
        service.complete_poll(request.id(), PollOutcome::Pending, seconds(101)),
        PollCompletion::Applied
    ));
    assert_eq!(next_deadline(&mut service, 101), 106);
}

#[test]
fn slow_down_permanently_increases_the_provider_interval() {
    let mut service = pending();
    let first = service.next_poll(seconds(5)).unwrap();
    service.complete_poll(first.id(), PollOutcome::SlowDown, seconds(5));
    assert_eq!(next_deadline(&mut service, 5), 15);
    assert!(service.next_poll(seconds(14)).is_none());
    let second = service.next_poll(seconds(15)).unwrap();
    service.complete_poll(second.id(), PollOutcome::Pending, seconds(15));
    assert_eq!(next_deadline(&mut service, 15), 25);
    let third = service.next_poll(seconds(25)).unwrap();
    service.complete_poll(third.id(), PollOutcome::SlowDown, seconds(25));
    assert_eq!(next_deadline(&mut service, 25), 40);
}

#[test]
fn transient_errors_are_visible_and_back_off_with_retry_after() {
    let mut service = pending();
    let first = service.next_poll(seconds(5)).unwrap();
    service.complete_poll(
        first.id(),
        PollOutcome::Retryable {
            error: AuthError::Network(NetworkFailure::Timeout),
            retry_after: None,
        },
        seconds(5),
    );
    assert_eq!(next_deadline(&mut service, 5), 10);
    assert!(matches!(
        service.status(seconds(5)),
        AuthStatus::AwaitingApproval {
            last_error: Some(AuthError::Network(NetworkFailure::Timeout)),
            ..
        }
    ));
    let second = service.next_poll(seconds(10)).unwrap();
    service.complete_poll(
        second.id(),
        PollOutcome::Retryable {
            error: AuthError::HttpStatus(429),
            retry_after: Some(seconds(40)),
        },
        seconds(10),
    );
    assert_eq!(next_deadline(&mut service, 10), 50);
    let third = service.next_poll(seconds(50)).unwrap();
    service.complete_poll(third.id(), PollOutcome::Pending, seconds(50));
    assert!(matches!(
        service.status(seconds(50)),
        AuthStatus::AwaitingApproval {
            last_error: None,
            ..
        }
    ));
    assert_eq!(next_deadline(&mut service, 50), 55);
}

#[test]
fn cancellation_discards_late_challenge_and_credentials() {
    let mut service = AuthService::default();
    let attempt = service.start().unwrap();
    service.cancel();
    assert_eq!(
        service.complete_start(attempt, Ok(device(device_json())), seconds(0)),
        ApplyResult::Ignored
    );
    assert_eq!(service.status(seconds(0)), AuthStatus::Cancelled);
    let attempt = service.start().unwrap();
    service.complete_start(attempt, Ok(device(device_json())), seconds(0));
    let request = service.next_poll(seconds(5)).unwrap();
    service.cancel();
    assert!(matches!(
        service.complete_poll(request.id(), PollOutcome::Granted(grant()), seconds(5)),
        PollCompletion::DiscardedGrant(_)
    ));
    assert_eq!(service.status(seconds(5)), AuthStatus::Cancelled);
}

#[test]
fn replaced_attempt_cannot_be_overwritten_by_an_old_poll() {
    let mut service = pending();
    let old = service.next_poll(seconds(5)).unwrap();
    let current = service.start().unwrap();
    service.complete_start(current, Ok(device(device_json())), seconds(6));
    assert!(matches!(
        service.complete_poll(old.id(), PollOutcome::Granted(grant()), seconds(7)),
        PollCompletion::DiscardedGrant(_)
    ));
    assert!(
        matches!(service.status(seconds(7)), AuthStatus::AwaitingApproval { attempt, .. } if attempt == current)
    );
}

#[test]
fn duplicate_poll_result_cannot_complete_the_next_request() {
    let mut service = pending();
    let first = service.next_poll(seconds(5)).unwrap();
    service.complete_poll(first.id(), PollOutcome::Pending, seconds(5));
    let second = service.next_poll(seconds(10)).unwrap();
    assert!(matches!(
        service.complete_poll(first.id(), PollOutcome::Granted(grant()), seconds(10)),
        PollCompletion::DiscardedGrant(_)
    ));
    assert!(matches!(
        service.complete_poll(second.id(), PollOutcome::Granted(grant()), seconds(10)),
        PollCompletion::Authorized(_)
    ));
}

#[test]
fn elapsed_or_provider_expiry_stops_polling_and_discards_late_grants() {
    let mut service = pending();
    let request = service.next_poll(seconds(5)).unwrap();
    assert_eq!(service.status(seconds(600)), AuthStatus::Expired);
    assert!(service.next_poll(seconds(600)).is_none());
    assert!(matches!(
        service.complete_poll(request.id(), PollOutcome::Granted(grant()), seconds(600)),
        PollCompletion::DiscardedGrant(_)
    ));
    let mut service = pending();
    let request = service.next_poll(seconds(5)).unwrap();
    service.complete_poll(request.id(), PollOutcome::Expired, seconds(5));
    assert_eq!(service.status(seconds(5)), AuthStatus::Expired);
}

#[test]
fn deny_and_fatal_errors_stop_without_retry() {
    for (outcome, expected) in [
        (PollOutcome::Denied, AuthStatus::Denied),
        (
            PollOutcome::Rejected(AuthError::ProviderRejected),
            AuthStatus::Failed(AuthError::ProviderRejected),
        ),
    ] {
        let mut service = pending();
        let request = service.next_poll(seconds(5)).unwrap();
        service.complete_poll(request.id(), outcome, seconds(5));
        assert_eq!(service.status(seconds(5)), expected);
        assert!(service.next_poll(seconds(100)).is_none());
    }
}

#[test]
fn provider_grant_does_not_claim_application_sign_in_or_store_credentials() {
    let mut service = pending();
    let request = service.next_poll(seconds(5)).unwrap();
    assert!(matches!(
        service.complete_poll(request.id(), PollOutcome::Granted(grant()), seconds(5)),
        PollCompletion::Authorized(_)
    ));
    assert!(matches!(
        service.status(seconds(5)),
        AuthStatus::ProviderAuthorized(_)
    ));
    assert!(service.next_poll(seconds(50)).is_none());
    assert!(!format!("{:?}", service.status(seconds(5))).contains("PRIVATE-ACCESS-TOKEN"));
}

#[test]
fn unrepresentable_provider_deadline_fails_visibly() {
    let mut service = AuthService::default();
    let attempt = service.start().unwrap();
    service.complete_start(attempt, Ok(device(device_json())), Duration::MAX);
    assert!(matches!(
        service.status(Duration::MAX),
        AuthStatus::Failed(AuthError::InvalidResponse(_))
    ));
}
