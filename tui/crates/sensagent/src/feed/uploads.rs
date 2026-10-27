//! Files attached to replies (`contracts/feed/` `attachment`), saved for the
//! agent before the reply is typed in, which names them by path:
//! `<home>/uploads/<reply id>/<name>`, directories 0700 and files 0600, each
//! created fresh and never through a symlink. A reply's directory is made
//! afresh for its first file (an earlier attempt's removed); a file is named
//! only from the reply's `name`, checked again here, never from rakumux.com's
//! answer. Files the agent was given stay [`KEEP`]; [`Uploads::clean`] removes
//! older ones, and those of a reply that won't be typed in go at once.

use std::fs::{self, DirBuilder, File, OpenOptions, Permissions};
use std::io::{self, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use crate::auth::{CredentialStore, WallClock};

use super::job::{FeedJob, FeedResult};
use super::replies::{Fetch, Fetched};
use super::wire::{self, Download, FeedFailure};

/// How long a reply's files are kept.
pub(crate) const KEEP: Duration = Duration::from_secs(7 * 24 * 3600);
/// How often older ones are looked for, besides when the service starts.
pub(crate) const CLEAN_EVERY: Duration = Duration::from_secs(24 * 3600);
/// Names tried for one file (`name`, then `name (2).ext` onwards).
const MAX_TRIES: usize = 100;
/// The longest file name Linux and macOS take, in bytes.
const MAX_NAME_BYTES: usize = 255;
/// A dot this near the end starts an extension, kept after ` (n)`.
const MAX_EXTENSION_BYTES: usize = 32;

/// Why a file wasn't saved. Static: never a path or a name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SaveError {
    /// Something this service didn't make is in the way (a symlink, a file,
    /// another user's directory), or the reply id or name isn't one.
    Refused,
    /// The file system failed.
    Io,
}

/// Where replies' files are saved: `<home>/uploads`, an absolute path.
#[derive(Clone, Debug)]
pub(crate) struct Uploads {
    root: PathBuf,
}

impl Uploads {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    /// Save `bytes` as one of reply `reply`'s files, as `name` or, when that
    /// is one of `taken` (its files saved already) ignoring case, as
    /// `stem (2).ext` and on. `first`: its directory is made afresh. Returns
    /// the file's path.
    pub fn save(
        &self,
        reply: &str,
        name: &str,
        taken: &[String],
        bytes: &[u8],
        first: bool,
    ) -> Result<PathBuf, SaveError> {
        if !crate::feed::is_entry_id(reply) || !wire::file_name(name) {
            return Err(SaveError::Refused);
        }
        private_dir(&self.root, true)?;
        let dir = self.root.join(reply);
        if first {
            match fs::symlink_metadata(&dir) {
                Ok(metadata) if metadata.file_type().is_dir() => {
                    fs::remove_dir_all(&dir).map_err(|_| SaveError::Io)?;
                }
                Ok(_) => return Err(SaveError::Refused),
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(_) => return Err(SaveError::Io),
            }
            match DirBuilder::new().mode(0o700).create(&dir) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                    return Err(SaveError::Refused)
                }
                Err(_) => return Err(SaveError::Io),
            }
        }
        private_dir(&dir, false)?;
        for candidate in names(name, taken).take(MAX_TRIES) {
            let path = dir.join(&candidate);
            let created = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
                .open(&path);
            match created {
                Ok(file) => return write(file, &path, bytes).map(|()| path),
                // Taken on disk though not by name here (macOS also equates
                // Unicode normalisations): the next name.
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(_) => return Err(SaveError::Io),
            }
        }
        Err(SaveError::Refused)
    }

    /// Remove reply `reply`'s files: it won't be typed in. Only a real
    /// directory named for a reply is removed, and nothing through a symlink.
    pub fn discard(&self, reply: &str) {
        if crate::feed::is_entry_id(reply) && real_dir(&self.root) {
            let dir = self.root.join(reply);
            if real_dir(&dir) {
                let _ = fs::remove_dir_all(dir);
            }
        }
    }

    /// Remove replies' directories last changed [`KEEP`] or longer before
    /// `now`. Only directories named for a reply (a ULID) are removed, as
    /// this service makes them, and nothing through a symlink. Returns how
    /// many went.
    pub fn clean(&self, now: SystemTime) -> usize {
        if !real_dir(&self.root) {
            return 0;
        }
        let Ok(entries) = fs::read_dir(&self.root) else {
            return 0;
        };
        let mut removed = 0;
        for entry in entries.flatten() {
            if !entry
                .file_name()
                .to_str()
                .is_some_and(crate::feed::is_entry_id)
            {
                continue;
            }
            let path = entry.path();
            let old = fs::symlink_metadata(&path)
                .ok()
                .filter(|metadata| metadata.file_type().is_dir())
                .and_then(|metadata| metadata.modified().ok())
                .and_then(|changed| now.duration_since(changed).ok())
                .is_some_and(|age| age >= KEEP);
            if old && fs::remove_dir_all(&path).is_ok() {
                removed += 1;
            }
        }
        removed
    }
}

