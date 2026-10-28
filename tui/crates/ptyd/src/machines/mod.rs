//! Other machines (capability `machines`): sessions that run on another
//! machine's daemon, listed and used through this one.
//!
//! A machine is another daemon reached through its `rakumux-api`'s
//! `/v1/daemon` ([`pipe`]). Its sessions exist there only: this daemon keeps
//! a pointer to each (its id, its machine and the local folder it's listed
//! under) and its last known view, in `machines.json` (0600, with the
//! machines' tokens). Requests naming one are forwarded; attaching relays the
//! machine's stream; while connected, the machine's events are passed on to
//! user subscriptions. Session callers never see other machines' sessions.
//!
//! An open connection keeps a machine that pauses when idle (a Sprite)
//! running, so a connection is opened only when something needs the machine
//! and closed again once nothing has for [`IDLE`], unless one of its
//! sessions' agents is working or someone is attached.

pub mod pipe;

use crate::persist::write_atomic;
use crate::{log_debug, log_info, log_warn};
use pipe::{Pipe, PipeReader, PipeWriter};
use session_core::api::{
    ApiError, CreateSession, ErrorCode, EventEnvelope, EventKind, Request, Response, SessionEvent,
    SubscribeRequest,
};
use session_core::frame::{self, Frame};
use session_core::ids::{MachineId, SessionId};
use session_core::model::{
    Lifecycle, MachineRef, MachineState, MachineView, ScopeRoot, SessionStatus, SessionView,
};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};
use std::time::{Duration, Instant};

/// How long a connection stays open after the last use, with no agent of
/// the machine working and nobody attached.
pub const IDLE: Duration = Duration::from_secs(60);
/// How often idle connections are closed and the file is saved.
const HOUSEKEEPING: Duration = Duration::from_secs(5);
const MAX_NAME: usize = 64;

/// The machines this daemon reaches, and the sessions it lists from them.
#[derive(Clone)]
pub struct Machines {
    shared: Arc<Shared>,
}

struct Shared {
    path: PathBuf,
    state: Mutex<State>,
    subscribers: Mutex<Vec<Subscriber>>,
    stop: AtomicBool,
}

#[derive(Default)]
struct State {
    records: Vec<Record>,
    live: HashMap<MachineId, Live>,
    sessions: BTreeMap<SessionId, Remote>,
    dirty: bool,
}

/// What `machines.json` holds.
#[derive(Default, serde::Serialize, serde::Deserialize)]
struct Saved {
    #[serde(default)]
    machines: Vec<Record>,
    #[serde(default)]
    sessions: Vec<Remote>,
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct Record {
    id: MachineId,
    name: String,
    url: String,
    token: String,
    added_at: i64,
    /// The user's home directory there, from its daemon's status.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    home: Option<PathBuf>,
}

/// A session on another machine: where it's listed here, and its last view
/// as that machine sent it.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct Remote {
    machine: MachineId,
    local_scope: ScopeRoot,
    view: SessionView,
}

#[derive(Default)]
struct Live {
    state: Option<MachineState>,
    error: Option<String>,
    version: Option<String>,
    /// The connection requests are forwarded on, one at a time.
    requests: Option<Arc<Mutex<Option<Requests>>>>,
    /// The event connection's writer while it runs: dropping it closes it.
    watch: Option<PipeWriter>,
    watching: bool,
    last_used: Option<Instant>,
    attaches: usize,
}

struct Requests {
    reader: PipeReader,
    writer: PipeWriter,
}

struct Subscriber {
    tx: Sender<EventEnvelope>,
    request: SubscribeRequest,
}

/// An attach to another machine's session: the connection to relay, kept
/// counted while it runs.
pub struct RemoteAttach {
    pub pipe: Pipe,
    pub guard: AttachGuard,
    pub machine: MachineId,
    pub machines: Machines,
}

/// Keeps a machine's connection while an attach relayed to it runs.
pub struct AttachGuard {
    shared: Weak<Shared>,
    machine: MachineId,
}

