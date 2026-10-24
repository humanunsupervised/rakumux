//! Blocking client for the daemon, shared by the TUI, the CLI and (later) the
//! MCP adapter.
//!
//! [`SessionApi`] is the interface clients program against; [`Client`] is the
//! real implementation over the daemon socket. Each method may be called from
//! any thread. Streams ([`AttachStream`], [`EventStream`]) use their own
//! connection and deliver messages through channels, so a UI can poll them
//! without blocking.

use crate::api::{
    AgentReply, ApiError, AttachRequest, Attached, Auth, CallerInfo, CreateSession,
    DaemonStatusInfo, ErrorCode, EventEnvelope, Hello, HelloOk, ReadRequest, ReadResult, Report,
    Request, Response, ReviveSession, SendRequest, StreamCommand, StreamEvent, SubscribeRequest,
    WaitRequest, WaitResult,
};
use crate::frame::{self, Frame};
use crate::ids::{ReplyId, SessionId};
use crate::model::{ProjectView, ScopeRoot, SessionView};
use crate::paths::{Layout, Registration};
use std::io;
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, TryRecvError};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use crate::api::{OpenTerminal, TerminalAttachRequest, TerminalAttached};

pub type Result<T> = std::result::Result<T, ApiError>;

/// Operations every client can perform. Implemented by [`Client`] and by test
/// fakes.
pub trait SessionApi: Send + Sync {
    fn caller(&self) -> &CallerInfo;
    fn list(&self, scope: Option<&ScopeRoot>, include_dead: bool) -> Result<Vec<SessionView>>;
    fn get(&self, id: &SessionId) -> Result<SessionView>;
    fn create(&self, request: CreateSession) -> Result<SessionView>;
    /// Restore saved sessions without starting their programs (user only,
    /// daemon capability `session.import`). Conflicts are never overwritten.
    fn import_sessions(&self, source: &Path) -> Result<Vec<SessionView>> {
        let _ = source;
        Err(unsupported("importing saved sessions"))
    }
    fn rename(&self, id: &SessionId, name: &str) -> Result<SessionView>;
    fn kill(&self, id: &SessionId, tree: bool) -> Result<Vec<SessionId>>;
    fn delete(&self, id: &SessionId) -> Result<()>;
    fn revive(&self, request: ReviveSession) -> Result<SessionView>;
    fn report(&self, report: Report) -> Result<()>;
    fn read(&self, request: ReadRequest) -> Result<ReadResult>;
    fn wait(&self, request: WaitRequest) -> Result<WaitResult>;
    fn send(&self, request: SendRequest) -> Result<()>;
    fn submit(&self, id: &SessionId, text: &str) -> Result<()>;
    fn touch(&self, id: &SessionId) -> Result<()>;
    fn status(&self) -> Result<DaemonStatusInfo>;
    fn shutdown(&self) -> Result<()>;
    /// Restart the daemon in place, keeping running sessions (capability
    /// `daemon.restart`); `binary` replaces its executable. Returns once
    /// the daemon accepted; it then closes every connection.
    fn restart_daemon(&self, binary: Option<&Path>) -> Result<()> {
        let _ = binary;
        Err(unsupported("restarting the daemon in place"))
    }
    /// Open a dedicated attach stream.
    fn attach(&self, request: AttachRequest) -> Result<AttachStream>;
    /// Open a temporary terminal pane. Requires `terminal.pane`; user only.
    fn open_terminal(&self, request: OpenTerminal) -> Result<TerminalStream> {
        let _ = request;
        Err(unsupported("temporary terminal panes"))
    }
    /// Reconnect a pane without creating any terminal or session.
    fn attach_terminal(&self, request: TerminalAttachRequest) -> Result<TerminalStream> {
        let _ = request;
        Err(unsupported("temporary terminal panes"))
    }
    /// Open a dedicated event stream.
    fn subscribe(&self, request: SubscribeRequest) -> Result<EventStream>;

    /// Sessions of several projects (user callers): the union of `scopes`.
    fn list_in(&self, scopes: &[ScopeRoot], include_dead: bool) -> Result<Vec<SessionView>> {
        let _ = (scopes, include_dead);
        Err(unsupported("listing several projects"))
    }
    /// Sessions of every folder (user callers; daemon capability
    /// [`crate::api::CAPABILITY_SESSIONS_ALL`]).
    fn list_all(&self, include_dead: bool) -> Result<Vec<SessionView>> {
        let _ = include_dead;
        Err(unsupported("listing every folder"))
    }
    /// Projects the caller can see.
    fn projects(&self) -> Result<Vec<ProjectView>> {
        Err(unsupported("projects"))
    }
    /// Remember a directory as a project (user callers).
    fn add_project(&self, path: &Path) -> Result<ProjectView> {
        let _ = path;
        Err(unsupported("projects"))
    }
    /// Forget a project without sessions (user callers).
    fn remove_project(&self, path: &ScopeRoot) -> Result<()> {
        let _ = path;
        Err(unsupported("projects"))
    }
    /// Update saved folder references after a move (user only).
    fn relocate_project(&self, from: &ScopeRoot, to: &ScopeRoot) -> Result<()> {
        let _ = (from, to);
        Err(unsupported("relocating a folder"))
    }
    fn pin(&self, id: &SessionId, pinned: bool) -> Result<SessionView> {
        let _ = (id, pinned);
        Err(unsupported("pinning"))
    }
    /// Bind `agent` conversations to the calling session (session callers).
    fn bind(&self, agent: &str, conversations: &[String]) -> Result<()> {
        let _ = (agent, conversations);
        Err(unsupported("binding conversations"))
    }
    /// The current turn of a live session's agent (user callers; capability
    /// `session.turn`).
    fn turn(&self, id: &SessionId) -> Result<crate::api::TurnContent> {
        let _ = id;
        Err(unsupported("reading an agent's turn"))
    }
    /// A live session's agent conversation after `after` (user callers;
    /// capability `session.transcript`).
    fn transcript(
        &self,
        id: &SessionId,
        after: Option<&crate::api::TranscriptCursor>,
        max_bytes: Option<u32>,
    ) -> Result<crate::api::TranscriptPage> {
        let _ = (id, after, max_bytes);
        Err(unsupported("reading an agent's conversation"))
    }
    /// The optional features the daemon announced in its hello; `None` when
    /// the implementation can't tell (adapters, fakes).
    fn daemon_capabilities(&self) -> Option<Vec<String>> {
        None
    }
    /// Start or stop capturing agent replies (user only, capability
    /// `session.replies`).
    fn set_reply_capture(&self, enabled: bool) -> Result<()> {
        let _ = enabled;
        Err(unsupported("capturing agent replies"))
    }
    /// Captured replies, oldest first, after `after` when given; and whether
    /// capture is on (user only, capability `session.replies`).
    fn replies(&self, after: Option<&ReplyId>) -> Result<(Vec<AgentReply>, bool)> {
        let _ = after;
        Err(unsupported("reading agent replies"))
    }
}

fn unsupported(what: &str) -> ApiError {
    ApiError::new(
        ErrorCode::Unsupported,
        format!("{what} is not supported by this client"),
    )
}

/// How a client authenticates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthMode {
    /// Session auth when `RAKUMUX_SESSION_ID`/`RAKUMUX_SESSION_TOKEN` are
    /// set, otherwise user auth from the registration file.
    Auto,
    User,
    Session {
        id: SessionId,
        token: String,
    },
    /// User token, acting for the session that bound `conversation` of
    /// `agent` (`Auth::Delegated`).
    Delegated {
        agent: String,
        conversation: String,
    },
}

#[derive(Debug, Clone)]
pub struct ClientConfig {
    pub layout: Layout,
    /// "tui", "cli", "mcp", …
    pub client_name: String,
    pub version: String,
    pub auth: AuthMode,
    /// Spawn the daemon if none is running (user clients only).
    pub spawn: bool,
    /// Explicit daemon binary; otherwise `paths::find_ptyd_binary`.
    pub ptyd_path: Option<PathBuf>,
}

/// How long the daemon may take to answer the hello.
const HELLO_TIMEOUT: Duration = Duration::from_secs(5);
/// How long an ordinary request may take before the connection is dropped.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// Extra time on top of a wait's own `timeout_ms`.
const WAIT_GRACE: Duration = Duration::from_secs(10);
/// How long to wait for a freshly spawned daemon to register.
const SPAWN_WAIT: Duration = Duration::from_secs(2);
const SPAWN_POLL: Duration = Duration::from_millis(20);
/// Input frames are split into pieces of at most this size.
const INPUT_CHUNK: usize = 64 * 1024;
/// How often a stream writer thread checks whether its reader has finished.
const WRITER_POLL: Duration = Duration::from_millis(250);

/// Real client over the daemon socket.
///
/// Ordinary requests share one connection behind a mutex. A connection that
/// broke while idle is re-established once per request (re-reading the
/// registration, spawning the daemon again when allowed); a request whose
/// answer was lost is only resent when it is idempotent. `wait`, `attach`
/// and `subscribe` use dedicated connections.
pub struct Client {
    config: ClientConfig,
    caller: CallerInfo,
    identity: Identity,
    capabilities: Vec<String>,
    daemon_version: String,
    connection: Mutex<Option<UnixStream>>,
}

impl Client {
    /// Connect (spawning the daemon first when `config.spawn` and none is
    /// running) and complete the hello handshake.
    pub fn connect(config: ClientConfig) -> Result<Self> {
        let identity = resolve_identity(&config.auth, &|name| std::env::var(name).ok());
        let (stream, hello) = open_connection(&config, &identity)?;
        Ok(Self {
            caller: hello.caller,
            capabilities: hello.capabilities,
            daemon_version: hello.version,
            identity,
            connection: Mutex::new(Some(stream)),
            config,
        })
    }

    pub fn config(&self) -> &ClientConfig {
        &self.config
    }

    /// Optional features the daemon announced in its hello.
    pub fn capabilities(&self) -> &[String] {
        &self.capabilities
    }

    /// The daemon's version from its hello.
    pub fn daemon_version(&self) -> &str {
        &self.daemon_version
    }

    fn open(&self) -> Result<UnixStream> {
        open_connection(&self.config, &self.identity).map(|(stream, _)| stream)
    }

