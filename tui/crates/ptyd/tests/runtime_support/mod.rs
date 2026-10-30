//! Shared helpers for the daemon runtime integration tests: a temp
//! `RAKUMUX_HOME`, real `bash --norc --noprofile` terminals, a fake
//! `Control`, and a socket client that mirrors attach streams.
#![allow(dead_code)]

use base64::Engine;
use ptyd::control_api::{Caller, Control};
use ptyd::host::{SpawnSpec, TerminalHost, TerminalInfo, TerminalState};
use ptyd::registration;
use ptyd::server::{Server, ServerHandle};
use ptyd::terminal::{Host, HostConfig};
use session_core::api::{
    ApiError, AttachRequest, Attached, Auth, CallerInfo, ErrorCode, EventEnvelope, Hello,
    ReadSource, Request, Response, Role, StreamCommand, StreamEvent, SubscribeRequest, TextFormat,
};
use session_core::frame::{self, Frame};
use session_core::ids::{SessionId, TerminalId, Token};
use session_core::model::{
    Creator, LaunchSpec, Lifecycle, ScopeRoot, SessionStatus, SessionView, TerminalView,
};
use session_core::paths::Layout;
use session_core::PROTOCOL_VERSION;
use std::collections::HashMap;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub const TIMEOUT: Duration = Duration::from_secs(10);

