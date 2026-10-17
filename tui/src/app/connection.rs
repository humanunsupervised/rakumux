//! Connecting to the daemon: the client configuration the TUI uses, the
//! connection state shown in the UI, and stopping a daemon that speaks
//! another protocol version (after the user consents).

use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};

use session_core::api::{ApiError, ErrorCode};
use session_core::client::{AuthMode, Client, ClientConfig, SessionApi};
use session_core::paths::{Layout, Registration};

/// Opens a connection to the daemon (spawning it when needed). Tests
/// substitute an in-memory fake.
pub type Connector = Arc<dyn Fn() -> Result<Arc<dyn SessionApi>, ApiError> + Send + Sync>;

/// The TUI is a user client: it authenticates with the registration-file
/// token even when started inside a session (a session caller may not
/// attach), and starts the daemon if none is running.
pub fn client_config(layout: Layout, ptyd_path: Option<PathBuf>) -> ClientConfig {
    ClientConfig {
        layout,
        client_name: "tui".into(),
        version: super::app_version().into(),
        auth: AuthMode::User,
        spawn: true,
        ptyd_path,
    }
}

pub fn client_connector(config: ClientConfig) -> Connector {
    Arc::new(move || {
        Client::connect(config.clone()).map(|client| Arc::new(client) as Arc<dyn SessionApi>)
    })
}

/// Connection to the daemon as the UI sees it.
#[derive(Clone)]
pub enum Connection {
    Connecting,
    Connected(Arc<dyn SessionApi>),
    Failed(ApiError),
}

impl Connection {
    pub fn api(&self) -> Option<Arc<dyn SessionApi>> {
        match self {
            Connection::Connected(api) => Some(api.clone()),
            _ => None,
        }
    }

    pub fn is_connected(&self) -> bool {
        matches!(self, Connection::Connected(_))
    }
}

/// Whether a failed connect should offer to restart the daemon.
pub fn is_protocol_mismatch(error: &ApiError) -> bool {
    error.code == ErrorCode::ProtocolMismatch
}

/// Version of the running daemon, from its registration file (or a legacy
/// one, `Layout::legacy`).
pub fn daemon_version(layout: &Layout) -> Option<String> {
    std::iter::once(layout.clone())
        .chain(layout.legacy())
        .find_map(|layout| Registration::load(&layout).ok())
        .map(|r| r.version)
}

/// Stop the registered daemon (the one in `layout`, else one an earlier
/// build registered in a legacy layout): SIGTERM, then SIGKILL after 5 s.
/// Refuses to signal a process that isn't `orchestra-ptyd` (stale
/// registration with a reused pid).
pub fn stop_daemon(layout: &Layout) -> Result<(), String> {
    let running = std::iter::once(layout.clone())
        .chain(layout.legacy())
        .find(|layout| {
            Registration::load(layout)
                .map(|r| process_alive(r.pid))
                .unwrap_or(false)
        });
    stop_registered(running.as_ref().unwrap_or(layout))
}

fn stop_registered(layout: &Layout) -> Result<(), String> {
    let registration = match Registration::load(layout) {
        Ok(registration) => registration,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(err) => {
            return Err(format!(
                "can't read {}: {err}",
                layout.registration_file().display()
            ))
        }
    };
    let pid = registration.pid;
    if !process_alive(pid) {
        return Ok(());
    }
    let name = process_name(pid)?;
    if !name.contains("orchestra-ptyd") {
        return Err(format!(
            "process {pid} from the registration file is '{name}', not orchestra-ptyd; \
             stop the old daemon manually"
        ));
    }
    signal(pid, "TERM")?;
    if wait_for_exit(pid, Duration::from_secs(5)) {
        return Ok(());
    }
    signal(pid, "KILL")?;
    if wait_for_exit(pid, Duration::from_secs(1)) {
        Ok(())
    } else {
        Err(format!("orchestra-ptyd (pid {pid}) did not exit"))
    }
}

fn process_alive(pid: u32) -> bool {
    Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn process_name(pid: u32) -> Result<String, String> {
    let output = Command::new("ps")
        .args(["-o", "comm=", "-p", &pid.to_string()])
        .output()
        .map_err(|e| format!("can't run ps: {e}"))?;
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn signal(pid: u32, name: &str) -> Result<(), String> {
    let status = Command::new("kill")
        .args([format!("-{name}"), pid.to_string()])
        .status()
        .map_err(|e| format!("can't run kill: {e}"))?;
    if status.success() || !process_alive(pid) {
        Ok(())
    } else {
        Err(format!("kill -{name} {pid} failed"))
    }
}

fn wait_for_exit(pid: u32, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if !process_alive(pid) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    !process_alive(pid)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tui_client_is_a_spawning_user_client() {
        let layout = Layout::with_home("/tmp/orchestra-test-home".into());
        let config = client_config(layout.clone(), Some("/opt/ptyd".into()));
        assert_eq!(config.client_name, "tui");
        assert_eq!(config.auth, AuthMode::User);
        assert!(config.spawn);
        assert_eq!(config.layout, layout);
        assert_eq!(config.ptyd_path, Some(PathBuf::from("/opt/ptyd")));
    }

    #[test]
    fn stopping_without_a_registration_is_a_no_op() {
        let home = tempfile::tempdir().unwrap();
        let layout = Layout::with_home(home.path().to_path_buf());
        assert_eq!(stop_daemon(&layout), Ok(()));
        assert_eq!(daemon_version(&layout), None);
    }

    #[test]
    fn refuses_to_signal_a_process_that_is_not_the_daemon() {
        let home = tempfile::tempdir().unwrap();
        let layout = Layout::with_home(home.path().to_path_buf());
        std::fs::create_dir_all(&layout.run_dir).unwrap();
        let registration = Registration {
            instance_id: "i".into(),
            pid: std::process::id(),
            protocol: 0,
            version: "0.5.0".into(),
            socket: "/tmp/none.sock".into(),
            token: "t".into(),
        };
        std::fs::write(
            layout.registration_file(),
            serde_json::to_vec(&registration).unwrap(),
        )
        .unwrap();
        assert_eq!(daemon_version(&layout).as_deref(), Some("0.5.0"));
        let err = stop_daemon(&layout).unwrap_err();
        assert!(err.contains("not orchestra-ptyd"), "{err}");
    }
}
