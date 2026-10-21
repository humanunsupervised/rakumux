use std::collections::HashMap;
use std::sync::Arc;

use session_core::ids::SessionId;

use crate::auth::SecretValue;
use crate::auth::{
    AccountService, AccountSession, ApplyResult, CredentialStore, SystemClock, WallClock,
};
use crate::cloud::{CaptureInput, CaptureMetadata, CloudAIClient, CloudAIError, ContextBuilder};
use crate::status::{
    AISessionStatusService, AccountGate, AccountSessionGate, ConsentPolicy, ConsentRevision,
    ConsentSnapshot, FolderConsent, GenerationTicket, Preparation, StatusError, StatusSource,
    StatusStore, StatusView,
};

use super::job::JobKey;
use super::{
    DispatchCompletion, DispatchError, DispatchPreparation, PreparedStatusDispatch,
    StatusCaptureJob, StatusConsentOffer, StatusDispatchJob,
};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    Capturing,
    Prepared,
    Committed,
}

struct Pending {
    ticket: GenerationTicket,
    cancel: crate::cloud::Cancellation,
    phase: Phase,
}

/// Explicitly driven, in-process control state for one fixed-target status owner.
///
/// All controls take `&mut self`, serializing dispatch commitment with local
/// revocation. Workers run separately and return opaque completions. The embedding
/// service must authenticate user-only mutations, attest scoped capture and call
/// `recheck` on freshness events/ticks. This is not a running per-home/IPC authority.
/// New instances never reuse destination consent implicitly, even from the same store.
pub struct StatusDispatchService<S, A, C = SystemClock> {
    owner: Arc<()>,
    account: Arc<AccountService<A, C>>,
    gate: Arc<AccountSessionGate<A, C>>,
    session: Option<AccountSession>,
    policy: AISessionStatusService<S, Arc<AccountSessionGate<A, C>>>,
    cloud: Arc<CloudAIClient>,
    origin: String,
    confirmed: bool,
    pending: HashMap<String, Pending>,
}

impl<S: StatusStore, A: CredentialStore, C: WallClock> StatusDispatchService<S, A, C> {
    /// Inject explicit stores/account/transport. No home reads, network calls, source
    /// capture, automatic sharing, threads or implicit account restoration occur.
    pub fn new(account: Arc<AccountService<A, C>>, store: S, cloud: CloudAIClient) -> Self {
        let gate = Arc::new(AccountSessionGate::new(account.clone()));
        let origin = cloud.origin();
        Self {
            owner: Arc::new(()),
            account,
            gate: gate.clone(),
            session: None,
            policy: AISessionStatusService::new(store, gate),
            cloud: Arc::new(cloud),
            origin,
            confirmed: false,
            pending: HashMap::new(),
        }
    }

    /// Adopt an owned verified account view. Every replacement first cancels work
    /// and requires new explicit consent; sign-in alone never resumes sharing.
    pub fn set_account_session(
        &mut self,
        session: Option<AccountSession>,
    ) -> Result<(), DispatchError> {
        self.invalidate();
        self.session = None;
        self.gate.set_session(None)?;
        if let Some(session) = session {
            if !self.account.is_current(&session)? {
                return Err(StatusError::AccountChanged.into());
            }
            self.gate.set_session(Some(session.clone()))?;
            self.session = Some(session);
        }
        Ok(())
    }

    /// Stop local sharing before attempting credential removal, even if storage fails.
    /// Browser logout and remote grant revocation are separate unimplemented operations.
    pub fn sign_out(&mut self) -> Result<(), DispatchError> {
        self.invalidate();
        self.session = None;
        self.gate.set_session(None)?;
        self.account.sign_out()?;
        Ok(())
    }

    /// Read the durable consent generation; this does not confirm a destination.
    pub fn consent(&mut self) -> Result<ConsentSnapshot, DispatchError> {
        let result = self.policy.consent();
        self.policy_result(result)
    }

