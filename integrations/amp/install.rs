//! The Rakumux plugin for Amp (bundled from `integrations/amp/rakumux-session/`)
//! and its installer.
//!
//! Amp runs system plugins from `<Amp config>/plugins/` in every Amp it
//! starts, with that Amp's environment: inside a Rakumux session, the
//! session's identity. The plugin binds the thread Amp shows to the session,
//! reports its state, and keeps a private copy of its threads under Amp's
//! cache for plugins, which the daemon reads (Amp keeps threads on
//! ampcode.com, not on this machine).

use std::path::{Path, PathBuf};
use std::process::ExitCode;

/// The plugin's source.
pub const PLUGIN: &str = concat!(
    include_str!("rakumux-session/activity.mjs"),
    "\n",
    include_str!("rakumux-session/runtime.js")
);
/// The plugin's file under `<Amp config>/plugins/`. Its name is the
/// plugin's, and so its cache directory's (`plugin-cache/rakumux-session`).
pub const PLUGIN_FILE: &str = "rakumux-session.js";

/// Amp's config directory: `$XDG_CONFIG_HOME/amp`, else `~/.config/amp`.
pub fn config_dir() -> Option<PathBuf> {
    let var = |name: &str| std::env::var_os(name).filter(|value| !value.is_empty());
    let base = match var("XDG_CONFIG_HOME") {
        Some(dir) => PathBuf::from(dir),
        None => PathBuf::from(var("HOME")?).join(".config"),
    };
    Some(base.join("amp"))
}

/// Where [`install`] puts the plugin for the config directory `config`.
pub fn plugin_path(config: &Path) -> PathBuf {
    config.join("plugins").join(PLUGIN_FILE)
}

/// Write (or overwrite) the plugin under `config`; returns its path. No Amp
/// setting is changed.
pub fn install(config: &Path) -> Result<PathBuf, String> {
    let path = plugin_path(config);
    let dir = path.parent().expect("plugin path has a parent");
    std::fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    std::fs::write(&path, PLUGIN).map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    Ok(path)
}

/// Handle `amp-plugin [--install [--dir DIR]]`.
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
        eprintln!("rakumux-mcp: can't find Amp's config directory; pass --dir");
        return ExitCode::FAILURE;
    };
    match install(&dir) {
        Ok(path) => {
            println!("Wrote {}.", path.display());
            println!("Restart Amp (or run `plugins: reload` in its command palette) to load it.");
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
    fn install_writes_the_plugin_where_amp_loads_system_plugins() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("amp");
        let path = install(&config).unwrap();
        assert_eq!(path, config.join("plugins/rakumux-session.js"));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), PLUGIN);
        // Reinstalling overwrites an older copy.
        std::fs::write(&path, "old").unwrap();
        install(&config).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), PLUGIN);
        // Nothing else is written: no settings.
        let entries: Vec<_> = std::fs::read_dir(&config)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(entries, vec![std::ffi::OsString::from("plugins")]);
    }

    #[test]
    fn the_plugin_never_handles_tool_calls() {
        assert!(PLUGIN.contains("export default function rakumux(amp)"));
        assert!(PLUGIN.contains("amp.on(\"agent.end\""));
        // `tool.call` can allow, reject or change a call: never registered.
        assert!(!PLUGIN.contains("\"tool.call\""));
        assert!(!PLUGIN.contains("'tool.call'"));
        assert!(!PLUGIN.contains("registerTool"));
    }
}
