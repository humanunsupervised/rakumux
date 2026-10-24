use super::*;
use crate::status::{AccountKey, ContextAvailability, SourceStamp};
use session_core::model::ScopeRoot;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

#[derive(Clone)]
struct Gate(Arc<Mutex<Option<AccountBinding>>>, Arc<AtomicBool>);

impl AccountGate for Gate {
    fn current(&self) -> Result<Option<AccountBinding>, StatusError> {
        if self.1.load(Ordering::SeqCst) {
            return Err(StatusError::AccountUnavailable);
        }
        Ok(self.0.lock().unwrap().clone())
    }
}

struct Clock;
impl StatusClock for Clock {
    fn now_seconds(&self) -> Result<u64, StatusError> {
        Ok(1_000)
    }
}

#[derive(Default)]
struct State {
    consent: ConsentSnapshot,
    cache: HashMap<SessionId, CachedStatus>,
    fail_write: bool,
    fail_remove: bool,
    after_read: Option<Box<dyn FnOnce() + Send>>,
    after_save: Option<Box<dyn FnOnce() + Send>>,
    after_consent: Option<Box<dyn FnOnce() + Send>>,
}

#[derive(Clone, Default)]
struct Store(Arc<Mutex<State>>);

impl StatusStore for Store {
    fn load_consent(&mut self) -> Result<ConsentSnapshot, StatusStoreError> {
        let consent = self.0.lock().unwrap().consent.clone();
        consent.validate()?;
        Ok(consent)
    }

    fn compare_and_swap_consent(
        &mut self,
        expected: ConsentRevision,
        policy: &ConsentPolicy,
    ) -> Result<ConsentSnapshot, StatusStoreError> {
        policy.validate()?;
        let mut state = self.0.lock().unwrap();
        if state.fail_write {
            return Err(StatusStoreError::Database);
        }
        if state.consent.revision != expected {
            return Err(StatusStoreError::Conflict);
        }
        let revision = expected
            .0
            .checked_add(1)
            .ok_or(StatusStoreError::RevisionExhausted)?;
        state.consent = ConsentSnapshot {
            revision: ConsentRevision(revision),
            policy: policy.clone(),
        };
        state.cache.clear();
        let result = state.consent.clone();
        let hook = state.after_consent.take();
        drop(state);
        if let Some(hook) = hook {
            hook();
        }
        Ok(result)
    }

    fn load_cache(
        &mut self,
        session: &SessionId,
    ) -> Result<Option<CachedStatus>, StatusStoreError> {
        let mut state = self.0.lock().unwrap();
        let result = state.cache.get(session).cloned();
        let hook = state.after_read.take();
        drop(state);
        if let Some(hook) = hook {
            hook();
        }
        Ok(result)
    }

    fn save_cache(
        &mut self,
        expected: ConsentRevision,
        entry: &CachedStatus,
    ) -> Result<(), StatusStoreError> {
        entry.validate()?;
        let mut state = self.0.lock().unwrap();
        if state.fail_write {
            return Err(StatusStoreError::Database);
        }
        if state.consent.revision != expected
            || entry.consent_revision != expected
            || !state.consent.policy.permits_cache(entry)
        {
            return Err(StatusStoreError::Conflict);
        }
        state.cache.insert(entry.session_id.clone(), entry.clone());
        let hook = state.after_save.take();
        drop(state);
        if let Some(hook) = hook {
            hook();
        }
        Ok(())
    }

    fn remove_cache(&mut self, session: &SessionId) -> Result<(), StatusStoreError> {
        let mut state = self.0.lock().unwrap();
        if state.fail_remove {
            return Err(StatusStoreError::Database);
        }
        state.cache.remove(session);
        Ok(())
    }
}

type Service = AISessionStatusService<Store, Gate, Clock>;

fn binding() -> AccountBinding {
    AccountBinding {
        account: AccountKey {
            issuer: "https://issuer.example/".into(),
            client_id: "native-client".into(),
            subject: "synthetic-subject".into(),
        },
        auth_revision: 2,
    }
}

fn source() -> StatusSource {
    StatusSource {
        session_id: SessionId::generate(),
        folder: ScopeRoot::from_canonical(PathBuf::from("/synthetic/folder")),
        stamp: SourceStamp {
            terminal_generation: 1,
            source_revision: 1,
        },
        private: false,
        live: true,
        context: ContextAvailability {
            reported_text: true,
            recent_output: true,
        },
    }
}

