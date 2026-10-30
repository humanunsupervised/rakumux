//! Blocking PTY threads: one reader, one writer and one child waiter per
//! terminal. They talk to the actor through bounded channels. Also the PTY
//! master handle the actor resizes through.

use super::actor::ActorMsg;
use crate::host::EnterCheck;
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
    /// `then` goes only while the process group that had the foreground when
    /// `bytes` were written still has it.
    pub(crate) same_foreground: bool,
    /// `then` goes only if this says so when it's due (after the pause).
    pub(crate) then_if: Option<EnterCheck>,
}

impl From<Vec<u8>> for PtyInput {
    fn from(bytes: Vec<u8>) -> Self {
        Self {
            bytes,
            then: None,
            same_foreground: false,
            then_if: None,
        }
    }
}

/// Write queued input to the PTY in order until the queue closes or a write
/// fails. A pause holds back what was queued after it.
pub(crate) fn run_writer(pty: File, rx: Receiver<PtyInput>) {
    let fd = pty.as_raw_fd();
    write_inputs(pty, rx, || {
        // SAFETY: tcgetpgrp only reads the descriptor, which `pty` keeps open.
        match unsafe { libc::tcgetpgrp(fd) } {
            pgrp if pgrp > 0 => Some(pgrp),
            _ => None,
        }
    });
}

/// [`run_writer`]'s loop: `foreground` reads the PTY's foreground process
/// group. What follows a pause (a submit's Enter) is written, when the input
/// asks (`same_foreground`), only while the group that had the foreground
/// when the text before it was written still has it: had that program
/// exited meanwhile, the Enter would run the text in the shell it left (the
/// shell's readline keeps a paste unrun until then). Without it the text is
/// type-ahead for whatever reads next, as typing is. With `then_if`, it is
/// written only if that says so then (an agent that came to ask something
/// meanwhile would take the Enter as its answer).
fn write_inputs(mut pty: impl Write, rx: Receiver<PtyInput>, foreground: impl Fn() -> Option<i32>) {
    for input in rx {
        let guarded = input.then.is_some() && input.same_foreground;
        let before = guarded.then(&foreground).flatten();
        if let Err(e) = pty.write_all(&input.bytes) {
            log_debug!("pty write failed: {e}");
            break;
        }
        if let Some((pause, bytes)) = input.then {
            std::thread::sleep(pause);
            if guarded && foreground() != before {
                log_debug!("the foreground changed after a submit's text; its Enter is dropped");
                continue;
            }
            if input.then_if.is_some_and(|check| !check()) {
                log_debug!("a submit's Enter is held back: its check refused it");
                continue;
            }
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::sync::{mpsc, Mutex};

    /// A PTY stand-in whose bytes the test reads once the loop ends.
    #[derive(Clone, Default)]
    struct Written(Arc<Mutex<Vec<u8>>>);

    impl Write for Written {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn submit(text: &str) -> PtyInput {
        PtyInput {
            bytes: text.as_bytes().to_vec(),
            then: Some((Duration::from_millis(1), b"\r".to_vec())),
            same_foreground: true,
            then_if: None,
        }
    }

    fn written_by(inputs: Vec<PtyInput>, foreground: impl Fn() -> Option<i32>) -> Vec<u8> {
        let (tx, rx) = mpsc::channel();
        for input in inputs {
            tx.send(input).unwrap();
        }
        drop(tx);
        let written = Written::default();
        write_inputs(written.clone(), rx, foreground);
        let bytes = written.0.lock().unwrap().clone();
        bytes
    }

    #[test]
    fn a_submits_enter_follows_its_text_while_the_same_program_has_the_foreground() {
        let after = PtyInput::from(b"x".to_vec());
        assert_eq!(written_by(vec![submit("hi"), after], || Some(42)), b"hi\rx");
        // Nothing to compare (the descriptor can't say): written as before.
        assert_eq!(written_by(vec![submit("hi")], || None), b"hi\r");
    }

    #[test]
    fn a_submits_enter_is_dropped_once_another_group_has_the_foreground() {
        // The agent (group 42) exits during the pause and its shell (group 7)
        // has the foreground: the text stays unrun at the shell's prompt.
        let reads = Cell::new(0);
        let foreground = || {
            reads.set(reads.get() + 1);
            Some(if reads.get() == 1 { 42 } else { 7 })
        };
        let after = PtyInput::from(b"x".to_vec());
        assert_eq!(
            written_by(vec![submit("rm -rf build"), after], foreground),
            b"rm -rf buildx"
        );
        // Not asked to: type-ahead, the Enter goes to whoever reads it.
        let reads = Cell::new(0);
        let foreground = || {
            reads.set(reads.get() + 1);
            Some(if reads.get() == 1 { 42 } else { 7 })
        };
        let ahead = PtyInput {
            same_foreground: false,
            ..submit("echo next")
        };
        assert_eq!(written_by(vec![ahead], foreground), b"echo next\r");
    }

    #[test]
    fn a_submits_enter_goes_only_if_its_check_says_so_once_the_text_is_in() {
        // Asked after the text was written: whatever opened meanwhile shows.
        let written = Written::default();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let check = |answer: bool| -> EnterCheck {
            let (written, seen) = (written.clone(), seen.clone());
            Box::new(move || {
                seen.lock().unwrap().push(written.0.lock().unwrap().clone());
                answer
            })
        };
        let (tx, rx) = mpsc::channel();
        for (text, answer) in [("approve?", false), ("hi", true)] {
            tx.send(PtyInput {
                then_if: Some(check(answer)),
                ..submit(text)
            })
            .unwrap();
        }
        tx.send(PtyInput::from(b"x".to_vec())).unwrap();
        drop(tx);
        write_inputs(written.clone(), rx, || Some(42));
        // The first Enter is held back (its text stays unsent); what follows
        // is written as usual.
        assert_eq!(*written.0.lock().unwrap(), b"approve?hi\rx");
        assert_eq!(
            *seen.lock().unwrap(),
            [b"approve?".to_vec(), b"approve?hi".to_vec()]
        );
    }
}
