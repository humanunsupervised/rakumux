//! Shared test support for the control layer: an in-memory `TerminalHost`
//! and a harness around `ControlService` with an isolated `RAKUMUX_HOME`.
#![allow(dead_code)]

use ptyd::control::{Clock, ControlConfig, ControlService, Detector, Limits};
use ptyd::control_api::{Caller, Control};
use ptyd::detect::{DetectInput, Detection};
use ptyd::host::{
    AttachControl, EnterCheck, HostAttachment, HostError, HostEvent, SpawnSpec, TerminalHost,
    TerminalInfo, TerminalState,
};
use session_core::api::{
    ApiError, Auth, CreateSession, ReadResult, ReadSource, Request, Response, Role, TextFormat,
};
use session_core::client::AttachEvent;
use session_core::ids::{SessionId, TerminalId, Token};
use session_core::model::{Lifecycle, ScopeRoot, SessionView};
use session_core::paths::Layout;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

// ---------------------------------------------------------------- fake host

#[derive(Debug, Clone)]
pub struct FakeTerminal {
    pub spec: SpawnSpec,
    pub info: TerminalInfo,
    pub screen: String,
    pub input: Vec<u8>,
    pub submits: Vec<String>,
    pub keys: Vec<(Vec<String>, Option<String>)>,
    pub terminated: bool,
    pub forgotten: bool,
    /// Someone is typing in it: a quiet submit is refused.
    pub typing: bool,
    /// For each checked submit whose Enter came due ([`FakeHost::enter_due`]),
    /// whether its check let the Enter go.
    pub enters: Vec<bool>,
}

#[derive(Default)]
struct FakeState {
    next_id: u64,
    terminals: BTreeMap<TerminalId, FakeTerminal>,
    fail_next_spawn: Option<String>,
    /// Checked submits whose text is in and whose Enter isn't due yet.
    pending_enters: Vec<(TerminalId, EnterCheck)>,
}

/// Terminals that exist only in memory. Tests drive output, exits and
/// screens explicitly and inspect what the control layer asked for.
#[derive(Default)]
pub struct FakeHost {
    state: Mutex<FakeState>,
    subscribers: Mutex<Vec<Sender<HostEvent>>>,
}

const KNOWN_KEYS: &[&str] = &["enter", "esc", "tab", "up", "down", "ctrl+c"];

impl FakeHost {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub fn emit(&self, event: HostEvent) {
        self.subscribers
            .lock()
            .unwrap()
            .retain(|tx| tx.send(event.clone()).is_ok());
    }

    /// Every spawn so far, oldest first (including forgotten terminals).
    pub fn spawns(&self) -> Vec<SpawnSpec> {
        let state = self.state.lock().unwrap();
        state.terminals.values().map(|t| t.spec.clone()).collect()
    }

    /// The newest terminal spawned for `session`.
    pub fn terminal_of(&self, session: &SessionId) -> TerminalId {
        let state = self.state.lock().unwrap();
        *state
            .terminals
            .iter()
            .rev()
            .find(|(_, t)| &t.spec.session_id == session)
            .map(|(id, _)| id)
            .expect("no terminal for session")
    }

    pub fn terminal(&self, id: TerminalId) -> FakeTerminal {
        self.state.lock().unwrap().terminals[&id].clone()
    }

    pub fn spec_of(&self, session: &SessionId) -> SpawnSpec {
        let id = self.terminal_of(session);
        self.terminal(id).spec
    }

    /// Replace the visible screen and announce output.
    pub fn show(&self, session: &SessionId, screen: &str) {
        let id = self.terminal_of(session);
        {
            let mut state = self.state.lock().unwrap();
            let terminal = state.terminals.get_mut(&id).unwrap();
            terminal.screen = screen.to_string();
            terminal.info.output_offset += screen.len() as u64;
            terminal.info.last_output_ms = Some(now_ms());
        }
        self.emit(HostEvent::Output {
            id,
            session_id: session.clone(),
        });
    }

