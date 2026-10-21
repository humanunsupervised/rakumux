//! Restarting the daemon in place (`daemon_restart`) without ending
//! sessions.
//!
//! The daemon `exec`s the new binary in its own process, so the pid stays
//! the same and every session's program stays its child. What crosses the
//! `exec`:
//! - descriptors, inherited (close-on-exec cleared): each running
//!   terminal's PTY master, the listening socket (clients connecting during
//!   the switch wait in its backlog) and the `service.lock` flock;
//! - a [`HandoffState`], as JSON in an unlinked 0600 file whose descriptor
//!   is passed with `serve --handoff-fd <fd>`: the registration (same
//!   instance id and user token), each terminal's screen, history and
//!   details, and the control layer's memory (session tokens, spawn
//!   environments, conversation bindings).
//!
//! Output a program writes during the switch waits in its PTY. A program
//! that exits during it stays a zombie until the new daemon reaps it (the
//! waiters only peek with `WNOWAIT`), so its exit code isn't lost.
//!
//! Before switching, the daemon asks the target which state versions it
//! reads (`rakumux-ptyd handoff-versions`); a binary that can't take over
//! is refused and nothing changes. If the `exec` itself fails, the daemon
//! tries its own executable, then restores the state in this process.

use crate::{log_error, log_info};
use base64::Engine;
use serde::{Deserialize, Serialize};
use session_core::ids::{PaneId, SessionId};
use session_core::paths::Registration;
use std::ffi::CString;
use std::fs::{File, OpenOptions};
use std::io::{self, Seek, SeekFrom};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::io::{FromRawFd, IntoRawFd, RawFd};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Version of [`HandoffState`] this daemon writes and reads.
pub const HANDOFF_VERSION: u32 = 2;

/// This image can upgrade a version-1 daemon without ending its sessions.
pub const READABLE_HANDOFF_VERSIONS: &[u32] = &[1, HANDOFF_VERSION];

/// `rakumux-ptyd handoff-versions`: the state versions this build reads.
pub const VERSIONS_COMMAND: &str = "handoff-versions";

/// How long the target may take to answer `handoff-versions`.
const CHECK_TIMEOUT: Duration = Duration::from_secs(5);

/// Everything the next daemon image needs to carry on.
#[derive(Debug, Serialize, Deserialize)]
pub struct HandoffState {
    pub version: u32,
    /// Version of the daemon that handed over (for the log).
    pub from_version: String,
    pub home: PathBuf,
    pub run_dir: PathBuf,
    /// umask spawned programs get (the daemon itself runs with 077).
    #[serde(default)]
    pub child_umask: Option<u32>,
    pub claim: ClaimHandoff,
    pub control: ControlHandoff,
    pub terminals: Vec<TerminalHandoff>,
    /// Standalone terminal panes are not registry sessions or crash recovery.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub panes: Vec<PaneHandoff>,
}

/// The daemon's claim: same registration, inherited socket and lock.
#[derive(Debug, Serialize, Deserialize)]
pub struct ClaimHandoff {
    pub registration: Registration,
    pub listener_fd: RawFd,
    pub lock_fd: RawFd,
}

/// The control layer's memory.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct ControlHandoff {
    /// Last logical agent directories. The PTY's cwd and session scopes stay separate.
    #[serde(default)]
    pub working_directories: Vec<(
        SessionId,
        session_core::api::AgentWorkingDirectory,
        Option<u32>,
    )>,
    /// Ephemeral activity snapshots and their original receive times.
    #[serde(default)]
    pub activity: Vec<(SessionId, session_core::activity::ActivityUpdate, i64)>,
    /// Completed activity already opened by the user; do not make it unread again.
    #[serde(default)]
    pub activity_seen: Vec<SessionId>,
    /// Session tokens: the programs inside sessions keep authenticating.
    #[serde(default)]
    pub tokens: Vec<(SessionId, String)>,
    /// The environment each session's program was spawned with.
    #[serde(default)]
    pub spawn_env: Vec<(SessionId, Vec<(String, String)>)>,
    #[serde(default)]
    pub bindings: Vec<BindingHandoff>,
    #[serde(default)]
    pub binding_seq: u64,
}

/// An agent conversation a session bound (`session_bind`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BindingHandoff {
    pub session_id: SessionId,
    pub agent: String,
    pub conversation: String,
    pub seq: u64,
}

