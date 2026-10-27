//! Terminal management: one actor per PTY (reader thread, writer thread,
//! actor thread owning a responding `vt::Emulator`), a bounded output replay
//! ring, controller/observer fan-out, periodic scrollback persistence.
//!
//! Threads per terminal:
//! - **reader**: blocking PTY reads → bounded actor queue (backpressure on
//!   the program when the actor falls behind);
//! - **actor**: owns the emulator, ring, PTY master and attachments; handles
//!   output, commands and exit in order; coalesces output (8 KiB / 1 ms)
//!   before fan-out;
//! - **writer**: every byte written to the PTY (query replies, startup
//!   input, client input) goes through one queue, so writes stay ordered;
//! - **waiter**: waits for the child and reports its exit code.
//!
//! `info`/`list` and input (`write`, `submit`, `send_keys`, attachment
//! input) never wait for the actor: they use a shared snapshot of the
//! terminal's info, input modes and controller.

mod actor;
mod keys;
mod pty_io;
mod read;
mod ring;
mod sink;

use crate::handoff::{PaneHandoff, ProcessHandoff, TerminalHandoff};
use crate::host::{
    AttachControl, HostAttachment, HostError, HostEvent, PaneAttachment, PaneSpawnSpec, SpawnSpec,
    TerminalHost, TerminalInfo, TerminalProcessInfo, TerminalProgram, TerminalState,
};
use crate::persist::ScrollbackStore;
use crate::{log_debug, log_error, log_info, log_warn};
use actor::{Actor, ActorMsg, ActorParts, Adopted, AttachReply};
use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use pty_io::{PtyInput, PtyMaster};
use session_core::api::{ReadResult, ReadSource, Role, TextFormat};
use session_core::ids::{PaneId, SessionId, TerminalId};
use session_core::paths::Layout;
use sink::AttachmentId;
use std::collections::HashMap;
use std::os::unix::io::{FromRawFd, OwnedFd};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, SyncSender, TrySendError};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

/// Messages queued for an actor (output chunks and commands).
const ACTOR_QUEUE: usize = 64;
/// Writes queued for a PTY.
const WRITER_QUEUE: usize = 256;
/// How long input waits for a program that doesn't read its input.
const INPUT_PATIENCE: Duration = Duration::from_secs(5);
/// A submitted text's Enter is written this long after its paste, in a write
/// of its own. Programs that take a paste in steps hold back an Enter that
/// comes with it, and can drop it: Claude Code drops one that arrives while it
/// checks whether a paste ending in `.png`, `.jpg`, `.gif` or `.webp` names an
/// image (asking the clipboard, through `osascript` on macOS), so such a reply
/// stayed in its prompt unsent and the next one was added to it.
const SUBMIT_ENTER_AFTER: Duration = Duration::from_millis(500);
/// Smallest size applied to a PTY and its emulator (vt100 panics below 2×2).
pub const MIN_COLS: u16 = 2;
pub const MIN_ROWS: u16 = 2;
/// How long `shutdown` waits for terminals to exit after SIGHUP/SIGKILL.
const SHUTDOWN_WAIT: Duration = Duration::from_secs(5);

#[derive(Debug, Clone)]
pub struct HostConfig {
    pub layout: Layout,
    /// Colours reported to programs that ask (OSC 10/11).
    pub colors: vt::Colors,
    /// Emulator scrollback kept in memory.
    pub scrollback_rows: usize,
    /// Raw output replay ring per terminal.
    pub replay_bytes: usize,
    /// How long an exited terminal stays in memory before `forget`.
    pub exited_retention: Duration,
    /// How often running terminals persist scrollback.
    pub persist_interval: Duration,
    /// umask for spawned programs. `None` inherits the daemon's; the daemon
    /// runs with 077 and sets this to the umask it started with.
    pub child_umask: Option<u32>,
    /// Grace for a pane's owning window to reconnect after handoff. `None`
    /// uses 30 seconds; unclaimed panes then end instead of becoming orphans.
    pub pane_reconnect_grace: Option<Duration>,
}

impl HostConfig {
    pub fn new(layout: Layout) -> Self {
        Self {
            layout,
            colors: vt::Colors::default(),
            scrollback_rows: 2000,
            replay_bytes: 256 * 1024,
            exited_retention: Duration::from_secs(600),
            persist_interval: Duration::from_secs(30),
            child_umask: None,
            pane_reconnect_grace: None,
        }
    }
}

