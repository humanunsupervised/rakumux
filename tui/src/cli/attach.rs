//! `rakumux attach`: a full-screen raw client.
//!
//! The user's terminal does the emulation: after the checkpoint, output
//! bytes go straight to stdout and stdin bytes go straight to the session.
//! A passive mirror emulator follows the stream so the screen can be redrawn
//! when the program leaves its alternate screen (we already occupy the real
//! alternate screen, so passing `?1049l` through would reveal the user's
//! shell), after a resize, and after a notice.
//!
//! Prefix `Ctrl-\\` (the TUI's Rakumux key), then: `d` detach, `>`/`<`
//! next/previous running session in scope by recency, `n` new session in the
//! current directory, `Ctrl-\\` a literal `Ctrl-\\`.

use super::{CliError, Invocation};
use base64::Engine as _;
use session_core::api::{
    ApiError, AttachRequest, CreateSession, ErrorCode, ReviveSession, Role, StreamCommand,
    StreamEvent,
};
use session_core::client::{AttachEvent, AttachSender, AttachStream, SessionApi};
use session_core::ids::SessionId;
use session_core::model::{ScopeRoot, SessionStatus, SessionView};
use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

/// `Ctrl-\\` (0x1C), like the TUI's Rakumux key: coding agents and
/// editors leave it alone (`Ctrl-b` is Claude Code's and OpenCode's
/// "background" key).
const PREFIX: u8 = 0x1c;
/// How often the terminal size is checked (stands in for SIGWINCH).
const SIZE_POLL: Duration = Duration::from_millis(100);
/// How long a notice stays on the bottom line.
const NOTICE_TIME: Duration = Duration::from_secs(2);
/// Puts back every mode a program may have switched on in the real
/// terminal: attributes, mouse reporting, focus events, bracketed paste,
/// application cursor/keypad, cursor visibility, autowrap, scroll region.
const RESET_MODES: &[u8] = b"\x1b[0m\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1005l\x1b[?1006l\
\x1b[?1015l\x1b[?1004l\x1b[?2004l\x1b[?1l\x1b>\x1b[?25h\x1b[?7h\x1b[r";
const CLEAR: &[u8] = b"\x1b[H\x1b[2J";
const ENTER_ALTERNATE: &[u8] = b"\x1b[?1049h";
/// Default cursor shape, then back to the main screen.
const LEAVE_ALTERNATE: &[u8] = b"\x1b[0 q\x1b[?1049l";
/// Sequences with which a program leaves its alternate screen.
const ALTERNATE_EXITS: [&[u8]; 3] = [b"\x1b[?1049l", b"\x1b[?1047l", b"\x1b[?47l"];

/// Both stdin and stdout are terminals.
pub(crate) fn is_terminal() -> bool {
    atty::is(atty::Stream::Stdin) && atty::is(atty::Stream::Stdout)
}

/// The controlling terminal's size as (cols, rows), or 80×24.
pub(crate) fn terminal_size() -> (u16, u16) {
    match crossterm::terminal::window_size() {
        Ok(size) if size.columns > 0 && size.rows > 0 => (size.columns, size.rows),
        _ => (80, 24),
    }
}

/// What the attach client needs to create and find sessions.
#[derive(Debug, Clone)]
pub(crate) struct AttachContext {
    /// Request scope (`None` for session callers).
    pub scope: Option<ScopeRoot>,
    /// Directory for sessions created with `prefix n`.
    pub cwd: PathBuf,
    pub env: Vec<(String, String)>,
    /// The session this process runs in; never attached (nesting guard).
    pub self_id: Option<SessionId>,
}

impl AttachContext {
    pub(crate) fn new(invocation: &Invocation, scope: Option<ScopeRoot>) -> Self {
        Self {
            scope,
            cwd: invocation.cwd.clone(),
            env: invocation.env.clone(),
            self_id: invocation.self_id.clone(),
        }
    }
}

