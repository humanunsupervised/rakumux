use std::time::Duration;
use thiserror::Error;

use super::RemoteErrorCode;
use crate::auth::NetworkFailure;

/// Static native transport/capture failures. Never contains source text, URLs or credentials.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum CloudAIError {
    /// Public deployment configuration violated the fixed HTTPS trust contract.
    #[error("invalid cloud AI configuration: {0}")]
    InvalidConfiguration(&'static str),
    /// The complete request violated the canonical wire contract.
    #[error("invalid AI-status request")]
    InvalidRequest,
    /// Meaningful context was absent after selection/redaction; submit nothing.
    #[error("not enough allowed recent activity for AI status")]
    NoContext,
    /// Inspect only bounded local source windows, never entire histories/environments.
    #[error("AI-status source window exceeded the local inspection limit")]
    CaptureTooLarge,
    /// A caller supplied terminal output to a reports-only job.
    #[error("the AI-status job does not permit terminal-output sharing")]
    DataNotPermitted,
    /// Current local account/consent/source policy could not be rechecked.
    #[error("current local AI-status authorization is unavailable")]
    PolicyUnavailable,
    /// The response's complete syntax/shape/encoding/bounds were invalid.
    #[error("invalid AI-status service response")]
    InvalidResponse,
    /// A valid response belongs to another request.
    #[error("AI-status response correlation did not match")]
    CorrelationMismatch,
    /// Cancellation suppresses dispatch or discards late data; no remote revocation promise.
    #[error("AI-status request was canceled")]
    Canceled,
    /// Transport categories exclude arbitrary reqwest/provider diagnostics.
    #[error("AI-status network failure: {0:?}")]
    Network(NetworkFailure),
    /// Unsupported/nonconformant HTTP status, with no response-body echo.
    #[error("AI-status service returned HTTP {0}")]
    HttpStatus(u16),
    /// Verified canonical static server error; retries are never automatic.
    #[error("AI-status service rejected the request: {code:?}")]
    Remote {
        /// HTTP status bound to the canonical code.
        status: u16,
        /// Static canonical error classification.
        code: RemoteErrorCode,
        /// Bounded provider-suggested delay for the future owner's retry policy.
        retry_after: Option<Duration>,
    },
}
