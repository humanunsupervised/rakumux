use std::collections::VecDeque;
use std::io::{self, Cursor};
use std::sync::atomic::AtomicUsize;
use std::sync::Mutex;

use serde_json::json;

use super::*;
use crate::cloud::RemoteErrorCode;

const PUBLIC_ID: &str = "synthetic-public-001";
const SECRET: &str = "synthetic-access-token";

struct RecordedRequest {
    request: Request,
    body: Vec<u8>,
    body_was_streaming: bool,
    request_was_cloneable: bool,
}

struct FakeTransport {
    requests: Arc<Mutex<Vec<RecordedRequest>>>,
    replies: Mutex<VecDeque<Result<HttpReply, CloudAIError>>>,
    cancel_after_send: Option<Cancellation>,
}

impl HttpTransport for FakeTransport {
    fn send(&self, mut request: Request) -> Result<HttpReply, CloudAIError> {
        // Capture the production request's replay properties before consuming
        // its reader. Only this no-network fake buffers a detached body.
        // A GET (features) has no body.
        let body_was_streaming = request.body().is_some_and(|body| body.as_bytes().is_none());
        let request_was_cloneable = request.try_clone().is_some();
        let body = match request.body_mut().take() {
            Some(mut body) => body
                .buffer()
                .expect("synthetic request reader failed")
                .to_vec(),
            None => Vec::new(),
        };
        self.requests.lock().unwrap().push(RecordedRequest {
            request,
            body,
            body_was_streaming,
            request_was_cloneable,
        });
        if let Some(cancel) = &self.cancel_after_send {
            cancel.cancel();
        }
        self.replies
            .lock()
            .unwrap()
            .pop_front()
            .expect("unexpected extra HTTP dispatch")
    }
}

fn config() -> CloudAIConfig {
    CloudAIConfig::new("https://reviewed.example:8443/").unwrap()
}

fn client_with(
    result: Result<HttpReply, CloudAIError>,
    cancel_after_send: Option<Cancellation>,
) -> (CloudAIClient, Arc<Mutex<Vec<RecordedRequest>>>) {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let transport = FakeTransport {
        requests: requests.clone(),
        replies: Mutex::new(VecDeque::from([result])),
        cancel_after_send,
    };
    (
        CloudAIClient::with_transport(config(), Box::new(transport)),
        requests,
    )
}

fn request() -> ValidatedStatusRequest {
    ValidatedStatusRequest::from_value(&json!({
        "version":1,
        "request_id":PUBLIC_ID,
        "context":{
            "lifecycle":"working",
            "activity_phase":"running_tool",
            "reported_text":"Checking a synthetic example",
            "recent_output":"Synthetic checks passed\n"
        }
    }))
    .unwrap()
}

fn token() -> SecretValue {
    SecretValue::try_from(SECRET.to_owned()).unwrap()
}

fn json_headers() -> HeaderMap {
    HeaderMap::from_iter([(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    )])
}

fn reply(status: u16, body: impl Into<Vec<u8>>) -> HttpReply {
    HttpReply {
        status,
        headers: json_headers(),
        body: Box::new(Cursor::new(body.into())),
    }
}

fn success_body() -> Vec<u8> {
    serde_json::to_vec(
        &json!({"version":1,"request_id":PUBLIC_ID,"status_text":"Checking synthetic validation"}),
    )
    .unwrap()
}

fn success() -> HttpReply {
    reply(200, success_body())
}

fn error_reply(status: u16, code: RemoteErrorCode, delay: Option<u16>) -> HttpReply {
    let mut value = json!({"error":{"code":code.as_str(), "message":code.message()}});
    if let Some(delay) = delay {
        value["error"]["retry_after_seconds"] = json!(delay);
    }
    reply(status, serde_json::to_vec(&value).unwrap())
}

fn run(reply: HttpReply) -> Result<StatusResponse, CloudAIError> {
    let (client, _) = client_with(Ok(reply), None);
    client.generate(&token(), &request(), &Cancellation::new())
}

