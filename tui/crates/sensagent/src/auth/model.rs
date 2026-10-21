use reqwest::Url;
use serde::Deserialize;
use std::fmt;
use std::time::{Duration, Instant, SystemTime};

use super::{AuthError, DeviceAuthConfig};

const MAX_SECRET_BYTES: usize = 16 * 1024;

/// A credential deliberately lacking Display / Serialize and redacted in Debug.
///
/// This prevents routine formatting leaks; it is not a memory-zeroization promise.
#[derive(Clone, Deserialize)]
#[serde(try_from = "String")]
pub struct SecretValue(String);

impl SecretValue {
    /// Explicit access for protocol operations or a future private credential store.
    pub fn expose_secret(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for SecretValue {
    type Error = &'static str;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        if value.is_empty()
            || value.len() > MAX_SECRET_BYTES
            || !value.bytes().all(|byte| byte.is_ascii_graphic())
        {
            return Err("invalid credential value");
        }
        Ok(Self(value))
    }
}

impl fmt::Debug for SecretValue {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("[REDACTED]")
    }
}

/// Only the public code and trusted verification URLs may reach a UI or clipboard.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerificationPrompt {
    pub(crate) user_code: String,
    pub(crate) verification_uri: Url,
    pub(crate) verification_uri_complete: Option<Url>,
}

impl VerificationPrompt {
    /// The public code the user confirms in their own initiating terminal.
    pub fn user_code(&self) -> &str {
        &self.user_code
    }

    /// Always display this canonical URL together with the public code.
    pub fn verification_uri(&self) -> &Url {
        &self.verification_uri
    }

    /// Optional convenience link that pre-fills the same public code.
    pub fn verification_uri_complete(&self) -> Option<&Url> {
        self.verification_uri_complete.as_ref()
    }
}

/// A validated pending device request. Never serialize it to logs or UI state.
#[derive(Clone, Debug)]
pub struct DeviceAuthorization {
    pub(crate) provider: ProviderIdentity,
    pub(crate) device_code: SecretValue,
    pub(crate) prompt: VerificationPrompt,
    pub(crate) expires_in: Duration,
    pub(crate) interval: Duration,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ProviderIdentity {
    pub(crate) issuer: Url,
    pub(crate) client_id: String,
    pub(crate) verification_uri: Url,
    pub(crate) requested_scopes: Vec<String>,
    pub(crate) userinfo_endpoint: Option<Url>,
}

impl From<&DeviceAuthConfig> for ProviderIdentity {
    fn from(config: &DeviceAuthConfig) -> Self {
        // OAuth scope order has no meaning, but the requested set is provenance.
        let mut requested_scopes = config.scopes.clone();
        requested_scopes.sort();
        Self {
            issuer: config.issuer.clone(),
            client_id: config.client_id.clone(),
            verification_uri: config.verification_uri.clone(),
            requested_scopes,
            userinfo_endpoint: config.userinfo_endpoint.clone(),
        }
    }
}

impl DeviceAuthorization {
    /// Safe display information, excluding the private device code.
    pub fn prompt(&self) -> &VerificationPrompt {
        &self.prompt
    }

    /// Provider-advertised request lifetime, not a hardcoded Clerk default.
    pub fn expires_in(&self) -> Duration {
        self.expires_in
    }

    /// Provider polling interval, or RFC 8628's five-second default when absent.
    pub fn interval(&self) -> Duration {
        self.interval
    }
}

/// Validated OAuth tokens, not proof that application sign-in has completed.
///
/// Account validation and secure persistence must succeed before a UI says
/// "Signed in". ID tokens are intentionally not exposed as API credentials.
#[derive(Clone, Debug)]
pub struct AuthorizationGrant {
    pub(crate) provider: Box<ProviderIdentity>,
    pub(crate) access_token: SecretValue,
    pub(crate) refresh_token: Option<SecretValue>,
    pub(crate) expires_in: Duration,
    pub(crate) scopes: Vec<String>,
    pub(crate) receipt: Option<TokenReceipt>,
}

/// In-process receipt provenance. Monotonic instants are never written to disk.
#[derive(Clone, Copy, Debug)]
pub(crate) struct TokenReceipt {
    pub(crate) wall: SystemTime,
    pub(crate) monotonic: Instant,
}

impl TokenReceipt {
    pub(crate) fn now() -> Self {
        // Sampling monotonic time first is conservative if the thread is paused
        // between the two reads: that delay cannot extend the token lifetime.
        let monotonic = Instant::now();
        Self {
            wall: SystemTime::now(),
            monotonic,
        }
    }
}

impl AuthorizationGrant {
    /// Bearer access token for the approved API, never an ID token.
    pub fn access_token(&self) -> &SecretValue {
        &self.access_token
    }

    /// Optional refresh token; required when offline_access was requested.
    pub fn refresh_token(&self) -> Option<&SecretValue> {
        self.refresh_token.as_ref()
    }

    /// Advertised access-token lifetime for a future credential owner.
    pub fn expires_in(&self) -> Duration {
        self.expires_in
    }

