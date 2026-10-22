//! Synthetic integration tests: real account persistence, no network or session capture.
#![cfg(unix)]

use std::collections::HashMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::sync::atomic::AtomicU64;
use std::sync::{mpsc, Mutex};
use std::thread;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};
use session_core::activity::ActivityPhase;
use session_core::ids::SessionId;
use session_core::model::{Lifecycle, ScopeRoot};

use super::super::*;
use crate::auth::tests::{clock_origin, config as account_config, verified};
use crate::auth::{
    AccountError, AccountService, AccountSession, ApplyResult, CredentialStore,
    CredentialStoreError, FileCredentialStore, WallClock,
};
use crate::cloud::{CaptureInput, CaptureMetadata};
use crate::dispatch::{
    DispatchError, DispatchPreparation, PreparedStatusDispatch, StatusCaptureJob,
    StatusDispatchJob, StatusDispatchService,
};
use crate::status::{
    CachedStatus, ConsentPolicy, ConsentRevision, ConsentSnapshot, ContextAvailability,
    FolderConsent, InputSelection, SourceStamp, StatusError, StatusSource, StatusStore,
    StatusStoreError, StatusView, MAX_CACHE_ENTRIES,
};

const ORIGIN: &str = "https://ai.example:8443";
const SUBJECT: &str = "synthetic-dispatch-subject";
const ACCESS_TOKEN: &str = "PRIVATE-ACCESS-TOKEN";
const REPORT: &str = "Checking a synthetic dispatch example";
const OUTPUT: &str = "Synthetic dispatch checks passed";
const STATUS: &str = "Checking synthetic dispatch safety";
const FOLDER: &str = "/synthetic/dispatch-private-folder";

#[derive(Clone, Default)]
struct FakeClock(Arc<AtomicU64>);

impl WallClock for FakeClock {
    fn now(&self) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(1000 + self.0.load(Ordering::SeqCst))
    }

    fn monotonic_now(&self) -> Instant {
        clock_origin() + Duration::from_secs(self.0.load(Ordering::SeqCst))
    }
}

#[derive(Default)]
struct StoreState {
    consent: ConsentSnapshot,
    cache: HashMap<SessionId, CachedStatus>,
    fail_load: bool,
    fail_write: bool,
    fail_remove: bool,
    cache_reads: usize,
    consent_writes: usize,
}

#[derive(Clone, Default)]
struct MemoryStatusStore(Arc<Mutex<StoreState>>);

impl StatusStore for MemoryStatusStore {
    fn load_consent(&mut self) -> Result<ConsentSnapshot, StatusStoreError> {
        let state = self.0.lock().unwrap();
        if state.fail_load {
            return Err(StatusStoreError::Database);
        }
        state.consent.validate()?;
        Ok(state.consent.clone())
    }

    fn compare_and_swap_consent(
        &mut self,
        expected: ConsentRevision,
        policy: &ConsentPolicy,
    ) -> Result<ConsentSnapshot, StatusStoreError> {
        let mut state = self.0.lock().unwrap();
        if state.fail_write {
            return Err(StatusStoreError::Database);
        }
        if state.consent.revision != expected {
            return Err(StatusStoreError::Conflict);
        }
        policy.validate()?;
        let revision = expected
            .0
            .checked_add(1)
            .ok_or(StatusStoreError::RevisionExhausted)?;
        state.consent = ConsentSnapshot {
            revision: ConsentRevision(revision),
            policy: policy.clone(),
        };
        state.cache.clear();
        state.consent_writes += 1;
        Ok(state.consent.clone())
    }

    fn load_cache(
        &mut self,
        session: &SessionId,
    ) -> Result<Option<CachedStatus>, StatusStoreError> {
        let mut state = self.0.lock().unwrap();
        state.cache_reads += 1;
        let entry = state.cache.get(session).cloned();
        if let Some(entry) = &entry {
            entry.validate()?;
        }
        Ok(entry)
    }

