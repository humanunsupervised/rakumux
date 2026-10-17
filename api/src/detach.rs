//! `orchestra-api --detach`: start the API in the background (new session,
//! stdio detached, log in `logs/api.log`) and return once `api.json` is
//! written and the API answers `GET /v1/health`. When a healthy API already
//! runs for this home, return that one instead.

use crate::discovery::{self, ApiInfo};
use crate::probe;
use session_core::paths::Layout;
use std::fs::OpenOptions;
use std::io;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// How long the background API may take to become healthy.
pub const START_TIMEOUT: Duration = Duration::from_secs(10);
const POLL: Duration = Duration::from_millis(50);
const PROBE_TIMEOUT: Duration = Duration::from_secs(2);
/// The log is rotated (to `api.log.1`) when it is larger than this at start.
const MAX_LOG_BYTES: u64 = 1024 * 1024;

/// A healthy API for `layout`, if one runs: its `api.json` names a live
/// process that answers `/v1/health` with that pid.
pub fn running(layout: &Layout) -> Option<ApiInfo> {
    let info = ApiInfo::load(layout).ok()?;
    if !info.process_alive() {
        return None;
    }
    let health = probe::health(&info.url, PROBE_TIMEOUT).ok()?;
    (health.pid == info.pid).then_some(info)
}

/// Start a background API (unless one is healthy) and wait for it.
/// `remote_listen` is passed on as `--remote-listen`.
pub fn start(
    layout: &Layout,
    port: u16,
    ptyd_path: Option<&Path>,
    remote_listen: &[SocketAddr],
) -> Result<ApiInfo, String> {
    if let Some(info) = running(layout) {
        return Ok(info);
    }
    let exe = std::env::current_exe().map_err(|e| format!("cannot find orchestra-api: {e}"))?;
    let log_path = discovery::log_path(layout);
    let log =
        open_log(&log_path).map_err(|e| format!("cannot open {}: {e}", log_path.display()))?;
    let mut command = Command::new(exe);
    command.arg("--port").arg(port.to_string());
    for addr in remote_listen {
        command.arg("--remote-listen").arg(addr.to_string());
    }
    if let Some(path) = ptyd_path {
        let path = std::path::absolute(path).map_err(|e| format!("{}: {e}", path.display()))?;
        command.arg("--ptyd-path").arg(path);
    }
    // The child runs in `/`: a relative ORCHESTRA_HOME would move.
    if let Some(home) = std::env::var_os(session_core::env::HOME) {
        let home = std::path::absolute(PathBuf::from(home)).map_err(|e| e.to_string())?;
        command.env(session_core::env::HOME, home);
    }
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(log);
    let mut child =
        spawn_detached(command).map_err(|e| format!("cannot start orchestra-api: {e}"))?;
    wait_healthy(layout, &mut child, &log_path)
}

fn wait_healthy(layout: &Layout, child: &mut Child, log_path: &Path) -> Result<ApiInfo, String> {
    let deadline = Instant::now() + START_TIMEOUT;
    loop {
        if let Ok(info) = ApiInfo::load(layout) {
            if info.pid == child.id() || info.process_alive() {
                if let Ok(health) = probe::health(&info.url, PROBE_TIMEOUT) {
                    if health.pid == info.pid {
                        return Ok(info);
                    }
                }
            }
        }
        if let Ok(Some(status)) = child.try_wait() {
            // Another API may have won the race for this home.
            if let Some(info) = running(layout) {
                return Ok(info);
            }
            return Err(format!(
                "orchestra-api exited ({status}) before it was ready; see {}",
                log_path.display()
            ));
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "orchestra-api did not answer within {} s; see {}",
                START_TIMEOUT.as_secs(),
                log_path.display()
            ));
        }
        std::thread::sleep(POLL);
    }
}

fn open_log(path: &Path) -> io::Result<std::fs::File> {
    if let Some(dir) = path.parent() {
        discovery::ensure_private_dir(dir)?;
    }
    if std::fs::metadata(path).is_ok_and(|meta| meta.len() > MAX_LOG_BYTES) {
        let mut rotated = path.as_os_str().to_owned();
        rotated.push(".1");
        std::fs::rename(path, PathBuf::from(rotated))?;
    }
    use std::os::unix::fs::OpenOptionsExt;
    OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(path)
}

/// Spawn in a new session (`setsid`) with cwd `/`, so the API outlives the
/// terminal or agent that started it.
fn spawn_detached(mut command: Command) -> io::Result<Child> {
    use std::os::unix::process::CommandExt;
    command.current_dir("/");
    // The API acts for whoever sends identity headers, never for the
    // session that happened to start it.
    for name in [
        session_core::env::SESSION_ID,
        session_core::env::SESSION_TOKEN,
        session_core::env::SOCKET,
    ] {
        command.env_remove(name);
    }
    // SAFETY: `setsid` is async-signal-safe and touches no Rust state.
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    command.spawn()
}
