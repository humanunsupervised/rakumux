use std::fmt;
use std::io::{self, Cursor, Read};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use reqwest::blocking::{Body, Client, Request};
use reqwest::header::{self, HeaderMap, HeaderValue};
use reqwest::{redirect, Method, Url};

use crate::auth::{NetworkFailure, SecretValue};

use super::wire::{decode_error, decode_success, MAX_BODY_BYTES};
use super::{CloudAIError, StatusResponse, ValidatedStatusRequest};

const STATUS_PATH: &str = "/api/v1/ai/session-status";
const MAX_ORIGIN_BYTES: usize = 2048;
const MAX_AUTHORIZATION_BYTES: usize = 8192;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const TOTAL_TIMEOUT: Duration = Duration::from_secs(15);

/// Explicit reviewed public service origin, with one fixed endpoint and no fallback.
#[derive(Clone)]
pub struct CloudAIConfig {
    endpoint: Url,
}

impl CloudAIConfig {
    /// Accept a clean ASCII HTTPS root origin of at most 2048 bytes, with an
    /// optional explicit port and trailing slash. No credentials, path, query,
    /// fragment, URL escapes, backslashes or whitespace are accepted.
    ///
    /// The caller must review the deployment origin. No environment, discovery,
    /// model output or default origin is consulted by this adapter.
    pub fn new(origin: &str) -> Result<Self, CloudAIError> {
        let invalid = || CloudAIError::InvalidConfiguration("expected a clean HTTPS root origin");
        if origin.len() > MAX_ORIGIN_BYTES
            || !origin.bytes().all(|byte| byte.is_ascii_graphic())
            || origin
                .bytes()
                .any(|byte| matches!(byte, b'@' | b'?' | b'#' | b'%' | b'\\'))
        {
            return Err(invalid());
        }
        let authority = origin.strip_prefix("https://").ok_or_else(invalid)?;
        if authority.is_empty()
            || authority
                .split_once('/')
                .is_some_and(|(_, remainder)| !remainder.is_empty())
        {
            return Err(invalid());
        }
        let mut endpoint = Url::parse(origin).map_err(|_| invalid())?;
        if endpoint.scheme() != "https"
            || endpoint.host_str().is_none()
            || endpoint.path() != "/"
            || !endpoint.username().is_empty()
            || endpoint.password().is_some()
            || endpoint.query().is_some()
            || endpoint.fragment().is_some()
        {
            return Err(invalid());
        }
        endpoint.set_path(STATUS_PATH);
        Ok(Self { endpoint })
    }
}

impl fmt::Debug for CloudAIConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("CloudAIConfig([REDACTED])")
    }
}

/// Cooperative cancellation: suppress dispatch and discard late results.
///
/// Blocking I/O cannot be interrupted by this flag. It remains bounded by the
/// client's 15-second total timeout. Accepted upstream work/billing may continue.
#[derive(Clone, Default)]
pub struct Cancellation {
    canceled: Arc<AtomicBool>,
}

impl Cancellation {
    /// Create a fresh, uncanceled operation flag.
    pub fn new() -> Self {
        Self::default()
    }

    /// Permanently cancel every clone of this operation flag.
    pub fn cancel(&self) {
        self.canceled.store(true, Ordering::Release);
    }

    /// Whether cancellation has been requested.
    pub fn is_canceled(&self) -> bool {
        self.canceled.load(Ordering::Acquire)
    }

    fn check(&self) -> Result<(), CloudAIError> {
        if self.is_canceled() {
            Err(CloudAIError::Canceled)
        } else {
            Ok(())
        }
    }
}

/// Blocking native JSON client. Construct and invoke only off the UI thread.
///
/// Performs exactly one request, with no redirects, retries or inherited proxy
/// configuration. This is not a consent, account or IPC permission boundary.
/// The owner must serialize its final `is_current` check, dispatch and revocation,
/// and fence stale completions independently of public request correlation.
pub struct CloudAIClient {
    config: CloudAIConfig,
    transport: Box<dyn HttpTransport>,
}