    fn save_cache(
        &mut self,
        expected: ConsentRevision,
        entry: &CachedStatus,
    ) -> Result<(), StatusStoreError> {
        entry.validate()?;
        let mut state = self.0.lock().unwrap();
        if state.consent.revision != expected || !state.consent.policy.permits_cache(entry) {
            return Err(StatusStoreError::Conflict);
        }
        if !state.cache.contains_key(&entry.session_id) && state.cache.len() >= MAX_CACHE_ENTRIES {
            let oldest = state
                .cache
                .values()
                .min_by_key(|entry| entry.captured_at)
                .unwrap()
                .session_id
                .clone();
            state.cache.remove(&oldest);
        }
        state.cache.insert(entry.session_id.clone(), entry.clone());
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

struct RecordedRequest {
    request: Request,
    body: Vec<u8>,
    streaming: bool,
    cloneable: bool,
}

type Requests = Arc<Mutex<Vec<RecordedRequest>>>;

enum ReplyMode {
    Success,
    /// A canonical service error, with an optional suggested delay in seconds.
    Remote(u16, crate::cloud::RemoteErrorCode, Option<u16>),
    BlockedBody {
        entered: mpsc::Sender<()>,
        release: mpsc::Receiver<()>,
        fail: bool,
    },
}

struct FakeTransport {
    requests: Requests,
    mode: Mutex<Option<ReplyMode>>,
}

impl HttpTransport for FakeTransport {
    fn send(&self, mut request: Request) -> Result<HttpReply, CloudAIError> {
        let streaming = request.body().unwrap().as_bytes().is_none();
        let cloneable = request.try_clone().is_some();
        // reqwest 0.11's blocking Body is not Read. Buffer only the detached
        // synthetic reader; never make the production request replayable.
        let mut detached = request.body_mut().take().unwrap();
        let body = detached.buffer().unwrap().to_vec();
        let value: Value = serde_json::from_slice(&body).unwrap();
        let response = serde_json::to_vec(&json!({
            "version": 1,
            "request_id": value["request_id"],
            "status_text": STATUS,
        }))
        .unwrap();
        self.requests.lock().unwrap().push(RecordedRequest {
            request,
            body,
            streaming,
            cloneable,
        });
        let body: Box<dyn Read + Send> = match self.mode.lock().unwrap().take() {
            Some(ReplyMode::Remote(status, code, delay)) => {
                return Ok(super::error_reply(status, code, delay))
            }
            Some(ReplyMode::BlockedBody {
                entered,
                release,
                fail,
            }) => Box::new(BlockedBody {
                entered: Some(entered),
                release,
                fail,
                bytes: Cursor::new(response),
            }),
            Some(ReplyMode::Success) | None => Box::new(Cursor::new(response)),
        };
        Ok(HttpReply {
            status: 200,
            headers: HeaderMap::from_iter([(
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/json"),
            )]),
            body,
        })
    }
}

struct BlockedBody {
    entered: Option<mpsc::Sender<()>>,
    release: mpsc::Receiver<()>,
    fail: bool,
    bytes: Cursor<Vec<u8>>,
}

impl Read for BlockedBody {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if let Some(entered) = self.entered.take() {
            entered.send(()).unwrap();
            self.release.recv().unwrap();
        }
        if self.fail {
            Err(io::Error::other("synthetic-private-response-diagnostic"))
        } else {
            self.bytes.read(buffer)
        }
    }
}

fn cloud(origin: &str, requests: Requests, mode: ReplyMode) -> CloudAIClient {
    CloudAIClient::with_transport(
        CloudAIConfig::new(origin).unwrap(),
        Box::new(FakeTransport {
            requests,
            mode: Mutex::new(Some(mode)),
        }),
    )
}

type Owner = StatusDispatchService<MemoryStatusStore, FileCredentialStore, FakeClock>;
type Worker = StatusDispatchJob<FileCredentialStore, FakeClock>;

struct Fixture {
    directory: tempfile::TempDir,
    account: Arc<AccountService<FileCredentialStore, FakeClock>>,
    credentials: FileCredentialStore,
    session: AccountSession,
    clock: FakeClock,
    store: MemoryStatusStore,
    requests: Requests,
    owner: Owner,
}

impl Fixture {
    fn new(mode: ReplyMode) -> Self {
        let directory = tempfile::Builder::new()
            .prefix("synthetic-dispatch-")
            .tempdir()
            .unwrap();
        let credentials = FileCredentialStore::new(directory.path().join("private-auth"));
        let clock = FakeClock::default();
        let account = Arc::new(AccountService::with_clock(
            account_config(),
            credentials.clone(),
            clock.clone(),
        ));
        let session = login(&account, SUBJECT);
        let store = MemoryStatusStore::default();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let mut owner = StatusDispatchService::new(
            account.clone(),
            store.clone(),
            cloud(ORIGIN, requests.clone(), mode),
        );
        owner.set_account_session(Some(session.clone())).unwrap();
        owner.observe(source(1)).unwrap();
        Self {
            directory,
            account,
            credentials,
            session,
            clock,
            store,
            requests,
            owner,
        }
    }

    fn enabled(include_output: bool) -> Self {
        let mut fixture = Self::new(ReplyMode::Success);
        fixture.enable(include_output);
        fixture
    }

    fn enable(&mut self, include_output: bool) {
        let offer = self.owner.consent_offer().unwrap();
        self.owner.enable(&offer, folders(include_output)).unwrap();
    }

    fn worker(&mut self, number: u8) -> (Box<StatusCaptureJob>, Worker) {
        let capture = capture(&mut self.owner, number);
        let prepared = build(&mut self.owner, &capture);
        let worker = self.owner.commit(prepared).unwrap();
        (capture, worker)
    }

    fn calls(&self) -> usize {
        self.requests.lock().unwrap().len()
    }