    fn lock_connection(&self) -> MutexGuard<'_, Option<UnixStream>> {
        match self.connection.lock() {
            Ok(guard) => guard,
            Err(poisoned) => {
                // A panic mid-request leaves the connection in an unknown
                // state; start over with a fresh one.
                let mut guard = poisoned.into_inner();
                *guard = None;
                guard
            }
        }
    }

    /// Send `request` on the shared connection and return the daemon's
    /// answer (`Response::Error` becomes `Err`).
    fn request(&self, request: &Request) -> Result<Response> {
        let mut guard = self.lock_connection();
        let mut retried = false;
        loop {
            let mut stream = match guard.take() {
                Some(stream) => stream,
                None => self.open()?,
            };
            match exchange(&mut stream, request, REQUEST_TIMEOUT) {
                Ok(response) => {
                    *guard = Some(stream);
                    return into_result(response);
                }
                Err(failure) => {
                    let retry = match &failure {
                        ExchangeError::NotSent(_) => true,
                        ExchangeError::Lost(_) => is_idempotent(request),
                        ExchangeError::Failed(_) => false,
                    };
                    if retry && !retried {
                        retried = true;
                        continue;
                    }
                    return Err(failure.into_api_error());
                }
            }
        }
    }

    fn expect_ok(&self, request: &Request) -> Result<()> {
        match self.request(request)? {
            Response::Ok => Ok(()),
            other => Err(unexpected(&other)),
        }
    }

    fn expect_session(&self, request: &Request) -> Result<SessionView> {
        match self.request(request)? {
            Response::Session { session } => Ok(session),
            other => Err(unexpected(&other)),
        }
    }

    /// Open a dedicated connection and turn it into a stream with `request`;
    /// returns the socket (read timeout cleared) and the first response.
    fn open_stream(&self, request: &Request) -> Result<(UnixStream, Response)> {
        let mut stream = self.open()?;
        let response = exchange(&mut stream, request, REQUEST_TIMEOUT)
            .map_err(ExchangeError::into_api_error)
            .and_then(into_result)?;
        set_read_timeout(&stream, None).map_err(unavailable_io)?;
        Ok((stream, response))
    }
}

impl SessionApi for Client {
    fn caller(&self) -> &CallerInfo {
        &self.caller
    }

    fn relocate_project(&self, from: &ScopeRoot, to: &ScopeRoot) -> Result<()> {
        self.expect_ok(&Request::ProjectRelocate {
            from: from.clone(),
            to: to.clone(),
        })
    }

    fn import_sessions(&self, source: &Path) -> Result<Vec<SessionView>> {
        match self.request(&Request::SessionImport {
            source: source.to_path_buf(),
        })? {
            Response::Sessions { sessions } => Ok(sessions),
            other => Err(unexpected(&other)),
        }
    }

    fn list(&self, scope: Option<&ScopeRoot>, include_dead: bool) -> Result<Vec<SessionView>> {
        let request = Request::SessionList {
            scope: scope.cloned(),
            scopes: Vec::new(),
            include_dead,
            all: false,
        };
        match self.request(&request)? {
            Response::Sessions { sessions } => Ok(sessions),
            other => Err(unexpected(&other)),
        }
    }

    fn list_in(&self, scopes: &[ScopeRoot], include_dead: bool) -> Result<Vec<SessionView>> {
        let request = Request::SessionList {
            scope: None,
            scopes: scopes.to_vec(),
            include_dead,
            all: false,
        };
        match self.request(&request)? {
            Response::Sessions { sessions } => Ok(sessions),
            other => Err(unexpected(&other)),
        }
    }

    fn list_all(&self, include_dead: bool) -> Result<Vec<SessionView>> {
        let request = Request::SessionList {
            scope: None,
            scopes: Vec::new(),
            include_dead,
            all: true,
        };
        match self.request(&request)? {
            Response::Sessions { sessions } => Ok(sessions),
            other => Err(unexpected(&other)),
        }
    }

    fn projects(&self) -> Result<Vec<ProjectView>> {
        match self.request(&Request::ProjectList)? {
            Response::Projects { projects } => Ok(projects),
            other => Err(unexpected(&other)),
        }
    }

    fn add_project(&self, path: &Path) -> Result<ProjectView> {
        let request = Request::ProjectAdd {
            path: path.to_path_buf(),
        };
        match self.request(&request)? {
            Response::Project { project } => Ok(project),
            other => Err(unexpected(&other)),
        }
    }

    fn remove_project(&self, path: &ScopeRoot) -> Result<()> {
        self.expect_ok(&Request::ProjectRemove { path: path.clone() })
    }

    fn pin(&self, id: &SessionId, pinned: bool) -> Result<SessionView> {
        self.expect_session(&Request::SessionPin {
            id: id.clone(),
            pinned,
        })
    }

    fn turn(&self, id: &SessionId) -> Result<crate::api::TurnContent> {
        match self.request(&Request::SessionTurn { id: id.clone() })? {
            Response::Turn { turn } => Ok(turn),
            other => Err(unexpected(&other)),
        }
    }

    fn transcript(
        &self,
        id: &SessionId,
        after: Option<&crate::api::TranscriptCursor>,
        max_bytes: Option<u32>,
    ) -> Result<crate::api::TranscriptPage> {
        let request = Request::SessionTranscript {
            id: id.clone(),
            after: after.cloned(),
            max_bytes,
        };
        match self.request(&request)? {
            Response::Transcript { transcript } => Ok(transcript),
            other => Err(unexpected(&other)),
        }
    }

    fn bind(&self, agent: &str, conversations: &[String]) -> Result<()> {
        self.expect_ok(&Request::SessionBind {
            agent: agent.to_owned(),
            conversations: conversations.to_vec(),
        })
    }

    fn daemon_capabilities(&self) -> Option<Vec<String>> {
        Some(self.capabilities.clone())
    }

    fn set_reply_capture(&self, enabled: bool) -> Result<()> {
        self.expect_ok(&Request::ReplyCapture { enabled })
    }

    fn replies(&self, after: Option<&ReplyId>) -> Result<(Vec<AgentReply>, bool)> {
        match self.request(&Request::ReplyList {
            after: after.cloned(),
        })? {
            Response::Replies { replies, capturing } => Ok((replies, capturing)),
            other => Err(unexpected(&other)),
        }
    }

    fn get(&self, id: &SessionId) -> Result<SessionView> {
        self.expect_session(&Request::SessionGet { id: id.clone() })
    }

    fn create(&self, request: CreateSession) -> Result<SessionView> {
        self.expect_session(&Request::SessionCreate(request))
    }

    fn rename(&self, id: &SessionId, name: &str) -> Result<SessionView> {
        self.expect_session(&Request::SessionRename {
            id: id.clone(),
            name: name.to_owned(),
        })
    }

    fn kill(&self, id: &SessionId, tree: bool) -> Result<Vec<SessionId>> {
        let request = Request::SessionKill {
            id: id.clone(),
            tree,
        };
        match self.request(&request)? {
            Response::Killed { killed } => Ok(killed),
            other => Err(unexpected(&other)),
        }
    }

    fn delete(&self, id: &SessionId) -> Result<()> {
        self.expect_ok(&Request::SessionDelete { id: id.clone() })
    }

    fn revive(&self, request: ReviveSession) -> Result<SessionView> {
        self.expect_session(&Request::SessionRevive(request))
    }

    fn report(&self, report: Report) -> Result<()> {
        self.expect_ok(&Request::SessionReport(report))
    }

    fn read(&self, request: ReadRequest) -> Result<ReadResult> {
        match self.request(&Request::SessionRead(request))? {
            Response::Read(result) => Ok(result),
            other => Err(unexpected(&other)),
        }
    }

    fn wait(&self, request: WaitRequest) -> Result<WaitResult> {
        // Dedicated connection: a wait may block for a long time and must not
        // hold up other requests on the shared one.
        let timeout = Duration::from_millis(request.timeout_ms).saturating_add(WAIT_GRACE);
        let mut stream = self.open()?;
        let response = exchange(&mut stream, &Request::SessionWait(request), timeout)
            .map_err(ExchangeError::into_api_error);
        let _ = stream.shutdown(Shutdown::Both);
        match into_result(response?)? {
            Response::Wait(result) => Ok(result),
            other => Err(unexpected(&other)),
        }
    }

    fn send(&self, request: SendRequest) -> Result<()> {
        self.expect_ok(&Request::SessionSend(request))
    }

    fn submit(&self, id: &SessionId, text: &str) -> Result<()> {
        self.expect_ok(&Request::SessionSubmit {
            id: id.clone(),
            text: text.to_owned(),
        })
    }

    fn touch(&self, id: &SessionId) -> Result<()> {
        self.expect_ok(&Request::SessionTouch { id: id.clone() })
    }

    fn status(&self) -> Result<DaemonStatusInfo> {
        match self.request(&Request::DaemonStatus)? {
            Response::Status(info) => Ok(info),
            other => Err(unexpected(&other)),
        }
    }

    fn shutdown(&self) -> Result<()> {
        let mut guard = self.lock_connection();
        // The daemon goes away either way; never reuse this connection.
        let mut stream = match guard.take() {
            Some(stream) => stream,
            None => self.open()?,
        };
        match exchange(&mut stream, &Request::DaemonShutdown, REQUEST_TIMEOUT) {
            Ok(response) => match into_result(response)? {
                Response::Ok => Ok(()),
                other => Err(unexpected(&other)),
            },
            // A daemon may exit before its answer reaches us.
            Err(ExchangeError::Lost(_)) => Ok(()),
            Err(other) => Err(other.into_api_error()),
        }
    }

    fn restart_daemon(&self, binary: Option<&Path>) -> Result<()> {
        let request = Request::DaemonRestart {
            binary: binary.map(Path::to_path_buf),
        };
        let mut guard = self.lock_connection();
        // The daemon closes every connection; never reuse this one.
        let mut stream = match guard.take() {
            Some(stream) => stream,
            None => self.open()?,
        };
        match exchange(&mut stream, &request, REQUEST_TIMEOUT) {
            Ok(response) => match into_result(response)? {
                Response::Ok => Ok(()),
                other => Err(unexpected(&other)),
            },
            // The daemon may close the connection before its answer arrives.
            Err(ExchangeError::Lost(_)) => Ok(()),
            Err(other) => Err(other.into_api_error()),
        }
    }

