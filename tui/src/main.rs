mod account;
mod app;
mod auth;
mod cli;
mod components;
mod escape_time;
mod help;
mod icons;
mod layout;
mod naming;
mod qr;
mod remote_access;
mod restart;
mod terminal_view;
mod theme;
mod ui;
mod updater;

use std::io::{self, Write};
use std::path::PathBuf;
use std::time::Duration;

use anyhow::Result;
use clap::Parser;
use crossterm::{
    event::{
        DisableBracketedPaste, DisableFocusChange, DisableMouseCapture, EnableBracketedPaste,
        EnableFocusChange, EnableMouseCapture, EventStream,
    },
    execute,
    terminal::{
        disable_raw_mode, enable_raw_mode, Clear, ClearType, EnterAlternateScreen,
        LeaveAlternateScreen,
    },
};
use futures_util::StreamExt;
use ratatui::{backend::CrosstermBackend, Terminal};
use session_core::ids::SessionId;
use session_core::model::ScopeRoot;
use session_core::paths::Layout;
use tokio::time::Instant;

use crate::app::types::AppResult;
use crate::app::{app_version, App, AppOptions, ExecMode, Settings};

/// Minimum time between redraws caused by output (~60 fps). Input redraws
/// at once.
const FRAME: Duration = Duration::from_millis(16);
const TICK_IDLE: Duration = Duration::from_secs(1);
const TICK_ANIMATING: Duration = Duration::from_millis(100);

enum RunOutcome {
    Exit,
    Restart(Option<std::path::PathBuf>),
    SwitchDirectory(String),
}

fn restart_current_process(launcher: Option<&std::path::Path>) -> Result<()> {
    restart::current_process(launcher).map_err(Into::into)
}

/// Note a newer release for the next start (runs in the background).
async fn check_for_updates_background() -> Result<()> {
    let mut state = updater::UpdateState::load().unwrap_or_default();
    if !state.should_check_for_updates() {
        return Ok(());
    }
    if let Some((latest_version, method)) = updater::check_for_updates(app_version()).await? {
        state.mark_update_available(latest_version, method);
    } else {
        state.clear_pending_upgrade();
    }
    state.update_last_check();
    state.save()
}

fn print_cli_help() {
    println!("rakumux - terminal sessions for your folders");
    println!();
    println!("Usage: rakumux [OPTIONS]            Open the session screen for this folder");
    println!("       rakumux <COMMAND> [ARGS]     See `rakumux help`; commands: new, ls, attach,");
    println!("                                      rename, pin, unpin, kill, rm, folders, session, daemon,");
    println!("                                      remote, account");
    println!();
    println!("Options:");
    println!("  -d, --debug              Enable debug output");
    println!("      --new-server         Start a new server even if one is running: it takes over");
    println!("                           the running sessions, which keep running");
    println!("      --ptyd-path <PATH>   Path to the rakumux-ptyd server binary");
    println!("  -h, --help               Show this help and exit");
    println!("      --version            Show version and exit");
    println!();
    println!("The session screen lists the sessions of every folder, and P filters them by");
    println!("folder. Sessions run in a background server (rakumux-ptyd), one per user on");
    println!("this machine: Rakumux connects to it from any login, and starts it when none");
    println!("runs. Sessions keep running after you quit, and come back after a crash or a");
    println!("reboot (agents resume their conversation).");
    println!("The interactive screen must run in a terminal; it can't run when stdout");
    println!("is captured.");
}

#[derive(Parser)]
#[command(name = "rakumux")]
#[command(about = "Terminal sessions for your folders")]
struct Cli {
    /// Enable debug output
    #[arg(short, long)]
    debug: bool,

    /// Path to the rakumux-ptyd server binary
    #[arg(long, value_name = "PATH")]
    ptyd_path: Option<PathBuf>,

    /// Start a new server even if one is running (it takes over the
    /// running sessions)
    #[arg(long)]
    new_server: bool,

    /// Ignored (older launchers pass it)
    #[arg(long, value_name = "PATH", hide = true)]
    #[allow(dead_code)]
    services_path: Option<String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    // CLI subcommands (`rakumux new`, `ls`, `session …`) run without the TUI.
    if let Some(code) = cli::dispatch(&std::env::args().collect::<Vec<_>>()) {
        std::process::exit(code);
    }

