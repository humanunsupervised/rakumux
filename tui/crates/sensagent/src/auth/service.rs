use std::time::Duration;

use super::model::ProviderIdentity;
use super::{
    AuthError, AuthorizationGrant, DeviceAuthorization, PollOutcome, SecretValue,
    VerificationPrompt,
};

/// Unique authorization-attempt generation within this service lifetime.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AttemptId(u64);

/// Identity of one in-flight poll, protecting against duplicate / late responses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PollId {
    attempt: AttemptId,
    sequence: u64,
}

/// A private-code-bearing request for a background network worker, never UI text.
#[derive(Debug)]
pub struct PollRequest {
    id: PollId,
    pub(crate) provider: ProviderIdentity,
    pub(crate) device_code: SecretValue,
}

impl PollRequest {
    /// Identity to return with the completed request.
    pub fn id(&self) -> PollId {
        self.id
    }
}

/// Safe local status. ProviderAuthorized deliberately does not mean SignedIn.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AuthStatus {
    /// No attempt has started.
    Idle,
    /// A background request is obtaining the provider challenge.
    Starting(AttemptId),
    /// User confirmation is pending; fields contain no private device code.
    AwaitingApproval {
        /// Current attempt generation.
        attempt: AttemptId,
        /// Trusted public URLs / code for the user's browser.
        prompt: Box<VerificationPrompt>,
        /// Monotonic deadline relative to the caller's clock origin.
        expires_at: Duration,
        /// Earliest next poll, even if a worker asks sooner.
        next_poll_at: Duration,
        /// Whether exactly one poll is already in flight.
        poll_in_flight: bool,
        /// Most recent transient failure, without secrets or provider-body text.
        last_error: Option<AuthError>,
    },
    /// Provider approved; account validation / secure persistence must still finish.
    ProviderAuthorized(AttemptId),
    /// User denied the provider request.
    Denied,
    /// Provider or local monotonic deadline expired.
    Expired,
    /// The local user cancelled this attempt.
    Cancelled,
    /// A visible fatal configuration / protocol error.
    Failed(AuthError),
}

/// Whether an asynchronous challenge result belongs to the current attempt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ApplyResult {
    /// The current attempt consumed the result.
    Applied,
    /// A cancelled / replaced attempt's response was ignored.
    Ignored,
}

/// Poll application result. A stale grant is returned for safe disposal / revocation.
#[derive(Debug)]
pub enum PollCompletion {
    /// Current progress / denial / expiry / failure was applied.
    Applied,
    /// Provider credentials need separate account validation and private storage.
    Authorized(AuthorizationGrant),
    /// A non-credential response belongs to an obsolete request.
    Ignored,
    /// Never persist these credentials; the attempt was cancelled / replaced / expired.
    DiscardedGrant(AuthorizationGrant),
}

struct Pending {
    attempt: AttemptId,
    authorization: DeviceAuthorization,
    expires_at: Duration,
    next_poll_at: Duration,
    interval: Duration,
    network_backoff: Option<Duration>,
    last_error: Option<AuthError>,
    in_flight: Option<PollId>,
}

enum State {
    Idle,
    Starting(AttemptId),
    Pending(Box<Pending>),
    Finished(AuthStatus),
}

/// Local, I/O-free device-authorization coordinator.
///
/// Pass elapsed time from one monotonic clock origin, not Unix wall time.
/// Prepare network work here, execute it outside the UI / service lock, then
/// apply its generation-tagged response. No timers, credentials or auth flags
/// are installed into core Rakumux by constructing this service.
pub struct AuthService {
    state: State,
    generation: u64,
    poll_sequence: u64,
}

impl Default for AuthService {
    fn default() -> Self {
        Self {
            state: State::Idle,
            generation: 0,
            poll_sequence: 0,
        }
    }
}

impl AuthService {
    /// Begin / replace an attempt before scheduling the provider's start call.
    pub fn start(&mut self) -> Result<AttemptId, AuthError> {
        self.generation = self
            .generation
            .checked_add(1)
            .ok_or(AuthError::GenerationExhausted)?;
        let attempt = AttemptId(self.generation);
        self.state = State::Starting(attempt);
        Ok(attempt)
    }

    /// Accept a background challenge response only for the still-current attempt.
    pub fn complete_start(
        &mut self,
        attempt: AttemptId,
        result: Result<DeviceAuthorization, AuthError>,
        now: Duration,
    ) -> ApplyResult {
        if !matches!(self.state, State::Starting(current) if current == attempt) {
            return ApplyResult::Ignored;
        }
        let authorization = match result {
            Ok(value) => value,
            Err(error) => {
                self.state = State::Finished(AuthStatus::Failed(error));
                return ApplyResult::Applied;
            }
        };
        let deadlines = now
            .checked_add(authorization.expires_in)
            .zip(now.checked_add(authorization.interval));
        let (expires_at, next_poll_at) = match deadlines {
            Some(value) => value,
            None => {
                self.state = State::Finished(AuthStatus::Failed(AuthError::InvalidResponse(
                    "device deadline overflow",
                )));
                return ApplyResult::Applied;
            }
        };
        self.state = State::Pending(Box::new(Pending {
            attempt,
            interval: authorization.interval,
            authorization,
            expires_at,
            next_poll_at,
            network_backoff: None,
            last_error: None,
            in_flight: None,
        }));
        ApplyResult::Applied
    }

