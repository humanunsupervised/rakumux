use std::collections::{HashMap, HashSet};
use std::fmt;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use session_core::ids::SessionId;
use thiserror::Error;

use super::model::{validate_folder, validate_session};
use super::{
    AccountBinding, AccountGate, CachedStatus, ConsentPolicy, ConsentRevision, ConsentSnapshot,
    FolderConsent, InputSelection, StatusSource, StatusStore, StatusStoreError, StatusText,
    MAX_CACHE_ENTRIES,
};
use crate::auth::ApplyResult;

/// Injectable local metadata clock. Authentication freshness belongs to AccountGate.
pub trait StatusClock: Send {
    /// Unix seconds for local capture metadata, never a provider authorization proof.
    fn now_seconds(&self) -> Result<u64, StatusError>;
}

/// Production local metadata clock.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemStatusClock;

impl StatusClock for SystemStatusClock {
    fn now_seconds(&self) -> Result<u64, StatusError> {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|time| time.as_secs())
            .map_err(|_| StatusError::ClockChanged)
    }
}

/// Static failures, with no raw context, account subject, credential or filesystem path.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum StatusError {
    /// Private persistence failed or a newer owner decision won.
    #[error(transparent)]
    Store(#[from] StatusStoreError),
    /// Authentication could not be checked; fail closed for AI only.
    #[error("current account authentication is unavailable")]
    AccountUnavailable,
    /// A signed-in account without the required OAuth feature scope cannot share context.
    #[error("the current account is not authorized for AI session status")]
    FeatureUnauthorized,
    /// Explicit consent cannot be enabled without a verified current account.
    #[error("sign in before enabling AI session status")]
    SignedOut,
    /// A consent action was prepared for another account/credential generation.
    #[error("the account changed; confirm AI-status consent again")]
    AccountChanged,
    /// A local source update violates ordered incarnation/revision invariants.
    #[error("invalid or inconsistent local AI-status source revision")]
    InvalidSource,
    /// Local bookkeeping is bounded independently of any hosted quota.
    #[error("the local AI-status source limit was reached")]
    Capacity,
    /// Correlation values must use operating-system randomness, not session identifiers.
    #[error("unable to obtain randomness for AI-status correlation")]
    RandomUnavailable,
    /// The response did not match its original request.
    #[error("AI-status response correlation did not match")]
    InvalidResponse,
    /// Invalid wall time cannot become capture metadata.
    #[error("the local AI-status clock is invalid")]
    ClockChanged,
}

/// Honest local view states. No state changes native session lifecycle or terminal size.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StatusView {
    /// No explicit sharing decision.
    Disabled,
    /// No current verified account.
    SignedOut,
    /// Consent is bound to a different account/credential generation.
    ConsentRequired,
    /// This folder was not explicitly selected.
    Excluded,
    /// Private sessions never generate or reveal cache text.
    Private,
    /// This first slice generates only for live sessions.
    NotLive,
    /// No meaningful allowed report/output exists.
    NoContext,
    /// A matching job is outstanding or local slots are occupied.
    Pending,
    /// No observed source/cache exists.
    Missing,
    /// Old output is hidden because source/current bindings differ.
    Stale,
    /// Matching account/consent/source advisory output, never an execution result.
    Ready(Box<CachedStatus>),
}

/// One prepared generation, or a reason no data should be sent.
#[derive(Clone, Debug)]
pub enum Preparation {
    /// A future worker may capture only the ticket's allowed fields and run one request.
    Ready(Box<GenerationTicket>),
    /// No cloud work may be submitted for this decision.
    Skipped(StatusView),
}

/// Opaque service-owned local fencing. Never serialize this into a cloud body.
#[derive(Clone)]
pub struct GenerationTicket {
    owner: Arc<()>,
    request_id: String,
    source: StatusSource,
    binding: AccountBinding,
    consent: ConsentRevision,
    selection: InputSelection,
}

impl GenerationTicket {
    /// Fresh public correlation; unrelated to local session/account/cache identifiers.
    pub fn request_id(&self) -> &str {
        &self.request_id
    }
    /// Explicit permitted data kinds. Raw text still needs bounding/redaction/validation.
    pub fn input_selection(&self) -> InputSelection {
        self.selection
    }

    pub(crate) fn source(&self) -> &StatusSource {
        &self.source
    }

    pub(crate) fn account_binding(&self) -> &AccountBinding {
        &self.binding
    }
}

impl fmt::Debug for GenerationTicket {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("GenerationTicket { local_bindings: [REDACTED] }")
    }
}

