use std::fs;
use std::io;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
use std::os::unix::io::AsRawFd;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};

use session_core::paths::Layout;

use super::OwnerError;

/// Where one Rakumux home's status owner keeps its files. Proposed, for review:
///
/// ```text
/// $RAKUMUX_HOME/
///   sensagent/                 0700, the owner's alone
///     status/                  consent and cached status (status.sqlite3, status.lock)
///     credentials/             sign-in credentials (unencrypted private files)
///   uploads/<reply id>/        0700: files attached to feed replies (0600), kept 7 days
///   run/                       shared with rakumux-ptyd, 0700
///     sensagent.json           registration: pid, socket, token (0600)
///     sensagent.lock           flock held by the running owner
/// /tmp/rakumux-<uid>/sensagent-<hash>.sock
/// ```
///
/// The socket sits next to the daemon's and uses the same hash of the runtime
/// directory, so isolated homes get separate owners. Nothing here reads the daemon's
/// registry or registration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OwnerLayout {
    home: PathBuf,
    run_dir: PathBuf,
    socket: PathBuf,
}

impl OwnerLayout {
    /// The owner's files for a Rakumux home layout.
    pub fn new(layout: &Layout) -> Self {
        let daemon = layout.socket_path();
        let name = daemon
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(|name| name.strip_prefix("ptyd-"))
            .map(|hash| format!("sensagent-{hash}"))
            .unwrap_or_else(|| "sensagent.sock".into());
        Self {
            home: layout.home.clone(),
            run_dir: layout.run_dir.clone(),
            socket: daemon.with_file_name(name),
        }
    }

    /// `$RAKUMUX_HOME/sensagent`.
    pub fn root(&self) -> PathBuf {
        self.home.join("sensagent")
    }

    /// The dedicated status-store directory.
    pub fn status_dir(&self) -> PathBuf {
        self.root().join("status")
    }

    /// The dedicated credential directory.
    pub fn credentials_dir(&self) -> PathBuf {
        self.root().join("credentials")
    }

    /// `$RAKUMUX_HOME/uploads`: files attached to feed replies, a directory
    /// per reply, for the agents they're typed to (made when first needed).
    pub fn uploads_dir(&self) -> PathBuf {
        self.home.join("uploads")
    }

    pub fn registration_file(&self) -> PathBuf {
        self.run_dir.join("sensagent.json")
    }

    pub fn lock_file(&self) -> PathBuf {
        self.run_dir.join("sensagent.lock")
    }

    pub fn socket(&self) -> &Path {
        &self.socket
    }

    /// Create the home (if missing), the runtime and socket directories (0700,
    /// tightened like the daemon does) and the owner's own directory. The owner's
    /// directory must be a real 0700 directory of this user; it is never chmodded.
    pub fn prepare(&self) -> Result<(), OwnerError> {
        if !self.home.exists() {
            create_private(&self.home)?;
        }
        tighten_private(&self.run_dir)?;
        let socket_dir = self.socket.parent().ok_or(OwnerError::Layout)?;
        tighten_private(socket_dir)?;
        match fs::DirBuilder::new().mode(0o700).create(self.root()) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(_) => return Err(OwnerError::Layout),
        }
        let metadata = fs::symlink_metadata(self.root()).map_err(|_| OwnerError::Layout)?;
        if !metadata.is_dir()
            || metadata.uid() != current_uid()
            || metadata.mode() & 0o7777 != 0o700
        {
            return Err(OwnerError::Layout);
        }
        Ok(())
    }
}

pub(super) fn current_uid() -> u32 {
    // SAFETY: geteuid has no preconditions.
    unsafe { libc::geteuid() }
}

/// Create `dir` (0700) and any missing parents. The directory itself is made with a
/// plain `mkdir` and checked afterwards, so something planted at its path is refused,
/// never followed.
fn create_private(dir: &Path) -> Result<(), OwnerError> {
    if let Some(parent) = dir.parent() {
        fs::create_dir_all(parent).map_err(|_| OwnerError::Layout)?;
    }
    match fs::DirBuilder::new().mode(0o700).create(dir) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
        Err(_) => return Err(OwnerError::Layout),
    }
    tighten_private(dir)
}

/// A real directory of this user, mode 0700 (tightened if looser), created when
/// missing. Symlinks and other users' directories are refused: another user could
/// pre-create `/tmp/rakumux-<uid>`.
fn tighten_private(dir: &Path) -> Result<(), OwnerError> {
    match fs::symlink_metadata(dir) {
        Ok(metadata) => {
            if !metadata.file_type().is_dir() || metadata.uid() != current_uid() {
                return Err(OwnerError::Layout);
            }
            if metadata.mode() & 0o777 != 0o700 {
                fs::set_permissions(dir, fs::Permissions::from_mode(0o700))
                    .map_err(|_| OwnerError::Layout)?;
            }
            Ok(())
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => create_private(dir),
        Err(_) => Err(OwnerError::Layout),
    }
}

/// The connected process's user id.
pub(super) fn peer_uid(stream: &UnixStream) -> io::Result<u32> {
    #[cfg(any(target_os = "linux", target_os = "android"))]
    {
        // SAFETY: getsockopt writes at most `length` bytes into `credentials`.
        unsafe {
            let mut credentials: libc::ucred = std::mem::zeroed();
            let mut length = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
            let result = libc::getsockopt(
                stream.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_PEERCRED,
                (&mut credentials as *mut libc::ucred).cast(),
                &mut length,
            );
            if result != 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(credentials.uid)
        }
    }
    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    {
        let (mut uid, mut gid): (libc::uid_t, libc::gid_t) = (0, 0);
        // SAFETY: getpeereid writes the two ids.
        if unsafe { libc::getpeereid(stream.as_raw_fd(), &mut uid, &mut gid) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(uid)
    }
}
