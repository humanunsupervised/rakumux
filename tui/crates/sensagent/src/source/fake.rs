//! In-memory SessionApi for AI-status source tests: scripted views and output, a
//! listed session set, queued event streams and recorded calls. Every operation other
//! than `get`, `read`, `list_all` and `subscribe` fails and is recorded.

use std::collections::VecDeque;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Mutex;

use session_core::api::{
    ApiError, AttachRequest, CallerInfo, CreateSession, DaemonStatusInfo, ErrorCode, EventEnvelope,
    ReadRequest, ReadResult, Report, ReviveSession, SendRequest, SubscribeRequest, WaitRequest,
    WaitResult,
};
use session_core::client::{AttachStream, EventStream, Result, SessionApi};
use session_core::ids::SessionId;
use session_core::model::{
    Creator, LaunchSpec, Lifecycle, ScopeRoot, SessionStatus, SessionView, TerminalView,
};

/// A live user session in `folder` whose terminal runs `pid`.
pub(crate) fn live_view(id: &SessionId, folder: &str, pid: u32) -> SessionView {
    SessionView {
        agent_output: None,
        id: id.clone(),
        name: "synthetic-session-name".into(),
        scope: ScopeRoot::from_canonical(folder.into()),
        is_self: false,
        cwd: format!("{folder}/synthetic-cwd").into(),
        status: SessionStatus::Live,
        exit_code: None,
        lifecycle: Lifecycle::Working,
        agent: None,
        reported: None,
        activity: None,
        created_by: Creator::User,
        parent_id: None,
        depth: 0,
        children: vec![],
        launch: LaunchSpec::default(),
        resume: None,
        auto_named: false,
        description: None,
        notes: None,
        labels: Default::default(),
        private: false,
        created_at: 1,
        last_active_at: 2,
        terminal: Some(TerminalView {
            pid: Some(pid),
            foreground_process: Some("synthetic-agent".into()),
            title: "synthetic-title".into(),
            cols: 80,
            rows: 24,
            controlled: false,
            alternate_screen: false,
        }),
        pinned: false,
        git: None,
    }
}

type Events = Receiver<std::result::Result<EventEnvelope, String>>;

pub(crate) struct FakeSessionApi {
    caller: CallerInfo,
    views: Mutex<VecDeque<Result<SessionView>>>,
    listed: Mutex<Vec<SessionView>>,
    streams: Mutex<VecDeque<Events>>,
    output: Mutex<Result<String>>,
    calls: Mutex<Vec<String>>,
    reads: Mutex<Vec<ReadRequest>>,
    panic_get: Mutex<Option<usize>>,
}

impl FakeSessionApi {
    pub(crate) fn user(view: SessionView, output: &str) -> Self {
        Self::with_caller(CallerInfo::User, view, output)
    }

    pub(crate) fn session_caller(view: SessionView) -> Self {
        let caller = CallerInfo::Session {
            id: view.id.clone(),
            scope: view.scope.clone(),
        };
        Self::with_caller(caller, view, "")
    }

    fn with_caller(caller: CallerInfo, view: SessionView, output: &str) -> Self {
        Self {
            caller,
            views: Mutex::new(VecDeque::from([Ok(view)])),
            listed: Mutex::new(Vec::new()),
            streams: Mutex::new(VecDeque::new()),
            output: Mutex::new(Ok(output.to_owned())),
            calls: Mutex::new(Vec::new()),
            reads: Mutex::new(Vec::new()),
            panic_get: Mutex::new(None),
        }
    }

    /// Answers for the next `get` calls, in order; the last one repeats.
    pub(crate) fn script(&self, views: Vec<Result<SessionView>>) {
        *self.views.lock().unwrap() = views.into();
    }

    /// The sessions `list_all` returns. Unless a script is pending, `get` answers
    /// with the listed view of that id.
    pub(crate) fn set_listed(&self, views: Vec<SessionView>) {
        *self.listed.lock().unwrap() = views;
    }

    /// Queue one event stream for the next `subscribe`; send `Err` or drop the
    /// sender to end it. Without a queued stream, `subscribe` fails.
    pub(crate) fn stream(&self) -> Sender<std::result::Result<EventEnvelope, String>> {
        let (sender, receiver) = mpsc::channel();
        self.streams.lock().unwrap().push_back(receiver);
        sender
    }

