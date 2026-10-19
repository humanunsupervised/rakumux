//! Requests, responses, stream messages and events exchanged with the daemon.
//!
//! Connection flow (see `frame` for the byte format):
//! 1. Client sends [`Request::Hello`]; daemon answers [`Response::Hello`] or
//!    an error and closes.
//! 2. Request/response: one JSON control frame each way, strictly in turn.
//! 3. [`Request::Attach`] and [`Request::Subscribe`] turn the connection into
//!    a stream for its remaining lifetime. A client opens a new connection for
//!    other requests.
//!
//! Compatibility: new fields are optional; enums have an `Unknown` fallback;
//! an op never changes meaning (add a new op instead).

use crate::ids::SessionId;
use crate::model::{
    AgentInfo, Lifecycle, ProjectView, ReportResult, Reported, ScopeRoot, SessionStatus,
    SessionView,
};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

// ---------------------------------------------------------------- handshake

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Auth {
    /// A user client (TUI, CLI outside sessions). Token from the daemon's
    /// registration file.
    User { token: String },
    /// A process inside a session. Values from `RAKUMUX_SESSION_ID` and
    /// `RAKUMUX_SESSION_TOKEN`.
    Session { id: SessionId, token: String },
    /// A user client acting for the session that bound an agent's
    /// conversation (`Request::SessionBind`; capability `auth.delegated`).
    /// For agents whose tool calls arrive without the session's environment,
    /// such as OpenCode 2.x, whose MCP servers run in a service shared by
    /// every conversation. The caller is then that session: its project and
    /// policy apply. Never an escalation: the user token allows everything.
    Delegated {
        token: String,
        /// Agent kind, e.g. "opencode".
        agent: String,
        /// The agent's conversation id, e.g. OpenCode's `ses_…`.
        conversation: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hello {
    pub protocol: u32,
    /// e.g. "tui", "cli", "mcp".
    pub client: String,
    pub version: String,
    pub auth: Auth,
    /// Longest the client can wait on one request (e.g. an MCP host's tool
    /// timeout). The daemon never blocks a request longer: waits return
    /// unmatched with a timeout reason instead.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_blocking_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HelloOk {
    pub protocol: u32,
    pub version: String,
    /// Optional features this daemon supports; clients disable actions
    /// whose capability is missing. A client may only send an optional
    /// request field that changes an op's meaning when its capability is
    /// listed (an older daemon would silently ignore it). Reserved names:
    /// `session.update`, `session.focus`, `submit.wait`, `create.prompt`,
    /// `revive.restart`, `auth.delegated`, `approvals`, `audit`.
    #[serde(default)]
    pub capabilities: Vec<String>,
    pub caller: CallerInfo,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CallerInfo {
    User,
    Session { id: SessionId, scope: ScopeRoot },
}

// ---------------------------------------------------------------- requests

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Request {
    Hello(Hello),
    Ping,

    /// Sessions in a scope. User clients must pass `scope` or `scopes`
    /// (capability `projects`: the union of both) or `all`; session callers
    /// always get their own session's scope and may omit them.
    SessionList {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        scope: Option<ScopeRoot>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        scopes: Vec<ScopeRoot>,
        #[serde(default)]
        include_dead: bool,
        /// Every scope's sessions (capability `sessions.all`; user callers
        /// only, session callers get `out_of_scope`). `scope` and `scopes`
        /// are ignored when set.
        #[serde(default, skip_serializing_if = "is_false")]
        all: bool,
    },
    SessionGet {
        id: SessionId,
    },
    SessionCreate(CreateSession),
    /// User-only restore of saved sessions from a private archive directory
    /// containing `registry.sqlite3`, `sessions/` and optional `history/`.
    /// Preserves ids and metadata, refuses conflicts, and starts no programs.
    SessionImport {
        source: PathBuf,
    },
    SessionRename {
        id: SessionId,
        name: String,
    },
    /// Terminate the session's process. The record stays (status Exited).
    SessionKill {
        id: SessionId,
        /// Also kill sessions this one created, recursively.
        #[serde(default)]
        tree: bool,
    },
    /// Terminate if running, then remove the record and its scrollback.
    SessionDelete {
        id: SessionId,
    },
    /// Start a new terminal for a Dead or Exited session (same id, cwd,
    /// launch spec, resume command).
    SessionRevive(ReviveSession),
    /// Self only: report state, result, agent identity, resume command.
    SessionReport(Report),
    /// Reads and waits never mark a `done` session as seen; only
    /// `SessionTouch`, attaching as controller and focus do.
    SessionRead(ReadRequest),
    SessionWait(WaitRequest),
    /// Write keys/text to a session without attaching (agents, scripts).
    SessionSend(SendRequest),
    /// Paste-aware text + Enter as one ordered submission.
    SessionSubmit {
        id: SessionId,
        text: String,
    },
    /// Mark a session as just used (list ordering, `done` → `idle`).
    SessionTouch {
        id: SessionId,
    },
    /// Pin or unpin (capability `session.pin`). User callers, or a session
    /// pinning itself.
    SessionPin {
        id: SessionId,
        pinned: bool,
    },
    /// Session callers only: these conversations of `agent` belong to the
    /// calling session, so a user client may act for it with
    /// `Auth::Delegated` (capability `auth.delegated`). The newest binding of
    /// a conversation wins; bindings live in daemon memory and end with the
    /// session.
    SessionBind {
        agent: String,
        conversations: Vec<String>,
    },

    /// Projects the caller can see (capability `projects`): every known
    /// project for user callers, the caller's own for sessions.
    ProjectList,
    /// User only: remember a directory as a project (canonicalized; must
    /// exist and be a directory). Adding a known project is not an error.
    ProjectAdd {
        path: PathBuf,
    },
    /// User only: forget a project. Fails while it still has sessions.
    ProjectRemove {
        path: ScopeRoot,
    },
    /// User-only update after a folder was moved. Changes saved scope/cwd
    /// prefixes without restarting session programs. Capability `projects.relocate`.
    ProjectRelocate {
        from: ScopeRoot,
        to: ScopeRoot,
    },

    /// Stream the terminal. Turns the connection into an attach stream.
    Attach(AttachRequest),
    /// Stream session events. Turns the connection into an event stream.
    Subscribe(SubscribeRequest),

    DaemonStatus,
    /// User clients only. Ends every running session.
    DaemonShutdown,
    /// User clients only (capability `daemon.restart`): replace the daemon
    /// with `binary` (an absolute path; default: the daemon's own
    /// executable) without ending sessions. The process keeps its pid,
    /// socket, user token and running terminals; open connections close and
    /// clients reconnect. Answered with `Ok` before the switch, or an error
    /// when `binary` can't take over (then nothing changed).
    DaemonRestart {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        binary: Option<PathBuf>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CreateSession {
    /// User clients: required. Session callers: omitted or their own scope
    /// (another is `OutOfScope`); the session is created in their own scope.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<ScopeRoot>,
    /// None → auto-generated name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Must be inside the scope. None → the scope root.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<PathBuf>,
    /// None → the user's login shell.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub argv: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub startup_cmds: Vec<String>,
    /// Environment of the requesting client; the child inherits it (minus
    /// variables the daemon strips) instead of the daemon's stale env.
    #[serde(default)]
    pub env: Vec<(String, String)>,
    pub cols: u16,
    pub rows: u16,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReviveSession {
    pub id: SessionId,
    #[serde(default)]
    pub env: Vec<(String, String)>,
    pub cols: u16,
    pub rows: u16,
    /// Run the session's resume command after the shell starts.
    #[serde(default = "default_true")]
    pub resume: bool,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Report {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<Lifecycle>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<ReportResult>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<AgentInfo>,
    /// Command that resumes the agent's conversation in this session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resume: Option<Vec<String>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReadSource {
    /// The visible screen.
    #[default]
    Visible,
    /// The last `lines` rows of history + screen.
    Recent,
    /// Like `Recent`, with soft-wrapped rows joined.
    RecentUnwrapped,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TextFormat {
    #[default]
    Text,
    Ansi,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReadRequest {
    pub id: SessionId,
    #[serde(default)]
    pub source: ReadSource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lines: Option<u32>,
    #[serde(default)]
    pub format: TextFormat,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WaitUntil {
    /// Idle, done, blocked or exited.
    Settled,
    Idle,
    Blocked,
    Working,
    Exited,
    /// The session reported something new.
    Reported,
    /// Visible text matches `pattern`.
    Output,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WaitRequest {
    pub id: SessionId,
    pub until: WaitUntil,
    /// Substring for `Output` (regex not supported in v1).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pattern: Option<String>,
    pub timeout_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SendRequest {
    pub id: SessionId,
    /// Named keys, e.g. "enter", "esc", "ctrl+c", "up". Validated before any
    /// byte is written.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub keys: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    /// Owns input and terminal size. At most one per terminal.
    Controller,
    /// Read-only mirror.
    Observer,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttachRequest {
    pub id: SessionId,
    pub role: Role,
    /// The client's pane size. Applied to the PTY only for controllers.
    pub cols: u16,
    pub rows: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    Created,
    Updated,
    Removed,
    Exited,
    Lifecycle,
    Reported,
    Activity,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct SubscribeRequest {
    /// User clients: the scope to watch (this or `scopes` is required).
    /// Session callers: own scope.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<ScopeRoot>,
    /// More scopes to watch (capability `projects`); unioned with `scope`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub scopes: Vec<ScopeRoot>,
    /// Empty = all sessions in scope.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub session_ids: Vec<SessionId>,
    /// Empty = all kinds.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub kinds: Vec<EventKind>,
    /// Watch every scope, including ones that get their first session
    /// later (capability `sessions.all`; user callers only, session callers
    /// get `out_of_scope`). `scope` and `scopes` are ignored when set.
    #[serde(default, skip_serializing_if = "is_false")]
    pub all: bool,
}

/// The capability a daemon announces when it accepts `all` on
/// `session_list` and `subscribe`.
pub const CAPABILITY_SESSIONS_ALL: &str = "sessions.all";

/// The capability a daemon announces when it accepts `daemon_restart`:
/// it restarts in place and running sessions keep running.
pub const CAPABILITY_DAEMON_RESTART: &str = "daemon.restart";

fn is_false(value: &bool) -> bool {
    !*value
}

// ---------------------------------------------------------------- responses

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Response {
    Hello(HelloOk),
    Pong,
    Ok,
    Session {
        session: SessionView,
    },
    Sessions {
        sessions: Vec<SessionView>,
    },
    Projects {
        projects: Vec<ProjectView>,
    },
    Project {
        project: ProjectView,
    },
    Read(ReadResult),
    Wait(WaitResult),
    Killed {
        killed: Vec<SessionId>,
    },
    /// First message of an attach stream.
    Attached(Attached),
    /// First message of an event stream.
    Subscribed,
    Status(DaemonStatusInfo),
    Error {
        error: ApiError,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReadResult {
    pub text: String,
    pub rows: u16,
    pub cols: u16,
    /// Zero-based (row, col) of the cursor on the visible screen.
    pub cursor: (u16, u16),
    /// More history existed than was returned.
    pub truncated: bool,
    /// Always true: terminal output is untrusted input for agents.
    pub untrusted: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WaitResult {
    pub matched: bool,
    pub status: SessionStatus,
    pub lifecycle: Lifecycle,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reported: Option<Reported>,
    /// Human-readable explanation, e.g. "timeout after 30000 ms".
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attached {
    pub session: SessionView,
    pub role: Role,
    /// Current terminal size (the controller's). Observers render at this size.
    pub cols: u16,
    pub rows: u16,
    /// Base64 of bytes that bring a fresh emulator of `rows`×`cols` to the
    /// current screen (see `vt::Emulator::checkpoint`).
    pub checkpoint: String,
    /// Output offset the checkpoint corresponds to; output frames continue
    /// from here.
    pub offset: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DaemonStatusInfo {
    pub pid: u32,
    pub version: String,
    pub protocol: u32,
    pub uptime_secs: u64,
    pub sessions: u32,
    pub live_terminals: u32,
    pub clients: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rss_bytes: Option<u64>,
}

// ---------------------------------------------------------------- errors

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    Unauthenticated,
    ProtocolMismatch,
    NotFound,
    OutOfScope,
    PermissionDenied,
    ApprovalRequired,
    ApprovalDenied,
    /// Nobody decided an approval in time.
    ApprovalTimeout,
    /// An approval is needed but no user client is attached to decide it.
    ApprovalUnavailable,
    LimitExceeded,
    SessionBlocked,
    /// A human holds controller focus on the target.
    SessionInUse,
    SessionNotLive,
    PromptStalled,
    Timeout,
    InvalidArgument,
    DaemonUnavailable,
    Unsupported,
    Internal,
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApiError {
    pub code: ErrorCode,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<serde_json::Value>,
}

impl ApiError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            details: None,
        }
    }
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let code = serde_json::to_value(self.code)
            .ok()
            .and_then(|v| v.as_str().map(str::to_owned))
            .unwrap_or_else(|| "unknown".into());
        write!(f, "{}: {}", code, self.message)
    }
}

impl std::error::Error for ApiError {}

// ---------------------------------------------------------------- streams

/// Client → daemon control messages on an attach stream. Input bytes travel
/// as raw frames (see `frame`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum StreamCommand {
    /// Controller only. Resizes the PTY.
    Resize {
        cols: u16,
        rows: u16,
    },
    /// Become controller; the previous controller is demoted, not closed.
    Promote {
        cols: u16,
        rows: u16,
    },
    Demote,
    /// Observer: resize the terminal to this size, so the session fits the
    /// window whose user is working now (clients send it when their user
    /// does something). Ignored while the controller is typing (input in
    /// the last few seconds); daemons before 0.6.0 ignore it whenever a
    /// controller is attached.
    Fit {
        cols: u16,
        rows: u16,
    },
    Detach,
}

/// Daemon → client control messages on an attach stream. Output bytes travel
/// as raw frames tagged with their offset.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum StreamEvent {
    /// The terminal was resized (by the controller). `checkpoint` is base64
    /// of the post-resize screen; observers reset their emulator to it.
    Resized {
        cols: u16,
        rows: u16,
        checkpoint: String,
        offset: u64,
    },
    RoleChanged {
        role: Role,
    },
    Title {
        title: String,
    },
    Cwd {
        path: PathBuf,
    },
    Bell,
    /// OSC 52 from the program; only sent to the controller.
    Clipboard {
        selection: String,
        data: String,
    },
    /// The process exited; the stream ends after this.
    Exited {
        code: Option<i32>,
    },
    /// Output older than the client's position was dropped.
    Lagged {
        offset: u64,
    },
}

/// One event on a subscription stream.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EventEnvelope {
    pub scope: ScopeRoot,
    pub session_id: SessionId,
    /// Unix milliseconds.
    pub at: i64,
    pub event: SessionEvent,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SessionEvent {
    Created {
        session: SessionView,
    },
    /// Any change to the view (name, status, cwd, title, controller, …).
    Updated {
        session: SessionView,
    },
    Removed,
    Exited {
        code: Option<i32>,
    },
    Lifecycle {
        lifecycle: Lifecycle,
    },
    Reported {
        reported: Reported,
    },
    /// New output (throttled to at most one event per second per session).
    Activity,
    #[serde(other)]
    Unknown,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_is_sent_only_when_set_and_defaults_to_off() {
        let all = Request::SessionList {
            scope: None,
            scopes: Vec::new(),
            include_dead: true,
            all: true,
        };
        let json = serde_json::to_value(&all).unwrap();
        assert_eq!(json["op"], "session_list");
        assert_eq!(json["all"], true);
        let one = Request::SessionList {
            scope: None,
            scopes: Vec::new(),
            include_dead: false,
            all: false,
        };
        assert!(serde_json::to_value(&one).unwrap().get("all").is_none());
        let watch = SubscribeRequest {
            all: true,
            ..Default::default()
        };
        assert_eq!(serde_json::to_value(&watch).unwrap()["all"], true);
        // Older clients never send it.
        let old: SubscribeRequest = serde_json::from_str(r#"{"scope":"/r"}"#).unwrap();
        assert!(!old.all);
        let old: Request = serde_json::from_str(r#"{"op":"session_list","scope":"/r"}"#).unwrap();
        assert!(matches!(old, Request::SessionList { all: false, .. }));
    }

    #[test]
    fn project_and_pin_ops_round_trip() {
        let pin = Request::SessionPin {
            id: SessionId::from_string("A"),
            pinned: true,
        };
        let json = serde_json::to_value(&pin).unwrap();
        assert_eq!(json["op"], "session_pin");
        assert_eq!(serde_json::from_value::<Request>(json).unwrap(), pin);
        let json = serde_json::to_value(Request::ProjectList).unwrap();
        assert_eq!(json, serde_json::json!({"op": "project_list"}));
        // A list request from an older client has no `scopes`.
        let old: Request =
            serde_json::from_value(serde_json::json!({"op": "session_list", "scope": "/repo"}))
                .unwrap();
        assert!(matches!(old, Request::SessionList { scopes, .. } if scopes.is_empty()));
    }

    #[test]
    fn delegated_auth_and_bind_round_trip() {
        let auth = Auth::Delegated {
            token: "t".into(),
            agent: "opencode".into(),
            conversation: "ses_1".into(),
        };
        let json = serde_json::to_value(&auth).unwrap();
        assert_eq!(
            json,
            serde_json::json!({"kind": "delegated", "token": "t", "agent": "opencode",
                               "conversation": "ses_1"})
        );
        assert_eq!(serde_json::from_value::<Auth>(json).unwrap(), auth);
        let bind = Request::SessionBind {
            agent: "opencode".into(),
            conversations: vec!["ses_1".into(), "ses_2".into()],
        };
        let json = serde_json::to_value(&bind).unwrap();
        assert_eq!(json["op"], "session_bind");
        assert_eq!(serde_json::from_value::<Request>(json).unwrap(), bind);
    }

    #[test]
    fn requests_are_tagged_by_op() {
        let req = Request::SessionKill {
            id: SessionId::from_string("X"),
            tree: false,
        };
        assert_eq!(
            serde_json::to_string(&req).unwrap(),
            r#"{"op":"session_kill","id":"X","tree":false}"#
        );
        let parsed: Request = serde_json::from_str(r#"{"op":"session_kill","id":"X"}"#).unwrap();
        assert_eq!(parsed, req);
    }

    #[test]
    fn unknown_error_codes_and_events_are_tolerated() {
        let err: ApiError =
            serde_json::from_str(r#"{"code":"quota_exceeded","message":"m"}"#).unwrap();
        assert_eq!(err.code, ErrorCode::Unknown);
        let event: SessionEvent = serde_json::from_str(r#"{"kind":"teleported"}"#).unwrap();
        assert_eq!(event, SessionEvent::Unknown);
    }

    #[test]
    fn error_display_uses_snake_case_code() {
        let err = ApiError::new(ErrorCode::OutOfScope, "nope");
        assert_eq!(err.to_string(), "out_of_scope: nope");
    }
}
