use super::*;
use crate::auth::CredentialSnapshot;

fn signed_in<S: CredentialStore>(service: &AccountService<S, Clock>) -> AccountSession {
    let login = service.begin_login().unwrap();
    service.commit_login(&login, verified("user_one")).unwrap()
}

#[test]
fn lease_uses_only_the_current_verified_account_and_does_not_write_credentials() {
    let (_directory, service, store, _clock) = setup();
    let session = signed_in(&service);
    let revision = store.load().unwrap().revision();
    let lease = service.lease_access_token(&session).unwrap().unwrap();
    assert_eq!(lease.session().revision(), revision);
    assert_eq!(lease.session().account().subject(), "user_one");
    assert_eq!(store.load().unwrap().revision(), revision);
    let used = service
        .consume_access_token(lease, |token| {
            assert_eq!(token.expose_secret(), "PRIVATE-ACCESS-TOKEN");
            "synthetic dispatch result"
        })
        .unwrap();
    assert_eq!(used, Some("synthetic dispatch result"));
    assert_eq!(store.load().unwrap().revision(), revision);
}

#[test]
fn signout_suppresses_both_lease_creation_and_pending_token_use() {
    let (_directory, service, _store, _clock) = setup();
    let session = signed_in(&service);
    let lease = service.lease_access_token(&session).unwrap().unwrap();
    service.sign_out().unwrap();
    assert!(service.lease_access_token(&session).unwrap().is_none());
    assert!(service
        .consume_access_token(lease, |_| panic!("signed-out dispatch"))
        .unwrap()
        .is_none());
}

#[test]
fn replacing_an_account_cannot_retarget_a_prepared_token_use() {
    let (_directory, service, _store, _clock) = setup();
    let session = signed_in(&service);
    let lease = service.lease_access_token(&session).unwrap().unwrap();
    let login = service.begin_login().unwrap();
    service
        .commit_login(&login, verified("user_other"))
        .unwrap();
    assert!(service
        .consume_access_token(lease, |_| panic!("retargeted dispatch"))
        .unwrap()
        .is_none());
}

#[test]
fn even_same_account_login_intents_invalidate_old_token_use() {
    let (_directory, service, _store, _clock) = setup();
    let session = signed_in(&service);
    let lease = service.lease_access_token(&session).unwrap().unwrap();
    let login = service.begin_login().unwrap();
    service.cancel_login(&login).unwrap();
    assert!(service
        .consume_access_token(lease, |_| panic!("old revision dispatch"))
        .unwrap()
        .is_none());
}

#[test]
fn foreign_service_cannot_issue_or_consume_a_lease_even_at_matching_revisions() {
    let (_first_directory, first, _first_store, _first_clock) = setup();
    let (_second_directory, second, _second_store, _second_clock) = setup();
    let first_session = signed_in(&first);
    let second_session = signed_in(&second);
    assert_eq!(first_session.revision(), second_session.revision());
    assert!(second.lease_access_token(&first_session).unwrap().is_none());
    let lease = first.lease_access_token(&first_session).unwrap().unwrap();
    assert!(second
        .consume_access_token(lease, |_| panic!("foreign dispatch"))
        .unwrap()
        .is_none());
}

#[test]
fn reopening_the_same_store_does_not_transfer_lease_ownership() {
    let (_directory, original, store, clock) = setup();
    let session = signed_in(&original);
    let lease = original.lease_access_token(&session).unwrap().unwrap();
    let replacement = AccountService::with_clock(config(), store, clock);
    assert!(replacement
        .consume_access_token(lease, |_| panic!("reopened owner dispatch"))
        .unwrap()
        .is_none());
}

#[test]
fn token_use_expires_at_thirty_seconds_without_extending_account_lifetime() {
    for elapsed in [29, 30, 31] {
        let (_directory, service, _store, clock) = setup();
        let session = signed_in(&service);
        let lease = service.lease_access_token(&session).unwrap().unwrap();
        clock.0.store(1000 + elapsed, Ordering::SeqCst);
        clock.1.store(elapsed, Ordering::SeqCst);
        let used = service.consume_access_token(lease, |_| {
            assert_eq!(elapsed, 29);
            true
        });
        if elapsed == 29 {
            assert_eq!(used.unwrap(), Some(true));
        } else {
            assert_eq!(used.unwrap_err(), AccountError::TokenLeaseExpired);
        }
        assert!(service.is_current(&session).unwrap());
    }
}

#[test]
fn account_expiry_wins_even_when_the_token_use_was_just_prepared() {
    let (_directory, service, _store, clock) = setup();
    let session = signed_in(&service);
    clock.0.store(4595, Ordering::SeqCst);
    clock.1.store(3595, Ordering::SeqCst);
    let lease = service.lease_access_token(&session).unwrap().unwrap();
    clock.0.store(4600, Ordering::SeqCst);
    clock.1.store(3600, Ordering::SeqCst);
    assert_eq!(
        service
            .consume_access_token(lease, |_| panic!("expired dispatch"))
            .unwrap_err(),
        AccountError::Expired
    );
    assert_eq!(
        service.lease_access_token(&session).unwrap_err(),
        AccountError::Expired
    );
}

