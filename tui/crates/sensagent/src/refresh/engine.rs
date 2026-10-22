use std::collections::HashSet;
use std::fmt;
use std::panic::{self, AssertUnwindSafe};
use std::time::Duration;

use session_core::api::{EventEnvelope, SessionEvent};
use session_core::client::SessionApi;
use session_core::ids::SessionId;
use session_core::model::SessionView;

use super::schedule::{Outcome, RefreshFailure, RefreshScheduler, MAX_RUNNING};
use crate::auth::{
    AccountError, AccountSession, ApplyResult, CredentialStore, SystemClock, WallClock,
};
use crate::cloud::{CloudAIError, RemoteErrorCode};
use crate::dispatch::{
    DispatchCompletion, DispatchError, DispatchPreparation, StatusCaptureJob, StatusConsentOffer,
    StatusDispatchJob, StatusDispatchService,
};
use crate::source::{CaptureError, CaptureExpectation, SessionCapture};
use crate::status::{
    ConsentRevision, ConsentSnapshot, FolderConsent, InputSelection, SourceStamp, StatusStore,
    StatusView,
};

/// A session's AI status as the refresher sees it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AIStatus {
    /// The status policy's view: cached text only under current account, consent and source.
    pub view: StatusView,
    /// Why the last refresh produced no text, until one succeeds.
    pub failure: Option<RefreshFailure>,
}

/// Refreshing as a whole. Native session status never depends on it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RefreshState {
    /// The daemon's session list and events are being followed.
    pub connected: bool,
    /// The service asked for fewer requests; every session waits.
    pub rate_limited: bool,
    /// The account was refused; nothing refreshes until it or consent changes.
    pub unauthorized: bool,
}

/// One scoped capture to run off the owner thread.
pub struct CaptureWork {
    job: Box<StatusCaptureJob>,
    expectation: CaptureExpectation,
}

impl CaptureWork {
    /// Read the job's windows through a user SessionApi client. After `Changed`, the
    /// session is read again so the owner can correct its facts. A panic in either
    /// counts as an unavailable read, so the job always comes back.
    pub fn capture<Api: SessionApi + ?Sized>(self, api: &Api) -> Captured {
        let Self { job, expectation } = self;
        let (result, refreshed) = panic::catch_unwind(AssertUnwindSafe(|| {
            let result = expectation.capture(api, &job);
            let refreshed = match result {
                Err(CaptureError::Changed) => api.get(job.session_id()).ok(),
                _ => None,
            };
            (result, refreshed)
        }))
        .unwrap_or((Err(CaptureError::Unavailable), None));
        Captured {
            job,
            result,
            refreshed,
        }
    }
}

/// A finished capture, with the session's fresh view when it had changed.
pub struct Captured {
    job: Box<StatusCaptureJob>,
    result: Result<SessionCapture, CaptureError>,
    refreshed: Option<SessionView>,
}

/// One committed request to run off the owner thread.
pub struct Dispatch<A, C = SystemClock> {
    session: SessionId,
    stamp: SourceStamp,
    worker: StatusDispatchJob<A, C>,
}

impl<A: CredentialStore, C: WallClock> Dispatch<A, C> {
    /// Make the request, at most once. A panic loses the job: its slot stays fenced.
    pub fn run(self) -> Completed {
        Completed {
            session: self.session,
            stamp: self.stamp,
            completion: self.worker.run(),
        }
    }
}

/// A finished request for the owner to apply.
pub struct Completed {
    session: SessionId,
    stamp: SourceStamp,
    completion: DispatchCompletion,
}

/// Drives one dispatch coordinator: follows daemon views and events, decides what
/// to refresh, and turns captures and requests into status. Synchronous and
/// deterministic over an injected monotonic `now`; the [runner](super::RefreshRunner)
/// supplies the threads. Captures and requests run elsewhere and come back here.
///
/// Every account and consent control goes through this type, so its scheduler
/// reconsiders every session after one.
pub struct StatusRefresher<S, A, C = SystemClock> {
    service: StatusDispatchService<S, A, C>,
    scheduler: RefreshScheduler,
    /// Sessions with a job out. A lost job stays here, as its slot stays fenced.
    running: HashSet<SessionId>,
    /// Running sessions observed since their job started: a view read by that job
    /// may be older than what was applied, so it is not applied.
    observed: HashSet<SessionId>,
    connected: bool,
    /// Sharing is off, signed out or unconfirmed: nothing to plan until a control.
    idle: bool,
}

