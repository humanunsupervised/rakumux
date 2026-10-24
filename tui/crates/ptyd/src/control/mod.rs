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
mod lifecycle;
mod names;
mod panes;
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
use session_core::api::{
    ApiError, AttachRequest, EventEnvelope, Hello, Request, Response, SubscribeRequest,
};
use session_core::ids::{TerminalId, Token};
use session_core::model::SessionView;
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
        // Subscribe before adopting terminals so no event falls in between.
        let host_events = host.subscribe();
        let inner = Arc::new(service::Inner::new(config, host, registry, detector));
        inner.adopt_host_terminals(handed);
        host_events::start(&inner, host_events)?;
        Ok(Self { inner })
    }
}

impl Drop for ControlService {
    fn drop(&mut self) {
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
        self.inner.handle(caller, request)
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
        self.inner.subscribe(caller, request)
    }

    fn session_count(&self) -> u32 {
        self.inner.session_count()
    }

    fn shutdown(&self) {
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
