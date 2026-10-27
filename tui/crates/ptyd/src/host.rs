//! Contract between terminal management (`terminal`) and the control layer
//! (`control`). The control layer owns sessions and policy; the host owns
//! PTYs, processes and emulators and knows nothing about scopes or callers.

use crate::handoff::TerminalHandoff;
use session_core::api::{ReadResult, ReadSource, Role, TextFormat};
use session_core::client::AttachEvent;
use session_core::ids::{PaneId, SessionId, TerminalId};
use std::path::PathBuf;
use std::sync::mpsc::Receiver;

/// Everything needed to start a terminal. The control layer resolves the
/// argv (login shell), environment and startup input before calling `spawn`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpawnSpec {
    pub session_id: SessionId,
    pub argv: Vec<String>,
    pub cwd: PathBuf,
    /// Complete child environment (the host adds nothing).
    pub env: Vec<(String, String)>,
    pub cols: u16,
    pub rows: u16,
    /// Typed into the terminal once the program has produced its first
    /// output (or after 500 ms): startup commands, resume command.
    pub startup_input: Vec<u8>,
}

/// A terminal program, independent of the session registry or caller policy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalProgram {
    pub argv: Vec<String>,
    pub cwd: PathBuf,
    pub env: Vec<(String, String)>,
    pub cols: u16,
    pub rows: u16,
    pub startup_input: Vec<u8>,
}

/// A temporary pane's program and its opaque, non-session identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaneSpawnSpec {
    pub id: PaneId,
    pub program: TerminalProgram,
}

impl From<SpawnSpec> for TerminalProgram {
    fn from(spec: SpawnSpec) -> Self {
        Self {
            argv: spec.argv,
            cwd: spec.cwd,
            env: spec.env,
            cols: spec.cols,
            rows: spec.rows,
            startup_input: spec.startup_input,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TerminalState {
    Running,
    Exited { code: Option<i32> },
    Failed { message: String },
}

/// Process details of the PTY's foreground process group leader.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessInfo {
    pub pid: u32,
    /// Executable name, e.g. "claude", "node", "bash".
    pub name: String,
    pub cmdline: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalInfo {
    pub id: TerminalId,
    pub session_id: SessionId,
    pub pid: Option<u32>,
    /// Last known working directory (OSC 7 or process inspection).
    pub cwd: Option<PathBuf>,
    pub title: String,
    pub foreground: Option<ProcessInfo>,
    pub state: TerminalState,
    pub cols: u16,
    pub rows: u16,
    pub alternate_screen: bool,
    pub bracketed_paste: bool,
    /// A controller is attached.
    pub controlled: bool,
    /// Total bytes of output so far.
    pub output_offset: u64,
    /// Unix milliseconds of the last output.
    pub last_output_ms: Option<i64>,
    pub started_ms: i64,
}

/// Process and screen metadata shared by session and standalone terminals.
/// This type deliberately has no session identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalProcessInfo {
    pub id: TerminalId,
    pub pid: Option<u32>,
    pub cwd: Option<PathBuf>,
    pub title: String,
    pub foreground: Option<ProcessInfo>,
    pub state: TerminalState,
    pub cols: u16,
    pub rows: u16,
    pub alternate_screen: bool,
    pub bracketed_paste: bool,
    pub controlled: bool,
    pub output_offset: u64,
    pub last_output_ms: Option<i64>,
    pub started_ms: i64,
}

impl TerminalProcessInfo {
    /// Project an existing session's metadata without inventing an identity.
    pub(crate) fn for_session(self, session_id: SessionId) -> TerminalInfo {
        TerminalInfo {
            session_id,
            id: self.id,
            pid: self.pid,
            cwd: self.cwd,
            title: self.title,
            foreground: self.foreground,
            state: self.state,
            cols: self.cols,
            rows: self.rows,
            alternate_screen: self.alternate_screen,
            bracketed_paste: self.bracketed_paste,
            controlled: self.controlled,
            output_offset: self.output_offset,
            last_output_ms: self.last_output_ms,
            started_ms: self.started_ms,
        }
    }
}

/// Terminal-level events for the control layer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostEvent {
    Exited {
        id: TerminalId,
        session_id: SessionId,
        code: Option<i32>,
    },
    Title {
        id: TerminalId,
        session_id: SessionId,
        title: String,
    },
    Cwd {
        id: TerminalId,
        session_id: SessionId,
        path: PathBuf,
    },
    Bell {
        id: TerminalId,
        session_id: SessionId,
    },
    /// Output arrived. Throttled: at most one per second per terminal, plus
    /// one final event 500 ms after output stops (so detection sees the
    /// settled screen).
    Output {
        id: TerminalId,
        session_id: SessionId,
    },
    ControllerChanged {
        id: TerminalId,
        session_id: SessionId,
        controlled: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostError {
    NotFound,
    NotRunning,
    /// Input from an observer, or a second controller conflict.
    NotController,
    /// Someone typed in the terminal too recently (`submit_when_quiet`).
    Typing,
    /// Another program than the one expected is in the foreground
    /// (`submit_expecting`).
    ForegroundChanged,
    InvalidKey(String),
    Spawn(String),
    Io(String),
}

impl std::fmt::Display for HostError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HostError::NotFound => write!(f, "terminal not found"),
            HostError::NotRunning => write!(f, "terminal is not running"),
            HostError::NotController => write!(f, "only the controller can do that"),
            HostError::Typing => write!(f, "someone is typing in this terminal"),
            HostError::ForegroundChanged => {
                write!(f, "another program is in this terminal's foreground")
            }
            HostError::InvalidKey(k) => write!(f, "unknown key {k:?}"),
            HostError::Spawn(m) => write!(f, "spawn failed: {m}"),
            HostError::Io(m) => write!(f, "I/O error: {m}"),
        }
    }
}