impl Drop for AttachGuard {
    fn drop(&mut self) {
        if let Some(shared) = self.shared.upgrade() {
            let mut state = lock(&shared.state);
            if let Some(live) = state.live.get_mut(&self.machine) {
                live.attaches = live.attaches.saturating_sub(1);
                live.last_used = Some(Instant::now());
            }
        }
    }
}

impl Machines {
    /// Load `machines.json` from `home` (none yet: no machines) and start
    /// the housekeeping thread.
    pub fn open(home: &Path) -> Self {
        let path = home.join("machines.json");
        let saved = match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice::<Saved>(&bytes).unwrap_or_else(|e| {
                log_warn!(
                    "machines: {} unreadable, starting empty: {e}",
                    path.display()
                );
                Saved::default()
            }),
            Err(_) => Saved::default(),
        };
        let state = State {
            sessions: saved
                .sessions
                .into_iter()
                .map(|remote| (remote.view.id.clone(), remote))
                .collect(),
            records: saved.machines,
            ..State::default()
        };
        let shared = Arc::new(Shared {
            path,
            state: Mutex::new(state),
            subscribers: Mutex::new(Vec::new()),
            stop: AtomicBool::new(false),
        });
        let weak = Arc::downgrade(&shared);
        let _ = std::thread::Builder::new()
            .name("ptyd-machines".into())
            .spawn(move || housekeeping(weak));
        Self { shared }
    }

    /// Stop the housekeeping thread and close every connection.
    pub fn close(&self) {
        self.shared.stop.store(true, Ordering::SeqCst);
        let mut state = lock(&self.shared.state);
        for live in state.live.values_mut() {
            live.watch = None;
            live.requests = None;
        }
        save(&self.shared.path, &mut state);
    }

    /// Reach the machine at `url` with `token`: connects once to check them
    /// and learn its home folder, then keeps it.
    pub fn add(&self, name: &str, url: &str, token: &str) -> Result<MachineView, ApiError> {
        let name = name.trim();
        if name.is_empty() || name.chars().count() > MAX_NAME || name.chars().any(char::is_control)
        {
            return Err(invalid("a machine needs a name of up to 64 characters"));
        }
        let url = url.trim().trim_end_matches('/').to_string();
        pipe::check_url(&url).map_err(|e| invalid(e.to_string()))?;
        if token.is_empty() || token.chars().any(|c| c.is_control() || c.is_whitespace()) {
            return Err(invalid("the token must be one word"));
        }
        if lock(&self.shared.state)
            .records
            .iter()
            .any(|r| r.url == url)
        {
            return Err(invalid(format!("{url} is already added")));
        }
        let mut requests = open_requests(&url, token).map_err(|e| unreachable(&url, &e))?;
        let version = requests.1.clone();
        let home = match exchange(&mut requests.0, &Request::DaemonStatus) {
            Ok(Response::Status(status)) => status.home,
            Ok(other) => return Err(unexpected(&other)),
            Err(e) => return Err(unreachable(&url, &e)),
        };
        let record = Record {
            id: MachineId::generate(),
            name: name.to_string(),
            url,
            token: token.to_string(),
            added_at: now_secs(),
            home,
        };
        let id = record.id.clone();
        {
            let mut state = lock(&self.shared.state);
            state.records.push(record);
            state.live.insert(
                id.clone(),
                Live {
                    state: Some(MachineState::Connected),
                    version: Some(version),
                    requests: Some(Arc::new(Mutex::new(Some(requests.0)))),
                    last_used: Some(Instant::now()),
                    ..Live::default()
                },
            );
            state.dirty = true;
            save(&self.shared.path, &mut state);
        }
        log_info!("machines: added {id}");
        self.ensure_watch(&id);
        self.view(&id)
            .ok_or_else(|| ApiError::new(ErrorCode::Internal, "the machine was not saved"))
    }

    pub fn list(&self) -> Vec<MachineView> {
        let state = lock(&self.shared.state);
        state
            .records
            .iter()
            .map(|record| machine_view(record, state.live.get(&record.id)))
            .collect()
    }

    fn view(&self, id: &MachineId) -> Option<MachineView> {
        let state = lock(&self.shared.state);
        let record = state.records.iter().find(|r| r.id == *id)?;
        Some(machine_view(record, state.live.get(id)))
    }

    /// Forget a machine and the sessions listed from it.
    pub fn remove(&self, id: &MachineId) -> Result<(), ApiError> {
        let removed: Vec<Remote> = {
            let mut state = lock(&self.shared.state);
            let before = state.records.len();
            state.records.retain(|r| r.id != *id);
            if state.records.len() == before {
                return Err(not_found_machine(id));
            }
            state.live.remove(id);
            let ids: Vec<SessionId> = state
                .sessions
                .iter()
                .filter(|(_, r)| r.machine == *id)
                .map(|(sid, _)| sid.clone())
                .collect();
            let removed = ids
                .iter()
                .filter_map(|sid| state.sessions.remove(sid))
                .collect();
            state.dirty = true;
            save(&self.shared.path, &mut state);
            removed
        };
        for remote in removed {
            self.publish(EventEnvelope {
                scope: remote.local_scope.clone(),
                session_id: remote.view.id.clone(),
                at: now_ms(),
                event: SessionEvent::Removed,
            });
        }
        log_info!("machines: removed {id}");
        Ok(())
    }

    /// The machine a session runs on, when it's another machine's.
    pub fn owner(&self, id: &SessionId) -> Option<MachineId> {
        lock(&self.shared.state)
            .sessions
            .get(id)
            .map(|remote| remote.machine.clone())
    }

    /// Other machines' sessions listed under `scopes` (every one with
    /// `None`), as last seen. Reconnects to their machines meanwhile, so
    /// changes follow as events.
    pub fn sessions(&self, scopes: Option<&[ScopeRoot]>, include_dead: bool) -> Vec<SessionView> {
        let (views, machines) = {
            let state = lock(&self.shared.state);
            let mut machines = Vec::new();
            let views = state
                .sessions
                .values()
                .filter(|r| scopes.is_none_or(|scopes| scopes.contains(&r.local_scope)))
                .filter(|r| include_dead || r.view.status != SessionStatus::Dead)
                .filter_map(|r| {
                    let record = state.records.iter().find(|m| m.id == r.machine)?;
                    if !machines.contains(&r.machine) {
                        machines.push(r.machine.clone());
                    }
                    Some(stamped(r, record))
                })
                .collect();
            (views, machines)
        };
        for machine in machines {
            self.ensure_watch(&machine);
        }
        views
    }

    /// Forward `request` to the machine `id` runs on and answer with what it
    /// said, its sessions as listed here.
    pub fn forward(&self, machine: &MachineId, request: &Request) -> Result<Response, ApiError> {
        let response = self.exchange(machine, request)?;
        Ok(self.take_response(machine, None, response))
    }

    /// Create a session on `machine`, listed under `create.scope` here: in a
    /// folder of that name in the machine's home folder, made when missing.
    pub fn create(
        &self,
        machine: &MachineId,
        create: CreateSession,
    ) -> Result<SessionView, ApiError> {
        let local_scope = create.scope.clone().ok_or_else(|| {
            invalid("creating a session on another machine needs the local folder")
        })?;
        let home = self.home(machine)?;
        let folder = local_scope
            .path()
            .file_name()
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("work"));
        let remote_folder = home.join(folder);
        match self.exchange(
            machine,
            &Request::ProjectAdd {
                path: remote_folder.clone(),
                create: true,
            },
        )? {
            Response::Project { .. } => {}
            Response::Error { error } => return Err(error),
            other => return Err(unexpected(&other)),
        }
        let request = Request::SessionCreate(CreateSession {
            scope: Some(ScopeRoot::from_canonical(remote_folder)),
            cwd: None,
            // The machine's own environment, not this one's.
            env: Vec::new(),
            machine: None,
            ..create
        });
        match self.exchange(machine, &request)? {
            Response::Session { session } => {
                let response =
                    self.take_response(machine, Some(&local_scope), Response::Session { session });
                let Response::Session { session } = response else {
                    return Err(ApiError::new(ErrorCode::Internal, "lost the new session"));
                };
                self.publish(EventEnvelope {
                    scope: local_scope,
                    session_id: session.id.clone(),
                    at: now_ms(),
                    event: SessionEvent::Created {
                        session: session.clone(),
                    },
                });
                Ok(session)
            }
            Response::Error { error } => Err(error),
            other => Err(unexpected(&other)),
        }
    }

    /// A new connection to `machine` for an attach stream; the guard keeps
    /// the machine's connection open while the stream runs.
    pub fn attach(&self, machine: &MachineId) -> Result<(Pipe, AttachGuard), ApiError> {
        let (url, token) = self.address(machine)?;
        let pipe = pipe::connect(&url, &token).map_err(|e| {
            self.mark_unreachable(machine, &e);
            unreachable(&url, &e)
        })?;
        {
            let mut state = lock(&self.shared.state);
            let live = state.live.entry(machine.clone()).or_default();
            live.attaches += 1;
            live.last_used = Some(Instant::now());
        }
        self.ensure_watch(machine);
        Ok((
            pipe,
            AttachGuard {
                shared: Arc::downgrade(&self.shared),
                machine: machine.clone(),
            },
        ))
    }

    /// A session view from `machine` as it's listed here.
    pub fn stamp(&self, machine: &MachineId, view: SessionView) -> SessionView {
        let state = lock(&self.shared.state);
        let local_scope = state
            .sessions
            .get(&view.id)
            .map(|r| r.local_scope.clone())
            .unwrap_or_else(|| view.scope.clone());
        match state.records.iter().find(|r| r.id == *machine) {
            Some(record) => stamped(
                &Remote {
                    machine: machine.clone(),
                    local_scope,
                    view,
                },
                record,
            ),
            None => view,
        }
    }

    /// Other machines' events for a user subscription.
    pub fn subscribe(&self, request: &SubscribeRequest) -> Receiver<EventEnvelope> {
        let (tx, rx) = mpsc::channel();
        lock(&self.shared.subscribers).push(Subscriber {
            tx,
            request: request.clone(),
        });
        rx
    }

    // ------------------------------------------------------------ internals

    fn address(&self, machine: &MachineId) -> Result<(String, String), ApiError> {
        let state = lock(&self.shared.state);
        let record = state
            .records
            .iter()
            .find(|r| r.id == *machine)
            .ok_or_else(|| not_found_machine(machine))?;
        Ok((record.url.clone(), record.token.clone()))
    }

    fn home(&self, machine: &MachineId) -> Result<PathBuf, ApiError> {
        if let Some(home) = lock(&self.shared.state)
            .records
            .iter()
            .find(|r| r.id == *machine)
            .and_then(|r| r.home.clone())
        {
            return Ok(home);
        }
        match self.exchange(machine, &Request::DaemonStatus)? {
            Response::Status(status) => {
                let home = status.home.ok_or_else(|| {
                    ApiError::new(
                        ErrorCode::Unsupported,
                        "the machine's Rakumux doesn't say where its home folder is: update it",
                    )
                })?;
                let mut state = lock(&self.shared.state);
                if let Some(record) = state.records.iter_mut().find(|r| r.id == *machine) {
                    record.home = Some(home.clone());
                    state.dirty = true;
                }
                Ok(home)
            }
            other => Err(unexpected(&other)),
        }
    }

    /// One request and its answer on the machine's request connection,
    /// reconnecting once when it was closed meanwhile.
    fn exchange(&self, machine: &MachineId, request: &Request) -> Result<Response, ApiError> {
        let (url, token) = self.address(machine)?;
        let slot = {
            let mut state = lock(&self.shared.state);
            let live = state.live.entry(machine.clone()).or_default();
            live.last_used = Some(Instant::now());
            live.requests
                .get_or_insert_with(|| Arc::new(Mutex::new(None)))
                .clone()
        };
        let mut connection = lock(&slot);
        for attempt in 0..2 {
            if connection.is_none() {
                match open_requests(&url, &token) {
                    Ok((requests, version)) => {
                        *connection = Some(requests);
                        self.mark_connected(machine, version);
                    }
                    Err(e) => {
                        self.mark_unreachable(machine, &e);
                        return Err(unreachable(&url, &e));
                    }
                }
            }
            let Some(requests) = connection.as_mut() else {
                continue;
            };
            match exchange(requests, request) {
                Ok(response) => {
                    drop(connection);
                    self.ensure_watch(machine);
                    return Ok(response);
                }
                Err(e) => {
                    *connection = None;
                    if attempt == 1 {
                        self.mark_unreachable(machine, &e);
                        return Err(unreachable(&url, &e));
                    }
                }
            }
        }
        Err(ApiError::new(
            ErrorCode::DaemonUnavailable,
            "the machine didn't answer",
        ))
    }

    /// Remember the sessions in a machine's answer and list them here.
    fn take_response(
        &self,
        machine: &MachineId,
        local_scope: Option<&ScopeRoot>,
        response: Response,
    ) -> Response {
        let mut keep = |view: SessionView| -> SessionView {
            let mut state = lock(&self.shared.state);
            let scope = local_scope
                .cloned()
                .or_else(|| state.sessions.get(&view.id).map(|r| r.local_scope.clone()))
                .unwrap_or_else(|| view.scope.clone());
            let remote = Remote {
                machine: machine.clone(),
                local_scope: scope,
                view,
            };
            let out = match state.records.iter().find(|r| r.id == *machine) {
                Some(record) => stamped(&remote, record),
                None => remote.view.clone(),
            };
            state.sessions.insert(remote.view.id.clone(), remote);
            state.dirty = true;
            out
        };
        match response {
            Response::Session { session } => Response::Session {
                session: keep(session),
            },
            Response::Sessions { sessions } => Response::Sessions {
                sessions: sessions.into_iter().map(&mut keep).collect(),
            },
            other => other,
        }
    }

    /// Forget a session after the machine deleted it.
    pub fn forget(&self, id: &SessionId) {
        let removed = {
            let mut state = lock(&self.shared.state);
            let removed = state.sessions.remove(id);
            state.dirty |= removed.is_some();
            removed
        };
        if let Some(remote) = removed {
            self.publish(EventEnvelope {
                scope: remote.local_scope,
                session_id: id.clone(),
                at: now_ms(),
                event: SessionEvent::Removed,
            });
        }
    }

    fn mark_connected(&self, machine: &MachineId, version: String) {
        let mut state = lock(&self.shared.state);
        let live = state.live.entry(machine.clone()).or_default();
        live.state = Some(MachineState::Connected);
        live.error = None;
        live.version = Some(version);
    }

    fn mark_unreachable(&self, machine: &MachineId, error: &std::io::Error) {
        let mut state = lock(&self.shared.state);
        let live = state.live.entry(machine.clone()).or_default();
        live.state = Some(MachineState::Unreachable);
        live.error = Some(error.to_string());
        log_warn!("machines: {machine} unreachable: {error}");
    }

    /// Watch the machine's events unless that's already happening.
    fn ensure_watch(&self, machine: &MachineId) {
        {
            let mut state = lock(&self.shared.state);
            if !state.records.iter().any(|r| r.id == *machine) {
                return;
            }
            let live = state.live.entry(machine.clone()).or_default();
            live.last_used = Some(Instant::now());
            if live.watching {
                return;
            }
            live.watching = true;
        }
        let this = self.clone();
        let id = machine.clone();
        let spawned = std::thread::Builder::new()
            .name("ptyd-machine-events".into())
            .spawn(move || this.watch(&id));
        if spawned.is_err() {
            if let Some(live) = lock(&self.shared.state).live.get_mut(machine) {
                live.watching = false;
            }
        }
    }

    /// Follow a machine's events until its connection closes: first its
    /// sessions, then each change, passed on to subscribers.
    fn watch(&self, machine: &MachineId) {
        let result = (|| -> Result<(), std::io::Error> {
            let (url, token) = self
                .address(machine)
                .map_err(|e| std::io::Error::other(e.message))?;
            let Pipe {
                hello,
                mut reader,
                mut writer,
            } = pipe::connect(&url, &token)?;
            self.mark_connected(machine, hello.version);
            frame::write_control(
                &mut writer,
                &Request::Subscribe(SubscribeRequest {
                    all: true,
                    ..SubscribeRequest::default()
                }),
            )?;
            match read_response(&mut reader)? {
                Response::Subscribed => {}
                other => return Err(std::io::Error::other(format!("subscribe: {other:?}"))),
            }
            if let Some(live) = lock(&self.shared.state).live.get_mut(machine) {
                live.watch = Some(writer);
            }
            // Catch up on what changed while not connected.
            if let Ok(Response::Sessions { sessions }) = self.exchange(
                machine,
                &Request::SessionList {
                    scope: None,
                    scopes: Vec::new(),
                    include_dead: true,
                    all: true,
                },
            ) {
                self.replace_sessions(machine, sessions);
            }
            loop {
                match frame::read_frame(&mut reader)? {
                    Some(Frame::Control(payload)) => {
                        let envelope: EventEnvelope = frame::parse_control(&payload)?;
                        self.take_event(machine, envelope);
                    }
                    Some(Frame::Data(_)) => {}
                    None => return Ok(()),
                }
            }
        })();
        let mut state = lock(&self.shared.state);
        if let Some(live) = state.live.get_mut(machine) {
            live.watching = false;
            live.watch = None;
            match result {
                Err(error) => {
                    log_debug!("machines: {machine} events ended: {error}");
                    live.state = Some(MachineState::Unreachable);
                    live.error = Some(error.to_string());
                }
                // Closed by the idle policy, or by the machine.
                Ok(()) => live.state = Some(MachineState::Idle),
            }
        }
    }

    /// A machine's whole session list: replaces what was kept for it, and
    /// tells subscribers what changed.
    fn replace_sessions(&self, machine: &MachineId, sessions: Vec<SessionView>) {
        let mut events = Vec::new();
        {
            let mut state = lock(&self.shared.state);
            let Some(record) = state.records.iter().find(|r| r.id == *machine).cloned() else {
                return;
            };
            let gone: Vec<SessionId> = state
                .sessions
                .iter()
                .filter(|(id, r)| r.machine == *machine && !sessions.iter().any(|s| s.id == **id))
                .map(|(id, _)| id.clone())
                .collect();
            for id in gone {
                if let Some(remote) = state.sessions.remove(&id) {
                    events.push(EventEnvelope {
                        scope: remote.local_scope,
                        session_id: id,
                        at: now_ms(),
                        event: SessionEvent::Removed,
                    });
                }
            }
            for view in sessions {
                let known = state.sessions.get(&view.id);
                let local_scope = known
                    .map(|r| r.local_scope.clone())
                    .unwrap_or_else(|| view.scope.clone());
                let changed = known.is_none_or(|r| r.view != view);
                let created = known.is_none();
                let remote = Remote {
                    machine: machine.clone(),
                    local_scope: local_scope.clone(),
                    view,
                };
                if changed {
                    let session = stamped(&remote, &record);
                    events.push(EventEnvelope {
                        scope: local_scope,
                        session_id: session.id.clone(),
                        at: now_ms(),
                        event: if created {
                            SessionEvent::Created { session }
                        } else {
                            SessionEvent::Updated { session }
                        },
                    });
                }
                state.sessions.insert(remote.view.id.clone(), remote);
            }
            state.dirty = true;
        }
        for event in events {
            self.publish(event);
        }
    }

    /// One of a machine's events: kept, and passed on as listed here.
    fn take_event(&self, machine: &MachineId, envelope: EventEnvelope) {
        let EventEnvelope {
            session_id,
            at,
            event,
            ..
        } = envelope;
        let (scope, event) = {
            let mut state = lock(&self.shared.state);
            let Some(record) = state.records.iter().find(|r| r.id == *machine).cloned() else {
                return;
            };
            let known = state
                .sessions
                .get(&session_id)
                .map(|r| r.local_scope.clone());
            match event {
                SessionEvent::Created { session } | SessionEvent::Updated { session } => {
                    let created = known.is_none();
                    let local_scope = known.unwrap_or_else(|| session.scope.clone());
                    let remote = Remote {
                        machine: machine.clone(),
                        local_scope: local_scope.clone(),
                        view: session,
                    };
                    let session = stamped(&remote, &record);
                    state.sessions.insert(session_id.clone(), remote);
                    state.dirty = true;
                    let event = if created {
                        SessionEvent::Created { session }
                    } else {
                        SessionEvent::Updated { session }
                    };
                    (local_scope, event)
                }
                // Reply text stays on its machine.
                SessionEvent::Reply { .. } | SessionEvent::Unknown => return,
                event => {
                    let Some(scope) = known else {
                        return;
                    };
                    match &event {
                        SessionEvent::Removed => {
                            state.sessions.remove(&session_id);
                        }
                        SessionEvent::Lifecycle { lifecycle } => {
                            if let Some(remote) = state.sessions.get_mut(&session_id) {
                                remote.view.lifecycle = *lifecycle;
                            }
                        }
                        SessionEvent::Exited { code } => {
                            if let Some(remote) = state.sessions.get_mut(&session_id) {
                                remote.view.status = SessionStatus::Exited;
                                remote.view.exit_code = *code;
                                remote.view.terminal = None;
                            }
                        }
                        SessionEvent::Reported { reported } => {
                            if let Some(remote) = state.sessions.get_mut(&session_id) {
                                remote.view.reported = Some(reported.clone());
                            }
                        }
                        _ => {}
                    }
                    state.dirty = true;
                    (scope, event)
                }
            }
        };
        self.publish(EventEnvelope {
            scope,
            session_id,
            at,
            event,
        });
    }

    fn publish(&self, envelope: EventEnvelope) {
        let kind = kind_of(&envelope.event);
        let mut subscribers = lock(&self.shared.subscribers);
        subscribers.retain(|subscriber| {
            if !wants(&subscriber.request, &envelope, kind) {
                return true;
            }
            subscriber.tx.send(envelope.clone()).is_ok()
        });
    }

    /// Whether any session of `machine` has an agent working.
    fn working(state: &State, machine: &MachineId) -> bool {
        state.sessions.values().any(|r| {
            r.machine == *machine
                && r.view.status == SessionStatus::Live
                && r.view.lifecycle == Lifecycle::Working
        })
    }
}

