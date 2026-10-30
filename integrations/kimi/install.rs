//! The Rakumux plugin for Kimi Code (`rakumux-plugin/kimi.plugin.json`) and
//! its installer (`rakumux-mcp kimi-plugin`).
//!
//! The plugin holds two things Kimi Code loads together:
//! - hooks on `SessionStart`, `UserPromptSubmit` and `TurnStarted` that run
//!   `rakumux session hook --agent kimi`, which tells the Rakumux session
//!   Kimi runs in which conversation it shows and where it works (never
//!   prompts or lifecycle: Kimi reports its state through OSC 7501);
//! - Rakumux's MCP server, `rakumux-mcp` from `PATH`, which inherits the
//!   session's identity (`RAKUMUX_SESSION_ID` and `RAKUMUX_SESSION_TOKEN`)
//!   with the rest of Kimi's environment.
//!
//! Installing does what Kimi's `/plugins install <dir>` does for a folder:
//! the plugin is copied to `<Kimi home>/plugins/managed/rakumux/` and
//! recorded in `<Kimi home>/plugins/installed.json` (version 1), leaving
//! every other plugin as it was. Kimi loads it on its next start (or
//! `/plugins reload`).

use std::path::{Path, PathBuf};
use std::process::ExitCode;

/// The plugin's manifest.
pub const PLUGIN: &str = include_str!("rakumux-plugin/kimi.plugin.json");
/// The plugin's id (its manifest's `name`), and its folder under
/// `plugins/managed/`.
pub const PLUGIN_ID: &str = "rakumux";

/// Kimi Code's home: `$KIMI_CODE_HOME`, else `~/.kimi-code`.
pub fn home_dir() -> Option<PathBuf> {
    let var = |name: &str| std::env::var_os(name).filter(|value| !value.is_empty());
    match var("KIMI_CODE_HOME") {
        Some(home) => Some(PathBuf::from(home)),
        None => Some(PathBuf::from(var("HOME")?).join(".kimi-code")),
    }
}

/// Where [`install`] puts the plugin for the Kimi home `home`.
pub fn plugin_dir(home: &Path) -> PathBuf {
    home.join("plugins").join("managed").join(PLUGIN_ID)
}

/// Install (or update) the plugin under the Kimi home `home`, `now` being
/// the time to record (RFC 3339); returns its folder. Refuses an
/// `installed.json` it can't read, so nothing of another plugin is lost.
pub fn install(home: &Path, now: &str) -> Result<PathBuf, String> {
    let registry = home.join("plugins").join("installed.json");
    let mut installed = match std::fs::read_to_string(&registry) {
        Ok(text) => serde_json::from_str::<serde_json::Value>(&text)
            .map_err(|e| format!("cannot read {}: {e}", registry.display()))?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            serde_json::json!({"version": 1, "plugins": []})
        }
        Err(e) => return Err(format!("cannot read {}: {e}", registry.display())),
    };
    if installed.get("version").and_then(serde_json::Value::as_u64) != Some(1) {
        return Err(format!(
            "{} is of a Kimi Code version this installer doesn't know",
            registry.display()
        ));
    }
    let Some(plugins) = installed
        .get_mut("plugins")
        .and_then(serde_json::Value::as_array_mut)
    else {
        return Err(format!("{} has no plugin list", registry.display()));
    };
    let dir = plugin_dir(home);
    std::fs::create_dir_all(&dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    let manifest = dir.join("kimi.plugin.json");
    std::fs::write(&manifest, PLUGIN)
        .map_err(|e| format!("cannot write {}: {e}", manifest.display()))?;
    let root = std::fs::canonicalize(&dir).unwrap_or(dir.clone());
    let root = root.to_string_lossy().into_owned();
    let existing = plugins
        .iter()
        .position(|plugin| plugin.get("id").and_then(serde_json::Value::as_str) == Some(PLUGIN_ID));
    let kept = |key: &str, default: serde_json::Value| {
        existing
            .and_then(|at| plugins[at].get(key).cloned())
            .unwrap_or(default)
    };
    let mut record = serde_json::json!({
        "id": PLUGIN_ID,
        "root": root,
        "source": "local-path",
        "enabled": kept("enabled", serde_json::Value::Bool(true)),
        "installedAt": kept("installedAt", serde_json::Value::String(now.into())),
        "updatedAt": now,
    });
    if let Some(capabilities) = existing.and_then(|at| plugins[at].get("capabilities").cloned()) {
        record["capabilities"] = capabilities;
    }
    match existing {
        Some(at) => plugins[at] = record,
        None => plugins.push(record),
    }
    let text = serde_json::to_string_pretty(&installed).map_err(|e| e.to_string())?;
    let staged = registry.with_extension("json.rakumux.tmp");
    std::fs::write(&staged, text).map_err(|e| format!("cannot write {}: {e}", staged.display()))?;
    std::fs::rename(&staged, &registry)
        .map_err(|e| format!("cannot write {}: {e}", registry.display()))?;
    Ok(dir)
}

