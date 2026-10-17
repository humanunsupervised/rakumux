//! Filesystem layout shared by the daemon and its clients.
//!
//! ```text
//! ~/.orchestra/                    (or $ORCHESTRA_HOME)
//!   orchestra.sqlite3              session registry (daemon-owned)
//!   sessions/<id>/scrollback.ansi  saved scrollback for revive
//!   sessions/<id>/meta.json
//!   history/<id>.log               command history (shell hook)
//!   logs/ptyd.log
//!   remote.json                    remote access: enabled, port, token (0600)
//!   run/                           runtime files, mode 0700
//!     service.json                 registration: pid, socket, token (0600)
//!     service.lock                 flock held by the running daemon
//! /tmp/orchestra-<uid>/ptyd-<hash>.sock
//! ```
//!
//! The runtime directory depends on the home and nothing else, so every
//! login finds the same daemon. Earlier builds used `$XDG_RUNTIME_DIR/
//! orchestra` when that was set; SSH logins set it and mosh doesn't, so two
//! logins could each start a daemon over one registry. Clients still look
//! there ([`Layout::legacy`]) when no daemon answers in `run/`, so a daemon
//! an earlier build started stays reachable.

use crate::ids::SessionId;
use serde::{Deserialize, Serialize};
use std::io;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layout {
    pub home: PathBuf,
    pub run_dir: PathBuf,
}

impl Layout {
    /// `$ORCHESTRA_HOME` or `~/.orchestra`, runtime files in its `run`
    /// directory.
    pub fn from_env() -> io::Result<Self> {
        if let Some(home) = std::env::var_os(crate::env::HOME).filter(|h| !h.is_empty()) {
            return Ok(Self::with_home(PathBuf::from(home)));
        }
        Ok(Self::with_home(default_home()?))
    }

    /// Everything under one directory.
    pub fn with_home(home: PathBuf) -> Self {
        let run_dir = home.join("run");
        Self { home, run_dir }
    }

    /// Layouts earlier builds used for this home, where a daemon they
    /// started may still run: `$XDG_RUNTIME_DIR/orchestra` (and, on Linux,
    /// `/run/user/<uid>/orchestra`, for logins without the variable), for
    /// the default home only. Empty for any other home.
    pub fn legacy(&self) -> Vec<Layout> {
        let is_default = default_home().is_ok_and(|home| same_dir(&home, &self.home));
        if !is_default {
            return Vec::new();
        }
        let mut dirs: Vec<PathBuf> = Vec::new();
        if let Some(dir) = std::env::var_os("XDG_RUNTIME_DIR").filter(|d| !d.is_empty()) {
            dirs.push(PathBuf::from(dir).join("orchestra"));
        }
        if cfg!(target_os = "linux") {
            dirs.push(PathBuf::from(format!(
                "/run/user/{}/orchestra",
                unsafe_free_uid()
            )));
        }
        let mut layouts: Vec<Layout> = Vec::new();
        for run_dir in dirs {
            if same_dir(&run_dir, &self.run_dir) || layouts.iter().any(|l| l.run_dir == run_dir) {
                continue;
            }
            layouts.push(Layout {
                home: self.home.clone(),
                run_dir,
            });
        }
        layouts
    }

    pub fn registry_db(&self) -> PathBuf {
        self.home.join("orchestra.sqlite3")
    }

    pub fn session_dir(&self, id: &SessionId) -> PathBuf {
        self.home.join("sessions").join(id.as_str())
    }

    pub fn history_file(&self, id: &SessionId) -> PathBuf {
        self.home
            .join("history")
            .join(format!("{}.log", id.as_str()))
    }

    pub fn log_file(&self) -> PathBuf {
        self.home.join("logs").join("ptyd.log")
    }

    /// `remote.json`: remote access settings (see [`crate::remote`]).
    pub fn remote_file(&self) -> PathBuf {
        self.home.join("remote.json")
    }

    pub fn registration_file(&self) -> PathBuf {
        self.run_dir.join("service.json")
    }