#[test]
fn reviewed_clean_https_root_configuration_only() {
    for origin in [
        "https://reviewed.example",
        "https://reviewed.example/",
        "https://reviewed.example:8443",
        "https://[2001:db8::1]:8443/",
    ] {
        let config = CloudAIConfig::new(origin).unwrap();
        assert_eq!(config.endpoint.path(), STATUS_PATH);
        assert!(config.endpoint.query().is_none());
        assert!(config.endpoint.fragment().is_none());
    }
    for origin in [
        "",
        "http://reviewed.example",
        "ftp://reviewed.example",
        "reviewed.example",
        "https:reviewed.example",
        "https:///reviewed.example",
        "https://",
        "https://reviewed.example/api/v1/ai/session-status",
        "https://reviewed.example/path/..",
        "https://reviewed.example/.",
        "https://reviewed.example//",
        "https://reviewed.example?",
        "https://reviewed.example/#fragment",
        "https://user:password@reviewed.example",
        "https://@reviewed.example",
        "https://reviewed.example\\@untrusted.example",
        "https://reviewed.example/\n",
        " https://reviewed.example",
        "https://reviewed.example\t",
        "https://reviewed.example/%2e",
        "https://revie%77ed.example",
        "https://réviewed.example",
        "https://reviewed.example:99999",
    ] {
        assert!(CloudAIConfig::new(origin).is_err(), "{origin:?}");
    }
    assert!(CloudAIConfig::new(&format!("https://{}", "a".repeat(MAX_ORIGIN_BYTES))).is_err());
    assert_eq!(CONNECT_TIMEOUT, Duration::from_secs(5));
    assert_eq!(TOTAL_TIMEOUT, Duration::from_secs(15));
    let diagnostic = format!("{:?}", config());
    assert!(!diagnostic.contains("reviewed.example"));
    assert!(diagnostic.contains("REDACTED"));
}

#[test]
fn recorded_request_has_only_fixed_native_headers_and_sealed_body() {
    let (client, recorded) = client_with(Ok(success()), None);
    let request = request();
    let response = client
        .generate(&token(), &request, &Cancellation::new())
        .unwrap();
    assert_eq!(response.request_id(), PUBLIC_ID);
    assert_eq!(response.text().as_str(), "Checking synthetic validation");
    let recorded = recorded.lock().unwrap();
    assert_eq!(recorded.len(), 1);
    let outgoing = &recorded[0].request;
    assert_eq!(outgoing.method(), Method::POST);
    assert_eq!(
        outgoing.url().as_str(),
        "https://reviewed.example:8443/api/v1/ai/session-status"
    );
    assert_eq!(outgoing.headers().len(), 5);
    assert_eq!(
        outgoing.headers()[header::AUTHORIZATION],
        format!("Bearer {SECRET}")
    );
    assert!(outgoing.headers()[header::AUTHORIZATION].is_sensitive());
    assert_eq!(outgoing.headers()[header::CONTENT_TYPE], "application/json");
    assert_eq!(outgoing.headers()[header::ACCEPT], "application/json");
    assert_eq!(outgoing.headers()[header::ACCEPT_ENCODING], "identity");
    assert_eq!(
        outgoing.headers()[header::CONTENT_LENGTH],
        request.body().len().to_string()
    );
    assert!(!outgoing.headers().contains_key(header::TRANSFER_ENCODING));
    assert_eq!(recorded[0].body, request.body());
    let value: serde_json::Value = serde_json::from_slice(request.body()).unwrap();
    assert_eq!(value.as_object().unwrap().len(), 3);
    assert_eq!(value["context"].as_object().unwrap().len(), 4);
    assert!(!String::from_utf8_lossy(request.body()).contains(SECRET));
    for field in [
        "account_id",
        "session_id",
        "path",
        "model",
        "provider_url",
        "consent_revision",
        "cache_key",
        "tools",
        "messages",
    ] {
        assert!(value.get(field).is_none());
        assert!(value["context"].get(field).is_none());
    }
}

#[test]
fn recorded_request_carries_the_total_async_body_deadline() {
    let (client, recorded) = client_with(Ok(success()), None);
    client
        .generate(&token(), &request(), &Cancellation::new())
        .unwrap();
    let recorded = recorded.lock().unwrap();
    assert_eq!(
        recorded[0].request.timeout(),
        Some(&Duration::from_secs(15))
    );
}

