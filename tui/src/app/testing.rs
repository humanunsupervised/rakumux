//! In-memory `SessionApi` for tests: sessions in a vector, a folder
//! registry (`project_*` on the wire), listing every folder (`sessions.all`)
//! and pins, or `unsupported` answers like an older daemon; attach streams
//! from `AttachStream::pair`, event streams from channels, every call
//! recorded.

use std::collections::HashMap;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use session_core::paths::Layout;

use base64::Engine;
use session_core::api::{
    ApiError, AttachRequest, Attached, CallerInfo, CreateSession, DaemonStatusInfo, ErrorCode,
    EventEnvelope, ReadRequest, ReadResult, Report, ReviveSession, SendRequest, SessionEvent,
    SubscribeRequest, WaitRequest, WaitResult,
};
use session_core::client::{
    AttachEvent, AttachOutgoing, AttachPeer, AttachStream, EventStream, Result, SessionApi,
};
use session_core::ids::SessionId;
use session_core::model::{
    Creator, GitInfo, LaunchSpec, Lifecycle, ProjectView, ScopeRoot, SessionStatus, SessionView,
    TerminalView,
};
use std::path::Path;

use super::types::AppResult;
use super::{App, AppOptions, Config, ExecMode, Settings};

/// A live session in `/repo` with the given recency.
pub fn view(id: &str, name: &str, last_active_at: i64) -> SessionView {
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
        id: SessionId::from_string(id),
        name: name.into(),
        scope: ScopeRoot::from_canonical("/repo".into()),
        is_self: false,
        cwd: "/repo".into(),
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
        created_at: last_active_at,
        last_active_at,
        terminal: Some(TerminalView {
            pid: Some(4242),
            foreground_process: Some("bash".into()),
            title: String::new(),
            cols: 80,
            rows: 24,
            controlled: false,
            alternate_screen: false,
        }),
    }
}

/// Like [`view`], created at `created_at` (the list's order) and last used
/// at `last_active_at` (which session is selected first).
pub fn aged(id: &str, name: &str, created_at: i64, last_active_at: i64) -> SessionView {
    let mut session = view(id, name, last_active_at);
    session.created_at = created_at;
    session
}

/// Like [`view`], in `scope`.
pub fn view_in(scope: &ScopeRoot, id: &str, name: &str, last_active_at: i64) -> SessionView {
    let mut session = view(id, name, last_active_at);
    session.scope = scope.clone();
    session.cwd = scope.path().to_path_buf();
    session
}

pub fn scope() -> ScopeRoot {
    ScopeRoot::from_canonical("/repo".into())
}

/// A folder path that needn't exist (the fake never touches the disk).
pub fn folder(path: &str) -> ScopeRoot {
    ScopeRoot::from_canonical(path.into())
}

/// Git context on `branch`.
pub fn on_branch(branch: &str) -> Option<GitInfo> {
    Some(GitInfo {
        repo_root: "/repo".into(),
        branch: Some(branch.into()),
        detached_at: None,
        worktree: None,
        // No remote: the folder's own name, which places don't repeat.
        repo: Some("repo".into()),
    })
}

pub struct FakeAttach {
    pub request: AttachRequest,
    pub peer: AttachPeer,
}

pub struct FakePane {
    pub id: session_core::ids::PaneId,
    pub peer: AttachPeer,
}

#[derive(Default)]
pub struct FakeState {
    pub terminal_opens: Vec<session_core::api::OpenTerminal>,
    pub terminal_attaches: Vec<session_core::api::TerminalAttachRequest>,
    pub pane_streams: Vec<FakePane>,
    pub panes: HashMap<session_core::ids::PaneId, session_core::api::TerminalAttached>,
    /// The daemon restarts in place (`daemon.restart`).
    pub restarts_in_place: bool,
    pub sessions: Vec<SessionView>,
    pub calls: Vec<String>,
    /// Every create request, in order.
    pub creates: Vec<CreateSession>,
    pub attaches: Vec<FakeAttach>,
    pub subscribers: Vec<mpsc::Sender<std::result::Result<EventEnvelope, String>>>,
    /// Saved output returned by `read`, per session.
    pub reads: HashMap<SessionId, String>,
    /// Errors to return from the named method (once).
    pub fail: HashMap<&'static str, ApiError>,
    /// Screen bytes sent as the attach checkpoint.
    pub checkpoint: Vec<u8>,
    pub next_id: u32,
    pub now: i64,
    /// The folder registry: path, added at, git context.
    pub folders: Vec<(ScopeRoot, i64, Option<GitInfo>)>,
    /// Answer `unsupported` to folders, lists of several folders and pins,
    /// like a daemon from before them.
    pub folders_unsupported: bool,
    /// No `sessions.all`, like a daemon from before it: `list_all` and
    /// `subscribe { all }` fail.
    pub all_unsupported: bool,
}