/// `(cols, rows)` raised to the supported minimum.
pub(crate) fn clamp_size(cols: u16, rows: u16) -> (u16, u16) {
    (cols.max(MIN_COLS), rows.max(MIN_ROWS))
}

pub(crate) fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// State shared by the host, the actor and attachments.
pub(crate) struct SharedState {
    pub(crate) info: TerminalProcessInfo,
    pub(crate) modes: vt::Modes,
    pub(crate) controller: Option<AttachmentId>,
    /// When the controller last sent input (someone typing in it).
    pub(crate) controller_input_at: Option<Instant>,
}

/// One terminal as seen from outside its actor.
#[derive(Clone)]
enum TerminalOwner {
    Session(SessionId),
    Pane(PaneId),
}

impl TerminalOwner {
    fn session_id(&self) -> Option<&SessionId> {
        match self {
            Self::Session(id) => Some(id),
            Self::Pane(_) => None,
        }
    }
}

pub(crate) struct Shared {
    pub(crate) id: TerminalId,
    owner: TerminalOwner,
    commands: SyncSender<ActorMsg>,
    state: Mutex<SharedState>,
    /// PTY input queue; `None` once the terminal ended.
    input: Mutex<Option<SyncSender<PtyInput>>>,
    /// Set by `forget`. Every scrollback save holds this lock while it
    /// checks the flag and writes, so once `forget` returns no save is in
    /// progress and none will start: the control layer can delete the
    /// session's files without them being recreated.
    forgotten: Mutex<bool>,
}

impl Shared {
    pub(crate) fn lock(&self) -> MutexGuard<'_, SharedState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Hold this while saving scrollback; skip the save when it is `true`.
    pub(crate) fn persist_guard(&self) -> MutexGuard<'_, bool> {
        self.forgotten.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub(crate) fn is_forgotten(&self) -> bool {
        *self.persist_guard()
    }

    /// Mark forgotten, waiting for a save in progress to finish.
    fn mark_forgotten(&self) {
        *self.persist_guard() = true;
    }

    pub(crate) fn close_input(&self) {
        self.input.lock().unwrap_or_else(|e| e.into_inner()).take();
    }

    fn is_running(&self) -> bool {
        self.lock().info.state == TerminalState::Running
    }

    /// Queue bytes for the PTY, waiting (bounded) while the program isn't
    /// reading its input.
    fn send_input(&self, bytes: Vec<u8>) -> Result<(), HostError> {
        if bytes.is_empty() {
            return Ok(());
        }
        self.queue_input(PtyInput::from(bytes))
    }

    fn queue_input(&self, input: PtyInput) -> Result<(), HostError> {
        let sender = self
            .input
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
            .ok_or(HostError::NotRunning)?;
        let deadline = Instant::now() + INPUT_PATIENCE;
        let mut input = input;
        loop {
            match sender.try_send(input) {
                Ok(()) => return Ok(()),
                Err(TrySendError::Disconnected(_)) => return Err(HostError::NotRunning),
                Err(TrySendError::Full(back)) => {
                    if Instant::now() >= deadline {
                        return Err(HostError::Io("the program is not reading its input".into()));
                    }
                    input = back;
                    std::thread::sleep(Duration::from_millis(2));
                }
            }
        }
    }

    /// Send a command to the actor; `NotFound` when the actor is gone.
    fn command(&self, msg: ActorMsg) -> Result<(), HostError> {
        self.commands.send(msg).map_err(|_| HostError::NotFound)
    }

    /// Send a command and wait for its reply.
    fn request<T>(&self, make: impl FnOnce(Sender<T>) -> ActorMsg) -> Result<T, HostError> {
        let (tx, rx) = mpsc::channel();
        self.command(make(tx))?;
        rx.recv().map_err(|_| HostError::NotRunning)
    }
}

pub(crate) struct HostInner {
    pub(crate) config: HostConfig,
    pub(crate) store: ScrollbackStore,
    terminals: Mutex<HashMap<TerminalId, Arc<Shared>>>,
    subscribers: Mutex<Vec<Sender<HostEvent>>>,
    next_id: AtomicU64,
    shutting_down: AtomicBool,
    pane_handoff: Mutex<Vec<PaneHandoff>>,
    /// Scrollback saves performed (diagnostics and tests).
    pub(crate) saves: AtomicU64,
}

