use std::collections::HashMap;
use std::fmt;
use std::io::{self, Read, Write};
use std::os::unix::io::AsRawFd;
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use session_core::ids::{SessionId, Token};

use super::layout::{current_uid, peer_uid};
use super::protocol::{
    read_payload, write_message, AccountView, ConsentView, Hello, OfferView, OwnerErrorCode,
    OwnerRequest, OwnerResponse, OwnerState, SignInView, StatusLine, MAX_CLIENT_NAME, MAX_MESSAGE,
    MAX_SESSIONS, PROTOCOL,
};
use super::{OwnerClaim, OwnerError};
use crate::dispatch::StatusConsentOffer;
use crate::refresh::{RefreshClient, RefreshError};
use crate::status::ConsentRevision;

/// A client must finish its hello within this time.
const HELLO_TIMEOUT: Duration = if cfg!(test) {
    Duration::from_millis(500)
} else {
    Duration::from_secs(5)
};
/// A hello is a version, a label and a token.
const MAX_HELLO: usize = 4 * 1024;
/// Connections at once; more are closed at once.
const MAX_CONNECTIONS: usize = 32;

type Connections = Arc<Mutex<HashMap<u64, UnixStream>>>;

/// Serves the owner's socket: user clients of this home only (same uid, and the
/// registration's token). A connection thread per client forwards each request to
/// the refresher's owner thread and waits for its answer. Session callers have no
/// access: their environment carries no token for this socket, though a same-user
/// process can read the registration file, as with the daemon's user token.
pub struct OwnerServer {
    stop: Arc<AtomicBool>,
    /// A client asked the embedding service to stop.
    shutdown: Arc<AtomicBool>,
    /// Written on stop to wake the accept loop, which polls its other end.
    waker: UnixStream,
    connections: Connections,
    accept: Option<JoinHandle<()>>,
}

impl OwnerServer {
    /// Serve `claim`'s socket with `client`'s refresher. A claim serves once.
    pub fn start(
        claim: &mut OwnerClaim,
        client: RefreshClient,
        version: &str,
    ) -> Result<Self, OwnerError> {
        let listener = claim.take_listener().ok_or(OwnerError::Socket)?;
        listener
            .set_nonblocking(true)
            .map_err(|_| OwnerError::Socket)?;
        let (waker, woken) = UnixStream::pair().map_err(|_| OwnerError::Socket)?;
        let token = claim.token().clone();
        let stop = Arc::new(AtomicBool::new(false));
        let shutdown = Arc::new(AtomicBool::new(false));
        let connections: Connections = Arc::default();
        let context = Arc::new(Context {
            token,
            client,
            version: version.to_owned(),
            shutdown: shutdown.clone(),
        });
        let accept = {
            let (stop, connections) = (stop.clone(), connections.clone());
            thread::Builder::new()
                .name("rakumux-status-accept".into())
                .spawn(move || accept(&listener, &woken, &stop, &connections, &context))
                .map_err(|_| OwnerError::Socket)?
        };
        Ok(Self {
            stop,
            shutdown,
            waker,
            connections,
            accept: Some(accept),
        })
    }

    /// Whether a client asked the service to stop (`OwnerRequest::Shutdown`). The
    /// embedding service then stops this server and itself.
    pub fn shutdown_requested(&self) -> bool {
        self.shutdown.load(Ordering::SeqCst)
    }

    /// Close every connection and stop accepting.
    pub fn stop(mut self) {
        self.shutdown();
    }

    /// Connection threads are not joined: each ends once its socket is shut down
    /// and any request it forwarded is answered.
    fn shutdown(&mut self) {
        let Some(accept) = self.accept.take() else {
            return;
        };
        {
            // Under the lock, so the accept loop can't register a connection after
            // these are closed.
            let connections = self.connections.lock();
            self.stop.store(true, Ordering::SeqCst);
            if let Ok(connections) = connections {
                for stream in connections.values() {
                    let _ = stream.shutdown(std::net::Shutdown::Both);
                }
            }
        }
        // Wake the accept loop; this works even once the socket file is gone.
        let _ = (&self.waker).write_all(&[1]);
        let _ = accept.join();
    }
}

impl Drop for OwnerServer {
    fn drop(&mut self) {
        self.shutdown();
    }
}

impl fmt::Debug for OwnerServer {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("OwnerServer")
    }
}

struct Context {
    token: Token,
    client: RefreshClient,
    version: String,
    shutdown: Arc<AtomicBool>,
}

