//! The Rakumux plugin for OpenCode 2.x's TUI
//! (bundled from `Integrations/Open Code/rakumux-session/`) and its installer.
//!
//! OpenCode 2.x runs MCP servers in a service shared by every conversation, so
//! tool calls reach this server without the calling session's environment; they
//! only name the OpenCode conversation. The plugin runs in the TUI (inside the
//! Rakumux session) and binds the conversations it shows to that session with
//! `rakumux session bind`; the server then acts as the session that bound a
//! call's conversation. OpenCode 1.x needs no plugin: its MCP servers run inside
//! each TUI, with the session's environment.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

/// The plugin's source.
pub const PLUGIN: &str = concat!(
    include_str!("rakumux-session/activity.mjs"),
    "\n",
    include_str!("rakumux-session/runtime.js")
);
/// The plugin's directory under `<OpenCode config>/plugins/`. OpenCode 2.x
/// loads every directory there (its `tui` module) without configuration.
pub const PLUGIN_DIR: &str = "rakumux-session";

/// OpenCode's config directory: `$XDG_CONFIG_HOME/opencode`, else
/// `~/.config/opencode`.
pub fn config_dir() -> Option<PathBuf> {
    let var = |name: &str| std::env::var_os(name).filter(|value| !value.is_empty());
    let base = match var("XDG_CONFIG_HOME") {
        Some(dir) => PathBuf::from(dir),
        None => PathBuf::from(var("HOME")?).join(".config"),
    };
    Some(base.join("opencode"))
}

/// Where [`install`] puts the plugin for the config directory `config`.
pub fn plugin_path(config: &Path) -> PathBuf {
    config.join("plugins").join(PLUGIN_DIR).join("tui.js")
}

/// Write (or overwrite) the plugin under `config`; returns its path. No
/// OpenCode config file is changed.
pub fn install(config: &Path) -> Result<PathBuf, String> {
    let path = plugin_path(config);
    let dir = path.parent().expect("plugin path has a parent");
    std::fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    std::fs::write(&path, PLUGIN).map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    Ok(path)
}

/// Handle the existing `opencode-plugin [--install [--dir DIR]]` command.
pub fn command(args: Vec<String>, usage: &str) -> ExitCode {
    let mut install_plugin = false;
    let mut dir = None;
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--install" => install_plugin = true,
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
    if !install_plugin {
        if dir.is_some() {
            eprintln!("rakumux-mcp: --dir needs --install");
            return ExitCode::from(2);
        }
        print!("{PLUGIN}");
        return ExitCode::SUCCESS;
    }
    let Some(dir) = dir.or_else(config_dir) else {
        eprintln!("rakumux-mcp: can't find OpenCode's config directory; pass --dir");
        return ExitCode::FAILURE;
    };
    match install(&dir) {
        Ok(path) => {
            println!("Wrote {}.", path.display());
            println!("Restart OpenCode (2.x) to load it.");
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
    fn install_writes_the_plugin_where_opencode_discovers_it() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("opencode");
        let path = install(&config).unwrap();
        assert_eq!(
            path,
            config.join("plugins/rakumux-session/tui.js"),
            "OpenCode 2.x loads <config>/plugins/<dir>/tui.js"
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), PLUGIN);
        // Reinstalling overwrites an older copy.
        std::fs::write(&path, "old").unwrap();
        install(&config).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), PLUGIN);
        // Nothing else is written.
        let entries: Vec<_> = std::fs::read_dir(&config)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(entries, vec![std::ffi::OsString::from("plugins")]);
    }

    #[test]
    fn the_plugin_is_an_opencode_2_plugin() {
        assert!(PLUGIN.contains("Plugin.define"));
        assert!(PLUGIN.contains("snapshot(context)"));
        assert!(PLUGIN.contains("context.data.session"));
    }
}