impl FakeState {
    /// A folder as the daemon would report it, with counts from the
    /// sessions.
    fn folder_view(&self, path: &ScopeRoot) -> Option<ProjectView> {
        let (_, added_at, git) = self.folders.iter().find(|(p, _, _)| p == path)?;
        let sessions: Vec<&SessionView> =
            self.sessions.iter().filter(|s| &s.scope == path).collect();
        Some(ProjectView {
            path: path.clone(),
            name: path
                .path()
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
            sessions: sessions.len() as u32,
            live: sessions
                .iter()
                .filter(|s| s.status == SessionStatus::Live)
                .count() as u32,
            last_active_at: sessions.iter().map(|s| s.last_active_at).max(),
            added_at: *added_at,
            git: git.clone(),
        })
    }

    fn remember_folder(&mut self, path: &ScopeRoot) {
        if !self.folders.iter().any(|(p, _, _)| p == path) {
            let now = self.now;
            self.folders.push((path.clone(), now, None));
        }
    }
}

fn unsupported() -> ApiError {
    ApiError::new(ErrorCode::Unsupported, "not supported by this daemon")
}

pub struct FakeApi {
    caller: CallerInfo,
    pub state: Mutex<FakeState>,
}

impl FakeApi {
    pub fn pane_count(&self) -> usize {
        self.state.lock().unwrap().pane_streams.len()
    }

    pub fn pane_outgoing(&self, index: usize) -> Vec<AttachOutgoing> {
        let state = self.state.lock().unwrap();
        std::iter::from_fn(|| state.pane_streams[index].peer.outgoing.try_recv().ok()).collect()
    }

    pub fn send_pane(&self, index: usize, event: AttachEvent) {
        let mut state = self.state.lock().unwrap();
        if matches!(
            event,
            AttachEvent::Event(session_core::api::StreamEvent::Exited { .. })
        ) {
            let id = state.pane_streams[index].id.clone();
            state.panes.remove(&id);
        }
        let _ = state.pane_streams[index].peer.events.send(event);
    }

    /// A current daemon; the sessions' folders are known.
    pub fn new(sessions: Vec<SessionView>) -> Arc<Self> {
        let mut state = FakeState {
            sessions,
            checkpoint: b"hello from the fake".to_vec(),
            now: 1_000,
            ..Default::default()
        };
        let scopes: Vec<ScopeRoot> = state.sessions.iter().map(|s| s.scope.clone()).collect();
        for scope in &scopes {
            state.remember_folder(scope);
        }
        Arc::new(Self {
            caller: CallerInfo::User,
            state: Mutex::new(state),
        })
    }

    /// A daemon from before folders and pins: those calls answer
    /// `unsupported`.
    pub fn without_folders(sessions: Vec<SessionView>) -> Arc<Self> {
        let fake = Self::without_all(sessions);
        fake.state.lock().unwrap().folders_unsupported = true;
        fake
    }

    /// A daemon with folders and pins but from before `sessions.all`.
    pub fn without_all(sessions: Vec<SessionView>) -> Arc<Self> {
        let fake = Self::new(sessions);
        fake.state.lock().unwrap().all_unsupported = true;
        fake
    }

    /// Register a folder (with its git context).
    pub fn add_known_folder(&self, path: &ScopeRoot, git: Option<GitInfo>) {
        let mut state = self.state.lock().unwrap();
        state.folders.retain(|(p, _, _)| p != path);
        state.folders.push((path.clone(), 0, git));
    }

    fn check_folders(&self) -> Result<()> {
        if self.state.lock().unwrap().folders_unsupported {
            Err(unsupported())
        } else {
            Ok(())
        }
    }

    pub fn calls(&self) -> Vec<String> {
        self.state.lock().unwrap().calls.clone()
    }

    /// The create requests received so far.
    pub fn creates(&self) -> Vec<CreateSession> {
        self.state.lock().unwrap().creates.clone()
    }

    pub fn clear_calls(&self) {
        self.state.lock().unwrap().calls.clear();
    }

    pub fn fail_next(&self, method: &'static str, error: ApiError) {
        self.state.lock().unwrap().fail.insert(method, error);
    }

    pub fn set_read(&self, id: &str, text: &str) {
        self.state
            .lock()
            .unwrap()
            .reads
            .insert(SessionId::from_string(id), text.into());
    }

    /// Send an event (in the `/repo` scope) to every subscriber.
    pub fn emit(&self, session_id: &str, event: SessionEvent) {
        self.emit_in(&scope(), session_id, event);
    }

    pub fn emit_in(&self, scope: &ScopeRoot, session_id: &str, event: SessionEvent) {
        let state = self.state.lock().unwrap();
        let envelope = EventEnvelope {
            scope: scope.clone(),
            session_id: SessionId::from_string(session_id),
            at: 0,
            event,
        };
        for tx in &state.subscribers {
            let _ = tx.send(Ok(envelope.clone()));
        }
    }

    /// Break every subscription (daemon gone).
    pub fn drop_subscribers(&self) {
        self.state.lock().unwrap().subscribers.clear();
    }

    pub fn attach_count(&self) -> usize {
        self.state.lock().unwrap().attaches.len()
    }

    pub fn attach_request(&self, index: usize) -> AttachRequest {
        self.state.lock().unwrap().attaches[index].request.clone()
    }

    /// Push an event into attach stream `index`.
    pub fn send_attach(&self, index: usize, event: AttachEvent) {
        let state = self.state.lock().unwrap();
        let _ = state.attaches[index].peer.events.send(event);
    }

