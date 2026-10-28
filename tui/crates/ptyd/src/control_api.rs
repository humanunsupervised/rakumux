//! Contract between the socket server (`server`) and the control layer
//! (`control`). The server owns connections and framing; the control layer
//! owns sessions, the registry, identity, permissions, waits and audit.

use crate::handoff::ControlHandoff;
use session_core::api::{
    ApiError, AttachRequest, CallerInfo, EventEnvelope, Hello, Request, Response, SubscribeRequest,
};
use session_core::ids::{SessionId, TerminalId};
use session_core::model::SessionView;
use std::sync::mpsc::Receiver;

/// An authenticated connection's identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Caller {
    pub info: CallerInfo,
    /// Client name from the hello ("tui", "cli", …).
    pub client: String,
    /// `Hello::max_blocking_ms`: no request may block this caller longer.
    pub max_blocking_ms: Option<u64>,
}

impl Caller {
    pub fn is_user(&self) -> bool {
        matches!(self.info, CallerInfo::User)
    }

    /// The user, reaching other machines' sessions through this daemon:
    /// not another machine's daemon relayed here
    /// ([`session_core::api::MACHINE_CLIENT`]), which sees this machine's own.
    pub fn reaches_machines(&self) -> bool {
        self.is_user() && self.client != session_core::api::MACHINE_CLIENT
    }

    pub fn session_id(&self) -> Option<&SessionId> {
        match &self.info {
            CallerInfo::Session { id, .. } => Some(id),
            CallerInfo::User => None,
        }
    }
}

/// Session control. Implemented by `control::ControlService`.
pub trait Control: Send + Sync + 'static {
    /// Attach to a session that runs on another machine (capability
    /// `machines`, user callers): `None` when it's this machine's.
    fn remote_attach(
        &self,
        caller: &Caller,
        request: &AttachRequest,
    ) -> Option<Result<crate::machines::RemoteAttach, ApiError>> {
        let _ = (caller, request);
        None
    }

    /// User-only standalone shell, with no registry or recovery record.
    fn open_terminal(
        &self,
        caller: &Caller,
        request: &session_core::api::OpenTerminal,
    ) -> Result<crate::host::PaneAttachment, ApiError> {
        let _ = (caller, request);
        Err(ApiError::new(
            session_core::api::ErrorCode::Unsupported,
            "temporary terminal panes are unsupported",
        ))
    }

    /// Reattach only an existing standalone terminal after a reconnect.
    fn attach_terminal(
        &self,
        caller: &Caller,
        request: &session_core::api::TerminalAttachRequest,
    ) -> Result<crate::host::PaneAttachment, ApiError> {
        let _ = (caller, request);
        Err(ApiError::new(
            session_core::api::ErrorCode::Unsupported,
            "temporary terminal panes are unsupported",
        ))
    }

    /// Verify the hello's credentials and protocol version.
    fn authenticate(&self, hello: &Hello) -> Result<Caller, ApiError>;

    /// Handle a request/response op: every `Request` except `Hello`,
    /// `Attach`, `Subscribe`, `DaemonStatus`, `DaemonShutdown` and
    /// `DaemonRestart`, which the server handles.
    /// May block (e.g. `SessionWait`); the server calls it on the
    /// connection's own thread.
    fn handle(&self, caller: &Caller, request: Request) -> Response;

    /// Check that `caller` may attach with the requested role and resolve
    /// the session's live terminal.
    fn prepare_attach(
        &self,
        caller: &Caller,
        request: &AttachRequest,
    ) -> Result<(TerminalId, SessionView), ApiError>;

    /// Session-level events visible to `caller`, filtered per `request`.
    fn subscribe(
        &self,
        caller: &Caller,
        request: &SubscribeRequest,
    ) -> Result<Receiver<EventEnvelope>, ApiError>;

    /// Number of sessions in the registry (for `DaemonStatus`).
    fn session_count(&self) -> u32;

    /// Flush state before the daemon exits (persist scrollback, mark
    /// sessions, close the registry).
    fn shutdown(&self);

    /// The user asked the daemon to stop (`daemon_shutdown`): the sessions
    /// end on purpose, so the next start revives none of them.
    fn stopped_by_user(&self) {}

    /// Revive the sessions that were running when the previous daemon ended
    /// without the user stopping it (a crash, a reboot). Returns how many.
    fn revive_interrupted(&self) -> usize {
        0
    }

    /// Like `shutdown`, for a restart in place: also returns what the next
    /// daemon image needs to keep serving the running sessions.
    fn handoff(&self) -> ControlHandoff {
        self.shutdown();
        ControlHandoff::default()
    }
}
