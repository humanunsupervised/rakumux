use std::fmt;
use std::io::{self, Write};
use std::marker::PhantomData;
use std::sync::LazyLock;
use std::time::Duration;

use regex::Regex;
use serde::de::{Error as _, MapAccess, Visitor};
use serde::{Deserialize, Deserializer};
use serde_json::value::RawValue;

use crate::status::{SessionTitle, StatusText};

use super::CloudAIError;

pub(crate) const MAX_BODY_BYTES: usize = 16_384;
const MAX_REPORT_BYTES: usize = 2_048;
const MAX_OUTPUT_BYTES: usize = 8_192;
const LIFECYCLES: &[&str] = &["working", "blocked", "done", "idle", "unknown", "none"];
const ACTIVITY_PHASES: &[&str] = &[
    "working",
    "waiting_for_response",
    "running_tool",
    "needs_input",
    "idle",
    "done",
];

struct ContextPatterns {
    meaningful: Regex,
    prohibited: Regex,
}

static CONTEXT_PATTERNS: LazyLock<Result<ContextPatterns, regex::Error>> = LazyLock::new(|| {
    Ok(ContextPatterns {
        meaningful: Regex::new(r"[\p{L}\p{N}\p{P}\p{S}]")?,
        // Cs cannot occur in a Rust scalar string. JSON decoding rejects lone
        // surrogates; regex does not support the otherwise redundant Cs property.
        prohibited: Regex::new(
            r"[\x00-\x08\x0b\x0c\x0e-\x1f\x7f-\x{009f}\p{Cf}\p{Default_Ignorable_Code_Point}]",
        )?,
    })
});

/// A complete canonical request, with its original bounded serialized bytes sealed.
///
/// Validation does not redact secrets, establish consent or prove that an ID is
/// ephemeral. The owner must do that before constructing this low-level value.
#[derive(Clone)]
pub struct ValidatedStatusRequest {
    request_id: String,
    body: Vec<u8>,
}

impl ValidatedStatusRequest {
    /// Validate original UTF-8 JSON bytes, including whitespace and escape spelling.
    /// Duplicate decoded members, unknown fields and explicit optional nulls fail.
    pub fn from_slice(body: &[u8]) -> Result<Self, CloudAIError> {
        let request: RequestEnvelope = decode(body).map_err(|_| CloudAIError::InvalidRequest)?;
        if !valid_request_id(&request.request_id) || !request.context.valid() {
            return Err(CloudAIError::InvalidRequest);
        }
        Ok(Self {
            request_id: request.request_id,
            body: body.to_vec(),
        })
    }

    /// Serialize and validate a local value. This cannot certify the original
    /// bytes or duplicate members of JSON parsed elsewhere; use `from_slice` there.
    pub fn from_value(value: &serde_json::Value) -> Result<Self, CloudAIError> {
        let mut body = BoundedBody(Vec::new());
        serde_json::to_writer(&mut body, value).map_err(|_| CloudAIError::InvalidRequest)?;
        Self::from_slice(&body.0)
    }

    /// Public correlation value, compared exactly with the response.
    pub fn request_id(&self) -> &str {
        &self.request_id
    }

    /// The exact validated body. Treat this as private selected context, not logs.
    pub fn body(&self) -> &[u8] {
        &self.body
    }
}

struct BoundedBody(Vec<u8>);

impl Write for BoundedBody {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > MAX_BODY_BYTES - self.0.len() {
            return Err(io::Error::other("request body exceeded its bound"));
        }
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl fmt::Debug for ValidatedStatusRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ValidatedStatusRequest([REDACTED])")
    }
}

/// A fully validated, exactly correlated advisory response. Never executes actions.
#[derive(Clone)]
pub struct StatusResponse {
    request_id: String,
    text: StatusText,
    title: Option<SessionTitle>,
}

impl StatusResponse {
    /// Public correlation value already checked against the outstanding request.
    pub fn request_id(&self) -> &str {
        &self.request_id
    }

    /// Borrow the validated advisory line without normalization.
    pub fn text(&self) -> &StatusText {
        &self.text
    }

    /// Consume the response and retain only its advisory text.
    pub fn into_text(self) -> StatusText {
        self.text
    }

    /// The session title, when the service sent one (an older one never does).
    pub fn title(&self) -> Option<&SessionTitle> {
        self.title.as_ref()
    }

    /// Consume the response: its advisory text and its title, if any.
    pub fn into_parts(self) -> (StatusText, Option<SessionTitle>) {
        (self.text, self.title)
    }
}

impl fmt::Debug for StatusResponse {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("StatusResponse([REDACTED])")
    }
}