    /// Everything the client sent on attach stream `index` so far.
    pub fn outgoing(&self, index: usize) -> Vec<AttachOutgoing> {
        let state = self.state.lock().unwrap();
        let mut out = Vec::new();
        while let Ok(message) = state.attaches[index].peer.outgoing.try_recv() {
            out.push(message);
        }
        out
    }

    fn record(&self, call: String) {
        self.state.lock().unwrap().calls.push(call);
    }

    fn take_failure(&self, method: &'static str) -> Result<()> {
        match self.state.lock().unwrap().fail.remove(method) {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }

    fn find(&self, id: &SessionId) -> Result<SessionView> {
        self.state
            .lock()
            .unwrap()
            .sessions
            .iter()
            .find(|s| &s.id == id)
            .cloned()
            .ok_or_else(|| ApiError::new(ErrorCode::NotFound, format!("no session {id}")))
    }

    fn update(&self, id: &SessionId, f: impl FnOnce(&mut SessionView)) -> Result<SessionView> {
        let mut state = self.state.lock().unwrap();
        let session = state
            .sessions
            .iter_mut()
            .find(|s| &s.id == id)
            .ok_or_else(|| ApiError::new(ErrorCode::NotFound, format!("no session {id}")))?;
        f(session);
        Ok(session.clone())
    }
}

impl SessionApi for FakeApi {
    fn open_terminal(
        &self,
        request: session_core::api::OpenTerminal,
    ) -> Result<session_core::client::TerminalStream> {
        self.take_failure("open_terminal")?;
        let mut state = self.state.lock().unwrap();
        let attached = session_core::api::TerminalAttached {
            id: request.id.clone(),
            cwd: Some(request.cwd.clone()),
            pid: Some(5000),
            role: session_core::api::Role::Controller,
            cols: request.cols,
            rows: request.rows,
            checkpoint: base64::engine::general_purpose::STANDARD.encode(&state.checkpoint),
            offset: 0,
        };
        state.panes.insert(request.id.clone(), attached.clone());
        state.terminal_opens.push(request);
        let (stream, peer) = session_core::client::TerminalStream::pair(attached.clone());
        state.pane_streams.push(FakePane {
            id: attached.id,
            peer,
        });
        Ok(stream)
    }

    fn attach_terminal(
        &self,
        request: session_core::api::TerminalAttachRequest,
    ) -> Result<session_core::client::TerminalStream> {
        self.take_failure("attach_terminal")?;
        let mut state = self.state.lock().unwrap();
        let mut attached = state
            .panes
            .get(&request.id)
            .cloned()
            .ok_or_else(|| ApiError::new(ErrorCode::NotFound, "terminal pane ended"))?;
        attached.role = request.role;
        state.terminal_attaches.push(request);
        let (stream, peer) = session_core::client::TerminalStream::pair(attached.clone());
        state.pane_streams.push(FakePane {
            id: attached.id,
            peer,
        });
        Ok(stream)
    }

    fn caller(&self) -> &CallerInfo {
        &self.caller
    }

    /// A current daemon's; an older one lacks `sessions.all` and what came
    /// after it, and one from before folders the rest too.
    fn daemon_capabilities(&self) -> Option<Vec<String>> {
        let state = self.state.lock().unwrap();
        let mut capabilities = vec!["attach"];
        if !state.folders_unsupported {
            capabilities.extend(["projects", "session.pin", "session.git"]);
        }
        if !state.all_unsupported {
            capabilities.extend([
                session_core::api::CAPABILITY_SESSIONS_ALL,
                session_core::api::CAPABILITY_TERMINAL_PANE,
                session_core::api::CAPABILITY_WORKING_DIRECTORY,
                session_core::api::CAPABILITY_AGENT_OUTPUT,
                session_core::api::CAPABILITY_USER_PROMPT,
                session_core::api::CAPABILITY_TURN,
                session_core::api::CAPABILITY_REPLIES,
                session_core::api::CAPABILITY_TRANSCRIPT,
                session_core::api::CAPABILITY_PLACES,
            ]);
        }
        if state.restarts_in_place {
            capabilities.push(session_core::api::CAPABILITY_DAEMON_RESTART);
        }
        Some(capabilities.into_iter().map(String::from).collect())
    }

    fn list(&self, scope: Option<&ScopeRoot>, include_dead: bool) -> Result<Vec<SessionView>> {
        self.record(format!("list dead={include_dead}"));
        self.take_failure("list")?;
        let state = self.state.lock().unwrap();
        Ok(state
            .sessions
            .iter()
            .filter(|s| scope.is_none_or(|scope| &s.scope == scope))
            .filter(|s| include_dead || s.status != SessionStatus::Dead)
            .cloned()
            .collect())
    }

    fn get(&self, id: &SessionId) -> Result<SessionView> {
        self.record(format!("get {id}"));
        self.find(id)
    }

