use reqwest::blocking::Client;
use reqwest::{redirect, Url};
use serde::de::DeserializeOwned;
use serde::Deserialize;
use std::io::Read;
use std::time::Duration;

use super::identity::IdentityResponse;
use super::model::{DeviceResponse, ProviderIdentity, TokenReceipt, TokenResponse};
use super::{
    AuthError, AuthorizationGrant, DeviceAuthConfig, DeviceAuthorization, NetworkFailure,
    PollOutcome, PollRequest, SecretValue, VerifiedAuthorization, DEVICE_GRANT_TYPE,
};

const MAX_RESPONSE_BYTES: usize = 64 * 1024;
const REFRESH_GRANT_TYPE: &str = "refresh_token";
/// A sign-out waits for its revocation (`revoke`): discovery and two requests, each
/// at most this long, well within an owner request's 30 s.
const REVOCATION_TIMEOUT: Duration = Duration::from_secs(8);

/// Blocking Clerk OAuth public client. Construct and call it off the UI thread.
///
/// All discovery / token endpoints are HTTPS and pinned to the configured issuer
/// origin. Redirects are disabled so private form values cannot be forwarded.
pub struct ClerkDeviceClient {
    config: DeviceAuthConfig,
    transport: Box<dyn HttpTransport>,
    device_endpoint: Url,
    token_endpoint: Url,
    userinfo_endpoint: Option<Url>,
    /// Where tokens are revoked (RFC 7009), when the issuer advertises it.
    revocation_endpoint: Option<Url>,
}

impl ClerkDeviceClient {
    /// Discover an explicitly configured issuer; missing device support is an error.
    pub fn discover(config: DeviceAuthConfig) -> Result<Self, AuthError> {
        Self::discover_within(config, Duration::from_secs(10), Duration::from_secs(30))
    }

    /// Discover the issuer for a sign-out's [`revoke`](Self::revoke), with
    /// shorter timeouts: the user waits for it.
    pub fn discover_for_sign_out(config: DeviceAuthConfig) -> Result<Self, AuthError> {
        Self::discover_within(config, Duration::from_secs(5), REVOCATION_TIMEOUT)
    }

