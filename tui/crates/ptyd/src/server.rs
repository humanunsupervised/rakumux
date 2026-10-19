//! Socket server: accept loop, one thread per connection, hello handshake,
//! request/response dispatch to `Control`, attach and event streams.
//!
//! Connection flow:
//! 1. Peers running as another user are dropped without a reply.
//! 2. The first request must be `hello`; `Control::authenticate` decides.
//!    Failed authentication is answered with an error and the connection is
//!    closed; a known op other than `hello` gets `unauthenticated` and is
//!    closed too.
//! 3. Requests are answered in turn. `attach` and `subscribe` turn the
//!    connection into a stream for the rest of its life. `daemon_status`,
//!    `daemon_shutdown` and `daemon_restart` are handled here; everything
//!    else goes to `Control::handle`.
//!
//! `daemon_restart` (and SIGUSR2, with the daemon's own executable)
//! restarts in place: see `handoff`.
//!
//! Unknown ops and unknown auth kinds are answered with `unsupported` and
//! malformed requests with `invalid_argument`; the connection stays usable,
//! so newer clients can probe an older daemon.

use crate::control::{ControlConfig, ControlService, Limits};
use crate::control_api::{Caller, Control};
use crate::handoff::{self, HandoffState};
use crate::host::{HostAttachment, HostError, TerminalHost, TerminalState};
use crate::registration::{self, Claim, ClaimError};
use crate::terminal::{Host, HostConfig};
use crate::{log_debug, log_error, log_info, log_warn};
use base64::Engine;
use session_core::api::{
    ApiError, AttachRequest, Attached, DaemonStatusInfo, ErrorCode, HelloOk, Request, Response,
    StreamCommand, SubscribeRequest,
};
use session_core::client::AttachEvent;
use session_core::frame::{self, Frame};
use session_core::paths::{Layout, Registration};
use session_core::PROTOCOL_VERSION;
use std::collections::HashMap;
use std::io;
use std::net::Shutdown;
use std::os::unix::io::{AsRawFd, RawFd};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc::RecvTimeoutError;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Daemon version reported in `hello`, `daemon_status` and `service.json`.
/// Release builds may set `RAKUMUX_VERSION` at compile time.
pub const VERSION: &str = match option_env!("RAKUMUX_VERSION") {
    Some(version) => version,
    None => env!("CARGO_PKG_VERSION"),
};

/// Optional features this daemon supports (`HelloOk::capabilities`).
pub const CAPABILITIES: &[&str] = &[
    "attach",
    "subscribe",
    "list",
    "get",
    "create",
    "rename",
    "kill",
    "delete",
    "revive",
    "report",
    "read",
    "wait",
    "send",
    "submit",
    "touch",
    "status",
    "shutdown",
    "projects",
    "projects.relocate",
    "session.pin",
    "session.git",
    "session.import",
    "auth.delegated",
    session_core::api::CAPABILITY_SESSIONS_ALL,
    session_core::api::CAPABILITY_DAEMON_RESTART,
];

/// Every `op` tag of `api::Request`; anything else is `unsupported`.
const KNOWN_OPS: &[&str] = &[
    "hello",
    "ping",
    "session_list",
    "session_get",
    "session_create",
    "session_import",
    "session_rename",
    "session_kill",
    "session_delete",
    "session_revive",
    "session_report",
    "session_read",
    "session_wait",
    "session_send",
    "session_submit",
    "session_touch",
    "session_pin",
    "session_bind",
    "project_list",
    "project_add",
    "project_remove",
    "project_relocate",
    "attach",
    "subscribe",
    "daemon_status",
    "daemon_shutdown",
    "daemon_restart",
];

/// `kind` tags of `api::Auth`.
const KNOWN_AUTH_KINDS: &[&str] = &["user", "session", "delegated"];

/// Time allowed for the hello.
const HELLO_TIMEOUT: Duration = Duration::from_secs(10);
/// Rejected frames tolerated before a hello.
const MAX_PRE_HELLO_ERRORS: u32 = 8;
/// How often an event stream checks for a closed client or shutdown.
const EVENT_POLL: Duration = Duration::from_millis(250);

const USAGE: &str = "usage: rakumux-ptyd serve | rakumux-ptyd --version";

/// Process entry point for the `rakumux-ptyd` binary. Returns the exit code.
/// `rakumux-ptyd serve` | `rakumux-ptyd --version`; internally also
/// `serve --handoff-fd <fd>` (a restart in place) and `handoff-versions`.
pub fn main_entry(args: Vec<String>) -> i32 {
    match args.get(1).map(String::as_str) {
        Some("--version" | "-V" | "version") if args.len() == 2 => {
            println!("rakumux-ptyd {VERSION} (protocol {PROTOCOL_VERSION})");
            0
        }
        Some("--help" | "-h" | "help") => {
            println!("{USAGE}");
            0
        }
        Some(handoff::VERSIONS_COMMAND) if args.len() == 2 => {
            println!("{}", handoff::HANDOFF_VERSION);
            0
        }
        Some("serve") if args.len() == 2 => serve_main(None),
        Some("serve") if args.len() == 4 && args[2] == "--handoff-fd" => {
            match args[3].parse::<RawFd>() {
                Ok(fd) => serve_main(Some(fd)),
                Err(_) => {
                    eprintln!("{USAGE}");
                    2
                }
            }
        }
        _ => {
            eprintln!("{USAGE}");
            2
        }
    }
}

/// The server of the running instance, for the signal and registration
/// threads (which outlive an in-process restart).
static CURRENT: Mutex<Option<ServerHandle>> = Mutex::new(None);