    fn create(&self, request: CreateSession) -> Result<SessionView> {
        self.take_failure("create")?;
        let mut state = self.state.lock().unwrap();
        state.next_id += 1;
        state.now += 1;
        let id = format!("N{}", state.next_id);
        let mut session = view(&id, &format!("auto-{}", state.next_id), state.now);
        session.auto_named = request.name.is_none();
        if let Some(name) = &request.name {
            session.name = name.clone();
        }
        if let Some(scope) = &request.scope {
            session.scope = scope.clone();
        }
        if let Some(cwd) = &request.cwd {
            session.cwd = cwd.clone();
        }
        session.launch = LaunchSpec {
            argv: request.argv.clone(),
            startup_cmds: request.startup_cmds.clone(),
        };
        if !state.folders_unsupported {
            state.remember_folder(&session.scope);
        }
        state.calls.push(format!(
            "create name={:?} cwd={:?} argv={:?} startup={:?} size={}x{}",
            request.name,
            request.cwd,
            request.argv,
            request.startup_cmds,
            request.cols,
            request.rows
        ));
        state.creates.push(request);
        state.sessions.push(session.clone());
        Ok(session)
    }

    fn rename(&self, id: &SessionId, name: &str) -> Result<SessionView> {
        self.record(format!("rename {id} {name}"));
        self.take_failure("rename")?;
        self.update(id, |s| {
            s.name = name.into();
            // Like the daemon: any rename makes the name user-chosen.
            s.auto_named = false;
        })
    }

    fn kill(&self, id: &SessionId, tree: bool) -> Result<Vec<SessionId>> {
        self.record(format!("kill {id} tree={tree}"));
        self.take_failure("kill")?;
        self.update(id, |s| {
            s.status = SessionStatus::Exited;
            s.exit_code = Some(0);
        })?;
        Ok(vec![id.clone()])
    }

    fn delete(&self, id: &SessionId) -> Result<()> {
        self.record(format!("delete {id}"));
        self.take_failure("delete")?;
        self.find(id)?;
        self.state.lock().unwrap().sessions.retain(|s| &s.id != id);
        Ok(())
    }

    fn revive(&self, request: ReviveSession) -> Result<SessionView> {
        self.record(format!(
            "revive {} resume={} size={}x{}",
            request.id, request.resume, request.cols, request.rows
        ));
        self.take_failure("revive")?;
        self.update(&request.id, |s| {
            s.status = SessionStatus::Live;
            s.exit_code = None;
        })
    }

    fn report(&self, _report: Report) -> Result<()> {
        self.record("report".into());
        Ok(())
    }

    fn read(&self, request: ReadRequest) -> Result<ReadResult> {
        self.record(format!(
            "read {} {:?} lines={:?}",
            request.id, request.source, request.lines
        ));
        self.take_failure("read")?;
        self.find(&request.id)?;
        let text = self
            .state
            .lock()
            .unwrap()
            .reads
            .get(&request.id)
            .cloned()
            .unwrap_or_default();
        Ok(ReadResult {
            text,
            rows: 24,
            cols: 80,
            cursor: (0, 0),
            truncated: false,
            untrusted: true,
        })
    }

    fn wait(&self, request: WaitRequest) -> Result<WaitResult> {
        self.record(format!("wait {}", request.id));
        Err(ApiError::new(ErrorCode::Unsupported, "fake"))
    }

    fn send(&self, request: SendRequest) -> Result<()> {
        self.record(format!("send {}", request.id));
        Ok(())
    }

    fn submit(&self, id: &SessionId, text: &str) -> Result<()> {
        self.record(format!("submit {id} {text}"));
        Ok(())
    }

    fn touch(&self, id: &SessionId) -> Result<()> {
        self.record(format!("touch {id}"));
        self.take_failure("touch")?;
        let mut state = self.state.lock().unwrap();
        state.now += 1;
        let now = state.now;
        if let Some(s) = state.sessions.iter_mut().find(|s| &s.id == id) {
            s.last_active_at = now;
        }
        Ok(())
    }

    fn status(&self) -> Result<DaemonStatusInfo> {
        Err(ApiError::new(ErrorCode::Unsupported, "fake"))
    }

    fn shutdown(&self) -> Result<()> {
        self.record("shutdown".into());
        Ok(())
    }

    fn restart_daemon(&self, binary: Option<&std::path::Path>) -> Result<()> {
        self.record(format!("restart_daemon {}", binary.is_some()));
        if !self.state.lock().unwrap().restarts_in_place {
            return Err(unsupported());
        }
        Ok(())
    }

    fn attach(&self, request: AttachRequest) -> Result<AttachStream> {
        self.record(format!(
            "attach {} {:?} {}x{}",
            request.id, request.role, request.cols, request.rows
        ));
        self.take_failure("attach")?;
        let session = self.find(&request.id)?;
        if session.status != SessionStatus::Live {
            return Err(ApiError::new(ErrorCode::SessionNotLive, "not live"));
        }
        let mut state = self.state.lock().unwrap();
        let (cols, rows) = match request.role {
            session_core::api::Role::Controller => (request.cols, request.rows),
            session_core::api::Role::Observer => (80, 24),
        };
        let attached = Attached {
            session,
            role: request.role,
            cols,
            rows,
            checkpoint: base64::engine::general_purpose::STANDARD.encode(&state.checkpoint),
            offset: 100,
        };
        let (stream, peer) = AttachStream::pair(attached);
        state.attaches.push(FakeAttach { request, peer });
        Ok(stream)
    }

