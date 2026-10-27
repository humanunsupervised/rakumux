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
const FEATURES_PATH: &str = "/api/v1/features";
const FEED_PATH: &str = "/api/v1/feed";
const FEED_ENTRIES_PATH: &str = "/api/v1/feed/entries";
const FEED_MESSAGES_PATH: &str = "/api/v1/feed/messages";
const FEED_TURN_PATH: &str = "/api/v1/feed/turn";
const FEED_REPLIES_PATH: &str = "/api/v1/feed/replies";
const FEED_REPLY_STATUS_PATH: &str = "/api/v1/feed/replies/status";
const FEED_SESSIONS_PATH: &str = "/api/v1/feed/sessions";
/// Followed by `/{id}`, a ULID.
const FEED_ATTACHMENTS_PATH: &str = "/api/v1/feed/replies/attachments";
/// The largest feed page read (`contracts/feed/page.schema.json`), and the
/// largest attached file (4 MiB, read up to its declared size).
pub(crate) const MAX_FEED_BODY_BYTES: usize = 4 * 1024 * 1024;
const MAX_ORIGIN_BYTES: usize = 2048;
const MAX_AUTHORIZATION_BYTES: usize = 8192;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const TOTAL_TIMEOUT: Duration = Duration::from_secs(15);
/// An attached file's download, up to 4 MiB: about 70 KB/s at the least.
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(60);

/// Explicit reviewed public service origin, with fixed endpoints and no fallback.
#[derive(Clone)]
pub struct CloudAIConfig {
    endpoint: Url,
    features: Url,
    feed: Url,
    feed_entries: Url,
    feed_messages: Url,
    feed_turn: Url,
    feed_replies: Url,
    feed_reply_status: Url,
    feed_sessions: Url,
    feed_attachments: Url,
}

/// The account's feature switches, as the destination reports them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AccountFeatures {
    /// AI session summaries: on unless the user turned them off on the website.
    pub ai_session_summaries: bool,
    /// The agent feed: off unless the user turned it on on the website.
    pub feed: bool,
}

/// Feature switches to change for the signed-in account; `None` leaves one as it is.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FeatureChange {
    pub ai_session_summaries: Option<bool>,
    pub feed: Option<bool>,
}

impl FeatureChange {
    /// Nothing to change.
    pub fn is_empty(&self) -> bool {
        self.ai_session_summaries.is_none() && self.feed.is_none()
    }

    /// `features` with this change made.
    pub fn apply(&self, features: AccountFeatures) -> AccountFeatures {
        AccountFeatures {
            ai_session_summaries: self
                .ai_session_summaries
                .unwrap_or(features.ai_session_summaries),
            feed: self.feed.unwrap_or(features.feed),
        }
    }

    /// This change, then `later` over it.
    pub fn then(self, later: FeatureChange) -> FeatureChange {
        FeatureChange {
            ai_session_summaries: later.ai_session_summaries.or(self.ai_session_summaries),
            feed: later.feed.or(self.feed),
        }
    }
}