fn current() -> Option<ServerHandle> {
    CURRENT.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

/// `rakumux-ptyd serve`: run the daemon in the foreground. With
/// `handoff_fd`, carry on from the previous image of this process.
fn serve_main(handoff_fd: Option<RawFd>) -> i32 {
    // Block the daemon's signals before any thread exists so every thread
    // inherits the mask; one thread then waits for them. (Spawned programs
    // get an empty mask from portable-pty.)
    let signals = match block_daemon_signals() {
        Ok(set) => set,
        Err(e) => {
            eprintln!("rakumux-ptyd: cannot block signals: {e}");
            return 1;
        }
    };
    let mut handed = match handoff_fd.map(handoff::read_state).transpose() {
        Ok(state) => state,
        Err(e) => {
            // The handed-over descriptors are lost with the state: exiting
            // closes them and the programs get SIGHUP.
            eprintln!("rakumux-ptyd: cannot read the handoff state: {e}");
            if let Ok(layout) = Layout::from_env() {
                crate::log::init(&layout);
                log_error!("cannot read the handoff state: {e}; running sessions end");
            }
            return 1;
        }
    };
    // SAFETY: umask has no failure mode; it is set before any thread starts.
    let inherited_umask = unsafe { libc::umask(0o077) };
    let child_umask = match &handed {
        // This process already runs with 077; keep what the programs got.
        Some(state) => state.child_umask,
        None => Some(inherited_umask as u32),
    };
    let layout = match &handed {
        Some(state) => Ok(Layout {
            home: state.home.clone(),
            run_dir: state.run_dir.clone(),
        }),
        None => Layout::from_env().and_then(absolute_layout),
    };
    let layout = match layout {
        Ok(layout) => layout,
        Err(e) => {
            eprintln!("rakumux-ptyd: cannot resolve the Rakumux home: {e}");
            return 1;
        }
    };
    crate::log::init(&layout);
    match &handed {
        Some(state) => log_info!(
            "rakumux-ptyd {VERSION} restarted in place from {} (pid {}, {} running terminal(s))",
            state.from_version,
            std::process::id(),
            state.terminals.len()
        ),
        None => log_info!(
            "rakumux-ptyd {VERSION} starting (pid {}, home {})",
            std::process::id(),
            layout.home.display()
        ),
    }
    if let Err(e) = std::env::set_current_dir("/") {
        log_warn!("cannot change directory to /: {e}");
    }
    let spawned = std::thread::Builder::new()
        .name("signals".into())
        .spawn(move || wait_for_signals(signals));
    if let Err(e) = spawned {
        log_warn!("cannot start the signal thread: {e}");
    }
    loop {
        let instance = Instance {
            layout: &layout,
            child_umask,
        };
        let outcome = match handed.take() {
            Some(state) => instance.resume(state),
            None => instance.start(),
        };
        let (binary, state) = match outcome {
            Ok(Ended::Stopped) => {
                log_info!("rakumux-ptyd stopped");
                return 0;
            }
            Ok(Ended::Restart { binary, state }) => (binary, *state),
            Err(code) => return code,
        };
        // Only returns when no exec worked.
        handed = Some(restart(&layout, &binary, state));
        log_warn!("restarting in this process instead");
    }
}

/// `exec` `binary` with `state`; if that fails, this process's own
/// executable. Returns the state when neither could start, to restore it
/// in this process.
fn restart(layout: &Layout, binary: &std::path::Path, state: HandoffState) -> HandoffState {
    let mut targets = vec![binary.to_path_buf()];
    if let Some(own) = own_executable().filter(|own| own.as_path() != binary) {
        targets.push(own);
    }
    for target in targets {
        let fd = match handoff::write_state(&layout.run_dir, &state) {
            Ok(fd) => fd,
            Err(e) => {
                log_error!("cannot write the handoff state: {e}");
                break;
            }
        };
        let _ = handoff::exec(&target, fd);
    }
    state
}

/// The executable this process runs, or the file now at its path when it
/// was replaced (an upgrade or a rebuild: Linux then names it
/// "<path> (deleted)").
fn own_executable() -> Option<PathBuf> {
    let path = std::env::current_exe().ok()?;
    if path.is_file() {
        return Some(path);
    }
    let replaced = PathBuf::from(path.to_str()?.strip_suffix(" (deleted)")?);
    replaced.is_file().then_some(replaced)
}

/// How an instance of the server ended.
enum Ended {
    Stopped,
    /// `daemon_restart`: exec `binary` with `state`.
    Restart {
        binary: PathBuf,
        state: Box<HandoffState>,
    },
}

/// One run of the server in this process: started fresh, or resumed from
/// a handoff.
struct Instance<'a> {
    layout: &'a Layout,
    child_umask: Option<u32>,
}

impl Instance<'_> {
    fn start(self) -> Result<Ended, i32> {
        let claim = match registration::claim(self.layout, VERSION) {
            Ok(claim) => claim,
            Err(e @ ClaimError::AlreadyRunning(_)) => {
                log_info!("not starting: {e}");
                eprintln!("rakumux-ptyd: {e}");
                return Err(1);
            }
            Err(e) => {
                log_error!("{e}");
                eprintln!("rakumux-ptyd: {e}");
                return Err(1);
            }
        };
        let host = Host::new(self.host_config());
        let host: Arc<dyn TerminalHost> = Arc::new(host);
        let control = ControlService::open(self.control_config(&claim), host.clone());
        // Bring back what the previous daemon left running when it ended
        // without being asked to (a crash, a reboot).
        let control = control.inspect(|control| {
            control.revive_interrupted();
        });
        self.serve(claim, host, control)
    }

    fn resume(self, state: HandoffState) -> Result<Ended, i32> {
        let HandoffState {
            claim,
            control,
            terminals,
            ..
        } = state;
        let terminal_fds: Vec<RawFd> = terminals.iter().map(|t| t.fd).collect();
        let claim = match registration::reclaim(self.layout, VERSION, claim) {
            Ok(claim) => claim,
            Err(e) => {
                log_error!("cannot take over the daemon's socket: {e}; running sessions end");
                eprintln!("rakumux-ptyd: {e}");
                for fd in terminal_fds {
                    handoff::close(fd);
                }
                return Err(1);
            }
        };
        let host = Host::new(self.host_config());
        let adopted = host.adopt(terminals);
        log_info!("adopted {adopted} running terminal(s)");
        let host: Arc<dyn TerminalHost> = Arc::new(host);
        let control =
            ControlService::open_after_handoff(self.control_config(&claim), host.clone(), control);
        self.serve(claim, host, control)
    }

    fn host_config(&self) -> HostConfig {
        let mut config = HostConfig::new(self.layout.clone());
        config.child_umask = self.child_umask;
        config
    }

    fn control_config(&self, claim: &Claim) -> ControlConfig {
        ControlConfig {
            layout: self.layout.clone(),
            user_token: claim.user_token.clone(),
            socket: claim.registration.socket.clone(),
            rakumux_bin: std::env::var_os(session_core::env::BIN).map(PathBuf::from),
            version: VERSION.to_string(),
            limits: Limits::default(),
        }
    }

    fn serve(
        &self,
        claim: Claim,
        host: Arc<dyn TerminalHost>,
        control: io::Result<ControlService>,
    ) -> Result<Ended, i32> {
        let control: Arc<dyn Control> = match control {
            Ok(control) => Arc::new(control),
            Err(e) => {
                log_error!("cannot open the session registry: {e}");
                eprintln!("rakumux-ptyd: cannot open the session registry: {e}");
                host.shutdown();
                return Err(1);
            }
        };
        let watched = (
            claim.layout().clone(),
            claim.registration.instance_id.clone(),
        );
        let server = Server::new(claim, host, control);
        *CURRENT.lock().unwrap_or_else(|e| e.into_inner()) = Some(server.handle());
        start_registration_watchdog(watched.0, watched.1);
        let ended = server.serve();
        *CURRENT.lock().unwrap_or_else(|e| e.into_inner()) = None;
        Ok(match ended {
            Served::Stopped => Ended::Stopped,
            Served::Restart { binary, state } => Ended::Restart {
                binary,
                state: Box::new(HandoffState {
                    version: handoff::HANDOFF_VERSION,
                    from_version: VERSION.to_string(),
                    home: self.layout.home.clone(),
                    run_dir: self.layout.run_dir.clone(),
                    child_umask: self.child_umask,
                    claim: state.claim,
                    control: state.control,
                    terminals: state.terminals,
                }),
            },
        })
    }
}

