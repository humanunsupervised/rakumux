use std::fmt;
use std::io;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::model::ProviderIdentity;
use super::{
    AccountIdentity, AuthorizationGrant, DeviceAuthConfig, SecretValue, VerifiedAuthorization,
};

/// Opaque persistent revision. Advancing it invalidates older asynchronous work.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CredentialRevision(pub(crate) u64);

impl CredentialRevision {
    /// Non-secret generation for internal status / stale-response checks.
    pub fn value(self) -> u64 {
        self.0
    }
}

/// Safe diagnostics; never include credential bytes, parser input or filesystem paths.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum CredentialStoreError {
    /// Another operation holds the private credential lock.
    #[error("the credential store is busy")]
    Busy,
    /// A newer operation changed the credential generation.
    #[error("credentials changed while this operation was in progress")]
    Conflict,
    /// Files / directories must be owned, private, regular and not linked elsewhere.
    #[error("the credential store has unsafe ownership, permissions or file type")]
    UnsafePath,
    /// Version or revision information is unsupported.
    #[error("unsupported credential-store format")]
    UnsupportedFormat,
    /// Untrusted disk data violated schema / model invariants.
    #[error("invalid credential-store data")]
    InvalidData,
    /// Read and write sizes are independently bounded.
    #[error("credential-store data exceeded its size limit")]
    TooLarge,
    /// The persistent generation cannot wrap around.
    #[error("credential-store revision exhausted")]
    RevisionExhausted,
    /// Temporary names require operating-system randomness.
    #[error("unable to obtain randomness for private credential storage")]
    RandomUnavailable,
    /// Filesystem errors report only their non-sensitive category.
    #[error("credential-store I/O failure: {0:?}")]
    Io(io::ErrorKind),
    /// Rename succeeded, but durability could not be confirmed afterwards.
    #[error("credentials may have changed, but durable storage could not be confirmed")]
    CommitUncertain,
}

/// Local private credentials plus cached identity. Loading alone is not verification.
pub struct SavedCredentials {
    pub(crate) account: AccountIdentity,
    pub(crate) grant: AuthorizationGrant,
    pub(crate) received_at: u64,
    pub(crate) expires_at: u64,
    pub(crate) monotonic_deadline: Option<Instant>,
}

impl fmt::Debug for SavedCredentials {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SavedCredentials")
            .field("account", &self.account)
            .field("credentials", &"[REDACTED]")
            .field("received_at", &self.received_at)
            .field("expires_at", &self.expires_at)
            .finish()
    }
}

impl SavedCredentials {
    /// Cached account data; verify online before calling a restored session signed in.
    pub fn account(&self) -> &AccountIdentity {
        &self.account
    }

    /// Explicit bearer access for the trusted local service, never UI serialization.
    pub fn access_token(&self) -> &SecretValue {
        self.grant.access_token()
    }

    /// Private refresh credential; refresh ownership is a later integration slice.
    pub fn refresh_token(&self) -> Option<&SecretValue> {
        self.grant.refresh_token()
    }

    /// Persisted token expiry in Unix seconds, not a guarantee of provider validity.
    pub fn expires_at(&self) -> u64 {
        self.expires_at
    }

    pub(crate) fn from_verified(
        verified: VerifiedAuthorization,
    ) -> Result<Self, CredentialStoreError> {
        let receipt = verified
            .grant
            .receipt
            .ok_or(CredentialStoreError::InvalidData)?;
        let received_at = unix_seconds(receipt.wall)?;
        let expires_at = received_at
            .checked_add(verified.grant.expires_in.as_secs())
            .filter(|expiry| *expiry > received_at)
            .ok_or(CredentialStoreError::InvalidData)?;
        let monotonic_deadline = receipt
            .monotonic
            .checked_add(verified.grant.expires_in)
            .ok_or(CredentialStoreError::InvalidData)?;
        Ok(Self {
            account: verified.identity,
            grant: verified.grant,
            received_at,
            expires_at,
            monotonic_deadline: Some(monotonic_deadline),
        })
    }

    pub(crate) fn matches_config(&self, config: &DeviceAuthConfig) -> bool {
        *self.grant.provider == ProviderIdentity::from(config)
            && self.grant.scopes.len() == config.scopes.len()
            && config
                .scopes
                .iter()
                .all(|scope| self.grant.scopes.contains(scope))
    }
}

/// A revision and optional cached credential. Never use this as a UI sign-in result.
#[derive(Debug, Default)]
pub struct CredentialSnapshot {
    pub(crate) revision: CredentialRevision,
    pub(crate) credentials: Option<SavedCredentials>,
}