#[test]
fn recorded_body_is_fixed_length_streaming_and_not_cloneable_for_replay() {
    let (client, recorded) = client_with(Ok(success()), None);
    let request = request();
    client
        .generate(&token(), &request, &Cancellation::new())
        .unwrap();
    let recorded = recorded.lock().unwrap();
    assert!(
        recorded[0].body_was_streaming,
        "buffered bodies enable reqwest's H2 replay"
    );
    assert!(
        !recorded[0].request_was_cloneable,
        "a dispatched body must not be reusable"
    );
    assert_eq!(recorded[0].body, request.body());
    assert_eq!(
        recorded[0].request.headers()[header::CONTENT_LENGTH],
        recorded[0].body.len().to_string()
    );
}

#[test]
fn complete_bearer_header_cap_is_checked_before_transport() {
    for (size, allowed) in [
        (MAX_AUTHORIZATION_BYTES - 7, true),
        (MAX_AUTHORIZATION_BYTES - 6, false),
        (16_384, false),
    ] {
        let token = SecretValue::try_from("x".repeat(size)).unwrap();
        let (client, recorded) = client_with(Ok(success()), None);
        let result = client.generate(&token, &request(), &Cancellation::new());
        assert_eq!(result.is_ok(), allowed);
        assert_eq!(recorded.lock().unwrap().len(), usize::from(allowed));
        if !allowed {
            assert_eq!(result.unwrap_err(), CloudAIError::InvalidRequest);
        }
    }
}

#[test]
fn redirects_are_never_followed_or_retried() {
    for status in [301, 302, 303, 307, 308] {
        let mut response = reply(status, "synthetic untrusted redirect body");
        response.headers.insert(
            header::LOCATION,
            HeaderValue::from_static("https://untrusted.example/collect"),
        );
        let (client, recorded) = client_with(Ok(response), None);
        assert_eq!(
            client
                .generate(&token(), &request(), &Cancellation::new())
                .unwrap_err(),
            CloudAIError::HttpStatus(status)
        );
        assert_eq!(recorded.lock().unwrap().len(), 1);
    }
}

#[test]
fn only_json_utf8_media_and_identity_encoding_are_accepted() {
    for media in [
        "application/json",
        "APPLICATION/JSON",
        "application/json; charset=utf-8",
        "application/json; CHARSET = \"UTF-8\"",
    ] {
        let mut response = success();
        response
            .headers
            .insert(header::CONTENT_TYPE, HeaderValue::from_str(media).unwrap());
        assert!(run(response).is_ok(), "{media}");
    }
    for media in [
        "text/html",
        "application/problem+json",
        "application/jsonp",
        "application/json; charset=latin1",
        "application/json; charset=utf8",
        "application/json; charset=utf-8; charset=utf-8",
        "application/json; profile=untrusted",
        "application/json;",
        "application/json, text/html",
    ] {
        let mut response = success();
        response
            .headers
            .insert(header::CONTENT_TYPE, HeaderValue::from_str(media).unwrap());
        assert_eq!(
            run(response).unwrap_err(),
            CloudAIError::InvalidResponse,
            "{media}"
        );
    }
    let mut missing = success();
    missing.headers.clear();
    assert_eq!(run(missing).unwrap_err(), CloudAIError::InvalidResponse);
    let mut duplicate = success();
    duplicate.headers.append(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    assert_eq!(run(duplicate).unwrap_err(), CloudAIError::InvalidResponse);
    let mut malformed = success();
    malformed.headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_bytes(b"application/json;\xff").unwrap(),
    );
    assert_eq!(run(malformed).unwrap_err(), CloudAIError::InvalidResponse);
    for encoding in ["identity", "IDENTITY"] {
        let mut response = success();
        response
            .headers
            .insert(header::CONTENT_ENCODING, HeaderValue::from_static(encoding));
        assert!(run(response).is_ok());
    }
    for encoding in [
        "gzip",
        "br",
        "deflate",
        "identity, gzip",
        "identity, identity",
        "",
    ] {
        let mut response = success();
        response
            .headers
            .insert(header::CONTENT_ENCODING, HeaderValue::from_static(encoding));
        assert_eq!(run(response).unwrap_err(), CloudAIError::InvalidResponse);
    }
    let mut duplicate = success();
    duplicate.headers.append(
        header::CONTENT_ENCODING,
        HeaderValue::from_static("identity"),
    );
    duplicate.headers.append(
        header::CONTENT_ENCODING,
        HeaderValue::from_static("identity"),
    );
    assert_eq!(run(duplicate).unwrap_err(), CloudAIError::InvalidResponse);
    let mut encoded_error = error_reply(429, RemoteErrorCode::RateLimited, None);
    encoded_error
        .headers
        .insert(header::CONTENT_ENCODING, HeaderValue::from_static("gzip"));
    assert_eq!(
        run(encoded_error).unwrap_err(),
        CloudAIError::InvalidResponse
    );
}