fn accept(
    listener: &UnixListener,
    woken: &UnixStream,
    stop: &AtomicBool,
    connections: &Connections,
    context: &Arc<Context>,
) {
    let ids = AtomicU64::new(0);
    loop {
        if stop.load(Ordering::SeqCst) {
            return;
        }
        let stream = match listener.accept() {
            Ok((stream, _)) => stream,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                wait(listener, woken);
                continue;
            }
            Err(_) => {
                // Out of descriptors, for example: don't spin.
                thread::sleep(Duration::from_millis(50));
                continue;
            }
        };
        // Accepted sockets may inherit the listener's non-blocking mode (BSDs).
        if stream.set_nonblocking(false).is_err() {
            continue;
        }
        let id = ids.fetch_add(1, Ordering::Relaxed);
        {
            let Ok(mut open) = connections.lock() else {
                return;
            };
            if stop.load(Ordering::SeqCst) {
                return;
            }
            if open.len() >= MAX_CONNECTIONS {
                let mut stream = stream;
                let _ = stream.set_write_timeout(Some(HELLO_TIMEOUT));
                let _ = write_message(&mut stream, &refuse(OwnerErrorCode::Busy));
                continue;
            }
            let Ok(registered) = stream.try_clone() else {
                continue;
            };
            open.insert(id, registered);
        }
        let slot = Slot {
            connections: connections.clone(),
            id,
        };
        let context = context.clone();
        // On a panic or a failed spawn, dropping the slot frees it.
        let _ = thread::Builder::new()
            .name("rakumux-status-client".into())
            .spawn(move || {
                let _slot = slot;
                serve(stream, &context);
            });
    }
}

/// A registered connection; dropping it unregisters the connection.
struct Slot {
    connections: Connections,
    id: u64,
}

impl Drop for Slot {
    fn drop(&mut self) {
        if let Ok(mut open) = self.connections.lock() {
            open.remove(&self.id);
        }
    }
}

/// Reads that must all finish by a deadline, however slowly bytes arrive.
struct Deadline<'a> {
    stream: &'a UnixStream,
    until: Instant,
}

impl Read for Deadline<'_> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let remaining = self.until.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(io::ErrorKind::TimedOut.into());
        }
        self.stream.set_read_timeout(Some(remaining))?;
        (&mut &*self.stream).read(buffer)
    }
}

/// Until a client connects or the server is stopped.
fn wait(listener: &UnixListener, woken: &UnixStream) {
    let mut descriptors = [
        libc::pollfd {
            fd: listener.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        },
        libc::pollfd {
            fd: woken.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        },
    ];
    // SAFETY: `descriptors` is a valid array of two pollfd for the call's duration.
    unsafe { libc::poll(descriptors.as_mut_ptr(), 2, -1) };
}

/// One client: peer check, hello, then requests until it disconnects.
fn serve(mut stream: UnixStream, context: &Context) {
    if peer_uid(&stream).ok() != Some(current_uid()) {
        return;
    }
    let mut deadline = Deadline {
        stream: &stream,
        until: Instant::now() + HELLO_TIMEOUT,
    };
    let hello = match read_payload(&mut deadline, MAX_HELLO) {
        Ok(Some(payload)) => serde_json::from_slice::<Hello>(&payload).ok(),
        _ => return,
    };
    let refusal = match &hello {
        None => Some(OwnerErrorCode::Unauthenticated),
        Some(hello) if hello.protocol != PROTOCOL => Some(OwnerErrorCode::ProtocolMismatch),
        Some(hello) if !context.token.matches(&hello.token) => {
            Some(OwnerErrorCode::Unauthenticated)
        }
        Some(hello) if hello.client.len() > MAX_CLIENT_NAME => Some(OwnerErrorCode::InvalidRequest),
        Some(_) => None,
    };
    if let Some(code) = refusal {
        let _ = write_message(&mut stream, &OwnerResponse::Error { code });
        return;
    }
    let welcome = OwnerResponse::Welcome {
        protocol: PROTOCOL,
        version: context.version.clone(),
    };
    if write_message(&mut stream, &welcome).is_err() || stream.set_read_timeout(None).is_err() {
        return;
    }
    // This connection's latest consent offer; enabling consumes it.
    let mut offer: Option<(String, StatusConsentOffer)> = None;
    loop {
        let response = match read_payload(&mut stream, MAX_MESSAGE) {
            Ok(Some(payload)) => match serde_json::from_slice::<OwnerRequest>(&payload) {
                Ok(OwnerRequest::Shutdown) => {
                    context.shutdown.store(true, Ordering::SeqCst);
                    OwnerResponse::Done
                }
                Ok(request) => handle(request, &context.client, &mut offer),
                Err(_) => refuse(OwnerErrorCode::InvalidRequest),
            },
            // A framing error leaves the stream unusable.
            Ok(None) | Err(_) => return,
        };
        if write_message(&mut stream, &response).is_err() {
            return;
        }
    }
}

