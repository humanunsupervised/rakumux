//! Serve the API: claim the home, bind 127.0.0.1 (and, with remote access
//! on, the remote listener), publish `api.json`, serve until shutdown, then
//! remove `api.json`.

use crate::daemon::{DaemonConfig, DaemonPool};
use crate::discovery::{self, ApiInfo};
use crate::instance::{self, ClaimError};
use crate::remote::{self, RemotePlan};
use crate::routes::{self, AppState, Listener};
use crate::{log, VERSION};
use hyper::server::Server;
use hyper::service::{make_service_fn, service_fn};
use session_core::ids::Token;
use session_core::paths::Layout;
use std::convert::Infallible;
use std::future::Future;
use std::io;
use std::net::{Ipv4Addr, SocketAddr, TcpListener};
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::watch;

/// How long a client may take to send the request head.
const HEADER_TIMEOUT: Duration = Duration::from_secs(10);
/// How often the API checks that `api.json` still describes it.
const DISCOVERY_CHECK: Duration = Duration::from_secs(5);
/// TCP keepalive on accepted connections (a phone that vanished).
const TCP_KEEPALIVE: Duration = Duration::from_secs(60);
/// How long WebSocket streams get to say goodbye at shutdown.
const STREAMS_CLOSE: Duration = Duration::from_secs(1);

/// What to serve.
#[derive(Debug, Clone)]
pub struct ServeOptions {
    pub layout: Layout,
    /// Port on 127.0.0.1; 0 picks a free one.
    pub port: u16,
    /// `rakumux-ptyd` to start when the daemon isn't running.
    pub ptyd_path: Option<PathBuf>,
    /// `--remote-listen`: addresses for the remote listener instead of the
    /// Tailscale addresses (needs remote access on in `remote.json`).
    pub remote_listen: Vec<SocketAddr>,
}

#[derive(Debug)]
pub enum ServeError {
    Claim(ClaimError),
    Io(io::Error),
    /// Remote access was asked for (`--remote-listen`) and can't be served.
    Remote(String),
}

