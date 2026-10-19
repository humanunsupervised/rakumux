//! In-memory `SessionApi` for CLI tests: canned sessions, recorded calls,
//! attach streams over `AttachStream::pair`.

use base64::Engine as _;
use session_core::api::{
    ApiError, AttachRequest, Attached, CallerInfo, CreateSession, DaemonStatusInfo, ErrorCode,
    ReadRequest, ReadResult, Report, ReviveSession, SendRequest, SubscribeRequest, WaitRequest,
    WaitResult,
};
use session_core::client::{AttachPeer, AttachStream, EventStream, Result, SessionApi};
use session_core::ids::SessionId;
use session_core::model::{
    Creator, LaunchSpec, Lifecycle, ProjectView, ScopeRoot, SessionStatus, SessionView,
};
use std::path::Path;
use std::sync::Mutex;

/// A live session named `name` in scope `/scope`.
pub fn session(id: &str, name: &str) -> SessionView {
    SessionView {
        pinned: false,
        git: None,
        auto_named: false,
        activity_summary: None,
        description: None,
        notes: None,
        labels: Default::default(),
        private: false,
        id: SessionId::from_string(id),
        name: name.into(),
        scope: ScopeRoot::from_canonical("/scope".into()),
        is_self: false,
        cwd: "/scope".into(),
        status: SessionStatus::Live,
        exit_code: None,
        lifecycle: Lifecycle::None,
        agent: None,
        reported: None,
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

/// A live session named `name` whose folder and cwd are `scope`.
pub fn in_scope(id: &str, name: &str, scope: &Path) -> SessionView {
    SessionView {
        scope: ScopeRoot::from_canonical(scope.to_path_buf()),
        cwd: scope.to_path_buf(),
        ..session(id, name)
    }
}

/// A folder at `path` without sessions (a `ProjectView` on the wire).
pub fn folder(path: &Path) -> ProjectView {
    ProjectView {
        path: ScopeRoot::from_canonical(path.to_path_buf()),
        name: path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default(),
        sessions: 0,
        live: 0,
        last_active_at: None,
        added_at: 0,
        git: None,
    }
}

/// Checkpoint the fake sends for session `id`.
pub fn checkpoint_of(id: &str) -> Vec<u8> {
    format!("<checkpoint {id}>").into_bytes()
}

pub struct FakeApi {
    caller: CallerInfo,
    pub sessions: Mutex<Vec<SessionView>>,
    pub projects: Mutex<Vec<ProjectView>>,
    /// One line per call, e.g. `attach S1 controller 80x24`.
    pub calls: Mutex<Vec<String>>,
    pub created: Mutex<Vec<CreateSession>>,
    pub reports: Mutex<Vec<Report>>,
    pub binds: Mutex<Vec<(String, Vec<String>)>>,
    pub reads: Mutex<Vec<ReadRequest>>,
    pub waits: Mutex<Vec<WaitRequest>>,
    /// Far ends of attach streams, in attach order.
    pub peers: Mutex<Vec<(SessionId, AttachPeer)>>,
    /// Returned by the next call that can fail, once.
    pub fail_next: Mutex<Option<ApiError>>,
}

impl FakeApi {
    pub fn user(sessions: Vec<SessionView>) -> Self {
        Self::with_caller(CallerInfo::User, sessions)
    }

    pub fn session_caller(id: &str, scope: &str, sessions: Vec<SessionView>) -> Self {
        Self::with_caller(
            CallerInfo::Session {
                id: SessionId::from_string(id),
                scope: ScopeRoot::from_canonical(scope.into()),
            },
            sessions,
        )
    }

    fn with_caller(caller: CallerInfo, sessions: Vec<SessionView>) -> Self {
        Self {
            caller,
            sessions: Mutex::new(sessions),
            projects: Mutex::new(vec![]),
            calls: Mutex::new(vec![]),
            created: Mutex::new(vec![]),
            reports: Mutex::new(vec![]),
            binds: Mutex::new(vec![]),
            reads: Mutex::new(vec![]),
            waits: Mutex::new(vec![]),
            peers: Mutex::new(vec![]),
            fail_next: Mutex::new(None),
        }
    }

    pub fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }

    fn record(&self, call: String) -> Result<()> {
        self.calls.lock().unwrap().push(call);
        match self.fail_next.lock().unwrap().take() {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }

    fn find(&self, id: &SessionId) -> Result<SessionView> {
        self.sessions
            .lock()
            .unwrap()
            .iter()
            .find(|s| &s.id == id)
            .cloned()
            .ok_or_else(|| ApiError::new(ErrorCode::NotFound, format!("no session {id}")))
    }
}

impl SessionApi for FakeApi {
    fn caller(&self) -> &CallerInfo {
        &self.caller
    }

    fn list(&self, scope: Option<&ScopeRoot>, include_dead: bool) -> Result<Vec<SessionView>> {
        let scope = scope.map(|s| s.path().display().to_string());
        self.record(format!("list {scope:?} dead={include_dead}"))?;
        Ok(self
            .sessions
            .lock()
            .unwrap()
            .iter()
            .filter(|s| include_dead || s.status != SessionStatus::Dead)
            .cloned()
            .collect())
    }

    fn get(&self, id: &SessionId) -> Result<SessionView> {
        self.record(format!("get {id}"))?;
        self.find(id)
    }

    fn create(&self, request: CreateSession) -> Result<SessionView> {
        self.record(format!("create {:?}", request.name))?;
        let count = self.created.lock().unwrap().len();
        let mut view = session(
            &format!("NEW{count}"),
            request.name.as_deref().unwrap_or("auto"),
        );
        if let Some(cwd) = &request.cwd {
            view.cwd = cwd.clone();
        }
        view.last_active_at = 1_000_000;
        self.created.lock().unwrap().push(request);
        self.sessions.lock().unwrap().push(view.clone());
        Ok(view)
    }

    fn rename(&self, id: &SessionId, name: &str) -> Result<SessionView> {
        self.record(format!("rename {id} {name}"))?;
        let mut view = self.find(id)?;
        view.name = name.into();
        Ok(view)
    }

    fn kill(&self, id: &SessionId, tree: bool) -> Result<Vec<SessionId>> {
        self.record(format!("kill {id} tree={tree}"))?;
        self.find(id)?;
        Ok(vec![id.clone()])
    }

    fn delete(&self, id: &SessionId) -> Result<()> {
        self.record(format!("delete {id}"))
    }

    fn revive(&self, request: ReviveSession) -> Result<SessionView> {
        self.record(format!("revive {}", request.id))?;
        self.find(&request.id)
    }

    fn report(&self, report: Report) -> Result<()> {
        self.record("report".into())?;
        self.reports.lock().unwrap().push(report);
        Ok(())
    }

    fn bind(&self, agent: &str, conversations: &[String]) -> Result<()> {
        self.record(format!("bind {agent}"))?;
        self.binds
            .lock()
            .unwrap()
            .push((agent.to_owned(), conversations.to_vec()));
        Ok(())
    }

    fn read(&self, request: ReadRequest) -> Result<ReadResult> {
        self.record(format!("read {}", request.id))?;
        self.reads.lock().unwrap().push(request);
        Ok(ReadResult {
            text: "$ ".into(),
            rows: 24,
            cols: 80,
            cursor: (0, 2),
            truncated: false,
            untrusted: true,
        })
    }

    fn wait(&self, request: WaitRequest) -> Result<WaitResult> {
        self.record(format!("wait {}", request.id))?;
        self.waits.lock().unwrap().push(request);
        Ok(WaitResult {
            matched: false,
            status: SessionStatus::Live,
            lifecycle: Lifecycle::Working,
            reported: None,
            reason: "timeout".into(),
        })
    }

    fn send(&self, request: SendRequest) -> Result<()> {
        self.record(format!("send {}", request.id))
    }

    fn submit(&self, id: &SessionId, text: &str) -> Result<()> {
        self.record(format!("submit {id} {text}"))
    }

    fn touch(&self, id: &SessionId) -> Result<()> {
        self.record(format!("touch {id}"))
    }

    fn status(&self) -> Result<DaemonStatusInfo> {
        self.record("status".into())?;
        Ok(DaemonStatusInfo {
            pid: 42,
            version: "fake".into(),
            protocol: 1,
            uptime_secs: 3725,
            sessions: 3,
            live_terminals: 2,
            clients: 1,
            rss_bytes: Some(5 * 1024 * 1024),
        })
    }

    fn shutdown(&self) -> Result<()> {
        self.record("shutdown".into())
    }

    fn attach(&self, request: AttachRequest) -> Result<AttachStream> {
        let role = format!("{:?}", request.role).to_lowercase();
        self.record(format!(
            "attach {} {role} {}x{}",
            request.id, request.cols, request.rows
        ))?;
        let session = self.find(&request.id)?;
        if session.status != SessionStatus::Live {
            return Err(ApiError::new(ErrorCode::SessionNotLive, "not running"));
        }
        let (stream, peer) = AttachStream::pair(Attached {
            session,
            role: request.role,
            cols: request.cols,
            rows: request.rows,
            checkpoint: base64::engine::general_purpose::STANDARD
                .encode(checkpoint_of(request.id.as_str())),
            offset: 0,
        });
        self.peers.lock().unwrap().push((request.id, peer));
        Ok(stream)
    }

    fn subscribe(&self, _request: SubscribeRequest) -> Result<EventStream> {
        self.record("subscribe".into())?;
        let (_tx, rx) = std::sync::mpsc::channel();
        Ok(EventStream::from_receiver(rx))
    }

    fn list_in(&self, scopes: &[ScopeRoot], include_dead: bool) -> Result<Vec<SessionView>> {
        let paths: Vec<String> = scopes
            .iter()
            .map(|s| s.path().display().to_string())
            .collect();
        self.record(format!("list_in {paths:?} dead={include_dead}"))?;
        Ok(self
            .sessions
            .lock()
            .unwrap()
            .iter()
            .filter(|s| scopes.contains(&s.scope))
            .filter(|s| include_dead || s.status != SessionStatus::Dead)
            .cloned()
            .collect())
    }

    fn projects(&self) -> Result<Vec<ProjectView>> {
        self.record("projects".into())?;
        Ok(self.projects.lock().unwrap().clone())
    }

    fn add_project(&self, path: &Path) -> Result<ProjectView> {
        self.record(format!("add_project {}", path.display()))?;
        let mut projects = self.projects.lock().unwrap();
        if let Some(known) = projects.iter().find(|p| p.path.path() == path) {
            return Ok(known.clone());
        }
        let added = folder(path);
        projects.push(added.clone());
        Ok(added)
    }

    fn remove_project(&self, path: &ScopeRoot) -> Result<()> {
        self.record(format!("remove_project {}", path.path().display()))?;
        if self
            .sessions
            .lock()
            .unwrap()
            .iter()
            .any(|s| &s.scope == path)
        {
            return Err(ApiError::new(
                ErrorCode::InvalidArgument,
                "the project still has sessions",
            ));
        }
        let mut projects = self.projects.lock().unwrap();
        let before = projects.len();
        projects.retain(|p| &p.path != path);
        if projects.len() == before {
            return Err(ApiError::new(ErrorCode::NotFound, "no such project"));
        }
        Ok(())
    }

    fn pin(&self, id: &SessionId, pinned: bool) -> Result<SessionView> {
        self.record(format!("pin {id} {pinned}"))?;
        let mut sessions = self.sessions.lock().unwrap();
        let session = sessions
            .iter_mut()
            .find(|s| &s.id == id)
            .ok_or_else(|| ApiError::new(ErrorCode::NotFound, format!("no session {id}")))?;
        session.pinned = pinned;
        Ok(session.clone())
    }
}