/// Start the registration watchdog once per process (the instance id stays
/// the same across restarts in place).
fn start_registration_watchdog(layout: Layout, instance_id: String) {
    static STARTED: AtomicBool = AtomicBool::new(false);
    if STARTED.swap(true, Ordering::SeqCst) {
        return;
    }
    let spawned = std::thread::Builder::new()
        .name("registration".into())
        .spawn(move || watch_registration(layout, instance_id));
    if let Err(e) = spawned {
        log_warn!("cannot start the registration watchdog: {e}");
    }
}

/// Make `RAKUMUX_HOME` absolute before the daemon changes directory.
fn absolute_layout(layout: Layout) -> io::Result<Layout> {
    if layout.home.is_absolute() && layout.run_dir.is_absolute() {
        return Ok(layout);
    }
    let cwd = std::env::current_dir()?;
    Ok(Layout {
        home: cwd.join(&layout.home),
        run_dir: cwd.join(&layout.run_dir),
    })
}

/// SIGTERM and SIGINT stop the daemon; SIGUSR2 restarts it in place with
/// its own executable.
fn daemon_signal_set() -> libc::sigset_t {
    // SAFETY: sigemptyset/sigaddset initialise and fill a local set.
    unsafe {
        let mut set: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut set);
        libc::sigaddset(&mut set, libc::SIGTERM);
        libc::sigaddset(&mut set, libc::SIGINT);
        libc::sigaddset(&mut set, libc::SIGUSR2);
        set
    }
}

fn block_daemon_signals() -> io::Result<libc::sigset_t> {
    let set = daemon_signal_set();
    // SAFETY: `set` is initialised; the old mask is not needed.
    let rc = unsafe { libc::pthread_sigmask(libc::SIG_BLOCK, &set, std::ptr::null_mut()) };
    if rc != 0 {
        return Err(io::Error::from_raw_os_error(rc));
    }
    Ok(set)
}

fn wait_for_signals(set: libc::sigset_t) {
    loop {
        let mut signal: libc::c_int = 0;
        // SAFETY: `set` holds the blocked signals; `signal` is a valid out slot.
        let rc = unsafe { libc::sigwait(&set, &mut signal) };
        if rc != 0 {
            log_warn!("sigwait failed: {}", io::Error::from_raw_os_error(rc));
            return;
        }
        let Some(handle) = current() else {
            continue;
        };
        if signal == libc::SIGUSR2 {
            match own_executable() {
                Some(binary) => {
                    log_info!("signal {signal} received, restarting in place");
                    handle.restart(binary);
                }
                None => log_warn!("signal {signal}: can't find this daemon's executable"),
            }
            continue;
        }
        log_info!("signal {signal} received, shutting down");
        handle.shutdown();
        return;
    }
}

/// How often the daemon checks that its registration file is still its own.
const REGISTRATION_CHECK: std::time::Duration = std::time::Duration::from_secs(5);

/// Shut down when the registration file is gone or names another daemon:
/// the runtime directory was deleted (e.g. a removed temporary
/// `RAKUMUX_HOME`), so no client could find this daemon any more.
fn watch_registration(layout: Layout, instance_id: String) {
    loop {
        std::thread::sleep(REGISTRATION_CHECK);
        let Some(handle) = current() else {
            // Between instances (a restart in place).
            continue;
        };
        let ours = Registration::load(&layout)
            .map(|current| current.instance_id == instance_id)
            .unwrap_or(false);
        if !ours {
            log_warn!("registration file removed or replaced; shutting down");
            handle.shutdown();
            return;
        }
    }
}

/// Serve until shutdown (DaemonShutdown from a user client, SIGTERM/SIGINT).
///
/// Signal handling is installed by `main_entry`; embedders stop the server
/// with a [`ServerHandle`] (see [`Server`]). A restart request ends it too;
/// the handed-over terminals are then closed.
pub fn run(claim: Claim, host: Arc<dyn TerminalHost>, control: Arc<dyn Control>) {
    if let Served::Restart { state, .. } = Server::new(claim, host, control).serve() {
        handoff::close(state.claim.listener_fd);
        handoff::close(state.claim.lock_fd);
        for terminal in state.terminals {
            handoff::close(terminal.fd);
        }
    }
}

/// How [`Server::serve`] ended.
pub enum Served {
    Stopped,
    /// A restart in place was requested: exec `binary` with `state`.
    Restart {
        binary: PathBuf,
        state: Box<ServedHandoff>,
    },
}

/// What a server hands over when it restarts in place.
pub struct ServedHandoff {
    pub claim: handoff::ClaimHandoff,
    pub control: handoff::ControlHandoff,
    pub terminals: Vec<handoff::TerminalHandoff>,
}

/// The daemon's socket server. `serve` blocks until shutdown, then runs the
/// shutdown sequence: `Control::shutdown`, `TerminalHost::shutdown`, close
/// every connection, release the claim.
pub struct Server {
    claim: Claim,
    state: Arc<ServerState>,
}