impl HostInner {
    pub(crate) fn publish(&self, event: HostEvent) {
        let mut subscribers = self.subscribers.lock().unwrap_or_else(|e| e.into_inner());
        subscribers.retain(|tx| tx.send(event.clone()).is_ok());
    }

    fn terminals(&self) -> MutexGuard<'_, HashMap<TerminalId, Arc<Shared>>> {
        self.terminals.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn get(&self, id: TerminalId) -> Result<Arc<Shared>, HostError> {
        self.terminals()
            .get(&id)
            .cloned()
            .ok_or(HostError::NotFound)
    }

    fn pane(&self, id: &PaneId) -> Result<Arc<Shared>, HostError> {
        self.terminals()
            .values()
            .find(|shared| matches!(&shared.owner, TerminalOwner::Pane(pane) if pane == id))
            .cloned()
            .ok_or(HostError::NotFound)
    }

    /// Remove `shared` from the table unless it was replaced or removed.
    pub(crate) fn remove_if_same(&self, shared: &Arc<Shared>) {
        let mut terminals = self.terminals();
        if terminals
            .get(&shared.id)
            .is_some_and(|current| Arc::ptr_eq(current, shared))
        {
            terminals.remove(&shared.id);
        }
    }
}

pub struct Host {
    inner: Arc<HostInner>,
}

impl Host {
    pub fn new(config: HostConfig) -> Self {
        let store = ScrollbackStore::new(config.layout.clone());
        Self {
            inner: Arc::new(HostInner {
                config,
                store,
                terminals: Mutex::new(HashMap::new()),
                subscribers: Mutex::new(Vec::new()),
                next_id: AtomicU64::new(1),
                shutting_down: AtomicBool::new(false),
                pane_handoff: Mutex::new(Vec::new()),
                saves: AtomicU64::new(0),
            }),
        }
    }

    /// Scrollback saves performed so far by all terminals.
    pub fn scrollback_saves(&self) -> u64 {
        self.inner.saves.load(Ordering::SeqCst)
    }

    pub fn config(&self) -> &HostConfig {
        &self.inner.config
    }

    /// Raw output of terminal `id` from absolute `offset` to now, from the
    /// replay ring (`HostConfig::replay_bytes`). `Ok(None)` when that output
    /// is no longer retained (or `offset` is in the future): the caller needs
    /// a fresh checkpoint instead. For clients resuming after a short gap.
    pub fn replay_since(&self, id: TerminalId, offset: u64) -> Result<Option<Vec<u8>>, HostError> {
        self.inner
            .get(id)?
            .request(|reply| ActorMsg::ReplaySince { offset, reply })
    }

    fn validate(spec: &TerminalProgram) -> Result<(), HostError> {
        if spec.argv.is_empty() || spec.argv[0].is_empty() {
            return Err(HostError::Spawn("empty command".into()));
        }
        if !spec.cwd.is_dir() {
            return Err(HostError::Spawn(format!(
                "working directory {} does not exist or is not a directory",
                spec.cwd.display()
            )));
        }
        Ok(())
    }

