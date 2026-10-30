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

use crate::ids::{MachineId, PaneId, ReplyId, SessionId};
use crate::model::{
    AgentInfo, Lifecycle, MachineView, ProjectView, ReportResult, Reported, ScopeRoot,
    SessionStatus, SessionView,
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
    /// Paste-aware text + Enter as one ordered submission: the Enter in a
    /// write of its own half a second after the paste (programs that take a
    /// paste in steps drop an Enter that comes with it), nothing else written
    /// to the session in between. Answered once both are queued.
    SessionSubmit {
        id: SessionId,
        text: String,
        /// Refuse with `session_in_use` while someone typed in the session
        /// (its controller sent input) within this many milliseconds, so the
        /// text isn't added to what they're writing (capability
        /// `session.submit.quiet`). A daemon without it submits anyway.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        quiet_ms: Option<u64>,
        /// Refuse with `foreground_changed` unless the program in the
        /// session's foreground is this one now (its process name, as
        /// `TerminalView::foreground_process` says), read again as the text
        /// is queued; its Enter then goes only while that process group still
        /// has the foreground, so text meant for an agent is never run by the
        /// shell it exited to (capability `session.submit.expect`). A daemon
        /// without it submits anyway.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        expect_foreground: Option<String>,
        /// Refuse with `session_blocked` while the session's agent asks
        /// something (lifecycle `blocked`: a permission prompt, a question or
        /// a menu, which would take the text as its answer), as the daemon
        /// has it and as its screen and foreground process show now; its
        /// Enter then goes only if the screen still shows no such prompt
        /// when it's due, so it never answers one that opened after the
        /// paste (capability `session.submit.blocked`). A daemon without it
        /// submits anyway.
        #[serde(default, skip_serializing_if = "is_false")]
        refuse_if_blocked: bool,
    },
    /// Mark a session as just used (list ordering, `done` → `idle`).
    SessionTouch {
        id: SessionId,
    },
    /// User only: the current turn of a live session's agent, for an AI
    /// summary (capability `session.turn`).
    SessionTurn {
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
        /// Make the directory (and its parents) when it doesn't exist
        /// (capability `projects.create`).
        #[serde(default, skip_serializing_if = "is_false")]
        create: bool,
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

    /// User only (capability `machines`): reach another machine running
    /// Rakumux through its `rakumux-api` remote listener at `url`, with that
    /// listener's `token`, and list its sessions with this machine's.
    /// Answered with `Machine`.
    MachineAdd {
        name: String,
        url: String,
        token: String,
    },
    /// User only (capability `machines`): the machines added. Answered with
    /// `Machines`.
    MachineList,
    /// User only (capability `machines`): forget a machine and the sessions
    /// listed from it. Nothing on the machine changes.
    MachineRemove {
        id: MachineId,
    },

    /// Stream the terminal. Turns the connection into an attach stream.
    Attach(AttachRequest),
    /// User-only temporary shell, with no session record or saved files.
    /// Opens a stream; closing the stream ends the pane (except during handoff).
    TerminalOpen(OpenTerminal),
    /// Reconnect an existing temporary shell after a daemon restart in place.
    /// Never creates a replacement terminal. User only.
    TerminalAttach(TerminalAttachRequest),
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

    /// User only (capability `session.replies`): start or stop capturing
    /// what agents say when they finish a turn. Off by default and after a
    /// daemon start or restart; turning it off forgets captured replies.
    ReplyCapture {
        enabled: bool,
    },
    /// User only (capability `session.replies`): captured replies, oldest
    /// first; with `after`, only those captured after that one (all of them
    /// when it was forgotten). Answered with `Replies`.
    ReplyList {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        after: Option<ReplyId>,
    },
    /// User only (capability `session.transcript`): the messages of a live
    /// session's agent conversation (Claude Code, Codex, OpenCode) after
    /// `after` (from its start without one, or when it names another
    /// conversation), oldest first, read from the agent's own log or
    /// database: prompts, the agent's text, reasoning and tool calls, and
    /// the tools' output. At most about `max_bytes` (default
    /// [`TRANSCRIPT_BYTES`], at most [`MAX_TRANSCRIPT_BYTES`]) of the log is
    /// read at once. Answered with `Transcript`.
    SessionTranscript {
        id: SessionId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        after: Option<TranscriptCursor>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max_bytes: Option<u32>,
    },
    /// User only (capability `resources`): what the machine, the daemon,
    /// its clients' processes and every live session's processes use now.
    /// Counters are cumulative, so rates come from two answers. Answered
    /// with `Resources`.
    ResourceUsage,
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
    /// Create it on this machine (capability `machines`): `scope` and `cwd`
    /// are then that machine's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub machine: Option<MachineId>,
    /// Typed into the new shell without Enter, for the user to run or not
    /// (capability `session.prefill`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prefill: Option<String>,
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
    /// The agent's active logical directory, which may differ from its PTY
    /// process cwd. Self-only metadata; does not change folder permissions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub working_directory: Option<AgentWorkingDirectory>,
    /// Agent-neutral hook activity; never contains terminal or prompt text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activity: Option<crate::activity::ActivityUpdate>,
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

/// The active (or last used) directory of the agent shown in a session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentWorkingDirectory {
    pub agent: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conversation_id: Option<String>,
    pub path: PathBuf,
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

/// Start a window-local terminal without creating a Rakumux session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenTerminal {
    pub id: PaneId,
    pub cwd: PathBuf,
    #[serde(default)]
    pub env: Vec<(String, String)>,
    pub cols: u16,
    pub rows: u16,
}

/// Attach only to an existing standalone terminal, never to a session's PTY.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalAttachRequest {
    pub id: PaneId,
    pub role: Role,
    pub cols: u16,
    pub rows: u16,
}