    let args: Vec<String> = std::env::args().collect();
    let force_debug = std::env::var("RAKUMUX_DEBUG").is_ok();
    if force_debug {
        eprintln!("DEBUG: Rakumux starting up...");
        eprintln!("DEBUG: Args: {:?}", args);
        eprintln!("DEBUG: Current dir: {:?}", std::env::current_dir());
        eprintln!("DEBUG: TERM: {:?}", std::env::var("TERM"));
    }

    for arg in &args[1..] {
        if arg == "--help" || arg == "-h" {
            print_cli_help();
            return Ok(());
        }
        if arg == "--version" {
            println!("rakumux {}", app_version());
            return Ok(());
        }
    }

    if !atty::is(atty::Stream::Stdout) {
        eprintln!("Error: Cannot run the session screen when stdout is captured.");
        eprintln!("Run `rakumux` directly in a terminal, or use a subcommand such as");
        eprintln!("`rakumux ls` from scripts.");
        return Err(anyhow::anyhow!("Cannot run TUI with captured stdout"));
    }

    let cli = Cli::parse();
    let debug = cli.debug || force_debug;

    if std::env::var("TERM").unwrap_or_default().is_empty() {
        eprintln!("Error: TERM environment variable not set - not running in a terminal");
        return Err(anyhow::anyhow!("Must run in a terminal"));
    }

    // `--new-server`: replace the running server before connecting (it may
    // ask on the terminal, so before the TUI takes it over).
    let new_server = if cli.new_server {
        match cli::new_server(cli.ptyd_path.clone()) {
            Ok(message) => Some(message),
            Err(message) => {
                eprintln!("rakumux: {message}");
                return Err(anyhow::anyhow!("{message}"));
            }
        }
    } else {
        None
    };

    // Scope = the canonical folder Rakumux was started from.
    let cwd = std::env::current_dir()
        .map_err(|e| anyhow::anyhow!("Can't read the current directory: {e}"))?;
    let scope = ScopeRoot::resolve(&cwd)
        .map_err(|e| anyhow::anyhow!("Can't resolve {}: {e}", cwd.display()))?;
    let paths = Layout::from_env()
        .map_err(|e| anyhow::anyhow!("Can't locate Rakumux's data directory: {e}"))?;
    // Remote access on (`rakumux remote on`) and no API running, e.g.
    // after a reboot: start one in the background.
    remote_access::autostart(&paths, cli.ptyd_path.clone());
    let phone_layout = paths.clone();
    let account_layout = paths.clone();
    let config_path = Settings::config_file_path().ok();
    let (settings, config_error) = match config_path.as_deref().map(Settings::load_from) {
        Some(Ok(settings)) => (settings, None),
        Some(Err(e)) => (Settings::default(), Some(e.to_string())),
        None => (Settings::default(), None),
    };

    let connector = app::client_connector(app::client_config(paths.clone(), cli.ptyd_path.clone()));
    let mut app = App::new(AppOptions {
        settings,
        config_path,
        scope,
        paths: Some(paths),
        self_session: std::env::var(session_core::env::SESSION_ID)
            .ok()
            .filter(|id| !id.is_empty())
            .map(SessionId::from_string),
        connector,
        namer: app::ai_namer,
        phone: std::sync::Arc::new(remote_access::LocalPhoneAccess {
            layout: phone_layout,
            ptyd_path: cli.ptyd_path.clone(),
        }),
        account: std::sync::Arc::new(account::LocalAccount::new(
            account_layout,
            std::env::current_exe().unwrap_or_else(|_| PathBuf::from("rakumux")),
            "tui",
            app::app_version(),
        )),
        updater: std::sync::Arc::new(updater::SystemUpdateService),
        exec_mode: ExecMode::Threaded,
        debug,
    });
    app.ptyd_path = cli.ptyd_path.clone();
    app.load_update_state();
    if !app.settings.sessions.welcome_seen {
        app.open_welcome();
    }
    if let Some(message) = new_server {
        app.set_status(message);
    }
    if let Some(error) = config_error {
        app.set_status(format!(
            "Couldn't read ~/.rakumux/config.json ({error}); using defaults"
        ));
    }