    /// Start the actor, reader, writer and waiter threads of a terminal
    /// and add it to the table.
    fn start(
        &self,
        master: PtyMaster,
        info: TerminalProcessInfo,
        owner: TerminalOwner,
        modes: vt::Modes,
        startup_input: Vec<u8>,
        adopted: Option<Adopted>,
    ) -> Result<(), HostError> {
        let id = info.id;
        let pid = info
            .pid
            .ok_or_else(|| HostError::Spawn("terminal without a process".into()))?;
        let (reader_fd, writer_fd) = pty_io::dup_fd(master.raw_fd())
            .and_then(|reader| Ok((reader, pty_io::dup_fd(master.raw_fd())?)))
            .map_err(|e| HostError::Spawn(format!("cannot duplicate the pty: {e}")))?;
        let (command_tx, command_rx) = mpsc::sync_channel(ACTOR_QUEUE);
        let (input_tx, input_rx) = mpsc::sync_channel(WRITER_QUEUE);
        let shared = Arc::new(Shared {
            id,
            owner,
            commands: command_tx.clone(),
            state: Mutex::new(SharedState {
                modes,
                controller: None,
                controller_input_at: None,
                info: info.clone(),
            }),
            input: Mutex::new(Some(input_tx.clone())),
            forgotten: Mutex::new(false),
        });
        let reader_stop = Arc::new(AtomicBool::new(false));
        let mut actor = Actor::new(ActorParts {
            shared: shared.clone(),
            host: self.inner.clone(),
            master,
            rx: command_rx,
            writer: input_tx,
            reader_stop: reader_stop.clone(),
            pid,
            cols: info.cols,
            rows: info.rows,
            startup_input,
            adopted,
        });

        let started = (|| {
            spawn_thread(format!("t{id}-writer"), move || {
                pty_io::run_writer(writer_fd, input_rx)
            })?;
            let actor_shared = shared.clone();
            let actor_host = self.inner.clone();
            spawn_thread(format!("t{id}-actor"), move || {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| actor.run()));
                if let Err(panic) = result {
                    on_actor_panic(&actor_host, &actor_shared, pid, panic);
                }
            })?;
            let reader_tx = command_tx.clone();
            spawn_thread(format!("t{id}-reader"), move || {
                pty_io::run_reader(reader_fd, reader_tx, reader_stop)
            })?;
            spawn_thread(format!("t{id}-waiter"), move || {
                pty_io::run_waiter(pid, command_tx)
            })
        })();
        if let Err(e) = started {
            let _ = shared.command(ActorMsg::Stop);
            return Err(e);
        }
        self.inner.terminals().insert(id, shared);
        Ok(())
    }

    /// Take over the terminals a previous daemon image handed over
    /// (`handoff`): their programs keep running, their screens and history
    /// come back. Returns how many were adopted; a terminal that can't be
    /// is closed (its program gets SIGHUP).
    pub fn adopt(&self, terminals: Vec<TerminalHandoff>) -> usize {
        self.adopt_processes(terminals.into_iter().map(|terminal| {
            (
                TerminalOwner::Session(terminal.session_id.clone()),
                terminal.into(),
            )
        }))
    }

    /// Restore standalone panes without ever adopting them as sessions.
    pub fn adopt_panes(&self, panes: Vec<PaneHandoff>) -> usize {
        self.adopt_processes(
            panes
                .into_iter()
                .map(|pane| (TerminalOwner::Pane(pane.id), pane.terminal)),
        )
    }

    fn adopt_processes(
        &self,
        terminals: impl IntoIterator<Item = (TerminalOwner, ProcessHandoff)>,
    ) -> usize {
        let mut adopted = 0;
        for (owner, handed) in terminals {
            let id = TerminalId(handed.id);
            if let Err(e) = crate::handoff::set_cloexec(handed.fd, true) {
                log_warn!("terminal {id}: handed-over pty unusable: {e}");
                continue;
            }
            // SAFETY: the previous image passed this descriptor; we own it.
            let master = PtyMaster::new(unsafe { OwnedFd::from_raw_fd(handed.fd) });
            let (cols, rows) = clamp_size(handed.cols, handed.rows);
            let emulator = restore_emulator(&self.inner.config, rows, cols, &handed);
            let modes = emulator.modes();
            let info = TerminalProcessInfo {
                id,
                pid: Some(handed.pid),
                cwd: handed.cwd.clone(),
                title: handed.title.clone(),
                foreground: None,
                state: TerminalState::Running,
                cols,
                rows,
                alternate_screen: modes.alternate_screen,
                bracketed_paste: modes.bracketed_paste,
                controlled: false,
                output_offset: handed.output_offset,
                last_output_ms: handed.last_output_ms,
                started_ms: handed.started_ms,
            };
            let startup_input = handed
                .startup_input
                .as_deref()
                .map(crate::handoff::decode)
                .unwrap_or_default();
            let parts = Adopted {
                emulator,
                total: handed.output_offset,
                osc7_cwd: handed.osc7_cwd,
                osc7_foreground: handed.osc7_foreground,
                osc7_process_cwd: handed.osc7_process_cwd,
            };
            match self.start(master, info, owner, modes, startup_input, Some(parts)) {
                Ok(()) => {
                    adopted += 1;
                    self.inner
                        .next_id
                        .fetch_max(handed.id + 1, Ordering::SeqCst);
                    log_info!("terminal {id}: adopted (pid {})", handed.pid,);
                }
                Err(e) => log_warn!("terminal {id}: can't adopt it: {e}"),
            }
        }
        adopted
    }
}

