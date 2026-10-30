//! Pokes (`contracts/feed/` "Pokes"): instead of reading its replies every
//! few seconds, the service keeps a WebSocket to rakumux.com's realtime
//! worker and reads them when it's told to. A poke says only "read now"; the
//! read stays the truth, so a lost or doubled poke costs nothing but the
//! once-a-minute read kept for that (`ReplySync::set_pushed`).
//!
//! [`PokeSync`] decides on the owner thread when to ask for a token
//! (`GET /api/v1/feed/realtime`); [`listen`] holds the connection on a
//! thread of its own (it blocks for hours), sends a `ping` every 30 s,
//! takes a connection silent for 90 s or a day old as gone, and says what
//! happened through its callback. A cloud without pokes answers the token
//! request 404 and is asked again in half an hour; meanwhile replies are
//! read as `poll_ms` says.

use std::io::{self, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::wire::{realtime_url, FeedFailure, RealtimeToken};

/// The first and longest waits before connecting again.
const RETRY_MIN: Duration = Duration::from_secs(1);
const RETRY_MAX: Duration = Duration::from_secs(60);
/// A cloud that offers no pokes is asked again this much later.
const NOT_OFFERED_WAIT: Duration = Duration::from_secs(30 * 60);
/// A keep-alive goes this often (the worker answers it without waking).
const PING_EVERY: Duration = Duration::from_secs(30);
/// A connection that says nothing this long is gone.
const SILENT_FOR: Duration = Duration::from_secs(90);
/// Connected this long, the connection is made again with a new token.
const MAX_AGE: Duration = Duration::from_secs(12 * 60 * 60);
/// Connecting, the TLS and WebSocket handshakes included, takes at most this.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// How often the connection's thread looks up from its socket (to stop, ping
/// or give up).
const READ_STEP: Duration = Duration::from_secs(1);

/// What a connection did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PokeEvent {
    /// It's open: replies need only be read when poked.
    Connected,
    /// Read replies now.
    Poked,
    /// It closed (or never opened): read as `poll_ms` says until it's back.
    Closed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    /// Ask for a token once this is past.
    Waiting(Duration),
    /// A token request is out.
    Asking,
    /// A connection's thread runs (connecting, or connected).
    Running,
}

/// When to ask for a token and connect, on the owner thread.
pub(crate) struct PokeSync {
    enabled: bool,
    state: State,
    retry: Duration,
    /// Each connection started has the next number; events of an older one
    /// are ignored.
    generation: u64,
}

impl PokeSync {
    pub fn new() -> Self {
        Self {
            enabled: false,
            state: State::Waiting(Duration::ZERO),
            retry: RETRY_MIN,
            generation: 0,
        }
    }

    /// Follow the account (signed in, feed on, its name confirmed).
    /// Returns true when a running connection must stop.
    pub fn set_enabled(&mut self, enabled: bool, now: Duration) -> bool {
        if enabled == self.enabled {
            return false;
        }
        self.enabled = enabled;
        let running = self.state == State::Running;
        self.state = State::Waiting(now);
        self.retry = RETRY_MIN;
        if running {
            self.generation += 1;
        }
        running
    }

    /// Whether to ask for a token now.
    pub fn next_request(&mut self, now: Duration) -> bool {
        match self.state {
            State::Waiting(at) if self.enabled && now >= at => {
                self.state = State::Asking;
                true
            }
            _ => false,
        }
    }

    /// The token request couldn't start (no usable sign-in).
    pub fn not_started(&mut self, now: Duration) {
        self.state = State::Waiting(now + RETRY_MAX);
    }

    /// The token request finished, made for the account signed in now when
    /// `current`. A token to connect with comes back, with the connection's
    /// generation for its events.
    pub fn token_done(
        &mut self,
        current: bool,
        outcome: Result<RealtimeToken, FeedFailure>,
        now: Duration,
    ) -> Option<(RealtimeToken, u64)> {
        if self.state != State::Asking {
            return None;
        }
        if !current || !self.enabled {
            self.state = State::Waiting(now);
            return None;
        }
        match outcome {
            Ok(token) => {
                self.state = State::Running;
                self.generation += 1;
                Some((token, self.generation))
            }
            // No pokes offered (or a broken answer): read as before.
            Err(FeedFailure::Invalid) => {
                self.state = State::Waiting(now + NOT_OFFERED_WAIT);
                None
            }
            Err(FeedFailure::RateLimited(Some(delay)) | FeedFailure::Unavailable(Some(delay))) => {
                self.state = State::Waiting(now + delay.clamp(self.retry, NOT_OFFERED_WAIT));
                self.retry = self.retry.saturating_mul(2).min(RETRY_MAX);
                None
            }
            Err(_) => {
                self.state = State::Waiting(now + self.retry);
                self.retry = self.retry.saturating_mul(2).min(RETRY_MAX);
                None
            }
        }
    }

    /// Whether an event of connection `generation` is of the one running now.
    pub fn current(&self, generation: u64) -> bool {
        self.state == State::Running && generation == self.generation
    }

    /// The running connection opened.
    pub fn connected(&mut self) {
        self.retry = RETRY_MIN;
    }

    /// The running connection closed: connect again after the backoff.
    pub fn closed(&mut self, now: Duration) {
        self.state = State::Waiting(now + self.retry);
        self.retry = self.retry.saturating_mul(2).min(RETRY_MAX);
    }
}