/// Close connections nothing needs, and save what changed.
fn housekeeping(shared: Weak<Shared>) {
    loop {
        std::thread::sleep(HOUSEKEEPING);
        let Some(shared) = shared.upgrade() else {
            return;
        };
        if shared.stop.load(Ordering::SeqCst) {
            return;
        }
        let mut state = lock(&shared.state);
        let idle: Vec<MachineId> = state
            .live
            .iter()
            .filter(|(id, live)| {
                (live.watching || live.requests.is_some())
                    && live.attaches == 0
                    && live.last_used.is_none_or(|t| t.elapsed() >= IDLE)
                    && !Machines::working(&state, id)
            })
            .map(|(id, _)| id.clone())
            .collect();
        for id in idle {
            if let Some(live) = state.live.get_mut(&id) {
                live.watch = None;
                live.requests = None;
                if live.state == Some(MachineState::Connected) {
                    live.state = Some(MachineState::Idle);
                }
                log_debug!("machines: {id} idle, connection closed");
            }
        }
        save(&shared.path, &mut state);
    }
}

fn save(path: &Path, state: &mut State) {
    if !state.dirty {
        return;
    }
    let saved = Saved {
        machines: state.records.clone(),
        sessions: state.sessions.values().cloned().collect(),
    };
    match serde_json::to_vec_pretty(&saved) {
        Ok(bytes) => match write_atomic(path, &bytes) {
            Ok(()) => state.dirty = false,
            Err(e) => log_warn!("machines: cannot save {}: {e}", path.display()),
        },
        Err(e) => log_warn!("machines: cannot encode: {e}"),
    }
}