struct CountingBody {
    remaining: usize,
    count: Arc<AtomicUsize>,
}

impl Read for CountingBody {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let count = self.remaining.min(buffer.len());
        buffer[..count].fill(b' ');
        self.remaining -= count;
        self.count.fetch_add(count, Ordering::Relaxed);
        Ok(count)
    }
}

#[test]
fn original_response_stream_is_bounded_without_trusting_content_length() {
    let mut exact = success_body();
    exact.resize(MAX_BODY_BYTES, b' ');
    assert!(run(reply(200, exact.clone())).is_ok());
    exact.push(b' ');
    assert_eq!(
        run(reply(200, exact)).unwrap_err(),
        CloudAIError::InvalidResponse
    );
    let count = Arc::new(AtomicUsize::new(0));
    let response = HttpReply {
        status: 200,
        headers: HeaderMap::from_iter([
            (
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/json"),
            ),
            (header::CONTENT_LENGTH, HeaderValue::from_static("1")),
        ]),
        body: Box::new(CountingBody {
            remaining: 1_000_000,
            count: count.clone(),
        }),
    };
    assert_eq!(run(response).unwrap_err(), CloudAIError::InvalidResponse);
    assert_eq!(count.load(Ordering::Relaxed), MAX_BODY_BYTES + 1);
    let mut declared_huge = success();
    declared_huge.headers.insert(
        header::CONTENT_LENGTH,
        HeaderValue::from_static("1000000000"),
    );
    assert!(
        run(declared_huge).is_ok(),
        "only received bytes establish the limit"
    );
    let mut error = serde_json::to_vec(
        &json!({"error":{"code":"rate_limited", "message":RemoteErrorCode::RateLimited.message()}}),
    )
    .unwrap();
    error.resize(MAX_BODY_BYTES + 1, b' ');
    assert_eq!(
        run(reply(429, error)).unwrap_err(),
        CloudAIError::InvalidResponse
    );
}

