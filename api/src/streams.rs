//! Live streams over WebSocket (`plans/mobile-client.md` §3), on both
//! listeners with the same checks as every other route:
//!
//! - `GET /v1/sessions/{id}/attach`: a session's terminal. Text messages
//!   are [`AttachMessage`]s (first `attached` with a checkpoint), binary
//!   frames the terminal's output; the client sends input as binary frames
//!   and [`AttachCommand`]s as text. Closing the socket detaches.
//! - `GET /v1/events`: session events of every folder ([`EventMessage`]).
//!
//! The daemon stream is opened before the upgrade, so its errors are
//! ordinary JSON answers (404 unknown session, 409 `session_not_live`).
//! `session_core`'s streams are blocking: a thread per stream moves their
//! messages onto a bounded channel the WebSocket task reads. The server
//! pings every [`PING_INTERVAL`] and drops a client that didn't answer the
//! previous ping; it closes the socket when the daemon stream ends.

use crate::daemon::Identity;
use crate::dto::{AttachCommand, AttachMessage, EventMessage};
use crate::error::HttpError;
use crate::log;
use crate::query::AttachQuery;
use crate::routes::{self, AppState};
use futures_util::stream::SplitSink;
use futures_util::{SinkExt, StreamExt};
use hyper::header::{
    HeaderMap, HeaderValue, CONNECTION, SEC_WEBSOCKET_ACCEPT, SEC_WEBSOCKET_KEY,
    SEC_WEBSOCKET_VERSION, UPGRADE,
};
use hyper::upgrade::Upgraded;
use hyper::{Body, Request, Response, StatusCode};
use session_core::api::{AttachRequest, CallerInfo, SubscribeRequest, CAPABILITY_SESSIONS_ALL};
use session_core::client::{AttachEvent, AttachStream, EventStream, SessionApi};
use session_core::ids::SessionId;
use std::borrow::Cow;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, watch};
use tokio_tungstenite::tungstenite::handshake::derive_accept_key;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use tokio_tungstenite::tungstenite::protocol::{CloseFrame, Role as WsRole, WebSocketConfig};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::WebSocketStream;

/// How often the server pings; a client that didn't answer the previous
/// ping by the next one is dropped.
pub const PING_INTERVAL: Duration = Duration::from_secs(30);
/// Messages queued between a daemon stream and its WebSocket.
const QUEUE: usize = 256;
/// How often a stream's thread checks that its WebSocket still listens.
const POLL: Duration = Duration::from_millis(250);
/// Largest message a client may send (input and commands are small).
const MAX_CLIENT_MESSAGE: usize = 1024 * 1024;
/// Longest close reason a WebSocket close frame carries.
const MAX_CLOSE_REASON: usize = 123;

type Socket = WebSocketStream<Upgraded>;
type Sink = SplitSink<Socket, Message>;

/// `GET /v1/sessions/{id}/attach`.
pub async fn attach(
    state: &Arc<AppState>,
    identity: Identity,
    request: &mut Request<Body>,
    id: SessionId,
    query: AttachQuery,
) -> Result<Response<Body>, HttpError> {
    let accept = handshake(request.headers())?;
    let attach = AttachRequest {
        id,
        role: query.role,
        cols: query.cols,
        rows: query.rows,
    };
    let stream = routes::call(state, identity, move |api| Ok(api.attach(attach)?)).await?;
    let upgrade = hyper::upgrade::on(request);
    spawn_stream(
        state,
        "attach",
        move |socket, shutdown| run_attach(socket, stream, shutdown),
        upgrade,
    );
    Ok(switching_protocols(&accept))
}

/// `GET /v1/events`.
pub async fn events(
    state: &Arc<AppState>,
    identity: Identity,
    request: &mut Request<Body>,
) -> Result<Response<Body>, HttpError> {
    let accept = handshake(request.headers())?;
    let stream = routes::call(state, identity, subscribe_everything).await?;
    let upgrade = hyper::upgrade::on(request);
    spawn_stream(
        state,
        "events",
        move |socket, shutdown| run_events(socket, stream, shutdown),
        upgrade,
    );
    Ok(switching_protocols(&accept))
}

/// Every folder's events for the user (the daemon's `all`, or the folders
/// it knows when it predates `sessions.all`); its own folder's for a
/// session caller.
fn subscribe_everything(api: &dyn SessionApi) -> Result<EventStream, HttpError> {
    let request = match api.caller() {
        CallerInfo::Session { .. } => SubscribeRequest::default(),
        CallerInfo::User => {
            let all = api
                .daemon_capabilities()
                .is_none_or(|caps| caps.iter().any(|c| c == CAPABILITY_SESSIONS_ALL));
            if all {
                SubscribeRequest {
                    all: true,
                    ..SubscribeRequest::default()
                }
            } else {
                SubscribeRequest {
                    scopes: api.projects()?.into_iter().map(|p| p.path).collect(),
                    ..SubscribeRequest::default()
                }
            }
        }
    };
    Ok(api.subscribe(request)?)
}