    /// Records `subscribe` and the scopes watched, comma-separated, or
    /// `subscribe all`.
    fn subscribe(&self, request: SubscribeRequest) -> Result<EventStream> {
        let scopes: Vec<String> = request
            .scope
            .iter()
            .chain(request.scopes.iter())
            .map(|s| s.path().display().to_string())
            .collect();
        if request.all {
            self.record("subscribe all".into());
            if self.state.lock().unwrap().all_unsupported {
                return Err(ApiError::new(ErrorCode::InvalidArgument, "scope required"));
            }
        } else {
            self.record(format!("subscribe {}", scopes.join(",")));
        }
        self.take_failure("subscribe")?;
        let (tx, rx) = mpsc::channel();
        self.state.lock().unwrap().subscribers.push(tx);
        Ok(EventStream::from_receiver(rx))
    }

    fn list_in(&self, scopes: &[ScopeRoot], include_dead: bool) -> Result<Vec<SessionView>> {
        self.check_folders()?;
        let names: Vec<String> = scopes
            .iter()
            .map(|s| s.path().display().to_string())
            .collect();
        self.record(format!("list_in {} dead={include_dead}", names.join(",")));
        self.take_failure("list_in")?;
        let state = self.state.lock().unwrap();
        Ok(state
            .sessions
            .iter()
            .filter(|s| scopes.contains(&s.scope))
            .filter(|s| include_dead || s.status != SessionStatus::Dead)
            .cloned()
            .collect())
    }

    fn list_all(&self, include_dead: bool) -> Result<Vec<SessionView>> {
        if self.state.lock().unwrap().all_unsupported {
            return Err(unsupported());
        }
        self.record(format!("list_all dead={include_dead}"));
        self.take_failure("list_all")?;
        let state = self.state.lock().unwrap();
        Ok(state
            .sessions
            .iter()
            .filter(|s| include_dead || s.status != SessionStatus::Dead)
            .cloned()
            .collect())
    }

    fn projects(&self) -> Result<Vec<ProjectView>> {
        self.check_folders()?;
        self.record("projects".into());
        self.take_failure("projects")?;
        let state = self.state.lock().unwrap();
        Ok(state
            .folders
            .iter()
            .filter_map(|(path, _, _)| state.folder_view(path))
            .collect())
    }

    fn add_project(&self, path: &Path) -> Result<ProjectView> {
        self.check_folders()?;
        self.record(format!("add_project {}", path.display()));
        self.take_failure("add_project")?;
        let mut state = self.state.lock().unwrap();
        let scope = ScopeRoot::from_canonical(path.to_path_buf());
        state.remember_folder(&scope);
        state
            .folder_view(&scope)
            .ok_or_else(|| ApiError::new(ErrorCode::Internal, "folder vanished"))
    }

    fn pin(&self, id: &SessionId, pinned: bool) -> Result<SessionView> {
        self.check_folders()?;
        self.record(format!("pin {id} {pinned}"));
        self.take_failure("pin")?;
        self.update(id, |s| s.pinned = pinned)
    }
}

// ---------------------------------------------------------------- harness

/// Suggests `fixed_name` when the screen has text; never touches a provider.
pub fn fake_namer(
    _: &Config,
    input: &crate::naming::NamingInput,
) -> std::result::Result<Option<String>, String> {
    Ok((!input.screen.is_empty()).then(|| "fixed_name".to_string()))
}

/// Phone access in tests: off and not worth offering unless set; `enable`
/// gives a fixed pairing. Every call is recorded.
#[derive(Default)]
pub struct FakePhone {
    pub enabled: Mutex<bool>,
    pub offer: bool,
    /// Make the next `enable` fail with this.
    pub fail_enable: Mutex<Option<String>>,
    pub calls: Mutex<Vec<&'static str>>,
}

impl FakePhone {
    pub fn pairing() -> crate::remote_access::Pairing {
        crate::remote_access::Pairing {
            urls: vec![
                "http://box.tail1234.ts.net:7337".into(),
                "http://100.88.129.91:7337".into(),
            ],
            token: "abcdefghijklmnopqrstuvwx27".into(),
        }
    }

    pub fn calls(&self) -> Vec<&'static str> {
        self.calls.lock().unwrap().clone()
    }

    fn call(&self, name: &'static str) {
        self.calls.lock().unwrap().push(name);
    }
}

impl crate::remote_access::PhoneAccess for FakePhone {
    fn enabled(&self) -> bool {
        *self.enabled.lock().unwrap()
    }

    fn worth_offering(&self) -> bool {
        self.call("worth_offering");
        self.offer
    }

    fn enable(&self) -> std::result::Result<crate::remote_access::Pairing, String> {
        self.call("enable");
        if let Some(error) = self.fail_enable.lock().unwrap().take() {
            return Err(error);
        }
        *self.enabled.lock().unwrap() = true;
        Ok(Self::pairing())
    }