    fn assert_no_cache(&self) {
        assert!(self.store.0.lock().unwrap().cache.is_empty());
    }
}

fn login(
    account: &AccountService<FileCredentialStore, FakeClock>,
    subject: &str,
) -> AccountSession {
    let ticket = account.begin_login().unwrap();
    account.commit_login(&ticket, verified(subject)).unwrap()
}

fn session(number: u8) -> SessionId {
    SessionId::from_string(format!("synthetic-local-session-{number}"))
}

fn folder() -> ScopeRoot {
    ScopeRoot::from_canonical(FOLDER.into())
}

fn folders(include_output: bool) -> Vec<FolderConsent> {
    vec![FolderConsent {
        folder: folder(),
        include_terminal_output: include_output,
    }]
}

fn source(number: u8) -> StatusSource {
    StatusSource {
        session_id: session(number),
        folder: folder(),
        stamp: SourceStamp {
            terminal_generation: 731,
            source_revision: 953,
        },
        private: false,
        live: true,
        context: ContextAvailability {
            reported_text: true,
            recent_output: true,
        },
    }
}

fn metadata() -> CaptureMetadata {
    CaptureMetadata {
        lifecycle: Lifecycle::Working,
        activity_phase: Some(ActivityPhase::RunningTool),
    }
}

fn capture(owner: &mut Owner, number: u8) -> Box<StatusCaptureJob> {
    match owner.prepare(&session(number)).unwrap() {
        DispatchPreparation::Ready(capture) => capture,
        other => panic!("expected a synthetic capture, got {other:?}"),
    }
}

fn assert_skipped(owner: &mut Owner, number: u8, expected: StatusView) {
    match owner.prepare(&session(number)).unwrap() {
        DispatchPreparation::Skipped(view) => assert_eq!(view, expected),
        other => panic!("unexpected synthetic capture: {other:?}"),
    }
}

fn build(owner: &mut Owner, capture: &StatusCaptureJob) -> PreparedStatusDispatch {
    owner
        .build(
            capture,
            capture.source_stamp(),
            metadata(),
            CaptureInput {
                reported_text: Some(REPORT),
                recent_output: None,
            },
            &[],
        )
        .unwrap()
}

fn assert_redacted(diagnostic: &str) {
    assert!(diagnostic.contains("REDACTED"));
    for private in [
        SUBJECT,
        ACCESS_TOKEN,
        "PRIVATE-REFRESH-TOKEN",
        "NOT-AN-ACCESS-TOKEN",
        "person@example.com",
        FOLDER,
        "synthetic-local-session-1",
        REPORT,
        OUTPUT,
        STATUS,
        "ai.example",
    ] {
        assert!(!diagnostic.contains(private), "leaked {private}");
    }
}

#[test]
fn sign_in_and_consent_offers_do_not_authorize_capture_or_http() {
    let mut fixture = Fixture::new(ReplyMode::Success);
    let mut unsigned = StatusDispatchService::new(
        fixture.account.clone(),
        fixture.store.clone(),
        cloud(ORIGIN, fixture.requests.clone(), ReplyMode::Success),
    );
    assert_eq!(
        unsigned.consent_offer().unwrap_err(),
        DispatchError::Policy(StatusError::SignedOut)
    );
    unsigned
        .set_account_session(Some(fixture.session.clone()))
        .unwrap();
    assert_skipped(&mut unsigned, 1, StatusView::Disabled);
    let offer = fixture.owner.consent_offer().unwrap();
    assert_eq!(offer.origin(), ORIGIN);
    assert_eq!(offer.account().account.subject, SUBJECT);
    assert_eq!(
        offer.account().auth_revision,
        fixture.session.revision().value()
    );
    assert_eq!(offer.consent_revision(), ConsentRevision(0));
    assert_skipped(&mut fixture.owner, 1, StatusView::Disabled);
    assert_eq!(fixture.store.0.lock().unwrap().consent_writes, 0);
    assert_eq!(fixture.calls(), 0);
}

#[test]
fn stale_consent_and_account_offers_cannot_retarget_sharing() {
    let mut fixture = Fixture::new(ReplyMode::Success);
    let stale_consent = fixture.owner.consent_offer().unwrap();
    fixture.enable(false);
    let saved = fixture.owner.consent().unwrap();
    assert_eq!(
        fixture
            .owner
            .enable(&stale_consent, folders(true))
            .unwrap_err(),
        DispatchError::Policy(StatusError::Store(StatusStoreError::Conflict))
    );
    assert_eq!(fixture.owner.consent().unwrap(), saved);
    assert_skipped(&mut fixture.owner, 1, StatusView::ConsentRequired);

    let stale_account = fixture.owner.consent_offer().unwrap();
    let replacement = login(&fixture.account, "synthetic-replacement-subject");
    fixture
        .owner
        .set_account_session(Some(replacement))
        .unwrap();
    assert_eq!(
        fixture
            .owner
            .enable(&stale_account, folders(true))
            .unwrap_err(),
        DispatchError::Policy(StatusError::AccountChanged)
    );
    assert_eq!(fixture.owner.consent().unwrap(), saved);
    assert_skipped(&mut fixture.owner, 1, StatusView::ConsentRequired);
    assert_eq!(fixture.calls(), 0);
}

#[test]
fn foreign_owner_offer_is_rejected_before_mutation_even_for_the_same_origin() {
    let mut fixture = Fixture::enabled(false);
    let capture = capture(&mut fixture.owner, 1);
    let prepared = build(&mut fixture.owner, &capture);
    let saved = fixture.owner.consent().unwrap();
    for origin in [ORIGIN, "https://ai.example:9443"] {
        let mut other = StatusDispatchService::new(
            fixture.account.clone(),
            fixture.store.clone(),
            cloud(origin, fixture.requests.clone(), ReplyMode::Success),
        );
        other
            .set_account_session(Some(fixture.session.clone()))
            .unwrap();
        let foreign = other.consent_offer().unwrap();
        assert_eq!(foreign.origin(), origin);
        assert_eq!(
            fixture.owner.enable(&foreign, folders(true)).unwrap_err(),
            DispatchError::ForeignOwner
        );
        assert!(!capture.is_canceled());
        assert_eq!(fixture.owner.consent().unwrap(), saved);
    }
    let completion = fixture.owner.commit(prepared).unwrap().run();
    assert_eq!(
        fixture.owner.finish(completion).unwrap(),
        ApplyResult::Applied
    );
    assert_eq!(fixture.calls(), 1);
    assert_eq!(
        fixture.requests.lock().unwrap()[0].request.url().port(),
        Some(8443)
    );
}

#[test]
fn every_new_owner_requires_fresh_confirmation_even_with_matching_persisted_cache() {
    for origin in [ORIGIN, "https://another-ai.example:9443"] {
        let mut fixture = Fixture::enabled(false);
        let (_, worker) = fixture.worker(1);
        assert_eq!(
            fixture.owner.finish(worker.run()).unwrap(),
            ApplyResult::Applied
        );
        let saved = fixture.owner.consent().unwrap();
        let cache_reads = fixture.store.0.lock().unwrap().cache_reads;
        let mut replacement = StatusDispatchService::new(
            fixture.account.clone(),
            fixture.store.clone(),
            cloud(origin, fixture.requests.clone(), ReplyMode::Success),
        );
        replacement.observe(source(1)).unwrap();
        assert_eq!(
            replacement.view(&session(1)).unwrap(),
            StatusView::SignedOut
        );
        replacement
            .set_account_session(Some(fixture.session.clone()))
            .unwrap();
        assert_eq!(replacement.consent().unwrap(), saved);
        assert_eq!(
            replacement.view(&session(1)).unwrap(),
            StatusView::ConsentRequired
        );
        assert_skipped(&mut replacement, 1, StatusView::ConsentRequired);
        assert_eq!(fixture.store.0.lock().unwrap().cache_reads, cache_reads);
        assert_eq!(fixture.store.0.lock().unwrap().cache.len(), 1);
        let offer = replacement.consent_offer().unwrap();
        assert_eq!(offer.origin(), origin);
        replacement.enable(&offer, folders(false)).unwrap();
        fixture.assert_no_cache();
        let capture = capture(&mut replacement, 1);
        replacement.acknowledge_capture(&capture).unwrap();
        assert_eq!(fixture.calls(), 1);
    }
}

#[test]
fn reports_only_capture_rejects_terminal_output_including_an_empty_window() {
    let mut fixture = Fixture::enabled(false);
    for output in ["", OUTPUT] {
        let capture = capture(&mut fixture.owner, 1);
        assert_eq!(capture.session_id(), &session(1));
        assert_eq!(capture.folder(), &folder());
        assert_eq!(capture.source_stamp(), source(1).stamp);
        assert_eq!(capture.input_selection(), InputSelection::ReportOnly);
        assert_eq!(
            fixture
                .owner
                .build(
                    &capture,
                    capture.source_stamp(),
                    metadata(),
                    CaptureInput {
                        reported_text: Some(REPORT),
                        recent_output: Some(output)
                    },
                    &[],
                )
                .unwrap_err(),
            DispatchError::Cloud(CloudAIError::DataNotPermitted)
        );
        assert!(capture.is_canceled());
        assert_eq!(
            fixture.owner.acknowledge_capture(&capture).unwrap(),
            ApplyResult::Applied
        );
    }
    let mut output_only = source(2);
    output_only.context.reported_text = false;
    fixture.owner.observe(output_only).unwrap();
    assert_skipped(&mut fixture.owner, 2, StatusView::NoContext);
    let mut excluded = source(3);
    excluded.folder = ScopeRoot::from_canonical("/synthetic/never-shared-folder".into());
    fixture.owner.observe(excluded).unwrap();
    assert_skipped(&mut fixture.owner, 3, StatusView::Excluded);
    assert_eq!(fixture.calls(), 0);
    fixture.assert_no_cache();
}

#[test]
fn prepare_and_build_are_token_free_and_make_no_http_or_persistent_mutation() {
    let mut fixture = Fixture::enabled(false);
    let credential_path = fixture
        .directory
        .path()
        .join("private-auth/credentials.json");
    let credentials_before = fs::read(&credential_path).unwrap();
    let consent_before = fixture.owner.consent().unwrap();
    let capture = capture(&mut fixture.owner, 1);
    let prepared = build(&mut fixture.owner, &capture);
    assert_redacted(&format!("{capture:?} {prepared:?}"));
    assert_eq!(fs::read(credential_path).unwrap(), credentials_before);
    assert_eq!(fixture.owner.consent().unwrap(), consent_before);
    assert_eq!(fixture.store.0.lock().unwrap().consent_writes, 1);
    assert_eq!(fixture.calls(), 0);
    fixture.assert_no_cache();
    assert_eq!(
        fixture.owner.acknowledge_capture(&capture).unwrap(),
        ApplyResult::Applied
    );
    assert_eq!(
        fixture.owner.commit(prepared).unwrap_err(),
        DispatchError::Canceled
    );
}

#[test]
fn happy_path_sends_only_selected_redacted_context_and_caches_only_after_finish() {
    let mut fixture = Fixture::enabled(true);
    let capture = capture(&mut fixture.owner, 1);
    assert_eq!(capture.input_selection(), InputSelection::ReportAndOutput);
    let secret = SecretValue::try_from("synthetic-context-secret".to_owned()).unwrap();
    let report = format!("{REPORT} at {FOLDER}/source.rs with synthetic-context-secret");
    let prepared = fixture
        .owner
        .build(
            &capture,
            capture.source_stamp(),
            metadata(),
            CaptureInput {
                reported_text: Some(&report),
                recent_output: Some(OUTPUT),
            },
            &[&secret],
        )
        .unwrap();
    let worker = fixture.owner.commit(prepared).unwrap();
    assert_eq!(fixture.calls(), 0);
    let completion = thread::spawn(move || worker.run()).join().unwrap();
    fixture.assert_no_cache();
    assert_eq!(
        fixture.owner.view(&session(1)).unwrap(),
        StatusView::Pending
    );
    let request_id = {
        let requests = fixture.requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        let recorded = &requests[0];
        assert_eq!(recorded.request.method(), Method::POST);
        assert_eq!(
            recorded.request.url().as_str(),
            format!("{ORIGIN}{STATUS_PATH}")
        );
        assert_eq!(recorded.request.headers().len(), 5);
        assert_eq!(
            recorded.request.headers()[header::AUTHORIZATION],
            format!("Bearer {ACCESS_TOKEN}")
        );
        assert!(recorded.request.headers()[header::AUTHORIZATION].is_sensitive());
        assert_eq!(
            recorded.request.headers()[header::CONTENT_TYPE],
            "application/json"
        );
        assert_eq!(
            recorded.request.headers()[header::CONTENT_LENGTH],
            recorded.body.len().to_string()
        );
        assert!(recorded.streaming);
        assert!(!recorded.cloneable);
        let value: Value = serde_json::from_slice(&recorded.body).unwrap();
        assert_eq!(value.as_object().unwrap().len(), 3);
        assert_eq!(value["version"], 1);
        assert_eq!(
            value["context"],
            json!({
                "lifecycle": "working",
                "activity_phase": "running_tool",
                "reported_text": format!("{REPORT} at [PATH] with [REDACTED]"),
                "recent_output": OUTPUT,
            })
        );
        let body = String::from_utf8_lossy(&recorded.body);
        for private in [
            SUBJECT,
            ACCESS_TOKEN,
            "PRIVATE-REFRESH-TOKEN",
            "NOT-AN-ACCESS-TOKEN",
            FOLDER,
            "synthetic-context-secret",
            "synthetic-local-session-1",
        ] {
            assert!(!body.contains(private));
        }
        for field in [
            "account",
            "account_id",
            "session_id",
            "folder",
            "source_stamp",
            "terminal_generation",
            "source_revision",
            "auth_revision",
            "consent_revision",
            "provider_url",
            "model",
            "tools",
            "messages",
        ] {
            assert!(value.get(field).is_none());
            assert!(value["context"].get(field).is_none());
        }
        value["request_id"].as_str().unwrap().to_owned()
    };
    assert_eq!(
        fixture.owner.finish(completion).unwrap(),
        ApplyResult::Applied
    );
    let StatusView::Ready(cached) = fixture.owner.view(&session(1)).unwrap() else {
        panic!("expected current synthetic cache");
    };
    assert_eq!(cached.text.as_str(), STATUS);
    assert_eq!(cached.request_id, request_id);
    assert_ne!(cached.request_id, session(1).as_str());
    assert_eq!(cached.account.subject, SUBJECT);
    assert_eq!(cached.auth_revision, fixture.session.revision().value());
    assert_eq!(
        cached.consent_revision,
        fixture.owner.consent().unwrap().revision
    );
    assert_eq!(cached.folder, folder());
    assert_eq!(cached.stamp, source(1).stamp);
    assert_skipped(&mut fixture.owner, 1, StatusView::Ready(cached));
}

#[test]
fn reports_only_wire_body_omits_output_and_local_bindings() {
    let mut fixture = Fixture::enabled(false);
    let (_, worker) = fixture.worker(1);
    assert_eq!(
        fixture.owner.finish(worker.run()).unwrap(),
        ApplyResult::Applied
    );
    let requests = fixture.requests.lock().unwrap();
    let value: Value = serde_json::from_slice(&requests[0].body).unwrap();
    assert_eq!(
        value["context"],
        json!({
            "lifecycle": "working", "activity_phase": "running_tool", "reported_text": REPORT,
        })
    );
    assert_eq!(value.as_object().unwrap().len(), 3);
}

#[test]
fn foreign_capture_prepared_and_completion_do_not_change_receivers_policy_or_slots() {
    let mut issuer = Fixture::enabled(false);
    let mut receiver = Fixture::enabled(false);
    let own = capture(&mut receiver.owner, 1);
    let saved = receiver.owner.consent().unwrap();
    let foreign = capture(&mut issuer.owner, 1);
    assert_eq!(
        receiver
            .owner
            .build(
                &foreign,
                foreign.source_stamp(),
                metadata(),
                CaptureInput {
                    reported_text: Some(REPORT),
                    recent_output: None
                },
                &[]
            )
            .unwrap_err(),
        DispatchError::ForeignOwner
    );
    assert_eq!(
        receiver.owner.acknowledge_capture(&foreign).unwrap_err(),
        DispatchError::ForeignOwner
    );
    let prepared = build(&mut issuer.owner, &foreign);
    assert_eq!(
        receiver.owner.commit(prepared).unwrap_err(),
        DispatchError::ForeignOwner
    );
    issuer.owner.acknowledge_capture(&foreign).unwrap();
    let (_, worker) = issuer.worker(1);
    assert_eq!(
        receiver.owner.finish(worker.run()).unwrap_err(),
        DispatchError::ForeignOwner
    );
    assert!(!own.is_canceled());
    assert_eq!(receiver.owner.consent().unwrap(), saved);
    assert_eq!(receiver.calls(), 0);
    receiver.assert_no_cache();
    receiver.owner.observe(source(2)).unwrap();
    let second = capture(&mut receiver.owner, 2);
    let prepared = build(&mut receiver.owner, &own);
    let completion = receiver.owner.commit(prepared).unwrap().run();
    assert_eq!(
        receiver.owner.finish(completion).unwrap(),
        ApplyResult::Applied
    );
    receiver.owner.acknowledge_capture(&second).unwrap();
}

#[test]
fn revocation_before_commit_suppresses_dispatch_and_requires_capture_acknowledgement() {
    for control in 0..3 {
        let mut fixture = Fixture::enabled(false);
        let capture = capture(&mut fixture.owner, 1);
        let prepared = build(&mut fixture.owner, &capture);
        match control {
            0 => {
                let revision = fixture.owner.consent().unwrap().revision;
                fixture.owner.disable(revision).unwrap();
            }
            1 => fixture.owner.sign_out().unwrap(),
            _ => fixture
                .owner
                .set_account_session(Some(fixture.session.clone()))
                .unwrap(),
        }
        assert!(capture.is_canceled());
        assert_eq!(
            fixture.owner.commit(prepared).unwrap_err(),
            DispatchError::Canceled
        );
        assert_eq!(fixture.calls(), 0);
        assert_eq!(
            fixture.owner.acknowledge_capture(&capture).unwrap(),
            ApplyResult::Applied
        );
        assert_eq!(
            fixture.owner.acknowledge_capture(&capture).unwrap(),
            ApplyResult::Ignored
        );
        fixture.assert_no_cache();
    }
}

#[test]
fn revocation_before_queued_worker_run_makes_zero_calls_and_discards_completion() {
    for control in 0..3 {
        let mut fixture = Fixture::enabled(false);
        let (capture, worker) = fixture.worker(1);
        match control {
            0 => {
                let revision = fixture.owner.consent().unwrap().revision;
                fixture.owner.disable(revision).unwrap();
            }
            1 => fixture.owner.sign_out().unwrap(),
            _ => fixture
                .owner
                .set_account_session(Some(fixture.session.clone()))
                .unwrap(),
        }
        assert!(capture.is_canceled());
        assert_eq!(
            fixture.owner.acknowledge_capture(&capture).unwrap_err(),
            DispatchError::InvalidStage
        );
        assert_eq!(
            fixture.owner.finish(worker.run()).unwrap(),
            ApplyResult::Ignored
        );
        assert_eq!(fixture.calls(), 0);
        fixture.assert_no_cache();
    }
}

#[test]
fn revocation_during_accepted_http_does_not_block_controls_or_apply_late_body_or_error() {
    for fail in [false, true] {
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let mut fixture = Fixture::new(ReplyMode::BlockedBody {
            entered: entered_tx,
            release: release_rx,
            fail,
        });
        fixture.enable(false);
        let (capture, worker) = fixture.worker(1);
        let worker = thread::spawn(move || worker.run());
        entered_rx.recv_timeout(Duration::from_secs(10)).unwrap();
        let offer = fixture.owner.consent_offer();
        let revision = fixture.owner.consent().unwrap().revision;
        // These controls must finish while the accepted synthetic response is blocked.
        let disabled = fixture.owner.disable(revision);
        let view = fixture.owner.view(&session(1));
        let acknowledgement = fixture.owner.acknowledge_capture(&capture);
        release_tx.send(()).unwrap();
        assert_eq!(offer.unwrap().origin(), ORIGIN);
        disabled.unwrap();
        assert_eq!(view.unwrap(), StatusView::Disabled);
        assert_eq!(acknowledgement.unwrap_err(), DispatchError::InvalidStage);
        assert!(capture.is_canceled());
        assert_eq!(
            fixture.owner.finish(worker.join().unwrap()).unwrap(),
            ApplyResult::Ignored
        );
        assert_eq!(fixture.calls(), 1);
        fixture.assert_no_cache();
    }
}

#[test]
fn external_account_replacement_is_checked_before_worker_bearer_use() {
    let mut fixture = Fixture::enabled(false);
    let (_, worker) = fixture.worker(1);
    let replacement = login(&fixture.account, "synthetic-external-account");
    assert_ne!(replacement.revision(), fixture.session.revision());
    // Deliberately no set_account_session or recheck event before worker execution.
    let completion = worker.run();
    assert_eq!(fixture.calls(), 0);
    assert_eq!(
        fixture.owner.finish(completion).unwrap(),
        ApplyResult::Ignored
    );
    assert_eq!(
        fixture.owner.view(&session(1)).unwrap(),
        StatusView::SignedOut
    );
    fixture.assert_no_cache();
}

#[test]
fn external_account_replacement_after_response_is_checked_again_at_finish() {
    let mut fixture = Fixture::enabled(false);
    let (_, worker) = fixture.worker(1);
    let completion = worker.run();
    login(&fixture.account, "synthetic-external-account");
    assert_eq!(
        fixture.owner.finish(completion).unwrap(),
        ApplyResult::Ignored
    );
    assert_eq!(fixture.calls(), 1);
    fixture.assert_no_cache();
}

#[test]
fn duplicate_build_and_early_capture_ack_are_forbidden_but_abandon_frees_only_at_finish() {
    let mut fixture = Fixture::enabled(false);
    let original_capture = capture(&mut fixture.owner, 1);
    let prepared = build(&mut fixture.owner, &original_capture);
    assert_eq!(
        fixture
            .owner
            .build(
                &original_capture,
                original_capture.source_stamp(),
                metadata(),
                CaptureInput {
                    reported_text: Some(REPORT),
                    recent_output: None
                },
                &[]
            )
            .unwrap_err(),
        DispatchError::InvalidStage
    );
    let worker = fixture.owner.commit(prepared).unwrap();
    assert_eq!(
        fixture
            .owner
            .acknowledge_capture(&original_capture)
            .unwrap_err(),
        DispatchError::InvalidStage
    );
    let completion = worker.abandon();
    assert!(original_capture.is_canceled());
    assert_skipped(&mut fixture.owner, 1, StatusView::Pending);
    assert_eq!(
        fixture
            .owner
            .acknowledge_capture(&original_capture)
            .unwrap_err(),
        DispatchError::InvalidStage
    );
    assert_eq!(
        fixture.owner.finish(completion).unwrap(),
        ApplyResult::Ignored
    );
    assert_eq!(
        fixture
            .owner
            .acknowledge_capture(&original_capture)
            .unwrap(),
        ApplyResult::Ignored
    );
    let replacement = capture(&mut fixture.owner, 1);
    fixture.owner.acknowledge_capture(&replacement).unwrap();
    assert_eq!(fixture.calls(), 0);
    fixture.assert_no_cache();
}

#[test]
fn invalidated_capture_slots_stay_bounded_until_actual_capture_acknowledgement() {
    let mut fixture = Fixture::enabled(false);
    fixture.owner.observe(source(2)).unwrap();
    fixture.owner.observe(source(3)).unwrap();
    let first = capture(&mut fixture.owner, 1);
    let _prepared = build(&mut fixture.owner, &first);
    let second = capture(&mut fixture.owner, 2);
    assert_skipped(&mut fixture.owner, 3, StatusView::Pending);
    let revision = fixture.owner.consent().unwrap().revision;
    fixture.owner.disable(revision).unwrap();
    fixture.enable(false);
    assert!(first.is_canceled() && second.is_canceled());
    assert_skipped(&mut fixture.owner, 3, StatusView::Pending);
    assert_eq!(
        fixture.owner.acknowledge_capture(&first).unwrap(),
        ApplyResult::Applied
    );
    let third = capture(&mut fixture.owner, 3);
    assert_skipped(&mut fixture.owner, 1, StatusView::Pending);
    fixture.owner.acknowledge_capture(&second).unwrap();
    let replacement = capture(&mut fixture.owner, 1);
    fixture.owner.acknowledge_capture(&third).unwrap();
    fixture.owner.acknowledge_capture(&replacement).unwrap();
    assert_eq!(fixture.calls(), 0);
}

#[test]
fn invalidated_committed_slots_survive_revocation_and_abandon_until_finished() {
    let mut fixture = Fixture::enabled(false);
    fixture.owner.observe(source(2)).unwrap();
    fixture.owner.observe(source(3)).unwrap();
    let (first_capture, first) = fixture.worker(1);
    let (_, second) = fixture.worker(2);
    let revision = fixture.owner.consent().unwrap().revision;
    fixture.owner.disable(revision).unwrap();
    fixture.enable(false);
    assert_eq!(
        fixture
            .owner
            .acknowledge_capture(&first_capture)
            .unwrap_err(),
        DispatchError::InvalidStage
    );
    assert_skipped(&mut fixture.owner, 3, StatusView::Pending);
    let abandoned = first.abandon();
    assert_skipped(&mut fixture.owner, 3, StatusView::Pending);
    assert_eq!(
        fixture.owner.finish(abandoned).unwrap(),
        ApplyResult::Ignored
    );
    let third = capture(&mut fixture.owner, 3);
    assert_skipped(&mut fixture.owner, 1, StatusView::Pending);
    assert_eq!(
        fixture.owner.finish(second.run()).unwrap(),
        ApplyResult::Ignored
    );
    fixture.owner.acknowledge_capture(&third).unwrap();
    assert_eq!(fixture.calls(), 0);
}

fn fence_source(owner: &mut Owner, change: u8) -> StatusView {
    let mut changed = source(1);
    match change {
        0 => {
            changed.stamp.source_revision += 1;
            owner.observe(changed).unwrap();
            StatusView::Missing
        }
        1 => {
            changed.private = true;
            owner.observe(changed).unwrap();
            StatusView::Private
        }
        2 => {
            changed.stamp.source_revision += 1;
            changed.live = false;
            owner.observe(changed).unwrap();
            StatusView::NotLive
        }
        _ => {
            owner.remove(&session(1)).unwrap();
            assert_eq!(owner.observe(changed).unwrap(), ApplyResult::Ignored);
            StatusView::Missing
        }
    }
}

#[test]
fn source_update_privacy_exit_and_removal_fence_prepared_dispatch() {
    for change in 0..4 {
        let mut fixture = Fixture::enabled(false);
        let capture = capture(&mut fixture.owner, 1);
        let prepared = build(&mut fixture.owner, &capture);
        let expected = fence_source(&mut fixture.owner, change);
        assert!(capture.is_canceled());
        assert_eq!(
            fixture.owner.commit(prepared).unwrap_err(),
            DispatchError::Canceled
        );
        fixture.owner.acknowledge_capture(&capture).unwrap();
        assert_eq!(fixture.owner.view(&session(1)).unwrap(), expected);
        assert_eq!(fixture.calls(), 0);
        fixture.assert_no_cache();
    }
}

#[test]
fn source_update_privacy_exit_and_removal_discard_already_received_results() {
    for change in 0..4 {
        let mut fixture = Fixture::enabled(false);
        let (capture, worker) = fixture.worker(1);
        let completion = worker.run();
        let expected = fence_source(&mut fixture.owner, change);
        assert!(capture.is_canceled());
        assert_eq!(
            fixture.owner.finish(completion).unwrap(),
            ApplyResult::Ignored
        );
        assert_eq!(fixture.owner.view(&session(1)).unwrap(), expected);
        assert_eq!(fixture.calls(), 1);
        fixture.assert_no_cache();
    }
}

#[test]
fn capture_must_attest_the_exact_reserved_source_stamp() {
    let mut fixture = Fixture::enabled(false);
    let capture = capture(&mut fixture.owner, 1);
    let mut wrong_stamp = capture.source_stamp();
    wrong_stamp.terminal_generation += 1;
    assert_eq!(
        fixture
            .owner
            .build(
                &capture,
                wrong_stamp,
                metadata(),
                CaptureInput {
                    reported_text: Some(REPORT),
                    recent_output: None
                },
                &[]
            )
            .unwrap_err(),
        DispatchError::Policy(StatusError::InvalidSource)
    );
    assert!(capture.is_canceled());
    assert_eq!(
        fixture
            .owner
            .build(
                &capture,
                capture.source_stamp(),
                metadata(),
                CaptureInput {
                    reported_text: Some(REPORT),
                    recent_output: None
                },
                &[]
            )
            .unwrap_err(),
        DispatchError::Canceled
    );
    fixture.owner.acknowledge_capture(&capture).unwrap();
    assert_eq!(fixture.calls(), 0);
}

#[test]
fn queued_token_use_expires_at_thirty_seconds_and_account_expiry_discards_results() {
    for elapsed in [29, 30, 3600] {
        let mut fixture = Fixture::enabled(false);
        let (_, worker) = fixture.worker(1);
        fixture.clock.0.store(elapsed, Ordering::SeqCst);
        let result = fixture.owner.finish(worker.run());
        match elapsed {
            29 => {
                assert_eq!(result.unwrap(), ApplyResult::Applied);
                assert_eq!(fixture.calls(), 1);
            }
            30 => {
                assert_eq!(
                    result.unwrap_err(),
                    DispatchError::Account(AccountError::TokenLeaseExpired)
                );
                assert_eq!(fixture.calls(), 0);
                fixture.assert_no_cache();
            }
            _ => {
                assert_eq!(result.unwrap(), ApplyResult::Ignored);
                assert_eq!(fixture.calls(), 0);
                assert_eq!(
                    fixture.owner.view(&session(1)).unwrap(),
                    StatusView::SignedOut
                );
                fixture.assert_no_cache();
            }
        }
    }
}

#[test]
fn dropping_owner_cancels_a_queued_worker_without_http_or_cache() {
    let mut fixture = Fixture::enabled(false);
    let (capture, worker) = fixture.worker(1);
    let requests = fixture.requests.clone();
    let store = fixture.store.clone();
    drop(fixture.owner);
    assert!(capture.is_canceled());
    let completion = worker.run();
    assert_redacted(&format!("{completion:?}"));
    assert!(requests.lock().unwrap().is_empty());
    assert!(store.0.lock().unwrap().cache.is_empty());
}

#[test]
fn policy_load_failure_cancels_work_and_keeps_persisted_consent_unconfirmed_after_recovery() {
    let mut fixture = Fixture::enabled(false);
    let (_, worker) = fixture.worker(1);
    fixture.owner.finish(worker.run()).unwrap();
    let mut changed = source(1);
    changed.stamp.source_revision += 1;
    fixture.owner.observe(changed).unwrap();
    let (capture, worker) = fixture.worker(1);
    let saved = fixture.owner.consent().unwrap();
    fixture.store.0.lock().unwrap().fail_load = true;
    assert_eq!(
        fixture.owner.recheck().unwrap_err(),
        DispatchError::Policy(StatusError::Store(StatusStoreError::Database))
    );
    assert!(capture.is_canceled());
    fixture.store.0.lock().unwrap().fail_load = false;
    assert_eq!(fixture.owner.consent().unwrap(), saved);
    assert_eq!(
        fixture.owner.view(&session(1)).unwrap(),
        StatusView::ConsentRequired
    );
    assert_skipped(&mut fixture.owner, 1, StatusView::ConsentRequired);
    assert_eq!(
        fixture.owner.finish(worker.run()).unwrap(),
        ApplyResult::Ignored
    );
    assert_eq!(fixture.calls(), 1);
    assert_eq!(fixture.store.0.lock().unwrap().cache.len(), 1);
}

#[test]
fn failed_opt_out_and_sign_out_fail_closed_even_when_persisted_enabled_state_survives() {
    for sign_out in [false, true] {
        let mut fixture = Fixture::enabled(false);
        let (capture, worker) = fixture.worker(1);
        let saved = fixture.owner.consent().unwrap();
        let expected_view = if sign_out {
            let directory = fixture.directory.path().join("private-auth");
            fs::set_permissions(&directory, fs::Permissions::from_mode(0o755)).unwrap();
            let result = fixture.owner.sign_out();
            fs::set_permissions(directory, fs::Permissions::from_mode(0o700)).unwrap();
            assert_eq!(
                result.unwrap_err(),
                DispatchError::Account(AccountError::Store(CredentialStoreError::UnsafePath))
            );
            assert!(fixture.credentials.load().unwrap().credentials().is_some());
            StatusView::SignedOut
        } else {
            fixture.store.0.lock().unwrap().fail_write = true;
            let result = fixture.owner.disable(saved.revision);
            fixture.store.0.lock().unwrap().fail_write = false;
            assert_eq!(
                result.unwrap_err(),
                DispatchError::Policy(StatusError::Store(StatusStoreError::Database))
            );
            StatusView::ConsentRequired
        };
        assert!(capture.is_canceled());
        assert_eq!(fixture.owner.consent().unwrap(), saved);
        assert_eq!(fixture.owner.view(&session(1)).unwrap(), expected_view);
        assert_eq!(
            fixture.owner.finish(worker.run()).unwrap(),
            ApplyResult::Ignored
        );
        assert_eq!(fixture.calls(), 0);
        fixture.assert_no_cache();
    }
}

#[test]
fn opaque_dispatch_debug_redacts_bindings_context_credentials_and_success_text() {
    let mut fixture = Fixture::enabled(true);
    let offer = fixture.owner.consent_offer().unwrap();
    assert_redacted(&format!("{offer:?}"));
    let capture = capture(&mut fixture.owner, 1);
    assert_redacted(&format!("{capture:?}"));
    let prepared = fixture
        .owner
        .build(
            &capture,
            capture.source_stamp(),
            metadata(),
            CaptureInput {
                reported_text: Some(REPORT),
                recent_output: Some(OUTPUT),
            },
            &[],
        )
        .unwrap();
    assert_redacted(&format!("{prepared:?}"));
    let worker = fixture.owner.commit(prepared).unwrap();
    assert_redacted(&format!("{worker:?}"));
    let completion = worker.run();
    assert_redacted(&format!("{completion:?}"));
    assert_eq!(
        fixture.owner.finish(completion).unwrap(),
        ApplyResult::Applied
    );
}

mod owner;
mod refresh;
mod source;
