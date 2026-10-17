//! `orchestra daemon status|stop|restart|logs`.

use super::args::DaemonCommand;
use super::{layout, output_error, CliError, Globals};
use serde_json::json;
use session_core::api::{ApiError, DaemonStatusInfo, ErrorCode, CAPABILITY_DAEMON_RESTART};
use session_core::client::SessionApi;
use session_core::paths::{Layout, Registration};
use std::io::{self, BufRead, Read, Seek, SeekFrom, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::{Duration, Instant};

/// How long `stop` waits for the daemon to go away.
const STOP_WAIT: Duration = Duration::from_secs(5);
const FOLLOW_POLL: Duration = Duration::from_millis(250);

pub(crate) fn run(
    globals: &Globals,
    command: DaemonCommand,
    out: &mut dyn Write,
) -> Result<i32, CliError> {
    let layout = layout()?;
    match command {
        DaemonCommand::Status { json } => status(globals, &layout, json, out),
        DaemonCommand::Stop { yes } => {
            if stop(globals, &layout, yes)? {
                writeln!(out, "orchestra-ptyd stopped").map_err(output_error)?;
            } else {
                writeln!(out, "orchestra-ptyd is not running").map_err(output_error)?;
            }
            Ok(0)
        }
        DaemonCommand::Start => {
            let message = start(globals, &layout)?;
            writeln!(out, "{message}").map_err(output_error)?;
            Ok(0)
        }
        DaemonCommand::Restart { yes } => {
            let message = restart(globals, &layout, yes)?;
            writeln!(out, "{message}").map_err(output_error)?;
            Ok(0)
        }
        DaemonCommand::Logs { lines, follow } => logs(&layout.log_file(), lines, follow, out),
    }
}

/// A daemon answers on the socket named in the registration file (or in a
/// legacy one, see `Layout::legacy`).
fn is_running(layout: &Layout) -> bool {
    std::iter::once(layout.clone())
        .chain(layout.legacy())
        .any(|layout| {
            Registration::load(&layout)
                .map(|registration| UnixStream::connect(registration.socket).is_ok())
                .unwrap_or(false)
        })
}

/// How long a restart in place may take before the daemon answers again.
const RESTART_WAIT: Duration = Duration::from_secs(15);

/// Start the daemon unless one is running; says which.
fn start(globals: &Globals, layout: &Layout) -> Result<String, CliError> {
    match globals.connect(false) {
        Ok(client) => {
            let info = client.status()?;
            return Ok(format!(
                "orchestra-ptyd {} is already running (pid {})",
                info.version, info.pid
            ));
        }
        Err(CliError::Api(error))
            if error.code == ErrorCode::DaemonUnavailable && !is_running(layout) => {}
        Err(error) => return Err(error),
    }
    let info = globals.connect(true)?.status()?;
    Ok(format!(
        "orchestra-ptyd {} started (pid {})",
        info.version, info.pid
    ))
}

/// Replace the daemon with a new one from this Orchestra's
/// `orchestra-ptyd`. A daemon that can restart in place (`daemon.restart`)
/// keeps every session running; an older one is stopped (after asking,
/// unless `yes`) and a new one started; with none running, one starts.
/// Returns what happened, for the person.
pub(crate) fn restart(globals: &Globals, layout: &Layout, yes: bool) -> Result<String, CliError> {
    let client = match globals.connect(false) {
        Ok(client) => Some(client),
        Err(CliError::Api(error))
            if error.code == ErrorCode::DaemonUnavailable && !is_running(layout) =>
        {
            return start(globals, layout);
        }
        // A daemon speaking another protocol can't be asked; `stop` signals it.
        Err(CliError::Api(error)) if error.code == ErrorCode::ProtocolMismatch => None,
        Err(error) => return Err(error),
    };
    let in_place = client.as_ref().is_some_and(|client| {
        client
            .capabilities()
            .iter()
            .any(|c| c == CAPABILITY_DAEMON_RESTART)
    });
    let mut refused = None;
    if let (Some(client), true) = (&client, in_place) {
        let binary = ptyd_binary(globals)?;
        match client.restart_daemon(Some(&binary)) {
            Ok(()) => {
                let info = wait_for_restart(globals)?;
                return Ok(format!(
                    "orchestra-ptyd {} restarted in place (pid {}); {} running session{} kept running",
                    info.version,
                    info.pid,
                    info.live_terminals,
                    if info.live_terminals == 1 { "" } else { "s" }
                ));
            }
            // The new binary can't take over: say why and fall back.
            Err(error) if error.code == ErrorCode::Unsupported => refused = Some(error.message),
            Err(error) => return Err(error.into()),
        }
    }
    let why = refused.unwrap_or_else(|| {
        "The running orchestra-ptyd is from an older Orchestra and can't hand its sessions \
         to a new one."
            .to_string()
    });
    drop(client);
    stop_with(globals, layout, yes, &format!("{why} Restarting it"))?;
    let info = globals.connect(true)?.status()?;
    Ok(format!(
        "orchestra-ptyd {} started (pid {})",
        info.version, info.pid
    ))
}

/// The `orchestra-ptyd` this Orchestra uses, as an absolute path.
fn ptyd_binary(globals: &Globals) -> Result<std::path::PathBuf, CliError> {
    let binary = session_core::paths::find_ptyd_binary(globals.ptyd_path.as_deref())
        .ok_or_else(|| CliError::internal("orchestra-ptyd not found next to orchestra"))?;
    std::path::absolute(&binary)
        .map_err(|error| CliError::internal(format!("{}: {error}", binary.display())))
}

/// Wait until the restarted daemon answers (the old one stopped accepting
/// before it agreed, so whoever answers is the new one).
fn wait_for_restart(globals: &Globals) -> Result<DaemonStatusInfo, CliError> {
    let deadline = Instant::now() + RESTART_WAIT;
    loop {
        let answered = globals
            .connect(false)
            .ok()
            .and_then(|client| client.status().ok());
        if let Some(info) = answered {
            return Ok(info);
        }
        if Instant::now() >= deadline {
            return Err(CliError::Api(ApiError::new(
                ErrorCode::Timeout,
                format!(
                    "orchestra-ptyd didn't answer within {} s of restarting; see `orchestra daemon logs`",
                    RESTART_WAIT.as_secs()
                ),
            )));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn status(
    globals: &Globals,
    layout: &Layout,
    json: bool,
    out: &mut dyn Write,
) -> Result<i32, CliError> {
    let client = match globals.connect(false) {
        Ok(client) => client,
        Err(CliError::Api(error))
            if error.code == ErrorCode::DaemonUnavailable && !is_running(layout) =>
        {
            let text = if json {
                json!({ "running": false }).to_string()
            } else {
                "orchestra-ptyd is not running".into()
            };
            writeln!(out, "{text}").map_err(output_error)?;
            return Ok(1);
        }
        Err(error) => return Err(error),
    };
    let info = client.status()?;
    let text = if json {
        json!({
            "running": true,
            "status": info,
            "log_file": layout.log_file(),
        })
        .to_string()
    } else {
        describe(&info, &layout.log_file())
    };
    writeln!(out, "{text}").map_err(output_error)?;
    Ok(0)
}

fn describe(info: &DaemonStatusInfo, log_file: &Path) -> String {
    let memory = match info.rss_bytes {
        Some(bytes) => format!("{:.1} MiB", bytes as f64 / (1024.0 * 1024.0)),
        None => "unknown".into(),
    };
    [
        format!(
            "orchestra-ptyd {} (protocol {})",
            info.version, info.protocol
        ),
        format!("  pid       {}", info.pid),
        format!("  uptime    {}", uptime(info.uptime_secs)),
        format!(
            "  sessions  {} ({} running)",
            info.sessions, info.live_terminals
        ),
        format!("  clients   {}", info.clients),
        format!("  memory    {memory}"),
        format!("  log       {}", log_file.display()),
    ]
    .join("\n")
}

fn uptime(secs: u64) -> String {
    match secs {
        0..=59 => format!("{secs}s"),
        60..=3599 => format!("{}m {}s", secs / 60, secs % 60),
        3600..=86_399 => format!("{}h {}m", secs / 3600, secs % 3600 / 60),
        _ => format!("{}d {}h", secs / 86_400, secs % 86_400 / 3600),
    }
}

/// Stop the daemon. Returns false when none was running.
fn stop(globals: &Globals, layout: &Layout, yes: bool) -> Result<bool, CliError> {
    stop_with(globals, layout, yes, "Stopping orchestra-ptyd")
}

/// [`stop`], asking "`what` ends N running sessions. Continue?".
fn stop_with(globals: &Globals, layout: &Layout, yes: bool, what: &str) -> Result<bool, CliError> {
    let client = match globals.connect(false) {
        Ok(client) => client,
        Err(CliError::Api(error))
            if error.code == ErrorCode::DaemonUnavailable && !is_running(layout) =>
        {
            return Ok(false);
        }
        // A daemon speaking another protocol can't be asked; signal it.
        Err(CliError::Api(error)) if error.code == ErrorCode::ProtocolMismatch => {
            terminate(layout)?;
            wait_until_stopped(layout)?;
            return Ok(true);
        }
        Err(error) => return Err(error),
    };
    let info = client.status()?;
    if info.live_terminals > 0 && !yes {
        confirm(what, info.live_terminals)?;
    }
    client.shutdown()?;
    wait_until_stopped(layout)?;
    Ok(true)
}

/// Ask before ending running sessions; without a terminal, require --yes.
fn confirm(what: &str, running: u32) -> Result<(), CliError> {
    let sessions = if running == 1 {
        "1 running session".to_string()
    } else {
        format!("{running} running sessions")
    };
    if !atty::is(atty::Stream::Stdin) {
        return Err(CliError::invalid(format!(
            "{what} ends {sessions}; pass --yes to do it anyway"
        )));
    }
    eprint!("{what} ends {sessions}. Continue? [y/N] ");
    let _ = io::stderr().flush();
    let mut answer = String::new();
    io::stdin()
        .lock()
        .read_line(&mut answer)
        .map_err(|error| CliError::internal(format!("cannot read the answer: {error}")))?;
    match answer.trim().to_ascii_lowercase().as_str() {
        "y" | "yes" => Ok(()),
        _ => {
            eprintln!("Aborted.");
            Err(CliError::Exit(1))
        }
    }
}

/// SIGTERM the registered daemon (graceful shutdown on its side).
fn terminate(layout: &Layout) -> Result<(), CliError> {
    let registration = Registration::load(layout).map_err(|error| {
        CliError::internal(format!(
            "cannot read {}: {error}",
            layout.registration_file().display()
        ))
    })?;
    let status = std::process::Command::new("kill")
        .arg("-TERM")
        .arg(registration.pid.to_string())
        .status()
        .map_err(|error| CliError::internal(format!("cannot run kill: {error}")))?;
    if status.success() {
        Ok(())
    } else {
        Err(CliError::internal(format!(
            "cannot signal orchestra-ptyd (pid {})",
            registration.pid
        )))
    }
}

fn wait_until_stopped(layout: &Layout) -> Result<(), CliError> {
    let deadline = Instant::now() + STOP_WAIT;
    while is_running(layout) {
        if Instant::now() >= deadline {
            return Err(CliError::Api(ApiError::new(
                ErrorCode::Timeout,
                format!(
                    "orchestra-ptyd is still running after {} s",
                    STOP_WAIT.as_secs()
                ),
            )));
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Ok(())
}

/// Print the last `lines` lines of the log, then (with `follow`) new lines
/// as they arrive, across rotations.
fn logs(path: &Path, lines: usize, follow: bool, out: &mut dyn Write) -> Result<i32, CliError> {
    let mut file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound && !follow => {
            return Err(CliError::Api(ApiError::new(
                ErrorCode::NotFound,
                format!("no daemon log at {}", path.display()),
            )))
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return follow_from(path, 0, out);
        }
        Err(error) => {
            return Err(CliError::internal(format!(
                "cannot read {}: {error}",
                path.display()
            )))
        }
    };
    let mut content = Vec::new();
    file.read_to_end(&mut content)
        .map_err(|error| CliError::internal(format!("cannot read {}: {error}", path.display())))?;
    out.write_all(tail(&content, lines)).map_err(output_error)?;
    out.flush().map_err(output_error)?;
    if follow {
        return follow_from(path, content.len() as u64, out);
    }
    Ok(0)
}

/// The last `lines` lines of `content`.
fn tail(content: &[u8], lines: usize) -> &[u8] {
    if lines == 0 {
        return &[];
    }
    let body = content.strip_suffix(b"\n").unwrap_or(content);
    let start = body
        .iter()
        .enumerate()
        .rev()
        .filter(|(_, b)| **b == b'\n')
        .nth(lines - 1)
        .map(|(i, _)| i + 1)
        .unwrap_or(0);
    &content[start..]
}

fn follow_from(path: &Path, mut position: u64, out: &mut dyn Write) -> Result<i32, CliError> {
    loop {
        std::thread::sleep(FOLLOW_POLL);
        let Ok(meta) = std::fs::metadata(path) else {
            continue;
        };
        if meta.len() < position {
            // Rotated: the log starts over in a new file.
            position = 0;
        }
        if meta.len() == position {
            continue;
        }
        let Ok(mut file) = std::fs::File::open(path) else {
            continue;
        };
        let mut chunk = Vec::new();
        file.seek(SeekFrom::Start(position))
            .and_then(|_| file.read_to_end(&mut chunk))
            .map_err(|error| {
                CliError::internal(format!("cannot read {}: {error}", path.display()))
            })?;
        position += chunk.len() as u64;
        out.write_all(&chunk).map_err(output_error)?;
        out.flush().map_err(output_error)?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tail_returns_the_last_lines() {
        let log = b"a\nb\nc\nd\n";
        assert_eq!(tail(log, 2), b"c\nd\n");
        assert_eq!(tail(log, 10), log);
        assert_eq!(tail(log, 0), b"");
        assert_eq!(tail(b"x\ny", 1), b"y");
        assert_eq!(tail(b"", 3), b"");
    }

    #[test]
    fn logs_print_the_tail_or_explain_a_missing_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ptyd.log");
        let error = logs(&path, 5, false, &mut Vec::new()).unwrap_err();
        assert!(matches!(error, CliError::Api(e) if e.code == ErrorCode::NotFound));
        std::fs::write(&path, "one\ntwo\nthree\n").unwrap();
        let mut out = Vec::new();
        assert_eq!(logs(&path, 2, false, &mut out).unwrap(), 0);
        assert_eq!(out, b"two\nthree\n");
    }

    #[test]
    fn status_text_is_readable() {
        let info = DaemonStatusInfo {
            pid: 42,
            version: "0.6.0".into(),
            protocol: 1,
            uptime_secs: 3725,
            sessions: 3,
            live_terminals: 2,
            clients: 1,
            rss_bytes: Some(5 * 1024 * 1024),
        };
        let text = describe(&info, Path::new("/h/logs/ptyd.log"));
        assert!(text.starts_with("orchestra-ptyd 0.6.0 (protocol 1)\n"));
        assert!(text.contains("pid       42"));
        assert!(text.contains("uptime    1h 2m"));
        assert!(text.contains("sessions  3 (2 running)"));
        assert!(text.contains("memory    5.0 MiB"));
        assert!(text.ends_with("log       /h/logs/ptyd.log"));
        assert_eq!(uptime(59), "59s");
        assert_eq!(uptime(90_000), "1d 1h");
    }

    #[test]
    fn nothing_runs_in_an_empty_home() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!is_running(&Layout::with_home(dir.path().to_path_buf())));
    }
}