    fn disable(&self) -> std::result::Result<(), String> {
        self.call("disable");
        *self.enabled.lock().unwrap() = false;
        Ok(())
    }
}

/// The Rakumux account in tests: signed out unless set. `sign_in` waits for
/// approval until `approve`; statuses carry the service's descriptions from
/// `descriptions` (missing ones are `pending`), and `refreshing` while in
/// `refreshing`, which `summarize` (a new title) adds the session to (and
/// records it in `summarized`); the feed answers with `feed_pages` in turn, then an
/// empty page at the asked revision (signed out, off or ready, as the
/// account is). Every call is recorded, and each feed question in `feed_asks`
/// (with whether the feed was shown in `feed_shown`).
#[derive(Default)]
pub struct FakeAccount {
    pub account: Mutex<Option<crate::account::Account>>,
    /// What `sign_in_status` answers; `None` until `sign_in`.
    pub sign_in: Mutex<Option<rakumux_sensagent::owner::protocol::SignInView>>,
    pub descriptions: Mutex<HashMap<SessionId, String>>,
    pub feed_pages: Mutex<std::collections::VecDeque<rakumux_sensagent::feed::FeedPage>>,
    pub feed_asks: Mutex<Vec<(Option<u64>, Option<u64>)>>,
    pub feed_shown: Mutex<Vec<bool>>,
    /// Sessions whose title is being made (`refreshing`).
    pub refreshing: Mutex<std::collections::HashSet<SessionId>>,
    /// Sessions a title was asked for, in order.
    pub summarized: Mutex<Vec<SessionId>>,
    /// What `feed_turn` answers in turn, then an empty turn; and what it was asked.
    pub turns: Mutex<std::collections::VecDeque<rakumux_sensagent::feed::FeedTurn>>,
    pub turn_asks: Mutex<Vec<(String, Option<u64>)>>,
    /// What `feed_reply` answers in turn (else that it was sent, with no
    /// reply); and the entries and texts it was given.
    pub reply_answers: Mutex<std::collections::VecDeque<rakumux_sensagent::feed::ReplyWrite>>,
    pub replies_written: Mutex<Vec<(String, String)>>,
    /// The session messages `session_message` was given: machine, session
    /// and text (answered from `reply_answers` too).
    pub messages_written: Mutex<Vec<(String, String, String)>>,
    pub calls: Mutex<Vec<&'static str>>,
}

impl FakeAccount {
    pub fn person() -> crate::account::Account {
        crate::account::Account {
            email: Some("person@example.com".into()),
            subject: "user_synthetic".into(),
            titles: Some(true),
            feed: Some(false),
            features_unsaved: false,
        }
    }

    /// Signed in with the Feed switch on.
    pub fn with_feed() -> Self {
        let fake = Self::default();
        *fake.account.lock().unwrap() = Some(crate::account::Account {
            feed: Some(true),
            ..Self::person()
        });
        fake
    }

    /// The feed answers `page` next.
    pub fn queue_feed(&self, page: rakumux_sensagent::feed::FeedPage) {
        self.feed_pages.lock().unwrap().push_back(page);
    }

    pub fn feed_asks(&self) -> Vec<(Option<u64>, Option<u64>)> {
        self.feed_asks.lock().unwrap().clone()
    }

    /// Whether each feed question was asked with the feed on screen.
    pub fn feed_shown(&self) -> Vec<bool> {
        self.feed_shown.lock().unwrap().clone()
    }

    pub fn signed_in() -> Self {
        let fake = Self::default();
        *fake.account.lock().unwrap() = Some(Self::person());
        fake
    }

    /// The code was approved in a browser.
    pub fn approve(&self) {
        *self.account.lock().unwrap() = Some(Self::person());
        *self.sign_in.lock().unwrap() =
            Some(rakumux_sensagent::owner::protocol::SignInView::SignedIn);
    }

    pub fn calls(&self) -> Vec<&'static str> {
        self.calls.lock().unwrap().clone()
    }

    fn call(&self, name: &'static str) {
        self.calls.lock().unwrap().push(name);
    }
}

impl crate::account::AccountAccess for FakeAccount {
    fn account(&self) -> std::result::Result<Option<crate::account::Account>, String> {
        self.call("account");
        Ok(self.account.lock().unwrap().clone())
    }

    fn sign_in(
        &self,
    ) -> std::result::Result<rakumux_sensagent::owner::protocol::SignInView, String> {
        self.call("sign_in");
        let waiting = rakumux_sensagent::owner::protocol::SignInView::Waiting {
            user_code: "WDJB-MJHT".into(),
            verification_uri: "https://accounts.example/device".into(),
            verification_uri_complete: None,
            connect_url: Some("https://site.example/connect?code=WDJB-MJHT".into()),
            expires_in_seconds: 600,
        };
        *self.sign_in.lock().unwrap() = Some(waiting.clone());
        Ok(waiting)
    }

    fn sign_in_status(
        &self,
    ) -> std::result::Result<rakumux_sensagent::owner::protocol::SignInView, String> {
        self.call("sign_in_status");
        Ok(self
            .sign_in
            .lock()
            .unwrap()
            .clone()
            .unwrap_or(rakumux_sensagent::owner::protocol::SignInView::Idle))
    }