#[test]
fn unknown_malformed_duplicate_and_uncorrelated_successes_are_rejected() {
    for body in [
        r#"{"version":2,"request_id":"synthetic-public-001","status_text":"Checking"}"#,
        r#"{"version":1,"request_id":"synthetic-public-001","status_text":"Checking","tool_calls":[]}"#,
        r#"{"version":1,"request_id":"synthetic-public-001","status_text":"Checking","status_\u0074ext":"Checking"}"#,
        r#"{"version":1,"request_id":"synthetic-public-001","request_\u0069d":"synthetic-public-001","status_text":"Checking"}"#,
        r#"{"version":1,"request_id":"synthetic-public-001","status_text":null}"#,
        r#"{"version":1,"request_id":"synthetic-public-001","status_text":"Checking\ud800"}"#,
        r#"{"version":1,"request_id":"synthetic-public-001","status_text":"Checking\u001b[31m"}"#,
        r#"{"version":1,"request_id":"synthetic-public-001","status_text":"Checking\n"}"#,
        r#"{"version":1,"request_id":"synthetic-public-001","status_text":"Checking"}{}"#,
        r#"[1,"synthetic-public-001","Checking"]"#,
        "not JSON",
        "",
    ] {
        assert_eq!(
            run(reply(200, body)).unwrap_err(),
            CloudAIError::InvalidResponse,
            "{body}"
        );
    }
    for body in [vec![0xff], b"\xef\xbb\xbf{}".to_vec()] {
        assert_eq!(
            run(reply(200, body)).unwrap_err(),
            CloudAIError::InvalidResponse
        );
    }
    for id in ["synthetic-public-002", "Synthetic-public-001"] {
        let body =
            serde_json::to_vec(&json!({"version":1,"request_id":id,"status_text":"Checking"}))
                .unwrap();
        assert_eq!(
            run(reply(200, body)).unwrap_err(),
            CloudAIError::CorrelationMismatch
        );
    }
    // Matching correlation cannot bypass complete schema validation.
    assert_eq!(
        run(reply(
            200,
            format!(r#"{{"version":2,"request_id":"{PUBLIC_ID}","status_text":"Checking"}}"#)
        ))
        .unwrap_err(),
        CloudAIError::InvalidResponse
    );
}

#[test]
fn canonical_error_codes_are_bound_to_http_status_and_never_auto_retry() {
    for (code, statuses) in [
        (RemoteErrorCode::InvalidRequest, vec![400, 405, 413, 415]),
        (RemoteErrorCode::Unauthenticated, vec![401]),
        (RemoteErrorCode::Forbidden, vec![403]),
        (RemoteErrorCode::RateLimited, vec![429]),
        (RemoteErrorCode::UpstreamUnavailable, vec![503]),
        (RemoteErrorCode::InvalidOutput, vec![502]),
    ] {
        for status in statuses {
            let (client, recorded) = client_with(Ok(error_reply(status, code, None)), None);
            assert_eq!(
                client
                    .generate(&token(), &request(), &Cancellation::new())
                    .unwrap_err(),
                CloudAIError::Remote {
                    status,
                    code,
                    retry_after: None
                }
            );
            assert_eq!(recorded.lock().unwrap().len(), 1);
        }
        for status in [
            200, 201, 400, 401, 403, 404, 405, 413, 415, 418, 429, 500, 502, 503,
        ] {
            if !code.accepts_status(status) {
                assert_eq!(
                    run(error_reply(status, code, None)).unwrap_err(),
                    CloudAIError::InvalidResponse,
                    "{code:?} {status}"
                );
            }
        }
    }
}

#[test]
fn retry_after_is_optional_bounded_numeric_conservative_and_never_automatic() {
    for (header, expected) in [
        ("1", Some(1)),
        ("3600", Some(3600)),
        ("0002", Some(2)),
        ("0", None),
        ("3601", None),
        ("-1", None),
        ("1.0", None),
        ("+2", None),
        ("2e1", None),
        ("Wed, 21 Oct 2026 07:28:00 GMT", None),
        ("99999999999999999999999", None),
        (" 2", None),
        ("", None),
    ] {
        let mut response = error_reply(429, RemoteErrorCode::RateLimited, None);
        response
            .headers
            .insert(header::RETRY_AFTER, HeaderValue::from_str(header).unwrap());
        assert_eq!(
            run(response).unwrap_err(),
            CloudAIError::Remote {
                status: 429,
                code: RemoteErrorCode::RateLimited,
                retry_after: expected.map(Duration::from_secs)
            },
            "{header}"
        );
    }
    for (body_delay, header_delay, expected) in [(3, "7", 7), (7, "3", 7), (7, "3601", 7)] {
        let mut response = error_reply(503, RemoteErrorCode::UpstreamUnavailable, Some(body_delay));
        response
            .headers
            .insert(header::RETRY_AFTER, HeaderValue::from_static(header_delay));
        assert_eq!(
            run(response).unwrap_err(),
            CloudAIError::Remote {
                status: 503,
                code: RemoteErrorCode::UpstreamUnavailable,
                retry_after: Some(Duration::from_secs(expected))
            }
        );
    }
    let mut duplicate = error_reply(429, RemoteErrorCode::RateLimited, Some(3));
    duplicate
        .headers
        .append(header::RETRY_AFTER, HeaderValue::from_static("7"));
    duplicate
        .headers
        .append(header::RETRY_AFTER, HeaderValue::from_static("9"));
    assert_eq!(
        run(duplicate).unwrap_err(),
        CloudAIError::Remote {
            status: 429,
            code: RemoteErrorCode::RateLimited,
            retry_after: Some(Duration::from_secs(3))
        }
    );
    let mut forbidden = error_reply(403, RemoteErrorCode::Forbidden, None);
    forbidden
        .headers
        .insert(header::RETRY_AFTER, HeaderValue::from_static("5"));
    assert_eq!(
        run(forbidden).unwrap_err(),
        CloudAIError::Remote {
            status: 403,
            code: RemoteErrorCode::Forbidden,
            retry_after: None
        }
    );
    assert_eq!(
        run(error_reply(403, RemoteErrorCode::Forbidden, Some(5))).unwrap_err(),
        CloudAIError::InvalidResponse
    );
}

#[test]
fn arbitrary_remote_diagnostics_never_escape_errors() {
    for body in [
        r#"{"error":{"code":"upstream_unavailable","message":"synthetic-secret-provider-diagnostic"}}"#,
        r#"{"error":{"code":"unknown","message":"synthetic-secret-provider-diagnostic"}}"#,
        r#"{"error":{"code":"upstream_unavailable","message":"AI session status is temporarily unavailable.","detail":"synthetic-secret-provider-diagnostic"}}"#,
        r#"{"error":{"code":"upstream_unavailable","co\u0064e":"upstream_unavailable","message":"AI session status is temporarily unavailable."}}"#,
        r#"{"error":{"code":"upstream_unavailable","message":"AI session status is temporarily unavailable.","retry_after_seconds":null}}"#,
        "synthetic-secret-provider-diagnostic https://untrusted.example/?token=secret",
        "<html>synthetic-secret-provider-diagnostic</html>",
    ] {
        let error = run(reply(503, body)).unwrap_err();
        assert_eq!(error, CloudAIError::InvalidResponse);
        for diagnostic in [error.to_string(), format!("{error:?}")] {
            assert!(!diagnostic.contains("synthetic-secret-provider-diagnostic"));
            assert!(!diagnostic.contains("untrusted.example"));
            assert!(!diagnostic.contains(SECRET));
        }
    }
}

#[test]
fn cancellation_before_dispatch_and_after_every_transport_outcome_wins() {
    let cancel = Cancellation::new();
    assert!(!cancel.is_canceled());
    let clone = cancel.clone();
    clone.cancel();
    assert!(cancel.is_canceled());
    let (client, recorded) = client_with(Ok(success()), None);
    assert_eq!(
        client.generate(&token(), &request(), &cancel).unwrap_err(),
        CloudAIError::Canceled
    );
    assert_eq!(recorded.lock().unwrap().len(), 0);
    for outcome in [
        Ok(success()),
        Ok(error_reply(503, RemoteErrorCode::UpstreamUnavailable, None)),
        Err(CloudAIError::Network(NetworkFailure::Timeout)),
        Err(CloudAIError::Network(NetworkFailure::Connection)),
    ] {
        let cancel = Cancellation::new();
        let (client, recorded) = client_with(outcome, Some(cancel.clone()));
        assert_eq!(
            client.generate(&token(), &request(), &cancel).unwrap_err(),
            CloudAIError::Canceled
        );
        assert_eq!(recorded.lock().unwrap().len(), 1);
    }
}

struct CancelingBody {
    cancel: Cancellation,
    fail: bool,
    reads: Arc<AtomicUsize>,
}

impl Read for CancelingBody {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        self.reads.fetch_add(1, Ordering::Relaxed);
        self.cancel.cancel();
        if self.fail {
            Err(io::Error::other("synthetic-secret-read-diagnostic"))
        } else {
            buffer[0] = b'{';
            Ok(1)
        }
    }
}