fn setup() -> (Service, Store, Gate, StatusSource) {
    let store = Store::default();
    let gate = Gate(
        Arc::new(Mutex::new(Some(binding()))),
        Arc::new(AtomicBool::new(false)),
    );
    let service = AISessionStatusService::with_clock(store.clone(), gate.clone(), Clock);
    (service, store, gate, source())
}

fn folders(source: &StatusSource, include_terminal_output: bool) -> Vec<FolderConsent> {
    vec![FolderConsent {
        folder: source.folder.clone(),
        include_terminal_output,
    }]
}

fn enabled(service: &mut Service, source: &StatusSource, output: bool) {
    service.observe(source.clone()).unwrap();
    service
        .enable(ConsentRevision(0), &binding(), folders(source, output))
        .unwrap();
}

fn ticket(service: &mut Service, source: &StatusSource) -> Box<GenerationTicket> {
    match service.prepare(&source.session_id).unwrap() {
        Preparation::Ready(ticket) => ticket,
        other => panic!("expected prepared generation, got {other:?}"),
    }
}

fn text() -> StatusText {
    StatusText::try_from("Checking synthetic changes".to_owned()).unwrap()
}

#[test]
fn signed_in_alone_never_enables_sharing_and_initial_policy_is_disabled() {
    let (mut service, store, _gate, source) = setup();
    service.observe(source.clone()).unwrap();
    assert_eq!(service.consent().unwrap(), ConsentSnapshot::default());
    assert!(matches!(
        service.prepare(&source.session_id).unwrap(),
        Preparation::Skipped(StatusView::Disabled)
    ));
    assert!(store.0.lock().unwrap().cache.is_empty());
}

#[test]
fn enable_requires_current_authentication_and_nonempty_unique_folders() {
    let (mut service, store, gate, source) = setup();
    *gate.0.lock().unwrap() = None;
    assert_eq!(
        service
            .enable(ConsentRevision(0), &binding(), folders(&source, false))
            .unwrap_err(),
        StatusError::SignedOut
    );
    *gate.0.lock().unwrap() = Some(binding());
    for selected in [
        vec![],
        [folders(&source, false), folders(&source, true)].concat(),
    ] {
        assert!(matches!(
            service.enable(ConsentRevision(0), &binding(), selected),
            Err(StatusError::Store(StatusStoreError::InvalidData))
        ));
    }
    assert_eq!(store.0.lock().unwrap().consent.revision, ConsentRevision(0));
}

#[test]
fn reports_only_consent_never_authorizes_terminal_output_as_fallback() {
    let (mut service, _store, _gate, mut source) = setup();
    source.context.reported_text = false;
    enabled(&mut service, &source, false);
    assert_eq!(
        service.view(&source.session_id).unwrap(),
        StatusView::NoContext
    );
    service
        .enable(ConsentRevision(1), &binding(), folders(&source, true))
        .unwrap();
    assert_eq!(
        ticket(&mut service, &source).input_selection(),
        InputSelection::ReportAndOutput
    );
}

#[test]
fn unselected_private_nonlive_and_empty_sources_prepare_no_jobs() {
    for (case, expected) in [
        (0, StatusView::Excluded),
        (1, StatusView::Private),
        (2, StatusView::NotLive),
        (3, StatusView::NoContext),
    ] {
        let (mut service, _store, _gate, allowed) = setup();
        enabled(&mut service, &allowed, true);
        let mut excluded = source();
        match case {
            0 => excluded.folder = ScopeRoot::from_canonical(PathBuf::from("/other/folder")),
            1 => excluded.private = true,
            2 => excluded.live = false,
            _ => excluded.context = ContextAvailability::default(),
        }
        service.observe(excluded.clone()).unwrap();
        assert!(
            matches!(service.prepare(&excluded.session_id).unwrap(), Preparation::Skipped(view) if view == expected)
        );
    }
}

