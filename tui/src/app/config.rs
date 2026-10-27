use anyhow::Result;
use serde::{Deserialize, Deserializer, Serialize};
use session_core::ids::SessionId;
use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use crate::icons::Icons;
use crate::layout::SessionLayout;

/// The order the user gave each folder's sessions: folder → session ids,
/// first to last.
pub type SessionOrder = BTreeMap<PathBuf, Vec<SessionId>>;

/// The order the user gave the sessions of each app or status, under the
/// App and Status tabs: `app:claude` or `status:working` → session ids,
/// first to last (see `grouping::Section::key`).
pub type SectionOrder = BTreeMap<String, Vec<SessionId>>;

/// Optional provider settings for the pre-existing AI session naming feature.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Default)]
#[serde(default)]
pub struct Config {
    pub anthropic_api_key: Option<String>,
    pub openai_api_key: Option<String>,
    pub ai_primary_provider: Option<String>,
    pub openai_model: Option<String>,
}

impl std::fmt::Debug for Config {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Config")
            .field(
                "anthropic_api_key",
                &self.anthropic_api_key.as_ref().map(|_| "<redacted>"),
            )
            .field(
                "openai_api_key",
                &self.openai_api_key.as_ref().map(|_| "<redacted>"),
            )
            .field("ai_primary_provider", &self.ai_primary_provider)
            .field("openai_model", &self.openai_model)
            .finish()
    }
}

/// Everything in `~/.rakumux/config.json` (`$RAKUMUX_HOME` respected):
/// the AI settings at the top level plus the `"sessions"` section. Load
/// and save through this type so neither part is lost.
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq, Default)]
pub struct Settings {
    #[serde(flatten)]
    pub ai: Config,
    #[serde(default)]
    pub sessions: SessionsConfig,
}

/// The `"sessions"` section. The TUI reads `rakumux_key` (and the older
/// `prefix_key`), `cycle_keys`, `cycle_keys_in_session`, `welcome_seen`,
/// `phone_offer_seen`, `remote_replies`, `icons`, `sidebar_width`,
/// `stacked_list_percent`, `feed_width`, `session_order` and `section_order`;
/// other keys in the section (daemon tuning such as `scrollback_rows`) are kept as they are
/// when the TUI saves the file, except the retired ones
/// ([`RETIRED_SESSION_KEYS`]), which are dropped.
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
#[serde(default)]
pub struct SessionsConfig {
    /// The key that takes you from a session to Rakumux mode, e.g.
    /// `"ctrl+\\"`; see [`SessionsConfig::rakumux_key_spec`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rakumux_key: Option<String>,
    /// The in-session prefix of earlier versions. Kept as it is in the
    /// file; one the user changed from `"ctrl+b"` becomes the Rakumux key
    /// while `rakumux_key` isn't set.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prefix_key: Option<String>,
    /// Keys that select the previous/next session in Rakumux mode.
    pub cycle_keys: CycleKeysConfig,
    /// The cycle keys also work in session mode, so the program in the
    /// session never receives them.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub cycle_keys_in_session: bool,
    /// Don't show the welcome tour on startup (the box on its cards).
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub welcome_seen: bool,
    /// "Use Rakumux from your phone?" was offered (once, when Tailscale
    /// runs here and phone access was never set up); Settings' Phone access
    /// row turns it on any time.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub phone_offer_seen: bool,
    /// Type replies written on rakumux.com or on the account's other
    /// machines into this machine's agent sessions (Settings → Replies from
    /// rakumux.com). Off, only replies written in this machine's TUI are.
    /// Local only, never set from the website; the status service reads it
    /// every few seconds.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub remote_replies: bool,
    /// The layout this window shows: Master detail, Grid or Tabs. Not saved:
    /// every start opens Master detail (an older file's `layout` is dropped).
    #[serde(skip)]
    pub layout: SessionLayout,
    /// Marks for folders and branches: plain, emoji or Nerd Font. A value
    /// this version doesn't know reads as plain.
    #[serde(deserialize_with = "lenient_icons", skip_serializing_if = "is_plain")]
    pub icons: Icons,
    /// When an agent's chat gets its AI title (and description): once it has
    /// written `every_messages` messages (5) or its turn finishes sooner, from
    /// your prompt and its newest `recent_messages` (10). Read by the status
    /// service each minute.
    #[serde(
        deserialize_with = "lenient_ai_summary",
        skip_serializing_if = "AiSummaryConfig::is_default"
    )]
    pub ai_summary: AiSummaryConfig,
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
    /// Docked feed pane width set by dragging the line left of it; `None`
    /// sizes it to the screen. Ignored unless it's a number.
    #[serde(
        deserialize_with = "lenient_width",
        skip_serializing_if = "Option::is_none"
    )]
    pub feed_width: Option<u16>,
    /// The order of each folder's sessions in the list, as the user set it
    /// by dragging or with `J`/`K`. Sessions not listed go after the listed
    /// ones, oldest first. Anything but a map of folders to lists of ids
    /// reads as empty.
    #[serde(
        deserialize_with = "lenient_order",
        skip_serializing_if = "BTreeMap::is_empty"
    )]
    pub session_order: SessionOrder,
    /// The order of each app's or status's sessions under the App and
    /// Status tabs, set the same ways; each tab orders its own groups.
    /// Anything but a map of section keys to lists of ids reads as empty.
    #[serde(
        deserialize_with = "lenient_section_order",
        skip_serializing_if = "BTreeMap::is_empty"
    )]
    pub section_order: SectionOrder,
    #[serde(flatten)]
    pub other: serde_json::Map<String, serde_json::Value>,
}