#[test]
fn cancellation_after_body_reads_discards_late_bytes_and_late_io_failures() {
    for fail in [false, true] {
        let cancel = Cancellation::new();
        let reads = Arc::new(AtomicUsize::new(0));
        let response = HttpReply {
            status: 200,
            headers: json_headers(),
            body: Box::new(CancelingBody {
                cancel: cancel.clone(),
                fail,
                reads: reads.clone(),
            }),
        };
        let (client, _) = client_with(Ok(response), None);
        assert_eq!(
            client.generate(&token(), &request(), &cancel).unwrap_err(),
            CloudAIError::Canceled
        );
        assert_eq!(reads.load(Ordering::Relaxed), 1);
    }
}

struct FailedBody(io::ErrorKind);

impl Read for FailedBody {
    fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
        Err(io::Error::new(self.0, "synthetic-secret-read-diagnostic"))
    }
}

#[test]
fn read_and_transport_failure_categories_are_static_and_redacted() {
    for (kind, expected) in [
        (io::ErrorKind::TimedOut, NetworkFailure::Timeout),
        (io::ErrorKind::Other, NetworkFailure::Request),
    ] {
        let response = HttpReply {
            status: 200,
            headers: json_headers(),
            body: Box::new(FailedBody(kind)),
        };
        let error = run(response).unwrap_err();
        assert_eq!(error, CloudAIError::Network(expected));
        assert!(!format!("{error:?} {error}").contains("synthetic-secret-read-diagnostic"));
    }
    for failure in [
        NetworkFailure::Timeout,
        NetworkFailure::Connection,
        NetworkFailure::Request,
    ] {
        let (client, recorded) = client_with(Err(CloudAIError::Network(failure)), None);
        assert_eq!(
            client
                .generate(&token(), &request(), &Cancellation::new())
                .unwrap_err(),
            CloudAIError::Network(failure)
        );
        assert_eq!(recorded.lock().unwrap().len(), 1);
    }
}

