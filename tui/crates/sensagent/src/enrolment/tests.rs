use std::os::unix::fs::PermissionsExt;
use std::time::SystemTime;

use super::*;
use crate::auth::{AccountService, FileCredentialStore};
use crate::cloud::CloudAIConfig;

const MACHINE: &str = "01M6161MQTFHYKYSMGSPK6VW17";
const SECRET: &str = "rkm_3q2-7wF6m0ZxQh9b1y4Ku8LpN5sTvR2cJdE0aBfGhIk";

fn account() -> AccountIdentity {
    AccountIdentity {
        subject: "user_123".into(),
        email: Some("someone@example.com".into()),
        email_verified: true,
        name: None,
    }
}

fn credential() -> MachineCredential {
    MachineCredential::new(MACHINE, SECRET, &account()).unwrap()
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}

fn token_answer(token: &str, expires_at: u64) -> String {
    format!(r#"{{"version":1,"access_token":"{token}","expires_at":{expires_at}}}"#)
}

fn tokens(replies: Vec<(u16, &str)>) -> (MachineTokens, crate::cloud::Asked) {
    let config = CloudAIConfig::new("https://reviewed.example/")
        .unwrap()
        .with_machine_id(MACHINE)
        .unwrap();
    let (cloud, asked) = CloudAIClient::scripted(config, replies);
    (
        MachineTokens::new(cloud, crate::auth::tests::config(), credential()),
        asked,
    )
}

const ACCESS: &str = "rkma.v1.eyJzdWIiOiJ1c2VyXzEyMyJ9.c2lnbmF0dXJlLXNpZ25hdHVyZQ";

#[test]
fn the_credential_file_is_read_only_when_private_to_this_user() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(CREDENTIAL_FILE);
    assert_eq!(MachineCredential::load(&path), Ok(None));

    fs::write(&path, credential().to_json()).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    let loaded = MachineCredential::load(&path).unwrap().unwrap();
    assert_eq!(loaded.machine_id(), MACHINE);
    assert_eq!(loaded.secret.expose_secret(), SECRET);
    assert_eq!(loaded.account.subject(), "user_123");
    assert!(!format!("{loaded:?}").contains(SECRET));

    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
    assert_eq!(
        MachineCredential::load(&path).unwrap_err(),
        CredentialError::Unsafe
    );

    let linked = dir.path().join("linked.json");
    std::os::unix::fs::symlink(&path, &linked).unwrap();
    assert_eq!(
        MachineCredential::load(&linked).unwrap_err(),
        CredentialError::Unsafe
    );

    for text in [
        "not json".to_owned(),
        credential().to_json().replace(SECRET, "rkm_short"),
        credential().to_json().replace(MACHINE, "sprite-1"),
        credential()
            .to_json()
            .replace("\"version\":1", "\"version\":2"),
        credential()
            .to_json()
            .replace("\"version\":1", "\"version\":1,\"more\":true"),
    ] {
        fs::write(&path, text).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(
            MachineCredential::load(&path).unwrap_err(),
            CredentialError::Invalid
        );
    }
}

impl PartialEq for MachineCredential {
    fn eq(&self, other: &Self) -> bool {
        self.machine_id == other.machine_id
    }
}

#[test]
fn the_secret_buys_an_access_token_kept_like_a_sign_ins() {
    let answer = token_answer(ACCESS, now_ms() + 15 * 60 * 1000);
    let (tokens, asked) = tokens(vec![(200, &answer)]);
    let grant = tokens.exchange().unwrap();
    assert_eq!(grant.access_token().expose_secret(), ACCESS);
    assert!((14 * 60..=15 * 60).contains(&grant.expires_in().as_secs()));
    assert_eq!(grant.scopes(), crate::auth::tests::config().scopes());

    let asked = asked.lock().unwrap();
    let (method, url, headers) = &asked[0];
    assert_eq!(method, &reqwest::Method::POST);
    assert_eq!(url.path(), "/api/v1/machines/token");
    assert_eq!(
        headers["authorization"],
        format!("Bearer {SECRET}").as_str()
    );
    assert_eq!(headers["x-rakumux-machine-id"], MACHINE);
}

