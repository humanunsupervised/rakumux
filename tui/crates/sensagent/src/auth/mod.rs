//! Device Authorization Grant infrastructure and a deterministic local coordinator.
//!
//! Network calls are blocking and must run off the UI thread. [`AuthService`]
//! separately prepares / accepts results so cancellation can happen while a
//! request is in flight. A provider grant is not an application sign-in: account
//! validation and private credential persistence are separate completion steps
//! implemented by [`AccountService`]. Cached disk identity is never sufficient.

mod account;
mod client;
mod config;
mod continuation;
mod credentials;
mod error;
#[cfg(unix)]
mod file_store;
mod identity;
mod model;
mod service;

pub use account::{
    AccessTokenLease, AccountError, AccountService, AccountSession, LoginTicket, SystemClock,
    WallClock,
};
pub use client::ClerkDeviceClient;
pub use config::{DeviceAuthConfig, AI_STATUS_SCOPE};
pub use continuation::{AccountWebsite, BrowserContinuationError};
pub use credentials::{
    CredentialRevision, CredentialSnapshot, CredentialStore, CredentialStoreError, SavedCredentials,
};
pub use error::{AuthError, NetworkFailure};
#[cfg(unix)]
pub use file_store::FileCredentialStore;
pub use identity::{AccountIdentity, VerifiedAuthorization};
pub use model::{
    AuthorizationGrant, DeviceAuthorization, PollOutcome, SecretValue, VerificationPrompt,
};
pub use service::{
    ApplyResult, AttemptId, AuthService, AuthStatus, PollCompletion, PollId, PollRequest,
};

/// The OAuth grant type standardized by RFC 8628.
pub const DEVICE_GRANT_TYPE: &str = "urn:ietf:params:oauth:grant-type:device_code";

#[cfg(test)]
pub(crate) mod tests;