/// Handle `kimi-plugin [--install [--dir DIR]]`.
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
    let Some(home) = dir.or_else(home_dir) else {
        eprintln!("rakumux-mcp: can't find Kimi Code's home; pass --dir");
        return ExitCode::FAILURE;
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_secs() as i64);
    match install(&home, &rakumux_api::time::rfc3339(now)) {
        Ok(dir) => {
            println!("Installed the Rakumux plugin in {}.", dir.display());
            println!("Restart Kimi Code (or run /plugins reload) to load it.");
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
    use serde_json::{json, Value};

    fn read(path: &Path) -> Value {
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    }

    #[test]
    fn the_plugin_holds_the_hooks_and_the_mcp_server() {
        let manifest: Value = serde_json::from_str(PLUGIN).unwrap();
        assert_eq!(manifest["name"], PLUGIN_ID);
        let events: Vec<&str> = manifest["hooks"]
            .as_array()
            .unwrap()
            .iter()
            .map(|hook| hook["event"].as_str().unwrap())
            .collect();
        assert_eq!(events, ["SessionStart", "UserPromptSubmit", "TurnStarted"]);
        for hook in manifest["hooks"].as_array().unwrap() {
            // Kimi refuses a hook with any other field.
            let mut keys: Vec<&str> = hook
                .as_object()
                .unwrap()
                .keys()
                .map(String::as_str)
                .collect();
            keys.sort_unstable();
            assert_eq!(keys, ["command", "event", "timeout"]);
            assert_eq!(
                hook["command"],
                "\"${RAKUMUX_BIN:-rakumux}\" session hook --agent kimi"
            );
        }
        // A plugin's MCP command must be on PATH (or inside the plugin).
        assert_eq!(
            manifest["mcpServers"]["rakumux"],
            json!({"command": "rakumux-mcp"})
        );
    }

    #[test]
    fn install_records_the_plugin_beside_the_others() {
        let home = tempfile::tempdir().unwrap();
        let registry = home.path().join("plugins/installed.json");
        std::fs::create_dir_all(registry.parent().unwrap()).unwrap();
        let other = json!({"id": "other", "root": "/x", "source": "github", "enabled": false,
            "installedAt": "2026-01-01T00:00:00Z", "updatedAt": "2026-01-01T00:00:00Z",
            "github": {"owner": "o", "repo": "r"}});
        std::fs::write(
            &registry,
            json!({"version": 1, "plugins": [other.clone()]}).to_string(),
        )
        .unwrap();
        let dir = install(home.path(), "2026-10-30T00:00:00Z").unwrap();
        assert_eq!(dir, home.path().join("plugins/managed/rakumux"));
        assert_eq!(
            std::fs::read_to_string(dir.join("kimi.plugin.json")).unwrap(),
            PLUGIN
        );
        let installed = read(&registry);
        assert_eq!(installed["plugins"][0], other);
        let record = &installed["plugins"][1];
        assert_eq!(record["id"], "rakumux");
        assert_eq!(record["source"], "local-path");
        assert_eq!(record["enabled"], true);
        assert_eq!(
            record["root"],
            std::fs::canonicalize(&dir).unwrap().to_str().unwrap()
        );
        // Installing again updates it in place, keeping what the user set.
        let mut edited = installed.clone();
        edited["plugins"][1]["enabled"] = json!(false);
        edited["plugins"][1]["capabilities"] =
            json!({"mcpServers": {"rakumux": {"enabled": false}}});
        std::fs::write(&registry, edited.to_string()).unwrap();
        install(home.path(), "2026-10-31T00:00:00Z").unwrap();
        let again = read(&registry);
        assert_eq!(again["plugins"].as_array().unwrap().len(), 2);
        let record = &again["plugins"][1];
        assert_eq!(record["enabled"], false);
        assert_eq!(record["installedAt"], "2026-10-30T00:00:00Z");
        assert_eq!(record["updatedAt"], "2026-10-31T00:00:00Z");
        assert_eq!(
            record["capabilities"]["mcpServers"]["rakumux"]["enabled"],
            false
        );
    }

    #[test]
    fn install_starts_a_registry_and_refuses_one_it_cant_read() {
        let home = tempfile::tempdir().unwrap();
        install(home.path(), "2026-10-30T00:00:00Z").unwrap();
        let installed = read(&home.path().join("plugins/installed.json"));
        assert_eq!(installed["version"], 1);
        assert_eq!(installed["plugins"][0]["id"], "rakumux");
        for unreadable in [
            "not json",
            r#"{"version": 2, "plugins": []}"#,
            r#"{"version": 1}"#,
        ] {
            std::fs::write(home.path().join("plugins/installed.json"), unreadable).unwrap();
            assert!(install(home.path(), "x").is_err(), "{unreadable}");
            assert_eq!(
                std::fs::read_to_string(home.path().join("plugins/installed.json")).unwrap(),
                unreadable
            );
        }
    }
}