struct Observed {
    source: StatusSource,
    valid: bool,
}

struct Eligible {
    source: StatusSource,
    binding: AccountBinding,
    consent: ConsentRevision,
    selection: InputSelection,
}

enum Decision {
    Eligible(Box<Eligible>),
    Skipped(StatusView),
}

/// Local consent/cache policy for a future single credential/feature owner.
///
/// This service performs no network, PTY, registry, UI or provider operations. Its
/// embedding owner must authorize user-only mutations and attest canonical session
/// observations. It is not an IPC permission boundary or a running scheduler.
pub struct AISessionStatusService<S, A, C = SystemStatusClock> {
    owner: Arc<()>,
    store: S,
    account: A,
    clock: C,
    sources: HashMap<SessionId, Observed>,
    retired: HashSet<SessionId>,
    pending: HashMap<String, GenerationTicket>,
}

impl<S: StatusStore, A: AccountGate> AISessionStatusService<S, A, SystemStatusClock> {
    /// Construct without reading home data or enabling a feature implicitly.
    pub fn new(store: S, account: A) -> Self {
        Self::with_clock(store, account, SystemStatusClock)
    }
}

impl<S: StatusStore, A: AccountGate, C: StatusClock> AISessionStatusService<S, A, C> {
    /// Inject an explicit store/account gate and local metadata clock.
    pub fn with_clock(store: S, account: A, clock: C) -> Self {
        Self {
            owner: Arc::new(()),
            store,
            account,
            clock,
            sources: HashMap::new(),
            retired: HashSet::new(),
            pending: HashMap::new(),
        }
    }

    /// Current durable consent revision for compare-and-swap UI actions.
    pub fn consent(&mut self) -> Result<ConsentSnapshot, StatusError> {
        let snapshot = self.store.load_consent()?;
        snapshot.validate()?;
        Ok(snapshot)
    }

    /// Explicit local-owner opt-in for the exact account shown by the consent UI.
    /// Sign-in itself never calls this method; stale account intents cannot retarget.
    pub fn enable(
        &mut self,
        expected: ConsentRevision,
        expected_account: &AccountBinding,
        folders: Vec<FolderConsent>,
    ) -> Result<ConsentSnapshot, StatusError> {
        expected_account.validate()?;
        let binding = self.current_account()?.ok_or(StatusError::SignedOut)?;
        if &binding != expected_account {
            return Err(StatusError::AccountChanged);
        }
        let policy = ConsentPolicy::Enabled {
            account: binding.account.clone(),
            auth_revision: binding.auth_revision,
            folders,
        };
        policy.validate()?;
        let saved = self.store.compare_and_swap_consent(expected, &policy)?;
        let failure = match self.current_account() {
            Ok(Some(current)) if current == binding => None,
            Ok(Some(_)) => Some(StatusError::AccountChanged),
            Ok(None) => Some(StatusError::SignedOut),
            Err(error) => Some(error),
        };
        if let Some(failure) = failure {
            match self
                .store
                .compare_and_swap_consent(saved.revision, &ConsentPolicy::Disabled)
            {
                Ok(_) | Err(StatusStoreError::Conflict) => return Err(failure),
                Err(error) => return Err(error.into()),
            }
        }
        Ok(saved)
    }

    /// Explicit opt-out. A stale UI revision cannot overwrite a newer decision.
    pub fn disable(&mut self, expected: ConsentRevision) -> Result<ConsentSnapshot, StatusError> {
        Ok(self
            .store
            .compare_and_swap_consent(expected, &ConsentPolicy::Disabled)?)
    }

    /// Apply a trusted local observation; late older snapshots cannot replace newer ones.
    pub fn observe(&mut self, source: StatusSource) -> Result<ApplyResult, StatusError> {
        validate_folder(&source.folder)?;
        validate_session(&source.session_id)?;
        if self.retired.contains(&source.session_id) {
            return Ok(ApplyResult::Ignored);
        }
        if source.stamp.terminal_generation == 0 || source.stamp.source_revision == 0 {
            return Err(StatusError::InvalidSource);
        }
        if let Some(previous) = self.sources.get_mut(&source.session_id) {
            if source.stamp < previous.source.stamp {
                return Ok(ApplyResult::Ignored);
            }
            if source.stamp == previous.source.stamp {
                if source == previous.source {
                    return Ok(ApplyResult::Ignored);
                }
                let mut tightened = previous.source.clone();
                tightened.private = true;
                if !previous.source.private && source == tightened {
                    previous.source = source.clone();
                    self.store.remove_cache(&source.session_id)?;
                    return Ok(ApplyResult::Applied);
                }
                previous.valid = false;
                return Err(StatusError::InvalidSource);
            }
        } else if self.sources.len() + self.retired.len() >= MAX_CACHE_ENTRIES {
            return Err(StatusError::Capacity);
        }
        let private = source.private;
        let session = source.session_id.clone();
        self.sources.insert(
            session.clone(),
            Observed {
                source,
                valid: true,
            },
        );
        if private {
            self.store.remove_cache(&session)?;
        }
        Ok(ApplyResult::Applied)
    }

