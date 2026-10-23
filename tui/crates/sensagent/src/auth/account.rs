use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use thiserror::Error;

use super::credentials::unix_seconds;
use super::model::ProviderIdentity;
use super::{
    AccountIdentity, ApplyResult, AuthError, ClerkDeviceClient, CredentialRevision,
    CredentialStore, CredentialStoreError, DeviceAuthConfig, SavedCredentials,
    VerifiedAuthorization,
};

mod access;
pub use access::AccessTokenLease;

/// Injectable clocks for persisted expiry and a running service's lifetime bound.
pub trait WallClock: Send + Sync {
    /// Current wall time. Observed rollback is rejected instead of extending credentials.
    fn now(&self) -> SystemTime;

    /// Current monotonic time. Tests may override this with a controlled steady origin.
    fn monotonic_now(&self) -> Instant {
        Instant::now()
    }
}

/// Production clock, separate from the device coordinator's monotonic deadline origin.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemClock;

impl WallClock for SystemClock {
    fn now(&self) -> SystemTime {
        SystemTime::now()
    }
}

/// Failures safe to display without stored credentials or provider-response bodies.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum AccountError {
    /// Provider verification / trust checks failed.
    #[error(transparent)]
    Provider(#[from] AuthError),
    /// Private persistence / stale-generation checks failed.
    #[error(transparent)]
    Store(#[from] CredentialStoreError),
    /// The configured issuer / public client / scopes do not match cached credentials.
    #[error("credentials do not match the configured identity provider and scopes")]
    ProviderMismatch,
    /// A login ticket belongs to another service instance, even over the same store.
    #[error("the login operation belongs to another account service")]
    OwnerMismatch,
    /// A restored token resolves to a different subject than the cached owner.
    #[error("the verified account does not match the stored account")]
    AccountMismatch,
    /// An expired cached token cannot be reported as authenticated.
    #[error("the access token expired; authentication must be refreshed or restarted")]
    Expired,
    /// Persisted timestamps cannot be trusted under a backwards clock.
    #[error("the system clock moved backwards or predates the saved credentials")]
    ClockChanged,
    /// A poisoned clock-state lock cannot safely establish freshness.
    #[error("credential freshness could not be established")]
    ClockUnavailable,
    /// A prepared token use must be reissued instead of waiting indefinitely in a queue.
    #[error("the prepared access-token use expired")]
    TokenLeaseExpired,
}

/// A generation-tagged local completion intent, not a bearer credential.
#[derive(Clone, Debug)]
pub struct LoginTicket {
    owner: Arc<()>,
    provider: ProviderIdentity,
    revision: CredentialRevision,
}

/// Verified and persisted account view with no OAuth tokens.
///
/// Consumers must still reject stale async UI events using the returned revision.
/// This view is not an authorization substitute for the provider-backed API.
#[derive(Clone, Debug)]
pub struct AccountSession {
    owner: Arc<()>,
    account: AccountIdentity,
    revision: CredentialRevision,
    expires_at: u64,
    monotonic_deadline: Instant,
}

impl AccountSession {
    /// Verified current account identity for explicit UI rendering.
    pub fn account(&self) -> &AccountIdentity {
        &self.account
    }
    /// Generation to reject completion after sign-out / a newer account operation.
    pub fn revision(&self) -> CredentialRevision {
        self.revision
    }
    /// Cached expiry; online validity is still checked by the API.
    pub fn expires_at(&self) -> u64 {
        self.expires_at
    }
}

/// Local account completion / restoration. Network work runs outside store locks.
pub struct AccountService<S, C = SystemClock> {
    owner: Arc<()>,
    config: DeviceAuthConfig,
    store: S,
    clock: C,
    observation: Mutex<Option<ClockObservation>>,
}

struct ClockObservation {
    wall: SystemTime,
    monotonic: Instant,
}

impl<S: CredentialStore> AccountService<S, SystemClock> {
    /// Use the system wall clock and an explicitly selected private storage backend.
    pub fn new(config: DeviceAuthConfig, store: S) -> Self {
        Self::with_clock(config, store, SystemClock)
    }
}

impl<S: CredentialStore, C: WallClock> AccountService<S, C> {
    /// Reviewed public provider configuration; contains no credentials.
    pub fn config(&self) -> &DeviceAuthConfig {
        &self.config
    }

    /// Inject a clock for deterministic expiry / rollback testing.
    pub fn with_clock(config: DeviceAuthConfig, store: S, clock: C) -> Self {
        Self {
            owner: Arc::new(()),
            config,
            store,
            clock,
            observation: Mutex::new(None),
        }
    }

    /// Fence older work while preserving any existing account during a new login.
    pub fn begin_login(&self) -> Result<LoginTicket, AccountError> {
        let snapshot = self.store.load()?;
        let revision = self
            .store
            .compare_and_swap(snapshot.revision, snapshot.credentials.as_ref())?;
        Ok(LoginTicket {
            owner: self.owner.clone(),
            provider: ProviderIdentity::from(&self.config),
            revision,
        })
    }

    /// Persist only provider-verified credentials for the still-current login intent.
    ///
    /// The provider client stamps receipt before account verification. Verification
    /// and persistence latency cannot reset the grant's monotonic lifetime.
    pub fn commit_login(
        &self,
        ticket: &LoginTicket,
        verified: VerifiedAuthorization,
    ) -> Result<AccountSession, AccountError> {
        self.check_ticket(ticket)?;
        let mut credentials = SavedCredentials::from_verified(verified)?;
        self.check_credentials(&mut credentials)?;
        self.save(ticket.revision, &mut credentials)
    }

    /// Invalidate an unfinished login without signing out an existing stored account.
    pub fn cancel_login(&self, ticket: &LoginTicket) -> Result<ApplyResult, AccountError> {
        self.check_ticket(ticket)?;
        let snapshot = self.store.load()?;
        if snapshot.revision != ticket.revision {
            return Ok(ApplyResult::Ignored);
        }
        match self
            .store
            .compare_and_swap(ticket.revision, snapshot.credentials.as_ref())
        {
            Ok(_) => Ok(ApplyResult::Applied),
            Err(CredentialStoreError::Conflict) => Ok(ApplyResult::Ignored),
            Err(error) => Err(error.into()),
        }
    }

    /// Clear private local credentials and retain a tombstone generation.
    ///
    /// This does not revoke a remote OAuth grant or log out a browser; those are
    /// separate provider operations. On storage error, stop AI use until resolved.
    pub fn sign_out(&self) -> Result<CredentialRevision, AccountError> {
        let snapshot = self.store.load()?;
        Ok(self.store.compare_and_swap(snapshot.revision, None)?)
    }

    /// Restore only after live account verification and a successful revision check.
    /// An access token that expired meanwhile is renewed with its refresh token.
    pub fn restore(
        &self,
        client: &ClerkDeviceClient,
    ) -> Result<Option<AccountSession>, AccountError> {
        let snapshot = self.store.load()?;
        let Some(mut credentials) = snapshot.credentials else {
            return Ok(None);
        };
        match self.check_credentials(&mut credentials) {
            Err(AccountError::Expired) => {
                return self
                    .renew_credentials(snapshot.revision, credentials, client)
                    .map(Some);
            }
            result => {
                result?;
            }
        }
        let verified = client.verify_account(credentials.grant.clone())?;
        self.commit_restore(snapshot.revision, credentials, verified)
            .map(Some)
    }

    /// Renew `session` with its refresh token: a new access token, verified online
    /// for the same account and saved over the session's revision (so the returned
    /// session has a new one). Works after the access token expired; the refresh
    /// token's own validity is the provider's. `None` when `session` is no longer
    /// the stored account (signed out or replaced meanwhile).
    pub fn renew(
        &self,
        session: &AccountSession,
        client: &ClerkDeviceClient,
    ) -> Result<Option<AccountSession>, AccountError> {
        if !Arc::ptr_eq(&self.owner, &session.owner) {
            return Err(AccountError::OwnerMismatch);
        }
        let snapshot = self.store.load()?;
        let Some(credentials) = snapshot.credentials else {
            return Ok(None);
        };
        if snapshot.revision != session.revision
            || credentials.account.subject != session.account.subject
        {
            return Ok(None);
        }
        self.renew_credentials(snapshot.revision, credentials, client)
            .map(Some)
    }

    fn renew_credentials(
        &self,
        revision: CredentialRevision,
        credentials: SavedCredentials,
        client: &ClerkDeviceClient,
    ) -> Result<AccountSession, AccountError> {
        if !credentials.matches_config(&self.config) {
            return Err(AccountError::ProviderMismatch);
        }
        let observation = self.observe_time()?;
        if unix_seconds(observation.wall)? < credentials.received_at {
            return Err(AccountError::ClockChanged);
        }
        // Network work runs outside store locks; the save below is conditional.
        let grant = client.refresh(&credentials.grant)?;
        let verified = client.verify_account(grant)?;
        if verified.identity.subject != credentials.account.subject {
            return Err(AccountError::AccountMismatch);
        }
        let mut renewed = SavedCredentials::from_verified(verified)?;
        self.check_credentials(&mut renewed)?;
        self.save(revision, &mut renewed)
    }

    /// Whether a safe account view still matches disk state and cached expiry.
    ///
    /// This is a local stale-event check, not an online provider / entitlement check.
    pub fn is_current(&self, session: &AccountSession) -> Result<bool, AccountError> {
        Ok(self.current_credentials(session)?.is_some())
    }

    fn current_credentials(
        &self,
        session: &AccountSession,
    ) -> Result<Option<(SavedCredentials, ClockObservation)>, AccountError> {
        if !Arc::ptr_eq(&self.owner, &session.owner) {
            return Ok(None);
        }
        let snapshot = self.store.load()?;
        let Some(mut credentials) = snapshot.credentials else {
            return Ok(None);
        };
        if snapshot.revision != session.revision
            || credentials.account.subject != session.account.subject
        {
            return Ok(None);
        }
        credentials.monotonic_deadline = Some(session.monotonic_deadline);
        let observation = self.check_credentials(&mut credentials)?;
        Ok(Some((credentials, observation)))
    }

    fn commit_restore(
        &self,
        revision: CredentialRevision,
        mut credentials: SavedCredentials,
        verified: VerifiedAuthorization,
    ) -> Result<AccountSession, AccountError> {
        if verified.grant.provider != credentials.grant.provider
            || verified.identity.subject != credentials.account.subject
        {
            return Err(AccountError::AccountMismatch);
        }
        credentials.account = verified.identity;
        credentials.grant = verified.grant;
        self.check_credentials(&mut credentials)?;
        self.save(revision, &mut credentials)
    }

    fn check_ticket(&self, ticket: &LoginTicket) -> Result<(), AccountError> {
        if !Arc::ptr_eq(&self.owner, &ticket.owner) {
            return Err(AccountError::OwnerMismatch);
        }
        if ticket.provider != ProviderIdentity::from(&self.config) {
            return Err(AccountError::ProviderMismatch);
        }
        Ok(())
    }

    fn check_credentials(
        &self,
        credentials: &mut SavedCredentials,
    ) -> Result<ClockObservation, AccountError> {
        if !credentials.matches_config(&self.config) {
            return Err(AccountError::ProviderMismatch);
        }
        let observation = self.observe_time()?;
        if credentials.grant.receipt.is_some_and(|receipt| {
            observation.wall < receipt.wall || observation.monotonic < receipt.monotonic
        }) {
            return Err(AccountError::ClockChanged);
        }
        let now = unix_seconds(observation.wall)?;
        if now < credentials.received_at {
            return Err(AccountError::ClockChanged);
        }
        if now >= credentials.expires_at {
            return Err(AccountError::Expired);
        }
        let deadline = match credentials.monotonic_deadline {
            Some(deadline) => deadline,
            None => {
                // Restored instants cannot survive a process restart. Anchor the
                // remaining wall lifetime before online verification, only once.
                let expires_at = UNIX_EPOCH
                    .checked_add(Duration::from_secs(credentials.expires_at))
                    .ok_or(AccountError::Expired)?;
                let remaining = expires_at
                    .duration_since(observation.wall)
                    .map_err(|_| AccountError::Expired)?;
                observation
                    .monotonic
                    .checked_add(remaining)
                    .ok_or(AccountError::Expired)?
            }
        };
        if observation.monotonic >= deadline {
            return Err(AccountError::Expired);
        }
        credentials.monotonic_deadline = Some(deadline);
        Ok(observation)
    }

    fn observe_time(&self) -> Result<ClockObservation, AccountError> {
        // Sample under the lock so concurrent callers cannot apply observations
        // in reverse order. Never hold this lock across provider or storage I/O.
        let mut observation = self
            .observation
            .lock()
            .map_err(|_| AccountError::ClockUnavailable)?;
        let monotonic = self.clock.monotonic_now();
        // Anchor remaining wall lifetime to the earlier monotonic sample. A pause
        // between reads must shorten restoration's bound, never extend it.
        let wall = self.clock.now();
        if observation
            .as_ref()
            .is_some_and(|previous| wall < previous.wall || monotonic < previous.monotonic)
        {
            return Err(AccountError::ClockChanged);
        }
        *observation = Some(ClockObservation { wall, monotonic });
        Ok(ClockObservation { wall, monotonic })
    }

    fn save(
        &self,
        revision: CredentialRevision,
        credentials: &mut SavedCredentials,
    ) -> Result<AccountSession, AccountError> {
        let revision = self.store.compare_and_swap(revision, Some(credentials))?;
        // Persistence (including fsync) can outlive the remaining token lifetime.
        // Clear only our own revision; a newer account operation must survive.
        if let Err(error) = self.check_credentials(credentials) {
            match self.store.compare_and_swap(revision, None) {
                Ok(_) | Err(CredentialStoreError::Conflict) => return Err(error),
                Err(store_error) => return Err(store_error.into()),
            }
        }
        Ok(AccountSession {
            owner: self.owner.clone(),
            account: credentials.account.clone(),
            revision,
            expires_at: credentials.expires_at,
            monotonic_deadline: credentials
                .monotonic_deadline
                .ok_or(AccountError::ClockUnavailable)?,
        })
    }
}

#[cfg(all(test, unix))]
mod tests;
