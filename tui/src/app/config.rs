use anyhow::Result;
use serde::{Deserialize, Deserializer, Serialize};
use session_core::ids::SessionId;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use crate::icons::Icons;
use crate::layout::SessionLayout;

/// The order the user gave each folder's sessions: folder → session ids,
/// first to last.
pub type SessionOrder = BTreeMap<PathBuf, Vec<SessionId>>;

/// AI provider settings (API keys, provider, model). Also read by
/// `crate::naming`. Stored at the top level of `config.json`.
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq, Default)]
#[serde(default)]
pub struct Config {
    pub anthropic_api_key: Option<String>,
    pub openai_api_key: Option<String>,
    pub ai_primary_provider: Option<String>,
    pub openai_model: Option<String>,
}

/// Everything in `~/.orchestra/config.json` (`$ORCHESTRA_HOME` respected):
/// the AI settings at the top level plus the `"sessions"` section. Load
/// and save through this type so neither part is lost.
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq, Default)]
pub struct Settings {
    #[serde(flatten)]
    pub ai: Config,
    #[serde(default)]
    pub sessions: SessionsConfig,
}

/// The `"sessions"` section. The TUI reads `orchestra_key` (and the older
/// `prefix_key`), `cycle_keys`, `cycle_keys_in_session`, `welcome_seen`,
/// `phone_offer_seen`, `layout`, `icons`, `sidebar_width`, `stacked_list_percent` and
/// `session_order`; other keys in the
/// section (daemon tuning such as `scrollback_rows`) are kept as they are
/// when the TUI saves the file, except the retired ones
/// ([`RETIRED_SESSION_KEYS`]), which are dropped.
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
#[serde(default)]
pub struct SessionsConfig {
    /// The key that takes you from a session to Orchestra mode, e.g.
    /// `"ctrl+\\"`; see [`SessionsConfig::orchestra_key_spec`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub orchestra_key: Option<String>,
    /// The in-session prefix of earlier versions. Kept as it is in the
    /// file; one the user changed from `"ctrl+b"` becomes the Orchestra key
    /// while `orchestra_key` isn't set.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prefix_key: Option<String>,
    /// Keys that select the previous/next session in Orchestra mode.
    pub cycle_keys: CycleKeysConfig,
    /// The cycle keys also work in session mode, so the program in the
    /// session never receives them.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub cycle_keys_in_session: bool,
    /// Don't show the welcome tour on startup (the box on its cards).
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub welcome_seen: bool,
    /// "Use Orchestra from your phone?" was offered (once, when Tailscale
    /// runs here and phone access was never set up); Settings' Phone access
    /// row turns it on any time.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub phone_offer_seen: bool,
    /// Sidebar (list + detail) or Tabs (tab bar + detail). A value this
    /// version doesn't know (e.g. written by a newer one) reads as the
    /// default instead of breaking the whole config.
    #[serde(deserialize_with = "lenient_layout")]
    pub layout: SessionLayout,
    /// Marks for folders and branches: plain, emoji or Nerd Font. A value
    /// this version doesn't know reads as plain.
    #[serde(deserialize_with = "lenient_icons", skip_serializing_if = "is_plain")]
    pub icons: Icons,
    /// Sidebar list width set by dragging the divider; `None` sizes the
    /// list to the screen. Ignored unless it's a number.
    #[serde(
        deserialize_with = "lenient_width",
        skip_serializing_if = "Option::is_none"
    )]
    pub sidebar_width: Option<u16>,
    /// Share of the height (percent) the list stacked above the session
    /// takes on narrow screens, set by dragging the divider between them;
    /// `None` gives it 40%. A share, so the split follows a phone's keyboard
    /// coming and going. Ignored unless it's a number.
    #[serde(
        deserialize_with = "lenient_width",
        skip_serializing_if = "Option::is_none"
    )]
    pub stacked_list_percent: Option<u16>,
    /// The order of each folder's sessions in the list, as the user set it
    /// by dragging or with `J`/`K`. Sessions not listed go after the listed
    /// ones, oldest first. Anything but a map of folders to lists of ids
    /// reads as empty.
    #[serde(
        deserialize_with = "lenient_order",
        skip_serializing_if = "BTreeMap::is_empty"
    )]
    pub session_order: SessionOrder,
    #[serde(flatten)]
    pub other: serde_json::Map<String, serde_json::Value>,
}

impl Default for SessionsConfig {
    fn default() -> Self {
        Self {
            orchestra_key: None,
            prefix_key: None,
            cycle_keys: CycleKeysConfig::default(),
            cycle_keys_in_session: false,
            welcome_seen: false,
            phone_offer_seen: false,
            layout: SessionLayout::default(),
            icons: Icons::default(),
            sidebar_width: None,
            stacked_list_percent: None,
            session_order: SessionOrder::new(),
            other: serde_json::Map::new(),
        }
    }
}

