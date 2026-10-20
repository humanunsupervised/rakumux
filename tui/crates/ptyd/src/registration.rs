//! Single-instance election and the registration file.
//!
//! The daemon holding an exclusive `flock` on `service.lock` is the one
//! daemon for its layout. With the lock held, any socket or registration file
//! left behind is stale (its owner died) and is replaced.

use crate::handoff::{self, ClaimHandoff};
use session_core::ids::Token;
use session_core::paths::{Layout, Registration};
use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::io::{AsRawFd, FromRawFd};
use std::os::unix::net::UnixListener;
use std::path::Path;

/// A running daemon's claim: holds the `service.lock` flock for its
/// lifetime, owns the socket and registration file, and removes them on drop
/// (only if they are still its own).
pub struct Claim {
    pub registration: Registration,
    pub listener: UnixListener,
    pub user_token: Token,
    layout: Layout,
    /// Holds the flock; released when dropped.
    lock: File,
    /// `(dev, ino)` of the socket file this claim bound.
    socket_id: (u64, u64),
    /// Handed to the next daemon image: keep the files on drop.
    handed_off: bool,
}

impl Claim {
    /// The layout this claim was made for.
    pub fn layout(&self) -> &Layout {
        &self.layout
    }

    /// Hand the claim to the next daemon image (`handoff`): inheritable
    /// duplicates of the socket and lock descriptors (the lock stays held
    /// through them), and the registration. The socket and registration
    /// files stay.
    pub fn hand_off(mut self) -> io::Result<ClaimHandoff> {
        let listener_fd = handoff::dup_inheritable(self.listener.as_raw_fd())?;
        let lock_fd = match handoff::dup_inheritable(self.lock.as_raw_fd()) {
            Ok(fd) => fd,
            Err(e) => {
                handoff::close(listener_fd);
                return Err(e);
            }
        };
        self.handed_off = true;
        Ok(ClaimHandoff {
            registration: self.registration.clone(),
            listener_fd,
            lock_fd,
        })
    }
}

#[derive(Debug)]
pub enum ClaimError {
    /// Another daemon holds the lock; contains its registration if readable.
    AlreadyRunning(Option<Registration>),
    Io(std::io::Error),
}

impl std::fmt::Display for ClaimError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ClaimError::AlreadyRunning(Some(reg)) => {
                write!(f, "another rakumux-ptyd is running (pid {})", reg.pid)
            }
            ClaimError::AlreadyRunning(None) => write!(f, "another rakumux-ptyd is running"),
            ClaimError::Io(e) => write!(f, "cannot claim the daemon slot: {e}"),
        }
    }
}

impl std::error::Error for ClaimError {}

impl From<io::Error> for ClaimError {
    fn from(e: io::Error) -> Self {
        ClaimError::Io(e)
    }
}

/// Create dirs (0700), take the lock, remove a stale socket/registration,
/// bind the socket (0600), write `service.json` atomically (0600).
pub fn claim(layout: &Layout, version: &str) -> Result<Claim, ClaimError> {
    if !layout.home.exists() {
        create_private_dir(&layout.home)?;
    }
    ensure_private_dir(&layout.run_dir)?;
    let socket = layout.socket_path();
    let socket_dir = socket.parent().ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "socket path has no directory")
    })?;
    ensure_private_dir(socket_dir)?;

    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(layout.lock_file())?;
    // SAFETY: flock on a descriptor owned by `lock`, which outlives the call.
    if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        let err = io::Error::last_os_error();
        return Err(match err.raw_os_error() {
            Some(code) if code == libc::EWOULDBLOCK || code == libc::EAGAIN => {
                ClaimError::AlreadyRunning(Registration::load(layout).ok())
            }
            _ => ClaimError::Io(err),
        });
    }

    // A daemon an earlier build started in a legacy runtime directory
    // serves this registry too; clients find it there, so don't compete.
    for legacy in layout.legacy() {
        if lock_is_held(&legacy.lock_file()) {
            return Err(ClaimError::AlreadyRunning(Registration::load(&legacy).ok()));
        }
    }

    // We hold the lock, so whatever is left here belongs to a dead daemon.
    remove_if_present(&socket)?;
    remove_if_present(&layout.registration_file())?;

    let listener = UnixListener::bind(&socket)?;
    std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600))?;
    let meta = std::fs::symlink_metadata(&socket)?;
    let socket_id = (meta.dev(), meta.ino());

    let user_token = Token::generate();
    let registration = Registration {
        instance_id: Token::generate().as_str()[..32].to_string(),
        pid: std::process::id(),
        protocol: session_core::PROTOCOL_VERSION,
        version: version.to_string(),
        socket,
        token: user_token.as_str().to_string(),
    };
    let json = serde_json::to_vec_pretty(&registration).map_err(io::Error::other)?;
    write_atomic(&layout.registration_file(), &json)?;

    Ok(Claim {
        registration,
        listener,
        user_token,
        layout: layout.clone(),
        lock,
        socket_id,
        handed_off: false,
    })
}