impl CloudAIClient {
    /// Canonical public destination for the local owner's explicit consent offer.
    pub(crate) fn origin(&self) -> String {
        self.config.endpoint.origin().ascii_serialization()
    }

    /// Build a HTTPS-only transport without making a request or reading sessions.
    pub fn new(config: CloudAIConfig) -> Result<Self, CloudAIError> {
        let client = Client::builder()
            .https_only(true)
            .redirect(redirect::Policy::none())
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(TOTAL_TIMEOUT)
            // Hyper can retry an unstarted request on a lost reused connection,
            // independently of reqwest's body-replay check. Disable that pool.
            .pool_max_idle_per_host(0)
            .no_proxy()
            .no_gzip()
            .no_brotli()
            .no_deflate()
            .build()
            .map_err(network_error)?;
        Ok(Self::with_transport(
            config,
            Box::new(ReqwestTransport { client }),
        ))
    }

    /// Send exactly the sealed context once. The complete `Bearer ` header must
    /// fit 8192 bytes. Cancellation wins over late transport/read failures or data.
    /// No URLs, tokens, response bodies or raw transport errors enter diagnostics.
    pub fn generate(
        &self,
        access_token: &SecretValue,
        request: &ValidatedStatusRequest,
        cancel: &Cancellation,
    ) -> Result<StatusResponse, CloudAIError> {
        cancel.check()?;
        let mut outgoing = Request::new(Method::POST, self.config.endpoint.clone());
        // A blocking client's timeout alone only bounds individual waits. This
        // request timeout also creates the async deadline retained by its body.
        *outgoing.timeout_mut() = Some(TOTAL_TIMEOUT);
        let headers = outgoing.headers_mut();
        headers.insert(header::AUTHORIZATION, bearer_header(access_token)?);
        headers.insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        );
        headers.insert(header::ACCEPT, HeaderValue::from_static("application/json"));
        headers.insert(
            header::ACCEPT_ENCODING,
            HeaderValue::from_static("identity"),
        );
        let body_length = request.body().len() as u64;
        headers.insert(header::CONTENT_LENGTH, HeaderValue::from(body_length));
        // Vec::into() creates a reusable body that reqwest may replay on H2
        // REFUSED_STREAM/GOAWAY. A sized reader becomes a non-reusable stream,
        // while preserving exact bytes and Content-Length without chunking.
        *outgoing.body_mut() = Some(Body::sized(
            Cursor::new(request.body().to_vec()),
            body_length,
        ));
        cancel.check()?;
        let result = self.transport.send(outgoing);
        cancel.check()?;
        let reply = result?;
        if (300..=399).contains(&reply.status) {
            return Err(CloudAIError::HttpStatus(reply.status));
        }
        require_json_headers(&reply.headers)?;
        let body = read_body(reply.body, cancel)?;
        let result = if reply.status == 200 {
            decode_success(&body, request.request_id())
        } else {
            remote_failure(reply.status, &reply.headers, &body)
        };
        cancel.check()?;
        result
    }

    fn with_transport(config: CloudAIConfig, transport: Box<dyn HttpTransport>) -> Self {
        Self { config, transport }
    }
}

fn bearer_header(token: &SecretValue) -> Result<HeaderValue, CloudAIError> {
    if token.expose_secret().len() > MAX_AUTHORIZATION_BYTES - "Bearer ".len() {
        return Err(CloudAIError::InvalidRequest);
    }
    let mut value = HeaderValue::from_str(&format!("Bearer {}", token.expose_secret()))
        .map_err(|_| CloudAIError::InvalidRequest)?;
    value.set_sensitive(true);
    Ok(value)
}

struct HttpReply {
    status: u16,
    headers: HeaderMap,
    body: Box<dyn Read + Send>,
}

trait HttpTransport: Send + Sync {
    fn send(&self, request: Request) -> Result<HttpReply, CloudAIError>;
}

struct ReqwestTransport {
    client: Client,
}

impl HttpTransport for ReqwestTransport {
    fn send(&self, request: Request) -> Result<HttpReply, CloudAIError> {
        let response = self.client.execute(request).map_err(network_error)?;
        Ok(HttpReply {
            status: response.status().as_u16(),
            headers: response.headers().clone(),
            body: Box::new(response),
        })
    }
}

