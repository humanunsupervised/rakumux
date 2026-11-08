//! Release checks and verified updates. Never starts, stops or restarts the daemon.
mod binary;
mod releases;
mod signature;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::SystemTime;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallMethod {
    Binary,
    Npm,
    Homebrew,
    Source,
    Unknown,
}

impl InstallMethod {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Binary => "binary",
            Self::Npm => "npm",
            Self::Homebrew => "homebrew",
            Self::Source => "source",
            Self::Unknown => "unknown",
        }
    }
}

/// Another window is installing an update now: the installation's lock is
/// held. It isn't a failure; that window's install is the one that counts.
#[derive(Debug)]
pub struct UpdateBusy;

impl std::fmt::Display for UpdateBusy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("another update is already running")
    }
}

impl std::error::Error for UpdateBusy {}

impl UpdateBusy {
    /// Whether `error` (or what it wraps) says another update runs.
    pub fn is(error: &anyhow::Error) -> bool {
        error.chain().any(|cause| cause.is::<UpdateBusy>())
    }
}

/// Application port: tests cannot download or replace the user's installed app.
pub trait UpdateService: Send + Sync {
    /// How this app was installed, which decides how it updates.
    fn method(&self) -> InstallMethod;
    fn check(&self, current: &str) -> Result<Option<(String, InstallMethod)>>;
    /// The replacement launcher, when installation changed the running executable's path.
    /// Installing the version the installation already has changes nothing and succeeds.
    fn install(&self, version: &str, method: InstallMethod) -> Result<Option<PathBuf>>;
    /// What's new in `version`: its release notes (Markdown), when known.
    fn release_notes(&self, _version: &str) -> Result<Option<String>> {
        Ok(None)
    }
}

pub struct SystemUpdateService;

impl UpdateService for SystemUpdateService {
    fn method(&self) -> InstallMethod {
        detect_install_method()
    }
    fn check(&self, current: &str) -> Result<Option<(String, InstallMethod)>> {
        let method = detect_install_method();
        let latest = get_latest_version(method)?;
        Ok((compare_versions(current, &latest)? == Ordering::Less).then_some((latest, method)))
    }
    fn install(&self, version: &str, method: InstallMethod) -> Result<Option<PathBuf>> {
        let version = releases::stable_version(version)?.to_string();
        if method == InstallMethod::Binary {
            let root = binary::install_root(&executable()?)
                .context("binary installation layout is not recognized")?;
            let platform = binary::platform(std::env::consts::OS, std::env::consts::ARCH)?;
            return binary::install(
                &releases::GithubReleases::new()?,
                &signature::release_keys()?,
                &root,
                &version,
                platform,
            )
            .map(Some);
        }
        let output = match method {
            InstallMethod::Npm => Command::new("npm").args(["install", "-g", &format!("rakumux@{version}")]).stdin(Stdio::null()).output()?,
            InstallMethod::Homebrew => Command::new("brew").args(["upgrade", "rakumux"]).stdin(Stdio::null()).output()?,
            _ => bail!("This installation cannot update automatically. Rebuild a source checkout and press R, or rerun the binary installer."),
        };
        if !output.status.success() {
            bail!("package manager could not update Rakumux; update manually outside the app (administrator permission may be required)");
        }
        Ok(None)
    }
    fn release_notes(&self, version: &str) -> Result<Option<String>> {
        releases::notes(&releases::GithubReleases::new()?, version)
    }
}

/// `update_state.json`, shared by every window of a Rakumux home.
#[derive(Debug, Default, Serialize, Deserialize, Clone)]
#[serde(default)]
pub struct UpdateState {
    pub last_check: Option<u64>,
    pub latest_version: Option<String>,
    /// A newer release is out. Kept after a window installs it, so the
    /// other windows still running the older one learn of it; each compares
    /// it with its own version. Installed without asking only by a binary
    /// installation with `sessions.auto_update` on.
    pub pending_upgrade: bool,
    /// How the window that found it is installed. Not read: each window
    /// goes by its own installation (`UpdateService::method`).
    pub install_method: Option<String>,
    /// The newest version the user was told about, so neither another window
    /// nor a restart tells them again.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notified_version: Option<String>,
    /// An automatic install of this version failed: not tried again until
    /// the next check finds it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failed_version: Option<String>,
    /// The user answered `Later` to this version's update modal: it doesn't
    /// open by itself for it again (the footer still says it's out).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dismissed_version: Option<String>,
    /// The user answered `Later` to restarting into this installed version.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dismissed_installed: Option<String>,
}