    /// Show a detection fixture (`tests/fixtures/detect/<name>.txt`): its
    /// screen, foreground process, title, alternate screen and output age,
    /// then announce output. Returns the lifecycle the fixture expects.
    pub fn show_fixture(&self, session: &SessionId, name: &str) -> Lifecycle {
        let expected = self.put_fixture(session, name);
        self.emit(HostEvent::Output {
            id: self.terminal_of(session),
            session_id: session.clone(),
        });
        expected
    }

    /// [`show_fixture`](Self::show_fixture) without announcing it: the
    /// terminal shows it, the control layer hasn't looked yet.
    pub fn put_fixture(&self, session: &SessionId, name: &str) -> Lifecycle {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/detect")
            .join(format!("{name}.txt"));
        let text = std::fs::read_to_string(&path).unwrap();
        let (header, screen) = text.split_once("\n---\n").unwrap();
        let field = |key: &str| {
            header.lines().find_map(|line| {
                let (k, v) = line.split_once(':')?;
                (k == key).then(|| v.trim().to_string())
            })
        };
        let process = field("process").map(|name| ptyd::host::ProcessInfo {
            pid: 4242,
            cmdline: field("cmdline")
                .map(|c| c.split_whitespace().map(String::from).collect())
                .unwrap_or_else(|| vec![name.clone()]),
            name,
            exe: field("exe").map(PathBuf::from),
        });
        let expected = match field("expect").as_deref() {
            Some("working") => Lifecycle::Working,
            Some("blocked") => Lifecycle::Blocked,
            Some("idle") => Lifecycle::Idle,
            Some("unknown") => Lifecycle::Unknown,
            _ => Lifecycle::None,
        };
        let id = self.terminal_of(session);
        {
            let mut state = self.state.lock().unwrap();
            let terminal = state.terminals.get_mut(&id).unwrap();
            terminal.screen = screen.to_string();
            terminal.info.foreground = process;
            terminal.info.title = field("title").unwrap_or_default();
            terminal.info.alternate_screen = field("alternate_screen").as_deref() == Some("true");
            terminal.info.output_offset += screen.len() as u64;
            let age: i64 = field("ms_since_output").map_or(0, |ms| ms.parse().unwrap());
            terminal.info.last_output_ms = Some(now_ms() - age);
        }
        expected
    }

    /// The session's process exits with `code`.
    pub fn exit(&self, session: &SessionId, code: Option<i32>) {
        let id = self.terminal_of(session);
        self.exit_terminal(id, code);
    }

    fn exit_terminal(&self, id: TerminalId, code: Option<i32>) {
        let session_id = {
            let mut state = self.state.lock().unwrap();
            let terminal = state.terminals.get_mut(&id).unwrap();
            terminal.info.state = TerminalState::Exited { code };
            terminal.spec.session_id.clone()
        };
        self.emit(HostEvent::Exited {
            id,
            session_id,
            code,
        });
    }

    /// The pause after the session's checked submits is over: ask each
    /// one's Enter check (without the host's lock, as the real writer thread
    /// asks it) and note in `enters` whether the Enter went.
    pub fn enter_due(&self, session: &SessionId) {
        let id = self.terminal_of(session);
        let due: Vec<EnterCheck> = {
            let mut state = self.state.lock().unwrap();
            let (due, kept) = std::mem::take(&mut state.pending_enters)
                .into_iter()
                .partition(|(terminal, _)| *terminal == id);
            state.pending_enters = kept;
            due.into_iter().map(|(_, check)| check).collect()
        };
        for check in due {
            let went = check();
            self.state
                .lock()
                .unwrap()
                .terminals
                .get_mut(&id)
                .unwrap()
                .enters
                .push(went);
        }
    }

    pub fn fail_next_spawn(&self, message: &str) {
        self.state.lock().unwrap().fail_next_spawn = Some(message.to_string());
    }

    pub fn set_typing(&self, session: &SessionId, typing: bool) {
        let id = self.terminal_of(session);
        self.state
            .lock()
            .unwrap()
            .terminals
            .get_mut(&id)
            .unwrap()
            .typing = typing;
    }

