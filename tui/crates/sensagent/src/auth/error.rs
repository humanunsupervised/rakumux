use thiserror::Error;

/// Credential-safe transport failure categories; never include a URL or body.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NetworkFailure {
    /// The request exceeded the configured network deadline.
    Timeout,
    /// A connection could not be established.
    Connection,
    /// The request or response transport failed for another reason.
    Request,
}

/// Explicit authentication failures safe to display without provider-body leakage.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum AuthError {
    /// Local public-client configuration is invalid.
    #[error("invalid authentication configuration: {0}")]
    InvalidConfiguration(&'static str),
    /// The configured issuer does not advertise the required device grant.
    #[error("the configured identity provider does not support device authorization")]
    UnsupportedDeviceGrant,
    /// No trusted UserInfo endpoint was advertised for account verification.
    #[error("the identity provider does not advertise supported account verification")]
    UnsupportedAccountVerification,
    /// An endpoint or verification URL crossed the configured trust boundary.
    #[error("the identity provider returned an untrusted endpoint")]
    UntrustedEndpoint,
    /// Provider JSON violated a required invariant.
    #[error("invalid identity-provider response: {0}")]
    InvalidResponse(&'static str),
    /// The response exceeded the bounded authentication payload size.
    #[error("the identity-provider response exceeded its size limit")]
    ResponseTooLarge,
    /// Network details intentionally exclude raw reqwest errors and credentials.
    #[error("authentication network failure: {0:?}")]
    Network(NetworkFailure),
    /// Response I/O failed after headers arrived.
    #[error("unable to read the identity-provider response")]
    ResponseRead,
    /// Unexpected status; the untrusted response body is never included.
    #[error("the identity provider returned HTTP {0}")]
    HttpStatus(u16),
    /// A terminal OAuth error not safe to retry without user / configuration action.
    #[error("the identity provider rejected the authorization request")]
    ProviderRejected,
    /// The OAuth application's public client configuration was rejected.
    #[error("the identity provider rejected the public OAuth client configuration")]
    InvalidClient,
    /// Requested scopes are not enabled for this public client.
    #[error("the identity provider rejected the requested OAuth scopes")]
    InvalidScope,
    /// The one-time grant is no longer valid and must not be retried indefinitely.
    #[error("the identity provider rejected the OAuth grant; start a new authorization")]
    InvalidGrant,
    /// No more unique in-process generations can be represented.
    #[error("authentication attempt generation exhausted; restart authentication")]
    GenerationExhausted,
}