impl std::error::Error for HostError {}

/// A client's view of a terminal it attached to.
pub struct HostAttachment<Info = TerminalInfo> {
    pub info: Info,
    pub role: Role,
    /// Bytes that bring a fresh emulator of `info.rows`×`info.cols` to the
    /// current screen.
    pub checkpoint: Vec<u8>,
    /// Output offset the checkpoint corresponds to.
    pub offset: u64,
    /// Output, stream events and closure, in order.
    pub events: Receiver<AttachEvent>,
    pub control: Box<dyn AttachControl>,
}

/// A standalone attachment has no registry session in its metadata.
pub type PaneAttachment = HostAttachment<TerminalProcessInfo>;

/// Commands from an attached client. Dropping the control detaches.
pub trait AttachControl: Send {
    /// Send the current screen to this attachment without changing its size.
    fn checkpoint(&self) -> Result<(), HostError> {
        Err(HostError::Io("checkpoints are unsupported".into()))
    }
    /// Controller only.
    fn input(&self, bytes: &[u8]) -> Result<(), HostError>;
    /// Controller only.
    fn resize(&self, cols: u16, rows: u16) -> Result<(), HostError>;
    /// Become controller and resize to the client's pane. The previous
    /// controller is demoted (it receives `RoleChanged`).
    fn promote(&self, cols: u16, rows: u16) -> Result<(), HostError>;
    fn demote(&self);
    /// Resize to the client's pane (its user is the one working now),
    /// unless the controller typed in the last few seconds; a no-op then.
    fn fit(&self, cols: u16, rows: u16) -> Result<(), HostError>;
    fn detach(&self);
}