    pub fn set_size(&self, session: &SessionId, cols: u16, rows: u16) {
        let id = self.terminal_of(session);
        let mut state = self.state.lock().unwrap();
        let terminal = state.terminals.get_mut(&id).unwrap();
        terminal.info.cols = cols;
        terminal.info.rows = rows;
    }

    pub fn set_foreground(&self, session: &SessionId, process: ptyd::host::ProcessInfo) {
        let id = self.terminal_of(session);
        self.state
            .lock()
            .unwrap()
            .terminals
            .get_mut(&id)
            .unwrap()
            .info
            .foreground = Some(process);
        self.emit(HostEvent::Output {
            id,
            session_id: session.clone(),
        });
    }

    fn with_running<T>(
        &self,
        id: TerminalId,
        f: impl FnOnce(&mut FakeTerminal) -> T,
    ) -> Result<T, HostError> {
        let mut state = self.state.lock().unwrap();
        let terminal = state
            .terminals
            .get_mut(&id)
            .filter(|t| !t.forgotten)
            .ok_or(HostError::NotFound)?;
        if terminal.info.state != TerminalState::Running {
            return Err(HostError::NotRunning);
        }
        Ok(f(terminal))
    }
}

struct NoopControl;

impl AttachControl for NoopControl {
    fn input(&self, _bytes: &[u8]) -> Result<(), HostError> {
        Ok(())
    }
    fn resize(&self, _cols: u16, _rows: u16) -> Result<(), HostError> {
        Ok(())
    }
    fn promote(&self, _cols: u16, _rows: u16) -> Result<(), HostError> {
        Ok(())
    }
    fn demote(&self) {}
    fn fit(&self, _cols: u16, _rows: u16) -> Result<(), HostError> {
        Ok(())
    }
    fn detach(&self) {}
}

impl TerminalHost for FakeHost {
    fn spawn(&self, spec: SpawnSpec) -> Result<TerminalInfo, HostError> {
        let mut state = self.state.lock().unwrap();
        if let Some(message) = state.fail_next_spawn.take() {
            return Err(HostError::Spawn(message));
        }
        state.next_id += 1;
        let id = TerminalId(state.next_id);
        let info = TerminalInfo {
            id,
            session_id: spec.session_id.clone(),
            pid: Some(1000 + state.next_id as u32),
            cwd: Some(spec.cwd.clone()),
            title: String::new(),
            foreground: None,
            state: TerminalState::Running,
            cols: spec.cols,
            rows: spec.rows,
            alternate_screen: false,
            bracketed_paste: false,
            controlled: false,
            output_offset: 0,
            last_output_ms: None,
            started_ms: now_ms(),
        };
        state.terminals.insert(
            id,
            FakeTerminal {
                spec,
                info: info.clone(),
                screen: String::new(),
                input: Vec::new(),
                submits: Vec::new(),
                keys: Vec::new(),
                terminated: false,
                forgotten: false,
                typing: false,
                enters: Vec::new(),
            },
        );
        Ok(info)
    }

    fn list(&self) -> Vec<TerminalInfo> {
        let state = self.state.lock().unwrap();
        state
            .terminals
            .values()
            .filter(|t| !t.forgotten)
            .map(|t| t.info.clone())
            .collect()
    }

    fn info(&self, id: TerminalId) -> Option<TerminalInfo> {
        let state = self.state.lock().unwrap();
        state
            .terminals
            .get(&id)
            .filter(|t| !t.forgotten)
            .map(|t| t.info.clone())
    }

    fn read(
        &self,
        id: TerminalId,
        _source: ReadSource,
        _lines: Option<u32>,
        _format: TextFormat,
    ) -> Result<ReadResult, HostError> {
        let state = self.state.lock().unwrap();
        let terminal = state
            .terminals
            .get(&id)
            .filter(|t| !t.forgotten)
            .ok_or(HostError::NotFound)?;
        Ok(ReadResult {
            text: terminal.screen.clone(),
            rows: terminal.info.rows,
            cols: terminal.info.cols,
            cursor: (0, 0),
            truncated: false,
            untrusted: false,
        })
    }

