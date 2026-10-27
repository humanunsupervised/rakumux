//! Blocking PTY threads: one reader, one writer and one child waiter per
//! terminal. They talk to the actor through bounded channels. Also the PTY
//! master handle the actor resizes through.

use super::actor::ActorMsg;
use crate::log_debug;
use std::fs::File;
use std::io::{self, Read, Write};
use std::os::unix::io::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, SyncSender};
use std::sync::Arc;
use std::time::Duration;

/// Bytes per PTY read.
const READ_CHUNK: usize = 16 * 1024;
/// How often a blocked reader checks its stop flag.
const READ_POLL_MS: i32 = 200;

/// A close-on-exec duplicate of `fd`, owned by the returned file.
pub(crate) fn dup_fd(fd: RawFd) -> io::Result<File> {
    // SAFETY: F_DUPFD_CLOEXEC returns a new descriptor (or -1) and does not
    // touch memory.
    let dup = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 0) };
    if dup < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `dup` is a fresh descriptor nobody else owns.
    Ok(unsafe { File::from_raw_fd(dup) })
}

/// Read PTY output into the actor until EOF, an error, or `stop`. Polls so a
/// stop request is noticed even when a background process keeps the PTY
/// slave open.
pub(crate) fn run_reader(mut pty: File, tx: SyncSender<ActorMsg>, stop: Arc<AtomicBool>) {
    let mut buf = vec![0u8; READ_CHUNK];
    while !stop.load(Ordering::Relaxed) {
        let mut pfd = libc::pollfd {
            fd: pty.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: one valid pollfd for the duration of the call.
        let ready = unsafe { libc::poll(&mut pfd, 1, READ_POLL_MS) };
        if ready < 0 {
            let err = io::Error::last_os_error();
            if err.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            log_debug!("pty poll failed: {err}");
            break;
        }
        if ready == 0 {
            continue;
        }
        match pty.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                if tx.send(ActorMsg::Output(buf[..n].to_vec())).is_err() {
                    return;
                }
            }
            Err(e)
                if e.kind() == io::ErrorKind::Interrupted
                    || e.kind() == io::ErrorKind::WouldBlock => {}
            // EIO: every slave descriptor is closed.
            Err(_) => break,
        }
    }
    let _ = tx.send(ActorMsg::Eof);
}

/// Input for a terminal's writer: bytes written in one go, then optionally
/// more once the program has had a while to take them in, with nothing else
/// written to the PTY in between.
pub(crate) struct PtyInput {
    pub(crate) bytes: Vec<u8>,
    /// Written after the pause, in a write (and so a read) of its own.
    pub(crate) then: Option<(Duration, Vec<u8>)>,
}

impl From<Vec<u8>> for PtyInput {
    fn from(bytes: Vec<u8>) -> Self {
        Self { bytes, then: None }
    }
}

/// Write queued input to the PTY in order until the queue closes or a write
/// fails. A pause holds back what was queued after it.
pub(crate) fn run_writer(mut pty: File, rx: Receiver<PtyInput>) {
    for input in rx {
        if let Err(e) = pty.write_all(&input.bytes) {
            log_debug!("pty write failed: {e}");
            break;
        }
        if let Some((pause, bytes)) = input.then {
            std::thread::sleep(pause);
            if let Err(e) = pty.write_all(&bytes) {
                log_debug!("pty write failed: {e}");
                break;
            }
        }
    }
}

/// A PTY master: resizes and the foreground process group. Owns its
/// (close-on-exec) descriptor.
pub(crate) struct PtyMaster {
    fd: OwnedFd,
}

impl PtyMaster {
    pub(crate) fn new(fd: OwnedFd) -> Self {
        Self { fd }
    }

    pub(crate) fn raw_fd(&self) -> RawFd {
        self.fd.as_raw_fd()
    }

    /// Set the window size; the kernel signals the foreground group
    /// (SIGWINCH) when it changed.
    pub(crate) fn resize(&self, cols: u16, rows: u16) -> io::Result<()> {
        let size = libc::winsize {
            ws_row: rows,
            ws_col: cols,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        // SAFETY: TIOCSWINSZ reads one `winsize` that outlives the call.
        if unsafe { libc::ioctl(self.fd.as_raw_fd(), libc::TIOCSWINSZ as _, &size) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    /// The PTY's foreground process group.
    pub(crate) fn process_group_leader(&self) -> Option<i32> {
        // SAFETY: tcgetpgrp only reads the descriptor.
        match unsafe { libc::tcgetpgrp(self.fd.as_raw_fd()) } {
            pid if pid > 0 => Some(pid),
            _ => None,
        }
    }
}

/// Wait until the child `pid` has exited, without reaping it (`WNOWAIT`),
/// then tell the actor, which reaps it ([`reap`]). A child that exits
/// while its terminal is being handed to a new daemon image stays a zombie
/// for that image to reap, so its exit code isn't lost.
pub(crate) fn run_waiter(pid: u32, tx: SyncSender<ActorMsg>) {
    loop {
        // SAFETY: waitid writes one `siginfo_t` we own.
        let rc = unsafe {
            let mut info: libc::siginfo_t = std::mem::zeroed();
            libc::waitid(
                libc::P_PID,
                pid as libc::id_t,
                &mut info,
                libc::WEXITED | libc::WNOWAIT,
            )
        };
        if rc == 0 {
            break;
        }
        let err = io::Error::last_os_error();
        if err.kind() != io::ErrorKind::Interrupted {
            // ECHILD: already reaped, or not our child.
            log_debug!("waiting for {pid} failed: {err}");
            break;
        }
    }
    let _ = tx.send(ActorMsg::ChildExited);
}

/// Reap the exited child `pid`: its exit code, or `None` when a signal
/// ended it or it was already reaped.
pub(crate) fn reap(pid: u32) -> Option<i32> {
    let mut status: libc::c_int = 0;
    loop {
        // SAFETY: waitpid writes one int we own.
        let rc = unsafe { libc::waitpid(pid as libc::pid_t, &mut status, libc::WNOHANG) };
        if rc == pid as libc::pid_t {
            return libc::WIFEXITED(status).then(|| libc::WEXITSTATUS(status));
        }
        if rc < 0 && io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
            continue;
        }
        return None;
    }
}