impl CredentialSnapshot {
    /// Generation to use for a conditional credential mutation.
    pub fn revision(&self) -> CredentialRevision {
        self.revision
    }

    /// Private cached record for the local credential owner.
    pub fn credentials(&self) -> Option<&SavedCredentials> {
        self.credentials.as_ref()
    }
}

/// Private storage boundary suitable for file / future OS-keychain implementations.
///
/// Conditional writes must be serialized across owners and retain a new revision
/// even when clearing credentials. Otherwise a late initial login can resurrect
/// a session after sign-out.
pub trait CredentialStore: Send + Sync {
    /// Read a bounded, validated snapshot; never authenticate it by disk contents alone.
    fn load(&self) -> Result<CredentialSnapshot, CredentialStoreError>;

    /// Atomically replace / clear only the expected revision, then advance it.
    fn compare_and_swap(
        &self,
        expected: CredentialRevision,
        credentials: Option<&SavedCredentials>,
    ) -> Result<CredentialRevision, CredentialStoreError>;
}

pub(crate) fn unix_seconds(time: SystemTime) -> Result<u64, CredentialStoreError> {
    time.duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .map_err(|_| CredentialStoreError::InvalidData)
}

// Only this private disk DTO serializes secrets. It deliberately has no Debug.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct StoredDocument {
    pub(crate) version: u32,
    pub(crate) revision: u64,
    pub(crate) credential: Option<StoredCredential>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct StoredCredential {
    issuer: String,
    client_id: String,
    verification_uri: String,
    requested_scopes: Vec<String>,
    userinfo_endpoint: Option<String>,
    subject: String,
    email: Option<String>,
    email_verified: bool,
    name: Option<String>,
    access_token: String,
    refresh_token: Option<String>,
    scopes: Vec<String>,
    received_at: u64,
    expires_at: u64,
}

impl From<&SavedCredentials> for StoredCredential {
    fn from(record: &SavedCredentials) -> Self {
        Self {
            issuer: record.grant.provider.issuer.to_string(),
            client_id: record.grant.provider.client_id.clone(),
            verification_uri: record.grant.provider.verification_uri.to_string(),
            requested_scopes: record.grant.provider.requested_scopes.clone(),
            userinfo_endpoint: record
                .grant
                .provider
                .userinfo_endpoint
                .as_ref()
                .map(ToString::to_string),
            subject: record.account.subject.clone(),
            email: record.account.email.clone(),
            email_verified: record.account.email_verified,
            name: record.account.name.clone(),
            access_token: record.access_token().expose_secret().to_owned(),
            refresh_token: record
                .refresh_token()
                .map(|token| token.expose_secret().to_owned()),
            scopes: record.grant.scopes.clone(),
            received_at: record.received_at,
            expires_at: record.expires_at,
        }
    }
}

impl StoredCredential {
    pub(crate) fn validate(self) -> Result<SavedCredentials, CredentialStoreError> {
        let invalid = || CredentialStoreError::InvalidData;
        let requested_scopes: Vec<_> = self.requested_scopes.iter().map(String::as_str).collect();
        let mut config = DeviceAuthConfig::new(
            &self.issuer,
            &self.client_id,
            &self.verification_uri,
            &requested_scopes,
        )
        .map_err(|_| invalid())?;
        if let Some(endpoint) = self.userinfo_endpoint {
            config = config
                .with_userinfo_endpoint(&endpoint)
                .map_err(|_| invalid())?;
        }
        let mut granted_scopes = self.scopes.clone();
        granted_scopes.sort();
        let provider = ProviderIdentity::from(&config);
        if granted_scopes != provider.requested_scopes || self.expires_at <= self.received_at {
            return Err(invalid());
        }
        let access_token = SecretValue::try_from(self.access_token).map_err(|_| invalid())?;
        let refresh_token = self
            .refresh_token
            .map(SecretValue::try_from)
            .transpose()
            .map_err(|_| invalid())?;
        if self.scopes.iter().any(|scope| scope == "offline_access") && refresh_token.is_none() {
            return Err(invalid());
        }
        let grant = AuthorizationGrant {
            provider: Box::new(provider),
            access_token,
            refresh_token,
            expires_in: Duration::from_secs(self.expires_at - self.received_at),
            scopes: self.scopes,
            receipt: None,
        };
        let account = AccountIdentity {
            subject: self.subject,
            email: self.email,
            email_verified: self.email_verified,
            name: self.name,
        }
        .validate()
        .map_err(|_| invalid())?;
        account.reject_credentials(&grant).map_err(|_| invalid())?;
        Ok(SavedCredentials {
            account,
            grant,
            received_at: self.received_at,
            expires_at: self.expires_at,
            monotonic_deadline: None,
        })
    }
}