/// A standalone terminal's checkpoint. No session identity is allocated.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalAttached {
    pub id: PaneId,
    pub cwd: Option<PathBuf>,
    pub pid: Option<u32>,
    pub role: Role,
    pub cols: u16,
    pub rows: u16,
    pub checkpoint: String,
    pub offset: u64,
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
    /// Only on subscriptions that ask for `replies`.
    Reply,
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
    /// Also deliver [`SessionEvent::Reply`] (capability `session.replies`;
    /// user callers only, session callers get `permission_denied`). Other
    /// subscriptions never see reply text.
    #[serde(default, skip_serializing_if = "is_false")]
    pub replies: bool,
}

/// The capability a daemon announces when it accepts `all` on
/// `session_list` and `subscribe`.
pub const CAPABILITY_SESSIONS_ALL: &str = "sessions.all";

/// The capability a daemon announces when it accepts `daemon_restart`:
/// it restarts in place and running sessions keep running.
pub const CAPABILITY_DAEMON_RESTART: &str = "daemon.restart";

/// Versioned, self-reported agent activity and normalized session projections.
pub const CAPABILITY_ACTIVITY: &str = "session.activity";

/// Temporary daemon-owned terminal panes, outside the session registry.
pub const CAPABILITY_TERMINAL_PANE: &str = "terminal.pane";

/// Self-reported logical agent directories, separate from the terminal cwd.
pub const CAPABILITY_WORKING_DIRECTORY: &str = "session.working_directory";

/// Captured agent replies: `reply_capture`, `reply_list` and `replies` on
/// `subscribe`.
pub const CAPABILITY_REPLIES: &str = "session.replies";

/// What an agent said when it finished a turn, captured by the daemon from
/// the agent's own conversation log on this machine. Kept in daemon memory
/// only and delivered only to user callers that ask for replies.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentReply {
    pub id: ReplyId,
    pub session_id: SessionId,
    /// The agent as the feed names it: `claude-code`, `codex`, `opencode`,
    /// `copilot`, `cursor-agent`, `pi`, `grok`, `kimi`, `droid` or `amp`.
    pub agent: String,
    /// The agent's own conversation id, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conversation_id: Option<String>,
    /// The reply's text, at most [`MAX_REPLY_BYTES`].
    pub text: String,
    /// The reply was longer; `text` is its start.
    #[serde(default, skip_serializing_if = "is_false")]
    pub truncated: bool,
    /// When the turn finished (Unix milliseconds).
    pub finished_at: i64,
    /// The [`TranscriptMessage::position`] of the turn's last message in its
    /// conversation, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position: Option<u64>,
}

