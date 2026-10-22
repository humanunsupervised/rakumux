use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::io::AsRawFd;
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use session_core::ids::Token;

use super::protocol::PROTOCOL;
use super::{OwnerError, OwnerLayout};

/// Registrations are small; a larger file is not ours.
const MAX_REGISTRATION: u64 = 16 * 1024;

/// `run/sensagent.json`, written atomically with mode 0600 by the running owner.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OwnerRegistration {
    pub instance_id: String,
    pub pid: u32,
    pub protocol: u32,
    pub version: String,
    pub socket: PathBuf,
    /// User-client token for the owner's socket.
    pub token: String,
}

impl OwnerRegistration {
    /// Read the running owner's registration: a regular 0600 file of this user.
    pub fn load(layout: &OwnerLayout) -> Result<Self, OwnerError> {
        let path = layout.registration_file();
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&path)
            .map_err(|error| match error.kind() {
                io::ErrorKind::NotFound => OwnerError::NotRunning,
                _ => OwnerError::Registration,
            })?;
        let metadata = file.metadata().map_err(|_| OwnerError::Registration)?;
        if !metadata.is_file()
            || metadata.uid() != super::layout::current_uid()
            || metadata.mode() & 0o077 != 0
            || metadata.len() > MAX_REGISTRATION
        {
            return Err(OwnerError::Registration);
        }
        let mut raw = Vec::new();
        file.take(MAX_REGISTRATION + 1)
            .read_to_end(&mut raw)
            .map_err(|_| OwnerError::Registration)?;
        serde_json::from_slice(&raw).map_err(|_| OwnerError::Registration)
    }
}

impl fmt::Debug for OwnerRegistration {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OwnerRegistration")
            .field("pid", &self.pid)
            .field("protocol", &self.protocol)
            .field("token", &"[REDACTED]")
            .finish_non_exhaustive()
    }
}

/// The one status owner of a Rakumux home: holds the `sensagent.lock` flock for
/// its lifetime, owns the socket and registration, and removes them on drop (only
/// if they are still its own). With the lock held, files left behind are stale.
pub struct OwnerClaim {
    registration: OwnerRegistration,
    token: Token,
    listener: Option<UnixListener>,
    layout: OwnerLayout,
    /// Holds the flock; released when dropped.
    _lock: File,
    /// `(dev, ino)` of the socket this claim bound.
    socket_id: (u64, u64),
}

impl OwnerClaim {
    /// Prepare the layout, take the lock, replace stale files, bind the socket (0600)
    /// and write the registration. A second owner gets `AlreadyRunning`.
    pub fn claim(layout: &OwnerLayout, version: &str) -> Result<Self, OwnerError> {
        layout.prepare()?;
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(layout.lock_file())
            .map_err(|_| OwnerError::Layout)?;
        // SAFETY: flock on a descriptor owned by `lock`, which outlives the call.
        if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            let error = io::Error::last_os_error();
            return Err(match error.raw_os_error() {
                Some(code) if code == libc::EWOULDBLOCK || code == libc::EAGAIN => {
                    OwnerError::AlreadyRunning
                }
                _ => OwnerError::Layout,
            });
        }
        let socket = layout.socket().to_path_buf();
        remove_if_present(&socket)?;
        remove_if_present(&layout.registration_file())?;
        let listener = UnixListener::bind(&socket).map_err(|_| OwnerError::Socket)?;
        fs::set_permissions(&socket, fs::Permissions::from_mode(0o600))
            .map_err(|_| OwnerError::Socket)?;
        let metadata = fs::symlink_metadata(&socket).map_err(|_| OwnerError::Socket)?;
        let token = Token::generate();
        let registration = OwnerRegistration {
            instance_id: Token::generate().as_str()[..32].to_owned(),
            pid: std::process::id(),
            protocol: PROTOCOL,
            version: version.to_owned(),
            socket,
            token: token.as_str().to_owned(),
        };
        let json =
            serde_json::to_vec_pretty(&registration).map_err(|_| OwnerError::Registration)?;
        write_atomic(&layout.registration_file(), &json).map_err(|_| OwnerError::Registration)?;
        Ok(Self {
            registration,
            token,
            listener: Some(listener),
            layout: layout.clone(),
            _lock: lock,
            socket_id: (metadata.dev(), metadata.ino()),
        })
    }

    pub fn registration(&self) -> &OwnerRegistration {
        &self.registration
    }

    pub(super) fn token(&self) -> &Token {
        &self.token
    }

    /// The bound socket, once, for the server.
    pub(super) fn take_listener(&mut self) -> Option<UnixListener> {
        self.listener.take()
    }
}

impl Drop for OwnerClaim {
    fn drop(&mut self) {
        let ours = OwnerRegistration::load(&self.layout)
            .is_ok_and(|current| current.instance_id == self.registration.instance_id);
        if ours {
            let _ = fs::remove_file(self.layout.registration_file());
        }
        if let Ok(metadata) = fs::symlink_metadata(&self.registration.socket) {
            if (metadata.dev(), metadata.ino()) == self.socket_id {
                let _ = fs::remove_file(&self.registration.socket);
            }
        }
    }
}

impl fmt::Debug for OwnerClaim {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OwnerClaim")
            .field("registration", &self.registration)
            .finish_non_exhaustive()
    }
}

fn remove_if_present(path: &Path) -> Result<(), OwnerError> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(_) => Err(OwnerError::Layout),
    }
}

/// Write via a 0600 temporary file in the same directory and a rename, so readers
/// never see a partial file.
fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut name = path.as_os_str().to_owned();
    name.push(format!(".{}.tmp", std::process::id()));
    let temporary = PathBuf::from(name);
    let result = (|| {
        // A leftover from a crashed owner with this pid could have another mode.
        match fs::remove_file(&temporary) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(&temporary, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}