#[test]
fn matching_completion_is_advisory_cached_and_unchanged_input_is_not_regenerated() {
    let (mut service, _store, _gate, source) = setup();
    enabled(&mut service, &source, false);
    let ticket = ticket(&mut service, &source);
    assert_eq!(ticket.input_selection(), InputSelection::ReportOnly);
    assert_eq!(ticket.request_id().len(), 32);
    assert_ne!(ticket.request_id(), source.session_id.as_str());
    assert_eq!(
        service
            .complete(&ticket, ticket.request_id(), text())
            .unwrap(),
        ApplyResult::Applied
    );
    let StatusView::Ready(cached) = service.view(&source.session_id).unwrap() else {
        panic!("missing cache");
    };
    assert_eq!(cached.text.as_str(), "Checking synthetic changes");
    assert_eq!(cached.captured_at, 1000);
    assert!(matches!(
        service.prepare(&source.session_id).unwrap(),
        Preparation::Skipped(StatusView::Ready(_))
    ));
    let debug = format!("{cached:?} {ticket:?}");
    for sensitive in [
        "Checking synthetic changes",
        "synthetic-subject",
        "/synthetic/folder",
    ] {
        assert!(!debug.contains(sensitive));
    }
}

#[test]
fn optout_invalidates_dispatch_results_and_stale_window_reenable() {
    let (mut service, store, _gate, source) = setup();
    enabled(&mut service, &source, true);
    let ticket = ticket(&mut service, &source);
    service.disable(ConsentRevision(1)).unwrap();
    assert!(!service.is_current(&ticket).unwrap());
    assert_eq!(
        service
            .complete(&ticket, ticket.request_id(), text())
            .unwrap(),
        ApplyResult::Ignored
    );
    assert!(matches!(
        service.enable(ConsentRevision(1), &binding(), folders(&source, true)),
        Err(StatusError::Store(StatusStoreError::Conflict))
    ));
    assert_eq!(
        service.view(&source.session_id).unwrap(),
        StatusView::Disabled
    );
    assert!(store.0.lock().unwrap().cache.is_empty());
}

#[test]
fn signout_account_switch_and_auth_revision_changes_hide_cache_and_discard_results() {
    for case in 0..3 {
        let (mut service, store, gate, source) = setup();
        enabled(&mut service, &source, true);
        let ticket = ticket(&mut service, &source);
        let mut changed = binding();
        if case == 1 {
            changed.account.subject = "different-subject".into();
        }
        if case == 2 {
            changed.auth_revision += 1;
        }
        *gate.0.lock().unwrap() = if case == 0 { None } else { Some(changed) };
        assert!(!service.is_current(&ticket).unwrap());
        assert_eq!(
            service
                .complete(&ticket, ticket.request_id(), text())
                .unwrap(),
            ApplyResult::Ignored
        );
        assert_eq!(
            service.view(&source.session_id).unwrap(),
            if case == 0 {
                StatusView::SignedOut
            } else {
                StatusView::ConsentRequired
            }
        );
        assert!(store.0.lock().unwrap().cache.is_empty());
    }
}

#[test]
fn changed_source_terminal_privacy_or_removed_session_discards_old_results() {
    for case in 0..4 {
        let (mut service, store, _gate, mut source) = setup();
        enabled(&mut service, &source, true);
        let ticket = ticket(&mut service, &source);
        match case {
            0 => source.stamp.source_revision += 1,
            1 => {
                source.stamp.terminal_generation += 1;
                source.stamp.source_revision = 1;
            }
            2 => source.private = true,
            _ => {}
        }
        if case == 3 {
            service.remove(&source.session_id).unwrap();
        } else {
            service.observe(source).unwrap();
        }
        assert!(!service.is_current(&ticket).unwrap());
        assert_eq!(
            service
                .complete(&ticket, ticket.request_id(), text())
                .unwrap(),
            ApplyResult::Ignored
        );
        assert!(store.0.lock().unwrap().cache.is_empty());
    }
}

#[test]
fn old_observations_cannot_downgrade_a_newer_private_source() {
    let (mut service, _store, _gate, mut source) = setup();
    enabled(&mut service, &source, true);
    let older = source.clone();
    source.private = true;
    source.stamp.source_revision += 1;
    service.observe(source.clone()).unwrap();
    assert_eq!(service.observe(older).unwrap(), ApplyResult::Ignored);
    assert_eq!(
        service.view(&source.session_id).unwrap(),
        StatusView::Private
    );
    let mut inconsistent = source.clone();
    inconsistent.private = false;
    assert_eq!(
        service.observe(inconsistent).unwrap_err(),
        StatusError::InvalidSource
    );
    assert_eq!(service.view(&source.session_id).unwrap(), StatusView::Stale);
    source.stamp.source_revision += 1;
    service.observe(source.clone()).unwrap();
    assert_eq!(
        service.view(&source.session_id).unwrap(),
        StatusView::Private
    );
}