/// The six canonical public error classifications; no remote diagnostic strings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RemoteErrorCode {
    /// The request violates the public input contract.
    InvalidRequest,
    /// Authentication is missing or invalid.
    Unauthenticated,
    /// AI session status is not authorized.
    Forbidden,
    /// The service is rate limited.
    RateLimited,
    /// The upstream service is temporarily unavailable.
    UpstreamUnavailable,
    /// Generated text failed the output contract.
    InvalidOutput,
}

impl<'de> Deserialize<'de> for RemoteErrorCode {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        // Derived unit enums also accept externally tagged objects, but the
        // canonical code field permits only an exact string.
        match String::deserialize(deserializer)?.as_str() {
            "invalid_request" => Ok(Self::InvalidRequest),
            "unauthenticated" => Ok(Self::Unauthenticated),
            "forbidden" => Ok(Self::Forbidden),
            "rate_limited" => Ok(Self::RateLimited),
            "upstream_unavailable" => Ok(Self::UpstreamUnavailable),
            "invalid_output" => Ok(Self::InvalidOutput),
            _ => Err(D::Error::custom("invalid error code")),
        }
    }
}

impl RemoteErrorCode {
    /// Exact canonical wire spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::InvalidRequest => "invalid_request",
            Self::Unauthenticated => "unauthenticated",
            Self::Forbidden => "forbidden",
            Self::RateLimited => "rate_limited",
            Self::UpstreamUnavailable => "upstream_unavailable",
            Self::InvalidOutput => "invalid_output",
        }
    }

    /// Exact static schema message, never the untrusted response's message storage.
    pub fn message(self) -> &'static str {
        match self {
            Self::InvalidRequest => "Invalid session-status request.",
            Self::Unauthenticated => "Sign in to use AI session status.",
            Self::Forbidden => "AI session status is not authorized.",
            Self::RateLimited => "Too many AI session-status requests.",
            Self::UpstreamUnavailable => "AI session status is temporarily unavailable.",
            Self::InvalidOutput => "AI session status returned invalid output.",
        }
    }

    pub(crate) fn accepts_status(self, status: u16) -> bool {
        match self {
            Self::InvalidRequest => matches!(status, 400 | 405 | 413 | 415),
            Self::Unauthenticated => status == 401,
            Self::Forbidden => status == 403,
            Self::RateLimited => status == 429,
            Self::UpstreamUnavailable => status == 503,
            Self::InvalidOutput => status == 502,
        }
    }

    pub(crate) fn permits_retry_delay(self) -> bool {
        matches!(self, Self::RateLimited | Self::UpstreamUnavailable)
    }
}

pub(crate) fn decode_success(
    body: &[u8],
    expected_id: &str,
) -> Result<StatusResponse, CloudAIError> {
    let response: SuccessEnvelope = decode(body)?;
    if !valid_request_id(&response.request_id) {
        return Err(CloudAIError::InvalidResponse);
    }
    if response.request_id != expected_id {
        return Err(CloudAIError::CorrelationMismatch);
    }
    Ok(StatusResponse {
        request_id: response.request_id,
        text: response.status_text,
        title: response.title,
    })
}

pub(crate) fn decode_error(
    body: &[u8],
) -> Result<(RemoteErrorCode, Option<Duration>), CloudAIError> {
    let envelope: ErrorEnvelope = decode(body)?;
    let error = envelope.error;
    if error.message != error.code.message()
        || (error.retry_after_seconds.is_some() && !error.code.permits_retry_delay())
    {
        return Err(CloudAIError::InvalidResponse);
    }
    Ok((
        error.code,
        error
            .retry_after_seconds
            .map(|delay| Duration::from_secs(delay.0.into())),
    ))
}

fn decode<'de, T: Deserialize<'de>>(body: &'de [u8]) -> Result<T, CloudAIError> {
    if body.len() > MAX_BODY_BYTES || std::str::from_utf8(body).is_err() {
        return Err(CloudAIError::InvalidResponse);
    }
    // Typed, strict structs reject duplicate decoded field names, including
    // escaped spellings. No Value/map parse may erase duplicates first.
    let mut deserializer = serde_json::Deserializer::from_slice(body);
    let value = object(&mut deserializer).map_err(|_| CloudAIError::InvalidResponse)?;
    deserializer
        .end()
        .map_err(|_| CloudAIError::InvalidResponse)?;
    Ok(value)
}

// Derived serde structs also support positional arrays. Every schema object
// explicitly requires a map, at both the root and nested object boundaries.
fn object<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    struct ObjectVisitor<T>(PhantomData<T>);

    impl<'de, T: Deserialize<'de>> Visitor<'de> for ObjectVisitor<T> {
        type Value = T;

        fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("a JSON object")
        }

        fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<T, A::Error> {
            T::deserialize(serde::de::value::MapAccessDeserializer::new(map))
        }
    }

    deserializer.deserialize_map(ObjectVisitor(PhantomData))
}