impl SessionsConfig {
    /// The Orchestra key to use: `orchestra_key`, else a customised
    /// `prefix_key`, else the default (`Ctrl-\\`).
    pub fn orchestra_key_spec(&self) -> &str {
        let customised_prefix =
            self.prefix_key.as_deref().map(str::trim).filter(|p| {
                !p.is_empty() && !p.eq_ignore_ascii_case(super::keys::OLD_DEFAULT_PREFIX)
            });
        self.orchestra_key
            .as_deref()
            .or(customised_prefix)
            .unwrap_or(super::keys::DEFAULT_ORCHESTRA_KEY)
    }
}

fn lenient_width<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<u16>, D::Error> {
    let value = serde_json::Value::deserialize(deserializer)?;
    Ok(serde_json::from_value(value).unwrap_or_default())
}

fn lenient_order<'de, D: Deserializer<'de>>(deserializer: D) -> Result<SessionOrder, D::Error> {
    let value = serde_json::Value::deserialize(deserializer)?;
    Ok(serde_json::from_value(value).unwrap_or_default())
}

fn lenient_icons<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Icons, D::Error> {
    let value = serde_json::Value::deserialize(deserializer)?;
    Ok(serde_json::from_value(value).unwrap_or_default())
}

fn is_plain(icons: &Icons) -> bool {
    *icons == Icons::Plain
}

fn lenient_layout<'de, D: Deserializer<'de>>(deserializer: D) -> Result<SessionLayout, D::Error> {
    let value = serde_json::Value::deserialize(deserializer)?;
    Ok(serde_json::from_value(value).unwrap_or_default())
}

/// `"cycle_keys": { "previous": "alt+up", "next": "alt+down" }`. Both
/// empty turns them off (`j`/`k` and the arrows still move in Orchestra
/// mode).
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
#[serde(default)]
pub struct CycleKeysConfig {
    pub previous: String,
    pub next: String,
}

impl Default for CycleKeysConfig {
    fn default() -> Self {
        Self {
            previous: super::keys::DEFAULT_CYCLE_PREVIOUS.to_string(),
            next: super::keys::DEFAULT_CYCLE_NEXT.to_string(),
        }
    }
}

/// Keys of the `"sessions"` section that earlier versions wrote and this
/// one no longer reads: `selected_projects` (the shown projects; the folder
/// filter isn't saved). They're dropped on load, so the next save leaves
/// them out.
pub const RETIRED_SESSION_KEYS: [&str; 1] = ["selected_projects"];

