//! The Rakumux plugin for OpenCode 2.x's TUI
//! (`mcp/opencode/rakumux-session/tui.js`) and its installer.
//!
//! OpenCode 2.x runs MCP servers in a service shared by every conversation, so
//! tool calls reach this server without the calling session's environment; they
//! only name the OpenCode conversation. The plugin runs in the TUI (inside the
//! Rakumux session) and binds the conversations it shows to that session with
//! `rakumux session bind`; the server then acts as the session that bound a
//! call's conversation. OpenCode 1.x needs no plugin: its MCP servers run inside
//! each TUI, with the session's environment.

use std::path::{Path, PathBuf};

/// The plugin's source.
pub const PLUGIN: &str = include_str!("../opencode/rakumux-session/tui.js");
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
        assert!(PLUGIN.contains("export default {"));
        assert!(PLUGIN.contains("setup,"));
        assert!(PLUGIN.contains("\"session\", \"bind\", \"--agent\", AGENT"));
    }
}
