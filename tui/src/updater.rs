use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallMethod {
    Npm,
    Homebrew,
    Source,
    Unknown,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct UpdateState {
    pub last_check: Option<u64>,
    pub latest_version: Option<String>,
    pub pending_upgrade: bool,
    pub install_method: Option<String>,
}

impl Default for UpdateState {
    fn default() -> Self {
        Self {
            last_check: None,
            latest_version: None,
            pending_upgrade: false,
            install_method: None,
        }
    }
}

impl UpdateState {
    fn state_file_path() -> Result<PathBuf> {
        let home_dir = std::env::var("HOME")
            .map_err(|_| anyhow::anyhow!("HOME environment variable not set"))?;
        Ok(PathBuf::from(home_dir)
            .join(".rakumux")
            .join("update_state.json"))
    }

    pub fn load() -> Result<Self> {
        let path = Self::state_file_path()?;
        if path.exists() {
            let content = fs::read_to_string(&path)?;
            let state: UpdateState = serde_json::from_str(&content)?;
            Ok(state)
        } else {
            Ok(UpdateState::default())
        }
    }

    pub fn save(&self) -> Result<()> {
        let path = Self::state_file_path()?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let content = serde_json::to_string_pretty(self)?;
        fs::write(&path, content)?;
        Ok(())
    }

    pub fn mark_update_available(&mut self, version: String, method: InstallMethod) {
        self.latest_version = Some(version);
        self.pending_upgrade = true;
        self.install_method = Some(method.to_string());
        let _ = self.save();
    }

    pub fn clear_pending_upgrade(&mut self) {
        self.pending_upgrade = false;
        let _ = self.save();
    }

    pub fn update_last_check(&mut self) {
        self.last_check = Some(
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_secs(),
        );
        let _ = self.save();
    }

    pub fn should_check_for_updates(&self) -> bool {
        if let Some(last) = self.last_check {
            let now = SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_secs();
            let elapsed = Duration::from_secs(now - last);
            elapsed > Duration::from_secs(24 * 3600)
        } else {
            true
        }
    }
}

impl InstallMethod {
    fn to_string(&self) -> String {
        match self {
            InstallMethod::Npm => "npm".to_string(),
            InstallMethod::Homebrew => "homebrew".to_string(),
            InstallMethod::Source => "source".to_string(),
            InstallMethod::Unknown => "unknown".to_string(),
        }
    }

    pub fn from_string(s: &str) -> Self {
        match s {
            "npm" => InstallMethod::Npm,
            "homebrew" => InstallMethod::Homebrew,
            "source" => InstallMethod::Source,
            _ => InstallMethod::Unknown,
        }
    }
}

pub fn detect_install_method() -> InstallMethod {
    // The running binary's real location is the most reliable signal;
    // `which rakumux` is a fallback. Both are canonicalised because
    // Homebrew and npm put symlinks on PATH.
    let mut candidates = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        candidates.push(exe.canonicalize().unwrap_or(exe));
    }
    if let Ok(output) = Command::new("which").arg("rakumux").output() {
        if output.status.success() {
            let path = PathBuf::from(String::from_utf8_lossy(&output.stdout).trim());
            candidates.push(path.canonicalize().unwrap_or(path));
        }
    }
    candidates
        .iter()
        .find_map(|path| install_method_for_path(path))
        .unwrap_or(InstallMethod::Unknown)
}

/// How a binary at `path` (canonical) was installed, if recognisable.
fn install_method_for_path(path: &Path) -> Option<InstallMethod> {
    let path = path.to_string_lossy();
    if path.contains("/node_modules/") || path.contains("/.npm/") || path.contains("/.nvm/") {
        Some(InstallMethod::Npm)
    } else if path.contains("/Cellar/")
        || path.contains("/Homebrew/")
        || path.contains("/.linuxbrew/")
    {
        Some(InstallMethod::Homebrew)
    } else if path.contains("/target/release/") || path.contains("/target/debug/") {
        Some(InstallMethod::Source)
    } else {
        None
    }
}

pub fn get_latest_version(method: InstallMethod) -> Result<String> {
    match method {
        InstallMethod::Npm => {
            let output = Command::new("npm")
                .args(["view", "rakumux", "version"])
                .output()?;

            if output.status.success() {
                let version = String::from_utf8_lossy(&output.stdout);
                Ok(version.trim().to_string())
            } else {
                Err(anyhow::anyhow!("Failed to get npm version"))
            }
        }
        InstallMethod::Homebrew => {
            let output = Command::new("brew")
                .args(["info", "--json", "rakumux"])
                .output()?;

            if output.status.success() {
                let json_str = String::from_utf8_lossy(&output.stdout);
                if let Ok(json) = serde_json::from_str::<serde_json::Value>(&json_str) {
                    if let Some(versions) = json[0]["versions"]["stable"].as_str() {
                        return Ok(versions.to_string());
                    }
                }
            }
            Err(anyhow::anyhow!("Failed to get brew version"))
        }
        _ => Err(anyhow::anyhow!(
            "Unsupported install method for update checking"
        )),
    }
}

