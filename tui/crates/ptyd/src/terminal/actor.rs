//! The per-terminal actor: one thread that owns the emulator, the replay
//! ring, the PTY master (for resizes and the foreground process group) and
//! the attachments. Output, commands and child exit arrive on one bounded
//! channel and are handled strictly in order.

use super::pty_io::{self, PtyInput, PtyMaster};
use super::read::{formatted_scrollback, read_screen};
use super::ring::ReplayRing;
use super::sink::{AttachmentId, Sink};
use super::{clamp_size, now_ms, HostInner, Shared};
use crate::handoff::{self, ProcessHandoff};
use crate::host::{HostError, HostEvent, TerminalProcessInfo, TerminalState};
use crate::persist::SessionMeta;
use crate::{log_debug, log_warn};
use base64::Engine;
use session_core::api::{ReadResult, ReadSource, Role, StreamEvent, TextFormat};
use session_core::client::AttachEvent;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, SyncSender, TrySendError};
use std::sync::Arc;
use std::time::{Duration, Instant};
use vt::Emulator;

/// Flush coalesced output once this much is pending…
const COALESCE_BYTES: usize = 8 * 1024;
/// …or once the oldest pending byte is this old.
const COALESCE_DELAY: Duration = Duration::from_millis(1);
/// Events queued per attachment channel (beyond that: the sink backlog).
pub(crate) const ATTACH_QUEUE: usize = 64;
/// A sink backlog larger than this triggers backpressure or a resync.
const BACKLOG_LIMIT: usize = 1024 * 1024;
/// How long output waits for a slow controller before it is resynced.
const CONTROLLER_PATIENCE: Duration = Duration::from_secs(5);
/// An attachment whose backlog makes no progress this long is closed.
const STALL_LIMIT: Duration = Duration::from_secs(10);
/// Startup input is typed after the first output or this delay.
const STARTUP_DELAY: Duration = Duration::from_millis(500);
/// `HostEvent::Output` at most this often…
const OUTPUT_EVENT_INTERVAL: Duration = Duration::from_secs(1);
/// …plus one this long after output stops.
const OUTPUT_SETTLE: Duration = Duration::from_millis(500);
/// Foreground process refresh interval while output flows.
const FOREGROUND_REFRESH: Duration = Duration::from_secs(2);
/// SIGKILL follows SIGHUP after this long.
const KILL_GRACE: Duration = Duration::from_secs(3);
/// After the child exits, wait this long for the PTY to drain.
const EXIT_DRAIN: Duration = Duration::from_millis(250);
/// Idle wake-up interval for timers.
const TICK: Duration = Duration::from_millis(50);
/// Wake-up interval while some attachment has a backlog.
const BUSY_TICK: Duration = Duration::from_millis(5);
/// While the controller typed this recently, another window's `Fit` is
/// ignored: the person typing keeps their size.
const CONTROLLER_TYPING: Duration = Duration::from_secs(5);
/// How long a handoff waits for the reader to pass on what it read.
const HANDOFF_DRAIN: Duration = Duration::from_secs(1);

pub(crate) enum ActorMsg {
    Output(Vec<u8>),
    /// The PTY reader ended (slave closed, read error, or stopped).
    Eof,
    /// The child exited (not reaped yet; the actor reaps it).
    ChildExited,
    Attach {
        role: Role,
        cols: u16,
        rows: u16,
        reply: Sender<Result<AttachReply, HostError>>,
    },
    Detach(AttachmentId),
    Checkpoint {
        attachment: AttachmentId,
        reply: Sender<Result<(), HostError>>,
    },
    Resize {
        attachment: AttachmentId,
        cols: u16,
        rows: u16,
        reply: Sender<Result<(), HostError>>,
    },
    Promote {
        attachment: AttachmentId,
        cols: u16,
        rows: u16,
        reply: Sender<Result<(), HostError>>,
    },
    Demote(AttachmentId),
    Fit {
        attachment: AttachmentId,
        cols: u16,
        rows: u16,
        reply: Sender<Result<(), HostError>>,
    },
    Read {
        source: ReadSource,
        lines: Option<u32>,
        format: TextFormat,
        reply: Sender<ReadResult>,
    },
    /// Raw output from an offset, if still retained.
    ReplaySince {
        offset: u64,
        reply: Sender<Option<Vec<u8>>>,
    },
    /// The foreground process's name now, read afresh.
    Foreground {
        reply: Sender<Option<String>>,
    },
    Terminate {
        reply: Sender<Result<(), HostError>>,
    },
    /// Save scrollback now.
    Persist {
        reply: Sender<()>,
    },
    /// The terminal was forgotten: no more events or persistence; terminate
    /// the process if it still runs and stop once it is gone.
    Forget,
    /// Hand the running terminal to the next daemon image and stop; `None`
    /// (and keep going) when it isn't running.
    Handoff {
        reply: Sender<Option<ProcessHandoff>>,
    },
    /// Stop the actor now.
    Stop,
}