#[test]
fn monotonic_elapsed_time_cannot_be_bypassed_by_a_stalled_wall_clock() {
    let (_directory, service, _store, clock) = setup();
    let session = signed_in(&service);
    let lease = service.lease_access_token(&session).unwrap().unwrap();
    clock.1.store(30, Ordering::SeqCst);
    assert_eq!(
        service
            .consume_access_token(lease, |_| panic!("stalled clock dispatch"))
            .unwrap_err(),
        AccountError::TokenLeaseExpired
    );
}

#[test]
fn clock_rollback_and_unsafe_storage_suppress_token_use() {
    let (_directory, service, store, clock) = setup();
    let session = signed_in(&service);
    let lease = service.lease_access_token(&session).unwrap().unwrap();
    clock.0.store(999, Ordering::SeqCst);
    assert_eq!(
        service
            .consume_access_token(lease, |_| panic!("rollback dispatch"))
            .unwrap_err(),
        AccountError::ClockChanged
    );

    // A separate owner avoids the deliberately sticky rollback observation.
    let (directory, service, _store, _clock) = setup();
    let session = signed_in(&service);
    let lease = service.lease_access_token(&session).unwrap().unwrap();
    fs::set_permissions(
        directory.path().join("private-auth"),
        fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    assert_eq!(
        service
            .consume_access_token(lease, |_| panic!("unsafe storage dispatch"))
            .unwrap_err(),
        AccountError::Store(CredentialStoreError::UnsafePath)
    );
    assert!(store.load().unwrap().credentials().is_some());
}

#[test]
fn lease_debug_redacts_account_and_revision_in_every_format() {
    let (_directory, service, _store, _clock) = setup();
    let session = signed_in(&service);
    let lease = service.lease_access_token(&session).unwrap().unwrap();
    let rendered = format!("{lease:?} {lease:#?}");
    for private in [
        "user_one",
        "PRIVATE-ACCESS-TOKEN",
        "PRIVATE-REFRESH-TOKEN",
        "clerk.example.com",
        "native-client",
    ] {
        assert!(!rendered.contains(private));
    }
}

#[derive(Clone, Copy)]
enum SnapshotFault {
    None,
    Subject,
    Provider,
    Scopes,
}

struct ControlledReadStore {
    inner: FileCredentialStore,
    clock: Clock,
    delay: AtomicU64,
    reads: AtomicU64,
    fail: AtomicBool,
    fault: Mutex<SnapshotFault>,
}

impl CredentialStore for ControlledReadStore {
    fn load(&self) -> Result<CredentialSnapshot, CredentialStoreError> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        if self.fail.load(Ordering::SeqCst) {
            return Err(CredentialStoreError::Busy);
        }
        let mut snapshot = self.inner.load()?;
        if let Some(credentials) = snapshot.credentials.as_mut() {
            match *self.fault.lock().unwrap() {
                SnapshotFault::None => {}
                SnapshotFault::Subject => credentials.account.subject = "user_other".into(),
                SnapshotFault::Provider => credentials.grant.provider.client_id.push_str("-other"),
                SnapshotFault::Scopes => credentials
                    .grant
                    .scopes
                    .retain(|scope| scope != "ai:session-status"),
            }
        }
        let delay = self.delay.load(Ordering::SeqCst);
        self.clock.0.fetch_add(delay, Ordering::SeqCst);
        self.clock.1.fetch_add(delay, Ordering::SeqCst);
        Ok(snapshot)
    }

    fn compare_and_swap(
        &self,
        expected: CredentialRevision,
        credentials: Option<&SavedCredentials>,
    ) -> Result<CredentialRevision, CredentialStoreError> {
        self.inner.compare_and_swap(expected, credentials)
    }
}

fn controlled_read_service(
    store: FileCredentialStore,
    clock: Clock,
) -> AccountService<ControlledReadStore, Clock> {
    AccountService::with_clock(
        config(),
        ControlledReadStore {
            inner: store,
            clock: clock.clone(),
            delay: AtomicU64::new(0),
            reads: AtomicU64::new(0),
            fail: AtomicBool::new(false),
            fault: Mutex::new(SnapshotFault::None),
        },
        clock,
    )
}