/// Attach to `target` full-screen until the user detaches, the session
/// exits or the connection is lost. Prints a one-line summary to `out`
/// afterwards.
pub(crate) fn run(
    api: &dyn SessionApi,
    ctx: &AttachContext,
    target: &SessionView,
    out: &mut dyn Write,
) -> Result<i32, CliError> {
    if !is_terminal() {
        return Err(CliError::invalid(
            "attaching needs a terminal on stdin and stdout",
        ));
    }
    if target.is_self || ctx.self_id.as_ref() == Some(&target.id) {
        return Err(CliError::invalid(format!(
            "`{}` is the session this command runs in; attaching it would mirror itself",
            target.name
        )));
    }
    let (cols, rows) = terminal_size();
    // Coming back to a session that stopped (exited, or lost in a daemon
    // restart) starts it again in its folder, with its resume command.
    if matches!(target.status, SessionStatus::Dead | SessionStatus::Exited) {
        api.revive(ReviveSession {
            id: target.id.clone(),
            env: ctx.env.clone(),
            cols,
            rows,
            resume: true,
        })?;
    } else if target.status != SessionStatus::Live {
        return Err(CliError::Api(ApiError::new(
            ErrorCode::SessionNotLive,
            format!("session `{}` is not running", target.name),
        )));
    }
    let stream = api.attach(AttachRequest {
        id: target.id.clone(),
        role: Role::Controller,
        cols,
        rows,
    })?;
    let (tx, rx) = mpsc::channel();
    let guard = TerminalGuard::enter()
        .map_err(|error| CliError::internal(format!("cannot set up the terminal: {error}")))?;
    spawn_stdin_reader(tx.clone())
        .map_err(|error| CliError::internal(format!("cannot read the terminal: {error}")))?;
    let outcome = AttachLoop::new(api, ctx, tx, rx, terminal_size, io::stdout()).run(stream);
    drop(guard);
    let emit =
        |out: &mut dyn Write, line: String| writeln!(out, "{line}").map_err(super::output_error);
    match outcome {
        Ok(Outcome::Detached { name }) => emit(out, format!("[detached from {name}]")).map(|_| 0),
        Ok(Outcome::Exited { name, code }) => {
            let line = match code {
                Some(code) => format!("[{name} exited with code {code}]"),
                None => format!("[{name} exited]"),
            };
            emit(out, line).map(|_| 0)
        }
        Ok(Outcome::InputClosed) => Ok(0),
        Ok(Outcome::Lost { name, error }) => Err(CliError::Api(ApiError::new(
            ErrorCode::DaemonUnavailable,
            match error {
                Some(error) => format!("lost the connection to `{name}`: {error}"),
                None => format!("lost the connection to `{name}`"),
            },
        ))),
        Err(error) => Err(CliError::internal(format!("terminal I/O failed: {error}"))),
    }
}

/// Raw mode and the alternate screen for the guard's lifetime; on drop,
/// every mode a program may have left on is reset.
struct TerminalGuard;

impl TerminalGuard {
    fn enter() -> io::Result<Self> {
        crossterm::terminal::enable_raw_mode()?;
        let guard = Self;
        let mut out = io::stdout().lock();
        out.write_all(ENTER_ALTERNATE)?;
        out.flush()?;
        Ok(guard)
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let mut out = io::stdout().lock();
        let _ = out.write_all(RESET_MODES);
        let _ = out.write_all(LEAVE_ALTERNATE);
        let _ = out.flush();
        let _ = crossterm::terminal::disable_raw_mode();
    }
}

fn spawn_stdin_reader(tx: Sender<Msg>) -> io::Result<()> {
    std::thread::Builder::new()
        .name("rakumux-stdin".into())
        .spawn(move || {
            let mut stdin = io::stdin();
            let mut buf = vec![0u8; 16 * 1024];
            loop {
                match stdin.read(&mut buf) {
                    Ok(0) => {
                        let _ = tx.send(Msg::StdinClosed);
                        return;
                    }
                    Ok(n) => {
                        if tx.send(Msg::Stdin(buf[..n].to_vec())).is_err() {
                            return;
                        }
                    }
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                    Err(_) => {
                        let _ = tx.send(Msg::StdinClosed);
                        return;
                    }
                }
            }
        })
        .map(|_| ())
}

// ---------------------------------------------------------------- input

/// What a chunk of keyboard input asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum InputAction {
    /// Bytes for the session.
    Forward(Vec<u8>),
    Detach,
    Next,
    Previous,
    New,
}

/// Splits keyboard input into bytes for the session and prefix commands.
/// The prefix state survives across chunks.
#[derive(Debug, Default)]
pub(crate) struct PrefixParser {
    pending: bool,
}