    fn attach(&self, request: AttachRequest) -> Result<AttachStream> {
        let (socket, response) = self.open_stream(&Request::Attach(request))?;
        let attached = match response {
            Response::Attached(attached) => attached,
            other => return Err(unexpected(&other)),
        };
        connect_attach_stream(socket, attached)
    }

    fn open_terminal(&self, request: OpenTerminal) -> Result<TerminalStream> {
        let (socket, response) = self.open_stream(&Request::TerminalOpen(request))?;
        match response {
            Response::TerminalAttached(attached) => connect_attach_stream(socket, attached),
            other => Err(unexpected(&other)),
        }
    }

    fn attach_terminal(&self, request: TerminalAttachRequest) -> Result<TerminalStream> {
        let (socket, response) = self.open_stream(&Request::TerminalAttach(request))?;
        match response {
            Response::TerminalAttached(attached) => connect_attach_stream(socket, attached),
            other => Err(unexpected(&other)),
        }
    }

    fn subscribe(&self, request: SubscribeRequest) -> Result<EventStream> {
        let (socket, response) = self.open_stream(&Request::Subscribe(request))?;
        if response != Response::Subscribed {
            return Err(unexpected(&response));
        }
        let (tx, rx) = mpsc::channel();
        spawn_thread("rakumux-events-rx", move || event_reader(socket, tx))?;
        Ok(EventStream::from_receiver(rx))
    }
}

// ---------------------------------------------------------------- connecting

/// Who the client is, resolved once from [`AuthMode`] and the environment.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Identity {
    /// Token and socket come from the registration file, re-read on every
    /// reconnect so a restarted daemon is picked up.
    User,
    Session {
        id: SessionId,
        token: String,
        /// `RAKUMUX_SOCKET`; otherwise the registration file's socket.
        socket: Option<PathBuf>,
    },
    /// Like `User` for the token and socket.
    Delegated { agent: String, conversation: String },
}

fn resolve_identity(mode: &AuthMode, env: &dyn Fn(&str) -> Option<String>) -> Identity {
    let var = |name: &str| env(name).filter(|value| !value.is_empty());
    let socket = || var(crate::env::SOCKET).map(PathBuf::from);
    match mode {
        AuthMode::User => Identity::User,
        AuthMode::Delegated {
            agent,
            conversation,
        } => Identity::Delegated {
            agent: agent.clone(),
            conversation: conversation.clone(),
        },
        AuthMode::Session { id, token } => Identity::Session {
            id: id.clone(),
            token: token.clone(),
            socket: socket(),
        },
        AuthMode::Auto => match (var(crate::env::SESSION_ID), var(crate::env::SESSION_TOKEN)) {
            (Some(id), Some(token)) => Identity::Session {
                id: SessionId::from_string(id),
                token,
                socket: socket(),
            },
            _ => Identity::User,
        },
    }
}

/// Why opening a connection failed.
enum OpenError {
    /// No daemon answers (no registration, socket missing or refused), so
    /// spawning one may help.
    NotRunning(ApiError),
    /// A daemon answered but the handshake failed, or something else broke.
    Other(ApiError),
}

impl OpenError {
    fn into_api_error(self) -> ApiError {
        match self {
            OpenError::NotRunning(error) | OpenError::Other(error) => error,
        }
    }
}

/// Connect and say hello, spawning the daemon once if allowed and needed.
fn open_connection(config: &ClientConfig, identity: &Identity) -> Result<(UnixStream, HelloOk)> {
    match try_open(config, identity) {
        Ok(opened) => Ok(opened),
        Err(OpenError::NotRunning(_)) if config.spawn && *identity == Identity::User => {
            spawn_daemon(config)?;
            wait_for_daemon(config, identity)
        }
        Err(error) => Err(error.into_api_error()),
    }
}

/// Poll until a freshly spawned daemon accepts a hello, giving up after
/// [`SPAWN_WAIT`].
fn wait_for_daemon(config: &ClientConfig, identity: &Identity) -> Result<(UnixStream, HelloOk)> {
    let deadline = Instant::now() + SPAWN_WAIT;
    loop {
        match try_open(config, identity) {
            Ok(opened) => return Ok(opened),
            Err(OpenError::Other(error)) => return Err(error),
            Err(OpenError::NotRunning(_)) if Instant::now() >= deadline => {
                return Err(ApiError::new(
                    ErrorCode::DaemonUnavailable,
                    format!(
                        "rakumux-ptyd did not start within {} s; see {}",
                        SPAWN_WAIT.as_secs(),
                        config.layout.log_file().display()
                    ),
                ));
            }
            Err(OpenError::NotRunning(_)) => std::thread::sleep(SPAWN_POLL),
        }
    }
}

/// Open a connection in the configured layout or, when no daemon answers
/// there, in a legacy layout ([`Layout::legacy`]) where a daemon an earlier
/// build started may still run. The configured layout's error is reported.
fn try_open(
    config: &ClientConfig,
    identity: &Identity,
) -> std::result::Result<(UnixStream, HelloOk), OpenError> {
    let explicit_socket = matches!(
        identity,
        Identity::Session {
            socket: Some(_),
            ..
        }
    );
    let first = try_open_in(config, &config.layout, identity);
    if explicit_socket || !matches!(first, Err(OpenError::NotRunning(_))) {
        return first;
    }
    for layout in config.layout.legacy() {
        if let Ok(opened) = try_open_in(config, &layout, identity) {
            return Ok(opened);
        }
    }
    first
}

fn try_open_in(
    config: &ClientConfig,
    layout: &Layout,
    identity: &Identity,
) -> std::result::Result<(UnixStream, HelloOk), OpenError> {
    let (socket, auth) = match identity {
        Identity::User => {
            let registration = load_registration(layout)?;
            let auth = Auth::User {
                token: registration.token,
            };
            (registration.socket, auth)
        }
        Identity::Delegated {
            agent,
            conversation,
        } => {
            let registration = load_registration(layout)?;
            let auth = Auth::Delegated {
                token: registration.token,
                agent: agent.clone(),
                conversation: conversation.clone(),
            };
            (registration.socket, auth)
        }
        Identity::Session { id, token, socket } => {
            let socket = match socket {
                Some(socket) => socket.clone(),
                None => load_registration(layout)?.socket,
            };
            let auth = Auth::Session {
                id: id.clone(),
                token: token.clone(),
            };
            (socket, auth)
        }
    };
    let mut stream = UnixStream::connect(&socket).map_err(|error| {
        let not_running = matches!(
            error.kind(),
            io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
        );
        let error = ApiError::new(
            ErrorCode::DaemonUnavailable,
            format!("cannot connect to {}: {error}", socket.display()),
        );
        if not_running {
            OpenError::NotRunning(error)
        } else {
            OpenError::Other(error)
        }
    })?;
    let hello = hello(&mut stream, config, auth).map_err(OpenError::Other)?;
    Ok((stream, hello))
}

fn load_registration(layout: &Layout) -> std::result::Result<Registration, OpenError> {
    Registration::load(layout).map_err(|error| {
        let path = layout.registration_file();
        if error.kind() == io::ErrorKind::NotFound {
            OpenError::NotRunning(ApiError::new(
                ErrorCode::DaemonUnavailable,
                format!("rakumux-ptyd is not running (no {})", path.display()),
            ))
        } else {
            OpenError::Other(ApiError::new(
                ErrorCode::DaemonUnavailable,
                format!("cannot read {}: {error}", path.display()),
            ))
        }
    })
}

fn hello(stream: &mut UnixStream, config: &ClientConfig, auth: Auth) -> Result<HelloOk> {
    let request = Request::Hello(Hello {
        max_blocking_ms: None,
        protocol: crate::PROTOCOL_VERSION,
        client: config.client_name.clone(),
        version: config.version.clone(),
        auth,
    });
    let response = exchange(stream, &request, HELLO_TIMEOUT)
        .map_err(ExchangeError::into_api_error)
        .and_then(into_result)?;
    match response {
        Response::Hello(ok) if ok.protocol == crate::PROTOCOL_VERSION => Ok(ok),
        Response::Hello(ok) => Err(ApiError::new(
            ErrorCode::ProtocolMismatch,
            format!(
                "rakumux-ptyd {} speaks protocol {}, this client speaks {}",
                ok.version,
                ok.protocol,
                crate::PROTOCOL_VERSION
            ),
        )),
        other => Err(unexpected(&other)),
    }
}

// ---------------------------------------------------------------- spawning

/// Start `rakumux-ptyd serve` fully detached from this process.
fn spawn_daemon(config: &ClientConfig) -> Result<()> {
    let binary = crate::paths::find_ptyd_binary(config.ptyd_path.as_deref()).ok_or_else(|| {
        let message = match &config.ptyd_path {
            Some(path) => format!("rakumux-ptyd not found at {}", path.display()),
            None => format!(
                "rakumux-ptyd not found (set {} or install it next to rakumux)",
                crate::env::PTYD_BIN
            ),
        };
        ApiError::new(ErrorCode::DaemonUnavailable, message)
    })?;
    // The child runs in `/`, so a relative path would no longer resolve.
    let binary = std::path::absolute(&binary).map_err(unavailable_io)?;
    let mut command = Command::new(&binary);
    command.arg("serve");
    // The daemon must not believe it runs inside one of its sessions.
    for name in [
        crate::env::SESSION_ID,
        crate::env::SESSION_TOKEN,
        crate::env::SOCKET,
    ] {
        command.env_remove(name);
    }
    // The daemon derives its layout from the environment; make it match ours.
    let home = std::path::absolute(&config.layout.home).map_err(unavailable_io)?;
    command.env(crate::env::HOME, home);
    spawn_detached(command).map(|_| ()).map_err(|error| {
        ApiError::new(
            ErrorCode::DaemonUnavailable,
            format!("cannot start {}: {error}", binary.display()),
        )
    })
}

