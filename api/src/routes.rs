//! HTTP routing (`openapi.yaml` is the contract).
//!
//! Every request on the loopback listener: `Host` and browser checks
//! (`guard`), then, except for `GET /v1/health`, the API token, then the
//! caller's identity. On the remote listener: the remote token, and the
//! caller is always the user. Unknown paths answer 404 only after
//! authentication, so they reveal nothing. Daemon calls are blocking and run
//! on `spawn_blocking`; the WebSocket routes hand over to `streams`.

use crate::daemon::{DaemonPool, Identity};
use crate::dto::{
    CreateSessionBody, FolderList, Health, ReviveSessionBody, SessionEnvelope, SessionList,
    UpdateSessionBody,
};
use crate::error::HttpError;
use crate::{folders, guard, log, query, sessions, streams};
use hyper::body::HttpBody;
use hyper::header::{HeaderValue, CONTENT_LENGTH, CONTENT_TYPE};
use hyper::{Body, Method, Request, Response, StatusCode};
use serde::Serialize;
use session_core::client::SessionApi;
use session_core::ids::{SessionId, Token};
use std::convert::Infallible;
use std::sync::atomic::AtomicUsize;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::watch;

/// Largest accepted request body.
pub const MAX_BODY_BYTES: usize = 64 * 1024;
/// Longest accepted session id in a path.
const MAX_ID_LEN: usize = 128;

/// Which listener a request arrived on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Listener {
    /// 127.0.0.1: the API token from `api.json`, a loopback `Host`, no
    /// browsers; identity headers select a session caller.
    Loopback,
    /// Remote access: the pairing token from `remote.json`; any `Host`,
    /// `Origin` allowed; always the user.
    Remote,
}

/// Shared by every request.
pub struct AppState {
    /// The bearer token published in `api.json`.
    pub token: Token,
    /// The remote pairing token, while a remote listener runs.
    pub remote_token: Option<Token>,
    pub pool: DaemonPool,
    /// Becomes `true` when the API stops; WebSocket streams close then.
    pub shutdown: watch::Receiver<bool>,
    /// WebSocket streams running now.
    pub streams: AtomicUsize,
}

/// The routes of API v1.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Route {
    Health,
    /// `/v1/folders`, and its deprecated alias `/v1/projects`.
    Folders,
    Sessions,
    Session(String),
    Content(String),
    Revive(String),
    Kill(String),
    /// WebSocket: the session's terminal.
    Attach(String),
    /// WebSocket: session events.
    Events,
}

impl Route {
    fn parse(path: &str) -> Option<Self> {
        let rest = path.strip_prefix("/v1/")?;
        let segments: Vec<&str> = rest.split('/').collect();
        let id = |id: &&str| (!id.is_empty()).then(|| (*id).to_owned());
        match segments.as_slice() {
            ["health"] => Some(Route::Health),
            ["folders"] | ["projects"] => Some(Route::Folders),
            ["events"] => Some(Route::Events),
            ["sessions"] => Some(Route::Sessions),
            ["sessions", session] => id(session).map(Route::Session),
            ["sessions", session, "content"] => id(session).map(Route::Content),
            ["sessions", session, "revive"] => id(session).map(Route::Revive),
            ["sessions", session, "kill"] => id(session).map(Route::Kill),
            ["sessions", session, "attach"] => id(session).map(Route::Attach),
            _ => None,
        }
    }

    fn methods(&self) -> &'static [Method] {
        match self {
            Route::Health | Route::Folders | Route::Content(_) => &[Method::GET],
            Route::Attach(_) | Route::Events => &[Method::GET],
            Route::Revive(_) | Route::Kill(_) => &[Method::POST],
            Route::Sessions => &[Method::GET, Method::POST],
            Route::Session(_) => &[Method::GET, Method::PATCH, Method::DELETE],
        }
    }
}