impl PrefixParser {
    pub(crate) fn feed(&mut self, bytes: &[u8]) -> Vec<InputAction> {
        let mut actions = Vec::new();
        let mut plain = Vec::new();
        for &byte in bytes {
            if !self.pending {
                if byte == PREFIX {
                    self.pending = true;
                } else {
                    plain.push(byte);
                }
                continue;
            }
            self.pending = false;
            let action = match byte {
                b'd' => InputAction::Detach,
                b'>' => InputAction::Next,
                b'<' => InputAction::Previous,
                b'n' => InputAction::New,
                PREFIX => {
                    plain.push(PREFIX);
                    continue;
                }
                // Unbound printable keys are swallowed (like tmux); anything
                // else (Esc, arrows) cancels the prefix and goes through.
                0x20..=0x7e => continue,
                _ => {
                    plain.push(byte);
                    continue;
                }
            };
            if !plain.is_empty() {
                actions.push(InputAction::Forward(std::mem::take(&mut plain)));
            }
            actions.push(action);
        }
        if !plain.is_empty() {
            actions.push(InputAction::Forward(plain));
        }
        actions
    }
}

// ---------------------------------------------------------------- loop

/// Messages the attach loop waits on.
pub(crate) enum Msg {
    Stdin(Vec<u8>),
    StdinClosed,
    /// An event of the attachment with this generation.
    Stream {
        generation: u64,
        event: AttachEvent,
    },
}

/// How an attach ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Outcome {
    Detached { name: String },
    Exited { name: String, code: Option<i32> },
    Lost { name: String, error: Option<String> },
    InputClosed,
}

/// The attachment currently shown.
struct Current {
    generation: u64,
    id: SessionId,
    name: String,
    sender: AttachSender,
    role: Role,
    /// Mirror of the session's screen, for redraws.
    mirror: vt::Emulator,
}

/// Event loop of the raw attach client, independent of the real terminal
/// so tests can drive it.
pub(crate) struct AttachLoop<'a, W, S> {
    api: &'a dyn SessionApi,
    ctx: &'a AttachContext,
    tx: Sender<Msg>,
    rx: Receiver<Msg>,
    size: S,
    out: W,
    parser: PrefixParser,
    generation: u64,
    /// Last terminal size seen, (cols, rows).
    terminal_size: (u16, u16),
    last_size_poll: Instant,
    /// Stable order for `>`/`<`, so cycling visits every session even
    /// though attaching changes recency.
    cycle: Vec<SessionId>,
    notice_until: Option<Instant>,
}

impl<'a, W: Write, S: FnMut() -> (u16, u16)> AttachLoop<'a, W, S> {
    pub(crate) fn new(
        api: &'a dyn SessionApi,
        ctx: &'a AttachContext,
        tx: Sender<Msg>,
        rx: Receiver<Msg>,
        mut size: S,
        out: W,
    ) -> Self {
        let terminal_size = size();
        Self {
            api,
            ctx,
            tx,
            rx,
            size,
            out,
            parser: PrefixParser::default(),
            generation: 0,
            terminal_size,
            last_size_poll: Instant::now(),
            cycle: Vec::new(),
            notice_until: None,
        }
    }

    pub(crate) fn run(mut self, first: AttachStream) -> io::Result<Outcome> {
        let mut current = self.install(first)?;
        loop {
            let message = self.rx.recv_timeout(SIZE_POLL);
            self.poll_size(&mut current);
            self.expire_notice(&mut current)?;
            let message = match message {
                Ok(message) => message,
                Err(RecvTimeoutError::Timeout) => continue,
                Err(RecvTimeoutError::Disconnected) => return Ok(Outcome::InputClosed),
            };
            match message {
                Msg::StdinClosed => {
                    let _ = current.sender.command(StreamCommand::Detach);
                    return Ok(Outcome::InputClosed);
                }
                Msg::Stdin(bytes) => {
                    for action in self.parser.feed(&bytes) {
                        match action {
                            InputAction::Forward(bytes) => self.forward(&mut current, bytes),
                            InputAction::Detach => {
                                let _ = current.sender.command(StreamCommand::Detach);
                                return Ok(Outcome::Detached { name: current.name });
                            }
                            InputAction::Next => self.cycle(&mut current, 1)?,
                            InputAction::Previous => self.cycle(&mut current, -1)?,
                            InputAction::New => self.new_session(&mut current)?,
                        }
                    }
                }
                Msg::Stream { generation, .. } if generation != current.generation => {}
                Msg::Stream { event, .. } => match event {
                    AttachEvent::Output { bytes, .. } => self.output(&mut current, &bytes)?,
                    AttachEvent::Event(StreamEvent::Resized {
                        cols,
                        rows,
                        checkpoint,
                        ..
                    }) => {
                        current.mirror = vt::Emulator::passive(rows.max(1), cols.max(1), 0);
                        current.mirror.feed(&decode(&checkpoint));
                        self.redraw(&mut current)?;
                    }
                    AttachEvent::Event(StreamEvent::RoleChanged { role }) => current.role = role,
                    AttachEvent::Event(StreamEvent::Exited { code }) => {
                        return Ok(Outcome::Exited {
                            name: current.name,
                            code,
                        })
                    }
                    // Title, cwd, bell and clipboard already reached the
                    // terminal inside the output bytes.
                    AttachEvent::Event(_) => {}
                    AttachEvent::Closed(error) => {
                        return Ok(Outcome::Lost {
                            name: current.name,
                            error,
                        })
                    }
                },
            }
        }
    }