/// Check an upgrade request (RFC 6455 §4.2.1); returns the
/// `Sec-WebSocket-Accept` value.
pub fn handshake(headers: &HeaderMap) -> Result<String, HttpError> {
    let lists = |name| {
        headers
            .get_all(name)
            .iter()
            .filter_map(|value| value.to_str().ok())
            .flat_map(|value| value.split(','))
            .map(str::trim)
            .collect::<Vec<_>>()
    };
    let upgrade = lists(CONNECTION)
        .iter()
        .any(|token| token.eq_ignore_ascii_case("upgrade"));
    let websocket = lists(UPGRADE)
        .iter()
        .any(|token| token.eq_ignore_ascii_case("websocket"));
    if !upgrade || !websocket {
        return Err(upgrade_required(
            "this route is a WebSocket: send `Connection: Upgrade` and `Upgrade: websocket`",
        ));
    }
    if lists(SEC_WEBSOCKET_VERSION) != ["13"] {
        return Err(upgrade_required("WebSocket version 13 is required"));
    }
    let key = headers
        .get(SEC_WEBSOCKET_KEY)
        .map(HeaderValue::as_bytes)
        .filter(|key| !key.is_empty())
        .ok_or_else(|| HttpError::invalid("Sec-WebSocket-Key is missing"))?;
    Ok(derive_accept_key(key))
}

/// 426: the route needs a WebSocket upgrade (the answer names version 13).
fn upgrade_required(message: &str) -> HttpError {
    HttpError::new(StatusCode::UPGRADE_REQUIRED, "upgrade_required", message)
}

fn switching_protocols(accept: &str) -> Response<Body> {
    let mut response = Response::new(Body::empty());
    *response.status_mut() = StatusCode::SWITCHING_PROTOCOLS;
    let headers = response.headers_mut();
    headers.insert(CONNECTION, HeaderValue::from_static("upgrade"));
    headers.insert(UPGRADE, HeaderValue::from_static("websocket"));
    if let Ok(accept) = HeaderValue::from_str(accept) {
        headers.insert(SEC_WEBSOCKET_ACCEPT, accept);
    }
    response
}

/// After the 101 answer: take over the connection and run `serve` on it.
/// Counted in `AppState::streams` until it ends.
fn spawn_stream<F, Fut>(
    state: &Arc<AppState>,
    what: &'static str,
    serve: F,
    upgrade: hyper::upgrade::OnUpgrade,
) where
    F: FnOnce(Socket, watch::Receiver<bool>) -> Fut + Send + 'static,
    Fut: std::future::Future<Output = ()> + Send + 'static,
{
    let shutdown = state.shutdown.clone();
    let active = StreamCount::new(state);
    tokio::spawn(async move {
        let _active = active;
        let upgraded = match upgrade.await {
            Ok(upgraded) => upgraded,
            Err(error) => {
                log::warn(format_args!("{what}: WebSocket upgrade failed: {error}"));
                return;
            }
        };
        let mut config = WebSocketConfig::default();
        config.max_message_size = Some(MAX_CLIENT_MESSAGE);
        config.max_frame_size = Some(MAX_CLIENT_MESSAGE);
        let socket = WebSocketStream::from_raw_socket(upgraded, WsRole::Server, Some(config)).await;
        log::debug(format_args!("{what}: WebSocket open"));
        serve(socket, shutdown).await;
        log::debug(format_args!("{what}: WebSocket closed"));
    });
}

/// Keeps `AppState::streams` up to date while a stream runs.
struct StreamCount(Arc<AppState>);

impl StreamCount {
    fn new(state: &Arc<AppState>) -> Self {
        state.streams.fetch_add(1, Ordering::SeqCst);
        Self(state.clone())
    }
}