fn wrapped_reqwest_read_error(kind: io::ErrorKind) -> io::Error {
    // Body::buffer constructs a real reqwest::Error from this synthetic reader's
    // I/O error. No client, resolver, socket, filesystem or network is involved.
    let mut body = Body::new(FailedBody(kind));
    let error = body.buffer().unwrap_err();
    assert_eq!(error.is_timeout(), kind == io::ErrorKind::TimedOut);
    // This matches Response::read's into_io() wrapper. Its reqwest error kind
    // is Builder here rather than Body/Decode, but the typed timeout source and
    // outer Other(reqwest::Error) shape are the same.
    io::Error::other(error)
}

struct WrappedReadFailure {
    kind: io::ErrorKind,
    cancel: Option<Cancellation>,
}

impl Read for WrappedReadFailure {
    fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
        if let Some(cancel) = &self.cancel {
            cancel.cancel();
        }
        Err(wrapped_reqwest_read_error(self.kind))
    }
}

#[test]
fn other_wrapped_reqwest_timeouts_are_classified_without_diagnostic_leakage() {
    for (kind, expected) in [
        (io::ErrorKind::TimedOut, NetworkFailure::Timeout),
        (io::ErrorKind::Other, NetworkFailure::Request),
    ] {
        let wrapped = wrapped_reqwest_read_error(kind);
        assert_eq!(wrapped.kind(), io::ErrorKind::Other);
        assert!(wrapped.get_ref().unwrap().is::<reqwest::Error>());
        let response = HttpReply {
            status: 200,
            headers: json_headers(),
            body: Box::new(WrappedReadFailure { kind, cancel: None }),
        };
        let error = run(response).unwrap_err();
        assert_eq!(error, CloudAIError::Network(expected));
        assert!(!format!("{error:?} {error}").contains("synthetic-secret-read-diagnostic"));
    }
}

#[test]
fn cancellation_wins_over_late_other_wrapped_reqwest_timeouts() {
    let cancel = Cancellation::new();
    let response = HttpReply {
        status: 200,
        headers: json_headers(),
        body: Box::new(WrappedReadFailure {
            kind: io::ErrorKind::TimedOut,
            cancel: Some(cancel.clone()),
        }),
    };
    let (client, _) = client_with(Ok(response), None);
    assert_eq!(
        client.generate(&token(), &request(), &cancel).unwrap_err(),
        CloudAIError::Canceled
    );
}
mod dispatch;

fn features_with(reply: HttpReply) -> Result<AccountFeatures, CloudAIError> {
    let (client, _) = client_with(Ok(reply), None);
    client.features(&token(), &Cancellation::new())
}

#[test]
fn features_are_one_bodyless_get_with_the_bearer() {
    let (client, recorded) = client_with(
        Ok(reply(
            200,
            r#"{"version":1,"features":{"ai_session_summaries":false}}"#,
        )),
        None,
    );
    let features = client.features(&token(), &Cancellation::new()).unwrap();
    assert!(!features.ai_session_summaries);
    let recorded = recorded.lock().unwrap();
    assert_eq!(recorded.len(), 1);
    let outgoing = &recorded[0].request;
    assert_eq!(outgoing.method(), Method::GET);
    assert_eq!(
        outgoing.url().as_str(),
        "https://reviewed.example:8443/api/v1/features"
    );
    assert_eq!(
        outgoing.headers()[header::AUTHORIZATION],
        format!("Bearer {SECRET}")
    );
    assert!(outgoing.headers()[header::AUTHORIZATION].is_sensitive());
    assert!(outgoing.headers().get(header::CONTENT_TYPE).is_none());
    assert!(recorded[0].body.is_empty());
}