    fn discover_within(
        config: DeviceAuthConfig,
        connect: Duration,
        total: Duration,
    ) -> Result<Self, AuthError> {
        let client = Client::builder()
            .https_only(true)
            .redirect(redirect::Policy::none())
            .connect_timeout(connect)
            .timeout(total)
            .user_agent(concat!("rakumux-sensagent/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(network_error)?;
        Self::with_transport(config, Box::new(ReqwestTransport { client }))
    }

    fn with_transport(
        config: DeviceAuthConfig,
        transport: Box<dyn HttpTransport>,
    ) -> Result<Self, AuthError> {
        let discovery_url = config
            .issuer
            .join(".well-known/oauth-authorization-server")
            .map_err(|_| AuthError::InvalidConfiguration("invalid discovery URL"))?;
        let response = transport.request(&discovery_url, None, None)?;
        require_success(response.status)?;
        let metadata: Metadata = decode(&response)?;
        let issuer = config.provider_endpoint(&metadata.issuer)?;
        if issuer != config.issuer {
            return Err(AuthError::UntrustedEndpoint);
        }
        if !metadata
            .grant_types_supported
            .iter()
            .any(|value| value == DEVICE_GRANT_TYPE)
        {
            return Err(AuthError::UnsupportedDeviceGrant);
        }
        let device_endpoint = config.provider_endpoint(
            metadata
                .device_authorization_endpoint
                .as_deref()
                .ok_or(AuthError::UnsupportedDeviceGrant)?,
        )?;
        let token_endpoint = config.provider_endpoint(&metadata.token_endpoint)?;
        let advertised_userinfo = metadata
            .userinfo_endpoint
            .as_deref()
            .map(|value| config.provider_endpoint(value))
            .transpose()?;
        let userinfo_endpoint = match (&config.userinfo_endpoint, advertised_userinfo) {
            (Some(pinned), Some(advertised)) if *pinned != advertised => {
                return Err(AuthError::UntrustedEndpoint);
            }
            (Some(pinned), _) => Some(pinned.clone()),
            (None, advertised) => advertised,
        };
        let revocation_endpoint = metadata
            .revocation_endpoint
            .as_deref()
            .map(|value| config.provider_endpoint(value))
            .transpose()?;
        Ok(Self {
            config,
            transport,
            device_endpoint,
            token_endpoint,
            userinfo_endpoint,
            revocation_endpoint,
        })
    }

    /// Request a fresh one-time device grant. No account or feature is enabled here.
    pub fn start(&self) -> Result<DeviceAuthorization, AuthError> {
        let scopes = self.config.scopes.join(" ");
        let form = [
            ("client_id", self.config.client_id.as_str()),
            ("scope", scopes.as_str()),
        ];
        let response = self
            .transport
            .request(&self.device_endpoint, Some(&form), None)?;
        if response.status != 200 {
            if response.status == 400 || response.status == 403 {
                if let Ok(error) = decode::<OAuthError>(&response) {
                    return Err(oauth_rejection(&error.error));
                }
            }
            return Err(AuthError::HttpStatus(response.status));
        }
        decode::<DeviceResponse>(&response)?.validate(&self.config)
    }

    /// Execute exactly one prepared poll. The AuthService owns timing and retries.
    pub fn poll(&self, request: &PollRequest) -> PollOutcome {
        if request.provider != ProviderIdentity::from(&self.config) {
            return PollOutcome::Rejected(AuthError::UntrustedEndpoint);
        }
        let form = [
            ("client_id", self.config.client_id.as_str()),
            ("grant_type", DEVICE_GRANT_TYPE),
            ("device_code", request.device_code.expose_secret()),
        ];
        let response = match self
            .transport
            .request(&self.token_endpoint, Some(&form), None)
        {
            Ok(response) => response,
            Err(error @ (AuthError::Network(_) | AuthError::ResponseRead)) => {
                return PollOutcome::Retryable {
                    error,
                    retry_after: None,
                };
            }
            Err(error) => return PollOutcome::Rejected(error),
        };
        if response.status == 429 || (500..=599).contains(&response.status) {
            return PollOutcome::Retryable {
                error: AuthError::HttpStatus(response.status),
                retry_after: response.retry_after,
            };
        }
        if response.status == 200 {
            return match decode::<TokenResponse>(&response)
                .and_then(|value| value.validate(&self.config, response.receipt))
            {
                Ok(tokens) => PollOutcome::Granted(tokens),
                Err(error) => PollOutcome::Rejected(error),
            };
        }
        if response.status != 400 && response.status != 403 {
            return PollOutcome::Rejected(AuthError::HttpStatus(response.status));
        }
        match decode::<OAuthError>(&response).map(|value| value.error) {
            Ok(code) => match code.as_str() {
                "authorization_pending" => PollOutcome::Pending,
                "slow_down" => PollOutcome::SlowDown,
                "access_denied" => PollOutcome::Denied,
                "expired_token" => PollOutcome::Expired,
                other => PollOutcome::Rejected(oauth_rejection(other)),
            },
            Err(error) => PollOutcome::Rejected(error),
        }
    }

    /// Exchange `grant`'s refresh token for a new grant (RFC 6749 §6), once. A
    /// response without a refresh token keeps the old one; a new one replaces it.
    /// The new grant must still be verified with `verify_account` before use.
    pub fn refresh(&self, grant: &AuthorizationGrant) -> Result<AuthorizationGrant, AuthError> {
        if *grant.provider != ProviderIdentity::from(&self.config) {
            return Err(AuthError::UntrustedEndpoint);
        }
        let refresh_token = grant.refresh_token().ok_or(AuthError::InvalidGrant)?;
        let form = [
            ("client_id", self.config.client_id.as_str()),
            ("grant_type", REFRESH_GRANT_TYPE),
            ("refresh_token", refresh_token.expose_secret()),
        ];
        let response = self
            .transport
            .request(&self.token_endpoint, Some(&form), None)?;
        if response.status == 200 {
            let mut tokens = decode::<TokenResponse>(&response)?;
            tokens.keep_refresh_token(refresh_token);
            return tokens.validate(&self.config, response.receipt);
        }
        if response.status == 400 || response.status == 401 || response.status == 403 {
            if let Ok(error) = decode::<OAuthError>(&response) {
                return Err(oauth_rejection(&error.error));
            }
        }
        Err(AuthError::HttpStatus(response.status))
    }

    /// Revoke `grant` at the issuer's advertised revocation endpoint (RFC 7009),
    /// as the public client: its refresh token, which ends the grant and every
    /// access token made from it, then its access token where the issuer can
    /// revoke one (Clerk's JWT access tokens can't be, and expire within a day:
    /// rakumux.com's "Sign out all terminals" refuses them sooner). Succeeds
    /// once the refresh token is revoked, or, without one, the access token.
    /// The issuer answers 200 for a token it no longer knows, so revoking twice
    /// is harmless.
    pub fn revoke(&self, grant: &AuthorizationGrant) -> Result<(), AuthError> {
        if *grant.provider != ProviderIdentity::from(&self.config) {
            return Err(AuthError::UntrustedEndpoint);
        }
        let endpoint = self
            .revocation_endpoint
            .as_ref()
            .ok_or(AuthError::UnsupportedRevocation)?;
        let Some(refresh_token) = grant.refresh_token() else {
            return self.revoke_token(endpoint, grant.access_token(), "access_token");
        };
        self.revoke_token(endpoint, refresh_token, "refresh_token")?;
        // Best effort: the grant is over already.
        let _ = self.revoke_token(endpoint, grant.access_token(), "access_token");
        Ok(())
    }

    fn revoke_token(
        &self,
        endpoint: &Url,
        token: &SecretValue,
        hint: &str,
    ) -> Result<(), AuthError> {
        let form = [
            ("client_id", self.config.client_id.as_str()),
            ("token", token.expose_secret()),
            ("token_type_hint", hint),
        ];
        let response = self.transport.request(endpoint, Some(&form), None)?;
        if response.status == 200 {
            return Ok(());
        }
        if response.status == 400 || response.status == 401 || response.status == 403 {
            if let Ok(error) = decode::<OAuthError>(&response) {
                return Err(oauth_rejection(&error.error));
            }
        }
        Err(AuthError::HttpStatus(response.status))
    }

    /// Verify the exact access token against advertised, issuer-pinned UserInfo.
    ///
    /// Never infer an account by decoding an unverified ID token or email address.
    /// If OAuth metadata omitted UserInfo, consult the documented OIDC metadata;
    /// no endpoint path is guessed and redirects remain disabled.
    pub fn verify_account(
        &self,
        grant: AuthorizationGrant,
    ) -> Result<VerifiedAuthorization, AuthError> {
        if *grant.provider != ProviderIdentity::from(&self.config) {
            return Err(AuthError::UntrustedEndpoint);
        }
        if !grant.scopes.iter().any(|scope| scope == "openid")
            || grant.scopes.len() != self.config.scopes.len()
            || self
                .config
                .scopes
                .iter()
                .any(|scope| !grant.scopes.contains(scope))
        {
            return Err(AuthError::InvalidScope);
        }
        let endpoint = self.account_endpoint()?;
        let response = self
            .transport
            .request(&endpoint, None, Some(grant.access_token()))?;
        require_success(response.status)?;
        let identity = decode::<IdentityResponse>(&response)?.validate()?;
        identity.reject_credentials(&grant)?;
        Ok(VerifiedAuthorization { identity, grant })
    }

    fn account_endpoint(&self) -> Result<Url, AuthError> {
        if let Some(endpoint) = &self.userinfo_endpoint {
            return Ok(endpoint.clone());
        }
        let metadata_url = self
            .config
            .issuer
            .join(".well-known/openid-configuration")
            .map_err(|_| AuthError::InvalidConfiguration("invalid OIDC discovery URL"))?;
        let response = self.transport.request(&metadata_url, None, None)?;
        require_success(response.status)?;
        let metadata: IdentityMetadata = decode(&response)?;
        if self.config.provider_endpoint(&metadata.issuer)? != self.config.issuer {
            return Err(AuthError::UntrustedEndpoint);
        }
        self.config.provider_endpoint(
            metadata
                .userinfo_endpoint
                .as_deref()
                .ok_or(AuthError::UnsupportedAccountVerification)?,
        )
    }
}

#[derive(Deserialize)]
struct Metadata {
    issuer: String,
    device_authorization_endpoint: Option<String>,
    token_endpoint: String,
    userinfo_endpoint: Option<String>,
    #[serde(default)]
    revocation_endpoint: Option<String>,
    #[serde(default)]
    grant_types_supported: Vec<String>,
}

#[derive(Deserialize)]
struct IdentityMetadata {
    issuer: String,
    userinfo_endpoint: Option<String>,
}

#[derive(Deserialize)]
struct OAuthError {
    error: String,
}

struct HttpReply {
    receipt: TokenReceipt,
    status: u16,
    content_type: Option<String>,
    retry_after: Option<Duration>,
    body: Vec<u8>,
}

trait HttpTransport: Send + Sync {
    fn request(
        &self,
        endpoint: &Url,
        form: Option<&[(&str, &str)]>,
        bearer: Option<&SecretValue>,
    ) -> Result<HttpReply, AuthError>;
}

struct ReqwestTransport {
    client: Client,
}

impl HttpTransport for ReqwestTransport {
    fn request(
        &self,
        endpoint: &Url,
        form: Option<&[(&str, &str)]>,
        bearer: Option<&SecretValue>,
    ) -> Result<HttpReply, AuthError> {
        let request = match form {
            Some(form) => self.client.post(endpoint.clone()).form(form),
            None => self.client.get(endpoint.clone()),
        };
        let request = match bearer {
            Some(token) => request.bearer_auth(token.expose_secret()),
            None => request,
        };
        let response = request
            .header(reqwest::header::ACCEPT, "application/json")
            .send()
            .map_err(network_error)?;
        // Preserve receipt before body reading, JSON decoding, worker dispatch
        // or UserInfo verification can consume the advertised token lifetime.
        let receipt = TokenReceipt::now();
        let status = response.status().as_u16();
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        let retry_after = response
            .headers()
            .get(reqwest::header::RETRY_AFTER)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse::<u64>().ok())
            .map(Duration::from_secs);
        let mut body = Vec::new();
        response
            .take((MAX_RESPONSE_BYTES + 1) as u64)
            .read_to_end(&mut body)
            .map_err(|_| AuthError::ResponseRead)?;
        if body.len() > MAX_RESPONSE_BYTES {
            return Err(AuthError::ResponseTooLarge);
        }
        Ok(HttpReply {
            receipt,
            status,
            content_type,
            retry_after,
            body,
        })
    }
}

fn decode<T: DeserializeOwned>(response: &HttpReply) -> Result<T, AuthError> {
    if response.body.len() > MAX_RESPONSE_BYTES {
        return Err(AuthError::ResponseTooLarge);
    }
    let content_type = response
        .content_type
        .as_deref()
        .unwrap_or("")
        .split(';')
        .next()
        .unwrap_or("")
        .trim();
    if !content_type.eq_ignore_ascii_case("application/json") {
        return Err(AuthError::InvalidResponse("expected application/json"));
    }
    serde_json::from_slice(&response.body)
        .map_err(|_| AuthError::InvalidResponse("malformed OAuth JSON"))
}

fn require_success(status: u16) -> Result<(), AuthError> {
    if status == 200 {
        Ok(())
    } else {
        Err(AuthError::HttpStatus(status))
    }
}

fn oauth_rejection(code: &str) -> AuthError {
    match code {
        "invalid_client" | "unauthorized_client" => AuthError::InvalidClient,
        "invalid_scope" => AuthError::InvalidScope,
        "invalid_grant" => AuthError::InvalidGrant,
        _ => AuthError::ProviderRejected,
    }
}

fn network_error(error: reqwest::Error) -> AuthError {
    let kind = if error.is_timeout() {
        NetworkFailure::Timeout
    } else if error.is_connect() {
        NetworkFailure::Connection
    } else {
        NetworkFailure::Request
    };
    AuthError::Network(kind)
}

#[cfg(test)]
mod tests;
