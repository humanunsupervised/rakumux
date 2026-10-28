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
//!
//! What a machine says is taken as data from elsewhere: a session it lists
//! must be its own (never one of this machine's, another machine's, or one it
//! lists from a machine of its own), at most [`MAX_SESSIONS`] of them, each
//! view at most [`MAX_VIEW_BYTES`], with control characters removed from its
//! text; and it gets [`REQUEST_TIMEOUT`] to answer a request.

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
/// The most sessions listed from one machine.
pub const MAX_SESSIONS: usize = 500;
/// The largest session view kept from a machine, as JSON.
pub const MAX_VIEW_BYTES: usize = 256 * 1024;
/// How long a machine may take to answer a request.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// Extra time on top of a forwarded wait's own `timeout_ms`.
const WAIT_GRACE: Duration = Duration::from_secs(10);

/// Whether this machine has a session with an id: another machine's never
/// stands in for it.
pub type IsLocal = Arc<dyn Fn(&SessionId) -> bool + Send + Sync>;

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
    is_local: IsLocal,
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
    /// the housekeeping thread. `is_local` tells this machine's sessions.
    pub fn open(home: &Path, is_local: IsLocal) -> Self {
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
                .filter_map(|remote| {
                    let view = cleaned(&remote.view)?;
                    Some((view.id.clone(), Remote { view, ..remote }))
                })
                .collect(),
            records: saved.machines,
            ..State::default()
        };
        let shared = Arc::new(Shared {
            path,
            state: Mutex::new(state),
            subscribers: Mutex::new(Vec::new()),
            stop: AtomicBool::new(false),
            is_local,
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
        let version = short_text(&requests.1);
        let home = match exchange(&mut requests.0, &Request::DaemonStatus, REQUEST_TIMEOUT) {
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
        let machine = lock(&self.shared.state)
            .sessions
            .get(id)
            .map(|remote| remote.machine.clone())?;
        (!(self.shared.is_local)(id)).then_some(machine)
    }

    /// Other machines' sessions listed under `scopes` (every one with
    /// `None`), as last seen. Reconnects to their machines meanwhile, so
    /// changes follow as events.
    pub fn sessions(&self, scopes: Option<&[ScopeRoot]>, include_dead: bool) -> Vec<SessionView> {
        let (views, machines) = {
            let state = lock(&self.shared.state);
            let mut machines = Vec::new();
            let views: Vec<SessionView> = state
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
            .into_iter()
            .filter(|view| !(self.shared.is_local)(&view.id))
            .collect()
    }

    /// Forward `request` to the machine `id` runs on and answer with what it
    /// said, its sessions as listed here.
    pub fn forward(&self, machine: &MachineId, request: &Request) -> Result<Response, ApiError> {
        let response = match request {
            // A wait holds its connection for as long as it waits: not the
            // one every other request to the machine takes turns on.
            Request::SessionWait(wait) => {
                let limit = Duration::from_millis(wait.timeout_ms).saturating_add(WAIT_GRACE);
                self.exchange_alone(machine, request, limit)?
            }
            request => self.exchange(machine, request)?,
        };
        self.take_response(machine, None, response)
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
        // The clone the prefill types is for a folder that's new there.
        let known = match self.exchange(machine, &Request::ProjectList)? {
            Response::Projects { projects } => {
                projects.iter().any(|p| p.path.path() == remote_folder)
            }
            _ => false,
        };
        let prefill = if known { None } else { create.prefill.clone() };
        match self.exchange(
            machine,
            &Request::ProjectAdd {
                path: remote_folder.clone(),
                create: true,
            },
        )? {
            Response::Project { .. } => {}
            other => return Err(unexpected(&other)),
        }
        let request = Request::SessionCreate(CreateSession {
            scope: Some(ScopeRoot::from_canonical(remote_folder)),
            cwd: None,
            // The machine's own environment, not this one's.
            env: Vec::new(),
            machine: None,
            prefill,
            ..create
        });
        match self.exchange(machine, &request)? {
            Response::Session { session } => {
                let response =
                    self.take_response(machine, Some(&local_scope), Response::Session { session })?;
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

    /// A session view from `machine` as it's listed here: refused unless
    /// it's a session of that machine's own.
    pub fn stamp(&self, machine: &MachineId, view: SessionView) -> Result<SessionView, ApiError> {
        let view = self.admit(view).ok_or_else(not_its_own)?;
        let state = lock(&self.shared.state);
        let local_scope = match state.sessions.get(&view.id) {
            Some(known) if known.machine != *machine => return Err(not_its_own()),
            Some(known) => known.local_scope.clone(),
            None => view.scope.clone(),
        };
        let record = state
            .records
            .iter()
            .find(|r| r.id == *machine)
            .ok_or_else(|| not_found_machine(machine))?;
        Ok(stamped(
            &Remote {
                machine: machine.clone(),
                local_scope,
                view,
            },
            record,
        ))
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
            match exchange(requests, request, REQUEST_TIMEOUT) {
                Ok(response) => {
                    drop(connection);
                    self.ensure_watch(machine);
                    return Ok(response);
                }
                Err(e) => {
                    *connection = None;
                    // A connection closed while it lay unused is opened
                    // again; a request that wasn't answered isn't sent twice.
                    if attempt == 1 || e.kind() == std::io::ErrorKind::TimedOut {
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

    /// One request on a connection of its own, closed after its answer, which
    /// may take up to `limit`.
    fn exchange_alone(
        &self,
        machine: &MachineId,
        request: &Request,
        limit: Duration,
    ) -> Result<Response, ApiError> {
        let (url, token) = self.address(machine)?;
        if let Some(live) = lock(&self.shared.state).live.get_mut(machine) {
            live.last_used = Some(Instant::now());
        }
        let (mut requests, version) = open_requests(&url, &token).map_err(|e| {
            self.mark_unreachable(machine, &e);
            unreachable(&url, &e)
        })?;
        self.mark_connected(machine, version);
        exchange(&mut requests, request, limit).map_err(|e| unreachable(&url, &e))
    }

    /// Remember the sessions in a machine's answer and list them here. Only
    /// its own: a list leaves the others out, and an answer about one of
    /// them is refused.
    fn take_response(
        &self,
        machine: &MachineId,
        local_scope: Option<&ScopeRoot>,
        response: Response,
    ) -> Result<Response, ApiError> {
        Ok(match response {
            Response::Session { session } => Response::Session {
                session: self
                    .keep(machine, local_scope, session)
                    .ok_or_else(not_its_own)?,
            },
            Response::Sessions { sessions } => Response::Sessions {
                sessions: sessions
                    .into_iter()
                    .filter_map(|view| self.keep(machine, local_scope, view))
                    .collect(),
            },
            Response::Error { error } => Response::Error {
                error: clean_error(error),
            },
            other => other,
        })
    }

    /// Keep a session `machine` sent when it's its own, and give it as
    /// listed here.
    fn keep(
        &self,
        machine: &MachineId,
        local_scope: Option<&ScopeRoot>,
        view: SessionView,
    ) -> Option<SessionView> {
        let view = self.admit(view)?;
        let mut state = lock(&self.shared.state);
        let record = state.records.iter().find(|r| r.id == *machine)?.clone();
        let scope = match state.sessions.get(&view.id) {
            Some(known) if known.machine != *machine => return None,
            Some(known) => local_scope
                .cloned()
                .unwrap_or_else(|| known.local_scope.clone()),
            None if count_of(&state, machine) >= MAX_SESSIONS => return None,
            None => local_scope.cloned().unwrap_or_else(|| view.scope.clone()),
        };
        let remote = Remote {
            machine: machine.clone(),
            local_scope: scope,
            view,
        };
        let out = stamped(&remote, &record);
        state.sessions.insert(remote.view.id.clone(), remote);
        state.dirty = true;
        Some(out)
    }

    /// A view a machine sent, cleaned, unless it can't be a session of its
    /// own: one of this machine's, one it lists from a machine of its own, or
    /// one too large to keep.
    fn admit(&self, view: SessionView) -> Option<SessionView> {
        if view.machine.is_some() || (self.shared.is_local)(&view.id) {
            return None;
        }
        cleaned(&view)
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
        live.version = Some(short_text(&version));
    }

    fn mark_unreachable(&self, machine: &MachineId, error: &std::io::Error) {
        let error = clean_text(&error.to_string());
        let mut state = lock(&self.shared.state);
        let live = state.live.entry(machine.clone()).or_default();
        live.state = Some(MachineState::Unreachable);
        log_warn!("machines: {machine} unreachable: {error}");
        live.error = Some(error);
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
            reader.set_deadline(Some(Instant::now() + REQUEST_TIMEOUT));
            match read_response(&mut reader)? {
                Response::Subscribed => {}
                other => return Err(std::io::Error::other(format!("subscribe: {other:?}"))),
            }
            reader.set_deadline(None);
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
                    live.error = Some(clean_text(&error.to_string()));
                }
                // Closed by the idle policy, or by the machine.
                Ok(()) => live.state = Some(MachineState::Idle),
            }
        }
    }

    /// A machine's whole session list: replaces what was kept for it, and
    /// tells subscribers what changed.
    fn replace_sessions(&self, machine: &MachineId, sessions: Vec<SessionView>) {
        let sessions: Vec<SessionView> = sessions
            .into_iter()
            .filter_map(|view| self.admit(view))
            .collect();
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
            let mut kept = count_of(&state, machine);
            for view in sessions {
                let known = state.sessions.get(&view.id);
                match known {
                    Some(known) if known.machine != *machine => continue,
                    None if kept >= MAX_SESSIONS => continue,
                    None => kept += 1,
                    Some(_) => {}
                }
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
        // About a session of its own, its text cleaned.
        if (self.shared.is_local)(&session_id) {
            return;
        }
        let Some(event) = cleaned(&event) else {
            return;
        };
        let (scope, event) = {
            let mut state = lock(&self.shared.state);
            let Some(record) = state.records.iter().find(|r| r.id == *machine).cloned() else {
                return;
            };
            let known = match state.sessions.get(&session_id) {
                Some(remote) if remote.machine != *machine => return,
                known => known.map(|r| r.local_scope.clone()),
            };
            match event {
                SessionEvent::Created { session } | SessionEvent::Updated { session } => {
                    if session.id != session_id || session.machine.is_some() {
                        return;
                    }
                    if known.is_none() && count_of(&state, machine) >= MAX_SESSIONS {
                        return;
                    }
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

/// Send `request` and read its answer, waiting at most `limit` for it.
fn exchange(
    requests: &mut Requests,
    request: &Request,
    limit: Duration,
) -> std::io::Result<Response> {
    frame::write_control(&mut requests.writer, request)?;
    requests.reader.set_deadline(Some(Instant::now() + limit));
    let response = read_response(&mut requests.reader);
    requests.reader.set_deadline(None);
    response
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

/// How many sessions are listed from `machine`.
fn count_of(state: &State, machine: &MachineId) -> usize {
    state
        .sessions
        .values()
        .filter(|r| r.machine == *machine)
        .count()
}

/// `value` with control characters removed from its text, unless it's
/// larger than [`MAX_VIEW_BYTES`] as JSON.
fn cleaned<T: serde::Serialize + serde::de::DeserializeOwned>(value: &T) -> Option<T> {
    let bytes = serde_json::to_vec(value).ok()?;
    if bytes.len() > MAX_VIEW_BYTES {
        return None;
    }
    let mut json: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    clean_value(&mut json);
    serde_json::from_value(json).ok()
}

/// Remove control characters from every string in `value`, keys too.
fn clean_value(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::String(text) => *text = clean_text(text),
        serde_json::Value::Array(items) => items.iter_mut().for_each(clean_value),
        serde_json::Value::Object(map) => {
            *map = std::mem::take(map)
                .into_iter()
                .map(|(key, mut value)| {
                    clean_value(&mut value);
                    (clean_text(&key), value)
                })
                .collect();
        }
        _ => {}
    }
}

/// `text` without the control characters a terminal would act on rather
/// than show (newlines and tabs stay).
pub fn clean_text(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_control() || matches!(c, '\n' | '\t'))
        .collect()
}

/// A short text from a machine (its version), cleaned and cut.
fn short_text(text: &str) -> String {
    clean_text(text).chars().take(MAX_NAME).collect()
}

/// A machine's error with its text cleaned.
pub fn clean_error(mut error: ApiError) -> ApiError {
    error.message = clean_text(&error.message);
    if let Some(details) = error.details.as_mut() {
        clean_value(details);
    }
    error
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
    ApiError::new(code, clean_text(&format!("{url}: {error}")))
}

/// The answer about a session that isn't the machine's own to give.
fn not_its_own() -> ApiError {
    ApiError::new(
        ErrorCode::PermissionDenied,
        "the machine answered about a session that isn't its own",
    )
}

fn unexpected(response: &Response) -> ApiError {
    match response {
        Response::Error { error } => clean_error(error.clone()),
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

#[cfg(test)]
mod tests {
    use super::*;
    use session_core::model::{Creator, LaunchSpec};

    const LOCAL: &str = "01J000000000000000000LOCAL";

    fn machines(home: &Path) -> Machines {
        let machines = Machines::open(home, Arc::new(|id: &SessionId| id.as_str() == LOCAL));
        let mut state = lock(&machines.shared.state);
        for (id, name) in [
            ("01J00000000000000000000MA1", "a"),
            ("01J00000000000000000000MA2", "b"),
        ] {
            state.records.push(Record {
                id: MachineId::from_string(id),
                name: name.into(),
                url: format!("http://127.0.0.1:9/{name}"),
                token: "t".into(),
                added_at: 0,
                home: None,
            });
        }
        drop(state);
        machines
    }

    fn a() -> MachineId {
        MachineId::from_string("01J00000000000000000000MA1")
    }

    fn b() -> MachineId {
        MachineId::from_string("01J00000000000000000000MA2")
    }

    fn view(id: &str, name: &str) -> SessionView {
        SessionView {
            pinned: false,
            git: None,
            auto_named: false,
            description: None,
            notes: None,
            labels: Default::default(),
            private: false,
            id: SessionId::from_string(id),
            name: name.into(),
            scope: ScopeRoot::from_canonical("/home/sprite/web".into()),
            is_self: false,
            cwd: "/home/sprite/web".into(),
            status: SessionStatus::Live,
            exit_code: None,
            lifecycle: Lifecycle::Idle,
            agent: None,
            reported: None,
            activity: None,
            created_by: Creator::User,
            parent_id: None,
            depth: 0,
            children: vec![],
            launch: LaunchSpec::default(),
            resume: None,
            created_at: 1,
            last_active_at: 2,
            last_message_at: None,
            terminal: None,
            agent_output: None,
            user_prompt: None,
            agent_turn: None,
            places: vec![],
            machine: None,
        }
    }

    fn event(session: SessionView) -> EventEnvelope {
        EventEnvelope {
            scope: session.scope.clone(),
            session_id: session.id.clone(),
            at: 1,
            event: SessionEvent::Updated { session },
        }
    }

    fn listed(machines: &Machines) -> Vec<(String, String, MachineId)> {
        lock(&machines.shared.state)
            .sessions
            .values()
            .map(|r| {
                (
                    r.view.id.to_string(),
                    r.view.name.clone(),
                    r.machine.clone(),
                )
            })
            .collect()
    }

    #[test]
    fn a_machine_speaks_only_for_sessions_of_its_own() {
        let home = tempfile::tempdir().unwrap();
        let machines = machines(home.path());
        machines.take_event(&a(), event(view("01J0000000000000000000000S", "web")));
        // Another machine can't take it over, change it or remove it.
        machines.take_event(&b(), event(view("01J0000000000000000000000S", "stolen")));
        machines.take_event(
            &b(),
            EventEnvelope {
                event: SessionEvent::Removed,
                ..event(view("01J0000000000000000000000S", "web"))
            },
        );
        // Nor one of this machine's, one it lists from a machine of its own,
        // or one under another id than the event's.
        machines.take_event(&b(), event(view(LOCAL, "shadow")));
        let mut relayed = view("01J0000000000000000000000R", "relayed");
        relayed.machine = Some(MachineRef {
            id: a(),
            name: "a".into(),
            scope: "/x".into(),
        });
        machines.take_event(&b(), event(relayed));
        machines.take_event(
            &b(),
            EventEnvelope {
                session_id: SessionId::from_string("01J0000000000000000000000T"),
                ..event(view("01J0000000000000000000000U", "other id"))
            },
        );
        assert_eq!(
            listed(&machines),
            vec![("01J0000000000000000000000S".into(), "web".into(), a())]
        );

        // Lists too.
        machines.replace_sessions(
            &b(),
            vec![
                view("01J0000000000000000000000S", "stolen"),
                view(LOCAL, "shadow"),
                view("01J0000000000000000000000V", "its own"),
            ],
        );
        let mut all = listed(&machines);
        all.sort();
        assert_eq!(
            all,
            vec![
                ("01J0000000000000000000000S".into(), "web".into(), a()),
                ("01J0000000000000000000000V".into(), "its own".into(), b()),
            ]
        );
    }

    #[test]
    fn this_machines_sessions_are_never_another_machines() {
        let home = tempfile::tempdir().unwrap();
        let machines = machines(home.path());
        // Kept from before this check, or when the id became local later.
        let local = Remote {
            machine: a(),
            local_scope: ScopeRoot::from_canonical("/repo".into()),
            view: view(LOCAL, "shadow"),
        };
        lock(&machines.shared.state)
            .sessions
            .insert(local.view.id.clone(), local);
        assert_eq!(machines.owner(&SessionId::from_string(LOCAL)), None);
        assert!(machines.stamp(&a(), view(LOCAL, "shadow")).is_err());
        assert!(machines.sessions(None, true).is_empty());
    }

    #[test]
    fn what_a_machine_says_is_cleaned_and_bounded() {
        let home = tempfile::tempdir().unwrap();
        let machines = machines(home.path());
        let mut loud = view(
            "01J0000000000000000000000S",
            "web\x1b]52;c;aGk=\x07\u{9b}2J",
        );
        loud.cwd = "/home/sprite/\x1b[31mweb".into();
        machines.take_event(&a(), event(loud));
        let kept = machines
            .stamp(&a(), view("01J0000000000000000000000S", "web"))
            .unwrap();
        assert_eq!(kept.machine.unwrap().name, "a");
        let state = lock(&machines.shared.state);
        let remote = state.sessions.values().next().unwrap();
        assert_eq!(remote.view.name, "web]52;c;aGk=2J");
        assert_eq!(remote.view.cwd, PathBuf::from("/home/sprite/[31mweb"));
        drop(state);

        let mut huge = view("01J0000000000000000000000H", "huge");
        huge.agent_output = Some("x".repeat(MAX_VIEW_BYTES));
        machines.take_event(&a(), event(huge));
        assert_eq!(listed(&machines).len(), 1);

        let many = (0..MAX_SESSIONS + 10)
            .map(|n| view(&format!("01J000000000000000000{n:05}"), "s"))
            .collect();
        machines.replace_sessions(&b(), many);
        assert_eq!(count_of(&lock(&machines.shared.state), &b()), MAX_SESSIONS);

        assert_eq!(clean_text("a\tb\nc\x1b[2J\x07\u{85}d"), "a\tb\nc[2Jd");
        let error = clean_error(ApiError::new(ErrorCode::NotFound, "no \x1b]0;x\x07such"));
        assert_eq!(error.message, "no ]0;xsuch");
    }
}