impl Settings {
    pub fn load_from(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Settings::default());
        }
        let content = fs::read_to_string(path)?;
        let mut settings: Settings = serde_json::from_str(&content)?;
        for key in RETIRED_SESSION_KEYS {
            settings.sessions.other.remove(key);
        }
        Ok(settings)
    }

    pub fn save_to(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let content = serde_json::to_string_pretty(self)?;
        fs::write(path, content)?;
        Ok(())
    }

    pub fn config_file_path() -> Result<PathBuf> {
        if let Some(home) = std::env::var_os(session_core::env::HOME) {
            return Ok(PathBuf::from(home).join("config.json"));
        }
        let home_dir = std::env::var("HOME")
            .map_err(|_| anyhow::anyhow!("HOME environment variable not set"))?;
        Ok(PathBuf::from(home_dir)
            .join(".orchestra")
            .join("config.json"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_roundtrip_works() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("config.json");

        let settings = Settings::load_from(&path).expect("load default config");
        assert_eq!(settings, Settings::default());
        assert_eq!(settings.sessions.orchestra_key_spec(), "ctrl+\\");

        let settings = Settings {
            ai: Config {
                anthropic_api_key: Some("sk-ant-TEST".into()),
                openai_api_key: Some("sk-TEST".into()),
                ai_primary_provider: Some("openai".into()),
                openai_model: Some("gpt-4o-mini".into()),
            },
            ..Settings::default()
        };
        settings.save_to(&path).expect("save config");

        let reloaded = Settings::load_from(&path).expect("reload config");
        assert_eq!(reloaded, settings);
        let raw: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        // AI settings stay at the top level, as older versions wrote them.
        assert_eq!(raw["anthropic_api_key"], "sk-ant-TEST");
    }

    #[test]
    fn unknown_session_settings_survive_a_save() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        fs::write(
            &path,
            r#"{"anthropic_api_key":"sk-ant-x","sessions":{"prefix_key":"ctrl+a","scrollback_rows":5000}}"#,
        )
        .unwrap();
        let mut settings = Settings::load_from(&path).unwrap();
        assert_eq!(settings.ai.anthropic_api_key.as_deref(), Some("sk-ant-x"));
        assert_eq!(settings.sessions.prefix_key.as_deref(), Some("ctrl+a"));
        assert_eq!(settings.sessions.layout, SessionLayout::Sidebar);
        assert_eq!(settings.sessions.cycle_keys, CycleKeysConfig::default());
        settings.sessions.layout = SessionLayout::Tabs;
        settings.ai.openai_api_key = Some("sk-y".into());
        settings.save_to(&path).unwrap();
        let raw: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(raw["sessions"]["scrollback_rows"], 5000);
        assert_eq!(raw["sessions"]["prefix_key"], "ctrl+a");
        assert_eq!(raw["sessions"]["layout"], "tabs");
        assert_eq!(raw["sessions"]["cycle_keys"]["next"], "alt+down");
        assert_eq!(raw["openai_api_key"], "sk-y");
        assert_eq!(raw["anthropic_api_key"], "sk-ant-x");
    }

    #[test]
    fn an_old_selected_projects_still_loads_and_is_dropped_on_save() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        fs::write(
            &path,
            r#"{"sessions":{"layout":"tabs","selected_projects":["/repo","/web"],
                "scrollback_rows":5000,"future":{"a":1}}}"#,
        )
        .unwrap();
        let settings = Settings::load_from(&path).unwrap();
        assert_eq!(settings.sessions.layout, SessionLayout::Tabs);
        assert!(!settings.sessions.other.contains_key("selected_projects"));
        settings.save_to(&path).unwrap();
        let raw: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert!(raw["sessions"].get("selected_projects").is_none(), "{raw}");
        // Other keys this version doesn't know survive.
        assert_eq!(raw["sessions"]["scrollback_rows"], 5000);
        assert_eq!(raw["sessions"]["future"]["a"], 1);
        // Whatever its value.
        fs::write(&path, r#"{"sessions":{"selected_projects":"oops"}}"#).unwrap();
        assert_eq!(Settings::load_from(&path).unwrap(), Settings::default());
    }

    #[test]
    fn the_session_order_round_trips_and_is_left_out_when_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        // Older files have none.
        fs::write(&path, r#"{"sessions":{"layout":"tabs"}}"#).unwrap();
        let mut settings = Settings::load_from(&path).unwrap();
        assert!(settings.sessions.session_order.is_empty());
        settings.save_to(&path).unwrap();
        let raw: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert!(raw["sessions"].get("session_order").is_none());
        settings.sessions.session_order.insert(
            "/repo".into(),
            vec![SessionId::from_string("B"), SessionId::from_string("A")],
        );
        settings.save_to(&path).unwrap();
        let raw: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(
            raw["sessions"]["session_order"],
            serde_json::json!({"/repo": ["B", "A"]})
        );
        assert_eq!(Settings::load_from(&path).unwrap(), settings);
        // A value this version can't read is ignored, not fatal.
        fs::write(&path, r#"{"sessions":{"session_order":["A"]}}"#).unwrap();
        assert!(Settings::load_from(&path)
            .unwrap()
            .sessions
            .session_order
            .is_empty());
    }

    #[test]
    fn the_stacked_list_height_is_optional_and_lenient() {
        let read = |json: &str| -> SessionsConfig { serde_json::from_str(json).unwrap() };
        assert_eq!(read("{}").stacked_list_percent, None);
        assert_eq!(
            read(r#"{"stacked_list_percent":20}"#).stacked_list_percent,
            Some(20)
        );
        // Anything but a number reads as unset.
        assert_eq!(
            read(r#"{"stacked_list_percent":"tall"}"#).stacked_list_percent,
            None
        );
        let json = serde_json::to_value(read("{}")).unwrap();
        assert!(json.get("stacked_list_percent").is_none());
        let json = serde_json::to_value(read(r#"{"stacked_list_percent":12}"#)).unwrap();
        assert_eq!(json["stacked_list_percent"], 12);
    }

    #[test]
    fn an_unknown_layout_reads_as_the_default() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        fs::write(
            &path,
            r#"{"openai_model":"gpt-4.1","sessions":{"layout":"grid","prefix_key":"ctrl+a"}}"#,
        )
        .unwrap();
        let settings = Settings::load_from(&path).unwrap();
        assert_eq!(settings.sessions.layout, SessionLayout::Sidebar);
        assert_eq!(settings.sessions.prefix_key.as_deref(), Some("ctrl+a"));
        assert_eq!(settings.ai.openai_model.as_deref(), Some("gpt-4.1"));
    }

    #[test]
    fn the_orchestra_key_replaces_the_old_default_prefix_but_keeps_a_custom_one() {
        let read = |json: &str| -> SessionsConfig { serde_json::from_str(json).unwrap() };
        assert_eq!(read("{}").orchestra_key_spec(), "ctrl+\\");
        // Earlier versions saved their default prefix; it doesn't stick.
        assert_eq!(
            read(r#"{"prefix_key":"ctrl+b"}"#).orchestra_key_spec(),
            "ctrl+\\"
        );
        assert_eq!(
            read(r#"{"prefix_key":"ctrl+a"}"#).orchestra_key_spec(),
            "ctrl+a"
        );
        assert_eq!(
            read(r#"{"prefix_key":"ctrl+a","orchestra_key":"ctrl+q"}"#).orchestra_key_spec(),
            "ctrl+q"
        );
        let config = read(r#"{"cycle_keys_in_session":true,"icons":"emoji"}"#);
        assert!(config.cycle_keys_in_session);
        assert_eq!(config.icons, Icons::Emoji);
        assert_eq!(read(r#"{"icons":"sparkles"}"#).icons, Icons::Plain);
        // Off isn't written out.
        let json = serde_json::to_value(read("{}")).unwrap();
        assert!(json.get("cycle_keys_in_session").is_none());
        assert!(json.get("orchestra_key").is_none());
        assert!(json.get("icons").is_none());
    }
}