pub(crate) struct AttachReply {
    pub(crate) attachment: AttachmentId,
    pub(crate) info: TerminalProcessInfo,
    pub(crate) checkpoint: Vec<u8>,
    pub(crate) offset: u64,
    pub(crate) events: Receiver<AttachEvent>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Running,
    /// The child exited; waiting (briefly) for the PTY to drain.
    Exiting {
        code: Option<i32>,
        deadline: Instant,
    },
    Exited {
        at: Instant,
    },
}

#[derive(PartialEq, Eq)]
enum Flow {
    Continue,
    Stop,
}

/// Everything the actor needs at start.
pub(crate) struct ActorParts {
    pub(crate) shared: Arc<Shared>,
    pub(crate) host: Arc<HostInner>,
    pub(crate) master: PtyMaster,
    pub(crate) rx: Receiver<ActorMsg>,
    pub(crate) writer: SyncSender<PtyInput>,
    pub(crate) reader_stop: Arc<AtomicBool>,
    pub(crate) pid: u32,
    pub(crate) cols: u16,
    pub(crate) rows: u16,
    pub(crate) startup_input: Vec<u8>,
    /// A terminal a previous daemon image handed over.
    pub(crate) adopted: Option<Adopted>,
}

/// What a terminal handed over brings besides its PTY.
pub(crate) struct Adopted {
    /// Already showing the handed-over screen and history.
    pub(crate) emulator: Emulator,
    /// Output bytes so far.
    pub(crate) total: u64,
    pub(crate) osc7_cwd: bool,
    pub(crate) osc7_foreground: Option<u32>,
    pub(crate) osc7_process_cwd: Option<std::path::PathBuf>,
}

pub(crate) struct Actor {
    shared: Arc<Shared>,
    host: Arc<HostInner>,
    master: PtyMaster,
    rx: Receiver<ActorMsg>,
    /// Query replies and startup input; `None` once the terminal ended.
    writer: Option<SyncSender<PtyInput>>,
    reader_stop: Arc<AtomicBool>,
    pid: u32,
    emulator: Emulator,
    ring: ReplayRing,
    /// Total output bytes received (= offset of the next byte).
    total: u64,
    pending: Vec<u8>,
    pending_offset: u64,
    pending_since: Option<Instant>,
    sinks: Vec<Sink>,
    next_attachment: AttachmentId,
    controller: Option<AttachmentId>,
    startup: Option<Vec<u8>>,
    startup_due: Instant,
    last_output_event: Option<Instant>,
    settle_due: Option<Instant>,
    last_foreground_refresh: Option<Instant>,
    /// The program reported its cwd via OSC 7 (preferred over inspection).
    osc7_cwd: bool,
    osc7_foreground: Option<u32>,
    osc7_process_cwd: Option<std::path::PathBuf>,
    output_since_persist: bool,
    last_persist: Instant,
    phase: Phase,
    eof: bool,
    exit_code: Option<i32>,
    kill_due: Option<Instant>,
    kill_groups: Vec<i32>,
    forgotten: bool,
    /// Handed over by a previous daemon image (refresh at start).
    adopted: bool,
    pane_reconnect_due: Option<Instant>,
}

impl Actor {
    pub(crate) fn new(parts: ActorParts) -> Self {
        let now = Instant::now();
        let config = &parts.host.config;
        let adopted = parts.adopted.is_some();
        let pane_reconnect_due =
            (adopted && parts.shared.owner.session_id().is_none()).then(|| {
                now + config
                    .pane_reconnect_grace
                    .unwrap_or(Duration::from_secs(30))
            });
        let (emulator, total, osc7_cwd, osc7_foreground, osc7_process_cwd) = match parts.adopted {
            Some(adopted) => (
                adopted.emulator,
                adopted.total,
                adopted.osc7_cwd,
                adopted.osc7_foreground,
                adopted.osc7_process_cwd,
            ),
            None => (
                Emulator::responding(
                    parts.rows,
                    parts.cols,
                    config.scrollback_rows,
                    config.colors,
                ),
                0,
                false,
                None,
                None,
            ),
        };
        let ring = ReplayRing::new(config.replay_bytes);
        Self {
            shared: parts.shared,
            master: parts.master,
            rx: parts.rx,
            writer: Some(parts.writer),
            reader_stop: parts.reader_stop,
            pid: parts.pid,
            emulator,
            ring,
            total,
            pending: Vec::new(),
            pending_offset: total,
            pending_since: None,
            sinks: Vec::new(),
            next_attachment: 1,
            controller: None,
            startup: (!parts.startup_input.is_empty()).then_some(parts.startup_input),
            startup_due: now + STARTUP_DELAY,
            last_output_event: None,
            settle_due: None,
            last_foreground_refresh: None,
            osc7_cwd,
            osc7_foreground,
            osc7_process_cwd,
            output_since_persist: false,
            last_persist: now,
            phase: Phase::Running,
            eof: false,
            exit_code: None,
            kill_due: None,
            kill_groups: Vec::new(),
            forgotten: false,
            adopted,
            pane_reconnect_due,
            host: parts.host,
        }
    }