/// An emulator showing a handed-over terminal: its history rows scrolled
/// into the scrollback, then its screen.
fn restore_emulator(
    config: &HostConfig,
    rows: u16,
    cols: u16,
    handed: &ProcessHandoff,
) -> vt::Emulator {
    let mut emulator = vt::Emulator::responding(rows, cols, config.scrollback_rows, config.colors);
    if !handed.history.is_empty() {
        let mut bytes = Vec::new();
        for row in &handed.history {
            bytes.extend_from_slice(&crate::handoff::decode(row));
            bytes.extend_from_slice(b"\r\n");
        }
        // Scroll the last history rows off the screen too.
        bytes.extend(std::iter::repeat_n(b'\n', rows.saturating_sub(1) as usize));
        bytes.extend_from_slice(b"\x1b[0m\x1b[H\x1b[2J");
        emulator.feed(&bytes);
    }
    emulator.feed(&crate::handoff::decode(&handed.screen));
    emulator
}

/// Run `f` on a named thread.
fn spawn_thread(name: String, f: impl FnOnce() + Send + 'static) -> Result<(), HostError> {
    std::thread::Builder::new()
        .name(name)
        .spawn(f)
        .map(|_| ())
        .map_err(|e| HostError::Spawn(format!("cannot start a thread: {e}")))
}

fn kill_group(pid: u32) {
    // SAFETY: kill has no memory-safety preconditions.
    unsafe {
        libc::kill(-(pid as i32), libc::SIGKILL);
    }
}

impl Host {
    /// Paste `text`, then Enter in a write of its own once the program took
    /// the paste in; with `same_foreground`, the Enter only while the process
    /// group that had the foreground at the paste still has it.
    fn queue_submit(
        &self,
        id: TerminalId,
        text: &str,
        same_foreground: bool,
    ) -> Result<(), HostError> {
        let shared = self.inner.get(id)?;
        if !shared.is_running() {
            return Err(HostError::NotRunning);
        }
        let modes = shared.lock().modes;
        // The text must not be able to open or close a paste bracket itself.
        let text = text.replace("\x1b[200~", "").replace("\x1b[201~", "");
        shared.queue_input(PtyInput {
            bytes: vt::input::encode_paste(&text, &modes),
            then: Some((SUBMIT_ENTER_AFTER, b"\r".to_vec())),
            same_foreground,
        })
    }

    /// [`HostError::Typing`] while the controller sent input within `quiet`.
    fn refuse_typing(&self, id: TerminalId, quiet: Duration) -> Result<(), HostError> {
        let typing = self
            .inner
            .get(id)?
            .lock()
            .controller_input_at
            .is_some_and(|at| at.elapsed() < quiet);
        if typing {
            return Err(HostError::Typing);
        }
        Ok(())
    }