    pub fn lock_file(&self) -> PathBuf {
        self.run_dir.join("service.lock")
    }

    /// Socket under a per-user `/tmp` dir to stay below the ~104-byte
    /// `sun_path` limit; the name hashes the runtime dir so isolated homes
    /// get separate daemons.
    pub fn socket_path(&self) -> PathBuf {
        let uid = unsafe_free_uid();
        PathBuf::from(format!("/tmp/orchestra-{uid}")).join(format!(
            "ptyd-{:016x}.sock",
            fnv1a(self.run_dir.to_string_lossy().as_bytes())
        ))
    }
}

/// `~/.orchestra`.
fn default_home() -> io::Result<PathBuf> {
    std::env::var_os("HOME")
        .filter(|home| !home.is_empty())
        .map(|home| PathBuf::from(home).join(".orchestra"))
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "HOME is not set"))
}

/// Same directory, comparing canonical paths when both exist.
fn same_dir(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

/// Current user id without libc: the owner of `/proc/self` (Linux) or of a
/// fresh temp file (macOS).
fn unsafe_free_uid() -> u32 {
    use std::os::unix::fs::MetadataExt;
    if let Ok(meta) = std::fs::metadata("/proc/self") {
        return meta.uid();
    }
    let probe = std::env::temp_dir().join(format!("orchestra-uid-{}", std::process::id()));
    let uid = std::fs::File::create(&probe)
        .and_then(|_| std::fs::metadata(&probe))
        .map(|m| m.uid())
        .unwrap_or(0);
    let _ = std::fs::remove_file(&probe);
    uid
}

fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf29ce484222325u64, |hash, b| {
        (hash ^ *b as u64).wrapping_mul(0x100000001b3)
    })
}

/// Contents of `service.json`, written atomically by the daemon with mode 0600.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Registration {
    pub instance_id: String,
    pub pid: u32,
    pub protocol: u32,
    pub version: String,
    pub socket: PathBuf,
    /// User-client token (`api::Auth::User`).
    pub token: String,
}

impl Registration {
    pub fn load(layout: &Layout) -> io::Result<Self> {
        let raw = std::fs::read(layout.registration_file())?;
        serde_json::from_slice(&raw).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
    }
}

/// Locate `orchestra-ptyd`: explicit path, then `$ORCHESTRA_PTYD_BIN`, then
/// next to the current executable, then `PATH`.
pub fn find_ptyd_binary(explicit: Option<&Path>) -> Option<PathBuf> {
    let executable = |p: &Path| {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(p)
            .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
    };
    if let Some(path) = explicit {
        return executable(path).then(|| path.to_path_buf());
    }
    if let Some(path) = std::env::var_os(crate::env::PTYD_BIN).map(PathBuf::from) {
        if executable(&path) {
            return Some(path);
        }
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let sibling = dir.join("orchestra-ptyd");
            if executable(&sibling) {
                return Some(sibling);
            }
        }
    }
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|dir| dir.join("orchestra-ptyd"))
            .find(|p| executable(p))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn isolated_homes_get_distinct_short_sockets() {
        let a = Layout::with_home("/tmp/a-home".into());
        let b = Layout::with_home("/tmp/b-home".into());
        assert_ne!(a.socket_path(), b.socket_path());
        assert!(a.socket_path().to_string_lossy().len() < 100);
        assert_eq!(
            a.registration_file(),
            PathBuf::from("/tmp/a-home/run/service.json")
        );
    }

    #[test]
    fn other_homes_have_no_legacy_layouts() {
        let layout = Layout::with_home("/tmp/orchestra-not-the-default-home".into());
        assert!(layout.legacy().is_empty());
    }

    #[test]
    fn session_paths_use_the_id() {
        let layout = Layout::with_home("/h".into());
        let id = SessionId::from_string("01ABC");
        assert_eq!(layout.session_dir(&id), PathBuf::from("/h/sessions/01ABC"));
        assert_eq!(
            layout.history_file(&id),
            PathBuf::from("/h/history/01ABC.log")
        );
    }
}
