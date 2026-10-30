//! The Rakumux extension for Pi (bundled from
//! `integrations/pi/rakumux-session/`) and its installer.
//!
//! Inside a Rakumux session the extension reports what Pi does (activity v1),
//! binds the conversation Pi shows to the session (`rakumux session bind`) and
//! names that conversation's folder, so the daemon follows, captures and
//! resumes the right conversation. Pi's MCP servers run inside its own
//! process, with the session's environment, so `rakumux-mcp` needs nothing
//! from the extension.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

/// The extension's source: one module, loaded by Pi as TypeScript.
pub const EXTENSION: &str = concat!(
    include_str!("rakumux-session/activity.mjs"),
    "\n",
    include_str!("rakumux-session/runtime.mjs")
);
/// The extension's file under `<agent dir>/extensions/`, where Pi discovers
/// it without configuration.
pub const EXTENSION_FILE: &str = "rakumux-session.ts";

/// Pi's agent directory: `$PI_CODING_AGENT_DIR`, else `~/.pi/agent`.
pub fn agent_dir() -> Option<PathBuf> {
    let var = |name: &str| std::env::var_os(name).filter(|value| !value.is_empty());
    let home = || var("HOME").map(PathBuf::from);
    match var("PI_CODING_AGENT_DIR").map(PathBuf::from) {
        Some(dir) => match dir.strip_prefix("~") {
            Ok(rest) => Some(home()?.join(rest)),
            Err(_) => Some(dir),
        },
        None => Some(home()?.join(".pi/agent")),
    }
}

/// Where [`install`] puts the extension for the agent directory `agent`.
pub fn extension_path(agent: &Path) -> PathBuf {
    agent.join("extensions").join(EXTENSION_FILE)
}

/// Write (or overwrite) the extension under `agent`; returns its path. No Pi
/// setting is changed.
pub fn install(agent: &Path) -> Result<PathBuf, String> {
    let path = extension_path(agent);
    let dir = path.parent().expect("extension path has a parent");
    std::fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    std::fs::write(&path, EXTENSION)
        .map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    Ok(path)
}

/// Handle `pi-plugin [--install [--dir DIR]]`.
pub fn command(args: Vec<String>, usage: &str) -> ExitCode {
    let mut install_extension = false;
    let mut dir = None;
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--install" => install_extension = true,
            "--dir" => match args.next() {
                Some(value) => dir = Some(PathBuf::from(value)),
                None => {
                    eprintln!("rakumux-mcp: --dir needs a directory");
                    return ExitCode::from(2);
                }
            },
            other => {
                eprintln!("rakumux-mcp: unknown argument {other:?}\n\n{usage}");
                return ExitCode::from(2);
            }
        }
    }
    if !install_extension {
        if dir.is_some() {
            eprintln!("rakumux-mcp: --dir needs --install");
            return ExitCode::from(2);
        }
        print!("{EXTENSION}");
        return ExitCode::SUCCESS;
    }
    let Some(dir) = dir.or_else(agent_dir) else {
        eprintln!("rakumux-mcp: can't find Pi's agent directory; pass --dir");
        return ExitCode::FAILURE;
    };
    match install(&dir) {
        Ok(path) => {
            println!("Wrote {}.", path.display());
            println!("Restart Pi, or run /reload in it, to load it.");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("rakumux-mcp: {error}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn install_writes_the_extension_where_pi_discovers_it() {
        let dir = tempfile::tempdir().unwrap();
        let agent = dir.path().join("agent");
        let path = install(&agent).unwrap();
        assert_eq!(
            path,
            agent.join("extensions/rakumux-session.ts"),
            "Pi loads <agent dir>/extensions/*.ts"
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), EXTENSION);
        // Reinstalling overwrites an older copy.
        std::fs::write(&path, "old").unwrap();
        install(&agent).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), EXTENSION);
        // Nothing else is written.
        let entries: Vec<_> = std::fs::read_dir(&agent)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(entries, vec![std::ffi::OsString::from("extensions")]);
    }

    #[test]
    fn the_extension_is_one_module_with_a_default_factory() {
        assert!(EXTENSION.contains("export default function rakumuxSession(pi)"));
        assert!(EXTENSION.contains("export function reduce("));
        assert!(EXTENSION.contains("\"session\", \"bind\", \"--agent\", \"pi\""));
        assert_eq!(EXTENSION.matches("export default").count(), 1);
    }
}
