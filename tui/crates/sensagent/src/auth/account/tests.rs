use super::*;
use crate::auth::tests::{clock_origin, config, verified};
use crate::auth::FileCredentialStore;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Arc,
};
use std::time::{Duration, UNIX_EPOCH};

mod access;

#[derive(Clone)]
struct Clock(Arc<AtomicU64>, Arc<AtomicU64>, Instant);

impl WallClock for Clock {
    fn now(&self) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(self.0.load(Ordering::SeqCst))
    }

    fn monotonic_now(&self) -> Instant {
        self.2 + Duration::from_secs(self.1.load(Ordering::SeqCst))
    }
}

fn setup() -> (
    tempfile::TempDir,
    AccountService<FileCredentialStore, Clock>,
    FileCredentialStore,
    Clock,
) {
    let directory = tempfile::tempdir().unwrap();
    let store = FileCredentialStore::new(directory.path().join("private-auth"));
    let clock = Clock(
        Arc::new(AtomicU64::new(1000)),
        Arc::new(AtomicU64::new(0)),
        clock_origin(),
    );
    let service = AccountService::with_clock(config(), store.clone(), clock.clone());
    (directory, service, store, clock)
}

#[test]
fn verified_account_is_ready_only_after_private_storage_succeeds() {
    let (_directory, service, store, _clock) = setup();
    let ticket = service.begin_login().unwrap();
    let session = service.commit_login(&ticket, verified("user_one")).unwrap();
    assert_eq!(session.account().subject(), "user_one");
    assert_eq!(session.expires_at(), 4600);
    assert!(service.is_current(&session).unwrap());
    let snapshot = store.load().unwrap();
    assert_eq!(snapshot.revision(), session.revision());
    let debug = format!("{snapshot:?} {session:?}");
    for private in [
        "PRIVATE-ACCESS-TOKEN",
        "PRIVATE-REFRESH-TOKEN",
        "person@example.com",
    ] {
        assert!(!debug.contains(private));
    }
}