/// Spawn `command` with stdin, stdout and stderr on `/dev/null`, in a new
/// session (`setsid`) with cwd `/`, and reap it in the background. Inherited
/// stdio would keep a `$(rakumux …)` command substitution open until the
/// child exits. Returns the child's pid.
fn spawn_detached(mut command: Command) -> io::Result<u32> {
    use std::os::unix::process::CommandExt;

    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .current_dir("/");
    // SAFETY: `setsid` is async-signal-safe and touches no Rust state.
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = command.spawn()?;
    let pid = child.id();
    // Reap the child when it exits so it never lingers as a zombie of a
    // long-running client.
    std::thread::Builder::new()
        .name("rakumux-reaper".into())
        .spawn(move || {
            let _ = child.wait();
        })?;
    Ok(pid)
}

// ---------------------------------------------------------------- exchanges

/// A closed macOS socket rejects timeout changes even while received bytes
/// remain buffered. A confirmed hangup cannot block; let reads drain those
/// bytes and writes report the disconnect through the normal retry path.
fn set_read_timeout(stream: &UnixStream, timeout: Option<Duration>) -> io::Result<()> {
    let result = stream.set_read_timeout(timeout);
    #[cfg(target_os = "macos")]
    if let Err(error) = &result {
        use std::os::fd::AsRawFd;

        if error.raw_os_error() == Some(libc::EINVAL) {
            let mut descriptor = libc::pollfd {
                fd: stream.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            // SAFETY: one initialized descriptor, borrowed for this zero-timeout poll.
            let ready = unsafe { libc::poll(&mut descriptor, 1, 0) };
            if ready == 1 && descriptor.revents & libc::POLLHUP != 0 {
                return Ok(());
            }
        }
    }
    result
}

/// How a request/response exchange failed.
enum ExchangeError {
    /// The request could not be written (the daemon closed the connection
    /// while it was idle): nothing was sent.
    NotSent(io::Error),
    /// The request was written but the connection closed before an answer.
    Lost(io::Error),
    /// Anything else: timeout, malformed answer.
    Failed(ApiError),
}

impl ExchangeError {
    fn into_api_error(self) -> ApiError {
        match self {
            ExchangeError::NotSent(error) => ApiError::new(
                ErrorCode::DaemonUnavailable,
                format!("cannot reach rakumux-ptyd: {error}"),
            ),
            ExchangeError::Lost(error) => ApiError::new(
                ErrorCode::DaemonUnavailable,
                format!("connection to rakumux-ptyd lost: {error}"),
            ),
            ExchangeError::Failed(error) => error,
        }
    }
}

/// Write one request and read one response, giving up after `timeout`.
fn exchange(
    stream: &mut UnixStream,
    request: &Request,
    timeout: Duration,
) -> std::result::Result<Response, ExchangeError> {
    set_read_timeout(stream, Some(timeout.max(Duration::from_millis(1))))
        .map_err(|error| ExchangeError::Failed(unavailable_io(error)))?;
    if let Err(error) = frame::write_control(stream, request) {
        return Err(match error.kind() {
            io::ErrorKind::BrokenPipe
            | io::ErrorKind::ConnectionReset
            | io::ErrorKind::NotConnected => ExchangeError::NotSent(error),
            _ => ExchangeError::Failed(unavailable_io(error)),
        });
    }
    match frame::read_frame(stream) {
        Ok(Some(Frame::Control(payload))) => {
            frame::parse_control::<Response>(&payload).map_err(|error| {
                ExchangeError::Failed(ApiError::new(
                    ErrorCode::Internal,
                    format!("unreadable response from rakumux-ptyd: {error}"),
                ))
            })
        }
        Ok(Some(Frame::Data(_))) => Err(ExchangeError::Failed(ApiError::new(
            ErrorCode::Internal,
            "rakumux-ptyd sent terminal data instead of a response",
        ))),
        Ok(None) => Err(ExchangeError::Lost(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "connection closed",
        ))),
        Err(error) => match error.kind() {
            io::ErrorKind::ConnectionReset
            | io::ErrorKind::UnexpectedEof
            | io::ErrorKind::BrokenPipe => Err(ExchangeError::Lost(error)),
            io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut => {
                Err(ExchangeError::Failed(ApiError::new(
                    ErrorCode::DaemonUnavailable,
                    format!(
                        "rakumux-ptyd did not answer within {:.1} s",
                        timeout.as_secs_f32()
                    ),
                )))
            }
            _ => Err(ExchangeError::Failed(unavailable_io(error))),
        },
    }
}

/// Requests that are safe to send twice when the first answer was lost.
fn is_idempotent(request: &Request) -> bool {
    matches!(
        request,
        Request::Ping
            | Request::SessionList { .. }
            | Request::ProjectList
            | Request::SessionPin { .. }
            | Request::SessionGet { .. }
            | Request::SessionRead(_)
            | Request::SessionTurn { .. }
            | Request::SessionTranscript { .. }
            | Request::SessionTouch { .. }
            | Request::DaemonStatus
    )
}

fn into_result(response: Response) -> Result<Response> {
    match response {
        Response::Error { error } => Err(error),
        other => Ok(other),
    }
}

fn unexpected(response: &Response) -> ApiError {
    let kind = serde_json::to_value(response)
        .ok()
        .and_then(|value| value.get("type")?.as_str().map(str::to_owned))
        .unwrap_or_else(|| "unknown".into());
    ApiError::new(
        ErrorCode::Internal,
        format!("unexpected `{kind}` response from rakumux-ptyd"),
    )
}

fn unavailable_io(error: io::Error) -> ApiError {
    ApiError::new(
        ErrorCode::DaemonUnavailable,
        format!("rakumux-ptyd connection failed: {error}"),
    )
}

fn spawn_thread(name: &str, body: impl FnOnce() + Send + 'static) -> Result<()> {
    std::thread::Builder::new()
        .name(name.into())
        .spawn(body)
        .map(|_| ())
        .map_err(|error| {
            ApiError::new(ErrorCode::Internal, format!("cannot start thread: {error}"))
        })
}

// ---------------------------------------------------------------- stream I/O

/// Daemon → client half of an attach stream: output frames and stream events
/// into the [`AttachStream`]'s channel. Ends with exactly one `Closed`.
fn attach_reader(mut socket: UnixStream, events: Sender<AttachEvent>, finished: Arc<AtomicBool>) {
    let closed = loop {
        let event = match frame::read_frame(&mut socket) {
            Ok(Some(Frame::Data(payload))) => match frame::split_output(&payload) {
                Ok((offset, bytes)) => AttachEvent::Output {
                    offset,
                    bytes: bytes.to_vec(),
                },
                Err(error) => break Some(error.to_string()),
            },
            Ok(Some(Frame::Control(payload))) => {
                match frame::parse_control::<StreamEvent>(&payload) {
                    Ok(event) => AttachEvent::Event(event),
                    Err(_) => match frame::parse_control::<Response>(&payload) {
                        Ok(Response::Error { error }) => break Some(error.to_string()),
                        // A newer daemon's event this client doesn't know.
                        _ => continue,
                    },
                }
            }
            Ok(None) => break None,
            // Our own writer shut the socket down after a detach.
            Err(_) if finished.load(Ordering::SeqCst) => break None,
            Err(error) => break Some(error.to_string()),
        };
        if events.send(event).is_err() {
            // Nobody listens any more.
            break None;
        }
    };
    finished.store(true, Ordering::SeqCst);
    let _ = events.send(AttachEvent::Closed(closed));
    let _ = socket.shutdown(Shutdown::Both);
}

/// Client → daemon half of an attach stream. Dropping every [`AttachSender`]
/// (the stream's own included) detaches.
fn attach_writer(
    mut socket: UnixStream,
    outgoing: Receiver<AttachOutgoing>,
    finished: Arc<AtomicBool>,
) {
    loop {
        let message = match outgoing.recv_timeout(WRITER_POLL) {
            Ok(message) => message,
            Err(RecvTimeoutError::Timeout) if finished.load(Ordering::SeqCst) => return,
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => AttachOutgoing::Command(StreamCommand::Detach),
        };
        let written = match &message {
            AttachOutgoing::Input(bytes) => bytes
                .chunks(INPUT_CHUNK)
                .try_for_each(|chunk| frame::write_input(&mut socket, chunk)),
            AttachOutgoing::Command(command) => frame::write_control(&mut socket, command),
        };
        if written.is_err() {
            let _ = socket.shutdown(Shutdown::Both);
            return;
        }
        if message == AttachOutgoing::Command(StreamCommand::Detach) {
            // The daemon ends the stream; the reader sees EOF and reports
            // `Closed(None)`.
            finished.store(true, Ordering::SeqCst);
            let _ = socket.shutdown(Shutdown::Write);
            return;
        }
    }
}

/// Reads event envelopes into an [`EventStream`]'s channel until the daemon
/// ends the stream or nobody listens any more.
fn event_reader(mut socket: UnixStream, tx: Sender<std::result::Result<EventEnvelope, String>>) {
    loop {
        let item = match frame::read_frame(&mut socket) {
            Ok(Some(Frame::Control(payload))) => {
                match frame::parse_control::<EventEnvelope>(&payload) {
                    Ok(envelope) => Ok(envelope),
                    Err(error) => match frame::parse_control::<Response>(&payload) {
                        Ok(Response::Error { error }) => Err(error.to_string()),
                        _ => Err(format!("unreadable event from rakumux-ptyd: {error}")),
                    },
                }
            }
            Ok(Some(Frame::Data(_))) => Err("unexpected terminal data on an event stream".into()),
            Ok(None) => Err("event stream closed by rakumux-ptyd".into()),
            Err(error) => Err(format!("event stream failed: {error}")),
        };
        let last = item.is_err();
        if tx.send(item).is_err() || last {
            break;
        }
    }
    let _ = socket.shutdown(Shutdown::Both);
}

// ---------------------------------------------------------------- streams

/// Something received on an attach stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AttachEvent {
    Output {
        offset: u64,
        bytes: Vec<u8>,
    },
    Event(StreamEvent),
    /// The stream ended; `Some` carries an error description.
    Closed(Option<String>),
}

/// Something a client sends on an attach stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AttachOutgoing {
    Input(Vec<u8>),
    Command(StreamCommand),
}

/// Cloneable handle for writing to an attach stream.
#[derive(Debug, Clone)]
pub struct AttachSender {
    tx: Sender<AttachOutgoing>,
}

impl AttachSender {
    pub fn input(&self, bytes: impl Into<Vec<u8>>) -> Result<()> {
        self.send(AttachOutgoing::Input(bytes.into()))
    }