/// The longest reply text kept, in UTF-8 bytes.
pub const MAX_REPLY_BYTES: usize = 32 * 1024;

/// `SessionView::agent_output`: a running agent's latest output line.
pub const CAPABILITY_AGENT_OUTPUT: &str = "session.agent_output";

/// `SessionView::user_prompt`: what the user last typed to a running agent.
pub const CAPABILITY_USER_PROMPT: &str = "session.user_prompt";

/// `session_turn` and `SessionView::agent_turn`: a running agent's current turn.
pub const CAPABILITY_TURN: &str = "session.turn";

/// `session_transcript`: a running agent's whole conversation.
pub const CAPABILITY_TRANSCRIPT: &str = "session.transcript";

/// `SessionView::places`: where a session has worked.
pub const CAPABILITY_PLACES: &str = "session.places";

/// `session_submit`'s `quiet_ms`: refused while someone typed in the session.
pub const CAPABILITY_SUBMIT_QUIET: &str = "session.submit.quiet";

/// `session_submit`'s `expect_foreground`: refused unless that program is in
/// the session's foreground; and then its Enter is never written once another
/// process group took the foreground after the text.
pub const CAPABILITY_SUBMIT_EXPECT: &str = "session.submit.expect";

/// `session_submit`'s `refuse_if_blocked`: refused with `session_blocked`
/// while the session's agent asks something; and then its Enter is never
/// written once the screen shows the agent asking something.
pub const CAPABILITY_SUBMIT_BLOCKED: &str = "session.submit.blocked";

/// `resource_usage`: what the machine and Rakumux's processes use.
pub const CAPABILITY_RESOURCES: &str = "resources";

/// `machine_add`, `machine_list`, `machine_remove`, `SessionView::machine`
/// and `CreateSession::machine`: sessions on other machines.
pub const CAPABILITY_MACHINES: &str = "machines";

/// `Hello::client` of a connection relaying another machine's daemon
/// (`rakumux-api`'s `/v1/daemon`). The daemon answers it as the user about
/// this machine alone: it never lists, forwards to or adds other machines on
/// it, so two machines that added each other don't loop. Any client may say
/// it; it only ever narrows what that client reaches.
pub const MACHINE_CLIENT: &str = "machine";

/// `CreateSession::prefill`: text typed into a new shell without Enter.
pub const CAPABILITY_PREFILL: &str = "session.prefill";

/// `project_add`'s `create`: make the folder when it doesn't exist.
pub const CAPABILITY_PROJECTS_CREATE: &str = "projects.create";

/// How much of an agent's log `session_transcript` reads by default, in bytes.
pub const TRANSCRIPT_BYTES: u32 = 512 * 1024;
/// The most it reads at once.
pub const MAX_TRANSCRIPT_BYTES: u32 = 2 * 1024 * 1024;
/// The longest text of a [`TranscriptPart`], in UTF-8 bytes: longer text is
/// cut and the part says `truncated`.
pub const MAX_PART_BYTES: usize = 64 * 1024;
/// The most parts a [`TranscriptMessage`] has; further ones are left out.
pub const MAX_PARTS: usize = 64;

/// Where a `session_transcript` read stopped; pass it back to read on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TranscriptCursor {
    /// The conversation read: a cursor of another one reads the current one
    /// from its start.
    pub conversation: String,
    /// How far: a byte offset into the agent's log, or the last
    /// `time_updated` OpenCode gave.
    pub offset: u64,
    /// OpenCode: the id of the last row read at `offset`. GitHub Copilot:
    /// the log file read (it rewrites its log when a turn is taken back).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
}

