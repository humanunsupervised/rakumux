use std::ffi::{CStr, CString};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use super::credentials::{StoredCredential, StoredDocument};
use super::{
    CredentialRevision, CredentialSnapshot, CredentialStore, CredentialStoreError, SavedCredentials,
};

const MAX_BYTES: usize = 64 * 1024;
const FILE_NAME: &CStr = c"credentials.json";
const LOCK_NAME: &CStr = c"credentials.lock";
/// How long an operation waits for another to release the lock before `Busy`.
/// Operations hold it only to read or replace small files, but an owner reads
/// credentials from several threads (status checks and requests). Reads share the
/// lock; replacements take it alone.
const LOCK_WAIT: Duration = Duration::from_millis(250);
const LOCK_RETRY: Duration = Duration::from_millis(2);

impl From<std::io::Error> for CredentialStoreError {
    fn from(error: std::io::Error) -> Self {
        if matches!(error.raw_os_error(), Some(libc::ELOOP | libc::ENOTDIR)) {
            Self::UnsafePath
        } else {
            Self::Io(error.kind())
        }
    }
}

/// Explicit unencrypted private-file backend for a trusted local parent directory.
///
/// The selected dedicated directory must be owned / 0700. Files must be owned,
/// regular, singly linked / 0600; unsafe existing paths are rejected, not chmodded.
/// All files are accessed relative to an opened no-follow directory descriptor.
/// This is not an OS keychain or protection from a hostile process with the same UID.
#[derive(Clone)]
pub struct FileCredentialStore {
    directory: PathBuf,
}

impl FileCredentialStore {
    /// Select a dedicated credential directory without reading environment or home data.
    /// Its parent must already exist and be trusted by the local owner.
    pub fn new(directory: impl Into<PathBuf>) -> Self {
        Self {
            directory: directory.into(),
        }
    }

    /// Open the directory under its lock: shared for reads (records are only ever
    /// replaced by rename), exclusive for replacements.
    fn locked_directory(&self, exclusive: bool) -> Result<(File, File), CredentialStoreError> {
        match fs::DirBuilder::new().mode(0o700).create(&self.directory) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.into()),
        }
        let directory = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&self.directory)?;
        validate_directory(&directory)?;
        let lock = open_at(&directory, LOCK_NAME, libc::O_RDWR | libc::O_CREAT, 0o600)?;
        validate_file(&lock)?;
        let deadline = Instant::now() + LOCK_WAIT;
        let mode = if exclusive {
            libc::LOCK_EX
        } else {
            libc::LOCK_SH
        };
        // SAFETY: the descriptor belongs to this owned File and flock takes no pointers.
        while unsafe { libc::flock(lock.as_raw_fd(), mode | libc::LOCK_NB) } != 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() != std::io::ErrorKind::WouldBlock {
                return Err(error.into());
            }
            if Instant::now() >= deadline {
                return Err(CredentialStoreError::Busy);
            }
            std::thread::sleep(LOCK_RETRY);
        }
        Ok((directory, lock))
    }
}

impl CredentialStore for FileCredentialStore {
    fn load(&self) -> Result<CredentialSnapshot, CredentialStoreError> {
        let (directory, _lock) = self.locked_directory(false)?;
        read_snapshot(&directory)
    }

    fn compare_and_swap(
        &self,
        expected: CredentialRevision,
        credentials: Option<&SavedCredentials>,
    ) -> Result<CredentialRevision, CredentialStoreError> {
        let (directory, _lock) = self.locked_directory(true)?;
        let snapshot = read_snapshot(&directory)?;
        if snapshot.revision != expected {
            return Err(CredentialStoreError::Conflict);
        }
        let revision = expected
            .0
            .checked_add(1)
            .ok_or(CredentialStoreError::RevisionExhausted)?;
        let document = StoredDocument {
            version: 1,
            revision,
            credential: credentials.map(StoredCredential::from),
        };
        // Round-trip through the private validation boundary before replacing any file.
        let bytes = serde_json::to_vec(&document).map_err(|_| CredentialStoreError::InvalidData)?;
        if bytes.len() > MAX_BYTES {
            return Err(CredentialStoreError::TooLarge);
        }
        let _validated = decode_document(&bytes)?;
        let mut pending = PendingFile::create(&directory)?;
        pending.file.write_all(&bytes)?;
        pending.file.sync_all()?;
        // SAFETY: both names are valid C strings and directory is a live owned FD.
        if unsafe {
            libc::renameat(
                directory.as_raw_fd(),
                pending.name.as_ptr(),
                directory.as_raw_fd(),
                FILE_NAME.as_ptr(),
            )
        } != 0
        {
            return Err(std::io::Error::last_os_error().into());
        }
        pending.committed = true;
        directory
            .sync_all()
            .map_err(|_| CredentialStoreError::CommitUncertain)?;
        Ok(CredentialRevision(revision))
    }
}

