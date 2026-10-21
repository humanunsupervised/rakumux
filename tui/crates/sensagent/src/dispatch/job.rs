use std::fmt;
use std::sync::Arc;

use session_core::ids::SessionId;
use session_core::model::ScopeRoot;
use thiserror::Error;

use crate::auth::{
    AccessTokenLease, AccountError, AccountService, CredentialStore, SystemClock, WallClock,
};
use crate::cloud::{
    Cancellation, CloudAIClient, CloudAIError, StatusResponse, ValidatedStatusRequest,
};
use crate::status::{
    AccountBinding, ConsentRevision, GenerationTicket, InputSelection, SourceStamp, StatusError,
    StatusView,
};

/// Static dispatch diagnostics, never account identifiers, context, URLs or credentials.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum DispatchError {
    /// Local consent/source/cache checks failed.
    #[error(transparent)]
    Policy(#[from] StatusError),
    /// Current account/revision/freshness checks failed.
    #[error(transparent)]
    Account(#[from] AccountError),
    /// Bounded context/transport/protocol checks failed.
    #[error(transparent)]
    Cloud(#[from] CloudAIError),
    /// Offers, jobs and results belong to one fixed-target coordinator instance.
    #[error("the AI-status operation belongs to another local owner")]
    ForeignOwner,
    /// Never dispatch, duplicate or acknowledge a job in the wrong phase.
    #[error("the AI-status operation is in a different phase")]
    InvalidStage,
    /// Revoked, obsolete or already acknowledged work cannot dispatch.
    #[error("the AI-status operation was canceled or is no longer current")]
    Canceled,
}

#[derive(Clone)]
pub(super) struct JobKey {
    pub(super) owner: Arc<()>,
    pub(super) request_id: String,
}

/// Token-free consent intent for one exact account/revision and immutable target.
/// A new coordinator always requires a new offer, even over existing enabled consent.
#[derive(Clone)]
pub struct StatusConsentOffer {
    pub(super) owner: Arc<()>,
    pub(super) origin: String,
    pub(super) account: AccountBinding,
    pub(super) consent: ConsentRevision,
}

impl StatusConsentOffer {
    /// Exact canonical HTTPS destination to show alongside the sharing decision.
    pub fn origin(&self) -> &str {
        &self.origin
    }

    /// Exact token-free account generation that the user's action will approve.
    pub fn account(&self) -> &AccountBinding {
        &self.account
    }

    /// Durable consent generation for rejecting stale user actions.
    pub fn consent_revision(&self) -> ConsentRevision {
        self.consent
    }
}

/// One bounded capture operation, or an honest reason no work should start.
#[derive(Debug)]
pub enum DispatchPreparation {
    /// Trusted capture code may read only these local source/data-kind bindings.
    Ready(Box<StatusCaptureJob>),
    /// No capture or network work is authorized by this result.
    Skipped(StatusView),
}

/// Opaque local capture handle. It contains no raw text or credentials.
/// After capture stops, acknowledge it or commit its prepared body. It is not cloneable.
pub struct StatusCaptureJob {
    pub(super) key: JobKey,
    pub(super) ticket: GenerationTicket,
    pub(super) cancel: Cancellation,
}

impl StatusCaptureJob {
    /// Exact local session for the future scoped SessionApi capture adapter.
    pub fn session_id(&self) -> &SessionId {
        &self.ticket.source().session_id
    }

    /// Canonical local folder; never serialized as cloud context.
    pub fn folder(&self) -> &ScopeRoot {
        &self.ticket.source().folder
    }

    /// Capture must attest this exact terminal incarnation and source revision.
    pub fn source_stamp(&self) -> SourceStamp {
        self.ticket.source().stamp
    }

    /// Reports-only selection never permits reading terminal output as a fallback.
    pub fn input_selection(&self) -> InputSelection {
        self.ticket.input_selection()
    }

    /// Capture should stop when revoked; acknowledgement must follow actual stop.
    pub fn is_canceled(&self) -> bool {
        self.cancel.is_canceled()
    }
}

/// Bounded, validated, token-free body awaiting the coordinator's dispatch commitment.
/// No public constructor, raw-body accessor, clone or serialization is provided.
///
/// ```compile_fail
/// use rakumux_sensagent::dispatch::PreparedStatusDispatch;
/// fn duplicate(body: &PreparedStatusDispatch) -> PreparedStatusDispatch { body.clone() }
/// ```
pub struct PreparedStatusDispatch {
    pub(super) key: JobKey,
    pub(super) request: ValidatedStatusRequest,
}

/// One committed worker for a fixed target and exact account-use intent.
///
/// Run off the UI/coordinator thread. No bearer is retained while queued: `run`
/// rereads current credentials and consumes a short-lived account intent. Revocation
/// after commitment cancels locally but cannot retract already accepted remote work.
/// Dropping a worker does not acknowledge its slot; use `abandon` after work stops.
pub struct StatusDispatchJob<A, C = SystemClock> {
    pub(super) key: JobKey,
    pub(super) account: Arc<AccountService<A, C>>,
    pub(super) lease: AccessTokenLease,
    pub(super) cloud: Arc<CloudAIClient>,
    pub(super) request: ValidatedStatusRequest,
    pub(super) cancel: Cancellation,
}

/// Opaque, token-free worker completion. Only the issuing coordinator may apply it.
/// Successful text is exposed only through the coordinator's current-policy cache view.
///
/// ```compile_fail
/// use rakumux_sensagent::dispatch::DispatchCompletion;
/// fn export(result: DispatchCompletion) { let _json = serde_json::to_vec(&result); }
/// ```
pub struct DispatchCompletion {
    pub(super) key: JobKey,
    pub(super) outcome: Result<StatusResponse, DispatchError>,
}

impl<A: CredentialStore, C: WallClock> StatusDispatchJob<A, C> {
    /// Make at most one native client call, or discard work before I/O if revoked,
    /// stale, signed out or expired. Always return through the coordinator's `finish`.
    pub fn run(self) -> DispatchCompletion {
        let outcome = if self.cancel.is_canceled() {
            Err(DispatchError::Canceled)
        } else {
            self.account
                .consume_access_token(self.lease, |token| {
                    self.cloud.generate(token, &self.request, &self.cancel)
                })
                .map_err(DispatchError::Account)
                .and_then(|result| match result {
                    Some(result) => result.map_err(DispatchError::Cloud),
                    None => Err(DispatchError::Canceled),
                })
        };
        DispatchCompletion {
            key: self.key,
            outcome: if self.cancel.is_canceled() {
                Err(DispatchError::Canceled)
            } else {
                outcome
            },
        }
    }

    /// Acknowledge a worker that will not run. This consumes it without network I/O.
    pub fn abandon(self) -> DispatchCompletion {
        self.cancel.cancel();
        DispatchCompletion {
            key: self.key,
            outcome: Err(DispatchError::Canceled),
        }
    }
}

impl fmt::Debug for StatusConsentOffer {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("StatusConsentOffer { bindings: [REDACTED] }")
    }
}

impl fmt::Debug for StatusCaptureJob {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("StatusCaptureJob { bindings: [REDACTED] }")
    }
}

impl fmt::Debug for PreparedStatusDispatch {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("PreparedStatusDispatch { context: [REDACTED] }")
    }
}

impl<A, C> fmt::Debug for StatusDispatchJob<A, C> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("StatusDispatchJob { bindings_and_context: [REDACTED] }")
    }
}

impl fmt::Debug for DispatchCompletion {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("DispatchCompletion { result: [REDACTED] }")
    }
}