/// Answer one request that arrived on `listener`. Never fails: errors
/// become the error envelope.
pub async fn handle(
    state: Arc<AppState>,
    listener: Listener,
    request: Request<Body>,
) -> Result<Response<Body>, Infallible> {
    let started = Instant::now();
    let method = request.method().clone();
    let path = request.uri().path().to_owned();
    let response = match route(&state, listener, request).await {
        Ok(response) => response,
        Err(error) => error_response(&error),
    };
    let status = response.status();
    let elapsed = started.elapsed().as_millis();
    let remote = if listener == Listener::Remote {
        " (remote)"
    } else {
        ""
    };
    if status.is_server_error() {
        log::warn(format_args!(
            "{method} {path}{remote} → {status} ({elapsed} ms)"
        ));
    } else {
        log::debug(format_args!(
            "{method} {path}{remote} → {status} ({elapsed} ms)"
        ));
    }
    Ok(response)
}

async fn route(
    state: &Arc<AppState>,
    listener: Listener,
    mut request: Request<Body>,
) -> Result<Response<Body>, HttpError> {
    if listener == Listener::Loopback {
        guard::check_host(request.headers())?;
        guard::check_not_browser(request.headers())?;
    }
    let route = Route::parse(request.uri().path());
    if route == Some(Route::Health) {
        check_method(&Route::Health, request.method())?;
        // No token needed; but on the network, why the daemon is down (it
        // names local paths) is only told to callers with the token.
        let detailed = match (listener, &state.remote_token) {
            (Listener::Loopback, _) => true,
            (Listener::Remote, Some(token)) => {
                guard::check_remote_bearer(request.headers(), token).is_ok()
            }
            (Listener::Remote, None) => false,
        };
        return health(state, detailed).await;
    }
    let identity = match listener {
        Listener::Loopback => {
            guard::check_bearer(request.headers(), &state.token)?;
            guard::identity(request.headers())?
        }
        Listener::Remote => {
            let token = state
                .remote_token
                .as_ref()
                .ok_or_else(|| HttpError::unauthenticated("remote access is off"))?;
            guard::check_remote_bearer(request.headers(), token)?;
            guard::refuse_identity(request.headers())?;
            Identity::User
        }
    };
    let route = route.ok_or_else(|| {
        HttpError::not_found(format!(
            "no route {} {}",
            request.method(),
            request.uri().path()
        ))
    })?;
    check_method(&route, request.method())?;
    let query = request.uri().query().map(str::to_owned);
    let method = request.method().clone();
    match (route, method) {
        (Route::Folders, _) => {
            query::parse_none(query.as_deref())?;
            let folders = call(state, identity, folders::list).await?;
            json(StatusCode::OK, FolderList::new(folders))
        }
        (Route::Sessions, Method::GET) => {
            let query = query::parse_list(query.as_deref())?;
            let sessions = call(state, identity, move |api| sessions::list(api, &query)).await?;
            json(StatusCode::OK, SessionList { sessions })
        }
        (Route::Sessions, _) => {
            query::parse_none(query.as_deref())?;
            let body: CreateSessionBody = read_json(request).await?;
            let session = call(state, identity, move |api| sessions::create(api, body)).await?;
            let location = HeaderValue::from_str(&format!("/v1/sessions/{}", session.id)).ok();
            let mut response = json(StatusCode::CREATED, SessionEnvelope { session })?;
            if let Some(location) = location {
                response.headers_mut().insert("location", location);
            }
            Ok(response)
        }
        (Route::Session(id), Method::GET) => {
            query::parse_none(query.as_deref())?;
            let id = session_id(&id)?;
            let session = call(state, identity, move |api| sessions::get(api, &id)).await?;
            json(StatusCode::OK, SessionEnvelope { session })
        }
        (Route::Session(id), Method::DELETE) => {
            query::parse_none(query.as_deref())?;
            let id = session_id(&id)?;
            call(state, identity, move |api| sessions::delete(api, &id)).await?;
            Ok(no_content())
        }
        (Route::Session(id), _) => {
            query::parse_none(query.as_deref())?;
            let id = session_id(&id)?;
            let body: UpdateSessionBody = read_json(request).await?;
            let session =
                call(state, identity, move |api| sessions::update(api, &id, body)).await?;
            json(StatusCode::OK, SessionEnvelope { session })
        }
        (Route::Content(id), _) => {
            let query = query::parse_content(query.as_deref())?;
            let id = session_id(&id)?;
            let content = call(state, identity, move |api| {
                sessions::content(api, &id, &query)
            })
            .await?;
            json(StatusCode::OK, content)
        }
        (Route::Revive(id), _) => {
            query::parse_none(query.as_deref())?;
            let id = session_id(&id)?;
            let body: ReviveSessionBody = read_optional_json(request).await?;
            let session =
                call(state, identity, move |api| sessions::revive(api, &id, body)).await?;
            json(StatusCode::OK, SessionEnvelope { session })
        }
        (Route::Kill(id), _) => {
            query::parse_none(query.as_deref())?;
            let id = session_id(&id)?;
            call(state, identity, move |api| sessions::kill(api, &id)).await?;
            Ok(no_content())
        }
        (Route::Attach(id), _) => {
            let query = query::parse_attach(query.as_deref())?;
            let id = session_id(&id)?;
            streams::attach(state, identity, &mut request, id, query).await
        }
        (Route::Events, _) => {
            query::parse_none(query.as_deref())?;
            streams::events(state, identity, &mut request).await
        }
        (Route::Health, _) => Err(HttpError::internal(
            "health is answered before authentication",
        )),
    }
}