/// The answer to `session_transcript`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TranscriptPage {
    /// The agent as the feed names it: `claude-code`, `codex`, `opencode`,
    /// `copilot`, `cursor-agent`, `pi`, `grok`, `kimi`, `droid` or `amp`.
    pub agent: String,
    /// The agent's own conversation id.
    pub conversation_id: String,
    /// In the order read: by position for logs; OpenCode sends a message
    /// again when it changes.
    #[serde(default)]
    pub messages: Vec<TranscriptMessage>,
    /// Where to read on from.
    pub cursor: TranscriptCursor,
    /// More was there to read: ask again at once.
    #[serde(default, skip_serializing_if = "is_false")]
    pub more: bool,
}

/// One message of an agent's conversation: one entry of its log, or one
/// block of an OpenCode reply. Terminal-like untrusted text, cut at
/// [`MAX_PART_BYTES`] per part, not otherwise cleaned.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TranscriptMessage {
    /// Unique in the conversation: the agent's own id, or the position.
    pub id: String,
    /// Where it goes: messages sort by it. A byte offset into Claude Code's
    /// and Codex's logs; OpenCode's own order times 1024, plus the block.
    pub position: u64,
    pub role: TranscriptRole,
    /// When the agent wrote it (Unix milliseconds).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<i64>,
    pub parts: Vec<TranscriptPart>,
}

/// Who a [`TranscriptMessage`] is from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TranscriptRole {
    /// What the user typed.
    User,
    /// The agent's text, reasoning and tool calls.
    Assistant,
    /// Tools' output.
    Tool,
    #[serde(other)]
    Unknown,
}

/// A piece of a [`TranscriptMessage`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TranscriptPart {
    Text {
        text: String,
        #[serde(default, skip_serializing_if = "is_false")]
        truncated: bool,
    },
    /// The agent's visible reasoning.
    Reasoning {
        text: String,
        #[serde(default, skip_serializing_if = "is_false")]
        truncated: bool,
    },
    /// A tool call; `input` as the agent gave it (usually JSON).
    ToolCall {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        call_id: Option<String>,
        name: String,
        input: String,
        #[serde(default, skip_serializing_if = "is_false")]
        truncated: bool,
    },
    /// A tool's output.
    ToolResult {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        call_id: Option<String>,
        output: String,
        #[serde(default, skip_serializing_if = "is_false")]
        error: bool,
        #[serde(default, skip_serializing_if = "is_false")]
        truncated: bool,
    },
    #[serde(other)]
    Unknown,
}

fn is_false(value: &bool) -> bool {
    !*value
}

// ---------------------------------------------------------------- responses

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Response {
    TerminalAttached(TerminalAttached),
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
    /// The answer to `machine_add`.
    Machine {
        machine: MachineView,
    },
    /// The answer to `machine_list`.
    Machines {
        machines: Vec<MachineView>,
    },
    Read(ReadResult),
    /// The answer to `session_turn`.
    Turn {
        turn: TurnContent,
    },
    /// The answer to `session_transcript`.
    Transcript {
        transcript: TranscriptPage,
    },
    Wait(WaitResult),
    Killed {
        killed: Vec<SessionId>,
    },
    /// First message of an attach stream.
    Attached(Attached),
    /// First message of an event stream.
    Subscribed,
    Status(DaemonStatusInfo),
    /// The answer to `resource_usage`.
    Resources {
        resources: ResourceSample,
    },
    /// Answer to `reply_list`.
    Replies {
        replies: Vec<AgentReply>,
        /// Capture is on.
        capturing: bool,
    },
    Error {
        error: ApiError,
    },
}

/// A live agent's current turn (`session_turn`): the user's last prompt and
/// the agent's messages since (reasoning, replies, tool calls), oldest
/// first, each flattened to one line. Read from the agent's own
/// conversation; terminal-like untrusted text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnContent {
    /// The same as `SessionView::agent_turn`'s.
    pub id: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    /// The newest messages the daemon keeps (100), oldest first.
    #[serde(default)]
    pub messages: Vec<String>,
    /// Every message of the turn, kept or not.
    pub total: u32,
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
    /// The user's home directory on the daemon's machine, where another
    /// machine makes folders for the sessions it creates here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub home: Option<PathBuf>,
}