fn validate_directory(directory: &File) -> Result<(), CredentialStoreError> {
    let metadata = directory.metadata()?;
    // SAFETY: geteuid has no pointers or preconditions.
    let owner = unsafe { libc::geteuid() };
    if !metadata.is_dir() || metadata.uid() != owner || metadata.mode() & 0o7777 != 0o700 {
        return Err(CredentialStoreError::UnsafePath);
    }
    Ok(())
}

fn validate_file(file: &File) -> Result<(), CredentialStoreError> {
    let metadata = file.metadata()?;
    // SAFETY: geteuid has no pointers or preconditions.
    let owner = unsafe { libc::geteuid() };
    if !metadata.is_file()
        || metadata.uid() != owner
        || metadata.nlink() != 1
        || metadata.mode() & 0o7777 != 0o600
    {
        return Err(CredentialStoreError::UnsafePath);
    }
    Ok(())
}

fn open_at(
    directory: &File,
    name: &CStr,
    flags: i32,
    mode: libc::c_uint,
) -> Result<File, CredentialStoreError> {
    // SAFETY: name is NUL terminated; openat returns a new FD, transferred once to File.
    // The variadic mode uses the promoted C type, including on Darwin's narrower mode_t.
    let descriptor = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            name.as_ptr(),
            flags | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK,
            mode,
        )
    };
    if descriptor < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    // SAFETY: descriptor is newly owned and has not been wrapped elsewhere.
    Ok(unsafe { File::from_raw_fd(descriptor) })
}

fn read_snapshot(directory: &File) -> Result<CredentialSnapshot, CredentialStoreError> {
    let file = match open_at(directory, FILE_NAME, libc::O_RDONLY, 0) {
        Ok(file) => file,
        Err(CredentialStoreError::Io(std::io::ErrorKind::NotFound)) => {
            return Ok(CredentialSnapshot::default())
        }
        Err(error) => return Err(error),
    };
    validate_file(&file)?;
    let mut bytes = Vec::new();
    file.take((MAX_BYTES + 1) as u64).read_to_end(&mut bytes)?;
    if bytes.len() > MAX_BYTES {
        return Err(CredentialStoreError::TooLarge);
    }
    decode_document(&bytes)
}

fn decode_document(bytes: &[u8]) -> Result<CredentialSnapshot, CredentialStoreError> {
    let document: StoredDocument =
        serde_json::from_slice(bytes).map_err(|_| CredentialStoreError::InvalidData)?;
    if document.version != 1 || document.revision == 0 {
        return Err(CredentialStoreError::UnsupportedFormat);
    }
    Ok(CredentialSnapshot {
        revision: CredentialRevision(document.revision),
        credentials: document
            .credential
            .map(StoredCredential::validate)
            .transpose()?,
    })
}

struct PendingFile<'a> {
    directory: &'a File,
    name: CString,
    file: File,
    committed: bool,
}

impl<'a> PendingFile<'a> {
    fn create(directory: &'a File) -> Result<Self, CredentialStoreError> {
        for _ in 0..8 {
            let mut random = [0u8; 8];
            getrandom::getrandom(&mut random)
                .map_err(|_| CredentialStoreError::RandomUnavailable)?;
            let name = CString::new(format!(
                ".credentials-{:016x}.tmp",
                u64::from_ne_bytes(random)
            ))
            .map_err(|_| CredentialStoreError::InvalidData)?;
            match open_at(
                directory,
                &name,
                libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL,
                0o600,
            ) {
                Ok(file) => {
                    let pending = Self {
                        directory,
                        name,
                        file,
                        committed: false,
                    };
                    validate_file(&pending.file)?;
                    return Ok(pending);
                }
                Err(CredentialStoreError::Io(std::io::ErrorKind::AlreadyExists)) => continue,
                Err(error) => return Err(error),
            }
        }
        Err(CredentialStoreError::Busy)
    }
}

impl Drop for PendingFile<'_> {
    fn drop(&mut self) {
        if !self.committed {
            // SAFETY: unlink only this operation's exact private temporary filename.
            let _ = unsafe { libc::unlinkat(self.directory.as_raw_fd(), self.name.as_ptr(), 0) };
        }
    }
}

#[cfg(test)]
mod tests;