impl<S: StatusStore, A: CredentialStore, C: WallClock> StatusRefresher<S, A, C> {
    /// Nothing refreshes until the first `resync`, and then only under the
    /// coordinator's confirmed consent.
    pub fn new(service: StatusDispatchService<S, A, C>) -> Self {
        Self {
            service,
            scheduler: RefreshScheduler::new(),
            running: HashSet::new(),
            observed: HashSet::new(),
            connected: false,
            idle: false,
        }
    }

    /// Apply a fresh daemon `list` after (re)subscribing; refreshing may start.
    pub fn resync(&mut self, views: &[SessionView], now: Duration) -> Result<(), DispatchError> {
        let result = self.service.resync(views);
        self.observed.extend(self.running.iter().cloned());
        // Missed output is assumed: resync left every live session dirty.
        for candidate in self.service.refresh_candidates() {
            if candidate.dirty {
                self.scheduler.output(&candidate.session, now);
            }
        }
        self.connected = true;
        result
    }

    /// The event stream ended: no capture starts until the next `resync`.
    pub fn disconnected(&mut self) {
        self.connected = false;
    }

    /// Apply one daemon view (`get`, or a refreshed view after a changed capture).
    pub fn apply_view(&mut self, view: &SessionView, now: Duration) -> Result<(), DispatchError> {
        self.observe(&view.id);
        let dirty = self.service.is_dirty(&view.id);
        let result = self.service.apply_view(view);
        if !dirty {
            self.note_output(&view.id, now);
        }
        result
    }

    /// Apply one subscribed daemon event.
    pub fn apply_event(
        &mut self,
        event: &EventEnvelope,
        now: Duration,
    ) -> Result<(), DispatchError> {
        let session = &event.session_id;
        self.observe(session);
        let dirty = self.service.is_dirty(session);
        let result = self.service.apply_event(event);
        match event.event {
            SessionEvent::Removed => self.scheduler.forget(session),
            SessionEvent::Activity => self.note_output(session, now),
            // A clean session that became dirty (a new activity phase) starts its quiet
            // period; other view changes, such as titles, don't restart it.
            _ if !dirty => self.note_output(session, now),
            _ => {}
        }
        result
    }

    /// Recheck outstanding jobs and start the captures due now: at most two jobs at
    /// once, within the global start budget. Run each with `CaptureWork::capture`
    /// off this thread and pass the result to `captured`.
    pub fn tick(&mut self, now: Duration) -> Vec<CaptureWork> {
        let mut work = Vec::new();
        if self.service.recheck().is_err() || !self.connected || self.idle {
            return work;
        }
        let candidates = self.service.refresh_candidates();
        for due in self.scheduler.plan(now, candidates, &self.running) {
            if self.running.len() >= MAX_RUNNING || !self.scheduler.has_budget(now) {
                break;
            }
            if due.advance {
                if self.service.advance(&due.session).is_err() {
                    break;
                }
                self.scheduler.advanced(&due.session);
            }
            let Some(stamp) = self.service.source_stamp(&due.session) else {
                continue;
            };
            let job = match self.service.prepare(&due.session) {
                Ok(DispatchPreparation::Ready(job)) => job,
                Ok(DispatchPreparation::Skipped(view)) => match view {
                    StatusView::Pending => break,
                    StatusView::Disabled | StatusView::SignedOut | StatusView::ConsentRequired => {
                        self.idle = true;
                        break;
                    }
                    StatusView::Excluded => {
                        self.scheduler.settle(&due.session, stamp, true);
                        continue;
                    }
                    _ => {
                        self.scheduler.settle(&due.session, stamp, false);
                        continue;
                    }
                },
                Err(_) => break,
            };
            // Output can't change a reports-only job's input: ignore it until the
            // stamp or consent changes.
            let reports_only = job.input_selection() == InputSelection::ReportOnly;
            self.scheduler
                .started(&due.session, stamp, reports_only, now);
            match self.service.expectation(&job) {
                Ok(expectation) => {
                    self.observed.remove(&due.session);
                    self.running.insert(due.session);
                    work.push(CaptureWork { job, expectation });
                }
                Err(error) => {
                    let _ = self.service.acknowledge_capture(&job);
                    self.scheduler
                        .finished(&due.session, stamp, outcome(&error), now);
                }
            }
        }
        work
    }