    /// Show a freshly attached stream and start forwarding its events.
    fn install(&mut self, stream: AttachStream) -> io::Result<Current> {
        self.generation += 1;
        let generation = self.generation;
        let attached = &stream.attached;
        let checkpoint = decode(&attached.checkpoint);
        let mut mirror = vt::Emulator::passive(attached.rows.max(1), attached.cols.max(1), 0);
        mirror.feed(&checkpoint);
        let current = Current {
            generation,
            id: attached.session.id.clone(),
            name: attached.session.name.clone(),
            sender: stream.sender(),
            role: attached.role,
            mirror,
        };
        self.out.write_all(RESET_MODES)?;
        self.out.write_all(CLEAR)?;
        self.out.write_all(&checkpoint)?;
        self.out.flush()?;
        let (cols, rows) = self.terminal_size;
        if current.role == Role::Controller && (attached.cols, attached.rows) != (cols, rows) {
            let _ = current.sender.command(StreamCommand::Resize { cols, rows });
        }
        // Attaching counts as using the session (list order, done → idle).
        let _ = self.api.touch(&current.id);
        let tx = self.tx.clone();
        std::thread::Builder::new()
            .name("rakumux-attach-fwd".into())
            .spawn(move || forward_stream(stream, generation, tx))?;
        Ok(current)
    }

    fn forward(&mut self, current: &mut Current, bytes: Vec<u8>) {
        if current.role != Role::Controller {
            // Typing takes control back (e.g. after a TUI promoted itself).
            let (cols, rows) = self.terminal_size;
            let _ = current
                .sender
                .command(StreamCommand::Promote { cols, rows });
            current.role = Role::Controller;
        }
        // A failed send means the stream closed; its `Closed` event follows.
        let _ = current.sender.input(bytes);
    }

    fn output(&mut self, current: &mut Current, bytes: &[u8]) -> io::Result<()> {
        let was_alternate = current.mirror.screen().alternate_screen();
        current.mirror.feed(bytes);
        let left_alternate = (was_alternate && !current.mirror.screen().alternate_screen())
            || ALTERNATE_EXITS.iter().any(|exit| contains(bytes, exit));
        if left_alternate {
            return self.redraw(current);
        }
        self.out.write_all(bytes)?;
        self.out.flush()
    }

    /// Repaint the whole screen from the mirror.
    fn redraw(&mut self, current: &mut Current) -> io::Result<()> {
        self.out.write_all(RESET_MODES)?;
        self.out.write_all(CLEAR)?;
        self.out.write_all(&current.mirror.checkpoint())?;
        self.out.flush()
    }

    fn poll_size(&mut self, current: &mut Current) {
        if self.last_size_poll.elapsed() < SIZE_POLL {
            return;
        }
        self.last_size_poll = Instant::now();
        let size = (self.size)();
        if size == self.terminal_size || size.0 == 0 || size.1 == 0 {
            return;
        }
        self.terminal_size = size;
        if current.role == Role::Controller {
            let (cols, rows) = size;
            let _ = current.sender.command(StreamCommand::Resize { cols, rows });
        }
    }

