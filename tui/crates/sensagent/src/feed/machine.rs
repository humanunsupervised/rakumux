//! This machine as the feed knows it: an id it keeps (a ULID, sent with every
//! request as `X-Rakumux-Machine-Id`) and its name, kept in a file (0600).
//! The name is the hostname when the file is made, and stays: a hostname that
//! changes (macOS takes one from the network) would leave the replies to the
//! old name with nobody to read them. Two of the account's machines can still
//! share a name (two laptops with the default one, two Rakumux homes on one
//! machine): the first to send its session list under it holds it, and
//! rakumux.com refuses the other (`machine_taken`), which takes the name with
//! a few characters of its id after it.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::sync::machine_name;

/// The longest name the feed contract takes.
const MAX_NAME: usize = 64;

#[derive(Serialize, Deserialize)]
struct MachineFile {
    version: u8,
    id: String,
    /// The hostname when the file was made.
    base: String,
    /// How often rakumux.com said another machine holds the name.
    #[serde(default)]
    renames: u32,
}

/// This machine's id and name for the feed.
pub(crate) struct MachineIdentity {
    path: Option<PathBuf>,
    file: MachineFile,
}

impl MachineIdentity {
    /// The identity kept in `path`, made (and kept there) when there's none
    /// yet; memory only without a path.
    pub fn load(path: Option<PathBuf>) -> Self {
        let kept = path
            .as_ref()
            .and_then(|path| std::fs::read(path).ok())
            .and_then(|bytes| serde_json::from_slice::<MachineFile>(&bytes).ok())
            .filter(|file| {
                file.version == 1
                    && crate::feed::is_entry_id(&file.id)
                    && !file.base.is_empty()
                    && file.base.chars().count() <= MAX_NAME
            });
        let made = kept.is_none();
        let file = kept.unwrap_or_else(|| MachineFile {
            version: 1,
            id: session_core::ids::SessionId::generate().as_str().to_owned(),
            base: machine_name(),
            renames: 0,
        });
        let identity = Self { path, file };
        if made {
            identity.save();
        }
        identity
    }

    /// The id rakumux.com tells this machine apart by.
    pub fn id(&self) -> &str {
        &self.file.id
    }

    /// The name it goes by: the hostname it had, then a few characters of its
    /// id after it once another machine held that.
    pub fn name(&self) -> String {
        if self.file.renames == 0 {
            return self.file.base.clone();
        }
        // Lowercase, so it reads as a suffix; longer with each refusal.
        let length = (2 + 2 * self.file.renames as usize).min(self.file.id.len());
        let suffix = self.file.id[self.file.id.len() - length..].to_lowercase();
        let base: String = self
            .file
            .base
            .chars()
            .take(MAX_NAME - suffix.len() - 1)
            .collect();
        format!("{base}-{suffix}")
    }

    /// rakumux.com said another machine holds the name: take the next one,
    /// and keep it.
    pub fn rename(&mut self) -> String {
        self.file.renames = self.file.renames.saturating_add(1);
        self.save();
        self.name()
    }

    fn save(&self) {
        let Some(path) = &self.path else {
            return;
        };
        let Ok(bytes) = serde_json::to_vec(&self.file) else {
            return;
        };
        if crate::feed::write_private(path, &bytes).is_err() {
            eprintln!("rakumux-status: saving this machine's feed identity failed");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_identity_is_made_once_and_kept() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("machine.json");
        let first = MachineIdentity::load(Some(path.clone()));
        assert!(crate::feed::is_entry_id(first.id()));
        assert_eq!(first.name(), machine_name());
        let again = MachineIdentity::load(Some(path.clone()));
        assert_eq!((again.id(), again.name()), (first.id(), first.name()));
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    #[test]
    fn a_name_another_machine_holds_takes_a_longer_suffix_of_the_id_each_time() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("machine.json");
        std::fs::write(
            &path,
            r#"{"version":1,"id":"01K8A0RZ0V3X5Q2N9T7W4Y6M8B","base":"studio"}"#,
        )
        .unwrap();
        let mut identity = MachineIdentity::load(Some(path.clone()));
        assert_eq!(identity.name(), "studio");
        assert_eq!(identity.rename(), "studio-6m8b");
        assert_eq!(identity.rename(), "studio-4y6m8b");
        // Kept: a restart goes by the same name.
        assert_eq!(MachineIdentity::load(Some(path)).name(), "studio-4y6m8b");
    }

    #[test]
    fn a_long_hostname_leaves_room_for_the_suffix() {
        let mut identity = MachineIdentity::load(None);
        identity.file.base = "h".repeat(MAX_NAME);
        let name = identity.rename();
        assert_eq!(name.chars().count(), MAX_NAME);
        assert!(name.ends_with(&identity.id()[22..].to_lowercase()));
    }
}
