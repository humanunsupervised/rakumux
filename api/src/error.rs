//! The error envelope `{"error": {"code", "message", "details"?}}` and the
//! mapping from the daemon's `ErrorCode` to an HTTP status.

use hyper::StatusCode;
use serde::{Deserialize, Serialize};
use session_core::api::{ApiError, ErrorCode};

/// An error answer: status plus the envelope's body.
#[derive(Debug, Clone, PartialEq)]
pub struct HttpError {
    pub status: StatusCode,
    pub body: ErrorBody,
    /// The daemon's code, when the error came from the daemon.
    pub daemon_code: Option<ErrorCode>,
}

/// What goes inside `{"error": …}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ErrorBody {
    /// The daemon's error code (`not_found`, `invalid_argument`, …) or one of
    /// the API's own: `method_not_allowed`, `unsupported_media_type`,
    /// `payload_too_large`.
    pub code: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<serde_json::Value>,
}

/// The complete envelope, as serialized.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ErrorEnvelope {
    pub error: ErrorBody,
}

impl HttpError {
    pub fn new(status: StatusCode, code: &str, message: impl Into<String>) -> Self {
        Self {
            status,
            body: ErrorBody {
                code: code.to_owned(),
                message: message.into(),
                details: None,
            },
            daemon_code: None,
        }
    }

    pub fn with_details(mut self, details: serde_json::Value) -> Self {
        self.body.details = Some(details);
        self
    }

    /// 400 `invalid_argument`.
    pub fn invalid(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, "invalid_argument", message)
    }

    /// 404 `not_found`.
    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_FOUND, "not_found", message)
    }

    /// 401 `unauthenticated`.
    pub fn unauthenticated(message: impl Into<String>) -> Self {
        Self::new(StatusCode::UNAUTHORIZED, "unauthenticated", message)
    }

    /// 403 `permission_denied`.
    pub fn forbidden(message: impl Into<String>) -> Self {
        Self::new(StatusCode::FORBIDDEN, "permission_denied", message)
    }

    /// 500 `internal`.
    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, "internal", message)
    }

    /// The envelope's error code.
    pub fn code(&self) -> &str {
        &self.body.code
    }

    /// The JSON body.
    pub fn envelope(&self) -> ErrorEnvelope {
        ErrorEnvelope {
            error: self.body.clone(),
        }
    }
}

impl From<ApiError> for HttpError {
    fn from(error: ApiError) -> Self {
        Self {
            status: status_for(error.code),
            body: ErrorBody {
                code: code_name(error.code),
                message: error.message,
                details: error.details,
            },
            daemon_code: Some(error.code),
        }
    }
}

/// HTTP status for a daemon error code (see `api/openapi.yaml`).
pub fn status_for(code: ErrorCode) -> StatusCode {
    match code {
        ErrorCode::NotFound => StatusCode::NOT_FOUND,
        ErrorCode::InvalidArgument => StatusCode::BAD_REQUEST,
        ErrorCode::Unauthenticated => StatusCode::UNAUTHORIZED,
        ErrorCode::OutOfScope | ErrorCode::PermissionDenied => StatusCode::FORBIDDEN,
        ErrorCode::SessionNotLive | ErrorCode::SessionInUse => StatusCode::CONFLICT,
        ErrorCode::LimitExceeded => StatusCode::TOO_MANY_REQUESTS,
        ErrorCode::Timeout => StatusCode::GATEWAY_TIMEOUT,
        ErrorCode::DaemonUnavailable => StatusCode::SERVICE_UNAVAILABLE,
        ErrorCode::Unsupported => StatusCode::NOT_IMPLEMENTED,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

/// The wire (snake_case) name of a code.
pub fn code_name(code: ErrorCode) -> String {
    serde_json::to_value(code)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_else(|| "unknown".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn daemon_codes_map_to_the_planned_statuses() {
        let cases = [
            (ErrorCode::NotFound, 404),
            (ErrorCode::InvalidArgument, 400),
            (ErrorCode::Unauthenticated, 401),
            (ErrorCode::OutOfScope, 403),
            (ErrorCode::PermissionDenied, 403),
            (ErrorCode::SessionNotLive, 409),
            (ErrorCode::SessionInUse, 409),
            (ErrorCode::LimitExceeded, 429),
            (ErrorCode::Timeout, 504),
            (ErrorCode::DaemonUnavailable, 503),
            (ErrorCode::Unsupported, 501),
            (ErrorCode::Internal, 500),
            (ErrorCode::ProtocolMismatch, 500),
            (ErrorCode::Unknown, 500),
        ];
        for (code, status) in cases {
            assert_eq!(status_for(code).as_u16(), status, "{code:?}");
        }
    }

    #[test]
    fn daemon_errors_keep_code_message_and_details() {
        let mut error = ApiError::new(ErrorCode::OutOfScope, "not yours");
        error.details = Some(serde_json::json!({"scope": "/x"}));
        let http = HttpError::from(error);
        assert_eq!(http.status, StatusCode::FORBIDDEN);
        let json = serde_json::to_value(http.envelope()).unwrap();
        assert_eq!(
            json,
            serde_json::json!({"error": {"code": "out_of_scope", "message": "not yours",
                                         "details": {"scope": "/x"}}})
        );
    }
}