/// A running terminal.
#[derive(Debug, Serialize, Deserialize)]
pub struct TerminalHandoff {
    pub id: u64,
    pub session_id: SessionId,
    /// Inherited PTY master.
    pub fd: RawFd,
    pub pid: u32,
    pub cols: u16,
    pub rows: u16,
    #[serde(default)]
    pub cwd: Option<PathBuf>,
    /// The program reports its cwd with OSC 7.
    #[serde(default)]
    pub osc7_cwd: bool,
    #[serde(default)]
    pub osc7_foreground: Option<u32>,
    #[serde(default)]
    pub osc7_process_cwd: Option<PathBuf>,
    #[serde(default)]
    pub title: String,
    pub started_ms: i64,
    #[serde(default)]
    pub last_output_ms: Option<i64>,
    /// Output bytes so far (offsets continue from here).
    pub output_offset: u64,
    /// Base64 of the emulator checkpoint (`vt::Emulator::checkpoint`).
    pub screen: String,
    /// Base64 of each history row above the screen, oldest first
    /// (`vt::Emulator::history_rows_formatted`).
    #[serde(default)]
    pub history: Vec<String>,
    /// Base64 of startup input not typed yet.
    #[serde(default)]
    pub startup_input: Option<String>,
}

/// A running PTY's state without any session identity.
#[derive(Debug, Serialize, Deserialize)]
pub struct ProcessHandoff {
    pub id: u64,
    pub fd: RawFd,
    pub pid: u32,
    pub cols: u16,
    pub rows: u16,
    #[serde(default)]
    pub cwd: Option<PathBuf>,
    #[serde(default)]
    pub osc7_cwd: bool,
    #[serde(default)]
    pub osc7_foreground: Option<u32>,
    #[serde(default)]
    pub osc7_process_cwd: Option<PathBuf>,
    #[serde(default)]
    pub title: String,
    pub started_ms: i64,
    #[serde(default)]
    pub last_output_ms: Option<i64>,
    pub output_offset: u64,
    pub screen: String,
    #[serde(default)]
    pub history: Vec<String>,
    #[serde(default)]
    pub startup_input: Option<String>,
}

/// A standalone terminal survives an in-place restart without a session record.
#[derive(Debug, Serialize, Deserialize)]
pub struct PaneHandoff {
    pub id: PaneId,
    pub terminal: ProcessHandoff,
}

impl ProcessHandoff {
    pub(crate) fn for_session(self, session_id: SessionId) -> TerminalHandoff {
        TerminalHandoff {
            session_id,
            id: self.id,
            fd: self.fd,
            pid: self.pid,
            cols: self.cols,
            rows: self.rows,
            cwd: self.cwd,
            osc7_cwd: self.osc7_cwd,
            osc7_foreground: self.osc7_foreground,
            osc7_process_cwd: self.osc7_process_cwd,
            title: self.title,
            started_ms: self.started_ms,
            last_output_ms: self.last_output_ms,
            output_offset: self.output_offset,
            screen: self.screen,
            history: self.history,
            startup_input: self.startup_input,
        }
    }
}

impl From<TerminalHandoff> for ProcessHandoff {
    fn from(state: TerminalHandoff) -> Self {
        Self {
            id: state.id,
            fd: state.fd,
            pid: state.pid,
            cols: state.cols,
            rows: state.rows,
            cwd: state.cwd,
            osc7_cwd: state.osc7_cwd,
            osc7_foreground: state.osc7_foreground,
            osc7_process_cwd: state.osc7_process_cwd,
            title: state.title,
            started_ms: state.started_ms,
            last_output_ms: state.last_output_ms,
            output_offset: state.output_offset,
            screen: state.screen,
            history: state.history,
            startup_input: state.startup_input,
        }
    }
}