/// What the machine and Rakumux's processes use at one moment
/// (`resource_usage`). Counters are cumulative since the process (or the
/// machine) started: a client takes two samples and divides the
/// differences by the time between them (`at_ms`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResourceSample {
    /// Unix milliseconds when it was taken.
    pub at_ms: i64,
    pub machine: MachineUsage,
    /// The daemon's own process.
    pub daemon: ProcessUsage,
    /// The daemon's connected clients, one per process however many
    /// connections it holds (TUIs, `rakumux-api`, the account service, CLI
    /// calls), oldest connection first. A client running inside a session
    /// is listed under that session instead.
    #[serde(default)]
    pub clients: Vec<ClientUsage>,
    /// Every live session's processes.
    #[serde(default)]
    pub sessions: Vec<SessionUsage>,
}

/// The whole machine's use. Fields are absent where the platform doesn't
/// say.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MachineUsage {
    /// Logical CPUs.
    pub cpus: u32,
    /// CPU time every CPU spent busy since boot, in ms.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu_busy_ms: Option<u64>,
    /// CPU time every CPU spent at all (busy or idle) since boot, in ms.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu_total_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_total_bytes: Option<u64>,
    /// Memory programs could still take without swapping.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_available_bytes: Option<u64>,
    /// Bytes received on every network interface but loopback.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub net_rx_bytes: Option<u64>,
    /// Bytes sent on every network interface but loopback.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub net_tx_bytes: Option<u64>,
}

/// A client of the daemon in [`ResourceSample::clients`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientUsage {
    /// The client name from its hello (`tui`, `api`, `cli`, …).
    pub client: String,
    /// Its open connections to the daemon.
    pub connections: u32,
    pub process: ProcessUsage,
}

/// A live session's processes in [`ResourceSample::sessions`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionUsage {
    pub id: SessionId,
    /// The program its terminal started, then each process's children
    /// after it, depth first; each but the first names its `parent`.
    #[serde(default)]
    pub processes: Vec<ProcessUsage>,
}

/// One process's use so far.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcessUsage {
    pub pid: u32,
    /// Its parent, when that is listed with it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<u32>,
    /// Executable name, as `TerminalView::foreground_process` gives it.
    pub name: String,
    /// Its command line, arguments joined by spaces and cut at
    /// [`MAX_COMMAND_CHARS`] characters.
    #[serde(default)]
    pub command: String,
    /// CPU time all its threads used (user and system), in ms.
    pub cpu_ms: u64,
    /// Resident memory.
    pub rss_bytes: u64,
    /// Bytes received over the TCP connections it holds open now, to
    /// anywhere but loopback (Linux; absent where the platform doesn't say).
    /// A connection two processes share counts for the first listed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub net_rx_bytes: Option<u64>,
    /// Bytes sent likewise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub net_tx_bytes: Option<u64>,
}