    pub fn command(&self, command: StreamCommand) -> Result<()> {
        self.send(AttachOutgoing::Command(command))
    }

    fn send(&self, message: AttachOutgoing) -> Result<()> {
        self.tx
            .send(message)
            .map_err(|_| ApiError::new(ErrorCode::DaemonUnavailable, "attach stream closed"))
    }
}

/// A live attach stream: the initial screen plus channels for output/events
/// and for input/commands.
pub struct AttachStream<Checkpoint = Attached> {
    pub attached: Checkpoint,
    events: Receiver<AttachEvent>,
    sender: AttachSender,
}

/// The far end of an [`AttachStream`]: used by the real client's I/O threads
/// and by fakes in tests.
pub struct AttachPeer {
    pub events: Sender<AttachEvent>,
    pub outgoing: Receiver<AttachOutgoing>,
}

/// A temporary terminal's stream, using the same input/output framing.
pub type TerminalStream = AttachStream<TerminalAttached>;

impl<Checkpoint> AttachStream<Checkpoint> {
    /// A connected stream/peer pair over in-memory channels.
    pub fn pair(attached: Checkpoint) -> (Self, AttachPeer) {
        let (events_tx, events_rx) = mpsc::channel();
        let (out_tx, out_rx) = mpsc::channel();
        (
            Self {
                attached,
                events: events_rx,
                sender: AttachSender { tx: out_tx },
            },
            AttachPeer {
                events: events_tx,
                outgoing: out_rx,
            },
        )
    }

    pub fn sender(&self) -> AttachSender {
        self.sender.clone()
    }

    /// Consume the initial checkpoint while preserving the live event stream.
    pub fn into_events(self) -> AttachStream<()> {
        AttachStream {
            attached: (),
            events: self.events,
            sender: self.sender,
        }
    }

    /// Non-blocking. `None` when nothing is pending; a disconnected stream
    /// yields `Closed` once.
    pub fn try_recv(&self) -> Option<AttachEvent> {
        match self.events.try_recv() {
            Ok(event) => Some(event),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => Some(AttachEvent::Closed(None)),
        }
    }

    pub fn recv_timeout(&self, timeout: Duration) -> Option<AttachEvent> {
        match self.events.recv_timeout(timeout) {
            Ok(event) => Some(event),
            Err(RecvTimeoutError::Timeout) => None,
            Err(RecvTimeoutError::Disconnected) => Some(AttachEvent::Closed(None)),
        }
    }

    /// Ask the daemon to end the stream. Dropping the stream also detaches.
    pub fn detach(&self) {
        let _ = self.sender.command(StreamCommand::Detach);
    }
}

fn connect_attach_stream<Checkpoint>(
    socket: UnixStream,
    attached: Checkpoint,
) -> Result<AttachStream<Checkpoint>> {
    let reader = socket.try_clone().map_err(unavailable_io)?;
    let abort = socket.try_clone().map_err(unavailable_io)?;
    let (stream, AttachPeer { events, outgoing }) = AttachStream::pair(attached);
    let finished = Arc::new(AtomicBool::new(false));
    let reader_finished = finished.clone();
    spawn_thread("rakumux-attach-rx", move || {
        attach_reader(reader, events, reader_finished)
    })
    .inspect_err(|_| {
        let _ = abort.shutdown(Shutdown::Both);
    })?;
    spawn_thread("rakumux-attach-tx", move || {
        attach_writer(socket, outgoing, finished)
    })
    .inspect_err(|_| {
        let _ = abort.shutdown(Shutdown::Both);
    })?;
    Ok(stream)
}

/// A live event subscription.
pub struct EventStream {
    events: Receiver<std::result::Result<EventEnvelope, String>>,
}

impl EventStream {
    /// A stream fed from `rx`; the sender side is the real client's reader
    /// thread or a test fake. `Err` items carry a stream error and end it.
    pub fn from_receiver(rx: Receiver<std::result::Result<EventEnvelope, String>>) -> Self {
        Self { events: rx }
    }

    pub fn try_recv(&self) -> Option<std::result::Result<EventEnvelope, String>> {
        match self.events.try_recv() {
            Ok(item) => Some(item),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => Some(Err("event stream closed".into())),
        }
    }