pub fn compare_versions(current: &str, latest: &str) -> std::cmp::Ordering {
    let current_parts: Vec<u32> = current
        .trim_start_matches('v')
        .split('.')
        .filter_map(|s| s.parse().ok())
        .collect();

    let latest_parts: Vec<u32> = latest
        .trim_start_matches('v')
        .split('.')
        .filter_map(|s| s.parse().ok())
        .collect();

    for i in 0..std::cmp::max(current_parts.len(), latest_parts.len()) {
        let current_part = current_parts.get(i).copied().unwrap_or(0);
        let latest_part = latest_parts.get(i).copied().unwrap_or(0);

        match current_part.cmp(&latest_part) {
            std::cmp::Ordering::Less => return std::cmp::Ordering::Less,
            std::cmp::Ordering::Greater => return std::cmp::Ordering::Greater,
            std::cmp::Ordering::Equal => continue,
        }
    }

    std::cmp::Ordering::Equal
}

pub async fn check_for_updates(current_version: &str) -> Result<Option<(String, InstallMethod)>> {
    let method = detect_install_method();

    if matches!(method, InstallMethod::Source | InstallMethod::Unknown) {
        return Ok(None);
    }

    match get_latest_version(method) {
        Ok(latest) => {
            if compare_versions(current_version, &latest) == std::cmp::Ordering::Less {
                Ok(Some((latest, method)))
            } else {
                Ok(None)
            }
        }
        Err(_) => Ok(None),
    }
}

pub fn perform_upgrade(method: InstallMethod) -> Result<()> {
    let status = match method {
        InstallMethod::Npm => Command::new("npm")
            .args(["install", "-g", "rakumux"])
            .status()?,
        InstallMethod::Homebrew => Command::new("brew").args(["upgrade", "rakumux"]).status()?,
        _ => return Err(anyhow::anyhow!("Unsupported install method for upgrades")),
    };

    if status.success() {
        Ok(())
    } else {
        Err(anyhow::anyhow!(
            "Upgrade command exited with status {}",
            status
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn install_method_comes_from_the_resolved_binary_path() {
        let method = |p: &str| install_method_for_path(Path::new(p));
        // Homebrew's bin/rakumux symlink resolves into the Cellar.
        assert_eq!(
            method("/opt/homebrew/Cellar/rakumux/0.6.0/libexec/rakumux-bin"),
            Some(InstallMethod::Homebrew)
        );
        assert_eq!(
            method("/home/u/.linuxbrew/Cellar/rakumux/0.6.0/libexec/rakumux-bin"),
            Some(InstallMethod::Homebrew)
        );
        assert_eq!(
            method("/usr/lib/node_modules/rakumux/resources/prebuilt/tui"),
            Some(InstallMethod::Npm)
        );
        assert_eq!(
            method("/home/u/src/rakumux/tui/target/release/tui"),
            Some(InstallMethod::Source)
        );
        assert_eq!(method("/usr/local/bin/rakumux"), None);
    }

    #[test]
    fn test_version_comparison() {
        assert_eq!(
            compare_versions("0.5.35", "0.5.38"),
            std::cmp::Ordering::Less
        );
        assert_eq!(
            compare_versions("0.5.38", "0.5.35"),
            std::cmp::Ordering::Greater
        );
        assert_eq!(
            compare_versions("0.5.38", "0.5.38"),
            std::cmp::Ordering::Equal
        );
        assert_eq!(
            compare_versions("1.0.0", "0.9.9"),
            std::cmp::Ordering::Greater
        );
        assert_eq!(
            compare_versions("v0.5.35", "0.5.38"),
            std::cmp::Ordering::Less
        );
    }

    #[test]
    fn test_update_state_roundtrip() {
        let mut state = UpdateState::default();
        state.latest_version = Some("0.5.40".to_string());
        state.pending_upgrade = true;
        state.install_method = Some("npm".to_string());

        assert_eq!(state.latest_version.as_deref(), Some("0.5.40"));
        assert_eq!(state.pending_upgrade, true);
    }
}