/// Terminal management. Implemented by `terminal::Host`; tests use fakes.
pub trait TerminalHost: Send + Sync + 'static {
    fn spawn(&self, spec: SpawnSpec) -> Result<TerminalInfo, HostError>;
    /// Start a temporary terminal without session files, events or recovery.
    fn spawn_pane(&self, spec: PaneSpawnSpec) -> Result<TerminalProcessInfo, HostError> {
        let _ = spec;
        Err(HostError::Spawn(
            "standalone terminals are unsupported".into(),
        ))
    }
    /// Look up only a standalone pane, never a session's terminal.
    fn pane_info(&self, id: &PaneId) -> Option<TerminalProcessInfo> {
        let _ = id;
        None
    }
    /// Attach only to an existing standalone pane.
    fn attach_pane(
        &self,
        id: &PaneId,
        role: Role,
        cols: u16,
        rows: u16,
    ) -> Result<PaneAttachment, HostError> {
        let _ = (id, role, cols, rows);
        Err(HostError::NotFound)
    }
    /// Forget and terminate only this standalone pane, if it still exists.
    fn close_pane(&self, id: &PaneId) {
        let _ = id;
    }
    /// Number of standalone panes, separate from registry session counts.
    fn pane_count(&self) -> usize {
        0
    }
    fn list(&self) -> Vec<TerminalInfo>;
    fn info(&self, id: TerminalId) -> Option<TerminalInfo>;
    fn read(
        &self,
        id: TerminalId,
        source: ReadSource,
        lines: Option<u32>,
        format: TextFormat,
    ) -> Result<ReadResult, HostError>;
    /// Write bytes regardless of controller (policy is the caller's job).
    fn write(&self, id: TerminalId, bytes: &[u8]) -> Result<(), HostError>;
    /// Bracketed-paste-aware text, then Enter in a write of its own after a
    /// short pause, with nothing else written in between.
    fn submit(&self, id: TerminalId, text: &str) -> Result<(), HostError>;
    /// [`submit`](Self::submit), unless its controller sent input within
    /// `quiet`: then [`HostError::Typing`] and nothing is written.
    fn submit_when_quiet(
        &self,
        id: TerminalId,
        text: &str,
        quiet: std::time::Duration,
    ) -> Result<(), HostError> {
        let _ = quiet;
        self.submit(id, text)
    }
    /// [`submit_when_quiet`](Self::submit_when_quiet) (plain
    /// [`submit`](Self::submit) without `quiet`), and only while the program
    /// in the foreground is `foreground` (a process name, as
    /// `TerminalView::foreground_process` says), read again now: else
    /// [`HostError::ForegroundChanged`] and nothing is written. Its Enter
    /// then goes only while the process group that had the foreground at
    /// the paste still has it.
    fn submit_expecting(
        &self,
        id: TerminalId,
        text: &str,
        quiet: Option<std::time::Duration>,
        foreground: Option<&str>,
    ) -> Result<(), HostError> {
        let _ = foreground;
        match quiet {
            Some(quiet) => self.submit_when_quiet(id, text, quiet),
            None => self.submit(id, text),
        }
    }
    /// Named keys (`enter`, `esc`, `tab`, `backspace`, `up`/`down`/`left`/
    /// `right`, `home`, `end`, `pageup`, `pagedown`, `ctrl+<c>`, `alt+<c>`)
    /// then optional literal text. All keys are validated before any byte is
    /// written.
    fn send_keys(
        &self,
        id: TerminalId,
        keys: &[String],
        text: Option<&str>,
    ) -> Result<(), HostError>;
    /// SIGHUP the process group, SIGKILL after 3 s.
    fn terminate(&self, id: TerminalId) -> Result<(), HostError>;
    /// Drop an exited terminal (after its scrollback was persisted).
    fn forget(&self, id: TerminalId);
    fn attach(
        &self,
        id: TerminalId,
        role: Role,
        cols: u16,
        rows: u16,
    ) -> Result<HostAttachment, HostError>;
    /// A new receiver of all host events from now on.
    fn subscribe(&self) -> Receiver<HostEvent>;
    /// Terminate everything (daemon shutdown).
    fn shutdown(&self);

    /// Stop every terminal without ending its program, for a restart in
    /// place: the running ones are returned with inheritable PTY
    /// descriptors (`handoff`); exited ones are dropped.
    fn handoff(&self) -> Vec<TerminalHandoff> {
        self.shutdown();
        Vec::new()
    }
    /// Standalone PTYs collected by `handoff`; no session records are involved.
    fn take_pane_handoff(&self) -> Vec<crate::handoff::PaneHandoff> {
        Vec::new()
    }
}
