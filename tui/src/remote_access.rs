//! Remote access for the Orchestra app on a phone (`plans/mobile-client.md`),
//! shared by the `orchestra remote` CLI and the TUI (the one-time offer and
//! Settings' Phone access row).
//!
//! The settings live in `<home>/remote.json` (`session_core::remote`);
//! `orchestra-api` reads them when it starts, so turning remote access on or
//! off, or rotating the token, restarts the API: SIGTERM to the pid in
//! `api.json` (only when that process is `orchestra-api`), wait for it to go,
//! then `orchestra-api --detach`. The TUI starts the API at startup when
//! remote access is on and none runs ([`autostart`]).

use serde::Deserialize;
use session_core::paths::Layout;
use session_core::remote::{is_tailscale, RemoteConfig};
use session_core::tailscale;
use std::io;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Environment variable naming the `orchestra-api` binary (as for
/// `orchestra-mcp`).
const API_BIN: &str = "ORCHESTRA_API_BIN";
/// How long a stopping API may take to exit.
const API_STOP_WAIT: Duration = Duration::from_secs(10);

/// What a phone needs to connect: the remote listener's URLs (the MagicDNS
/// one first) and the token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pairing {
    /// Empty when the API found no address to listen on (no Tailscale).
    pub urls: Vec<String>,
    pub token: String,
}

impl Pairing {
    /// `orchestra://connect?url=…&token=…` for the first URL: what the QR
    /// code holds and the app reads.
    pub fn link(&self) -> Option<String> {
        self.urls.first().map(|url| deep_link(url, &self.token))
    }
}

/// Turn remote access on (saved in `remote.json`, a new token if there is
/// none; `port` when given) and restart the API so it serves the remote
/// listener.
pub fn enable(
    layout: &Layout,
    ptyd_path: Option<&Path>,
    port: Option<u16>,
) -> Result<Pairing, String> {
    let (mut config, _) = load_or_new(layout)?;
    config.enabled = true;
    if let Some(port) = port {
        config.port = port;
    }
    save(&config, layout)?;
    stop_api(layout)?;
    let api = start_api(layout, ptyd_path)?;
    Ok(Pairing {
        urls: connect_urls(&api.remote_urls, magic_dns_name),
        token: config.token,
    })
}

/// Turn remote access off and restart a running API without the remote
/// listener. `Ok(false)`: it was off already, or no API ran.
pub fn disable(layout: &Layout, ptyd_path: Option<&Path>) -> Result<bool, String> {
    let Some(mut config) = load(layout)? else {
        return Ok(false);
    };
    config.enabled = false;
    save(&config, layout)?;
    if stop_api(layout)? {
        start_api(layout, ptyd_path)?;
        return Ok(true);
    }
    Ok(false)
}

/// Make a new token (paired apps need it) and restart the API when remote
/// access is on. Returns the settings with the new token.
pub fn rotate(layout: &Layout, ptyd_path: Option<&Path>) -> Result<RemoteConfig, String> {
    let (mut config, _) = load_or_new(layout)?;
    config
        .rotate_token()
        .map_err(|error| format!("cannot make a token: {error}"))?;
    save(&config, layout)?;
    if config.enabled && stop_api(layout)? {
        start_api(layout, ptyd_path)?;
    }
    Ok(config)
}

/// What to pair a phone with while remote access is on (`None`: it's off).
/// Starts the API when none runs, and restarts one that has no remote
/// listener while this machine now has a Tailscale address (Tailscale came
/// up after the API started).
pub fn pairing(layout: &Layout, ptyd_path: Option<&Path>) -> Result<Option<Pairing>, String> {
    let Some(config) = load(layout)?.filter(|config| config.enabled) else {
        return Ok(None);
    };
    let api = match running_api(layout) {
        Some(api) if !api.remote_urls.is_empty() => api,
        Some(_) if !tailscale::addresses().unwrap_or_default().is_empty() => {
            stop_api(layout)?;
            start_api(layout, ptyd_path)?
        }
        Some(api) => api,
        None => start_api(layout, ptyd_path)?,
    };
    Ok(Some(Pairing {
        urls: connect_urls(&api.remote_urls, magic_dns_name),
        token: config.token,
    }))
}