/// `GET /v1/health`: the API's version and whether the daemon answers
/// (never starts the daemon). Without `detailed`, `daemon.error` is generic.
async fn health(state: &Arc<AppState>, detailed: bool) -> Result<Response<Body>, HttpError> {
    let state = state.clone();
    let mut daemon = tokio::task::spawn_blocking(move || state.pool.health())
        .await
        .map_err(|e| HttpError::internal(format!("health check failed: {e}")))?;
    if !detailed && daemon.error.is_some() {
        daemon.error = Some("the daemon is not reachable".into());
    }
    json(
        StatusCode::OK,
        Health {
            status: "ok".into(),
            version: crate::VERSION.into(),
            pid: std::process::id(),
            daemon,
        },
    )
}

/// Run `work` with the caller's daemon client on the blocking pool.
pub(crate) async fn call<T, F>(
    state: &Arc<AppState>,
    identity: Identity,
    work: F,
) -> Result<T, HttpError>
where
    T: Send + 'static,
    F: FnOnce(&dyn SessionApi) -> Result<T, HttpError> + Send + 'static,
{
    let state = state.clone();
    tokio::task::spawn_blocking(move || {
        let result = state
            .pool
            .client(&identity)
            .map_err(HttpError::from)
            .and_then(|client| work(client.as_ref()));
        if let Err(HttpError {
            daemon_code: Some(code),
            ..
        }) = &result
        {
            state.pool.forget_after(&identity, *code);
        }
        result
    })
    .await
    .map_err(|e| HttpError::internal(format!("request task failed: {e}")))?
}

fn check_method(route: &Route, method: &Method) -> Result<(), HttpError> {
    let allowed = route.methods();
    if allowed.contains(method) {
        return Ok(());
    }
    let names: Vec<&str> = allowed.iter().map(Method::as_str).collect();
    Err(HttpError::new(
        StatusCode::METHOD_NOT_ALLOWED,
        "method_not_allowed",
        format!(
            "{method} is not allowed here (allowed: {})",
            names.join(", ")
        ),
    )
    .with_details(serde_json::json!({ "allow": names })))
}

/// Session ids are opaque but plain: letters, digits, `-` and `_`. Anything
/// else can't name a session.
fn session_id(raw: &str) -> Result<SessionId, HttpError> {
    let plain = raw.len() <= MAX_ID_LEN
        && raw
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if plain {
        Ok(SessionId::from_string(raw))
    } else {
        Err(HttpError::not_found(format!("no session {raw:?}")))
    }
}