#[test]
fn storage_failure_cannot_return_a_successful_account_session() {
    let (directory, service, store, _clock) = setup();
    let ticket = service.begin_login().unwrap();
    let before = fs::read(directory.path().join("private-auth/credentials.json")).unwrap();
    fs::set_permissions(
        directory.path().join("private-auth"),
        fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    assert!(matches!(
        service.commit_login(&ticket, verified("user_one")),
        Err(AccountError::Store(CredentialStoreError::UnsafePath))
    ));
    assert_eq!(
        fs::read(directory.path().join("private-auth/credentials.json")).unwrap(),
        before
    );
    fs::set_permissions(
        directory.path().join("private-auth"),
        fs::Permissions::from_mode(0o700),
    )
    .unwrap();
    assert!(store.load().unwrap().credentials().is_none());
}

#[test]
fn signout_fences_an_initial_login_even_before_any_credentials_were_saved() {
    let (_directory, service, store, _clock) = setup();
    let ticket = service.begin_login().unwrap();
    service.sign_out().unwrap();
    assert!(matches!(
        service.commit_login(&ticket, verified("user_one")),
        Err(AccountError::Store(CredentialStoreError::Conflict))
    ));
    assert!(store.load().unwrap().credentials().is_none());
}

#[test]
fn cancelling_a_new_login_preserves_the_previously_stored_account() {
    let (_directory, service, store, _clock) = setup();
    let first = service.begin_login().unwrap();
    service
        .commit_login(&first, verified("user_original"))
        .unwrap();
    let replacement = service.begin_login().unwrap();
    assert_eq!(
        service.cancel_login(&replacement).unwrap(),
        ApplyResult::Applied
    );
    assert!(matches!(
        service.commit_login(&replacement, verified("user_late")),
        Err(AccountError::Store(CredentialStoreError::Conflict))
    ));
    assert_eq!(
        store
            .load()
            .unwrap()
            .credentials()
            .unwrap()
            .account()
            .subject(),
        "user_original"
    );
}

#[test]
fn a_newer_login_wins_without_old_cancel_or_callback_overwriting_it() {
    let (_directory, service, store, _clock) = setup();
    let first = service.begin_login().unwrap();
    let second = service.begin_login().unwrap();
    assert_eq!(service.cancel_login(&first).unwrap(), ApplyResult::Ignored);
    assert!(matches!(
        service.commit_login(&first, verified("user_old")),
        Err(AccountError::Store(CredentialStoreError::Conflict))
    ));
    service.commit_login(&second, verified("user_new")).unwrap();
    assert_eq!(
        store
            .load()
            .unwrap()
            .credentials()
            .unwrap()
            .account()
            .subject(),
        "user_new"
    );
}

#[test]
fn expiry_and_clock_rollback_do_not_extend_credential_lifetime() {
    let (_directory, service, store, clock) = setup();
    let ticket = service.begin_login().unwrap();
    clock.0.store(4600, Ordering::SeqCst);
    assert!(matches!(
        service.commit_login(&ticket, verified("user_one")),
        Err(AccountError::Expired)
    ));
    clock.0.store(999, Ordering::SeqCst);
    assert!(matches!(
        service.commit_login(&ticket, verified("user_one")),
        Err(AccountError::ClockChanged)
    ));
    assert!(store.load().unwrap().credentials().is_none());
}

#[test]
fn a_different_config_cannot_store_or_restore_an_unrelated_provider_grant() {
    let (_directory, _service, store, clock) = setup();
    let other = DeviceAuthConfig::new(
        "https://clerk.example.com",
        "other-client",
        "https://accounts.example.com/device",
        &["openid", "offline_access", "ai:session-status"],
    )
    .unwrap();
    let service = AccountService::with_clock(other, store, clock);
    let ticket = service.begin_login().unwrap();
    assert!(matches!(
        service.commit_login(&ticket, verified("user_one")),
        Err(AccountError::ProviderMismatch)
    ));
}

#[test]
fn restored_identity_must_match_subject_not_email() {
    let (_directory, service, store, _clock) = setup();
    let ticket = service.begin_login().unwrap();
    service
        .commit_login(&ticket, verified("user_original"))
        .unwrap();
    let snapshot = store.load().unwrap();
    assert!(matches!(
        service.commit_restore(
            snapshot.revision,
            snapshot.credentials.unwrap(),
            verified("user_other")
        ),
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
        "user_original"
    );
}

#[test]
fn restore_rechecks_revision_after_online_verification_and_profile_updates() {
    let (_directory, service, store, _clock) = setup();
    let ticket = service.begin_login().unwrap();
    let original = service.commit_login(&ticket, verified("user_one")).unwrap();
    let snapshot = store.load().unwrap();
    let mut updated = verified("user_one");
    updated.identity.name = Some("Changed Name".into());
    let restored = service
        .commit_restore(snapshot.revision, snapshot.credentials.unwrap(), updated)
        .unwrap();
    assert_eq!(restored.account().name(), Some("Changed Name"));
    assert!(!service.is_current(&original).unwrap());
    let stale = store.load().unwrap();
    service.sign_out().unwrap();
    assert!(matches!(
        service.commit_restore(
            stale.revision,
            stale.credentials.unwrap(),
            verified("user_one")
        ),
        Err(AccountError::Store(CredentialStoreError::Conflict))
    ));
    assert!(!service.is_current(&restored).unwrap());
}

#[test]
fn foreign_tickets_and_sessions_cannot_cross_account_service_owners() {
    let (first_directory, first, first_store, _clock) = setup();
    let (second_directory, second, second_store, _clock) = setup();
    let first_ticket = first.begin_login().unwrap();
    let second_ticket = second.begin_login().unwrap();
    assert_eq!(first_ticket.revision, second_ticket.revision);
    let first_path = first_directory.path().join("private-auth/credentials.json");
    let second_path = second_directory
        .path()
        .join("private-auth/credentials.json");
    let before_first = fs::read(&first_path).unwrap();
    let before_second = fs::read(&second_path).unwrap();
    for (service, foreign) in [(&first, &second_ticket), (&second, &first_ticket)] {
        assert!(matches!(
            service.commit_login(foreign, verified("user_one")),
            Err(AccountError::OwnerMismatch)
        ));
        assert!(matches!(
            service.cancel_login(foreign),
            Err(AccountError::OwnerMismatch)
        ));
    }
    assert_eq!(fs::read(&first_path).unwrap(), before_first);
    assert_eq!(fs::read(&second_path).unwrap(), before_second);
    let first_session = first
        .commit_login(&first_ticket, verified("user_one"))
        .unwrap();
    let second_session = second
        .commit_login(&second_ticket, verified("user_one"))
        .unwrap();
    assert_eq!(first_session.revision(), second_session.revision());
    assert!(!first.is_current(&second_session).unwrap());
    assert!(!second.is_current(&first_session).unwrap());
    assert!(first.is_current(&first_session).unwrap());
    assert!(second.is_current(&second_session).unwrap());
    assert_eq!(
        first_store.load().unwrap().revision(),
        first_session.revision()
    );
    assert_eq!(
        second_store.load().unwrap().revision(),
        second_session.revision()
    );
}

#[test]
fn reopening_the_same_store_does_not_transfer_ticket_or_view_ownership() {
    let (_directory, original, store, clock) = setup();
    let ticket = original.begin_login().unwrap();
    let session = original
        .commit_login(&ticket, verified("user_one"))
        .unwrap();
    let replacement = AccountService::with_clock(config(), store.clone(), clock);
    let before = store.load().unwrap().revision();
    assert!(matches!(
        replacement.cancel_login(&ticket),
        Err(AccountError::OwnerMismatch)
    ));
    assert!(!replacement.is_current(&session).unwrap());
    assert_eq!(store.load().unwrap().revision(), before);
}

#[test]
fn differently_pinned_verification_proofs_cannot_be_committed() {
    let (_directory, _service, store, clock) = setup();
    let pinned = config()
        .with_userinfo_endpoint("https://clerk.example.com/pinned-info")
        .unwrap();
    let service = AccountService::with_clock(pinned.clone(), store.clone(), clock);
    let ticket = service.begin_login().unwrap();
    let before = store.load().unwrap().revision();
    for proof_config in [
        config(),
        config()
            .with_userinfo_endpoint("https://clerk.example.com/other-info")
            .unwrap(),
    ] {
        let mut proof = verified("user_one");
        proof.grant.provider = Box::new(ProviderIdentity::from(&proof_config));
        assert!(matches!(
            service.commit_login(&ticket, proof),
            Err(AccountError::ProviderMismatch)
        ));
        assert_eq!(store.load().unwrap().revision(), before);
    }
    let mut proof = verified("user_one");
    proof.grant.provider = Box::new(ProviderIdentity::from(&pinned));
    let session = service.commit_login(&ticket, proof).unwrap();
    let saved = store.load().unwrap().credentials.unwrap();
    assert!(saved.matches_config(&pinned));
    assert!(!saved.matches_config(&config()));
    assert!(service.is_current(&session).unwrap());
}

struct DelayedStore {
    inner: FileCredentialStore,
    clock: Clock,
    expire_on_write: AtomicBool,
    replacement: Option<SavedCredentials>,
}

impl CredentialStore for DelayedStore {
    fn load(&self) -> Result<super::super::CredentialSnapshot, CredentialStoreError> {
        self.inner.load()
    }

    fn compare_and_swap(
        &self,
        expected: CredentialRevision,
        credentials: Option<&SavedCredentials>,
    ) -> Result<CredentialRevision, CredentialStoreError> {
        let revision = self.inner.compare_and_swap(expected, credentials)?;
        if credentials.is_some() && self.expire_on_write.swap(false, Ordering::SeqCst) {
            self.clock.0.store(4600, Ordering::SeqCst);
            if let Some(replacement) = &self.replacement {
                self.inner.compare_and_swap(revision, Some(replacement))?;
            }
        }
        Ok(revision)
    }
}

#[test]
fn persistence_latency_cannot_return_an_expired_login_or_restored_session() {
    for restore in [false, true] {
        let (_directory, original, store, clock) = setup();
        if restore {
            let ticket = original.begin_login().unwrap();
            original
                .commit_login(&ticket, verified("user_one"))
                .unwrap();
        }
        let service = AccountService::with_clock(
            config(),
            DelayedStore {
                inner: store.clone(),
                clock: clock.clone(),
                expire_on_write: AtomicBool::new(true),
                replacement: None,
            },
            clock,
        );
        let result = if restore {
            let snapshot = store.load().unwrap();
            service.commit_restore(
                snapshot.revision,
                snapshot.credentials.unwrap(),
                verified("user_one"),
            )
        } else {
            let ticket = service.begin_login().unwrap();
            service.commit_login(&ticket, verified("user_one"))
        };
        assert!(matches!(result, Err(AccountError::Expired)));
        assert!(store.load().unwrap().credentials().is_none());
    }
}

#[test]
fn expired_write_cleanup_cannot_clear_a_newer_credential_revision() {
    let (_directory, _original, store, clock) = setup();
    let mut replacement = verified("user_newer");
    replacement.grant.receipt.as_mut().unwrap().wall = UNIX_EPOCH + Duration::from_secs(4600);
    let replacement = SavedCredentials::from_verified(replacement).unwrap();
    let service = AccountService::with_clock(
        config(),
        DelayedStore {
            inner: store.clone(),
            clock: clock.clone(),
            expire_on_write: AtomicBool::new(true),
            replacement: Some(replacement),
        },
        clock,
    );
    let ticket = service.begin_login().unwrap();
    assert!(matches!(
        service.commit_login(&ticket, verified("user_expired")),
        Err(AccountError::Expired)
    ));
    assert_eq!(
        store
            .load()
            .unwrap()
            .credentials()
            .unwrap()
            .account()
            .subject(),
        "user_newer"
    );
}

#[test]
fn observed_clock_rollback_cannot_extend_a_current_session() {
    let (_directory, service, _store, clock) = setup();
    let ticket = service.begin_login().unwrap();
    let session = service.commit_login(&ticket, verified("user_one")).unwrap();
    clock.0.store(4500, Ordering::SeqCst);
    clock.1.store(3500, Ordering::SeqCst);
    assert!(service.is_current(&session).unwrap());
    clock.0.store(1100, Ordering::SeqCst);
    clock.1.store(3501, Ordering::SeqCst);
    assert!(matches!(
        service.is_current(&session),
        Err(AccountError::ClockChanged)
    ));
    clock.0.store(4499, Ordering::SeqCst);
    assert!(matches!(
        service.is_current(&session),
        Err(AccountError::ClockChanged)
    ));
}

#[test]
fn monotonic_lifetime_expires_even_when_the_wall_clock_stalls() {
    let (_directory, service, _store, clock) = setup();
    let ticket = service.begin_login().unwrap();
    let session = service.commit_login(&ticket, verified("user_one")).unwrap();
    clock.1.store(3599, Ordering::SeqCst);
    assert!(service.is_current(&session).unwrap());
    clock.1.store(3600, Ordering::SeqCst);
    assert!(matches!(
        service.is_current(&session),
        Err(AccountError::Expired)
    ));
    clock.0.store(2000, Ordering::SeqCst);
    assert!(matches!(
        service.is_current(&session),
        Err(AccountError::Expired)
    ));
}

#[test]
fn invalid_monotonic_clock_rollback_is_rejected() {
    let (_directory, service, _store, clock) = setup();
    clock.1.store(10, Ordering::SeqCst);
    let ticket = service.begin_login().unwrap();
    let session = service.commit_login(&ticket, verified("user_one")).unwrap();
    clock.1.store(9, Ordering::SeqCst);
    assert!(matches!(
        service.is_current(&session),
        Err(AccountError::ClockChanged)
    ));
}

#[test]
fn receipt_lifetime_covers_verification_before_the_first_clock_observation() {
    let (_directory, service, store, clock) = setup();
    let ticket = service.begin_login().unwrap();
    let before = store.load().unwrap().revision();
    let mut proof = verified("user_expired");
    proof.grant.expires_in = Duration::from_secs(10);
    clock.1.store(11, Ordering::SeqCst);
    assert!(matches!(
        service.commit_login(&ticket, proof),
        Err(AccountError::Expired)
    ));
    let after = store.load().unwrap();
    assert_eq!(after.revision(), before);
    assert!(after.credentials().is_none());
}

#[test]
fn receipt_deadlines_are_per_grant_even_when_the_wall_clock_stalls() {
    let (_directory, service, _store, clock) = setup();
    let first = service.begin_login().unwrap();
    let mut proof = verified("user_old");
    proof.grant.expires_in = Duration::from_secs(10);
    let old = service.commit_login(&first, proof).unwrap();
    clock.1.store(11, Ordering::SeqCst);
    assert!(matches!(
        service.is_current(&old),
        Err(AccountError::Expired)
    ));

    let second = service.begin_login().unwrap();
    let mut fresh = verified("user_new");
    fresh.grant.access_token =
        super::super::SecretValue::try_from("PRIVATE-NEW-ACCESS".to_owned()).unwrap();
    fresh.grant.expires_in = Duration::from_secs(10);
    fresh.grant.receipt.as_mut().unwrap().monotonic = clock.monotonic_now();
    let current = service.commit_login(&second, fresh).unwrap();
    clock.1.store(20, Ordering::SeqCst);
    assert!(service.is_current(&current).unwrap());
    clock.1.store(21, Ordering::SeqCst);
    assert!(matches!(
        service.is_current(&current),
        Err(AccountError::Expired)
    ));
}

#[test]
fn receipt_clocks_cannot_predate_the_grant_even_within_the_same_wall_second() {
    for monotonic in [false, true] {
        let (_directory, service, store, clock) = setup();
        let ticket = service.begin_login().unwrap();
        let mut proof = verified("user_one");
        let receipt = proof.grant.receipt.as_mut().unwrap();
        if monotonic {
            receipt.monotonic = clock.monotonic_now() + Duration::from_nanos(1);
        } else {
            receipt.wall = clock.now() + Duration::from_nanos(1);
        }
        assert!(matches!(
            service.commit_login(&ticket, proof),
            Err(AccountError::ClockChanged)
        ));
        assert!(store.load().unwrap().credentials().is_none());
    }
}

#[test]
fn receipt_is_required_for_a_new_login_but_is_never_reconstructed_from_disk() {
    let (_directory, service, store, _clock) = setup();
    let ticket = service.begin_login().unwrap();
    let mut missing = verified("user_one");
    missing.grant.receipt = None;
    assert!(matches!(
        service.commit_login(&ticket, missing),
        Err(AccountError::Store(CredentialStoreError::InvalidData))
    ));
    service.commit_login(&ticket, verified("user_one")).unwrap();
    let saved = store.load().unwrap().credentials.unwrap();
    assert!(saved.grant.receipt.is_none());
    assert!(saved.monotonic_deadline.is_none());
}

#[test]
fn restoration_keeps_the_remaining_deadline_across_verification_latency() {
    let (_directory, original, store, clock) = setup();
    let ticket = original.begin_login().unwrap();
    let mut proof = verified("user_one");
    proof.grant.expires_in = Duration::from_secs(10);
    original.commit_login(&ticket, proof).unwrap();
    clock.0.store(1004, Ordering::SeqCst);
    clock.1.store(4, Ordering::SeqCst);
    let reopened = AccountService::with_clock(config(), store.clone(), clock.clone());
    let snapshot = store.load().unwrap();
    let mut saved = snapshot.credentials.unwrap();
    // This is the pre-network check performed by restore().
    reopened.check_credentials(&mut saved).unwrap();
    clock.1.store(11, Ordering::SeqCst);
    let mut verified_again = verified("user_one");
    verified_again.grant.expires_in = Duration::from_secs(10);
    assert!(matches!(
        reopened.commit_restore(snapshot.revision, saved, verified_again),
        Err(AccountError::Expired)
    ));
    assert_eq!(store.load().unwrap().revision(), snapshot.revision);
}

struct PausingWallClock {
    clock: Clock,
    pause_seconds: AtomicU64,
}

impl WallClock for PausingWallClock {
    fn now(&self) -> SystemTime {
        let wall = self.clock.now();
        self.clock.1.fetch_add(
            self.pause_seconds.swap(0, Ordering::SeqCst),
            Ordering::SeqCst,
        );
        wall
    }

    fn monotonic_now(&self) -> Instant {
        self.clock.monotonic_now()
    }
}

#[test]
fn restoration_sampling_cannot_extend_lifetime_when_paused_between_clock_reads() {
    let (_directory, original, store, clock) = setup();
    let ticket = original.begin_login().unwrap();
    let mut proof = verified("user_one");
    proof.grant.expires_in = Duration::from_secs(10);
    original.commit_login(&ticket, proof).unwrap();
    clock.0.store(1004, Ordering::SeqCst);
    clock.1.store(4, Ordering::SeqCst);
    let reopened = AccountService::with_clock(
        config(),
        store.clone(),
        PausingWallClock {
            clock: clock.clone(),
            pause_seconds: AtomicU64::new(3),
        },
    );
    let snapshot = store.load().unwrap();
    let mut saved = snapshot.credentials.unwrap();
    reopened.check_credentials(&mut saved).unwrap();
    assert_eq!(clock.1.load(Ordering::SeqCst), 7);
    assert_eq!(
        saved.monotonic_deadline,
        Some(clock_origin() + Duration::from_secs(10))
    );
    clock.1.store(11, Ordering::SeqCst);
    let mut verified_again = verified("user_one");
    verified_again.grant.expires_in = Duration::from_secs(10);
    assert!(matches!(
        reopened.commit_restore(snapshot.revision, saved, verified_again),
        Err(AccountError::Expired)
    ));
    assert_eq!(store.load().unwrap().revision(), snapshot.revision);
}

#[test]
fn status_gate_requires_a_current_owned_account_and_does_not_enable_sharing() {
    use crate::status::{AccountGate, AccountSessionGate};
    let (_directory, account, _store, _clock) = setup();
    let ticket = account.begin_login().unwrap();
    let session = account.commit_login(&ticket, verified("user_one")).unwrap();
    let account = Arc::new(account);
    let gate = AccountSessionGate::new(account.clone());
    assert!(gate.current().unwrap().is_none());
    gate.set_session(Some(session.clone())).unwrap();
    let binding = gate.current().unwrap().unwrap();
    assert_eq!(binding.account.subject, "user_one");
    assert_eq!(binding.auth_revision, session.revision().value());
    let (_other_directory, other, _other_store, _clock) = setup();
    let foreign_ticket = other.begin_login().unwrap();
    let foreign = other
        .commit_login(&foreign_ticket, verified("user_one"))
        .unwrap();
    gate.set_session(Some(foreign)).unwrap();
    assert!(gate.current().unwrap().is_none());
    gate.set_session(Some(session)).unwrap();
    account.sign_out().unwrap();
    assert!(gate.current().unwrap().is_none());
}

#[test]
fn status_gate_rejects_accounts_without_the_ai_feature_scope() {
    use crate::status::{AccountGate, AccountSessionGate, StatusError};
    let (_directory, _unused, store, clock) = setup();
    let minimal = DeviceAuthConfig::new(
        "https://clerk.example.com",
        "public-client",
        "https://accounts.example.com/device",
        &["openid", "offline_access"],
    )
    .unwrap();
    let mut proof = verified("user_one");
    proof.grant.provider = Box::new(ProviderIdentity::from(&minimal));
    proof.grant.scopes = minimal.scopes().to_vec();
    let account = Arc::new(AccountService::with_clock(minimal, store, clock));
    let ticket = account.begin_login().unwrap();
    let session = account.commit_login(&ticket, proof).unwrap();
    let gate = AccountSessionGate::new(account);
    gate.set_session(Some(session)).unwrap();
    assert_eq!(
        gate.current().unwrap_err(),
        StatusError::FeatureUnauthorized
    );
}

/// Records what it was asked to revoke, and answers with `result`.
struct Revoker {
    revoked: std::sync::Mutex<Vec<(String, Option<String>)>>,
    result: Result<(), AuthError>,
}

impl Revoker {
    fn answering(result: Result<(), AuthError>) -> Self {
        Self {
            revoked: std::sync::Mutex::new(Vec::new()),
            result,
        }
    }

    fn revoked(&self) -> Vec<(String, Option<String>)> {
        self.revoked.lock().unwrap().clone()
    }
}

impl GrantRevoker for Revoker {
    fn revoke(&self, grant: &AuthorizationGrant) -> Result<(), AuthError> {
        self.revoked.lock().unwrap().push((
            grant.access_token().expose_secret().to_owned(),
            grant
                .refresh_token()
                .map(|token| token.expose_secret().to_owned()),
        ));
        self.result.clone()
    }
}

#[test]
fn signing_out_revokes_the_removed_sign_in_once() {
    let (_directory, service, store, _clock) = setup();
    let ticket = service.begin_login().unwrap();
    service.commit_login(&ticket, verified("user_one")).unwrap();
    let revoker = Revoker::answering(Ok(()));
    // Nothing signed out yet: nothing to revoke.
    assert_eq!(
        service.revoke_signed_out(&revoker),
        SignOutRevocation::NotNeeded
    );

    service.sign_out().unwrap();
    assert!(store.load().unwrap().credentials().is_none());
    assert_eq!(
        service.revoke_signed_out(&revoker),
        SignOutRevocation::Revoked
    );
    assert_eq!(
        revoker.revoked(),
        vec![(
            "PRIVATE-ACCESS-TOKEN".to_owned(),
            Some("PRIVATE-REFRESH-TOKEN".to_owned())
        )]
    );
    // Taken once: a second sign-out request finds nothing more to revoke.
    assert_eq!(
        service.revoke_signed_out(&revoker),
        SignOutRevocation::NotNeeded
    );
    assert_eq!(revoker.revoked().len(), 1);
}

#[test]
fn a_failed_revocation_still_leaves_this_machine_signed_out() {
    let (_directory, service, store, _clock) = setup();
    let ticket = service.begin_login().unwrap();
    let session = service.commit_login(&ticket, verified("user_one")).unwrap();
    service.sign_out().unwrap();
    let revoker = Revoker::answering(Err(AuthError::Network(
        super::super::NetworkFailure::Connection,
    )));
    assert_eq!(
        service.revoke_signed_out(&revoker),
        SignOutRevocation::Failed(AuthError::Network(super::super::NetworkFailure::Connection))
    );
    assert_eq!(revoker.revoked().len(), 1);
    assert!(store.load().unwrap().credentials().is_none());
    assert!(!service.is_current(&session).unwrap());
    // What it holds is never shown.
    let pending = {
        let ticket = service.begin_login().unwrap();
        service.commit_login(&ticket, verified("user_one")).unwrap();
        service.sign_out().unwrap();
        service.take_signed_out().unwrap()
    };
    let shown = format!("{pending:?}");
    assert!(!shown.contains("PRIVATE"));
}

#[test]
fn an_enrolled_machines_tokens_are_never_sent_to_clerk() {
    let (_directory, service, _store, _clock) = setup();
    let ticket = service.begin_login().unwrap();
    let mut machine = verified("user_one");
    machine.grant.access_token =
        crate::auth::SecretValue::try_from("rkma.v1.claims.signature".to_owned()).unwrap();
    machine.grant.refresh_token =
        Some(crate::auth::SecretValue::try_from(format!("rkm_{}", "s".repeat(43))).unwrap());
    service.commit_login(&ticket, machine).unwrap();
    service.sign_out().unwrap();
    let revoker = Revoker::answering(Ok(()));
    assert_eq!(
        service.revoke_signed_out(&revoker),
        SignOutRevocation::NotNeeded
    );
    assert!(revoker.revoked().is_empty());
}