pub fn encode(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

pub fn decode(text: &str) -> Vec<u8> {
    base64::engine::general_purpose::STANDARD
        .decode(text)
        .unwrap_or_default()
}

/// Set or clear close-on-exec on `fd`.
pub fn set_cloexec(fd: RawFd, on: bool) -> io::Result<()> {
    // SAFETY: F_GETFD/F_SETFD only read and write descriptor flags.
    unsafe {
        let flags = libc::fcntl(fd, libc::F_GETFD);
        if flags < 0 {
            return Err(io::Error::last_os_error());
        }
        let flags = if on {
            flags | libc::FD_CLOEXEC
        } else {
            flags & !libc::FD_CLOEXEC
        };
        if libc::fcntl(fd, libc::F_SETFD, flags) < 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

/// A duplicate of `fd` that survives `exec`. The caller owns it.
pub fn dup_inheritable(fd: RawFd) -> io::Result<RawFd> {
    // SAFETY: F_DUPFD returns a new descriptor (without close-on-exec) or -1.
    let dup = unsafe { libc::fcntl(fd, libc::F_DUPFD, 3) };
    if dup < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(dup)
    }
}

/// Close a descriptor the caller owns.
pub fn close(fd: RawFd) {
    // SAFETY: the caller owns `fd` and doesn't use it afterwards.
    unsafe {
        libc::close(fd);
    }
}

/// Close every descriptor the state hands over (the switch failed for
/// good: the PTYs close and their programs get SIGHUP).
pub fn close_all(state: &HandoffState) {
    close(state.claim.listener_fd);
    close(state.claim.lock_fd);
    for terminal in &state.terminals {
        close(terminal.fd);
    }
    for pane in &state.panes {
        close(pane.terminal.fd);
    }
}

/// Write `state` to an unlinked private file in `dir`; returns its
/// descriptor, rewound and inheritable.
pub fn write_state(dir: &Path, state: &HandoffState) -> io::Result<RawFd> {
    let path = dir.join(format!("handoff.{}.json", std::process::id()));
    let _ = std::fs::remove_file(&path);
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)?;
    std::fs::remove_file(&path)?;
    serde_json::to_writer(&mut file, state).map_err(io::Error::other)?;
    file.seek(SeekFrom::Start(0))?;
    let fd = file.into_raw_fd();
    if let Err(e) = set_cloexec(fd, false) {
        close(fd);
        return Err(e);
    }
    Ok(fd)
}

/// Read the state from the descriptor `serve --handoff-fd` names, closing
/// it.
pub fn read_state(fd: RawFd) -> io::Result<HandoffState> {
    // SAFETY: the previous image passed us this descriptor; we own it now.
    let file = unsafe { File::from_raw_fd(fd) };
    let state: HandoffState = serde_json::from_reader(io::BufReader::new(file))
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    if !READABLE_HANDOFF_VERSIONS.contains(&state.version) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "handoff state version {} (this daemon reads {:?})",
                state.version, READABLE_HANDOFF_VERSIONS
            ),
        ));
    }
    Ok(state)
}

/// Check that `binary` is an executable file that reads this daemon's
/// handoff state. Runs `binary handoff-versions`.
pub fn check_target(binary: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    if !binary.is_absolute() {
        return Err(format!("{} is not an absolute path", binary.display()));
    }
    let meta = std::fs::metadata(binary).map_err(|e| format!("{}: {e}", binary.display()))?;
    if !meta.is_file() || meta.permissions().mode() & 0o111 == 0 {
        return Err(format!("{} is not an executable file", binary.display()));
    }
    let mut child = Command::new(binary)
        .arg(VERSIONS_COMMAND)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("can't run {}: {e}", binary.display()))?;
    let deadline = Instant::now() + CHECK_TIMEOUT;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(10)),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("{} didn't answer", binary.display()));
            }
            Err(e) => return Err(format!("can't run {}: {e}", binary.display())),
        }
    };
    let mut out = String::new();
    if let Some(mut stdout) = child.stdout.take() {
        let _ = io::Read::read_to_string(&mut stdout, &mut out);
    }
    let reads = status.success()
        && out
            .split_whitespace()
            .any(|v| v.parse::<u32>() == Ok(HANDOFF_VERSION));
    if reads {
        Ok(())
    } else {
        Err(format!(
            "{} can't take over running sessions (it's from a Rakumux without in-place restarts)",
            binary.display()
        ))
    }
}

