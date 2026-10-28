//! Session control: the registry (SQLite, daemon-owned), projects, scope
//! confinement, identity tokens, permission policy, waits, lifecycle
//! tracking, git context and audit.
//!
//! Submodules:
//! - `registry`: `sessions_v2`, `projects` and `audit_log` tables.
//! - `auth`: hello verification, in-memory session tokens.
//! - `policy`: stage-A permission matrix for session callers.
//! - `service`: request handlers and host-event handling.
//! - `waits`, `lifecycle`, `events`, `audit`, `env`, `names`, `saved`:
//!   wait conditions, Done/Idle tracking, subscriber fan-out, audit entries,
//!   child environment, generated names, saved-scrollback reads.
//! - `git_context`: per-session git context cache and its poll.
//! - `host_events`: threads feeding host events into the service.

mod activity;
mod audit;
mod auth;
mod env;
mod events;
mod git_context;
mod host_events;
mod import;
mod keep_awake;
mod lifecycle;
mod names;
mod panes;
mod places;
mod policy;
mod registry;
mod replies;
mod saved;
mod service;
mod waits;
mod working_directory;

use crate::control_api::{Caller, Control};
use crate::detect::{DetectInput, Detection};
use crate::handoff::ControlHandoff;
use crate::host::TerminalHost;
use crate::log_warn;
use session_core::api::{
    ApiError, AttachRequest, EventEnvelope, Hello, Request, Response, SubscribeRequest,
};
use session_core::ids::{SessionId, TerminalId, Token};
use session_core::model::{ScopeRoot, SessionView};
use session_core::paths::Layout;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::mpsc::Receiver;
use std::sync::Arc;

