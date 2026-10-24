use std::fmt;
use std::sync::Arc;

use crate::auth::{
    AccessTokenLease, AccountService, CredentialRevision, CredentialStore, SystemClock, WallClock,
};
use crate::cloud::{AccountFeatures, Cancellation, CloudAIClient, FeatureChange};

use super::DispatchError;

/// One read (or change, then read) of the signed-in account's feature switches, run
/// off the owner thread. It sends no session data, so it needs no consent; it
/// borrows the bearer only for the one request, like a dispatch.
pub struct FeaturesJob<A, C = SystemClock> {
    pub(super) account: Arc<AccountService<A, C>>,
    pub(super) cloud: Arc<CloudAIClient>,
    pub(super) lease: AccessTokenLease,
    pub(super) revision: CredentialRevision,
    /// The switches to change; `None` only reads them.
    pub(super) change: Option<FeatureChange>,
}

/// A finished features read, tagged with the account revision it was made for so an
/// answer for an older sign-in is never applied.
pub struct FeaturesCompletion {
    pub(crate) revision: CredentialRevision,
    /// The change the job made, if it was one.
    pub(crate) change: Option<FeatureChange>,
    pub(crate) outcome: Result<AccountFeatures, DispatchError>,
}

impl<A: CredentialStore, C: WallClock> FeaturesJob<A, C> {
    /// Make at most one request.
    pub fn run(self) -> FeaturesCompletion {
        let cancel = Cancellation::new();
        let change = self.change;
        let outcome = self
            .account
            .consume_access_token(self.lease, |token| match change {
                Some(change) => self.cloud.set_features(token, change, &cancel),
                None => self.cloud.features(token, &cancel),
            })
            .map_err(DispatchError::Account)
            .and_then(|result| match result {
                Some(result) => result.map_err(DispatchError::Cloud),
                None => Err(DispatchError::Canceled),
            });
        FeaturesCompletion {
            revision: self.revision,
            change,
            outcome,
        }
    }
}

impl<A, C> fmt::Debug for FeaturesJob<A, C> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("FeaturesJob { bindings: [REDACTED] }")
    }
}

impl fmt::Debug for FeaturesCompletion {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FeaturesCompletion")
            .field("ok", &self.outcome.is_ok())
            .finish()
    }
}