impl Drop for StreamCount {
    fn drop(&mut self) {
        self.0.streams.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Why a stream loop ended: the close frame to send, if any.
type Ending = Option<CloseFrame<'static>>;

fn close_frame(code: CloseCode, reason: &str) -> Ending {
    let mut end = reason.len().min(MAX_CLOSE_REASON);
    while !reason.is_char_boundary(end) {
        end -= 1;
    }
    Some(CloseFrame {
        code,
        reason: Cow::Owned(reason[..end].to_owned()),
    })
}

/// Liveness: a ping every [`PING_INTERVAL`]; the client must have answered
/// the previous one.
#[derive(Default)]
struct Heartbeat {
    awaiting_pong: bool,
}

/// Ticks every [`PING_INTERVAL`], the first one an interval from now.
fn ping_timer() -> tokio::time::Interval {
    let start = tokio::time::Instant::now() + PING_INTERVAL;
    let mut timer = tokio::time::interval_at(start, PING_INTERVAL);
    timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    timer
}

impl Heartbeat {
    /// On a tick: `Err` when the last ping went unanswered, else the ping
    /// to send.
    fn tick(&mut self) -> Result<Message, Ending> {
        if self.awaiting_pong {
            return Err(close_frame(CloseCode::Policy, "no answer to ping"));
        }
        self.awaiting_pong = true;
        Ok(Message::Ping(Vec::new()))
    }

    fn pong(&mut self) {
        self.awaiting_pong = false;
    }
}

async fn send_json(sink: &mut Sink, message: &impl serde::Serialize) -> Result<(), Ending> {
    let text = serde_json::to_string(message)
        .map_err(|e| close_frame(CloseCode::Error, &format!("cannot encode: {e}")))?;
    sink.send(Message::Text(text)).await.map_err(|_| None)
}

async fn finish(mut sink: Sink, ending: Ending) {
    if let Some(frame) = ending {
        let _ = sink.send(Message::Close(Some(frame))).await;
    }
    let _ = sink.close().await;
}

// ---------------------------------------------------------------- attach

async fn run_attach(socket: Socket, stream: AttachStream, mut shutdown: watch::Receiver<bool>) {
    let (mut sink, mut source) = socket.split();
    let sender = stream.sender();
    let first = AttachMessage::attached(stream.attached.clone());
    if let Err(ending) = send_json(&mut sink, &first).await {
        return finish(sink, ending).await;
    }
    let (tx, mut rx) = mpsc::channel(QUEUE);
    let pump = std::thread::Builder::new()
        .name("api-attach".into())
        .spawn(move || pump_attach(stream, tx));
    if let Err(error) = pump {
        let reason = format!("cannot start a thread: {error}");
        return finish(sink, close_frame(CloseCode::Error, &reason)).await;
    }
    let mut heartbeat = Heartbeat::default();
    let mut pings = ping_timer();
    let ending: Ending = loop {
        tokio::select! {
            event = rx.recv() => match event {
                Some(AttachEvent::Output { bytes, .. }) => {
                    if sink.send(Message::Binary(bytes)).await.is_err() {
                        break None;
                    }
                }
                Some(AttachEvent::Event(event)) => {
                    if let Some(message) = AttachMessage::from_event(event) {
                        if let Err(ending) = send_json(&mut sink, &message).await {
                            break ending;
                        }
                    }
                }
                Some(AttachEvent::Closed(Some(error))) => {
                    break close_frame(CloseCode::Error, &error);
                }
                Some(AttachEvent::Closed(None)) | None => {
                    break close_frame(CloseCode::Normal, "the terminal stream ended");
                }
            },
            message = source.next() => match message {
                Some(Ok(Message::Binary(bytes))) => {
                    // The daemon ignores an observer's input; a closed
                    // stream reports `Closed` on `rx`.
                    let _ = sender.input(bytes);
                }
                Some(Ok(Message::Text(text))) => match serde_json::from_str::<AttachCommand>(&text) {
                    Ok(command) => {
                        let _ = sender.command(command.into());
                    }
                    Err(error) => log::debug(format_args!("attach: ignored message: {error}")),
                },
                Some(Ok(Message::Pong(_))) => heartbeat.pong(),
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break None,
                Some(Ok(_)) => {}
            },
            _ = pings.tick() => match heartbeat.tick() {
                Ok(ping) => {
                    if sink.send(ping).await.is_err() {
                        break None;
                    }
                }
                Err(ending) => break ending,
            },
            _ = shutdown.changed() => {
                break close_frame(CloseCode::Away, "rakumux-api is stopping");
            }
        }
    };
    // Dropping `rx` ends the pump thread, which detaches.
    drop(rx);
    finish(sink, ending).await;
}

/// Move an attach stream's messages onto `tx` until it ends or nobody
/// listens; then detach.
fn pump_attach(stream: AttachStream, tx: mpsc::Sender<AttachEvent>) {
    while !tx.is_closed() {
        let Some(event) = stream.recv_timeout(POLL) else {
            continue;
        };
        let last = matches!(event, AttachEvent::Closed(_));
        if tx.blocking_send(event).is_err() || last {
            break;
        }
    }
    stream.detach();
}

// ---------------------------------------------------------------- events

async fn run_events(socket: Socket, stream: EventStream, mut shutdown: watch::Receiver<bool>) {
    let (mut sink, mut source) = socket.split();
    if let Err(ending) = send_json(&mut sink, &EventMessage::Subscribed).await {
        return finish(sink, ending).await;
    }
    let (tx, mut rx) = mpsc::channel(QUEUE);
    let pump = std::thread::Builder::new()
        .name("api-events".into())
        .spawn(move || pump_events(stream, tx));
    if let Err(error) = pump {
        let reason = format!("cannot start a thread: {error}");
        return finish(sink, close_frame(CloseCode::Error, &reason)).await;
    }
    let mut heartbeat = Heartbeat::default();
    let mut pings = ping_timer();
    let ending: Ending = loop {
        tokio::select! {
            item = rx.recv() => match item {
                Some(Ok(envelope)) => {
                    if let Some(message) = EventMessage::from_envelope(envelope) {
                        if let Err(ending) = send_json(&mut sink, &message).await {
                            break ending;
                        }
                    }
                }
                Some(Err(error)) => break close_frame(CloseCode::Error, &error),
                None => break close_frame(CloseCode::Error, "the event stream ended"),
            },
            message = source.next() => match message {
                Some(Ok(Message::Pong(_))) => heartbeat.pong(),
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break None,
                // Clients send nothing else on this stream.
                Some(Ok(_)) => {}
            },
            _ = pings.tick() => match heartbeat.tick() {
                Ok(ping) => {
                    if sink.send(ping).await.is_err() {
                        break None;
                    }
                }
                Err(ending) => break ending,
            },
            _ = shutdown.changed() => {
                break close_frame(CloseCode::Away, "rakumux-api is stopping");
            }
        }
    };
    drop(rx);
    finish(sink, ending).await;
}

type EventItem = Result<session_core::api::EventEnvelope, String>;

/// Move an event stream's items onto `tx` until it fails or nobody listens.
fn pump_events(stream: EventStream, tx: mpsc::Sender<EventItem>) {
    while !tx.is_closed() {
        let Some(item) = stream.recv_timeout(POLL) else {
            continue;
        };
        let last = item.is_err();
        if tx.blocking_send(item).is_err() || last {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(pairs: &[(&'static str, &str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (name, value) in pairs {
            map.append(*name, HeaderValue::from_str(value).unwrap());
        }
        map
    }

    const UPGRADE_HEADERS: [(&str, &str); 4] = [
        ("connection", "keep-alive, Upgrade"),
        ("upgrade", "websocket"),
        ("sec-websocket-version", "13"),
        ("sec-websocket-key", "dGhlIHNhbXBsZSBub25jZQ=="),
    ];

    #[test]
    fn the_handshake_follows_rfc_6455() {
        // RFC 6455 §1.3's example key and answer.
        assert_eq!(
            handshake(&headers(&UPGRADE_HEADERS)).unwrap(),
            "s3pPLMBiTxaQ9kYGzzhZRbK+xOo="
        );
        let response = switching_protocols("s3pPLMBiTxaQ9kYGzzhZRbK+xOo=");
        assert_eq!(response.status(), StatusCode::SWITCHING_PROTOCOLS);
        assert_eq!(response.headers()["upgrade"], "websocket");
        assert_eq!(response.headers()["connection"], "upgrade");
    }

    #[test]
    fn plain_requests_are_told_to_upgrade() {
        let error = handshake(&HeaderMap::new()).unwrap_err();
        assert_eq!(error.status, StatusCode::UPGRADE_REQUIRED);
        assert_eq!(error.code(), "upgrade_required");
        let mut old = UPGRADE_HEADERS;
        old[2] = ("sec-websocket-version", "8");
        assert_eq!(
            handshake(&headers(&old)).unwrap_err().status,
            StatusCode::UPGRADE_REQUIRED
        );
        let mut keyless = UPGRADE_HEADERS;
        keyless[3] = ("sec-websocket-key", "");
        assert_eq!(
            handshake(&headers(&keyless)).unwrap_err().status.as_u16(),
            400
        );
    }

    #[test]
    fn close_reasons_fit_a_close_frame() {
        let long = "é".repeat(100);
        let frame = close_frame(CloseCode::Error, &long).unwrap();
        assert!(frame.reason.len() <= MAX_CLOSE_REASON);
        assert!(long.starts_with(frame.reason.as_ref()));
        assert_eq!(
            close_frame(CloseCode::Normal, "done").unwrap().reason,
            "done"
        );
    }

    #[test]
    fn a_client_that_misses_a_ping_is_dropped() {
        let mut heartbeat = Heartbeat::default();
        assert!(matches!(heartbeat.tick(), Ok(Message::Ping(_))));
        heartbeat.pong();
        assert!(heartbeat.tick().is_ok());
        let ending = heartbeat.tick().unwrap_err().unwrap();
        assert_eq!(ending.code, CloseCode::Policy);
    }
}