/// Read a JSON body of at most [`MAX_BODY_BYTES`].
async fn read_json<T: serde::de::DeserializeOwned>(request: Request<Body>) -> Result<T, HttpError> {
    check_json_type(&request)?;
    let bytes = read_body(request).await?;
    if bytes.iter().all(u8::is_ascii_whitespace) {
        return Err(HttpError::invalid("the request body is empty"));
    }
    serde_json::from_slice(&bytes).map_err(|e| HttpError::invalid(format!("invalid body: {e}")))
}

/// Like [`read_json`], but an empty body (any `Content-Type`) means
/// `T::default()`.
async fn read_optional_json<T: serde::de::DeserializeOwned + Default>(
    request: Request<Body>,
) -> Result<T, HttpError> {
    let typed = check_json_type(&request);
    let bytes = read_body(request).await?;
    if bytes.iter().all(u8::is_ascii_whitespace) {
        return Ok(T::default());
    }
    typed?;
    serde_json::from_slice(&bytes).map_err(|e| HttpError::invalid(format!("invalid body: {e}")))
}

fn check_json_type(request: &Request<Body>) -> Result<(), HttpError> {
    let json = request
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(|value| {
            let media = value.split(';').next().unwrap_or("").trim();
            media.eq_ignore_ascii_case("application/json")
        });
    if json == Some(true) {
        Ok(())
    } else {
        Err(HttpError::new(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "unsupported_media_type",
            "the body must be JSON (Content-Type: application/json)",
        ))
    }
}