fn valid_request_id(value: &str) -> bool {
    (1..=64).contains(&value.len())
        && value.as_bytes()[0].is_ascii_alphanumeric()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RequestEnvelope {
    #[serde(rename = "version")]
    _version: VersionOne,
    request_id: String,
    #[serde(deserialize_with = "object")]
    context: RequestContext,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RequestContext {
    lifecycle: String,
    #[serde(default, deserialize_with = "present")]
    activity_phase: Option<String>,
    #[serde(default, deserialize_with = "present")]
    reported_text: Option<String>,
    #[serde(default, deserialize_with = "present")]
    recent_output: Option<String>,
}

impl RequestContext {
    fn valid(&self) -> bool {
        let Ok(patterns) = CONTEXT_PATTERNS.as_ref() else {
            return false;
        };
        LIFECYCLES.contains(&self.lifecycle.as_str())
            && self
                .activity_phase
                .as_deref()
                .is_none_or(|phase| ACTIVITY_PHASES.contains(&phase))
            && [
                (&self.reported_text, MAX_REPORT_BYTES),
                (&self.recent_output, MAX_OUTPUT_BYTES),
            ]
            .iter()
            .all(|(text, limit)| {
                text.as_deref()
                    .is_none_or(|text| text.len() <= *limit && !patterns.prohibited.is_match(text))
            })
            && [&self.reported_text, &self.recent_output]
                .iter()
                .any(|text| {
                    text.as_deref()
                        .is_some_and(|text| patterns.meaningful.is_match(text))
                })
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SuccessEnvelope {
    #[serde(rename = "version")]
    _version: VersionOne,
    request_id: String,
    status_text: StatusText,
    #[serde(default, deserialize_with = "present")]
    title: Option<SessionTitle>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ErrorEnvelope {
    #[serde(deserialize_with = "object")]
    error: ErrorFields,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ErrorFields {
    code: RemoteErrorCode,
    message: String,
    #[serde(default, deserialize_with = "present")]
    retry_after_seconds: Option<RetrySeconds>,
}

fn present<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    T::deserialize(deserializer).map(Some)
}

struct VersionOne;

impl<'de> Deserialize<'de> for VersionOne {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = Box::<RawValue>::deserialize(deserializer)?;
        if bounded_integer(raw.get(), 1) != Some(1) {
            return Err(D::Error::custom("invalid protocol version"));
        }
        Ok(Self)
    }
}

struct RetrySeconds(u16);

impl<'de> Deserialize<'de> for RetrySeconds {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = Box::<RawValue>::deserialize(deserializer)?;
        bounded_integer(raw.get(), 3600)
            .map(Self)
            .ok_or_else(|| D::Error::custom("invalid retry delay"))
    }
}

// JSON Schema integers are mathematical integers, not just integer spellings.
// Use decimal arithmetic on a syntactically parsed RawValue, never lossy f64:
// 1.0 and 1e0 pass, but 1.0000000000000000001 must not round down to 1.
fn bounded_integer(raw: &str, maximum: u16) -> Option<u16> {
    if !raw.as_bytes().first()?.is_ascii_digit() {
        return None;
    }
    let (mantissa, exponent) = match raw.split_once(['e', 'E']) {
        Some((mantissa, exponent)) => {
            let negative = exponent.starts_with('-');
            let digits = exponent.trim_start_matches(['+', '-']);
            let magnitude = digits.bytes().try_fold(0i32, |value, byte| {
                byte.is_ascii_digit()
                    .then(|| (value * 10 + i32::from(byte - b'0')).min(32_768))
            })?;
            (mantissa, if negative { -magnitude } else { magnitude })
        }
        None => (raw, 0),
    };
    let fractional_digits = mantissa.split_once('.').map_or(0, |(_, tail)| tail.len());
    let digits: String = mantissa
        .chars()
        .filter(|character| *character != '.')
        .collect();
    if !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let significant = digits.trim_start_matches('0');
    if significant.is_empty() {
        return None;
    }
    let trimmed = significant.trim_end_matches('0');
    let scale = exponent - fractional_digits as i32 + (significant.len() - trimmed.len()) as i32;
    if scale < 0 || trimmed.len() as i32 + scale > 4 {
        return None;
    }
    let value = trimmed
        .parse::<u16>()
        .ok()?
        .checked_mul(10u16.checked_pow(scale as u32)?)?;
    (value <= maximum).then_some(value)
}

#[cfg(test)]
mod tests;