    /// Prepare the exact current account, consent generation and immutable public
    /// destination that a trusted user-control UI must show before explicit opt-in.
    pub fn consent_offer(&mut self) -> Result<StatusConsentOffer, DispatchError> {
        let result = self.gate.current();
        let Some(account) = self.policy_result(result)? else {
            self.invalidate();
            return Err(StatusError::SignedOut.into());
        };
        let consent = self.consent()?.revision;
        Ok(StatusConsentOffer {
            owner: self.owner.clone(),
            origin: self.origin.clone(),
            account,
            consent,
        })
    }

    /// Confirm a user action for this exact offer and selected data kinds. Foreign,
    /// stale-account or stale-consent actions cannot retarget current sharing.
    /// Every local enable attempt fences old work before persistence.
    pub fn enable(
        &mut self,
        offer: &StatusConsentOffer,
        folders: Vec<FolderConsent>,
    ) -> Result<ConsentSnapshot, DispatchError> {
        if !Arc::ptr_eq(&self.owner, &offer.owner) || offer.origin != self.origin {
            return Err(DispatchError::ForeignOwner);
        }
        self.invalidate();
        let result = self.policy.enable(offer.consent, &offer.account, folders);
        let saved = self.policy_result(result)?;
        self.confirmed = true;
        Ok(saved)
    }

    /// Fence all local work before compare-and-swap opt-out. A failed/stale write
    /// leaves this coordinator unconfirmed, never silently sharing under old consent.
    pub fn disable(&mut self, expected: ConsentRevision) -> Result<ConsentSnapshot, DispatchError> {
        self.invalidate();
        let result = self.policy.disable(expected);
        self.policy_result(result)
    }

    /// Apply a trusted ordered daemon observation, then cancel affected jobs.
    pub fn observe(&mut self, source: StatusSource) -> Result<ApplyResult, DispatchError> {
        let result = self.policy.observe(source);
        let applied = self.policy_result(result)?;
        self.recheck()?;
        Ok(applied)
    }

    /// Retire a daemon-removed source and fence its workers. Never touches a PTY.
    pub fn remove(&mut self, session: &SessionId) -> Result<(), DispatchError> {
        let result = self.policy.remove(session);
        self.policy_result(result)?;
        self.recheck()
    }

    /// Recheck pending bindings on owner events/ticks. Invalidated work stays in
    /// both bounded registries until capture stops or a committed worker completes.
    pub fn recheck(&mut self) -> Result<(), DispatchError> {
        let tickets: Vec<_> = self
            .pending
            .values()
            .map(|job| job.ticket.clone())
            .collect();
        for ticket in tickets {
            let result = self.policy.is_current(&ticket);
            let current = self.policy_result(result)?;
            if !current || !self.confirmed {
                if let Some(job) = self.pending.get(ticket.request_id()) {
                    job.cancel.cancel();
                }
            }
        }
        Ok(())
    }

    /// Expose cache text only under current account/consent/source and this
    /// instance's destination confirmation. Existing disk consent alone is insufficient.
    pub fn view(&mut self, session: &SessionId) -> Result<StatusView, DispatchError> {
        self.recheck()?;
        if !self.confirmed {
            return self.unconfirmed_view();
        }
        let result = self.policy.view(session);
        self.policy_result(result)
    }

    /// Reserve one of at most two unacknowledged jobs. Capture is external, scoped
    /// and explicit; a signed-in account or stored consent does not start it.
    pub fn prepare(&mut self, session: &SessionId) -> Result<DispatchPreparation, DispatchError> {
        self.recheck()?;
        if !self.confirmed {
            return Ok(DispatchPreparation::Skipped(self.unconfirmed_view()?));
        }
        let result = self.policy.prepare(session);
        match self.policy_result(result)? {
            Preparation::Skipped(view) => Ok(DispatchPreparation::Skipped(view)),
            Preparation::Ready(ticket) => {
                let key = JobKey {
                    owner: self.owner.clone(),
                    request_id: ticket.request_id().to_owned(),
                };
                let cancel = crate::cloud::Cancellation::new();
                self.pending.insert(
                    key.request_id.clone(),
                    Pending {
                        ticket: (*ticket).clone(),
                        cancel: cancel.clone(),
                        phase: Phase::Capturing,
                    },
                );
                Ok(DispatchPreparation::Ready(Box::new(StatusCaptureJob {
                    key,
                    ticket: *ticket,
                    cancel,
                })))
            }
        }
    }