#[test]
fn a_refused_secret_or_a_broken_answer_is_no_token() {
    let later = now_ms() + 15 * 60 * 1000;
    let realtime = token_answer(
        "v1.eyJzdWIiOiJ1c2VyXzEyMyJ9.c2lnbmF0dXJlLXNpZ25hdHVyZQ",
        later,
    );
    let expired = token_answer(ACCESS, now_ms() - 1000);
    let unavailable = r#"{"error":{"code":"unavailable","message":"x"}}"#;
    let revoked = r#"{"error":{"code":"unauthenticated","message":"x"}}"#;
    for (status, body, expected) in [
        (401, revoked, AuthError::InvalidGrant),
        (503, unavailable, AuthError::HttpStatus(503)),
        (
            200,
            realtime.as_str(),
            AuthError::InvalidResponse("invalid machine token answer"),
        ),
        (
            200,
            expired.as_str(),
            AuthError::InvalidResponse("expired machine token"),
        ),
    ] {
        let (tokens, _) = tokens(vec![(status, body)]);
        assert_eq!(tokens.exchange().unwrap_err(), expected, "{body}");
    }
    // A day-long token is taken to last an hour.
    let long = token_answer(ACCESS, now_ms() + 24 * 60 * 60 * 1000);
    let (tokens, _) = tokens(vec![(200, &long)]);
    assert_eq!(tokens.exchange().unwrap().expires_in(), MAX_LIFETIME);
}

#[test]
fn the_account_service_signs_in_and_renews_with_the_secret_alone() {
    let dir = tempfile::tempdir().unwrap();
    let first = token_answer(ACCESS, now_ms() + 15 * 60 * 1000);
    let renewed_token = "rkma.v1.eyJzdWIiOiJ1c2VyXzEyMyIsIm4iOjJ9.c2Vjb25kLXNpZ25hdHVyZQ";
    let second = token_answer(renewed_token, now_ms() + 15 * 60 * 1000);
    let (tokens, asked) = tokens(vec![(200, &first), (200, &second)]);
    let service = AccountService::new(
        crate::auth::tests::config(),
        FileCredentialStore::new(dir.path().join("credentials")),
    );

    let ticket = service.begin_login().unwrap();
    let verified = tokens.verify_account(tokens.exchange().unwrap()).unwrap();
    let session = service.commit_login(&ticket, verified).unwrap();
    assert_eq!(session.account().subject(), "user_123");

    let renewed = service.renew(&session, &tokens).unwrap().unwrap();
    assert_ne!(renewed.revision(), session.revision());
    let lease = service.lease_access_token(&renewed).unwrap().unwrap();
    let used = service
        .consume_access_token(lease, |token| token.expose_secret().to_owned())
        .unwrap();
    assert_eq!(used.as_deref(), Some(renewed_token));
    // Both exchanges sent the secret, never a Clerk refresh.
    assert!(asked
        .lock()
        .unwrap()
        .iter()
        .all(|(_, url, _)| url.path() == "/api/v1/machines/token"));
}

#[test]
fn enrolling_names_the_machine_and_wakes_it_only_at_a_sprites_url() {
    let body = enrol_body(
        MACHINE,
        "my-first-sprite",
        Some("https://my-first-sprite-b5jcz.sprites.app"),
    )
    .unwrap();
    assert_eq!(
        String::from_utf8(body).unwrap(),
        r#"{"version":1,"machine_id":"01M6161MQTFHYKYSMGSPK6VW17","name":"my-first-sprite","wake_url":"https://my-first-sprite-b5jcz.sprites.app"}"#
    );
    assert!(enrol_body(MACHINE, "studio", None).is_some());
    for url in [
        "https://169.254.169.254",
        "http://s.sprites.app",
        "https://s.sprites.app/admin",
        "https://evil.com/.sprites.app",
        "https://-s.sprites.app",
        "https://S.sprites.app",
    ] {
        assert!(enrol_body(MACHINE, "s", Some(url)).is_none(), "{url}");
    }
    assert!(enrol_body("sprite-1", "s", None).is_none());
    assert!(enrol_body(MACHINE, "two\nlines", None).is_none());
    assert!(enrol_body(MACHINE, " ", None).is_none());
}

fn reply(status: u16, body: &str) -> FeedHttpReply {
    FeedHttpReply {
        status,
        retry_after: None,
        body: body.as_bytes().to_vec(),
    }
}

#[test]
fn an_enrol_answer_gives_the_secret_and_a_revoke_is_done_when_none_is_left() {
    let answer = format!(
        r#"{{"version":1,"machine_id":"{MACHINE}","secret":"{SECRET}","created_at":1761700000000}}"#
    );
    assert_eq!(enrolled(&reply(201, &answer)).unwrap(), SECRET);
    assert_eq!(
        enrolled(&reply(200, &answer.replace(SECRET, "rkm_short"))),
        Err(FeedFailure::Invalid)
    );
    assert_eq!(
        enrolled(&reply(
            401,
            r#"{"error":{"code":"unauthenticated","message":"x"}}"#
        )),
        Err(FeedFailure::Unauthorized)
    );
    assert_eq!(revoked(&reply(204, "")), Ok(()));
    assert_eq!(
        revoked(&reply(
            404,
            r#"{"error":{"code":"invalid_request","message":"x"}}"#
        )),
        Ok(())
    );
    assert_eq!(
        revoked(&reply(
            503,
            r#"{"error":{"code":"unavailable","message":"x"}}"#
        )),
        Err(FeedFailure::Unavailable(None))
    );
}