    /// Build and commit a finished capture. Returns the request to run off this
    /// thread, or `None` when the job ended here (and freed its slot). Nothing is
    /// committed while the service's rate limit or an account refusal holds.
    pub fn captured(&mut self, captured: Captured, now: Duration) -> Option<Dispatch<A, C>> {
        let Captured {
            job,
            result,
            refreshed,
        } = captured;
        let session = job.session_id().clone();
        let stamp = job.source_stamp();
        let committed = if self.scheduler.halted(now) {
            Err(DispatchError::Canceled)
        } else {
            result
                .map_err(DispatchError::from)
                .and_then(|capture| self.service.build_capture(&job, &capture, &[]))
                .and_then(|prepared| self.service.commit(prepared))
        };
        match committed {
            Ok(worker) => Some(Dispatch {
                session,
                stamp,
                worker,
            }),
            Err(error) => {
                let _ = self.service.acknowledge_capture(&job);
                self.running.remove(&session);
                let observed = self.observed.remove(&session);
                self.scheduler
                    .finished(&session, stamp, outcome(&error), now);
                if let Some(view) = refreshed.filter(|_| !observed) {
                    let _ = self.apply_view(&view, now);
                }
                None
            }
        }
    }

    /// Apply a finished request; its slot is freed.
    pub fn completed(&mut self, completed: Completed, now: Duration) {
        let Completed {
            session,
            stamp,
            completion,
        } = completed;
        self.running.remove(&session);
        self.observed.remove(&session);
        let result = match self.service.finish(completion) {
            Ok(ApplyResult::Applied) => Outcome::Done,
            Ok(ApplyResult::Ignored) => Outcome::Interrupted,
            Err(error) => outcome(&error),
        };
        self.scheduler.finished(&session, stamp, result, now);
    }

    /// A session's status and its last refresh failure.
    pub fn status(&mut self, session: &SessionId) -> Result<AIStatus, DispatchError> {
        Ok(AIStatus {
            view: self.service.view(session)?,
            failure: self.scheduler.failure(session),
        })
    }

    pub fn state(&self, now: Duration) -> RefreshState {
        RefreshState {
            connected: self.connected,
            rate_limited: self.scheduler.rate_limited(now),
            unauthorized: self.scheduler.unauthorized(),
        }
    }

    /// The adopted account view, for display.
    pub fn account(&self) -> Option<&AccountSession> {
        self.service.account_session()
    }

    /// The durable consent generation.
    pub fn consent(&mut self) -> Result<ConsentSnapshot, DispatchError> {
        self.service.consent()
    }

    /// The exact account, consent and destination a user must confirm.
    pub fn consent_offer(&mut self) -> Result<StatusConsentOffer, DispatchError> {
        self.service.consent_offer()
    }

    /// Confirm the user's choice for `offer`; every session is reconsidered.
    pub fn enable(
        &mut self,
        offer: &StatusConsentOffer,
        folders: Vec<FolderConsent>,
    ) -> Result<ConsentSnapshot, DispatchError> {
        let result = self.service.enable(offer, folders);
        self.reconsider();
        result
    }

    /// Stop sharing.
    pub fn disable(&mut self, expected: ConsentRevision) -> Result<ConsentSnapshot, DispatchError> {
        let result = self.service.disable(expected);
        self.reconsider();
        result
    }