/// Replace this process with `binary serve --handoff-fd <fd>`. Returns
/// only on failure; the descriptor is then closed.
pub fn exec(binary: &Path, state_fd: RawFd) -> io::Error {
    let args = [
        CString::new(binary.as_os_str().as_bytes()),
        CString::new("serve"),
        CString::new("--handoff-fd"),
        CString::new(state_fd.to_string()),
    ];
    let args: Vec<CString> = match args.into_iter().collect::<Result<_, _>>() {
        Ok(args) => args,
        Err(e) => {
            close(state_fd);
            return io::Error::new(io::ErrorKind::InvalidInput, e);
        }
    };
    let mut argv: Vec<*const libc::c_char> = args.iter().map(|a| a.as_ptr()).collect();
    argv.push(std::ptr::null());
    log_info!("restarting in place: exec {}", binary.display());
    // SAFETY: `argv` is a null-terminated array of valid C strings that
    // outlive the call; execv only returns on failure.
    unsafe {
        libc::execv(argv[0], argv.as_ptr());
    }
    let error = io::Error::last_os_error();
    log_error!("exec {} failed: {error}", binary.display());
    close(state_fd);
    error
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(dir: &Path) -> HandoffState {
        HandoffState {
            panes: Vec::new(),
            version: HANDOFF_VERSION,
            from_version: "0.1.0".into(),
            home: dir.to_path_buf(),
            run_dir: dir.join("run"),
            child_umask: Some(0o022),
            claim: ClaimHandoff {
                registration: Registration {
                    instance_id: "i".into(),
                    pid: 1,
                    protocol: 1,
                    version: "0.1.0".into(),
                    socket: "/tmp/s.sock".into(),
                    token: "t".into(),
                },
                listener_fd: 7,
                lock_fd: 8,
            },
            control: ControlHandoff {
                tokens: vec![(SessionId::from_string("01A"), "secret".into())],
                ..ControlHandoff::default()
            },
            terminals: vec![TerminalHandoff {
                id: 3,
                session_id: SessionId::from_string("01A"),
                fd: 9,
                pid: 42,
                cols: 80,
                rows: 24,
                cwd: Some("/tmp".into()),
                osc7_cwd: false,
                osc7_foreground: None,
                osc7_process_cwd: None,
                title: "t".into(),
                started_ms: 5,
                last_output_ms: None,
                output_offset: 1234,
                screen: encode(b"\x1b[Hhello"),
                history: vec![encode(b"old")],
                startup_input: None,
            }],
        }
    }

    #[test]
    fn state_round_trips_through_an_unlinked_private_file() {
        let dir = tempfile::tempdir().unwrap();
        let fd = write_state(dir.path(), &state(dir.path())).unwrap();
        // Nothing is left on disk; the descriptor survives exec.
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
        // SAFETY: F_GETFD on a descriptor we own.
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
        assert_eq!(flags & libc::FD_CLOEXEC, 0);
        let read = read_state(fd).unwrap();
        assert_eq!(read.terminals[0].output_offset, 1234);
        assert_eq!(decode(&read.terminals[0].screen), b"\x1b[Hhello");
        assert_eq!(read.control.tokens[0].1, "secret");
        assert_eq!(read.claim.registration.instance_id, "i");
    }

    #[test]
    fn another_state_version_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let mut newer = state(dir.path());
        newer.version = HANDOFF_VERSION + 1;
        let fd = write_state(dir.path(), &newer).unwrap();
        let error = read_state(fd).unwrap_err();
        assert!(error.to_string().contains("version"), "{error}");
    }

    #[test]
    fn legacy_handoff_state_is_still_readable_for_in_place_upgrades() {
        let dir = tempfile::tempdir().unwrap();
        let mut old = state(dir.path());
        old.version = 1;
        let fd = write_state(dir.path(), &old).unwrap();
        let restored = read_state(fd).unwrap();
        assert_eq!(restored.version, 1);
        assert!(restored.panes.is_empty());
        assert_eq!(restored.terminals.len(), 1);
    }

    #[test]
    fn targets_must_read_this_state_version() {
        let dir = tempfile::tempdir().unwrap();
        let script = |name: &str, body: &str| {
            use std::os::unix::fs::PermissionsExt;
            let path = dir.path().join(name);
            std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
            path
        };
        let reads = script("new", &format!("echo {HANDOFF_VERSION}"));
        assert_eq!(check_target(&reads), Ok(()));
        // A version-1 reader cannot preserve standalone pane descriptors.
        let session_only = script("session-only", "echo 1");
        assert!(check_target(&session_only).is_err());
        // An older daemon prints its usage and exits 2.
        let older = script("old", "echo usage >&2; exit 2");
        assert!(check_target(&older)
            .unwrap_err()
            .contains("can't take over"));
        assert!(check_target(Path::new("relative/ptyd")).is_err());
        assert!(check_target(&dir.path().join("missing")).is_err());
    }
}
