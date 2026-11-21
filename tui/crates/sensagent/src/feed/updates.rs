//! Releases on rakumux.com (`contracts/feed/` `release`). Every replies read
//! may say the newest stable release rakumux.com knows of (`release`), which
//! the service passes to TUIs with their feed pages so they offer it at once.
//!
//! A machine updates only from itself (`U`, automatic updates, the installer):
//! the service never says `remote_update` and never reads the `updates` a
//! replies read may carry, so nobody who can write to rakumux.com can have a
//! machine install anything.

use serde::Deserialize;

/// `version` is a stable release's (`major.minor.patch`, digits only): the
/// contract's `rakumuxVersion`.
pub(crate) fn is_version(version: &str) -> bool {
    let parts: Vec<&str> = version.split('.').collect();
    parts.len() == 3
        && parts.iter().all(|part| {
            !part.is_empty()
                && part.len() <= 6
                && part.bytes().all(|byte| byte.is_ascii_digit())
                && (part.len() == 1 || !part.starts_with('0'))
        })
}

/// The replies read's `release`: its version, when it fits the contract.
pub(crate) fn release(value: Option<serde_json::Value>) -> Option<String> {
    #[derive(Deserialize)]
    struct Release {
        version: String,
    }
    let release: Release = serde_json::from_value(value?).ok()?;
    is_version(&release.version).then_some(release.version)
}

#[cfg(test)]
mod tests;