#[test]
fn features_decode_strictly_and_default_a_missing_switch_on() {
    let on = |body: &str| features_with(reply(200, body.to_owned()));
    assert!(
        on(r#"{"version":1,"features":{"ai_session_summaries":true}}"#)
            .unwrap()
            .ai_session_summaries
    );
    assert!(
        on(r#"{"version":1,"features":{"newer_switch":false}}"#)
            .unwrap()
            .ai_session_summaries
    );
    for body in [
        r#"{"version":2,"features":{"ai_session_summaries":true}}"#,
        r#"{"version":1}"#,
        r#"{"version":1,"features":{"ai_session_summaries":"no"}}"#,
        "not json",
    ] {
        assert_eq!(on(body), Err(CloudAIError::InvalidResponse), "{body}");
    }
}

#[test]
fn features_failures_are_the_service_errors() {
    assert!(matches!(
        features_with(error_reply(401, RemoteErrorCode::Unauthenticated, None)),
        Err(CloudAIError::Remote { status: 401, .. })
    ));
    assert_eq!(
        features_with(reply(302, Vec::new())),
        Err(CloudAIError::HttpStatus(302))
    );
    let cancel = Cancellation::new();
    let (client, _) = client_with(
        Ok(reply(200, r#"{"version":1,"features":{}}"#)),
        Some(cancel.clone()),
    );
    assert_eq!(
        client.features(&token(), &cancel),
        Err(CloudAIError::Canceled)
    );
}

#[test]
fn the_feed_switch_defaults_off() {
    let features = features_with(reply(
        200,
        r#"{"version":1,"features":{"ai_session_summaries":true,"feed":true}}"#,
    ))
    .unwrap();
    assert!(features.feed);
    let features = features_with(reply(200, r#"{"version":1,"features":{}}"#)).unwrap();
    assert!(!features.feed);
}

#[test]
fn feed_uploads_are_one_sized_post_and_reads_one_get() {
    let (client, recorded) = client_with(Ok(reply(200, r#"{"version":1,"accepted":[]}"#)), None);
    let upload = client
        .feed(
            &token(),
            FeedHttp::Upload(b"{\"version\":1}"),
            &Cancellation::new(),
        )
        .unwrap();
    assert_eq!(upload.status, 200);
    {
        let recorded = recorded.lock().unwrap();
        let outgoing = &recorded[0].request;
        assert_eq!(outgoing.method(), Method::POST);
        assert_eq!(
            outgoing.url().as_str(),
            "https://reviewed.example:8443/api/v1/feed/entries"
        );
        assert_eq!(
            outgoing.headers()[header::AUTHORIZATION],
            format!("Bearer {SECRET}")
        );
        assert_eq!(outgoing.headers()[header::CONTENT_LENGTH], "13");
        assert_eq!(recorded[0].body, b"{\"version\":1}");
        assert!(recorded[0].body_was_streaming, "never replayed");
    }

    let (client, recorded) = client_with(
        Ok(reply(
            429,
            r#"{"error":{"code":"rate_limited","message":"Too many feed requests."}}"#,
        )),
        None,
    );
    let page = client
        .feed(
            &token(),
            FeedHttp::Page("after=12&limit=100"),
            &Cancellation::new(),
        )
        .unwrap();
    // Errors come back for the feed to decode against its own contract.
    assert_eq!(page.status, 429);
    let recorded = recorded.lock().unwrap();
    assert_eq!(recorded[0].request.method(), Method::GET);
    assert_eq!(
        recorded[0].request.url().as_str(),
        "https://reviewed.example:8443/api/v1/feed?after=12&limit=100"
    );
    assert!(recorded[0].body.is_empty());
    assert_eq!(
        client_with(Ok(reply(302, Vec::new())), None)
            .0
            .feed(&token(), FeedHttp::Page("limit=100"), &Cancellation::new())
            .err(),
        Some(CloudAIError::HttpStatus(302))
    );
}
