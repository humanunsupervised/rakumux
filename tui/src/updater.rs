//! Release checks and verified updates. Never starts, stops or restarts the daemon.
mod binary;
mod releases;

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
    pub fn from_string(value: &str) -> Self {
        match value {
            "binary" => Self::Binary,
            "npm" => Self::Npm,
            "homebrew" => Self::Homebrew,
            "source" => Self::Source,
            _ => Self::Unknown,
        }
    }
}

/// Application port: tests cannot download or replace the user's installed app.
pub trait UpdateService: Send + Sync {
    fn check(&self, current: &str) -> Result<Option<(String, InstallMethod)>>;
    /// The replacement launcher, when installation changed the running executable's path.
    fn install(&self, version: &str, method: InstallMethod) -> Result<Option<PathBuf>>;
}

pub struct SystemUpdateService;

impl UpdateService for SystemUpdateService {
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
            return binary::install(&releases::GithubReleases::new()?, &root, &version, platform)
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
}

#[derive(Debug, Default, Serialize, Deserialize, Clone)]
#[serde(default)]
pub struct UpdateState {
    pub last_check: Option<u64>,
    pub latest_version: Option<String>,
    /// An available update, never permission to install automatically on startup.
    pub pending_upgrade: bool,
    pub install_method: Option<String>,
}

impl UpdateState {
    fn path() -> Result<PathBuf> {
        Ok(session_core::paths::Layout::from_env()?
            .home
            .join("update_state.json"))
    }
    pub fn load() -> Result<Self> {
        Self::load_from(&Self::path()?)
    }
    pub fn load_from(path: &Path) -> Result<Self> {
        match fs::read(path) {
            Ok(bytes) => Ok(serde_json::from_slice(&bytes)?),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(error.into()),
        }
    }
    pub fn save(&self) -> Result<()> {
        self.save_to(&Self::path()?)
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
    pub fn clear_pending_upgrade(&mut self) {
        self.pending_upgrade = false;
        self.latest_version = None;
        self.install_method = None;
    }
    pub fn update_last_check(&mut self) {
        self.last_check = Some(now());
    }
    pub fn should_check_for_updates(&self) -> bool {
        self.last_check
            .is_none_or(|last| now().saturating_sub(last) > 24 * 3600)
    }
}

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
        InstallMethod::Source => bail!("Source build: rebuild your checkout with ./build.sh, then press R to reload the UI. Update will not overwrite local source builds."),
        InstallMethod::Unknown => bail!("Installation not recognized. Update by rerunning https://rakumux.com/install.sh with your original PREFIX."),
    }
}

pub fn compare_versions(current: &str, latest: &str) -> Result<Ordering> {
    let current = semver::Version::parse(current.strip_prefix('v').unwrap_or(current))?;
    let latest = releases::stable_version(latest)?;
    Ok(current.cmp(&latest))
}

pub async fn check_for_updates(current: &str) -> Result<Option<(String, InstallMethod)>> {
    let current = current.to_string();
    tokio::task::spawn_blocking(move || SystemUpdateService.check(&current)).await?
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
    }
    #[test]
    fn available_updates_are_not_startup_install_authorization() {
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
}