fn open_requests(url: &str, token: &str) -> std::io::Result<(Requests, String)> {
    let Pipe {
        hello,
        reader,
        writer,
    } = pipe::connect(url, token)?;
    Ok((Requests { reader, writer }, hello.version))
}

fn exchange(requests: &mut Requests, request: &Request) -> std::io::Result<Response> {
    frame::write_control(&mut requests.writer, request)?;
    read_response(&mut requests.reader)
}

fn read_response(reader: &mut PipeReader) -> std::io::Result<Response> {
    loop {
        match frame::read_frame(reader)? {
            Some(Frame::Control(payload)) => return frame::parse_control(&payload),
            Some(Frame::Data(_)) => {}
            None => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "the machine closed the connection",
                ))
            }
        }
    }
}

/// A machine's session as listed here: under its local folder, saying where
/// it runs.
fn stamped(remote: &Remote, record: &Record) -> SessionView {
    let mut view = remote.view.clone();
    let remote_scope = view.scope.path().to_path_buf();
    view.scope = remote.local_scope.clone();
    view.is_self = false;
    view.machine = Some(MachineRef {
        id: record.id.clone(),
        name: record.name.clone(),
        scope: remote_scope,
    });
    view
}

fn machine_view(record: &Record, live: Option<&Live>) -> MachineView {
    MachineView {
        id: record.id.clone(),
        name: record.name.clone(),
        url: record.url.clone(),
        state: live.and_then(|l| l.state).unwrap_or(MachineState::Idle),
        version: live.and_then(|l| l.version.clone()),
        error: live.and_then(|l| l.error.clone()),
        added_at: record.added_at,
    }
}