/// Take over the claim a previous daemon image handed over: its socket,
/// lock, instance id and user token. `service.json` is rewritten with this
/// build's version and protocol.
pub fn reclaim(layout: &Layout, version: &str, handed: ClaimHandoff) -> Result<Claim, ClaimError> {
    for fd in [handed.listener_fd, handed.lock_fd] {
        handoff::set_cloexec(fd, true)?;
    }
    // SAFETY: the previous image passed these descriptors to us; we own them.
    let listener = unsafe { UnixListener::from_raw_fd(handed.listener_fd) };
    // SAFETY: as above.
    let lock = unsafe { File::from_raw_fd(handed.lock_fd) };
    let meta = std::fs::symlink_metadata(&handed.registration.socket)?;
    let registration = Registration {
        pid: std::process::id(),
        protocol: session_core::PROTOCOL_VERSION,
        version: version.to_string(),
        ..handed.registration
    };
    let json = serde_json::to_vec_pretty(&registration).map_err(io::Error::other)?;
    write_atomic(&layout.registration_file(), &json)?;
    Ok(Claim {
        user_token: Token::from_string(registration.token.clone()),
        registration,
        listener,
        layout: layout.clone(),
        lock,
        socket_id: (meta.dev(), meta.ino()),
        handed_off: false,
    })
}

impl Drop for Claim {
    fn drop(&mut self) {
        if self.handed_off {
            return;
        }
        let ours = Registration::load(&self.layout)
            .map(|current| current.instance_id == self.registration.instance_id)
            .unwrap_or(false);
        if ours {
            let _ = std::fs::remove_file(self.layout.registration_file());
        }
        if let Ok(meta) = std::fs::symlink_metadata(&self.registration.socket) {
            if (meta.dev(), meta.ino()) == self.socket_id {
                let _ = std::fs::remove_file(&self.registration.socket);
            }
        }
    }
}

/// Another process holds the flock on `path` (false when there's no file).
fn lock_is_held(path: &Path) -> bool {
    let Ok(file) = File::open(path) else {
        return false;
    };
    // SAFETY: flock on a descriptor owned by `file`, which outlives the call;
    // a lock we get is released when `file` closes.
    let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    rc != 0
        && matches!(
            io::Error::last_os_error().raw_os_error(),
            Some(code) if code == libc::EWOULDBLOCK || code == libc::EAGAIN
        )
}

fn create_private_dir(dir: &Path) -> io::Result<()> {
    std::fs::create_dir_all(dir)?;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
}

/// Make sure `dir` is a real directory owned by us with mode 0700, creating
/// it if needed. Refuses symlinks and directories owned by someone else
/// (another user could otherwise pre-create `/tmp/rakumux-<uid>`).
fn ensure_private_dir(dir: &Path) -> io::Result<()> {
    match std::fs::symlink_metadata(dir) {
        Ok(meta) => {
            // SAFETY: geteuid has no preconditions.
            let uid = unsafe { libc::geteuid() };
            if !meta.file_type().is_dir() || meta.uid() != uid {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    format!(
                        "{} is not a directory owned by the current user",
                        dir.display()
                    ),
                ));
            }
            if meta.mode() & 0o777 != 0o700 {
                std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
            }
            Ok(())
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => create_private_dir(dir),
        Err(e) => Err(e),
    }
}

fn remove_if_present(path: &Path) -> io::Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

/// Write `bytes` to `path` via a 0600 temp file in the same directory and a
/// rename, so readers never see a partial file.
fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut tmp_name = path.as_os_str().to_owned();
    tmp_name.push(format!(".{}.tmp", std::process::id()));
    let tmp = std::path::PathBuf::from(tmp_name);
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
    use std::time::{Duration, Instant};

    #[test]
    fn atomic_write_is_private_and_replaces() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("service.json");
        write_atomic(&path, b"one").unwrap();
        write_atomic(&path, b"two").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"two");
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn a_lock_held_elsewhere_is_seen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("service.lock");
        assert!(!lock_is_held(&path));
        let holder = File::create(&path).unwrap();
        assert!(!lock_is_held(&path));
        // SAFETY: flock on an open descriptor.
        assert_eq!(
            unsafe { libc::flock(holder.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
            0
        );
        assert!(lock_is_held(&path));
        let duplicate = holder.try_clone().unwrap();
        drop(holder);
        assert!(lock_is_held(&path));
        drop(duplicate);

        // Concurrent process-spawning tests can briefly retain a copy of
        // this descriptor before exec on macOS. The lock must still release.
        let deadline = Instant::now() + Duration::from_secs(5);
        while lock_is_held(&path) {
            assert!(Instant::now() < deadline, "service lock was not released");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn private_dirs_are_tightened_and_symlinks_refused() {
        let dir = tempfile::tempdir().unwrap();
        let open = dir.path().join("open");
        std::fs::create_dir(&open).unwrap();
        std::fs::set_permissions(&open, std::fs::Permissions::from_mode(0o755)).unwrap();
        ensure_private_dir(&open).unwrap();
        let mode = std::fs::metadata(&open).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700);

        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&open, &link).unwrap();
        assert!(ensure_private_dir(&link).is_err());

        let nested = dir.path().join("a/b");
        ensure_private_dir(&nested).unwrap();
        assert!(nested.is_dir());
    }
}