#[test]
fn duplicate_foreign_replayed_and_mismatched_results_cannot_replace_cache() {
    let (mut first, store, gate, source) = setup();
    enabled(&mut first, &source, true);
    let first_ticket = ticket(&mut first, &source);
    assert!(matches!(
        first.prepare(&source.session_id).unwrap(),
        Preparation::Skipped(StatusView::Pending)
    ));
    let mut second = AISessionStatusService::with_clock(store.clone(), gate, Clock);
    second.observe(source.clone()).unwrap();
    let second_ticket = ticket(&mut second, &source);
    assert_eq!(
        second
            .complete(&first_ticket, first_ticket.request_id(), text())
            .unwrap(),
        ApplyResult::Ignored
    );
    assert!(second.is_current(&second_ticket).unwrap());
    assert_eq!(
        second
            .complete(&second_ticket, "wrong-id", text())
            .unwrap_err(),
        StatusError::InvalidResponse
    );
    assert!(store.0.lock().unwrap().cache.is_empty());
    assert_eq!(
        first
            .complete(&first_ticket, first_ticket.request_id(), text())
            .unwrap(),
        ApplyResult::Applied
    );
    assert_eq!(
        first
            .complete(&first_ticket, first_ticket.request_id(), text())
            .unwrap(),
        ApplyResult::Ignored
    );
}

#[test]
fn pending_slots_remain_bounded_until_stale_workers_acknowledge_completion() {
    let (mut service, _store, _gate, mut source) = setup();
    enabled(&mut service, &source, true);
    let first = ticket(&mut service, &source);
    source.stamp.source_revision += 1;
    service.observe(source.clone()).unwrap();
    let second = ticket(&mut service, &source);
    source.stamp.source_revision += 1;
    service.observe(source.clone()).unwrap();
    assert!(matches!(
        service.prepare(&source.session_id).unwrap(),
        Preparation::Skipped(StatusView::Pending)
    ));
    assert_eq!(service.abandon(&first), ApplyResult::Applied);
    assert_eq!(service.abandon(&first), ApplyResult::Ignored);
    let third = ticket(&mut service, &source);
    assert!(service.is_current(&third).unwrap());
    assert!(!service.is_current(&second).unwrap());
}

#[test]
fn an_old_cache_does_not_allow_duplicate_pending_work_for_a_new_source() {
    let (mut service, _store, _gate, mut source) = setup();
    enabled(&mut service, &source, true);
    let old = ticket(&mut service, &source);
    service.complete(&old, old.request_id(), text()).unwrap();
    source.stamp.source_revision += 1;
    service.observe(source.clone()).unwrap();
    // The old text stays shown through a revision of the same terminal, but
    // doesn't answer the new source: a new job may be made, once.
    assert!(matches!(
        service.view(&source.session_id).unwrap(),
        StatusView::Ready(_)
    ));
    let _new = ticket(&mut service, &source);
    assert!(matches!(
        service.view(&source.session_id).unwrap(),
        StatusView::Ready(_)
    ));
    assert!(matches!(
        service.prepare(&source.session_id).unwrap(),
        Preparation::Skipped(StatusView::Pending)
    ));
    // A new terminal process hides it.
    source.stamp.terminal_generation += 1;
    source.stamp.source_revision = 1;
    service.observe(source.clone()).unwrap();
    assert_eq!(service.view(&source.session_id).unwrap(), StatusView::Stale);
}

#[test]
fn slow_cache_read_or_write_cannot_expose_data_after_authentication_changes() {
    for read in [false, true] {
        let (mut service, store, gate, source) = setup();
        enabled(&mut service, &source, true);
        let ticket = ticket(&mut service, &source);
        if read {
            service
                .complete(&ticket, ticket.request_id(), text())
                .unwrap();
        }
        let changed_gate = gate.clone();
        let hook = Some(Box::new(move || {
            *changed_gate.0.lock().unwrap() = None;
        }) as Box<dyn FnOnce() + Send>);
        if read {
            store.0.lock().unwrap().after_read = hook;
            assert_eq!(
                service.view(&source.session_id).unwrap(),
                StatusView::SignedOut
            );
        } else {
            store.0.lock().unwrap().after_save = hook;
            assert_eq!(
                service
                    .complete(&ticket, ticket.request_id(), text())
                    .unwrap(),
                ApplyResult::Ignored
            );
            assert_eq!(
                service.view(&source.session_id).unwrap(),
                StatusView::SignedOut
            );
        }
    }
}

