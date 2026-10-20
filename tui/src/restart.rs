//! Restart only this UI through its launcher, loading the latest build.
//! The event loop detaches and restores the terminal before calling this.

use std::ffi::{OsStr, OsString};
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Replace the UI process. Returns only when the new UI could not start.
pub fn current_process(updated_launcher: Option<&Path>) -> io::Result<()> {
    let executable = match updated_launcher {
        Some(path) => path.to_path_buf(),
        None => std::env::current_exe()?,
    };
    let launcher = std::env::var_os(session_core::env::BIN).map(PathBuf::from);
    let mut command = match updated_launcher {
        Some(launcher) => updated_command(&executable, launcher, std::env::args_os().skip(1)),
        None => command(
            &executable,
            launcher.as_deref(),
            std::env::args_os().skip(1),
        ),
    };
    Err(command.exec())
}

fn command(
    executable: &Path,
    launcher: Option<&Path>,
    arguments: impl IntoIterator<Item = OsString>,
) -> Command {
    let target = launcher
        .map(Path::to_path_buf)
        .unwrap_or_else(|| replacement_executable(executable));
    let mut command = Command::new(target);
    // A UI restart must not repeat the initial daemon-replacement request.
    command.args(arguments.into_iter().filter(|arg| arg != "--new-server"));
    command
}

fn replacement_executable(executable: &Path) -> PathBuf {
    if executable.is_file() {
        return executable.to_path_buf();
    }
    // Linux appends this suffix after a rebuild replaces a running binary.
    match executable
        .as_os_str()
        .as_bytes()
        .strip_suffix(b" (deleted)")
    {
        Some(path) => PathBuf::from(OsStr::from_bytes(path)),
        None => executable.to_path_buf(),
    }
}

fn updated_command(
    executable: &Path,
    launcher: &Path,
    arguments: impl IntoIterator<Item = OsString>,
) -> Command {
    let mut command = command(executable, Some(launcher), arguments);
    if let Some(root) = launcher.parent() {
        command.env("GW_TUI_BIN", root.join("rakumux"));
        command.env("GW_PTYD_BIN", root.join("rakumux-ptyd"));
        command.env("RAKUMUX_API_BIN", root.join("rakumux-api"));
    }
    command
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uses_the_launcher_and_never_restarts_the_daemon() {
        let command = command(
            Path::new("/old/tui"),
            Some(Path::new("/new/rakumux.sh")),
            ["--new-server", "--debug", "--ptyd-path", "/custom/ptyd"].map(OsString::from),
        );
        assert_eq!(command.get_program(), "/new/rakumux.sh");
        assert_eq!(
            command.get_args().collect::<Vec<_>>(),
            ["--debug", "--ptyd-path", "/custom/ptyd"]
        );
    }

    #[test]
    fn an_updated_bundle_restarts_from_its_new_path_and_never_repeats_new_server() {
        let command = updated_command(
            Path::new("/removed/old/rakumux"),
            Path::new("/installed/rakumux.sh"),
            ["--new-server", "--debug"].map(OsString::from),
        );
        assert_eq!(command.get_program(), "/installed/rakumux.sh");
        assert_eq!(command.get_args().collect::<Vec<_>>(), ["--debug"]);
        let environment: std::collections::BTreeMap<_, _> = command.get_envs().collect();
        assert_eq!(
            environment[OsStr::new("GW_TUI_BIN")],
            Some(OsStr::new("/installed/rakumux"))
        );
    }

    #[test]
    fn direct_binary_launches_restart_the_replacement_after_a_rebuild() {
        let command = command(Path::new("/new/tui (deleted)"), None, []);
        assert_eq!(command.get_program(), "/new/tui");
    }

    #[test]
    fn existing_binary_names_are_preserved_even_with_a_deleted_suffix() {
        let directory = tempfile::tempdir().unwrap();
        let executable = directory.path().join("tui (deleted)");
        std::fs::write(&executable, b"binary").unwrap();
        let command = command(&executable, None, []);
        assert_eq!(command.get_program(), executable);
    }
}