    /// Validate exact source attestation, current bindings and job-selected context.
    /// No raw source/metadata/body can be substituted after this token-free stage.
    /// On failure, stop capture and call `acknowledge_capture` to free the slot.
    pub fn build(
        &mut self,
        job: &StatusCaptureJob,
        captured_stamp: crate::status::SourceStamp,
        metadata: CaptureMetadata,
        input: CaptureInput<'_>,
        known_secrets: &[&SecretValue],
    ) -> Result<PreparedStatusDispatch, DispatchError> {
        self.require_current(&job.key, Phase::Capturing)?;
        if captured_stamp != job.source_stamp() {
            job.cancel.cancel();
            return Err(StatusError::InvalidSource.into());
        }
        let request = match ContextBuilder::build_current(
            &mut self.policy,
            &job.ticket,
            metadata,
            input,
            known_secrets,
        ) {
            Ok(request) => request,
            Err(error) => {
                job.cancel.cancel();
                if error == CloudAIError::PolicyUnavailable {
                    self.invalidate();
                }
                return Err(error.into());
            }
        };
        self.require_current(&job.key, Phase::Capturing)?;
        if let Some(pending) = self.pending.get_mut(&job.key.request_id) {
            pending.phase = Phase::Prepared;
        }
        Ok(PreparedStatusDispatch {
            key: job.key.clone(),
            request,
        })
    }

    /// Linearize dispatch commitment with all local controls. The returned worker
    /// is fixed to this client/account/job and contains no bearer while queued.
    /// It must run promptly off-thread; the account intent expires within 30 seconds.
    pub fn commit(
        &mut self,
        prepared: PreparedStatusDispatch,
    ) -> Result<StatusDispatchJob<A, C>, DispatchError> {
        self.require_current(&prepared.key, Phase::Prepared)?;
        let session = self.session.as_ref().ok_or(DispatchError::Canceled)?;
        let lease = match self.account.lease_access_token(session) {
            Ok(Some(lease)) => lease,
            Ok(None) => {
                self.invalidate();
                return Err(DispatchError::Canceled);
            }
            Err(error) => {
                self.invalidate();
                return Err(error.into());
            }
        };
        let result = self.gate.current();
        let Some(binding) = self.policy_result(result)? else {
            self.invalidate();
            return Err(DispatchError::Canceled);
        };
        let pending = self
            .pending
            .get(&prepared.key.request_id)
            .ok_or(DispatchError::Canceled)?;
        if &binding != pending.ticket.account_binding()
            || lease.session().revision().value() != binding.auth_revision
            || lease.session().account().subject() != binding.account.subject
        {
            pending.cancel.cancel();
            return Err(StatusError::AccountChanged.into());
        }
        self.require_current(&prepared.key, Phase::Prepared)?;
        let pending = self
            .pending
            .get_mut(&prepared.key.request_id)
            .ok_or(DispatchError::Canceled)?;
        pending.phase = Phase::Committed;
        Ok(StatusDispatchJob {
            key: prepared.key,
            account: self.account.clone(),
            lease,
            cloud: self.cloud.clone(),
            request: prepared.request,
            cancel: pending.cancel.clone(),
        })
    }