impl UpdateState {
    pub fn load_from(path: &Path) -> Result<Self> {
        match fs::read(path) {
            Ok(bytes) => Ok(serde_json::from_slice(&bytes)?),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(error.into()),
        }
    }
    pub fn save_to(&self, path: &Path) -> Result<()> {
        use std::io::Write;
        let parent = path.parent().context("update state has no parent")?;
        fs::create_dir_all(parent)?;
        let mut file = tempfile::NamedTempFile::new_in(parent)?;
        serde_json::to_writer_pretty(&mut file, self)?;
        file.write_all(b"\n")?;
        file.as_file().sync_all()?;
        file.persist(path)?;
        Ok(())
    }
    pub fn mark_update_available(&mut self, version: String, method: InstallMethod) {
        self.latest_version = Some(version);
        self.pending_upgrade = true;
        self.install_method = Some(method.as_str().into());
    }
    /// A check found nothing newer than `current`. A newer version on record
    /// is stale (withdrawn) and goes; one `current` already has stays for
    /// the windows still running an older one, which install it too.
    pub fn no_update_for(&mut self, current: &str) {
        if self
            .latest_version
            .as_deref()
            .is_none_or(|latest| is_newer(current, latest))
        {
            self.pending_upgrade = false;
            self.latest_version = None;
            self.install_method = None;
        }
    }
    pub fn update_last_check(&mut self) {
        self.last_check = Some(now());
    }
    pub fn should_check_for_updates(&self) -> bool {
        self.last_check
            .is_none_or(|last| now().saturating_sub(last) > CHECK_EVERY_SECS)
    }
}

/// How often one window of a home checks GitHub for a release: hourly (a
/// signed-in machine also hears of one from rakumux.com as it's published).
pub const CHECK_EVERY_SECS: u64 = 3600;

fn now() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn executable() -> Result<PathBuf> {
    let current = std::env::current_exe()?;
    if let Ok(path) = current.canonicalize() {
        return Ok(path);
    }
    // Another TUI may have updated this bundle while this instance was running.
    for name in ["GW_TUI_BIN", session_core::env::BIN] {
        if let Some(path) = std::env::var_os(name).map(PathBuf::from) {
            let path = if name == session_core::env::BIN {
                path.parent().map(|parent| parent.join("rakumux"))
            } else {
                Some(path)
            };
            if let Some(path) = path.and_then(|path| path.canonicalize().ok()) {
                return Ok(path);
            }
        }
    }
    Ok(current)
}

pub fn detect_install_method() -> InstallMethod {
    executable()
        .ok()
        .and_then(|path| install_method_for_path(&path))
        .unwrap_or(InstallMethod::Unknown)
}

fn install_method_for_path(path: &Path) -> Option<InstallMethod> {
    let text = path.to_string_lossy();
    if text.contains("/node_modules/") || text.contains("/.npm/") || text.contains("/.nvm/") {
        Some(InstallMethod::Npm)
    } else if text.contains("/Cellar/")
        || text.contains("/Homebrew/")
        || text.contains("/.linuxbrew/")
    {
        Some(InstallMethod::Homebrew)
    } else if text.contains("/target/release/") || text.contains("/target/debug/") {
        Some(InstallMethod::Source)
    } else if binary::install_root(path).is_some() {
        Some(InstallMethod::Binary)
    } else {
        None
    }
}

pub fn get_latest_version(method: InstallMethod) -> Result<String> {
    match method {
        InstallMethod::Binary => releases::latest(&releases::GithubReleases::new()?),
        InstallMethod::Npm | InstallMethod::Homebrew => {
            let output = if method == InstallMethod::Npm {
                Command::new("npm").args(["view", "rakumux", "version"]).stdin(Stdio::null()).output()?
            } else { Command::new("brew").args(["info", "--json=v2", "rakumux"]).stdin(Stdio::null()).output()? };
            if !output.status.success() { bail!("could not check the installed package channel"); }
            let version = if method == InstallMethod::Npm { String::from_utf8(output.stdout)?.trim().to_string() }
                else { serde_json::from_slice::<serde_json::Value>(&output.stdout)?["formulae"][0]["versions"]["stable"].as_str().context("Homebrew returned no stable version")?.to_string() };
            Ok(releases::stable_version(&version)?.to_string())
        }
        // Only to say a release is out: a source build is never overwritten.
        InstallMethod::Source => releases::latest(&releases::GithubReleases::new()?),
        InstallMethod::Unknown => bail!("Installation not recognized. Update by rerunning https://rakumux.com/install.sh with your original PREFIX."),
    }
}