#[test]
fn consumption_lends_only_the_single_freshly_validated_snapshot() {
    let (_directory, _original, store, clock) = setup();
    let service = controlled_read_service(store, clock);
    let session = signed_in(&service);
    let lease = service.lease_access_token(&session).unwrap().unwrap();
    let before = service.store.reads.load(Ordering::SeqCst);
    assert_eq!(
        service
            .consume_access_token(lease, |token| {
                assert_eq!(token.expose_secret(), "PRIVATE-ACCESS-TOKEN");
                service.store.reads.load(Ordering::SeqCst)
            })
            .unwrap(),
        Some(before + 1)
    );
    assert_eq!(service.store.reads.load(Ordering::SeqCst), before + 1);
}

#[test]
fn slow_reads_cannot_return_a_lease_or_dispatch_after_expiry() {
    for delay in [30, 3600] {
        let (_directory, _original, store, clock) = setup();
        let service = controlled_read_service(store, clock);
        let session = signed_in(&service);
        let lease = service.lease_access_token(&session).unwrap().unwrap();
        service.store.delay.store(delay, Ordering::SeqCst);
        let expected = if delay == 30 {
            AccountError::TokenLeaseExpired
        } else {
            AccountError::Expired
        };
        assert_eq!(
            service
                .consume_access_token(lease, |_| panic!("late dispatch"))
                .unwrap_err(),
            expected
        );
    }
    let (_directory, _original, store, clock) = setup();
    let service = controlled_read_service(store, clock);
    let session = signed_in(&service);
    service.store.delay.store(3600, Ordering::SeqCst);
    assert_eq!(
        service.lease_access_token(&session).unwrap_err(),
        AccountError::Expired
    );
}

#[test]
fn read_failure_suppresses_use_and_foreign_owners_do_not_read_storage() {
    let (_directory, _original, store, clock) = setup();
    let service = controlled_read_service(store, clock);
    let session = signed_in(&service);
    let lease = service.lease_access_token(&session).unwrap().unwrap();
    service.store.fail.store(true, Ordering::SeqCst);
    assert_eq!(
        service
            .consume_access_token(lease, |_| panic!("failed read dispatch"))
            .unwrap_err(),
        AccountError::Store(CredentialStoreError::Busy)
    );
    assert_eq!(
        service.lease_access_token(&session).unwrap_err(),
        AccountError::Store(CredentialStoreError::Busy)
    );

    let (_other_directory, other, _other_store, _other_clock) = setup();
    let other_session = signed_in(&other);
    let lease = other.lease_access_token(&other_session).unwrap().unwrap();
    let reads = service.store.reads.load(Ordering::SeqCst);
    assert!(service
        .lease_access_token(&other_session)
        .unwrap()
        .is_none());
    assert!(service
        .consume_access_token(lease, |_| panic!("foreign dispatch"))
        .unwrap()
        .is_none());
    assert_eq!(service.store.reads.load(Ordering::SeqCst), reads);
}

#[test]
fn subject_provider_and_scope_mismatches_cannot_expose_a_bearer_even_at_the_same_revision() {
    for fault in [
        SnapshotFault::Subject,
        SnapshotFault::Provider,
        SnapshotFault::Scopes,
    ] {
        let (_directory, _original, store, clock) = setup();
        let service = controlled_read_service(store, clock);
        let session = signed_in(&service);
        let lease = service.lease_access_token(&session).unwrap().unwrap();
        *service.store.fault.lock().unwrap() = fault;
        let used = service.consume_access_token(lease, |_| panic!("mismatched dispatch"));
        let issued = service.lease_access_token(&session);
        match fault {
            SnapshotFault::Subject => {
                assert!(used.unwrap().is_none());
                assert!(issued.unwrap().is_none());
            }
            _ => {
                assert_eq!(used.unwrap_err(), AccountError::ProviderMismatch);
                assert_eq!(issued.unwrap_err(), AccountError::ProviderMismatch);
            }
        }
    }
}

#[test]
fn monotonic_rollback_suppresses_prepared_token_use() {
    let (_directory, service, _store, clock) = setup();
    let session = signed_in(&service);
    clock.1.store(10, Ordering::SeqCst);
    let lease = service.lease_access_token(&session).unwrap().unwrap();
    clock.1.store(9, Ordering::SeqCst);
    assert_eq!(
        service
            .consume_access_token(lease, |_| panic!("monotonic rollback dispatch"))
            .unwrap_err(),
        AccountError::ClockChanged
    );
}

#[test]
fn poisoned_clock_state_suppresses_both_lease_creation_and_consumption() {
    let (_directory, service, _store, _clock) = setup();
    let session = signed_in(&service);
    let lease = service.lease_access_token(&session).unwrap().unwrap();
    let poison = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _guard = service.observation.lock().unwrap();
        panic!("synthetic clock-state poison");
    }));
    assert!(poison.is_err());
    assert_eq!(
        service.lease_access_token(&session).unwrap_err(),
        AccountError::ClockUnavailable
    );
    assert_eq!(
        service
            .consume_access_token(lease, |_| panic!("poisoned clock dispatch"))
            .unwrap_err(),
        AccountError::ClockUnavailable
    );
}