#[test]
fn failed_store_writes_do_not_claim_consent_or_generation_success() {
    let (mut service, store, _gate, source) = setup();
    store.0.lock().unwrap().fail_write = true;
    assert!(matches!(
        service.enable(ConsentRevision(0), &binding(), folders(&source, false)),
        Err(StatusError::Store(StatusStoreError::Database))
    ));
    assert_eq!(store.0.lock().unwrap().consent, ConsentSnapshot::default());
    store.0.lock().unwrap().fail_write = false;
    enabled(&mut service, &source, false);
    let ticket = ticket(&mut service, &source);
    store.0.lock().unwrap().fail_write = true;
    assert!(matches!(
        service.complete(&ticket, ticket.request_id(), text()),
        Err(StatusError::Store(StatusStoreError::Database))
    ));
    assert!(store.0.lock().unwrap().cache.is_empty());
}

#[test]
fn expired_during_consent_write_clears_only_that_new_consent_revision() {
    let (mut service, store, gate, source) = setup();
    let changed = gate.clone();
    store.0.lock().unwrap().after_consent =
        Some(Box::new(move || *changed.0.lock().unwrap() = None));
    assert_eq!(
        service
            .enable(ConsentRevision(0), &binding(), folders(&source, false))
            .unwrap_err(),
        StatusError::SignedOut
    );
    assert_eq!(service.consent().unwrap().policy, ConsentPolicy::Disabled);
}

#[test]
fn a_corrupt_cache_key_cannot_display_another_sessions_text() {
    let (mut service, store, _gate, source) = setup();
    enabled(&mut service, &source, true);
    let ticket = ticket(&mut service, &source);
    service
        .complete(&ticket, ticket.request_id(), text())
        .unwrap();
    store
        .0
        .lock()
        .unwrap()
        .cache
        .get_mut(&source.session_id)
        .unwrap()
        .session_id = SessionId::generate();
    assert!(matches!(
        service.view(&source.session_id),
        Err(StatusError::Store(StatusStoreError::InvalidData))
    ));
}

#[test]
fn removed_sources_and_tickets_never_resurrect_after_reobservation() {
    let (mut service, store, _gate, mut source) = setup();
    enabled(&mut service, &source, true);
    let ticket = ticket(&mut service, &source);
    service.remove(&source.session_id).unwrap();
    assert_eq!(
        service.observe(source.clone()).unwrap(),
        ApplyResult::Ignored
    );
    source.stamp.source_revision += 1;
    assert_eq!(
        service.observe(source.clone()).unwrap(),
        ApplyResult::Ignored
    );
    assert!(!service.is_current(&ticket).unwrap());
    assert_eq!(
        service
            .complete(&ticket, ticket.request_id(), text())
            .unwrap(),
        ApplyResult::Ignored
    );
    assert_eq!(
        service.view(&source.session_id).unwrap(),
        StatusView::Missing
    );
    assert!(store.0.lock().unwrap().cache.is_empty());
}

#[test]
fn retirement_preserves_newer_private_fences_and_removed_first_event_ordering() {
    let (mut service, _store, _gate, mut current) = setup();
    enabled(&mut service, &current, true);
    let older_public = current.clone();
    current.private = true;
    current.stamp.source_revision += 1;
    service.observe(current.clone()).unwrap();
    service.remove(&current.session_id).unwrap();
    assert_eq!(service.observe(older_public).unwrap(), ApplyResult::Ignored);
    let unknown = source();
    service.remove(&unknown.session_id).unwrap();
    assert_eq!(service.observe(unknown).unwrap(), ApplyResult::Ignored);
}