/// Stops a running [`Server`] from another thread.
#[derive(Clone)]
pub struct ServerHandle {
    state: Arc<ServerState>,
}

impl ServerHandle {
    /// Begin a graceful shutdown; `Server::serve` returns once done.
    pub fn shutdown(&self) {
        self.state.begin_shutdown();
    }

    /// Begin a restart in place with `binary`; `Server::serve` returns
    /// [`Served::Restart`] once the terminals are handed over.
    pub fn restart(&self, binary: PathBuf) {
        self.state.begin_restart(binary);
    }
}

struct ServerState {
    host: Arc<dyn TerminalHost>,
    control: Arc<dyn Control>,
    /// Only peers with this uid may connect.
    allowed_uid: u32,
    socket: PathBuf,
    started: Instant,
    shutting_down: AtomicBool,
    clients: AtomicU32,
    next_connection: AtomicU64,
    /// Open connections, shut down when the server stops.
    connections: Mutex<HashMap<u64, UnixStream>>,
    /// Set by a restart request: the binary to exec.
    restart: Mutex<Option<PathBuf>>,
}

impl ServerState {
    fn begin_restart(&self, binary: PathBuf) {
        {
            let mut restart = self.restart.lock().unwrap_or_else(|e| e.into_inner());
            if self.is_shutting_down() {
                return;
            }
            *restart = Some(binary);
        }
        self.begin_shutdown();
    }

    fn begin_shutdown(&self) {
        if !self.shutting_down.swap(true, Ordering::SeqCst) {
            // Wake the accept loop.
            let _ = UnixStream::connect(&self.socket);
        }
    }

    fn is_shutting_down(&self) -> bool {
        self.shutting_down.load(Ordering::SeqCst)
    }

    fn status(&self) -> DaemonStatusInfo {
        let live_terminals = self
            .host
            .list()
            .iter()
            .filter(|t| t.state == TerminalState::Running)
            .count() as u32;
        DaemonStatusInfo {
            pid: std::process::id(),
            version: VERSION.to_string(),
            protocol: PROTOCOL_VERSION,
            uptime_secs: self.started.elapsed().as_secs(),
            sessions: self.control.session_count(),
            live_terminals,
            clients: self.clients.load(Ordering::SeqCst),
            rss_bytes: crate::procinfo::self_rss_bytes(),
        }
    }
}

impl Server {
    pub fn new(claim: Claim, host: Arc<dyn TerminalHost>, control: Arc<dyn Control>) -> Self {
        // SAFETY: geteuid has no preconditions.
        let uid = unsafe { libc::geteuid() };
        Self::with_allowed_uid(claim, host, control, uid)
    }

    fn with_allowed_uid(
        claim: Claim,
        host: Arc<dyn TerminalHost>,
        control: Arc<dyn Control>,
        allowed_uid: u32,
    ) -> Self {
        let socket = claim.registration.socket.clone();
        Self {
            claim,
            state: Arc::new(ServerState {
                host,
                control,
                allowed_uid,
                socket,
                started: Instant::now(),
                shutting_down: AtomicBool::new(false),
                clients: AtomicU32::new(0),
                next_connection: AtomicU64::new(1),
                connections: Mutex::new(HashMap::new()),
                restart: Mutex::new(None),
            }),
        }
    }

    pub fn handle(&self) -> ServerHandle {
        ServerHandle {
            state: self.state.clone(),
        }
    }

    /// Accept connections until shutdown, then shut everything down, or
    /// hand everything over for a restart in place.
    pub fn serve(self) -> Served {
        log_info!("listening on {}", self.state.socket.display());
        for incoming in self.claim.listener.incoming() {
            if self.state.is_shutting_down() {
                break;
            }
            match incoming {
                Ok(stream) => {
                    let id = self.state.next_connection.fetch_add(1, Ordering::SeqCst);
                    let state = self.state.clone();
                    let spawned = std::thread::Builder::new()
                        .name(format!("conn-{id}"))
                        .spawn(move || handle_connection(state, stream, id));
                    if let Err(e) = spawned {
                        log_error!("cannot start a connection thread: {e}");
                    }
                }
                Err(e) => {
                    log_warn!("accept failed: {e}");
                    std::thread::sleep(Duration::from_millis(50));
                }
            }
        }
        let restart = self
            .state
            .restart
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
        if let Some(binary) = restart {
            log_info!("restarting in place with {}", binary.display());
            let control = self.state.control.handoff();
            let terminals = self.state.host.handoff();
            self.close_connections();
            match self.claim.hand_off() {
                Ok(claim) => {
                    return Served::Restart {
                        binary,
                        state: Box::new(ServedHandoff {
                            claim,
                            control,
                            terminals,
                        }),
                    }
                }
                Err(e) => {
                    log_error!("cannot hand over the socket: {e}; running sessions end");
                    for terminal in terminals {
                        handoff::close(terminal.fd);
                    }
                    return Served::Stopped;
                }
            }
        }
        log_info!("shutting down");
        self.state.control.shutdown();
        self.state.host.shutdown();
        self.close_connections();
        drop(self.claim);
        Served::Stopped
    }

    fn close_connections(&self) {
        let connections: Vec<UnixStream> = self
            .state
            .connections
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .drain()
            .map(|(_, stream)| stream)
            .collect();
        for stream in connections {
            let _ = stream.shutdown(Shutdown::Both);
        }
    }
}

/// Uid of the process on the other end of `stream`.
pub fn peer_uid(stream: &UnixStream) -> io::Result<u32> {
    #[cfg(any(target_os = "linux", target_os = "android"))]
    {
        // SAFETY: getsockopt writes at most `len` bytes into `cred`.
        unsafe {
            let mut cred: libc::ucred = std::mem::zeroed();
            let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
            let rc = libc::getsockopt(
                stream.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_PEERCRED,
                (&mut cred as *mut libc::ucred).cast(),
                &mut len,
            );
            if rc != 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(cred.uid)
        }
    }
    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    {
        let mut uid: libc::uid_t = 0;
        let mut gid: libc::gid_t = 0;
        // SAFETY: getpeereid writes the two ids.
        let rc = unsafe { libc::getpeereid(stream.as_raw_fd(), &mut uid, &mut gid) };
        if rc != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(uid)
    }
}

/// Counts a connection as a client while it lives.
struct ConnectionGuard<'a> {
    state: &'a ServerState,
    id: u64,
}