    /// Acknowledge only stopped, uncommitted capture/preparation. Committed workers
    /// must return `run`/`abandon` completion; revocation alone never frees their slot.
    pub fn acknowledge_capture(
        &mut self,
        job: &StatusCaptureJob,
    ) -> Result<ApplyResult, DispatchError> {
        self.check_owner(&job.key)?;
        let Some(pending) = self.pending.get(&job.key.request_id) else {
            return Ok(ApplyResult::Ignored);
        };
        if pending.phase == Phase::Committed {
            return Err(DispatchError::InvalidStage);
        }
        if let Some(pending) = self.pending.remove(&job.key.request_id) {
            pending.cancel.cancel();
            return Ok(self.policy.abandon(&pending.ticket));
        }
        Ok(ApplyResult::Ignored)
    }

    /// Apply one finished worker after all local work stopped. Foreign, revoked,
    /// stale and replayed results cannot expose text or populate the cache.
    pub fn finish(&mut self, completion: DispatchCompletion) -> Result<ApplyResult, DispatchError> {
        self.check_owner(&completion.key)?;
        let Some(pending) = self.pending.get(&completion.key.request_id) else {
            return Ok(ApplyResult::Ignored);
        };
        if pending.phase != Phase::Committed {
            return Err(DispatchError::InvalidStage);
        }
        let result = self.policy.is_current(&pending.ticket);
        let current = match self.policy_result(result) {
            Ok(current) => current,
            Err(error) => {
                if let Some(pending) = self.pending.remove(&completion.key.request_id) {
                    self.policy.abandon(&pending.ticket);
                }
                return Err(error);
            }
        };
        let pending = self
            .pending
            .remove(&completion.key.request_id)
            .ok_or(DispatchError::Canceled)?;
        if !current || !self.confirmed || pending.cancel.is_canceled() {
            self.policy.abandon(&pending.ticket);
            return Ok(ApplyResult::Ignored);
        }
        match completion.outcome {
            Ok(response) => {
                let request_id = response.request_id().to_owned();
                let result =
                    self.policy
                        .complete(&pending.ticket, &request_id, response.into_text());
                self.policy_result(result)
            }
            Err(error) => {
                self.policy.abandon(&pending.ticket);
                Err(error)
            }
        }
    }

    fn unconfirmed_view(&mut self) -> Result<StatusView, DispatchError> {
        if self.consent()?.policy == ConsentPolicy::Disabled {
            return Ok(StatusView::Disabled);
        }
        let result = self.gate.current();
        Ok(if self.policy_result(result)?.is_none() {
            StatusView::SignedOut
        } else {
            StatusView::ConsentRequired
        })
    }

    fn require_current(&mut self, key: &JobKey, phase: Phase) -> Result<(), DispatchError> {
        self.check_owner(key)?;
        let pending = self
            .pending
            .get(&key.request_id)
            .ok_or(DispatchError::Canceled)?;
        if pending.phase != phase {
            return Err(DispatchError::InvalidStage);
        }
        if !self.confirmed || pending.cancel.is_canceled() {
            return Err(DispatchError::Canceled);
        }
        let result = self.policy.is_current(&pending.ticket);
        if !self.policy_result(result)? {
            if let Some(pending) = self.pending.get(&key.request_id) {
                pending.cancel.cancel();
            }
            return Err(DispatchError::Canceled);
        }
        Ok(())
    }

    fn check_owner(&self, key: &JobKey) -> Result<(), DispatchError> {
        if Arc::ptr_eq(&self.owner, &key.owner) {
            Ok(())
        } else {
            Err(DispatchError::ForeignOwner)
        }
    }

    fn policy_result<T>(&mut self, result: Result<T, StatusError>) -> Result<T, DispatchError> {
        result.map_err(|error| {
            self.invalidate();
            error.into()
        })
    }

    fn invalidate(&mut self) {
        self.confirmed = false;
        for pending in self.pending.values() {
            pending.cancel.cancel();
        }
    }
}

impl<S, A, C> Drop for StatusDispatchService<S, A, C> {
    fn drop(&mut self) {
        for pending in self.pending.values() {
            pending.cancel.cancel();
        }
    }
}