    /// Process messages until the terminal is stopped.
    pub(crate) fn run(&mut self) {
        if self.adopted {
            // Nothing may arrive for a while (an agent waiting for its
            // user): learn the foreground process now, and let the control
            // layer look at the screen once it listens.
            let now = Instant::now();
            self.refresh_foreground(now);
            self.settle_due = Some(now + OUTPUT_SETTLE);
        }
        loop {
            let now = Instant::now();
            let timeout = self.next_wakeup(now).saturating_duration_since(now);
            let flow = match self.rx.recv_timeout(timeout) {
                Ok(msg) => self.handle(msg),
                Err(RecvTimeoutError::Timeout) => Flow::Continue,
                Err(RecvTimeoutError::Disconnected) => Flow::Stop,
            };
            if flow == Flow::Stop || self.tick(Instant::now()) == Flow::Stop {
                break;
            }
        }
        self.teardown();
    }

    fn next_wakeup(&self, now: Instant) -> Instant {
        let tick = if self.sinks.iter().any(Sink::has_backlog) {
            BUSY_TICK
        } else {
            TICK
        };
        let mut at = now + tick;
        if let Some(since) = self.pending_since {
            at = at.min(since + COALESCE_DELAY);
        }
        at
    }

    fn running(&self) -> bool {
        self.phase == Phase::Running
    }

    fn handle(&mut self, msg: ActorMsg) -> Flow {
        let now = Instant::now();
        match msg {
            ActorMsg::Output(bytes) => self.on_output(&bytes, now),
            ActorMsg::Eof => {
                self.eof = true;
                if let Phase::Exiting { code, .. } = self.phase {
                    return self.finalize(code, now);
                }
            }
            ActorMsg::ChildExited => {
                let code = pty_io::reap(self.pid);
                self.exit_code = code;
                if self.eof {
                    return self.finalize(code, now);
                }
                self.phase = Phase::Exiting {
                    code,
                    deadline: now + EXIT_DRAIN,
                };
            }
            ActorMsg::Attach {
                role,
                cols,
                rows,
                reply,
            } => {
                let result = self.attach(role, cols, rows, now);
                let _ = reply.send(result);
            }
            ActorMsg::Detach(id) => self.detach(id),
            ActorMsg::Checkpoint { attachment, reply } => {
                self.flush(now);
                let (rows, cols) = self.emulator.size();
                let checkpoint =
                    base64::engine::general_purpose::STANDARD.encode(self.emulator.checkpoint());
                let result = match self.sinks.iter_mut().find(|sink| sink.id == attachment) {
                    Some(sink) => {
                        sink.push(
                            AttachEvent::Event(StreamEvent::Resized {
                                rows,
                                cols,
                                checkpoint,
                                offset: self.total,
                            }),
                            now,
                        );
                        sink.pump(now);
                        Ok(())
                    }
                    None => Err(HostError::NotFound),
                };
                let _ = reply.send(result);
            }
            ActorMsg::Resize {
                attachment,
                cols,
                rows,
                reply,
            } => {
                let result = if !self.running() {
                    Err(HostError::NotRunning)
                } else if self.controller != Some(attachment) {
                    Err(HostError::NotController)
                } else {
                    self.flush(now);
                    self.resize_to(cols, rows, now)
                };
                let _ = reply.send(result);
            }
            ActorMsg::Fit {
                attachment,
                cols,
                rows,
                reply,
            } => {
                let result = if !self.running() {
                    Err(HostError::NotRunning)
                } else if !self.sinks.iter().any(|s| s.id == attachment) {
                    Err(HostError::NotFound)
                } else if self.controller_typing() {
                    // Someone is typing in it elsewhere: their size wins.
                    Ok(())
                } else {
                    self.flush(now);
                    self.resize_to(cols, rows, now)
                };
                let _ = reply.send(result);
            }
            ActorMsg::Promote {
                attachment,
                cols,
                rows,
                reply,
            } => {
                let result = if !self.running() {
                    Err(HostError::NotRunning)
                } else if !self.sinks.iter().any(|s| s.id == attachment) {
                    Err(HostError::NotFound)
                } else {
                    self.flush(now);
                    self.take_control(attachment, cols, rows, now)
                };
                let _ = reply.send(result);
            }
            ActorMsg::Demote(id) => self.demote(id, now),
            ActorMsg::Read {
                source,
                lines,
                format,
                reply,
            } => {
                let _ = reply.send(read_screen(&mut self.emulator, source, lines, format));
            }
            ActorMsg::ReplaySince { offset, reply } => {
                let _ = reply.send(self.ring.since(offset));
            }
            ActorMsg::Foreground { reply } => {
                let leader = self.master.process_group_leader().map(|pid| pid as u32);
                let name = leader
                    .and_then(crate::procinfo::process_info)
                    .map(|process| process.name);
                let _ = reply.send(name);
            }
            ActorMsg::Terminate { reply } => {
                let result = if self.running() {
                    self.terminate(now);
                    Ok(())
                } else {
                    Err(HostError::NotRunning)
                };
                let _ = reply.send(result);
            }
            ActorMsg::Persist { reply } => {
                self.persist(now);
                let _ = reply.send(());
            }
            ActorMsg::Forget => {
                self.forgotten = true;
                match self.phase {
                    Phase::Running => self.terminate(now),
                    Phase::Exiting { .. } => {}
                    Phase::Exited { .. } => return Flow::Stop,
                }
            }
            ActorMsg::Handoff { reply } => {
                let handed = self.hand_off();
                let stop = handed.is_some();
                let _ = reply.send(handed);
                if stop {
                    return Flow::Stop;
                }
            }
            ActorMsg::Stop => return Flow::Stop,
        }
        self.reap_sinks();
        Flow::Continue
    }