    fn write(&self, id: TerminalId, bytes: &[u8]) -> Result<(), HostError> {
        self.with_running(id, |t| t.input.extend_from_slice(bytes))
    }

    fn submit(&self, id: TerminalId, text: &str) -> Result<(), HostError> {
        self.with_running(id, |t| t.submits.push(text.to_string()))
    }

    fn submit_when_quiet(
        &self,
        id: TerminalId,
        text: &str,
        _quiet: Duration,
    ) -> Result<(), HostError> {
        if self
            .state
            .lock()
            .unwrap()
            .terminals
            .get(&id)
            .is_some_and(|t| t.typing)
        {
            return Err(HostError::Typing);
        }
        self.submit(id, text)
    }

    fn submit_expecting(
        &self,
        id: TerminalId,
        text: &str,
        quiet: Option<Duration>,
        foreground: Option<&str>,
    ) -> Result<(), HostError> {
        if let Some(expected) = foreground {
            let state = self.state.lock().unwrap();
            let now = state
                .terminals
                .get(&id)
                .and_then(|t| t.info.foreground.as_ref())
                .map(|process| process.name.clone());
            if now.as_deref() != Some(expected) {
                return Err(HostError::ForegroundChanged);
            }
        }
        match quiet {
            Some(quiet) => self.submit_when_quiet(id, text, quiet),
            None => self.submit(id, text),
        }
    }

    fn submit_checked(
        &self,
        id: TerminalId,
        text: &str,
        quiet: Option<Duration>,
        foreground: Option<&str>,
        enter: EnterCheck,
    ) -> Result<(), HostError> {
        self.submit_expecting(id, text, quiet, foreground)?;
        // The text is in; its Enter waits for `enter_due`.
        self.state.lock().unwrap().pending_enters.push((id, enter));
        Ok(())
    }

    fn send_keys(
        &self,
        id: TerminalId,
        keys: &[String],
        text: Option<&str>,
    ) -> Result<(), HostError> {
        if let Some(bad) = keys.iter().find(|k| !KNOWN_KEYS.contains(&k.as_str())) {
            return Err(HostError::InvalidKey(bad.clone()));
        }
        self.with_running(id, |t| {
            t.keys.push((keys.to_vec(), text.map(str::to_string)))
        })
    }

    fn terminate(&self, id: TerminalId) -> Result<(), HostError> {
        self.with_running(id, |t| t.terminated = true)?;
        self.exit_terminal(id, None);
        Ok(())
    }

    fn forget(&self, id: TerminalId) {
        if let Some(t) = self.state.lock().unwrap().terminals.get_mut(&id) {
            t.forgotten = true;
        }
    }

    fn attach(
        &self,
        id: TerminalId,
        role: Role,
        _cols: u16,
        _rows: u16,
    ) -> Result<HostAttachment, HostError> {
        let info = self.info(id).ok_or(HostError::NotFound)?;
        let (_tx, rx): (Sender<AttachEvent>, Receiver<AttachEvent>) = mpsc::channel();
        Ok(HostAttachment {
            info,
            role,
            checkpoint: Vec::new(),
            offset: 0,
            events: rx,
            control: Box::new(NoopControl),
        })
    }

    fn subscribe(&self) -> Receiver<HostEvent> {
        let (tx, rx) = mpsc::channel();
        self.subscribers.lock().unwrap().push(tx);
        rx
    }

    fn shutdown(&self) {
        let running: Vec<TerminalId> = {
            let state = self.state.lock().unwrap();
            state
                .terminals
                .iter()
                .filter(|(_, t)| t.info.state == TerminalState::Running)
                .map(|(id, _)| *id)
                .collect()
        };
        for id in running {
            let _ = self.terminate(id);
        }
    }
}

// ---------------------------------------------------------------- detector