/// The body, at most [`MAX_BODY_BYTES`].
async fn read_body(request: Request<Body>) -> Result<Vec<u8>, HttpError> {
    let too_large = || {
        HttpError::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "payload_too_large",
            format!("the body is larger than {MAX_BODY_BYTES} bytes"),
        )
    };
    let declared = request
        .headers()
        .get(CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<usize>().ok());
    if declared.is_some_and(|length| length > MAX_BODY_BYTES) {
        return Err(too_large());
    }
    let mut body = request.into_body();
    let mut bytes = Vec::new();
    while let Some(chunk) = body.data().await {
        let chunk = chunk.map_err(|e| HttpError::invalid(format!("cannot read the body: {e}")))?;
        if bytes.len() + chunk.len() > MAX_BODY_BYTES {
            return Err(too_large());
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

/// A JSON answer.
fn json(status: StatusCode, body: impl Serialize) -> Result<Response<Body>, HttpError> {
    let body = serde_json::to_value(body)
        .map_err(|e| HttpError::internal(format!("cannot encode the response: {e}")))?;
    Ok(json_response(status, &body))
}

/// 204: done, nothing to say.
fn no_content() -> Response<Body> {
    let mut response = Response::new(Body::empty());
    *response.status_mut() = StatusCode::NO_CONTENT;
    response
        .headers_mut()
        .insert("cache-control", HeaderValue::from_static("no-store"));
    response
}

fn error_response(error: &HttpError) -> Response<Body> {
    let body = serde_json::to_value(error.envelope()).unwrap_or_else(
        |_| serde_json::json!({"error": {"code": "internal", "message": "unencodable error"}}),
    );
    let mut response = json_response(error.status, &body);
    let headers = response.headers_mut();
    if error.status == StatusCode::UNAUTHORIZED {
        headers.insert("www-authenticate", HeaderValue::from_static("Bearer"));
    }
    if error.status == StatusCode::UPGRADE_REQUIRED {
        headers.insert("upgrade", HeaderValue::from_static("websocket"));
        headers.insert("sec-websocket-version", HeaderValue::from_static("13"));
    }
    if error.status == StatusCode::METHOD_NOT_ALLOWED {
        let allow = error
            .body
            .details
            .as_ref()
            .and_then(|details| details.get("allow"))
            .and_then(|allow| allow.as_array())
            .map(|methods| {
                methods
                    .iter()
                    .filter_map(|m| m.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            });
        if let Some(value) = allow.and_then(|allow| HeaderValue::from_str(&allow).ok()) {
            headers.insert("allow", value);
        }
    }
    response
}

fn json_response(status: StatusCode, body: &serde_json::Value) -> Response<Body> {
    let bytes = serde_json::to_vec(body).unwrap_or_else(|_| b"{}".to_vec());
    let mut response = Response::new(Body::from(bytes));
    *response.status_mut() = status;
    let headers = response.headers_mut();
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    headers.insert("cache-control", HeaderValue::from_static("no-store"));
    headers.insert(
        "x-content-type-options",
        HeaderValue::from_static("nosniff"),
    );
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn routes_parse_strictly() {
        assert_eq!(Route::parse("/v1/health"), Some(Route::Health));
        assert_eq!(Route::parse("/v1/folders"), Some(Route::Folders));
        assert_eq!(Route::parse("/v1/projects"), Some(Route::Folders));
        assert_eq!(Route::parse("/v1/sessions"), Some(Route::Sessions));
        assert_eq!(Route::parse("/v1/events"), Some(Route::Events));
        assert_eq!(
            Route::parse("/v1/sessions/01J"),
            Some(Route::Session("01J".into()))
        );
        assert_eq!(
            Route::parse("/v1/sessions/01J/content"),
            Some(Route::Content("01J".into()))
        );
        assert_eq!(
            Route::parse("/v1/sessions/01J/revive"),
            Some(Route::Revive("01J".into()))
        );
        assert_eq!(
            Route::parse("/v1/sessions/01J/kill"),
            Some(Route::Kill("01J".into()))
        );
        assert_eq!(
            Route::parse("/v1/sessions/01J/attach"),
            Some(Route::Attach("01J".into()))
        );
        for bad in [
            "/",
            "/v1",
            "/v1/",
            "/v2/health",
            "/v1/sessions/",
            "/v1/sessions//content",
            "/v1/sessions//attach",
            "/v1/sessions/a/b",
            "/v1/health/",
            "/v1/folders/",
            "/v1/folder",
            "/v1/events/x",
        ] {
            assert_eq!(Route::parse(bad), None, "{bad}");
        }
    }

    #[test]
    fn methods_are_checked_per_route() {
        assert!(check_method(&Route::Sessions, &Method::POST).is_ok());
        let error = check_method(&Route::Folders, &Method::DELETE).unwrap_err();
        assert_eq!(error.status, StatusCode::METHOD_NOT_ALLOWED);
        let response = error_response(&error);
        assert_eq!(response.headers()["allow"], "GET");
        assert!(check_method(&Route::Session("x".into()), &Method::DELETE).is_ok());
        let error = check_method(&Route::Kill("x".into()), &Method::GET).unwrap_err();
        assert_eq!(error_response(&error).headers()["allow"], "POST");
        assert!(check_method(&Route::Events, &Method::POST).is_err());
    }

    #[test]
    fn odd_session_ids_are_not_found() {
        assert!(session_id("01JABC-x_y").is_ok());
        for bad in ["%2e%2e", "a.b", "a b", &"x".repeat(129)] {
            assert_eq!(session_id(bad).unwrap_err().status, StatusCode::NOT_FOUND);
        }
    }

    #[test]
    fn unauthorized_answers_name_the_scheme() {
        let response = error_response(&HttpError::unauthenticated("no"));
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(response.headers()["www-authenticate"], "Bearer");
        assert_eq!(response.headers()["content-type"], "application/json");
        assert!(response
            .headers()
            .get("access-control-allow-origin")
            .is_none());
    }

    #[test]
    fn no_content_has_no_body_type() {
        let response = no_content();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        assert!(response.headers().get("content-type").is_none());
    }
}
