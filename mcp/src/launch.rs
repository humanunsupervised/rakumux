//! Starting `orchestra-api` when no healthy API runs, the way the CLI starts
//! the daemon: `orchestra-api --detach`, which returns once the API answers.

use crate::client::ToolError;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

/// Longest `orchestra-api --detach` may take (it waits up to 10 s itself).
const START_TIMEOUT: Duration = Duration::from_secs(20);

/// How to find `orchestra-api`.
#[derive(Debug, Clone, Default)]
pub struct Launcher {
    /// `$ORCHESTRA_API_BIN`, when set.
    pub explicit: Option<PathBuf>,
}

impl Launcher {
    pub fn from_env() -> Self {
        Self {
            explicit: std::env::var_os(orchestra_api::env::API_BIN)
                .filter(|value| !value.is_empty())
                .map(PathBuf::from),
        }
    }

    /// `$ORCHESTRA_API_BIN`, else `orchestra-api` next to this executable
    /// (symlinks resolved, then as invoked), else on `PATH`.
    pub fn binary(&self) -> Result<PathBuf, ToolError> {
        if let Some(path) = &self.explicit {
            return if is_executable(path) {
                Ok(path.clone())
            } else {
                Err(ToolError::api_unavailable(format!(
                    "{}={} is not an executable file",
                    orchestra_api::env::API_BIN,
                    path.display()
                )))
            };
        }
        let exe = std::env::current_exe().ok();
        let siblings = [
            exe.as_ref().and_then(|exe| std::fs::canonicalize(exe).ok()),
            exe.clone(),
        ];
        for exe in siblings.into_iter().flatten() {
            if let Some(candidate) = exe.parent().map(|dir| dir.join("orchestra-api")) {
                if is_executable(&candidate) {
                    return Ok(candidate);
                }
            }
        }
        std::env::var_os("PATH")
            .and_then(|paths| {
                std::env::split_paths(&paths)
                    .map(|dir| dir.join("orchestra-api"))
                    .find(|candidate| is_executable(candidate))
            })
            .ok_or_else(|| {
                ToolError::api_unavailable(format!(
                    "orchestra-api not found: install it next to orchestra-mcp, put it on PATH \
                     or set {}",
                    orchestra_api::env::API_BIN
                ))
            })
    }

    /// Run `orchestra-api --detach` and wait for it. Its stdout never
    /// reaches ours (that is the MCP channel).
    pub async fn start(&self) -> Result<(), ToolError> {
        let binary = self.binary()?;
        let mut command = tokio::process::Command::new(&binary);
        command
            .arg("--detach")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        // The API is shared by every caller; identity travels per request.
        for name in [
            session_core::env::SESSION_ID,
            session_core::env::SESSION_TOKEN,
            session_core::env::SOCKET,
        ] {
            command.env_remove(name);
        }
        let output = tokio::time::timeout(START_TIMEOUT, command.output())
            .await
            .map_err(|_| {
                ToolError::api_unavailable(format!(
                    "{} --detach did not finish within {} s",
                    binary.display(),
                    START_TIMEOUT.as_secs()
                ))
            })?
            .map_err(|error| {
                ToolError::api_unavailable(format!("cannot run {}: {error}", binary.display()))
            })?;
        if output.status.success() {
            return Ok(());
        }
        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(ToolError::api_unavailable(format!(
            "{} --detach failed ({}): {}",
            binary.display(),
            output.status,
            stderr.trim()
        )))
    }
}

fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .map(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_explicit_binary_must_be_executable() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("orchestra-api");
        std::fs::write(&file, "#!/bin/sh\n").unwrap();
        let launcher = Launcher {
            explicit: Some(file.clone()),
        };
        let error = launcher.binary().unwrap_err();
        assert_eq!(error.code, "api_unavailable");
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(launcher.binary().unwrap(), file);
    }
}