fn read_body(body: Box<dyn Read + Send>, cancel: &Cancellation) -> Result<Vec<u8>, CloudAIError> {
    let mut bounded = body.take((MAX_BODY_BYTES + 1) as u64);
    let mut output = Vec::new();
    let mut buffer = [0u8; 4096];
    loop {
        cancel.check()?;
        let result = bounded.read(&mut buffer);
        cancel.check()?;
        let count = result.map_err(response_read_error)?;
        if count == 0 {
            return Ok(output);
        }
        output.extend_from_slice(&buffer[..count]);
        if output.len() > MAX_BODY_BYTES {
            return Err(CloudAIError::InvalidResponse);
        }
    }
}

fn response_read_error(error: io::Error) -> CloudAIError {
    // reqwest blocking Response::read uses Other(reqwest::Error), including
    // async body-deadline and blocking wait timeouts. Inspect the typed inner
    // error rather than formatting it or relying only on the outer I/O kind.
    let timed_out = error.kind() == io::ErrorKind::TimedOut
        || error
            .get_ref()
            .and_then(|source| source.downcast_ref::<reqwest::Error>())
            .is_some_and(reqwest::Error::is_timeout);
    CloudAIError::Network(if timed_out {
        NetworkFailure::Timeout
    } else {
        NetworkFailure::Request
    })
}

fn require_json_headers(headers: &HeaderMap) -> Result<(), CloudAIError> {
    let media =
        single_header(headers, header::CONTENT_TYPE).ok_or(CloudAIError::InvalidResponse)?;
    let mut parts = media.split(';').map(str::trim);
    if !parts
        .next()
        .is_some_and(|value| value.eq_ignore_ascii_case("application/json"))
    {
        return Err(CloudAIError::InvalidResponse);
    }
    if let Some(parameter) = parts.next() {
        let (name, value) = parameter
            .split_once('=')
            .ok_or(CloudAIError::InvalidResponse)?;
        let value = value.trim();
        if !name.trim().eq_ignore_ascii_case("charset")
            || !(value.eq_ignore_ascii_case("utf-8") || value.eq_ignore_ascii_case("\"utf-8\""))
            || parts.next().is_some()
        {
            return Err(CloudAIError::InvalidResponse);
        }
    }
    if headers.contains_key(header::CONTENT_ENCODING)
        && !single_header(headers, header::CONTENT_ENCODING)
            .is_some_and(|value| value.trim().eq_ignore_ascii_case("identity"))
    {
        return Err(CloudAIError::InvalidResponse);
    }
    Ok(())
}

fn single_header(headers: &HeaderMap, name: header::HeaderName) -> Option<&str> {
    let mut values = headers.get_all(name).iter();
    let value = values.next()?.to_str().ok()?;
    values.next().is_none().then_some(value)
}

fn retry_after(headers: &HeaderMap) -> Option<Duration> {
    let raw = single_header(headers, header::RETRY_AFTER)?;
    if raw.is_empty() || !raw.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let seconds = raw.parse::<u64>().ok()?;
    (1..=3600)
        .contains(&seconds)
        .then(|| Duration::from_secs(seconds))
}

fn remote_failure(
    status: u16,
    headers: &HeaderMap,
    body: &[u8],
) -> Result<StatusResponse, CloudAIError> {
    let (code, body_delay) = decode_error(body)?;
    if !code.accepts_status(status) {
        return Err(CloudAIError::InvalidResponse);
    }
    let header_delay = code
        .permits_retry_delay()
        .then(|| retry_after(headers))
        .flatten();
    Err(CloudAIError::Remote {
        status,
        code,
        retry_after: body_delay.max(header_delay),
    })
}

fn network_error(error: reqwest::Error) -> CloudAIError {
    let failure = if error.is_timeout() {
        NetworkFailure::Timeout
    } else if error.is_connect() {
        NetworkFailure::Connection
    } else {
        NetworkFailure::Request
    };
    CloudAIError::Network(failure)
}

#[cfg(test)]
mod tests;