    enable_raw_mode().map_err(|e| {
        eprintln!("Failed to enable raw mode: {}", e);
        e
    })?;
    let mut stdout = io::stdout();
    execute!(
        stdout,
        EnterAlternateScreen,
        EnableMouseCapture,
        EnableBracketedPaste,
        // The window coming to the front takes the session's size.
        EnableFocusChange
    )
    .map_err(|e| {
        let _ = disable_raw_mode();
        eprintln!("Failed to setup terminal: {}", e);
        e
    })?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend).map_err(|e| {
        let _ = disable_raw_mode();
        let _ = execute!(
            io::stdout(),
            LeaveAlternateScreen,
            DisableMouseCapture,
            DisableBracketedPaste,
            DisableFocusChange
        );
        eprintln!("Failed to create terminal: {}", e);
        e
    })?;

    let run_result = run_app(&mut terminal, &mut app).await;
    // Detach before the terminal is restored (dropping the app detaches).
    drop(app);
    let cleanup_result = cleanup_terminal(&mut terminal);

    let outcome = match run_result {
        Ok(outcome) => {
            cleanup_result?;
            outcome
        }
        Err(err) => {
            eprintln!("Error: {}", err);
            cleanup_result?;
            return Err(err);
        }
    };

    match outcome {
        RunOutcome::Exit => Ok(()),
        RunOutcome::Restart(launcher) => restart_current_process(launcher.as_deref()),
        RunOutcome::SwitchDirectory(path) => {
            // The shell wrapper evaluates this line.
            println!("cd \"{}\"", path);
            Ok(())
        }
    }
}

/// The terminal events Rakumux reads, and what it handles after joining
/// keys that came in pieces, with the state they found the app in: for
/// terminal apps that send keys unusually (Termius on a phone). The last
/// [`INPUT_LOG_LINES`] are kept in memory and written to `~/.rakumux/
/// logs/tui-input.log` on quit; `--debug` writes every line as it happens.
/// What's typed in session mode is the user's own: it's logged as
/// "(session key)" only.
struct InputLog {
    path: Option<std::path::PathBuf>,
    /// `--debug`: appended to as events come.
    live: Option<std::fs::File>,
    recent: std::collections::VecDeque<String>,
    started: std::time::Instant,
}

/// Lines of input kept for the log written on quit.
const INPUT_LOG_LINES: usize = 500;

impl InputLog {
    fn open(app: &App) -> Self {
        let path = app
            .paths()
            .map(|paths| paths.home.join("logs").join("tui-input.log"));
        let live = app
            .debug()
            .then_some(path.as_ref())
            .flatten()
            .and_then(|path| {
                std::fs::create_dir_all(path.parent()?).ok()?;
                std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(path)
                    .ok()
            });
        let mut log = Self {
            path,
            live,
            recent: std::collections::VecDeque::new(),
            started: std::time::Instant::now(),
        };
        let size = app.area;
        log.line(format!(
            "--- rakumux {} started, {}x{}, TERM={}",
            app_version(),
            size.width,
            size.height,
            std::env::var("TERM").unwrap_or_default()
        ));
        log
    }

    fn record(&mut self, what: &str, event: &crossterm::event::Event, app: &App) {
        let private = app.input_is_private()
            && matches!(
                event,
                crossterm::event::Event::Key(_) | crossterm::event::Event::Paste(_)
            );
        let text = match event {
            _ if private => "(private input)".to_string(),
            crossterm::event::Event::Paste(text) => {
                format!("Paste({} chars)", text.chars().count())
            }
            other => format!("{other:?}"),
        };
        self.line(format!("{what:<7} [{}] {text}", app.input_state()));
    }

    fn line(&mut self, text: String) {
        let line = format!("{:>8} {text}", self.started.elapsed().as_millis());
        if let Some(file) = self.live.as_mut() {
            let _ = writeln!(file, "{line}");
        }
        if self.recent.len() == INPUT_LOG_LINES {
            self.recent.pop_front();
        }
        self.recent.push_back(line);
    }
}

impl InputLog {
    /// Replace the log file with the last lines (on quit, or on SIGUSR1).
    /// With `--debug` the file already has every line.
    fn write(&mut self) {
        if self.live.is_some() {
            return;
        }
        let Some(path) = self.path.as_ref() else {
            return;
        };
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let mut text: String = self.recent.iter().map(|l| format!("{l}\n")).collect();
        text.push_str(&format!(
            "{:>8} --- written (pid {})\n",
            self.started.elapsed().as_millis(),
            std::process::id()
        ));
        let _ = std::fs::write(path, text);
    }
}

impl Drop for InputLog {
    /// Without `--debug`, the last lines replace the log on quit.
    fn drop(&mut self) {
        self.write();
    }
}