    /// Switch `step` places along the cycle of running sessions in scope.
    fn cycle(&mut self, current: &mut Current, step: isize) -> io::Result<()> {
        let sessions = match self.api.list(self.ctx.scope.as_ref(), false) {
            Ok(sessions) => sessions,
            Err(error) => return self.notice(&error.message),
        };
        let mut live: Vec<SessionView> = sessions
            .into_iter()
            .filter(|s| {
                s.status == SessionStatus::Live
                    && !s.is_self
                    && self.ctx.self_id.as_ref() != Some(&s.id)
            })
            .collect();
        live.sort_by(|a, b| {
            b.last_active_at
                .cmp(&a.last_active_at)
                .then_with(|| a.id.cmp(&b.id))
        });
        let current_id = current.id.clone();
        self.cycle
            .retain(|id| *id == current_id || live.iter().any(|s| &s.id == id));
        for session in &live {
            if !self.cycle.contains(&session.id) {
                self.cycle.push(session.id.clone());
            }
        }
        let index = match self.cycle.iter().position(|id| *id == current_id) {
            Some(index) => index,
            None => {
                self.cycle.insert(0, current_id);
                0
            }
        };
        if self.cycle.len() < 2 {
            return self.notice("no other running session here");
        }
        let len = self.cycle.len() as isize;
        let target = self.cycle[(index as isize + step).rem_euclid(len) as usize].clone();
        self.switch_to(current, &target)
    }

    fn new_session(&mut self, current: &mut Current) -> io::Result<()> {
        let (cols, rows) = self.terminal_size;
        let request = CreateSession {
            scope: self.ctx.scope.clone(),
            name: None,
            cwd: Some(self.ctx.cwd.clone()),
            argv: None,
            startup_cmds: vec![],
            env: self.ctx.env.clone(),
            cols,
            rows,
            machine: None,
            prefill: None,
        };
        match self.api.create(request) {
            Ok(session) => {
                if let Some(index) = self.cycle.iter().position(|id| *id == current.id) {
                    self.cycle.insert(index + 1, session.id.clone());
                }
                self.switch_to(current, &session.id)
            }
            Err(error) => self.notice(&format!("cannot create a session: {}", error.message)),
        }
    }

    /// Attach `id` first, then detach the current one, so a failure leaves
    /// the user where they were.
    fn switch_to(&mut self, current: &mut Current, id: &SessionId) -> io::Result<()> {
        let (cols, rows) = self.terminal_size;
        let request = AttachRequest {
            id: id.clone(),
            role: Role::Controller,
            cols,
            rows,
        };
        match self.api.attach(request) {
            Ok(stream) => {
                let _ = current.sender.command(StreamCommand::Detach);
                self.notice_until = None;
                *current = self.install(stream)?;
                Ok(())
            }
            Err(error) => self.notice(&format!("cannot attach: {}", error.message)),
        }
    }

    /// Show `message` on the bottom line for a moment.
    fn notice(&mut self, message: &str) -> io::Result<()> {
        let (cols, rows) = self.terminal_size;
        let text: String = format!(" rakumux: {message} ")
            .chars()
            .take(cols as usize)
            .collect();
        // DECSC/DECRC keep the program's cursor and attributes.
        write!(
            self.out,
            "\x1b7\x1b[{rows};1H\x1b[0;7m\x1b[2K{text}\x1b[0m\x1b8"
        )?;
        self.out.flush()?;
        self.notice_until = Some(Instant::now() + NOTICE_TIME);
        Ok(())
    }

    fn expire_notice(&mut self, current: &mut Current) -> io::Result<()> {
        match self.notice_until {
            Some(until) if Instant::now() >= until => {
                self.notice_until = None;
                self.redraw(current)
            }
            _ => Ok(()),
        }
    }
}

/// Move an attach stream's events into the loop's channel, tagged with its
/// generation, until it closes or the loop is gone.
fn forward_stream(stream: AttachStream, generation: u64, tx: Sender<Msg>) {
    loop {
        let Some(event) = stream.recv_timeout(Duration::from_secs(3600)) else {
            continue;
        };
        let closed = matches!(event, AttachEvent::Closed(_));
        if tx.send(Msg::Stream { generation, event }).is_err() || closed {
            return;
        }
    }
}

