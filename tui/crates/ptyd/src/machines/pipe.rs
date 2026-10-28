//! A connection to another machine's daemon: its `rakumux-api`'s
//! `/v1/daemon` WebSocket, which carries the daemon's own protocol
//! (`session_core::frame`) as binary messages after a `hello_ok`.
//!
//! One thread owns the socket (a tungstenite socket can't be split); the
//! [`PipeReader`] and [`PipeWriter`] halves talk to it through channels, so
//! the rest of the daemon reads and writes it like a Unix socket.

use session_core::api::HelloOk;
use session_core::frame::MAX_FRAME;
use std::io::{self, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, TryRecvError};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};
use tungstenite::client::IntoClientRequest;
use tungstenite::protocol::WebSocketConfig;
use tungstenite::Message;

use crate::log_debug;

/// Connecting, TLS and the WebSocket handshake, and the `hello_ok` after it.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(20);
/// How long the socket's thread waits for input before it looks for output
/// to send: the most a write waits.
const POLL: Duration = Duration::from_millis(10);
/// A ping this often keeps proxies from closing a quiet connection.
const PING_EVERY: Duration = Duration::from_secs(25);
/// A connection that hasn't heard from the machine this long, not even the
/// answer to a ping, is gone.
const SILENT: Duration = Duration::from_secs(90);
/// The largest message the machine may send: a whole frame of the protocol.
const MAX_MESSAGE: usize = MAX_FRAME + 5;
/// Bytes read from the machine and not yet taken: past this, the socket's
/// thread stops reading until they are, so a machine that sends faster than
/// they're used is slowed down rather than kept in memory.
const MAX_QUEUED: usize = 16 * 1024 * 1024;

/// An open connection to another machine's daemon, past its hello.
pub struct Pipe {
    pub hello: HelloOk,
    pub reader: PipeReader,
    pub writer: PipeWriter,
}

/// Bytes from the other daemon; `Ok(0)` once the connection closed.
pub struct PipeReader {
    rx: Receiver<Vec<u8>>,
    pending: Vec<u8>,
    at: usize,
    queued: Arc<AtomicUsize>,
    deadline: Option<Instant>,
}

impl PipeReader {
    /// Reads past `deadline` fail with `TimedOut`; `None` waits as long as
    /// the connection stays open.
    pub fn set_deadline(&mut self, deadline: Option<Instant>) {
        self.deadline = deadline;
    }
}

/// Bytes to the other daemon. Clones share the connection; dropping every
/// writer closes it.
#[derive(Clone)]
pub struct PipeWriter {
    tx: Sender<Vec<u8>>,
}

impl Read for PipeReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.at >= self.pending.len() {
            let received = match self.deadline {
                Some(deadline) => self
                    .rx
                    .recv_timeout(deadline.saturating_duration_since(Instant::now())),
                None => self.rx.recv().map_err(|_| RecvTimeoutError::Disconnected),
            };
            match received {
                Ok(bytes) => {
                    self.queued.fetch_sub(bytes.len(), Ordering::SeqCst);
                    self.pending = bytes;
                    self.at = 0;
                }
                Err(RecvTimeoutError::Timeout) => {
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "the machine didn't answer in time",
                    ))
                }
                Err(RecvTimeoutError::Disconnected) => return Ok(0),
            }
        }
        let n = buf.len().min(self.pending.len() - self.at);
        buf[..n].copy_from_slice(&self.pending[self.at..self.at + n]);
        self.at += n;
        Ok(n)
    }
}

impl Write for PipeWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.tx.send(buf.to_vec()).map_err(|_| {
            io::Error::new(
                io::ErrorKind::BrokenPipe,
                "the machine closed the connection",
            )
        })?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Open `url`'s `/v1/daemon` with `token` and read the daemon's hello.