#[test]
fn retired_metadata_is_bounded_and_never_evicted_to_accept_delayed_sources() {
    let (mut service, _store, _gate, _source) = setup();
    let mut retired = None;
    for _ in 0..MAX_CACHE_ENTRIES {
        let source = source();
        service.remove(&source.session_id).unwrap();
        retired = Some(source);
    }
    assert_eq!(
        service.observe(source()).unwrap_err(),
        StatusError::Capacity
    );
    assert_eq!(
        service.remove(&SessionId::generate()).unwrap_err(),
        StatusError::Capacity
    );
    assert_eq!(
        service.observe(retired.unwrap()).unwrap(),
        ApplyResult::Ignored
    );
}

#[test]
fn failed_cache_deletion_does_not_undo_source_retirement_or_expose_cached_text() {
    let (mut service, store, _gate, mut source) = setup();
    enabled(&mut service, &source, true);
    let completed = ticket(&mut service, &source);
    service
        .complete(&completed, completed.request_id(), text())
        .unwrap();
    source.stamp.source_revision += 1;
    service.observe(source.clone()).unwrap();
    let pending = ticket(&mut service, &source);
    store.0.lock().unwrap().fail_remove = true;
    assert!(matches!(
        service.remove(&source.session_id),
        Err(StatusError::Store(StatusStoreError::Database))
    ));
    assert!(
        !store.0.lock().unwrap().cache.is_empty(),
        "simulated failed deletion retained the row"
    );
    assert_eq!(
        service.observe(source.clone()).unwrap(),
        ApplyResult::Ignored
    );
    assert!(!service.is_current(&pending).unwrap());
    assert_eq!(
        service.view(&source.session_id).unwrap(),
        StatusView::Missing
    );
    assert_eq!(
        service
            .complete(&pending, pending.request_id(), text())
            .unwrap(),
        ApplyResult::Ignored
    );
}

#[test]
fn retiring_two_jobs_does_not_release_slots_before_worker_acknowledgement() {
    let (mut service, _store, _gate, first) = setup();
    enabled(&mut service, &first, true);
    let first_ticket = ticket(&mut service, &first);
    let second = source();
    service.observe(second.clone()).unwrap();
    let second_ticket = ticket(&mut service, &second);
    service.remove(&first.session_id).unwrap();
    service.remove(&second.session_id).unwrap();
    let third = source();
    service.observe(third.clone()).unwrap();
    assert!(matches!(
        service.prepare(&third.session_id).unwrap(),
        Preparation::Skipped(StatusView::Pending)
    ));
    service.abandon(&first_ticket);
    let third_ticket = ticket(&mut service, &third);
    assert_eq!(
        service
            .complete(&second_ticket, second_ticket.request_id(), text())
            .unwrap(),
        ApplyResult::Ignored
    );
    assert!(service.is_current(&third_ticket).unwrap());
}

#[test]
fn retiring_an_active_or_already_retired_id_succeeds_at_metadata_capacity() {
    let (mut service, _store, _gate, _source) = setup();
    let mut last = None;
    for _ in 0..MAX_CACHE_ENTRIES {
        let current = source();
        service.observe(current.clone()).unwrap();
        last = Some(current.session_id);
    }
    let id = last.unwrap();
    service.remove(&id).unwrap();
    service.remove(&id).unwrap();
    assert_eq!(
        service.observe(source()).unwrap_err(),
        StatusError::Capacity
    );
}

#[test]
fn stale_account_intents_cannot_enable_sharing_for_the_new_current_account() {
    for field in ["subject", "revision", "issuer", "client"] {
        let (mut service, store, gate, source) = setup();
        let approved = binding();
        let mut changed = approved.clone();
        match field {
            "subject" => changed.account.subject = "new-account".into(),
            "revision" => changed.auth_revision += 1,
            "issuer" => changed.account.issuer = "https://other-issuer.example/".into(),
            _ => changed.account.client_id = "other-native-client".into(),
        }
        *gate.0.lock().unwrap() = Some(changed);
        assert_eq!(
            service
                .enable(ConsentRevision(0), &approved, folders(&source, true))
                .unwrap_err(),
            StatusError::AccountChanged
        );
        assert_eq!(store.0.lock().unwrap().consent, ConsentSnapshot::default());
    }
}

#[test]
fn consent_cleanup_after_gate_error_preserves_error_and_disables_its_write() {
    let (mut service, store, gate, source) = setup();
    let failed = gate.clone();
    store.0.lock().unwrap().after_consent =
        Some(Box::new(move || failed.1.store(true, Ordering::SeqCst)));
    assert_eq!(
        service
            .enable(ConsentRevision(0), &binding(), folders(&source, true))
            .unwrap_err(),
        StatusError::AccountUnavailable
    );
    assert_eq!(service.consent().unwrap().policy, ConsentPolicy::Disabled);
    assert_eq!(service.consent().unwrap().revision, ConsentRevision(2));
}