/// Poll `check` until it returns true or `timeout` passes.
pub fn wait_until(timeout: Duration, mut check: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if check() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// A temporary Rakumux home plus a working directory for sessions.
pub struct TestHome {
    pub dir: tempfile::TempDir,
    pub layout: Layout,
    pub work: PathBuf,
}

impl TestHome {
    pub fn new() -> Self {
        let dir = tempfile::tempdir().expect("temp dir");
        let layout = Layout::with_home(dir.path().join("rakumux"));
        let work = dir.path().join("work");
        std::fs::create_dir_all(&work).expect("work dir");
        Self { dir, layout, work }
    }

    pub fn host(&self) -> Arc<Host> {
        self.host_with(|_| {})
    }

    /// A host with an adjusted configuration.
    pub fn host_with(&self, adjust: impl FnOnce(&mut HostConfig)) -> Arc<Host> {
        let mut config = HostConfig::new(self.layout.clone());
        adjust(&mut config);
        Arc::new(Host::new(config))
    }

    /// Environment for test shells: no rc files, no history in the real home.
    pub fn env(&self) -> Vec<(String, String)> {
        let path = std::env::var("PATH").unwrap_or_else(|_| "/usr/bin:/bin".into());
        vec![
            ("PATH".into(), path),
            ("HOME".into(), self.dir.path().display().to_string()),
            (
                "HISTFILE".into(),
                self.dir.path().join("history").display().to_string(),
            ),
            ("TERM".into(), "xterm-256color".into()),
            ("PS1".into(), "$ ".into()),
            ("LANG".into(), "C.UTF-8".into()),
        ]
    }

    pub fn bash_spec(&self, cols: u16, rows: u16, startup_input: &[u8]) -> SpawnSpec {
        self.spec(bash_argv(), cols, rows, startup_input)
    }

    pub fn spec(&self, argv: Vec<String>, cols: u16, rows: u16, startup_input: &[u8]) -> SpawnSpec {
        SpawnSpec {
            session_id: SessionId::generate(),
            argv,
            cwd: self.work.clone(),
            env: self.env(),
            cols,
            rows,
            startup_input: startup_input.to_vec(),
        }
    }
}

pub fn bash_argv() -> Vec<String> {
    vec!["bash".into(), "--norc".into(), "--noprofile".into()]
}

pub fn visible_text(host: &dyn TerminalHost, id: TerminalId) -> String {
    host.read(id, ReadSource::Visible, None, TextFormat::Text)
        .map(|r| r.text)
        .unwrap_or_default()
}

/// Wait until the visible screen contains `needle`; returns the screen.
pub fn wait_for_screen(host: &dyn TerminalHost, id: TerminalId, needle: &str) -> String {
    let mut screen = String::new();
    let found = wait_until(TIMEOUT, || {
        screen = visible_text(host, id);
        screen.contains(needle)
    });
    assert!(found, "screen never showed {needle:?}; screen:\n{screen}");
    screen
}

/// Wait for the shell prompt.
pub fn wait_for_prompt(host: &dyn TerminalHost, id: TerminalId) {
    wait_for_screen(host, id, "$");
}

pub fn wait_for_state(host: &dyn TerminalHost, id: TerminalId, timeout: Duration) -> TerminalState {
    let mut state = TerminalState::Running;
    wait_until(timeout, || {
        state = host
            .info(id)
            .map(|i| i.state)
            .unwrap_or(TerminalState::Failed {
                message: "gone".into(),
            });
        state != TerminalState::Running
    });
    state
}

pub fn pid_alive(pid: u32) -> bool {
    // SAFETY: signal 0 only checks for existence.
    unsafe { libc::kill(pid as i32, 0) == 0 }
}

// ---------------------------------------------------------------- fake control

/// A minimal `Control`: user-token auth and a few ops mapped straight onto
/// the host. Sessions live in memory.
pub struct FakeControl {
    host: Arc<dyn TerminalHost>,
    token: Token,
    work: PathBuf,
    env: Vec<(String, String)>,
    sessions: Mutex<HashMap<SessionId, TerminalId>>,
    subscribers: Mutex<Vec<Sender<EventEnvelope>>>,
    pub shutdown_called: AtomicBool,
}

impl FakeControl {
    pub fn new(host: Arc<dyn TerminalHost>, token: Token, home: &TestHome) -> Self {
        Self {
            host,
            token,
            work: home.work.clone(),
            env: home.env(),
            sessions: Mutex::new(HashMap::new()),
            subscribers: Mutex::new(Vec::new()),
            shutdown_called: AtomicBool::new(false),
        }
    }

    pub fn terminal(&self, id: &SessionId) -> Option<TerminalId> {
        self.sessions.lock().unwrap().get(id).copied()
    }

    pub fn publish(&self, envelope: EventEnvelope) {
        self.subscribers
            .lock()
            .unwrap()
            .retain(|tx| tx.send(envelope.clone()).is_ok());
    }

    fn view(&self, id: &SessionId, info: Option<TerminalInfo>) -> SessionView {
        let status = match info.as_ref().map(|i| &i.state) {
            Some(TerminalState::Running) => SessionStatus::Live,
            Some(_) => SessionStatus::Exited,
            None => SessionStatus::Dead,
        };
        SessionView {
            agent_output: None,
            user_prompt: None,
            agent_turn: None,
            places: vec![],
            pinned: false,
            git: None,
            auto_named: false,
            description: None,
            notes: None,
            labels: Default::default(),
            private: false,
            id: id.clone(),
            name: format!("s-{}", &id.as_str()[20..]),
            scope: ScopeRoot::from_canonical(self.work.clone()),
            is_self: false,
            cwd: self.work.clone(),
            status,
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
            last_message_at: None,
            terminal: info.map(|i| TerminalView {
                pid: i.pid,
                foreground_process: None,
                title: i.title,
                cols: i.cols,
                rows: i.rows,
                controlled: i.controlled,
                alternate_screen: i.alternate_screen,
            }),
            machine: None,
        }
    }

    fn lookup(&self, id: &SessionId) -> Result<TerminalId, ApiError> {
        self.terminal(id)
            .ok_or_else(|| ApiError::new(ErrorCode::NotFound, "no such session"))
    }
}

fn error(code: ErrorCode, message: impl Into<String>) -> Response {
    Response::Error {
        error: ApiError::new(code, message),
    }
}

fn host_failure(e: ptyd::host::HostError) -> Response {
    error(ErrorCode::Internal, e.to_string())
}

impl Control for FakeControl {
    fn authenticate(&self, hello: &Hello) -> Result<Caller, ApiError> {
        if hello.protocol != PROTOCOL_VERSION {
            return Err(ApiError::new(ErrorCode::ProtocolMismatch, "protocol"));
        }
        match &hello.auth {
            Auth::User { token } if self.token.matches(token) => Ok(Caller {
                max_blocking_ms: None,
                info: CallerInfo::User,
                client: hello.client.clone(),
            }),
            _ => Err(ApiError::new(ErrorCode::Unauthenticated, "bad token")),
        }
    }

    fn handle(&self, _caller: &Caller, request: Request) -> Response {
        match request {
            Request::Ping => Response::Pong,
            Request::SessionCreate(create) => {
                let id = SessionId::generate();
                let startup: String = create
                    .startup_cmds
                    .iter()
                    .map(|c| format!("{c}\r"))
                    .collect();
                let spec = SpawnSpec {
                    session_id: id.clone(),
                    argv: create.argv.unwrap_or_else(bash_argv),
                    cwd: create.cwd.unwrap_or_else(|| self.work.clone()),
                    env: if create.env.is_empty() {
                        self.env.clone()
                    } else {
                        create.env
                    },
                    cols: create.cols,
                    rows: create.rows,
                    startup_input: startup.into_bytes(),
                };
                match self.host.spawn(spec) {
                    Ok(info) => {
                        self.sessions.lock().unwrap().insert(id.clone(), info.id);
                        Response::Session {
                            session: self.view(&id, Some(info)),
                        }
                    }
                    Err(e) => host_failure(e),
                }
            }
            Request::SessionGet { id } => match self.lookup(&id) {
                Ok(tid) => Response::Session {
                    session: self.view(&id, self.host.info(tid)),
                },
                Err(error) => Response::Error { error },
            },
            Request::SessionSubmit { id, text, .. } => match self.lookup(&id) {
                Ok(tid) => match self.host.submit(tid, &text) {
                    Ok(()) => Response::Ok,
                    Err(e) => host_failure(e),
                },
                Err(error) => Response::Error { error },
            },
            Request::SessionSend(send) => match self.lookup(&send.id) {
                Ok(tid) => match self.host.send_keys(tid, &send.keys, send.text.as_deref()) {
                    Ok(()) => Response::Ok,
                    Err(e) => host_failure(e),
                },
                Err(error) => Response::Error { error },
            },
            Request::SessionRead(read) => match self.lookup(&read.id) {
                Ok(tid) => match self.host.read(tid, read.source, read.lines, read.format) {
                    Ok(result) => Response::Read(result),
                    Err(e) => host_failure(e),
                },
                Err(error) => Response::Error { error },
            },
            Request::SessionKill { id, .. } => match self.lookup(&id) {
                Ok(tid) => match self.host.terminate(tid) {
                    Ok(()) => Response::Killed { killed: vec![id] },
                    Err(e) => host_failure(e),
                },
                Err(error) => Response::Error { error },
            },
            _ => error(ErrorCode::Unsupported, "not in the fake"),
        }
    }

    fn prepare_attach(
        &self,
        _caller: &Caller,
        request: &AttachRequest,
    ) -> Result<(TerminalId, SessionView), ApiError> {
        let tid = self.lookup(&request.id)?;
        Ok((tid, self.view(&request.id, self.host.info(tid))))
    }

    fn subscribe(
        &self,
        _caller: &Caller,
        _request: &SubscribeRequest,
    ) -> Result<Receiver<EventEnvelope>, ApiError> {
        let (tx, rx) = mpsc::channel();
        self.subscribers.lock().unwrap().push(tx);
        Ok(rx)
    }

    fn session_count(&self) -> u32 {
        self.sessions.lock().unwrap().len() as u32
    }

    fn shutdown(&self) {
        self.shutdown_called.store(true, Ordering::SeqCst);
    }
}

// ---------------------------------------------------------------- daemon

/// A server on a real socket in a temp home, with the real `Host` and the
/// fake control.
pub struct Daemon {
    pub home: TestHome,
    pub socket: PathBuf,
    pub token: String,
    pub host: Arc<Host>,
    pub control: Arc<FakeControl>,
    pub handle: ServerHandle,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Daemon {
    pub fn start() -> Self {
        let home = TestHome::new();
        let claim = registration::claim(&home.layout, "test").expect("claim");
        let socket = claim.registration.socket.clone();
        let token = claim.user_token.as_str().to_string();
        let host = home.host();
        let control = Arc::new(FakeControl::new(
            host.clone(),
            claim.user_token.clone(),
            &home,
        ));
        let server = Server::new(claim, host.clone(), control.clone());
        let handle = server.handle();
        let thread = std::thread::spawn(move || {
            server.serve();
        });
        Self {
            home,
            socket,
            token,
            host,
            control,
            handle,
            thread: Some(thread),
        }
    }

    /// An authenticated user client.
    pub fn client(&self) -> TestClient {
        let mut client = TestClient::connect(&self.socket);
        match client.hello(&self.token) {
            Response::Hello(_) => client,
            other => panic!("hello failed: {other:?}"),
        }
    }

    /// Stop the server and wait for `serve` to return.
    pub fn stop(&mut self) -> bool {
        self.handle.shutdown();
        let Some(thread) = self.thread.take() else {
            return true;
        };
        wait_until(TIMEOUT, || thread.is_finished()) && thread.join().is_ok()
    }

    pub fn is_serving(&self) -> bool {
        self.thread.as_ref().is_some_and(|t| !t.is_finished())
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        self.stop();
    }
}

// ---------------------------------------------------------------- clients

pub struct TestClient {
    pub stream: UnixStream,
}

impl TestClient {
    pub fn connect(socket: &Path) -> Self {
        let stream = UnixStream::connect(socket).expect("connect");
        stream.set_read_timeout(Some(TIMEOUT)).unwrap();
        Self { stream }
    }

    pub fn hello(&mut self, token: &str) -> Response {
        self.request(&Request::Hello(Hello {
            max_blocking_ms: None,
            protocol: PROTOCOL_VERSION,
            client: "test".into(),
            version: "0".into(),
            auth: Auth::User {
                token: token.into(),
            },
        }))
    }

    pub fn request(&mut self, request: &Request) -> Response {
        frame::write_control(&mut self.stream, request).expect("send request");
        frame::read_control(&mut self.stream).expect("read response")
    }

    /// The connection was closed by the daemon.
    pub fn is_closed(&mut self) -> bool {
        matches!(frame::read_frame(&mut self.stream), Ok(None) | Err(_))
    }

    pub fn create(&mut self, cols: u16, rows: u16, startup_cmds: &[&str]) -> SessionView {
        let response = self.request(&Request::SessionCreate(session_core::api::CreateSession {
            scope: None,
            name: None,
            cwd: None,
            argv: None,
            startup_cmds: startup_cmds.iter().map(|s| s.to_string()).collect(),
            env: vec![],
            cols,
            rows,
            machine: None,
            prefill: None,
        }));
        match response {
            Response::Session { session } => session,
            other => panic!("create failed: {other:?}"),
        }
    }

    pub fn submit(&mut self, id: &SessionId, text: &str) {
        let response = self.request(&Request::SessionSubmit {
            id: id.clone(),
            text: text.into(),
            quiet_ms: None,
            expect_foreground: None,
            refuse_if_blocked: false,
        });
        assert_eq!(response, Response::Ok, "submit {text:?}");
    }

    pub fn read_visible(&mut self, id: &SessionId) -> String {
        match self.request(&Request::SessionRead(session_core::api::ReadRequest {
            id: id.clone(),
            source: ReadSource::Visible,
            lines: None,
            format: TextFormat::Text,
        })) {
            Response::Read(result) => result.text,
            other => panic!("read failed: {other:?}"),
        }
    }

    pub fn wait_for_text(&mut self, id: &SessionId, needle: &str) -> String {
        let mut screen = String::new();
        let found = wait_until(TIMEOUT, || {
            screen = self.read_visible(id);
            screen.contains(needle)
        });
        assert!(found, "never saw {needle:?}; screen:\n{screen}");
        screen
    }

    /// Turn this connection into an attach stream.
    pub fn attach(mut self, id: &SessionId, role: Role, cols: u16, rows: u16) -> AttachClient {
        let response = self.request(&Request::Attach(AttachRequest {
            id: id.clone(),
            role,
            cols,
            rows,
        }));
        let attached = match response {
            Response::Attached(attached) => attached,
            other => panic!("attach failed: {other:?}"),
        };
        AttachClient::new(self.stream, attached)
    }
}

/// Something received on an attach stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Received {
    Output { offset: u64, bytes: Vec<u8> },
    Event(StreamEvent),
    Closed,
}

/// An attach stream with a passive emulator mirroring the terminal the way a
/// real client does.
pub struct AttachClient {
    writer: UnixStream,
    incoming: Receiver<Received>,
    pub attached: Attached,
    pub role: Role,
    pub mirror: vt::Emulator,
    /// Offset of the next output byte this client expects.
    pub offset: u64,
    pub events: Vec<StreamEvent>,
    pub closed: bool,
}

impl Drop for AttachClient {
    /// Like a client process exiting: the socket closes.
    fn drop(&mut self) {
        let _ = self.writer.shutdown(std::net::Shutdown::Both);
    }
}

pub fn decode_checkpoint(checkpoint: &str) -> Vec<u8> {
    base64::engine::general_purpose::STANDARD
        .decode(checkpoint)
        .expect("checkpoint is base64")
}

impl AttachClient {
    fn new(stream: UnixStream, attached: Attached) -> Self {
        let writer = stream.try_clone().unwrap();
        let (tx, rx) = mpsc::channel();
        let mut reader = stream;
        reader.set_read_timeout(None).unwrap();
        std::thread::spawn(move || loop {
            let item = match frame::read_frame(&mut reader) {
                Ok(Some(Frame::Data(payload))) => {
                    let (offset, bytes) = frame::split_output(&payload).expect("output frame");
                    Received::Output {
                        offset,
                        bytes: bytes.to_vec(),
                    }
                }
                Ok(Some(Frame::Control(payload))) => {
                    Received::Event(frame::parse_control(&payload).expect("stream event"))
                }
                Ok(None) | Err(_) => {
                    let _ = tx.send(Received::Closed);
                    return;
                }
            };
            if tx.send(item).is_err() {
                return;
            }
        });
        let mut mirror = vt::Emulator::passive(attached.rows, attached.cols, 0);
        mirror.feed(&decode_checkpoint(&attached.checkpoint));
        Self {
            writer,
            incoming: rx,
            role: attached.role,
            offset: attached.offset,
            attached,
            mirror,
            events: Vec::new(),
            closed: false,
        }
    }

    /// Apply the next item to the mirror. `None` on timeout.
    pub fn next(&mut self, timeout: Duration) -> Option<Received> {
        let item = self.incoming.recv_timeout(timeout).ok()?;
        match &item {
            Received::Output { offset, bytes } => {
                assert_eq!(*offset, self.offset, "output must continue without gaps");
                self.mirror.feed(bytes);
                self.offset += bytes.len() as u64;
            }
            Received::Event(event) => {
                match event {
                    StreamEvent::Resized {
                        cols,
                        rows,
                        checkpoint,
                        offset,
                    } => {
                        self.mirror = vt::Emulator::passive(*rows, *cols, 0);
                        self.mirror.feed(&decode_checkpoint(checkpoint));
                        self.offset = *offset;
                    }
                    StreamEvent::RoleChanged { role } => self.role = *role,
                    _ => {}
                }
                self.events.push(event.clone());
            }
            Received::Closed => self.closed = true,
        }
        Some(item)
    }

    /// Consume items until `check` accepts one; panics on timeout.
    pub fn wait_for(&mut self, what: &str, mut check: impl FnMut(&Received) -> bool) -> Received {
        let deadline = Instant::now() + TIMEOUT;
        while let Some(left) = deadline.checked_duration_since(Instant::now()) {
            match self.next(left) {
                Some(item) if check(&item) => return item,
                Some(Received::Closed) => panic!("stream closed while waiting for {what}"),
                Some(_) => {}
                None => break,
            }
        }
        panic!(
            "timed out waiting for {what}; events so far: {:?}; screen:\n{}",
            self.events,
            self.mirror.text()
        );
    }

    pub fn wait_for_event(
        &mut self,
        what: &str,
        check: impl Fn(&StreamEvent) -> bool,
    ) -> StreamEvent {
        match self.wait_for(what, |item| matches!(item, Received::Event(e) if check(e))) {
            Received::Event(event) => event,
            _ => unreachable!(),
        }
    }

    /// Consume output until the mirrored screen contains `needle`.
    pub fn wait_for_screen(&mut self, needle: &str) {
        if self.mirror.text().contains(needle) {
            return;
        }
        let needle = needle.to_string();
        let deadline = Instant::now() + TIMEOUT;
        while let Some(left) = deadline.checked_duration_since(Instant::now()) {
            match self.next(left) {
                Some(Received::Closed) | None => break,
                Some(_) if self.mirror.text().contains(&needle) => return,
                Some(_) => {}
            }
        }
        panic!(
            "mirror never showed {needle:?}; screen:\n{}",
            self.mirror.text()
        );
    }

    /// Drain everything that arrives within `quiet`.
    pub fn drain(&mut self, quiet: Duration) {
        while self.next(quiet).is_some() {}
    }

    pub fn input(&mut self, bytes: &[u8]) {
        frame::write_input(&mut self.writer, bytes).expect("send input");
    }

    pub fn command(&mut self, command: &StreamCommand) {
        frame::write_control(&mut self.writer, command).expect("send command");
    }

    /// The mirrored screen row by row (as `read` returns it): rows joined
    /// with `\n`, trailing blank rows dropped.
    pub fn screen_rows(&mut self) -> String {
        let (rows, _) = self.mirror.size();
        let mut lines: Vec<String> = self
            .mirror
            .history_lines(0, rows as usize)
            .into_iter()
            .map(|row| row.content)
            .collect();
        while lines.last().is_some_and(|l| l.is_empty()) {
            lines.pop();
        }
        lines.join("\n")
    }

    /// Wait for the daemon to end the stream.
    pub fn wait_closed(&mut self) -> bool {
        let deadline = Instant::now() + TIMEOUT;
        while let Some(left) = deadline.checked_duration_since(Instant::now()) {
            match self.next(left) {
                Some(Received::Closed) => return true,
                Some(_) => {}
                None => return false,
            }
        }
        false
    }
}