impl Default for SessionsConfig {
    fn default() -> Self {
        Self {
            rakumux_key: None,
            prefix_key: None,
            cycle_keys: CycleKeysConfig::default(),
            cycle_keys_in_session: false,
            welcome_seen: false,
            phone_offer_seen: false,
            remote_replies: false,
            layout: SessionLayout::default(),
            icons: Icons::default(),
            ai_summary: AiSummaryConfig::default(),
            sidebar_width: None,
            stacked_list_percent: None,
            feed_width: None,
            session_order: SessionOrder::new(),
            section_order: SectionOrder::new(),
            other: serde_json::Map::new(),
        }
    }
}

impl SessionsConfig {
    /// The Rakumux key to use: `rakumux_key`, else a customised
    /// `prefix_key`, else the default (`Ctrl-\\`).
    pub fn rakumux_key_spec(&self) -> &str {
        let customised_prefix =
            self.prefix_key.as_deref().map(str::trim).filter(|p| {
                !p.is_empty() && !p.eq_ignore_ascii_case(super::keys::OLD_DEFAULT_PREFIX)
            });
        self.rakumux_key
            .as_deref()
            .or(customised_prefix)
            .unwrap_or(super::keys::DEFAULT_RAKUMUX_KEY)
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

fn lenient_section_order<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<SectionOrder, D::Error> {
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

/// `sessions.ai_summary`: when agents' chats get AI titles.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AiSummaryConfig {
    /// A chat's title is made once its agent has written this many messages
    /// since your prompt.
    pub every_messages: u32,
    /// The newest of its messages a title is made from, after your prompt.
    pub recent_messages: u32,
}

impl Default for AiSummaryConfig {
    fn default() -> Self {
        Self {
            every_messages: 5,
            recent_messages: 10,
        }
    }
}

impl AiSummaryConfig {
    fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

fn lenient_ai_summary<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<AiSummaryConfig, D::Error> {
    let value = serde_json::Value::deserialize(deserializer)?;
    Ok(serde_json::from_value(value).unwrap_or_default())
}

/// `"cycle_keys": { "previous": "alt+up", "next": "alt+down" }`. Both
/// empty turns them off (`j`/`k` and the arrows still move in Rakumux
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
/// filter isn't saved) and `layout` (every start opens Master detail).
/// They're dropped on load, so the next save leaves them out.
pub const RETIRED_SESSION_KEYS: [&str; 2] = ["selected_projects", "layout"];

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
        with_config_lock(path, || self.write_to(path))
    }

    fn write_to(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let content = serde_json::to_vec_pretty(self)?;
        static SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let sequence = SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let temporary =
            path.with_file_name(format!(".config.{}.{}.tmp", std::process::id(), sequence));
        let result = (|| -> Result<()> {
            let mut file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&temporary)?;
            file.write_all(&content)?;
            file.sync_all()?;
            fs::rename(&temporary, path)?;
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        result
    }

    pub fn config_file_path() -> Result<PathBuf> {
        if let Some(home) = std::env::var_os(session_core::env::HOME) {
            return Ok(PathBuf::from(home).join("config.json"));
        }
        let home_dir = std::env::var("HOME")
            .map_err(|_| anyhow::anyhow!("HOME environment variable not set"))?;
        Ok(PathBuf::from(home_dir).join(".rakumux").join("config.json"))
    }
}

fn with_config_lock<T>(path: &Path, operation: impl FnOnce() -> Result<T>) -> Result<T> {
    use std::os::fd::AsRawFd;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let name = path
        .file_name()
        .ok_or_else(|| anyhow::anyhow!("configuration path has no filename"))?;
    let lock_path = path.with_file_name(format!("{}.lock", name.to_string_lossy()));
    let lock = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(lock_path)?;
    loop {
        // SAFETY: lock is an open descriptor borrowed for this flock call.
        if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX) } == 0 {
            break;
        }
        let error = std::io::Error::last_os_error();
        if error.kind() != std::io::ErrorKind::Interrupted {
            return Err(error.into());
        }
    }
    // Closing the descriptor releases the lock, including on an error or panic.
    operation()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn naming_keys_are_saved_privately_and_atomically() {
        use std::os::unix::fs::PermissionsExt;
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.json");
        let mut settings = Settings::default();
        settings.ai.openai_api_key = Some("sk-test-naming-key".into());
        settings.save_to(&path).unwrap();
        assert_eq!(
            Settings::load_from(&path)
                .unwrap()
                .ai
                .openai_api_key
                .as_deref(),
            Some("sk-test-naming-key")
        );
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert!(
            fs::read_dir(directory.path()).unwrap().all(|entry| !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .ends_with(".tmp")),
            "atomic save leaves no temporary file"
        );
    }