/// The connection's byte stream: TLS, or plain to a worker on this machine.
enum Stream {
    Plain(TcpStream),
    Tls(Box<rustls::StreamOwned<rustls::ClientConnection, TcpStream>>),
}

impl Read for Stream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            Stream::Plain(stream) => stream.read(buf),
            Stream::Tls(stream) => stream.read(buf),
        }
    }
}

impl Write for Stream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            Stream::Plain(stream) => stream.write(buf),
            Stream::Tls(stream) => stream.write(buf),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self {
            Stream::Plain(stream) => stream.flush(),
            Stream::Tls(stream) => stream.flush(),
        }
    }
}

type Socket = tungstenite::WebSocket<Stream>;

/// TLS that trusts the same roots as the service's HTTPS requests.
fn tls_config() -> Arc<rustls::ClientConfig> {
    let mut roots = rustls::RootCertStore::empty();
    roots.add_trust_anchors(webpki_roots::TLS_SERVER_ROOTS.iter().map(|anchor| {
        rustls::OwnedTrustAnchor::from_subject_spki_name_constraints(
            anchor.subject,
            anchor.spki,
            anchor.name_constraints,
        )
    }));
    Arc::new(
        rustls::ClientConfig::builder()
            .with_safe_defaults()
            .with_root_certificates(roots)
            .with_no_client_auth(),
    )
}

/// Connect to `token.url` with its token, within [`CONNECT_TIMEOUT`]. The
/// socket underneath is returned too, for its timeouts.
fn open(token: &RealtimeToken) -> Option<(Socket, TcpStream)> {
    if !realtime_url(&token.url) {
        return None;
    }
    let tls = token.url.starts_with("wss://");
    let rest = token.url.split_once("://")?.1;
    let authority = rest.split('/').next()?;
    let (host, port) = match authority.rsplit_once(':') {
        Some((host, port)) => (host, port.parse::<u16>().ok()?),
        None => (authority, if tls { 443 } else { 80 }),
    };
    let address = (host, port).to_socket_addrs().ok()?.next()?;
    let tcp = TcpStream::connect_timeout(&address, CONNECT_TIMEOUT).ok()?;
    tcp.set_read_timeout(Some(CONNECT_TIMEOUT)).ok()?;
    tcp.set_write_timeout(Some(CONNECT_TIMEOUT)).ok()?;
    let _ = tcp.set_nodelay(true);
    let handle = tcp.try_clone().ok()?;
    let stream = if tls {
        let name = rustls::ServerName::try_from(host).ok()?;
        let connection = rustls::ClientConnection::new(tls_config(), name).ok()?;
        Stream::Tls(Box::new(rustls::StreamOwned::new(connection, tcp)))
    } else {
        Stream::Plain(tcp)
    };
    use tungstenite::client::IntoClientRequest;
    let mut request = token.url.as_str().into_client_request().ok()?;
    let bearer = format!("Bearer {}", token.token).parse().ok()?;
    request
        .headers_mut()
        .insert(tungstenite::http::header::AUTHORIZATION, bearer);
    let (socket, response) = tungstenite::client::client(request, stream).ok()?;
    (response.status() == tungstenite::http::StatusCode::SWITCHING_PROTOCOLS)
        .then_some((socket, handle))
}

/// Whether `text` is a poke (`poke.schema.json`).
fn is_poke(text: &str) -> bool {
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Poke {
        version: u8,
        r#type: String,
    }
    serde_json::from_str::<Poke>(text).is_ok_and(|poke| poke.version == 1 && poke.r#type == "poke")
}

/// Hold the connection `token` opens until `stop`, the server closes it, it
/// goes silent or a day is up, saying what happens through `event` (which
/// returns false once nobody listens).
pub(crate) fn listen(
    token: &RealtimeToken,
    stop: &AtomicBool,
    mut event: impl FnMut(PokeEvent) -> bool,
) {
    let Some((mut socket, handle)) = open(token) else {
        event(PokeEvent::Closed);
        return;
    };
    if !event(PokeEvent::Connected) || handle.set_read_timeout(Some(READ_STEP)).is_err() {
        let _ = socket.close(None);
        return;
    }
    let opened = Instant::now();
    let (mut heard, mut pinged) = (Instant::now(), Instant::now());
    let mut listening = true;
    while listening && !stop.load(Ordering::SeqCst) {
        if opened.elapsed() >= MAX_AGE || heard.elapsed() >= SILENT_FOR {
            break;
        }
        if pinged.elapsed() >= PING_EVERY {
            pinged = Instant::now();
            if socket.send(tungstenite::Message::text("ping")).is_err() {
                break;
            }
        }
        match socket.read() {
            Ok(tungstenite::Message::Text(text)) => {
                heard = Instant::now();
                if is_poke(text.as_str()) {
                    listening = event(PokeEvent::Poked);
                }
            }
            Ok(tungstenite::Message::Close(_)) => break,
            Ok(_) => heard = Instant::now(),
            Err(tungstenite::Error::Io(error))
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) => {}
            Err(_) => break,
        }
    }
    let _ = socket.close(None);
    let _ = socket.flush();
    if listening {
        event(PokeEvent::Closed);
    }
}

#[cfg(test)]
mod tests;