    fn cancel_sign_in(&self) -> std::result::Result<(), String> {
        self.call("cancel_sign_in");
        *self.sign_in.lock().unwrap() = None;
        Ok(())
    }

    fn sign_out(&self) -> std::result::Result<(), String> {
        self.call("sign_out");
        *self.account.lock().unwrap() = None;
        Ok(())
    }

    fn statuses(
        &self,
        sessions: &[SessionId],
    ) -> std::result::Result<Vec<rakumux_sensagent::owner::protocol::StatusLine>, String> {
        use rakumux_sensagent::owner::protocol::{LineState, StatusLine};
        self.call("statuses");
        let descriptions = self.descriptions.lock().unwrap();
        Ok(sessions
            .iter()
            .map(|id| {
                let text = descriptions.get(id).cloned();
                StatusLine {
                    session: id.clone(),
                    state: if text.is_some() {
                        LineState::Ready
                    } else {
                        LineState::Pending
                    },
                    text,
                    captured_at: None,
                    failure: None,
                    refreshing: self.refreshing.lock().unwrap().contains(id),
                }
            })
            .collect())
    }

    fn summarize(&self, session: &SessionId) -> std::result::Result<(), String> {
        self.call("summarize");
        self.summarized.lock().unwrap().push(session.clone());
        self.refreshing.lock().unwrap().insert(session.clone());
        Ok(())
    }

    fn feed_reply(
        &self,
        entry: &str,
        text: &str,
    ) -> std::result::Result<rakumux_sensagent::feed::ReplyWrite, String> {
        self.call("feed_reply");
        self.replies_written
            .lock()
            .unwrap()
            .push((entry.to_owned(), text.to_owned()));
        Ok(self.reply_answers.lock().unwrap().pop_front().unwrap_or(
            rakumux_sensagent::feed::ReplyWrite {
                status: rakumux_sensagent::feed::ReplyWriteStatus::Sent,
                reply: None,
            },
        ))
    }

    fn session_message(
        &self,
        machine: &str,
        session: &str,
        text: &str,
    ) -> std::result::Result<rakumux_sensagent::feed::ReplyWrite, String> {
        self.call("session_message");
        self.messages_written.lock().unwrap().push((
            machine.to_owned(),
            session.to_owned(),
            text.to_owned(),
        ));
        Ok(self.reply_answers.lock().unwrap().pop_front().unwrap_or(
            rakumux_sensagent::feed::ReplyWrite {
                status: rakumux_sensagent::feed::ReplyWriteStatus::Sent,
                reply: None,
            },
        ))
    }

    fn feed_turn(
        &self,
        entry: &str,
        after: Option<u64>,
    ) -> std::result::Result<rakumux_sensagent::feed::FeedTurn, String> {
        self.call("feed_turn");
        self.turn_asks
            .lock()
            .unwrap()
            .push((entry.to_owned(), after));
        Ok(self
            .turns
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or(rakumux_sensagent::feed::FeedTurn {
                status: rakumux_sensagent::feed::FeedTurnStatus::Ready,
                messages: Vec::new(),
                next: None,
            }))
    }

    fn set_features(
        &self,
        titles: Option<bool>,
        feed: Option<bool>,
    ) -> std::result::Result<(), String> {
        self.call("set_features");
        let mut account = self.account.lock().unwrap();
        let account = account.as_mut().ok_or("signed out")?;
        account.titles = titles.or(account.titles);
        account.feed = feed.or(account.feed);
        Ok(())
    }

    fn feed(
        &self,
        after: Option<u64>,
        generation: Option<u64>,
        shown: bool,
    ) -> std::result::Result<rakumux_sensagent::feed::FeedPage, String> {
        use rakumux_sensagent::feed::{FeedPage, FeedStatus};
        self.call("feed");
        self.feed_asks.lock().unwrap().push((after, generation));
        self.feed_shown.lock().unwrap().push(shown);
        if let Some(page) = self.feed_pages.lock().unwrap().pop_front() {
            return Ok(page);
        }
        let status = match &*self.account.lock().unwrap() {
            None => return Ok(crate::account::signed_out_feed()),
            Some(account) if account.feed == Some(true) => FeedStatus::Ready,
            Some(_) => FeedStatus::Off,
        };
        Ok(FeedPage {
            status,
            generation: generation.unwrap_or(1),
            revision: after.unwrap_or(0),
            entries: Vec::new(),
            replies: Vec::new(),
            more: false,
            open: None,
            machine: None,
        })
    }
}

/// An `App` connected to a `FakeApi` with the inline executor, sized
/// 160×48, with its config and data dirs in a temp dir.
pub struct Harness {
    pub app: App,
    pub fake: Arc<FakeApi>,
    pub home: tempfile::TempDir,
}

#[derive(Default)]
pub struct FakeUpdater {
    pub latest: Mutex<Option<String>>,
    pub check_error: Mutex<Option<String>>,
    pub install_error: Mutex<Option<String>>,
    pub launcher: Mutex<Option<std::path::PathBuf>>,
    pub calls: Mutex<Vec<String>>,
}

