//! One API per Orchestra home.
//!
//! The running API holds an exclusive `flock` on `<home>/api.lock`. While it
//! is held, `api.json` belongs to the holder; when a new API gets the lock,
//! any `api.json` left behind is stale (its writer died) and is replaced.

use crate::discovery::{self, ApiInfo};
use session_core::paths::Layout;
use std::fs::{File, OpenOptions};
use std::io;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::io::AsRawFd;

/// The running API's claim on its home. Dropping it releases the lock.
pub struct Instance {
    layout: Layout,
    /// Holds the flock until dropped.
    _lock: File,
    /// The `api.json` found when the lock was taken: written by an API that
    /// is gone.
    pub stale: Option<ApiInfo>,
}

impl Instance {
    pub fn layout(&self) -> &Layout {
        &self.layout
    }
}

/// Why the claim failed.
#[derive(Debug)]
pub enum ClaimError {
    /// Another API holds the lock; its `api.json` when readable.
    AlreadyRunning(Option<ApiInfo>),
    Io(io::Error),
}

impl std::fmt::Display for ClaimError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ClaimError::AlreadyRunning(Some(info)) => write!(
                f,
                "another orchestra-api is running (pid {}, {})",
                info.pid, info.url
            ),
            ClaimError::AlreadyRunning(None) => {
                write!(f, "another orchestra-api is running or starting")
            }
            ClaimError::Io(error) => write!(f, "cannot claim the API slot: {error}"),
        }
    }
}

impl std::error::Error for ClaimError {}

impl From<io::Error> for ClaimError {
    fn from(error: io::Error) -> Self {
        ClaimError::Io(error)
    }
}

/// Take the lock for `layout` (creating the home with mode 0700).
pub fn claim(layout: &Layout) -> Result<Instance, ClaimError> {
    discovery::ensure_private_dir(&layout.home)?;
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(discovery::lock_path(layout))?;
    // SAFETY: flock on a descriptor owned by `lock`, which outlives the call.
    if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        let error = io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::EWOULDBLOCK) {
            return Err(ClaimError::AlreadyRunning(ApiInfo::load(layout).ok()));
        }
        return Err(ClaimError::Io(error));
    }
    let stale = ApiInfo::load(layout).ok();
    Ok(Instance {
        layout: layout.clone(),
        _lock: lock,
        stale,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn second_claim_fails_until_the_first_is_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let layout = Layout::with_home(dir.path().join("home"));
        let first = claim(&layout).unwrap();
        assert!(first.stale.is_none());
        match claim(&layout) {
            Err(ClaimError::AlreadyRunning(None)) => {}
            Err(other) => panic!("unexpected error: {other}"),
            Ok(_) => panic!("second claim succeeded"),
        }
        drop(first);
        assert!(claim(&layout).is_ok());
    }

    #[test]
    fn leftover_api_json_is_reported_as_stale() {
        let dir = tempfile::tempdir().unwrap();
        let layout = Layout::with_home(dir.path().to_path_buf());
        let leftover = ApiInfo {
            url: "http://127.0.0.1:9".into(),
            token: "old".into(),
            pid: 999_999,
            version: "0.0.0".into(),
            remote_urls: Vec::new(),
        };
        discovery::write(&layout, &leftover).unwrap();
        let instance = claim(&layout).unwrap();
        assert_eq!(instance.stale, Some(leftover));
    }
}