    #[test]
    fn config_roundtrip_works() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("config.json");

        let settings = Settings::load_from(&path).expect("load default config");
        assert_eq!(settings, Settings::default());
        assert_eq!(settings.sessions.rakumux_key_spec(), "ctrl+\\");

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
        // The layout isn't saved: every start opens Master detail.
        assert!(raw["sessions"].get("layout").is_none(), "{raw}");
        assert_eq!(raw["sessions"]["cycle_keys"]["next"], "alt+down");
        assert_eq!(raw["openai_api_key"], "sk-y");
        assert_eq!(raw["anthropic_api_key"], "sk-ant-x");
    }

    #[test]
    fn an_old_selected_projects_or_layout_still_loads_and_is_dropped_on_save() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        fs::write(
            &path,
            r#"{"sessions":{"layout":"tabs","selected_projects":["/repo","/web"],
                "scrollback_rows":5000,"future":{"a":1}}}"#,
        )
        .unwrap();
        let settings = Settings::load_from(&path).unwrap();
        // A saved layout no longer applies: the start is Master detail.
        assert_eq!(settings.sessions.layout, SessionLayout::Sidebar);
        assert!(!settings.sessions.other.contains_key("selected_projects"));
        assert!(!settings.sessions.other.contains_key("layout"));
        settings.save_to(&path).unwrap();
        let raw: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert!(raw["sessions"].get("selected_projects").is_none(), "{raw}");
        assert!(raw["sessions"].get("layout").is_none(), "{raw}");
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
    fn the_feed_width_is_optional_and_lenient() {
        let read = |json: &str| -> SessionsConfig { serde_json::from_str(json).unwrap() };
        assert_eq!(read("{}").feed_width, None);
        assert_eq!(read(r#"{"feed_width":52}"#).feed_width, Some(52));
        assert_eq!(read(r#"{"feed_width":"wide"}"#).feed_width, None);
        let json = serde_json::to_value(read("{}")).unwrap();
        assert!(json.get("feed_width").is_none());
        let json = serde_json::to_value(read(r#"{"feed_width":40}"#)).unwrap();
        assert_eq!(json["feed_width"], 40);
    }

    #[test]
    fn any_saved_layout_reads_as_master_detail() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        fs::write(
            &path,
            r#"{"openai_model":"gpt-4.1","sessions":{"layout":"future-layout","prefix_key":"ctrl+a"}}"#,
        )
        .unwrap();
        let settings = Settings::load_from(&path).unwrap();
        assert_eq!(settings.sessions.layout, SessionLayout::Sidebar);
        assert_eq!(settings.sessions.prefix_key.as_deref(), Some("ctrl+a"));
        assert_eq!(settings.ai.openai_model.as_deref(), Some("gpt-4.1"));
    }

    #[test]
    fn the_rakumux_key_replaces_the_old_default_prefix_but_keeps_a_custom_one() {
        let read = |json: &str| -> SessionsConfig { serde_json::from_str(json).unwrap() };
        assert_eq!(read("{}").rakumux_key_spec(), "ctrl+\\");
        // Earlier versions saved their default prefix; it doesn't stick.
        assert_eq!(
            read(r#"{"prefix_key":"ctrl+b"}"#).rakumux_key_spec(),
            "ctrl+\\"
        );
        assert_eq!(
            read(r#"{"prefix_key":"ctrl+a"}"#).rakumux_key_spec(),
            "ctrl+a"
        );
        assert_eq!(
            read(r#"{"prefix_key":"ctrl+a","rakumux_key":"ctrl+q"}"#).rakumux_key_spec(),
            "ctrl+q"
        );
        let config = read(r#"{"cycle_keys_in_session":true,"icons":"emoji"}"#);
        assert!(config.cycle_keys_in_session);
        assert_eq!(config.icons, Icons::Emoji);
        assert_eq!(read(r#"{"icons":"sparkles"}"#).icons, Icons::Plain);
        // Off isn't written out.
        let json = serde_json::to_value(read("{}")).unwrap();
        assert!(json.get("cycle_keys_in_session").is_none());
        assert!(json.get("rakumux_key").is_none());
        assert!(json.get("icons").is_none());
        // An older file's session description choice is kept as it was.
        let older = read(r#"{"description_content":"agent_message"}"#);
        assert_eq!(
            serde_json::to_value(older).unwrap()["description_content"],
            "agent_message"
        );
    }

    #[test]
    fn ai_summary_triggers_default_to_five_and_ten() {
        let read = |sessions: &str| -> SessionsConfig { serde_json::from_str(sessions).unwrap() };
        let json = serde_json::to_value(SessionsConfig::default()).unwrap();
        assert!(json.get("ai_summary").is_none());
        assert_eq!(read("{}").ai_summary, AiSummaryConfig::default());
        assert_eq!(
            read(r#"{"ai_summary":{"every_messages":3}}"#).ai_summary,
            AiSummaryConfig {
                every_messages: 3,
                recent_messages: 10
            }
        );
        assert_eq!(
            read(r#"{"ai_summary":{"every_messages":"often"}}"#).ai_summary,
            AiSummaryConfig::default()
        );
    }
}