impl Drop for ConnectionGuard<'_> {
    fn drop(&mut self) {
        self.state.clients.fetch_sub(1, Ordering::SeqCst);
        self.state
            .connections
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&self.id);
    }
}

fn handle_connection(state: Arc<ServerState>, mut stream: UnixStream, id: u64) {
    if state.is_shutting_down() {
        return;
    }
    match peer_uid(&stream) {
        Ok(uid) if uid == state.allowed_uid => {}
        Ok(uid) => {
            log_warn!("connection {id}: rejected peer with uid {uid}");
            return;
        }
        Err(e) => {
            log_warn!("connection {id}: cannot identify the peer: {e}");
            return;
        }
    }
    state.clients.fetch_add(1, Ordering::SeqCst);
    let _guard = ConnectionGuard { state: &state, id };
    if let Ok(clone) = stream.try_clone() {
        state
            .connections
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id, clone);
    }
    let Some(caller) = handshake(&state, &mut stream, id) else {
        let _ = stream.shutdown(Shutdown::Both);
        return;
    };
    log_debug!(
        "connection {id}: {} client {:?}",
        caller.client,
        caller.info
    );
    if let Err(e) = serve_requests(&state, &mut stream, &caller, id) {
        log_debug!("connection {id}: ended: {e}");
    }
    let _ = stream.shutdown(Shutdown::Both);
}

fn reply(stream: &mut UnixStream, response: &Response) -> io::Result<()> {
    frame::write_control(stream, response)
}

fn reply_error(stream: &mut UnixStream, error: ApiError) -> io::Result<()> {
    reply(stream, &Response::Error { error })
}

/// Decode a request, telling unknown ops (`unsupported`) apart from
/// malformed requests (`invalid_argument`).
fn decode_request(payload: &[u8]) -> Result<Request, ApiError> {
    let value: serde_json::Value = serde_json::from_slice(payload).map_err(|e| {
        ApiError::new(
            ErrorCode::InvalidArgument,
            format!("malformed request: {e}"),
        )
    })?;
    let op = value
        .get("op")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| ApiError::new(ErrorCode::InvalidArgument, "request has no \"op\""))?
        .to_string();
    if !KNOWN_OPS.contains(&op.as_str()) {
        return Err(ApiError::new(
            ErrorCode::Unsupported,
            format!("unsupported op {op:?} (daemon {VERSION})"),
        ));
    }
    if op == "hello" {
        if let Some(kind) = value
            .pointer("/auth/kind")
            .and_then(serde_json::Value::as_str)
        {
            if !KNOWN_AUTH_KINDS.contains(&kind) {
                return Err(ApiError::new(
                    ErrorCode::Unsupported,
                    format!("unsupported auth kind {kind:?} (daemon {VERSION})"),
                ));
            }
        }
    }
    serde_json::from_value(value).map_err(|e| {
        ApiError::new(
            ErrorCode::InvalidArgument,
            format!("invalid {op} request: {e}"),
        )
    })
}

/// Wait for a valid hello and authenticate it. `None` closes the connection.
fn handshake(state: &ServerState, stream: &mut UnixStream, id: u64) -> Option<Caller> {
    let _ = stream.set_read_timeout(Some(HELLO_TIMEOUT));
    let mut errors = 0;
    while errors < MAX_PRE_HELLO_ERRORS {
        let payload = match frame::read_frame(stream) {
            Ok(Some(Frame::Control(payload))) => payload,
            Ok(Some(Frame::Data(_))) => {
                let _ = reply_error(
                    stream,
                    ApiError::new(ErrorCode::Unauthenticated, "send hello first"),
                );
                return None;
            }
            Ok(None) => return None,
            Err(e) => {
                log_debug!("connection {id}: no hello: {e}");
                return None;
            }
        };
        match decode_request(&payload) {
            Ok(Request::Hello(hello)) => {
                return match state.control.authenticate(&hello) {
                    Ok(caller) => {
                        let ok = Response::Hello(HelloOk {
                            protocol: PROTOCOL_VERSION,
                            version: VERSION.to_string(),
                            capabilities: CAPABILITIES.iter().map(|c| c.to_string()).collect(),
                            caller: caller.info.clone(),
                        });
                        let _ = stream.set_read_timeout(None);
                        reply(stream, &ok).ok().map(|()| caller)
                    }
                    Err(error) => {
                        log_info!(
                            "connection {id}: hello from {:?} rejected: {error}",
                            hello.client
                        );
                        let _ = reply_error(stream, error);
                        None
                    }
                };
            }
            Ok(_) => {
                let _ = reply_error(
                    stream,
                    ApiError::new(ErrorCode::Unauthenticated, "send hello first"),
                );
                return None;
            }
            Err(error) => {
                errors += 1;
                if reply_error(stream, error).is_err() {
                    return None;
                }
            }
        }
    }
    None
}

/// Answer requests until the client leaves or the connection becomes a
/// stream that ends.
fn serve_requests(
    state: &ServerState,
    stream: &mut UnixStream,
    caller: &Caller,
    id: u64,
) -> io::Result<()> {
    loop {
        let payload = match frame::read_frame(stream)? {
            None => return Ok(()),
            Some(Frame::Control(payload)) => payload,
            Some(Frame::Data(_)) => {
                reply_error(
                    stream,
                    ApiError::new(
                        ErrorCode::InvalidArgument,
                        "unexpected data frame outside an attach stream",
                    ),
                )?;
                continue;
            }
        };
        let request = match decode_request(&payload) {
            Ok(request) => request,
            Err(error) => {
                reply_error(stream, error)?;
                continue;
            }
        };
        match request {
            Request::Hello(_) => reply_error(
                stream,
                ApiError::new(ErrorCode::InvalidArgument, "already authenticated"),
            )?,
            Request::Attach(request) => {
                if attach_stream(state, stream, caller, request, id)? {
                    return Ok(());
                }
            }
            Request::Subscribe(request) => {
                if event_stream(state, stream, caller, request)? {
                    return Ok(());
                }
            }
            Request::DaemonStatus => reply(stream, &Response::Status(state.status()))?,
            Request::DaemonShutdown => {
                if !caller.is_user() {
                    reply_error(
                        stream,
                        ApiError::new(
                            ErrorCode::PermissionDenied,
                            "only user clients can stop the daemon",
                        ),
                    )?;
                    continue;
                }
                log_info!("connection {id}: shutdown requested by {}", caller.client);
                state.control.stopped_by_user();
                reply(stream, &Response::Ok)?;
                state.begin_shutdown();
                return Ok(());
            }
            Request::DaemonRestart { binary } => {
                if !caller.is_user() {
                    reply_error(
                        stream,
                        ApiError::new(
                            ErrorCode::PermissionDenied,
                            "only user clients can restart the daemon",
                        ),
                    )?;
                    continue;
                }
                let binary = match binary.or_else(own_executable) {
                    Some(binary) => binary,
                    None => {
                        reply_error(
                            stream,
                            ApiError::new(
                                ErrorCode::InvalidArgument,
                                "this daemon can't find its own executable; name a binary",
                            ),
                        )?;
                        continue;
                    }
                };
                if let Err(message) = handoff::check_target(&binary) {
                    reply_error(stream, ApiError::new(ErrorCode::Unsupported, message))?;
                    continue;
                }
                log_info!(
                    "connection {id}: restart in place requested by {}",
                    caller.client
                );
                // Stop accepting before answering, so a client that
                // reconnects right away reaches the new image.
                state.begin_restart(binary);
                reply(stream, &Response::Ok)?;
                return Ok(());
            }
            other => {
                let response = state.control.handle(caller, other);
                reply(stream, &response)?;
            }
        }
    }
}