/// `url` is the machine's API base URL (`https://…`, or `http://…` for a
/// listener on this machine or a private network).
pub fn connect(url: &str, token: &str) -> io::Result<Pipe> {
    let (tls, host, port) = parse_base(url)?;
    let address = (host.as_str(), port)
        .to_socket_addrs()?
        .next()
        .ok_or_else(|| invalid(format!("{host} has no address")))?;
    let tcp = TcpStream::connect_timeout(&address, CONNECT_TIMEOUT)?;
    tcp.set_read_timeout(Some(CONNECT_TIMEOUT))?;
    tcp.set_write_timeout(Some(CONNECT_TIMEOUT))?;
    let _ = tcp.set_nodelay(true);
    let handle = tcp.try_clone()?;
    let stream = if tls {
        let name = rustls::ServerName::try_from(host.as_str())
            .map_err(|e| invalid(format!("{host}: {e}")))?;
        let connection = rustls::ClientConnection::new(tls_config(), name)
            .map_err(|e| io::Error::other(format!("TLS: {e}")))?;
        Stream::Tls(Box::new(rustls::StreamOwned::new(connection, tcp)))
    } else {
        Stream::Plain(tcp)
    };
    let ws_url = format!(
        "{}{}/v1/daemon",
        if tls { "wss://" } else { "ws://" },
        url.split_once("://")
            .map_or(url, |(_, rest)| rest)
            .trim_end_matches('/')
    );
    let mut request = ws_url
        .as_str()
        .into_client_request()
        .map_err(|e| invalid(format!("{url}: {e}")))?;
    let bearer = format!("Bearer {token}")
        .parse()
        .map_err(|_| invalid("the token isn't a valid header value"))?;
    request
        .headers_mut()
        .insert(tungstenite::http::header::AUTHORIZATION, bearer);
    let config = WebSocketConfig {
        max_message_size: Some(MAX_MESSAGE),
        max_frame_size: Some(MAX_MESSAGE),
        ..WebSocketConfig::default()
    };
    let (mut socket, _) = tungstenite::client::client_with_config(request, stream, Some(config))
        .map_err(|e| match e {
            tungstenite::HandshakeError::Failure(tungstenite::Error::Http(response)) => {
                refused(response.status().as_u16())
            }
            other => io::Error::other(format!("WebSocket: {other}")),
        })?;
    let hello = loop {
        match socket.read().map_err(ws_error)? {
            Message::Text(text) => {
                break serde_json::from_str::<HelloOk>(&text)
                    .map_err(|e| invalid(format!("the machine's hello: {e}")))?
            }
            Message::Close(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::ConnectionAborted,
                    "the machine closed the connection",
                ))
            }
            _ => {}
        }
    };
    handle.set_read_timeout(Some(POLL))?;
    let (in_tx, in_rx) = mpsc::channel();
    let (out_tx, out_rx) = mpsc::channel();
    let queued = Arc::new(AtomicUsize::new(0));
    let input = Input {
        tx: in_tx,
        queued: queued.clone(),
    };
    std::thread::Builder::new()
        .name("ptyd-machine-ws".into())
        .spawn(move || pump(socket, input, out_rx))?;
    Ok(Pipe {
        hello,
        reader: PipeReader {
            rx: in_rx,
            pending: Vec::new(),
            at: 0,
            queued,
            deadline: None,
        },
        writer: PipeWriter { tx: out_tx },
    })
}

/// Where the socket's thread puts what it reads, counting the bytes the
/// [`PipeReader`] hasn't taken yet.
struct Input {
    tx: Sender<Vec<u8>>,
    queued: Arc<AtomicUsize>,
}

/// Move bytes between the socket and the halves until either side ends.
fn pump(mut socket: Socket, input: Input, output: Receiver<Vec<u8>>) {
    let ended = pump_until_closed(&mut socket, &input, &output);
    log_debug!("machines: connection closed: {ended}");
}