impl std::fmt::Display for ServeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ServeError::Claim(error) => error.fmt(f),
            ServeError::Io(error) => write!(f, "{error}"),
            ServeError::Remote(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for ServeError {}

impl From<io::Error> for ServeError {
    fn from(error: io::Error) -> Self {
        ServeError::Io(error)
    }
}

impl From<ClaimError> for ServeError {
    fn from(error: ClaimError) -> Self {
        ServeError::Claim(error)
    }
}

type Serving = Pin<Box<dyn Future<Output = hyper::Result<()>> + Send>>;

/// Serve until `shutdown` resolves. Requests in flight finish first;
/// WebSocket streams are closed.
pub async fn serve(
    options: ServeOptions,
    shutdown: impl Future<Output = ()>,
) -> Result<(), ServeError> {
    let instance = instance::claim(&options.layout)?;
    if let Some(stale) = &instance.stale {
        log::info(format_args!(
            "replacing stale {} (pid {}, {})",
            discovery::info_path(&options.layout).display(),
            stale.pid,
            stale.url
        ));
    }
    let loopback = bind((Ipv4Addr::LOCALHOST, options.port).into())?;
    let address = loopback.local_addr()?;
    let plan = remote::plan(&options.layout, &options.remote_listen).map_err(ServeError::Remote)?;
    let (remote_token, remote_listeners) = match plan {
        Some(plan) => bind_remote(plan)?,
        None => (None, Vec::new()),
    };
    let remote_urls = remote_listeners
        .iter()
        .map(|listener| listener.local_addr().map(remote::url))
        .collect::<io::Result<Vec<_>>>()?;
    let token = Token::generate();
    let info = ApiInfo {
        url: format!("http://{address}"),
        token: token.as_str().to_owned(),
        pid: std::process::id(),
        version: VERSION.to_owned(),
        remote_urls,
    };
    let (stop, stopped) = watch::channel(false);
    let state = Arc::new(AppState {
        token,
        remote_token,
        pool: DaemonPool::new(DaemonConfig {
            layout: options.layout.clone(),
            ptyd_path: options.ptyd_path.clone(),
            version: VERSION.to_owned(),
        }),
        shutdown: stopped.clone(),
        streams: AtomicUsize::new(0),
    });
    let mut servers: Vec<Serving> = vec![http_server(
        loopback,
        Listener::Loopback,
        &state,
        stopped.clone(),
    )?];
    for listener in remote_listeners {
        servers.push(http_server(
            listener,
            Listener::Remote,
            &state,
            stopped.clone(),
        )?);
    }

    // The sockets accept connections from here on, so publishing is safe.
    discovery::write(&options.layout, &info)?;
    log::info(format_args!(
        "rakumux-api {VERSION} listening on {} (pid {}, home {})",
        info.url,
        info.pid,
        options.layout.home.display()
    ));
    for url in &info.remote_urls {
        log::info(format_args!("remote access on {url}"));
    }
    let watchdog = tokio::spawn(keep_published(options.layout.clone(), info.clone()));

    let serving = futures_util::future::try_join_all(servers);
    tokio::pin!(serving);
    tokio::pin!(shutdown);
    let result = tokio::select! {
        // The servers only end early when one fails.
        result = &mut serving => result,
        () = &mut shutdown => {
            let _ = stop.send(true);
            serving.await
        }
    };
    let _ = stop.send(true);
    close_streams(&state).await;

    watchdog.abort();
    if let Err(error) = discovery::remove_if_owned(&options.layout, &info.token) {
        log::warn(format_args!("cannot remove api.json: {error}"));
    }
    drop(instance);
    log::info("rakumux-api stopped");
    result
        .map(|_| ())
        .map_err(|e| ServeError::Io(io::Error::other(e.to_string())))
}

/// A non-blocking listener on `addr`.
fn bind(addr: SocketAddr) -> io::Result<TcpListener> {
    let listener = TcpListener::bind(addr)?;
    listener.set_nonblocking(true)?;
    Ok(listener)
}

/// Bind the remote listener's addresses. Explicit ones (`--remote-listen`)
/// must all bind; otherwise failures are warnings and the API serves what
/// it could (loopback at least).
fn bind_remote(plan: RemotePlan) -> Result<(Option<Token>, Vec<TcpListener>), ServeError> {
    if plan.addrs.is_empty() {
        log::warn(
            "remote access is on, but this machine has no Tailscale address (and remote.json \
             names no `listen` address); serving 127.0.0.1 only",
        );
        return Ok((None, Vec::new()));
    }
    let mut listeners = Vec::new();
    for addr in &plan.addrs {
        match bind(*addr) {
            Ok(listener) => listeners.push(listener),
            Err(error) if plan.explicit => {
                return Err(ServeError::Remote(format!(
                    "cannot listen on {addr} for remote access: {error}"
                )))
            }
            Err(error) => log::warn(format_args!(
                "cannot listen on {addr} for remote access: {error}"
            )),
        }
    }
    let token = (!listeners.is_empty()).then_some(plan.token);
    Ok((token, listeners))
}

/// An HTTP/1.1 server on `listener` (WebSocket upgrades included) that
/// stops accepting when `stopped` becomes true.
fn http_server(
    listener: TcpListener,
    kind: Listener,
    state: &Arc<AppState>,
    mut stopped: watch::Receiver<bool>,
) -> Result<Serving, ServeError> {
    let state = state.clone();
    let make_service = make_service_fn(move |_connection| {
        let state = state.clone();
        async move {
            Ok::<_, Infallible>(service_fn(move |request| {
                routes::handle(state.clone(), kind, request)
            }))
        }
    });
    let server = Server::from_tcp(listener)
        .map_err(|e| io::Error::other(e.to_string()))?
        .tcp_nodelay(true)
        .tcp_keepalive(Some(TCP_KEEPALIVE))
        .http1_only(true)
        .http1_header_read_timeout(HEADER_TIMEOUT)
        .serve(make_service)
        .with_graceful_shutdown(async move {
            while !*stopped.borrow_and_update() {
                if stopped.changed().await.is_err() {
                    break;
                }
            }
        });
    Ok(Box::pin(server))
}

/// Give open WebSocket streams a moment to send their close frames.
async fn close_streams(state: &AppState) {
    let deadline = tokio::time::Instant::now() + STREAMS_CLOSE;
    while state.streams.load(Ordering::SeqCst) > 0 && tokio::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Rewrite `api.json` if someone deleted it while this API runs (it holds
/// the lock, so the file can only describe this API).
async fn keep_published(layout: Layout, info: ApiInfo) {
    let mut interval = tokio::time::interval(DISCOVERY_CHECK);
    loop {
        interval.tick().await;
        let current = ApiInfo::load(&layout).ok();
        if current.as_ref() != Some(&info) {
            log::warn("api.json was missing or changed; publishing it again");
            if let Err(error) = discovery::write(&layout, &info) {
                log::warn(format_args!("cannot write api.json: {error}"));
            }
        }
    }
}

/// Resolves on SIGINT or SIGTERM.
pub async fn shutdown_signal() {
    use tokio::signal::unix::{signal, SignalKind};
    let terminate = signal(SignalKind::terminate());
    let interrupt = signal(SignalKind::interrupt());
    match (terminate, interrupt) {
        (Ok(mut terminate), Ok(mut interrupt)) => {
            tokio::select! {
                _ = terminate.recv() => {}
                _ = interrupt.recv() => {}
            }
        }
        _ => {
            log::warn("cannot install signal handlers; stop the API with SIGKILL");
            std::future::pending::<()>().await;
        }
    }
}