fn host_error(error: HostError) -> ApiError {
    let code = match error {
        HostError::NotFound | HostError::NotRunning => ErrorCode::SessionNotLive,
        HostError::NotController => ErrorCode::SessionInUse,
        HostError::InvalidKey(_) => ErrorCode::InvalidArgument,
        HostError::Spawn(_) | HostError::Io(_) => ErrorCode::Internal,
    };
    ApiError::new(code, error.to_string())
}

/// Run an attach stream. `Ok(false)` when the attach was refused (the
/// connection stays in request mode); `Ok(true)` once the stream ended.
fn attach_stream(
    state: &ServerState,
    stream: &mut UnixStream,
    caller: &Caller,
    request: AttachRequest,
    id: u64,
) -> io::Result<bool> {
    let (terminal, session) = match state.control.prepare_attach(caller, &request) {
        Ok(found) => found,
        Err(error) => {
            reply_error(stream, error)?;
            return Ok(false);
        }
    };
    let attachment = match state
        .host
        .attach(terminal, request.role, request.cols, request.rows)
    {
        Ok(attachment) => attachment,
        Err(error) => {
            reply_error(stream, host_error(error))?;
            return Ok(false);
        }
    };
    let HostAttachment {
        info,
        role,
        checkpoint,
        offset,
        events,
        control,
    } = attachment;
    reply(
        stream,
        &Response::Attached(Attached {
            session,
            role,
            cols: info.cols,
            rows: info.rows,
            checkpoint: base64::engine::general_purpose::STANDARD.encode(checkpoint),
            offset,
        }),
    )?;
    log_debug!("connection {id}: attached to terminal {terminal} as {role:?} at offset {offset}");

    let mut out = stream.try_clone()?;
    let writer = std::thread::Builder::new()
        .name(format!("conn-{id}-out"))
        .spawn(move || {
            for event in events {
                let written = match event {
                    AttachEvent::Output { offset, bytes } => {
                        frame::write_output(&mut out, offset, &bytes)
                    }
                    AttachEvent::Event(event) => frame::write_control(&mut out, &event),
                    AttachEvent::Closed(_) => break,
                };
                if written.is_err() {
                    break;
                }
            }
            // Ends the reader below when the terminal closed the stream.
            let _ = out.shutdown(Shutdown::Both);
        })?;

    loop {
        match frame::read_frame(stream) {
            Ok(Some(Frame::Data(bytes))) => {
                if let Err(e) = control.input(&bytes) {
                    log_debug!("connection {id}: input dropped: {e}");
                }
            }
            Ok(Some(Frame::Control(payload))) => {
                match frame::parse_control::<StreamCommand>(&payload) {
                    Ok(StreamCommand::Resize { cols, rows }) => {
                        if let Err(e) = control.resize(cols, rows) {
                            log_debug!("connection {id}: resize refused: {e}");
                        }
                    }
                    Ok(StreamCommand::Promote { cols, rows }) => {
                        if let Err(e) = control.promote(cols, rows) {
                            log_debug!("connection {id}: promote refused: {e}");
                        }
                    }
                    Ok(StreamCommand::Demote) => control.demote(),
                    Ok(StreamCommand::Fit { cols, rows }) => {
                        if let Err(e) = control.fit(cols, rows) {
                            log_debug!("connection {id}: fit refused: {e}");
                        }
                    }
                    Ok(StreamCommand::Detach) => break,
                    Err(e) => log_debug!("connection {id}: ignored stream command: {e}"),
                }
            }
            Ok(None) | Err(_) => break,
        }
    }
    control.detach();
    drop(control);
    let _ = stream.shutdown(Shutdown::Both);
    let _ = writer.join();
    log_debug!("connection {id}: detached from terminal {terminal}");
    Ok(true)
}