/// Why [`pump`] stopped.
fn pump_until_closed(socket: &mut Socket, input: &Input, output: &Receiver<Vec<u8>>) -> String {
    let mut pinged = Instant::now();
    let mut heard = Instant::now();
    // A write the socket's short read timeout interrupted: TLS may read
    // while it writes. The message is buffered; flushing finishes it.
    let mut unflushed = false;
    loop {
        if unflushed {
            match socket.flush() {
                Ok(()) => unflushed = false,
                Err(e) if is_timeout(&e) => {}
                Err(e) => return format!("flushing: {e}"),
            }
        }
        loop {
            match output.try_recv() {
                Ok(bytes) => match socket.send(Message::Binary(bytes)) {
                    Ok(()) => {}
                    Err(e) if is_timeout(&e) => unflushed = true,
                    Err(e) => return format!("sending: {e}"),
                },
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    let _ = socket.close(None);
                    let _ = socket.flush();
                    return "closed here".into();
                }
            }
        }
        if pinged.elapsed() >= PING_EVERY {
            match socket.send(Message::Ping(Vec::new())) {
                Ok(()) => {}
                Err(e) if is_timeout(&e) => unflushed = true,
                Err(e) => return format!("pinging: {e}"),
            }
            pinged = Instant::now();
        }
        if heard.elapsed() >= SILENT {
            return format!("nothing from the machine for {} s", SILENT.as_secs());
        }
        if input.queued.load(Ordering::SeqCst) >= MAX_QUEUED {
            // Not reading isn't the machine's silence.
            heard = Instant::now();
            std::thread::sleep(POLL);
            continue;
        }
        match socket.read() {
            Ok(message) => {
                heard = Instant::now();
                match message {
                    Message::Binary(bytes) => {
                        input.queued.fetch_add(bytes.len(), Ordering::SeqCst);
                        if input.tx.send(bytes).is_err() {
                            let _ = socket.close(None);
                            let _ = socket.flush();
                            return "closed here".into();
                        }
                    }
                    Message::Close(frame) => return format!("closed by the machine: {frame:?}"),
                    _ => {}
                }
            }
            Err(e) if is_timeout(&e) => {}
            Err(e) => return format!("reading: {e}"),
        }
    }
}

/// The socket's short read timeout, not a failure.
fn is_timeout(error: &tungstenite::Error) -> bool {
    matches!(error, tungstenite::Error::Io(e)
        if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut))
}

/// `(tls, host, port)` of an `http(s)://host[:port][/]` base URL.
fn parse_base(url: &str) -> io::Result<(bool, String, u16)> {
    let (scheme, rest) = url
        .split_once("://")
        .ok_or_else(|| invalid(format!("{url} isn't an http(s):// URL")))?;
    let tls = match scheme {
        "https" => true,
        "http" => false,
        _ => return Err(invalid(format!("{url} isn't an http(s):// URL"))),
    };
    let authority = rest.trim_end_matches('/');
    if authority.is_empty() || authority.contains('/') || authority.contains('@') {
        return Err(invalid(format!(
            "{url} must be a base URL like https://host[:port]"
        )));
    }
    let (host, port) = match authority.rsplit_once(':') {
        Some((host, port)) if !host.ends_with(']') || authority.starts_with('[') => (
            host.trim_start_matches('[')
                .trim_end_matches(']')
                .to_string(),
            port.parse::<u16>()
                .map_err(|_| invalid(format!("{url}: bad port")))?,
        ),
        _ => (authority.to_string(), if tls { 443 } else { 80 }),
    };
    Ok((tls, host, port))
}

/// Check a machine URL without connecting.
pub fn check_url(url: &str) -> io::Result<()> {
    parse_base(url).map(|_| ())
}

fn refused(status: u16) -> io::Error {
    let message = match status {
        401 => "the machine refused the token".to_string(),
        404 => "the machine's rakumux-api has no /v1/daemon: update Rakumux there".to_string(),
        status => format!("the machine answered HTTP {status}"),
    };
    io::Error::new(io::ErrorKind::PermissionDenied, message)
}

fn ws_error(error: tungstenite::Error) -> io::Error {
    match error {
        tungstenite::Error::Io(e) => e,
        other => io::Error::other(other.to_string()),
    }
}

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message.into())
}

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

/// TLS trusting the webpki roots, built once.
fn tls_config() -> Arc<rustls::ClientConfig> {
    static CONFIG: OnceLock<Arc<rustls::ClientConfig>> = OnceLock::new();
    CONFIG
        .get_or_init(|| {
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
        })
        .clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_urls_parse_and_bad_ones_are_refused() {
        assert_eq!(
            parse_base("https://box-1.sprites.app").unwrap(),
            (true, "box-1.sprites.app".into(), 443)
        );
        assert_eq!(
            parse_base("http://127.0.0.1:7337/").unwrap(),
            (false, "127.0.0.1".into(), 7337)
        );
        assert_eq!(
            parse_base("http://[fd7a:115c::1]:7337").unwrap(),
            (false, "fd7a:115c::1".into(), 7337)
        );
        for bad in [
            "box.example",
            "ftp://box",
            "https://",
            "https://box/v1",
            "https://u@box",
        ] {
            assert!(parse_base(bad).is_err(), "{bad}");
        }
    }
}