    /// Granted scopes. Missing required requested scopes reject the response.
    pub fn scopes(&self) -> &[String] {
        &self.scopes
    }
}

/// One provider poll result. Unknown OAuth errors are fatal, not indefinite retries.
#[derive(Debug)]
pub enum PollOutcome {
    /// User approval is still pending.
    Pending,
    /// Increase the minimum polling interval by at least five seconds.
    SlowDown,
    /// User explicitly denied the request.
    Denied,
    /// The provider has expired the one-time device request.
    Expired,
    /// Credentials returned after approval; local completion is still required.
    Granted(AuthorizationGrant),
    /// A bounded transient network / provider failure.
    Retryable {
        /// Credential-safe diagnostic.
        error: AuthError,
        /// Optional numeric Retry-After minimum, in addition to local backoff.
        retry_after: Option<Duration>,
    },
    /// Stop the attempt and surface a configuration / protocol / OAuth failure.
    Rejected(AuthError),
}

#[derive(Deserialize)]
pub(crate) struct DeviceResponse {
    device_code: SecretValue,
    user_code: String,
    verification_uri: String,
    verification_uri_complete: Option<String>,
    expires_in: u64,
    interval: Option<u64>,
}

impl DeviceResponse {
    pub(crate) fn validate(
        self,
        config: &DeviceAuthConfig,
    ) -> Result<DeviceAuthorization, AuthError> {
        if self.user_code.is_empty()
            || self.user_code.len() > 128
            || !self
                .user_code
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
            || self.user_code.contains(self.device_code.expose_secret())
            || self.expires_in == 0
            || self.interval == Some(0)
        {
            return Err(AuthError::InvalidResponse("invalid device request fields"));
        }
        let verification_uri =
            Url::parse(&self.verification_uri).map_err(|_| AuthError::UntrustedEndpoint)?;
        if verification_uri != config.verification_uri {
            return Err(AuthError::UntrustedEndpoint);
        }
        let complete = self
            .verification_uri_complete
            .map(|value| validate_complete_uri(&value, config, &self.user_code))
            .transpose()?;
        Ok(DeviceAuthorization {
            provider: ProviderIdentity::from(config),
            device_code: self.device_code,
            prompt: VerificationPrompt {
                user_code: self.user_code,
                verification_uri,
                verification_uri_complete: complete,
            },
            expires_in: Duration::from_secs(self.expires_in),
            interval: Duration::from_secs(self.interval.unwrap_or(5)),
        })
    }
}

fn validate_complete_uri(
    value: &str,
    config: &DeviceAuthConfig,
    code: &str,
) -> Result<Url, AuthError> {
    let uri = Url::parse(value).map_err(|_| AuthError::UntrustedEndpoint)?;
    let query: Vec<_> = uri.query_pairs().collect();
    if uri.origin() != config.verification_uri.origin()
        || uri.path() != config.verification_uri.path()
        || !uri.username().is_empty()
        || uri.password().is_some()
        || uri.fragment().is_some()
        || query.len() != 1
        || query[0].0 != "user_code"
        || query[0].1 != code
    {
        return Err(AuthError::UntrustedEndpoint);
    }
    Ok(uri)
}

#[derive(Deserialize)]
pub(crate) struct TokenResponse {
    access_token: SecretValue,
    refresh_token: Option<SecretValue>,
    token_type: String,
    expires_in: u64,
    scope: Option<String>,
}

impl TokenResponse {
    pub(crate) fn validate(
        self,
        config: &DeviceAuthConfig,
        receipt: TokenReceipt,
    ) -> Result<AuthorizationGrant, AuthError> {
        if !self.token_type.eq_ignore_ascii_case("bearer") || self.expires_in == 0 {
            return Err(AuthError::InvalidResponse("invalid token response fields"));
        }
        let scopes = match self.scope {
            Some(value) => value.split_ascii_whitespace().map(str::to_owned).collect(),
            None => config.scopes.clone(),
        };
        if config.scopes.iter().any(|scope| !scopes.contains(scope)) {
            return Err(AuthError::InvalidResponse(
                "required OAuth scope was not granted",
            ));
        }
        if scopes.iter().any(|scope| !config.scopes.contains(scope)) {
            return Err(AuthError::InvalidResponse(
                "unexpected OAuth scope was granted",
            ));
        }
        if scopes.len() != config.scopes.len() {
            return Err(AuthError::InvalidResponse("invalid granted OAuth scopes"));
        }
        if scopes.iter().any(|scope| scope == "offline_access") && self.refresh_token.is_none() {
            return Err(AuthError::InvalidResponse(
                "offline_access requires a refresh token",
            ));
        }
        Ok(AuthorizationGrant {
            provider: Box::new(ProviderIdentity::from(config)),
            access_token: self.access_token,
            refresh_token: self.refresh_token,
            expires_in: Duration::from_secs(self.expires_in),
            scopes,
            receipt: Some(receipt),
        })
    }
}