    /// Return public display state, expiring any elapsed pending authorization first.
    pub fn status(&mut self, now: Duration) -> AuthStatus {
        self.expire(now);
        match &self.state {
            State::Idle => AuthStatus::Idle,
            State::Starting(attempt) => AuthStatus::Starting(*attempt),
            State::Pending(pending) => AuthStatus::AwaitingApproval {
                attempt: pending.attempt,
                prompt: Box::new(pending.authorization.prompt.clone()),
                expires_at: pending.expires_at,
                next_poll_at: pending.next_poll_at,
                poll_in_flight: pending.in_flight.is_some(),
                last_error: pending.last_error.clone(),
            },
            State::Finished(status) => status.clone(),
        }
    }

    /// Prepare at most one request, and never before the provider minimum interval.
    pub fn next_poll(&mut self, now: Duration) -> Option<PollRequest> {
        self.expire(now);
        let pending = match &mut self.state {
            State::Pending(pending)
                if pending.in_flight.is_none() && now >= pending.next_poll_at =>
            {
                pending
            }
            _ => return None,
        };
        self.poll_sequence = match self.poll_sequence.checked_add(1) {
            Some(sequence) => sequence,
            None => {
                self.state = State::Finished(AuthStatus::Failed(AuthError::GenerationExhausted));
                return None;
            }
        };
        let id = PollId {
            attempt: pending.attempt,
            sequence: self.poll_sequence,
        };
        pending.in_flight = Some(id);
        Some(PollRequest {
            id,
            provider: pending.authorization.provider.clone(),
            device_code: pending.authorization.device_code.clone(),
        })
    }

    /// Apply only the current in-flight poll. Late grants must never sign in a user.
    pub fn complete_poll(
        &mut self,
        id: PollId,
        outcome: PollOutcome,
        now: Duration,
    ) -> PollCompletion {
        self.expire(now);
        let pending = match &mut self.state {
            State::Pending(pending) if pending.in_flight == Some(id) => pending,
            _ => return discard(outcome),
        };
        pending.in_flight = None;
        let delay = match outcome {
            PollOutcome::Granted(grant) => {
                self.state = State::Finished(AuthStatus::ProviderAuthorized(id.attempt));
                return PollCompletion::Authorized(grant);
            }
            PollOutcome::Denied => {
                self.state = State::Finished(AuthStatus::Denied);
                return PollCompletion::Applied;
            }
            PollOutcome::Expired => {
                self.state = State::Finished(AuthStatus::Expired);
                return PollCompletion::Applied;
            }
            PollOutcome::Rejected(error) => {
                self.state = State::Finished(AuthStatus::Failed(error));
                return PollCompletion::Applied;
            }
            PollOutcome::Pending => {
                pending.network_backoff = None;
                pending.last_error = None;
                pending.interval
            }
            PollOutcome::SlowDown => {
                pending.network_backoff = None;
                pending.last_error = None;
                pending.interval = pending.interval.saturating_add(Duration::from_secs(5));
                pending.interval
            }
            PollOutcome::Retryable { error, retry_after } => {
                pending.last_error = Some(error);
                let backoff = pending
                    .network_backoff
                    .map(|delay| delay.saturating_mul(2))
                    .unwrap_or(pending.interval.max(Duration::from_secs(5)))
                    .min(Duration::from_secs(60))
                    .max(pending.interval);
                pending.network_backoff = Some(backoff);
                backoff.max(retry_after.unwrap_or_default())
            }
        };
        match now.checked_add(delay) {
            Some(deadline) => pending.next_poll_at = deadline,
            None => {
                self.state = State::Finished(AuthStatus::Failed(AuthError::InvalidResponse(
                    "poll deadline overflow",
                )))
            }
        }
        PollCompletion::Applied
    }

    /// Cancel without blocking on a network worker; later results are obsolete.
    pub fn cancel(&mut self) {
        self.state = State::Finished(AuthStatus::Cancelled);
    }

    fn expire(&mut self, now: Duration) {
        if matches!(&self.state, State::Pending(pending) if now >= pending.expires_at) {
            self.state = State::Finished(AuthStatus::Expired);
        }
    }
}

fn discard(outcome: PollOutcome) -> PollCompletion {
    match outcome {
        PollOutcome::Granted(grant) => PollCompletion::DiscardedGrant(grant),
        _ => PollCompletion::Ignored,
    }
}