fn kind_of(event: &SessionEvent) -> Option<EventKind> {
    Some(match event {
        SessionEvent::Created { .. } => EventKind::Created,
        SessionEvent::Updated { .. } => EventKind::Updated,
        SessionEvent::Removed => EventKind::Removed,
        SessionEvent::Exited { .. } => EventKind::Exited,
        SessionEvent::Lifecycle { .. } => EventKind::Lifecycle,
        SessionEvent::Reported { .. } => EventKind::Reported,
        SessionEvent::Activity => EventKind::Activity,
        SessionEvent::Reply { .. } => EventKind::Reply,
        SessionEvent::Unknown => return None,
    })
}

fn wants(request: &SubscribeRequest, envelope: &EventEnvelope, kind: Option<EventKind>) -> bool {
    let in_scope = request.all
        || request.scope.as_ref() == Some(&envelope.scope)
        || request.scopes.contains(&envelope.scope);
    let session =
        request.session_ids.is_empty() || request.session_ids.contains(&envelope.session_id);
    let kind = request.kinds.is_empty() || kind.is_some_and(|k| request.kinds.contains(&k));
    in_scope && session && kind
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn invalid(message: impl Into<String>) -> ApiError {
    ApiError::new(ErrorCode::InvalidArgument, message)
}

fn not_found_machine(id: &MachineId) -> ApiError {
    ApiError::new(ErrorCode::NotFound, format!("no machine {id}"))
}

fn unreachable(url: &str, error: &std::io::Error) -> ApiError {
    let code = if error.kind() == std::io::ErrorKind::PermissionDenied {
        ErrorCode::PermissionDenied
    } else {
        ErrorCode::DaemonUnavailable
    };
    ApiError::new(code, format!("{url}: {error}"))
}

fn unexpected(response: &Response) -> ApiError {
    match response {
        Response::Error { error } => error.clone(),
        other => ApiError::new(
            ErrorCode::Internal,
            format!("the machine answered unexpectedly: {other:?}"),
        ),
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn now_secs() -> i64 {
    now_ms() / 1000
}