fn handle(
    request: OwnerRequest,
    client: &RefreshClient,
    offer: &mut Option<(String, StatusConsentOffer)>,
) -> OwnerResponse {
    let result = match request {
        OwnerRequest::State => state(client),
        OwnerRequest::Statuses { sessions } => statuses(client, sessions),
        OwnerRequest::ConsentOffer => consent_offer(client, offer),
        OwnerRequest::Enable { offer: id, folders } => {
            match offer.take_if(|(issued, _)| Token::from_string(issued.clone()).matches(&id)) {
                Some((_, issued)) => client
                    .enable(issued, folders.into_iter().map(Into::into).collect())
                    .map(|consent| OwnerResponse::Consent {
                        consent: ConsentView::from(&consent),
                    })
                    .map_err(|error| OwnerErrorCode::of_enable(&error)),
                None => Err(OwnerErrorCode::StaleOffer),
            }
        }
        OwnerRequest::Disable { revision } => client
            .disable(ConsentRevision(revision))
            .map(|consent| OwnerResponse::Consent {
                consent: ConsentView::from(&consent),
            })
            .map_err(|error| OwnerErrorCode::from(&error)),
        OwnerRequest::SignOut => {
            *offer = None;
            client
                .sign_out()
                .map(|()| OwnerResponse::Done)
                .map_err(|error| OwnerErrorCode::from(&error))
        }
        OwnerRequest::SignInStart => {
            *offer = None;
            client
                .sign_in_start()
                .and_then(|()| client.sign_in_status())
                .map(|state| OwnerResponse::SignIn {
                    sign_in: SignInView::from(&state),
                })
                .map_err(|error| OwnerErrorCode::from(&error))
        }
        OwnerRequest::SignInStatus => client
            .sign_in_status()
            .map(|state| OwnerResponse::SignIn {
                sign_in: SignInView::from(&state),
            })
            .map_err(|error| OwnerErrorCode::from(&error)),
        OwnerRequest::SignInCancel => client
            .sign_in_cancel()
            .map(|()| OwnerResponse::Done)
            .map_err(|error| OwnerErrorCode::from(&error)),
        // Answered by the connection itself.
        OwnerRequest::Shutdown => Ok(OwnerResponse::Done),
        OwnerRequest::Renew => client
            .renew()
            .map(|()| OwnerResponse::Done)
            .map_err(|error| OwnerErrorCode::from(&error)),
        OwnerRequest::Summarize { session } => client
            .summarize(session)
            .map(|()| OwnerResponse::Done)
            .map_err(|error| OwnerErrorCode::from(&error)),
        OwnerRequest::Feed { after, generation } => client
            .feed(after, generation)
            .map(|feed| OwnerResponse::Feed { feed })
            .map_err(|error| OwnerErrorCode::from(&error)),
    };
    result.unwrap_or_else(refuse)
}

fn refuse(code: OwnerErrorCode) -> OwnerResponse {
    OwnerResponse::Error { code }
}

fn state(client: &RefreshClient) -> Result<OwnerResponse, OwnerErrorCode> {
    let code = |error: RefreshError| OwnerErrorCode::from(&error);
    let account = client.account().map_err(code)?;
    let consent = client.consent().map_err(code)?;
    let refresh = client.state().map_err(code)?;
    Ok(OwnerResponse::State {
        state: OwnerState {
            account: account.map(|session| AccountView {
                subject: session.account().subject().to_owned(),
                email: session.account().email().map(str::to_owned),
                expires_at: Some(session.expires_at()),
            }),
            consent: ConsentView::from(&consent),
            connected: refresh.connected,
            rate_limited: refresh.rate_limited,
            unauthorized: refresh.unauthorized,
            summaries: refresh.summaries,
            feed: refresh.feed,
        },
    })
}

fn statuses(
    client: &RefreshClient,
    sessions: Vec<SessionId>,
) -> Result<OwnerResponse, OwnerErrorCode> {
    if sessions.len() > MAX_SESSIONS {
        return Err(OwnerErrorCode::InvalidRequest);
    }
    let statuses = client
        .statuses(sessions.clone())
        .map_err(|error| OwnerErrorCode::from(&error))?;
    Ok(OwnerResponse::Statuses {
        statuses: sessions
            .into_iter()
            .zip(statuses)
            .map(|(session, status)| StatusLine::new(session, status))
            .collect(),
    })
}

fn consent_offer(
    client: &RefreshClient,
    offer: &mut Option<(String, StatusConsentOffer)>,
) -> Result<OwnerResponse, OwnerErrorCode> {
    *offer = None;
    let code = |error: RefreshError| OwnerErrorCode::from(&error);
    let issued = client.consent_offer().map_err(code)?;
    let subject = issued.account().account.subject.clone();
    let email = client
        .account()
        .map_err(code)?
        .filter(|session| session.account().subject() == subject)
        .and_then(|session| session.account().email().map(str::to_owned));
    let id = Token::generate().as_str()[..32].to_owned();
    let view = OfferView {
        id: id.clone(),
        origin: issued.origin().to_owned(),
        account: AccountView {
            subject,
            email,
            expires_at: None,
        },
        consent_revision: issued.consent_revision().0,
    };
    *offer = Some((id, issued));
    Ok(OwnerResponse::Offer { offer: view })
}