impl crate::updater::UpdateService for FakeUpdater {
    fn check(
        &self,
        _current: &str,
    ) -> anyhow::Result<Option<(String, crate::updater::InstallMethod)>> {
        self.calls.lock().unwrap().push("check".into());
        if let Some(error) = self.check_error.lock().unwrap().as_ref() {
            anyhow::bail!("{error}");
        }
        Ok(self
            .latest
            .lock()
            .unwrap()
            .clone()
            .map(|version| (version, crate::updater::InstallMethod::Binary)))
    }
    fn install(
        &self,
        version: &str,
        _method: crate::updater::InstallMethod,
    ) -> anyhow::Result<Option<std::path::PathBuf>> {
        self.calls
            .lock()
            .unwrap()
            .push(format!("install {version}"));
        if let Some(error) = self.install_error.lock().unwrap().as_ref() {
            anyhow::bail!("{error}");
        }
        Ok(self.launcher.lock().unwrap().clone())
    }
}

pub fn options(fake: &Arc<FakeApi>, scope: ScopeRoot, dir: &tempfile::TempDir) -> AppOptions {
    let api: Arc<dyn SessionApi> = fake.clone();
    AppOptions {
        settings: Settings::default(),
        config_path: Some(dir.path().join("config.json")),
        scope,
        paths: Some(Layout::with_home(dir.path().to_path_buf())),
        self_session: None,
        connector: Arc::new(move || Ok(api.clone())),
        namer: fake_namer,
        phone: Arc::new(FakePhone::default()),
        // Signed in, so the footer shows only its usual buttons.
        account: Arc::new(FakeAccount::signed_in()),
        updater: Arc::new(FakeUpdater::default()),
        exec_mode: ExecMode::Inline,
        debug: false,
    }
}

pub fn start(options: AppOptions, fake: Arc<FakeApi>, dir: tempfile::TempDir) -> Harness {
    let mut app = App::new(options);
    app.resize(160, 48);
    app.start();
    app.process_pending();
    Harness {
        app,
        fake,
        home: dir,
    }
}

pub fn harness(sessions: Vec<SessionView>) -> Harness {
    let fake = FakeApi::new(sessions);
    let dir = tempfile::tempdir().unwrap();
    let options = options(&fake, scope(), &dir);
    start(options, fake, dir)
}

/// Started in `/repo` on a daemon that knows `known` (with their git
/// context) and the sessions' folders, under the Folders tab.
pub fn harness_with_folders(
    sessions: Vec<SessionView>,
    known: &[(&str, Option<GitInfo>)],
    layout: crate::layout::SessionLayout,
) -> Harness {
    let fake = FakeApi::new(sessions);
    for (path, git) in known {
        fake.add_known_folder(&folder(path), git.clone());
    }
    let dir = tempfile::tempdir().unwrap();
    let mut options = options(&fake, scope(), &dir);
    options.settings.sessions.layout = layout;
    start(options, fake, dir).on_folders_tab()
}

impl Harness {
    /// The Folders tab made active, as if picked after the start (which is
    /// under Status).
    pub fn on_folders_tab(mut self) -> Self {
        self.app.show_tab(crate::app::grouping::Grouping::Folder);
        self.app.process_pending();
        self
    }

    pub async fn key(&mut self, key: KeyEvent) -> AppResult {
        let result = self.app.handle_key(key).await;
        self.app.process_pending();
        result
    }

    pub async fn press(&mut self, code: KeyCode) -> AppResult {
        self.key(KeyEvent::new(code, KeyModifiers::NONE)).await
    }

    pub async fn ctrl(&mut self, c: char) -> AppResult {
        self.key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL))
            .await
    }

    pub async fn alt(&mut self, code: KeyCode) -> AppResult {
        self.key(KeyEvent::new(code, KeyModifiers::ALT)).await
    }

    /// The Rakumux key as most terminals send it: 0x1C, which crossterm
    /// reads as Ctrl-4.
    pub async fn rakumux(&mut self) -> AppResult {
        self.ctrl('4').await
    }

    /// The Rakumux key, then Tab (the last session).
    pub async fn prefix_tab(&mut self) -> AppResult {
        self.rakumux().await;
        self.press(KeyCode::Tab).await
    }

    /// The Rakumux key, then `c`: a Rakumux mode key from a session.
    pub async fn prefix(&mut self, c: char) -> AppResult {
        self.rakumux().await;
        self.press(KeyCode::Char(c)).await
    }

    pub async fn type_text(&mut self, text: &str) {
        for c in text.chars() {
            self.press(KeyCode::Char(c)).await;
        }
    }

    pub fn selected(&self) -> Option<String> {
        self.app.list.selected_id().map(|id| id.to_string())
    }

    pub fn last_attach(&self) -> (usize, session_core::api::AttachRequest) {
        let index = self.fake.attach_count() - 1;
        (index, self.fake.attach_request(index))
    }

    pub fn event(&mut self, session: &str, event: SessionEvent) {
        self.fake.emit(session, event);
        self.app.process_pending();
    }

    pub fn attach_event(&mut self, index: usize, event: AttachEvent) {
        self.fake.send_attach(index, event);
        self.app.process_pending();
    }
}