/// A checkpoint the daemon sent; malformed data shows as an empty screen
/// until the program draws again.
fn decode(checkpoint: &str) -> Vec<u8> {
    base64::engine::general_purpose::STANDARD
        .decode(checkpoint)
        .unwrap_or_default()
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

#[cfg(test)]
mod tests {
    use super::super::fake::{checkpoint_of, session, FakeApi};
    use super::*;
    use session_core::client::{AttachOutgoing, AttachPeer};
    use std::sync::{Arc, Mutex};

    #[test]
    fn prefix_parser_splits_commands_from_input() {
        let mut parser = PrefixParser::default();
        assert_eq!(
            parser.feed(b"ls\r"),
            vec![InputAction::Forward(b"ls\r".to_vec())]
        );
        assert_eq!(
            parser.feed(b"ab\x1cdcd"),
            vec![
                InputAction::Forward(b"ab".to_vec()),
                InputAction::Detach,
                InputAction::Forward(b"cd".to_vec()),
            ]
        );
        assert_eq!(
            parser.feed(b"\x1c>\x1c<\x1cn"),
            vec![InputAction::Next, InputAction::Previous, InputAction::New]
        );
        // Literal Ctrl-\\, unbound keys swallowed, escape sequences pass.
        assert_eq!(
            parser.feed(b"x\x1c\x1cy\x1czq\x1c\x1b[A"),
            vec![InputAction::Forward(b"x\x1cyq\x1b[A".to_vec())]
        );
        // The prefix carries over to the next chunk.
        assert_eq!(
            parser.feed(b"k\x1c"),
            vec![InputAction::Forward(b"k".to_vec())]
        );
        assert_eq!(parser.feed(b"d"), vec![InputAction::Detach]);
    }

    #[derive(Clone, Default)]
    struct SharedBuf(Arc<Mutex<Vec<u8>>>);

    impl Write for SharedBuf {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl SharedBuf {
        fn bytes(&self) -> Vec<u8> {
            self.0.lock().unwrap().clone()
        }

        /// Wait (up to 5 s) until the loop has written `needle`: output
        /// reaches the loop on another thread than the test's input.
        fn wait_for(&self, needle: &[u8]) {
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            while !contains(&self.bytes(), needle) {
                assert!(std::time::Instant::now() < deadline, "never written");
                std::thread::sleep(Duration::from_millis(5));
            }
        }
    }

    fn ctx() -> AttachContext {
        AttachContext {
            scope: Some(ScopeRoot::from_canonical("/scope".into())),
            cwd: "/scope/sub".into(),
            env: vec![("TERM".into(), "xterm".into())],
            self_id: None,
        }
    }

    fn live(id: &str, name: &str, last_active_at: i64) -> SessionView {
        SessionView {
            pinned: false,
            git: None,
            last_active_at,
            ..session(id, name)
        }
    }

    /// Wait for the next attach on the fake and take its far end.
    fn take_peer(api: &FakeApi) -> (SessionId, AttachPeer) {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            {
                let mut peers = api.peers.lock().unwrap();
                if !peers.is_empty() {
                    return peers.remove(0);
                }
            }
            assert!(Instant::now() < deadline, "no attach");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn next_outgoing(peer: &AttachPeer) -> AttachOutgoing {
        peer.outgoing
            .recv_timeout(Duration::from_secs(5))
            .expect("outgoing message")
    }

    /// Run the loop on `first` in a thread while `script` drives it.
    fn drive(
        api: &FakeApi,
        ctx: &AttachContext,
        first: &str,
        size: Arc<Mutex<(u16, u16)>>,
        script: impl FnOnce(&Sender<Msg>),
    ) -> (Outcome, Vec<u8>) {
        drive_watching(api, ctx, first, size, |tx, _| script(tx))
    }

    /// [`drive`], with the script also seeing what the loop writes.
    fn drive_watching(
        api: &FakeApi,
        ctx: &AttachContext,
        first: &str,
        size: Arc<Mutex<(u16, u16)>>,
        script: impl FnOnce(&Sender<Msg>, &SharedBuf),
    ) -> (Outcome, Vec<u8>) {
        let (tx, rx) = mpsc::channel();
        let out = SharedBuf::default();
        let loop_out = out.clone();
        let loop_tx = tx.clone();
        let outcome = std::thread::scope(|scope| {
            let handle = scope.spawn(move || {
                let initial = *size.lock().unwrap();
                let stream = api
                    .attach(AttachRequest {
                        id: SessionId::from_string(first),
                        role: Role::Controller,
                        cols: initial.0,
                        rows: initial.1,
                    })
                    .unwrap();
                let size_fn = move || *size.lock().unwrap();
                AttachLoop::new(api, ctx, loop_tx, rx, size_fn, loop_out)
                    .run(stream)
                    .unwrap()
            });
            script(&tx, &out);
            handle.join().unwrap()
        });
        (outcome, out.bytes())
    }

    fn default_size() -> Arc<Mutex<(u16, u16)>> {
        Arc::new(Mutex::new((80, 24)))
    }

    fn count(haystack: &[u8], needle: &[u8]) -> usize {
        haystack
            .windows(needle.len())
            .filter(|w| *w == needle)
            .count()
    }

    #[test]
    fn passes_output_and_input_through_until_detach() {
        let api = FakeApi::user(vec![live("S1", "api", 3)]);
        let (outcome, out) = drive_watching(&api, &ctx(), "S1", default_size(), |tx, out| {
            let (_, peer) = take_peer(&api);
            peer.events
                .send(AttachEvent::Output {
                    offset: 0,
                    bytes: b"hello".to_vec(),
                })
                .unwrap();
            out.wait_for(b"hello");
            tx.send(Msg::Stdin(b"ls\r".to_vec())).unwrap();
            assert_eq!(
                next_outgoing(&peer),
                AttachOutgoing::Input(b"ls\r".to_vec())
            );
            tx.send(Msg::Stdin(b"\x1c\x1c".to_vec())).unwrap();
            assert_eq!(next_outgoing(&peer), AttachOutgoing::Input(vec![PREFIX]));
            tx.send(Msg::Stdin(b"\x1cd".to_vec())).unwrap();
            assert_eq!(
                next_outgoing(&peer),
                AttachOutgoing::Command(StreamCommand::Detach)
            );
        });
        assert_eq!(outcome, Outcome::Detached { name: "api".into() });
        assert!(contains(&out, &checkpoint_of("S1")));
        assert!(contains(&out, b"hello"));
        assert!(api.calls().contains(&"touch S1".to_string()));
    }

    #[test]
    fn cycles_through_running_sessions_by_recency() {
        let mut dead = live("S3", "dead", 25);
        dead.status = SessionStatus::Dead;
        let api = FakeApi::user(vec![
            live("S1", "one", 30),
            live("S2", "two", 20),
            dead,
            live("S4", "four", 10),
        ]);
        let (outcome, out) = drive(&api, &ctx(), "S1", default_size(), |tx| {
            let mut peers = vec![take_peer(&api)];
            for (keys, expected) in [
                (b"\x1c>", "S2"),
                (b"\x1c>", "S4"),
                (b"\x1c>", "S1"),
                (b"\x1c<", "S4"),
            ] {
                tx.send(Msg::Stdin(keys.to_vec())).unwrap();
                let (id, peer) = take_peer(&api);
                assert_eq!(id.as_str(), expected);
                // The previous attachment is detached after the switch.
                let previous = &peers.last().unwrap().1;
                assert_eq!(
                    next_outgoing(previous),
                    AttachOutgoing::Command(StreamCommand::Detach)
                );
                peers.push((id, peer));
            }
            // Events of an old attachment are ignored.
            peers[0]
                .1
                .events
                .send(AttachEvent::Output {
                    offset: 0,
                    bytes: b"stale".to_vec(),
                })
                .unwrap();
            tx.send(Msg::Stdin(b"\x1cd".to_vec())).unwrap();
            assert_eq!(
                next_outgoing(&peers.last().unwrap().1),
                AttachOutgoing::Command(StreamCommand::Detach)
            );
        });
        assert_eq!(
            outcome,
            Outcome::Detached {
                name: "four".into()
            }
        );
        assert!(contains(&out, &checkpoint_of("S2")));
        assert!(!contains(&out, b"stale"));
        assert!(api
            .calls()
            .iter()
            .all(|call| !call.starts_with("attach S3")));
    }

    #[test]
    fn a_lone_session_shows_a_notice_instead_of_switching() {
        let api = FakeApi::user(vec![live("S1", "solo", 1)]);
        let (outcome, out) = drive(&api, &ctx(), "S1", default_size(), |tx| {
            let (_, peer) = take_peer(&api);
            tx.send(Msg::Stdin(b"\x1c>".to_vec())).unwrap();
            tx.send(Msg::Stdin(b"\x1cd".to_vec())).unwrap();
            assert_eq!(
                next_outgoing(&peer),
                AttachOutgoing::Command(StreamCommand::Detach)
            );
        });
        assert_eq!(
            outcome,
            Outcome::Detached {
                name: "solo".into()
            }
        );
        let text = String::from_utf8_lossy(&out);
        assert!(text.contains("no other running session here"), "{text:?}");
    }

    #[test]
    fn prefix_n_creates_a_session_in_the_cwd_and_switches() {
        let api = FakeApi::user(vec![live("S1", "one", 1)]);
        let (outcome, _) = drive(&api, &ctx(), "S1", default_size(), |tx| {
            let (_, first) = take_peer(&api);
            tx.send(Msg::Stdin(b"\x1cn".to_vec())).unwrap();
            let (id, second) = take_peer(&api);
            assert_eq!(id.as_str(), "NEW0");
            assert_eq!(
                next_outgoing(&first),
                AttachOutgoing::Command(StreamCommand::Detach)
            );
            tx.send(Msg::Stdin(b"\x1cd".to_vec())).unwrap();
            assert_eq!(
                next_outgoing(&second),
                AttachOutgoing::Command(StreamCommand::Detach)
            );
        });
        assert_eq!(
            outcome,
            Outcome::Detached {
                name: "auto".into()
            }
        );
        let created = api.created.lock().unwrap();
        assert_eq!(created.len(), 1);
        assert_eq!(created[0].cwd, Some(PathBuf::from("/scope/sub")));
        assert_eq!(created[0].scope, ctx().scope);
        assert_eq!(created[0].env, ctx().env);
        assert_eq!((created[0].cols, created[0].rows), (80, 24));
    }

    #[test]
    fn terminal_resizes_are_sent_to_the_daemon() {
        let api = FakeApi::user(vec![live("S1", "one", 1)]);
        let size = default_size();
        let (outcome, out) = drive(&api, &ctx(), "S1", size.clone(), |_| {
            let (_, peer) = take_peer(&api);
            *size.lock().unwrap() = (120, 40);
            assert_eq!(
                next_outgoing(&peer),
                AttachOutgoing::Command(StreamCommand::Resize {
                    cols: 120,
                    rows: 40
                })
            );
            let checkpoint = base64::engine::general_purpose::STANDARD.encode(b"RESIZED SCREEN");
            peer.events
                .send(AttachEvent::Event(StreamEvent::Resized {
                    cols: 120,
                    rows: 40,
                    checkpoint,
                    offset: 0,
                }))
                .unwrap();
            peer.events
                .send(AttachEvent::Event(StreamEvent::Exited { code: Some(2) }))
                .unwrap();
        });
        assert_eq!(
            outcome,
            Outcome::Exited {
                name: "one".into(),
                code: Some(2)
            }
        );
        // The resize repainted the screen from the new checkpoint.
        assert!(contains(&out, b"RESIZED SCREEN"));
    }

    #[test]
    fn leaving_the_programs_alternate_screen_redraws_instead() {
        let api = FakeApi::user(vec![live("S1", "one", 1)]);
        let (_, out) = drive(&api, &ctx(), "S1", default_size(), |tx| {
            let (_, peer) = take_peer(&api);
            for bytes in [&b"\x1b[?1049hVIM"[..], b"\x1b[2J\x1b[?1049l$ "] {
                peer.events
                    .send(AttachEvent::Output {
                        offset: 0,
                        bytes: bytes.to_vec(),
                    })
                    .unwrap();
            }
            std::thread::sleep(Duration::from_millis(100));
            tx.send(Msg::Stdin(b"\x1cd".to_vec())).unwrap();
            next_outgoing(&peer);
        });
        assert!(contains(&out, b"VIM"));
        // `?1049l` never reaches the real terminal; the main screen is
        // repainted from the mirror instead.
        assert!(!contains(&out, b"\x1b[?1049l"));
        assert_eq!(count(&out, &checkpoint_of("S1")), 2);
    }

    #[test]
    fn typing_as_observer_promotes_first() {
        let api = FakeApi::user(vec![live("S1", "one", 1)]);
        let (outcome, _) = drive(&api, &ctx(), "S1", default_size(), |tx| {
            let (_, peer) = take_peer(&api);
            peer.events
                .send(AttachEvent::Event(StreamEvent::RoleChanged {
                    role: Role::Observer,
                }))
                .unwrap();
            std::thread::sleep(Duration::from_millis(100));
            tx.send(Msg::Stdin(b"x".to_vec())).unwrap();
            assert_eq!(
                next_outgoing(&peer),
                AttachOutgoing::Command(StreamCommand::Promote { cols: 80, rows: 24 })
            );
            assert_eq!(next_outgoing(&peer), AttachOutgoing::Input(b"x".to_vec()));
            drop(peer);
        });
        assert_eq!(
            outcome,
            Outcome::Lost {
                name: "one".into(),
                error: None
            }
        );
    }
}