    fn spawn_process(
        &self,
        spec: TerminalProgram,
        owner: TerminalOwner,
    ) -> Result<TerminalProcessInfo, HostError> {
        if self.inner.shutting_down.load(Ordering::SeqCst) {
            return Err(HostError::Spawn("the daemon is shutting down".into()));
        }
        Self::validate(&spec)?;
        let (cols, rows) = clamp_size(spec.cols, spec.rows);
        let pair = native_pty_system()
            .openpty(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| HostError::Spawn(format!("cannot open a pty: {e}")))?;
        let mut cmd = CommandBuilder::from_argv(spec.argv.iter().map(Into::into).collect());
        cmd.env_clear();
        for (key, value) in &spec.env {
            cmd.env(key, value);
        }
        cmd.cwd(&spec.cwd);
        if let Some(mask) = self.inner.config.child_umask {
            cmd.umask(Some(mask as libc::mode_t));
        }
        let child = pair
            .slave
            .spawn_command(cmd)
            .map_err(|e| HostError::Spawn(format!("{}: {e}", spec.argv[0])))?;
        drop(pair.slave);
        // The waiter waits for the pid itself (see `pty_io::run_waiter`).
        let pid = child.process_id();
        drop(child);
        let Some(pid) = pid else {
            return Err(HostError::Spawn(format!("{}: no process id", spec.argv[0])));
        };
        // Our own handle on the master; portable-pty's closes with `pair`.
        let master = pair
            .master
            .as_raw_fd()
            .ok_or_else(|| std::io::Error::other("pty master has no descriptor"))
            .and_then(pty_io::dup_fd)
            .map(|file| PtyMaster::new(file.into()));
        drop(pair.master);
        let master = match master {
            Ok(master) => master,
            Err(e) => {
                kill_group(pid);
                return Err(HostError::Spawn(format!("cannot duplicate the pty: {e}")));
            }
        };
        let id = TerminalId(self.inner.next_id.fetch_add(1, Ordering::SeqCst));
        let info = TerminalProcessInfo {
            id,
            pid: Some(pid),
            cwd: Some(spec.cwd.clone()),
            title: String::new(),
            foreground: None,
            state: TerminalState::Running,
            cols,
            rows,
            alternate_screen: false,
            bracketed_paste: false,
            controlled: false,
            output_offset: 0,
            last_output_ms: None,
            started_ms: now_ms(),
        };
        let modes = vt::Emulator::passive(rows, cols, 0).modes();
        self.start(
            master,
            info.clone(),
            owner,
            modes,
            spec.startup_input.clone(),
            None,
        )
        .inspect_err(|_| kill_group(pid))?;
        log_info!(
            "terminal {id}: spawned {:?} (pid {pid}) in {}",
            spec.argv,
            spec.cwd.display()
        );
        Ok(info)
    }
}

impl TerminalHost for Host {
    fn spawn(&self, spec: SpawnSpec) -> Result<TerminalInfo, HostError> {
        let id = spec.session_id.clone();
        self.spawn_process(spec.into(), TerminalOwner::Session(id.clone()))
            .map(|info| info.for_session(id))
    }

    fn spawn_pane(&self, spec: PaneSpawnSpec) -> Result<TerminalProcessInfo, HostError> {
        self.spawn_process(spec.program, TerminalOwner::Pane(spec.id))
    }

    fn pane_info(&self, id: &PaneId) -> Option<TerminalProcessInfo> {
        Some(self.inner.pane(id).ok()?.lock().info.clone())
    }

    fn attach_pane(
        &self,
        id: &PaneId,
        role: Role,
        cols: u16,
        rows: u16,
    ) -> Result<PaneAttachment, HostError> {
        let shared = self.inner.pane(id)?;
        let reply = shared.request(|reply| ActorMsg::Attach {
            role,
            cols,
            rows,
            reply,
        })??;
        Ok(HostAttachment {
            info: reply.info,
            role,
            checkpoint: reply.checkpoint,
            offset: reply.offset,
            events: reply.events,
            control: Box::new(Attachment {
                shared,
                id: reply.attachment,
                detached: AtomicBool::new(false),
            }),
        })
    }

    fn close_pane(&self, id: &PaneId) {
        if let Ok(shared) = self.inner.pane(id) {
            self.forget(shared.id);
        }
    }

    fn pane_count(&self) -> usize {
        self.inner
            .terminals()
            .values()
            .filter(|shared| matches!(shared.owner, TerminalOwner::Pane(_)))
            .count()
    }

    fn list(&self) -> Vec<TerminalInfo> {
        let terminals: Vec<Arc<Shared>> = self.inner.terminals().values().cloned().collect();
        let mut infos: Vec<TerminalInfo> = terminals
            .iter()
            .filter_map(|t| {
                t.owner
                    .session_id()
                    .map(|id| t.lock().info.clone().for_session(id.clone()))
            })
            .collect();
        infos.sort_by_key(|info| info.id);
        infos
    }

    fn info(&self, id: TerminalId) -> Option<TerminalInfo> {
        let shared = self.inner.get(id).ok()?;
        let info = shared
            .lock()
            .info
            .clone()
            .for_session(shared.owner.session_id()?.clone());
        Some(info)
    }

    fn read(
        &self,
        id: TerminalId,
        source: ReadSource,
        lines: Option<u32>,
        format: TextFormat,
    ) -> Result<ReadResult, HostError> {
        self.inner.get(id)?.request(|reply| ActorMsg::Read {
            source,
            lines,
            format,
            reply,
        })
    }

