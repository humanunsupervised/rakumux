use std::fmt;
use std::time::{Duration, Instant};

use super::{AccountError, AccountService, AccountSession, CredentialStore, WallClock};
use crate::auth::SecretValue;

const MAX_LEASE_AGE: Duration = Duration::from_secs(30);

/// One-use, process-local token-use intent for an exact verified account revision.
///
/// Contains no bearer. It cannot be cloned or serialized, and only its issuing
/// AccountService can consume it. Consumption rereads and validates credentials
/// before lending the bearer to trusted owner code. This is not an IPC capability,
/// feature consent, destination grant or provider-side authorization proof.
///
/// ```compile_fail
/// use rakumux_sensagent::auth::AccessTokenLease;
/// fn duplicate(lease: AccessTokenLease) { let _copy = lease.clone(); }
/// ```
///
/// ```compile_fail
/// use rakumux_sensagent::auth::AccessTokenLease;
/// fn export(lease: AccessTokenLease) { let _json = serde_json::to_string(&lease); }
/// ```
pub struct AccessTokenLease {
    session: AccountSession,
    deadline: Instant,
}

impl AccessTokenLease {
    /// Token-free account/revision for the owner's feature and destination checks.
    pub fn session(&self) -> &AccountSession {
        &self.session
    }
}

impl fmt::Debug for AccessTokenLease {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("AccessTokenLease { bindings: [REDACTED] }")
    }
}

impl<S: CredentialStore, C: WallClock> AccountService<S, C> {
    /// Prepare at most 30 seconds of token-free staging for a current account view.
    /// No bearer is retained, copied to a worker, or exposed by this operation.
    pub fn lease_access_token(
        &self,
        session: &AccountSession,
    ) -> Result<Option<AccessTokenLease>, AccountError> {
        let Some((_, observation)) = self.current_credentials(session)? else {
            return Ok(None);
        };
        let deadline = observation
            .monotonic
            .checked_add(MAX_LEASE_AGE)
            .ok_or(AccountError::ClockUnavailable)?
            .min(session.monotonic_deadline);
        Ok(Some(AccessTokenLease {
            session: session.clone(),
            deadline,
        }))
    }

    /// Consume once and lend the bearer from the same freshly validated snapshot.
    /// Stale/foreign/signed-out intents return None without invoking the callback.
    /// Expiry, clock and storage failures also suppress the callback.
    ///
    /// The trusted single owner must separately check feature scope, exact consent
    /// binding and fixed destination, and serialize its dispatch commitment with
    /// revocation. This method does not lock out concurrent account/store mutations,
    /// authorize arbitrary callbacks, or stop already committed upstream work.
    /// A callback can copy a secret; it must not export it to UI/IPC or retain it.
    pub fn consume_access_token<T>(
        &self,
        lease: AccessTokenLease,
        dispatch: impl FnOnce(&SecretValue) -> T,
    ) -> Result<Option<T>, AccountError> {
        let Some((credentials, observation)) = self.current_credentials(&lease.session)? else {
            return Ok(None);
        };
        if observation.monotonic >= lease.deadline {
            return Err(AccountError::TokenLeaseExpired);
        }
        Ok(Some(dispatch(credentials.access_token())))
    }
}