/// Detects a fake agent from screen markers: `[working]`, `[blocked]`,
/// `[idle]`. Anything else: no agent.
pub fn marker_detector(input: &DetectInput<'_>) -> Detection {
    let lifecycle = if input.screen.contains("[working]") {
        Lifecycle::Working
    } else if input.screen.contains("[blocked]") {
        Lifecycle::Blocked
    } else if input.screen.contains("[idle]") {
        Lifecycle::Idle
    } else {
        return Detection {
            agent: None,
            lifecycle: Lifecycle::None,
        };
    };
    Detection {
        agent: Some("fakeagent".into()),
        lifecycle,
    }
}

// ---------------------------------------------------------------- harness

pub struct Harness {
    pub dir: tempfile::TempDir,
    pub layout: Layout,
    pub scope: ScopeRoot,
    pub host: Arc<FakeHost>,
    pub control: ControlService,
    pub user_token: Token,
    pub user: Caller,
    pub detector: Detector,
}

pub const RAKUMUX_BIN: &str = "/opt/rakumux/bin/rakumux";

impl Harness {
    pub fn new() -> Self {
        Self::with_detector(ptyd::detect::detect)
    }

    pub fn with_detector(detector: Detector) -> Self {
        Self::on(detector, None)
    }

    /// [`Harness::with_detector`] on `clock` in place of the system's.
    pub fn with_clock(detector: Detector, clock: Clock) -> Self {
        Self::on(detector, Some(clock))
    }