    /// Retire a daemon-removed session for this owner's lifetime, including delayed
    /// observations and tickets. Not an offscreen eviction API. Never operates on a PTY.
    pub fn remove(&mut self, session: &SessionId) -> Result<(), StatusError> {
        validate_session(session)?;
        if !self.sources.contains_key(session)
            && !self.retired.contains(session)
            && self.sources.len() + self.retired.len() >= MAX_CACHE_ENTRIES
        {
            return Err(StatusError::Capacity);
        }
        self.sources.remove(session);
        self.retired.insert(session.clone());
        Ok(self.store.remove_cache(session)?)
    }

    /// Read a view only after current account, consent, privacy and source checks.
    pub fn view(&mut self, session: &SessionId) -> Result<StatusView, StatusError> {
        let decision = self.decision(session)?;
        match decision {
            Decision::Skipped(view) => Ok(view),
            Decision::Eligible(eligible) => self.eligible_view(&eligible),
        }
    }

    /// Prepare at most two unacknowledged jobs, with no duplicate work for one source.
    /// Invalidated jobs keep occupying a slot until completion or acknowledged abandon.
    pub fn prepare(&mut self, session: &SessionId) -> Result<Preparation, StatusError> {
        let eligible = match self.decision(session)? {
            Decision::Skipped(view) => return Ok(Preparation::Skipped(view)),
            Decision::Eligible(eligible) => eligible,
        };
        let view = self.eligible_view(&eligible)?;
        if !matches!(view, StatusView::Missing | StatusView::Stale) {
            return Ok(Preparation::Skipped(view));
        }
        if self.pending.len() >= 2 {
            return Ok(Preparation::Skipped(StatusView::Pending));
        }
        let mut random = [0u8; 16];
        getrandom::getrandom(&mut random).map_err(|_| StatusError::RandomUnavailable)?;
        let request_id: String = random.iter().map(|byte| format!("{byte:02x}")).collect();
        let ticket = GenerationTicket {
            owner: self.owner.clone(),
            request_id: request_id.clone(),
            source: eligible.source,
            binding: eligible.binding,
            consent: eligible.consent,
            selection: eligible.selection,
        };
        self.pending.insert(request_id, ticket.clone());
        Ok(Preparation::Ready(Box::new(ticket)))
    }

    /// Recheck immediately before capture/submission. The future owner must serialize
    /// dispatch with opt-out and stop/cancel revoked workers; this is not a network lease.
    pub fn is_current(&mut self, ticket: &GenerationTicket) -> Result<bool, StatusError> {
        if !Arc::ptr_eq(&self.owner, &ticket.owner)
            || !self.pending.contains_key(&ticket.request_id)
        {
            return Ok(false);
        }
        match self.decision(&ticket.source.session_id)? {
            Decision::Skipped(_) => Ok(false),
            Decision::Eligible(eligible) => Ok(eligible.source == ticket.source
                && eligible.binding == ticket.binding
                && eligible.consent == ticket.consent),
        }
    }

    /// Acknowledge a finished/cancelled worker; callers must stop work before freeing its slot.
    pub fn abandon(&mut self, ticket: &GenerationTicket) -> ApplyResult {
        if !Arc::ptr_eq(&self.owner, &ticket.owner) {
            return ApplyResult::Ignored;
        }
        if self.pending.remove(&ticket.request_id).is_some() {
            ApplyResult::Applied
        } else {
            ApplyResult::Ignored
        }
    }

