//! `api.json`: how clients find and authenticate to a running `rakumux-api`.
//!
//! ```text
//! <RAKUMUX_HOME or ~/.rakumux>/
//!   api.json    {url, token, pid, version, remote_urls?}, mode 0600, written
//!               atomically
//!   api.lock    flock held by the running API (see `instance`)
//!   logs/api.log
//! ```
//!
//! The API writes `api.json` once it listens and removes it on a clean exit.
//! A file whose `pid` is gone is stale: readers ignore it and a new API
//! replaces it.

use serde::{Deserialize, Serialize};
use session_core::paths::Layout;
use std::fs::OpenOptions;
use std::io::{self, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

/// Contents of `api.json`.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApiInfo {
    /// Base URL, e.g. `http://127.0.0.1:39123` (no trailing slash).
    pub url: String,
    /// Bearer token every request except `GET /v1/health` must carry.
    pub token: String,
    /// The API process.
    pub pid: u32,
    /// The API's version.
    pub version: String,
    /// What the remote listener bound (`http://<addr>:<port>`), when remote
    /// access is on (`remote.json`); absent otherwise.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub remote_urls: Vec<String>,
}

impl std::fmt::Debug for ApiInfo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ApiInfo")
            .field("url", &self.url)
            .field("token", &"…")
            .field("pid", &self.pid)
            .field("version", &self.version)
            .field("remote_urls", &self.remote_urls)
            .finish()
    }
}

impl ApiInfo {
    /// Read `api.json` from `layout`.
    pub fn load(layout: &Layout) -> io::Result<Self> {
        let raw = std::fs::read(info_path(layout))?;
        serde_json::from_slice(&raw).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
    }

    /// Whether the process that wrote the file still exists.
    pub fn process_alive(&self) -> bool {
        pid_alive(self.pid)
    }
}

/// `<home>/api.json`.
pub fn info_path(layout: &Layout) -> PathBuf {
    layout.home.join("api.json")
}

/// `<home>/api.lock`.
pub fn lock_path(layout: &Layout) -> PathBuf {
    layout.home.join("api.lock")
}

/// `<home>/logs/api.log`, where a detached API writes its log.
pub fn log_path(layout: &Layout) -> PathBuf {
    layout.home.join("logs").join("api.log")
}

/// Create `dir` (and parents) with mode 0700 when it doesn't exist.
pub fn ensure_private_dir(dir: &Path) -> io::Result<()> {
    if dir.is_dir() {
        return Ok(());
    }
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
}

/// Write `api.json` atomically (temp file + rename) with mode 0600.
pub fn write(layout: &Layout, info: &ApiInfo) -> io::Result<()> {
    ensure_private_dir(&layout.home)?;
    let bytes = serde_json::to_vec_pretty(info)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    write_atomic(&info_path(layout), &bytes)
}

/// Remove `api.json` if it still describes the API holding `token`; returns
/// whether it was removed. A file written by another instance is left alone.
pub fn remove_if_owned(layout: &Layout, token: &str) -> io::Result<bool> {
    match ApiInfo::load(layout) {
        Ok(info) if info.token == token => {
            std::fs::remove_file(info_path(layout))?;
            Ok(true)
        }
        Ok(_) => Ok(false),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

/// True when a process with this pid exists (signal 0; `EPERM` means it
/// exists but belongs to someone else).
pub fn pid_alive(pid: u32) -> bool {
    if pid == 0 || pid > i32::MAX as u32 {
        return false;
    }
    // SAFETY: signal 0 performs only the existence and permission check.
    if unsafe { libc::kill(pid as i32, 0) } == 0 {
        return true;
    }
    io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut tmp_name = path.as_os_str().to_owned();
    tmp_name.push(format!(".{}.tmp", std::process::id()));
    let tmp = PathBuf::from(tmp_name);
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        std::fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn info(token: &str) -> ApiInfo {
        ApiInfo {
            url: "http://127.0.0.1:1".into(),
            token: token.into(),
            pid: std::process::id(),
            version: "0.0.0".into(),
            remote_urls: Vec::new(),
        }
    }

    #[test]
    fn written_file_is_private_and_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let layout = Layout::with_home(dir.path().join("home"));
        write(&layout, &info("abc")).unwrap();
        let mode = std::fs::metadata(info_path(&layout))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
        let home_mode = std::fs::metadata(&layout.home)
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(home_mode, 0o700);
        let loaded = ApiInfo::load(&layout).unwrap();
        assert_eq!(loaded, info("abc"));
        assert!(loaded.process_alive());
        // Only the temp-free file remains.
        assert_eq!(std::fs::read_dir(&layout.home).unwrap().count(), 1);
    }

    #[test]
    fn removal_only_touches_our_own_file() {
        let dir = tempfile::tempdir().unwrap();
        let layout = Layout::with_home(dir.path().to_path_buf());
        write(&layout, &info("mine")).unwrap();
        assert!(!remove_if_owned(&layout, "theirs").unwrap());
        assert!(info_path(&layout).exists());
        assert!(remove_if_owned(&layout, "mine").unwrap());
        assert!(!info_path(&layout).exists());
        assert!(!remove_if_owned(&layout, "mine").unwrap());
    }

    #[test]
    fn remote_urls_are_optional() {
        let json = serde_json::to_value(info("t")).unwrap();
        assert!(json.get("remote_urls").is_none(), "{json}");
        let old: ApiInfo = serde_json::from_value(json).unwrap();
        assert!(old.remote_urls.is_empty());
        let remote = ApiInfo {
            remote_urls: vec!["http://100.64.0.1:7337".into()],
            ..info("t")
        };
        let json = serde_json::to_value(&remote).unwrap();
        assert_eq!(json["remote_urls"][0], "http://100.64.0.1:7337");
    }

    #[test]
    fn debug_hides_the_token() {
        assert!(!format!("{:?}", info("secret-token")).contains("secret-token"));
    }

    #[test]
    fn dead_and_invalid_pids_are_not_alive() {
        assert!(pid_alive(std::process::id()));
        assert!(!pid_alive(0));
        assert!(!pid_alive(u32::MAX));
        let mut child = std::process::Command::new("true").spawn().unwrap();
        let pid = child.id();
        child.wait().unwrap();
        assert!(!pid_alive(pid));
    }
}