    pub fn recv_timeout(
        &self,
        timeout: Duration,
    ) -> Option<std::result::Result<EventEnvelope, String>> {
        match self.events.recv_timeout(timeout) {
            Ok(item) => Some(item),
            Err(RecvTimeoutError::Timeout) => None,
            Err(RecvTimeoutError::Disconnected) => Some(Err("event stream closed".into())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::{EventKind, ReadSource, Role, SessionEvent, TextFormat, WaitUntil};
    use crate::model::{Creator, LaunchSpec, Lifecycle, SessionStatus};
    use base64::Engine as _;
    use std::collections::HashMap;
    use std::os::unix::net::UnixListener;
    use std::path::Path;

    fn view() -> SessionView {
        SessionView {
            agent_output: None,
            user_prompt: None,
            agent_turn: None,
            pinned: false,
            git: None,
            auto_named: false,
            description: None,
            notes: None,
            labels: Default::default(),
            private: false,
            id: SessionId::from_string("S1"),
            name: "s".into(),
            scope: ScopeRoot::from_canonical("/r".into()),
            is_self: false,
            cwd: "/r".into(),
            status: SessionStatus::Live,
            exit_code: None,
            lifecycle: Lifecycle::None,
            agent: None,
            reported: None,
            activity: None,
            created_by: Creator::User,
            parent_id: None,
            depth: 0,
            children: vec![],
            launch: LaunchSpec::default(),
            resume: None,
            created_at: 0,
            last_active_at: 0,
            terminal: None,
        }
    }

    fn named(id: &str, name: &str) -> SessionView {
        SessionView {
            pinned: false,
            git: None,
            id: SessionId::from_string(id),
            name: name.into(),
            ..view()
        }
    }

    #[test]
    fn attach_pair_carries_both_directions() {
        let attached = Attached {
            session: view(),
            role: Role::Observer,
            cols: 80,
            rows: 24,
            checkpoint: String::new(),
            offset: 0,
        };
        let (stream, peer) = AttachStream::pair(attached);
        stream.sender().input(b"ls\r".to_vec()).unwrap();
        stream.detach();
        assert_eq!(
            peer.outgoing.recv().unwrap(),
            AttachOutgoing::Input(b"ls\r".to_vec())
        );
        assert_eq!(
            peer.outgoing.recv().unwrap(),
            AttachOutgoing::Command(StreamCommand::Detach)
        );
        peer.events
            .send(AttachEvent::Output {
                offset: 0,
                bytes: b"hi".to_vec(),
            })
            .unwrap();
        assert!(matches!(
            stream.try_recv(),
            Some(AttachEvent::Output { .. })
        ));
        assert_eq!(stream.try_recv(), None);
        drop(peer);
        assert_eq!(stream.try_recv(), Some(AttachEvent::Closed(None)));
    }

    // ------------------------------------------------------------ fake daemon

    const USER_TOKEN: &str = "user-token";
    const SESSION_TOKEN: &str = "session-token";

    fn b64(bytes: &[u8]) -> String {
        base64::engine::general_purpose::STANDARD.encode(bytes)
    }

    /// In-process stand-in for `rakumux-ptyd`: serves the frame protocol on
    /// a Unix socket with canned answers and records what it receives.
    struct FakeDaemon {
        layout: Layout,
        state: Arc<FakeState>,
        _dir: Option<tempfile::TempDir>,
    }

    struct FakeState {
        /// Protocol announced in the hello answer.
        protocol: u32,
        registration_file: PathBuf,
        hellos: Mutex<Vec<Hello>>,
        requests: Mutex<Vec<Request>>,
        /// Commands received on attach streams.
        commands: Mutex<Vec<StreamCommand>>,
        /// Attach streams that ended (detach or disconnect).
        attach_ended: Mutex<usize>,
        stopped: AtomicBool,
    }

    impl FakeDaemon {
        fn start() -> Self {
            Self::start_with_protocol(crate::PROTOCOL_VERSION)
        }

        fn start_with_protocol(protocol: u32) -> Self {
            let dir = tempfile::tempdir().unwrap();
            let layout = Layout::with_home(dir.path().join("home"));
            let socket = dir.path().join("d.sock");
            let mut daemon = Self::serve(layout, &socket, protocol);
            daemon._dir = Some(dir);
            daemon
        }

        /// Bind `socket`, write the registration file and serve until a
        /// `DaemonShutdown`.
        fn serve(layout: Layout, socket: &Path, protocol: u32) -> Self {
            let listener = UnixListener::bind(socket).unwrap();
            listener.set_nonblocking(true).unwrap();
            std::fs::create_dir_all(&layout.run_dir).unwrap();
            let registration = Registration {
                instance_id: "fake".into(),
                pid: std::process::id(),
                protocol: crate::PROTOCOL_VERSION,
                version: "fake".into(),
                socket: socket.to_path_buf(),
                token: USER_TOKEN.into(),
            };
            std::fs::write(
                layout.registration_file(),
                serde_json::to_vec(&registration).unwrap(),
            )
            .unwrap();
            let state = Arc::new(FakeState {
                protocol,
                registration_file: layout.registration_file(),
                hellos: Mutex::new(vec![]),
                requests: Mutex::new(vec![]),
                commands: Mutex::new(vec![]),
                attach_ended: Mutex::new(0),
                stopped: AtomicBool::new(false),
            });
            let accept_state = state.clone();
            std::thread::spawn(move || {
                while !accept_state.stopped.load(Ordering::SeqCst) {
                    match listener.accept() {
                        Ok((stream, _)) => {
                            stream.set_nonblocking(false).unwrap();
                            let state = accept_state.clone();
                            std::thread::spawn(move || serve_connection(stream, state));
                        }
                        Err(_) => std::thread::sleep(Duration::from_millis(5)),
                    }
                }
            });
            Self {
                layout,
                state,
                _dir: None,
            }
        }

        fn config(&self, auth: AuthMode) -> ClientConfig {
            ClientConfig {
                layout: self.layout.clone(),
                client_name: "test".into(),
                version: "0.0.0-test".into(),
                auth,
                spawn: false,
                ptyd_path: None,
            }
        }

        fn connect_user(&self) -> Client {
            Client::connect(self.config(AuthMode::User)).unwrap()
        }

        fn requests(&self) -> Vec<Request> {
            self.state.requests.lock().unwrap().clone()
        }

        fn hello_count(&self) -> usize {
            self.state.hellos.lock().unwrap().len()
        }

        fn wait_stopped(&self, timeout: Duration) -> bool {
            let deadline = Instant::now() + timeout;
            while Instant::now() < deadline {
                if self.state.stopped.load(Ordering::SeqCst) {
                    return true;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            false
        }
    }

    fn scope() -> ScopeRoot {
        ScopeRoot::from_canonical("/scope".into())
    }

    fn serve_connection(mut stream: UnixStream, state: Arc<FakeState>) {
        let Ok(Request::Hello(hello)) = frame::read_control::<_, Request>(&mut stream) else {
            return;
        };
        state.hellos.lock().unwrap().push(hello.clone());
        let caller = match &hello.auth {
            Auth::User { token } if token == USER_TOKEN => CallerInfo::User,
            Auth::Session { id, token } if token == SESSION_TOKEN => CallerInfo::Session {
                id: id.clone(),
                scope: scope(),
            },
            _ => {
                let error = ApiError::new(ErrorCode::Unauthenticated, "bad token");
                let _ = frame::write_control(&mut stream, &Response::Error { error });
                return;
            }
        };
        let ok = HelloOk {
            protocol: state.protocol,
            version: "fake".into(),
            capabilities: vec!["attach".into()],
            caller,
        };
        if frame::write_control(&mut stream, &Response::Hello(ok)).is_err() {
            return;
        }
        loop {
            let Ok(request) = frame::read_control::<_, Request>(&mut stream) else {
                return;
            };
            state.requests.lock().unwrap().push(request.clone());
            let response = match request {
                Request::Ping => {
                    // Answer, then drop the connection (an idle connection
                    // the daemon closed).
                    let _ = frame::write_control(&mut stream, &Response::Pong);
                    return;
                }
                Request::SessionList { include_dead, .. } => {
                    let mut sessions = vec![named("S1", "alpha"), named("S2", "beta")];
                    if include_dead {
                        sessions.push(SessionView {
                            pinned: false,
                            git: None,
                            status: SessionStatus::Dead,
                            ..named("S3", "gamma")
                        });
                    }
                    Response::Sessions { sessions }
                }
                Request::SessionGet { id } if id.as_str() == "missing" => Response::Error {
                    error: ApiError::new(ErrorCode::NotFound, "no such session"),
                },
                Request::SessionGet { id } => Response::Session {
                    session: named(id.as_str(), "got"),
                },
                Request::SessionCreate(create) => Response::Session {
                    session: named("NEW", create.name.as_deref().unwrap_or("auto")),
                },
                Request::SessionRename { id, name } => Response::Session {
                    session: named(id.as_str(), &name),
                },
                Request::SessionKill { id, .. } if id.as_str() == "VANISH" => return,
                Request::SessionKill { id, tree } => Response::Killed {
                    killed: if tree {
                        vec![id, SessionId::from_string("CHILD")]
                    } else {
                        vec![id]
                    },
                },
                Request::SessionTouch { id } if id.as_str() == "WRONG" => Response::Pong,
                Request::SessionDelete { .. }
                | Request::SessionReport(_)
                | Request::SessionSend(_)
                | Request::SessionSubmit { .. }
                | Request::SessionTouch { .. } => Response::Ok,
                Request::SessionRevive(revive) => Response::Session {
                    session: named(revive.id.as_str(), "revived"),
                },
                Request::SessionTurn { id } => Response::Turn {
                    turn: crate::api::TurnContent {
                        id: 7,
                        prompt: Some(format!("prompt of {id}")),
                        messages: vec!["reading".into(), "done".into()],
                        total: 2,
                    },
                },
                Request::SessionTranscript { id, after, .. } => Response::Transcript {
                    transcript: crate::api::TranscriptPage {
                        agent: "codex".into(),
                        conversation_id: format!("chat-of-{id}"),
                        messages: vec![crate::api::TranscriptMessage {
                            id: "m1".into(),
                            position: 0,
                            role: crate::api::TranscriptRole::User,
                            at: None,
                            parts: vec![crate::api::TranscriptPart::Text {
                                text: "go".into(),
                                truncated: false,
                            }],
                        }],
                        cursor: crate::api::TranscriptCursor {
                            conversation: format!("chat-of-{id}"),
                            offset: after.map_or(0, |cursor| cursor.offset) + 10,
                            key: None,
                        },
                        more: false,
                    },
                },
                Request::SessionRead(read) => Response::Read(ReadResult {
                    text: format!("screen of {}", read.id),
                    rows: 24,
                    cols: 80,
                    cursor: (1, 2),
                    truncated: false,
                    untrusted: true,
                }),
                Request::SessionWait(wait) => Response::Wait(WaitResult {
                    matched: true,
                    status: SessionStatus::Live,
                    lifecycle: Lifecycle::Idle,
                    reported: None,
                    reason: format!("{:?} after {} ms", wait.until, wait.timeout_ms),
                }),
                Request::DaemonStatus => Response::Status(DaemonStatusInfo {
                    pid: std::process::id(),
                    version: "fake".into(),
                    protocol: crate::PROTOCOL_VERSION,
                    uptime_secs: 7,
                    sessions: 2,
                    live_terminals: 1,
                    clients: 1,
                    rss_bytes: None,
                }),
                Request::SessionPin { .. }
                | Request::TerminalOpen(_)
                | Request::TerminalAttach(_)
                | Request::SessionBind { .. }
                | Request::ProjectList
                | Request::ProjectAdd { .. }
                | Request::ProjectRemove { .. }
                | Request::ReplyCapture { .. }
                | Request::ReplyList { .. }
                | Request::DaemonRestart { .. } => Response::Error {
                    error: ApiError::new(ErrorCode::Unsupported, "not in the fake daemon"),
                },
                Request::DaemonShutdown => {
                    let _ = frame::write_control(&mut stream, &Response::Ok);
                    let _ = std::fs::remove_file(&state.registration_file);
                    state.stopped.store(true, Ordering::SeqCst);
                    return;
                }
                Request::Attach(attach) => {
                    serve_attach(stream, attach, &state);
                    *state.attach_ended.lock().unwrap() += 1;
                    return;
                }
                Request::Subscribe(_) => return serve_events(stream),
                Request::SessionImport { .. } => Response::Sessions { sessions: vec![] },
                Request::ProjectRelocate { .. } => Response::Ok,
                Request::Hello(_) => Response::Error {
                    error: ApiError::new(ErrorCode::InvalidArgument, "hello twice"),
                },
            };
            if frame::write_control(&mut stream, &response).is_err() {
                return;
            }
        }
    }

    /// Attach stream: "welcome", then echo input as output and answer
    /// commands; session "EXIT" exits right away.
    fn serve_attach(mut stream: UnixStream, attach: AttachRequest, state: &FakeState) {
        let attached = Attached {
            session: named(attach.id.as_str(), "attached"),
            role: attach.role,
            cols: attach.cols,
            rows: attach.rows,
            checkpoint: b64(b"CHECKPOINT"),
            offset: 0,
        };
        frame::write_control(&mut stream, &Response::Attached(attached)).unwrap();
        let mut offset = 0u64;
        frame::write_output(&mut stream, offset, b"welcome").unwrap();
        offset += 7;
        if attach.id.as_str() == "EXIT" {
            let exited = StreamEvent::Exited { code: Some(3) };
            frame::write_control(&mut stream, &exited).unwrap();
            return;
        }
        loop {
            let event = match frame::read_frame(&mut stream) {
                Ok(Some(Frame::Data(bytes))) => {
                    if frame::write_output(&mut stream, offset, &bytes).is_err() {
                        return;
                    }
                    offset += bytes.len() as u64;
                    continue;
                }
                Ok(Some(Frame::Control(payload))) => {
                    let command = frame::parse_control::<StreamCommand>(&payload).unwrap();
                    state.commands.lock().unwrap().push(command.clone());
                    match command {
                        StreamCommand::Checkpoint => StreamEvent::Resized {
                            cols: attach.cols,
                            rows: attach.rows,
                            checkpoint: b64(b"CHECKPOINT"),
                            offset,
                        },
                        StreamCommand::Resize { cols, rows }
                        | StreamCommand::Fit { cols, rows } => StreamEvent::Resized {
                            cols,
                            rows,
                            checkpoint: b64(b"RESIZED"),
                            offset,
                        },
                        StreamCommand::Promote { .. } => StreamEvent::RoleChanged {
                            role: Role::Controller,
                        },
                        StreamCommand::Demote => StreamEvent::RoleChanged {
                            role: Role::Observer,
                        },
                        StreamCommand::Detach => return,
                    }
                }
                _ => return,
            };
            if frame::write_control(&mut stream, &event).is_err() {
                return;
            }
        }
    }

    /// Event stream: two events, then the daemon ends the stream.
    fn serve_events(mut stream: UnixStream) {
        frame::write_control(&mut stream, &Response::Subscribed).unwrap();
        for event in [
            SessionEvent::Created {
                session: named("S1", "alpha"),
            },
            SessionEvent::Activity,
        ] {
            let envelope = EventEnvelope {
                scope: scope(),
                session_id: SessionId::from_string("S1"),
                at: 1,
                event,
            };
            frame::write_control(&mut stream, &envelope).unwrap();
        }
    }

    // ------------------------------------------------------------ connect

    #[test]
    fn auto_auth_prefers_session_variables() {
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            let map: HashMap<&str, &str> = pairs.iter().copied().collect();
            move |name: &str| map.get(name).map(|v| v.to_string())
        };
        let with_session = env(&[
            ("RAKUMUX_SESSION_ID", "S9"),
            ("RAKUMUX_SESSION_TOKEN", "tok"),
            ("RAKUMUX_SOCKET", "/tmp/x.sock"),
        ]);
        assert_eq!(
            resolve_identity(&AuthMode::Auto, &with_session),
            Identity::Session {
                id: SessionId::from_string("S9"),
                token: "tok".into(),
                socket: Some("/tmp/x.sock".into()),
            }
        );
        // An id without a token (or empty values) falls back to user auth.
        let partial = env(&[("RAKUMUX_SESSION_ID", "S9")]);
        assert_eq!(resolve_identity(&AuthMode::Auto, &partial), Identity::User);
        let empty = env(&[("RAKUMUX_SESSION_ID", ""), ("RAKUMUX_SESSION_TOKEN", "")]);
        assert_eq!(resolve_identity(&AuthMode::Auto, &empty), Identity::User);
        // Explicit modes ignore the session variables.
        assert_eq!(
            resolve_identity(&AuthMode::User, &with_session),
            Identity::User
        );
        let explicit = AuthMode::Session {
            id: SessionId::from_string("S1"),
            token: "t".into(),
        };
        assert_eq!(
            resolve_identity(&explicit, &env(&[])),
            Identity::Session {
                id: SessionId::from_string("S1"),
                token: "t".into(),
                socket: None,
            }
        );
    }

    #[test]
    fn user_hello_uses_registration_token() {
        let daemon = FakeDaemon::start();
        let client = daemon.connect_user();
        assert_eq!(client.caller(), &CallerInfo::User);
        assert_eq!(client.capabilities(), ["attach".to_string()]);
        assert_eq!(client.daemon_version(), "fake");
        let hellos = daemon.state.hellos.lock().unwrap().clone();
        assert_eq!(
            hellos,
            vec![Hello {
                max_blocking_ms: None,
                protocol: crate::PROTOCOL_VERSION,
                client: "test".into(),
                version: "0.0.0-test".into(),
                auth: Auth::User {
                    token: USER_TOKEN.into()
                },
            }]
        );
    }

    #[test]
    fn session_hello_identifies_the_session() {
        let daemon = FakeDaemon::start();
        let client = Client::connect(daemon.config(AuthMode::Session {
            id: SessionId::from_string("S1"),
            token: SESSION_TOKEN.into(),
        }))
        .unwrap();
        assert_eq!(
            client.caller(),
            &CallerInfo::Session {
                id: SessionId::from_string("S1"),
                scope: scope(),
            }
        );
    }

    #[test]
    fn bad_token_is_unauthenticated() {
        let daemon = FakeDaemon::start();
        let error = Client::connect(daemon.config(AuthMode::Session {
            id: SessionId::from_string("S1"),
            token: "wrong".into(),
        }))
        .err()
        .unwrap();
        assert_eq!(error.code, ErrorCode::Unauthenticated);
    }

    #[test]
    fn protocol_mismatch_is_reported() {
        let daemon = FakeDaemon::start_with_protocol(crate::PROTOCOL_VERSION + 1);
        let error = Client::connect(daemon.config(AuthMode::User))
            .err()
            .unwrap();
        assert_eq!(error.code, ErrorCode::ProtocolMismatch);
    }

    #[test]
    fn missing_daemon_is_unavailable_without_spawn() {
        let dir = tempfile::tempdir().unwrap();
        let config = ClientConfig {
            layout: Layout::with_home(dir.path().to_path_buf()),
            client_name: "test".into(),
            version: "0".into(),
            auth: AuthMode::User,
            spawn: false,
            ptyd_path: None,
        };
        let error = Client::connect(config.clone()).err().unwrap();
        assert_eq!(error.code, ErrorCode::DaemonUnavailable);
        assert!(error.message.contains("not running"), "{}", error.message);

        // A stale registration whose socket is gone is "not running" too.
        let registration = Registration {
            instance_id: "old".into(),
            pid: 1,
            protocol: 1,
            version: "old".into(),
            socket: dir.path().join("gone.sock"),
            token: "t".into(),
        };
        std::fs::create_dir_all(&config.layout.run_dir).unwrap();
        std::fs::write(
            config.layout.registration_file(),
            serde_json::to_vec(&registration).unwrap(),
        )
        .unwrap();
        let error = Client::connect(config).err().unwrap();
        assert_eq!(error.code, ErrorCode::DaemonUnavailable);
    }

    #[test]
    fn spawn_reports_missing_binary() {
        let dir = tempfile::tempdir().unwrap();
        let config = ClientConfig {
            layout: Layout::with_home(dir.path().to_path_buf()),
            client_name: "test".into(),
            version: "0".into(),
            auth: AuthMode::User,
            spawn: true,
            ptyd_path: Some(dir.path().join("no-such-ptyd")),
        };
        let error = Client::connect(config).err().unwrap();
        assert_eq!(error.code, ErrorCode::DaemonUnavailable);
        assert!(error.message.contains("not found"), "{}", error.message);
    }

    // ------------------------------------------------------------ requests

    #[test]
    fn closed_sockets_keep_buffered_responses_when_timeouts_change() {
        let (mut client, mut server) = UnixStream::pair().unwrap();
        set_read_timeout(&client, Some(REQUEST_TIMEOUT)).unwrap();
        frame::write_control(&mut server, &Response::Pong).unwrap();
        server.shutdown(Shutdown::Both).unwrap();
        drop(server);

        set_read_timeout(&client, None).unwrap();
        assert_eq!(
            frame::read_control::<_, Response>(&mut client).unwrap(),
            Response::Pong
        );
        assert_eq!(frame::read_frame(&mut client).unwrap(), None);
        set_read_timeout(&client, Some(REQUEST_TIMEOUT)).unwrap();
        assert!(matches!(
            exchange(&mut client, &Request::Ping, REQUEST_TIMEOUT),
            Err(ExchangeError::NotSent(_))
        ));
    }

    #[test]
    fn every_method_maps_to_its_request() {
        let daemon = FakeDaemon::start();
        let client = daemon.connect_user();
        let s1 = SessionId::from_string("S1");

        let sessions = client.list(Some(&scope()), false).unwrap();
        assert_eq!(sessions.len(), 2);
        assert_eq!(client.list(None, true).unwrap().len(), 3);
        assert_eq!(client.get(&s1).unwrap().name, "got");
        let create = CreateSession {
            scope: Some(scope()),
            name: Some("web".into()),
            cwd: Some("/scope/web".into()),
            argv: None,
            startup_cmds: vec!["npm run dev".into()],
            env: vec![("A".into(), "1".into())],
            cols: 100,
            rows: 30,
        };
        assert_eq!(client.create(create.clone()).unwrap().name, "web");
        assert_eq!(client.rename(&s1, "renamed").unwrap().name, "renamed");
        assert_eq!(
            client.kill(&s1, true).unwrap(),
            vec![s1.clone(), SessionId::from_string("CHILD")]
        );
        client.delete(&s1).unwrap();
        let revive = ReviveSession {
            id: s1.clone(),
            env: vec![],
            cols: 80,
            rows: 24,
            resume: true,
        };
        assert_eq!(client.revive(revive.clone()).unwrap().name, "revived");
        let report = Report {
            state: Some(Lifecycle::Done),
            text: Some("3 findings".into()),
            ..Report::default()
        };
        client.report(report.clone()).unwrap();
        let read = ReadRequest {
            id: s1.clone(),
            source: ReadSource::Recent,
            lines: Some(10),
            format: TextFormat::Text,
        };
        assert_eq!(client.read(read.clone()).unwrap().text, "screen of S1");
        let turn = client.turn(&s1).unwrap();
        assert_eq!(
            (turn.id, turn.prompt.as_deref(), turn.total),
            (7, Some("prompt of S1"), 2)
        );
        let chat = client.transcript(&s1, None, Some(4096)).unwrap();
        assert_eq!(chat.conversation_id, "chat-of-S1");
        assert_eq!(chat.messages.len(), 1);
        let more = client.transcript(&s1, Some(&chat.cursor), None).unwrap();
        assert_eq!(more.cursor.offset, 20);
        assert_eq!(turn.messages, ["reading", "done"]);
        let wait = WaitRequest {
            id: s1.clone(),
            until: WaitUntil::Settled,
            pattern: None,
            timeout_ms: 1000,
        };
        assert!(client.wait(wait.clone()).unwrap().matched);
        let send = SendRequest {
            id: s1.clone(),
            keys: vec!["ctrl+c".into()],
            text: None,
        };
        client.send(send.clone()).unwrap();
        client.submit(&s1, "echo hi").unwrap();
        client.touch(&s1).unwrap();
        assert_eq!(client.status().unwrap().uptime_secs, 7);

        assert_eq!(
            daemon.requests(),
            vec![
                Request::SessionList {
                    scopes: Vec::new(),
                    scope: Some(scope()),
                    include_dead: false,
                    all: false,
                },
                Request::SessionList {
                    scopes: Vec::new(),
                    scope: None,
                    include_dead: true,
                    all: false,
                },
                Request::SessionGet { id: s1.clone() },
                Request::SessionCreate(create),
                Request::SessionRename {
                    id: s1.clone(),
                    name: "renamed".into()
                },
                Request::SessionKill {
                    id: s1.clone(),
                    tree: true
                },
                Request::SessionDelete { id: s1.clone() },
                Request::SessionRevive(revive),
                Request::SessionReport(report),
                Request::SessionRead(read),
                Request::SessionTurn { id: s1.clone() },
                Request::SessionTranscript {
                    id: s1.clone(),
                    after: None,
                    max_bytes: Some(4096)
                },
                Request::SessionTranscript {
                    id: s1.clone(),
                    after: Some(chat.cursor.clone()),
                    max_bytes: None
                },
                Request::SessionWait(wait),
                Request::SessionSend(send),
                Request::SessionSubmit {
                    id: s1.clone(),
                    text: "echo hi".into()
                },
                Request::SessionTouch { id: s1.clone() },
                Request::DaemonStatus,
            ]
        );
        // One shared connection plus a dedicated one for the wait.
        assert_eq!(daemon.hello_count(), 2);
    }

    #[test]
    fn errors_and_unexpected_responses_are_mapped() {
        let daemon = FakeDaemon::start();
        let client = daemon.connect_user();
        let error = client.get(&SessionId::from_string("missing")).unwrap_err();
        assert_eq!(error.code, ErrorCode::NotFound);
        assert_eq!(error.message, "no such session");
        let error = client.touch(&SessionId::from_string("WRONG")).unwrap_err();
        assert_eq!(error.code, ErrorCode::Internal);
        assert!(error.message.contains("pong"), "{}", error.message);
        // The connection is still usable after an error answer.
        assert_eq!(client.list(None, false).unwrap().len(), 2);
        assert_eq!(daemon.hello_count(), 1);
    }

    #[test]
    fn reconnects_once_after_the_daemon_closed_the_connection() {
        let daemon = FakeDaemon::start();
        let client = daemon.connect_user();
        // The fake answers a ping and then closes the connection.
        assert_eq!(client.request(&Request::Ping).unwrap(), Response::Pong);
        std::thread::sleep(Duration::from_millis(50));
        // A non-idempotent request is resent because it was never written.
        let create = CreateSession {
            scope: Some(scope()),
            name: Some("again".into()),
            cwd: None,
            argv: None,
            startup_cmds: vec![],
            env: vec![],
            cols: 80,
            rows: 24,
        };
        assert_eq!(client.create(create).unwrap().name, "again");
        assert_eq!(daemon.hello_count(), 2);
    }

    #[test]
    fn lost_answers_to_mutations_are_not_resent() {
        let daemon = FakeDaemon::start();
        let client = daemon.connect_user();
        let error = client
            .kill(&SessionId::from_string("VANISH"), false)
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::DaemonUnavailable);
        let kills = daemon
            .requests()
            .into_iter()
            .filter(|r| matches!(r, Request::SessionKill { .. }))
            .count();
        assert_eq!(kills, 1);
        // The next request opens a fresh connection.
        assert_eq!(client.list(None, false).unwrap().len(), 2);
    }

    #[test]
    fn shutdown_stops_the_daemon() {
        let daemon = FakeDaemon::start();
        let client = daemon.connect_user();
        client.shutdown().unwrap();
        assert!(daemon.wait_stopped(Duration::from_secs(2)));
        assert!(!daemon.layout.registration_file().exists());
        let error = client.status().unwrap_err();
        assert_eq!(error.code, ErrorCode::DaemonUnavailable);
    }

    // ------------------------------------------------------------ streams

    fn recv(stream: &AttachStream) -> AttachEvent {
        stream
            .recv_timeout(Duration::from_secs(5))
            .expect("attach event")
    }

    #[test]
    fn attach_stream_carries_output_input_and_commands() {
        let daemon = FakeDaemon::start();
        let client = daemon.connect_user();
        let stream = client
            .attach(AttachRequest {
                id: SessionId::from_string("S1"),
                role: Role::Controller,
                cols: 100,
                rows: 30,
            })
            .unwrap();
        assert_eq!(stream.attached.role, Role::Controller);
        assert_eq!((stream.attached.cols, stream.attached.rows), (100, 30));
        assert_eq!(stream.attached.checkpoint, b64(b"CHECKPOINT"));
        assert_eq!(
            recv(&stream),
            AttachEvent::Output {
                offset: 0,
                bytes: b"welcome".to_vec()
            }
        );

        let sender = stream.sender();
        sender.input(b"ls\r".to_vec()).unwrap();
        assert_eq!(
            recv(&stream),
            AttachEvent::Output {
                offset: 7,
                bytes: b"ls\r".to_vec()
            }
        );
        // Large input is split into several frames but arrives intact.
        let big = vec![b'x'; INPUT_CHUNK + 10];
        sender.input(big.clone()).unwrap();
        let mut echoed = Vec::new();
        while echoed.len() < big.len() {
            match recv(&stream) {
                AttachEvent::Output { bytes, .. } => echoed.extend(bytes),
                other => panic!("{other:?}"),
            }
        }
        assert_eq!(echoed, big);

        sender
            .command(StreamCommand::Resize { cols: 90, rows: 20 })
            .unwrap();
        match recv(&stream) {
            AttachEvent::Event(StreamEvent::Resized { cols, rows, .. }) => {
                assert_eq!((cols, rows), (90, 20))
            }
            other => panic!("{other:?}"),
        }
        sender
            .command(StreamCommand::Promote { cols: 90, rows: 20 })
            .unwrap();
        assert_eq!(
            recv(&stream),
            AttachEvent::Event(StreamEvent::RoleChanged {
                role: Role::Controller
            })
        );

        stream.detach();
        assert_eq!(recv(&stream), AttachEvent::Closed(None));
        // Requests on the shared connection still work.
        assert_eq!(client.list(None, false).unwrap().len(), 2);
    }

    #[test]
    fn attach_reports_exit_then_closes() {
        let daemon = FakeDaemon::start();
        let client = daemon.connect_user();
        let stream = client
            .attach(AttachRequest {
                id: SessionId::from_string("EXIT"),
                role: Role::Observer,
                cols: 80,
                rows: 24,
            })
            .unwrap();
        assert!(matches!(recv(&stream), AttachEvent::Output { .. }));
        assert_eq!(
            recv(&stream),
            AttachEvent::Event(StreamEvent::Exited { code: Some(3) })
        );
        assert_eq!(recv(&stream), AttachEvent::Closed(None));
    }

    #[test]
    fn dropping_the_attach_stream_detaches() {
        let daemon = FakeDaemon::start();
        let client = daemon.connect_user();
        let stream = client
            .attach(AttachRequest {
                id: SessionId::from_string("S1"),
                role: Role::Observer,
                cols: 80,
                rows: 24,
            })
            .unwrap();
        drop(stream);
        // Either the writer sends Detach or the reader closes the socket;
        // both end the stream on the daemon side.
        let deadline = Instant::now() + Duration::from_secs(2);
        while *daemon.state.attach_ended.lock().unwrap() == 0 {
            assert!(Instant::now() < deadline, "stream still open after drop");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn event_stream_delivers_envelopes_until_closed() {
        let daemon = FakeDaemon::start();
        let client = daemon.connect_user();
        let events = client
            .subscribe(SubscribeRequest {
                scopes: Vec::new(),
                scope: Some(scope()),
                session_ids: vec![],
                kinds: vec![EventKind::Created, EventKind::Activity],
                all: false,
                replies: false,
            })
            .unwrap();
        let first = events
            .recv_timeout(Duration::from_secs(5))
            .unwrap()
            .unwrap();
        assert!(matches!(first.event, SessionEvent::Created { .. }));
        let second = events
            .recv_timeout(Duration::from_secs(5))
            .unwrap()
            .unwrap();
        assert_eq!(second.event, SessionEvent::Activity);
        let end = events.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(end.unwrap_err().contains("closed"));
        assert!(matches!(
            daemon.requests().last(),
            Some(Request::Subscribe(SubscribeRequest { kinds, .. })) if kinds.len() == 2
        ));
    }

    // ------------------------------------------------------------ spawning

    const SPAWN_SLEEP_ENV: &str = "SESSION_CORE_TEST_SPAWN_SLEEP";
    const FAKE_PTYD_MARKER: &str = "fake-ptyd";

    /// Helper process for `spawn_helper_detaches_stdio`: spawns `sleep 5`
    /// through `spawn_detached` and prints its pid. Does nothing unless the
    /// parent test set the environment variable.
    #[test]
    #[ignore = "helper process, run by spawn_helper_detaches_stdio"]
    fn spawn_sleep_helper() {
        if std::env::var_os(SPAWN_SLEEP_ENV).is_none() {
            return;
        }
        let mut command = Command::new("sleep");
        command.arg("5");
        let pid = spawn_detached(command).unwrap();
        println!("spawned-pid={pid}");
    }

    #[test]
    fn spawn_helper_detaches_stdio() {
        let exe = std::env::current_exe().unwrap();
        let started = Instant::now();
        let output = Command::new("bash")
            .arg("-c")
            .arg(r#"out=$("$0" --exact client::tests::spawn_sleep_helper --ignored --nocapture); printf '%s\n' "$out""#)
            .arg(&exe)
            .env(SPAWN_SLEEP_ENV, "1")
            .output()
            .unwrap();
        let elapsed = started.elapsed();
        let stdout = String::from_utf8_lossy(&output.stdout);
        let pid: i32 = stdout
            .lines()
            .find_map(|line| line.trim().strip_prefix("spawned-pid="))
            .unwrap_or_else(|| panic!("no pid in {stdout:?}"))
            .parse()
            .unwrap();
        #[cfg(target_os = "linux")]
        {
            // Own session, stdio on /dev/null.
            let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap();
            let fields: Vec<&str> = stat
                .rsplit(')')
                .next()
                .unwrap()
                .split_whitespace()
                .collect();
            assert_eq!(fields[3], pid.to_string(), "session id of {stat}");
            for fd in 0..3 {
                let target = std::fs::read_link(format!("/proc/{pid}/fd/{fd}")).unwrap();
                assert_eq!(target, Path::new("/dev/null"));
            }
        }
        unsafe {
            libc::kill(pid, libc::SIGTERM);
        }
        assert!(
            elapsed < Duration::from_millis(2500),
            "command substitution waited {elapsed:?}"
        );
    }

    /// Helper process for `connect_spawns_the_daemon`: started (through a
    /// shell script) as `<ptyd> serve`; serves the fake daemon until shut
    /// down. Does nothing unless the parent test left its marker file.
    #[test]
    #[ignore = "helper process, run by connect_spawns_the_daemon"]
    fn fake_ptyd_helper() {
        let Some(home) = std::env::var_os(crate::env::HOME).map(PathBuf::from) else {
            return;
        };
        if !home.join(FAKE_PTYD_MARKER).exists() {
            return;
        }
        let daemon = FakeDaemon::serve(
            Layout::with_home(home.clone()),
            &home.join("d.sock"),
            crate::PROTOCOL_VERSION,
        );
        daemon.wait_stopped(Duration::from_secs(30));
    }

    #[test]
    fn connect_spawns_the_daemon() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(home.join(FAKE_PTYD_MARKER), b"").unwrap();
        let exe = std::env::current_exe().unwrap();
        let script = dir.path().join("rakumux-ptyd");
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\n[ \"$1\" = serve ] || exit 64\nexec '{}' --exact client::tests::fake_ptyd_helper --ignored\n",
                exe.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

        let config = ClientConfig {
            layout: Layout::with_home(home.clone()),
            client_name: "test".into(),
            version: "0".into(),
            auth: AuthMode::User,
            spawn: true,
            ptyd_path: Some(script),
        };
        let client = Client::connect(config).unwrap();
        let status = client.status().unwrap();
        assert_ne!(status.pid, std::process::id());
        client.shutdown().unwrap();
    }
}