    fn on(detector: Detector, clock: Option<Clock>) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        std::fs::create_dir_all(root.join("project/sub")).unwrap();
        std::fs::create_dir_all(root.join("other")).unwrap();
        let layout = Layout::with_home(root.join("home"));
        let scope = ScopeRoot::from_canonical(root.join("project"));
        let host = FakeHost::new();
        let user_token = Token::generate();
        let control = match clock {
            Some(clock) => open_with_clock(&layout, &user_token, host.clone(), detector, clock),
            None => open(&layout, &user_token, host.clone(), detector),
        };
        let user = control
            .authenticate(&hello(Auth::User {
                token: user_token.as_str().into(),
            }))
            .unwrap();
        Self {
            dir,
            layout,
            scope,
            host,
            control,
            user_token,
            user,
            detector,
        }
    }

    /// Simulate a daemon restart: a fresh host (no terminals survive) and a
    /// fresh control service over the same registry.
    pub fn restart(&mut self) {
        self.control.shutdown();
        self.host = FakeHost::new();
        self.control = open(
            &self.layout,
            &self.user_token,
            self.host.clone(),
            self.detector,
        );
    }

    pub fn root(&self) -> PathBuf {
        self.scope.path().parent().unwrap().to_path_buf()
    }

    pub fn other_scope(&self) -> ScopeRoot {
        ScopeRoot::from_canonical(self.root().join("other"))
    }

    pub fn create_request(&self) -> CreateSession {
        CreateSession {
            scope: Some(self.scope.clone()),
            name: None,
            cwd: None,
            argv: None,
            startup_cmds: Vec::new(),
            env: vec![
                ("PATH".into(), "/usr/bin:/bin".into()),
                ("SHELL".into(), "/bin/bash".into()),
            ],
            cols: 80,
            rows: 24,
            machine: None,
            prefill: None,
        }
    }

    pub fn handle(&self, caller: &Caller, request: Request) -> Response {
        self.control.handle(caller, request)
    }

    /// Create a session in the harness scope as the user.
    pub fn create(&self, name: Option<&str>) -> SessionView {
        self.create_in(&self.scope, name)
    }

    pub fn create_in(&self, scope: &ScopeRoot, name: Option<&str>) -> SessionView {
        let mut request = self.create_request();
        request.scope = Some(scope.clone());
        request.name = name.map(str::to_string);
        session(self.handle(&self.user, Request::SessionCreate(request)))
    }

    pub fn get(&self, id: &SessionId) -> SessionView {
        session(self.handle(&self.user, Request::SessionGet { id: id.clone() }))
    }

    pub fn list(&self, include_dead: bool) -> Vec<SessionView> {
        match self.handle(
            &self.user,
            Request::SessionList {
                scopes: Vec::new(),
                scope: Some(self.scope.clone()),
                include_dead,
                all: false,
            },
        ) {
            Response::Sessions { sessions } => sessions,
            other => panic!("expected sessions, got {other:?}"),
        }
    }

    pub fn token_of(&self, id: &SessionId) -> String {
        let spec = self.host.spec_of(id);
        env_value(&spec.env, "RAKUMUX_SESSION_TOKEN").expect("session token in env")
    }

    /// Authenticate as a process inside `id`.
    pub fn session_caller(&self, id: &SessionId) -> Caller {
        self.control
            .authenticate(&hello(Auth::Session {
                id: id.clone(),
                token: self.token_of(id),
            }))
            .unwrap()
    }

    /// Rows of the audit log: (caller, method, target, decision, detail).
    pub fn audit(&self) -> Vec<AuditRow> {
        let conn = rusqlite::Connection::open(self.layout.registry_db()).unwrap();
        let mut stmt = conn
            .prepare(
                "SELECT caller, method, target_session_id, decision, detail, scope_root \
                 FROM audit_log ORDER BY id",
            )
            .unwrap();
        let rows = stmt
            .query_map([], |row| {
                Ok(AuditRow {
                    caller: row.get(0)?,
                    method: row.get(1)?,
                    target: row.get(2)?,
                    decision: row.get(3)?,
                    detail: row.get(4)?,
                    scope: row.get(5)?,
                })
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        rows
    }

    /// Wait until the registry recorded the session's exit (the host event
    /// is processed on the control layer's event thread).
    pub fn wait_recorded_exit(&self, id: &SessionId) {
        let db = self.db();
        eventually("exit recorded in the registry", || {
            let ended: Option<i64> = db
                .query_row(
                    "SELECT ended_at FROM sessions_v2 WHERE id = ?1",
                    [id.as_str()],
                    |r| r.get(0),
                )
                .unwrap();
            ended.map(|_| ())
        });
    }

    pub fn db(&self) -> rusqlite::Connection {
        rusqlite::Connection::open(self.layout.registry_db()).unwrap()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuditRow {
    pub caller: String,
    pub method: String,
    pub target: Option<String>,
    pub decision: String,
    pub detail: Option<String>,
    pub scope: Option<String>,
}

pub fn open(
    layout: &Layout,
    user_token: &Token,
    host: Arc<FakeHost>,
    detector: Detector,
) -> ControlService {
    ControlService::open_with_detector(config(layout, user_token), host, detector).unwrap()
}

/// [`open`] on `clock` in place of the system's.
pub fn open_with_clock(
    layout: &Layout,
    user_token: &Token,
    host: Arc<FakeHost>,
    detector: Detector,
    clock: Clock,
) -> ControlService {
    ControlService::open_with_clock(config(layout, user_token), host, detector, clock).unwrap()
}

fn config(layout: &Layout, user_token: &Token) -> ControlConfig {
    ControlConfig {
        layout: layout.clone(),
        user_token: user_token.clone(),
        socket: PathBuf::from("/tmp/rakumux-test/ptyd.sock"),
        rakumux_bin: Some(PathBuf::from(RAKUMUX_BIN)),
        version: "test".into(),
        limits: Limits::default(),
    }
}

pub fn hello(auth: Auth) -> session_core::api::Hello {
    session_core::api::Hello {
        max_blocking_ms: None,
        protocol: session_core::PROTOCOL_VERSION,
        client: "test".into(),
        version: "0".into(),
        auth,
    }
}

pub fn session(response: Response) -> SessionView {
    match response {
        Response::Session { session } => session,
        other => panic!("expected a session, got {other:?}"),
    }
}

pub fn error(response: Response) -> ApiError {
    match response {
        Response::Error { error } => error,
        other => panic!("expected an error, got {other:?}"),
    }
}

pub fn env_value(env: &[(String, String)], name: &str) -> Option<String> {
    env.iter().find(|(k, _)| k == name).map(|(_, v)| v.clone())
}

/// Poll `f` until it returns Some (host events are processed on a thread).
pub fn eventually<T>(what: &str, mut f: impl FnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(value) = f() {
            return value;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

pub fn path_str(path: &Path) -> String {
    path.to_str().unwrap().to_string()
}