/// Run an event stream. `Ok(false)` when the subscription was refused.
fn event_stream(
    state: &ServerState,
    stream: &mut UnixStream,
    caller: &Caller,
    request: SubscribeRequest,
) -> io::Result<bool> {
    let events = match state.control.subscribe(caller, &request) {
        Ok(events) => events,
        Err(error) => {
            reply_error(stream, error)?;
            return Ok(false);
        }
    };
    reply(stream, &Response::Subscribed)?;
    let closed = Arc::new(AtomicBool::new(false));
    let mut input = stream.try_clone()?;
    let reader_closed = closed.clone();
    let reader = std::thread::Builder::new()
        .name("events-in".into())
        .spawn(move || {
            // Clients send nothing on event streams; this only notices EOF.
            while let Ok(Some(_)) = frame::read_frame(&mut input) {}
            reader_closed.store(true, Ordering::SeqCst);
        })?;
    while !closed.load(Ordering::SeqCst) && !state.is_shutting_down() {
        match events.recv_timeout(EVENT_POLL) {
            Ok(envelope) => {
                if frame::write_control(stream, &envelope).is_err() {
                    break;
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
    let _ = stream.shutdown(Shutdown::Both);
    let _ = reader.join();
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control_api::Caller;
    use session_core::api::{
        Auth, CallerInfo, CreateSession, EventEnvelope, Hello, ReadRequest, Report, ReviveSession,
        Role, SendRequest, WaitRequest, WaitUntil,
    };
    use session_core::ids::{SessionId, TerminalId};
    use session_core::model::SessionView;
    use std::sync::mpsc::Receiver;

    struct AcceptAll;

    impl Control for AcceptAll {
        fn authenticate(&self, hello: &Hello) -> Result<Caller, ApiError> {
            Ok(Caller {
                info: CallerInfo::User,
                client: hello.client.clone(),
                max_blocking_ms: hello.max_blocking_ms,
            })
        }
        fn handle(&self, _caller: &Caller, request: Request) -> Response {
            match request {
                Request::Ping => Response::Pong,
                _ => Response::Error {
                    error: ApiError::new(ErrorCode::Unsupported, "fake"),
                },
            }
        }
        fn prepare_attach(
            &self,
            _caller: &Caller,
            _request: &AttachRequest,
        ) -> Result<(TerminalId, SessionView), ApiError> {
            Err(ApiError::new(ErrorCode::NotFound, "fake"))
        }
        fn subscribe(
            &self,
            _caller: &Caller,
            _request: &SubscribeRequest,
        ) -> Result<Receiver<EventEnvelope>, ApiError> {
            Err(ApiError::new(ErrorCode::NotFound, "fake"))
        }
        fn session_count(&self) -> u32 {
            0
        }
        fn shutdown(&self) {}
    }

    fn hello() -> Request {
        Request::Hello(Hello {
            max_blocking_ms: None,
            protocol: PROTOCOL_VERSION,
            client: "test".into(),
            version: "0".into(),
            auth: Auth::User { token: "t".into() },
        })
    }

    /// Records the requests and subscriptions that reach the control layer.
    #[derive(Default)]
    struct Recording {
        requests: Mutex<Vec<Request>>,
        subscriptions: Mutex<Vec<SubscribeRequest>>,
    }

    impl Control for Recording {
        fn authenticate(&self, hello: &Hello) -> Result<Caller, ApiError> {
            AcceptAll.authenticate(hello)
        }
        fn handle(&self, _caller: &Caller, request: Request) -> Response {
            self.requests.lock().unwrap().push(request);
            Response::Sessions {
                sessions: Vec::new(),
            }
        }
        fn prepare_attach(
            &self,
            _caller: &Caller,
            _request: &AttachRequest,
        ) -> Result<(TerminalId, SessionView), ApiError> {
            Err(ApiError::new(ErrorCode::NotFound, "fake"))
        }
        fn subscribe(
            &self,
            _caller: &Caller,
            request: &SubscribeRequest,
        ) -> Result<Receiver<EventEnvelope>, ApiError> {
            self.subscriptions.lock().unwrap().push(request.clone());
            // No events: the stream ends right after `subscribed`.
            Ok(std::sync::mpsc::channel().1)
        }
        fn session_count(&self) -> u32 {
            0
        }
        fn shutdown(&self) {}
    }

    fn start(allowed_uid: Option<u32>) -> (tempfile::TempDir, ServerHandle, PathBuf) {
        start_with(Arc::new(AcceptAll), allowed_uid)
    }

    fn start_with(
        control: Arc<dyn Control>,
        allowed_uid: Option<u32>,
    ) -> (tempfile::TempDir, ServerHandle, PathBuf) {
        let home = tempfile::tempdir().unwrap();
        let layout = Layout::with_home(home.path().to_path_buf());
        let claim = registration::claim(&layout, "test").unwrap();
        let socket = claim.registration.socket.clone();
        let host: Arc<dyn TerminalHost> = Arc::new(Host::new(HostConfig::new(layout)));
        let server = match allowed_uid {
            Some(uid) => Server::with_allowed_uid(claim, host, control, uid),
            None => Server::new(claim, host, control),
        };
        let handle = server.handle();
        std::thread::spawn(move || server.serve());
        (home, handle, socket)
    }

    fn roundtrip(stream: &mut UnixStream, payload: &[u8]) -> Response {
        let mut buf = Vec::new();
        buf.extend_from_slice(&(payload.len() as u32 + 1).to_be_bytes());
        buf.push(0);
        buf.extend_from_slice(payload);
        std::io::Write::write_all(stream, &buf).unwrap();
        frame::read_control(stream).unwrap()
    }

    fn error_code(response: Response) -> ErrorCode {
        match response {
            Response::Error { error } => error.code,
            other => panic!("expected an error, got {other:?}"),
        }
    }

    #[test]
    fn peers_with_another_uid_are_dropped_without_a_reply() {
        // SAFETY: geteuid has no preconditions.
        let other_uid = unsafe { libc::geteuid() } + 1;
        let (_home, handle, socket) = start(Some(other_uid));
        let mut stream = UnixStream::connect(&socket).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let _ = frame::write_control(&mut stream, &hello());
        assert!(matches!(frame::read_frame(&mut stream), Ok(None) | Err(_)));
        handle.shutdown();
    }

    #[test]
    fn own_uid_is_accepted() {
        let (_home, handle, socket) = start(None);
        let stream = UnixStream::connect(&socket).unwrap();
        // SAFETY: geteuid has no preconditions.
        assert_eq!(peer_uid(&stream).unwrap(), unsafe { libc::geteuid() });
        let mut stream = stream;
        frame::write_control(&mut stream, &hello()).unwrap();
        let response: Response = frame::read_control(&mut stream).unwrap();
        assert!(matches!(response, Response::Hello(_)));
        handle.shutdown();
    }

    #[test]
    fn unknown_ops_and_auth_kinds_are_unsupported_and_keep_the_connection() {
        let (_home, handle, socket) = start(None);
        let mut stream = UnixStream::connect(&socket).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        // Before the hello: probing is answered and the connection stays.
        let probe = roundtrip(&mut stream, br#"{"op":"teleport"}"#);
        assert_eq!(error_code(probe), ErrorCode::Unsupported);
        let auth = roundtrip(
            &mut stream,
            br#"{"op":"hello","protocol":1,"client":"x","version":"0","auth":{"kind":"oauth"}}"#,
        );
        assert_eq!(error_code(auth), ErrorCode::Unsupported);
        let garbage = roundtrip(&mut stream, b"{not json");
        assert_eq!(error_code(garbage), ErrorCode::InvalidArgument);
        frame::write_control(&mut stream, &hello()).unwrap();
        assert!(matches!(
            frame::read_control::<_, Response>(&mut stream).unwrap(),
            Response::Hello(_)
        ));
        // After the hello, too.
        let unknown = roundtrip(&mut stream, br#"{"op":"session_teleport","id":"x"}"#);
        assert_eq!(error_code(unknown), ErrorCode::Unsupported);
        let malformed = roundtrip(&mut stream, br#"{"op":"session_get"}"#);
        assert_eq!(error_code(malformed), ErrorCode::InvalidArgument);
        let no_op = roundtrip(&mut stream, br#"{"id":"x"}"#);
        assert_eq!(error_code(no_op), ErrorCode::InvalidArgument);
        assert_eq!(roundtrip(&mut stream, br#"{"op":"ping"}"#), Response::Pong);
        handle.shutdown();
    }

    /// A connection that has said hello; returns the daemon's capabilities.
    fn connect(socket: &PathBuf) -> (UnixStream, Vec<String>) {
        let mut stream = UnixStream::connect(socket).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        frame::write_control(&mut stream, &hello()).unwrap();
        match frame::read_control(&mut stream).unwrap() {
            Response::Hello(ok) => (stream, ok.capabilities),
            other => panic!("expected hello, got {other:?}"),
        }
    }

    #[test]
    fn every_project_is_announced_and_all_reaches_the_control_layer() {
        let control = Arc::new(Recording::default());
        let (_home, handle, socket) = start_with(control.clone(), None);
        let (mut stream, capabilities) = connect(&socket);
        assert!(
            capabilities
                .iter()
                .any(|c| c == session_core::api::CAPABILITY_SESSIONS_ALL),
            "{capabilities:?}"
        );
        // An older client's request (no `all`) and a newer one's.
        for payload in [
            &br#"{"op":"session_list","scope":"/r"}"#[..],
            br#"{"op":"session_list","all":true,"include_dead":true}"#,
        ] {
            assert!(matches!(
                roundtrip(&mut stream, payload),
                Response::Sessions { .. }
            ));
        }
        let scope = session_core::model::ScopeRoot::from_canonical("/r".into());
        assert_eq!(
            *control.requests.lock().unwrap(),
            vec![
                Request::SessionList {
                    scope: Some(scope.clone()),
                    scopes: Vec::new(),
                    include_dead: false,
                    all: false,
                },
                Request::SessionList {
                    scope: None,
                    scopes: Vec::new(),
                    include_dead: true,
                    all: true,
                },
            ]
        );
        // Each subscription takes over its connection.
        for payload in [
            &br#"{"op":"subscribe","scope":"/r"}"#[..],
            br#"{"op":"subscribe","all":true}"#,
        ] {
            let (mut events, _) = connect(&socket);
            assert_eq!(roundtrip(&mut events, payload), Response::Subscribed);
        }
        assert_eq!(
            *control.subscriptions.lock().unwrap(),
            vec![
                SubscribeRequest {
                    scope: Some(scope),
                    ..Default::default()
                },
                SubscribeRequest {
                    all: true,
                    ..Default::default()
                },
            ]
        );
        handle.shutdown();
    }

    #[test]
    fn known_ops_cover_every_request_variant() {
        let id = SessionId::from_string("S");
        let requests = vec![
            hello(),
            Request::Ping,
            Request::SessionList {
                scopes: Vec::new(),
                scope: None,
                include_dead: false,
                all: false,
            },
            Request::SessionGet { id: id.clone() },
            Request::SessionImport {
                source: "/archive".into(),
            },
            Request::SessionCreate(CreateSession {
                scope: None,
                name: None,
                cwd: None,
                argv: None,
                startup_cmds: vec![],
                env: vec![],
                cols: 80,
                rows: 24,
            }),
            Request::SessionRename {
                id: id.clone(),
                name: "n".into(),
            },
            Request::SessionKill {
                id: id.clone(),
                tree: false,
            },
            Request::SessionDelete { id: id.clone() },
            Request::SessionRevive(ReviveSession {
                id: id.clone(),
                env: vec![],
                cols: 80,
                rows: 24,
                resume: true,
            }),
            Request::SessionReport(Report::default()),
            Request::SessionRead(ReadRequest {
                id: id.clone(),
                source: Default::default(),
                lines: None,
                format: Default::default(),
            }),
            Request::SessionWait(WaitRequest {
                id: id.clone(),
                until: WaitUntil::Idle,
                pattern: None,
                timeout_ms: 1,
            }),
            Request::SessionSend(SendRequest {
                id: id.clone(),
                keys: vec![],
                text: None,
            }),
            Request::SessionSubmit {
                id: id.clone(),
                text: "t".into(),
            },
            Request::SessionTouch { id: id.clone() },
            Request::SessionPin {
                id: id.clone(),
                pinned: true,
            },
            Request::SessionBind {
                agent: "opencode".into(),
                conversations: vec!["ses_1".into()],
            },
            Request::ProjectList,
            Request::ProjectAdd { path: "/p".into() },
            Request::ProjectRemove {
                path: session_core::model::ScopeRoot::from_canonical("/p".into()),
            },
            Request::ProjectRelocate {
                from: session_core::model::ScopeRoot::from_canonical("/before".into()),
                to: session_core::model::ScopeRoot::from_canonical("/after".into()),
            },
            Request::Attach(AttachRequest {
                id: id.clone(),
                role: Role::Observer,
                cols: 80,
                rows: 24,
            }),
            Request::Subscribe(SubscribeRequest::default()),
            Request::DaemonStatus,
            Request::DaemonShutdown,
            Request::DaemonRestart { binary: None },
        ];
        assert_eq!(requests.len(), KNOWN_OPS.len());
        // Every `Auth` kind is known too.
        for auth in [
            session_core::api::Auth::User { token: "t".into() },
            session_core::api::Auth::Session {
                id: id.clone(),
                token: "t".into(),
            },
            session_core::api::Auth::Delegated {
                token: "t".into(),
                agent: "opencode".into(),
                conversation: "ses_1".into(),
            },
        ] {
            let kind = serde_json::to_value(&auth).unwrap()["kind"].clone();
            assert!(KNOWN_AUTH_KINDS.contains(&kind.as_str().unwrap()), "{kind}");
        }
        for request in requests {
            let json = serde_json::to_vec(&request).unwrap();
            assert_eq!(decode_request(&json).unwrap(), request);
        }
    }
}