pub fn compare_versions(current: &str, latest: &str) -> Result<Ordering> {
    let current = semver::Version::parse(current.strip_prefix('v').unwrap_or(current))?;
    let latest = releases::stable_version(latest)?;
    Ok(current.cmp(&latest))
}

/// `candidate` is a stable release newer than `current`: never a
/// prerelease, the same version or an older one.
pub fn is_newer(current: &str, candidate: &str) -> bool {
    compare_versions(current, candidate).is_ok_and(Ordering::is_lt)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn install_method_comes_from_the_resolved_binary_path() {
        for (path, method) in [
            (
                "/opt/homebrew/Cellar/rakumux/0.6.0/libexec/rakumux",
                InstallMethod::Homebrew,
            ),
            (
                "/home/u/.linuxbrew/Cellar/rakumux/0.6.0/libexec/rakumux",
                InstallMethod::Homebrew,
            ),
            (
                "/usr/lib/node_modules/rakumux/resources/prebuilt/tui",
                InstallMethod::Npm,
            ),
            (
                "/home/u/src/rakumux/tui/target/release/tui",
                InstallMethod::Source,
            ),
        ] {
            assert_eq!(install_method_for_path(Path::new(path)), Some(method));
        }
        assert_eq!(
            install_method_for_path(Path::new("/usr/local/bin/rakumux")),
            None
        );
    }
    #[test]
    fn versions_do_not_offer_downgrades_or_silently_accept_invalid_versions() {
        assert_eq!(compare_versions("v0.1.4", "0.1.5").unwrap(), Ordering::Less);
        assert_eq!(
            compare_versions("0.1.5-dev", "0.1.4").unwrap(),
            Ordering::Greater
        );
        assert_eq!(
            compare_versions("0.1.5-dev", "0.1.5").unwrap(),
            Ordering::Less
        );
        assert_eq!(compare_versions("0.1.5", "0.1.5").unwrap(), Ordering::Equal);
        assert!(compare_versions("garbage", "0.1.5").is_err());
        assert!(compare_versions("0.1.4", "../../bad").is_err());
        assert!(is_newer("0.1.4", "0.1.5"));
        for candidate in ["0.1.4", "0.1.3", "0.1.6-rc.1", "0.1.6+build", "garbage"] {
            assert!(!is_newer("0.1.4", candidate), "{candidate}");
        }
    }
    #[test]
    fn update_state_is_private_and_checked_at_most_daily() {
        use std::os::unix::fs::PermissionsExt;
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("update_state.json");
        let mut state = UpdateState::default();
        state.mark_update_available("0.1.5".into(), InstallMethod::Binary);
        state.save_to(&path).unwrap();
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert!(UpdateState::load_from(&path).unwrap().pending_upgrade);
        state.last_check = Some(u64::MAX);
        assert!(!state.should_check_for_updates());
    }
    #[test]
    fn a_release_this_version_has_stays_on_record_for_older_windows() {
        let mut state = UpdateState::default();
        state.mark_update_available("0.1.5".into(), InstallMethod::Binary);
        state.no_update_for("0.1.5");
        assert!(state.pending_upgrade);
        assert_eq!(state.latest_version.as_deref(), Some("0.1.5"));
        // Newer than the version that found nothing: withdrawn.
        state.no_update_for("0.1.4");
        assert!(!state.pending_upgrade);
        assert_eq!(state.latest_version, None);
    }
    #[test]
    fn notified_and_failed_versions_are_kept_and_older_files_still_read() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("update_state.json");
        fs::write(&path, br#"{"last_check":1,"latest_version":"0.1.5","pending_upgrade":true,"install_method":"binary"}"#).unwrap();
        let mut state = UpdateState::load_from(&path).unwrap();
        assert_eq!(state.notified_version, None);
        state.notified_version = Some("0.1.5".into());
        state.failed_version = Some("0.1.5".into());
        state.save_to(&path).unwrap();
        let read = UpdateState::load_from(&path).unwrap();
        assert_eq!(read.notified_version.as_deref(), Some("0.1.5"));
        assert_eq!(read.failed_version.as_deref(), Some("0.1.5"));
    }
}