    /// Adopt a verified account view; consent must be confirmed again.
    pub fn set_account_session(
        &mut self,
        session: Option<AccountSession>,
    ) -> Result<(), DispatchError> {
        let result = self.service.set_account_session(session);
        self.reconsider();
        result
    }

    /// Stop sharing and remove local credentials.
    pub fn sign_out(&mut self) -> Result<(), DispatchError> {
        let result = self.service.sign_out();
        self.reconsider();
        result
    }

    #[cfg(test)]
    pub(crate) fn source_stamp(&self, session: &SessionId) -> Option<SourceStamp> {
        self.service.source_stamp(session)
    }

    fn reconsider(&mut self) {
        self.scheduler.reset();
        self.idle = false;
    }

    fn observe(&mut self, session: &SessionId) {
        if self.running.contains(session) {
            self.observed.insert(session.clone());
        }
    }

    fn note_output(&mut self, session: &SessionId, now: Duration) {
        if self.service.is_dirty(session) {
            self.scheduler.output(session, now);
        }
    }
}

impl<S, A, C> fmt::Debug for StatusRefresher<S, A, C> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("StatusRefresher")
            .field("scheduler", &self.scheduler)
            .field("running", &self.running.len())
            .field("connected", &self.connected)
            .field("idle", &self.idle)
            .finish()
    }
}

impl fmt::Debug for CaptureWork {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("CaptureWork { bindings: [REDACTED] }")
    }
}

impl fmt::Debug for Captured {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Captured { windows: [REDACTED] }")
    }
}

impl<A, C> fmt::Debug for Dispatch<A, C> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Dispatch { bindings_and_context: [REDACTED] }")
    }
}

impl fmt::Debug for Completed {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Completed { result: [REDACTED] }")
    }
}

/// How an error ends an attempt. Only failures worth showing or backing off count.
pub(crate) fn outcome(error: &DispatchError) -> Outcome {
    use RefreshFailure::{Rejected, Unauthorized, Unavailable, Unreadable};
    match error {
        DispatchError::Capture(CaptureError::Unavailable | CaptureError::NotPermitted) => {
            Outcome::Failed(Unreadable)
        }
        DispatchError::Cloud(error) => match error {
            CloudAIError::NoContext => Outcome::Done,
            CloudAIError::InvalidRequest
            | CloudAIError::CaptureTooLarge
            | CloudAIError::DataNotPermitted => Outcome::Failed(Rejected),
            CloudAIError::Remote {
                code, retry_after, ..
            } => match code {
                RemoteErrorCode::InvalidRequest => Outcome::Failed(Rejected),
                RemoteErrorCode::Unauthenticated | RemoteErrorCode::Forbidden => {
                    Outcome::Failed(Unauthorized)
                }
                RemoteErrorCode::RateLimited => Outcome::RateLimited(*retry_after),
                RemoteErrorCode::UpstreamUnavailable | RemoteErrorCode::InvalidOutput => {
                    Outcome::Failed(Unavailable)
                }
            },
            CloudAIError::Canceled | CloudAIError::PolicyUnavailable => Outcome::Interrupted,
            CloudAIError::InvalidConfiguration(_)
            | CloudAIError::InvalidResponse
            | CloudAIError::CorrelationMismatch
            | CloudAIError::Network(_)
            | CloudAIError::HttpStatus(_) => Outcome::Failed(Unavailable),
        },
        DispatchError::Account(error) => match error {
            AccountError::TokenLeaseExpired => Outcome::Interrupted,
            AccountError::Store(_)
            | AccountError::ClockChanged
            | AccountError::ClockUnavailable => Outcome::Failed(Unavailable),
            AccountError::Expired
            | AccountError::Provider(_)
            | AccountError::ProviderMismatch
            | AccountError::OwnerMismatch
            | AccountError::AccountMismatch => Outcome::Failed(Unauthorized),
        },
        DispatchError::Capture(CaptureError::Canceled | CaptureError::Changed)
        | DispatchError::Policy(_)
        | DispatchError::Source(_)
        | DispatchError::ForeignOwner
        | DispatchError::InvalidStage
        | DispatchError::Canceled => Outcome::Interrupted,
    }
}