    /// Cache only a matching still-current validated result; late/foreign/replayed results are ignored.
    pub fn complete(
        &mut self,
        ticket: &GenerationTicket,
        response_request_id: &str,
        text: StatusText,
    ) -> Result<ApplyResult, StatusError> {
        if !Arc::ptr_eq(&self.owner, &ticket.owner)
            || self.pending.remove(&ticket.request_id).is_none()
        {
            return Ok(ApplyResult::Ignored);
        }
        if response_request_id != ticket.request_id {
            return Err(StatusError::InvalidResponse);
        }
        let eligible = match self.decision(&ticket.source.session_id)? {
            Decision::Skipped(_) => return Ok(ApplyResult::Ignored),
            Decision::Eligible(eligible) => eligible,
        };
        if eligible.source != ticket.source
            || eligible.binding != ticket.binding
            || eligible.consent != ticket.consent
        {
            return Ok(ApplyResult::Ignored);
        }
        let entry = CachedStatus {
            account: ticket.binding.account.clone(),
            auth_revision: ticket.binding.auth_revision,
            consent_revision: ticket.consent,
            session_id: ticket.source.session_id.clone(),
            folder: ticket.source.folder.clone(),
            stamp: ticket.source.stamp,
            request_id: ticket.request_id.clone(),
            text,
            captured_at: self.clock.now_seconds()?,
        };
        match self.store.save_cache(ticket.consent, &entry) {
            Ok(()) => {}
            Err(StatusStoreError::Conflict) => return Ok(ApplyResult::Ignored),
            Err(error) => return Err(error.into()),
        }
        if self.current_account()?.as_ref() != Some(&ticket.binding)
            || self.consent()?.revision != ticket.consent
        {
            return Ok(ApplyResult::Ignored);
        }
        Ok(ApplyResult::Applied)
    }

    fn current_account(&self) -> Result<Option<AccountBinding>, StatusError> {
        let current = self.account.current()?;
        if let Some(binding) = &current {
            binding.validate()?;
        }
        Ok(current)
    }

    fn decision(&mut self, session: &SessionId) -> Result<Decision, StatusError> {
        let consent = self.consent()?;
        let ConsentPolicy::Enabled {
            account,
            auth_revision,
            folders,
        } = &consent.policy
        else {
            return Ok(Decision::Skipped(StatusView::Disabled));
        };
        let Some(binding) = self.current_account()? else {
            return Ok(Decision::Skipped(StatusView::SignedOut));
        };
        if binding.account != *account || binding.auth_revision != *auth_revision {
            return Ok(Decision::Skipped(StatusView::ConsentRequired));
        }
        let Some(observed) = self.sources.get(session) else {
            return Ok(Decision::Skipped(StatusView::Missing));
        };
        if !observed.valid {
            return Ok(Decision::Skipped(StatusView::Stale));
        }
        let source = &observed.source;
        if source.private {
            return Ok(Decision::Skipped(StatusView::Private));
        }
        if !source.live {
            return Ok(Decision::Skipped(StatusView::NotLive));
        }
        let Some(folder) = folders.iter().find(|folder| folder.folder == source.folder) else {
            return Ok(Decision::Skipped(StatusView::Excluded));
        };
        let selection = if folder.include_terminal_output {
            InputSelection::ReportAndOutput
        } else {
            InputSelection::ReportOnly
        };
        if !source.context.reported_text
            && !(folder.include_terminal_output && source.context.recent_output)
        {
            return Ok(Decision::Skipped(StatusView::NoContext));
        }
        Ok(Decision::Eligible(Box::new(Eligible {
            source: source.clone(),
            binding,
            consent: consent.revision,
            selection,
        })))
    }

    fn eligible_view(&mut self, eligible: &Eligible) -> Result<StatusView, StatusError> {
        if self.pending.values().any(|ticket| {
            ticket.source == eligible.source
                && ticket.binding == eligible.binding
                && ticket.consent == eligible.consent
        }) {
            return Ok(StatusView::Pending);
        }
        let cached = self.store.load_cache(&eligible.source.session_id)?;
        match self.current_account()? {
            None => return Ok(StatusView::SignedOut),
            Some(binding) if binding != eligible.binding => return Ok(StatusView::ConsentRequired),
            Some(_) => {}
        }
        let current_consent = self.consent()?;
        if current_consent.revision != eligible.consent {
            return Ok(if current_consent.policy == ConsentPolicy::Disabled {
                StatusView::Disabled
            } else {
                StatusView::Stale
            });
        }
        if let Some(entry) = cached {
            entry.validate()?;
            if entry.session_id != eligible.source.session_id {
                return Err(StatusStoreError::InvalidData.into());
            }
            if entry.account == eligible.binding.account
                && entry.auth_revision == eligible.binding.auth_revision
                && entry.consent_revision == eligible.consent
                && entry.folder == eligible.source.folder
                && entry.stamp == eligible.source.stamp
            {
                return Ok(StatusView::Ready(Box::new(entry)));
            }
            return Ok(StatusView::Stale);
        }
        Ok(StatusView::Missing)
    }
}

#[cfg(test)]
mod tests;