/// Limits on sessions that sessions create (`LimitExceeded`). Sessions the
/// user creates don't count against them and aren't limited.
#[derive(Debug, Clone)]
pub struct Limits {
    /// Running sessions per project that sessions created.
    pub max_live_per_scope: u32,
    /// Running sessions one session created.
    pub max_children_per_parent: u32,
    /// Nesting: a session the user created is at depth 0, one it creates at 1.
    pub max_depth: u8,
    /// Creates per session in any 60 seconds.
    pub max_creates_per_minute: u32,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_live_per_scope: 8,
            max_children_per_parent: 4,
            max_depth: 2,
            max_creates_per_minute: 10,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ControlConfig {
    pub layout: Layout,
    /// Token user clients present (from the registration file).
    pub user_token: Token,
    /// Injected into sessions as `RAKUMUX_SOCKET`.
    pub socket: PathBuf,
    /// Injected into sessions as `RAKUMUX_BIN` when known.
    pub rakumux_bin: Option<PathBuf>,
    pub version: String,
    pub limits: Limits,
}

/// Agent detection used for lifecycle tracking. `detect::detect` in the
/// daemon; tests substitute their own.
pub type Detector = fn(&DetectInput<'_>) -> Detection;

/// The daemon's control layer (`control_api::Control`).
pub struct ControlService {
    inner: Arc<service::Inner>,
    /// Sessions on other machines, listed and used through this daemon.
    machines: crate::machines::Machines,
}

impl ControlService {
    /// Open (and migrate) the registry, reconcile it with the host's live
    /// terminals (none after a daemon start → every session Dead), and start
    /// the host-event processing thread.
    pub fn open(config: ControlConfig, host: Arc<dyn TerminalHost>) -> std::io::Result<Self> {
        Self::open_with_detector(config, host, crate::detect::detect)
    }

    /// [`ControlService::open`] with a custom agent detector.
    pub fn open_with_detector(
        config: ControlConfig,
        host: Arc<dyn TerminalHost>,
        detector: Detector,
    ) -> std::io::Result<Self> {
        Self::open_inner(config, host, detector, None)
    }

    /// [`ControlService::open`] after a restart in place: the host already
    /// runs the handed-over terminals, and their sessions get back their
    /// tokens, spawn environments and conversation bindings.
    pub fn open_after_handoff(
        config: ControlConfig,
        host: Arc<dyn TerminalHost>,
        handed: ControlHandoff,
    ) -> std::io::Result<Self> {
        Self::open_inner(config, host, crate::detect::detect, Some(handed))
    }

    fn open_inner(
        config: ControlConfig,
        host: Arc<dyn TerminalHost>,
        detector: Detector,
        handed: Option<ControlHandoff>,
    ) -> std::io::Result<Self> {
        let registry = registry::Registry::open(&config.layout.registry_db(), service::now_ms())?;
        let machines = crate::machines::Machines::open(&config.layout.home);
        // Subscribe before adopting terminals so no event falls in between.
        let host_events = host.subscribe();
        let inner = Arc::new(service::Inner::new(config, host, registry, detector));
        inner.adopt_host_terminals(handed);
        host_events::start(&inner, host_events)?;
        keep_awake::start(&inner);
        Ok(Self { inner, machines })
    }

    /// Requests about other machines and their sessions, user callers
    /// only; `Err` gives back the ones for this machine.
    fn route(&self, caller: &Caller, request: Request) -> Result<Response, Request> {
        let answer = |result: Result<Response, ApiError>| {
            result.unwrap_or_else(|error| Response::Error { error })
        };
        match request {
            Request::MachineAdd { name, url, token } => Ok(answer(
                user_only(caller)
                    .and_then(|()| self.machines.add(&name, &url, &token))
                    .map(|machine| Response::Machine { machine }),
            )),
            Request::MachineList => Ok(answer(user_only(caller).map(|()| Response::Machines {
                machines: self.machines.list(),
            }))),
            Request::MachineRemove { id } => Ok(answer(
                user_only(caller)
                    .and_then(|()| self.machines.remove(&id))
                    .map(|()| Response::Ok),
            )),
            Request::SessionCreate(create) => match create.machine.clone() {
                Some(machine) => Ok(answer(
                    user_only(caller)
                        .and_then(|()| self.machines.create(&machine, create))
                        .map(|session| Response::Session { session }),
                )),
                None => Err(Request::SessionCreate(create)),
            },
            Request::SessionList {
                scope,
                scopes,
                include_dead,
                all,
            } if caller.is_user() => {
                let wanted: Option<Vec<ScopeRoot>> =
                    (!all).then(|| scope.iter().chain(scopes.iter()).cloned().collect());
                let local = self.inner.handle(
                    caller,
                    Request::SessionList {
                        scope,
                        scopes,
                        include_dead,
                        all,
                    },
                );
                Ok(match local {
                    Response::Sessions { mut sessions } => {
                        sessions.extend(self.machines.sessions(wanted.as_deref(), include_dead));
                        Response::Sessions { sessions }
                    }
                    other => other,
                })
            }
            request => {
                let Some(id) = remote_target(&request).cloned() else {
                    return Err(request);
                };
                // Other callers get this machine's answer: not found.
                let Some(machine) = self.machines.owner(&id).filter(|_| caller.is_user()) else {
                    return Err(request);
                };
                let deleting = matches!(request, Request::SessionDelete { .. });
                let response = answer(self.machines.forward(&machine, &request));
                if deleting && response == Response::Ok {
                    self.machines.forget(&id);
                }
                Ok(response)
            }
        }
    }
}

/// The session a request is about, for requests another machine can answer.
fn remote_target(request: &Request) -> Option<&SessionId> {
    match request {
        Request::SessionGet { id }
        | Request::SessionRename { id, .. }
        | Request::SessionKill { id, .. }
        | Request::SessionDelete { id }
        | Request::SessionSubmit { id, .. }
        | Request::SessionTouch { id }
        | Request::SessionTurn { id }
        | Request::SessionPin { id, .. }
        | Request::SessionTranscript { id, .. } => Some(id),
        Request::SessionRevive(revive) => Some(&revive.id),
        Request::SessionRead(read) => Some(&read.id),
        Request::SessionWait(wait) => Some(&wait.id),
        Request::SessionSend(send) => Some(&send.id),
        _ => None,
    }
}

fn user_only(caller: &Caller) -> Result<(), ApiError> {
    if caller.is_user() {
        Ok(())
    } else {
        Err(ApiError::new(
            session_core::api::ErrorCode::PermissionDenied,
            "only the user reaches other machines",
        ))
    }
}

/// One stream of this machine's and other machines' events.
fn merge(
    local: Receiver<EventEnvelope>,
    remote: Receiver<EventEnvelope>,
) -> Receiver<EventEnvelope> {
    let (tx, rx) = std::sync::mpsc::channel();
    let spawned = std::thread::Builder::new()
        .name("ptyd-events-merge".into())
        .spawn(move || loop {
            match local.recv_timeout(std::time::Duration::from_millis(250)) {
                Ok(envelope) => {
                    if tx.send(envelope).is_err() {
                        return;
                    }
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return,
            }
            while let Ok(envelope) = remote.try_recv() {
                if tx.send(envelope).is_err() {
                    return;
                }
            }
        });
    if let Err(error) = spawned {
        log_warn!("cannot follow other machines' events: {error}");
    }
    rx
}

impl Drop for ControlService {
    fn drop(&mut self) {
        self.machines.close();
        // Stops the event threads; they only hold weak references.
        self.inner.stop.store(true, Ordering::SeqCst);
    }
}

impl Control for ControlService {
    fn open_terminal(
        &self,
        caller: &Caller,
        request: &session_core::api::OpenTerminal,
    ) -> Result<crate::host::PaneAttachment, ApiError> {
        self.inner.open_terminal(caller, request)
    }

    fn attach_terminal(
        &self,
        caller: &Caller,
        request: &session_core::api::TerminalAttachRequest,
    ) -> Result<crate::host::PaneAttachment, ApiError> {
        self.inner.attach_terminal(caller, request)
    }

    fn authenticate(&self, hello: &Hello) -> Result<Caller, ApiError> {
        self.inner.authenticate(hello)
    }

    fn handle(&self, caller: &Caller, request: Request) -> Response {
        match self.route(caller, request) {
            Ok(response) => response,
            Err(request) => self.inner.handle(caller, request),
        }
    }

    fn remote_attach(
        &self,
        caller: &Caller,
        request: &AttachRequest,
    ) -> Option<Result<crate::machines::RemoteAttach, ApiError>> {
        let machine = self
            .machines
            .owner(&request.id)
            .filter(|_| caller.is_user())?;
        Some(
            self.machines
                .attach(&machine)
                .map(|(pipe, guard)| crate::machines::RemoteAttach {
                    pipe,
                    guard,
                    machine,
                    machines: self.machines.clone(),
                }),
        )
    }

    fn prepare_attach(
        &self,
        caller: &Caller,
        request: &AttachRequest,
    ) -> Result<(TerminalId, SessionView), ApiError> {
        self.inner.prepare_attach(caller, request)
    }

    fn subscribe(
        &self,
        caller: &Caller,
        request: &SubscribeRequest,
    ) -> Result<Receiver<EventEnvelope>, ApiError> {
        let local = self.inner.subscribe(caller, request)?;
        if !caller.is_user() {
            return Ok(local);
        }
        Ok(merge(local, self.machines.subscribe(request)))
    }

    fn session_count(&self) -> u32 {
        self.inner.session_count()
    }

    fn shutdown(&self) {
        self.machines.close();
        self.inner.shutdown()
    }

    fn handoff(&self) -> ControlHandoff {
        self.inner.handoff()
    }

    fn stopped_by_user(&self) {
        self.inner.stopped_by_user()
    }

    fn revive_interrupted(&self) -> usize {
        self.inner.revive_interrupted()
    }
}