    fn write(&self, id: TerminalId, bytes: &[u8]) -> Result<(), HostError> {
        let shared = self.inner.get(id)?;
        if !shared.is_running() {
            return Err(HostError::NotRunning);
        }
        shared.send_input(bytes.to_vec())
    }

    fn submit(&self, id: TerminalId, text: &str) -> Result<(), HostError> {
        self.queue_submit(id, text, false)
    }

    fn submit_when_quiet(
        &self,
        id: TerminalId,
        text: &str,
        quiet: Duration,
    ) -> Result<(), HostError> {
        self.refuse_typing(id, quiet)?;
        self.submit(id, text)
    }

    fn submit_expecting(
        &self,
        id: TerminalId,
        text: &str,
        quiet: Option<Duration>,
        foreground: Option<&str>,
    ) -> Result<(), HostError> {
        if let Some(expected) = foreground {
            // Read now, not the last poll's: the agent may have just exited.
            let now = self
                .inner
                .get(id)?
                .request(|reply| ActorMsg::Foreground { reply })?;
            if now.as_deref() != Some(expected) {
                return Err(HostError::ForegroundChanged);
            }
        }
        if let Some(quiet) = quiet {
            self.refuse_typing(id, quiet)?;
        }
        // Expecting a program, its Enter goes only to it.
        self.queue_submit(id, text, foreground.is_some())
    }

    fn send_keys(
        &self,
        id: TerminalId,
        keys: &[String],
        text: Option<&str>,
    ) -> Result<(), HostError> {
        let shared = self.inner.get(id)?;
        let modes = shared.lock().modes;
        let bytes = keys::encode_keys(keys, text, &modes)?;
        if !shared.is_running() {
            return Err(HostError::NotRunning);
        }
        shared.send_input(bytes)
    }

    fn terminate(&self, id: TerminalId) -> Result<(), HostError> {
        let shared = self.inner.get(id)?;
        shared.request(|reply| ActorMsg::Terminate { reply })?
    }

    /// Drop the terminal: no events and no scrollback saves from it once
    /// this returns (a save in progress is waited for). A terminal that is
    /// still running is terminated.
    fn forget(&self, id: TerminalId) {
        let removed = self.inner.terminals().remove(&id);
        if let Some(shared) = removed {
            shared.mark_forgotten();
            log_debug!("terminal {id}: forgotten");
            let _ = shared.command(ActorMsg::Forget);
        }
    }

    fn attach(
        &self,
        id: TerminalId,
        role: Role,
        cols: u16,
        rows: u16,
    ) -> Result<HostAttachment, HostError> {
        let shared = self.inner.get(id)?;
        let session_id = shared
            .owner
            .session_id()
            .ok_or(HostError::NotFound)?
            .clone();
        let reply: AttachReply = shared.request(|reply| ActorMsg::Attach {
            role,
            cols,
            rows,
            reply,
        })??;
        Ok(HostAttachment {
            info: reply.info.for_session(session_id),
            role,
            checkpoint: reply.checkpoint,
            offset: reply.offset,
            events: reply.events,
            control: Box::new(Attachment {
                shared,
                id: reply.attachment,
                detached: AtomicBool::new(false),
            }),
        })
    }

    fn subscribe(&self) -> Receiver<HostEvent> {
        let (tx, rx) = mpsc::channel();
        self.inner
            .subscribers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(tx);
        rx
    }

    fn shutdown(&self) {
        self.inner.shutting_down.store(true, Ordering::SeqCst);
        let terminals: Vec<Arc<Shared>> = self.inner.terminals().values().cloned().collect();
        log_info!("shutting down {} terminal(s)", terminals.len());
        for shared in terminals.iter().filter(|t| t.is_running()) {
            let _ = shared.request(|reply| ActorMsg::Terminate { reply });
        }
        let deadline = Instant::now() + SHUTDOWN_WAIT;
        while Instant::now() < deadline && terminals.iter().any(|t| t.is_running()) {
            std::thread::sleep(Duration::from_millis(20));
        }
        for shared in &terminals {
            if shared.is_running() {
                log_warn!("terminal {}: still running at shutdown", shared.id);
                let _ = shared.request(|reply| ActorMsg::Persist { reply });
            }
            let _ = shared.command(ActorMsg::Stop);
        }
        self.inner.terminals().clear();
    }