/// Whether remote access is on (`remote.json` `enabled`).
pub fn is_enabled(layout: &Layout) -> bool {
    load(layout)
        .ok()
        .flatten()
        .is_some_and(|config| config.enabled)
}

/// Whether to offer phone access: it was never set up (no `remote.json`;
/// once the user turned it on or off, they decided) and Tailscale runs on
/// this machine.
pub fn worth_offering(layout: &Layout) -> bool {
    matches!(RemoteConfig::load(layout), Ok(None))
        && !tailscale::addresses().unwrap_or_default().is_empty()
}

// ---------------------------------------------------------------- remote.json

pub(crate) fn load(layout: &Layout) -> Result<Option<RemoteConfig>, String> {
    RemoteConfig::load(layout)
        .map_err(|error| format!("cannot read {}: {error}", layout.remote_file().display()))
}

/// `remote.json`, or new settings (off, a fresh token) when there is none;
/// `true` when they are new or got a new token (not saved yet).
pub(crate) fn load_or_new(layout: &Layout) -> Result<(RemoteConfig, bool), String> {
    let token_error = |error: io::Error| format!("cannot make a token: {error}");
    match load(layout)? {
        Some(config) if config.has_valid_token() => Ok((config, false)),
        Some(mut config) => {
            config.rotate_token().map_err(token_error)?;
            Ok((config, true))
        }
        None => Ok((RemoteConfig::new().map_err(token_error)?, true)),
    }
}

pub(crate) fn save(config: &RemoteConfig, layout: &Layout) -> Result<(), String> {
    config
        .save(layout)
        .map_err(|error| format!("cannot write {}: {error}", layout.remote_file().display()))
}

// ---------------------------------------------------------------- the API

/// The parts of `api.json` remote access needs. `orchestra-api` writes the
/// file (`orchestra_api::discovery::ApiInfo`, which the `tui` crate can't
/// use).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct RunningApi {
    pub url: String,
    pub pid: u32,
    #[serde(default)]
    pub remote_urls: Vec<String>,
}

pub(crate) fn api_file(layout: &Layout) -> PathBuf {
    layout.home.join("api.json")
}

pub(crate) fn api_log(layout: &Layout) -> PathBuf {
    layout.home.join("logs").join("api.log")
}

/// The API `api.json` names, when that process runs and is `orchestra-api`.
pub(crate) fn running_api(layout: &Layout) -> Option<RunningApi> {
    let raw = std::fs::read(api_file(layout)).ok()?;
    let api: RunningApi = serde_json::from_slice(&raw).ok()?;
    is_orchestra_api(api.pid).then_some(api)
}