#[cfg(test)]
mod escape_wait_tests {
    use std::time::{Duration, Instant};

    use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};

    use super::*;
    use crate::app::testing::{harness, view};
    use crate::app::types::Focus;
    use crate::escape_time::{EscapeJoiner, Link, ESCAPE_TIME_BRIEF, ESCAPE_TIME_RAKUMUX};

    fn key(code: KeyCode) -> Event {
        Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    #[tokio::test]
    async fn esc_leaves_the_feed_at_once_on_a_desktop_over_ssh() {
        let mut h = harness(vec![view("A", "a", 20), view("B", "b", 10)]);
        h.app.resize(200, 50);
        h.app.toggle_feed();
        assert!(h.app.feed_has_keys());
        let wait = escape_wait(&h.app, Link::Remote);
        assert_eq!(wait, ESCAPE_TIME_BRIEF);
        let mut escapes = EscapeJoiner::default();
        let now = Instant::now();
        assert_eq!(escapes.feed(key(KeyCode::Esc), now, wait), vec![]);
        assert_eq!(escapes.deadline(), Some(now + ESCAPE_TIME_BRIEF));
        let events = escapes.expire(now + ESCAPE_TIME_BRIEF);
        assert_eq!(events, vec![key(KeyCode::Esc)]);
        for event in events {
            h.app.handle_terminal_event(event).await;
        }
        assert!(!h.app.feed_has_keys(), "the list has the keys back");
        assert!(h.app.feed_shown(), "the feed stays beside the session");
        // The list's own Esc (back to a session) is as quick there.
        assert_eq!(escape_wait(&h.app, Link::Remote), ESCAPE_TIME_BRIEF);
    }

    #[tokio::test]
    async fn an_arrow_in_pieces_from_a_phone_over_ssh_is_still_an_arrow() {
        let mut h = harness(vec![view("A", "a", 20), view("B", "b", 10)]);
        h.app.resize(45, 40);
        assert!(h.app.phone());
        let wait = escape_wait(&h.app, Link::Remote);
        assert_eq!(wait, ESCAPE_TIME_RAKUMUX);
        let mut escapes = EscapeJoiner::default();
        let now = Instant::now();
        assert_eq!(escapes.feed(key(KeyCode::Esc), now, wait), vec![]);
        // The rest of the arrow 400 ms later still makes an arrow.
        let later = now + Duration::from_millis(400);
        assert_eq!(escapes.expire(later), vec![]);
        let mut events = escapes.feed(key(KeyCode::Char('[')), later, wait);
        events.extend(escapes.feed(
            Event::Key(KeyEvent::new(KeyCode::Char('A'), KeyModifiers::SHIFT)),
            later,
            wait,
        ));
        assert_eq!(events, vec![key(KeyCode::Up)]);
        for event in events {
            h.app.handle_terminal_event(event).await;
        }
        assert_eq!(h.app.focus, Focus::List, "not thrown into a session");
    }
}

#[cfg(test)]
mod input_log_tests {
    use super::*;

    #[test]
    fn private_dialog_input_never_appears_in_debug_logs() {
        let mut harness = crate::app::testing::harness(vec![]);
        harness.app.mode =
            crate::app::types::AppMode::AuthCode(crate::app::types::AuthCodeDialog {
                input: crate::app::TextInput::new("private-example-code"),
                error_message: None,
            });
        let mut log = InputLog {
            path: None,
            live: None,
            recent: std::collections::VecDeque::new(),
            started: std::time::Instant::now(),
        };
        let event = crossterm::event::Event::Key(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char('x'),
            crossterm::event::KeyModifiers::NONE,
        ));
        log.record("read", &event, &harness.app);
        let line = log.recent.back().unwrap();
        assert!(line.contains("(private input)"));
        assert!(!line.contains("Char("));
        assert!(!line.contains("private-example-code"));
    }
}

fn cleanup_terminal<B: ratatui::backend::Backend + Write>(
    terminal: &mut Terminal<B>,
) -> Result<()> {
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture,
        DisableBracketedPaste,
        DisableFocusChange,
        Clear(ClearType::All)
    )?;
    terminal.show_cursor()?;
    print!("\x1B[2J\x1B[H");
    let _ = io::stdout().flush();
    Ok(())
}