/// The longest [`ProcessUsage::command`], in characters.
pub const MAX_COMMAND_CHARS: usize = 200;

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
    /// The session's agent asks something: a permission prompt, a question
    /// or a menu (`session_submit`'s `refuse_if_blocked`).
    SessionBlocked,
    /// A human holds controller focus on the target.
    SessionInUse,
    /// Another program than the one expected is in the session's foreground
    /// (`session_submit`'s `expect_foreground`).
    ForegroundChanged,
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
    /// Request a fresh checkpoint without resizing or taking control.
    /// Requires `terminal.pane` support in the daemon.
    Checkpoint,
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
    /// The session's agent finished a turn with this reply (subscriptions
    /// with `replies` only).
    Reply {
        reply: AgentReply,
    },
    #[serde(other)]
    Unknown,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn machine_fields_are_left_out_unless_used() {
        let create = CreateSession {
            scope: None,
            name: None,
            cwd: None,
            argv: None,
            startup_cmds: vec![],
            env: vec![],
            cols: 80,
            rows: 24,
            machine: None,
            prefill: None,
        };
        let json = serde_json::to_string(&Request::SessionCreate(create.clone())).unwrap();
        assert!(
            !json.contains("machine") && !json.contains("prefill"),
            "{json}"
        );
        let older: CreateSession =
            serde_json::from_str(r#"{"env":[],"cols":80,"rows":24}"#).unwrap();
        assert_eq!(older, create);

        let add = Request::ProjectAdd {
            path: "/p".into(),
            create: false,
        };
        assert_eq!(
            serde_json::to_string(&add).unwrap(),
            r#"{"op":"project_add","path":"/p"}"#
        );
        let back: Request = serde_json::from_str(r#"{"op":"project_add","path":"/p"}"#).unwrap();
        assert_eq!(back, add);

        let machine = Request::MachineAdd {
            name: "Sprite".into(),
            url: "https://s.example".into(),
            token: "t".into(),
        };
        let json = serde_json::to_string(&machine).unwrap();
        assert!(json.starts_with(r#"{"op":"machine_add""#), "{json}");
        assert_eq!(serde_json::from_str::<Request>(&json).unwrap(), machine);
    }

    #[test]
    fn terminal_pane_contract_round_trips_without_session_metadata() {
        let id = PaneId::generate();
        let request = Request::TerminalOpen(OpenTerminal {
            id: id.clone(),
            cwd: "/repo".into(),
            env: Vec::new(),
            cols: 80,
            rows: 24,
        });
        let json = serde_json::to_value(&request).unwrap();
        assert_eq!(json["op"], "terminal_open");
        assert!(json.get("scope").is_none());
        assert_eq!(serde_json::from_value::<Request>(json).unwrap(), request);
        let request = Request::TerminalAttach(TerminalAttachRequest {
            id: id.clone(),
            role: Role::Observer,
            cols: 80,
            rows: 24,
        });
        let json = serde_json::to_value(&request).unwrap();
        assert_eq!(json["op"], "terminal_attach");
        assert_eq!(serde_json::from_value::<Request>(json).unwrap(), request);
        let response = Response::TerminalAttached(TerminalAttached {
            id,
            cwd: Some("/repo".into()),
            pid: Some(123),
            role: Role::Controller,
            cols: 80,
            rows: 24,
            checkpoint: String::new(),
            offset: 0,
        });
        let json = serde_json::to_value(&response).unwrap();
        assert_eq!(json["type"], "terminal_attached");
        assert!(json.get("session").is_none());
        assert_eq!(serde_json::from_value::<Response>(json).unwrap(), response);
        assert_eq!(
            serde_json::to_value(StreamCommand::Checkpoint).unwrap(),
            serde_json::json!({"cmd":"checkpoint"})
        );
    }

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
    fn refuse_if_blocked_is_sent_only_when_set_and_defaults_to_off() {
        let submit = |refuse_if_blocked| Request::SessionSubmit {
            id: SessionId::generate(),
            text: "hi".into(),
            quiet_ms: Some(10_000),
            expect_foreground: Some("claude".into()),
            refuse_if_blocked,
        };
        let json = serde_json::to_value(submit(true)).unwrap();
        assert_eq!(json["op"], "session_submit");
        assert_eq!(json["refuse_if_blocked"], true);
        let plain = serde_json::to_value(submit(false)).unwrap();
        assert!(plain.get("refuse_if_blocked").is_none());
        // Older clients never send it.
        let mut old = plain;
        old["id"] = serde_json::json!(SessionId::generate().as_str());
        let old: Request = serde_json::from_value(old).unwrap();
        assert!(matches!(
            old,
            Request::SessionSubmit {
                refuse_if_blocked: false,
                ..
            }
        ));
        let code: ErrorCode = serde_json::from_str(r#""session_blocked""#).unwrap();
        assert_eq!(code, ErrorCode::SessionBlocked);
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