/// Stop the running API: SIGTERM to the pid in `api.json`, only when that
/// process is `orchestra-api`; then wait for it to exit. Returns whether
/// one ran.
pub(crate) fn stop_api(layout: &Layout) -> Result<bool, String> {
    let Some(api) = running_api(layout) else {
        return Ok(false);
    };
    // SAFETY: kill(2) on a pid that was just checked to be orchestra-api.
    if unsafe { libc::kill(api.pid as libc::pid_t, libc::SIGTERM) } != 0 {
        let error = io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::ESRCH) {
            return Ok(false);
        }
        return Err(format!(
            "cannot stop orchestra-api (pid {}): {error}",
            api.pid
        ));
    }
    let deadline = Instant::now() + API_STOP_WAIT;
    while is_orchestra_api(api.pid) {
        if Instant::now() >= deadline {
            return Err(format!(
                "orchestra-api (pid {}) is still running after {} s",
                api.pid,
                API_STOP_WAIT.as_secs()
            ));
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Ok(true)
}

/// `orchestra-api --detach`: start the API (it returns once the API
/// answers) and read the `api.json` it wrote.
pub(crate) fn start_api(layout: &Layout, ptyd_path: Option<&Path>) -> Result<RunningApi, String> {
    let binary = api_binary().ok_or_else(|| {
        format!("orchestra-api not found next to orchestra, on PATH or in {API_BIN}")
    })?;
    let mut command = detach_command(&binary, ptyd_path)?;
    let output = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|error| format!("cannot run {}: {error}", binary.display()))?;
    if !output.status.success() {
        return Err(format!(
            "{} --detach failed: {}",
            binary.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    running_api(layout).ok_or_else(|| {
        format!(
            "orchestra-api started but {} doesn't name it; see {}",
            api_file(layout).display(),
            api_log(layout).display()
        )
    })
}

/// `<binary> --detach [--ptyd-path P]`, without this process's session
/// identity (the API acts for whoever calls it) and with stdin closed.
fn detach_command(binary: &Path, ptyd_path: Option<&Path>) -> Result<Command, String> {
    let mut command = Command::new(binary);
    command.arg("--detach").stdin(Stdio::null());
    if let Some(path) = ptyd_path {
        let path =
            std::path::absolute(path).map_err(|error| format!("{}: {error}", path.display()))?;
        command.arg("--ptyd-path").arg(path);
    }
    for name in [
        session_core::env::SESSION_ID,
        session_core::env::SESSION_TOKEN,
        session_core::env::SOCKET,
    ] {
        command.env_remove(name);
    }
    Ok(command)
}

/// `$ORCHESTRA_API_BIN`, else `orchestra-api` next to this executable
/// (symlinks resolved, then as invoked), else on `PATH`.
fn api_binary() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os(API_BIN).filter(|value| !value.is_empty()) {
        let path = PathBuf::from(path);
        return is_executable(&path).then_some(path);
    }
    let exe = std::env::current_exe().ok();
    let siblings = [
        exe.as_ref().and_then(|exe| std::fs::canonicalize(exe).ok()),
        exe.clone(),
    ];
    for exe in siblings.into_iter().flatten() {
        if let Some(candidate) = exe.parent().map(|dir| dir.join("orchestra-api")) {
            if is_executable(&candidate) {
                return Some(candidate);
            }
        }
    }
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|dir| dir.join("orchestra-api"))
            .find(|candidate| is_executable(candidate))
    })
}

fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .map(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

/// Whether `pid` is a running process named `orchestra-api`.
fn is_orchestra_api(pid: u32) -> bool {
    pid != 0
        && pid <= i32::MAX as u32
        && process_name(pid).is_some_and(|name| name == "orchestra-api")
}

/// The process's name (Linux: `/proc/<pid>/comm`).
#[cfg(target_os = "linux")]
fn process_name(pid: u32) -> Option<String> {
    let comm = std::fs::read_to_string(format!("/proc/{pid}/comm")).ok()?;
    Some(comm.trim_end_matches('\n').to_owned())
}

/// The process's name (macOS: `proc_name`).
#[cfg(target_os = "macos")]
fn process_name(pid: u32) -> Option<String> {
    let mut buffer = [0u8; 256];
    // SAFETY: proc_name writes at most `buffer.len()` bytes into `buffer`.
    let len = unsafe {
        libc::proc_name(
            pid as libc::c_int,
            buffer.as_mut_ptr().cast(),
            buffer.len() as u32,
        )
    };
    (len > 0).then(|| String::from_utf8_lossy(&buffer[..len as usize]).into_owned())
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn process_name(_pid: u32) -> Option<String> {
    None
}

// ---------------------------------------------------------------- TUI autostart

/// At TUI startup: when remote access is on and no API runs, start one in
/// the background (so remote access comes back after a reboot once
/// Orchestra is opened). Never blocks and never fails the TUI.
pub fn autostart(layout: &Layout, ptyd_path: Option<PathBuf>) {
    if !is_enabled(layout) || running_api(layout).is_some() {
        return;
    }
    let _ = std::thread::Builder::new()
        .name("remote-api".into())
        .spawn(move || {
            let Some(binary) = api_binary() else {
                return;
            };
            if let Ok(mut command) = detach_command(&binary, ptyd_path.as_deref()) {
                // `--detach` waits until the API answers; nobody reads its
                // output.
                let _ = command.stdout(Stdio::null()).stderr(Stdio::null()).status();
            }
        });
}

// ---------------------------------------------------------------- the TUI's port

/// What the TUI does with remote access (the one-time offer, Settings'
/// Phone access row and the pairing dialog); blocking, so the TUI runs it
/// off the UI thread. Tests use a fake.
pub trait PhoneAccess: Send + Sync {
    /// Remote access is on.
    fn enabled(&self) -> bool;
    /// See [`worth_offering`].
    fn worth_offering(&self) -> bool;
    /// See [`enable`].
    fn enable(&self) -> Result<Pairing, String>;
    /// See [`pairing`].
    fn pairing(&self) -> Result<Option<Pairing>, String>;
    /// See [`disable`].
    fn disable(&self) -> Result<(), String>;
}

/// Remote access of this machine's Orchestra home.
pub struct LocalPhoneAccess {
    pub layout: Layout,
    pub ptyd_path: Option<PathBuf>,
}

impl PhoneAccess for LocalPhoneAccess {
    fn enabled(&self) -> bool {
        is_enabled(&self.layout)
    }

    fn worth_offering(&self) -> bool {
        worth_offering(&self.layout)
    }

    fn enable(&self) -> Result<Pairing, String> {
        enable(&self.layout, self.ptyd_path.as_deref(), None)
    }

    fn pairing(&self) -> Result<Option<Pairing>, String> {
        pairing(&self.layout, self.ptyd_path.as_deref())
    }

    fn disable(&self) -> Result<(), String> {
        disable(&self.layout, self.ptyd_path.as_deref()).map(|_| ())
    }
}

// ---------------------------------------------------------------- URLs

/// The URLs to show for the remote listener's `remote_urls`: when one is a
/// Tailscale address and `magic_dns` names this machine,
/// `http://<name>:<port>` first; then every URL.
pub(crate) fn connect_urls(
    remote_urls: &[String],
    magic_dns: impl FnOnce() -> Option<String>,
) -> Vec<String> {
    let tailscale_port = remote_urls.iter().find_map(|url| {
        let addr: SocketAddr = url.strip_prefix("http://")?.parse().ok()?;
        is_tailscale(addr.ip()).then_some(addr.port())
    });
    let mut urls = Vec::new();
    if let Some(port) = tailscale_port {
        if let Some(name) = magic_dns() {
            urls.push(format!("http://{name}:{port}"));
        }
    }
    urls.extend(remote_urls.iter().cloned());
    urls
}

/// This machine's MagicDNS name, from `tailscale status --json` when the
/// `tailscale` CLI is there and answers in time. Optional: never required.
fn magic_dns_name() -> Option<String> {
    tailscale::status().and_then(|status| status.dns_name)
}

/// `orchestra://connect?url=<url>&token=<token>`, values percent-encoded.
pub(crate) fn deep_link(url: &str, token: &str) -> String {
    format!(
        "orchestra://connect?url={}&token={}",
        percent_encode(url),
        percent_encode(token)
    )
}

/// Percent-encode everything but RFC 3986's unreserved characters.
fn percent_encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn deep_links_encode_their_values() {
        assert_eq!(
            deep_link(
                "http://box.tail1234.ts.net:7337",
                "abcdefghijklmnopqrstuvwx27"
            ),
            "orchestra://connect?url=http%3A%2F%2Fbox.tail1234.ts.net%3A7337\
             &token=abcdefghijklmnopqrstuvwx27"
        );
        assert_eq!(
            percent_encode("http://[fd7a:115c:a1e0::1]:7337/?a=b&c d"),
            "http%3A%2F%2F%5Bfd7a%3A115c%3Aa1e0%3A%3A1%5D%3A7337%2F%3Fa%3Db%26c%20d"
        );
        assert_eq!(percent_encode("A-z_0.9~"), "A-z_0.9~");
        let pairing = Pairing {
            urls: vec!["http://100.64.0.1:7337".into()],
            token: "abcdefghijklmnopqrstuvwx27".into(),
        };
        assert_eq!(
            pairing.link().as_deref(),
            Some("orchestra://connect?url=http%3A%2F%2F100.64.0.1%3A7337&token=abcdefghijklmnopqrstuvwx27")
        );
        let nowhere = Pairing {
            urls: vec![],
            ..pairing
        };
        assert_eq!(nowhere.link(), None);
    }

    #[test]
    fn magic_dns_comes_first_for_tailscale_addresses() {
        let tailnet = vec![
            "http://100.88.129.91:7337".to_string(),
            "http://[fd7a:115c:a1e0::1]:7337".to_string(),
        ];
        assert_eq!(
            connect_urls(&tailnet, || Some("box.tail1234.ts.net".into())),
            vec![
                "http://box.tail1234.ts.net:7337".to_string(),
                tailnet[0].clone(),
                tailnet[1].clone(),
            ]
        );
        assert_eq!(connect_urls(&tailnet, || None), tailnet);
        // Not a Tailscale address: tailscale is never asked.
        let local = vec!["http://127.0.0.1:7400".to_string()];
        assert_eq!(
            connect_urls(&local, || panic!("tailscale was asked")),
            local
        );
        assert!(connect_urls(&[], || panic!("tailscale was asked")).is_empty());
    }

    #[test]
    fn only_orchestra_api_processes_count() {
        assert!(!is_orchestra_api(0));
        assert!(!is_orchestra_api(u32::MAX));
        // This test binary is not orchestra-api.
        assert!(!is_orchestra_api(std::process::id()));
        if cfg!(any(target_os = "linux", target_os = "macos")) {
            assert!(process_name(std::process::id()).is_some_and(|name| !name.is_empty()));
        }
    }

    #[test]
    fn api_json_without_a_running_api_is_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let layout = Layout::with_home(dir.path().to_path_buf());
        assert_eq!(running_api(&layout), None);
        // A live pid that isn't orchestra-api (this test) doesn't count, so
        // `stop_api` never signals it.
        let json = json!({"url": "http://127.0.0.1:1", "token": "t",
                          "pid": std::process::id(), "version": "0"});
        std::fs::write(api_file(&layout), json.to_string()).unwrap();
        assert_eq!(running_api(&layout), None);
        assert!(!stop_api(&layout).unwrap());
    }

    #[test]
    fn remote_json_is_created_with_a_token_when_missing() {
        let dir = tempfile::tempdir().unwrap();
        let layout = Layout::with_home(dir.path().to_path_buf());
        let (config, created) = load_or_new(&layout).unwrap();
        assert!(created);
        assert!(!config.enabled);
        assert!(config.has_valid_token());
        // A hand-edited token that isn't one is replaced.
        let broken = RemoteConfig {
            token: "short".into(),
            enabled: true,
            ..config
        };
        broken.save(&layout).unwrap();
        let (fixed, created) = load_or_new(&layout).unwrap();
        assert!(created);
        assert!(fixed.enabled);
        assert!(fixed.has_valid_token());
        fixed.save(&layout).unwrap();
        let (again, created) = load_or_new(&layout).unwrap();
        assert!(!created);
        assert_eq!(again, fixed);
    }

    #[test]
    fn phone_access_is_offered_only_before_it_was_ever_set_up() {
        let dir = tempfile::tempdir().unwrap();
        let layout = Layout::with_home(dir.path().to_path_buf());
        let tailscale_here = !tailscale::addresses().unwrap_or_default().is_empty();
        assert_eq!(worth_offering(&layout), tailscale_here);
        assert!(!is_enabled(&layout));
        // Turned off (or only a token printed): the user decided.
        RemoteConfig::new().unwrap().save(&layout).unwrap();
        assert!(!worth_offering(&layout));
        assert!(!is_enabled(&layout));
        // Off: no pairing, and nothing is started.
        assert_eq!(pairing(&layout, None).unwrap(), None);
        assert!(!disable(&layout, None).unwrap());
    }
}