/// Download one of a reply's files (`job`) and save it among them.
pub(crate) fn fetch<A: CredentialStore, C: WallClock>(
    job: FeedJob<A, C>,
    fetch: &Fetch,
    uploads: &Uploads,
) -> Fetched {
    saved(job.run().outcome, fetch, uploads)
}

/// What a download's `outcome` comes to, its file saved when it brought one.
pub(super) fn saved(
    outcome: Result<FeedResult, FeedFailure>,
    fetch: &Fetch,
    uploads: &Uploads,
) -> Fetched {
    match outcome {
        Ok(FeedResult::Download(Download::File(bytes))) => {
            let saved = uploads.save(
                &fetch.reply,
                &fetch.attachment.name,
                &fetch.taken,
                &bytes,
                fetch.first,
            );
            match saved {
                Ok(path) => Fetched::Saved(path),
                Err(error) => {
                    // Static codes only.
                    eprintln!("rakumux-status: saving a reply's file failed: {error:?}");
                    Fetched::Unsaved
                }
            }
        }
        Ok(FeedResult::Download(Download::Unavailable)) => Fetched::Unavailable,
        Ok(_) => Fetched::Failed(FeedFailure::Invalid),
        Err(failure) => Fetched::Failed(failure),
    }
}

/// Whether `path` is a directory itself, not a symlink to one.
fn real_dir(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_dir())
}

/// `dir` as a real directory of this user, mode 0700 (tightened when looser);
/// made when missing and `create`. A symlink or anything else is refused.
fn private_dir(dir: &Path, create: bool) -> Result<(), SaveError> {
    match fs::symlink_metadata(dir) {
        Ok(metadata) => {
            // SAFETY: geteuid has no preconditions.
            let uid = unsafe { libc::geteuid() };
            if !metadata.file_type().is_dir() || metadata.uid() != uid {
                return Err(SaveError::Refused);
            }
            if metadata.mode() & 0o777 != 0o700 {
                fs::set_permissions(dir, Permissions::from_mode(0o700))
                    .map_err(|_| SaveError::Io)?;
            }
            Ok(())
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound && create => {
            match DirBuilder::new().mode(0o700).create(dir) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(_) => return Err(SaveError::Io),
            }
            private_dir(dir, false)
        }
        Err(_) => Err(SaveError::Io),
    }
}

/// Write the whole of `bytes` to a file just made, 0600 whatever the umask;
/// a file not written whole is removed.
fn write(mut file: File, path: &Path, bytes: &[u8]) -> Result<(), SaveError> {
    let written = file
        .set_permissions(Permissions::from_mode(0o600))
        .and_then(|()| file.write_all(bytes));
    if written.is_err() {
        let _ = fs::remove_file(path);
        return Err(SaveError::Io);
    }
    Ok(())
}

/// The names a file called `name` may take beside `taken`, which none may
/// equal ignoring case (as macOS's file systems do): `name` itself, then
/// `stem (2).ext`, `stem (3).ext` and on.
fn names<'a>(name: &'a str, taken: &'a [String]) -> impl Iterator<Item = String> + 'a {
    let free = |candidate: &String| {
        let candidate = candidate.to_lowercase();
        !taken.iter().any(|name| name.to_lowercase() == candidate)
    };
    std::iter::once(name.to_owned())
        .chain((2..).map(move |n| numbered(name, n)))
        .filter(free)
}

/// `stem (n).ext`, the stem cut (at a character) so the name fits 255 bytes.
fn numbered(name: &str, n: usize) -> String {
    let (stem, extension) = match name.rfind('.') {
        Some(dot) if dot > 0 && name.len() - dot <= MAX_EXTENSION_BYTES => name.split_at(dot),
        _ => (name, ""),
    };
    let suffix = format!(" ({n}){extension}");
    let mut cut = stem.len().min(MAX_NAME_BYTES.saturating_sub(suffix.len()));
    while !stem.is_char_boundary(cut) {
        cut -= 1;
    }
    format!("{}{suffix}", &stem[..cut])
}

#[cfg(test)]
mod tests;