#[test]
fn consent_cleanup_after_account_change_reports_change_and_disables_its_write() {
    let (mut service, store, gate, source) = setup();
    let changed = gate.clone();
    store.0.lock().unwrap().after_consent = Some(Box::new(move || {
        changed.0.lock().unwrap().as_mut().unwrap().auth_revision += 1
    }));
    assert_eq!(
        service
            .enable(ConsentRevision(0), &binding(), folders(&source, true))
            .unwrap_err(),
        StatusError::AccountChanged
    );
    assert_eq!(service.consent().unwrap().policy, ConsentPolicy::Disabled);
}

#[test]
fn consent_cleanup_cannot_overwrite_a_newer_successful_account_decision() {
    let (mut service, store, gate, source) = setup();
    let mut replacement = binding();
    replacement.auth_revision += 1;
    replacement.account.subject = "new-account".into();
    let expected = ConsentPolicy::Enabled {
        account: replacement.account.clone(),
        auth_revision: replacement.auth_revision,
        folders: folders(&source, false),
    };
    let saved_policy = expected.clone();
    let mut concurrent = store.clone();
    store.0.lock().unwrap().after_consent = Some(Box::new(move || {
        *gate.0.lock().unwrap() = Some(replacement);
        concurrent
            .compare_and_swap_consent(ConsentRevision(1), &saved_policy)
            .unwrap();
    }));
    assert_eq!(
        service
            .enable(ConsentRevision(0), &binding(), folders(&source, true))
            .unwrap_err(),
        StatusError::AccountChanged
    );
    assert_eq!(
        service.consent().unwrap(),
        ConsentSnapshot {
            revision: ConsentRevision(2),
            policy: expected
        }
    );
}

#[cfg(unix)]
#[test]
fn sqlite_integration_requires_fresh_source_and_preserves_optout_across_owner_restart() {
    use crate::status::SqliteStatusStore;
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("private-status");
    let source = source();
    let gate = Gate(
        Arc::new(Mutex::new(Some(binding()))),
        Arc::new(AtomicBool::new(false)),
    );
    let store = SqliteStatusStore::open(&path).unwrap();
    let mut first = AISessionStatusService::with_clock(store, gate.clone(), Clock);
    first.observe(source.clone()).unwrap();
    first
        .enable(ConsentRevision(0), &binding(), folders(&source, false))
        .unwrap();
    let Preparation::Ready(old_ticket) = first.prepare(&source.session_id).unwrap() else {
        panic!("missing job");
    };
    assert_eq!(
        first
            .complete(&old_ticket, old_ticket.request_id(), text())
            .unwrap(),
        ApplyResult::Applied
    );
    assert!(matches!(
        SqliteStatusStore::open(&path),
        Err(StatusStoreError::Busy)
    ));
    drop(first);

    let store = SqliteStatusStore::open(&path).unwrap();
    let mut reopened = AISessionStatusService::with_clock(store, gate.clone(), Clock);
    assert_eq!(
        reopened.view(&source.session_id).unwrap(),
        StatusView::Missing
    );
    reopened.observe(source.clone()).unwrap();
    assert!(matches!(
        reopened.view(&source.session_id).unwrap(),
        StatusView::Ready(_)
    ));
    assert_eq!(
        reopened
            .complete(&old_ticket, old_ticket.request_id(), text())
            .unwrap(),
        ApplyResult::Ignored
    );
    reopened.disable(ConsentRevision(1)).unwrap();
    assert!(matches!(
        reopened.enable(ConsentRevision(1), &binding(), folders(&source, false)),
        Err(StatusError::Store(StatusStoreError::Conflict))
    ));
    drop(reopened);

    let store = SqliteStatusStore::open(&path).unwrap();
    let mut final_owner = AISessionStatusService::with_clock(store, gate, Clock);
    final_owner.observe(source.clone()).unwrap();
    assert_eq!(
        final_owner.view(&source.session_id).unwrap(),
        StatusView::Disabled
    );
    assert_eq!(final_owner.consent().unwrap().revision, ConsentRevision(2));
}