/// One agent feed request (`contracts/feed/`).
#[derive(Clone, Copy, Debug)]
pub(crate) enum FeedHttp<'a> {
    /// `POST /api/v1/feed/entries` with this upload body.
    Upload(&'a [u8]),
    /// `GET /api/v1/feed` with this query (`after=…&limit=…`).
    Page(&'a str),
    /// `POST /api/v1/feed/messages` with this chat upload body.
    Messages(&'a [u8]),
    /// `GET /api/v1/feed/turn` with this query (`entry=…`).
    Turn(&'a str),
    /// `POST /api/v1/feed/replies` with this reply.
    ReplyUpload(&'a [u8]),
    /// `GET /api/v1/feed/replies` with this query (`after=…`).
    Replies(&'a str),
    /// `POST /api/v1/feed/replies/status` with this status.
    ReplyStatus(&'a [u8]),
    /// `POST /api/v1/feed/sessions` with this machine's session list.
    Sessions(&'a [u8]),
    /// `GET /api/v1/feed/replies/attachments/{id}`: a file of `size` bytes.
    Attachment { id: &'a str, size: u64 },
}

/// A feed reply as received: the feed module decodes it against its contract.
pub(crate) struct FeedHttpReply {
    pub status: u16,
    pub retry_after: Option<Duration>,
    pub body: Vec<u8>,
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
        let path = |path: &str| {
            let mut url = endpoint.clone();
            url.set_path(path);
            url
        };
        let (features, feed, feed_entries) = (
            path(FEATURES_PATH),
            path(FEED_PATH),
            path(FEED_ENTRIES_PATH),
        );
        let (feed_messages, feed_turn) = (path(FEED_MESSAGES_PATH), path(FEED_TURN_PATH));
        let (feed_replies, feed_reply_status) =
            (path(FEED_REPLIES_PATH), path(FEED_REPLY_STATUS_PATH));
        let feed_sessions = path(FEED_SESSIONS_PATH);
        let feed_attachments = path(FEED_ATTACHMENTS_PATH);
        endpoint.set_path(STATUS_PATH);
        Ok(Self {
            endpoint,
            features,
            feed,
            feed_entries,
            feed_messages,
            feed_turn,
            feed_replies,
            feed_reply_status,
            feed_sessions,
            feed_attachments,
        })
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

    /// Read the signed-in account's feature switches (`GET /api/v1/features`) once,
    /// with the same transport rules as `generate`.
    pub fn features(
        &self,
        access_token: &SecretValue,
        cancel: &Cancellation,
    ) -> Result<AccountFeatures, CloudAIError> {
        cancel.check()?;
        let mut outgoing = Request::new(Method::GET, self.config.features.clone());
        *outgoing.timeout_mut() = Some(TOTAL_TIMEOUT);
        let headers = outgoing.headers_mut();
        headers.insert(header::AUTHORIZATION, bearer_header(access_token)?);
        headers.insert(header::ACCEPT, HeaderValue::from_static("application/json"));
        headers.insert(
            header::ACCEPT_ENCODING,
            HeaderValue::from_static("identity"),
        );
        let result = self.transport.send(outgoing);
        cancel.check()?;
        let reply = result?;
        if (300..=399).contains(&reply.status) {
            return Err(CloudAIError::HttpStatus(reply.status));
        }
        require_json_headers(&reply.headers)?;
        let body = read_body(reply.body, cancel)?;
        let result = if reply.status == 200 {
            decode_features(&body)
        } else {
            remote_failure(reply.status, &reply.headers, &body)
        };
        cancel.check()?;
        result
    }

    /// Make one agent feed request with the same transport rules as `generate`.
    /// Any JSON reply is returned for the feed module to decode, error or not.
    pub(crate) fn feed(
        &self,
        access_token: &SecretValue,
        request: FeedHttp<'_>,
        cancel: &Cancellation,
    ) -> Result<FeedHttpReply, CloudAIError> {
        cancel.check()?;
        if let FeedHttp::Attachment { id, size } = request {
            return self.download(access_token, id, size, cancel);
        }
        let mut outgoing = match request {
            FeedHttp::Upload(body)
            | FeedHttp::Messages(body)
            | FeedHttp::ReplyUpload(body)
            | FeedHttp::ReplyStatus(body)
            | FeedHttp::Sessions(body) => {
                let url = match request {
                    FeedHttp::Messages(_) => &self.config.feed_messages,
                    FeedHttp::ReplyUpload(_) => &self.config.feed_replies,
                    FeedHttp::ReplyStatus(_) => &self.config.feed_reply_status,
                    FeedHttp::Sessions(_) => &self.config.feed_sessions,
                    _ => &self.config.feed_entries,
                };
                let mut outgoing = Request::new(Method::POST, url.clone());
                let headers = outgoing.headers_mut();
                headers.insert(
                    header::CONTENT_TYPE,
                    HeaderValue::from_static("application/json"),
                );
                let length = body.len() as u64;
                headers.insert(header::CONTENT_LENGTH, HeaderValue::from(length));
                // A sized reader is never replayed (see `generate`).
                *outgoing.body_mut() = Some(Body::sized(Cursor::new(body.to_vec()), length));
                outgoing
            }
            FeedHttp::Page(query) | FeedHttp::Turn(query) | FeedHttp::Replies(query) => {
                let mut url = match request {
                    FeedHttp::Turn(_) => self.config.feed_turn.clone(),
                    FeedHttp::Replies(_) => self.config.feed_replies.clone(),
                    _ => self.config.feed.clone(),
                };
                url.set_query(Some(query));
                Request::new(Method::GET, url)
            }
            FeedHttp::Attachment { .. } => return Err(CloudAIError::InvalidRequest),
        };
        *outgoing.timeout_mut() = Some(TOTAL_TIMEOUT);
        let headers = outgoing.headers_mut();
        headers.insert(header::AUTHORIZATION, bearer_header(access_token)?);
        headers.insert(header::ACCEPT, HeaderValue::from_static("application/json"));
        headers.insert(
            header::ACCEPT_ENCODING,
            HeaderValue::from_static("identity"),
        );
        cancel.check()?;
        let result = self.transport.send(outgoing);
        cancel.check()?;
        let reply = result?;
        if (300..=399).contains(&reply.status) {
            return Err(CloudAIError::HttpStatus(reply.status));
        }
        require_json_headers(&reply.headers)?;
        let retry_after = retry_after(&reply.headers);
        let body = read_bounded(reply.body, cancel, MAX_FEED_BODY_BYTES)?;
        cancel.check()?;
        Ok(FeedHttpReply {
            status: reply.status,
            retry_after,
            body,
        })
    }

    /// Fetch a file attached to a reply once, with the same transport rules as
    /// `feed` but a deadline long enough for 4 MiB. A 200's body is bytes,
    /// never parsed, read up to one byte past `size` so another length shows;
    /// its headers say nothing about it but a content coding, which is
    /// refused. Any other answer is returned for the feed module to decode
    /// like every feed error.
    fn download(
        &self,
        access_token: &SecretValue,
        id: &str,
        size: u64,
        cancel: &Cancellation,
    ) -> Result<FeedHttpReply, CloudAIError> {
        if !crate::feed::is_entry_id(id) || size > MAX_FEED_BODY_BYTES as u64 {
            return Err(CloudAIError::InvalidRequest);
        }
        let mut url = self.config.feed_attachments.clone();
        url.set_path(&format!("{FEED_ATTACHMENTS_PATH}/{id}"));
        let mut outgoing = Request::new(Method::GET, url);
        *outgoing.timeout_mut() = Some(DOWNLOAD_TIMEOUT);
        let headers = outgoing.headers_mut();
        headers.insert(header::AUTHORIZATION, bearer_header(access_token)?);
        headers.insert(
            header::ACCEPT,
            HeaderValue::from_static("application/octet-stream, application/json"),
        );
        headers.insert(
            header::ACCEPT_ENCODING,
            HeaderValue::from_static("identity"),
        );
        cancel.check()?;
        let result = self.transport.send(outgoing);
        cancel.check()?;
        let reply = result?;
        if (300..=399).contains(&reply.status) {
            return Err(CloudAIError::HttpStatus(reply.status));
        }
        if reply.headers.contains_key(header::CONTENT_ENCODING)
            && !single_header(&reply.headers, header::CONTENT_ENCODING)
                .is_some_and(|value| value.trim().eq_ignore_ascii_case("identity"))
        {
            return Err(CloudAIError::InvalidResponse);
        }
        let retry_after = retry_after(&reply.headers);
        // An error's body is JSON of a few hundred bytes; a proxy's page is cut.
        let limit = if reply.status == 200 {
            size + 1
        } else {
            MAX_BODY_BYTES as u64
        };
        let body = read_up_to(reply.body, cancel, limit)?;
        cancel.check()?;
        Ok(FeedHttpReply {
            status: reply.status,
            retry_after,
            body,
        })
    }

    /// Change the signed-in account's feature switches (`PATCH /api/v1/features`,
    /// as the website's Features page does) and read them back, with the same
    /// transport rules as `features`.
    pub fn set_features(
        &self,
        access_token: &SecretValue,
        change: FeatureChange,
        cancel: &Cancellation,
    ) -> Result<AccountFeatures, CloudAIError> {
        cancel.check()?;
        let mut features = serde_json::Map::new();
        if let Some(on) = change.ai_session_summaries {
            features.insert("ai_session_summaries".into(), on.into());
        }
        if let Some(on) = change.feed {
            features.insert("feed".into(), on.into());
        }
        if features.is_empty() {
            return Err(CloudAIError::InvalidRequest);
        }
        let body = serde_json::to_vec(&serde_json::json!({"version": 1, "features": features}))
            .map_err(|_| CloudAIError::InvalidRequest)?;
        let mut outgoing = Request::new(Method::PATCH, self.config.features.clone());
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
        let length = body.len() as u64;
        headers.insert(header::CONTENT_LENGTH, HeaderValue::from(length));
        // A sized reader is never replayed (see `generate`).
        *outgoing.body_mut() = Some(Body::sized(Cursor::new(body), length));
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
            decode_features(&body)
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

fn decode_features(body: &[u8]) -> Result<AccountFeatures, CloudAIError> {
    #[derive(serde::Deserialize)]
    struct Wire {
        version: u32,
        features: Features,
    }
    #[derive(serde::Deserialize)]
    struct Features {
        // Newer switches are ignored; a missing one keeps the service default.
        #[serde(default = "enabled")]
        ai_session_summaries: bool,
        #[serde(default)]
        feed: bool,
    }
    fn enabled() -> bool {
        true
    }
    let wire: Wire = serde_json::from_slice(body).map_err(|_| CloudAIError::InvalidResponse)?;
    if wire.version != 1 {
        return Err(CloudAIError::InvalidResponse);
    }
    Ok(AccountFeatures {
        ai_session_summaries: wire.features.ai_session_summaries,
        feed: wire.features.feed,
    })
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
    read_bounded(body, cancel, MAX_BODY_BYTES)
}

fn read_bounded(
    body: Box<dyn Read + Send>,
    cancel: &Cancellation,
    maximum: usize,
) -> Result<Vec<u8>, CloudAIError> {
    let output = read_up_to(body, cancel, maximum as u64 + 1)?;
    if output.len() > maximum {
        return Err(CloudAIError::InvalidResponse);
    }
    Ok(output)
}

/// At most `limit` bytes of `body`; whatever follows is left unread.
fn read_up_to(
    body: Box<dyn Read + Send>,
    cancel: &Cancellation,
    limit: u64,
) -> Result<Vec<u8>, CloudAIError> {
    let mut bounded = body.take(limit);
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

fn remote_failure<T>(status: u16, headers: &HeaderMap, body: &[u8]) -> Result<T, CloudAIError> {
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
