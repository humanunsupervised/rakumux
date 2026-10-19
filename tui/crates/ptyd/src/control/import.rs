//! Saved-session archive files. The control service owns the registry
//! transaction; this guard removes only files it created if that fails.

use super::registry::SessionRecord;
use session_core::paths::Layout;
use std::fs::{self, DirBuilder, File, OpenOptions};
use std::io;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

pub(super) struct StagedFiles {
    files: Vec<PathBuf>,
    directories: Vec<PathBuf>,
    committed: bool,
}

impl StagedFiles {
    pub fn copy(source: &Path, target: &Layout, records: &[SessionRecord]) -> io::Result<Self> {
        let source = source.canonicalize()?;
        let mut staged = Self {
            files: Vec::new(),
            directories: Vec::new(),
            committed: false,
        };
        for record in records {
            crate::persist::validate_id(&record.id)?;
            let directory = target.session_dir(&record.id);
            private_dir(&target.home.join("sessions"))?;
            DirBuilder::new().mode(0o700).create(&directory)?;
            staged.directories.push(directory.clone());
            for name in ["scrollback.ansi", "meta.json"] {
                staged.copy_file(
                    &source,
                    &source.join("sessions").join(record.id.as_str()).join(name),
                    &directory.join(name),
                )?;
            }
            let history = target.history_file(&record.id);
            private_dir(&target.home.join("history"))?;
            staged.copy_file(
                &source,
                &source.join("history").join(format!("{}.log", record.id)),
                &history,
            )?;
        }
        Ok(staged)
    }

    fn copy_file(&mut self, root: &Path, source: &Path, target: &Path) -> io::Result<()> {
        let metadata = match fs::symlink_metadata(source) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error),
        };
        if !metadata.is_file() || !source.canonicalize()?.starts_with(root) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "archive file is not confined to its directory: {}",
                    source.display()
                ),
            ));
        }
        let mut input = File::open(source)?;
        let mut output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(target)?;
        self.files.push(target.to_path_buf());
        io::copy(&mut input, &mut output)?;
        output.sync_all()
    }

    pub fn commit(mut self) {
        self.committed = true;
    }
}

fn private_dir(path: &Path) -> io::Result<()> {
    DirBuilder::new().recursive(true).mode(0o700).create(path)
}

impl Drop for StagedFiles {
    fn drop(&mut self) {
        if self.committed {
            return;
        }
        for path in self.files.iter().rev() {
            if let Err(error) = fs::remove_file(path) {
                crate::log_warn!("import rollback: {}: {error}", path.display());
            }
        }
        for path in self.directories.iter().rev() {
            if let Err(error) = fs::remove_dir(path) {
                crate::log_warn!("import rollback: {}: {error}", path.display());
            }
        }
    }
}