    /// Stop reading, take in what the reader already read (the rest waits
    /// in the PTY for the next image) and describe the terminal with an
    /// inheritable PTY descriptor. `None` unless it is running.
    fn hand_off(&mut self) -> Option<ProcessHandoff> {
        if !self.running() {
            return None;
        }
        self.reader_stop.store(true, Ordering::Relaxed);
        let deadline = Instant::now() + HANDOFF_DRAIN;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.rx.recv_timeout(left) {
                Ok(ActorMsg::Output(bytes)) => self.on_output(&bytes, Instant::now()),
                Ok(ActorMsg::Eof) | Err(_) => break,
                // A child that exited stays unreaped for the next image;
                // requests are dropped, so their callers see an error.
                Ok(_) => {}
            }
        }
        self.flush(Instant::now());
        let fd = match handoff::dup_inheritable(self.master.raw_fd()) {
            Ok(fd) => fd,
            Err(e) => {
                log_warn!("terminal {}: can't hand over the pty: {e}", self.shared.id);
                return None;
            }
        };
        let history_len = self.emulator.history_len();
        let history = self
            .emulator
            .history_rows_formatted(0, history_len)
            .into_iter()
            .map(|row| handoff::encode(&row.content))
            .collect();
        let state = self.shared.lock();
        Some(ProcessHandoff {
            id: self.shared.id.0,
            fd,
            pid: self.pid,
            cols: state.info.cols,
            rows: state.info.rows,
            cwd: state.info.cwd.clone(),
            osc7_cwd: self.osc7_cwd,
            osc7_foreground: self.osc7_foreground,
            osc7_process_cwd: self.osc7_process_cwd.clone(),
            title: state.info.title.clone(),
            started_ms: state.info.started_ms,
            last_output_ms: state.info.last_output_ms,
            output_offset: self.total,
            screen: handoff::encode(&self.emulator.checkpoint()),
            history,
            startup_input: self.startup.take().map(|input| handoff::encode(&input)),
        })
    }

    fn tick(&mut self, now: Instant) -> Flow {
        if self.running()
            && self
                .last_foreground_refresh
                .is_none_or(|at| now.saturating_duration_since(at) >= FOREGROUND_REFRESH)
            && self.refresh_foreground(now)
            && self.settle_due.is_none()
        {
            // A quiet terminal's program changed; output in flight is
            // reported by its trailing event instead.
            self.publish(|session_id| HostEvent::Output {
                id: self.shared.id,
                session_id,
            });
        }
        if self.pane_reconnect_due.is_some_and(|due| now >= due) {
            self.pane_reconnect_due = None;
            if self.running() && self.sinks.is_empty() {
                log_debug!(
                    "terminal {}: no owning window reconnected; closing temporary pane",
                    self.shared.id
                );
                self.forgotten = true;
                self.shared.mark_forgotten();
                self.host.remove_if_same(&self.shared);
                self.terminate(now);
            }
        }
        if self
            .pending_since
            .is_some_and(|since| now >= since + COALESCE_DELAY)
        {
            self.flush(now);
        }
        for sink in &mut self.sinks {
            if sink.has_backlog() {
                sink.pump(now);
            }
            if sink.stalled_for(now).is_some_and(|d| d >= STALL_LIMIT) {
                log_warn!(
                    "terminal {}: closing attachment {} (not reading for {:?})",
                    self.shared.id,
                    sink.id,
                    STALL_LIMIT
                );
                sink.close();
            }
        }
        self.reap_sinks();
        if self.running() && self.startup.is_some() && now >= self.startup_due {
            self.send_startup_input();
        }
        if self.settle_due.is_some_and(|due| now >= due) {
            self.settle_due = None;
            // A program that started quietly is in the foreground by now.
            self.refresh_foreground(now);
            self.publish(|session_id| HostEvent::Output {
                id: self.shared.id,
                session_id,
            });
        }
        let persist_interval = self.host.config.persist_interval;
        if self.running()
            && self.shared.owner.session_id().is_some()
            && self.output_since_persist
            && now.saturating_duration_since(self.last_persist) >= persist_interval
        {
            self.persist(now);
        }
        if self.kill_due.is_some_and(|due| now >= due) {
            self.kill_due = None;
            if self.running() {
                self.signal_groups(libc::SIGKILL);
            }
        }
        match self.phase {
            Phase::Exiting { code, deadline } if now >= deadline => self.finalize(code, now),
            Phase::Exited { at }
                if now.saturating_duration_since(at) >= self.host.config.exited_retention =>
            {
                log_debug!("terminal {}: retention over, dropping", self.shared.id);
                Flow::Stop
            }
            _ => Flow::Continue,
        }
    }

    // ------------------------------------------------------------ output

    fn on_output(&mut self, bytes: &[u8], now: Instant) {
        let out = self.emulator.feed(bytes);
        self.ring.push(bytes);
        if self.pending.is_empty() {
            self.pending_offset = self.total;
            self.pending_since = Some(now);
        }
        self.pending.extend_from_slice(bytes);
        self.total += bytes.len() as u64;
        if !out.replies.is_empty() {
            self.write_pty(out.replies);
        }
        if !out.events.is_empty() {
            self.flush(now);
            for event in out.events {
                self.on_emulator_event(event);
            }
        } else if self.pending.len() >= COALESCE_BYTES {
            self.flush(now);
        }

        let modes = self.emulator.modes();
        {
            let mut state = self.shared.lock();
            state.info.output_offset = self.total;
            state.info.last_output_ms = Some(now_ms());
            state.info.alternate_screen = modes.alternate_screen;
            state.info.bracketed_paste = modes.bracketed_paste;
            state.modes = modes;
        }
        if matches!(self.phase, Phase::Exited { .. }) {
            // Late output after the exit was reported: keep the screen
            // current for reads, but the terminal's events are over.
            return;
        }
        self.output_since_persist = true;
        if self.startup.is_some() && self.running() {
            self.send_startup_input();
        }
        self.settle_due = Some(now + OUTPUT_SETTLE);
        if self
            .last_output_event
            .is_none_or(|at| now.saturating_duration_since(at) >= OUTPUT_EVENT_INTERVAL)
        {
            self.last_output_event = Some(now);
            self.publish(|session_id| HostEvent::Output {
                id: self.shared.id,
                session_id,
            });
        }
        if self
            .last_foreground_refresh
            .is_none_or(|at| now.saturating_duration_since(at) >= FOREGROUND_REFRESH)
        {
            self.refresh_foreground(now);
        }
    }

    fn on_emulator_event(&mut self, event: vt::Event) {
        let id = self.shared.id;
        let now = Instant::now();
        match event {
            vt::Event::Title(title) => {
                self.shared.lock().info.title = title.clone();
                self.broadcast(StreamEvent::Title {
                    title: title.clone(),
                });
                self.publish(|session_id| HostEvent::Title {
                    id,
                    session_id,
                    title,
                });
            }
            vt::Event::Cwd(path) => {
                self.osc7_cwd = true;
                let pid = self
                    .master
                    .process_group_leader()
                    .map(|pid| pid as u32)
                    .unwrap_or(self.pid);
                self.osc7_foreground = Some(pid);
                self.osc7_process_cwd = crate::procinfo::cwd_of(pid);
                self.set_cwd(path);
            }
            vt::Event::Bell => {
                self.broadcast(StreamEvent::Bell);
                self.publish(|session_id| HostEvent::Bell { id, session_id });
            }
            vt::Event::ProgramStatus(status) => {
                // The program that reports may have just started.
                self.refresh_foreground(now);
                self.publish(|session_id| HostEvent::ProgramStatus {
                    id,
                    session_id,
                    status,
                });
            }
            vt::Event::Clipboard { selection, data } => {
                if let Some(controller) = self.controller {
                    if let Some(sink) = self.sinks.iter_mut().find(|s| s.id == controller) {
                        sink.push(
                            AttachEvent::Event(StreamEvent::Clipboard { selection, data }),
                            now,
                        );
                    }
                }
            }
        }
    }

    fn set_cwd(&mut self, path: std::path::PathBuf) {
        {
            let mut state = self.shared.lock();
            if state.info.cwd.as_ref() == Some(&path) {
                return;
            }
            state.info.cwd = Some(path.clone());
        }
        self.broadcast(StreamEvent::Cwd { path: path.clone() });
        self.publish(|session_id| HostEvent::Cwd {
            id: self.shared.id,
            session_id,
            path,
        });
    }

    /// Re-read the foreground process and cwd; `true` when the foreground
    /// process changed.
    fn refresh_foreground(&mut self, now: Instant) -> bool {
        self.last_foreground_refresh = Some(now);
        let leader = self.master.process_group_leader().map(|pid| pid as u32);
        let foreground = leader.and_then(crate::procinfo::process_info);
        let changed = {
            let mut state = self.shared.lock();
            let changed = state.info.foreground != foreground;
            state.info.foreground = foreground;
            changed
        };
        let process = leader.unwrap_or(self.pid);
        let cwd = crate::procinfo::cwd_of(process);
        let process_moved = self
            .osc7_process_cwd
            .as_ref()
            .zip(cwd.as_ref())
            .is_some_and(|(before, after)| before != after);
        if self.osc7_cwd && (self.osc7_foreground != Some(process) || process_moved) {
            self.osc7_cwd = false;
            self.osc7_foreground = None;
            self.osc7_process_cwd = None;
        }
        if !self.osc7_cwd {
            if let Some(cwd) = cwd {
                self.set_cwd(cwd);
            }
        }
        changed
    }

    /// Send coalesced output to every attachment.
    fn flush(&mut self, now: Instant) {
        if self.pending.is_empty() {
            return;
        }
        let event = AttachEvent::Output {
            offset: self.pending_offset,
            bytes: std::mem::take(&mut self.pending),
        };
        self.pending_since = None;
        self.send_all(event, now);
        self.apply_backpressure(now);
    }

    fn broadcast(&mut self, event: StreamEvent) {
        self.flush(Instant::now());
        self.send_all(AttachEvent::Event(event), Instant::now());
    }

    fn send_all(&mut self, event: AttachEvent, now: Instant) {
        if let Some((last, rest)) = self.sinks.split_last_mut() {
            for sink in rest {
                sink.push(event.clone(), now);
            }
            last.push(event, now);
        }
    }

    /// Wait (bounded) for a controller that falls behind; resync observers
    /// whose backlog grew too large.
    fn apply_backpressure(&mut self, now: Instant) {
        let mut resync = Vec::new();
        for (index, sink) in self.sinks.iter_mut().enumerate() {
            if sink.backlog_bytes() <= BACKLOG_LIMIT {
                continue;
            }
            if sink.role == Role::Controller {
                let started = Instant::now();
                while sink.backlog_bytes() > BACKLOG_LIMIT
                    && !sink.is_closed()
                    && started.elapsed() < CONTROLLER_PATIENCE
                {
                    std::thread::sleep(Duration::from_millis(1));
                    sink.pump(Instant::now());
                }
                if sink.backlog_bytes() <= BACKLOG_LIMIT {
                    continue;
                }
            }
            resync.push(index);
        }
        if resync.is_empty() {
            return;
        }
        let (rows, cols) = self.emulator.size();
        let checkpoint =
            base64::engine::general_purpose::STANDARD.encode(self.emulator.checkpoint());
        for index in resync {
            log_debug!(
                "terminal {}: attachment {} lagged, resyncing at {}",
                self.shared.id,
                self.sinks[index].id,
                self.total
            );
            self.sinks[index].resync(
                AttachEvent::Event(StreamEvent::Lagged { offset: self.total }),
                AttachEvent::Event(StreamEvent::Resized {
                    cols,
                    rows,
                    checkpoint: checkpoint.clone(),
                    offset: self.total,
                }),
                now,
            );
        }
    }

    // ------------------------------------------------------------ attachments

    fn attach(
        &mut self,
        role: Role,
        cols: u16,
        rows: u16,
        now: Instant,
    ) -> Result<AttachReply, HostError> {
        if self.forgotten || self.shared.is_forgotten() {
            return Err(HostError::NotFound);
        }
        if !self.running() {
            return Err(HostError::NotRunning);
        }
        // A temporary pane has one owning window stream. Its lease must
        // never be shared with a second stream that could close it early.
        if self.shared.owner.session_id().is_none()
            && self.sinks.iter().any(|sink| !sink.is_closed())
        {
            return Err(HostError::NotController);
        }
        self.flush(now);
        let id = self.next_attachment;
        self.next_attachment += 1;
        let (tx, events) = std::sync::mpsc::sync_channel(ATTACH_QUEUE);
        if role == Role::Controller {
            // Take over first, so the checkpoint is taken at the new size.
            if let Err(e) = self.take_control(id, cols, rows, now) {
                self.set_controlled(None);
                return Err(e);
            }
        }
        let checkpoint = self.emulator.checkpoint();
        self.sinks.push(Sink::new(id, role, tx));
        self.pane_reconnect_due = None;
        Ok(AttachReply {
            attachment: id,
            info: self.shared.lock().info.clone(),
            checkpoint,
            offset: self.total,
            events,
        })
    }

    /// Make `id` the controller (demoting the previous one) and apply its
    /// size. `id` may not have a sink yet (attaching as controller).
    fn take_control(
        &mut self,
        id: AttachmentId,
        cols: u16,
        rows: u16,
        now: Instant,
    ) -> Result<(), HostError> {
        let previous = self.controller.replace(id);
        if let Some(previous) = previous.filter(|p| *p != id) {
            if let Some(sink) = self.sinks.iter_mut().find(|s| s.id == previous) {
                sink.role = Role::Observer;
                sink.push(
                    AttachEvent::Event(StreamEvent::RoleChanged {
                        role: Role::Observer,
                    }),
                    now,
                );
            }
        }
        if let Some(sink) = self.sinks.iter_mut().find(|s| s.id == id) {
            if sink.role != Role::Controller {
                sink.role = Role::Controller;
                sink.push(
                    AttachEvent::Event(StreamEvent::RoleChanged {
                        role: Role::Controller,
                    }),
                    now,
                );
            }
        }
        self.set_controlled(Some(id));
        self.resize_to(cols, rows, now)
    }

    fn demote(&mut self, id: AttachmentId, now: Instant) {
        if self.controller != Some(id) {
            return;
        }
        if let Some(sink) = self.sinks.iter_mut().find(|s| s.id == id) {
            sink.role = Role::Observer;
            sink.push(
                AttachEvent::Event(StreamEvent::RoleChanged {
                    role: Role::Observer,
                }),
                now,
            );
        }
        self.set_controlled(None);
    }

    fn detach(&mut self, id: AttachmentId) {
        self.sinks.retain(|s| s.id != id);
        if self.controller == Some(id) {
            self.set_controlled(None);
        }
    }

    /// Drop attachments whose receiver is gone.
    fn reap_sinks(&mut self) {
        let closed: Vec<AttachmentId> = self
            .sinks
            .iter()
            .filter(|s| s.is_closed())
            .map(|s| s.id)
            .collect();
        for id in closed {
            self.detach(id);
        }
    }

    /// The controller sent input within [`CONTROLLER_TYPING`].
    fn controller_typing(&self) -> bool {
        self.controller.is_some()
            && self
                .shared
                .lock()
                .controller_input_at
                .is_some_and(|at| at.elapsed() < CONTROLLER_TYPING)
    }

    /// Record the controller here and in the shared mirror; publish
    /// `ControllerChanged` when "has a controller" flips.
    fn set_controlled(&mut self, controller: Option<AttachmentId>) {
        self.controller = controller;
        let now_controlled = controller.is_some();
        let was = {
            let mut state = self.shared.lock();
            let was = state.controller.is_some();
            if state.controller != controller {
                state.controller_input_at = None;
            }
            state.controller = controller;
            state.info.controlled = now_controlled;
            was
        };
        if was != now_controlled && self.running() {
            self.publish(|session_id| HostEvent::ControllerChanged {
                id: self.shared.id,
                session_id,
                controlled: now_controlled,
            });
        }
    }

    /// Resize the PTY and the emulator together and tell every attachment.
    fn resize_to(&mut self, cols: u16, rows: u16, now: Instant) -> Result<(), HostError> {
        let (cols, rows) = clamp_size(cols, rows);
        if self.emulator.size() == (rows, cols) {
            return Ok(());
        }
        self.master
            .resize(cols, rows)
            .map_err(|e| HostError::Io(format!("resize failed: {e}")))?;
        self.emulator.resize(rows, cols);
        {
            let mut state = self.shared.lock();
            state.info.cols = cols;
            state.info.rows = rows;
        }
        let checkpoint =
            base64::engine::general_purpose::STANDARD.encode(self.emulator.checkpoint());
        self.send_all(
            AttachEvent::Event(StreamEvent::Resized {
                cols,
                rows,
                checkpoint,
                offset: self.total,
            }),
            now,
        );
        Ok(())
    }

    // ------------------------------------------------------------ process

    fn write_pty(&mut self, bytes: Vec<u8>) {
        let Some(writer) = &self.writer else {
            return;
        };
        match writer.try_send(PtyInput::from(bytes)) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => log_warn!(
                "terminal {}: input queue full, dropped a terminal query reply",
                self.shared.id
            ),
            Err(TrySendError::Disconnected(_)) => {}
        }
    }

    fn send_startup_input(&mut self) {
        let (Some(input), Some(writer)) = (self.startup.take(), &self.writer) else {
            return;
        };
        if let Err(TrySendError::Full(input)) = writer.try_send(PtyInput::from(input)) {
            // Retry on the next tick.
            self.startup = Some(input.bytes);
        }
    }

    /// SIGHUP the process groups now, SIGKILL after the grace period.
    fn terminate(&mut self, now: Instant) {
        self.signal_groups(libc::SIGHUP);
        self.kill_due = Some(now + KILL_GRACE);
    }

    /// Signal the child's process group and the PTY's foreground group.
    fn signal_groups(&mut self, signal: i32) {
        let pid = self.pid as i32;
        if !self.kill_groups.contains(&pid) {
            self.kill_groups.push(pid);
        }
        if let Some(leader) = self.master.process_group_leader() {
            if !self.kill_groups.contains(&leader) {
                self.kill_groups.push(leader);
            }
        }
        for group in &self.kill_groups {
            // SAFETY: kill has no memory-safety preconditions.
            unsafe {
                libc::kill(-group, signal);
            }
        }
    }

    /// The child is gone and the PTY drained: end every stream, persist,
    /// report the exit.
    fn finalize(&mut self, code: Option<i32>, now: Instant) -> Flow {
        self.flush(now);
        self.phase = Phase::Exited { at: now };
        self.kill_due = None;
        self.settle_due = None;
        self.startup = None;
        {
            let mut state = self.shared.lock();
            state.info.state = TerminalState::Exited { code };
            state.info.controlled = false;
            state.controller = None;
        }
        for sink in &mut self.sinks {
            sink.push(AttachEvent::Event(StreamEvent::Exited { code }), now);
            sink.push(AttachEvent::Closed(None), now);
            sink.pump(now);
        }
        self.sinks.clear();
        self.controller = None;
        self.writer = None;
        self.shared.close_input();
        self.reader_stop.store(true, Ordering::Relaxed);
        if self.forgotten || self.shared.is_forgotten() || self.shared.owner.session_id().is_none()
        {
            return Flow::Stop;
        }
        self.persist(now);
        self.publish(|session_id| HostEvent::Exited {
            id: self.shared.id,
            session_id,
            code,
        });
        Flow::Continue
    }

    /// Save scrollback and metadata, unless the terminal was forgotten. The
    /// guard is held through the write so `forget` can't interleave.
    fn persist(&mut self, now: Instant) {
        let shared = Arc::clone(&self.shared);
        let Some(session_id) = shared.owner.session_id() else {
            return;
        };
        let forgotten = shared.persist_guard();
        if *forgotten {
            return;
        }
        let scrollback = formatted_scrollback(&mut self.emulator);
        let meta = {
            let state = self.shared.lock();
            SessionMeta {
                cwd: state.info.cwd.clone(),
                title: state.info.title.clone(),
                foreground_process: state.info.foreground.as_ref().map(|p| p.name.clone()),
                exit_code: match state.info.state {
                    TerminalState::Exited { code } => code,
                    _ => None,
                },
                updated_at: now_ms() / 1000,
            }
        };
        if let Err(e) = self.host.store.save(session_id, &scrollback, &meta) {
            log_warn!(
                "terminal {}: saving scrollback for session {} failed: {e}",
                self.shared.id,
                session_id
            );
        }
        self.host.saves.fetch_add(1, Ordering::SeqCst);
        drop(forgotten);
        self.output_since_persist = false;
        self.last_persist = now;
    }

    fn publish(&self, event: impl FnOnce(session_core::ids::SessionId) -> HostEvent) {
        if !self.forgotten && !self.shared.is_forgotten() {
            if let Some(id) = self.shared.owner.session_id() {
                self.host.publish(event(id.clone()));
            }
        }
    }

    fn teardown(&mut self) {
        self.reader_stop.store(true, Ordering::Relaxed);
        self.writer = None;
        self.shared.close_input();
        self.sinks.clear();
        self.host.remove_if_same(&self.shared);
    }
}