fn draw<B: ratatui::backend::Backend + Write>(
    terminal: &mut Terminal<B>,
    app: &mut App,
) -> Result<()> {
    terminal.draw(|f| ui::draw(f, app))?;
    let passthrough = app.take_terminal_output();
    if !passthrough.is_empty() {
        let backend = terminal.backend_mut();
        backend.write_all(&passthrough)?;
        Write::flush(backend)?;
    }
    Ok(())
}

/// How long a lone Esc waits for the rest of a key in the app's state now:
/// where it would go (the session's program, Rakumux mode, a dialog) and
/// whether the window is a phone's (`escape_time::wait`).
fn escape_wait(app: &App, link: escape_time::Link) -> std::time::Duration {
    let target = if app.keys_go_to_session() {
        escape_time::EscTarget::Session
    } else if matches!(app.mode, app::types::AppMode::Normal) {
        escape_time::EscTarget::List
    } else {
        escape_time::EscTarget::Dialog
    };
    escape_time::wait(link, app.phone(), target)
}

async fn run_app<B: ratatui::backend::Backend + Write>(
    terminal: &mut Terminal<B>,
    app: &mut App,
) -> Result<RunOutcome> {
    let size = terminal.size()?;
    app.resize(size.width, size.height);
    draw(terminal, app)?;
    // Connect after the first paint so the screen shows at once.
    app.start();
    tokio::spawn(async {
        let _ = check_for_updates_background().await;
    });

    let mut background = app
        .take_receiver()
        .ok_or_else(|| anyhow::anyhow!("event receiver already taken"))?;
    let mut input = EventStream::new();
    // Keys whose escape sequence arrives in pieces (a phone over SSH).
    let mut escapes = escape_time::EscapeJoiner::default();
    let link = escape_time::Link::from_env(|name| std::env::var_os(name));
    let mut input_log = InputLog::open(app);
    // `kill -USR1 <pid>` writes the input log now (a running Rakumux on
    // a phone, without quitting it).
    let mut dump_signal =
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::user_defined1()).ok();
    let mut last_draw = Instant::now();
    let mut next_tick = Instant::now() + TICK_IDLE;
    let mut dirty = false;

    loop {
        let mut draw_now = false;
        let escape_deadline = escapes.deadline().map(Instant::from_std);
        let events = tokio::select! {
            event = input.next() => match event {
                Some(Ok(event)) => {
                    input_log.record("read", &event, app);
                    escapes.feed(event, std::time::Instant::now(), escape_wait(app, link))
                }
                Some(Err(e)) => return Err(anyhow::anyhow!("Failed to read input events: {e}")),
                None => return Ok(RunOutcome::Exit),
            },
            _ = tokio::time::sleep_until(escape_deadline.unwrap_or_else(Instant::now)),
                if escape_deadline.is_some() =>
            {
                escapes.expire(std::time::Instant::now())
            }
            Some(event) = background.recv() => {
                app.handle_event(event);
                while let Ok(event) = background.try_recv() {
                    app.handle_event(event);
                }
                dirty = true;
                Vec::new()
            }
            _ = tokio::time::sleep_until(next_tick) => {
                let tick = if app.is_animating() { TICK_ANIMATING } else { TICK_IDLE };
                next_tick = Instant::now() + tick;
                // Relative times and spinners change even when nothing happens.
                app.on_tick(std::time::Instant::now());
                dirty = true;
                Vec::new()
            }
            _ = tokio::time::sleep_until(last_draw + FRAME), if dirty => Vec::new(),
            Some(()) = async {
                match dump_signal.as_mut() {
                    Some(signal) => signal.recv().await,
                    None => std::future::pending().await,
                }
            } => {
                input_log.write();
                Vec::new()
            }
        };
        for event in events {
            input_log.record("handled", &event, app);
            match app.handle_terminal_event(event).await {
                AppResult::Continue => {}
                AppResult::Exit => return Ok(RunOutcome::Exit),
                AppResult::SwitchDirectory(path) => return Ok(RunOutcome::SwitchDirectory(path)),
            }
            draw_now = true;
        }
        if app.is_animating() && next_tick > Instant::now() + TICK_ANIMATING {
            next_tick = Instant::now() + TICK_ANIMATING;
        }
        if draw_now || (dirty && last_draw.elapsed() >= FRAME) {
            draw(terminal, app)?;
            last_draw = Instant::now();
            dirty = false;
        }
        if app.restart_requested {
            return Ok(RunOutcome::Restart(app.restart_launcher.clone()));
        }
    }
}
