use std::sync::{Arc, Mutex};

use super::{AccountBinding, AccountKey, StatusError};
use crate::auth::{AccountService, AccountSession, CredentialStore, WallClock};

/// Trusted local credential owner boundary. Implementations must check current
/// verified/persisted authentication, not cached disk identity or a UI boolean.
pub trait AccountGate: Send {
    /// Current token-free account observation, or None when signed out/stale/expired.
    fn current(&self) -> Result<Option<AccountBinding>, StatusError>;
}

impl<T: AccountGate + Sync> AccountGate for Arc<T> {
    fn current(&self) -> Result<Option<AccountBinding>, StatusError> {
        self.as_ref().current()
    }
}

/// Adapter for the existing verified AccountService. No network or credential export.
pub struct AccountSessionGate<S, C> {
    account: Arc<AccountService<S, C>>,
    session: Mutex<Option<AccountSession>>,
}

impl<S: CredentialStore, C: WallClock> AccountSessionGate<S, C> {
    /// Start signed out; the future single local owner supplies successful account views.
    pub fn new(account: Arc<AccountService<S, C>>) -> Self {
        Self {
            account,
            session: Mutex::new(None),
        }
    }

    /// Replace/clear an account view without changing feature consent or sharing.
    pub fn set_session(&self, session: Option<AccountSession>) -> Result<(), StatusError> {
        *self
            .session
            .lock()
            .map_err(|_| StatusError::AccountUnavailable)? = session;
        Ok(())
    }
}

impl<S: CredentialStore, C: WallClock> AccountGate for AccountSessionGate<S, C> {
    fn current(&self) -> Result<Option<AccountBinding>, StatusError> {
        let guard = self
            .session
            .lock()
            .map_err(|_| StatusError::AccountUnavailable)?;
        let Some(session) = guard.as_ref() else {
            return Ok(None);
        };
        match self.account.is_current(session) {
            Ok(true) => {}
            Ok(false)
            | Err(crate::auth::AccountError::Expired | crate::auth::AccountError::ClockChanged) => {
                return Ok(None)
            }
            Err(_) => return Err(StatusError::AccountUnavailable),
        }
        let config = self.account.config();
        if !config
            .scopes()
            .iter()
            .any(|scope| scope == config.status_scope())
        {
            return Err(StatusError::FeatureUnauthorized);
        }
        let binding = AccountBinding {
            account: AccountKey {
                issuer: self.account.config().issuer().to_string(),
                client_id: self.account.config().client_id().to_owned(),
                subject: session.account().subject().to_owned(),
            },
            auth_revision: session.revision().value(),
        };
        binding.validate()?;
        Ok(Some(binding))
    }
}