    /// Panic in the `call`th `get` (counting from 1), outside every lock.
    pub(crate) fn panic_on_get(&self, call: usize) {
        *self.panic_get.lock().unwrap() = Some(call);
    }

    pub(crate) fn set_output(&self, output: Result<String>) {
        *self.output.lock().unwrap() = output;
    }

    pub(crate) fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }

    pub(crate) fn reads(&self) -> Vec<ReadRequest> {
        self.reads.lock().unwrap().clone()
    }

    fn refuse<T>(&self, call: &str) -> Result<T> {
        self.calls.lock().unwrap().push(call.to_owned());
        Err(ApiError::new(
            ErrorCode::PermissionDenied,
            "synthetic-unexpected-daemon-operation",
        ))
    }
}

impl SessionApi for FakeSessionApi {
    fn caller(&self) -> &CallerInfo {
        &self.caller
    }

    fn list(&self, _scope: Option<&ScopeRoot>, _include_dead: bool) -> Result<Vec<SessionView>> {
        self.refuse("list")
    }

    fn get(&self, id: &SessionId) -> Result<SessionView> {
        let gets = {
            let mut calls = self.calls.lock().unwrap();
            calls.push(format!("get {id}"));
            calls.iter().filter(|call| call.starts_with("get ")).count()
        };
        let panic_at = *self.panic_get.lock().unwrap();
        if panic_at == Some(gets) {
            panic!("synthetic-get-panic");
        }
        let mut views = self.views.lock().unwrap();
        if views.len() > 1 {
            return views.pop_front().unwrap();
        }
        let listed = self.listed.lock().unwrap();
        match listed.iter().find(|view| &view.id == id) {
            Some(view) => Ok(view.clone()),
            None => views.front().unwrap().clone(),
        }
    }

    fn list_all(&self, include_dead: bool) -> Result<Vec<SessionView>> {
        assert!(!include_dead);
        self.calls.lock().unwrap().push("list_all".into());
        Ok(self.listed.lock().unwrap().clone())
    }

    fn create(&self, _request: CreateSession) -> Result<SessionView> {
        self.refuse("create")
    }

    fn rename(&self, _id: &SessionId, _name: &str) -> Result<SessionView> {
        self.refuse("rename")
    }

    fn kill(&self, _id: &SessionId, _tree: bool) -> Result<Vec<SessionId>> {
        self.refuse("kill")
    }

    fn delete(&self, _id: &SessionId) -> Result<()> {
        self.refuse("delete")
    }

    fn revive(&self, _request: ReviveSession) -> Result<SessionView> {
        self.refuse("revive")
    }

    fn report(&self, _report: Report) -> Result<()> {
        self.refuse("report")
    }

    fn read(&self, request: ReadRequest) -> Result<ReadResult> {
        self.calls
            .lock()
            .unwrap()
            .push(format!("read {}", request.id));
        self.reads.lock().unwrap().push(request);
        let text = self.output.lock().unwrap().clone()?;
        Ok(ReadResult {
            text,
            rows: 24,
            cols: 80,
            cursor: (0, 0),
            truncated: true,
            untrusted: true,
        })
    }

    fn wait(&self, _request: WaitRequest) -> Result<WaitResult> {
        self.refuse("wait")
    }

    fn send(&self, _request: SendRequest) -> Result<()> {
        self.refuse("send")
    }

    fn submit(&self, _id: &SessionId, _text: &str) -> Result<()> {
        self.refuse("submit")
    }

    fn touch(&self, _id: &SessionId) -> Result<()> {
        self.refuse("touch")
    }

    fn status(&self) -> Result<DaemonStatusInfo> {
        self.refuse("status")
    }

    fn shutdown(&self) -> Result<()> {
        self.refuse("shutdown")
    }

    fn attach(&self, _request: AttachRequest) -> Result<AttachStream> {
        self.refuse("attach")
    }

    fn subscribe(&self, request: SubscribeRequest) -> Result<EventStream> {
        assert!(request.all);
        self.calls.lock().unwrap().push("subscribe".into());
        match self.streams.lock().unwrap().pop_front() {
            Some(events) => Ok(EventStream::from_receiver(events)),
            None => Err(ApiError::new(
                ErrorCode::DaemonUnavailable,
                "synthetic-daemon-unavailable",
            )),
        }
    }
}