    fn handoff(&self) -> Vec<TerminalHandoff> {
        self.inner.shutting_down.store(true, Ordering::SeqCst);
        let terminals: Vec<Arc<Shared>> = self.inner.terminals().values().cloned().collect();
        let mut handed = Vec::new();
        for shared in &terminals {
            match shared.request(|reply| ActorMsg::Handoff { reply }) {
                Ok(Some(terminal)) => match &shared.owner {
                    TerminalOwner::Session(id) => handed.push(terminal.for_session(id.clone())),
                    TerminalOwner::Pane(id) => self
                        .inner
                        .pane_handoff
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .push(PaneHandoff {
                            id: id.clone(),
                            terminal,
                        }),
                },
                _ => {
                    if shared.is_running() {
                        log_warn!("terminal {}: not handed over; it ends", shared.id);
                        let _ = shared.request(|reply| ActorMsg::Persist { reply });
                    }
                    let _ = shared.command(ActorMsg::Stop);
                }
            }
        }
        self.inner.terminals().clear();
        log_info!("handing over {} running terminal(s)", handed.len());
        handed
    }

    fn take_pane_handoff(&self) -> Vec<PaneHandoff> {
        std::mem::take(
            &mut *self
                .inner
                .pane_handoff
                .lock()
                .unwrap_or_else(|e| e.into_inner()),
        )
    }
}

/// The actor panicked: the terminal is Failed, its process is killed and the
/// control layer hears that it exited. The daemon keeps running.
fn on_actor_panic(
    host: &HostInner,
    shared: &Arc<Shared>,
    pid: u32,
    panic: Box<dyn std::any::Any + Send>,
) {
    let message = panic
        .downcast_ref::<&str>()
        .map(|s| s.to_string())
        .or_else(|| panic.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "panic".into());
    log_error!("terminal {}: actor panicked: {message}", shared.id);
    kill_group(pid);
    shared.close_input();
    {
        let mut state = shared.lock();
        state.info.state = TerminalState::Failed {
            message: format!("terminal failed: {message}"),
        };
        state.info.controlled = false;
        state.controller = None;
    }
    if let Some(session_id) = shared.owner.session_id() {
        host.publish(HostEvent::Exited {
            id: shared.id,
            session_id: session_id.clone(),
            code: None,
        });
    } else {
        host.remove_if_same(shared);
    }
}

/// An attached client's handle. Dropping it detaches.
struct Attachment {
    shared: Arc<Shared>,
    id: AttachmentId,
    detached: AtomicBool,
}

impl AttachControl for Attachment {
    fn checkpoint(&self) -> Result<(), HostError> {
        self.shared.request(|reply| ActorMsg::Checkpoint {
            attachment: self.id,
            reply,
        })?
    }
    fn input(&self, bytes: &[u8]) -> Result<(), HostError> {
        {
            let mut state = self.shared.lock();
            if state.info.state != TerminalState::Running {
                return Err(HostError::NotRunning);
            }
            if state.controller != Some(self.id) {
                return Err(HostError::NotController);
            }
            state.controller_input_at = Some(Instant::now());
        }
        self.shared.send_input(bytes.to_vec())
    }

    fn resize(&self, cols: u16, rows: u16) -> Result<(), HostError> {
        self.shared.request(|reply| ActorMsg::Resize {
            attachment: self.id,
            cols,
            rows,
            reply,
        })?
    }

    fn promote(&self, cols: u16, rows: u16) -> Result<(), HostError> {
        self.shared.request(|reply| ActorMsg::Promote {
            attachment: self.id,
            cols,
            rows,
            reply,
        })?
    }

    fn demote(&self) {
        let _ = self.shared.command(ActorMsg::Demote(self.id));
    }

    fn fit(&self, cols: u16, rows: u16) -> Result<(), HostError> {
        self.shared.request(|reply| ActorMsg::Fit {
            attachment: self.id,
            cols,
            rows,
            reply,
        })?
    }

    fn detach(&self) {
        if !self.detached.swap(true, Ordering::SeqCst) {
            let _ = self.shared.command(ActorMsg::Detach(self.id));
        }
    }
}

impl Drop for Attachment {
    fn drop(&mut self) {
        self.detach();
    }
}
