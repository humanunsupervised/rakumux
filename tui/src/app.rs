//! Application state and the dispatch of input and background events.
//!
//! The TUI is a thin client of the session daemon
//! (`session_core::client::SessionApi`): it lists the sessions of every
//! folder (narrowed by the folder filter), keeps the list live from the
//! daemon's event stream, mirrors the selected session's terminal as an
//! observer and takes control when the user focuses it. Other TUIs and CLI
//! calls may act on the same sessions at any time.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossterm::event::{Event, KeyEvent, KeyEventKind};
use ratatui::layout::Rect;
use session_core::api::ApiError;
use session_core::ids::SessionId;
use session_core::model::ScopeRoot;
use session_core::paths::Layout;
use tokio::sync::mpsc::UnboundedReceiver;

use crate::auth::service::AuthService;
use crate::help::{ModalContext, SettingRow};
use crate::layout::SessionLayout;
use crate::remote_access::PhoneAccess;

mod auto_name;
pub mod config;
mod connection;
mod detail;
mod feature_flags;
pub mod folders;
mod handlers;
mod keys;
mod new_session;
pub mod path_complete;
mod phone_access;
mod runtime;
pub mod screen;
mod session_list;
mod sessions;
mod text_input;
pub mod types;
mod upgrade;

#[cfg(test)]
pub mod testing;
#[cfg(test)]
mod tests;

pub use auto_name::{ai_namer, Namer};
pub use config::{Config, Settings};
pub use connection::{client_config, client_connector, Connection, Connector};
pub use detail::{Detail, DetailView};
pub use feature_flags::FeatureFlags;
pub use folders::Folders;
pub use keys::{CycleKeys, KeyBinding};
pub use new_session::NewSessionDialog;
pub use runtime::{AppEvent, ExecMode};
pub use session_list::{ListRow, Selection, SessionList};
pub use text_input::TextInput;

use runtime::{CancelFlag, Exec};
use types::*;

/// Version helper: single place to resolve version string.
pub fn app_version() -> &'static str {
    option_env!("RAKUMUX_VERSION").unwrap_or(env!("CARGO_PKG_VERSION"))
}

const DEFAULT_OPENAI_MODEL: &str = "gpt-4o-mini";
/// How long a status message stays in the footer.
const STATUS_TTL: Duration = Duration::from_secs(6);
const RECONNECT_MAX: Duration = Duration::from_secs(10);

pub(crate) fn normalize_primary_provider(value: Option<&str>) -> &'static str {
    match value {
        Some(v) if v.eq_ignore_ascii_case("openai") => "openai",
        _ => "anthropic",
    }
}

pub(crate) fn effective_openai_model(value: Option<&str>) -> String {
    value.unwrap_or(DEFAULT_OPENAI_MODEL).to_string()
}

fn openai_model_options() -> Vec<String> {
    vec![
        "gpt-4o-mini".to_string(),
        "gpt-4.1-mini".to_string(),
        "gpt-4.1".to_string(),
    ]
}

/// The client's environment, passed on create/revive so sessions start
/// with a current environment rather than the daemon's.
fn env_snapshot() -> Vec<(String, String)> {
    std::env::vars_os()
        .filter_map(|(k, v)| Some((k.into_string().ok()?, v.into_string().ok()?)))
        .collect()
}

/// Mouse state of the line between the session list and the detail.
/// The session worked in now and the one before it (Tab in Rakumux mode
/// goes back to that one).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RecentSessions {
    pub current: Option<SessionId>,
    pub previous: Option<SessionId>,
}

impl RecentSessions {
    /// `id` took control; the session before it becomes the previous one.
    pub fn entered(&mut self, id: &SessionId) {
        if self.current.as_ref() != Some(id) {
            self.previous = self.current.replace(id.clone());
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DividerState {
    pub hover: bool,
    pub dragging: bool,
}

/// A session row (Sidebar) or tab (Tabs) held with the mouse: it moves as
/// the pointer crosses other sessions, and the order is saved on release.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionDrag {
    pub id: SessionId,
    /// The pointer's last row (Sidebar) or column (Tabs). A session only
    /// moves the way the pointer goes, so tabs of different widths don't
    /// swap back and forth under it.
    pub last: u16,
    /// The pointer moved with the button held.
    pub started: bool,
    /// The session moved.
    pub moved: bool,
}

impl SessionDrag {
    /// `id` pressed at row or column `at`.
    pub fn new(id: SessionId, at: u16) -> Self {
        Self {
            id,
            last: at,
            started: false,
            moved: false,
        }
    }
}

/// Clickable regions recorded while drawing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hit {
    /// Session list row (0 = the folder filter; folder headers count).
    ListRow(usize),
    /// The folder filter (top of the list or left end of the tab bar).
    FolderSelector,
    /// The open Folders modal (clicks inside it don't close it).
    FolderMenu,
    /// A row of the Folders modal.
    FolderRow(usize),
    /// The footer's mode badge: `SESSION` switches to Rakumux mode,
    /// `RAKUMUX` goes back to the session you came from.
    ModeBadge,
    /// A key hint on the right of the footer.
    FooterKey(FooterKey),
}

/// What a footer hint does when tapped: what its keys do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FooterKey {
    /// `Ctrl-\ rakumux` (session mode): switch to Rakumux mode.
    Rakumux,
    /// `? help`: open Help.
    Help,
    /// `, settings`: open Settings.
    Settings,
}

/// Everything `App::new` needs; tests swap the connector, executor and
/// paths.
pub struct AppOptions {
    pub settings: Settings,
    /// Where settings are saved (`config.json`); `None` disables saving.
    pub config_path: Option<std::path::PathBuf>,
    pub scope: ScopeRoot,
    /// Rakumux's data directories (history files for AI naming, daemon
    /// registration for restarts).
    pub paths: Option<Layout>,
    /// `RAKUMUX_SESSION_ID` when the TUI runs inside a session.
    pub self_session: Option<SessionId>,
    pub connector: Connector,
    /// Suggests names for auto-named sessions (`ai_namer` in production).
    pub namer: Namer,
    /// Remote access for the phone app (`LocalPhoneAccess` in production).
    pub phone: Arc<dyn PhoneAccess>,
    pub exec_mode: ExecMode,
    pub debug: bool,
}

pub struct App {
    pub settings: Settings,
    config_path: Option<std::path::PathBuf>,
    pub flags: FeatureFlags,
    pub debug: bool,
    /// The folder Rakumux started in.
    pub scope: ScopeRoot,
    /// The folders the daemon knows, and what it can list.
    pub folders: Folders,
    paths: Option<Layout>,
    pub self_session: Option<SessionId>,
    /// Takes you from session mode to Rakumux mode (`Ctrl-\\`).
    pub rakumux_key: KeyBinding,
    /// `None` when cycling is turned off in the config.
    pub cycle_keys: Option<CycleKeys>,
    /// Rakumux mode: the first key of `gg` was pressed.
    pub pending_key: Option<char>,
    /// The session Rakumux mode was entered from: Esc and the Rakumux
    /// key go back to it.
    pub came_from: Option<SessionId>,
    /// The sessions worked in most recently, for Tab ("last session").
    pub recent: RecentSessions,

    exec: Exec,
    rx: Option<UnboundedReceiver<AppEvent>>,
    connector: Connector,
    namer: Namer,
    phone: Arc<dyn PhoneAccess>,
    /// Phone access is on; `None` until checked at startup.
    pub phone_enabled: Option<bool>,
    /// The one-time phone access offer waits for nothing else to be on
    /// screen.
    phone_offer_pending: bool,
    pub connection: Connection,
    /// The daemon was reachable once; losing it triggers reconnects.
    was_connected: bool,
    /// Whether the daemon was checked for the features this TUI shows
    /// (once per run).
    checked_daemon_features: bool,
    /// The server was just restarted from here; the next connection says
    /// how.
    restarted_daemon: Option<ServerRestart>,
    /// The session the user was typing in when its stream closed (a server
    /// restart, a lost connection): the next sync goes back into it.
    resume_focus: Option<(SessionId, Instant)>,
    /// `--ptyd-path`: the server binary a restart in place switches to.
    pub ptyd_path: Option<std::path::PathBuf>,
    reconnect_at: Option<Instant>,
    reconnect_attempts: u32,
    sync_generation: u64,
    sync_cancel: Option<CancelFlag>,

    pub list: SessionList,
    pub detail: Detail,
    pub focus: Focus,
    pub zoomed: bool,
    /// The list/detail divider: hovered or being dragged.
    pub divider: DividerState,
    /// A session being dragged to another place in the list.
    pub drag: Option<SessionDrag>,
    pub mode: AppMode,
    /// Whole terminal area (kept current by the event loop and `draw`).
    pub area: Rect,
    /// Clickable regions from the last draw.
    pub hits: Vec<(Rect, Hit)>,
    /// Session tab click targets from the last draw (`layout::tab_at`).
    pub tab_hits: Vec<(usize, Rect)>,
    /// Detail generation whose attach should take focus when it completes.
    pending_focus: Option<u64>,
    status: Option<(String, Instant)>,
    naming_started: HashSet<SessionId>,
    /// Bytes to write to the real terminal (OSC 52 clipboard).
    terminal_output: Vec<u8>,

    pub pending_update_version: Option<String>,
    pub update_install_method: Option<crate::updater::InstallMethod>,
    /// Reload this UI after detaching and restoring the terminal. Does not
    /// stop or restart the daemon.
    pub restart_requested: bool,
    pub auth: AuthService,
}

impl App {
    pub fn new(options: AppOptions) -> Self {
        let (exec, rx) = Exec::new(options.exec_mode);
        let mut warnings = Vec::new();
        let rakumux_key = KeyBinding::parse(options.settings.sessions.rakumux_key_spec())
            .unwrap_or_else(|e| {
                warnings.push(format!("sessions.rakumux_key: {e}; using Ctrl-\\"));
                KeyBinding::default()
            });
        let cycle_keys = parse_cycle_keys(&options.settings, &mut warnings);
        let mut list = SessionList::new(options.scope.clone());
        list.set_session_order(&options.settings.sessions.session_order);
        let folders = Folders::new(options.scope.clone());
        let mut app = Self {
            flags: FeatureFlags::from_env(),
            debug: options.debug,
            scope: options.scope,
            folders,
            paths: options.paths,
            self_session: options.self_session,
            rakumux_key,
            cycle_keys,
            pending_key: None,
            came_from: None,
            recent: RecentSessions::default(),
            exec,
            rx: Some(rx),
            connector: options.connector,
            namer: options.namer,
            phone: options.phone,
            phone_enabled: None,
            phone_offer_pending: false,
            connection: Connection::Connecting,
            was_connected: false,
            checked_daemon_features: false,
            restarted_daemon: None,
            resume_focus: None,
            ptyd_path: None,
            reconnect_at: None,
            reconnect_attempts: 0,
            sync_generation: 0,
            sync_cancel: None,
            list,
            detail: Detail::default(),
            focus: Focus::List,
            zoomed: false,
            divider: DividerState::default(),
            drag: None,
            mode: AppMode::Normal,
            area: Rect::new(0, 0, 80, 24),
            hits: Vec::new(),
            tab_hits: Vec::new(),
            pending_focus: None,
            status: None,
            naming_started: HashSet::new(),
            terminal_output: Vec::new(),
            pending_update_version: None,
            update_install_method: None,
            restart_requested: false,
            auth: AuthService::new_from_env(),
            settings: options.settings,
            config_path: options.config_path,
        };
        if !warnings.is_empty() {
            app.set_status(warnings.join("; "));
        }
        app
    }

    /// Ask before reloading this UI. Confirming leaves the daemon and all
    /// session programs running; cancelling leaves this window unchanged.
    pub fn request_restart(&mut self) {
        self.mode = AppMode::Confirm(ConfirmDialog {
            title: "Restart Rakumux?".into(),
            message: "This will reload this UI with the latest build. The daemon and all running sessions will keep running.".into(),
            action: ConfirmAction::RestartUi,
        });
    }

    /// Start connecting to the daemon (call after the first paint).
    pub fn start(&mut self) {
        self.connect();
        self.check_phone_access();
    }

    /// The event receiver, for the event loop's `select!`.
    pub fn take_receiver(&mut self) -> Option<UnboundedReceiver<AppEvent>> {
        self.rx.take()
    }

    /// Handle every pending background event (tests; inline executor).
    #[cfg(test)]
    pub fn process_pending(&mut self) {
        loop {
            self.exec.poll_inline();
            let Some(rx) = self.rx.as_mut() else { return };
            let mut events = Vec::new();
            while let Ok(event) = rx.try_recv() {
                events.push(event);
            }
            if events.is_empty() {
                return;
            }
            for event in events {
                self.handle_event(event);
            }
        }
    }

    /// Write the settings to `config.json`.
    pub fn save_settings(&self) -> anyhow::Result<()> {
        match &self.config_path {
            Some(path) => {
                // An unrelated setting saved by a stale window must never
                // re-enable output sharing after another window opted out.
                Settings::update_at(path, |current| {
                    let consent = current.sessions.ai_activity_summaries;
                    *current = self.settings.clone();
                    current.sessions.ai_activity_summaries = consent;
                })
            }
            None => Err(anyhow::anyhow!("no config file location (HOME unset)")),
        }
    }

    /// Only the explicit summary confirmation/toggle may change this shared
    /// consent value. Preserve settings written by another window meanwhile.
    pub(super) fn save_summary_consent(&self, enabled: bool) -> anyhow::Result<()> {
        let path = self
            .config_path
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("no config file location (HOME unset)"))?;
        Settings::update_at(path, |settings| {
            settings.sessions.ai_activity_summaries = enabled
        })
    }

    // ------------------------------------------------------------ status

    pub fn set_status(&mut self, message: impl Into<String>) {
        self.status = Some((message.into(), Instant::now()));
    }

    pub fn status(&self) -> Option<&str> {
        self.status.as_ref().map(|(m, _)| m.as_str())
    }

    // ------------------------------------------------------------ layout

    pub fn session_layout(&self) -> SessionLayout {
        self.settings.sessions.layout
    }

    /// Where keys go now is the session's program (session mode with no
    /// dialog): what's typed there is the user's own and is never logged.
    pub fn keys_go_to_session(&self) -> bool {
        self.focus == Focus::Session && matches!(self.mode, AppMode::Normal)
    }

    /// Where keys go and what's open, for the input log: `session` or
    /// `rakumux`, then the mode (`Normal`, a dialog's name).
    pub fn input_state(&self) -> String {
        let mode = format!("{:?}", self.mode);
        let mode: String = mode
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric())
            .collect();
        let focus = match self.focus {
            Focus::Session => "session",
            Focus::List => "rakumux",
        };
        let pending = self
            .pending_key
            .map(|k| format!(" pending {k}"))
            .unwrap_or_default();
        let filter = if self.list.filter_editing {
            " filtering"
        } else {
            ""
        };
        format!("{focus} {mode}{pending}{filter}")
    }

    /// Rakumux's data directories.
    pub fn paths(&self) -> Option<&Layout> {
        self.paths.as_ref()
    }

    /// `--debug` (or `RAKUMUX_DEBUG`) was given.
    pub fn debug(&self) -> bool {
        self.debug
    }

    /// A phone-width window (`layout::PHONE_BELOW`): the list or a session,
    /// never both.
    pub fn phone(&self) -> bool {
        crate::layout::is_phone(self.area.width)
    }

    /// How the list keys and taps behave: a phone always shows the list in
    /// Rakumux mode, so it behaves like the Sidebar there.
    pub fn list_layout(&self) -> SessionLayout {
        if self.phone() {
            SessionLayout::Sidebar
        } else {
            self.session_layout()
        }
    }

    /// The list sizes set by dragging the dividers.
    fn pane_sizes(&self) -> crate::layout::PaneSizes {
        crate::layout::PaneSizes {
            list_cols: self.settings.sessions.sidebar_width,
            list_percent: self.settings.sessions.stacked_list_percent,
        }
    }

    pub fn screen(&self) -> screen::Screen {
        let mut screen = screen::compute(
            self.area,
            self.session_layout(),
            self.focus,
            self.zoomed,
            self.pane_sizes(),
        );
        if self.focus == Focus::List {
            if let (Some(terminal), Some(summary)) = (
                screen.terminal,
                self.detail
                    .session_id()
                    .and_then(|id| self.list.get(id))
                    .and_then(|session| session.activity_summary.as_ref()),
            ) {
                let text = crate::components::format::activity_summary_details(
                    summary,
                    crate::components::format::now_secs(),
                );
                let rows = crate::components::utils::wrapped_rows(
                    &text,
                    usize::from(terminal.width.saturating_sub(2)),
                )
                .min(usize::from(terminal.height.saturating_sub(4)))
                    as u16;
                if rows > 0 {
                    screen.activity_summary = Some(ratatui::layout::Rect {
                        height: rows,
                        ..terminal
                    });
                    screen.terminal = Some(ratatui::layout::Rect {
                        y: terminal.y + rows,
                        height: terminal.height - rows,
                        ..terminal
                    });
                }
            }
        }
        screen
    }

    /// `(cols, rows)` for attaching/resizing.
    pub fn terminal_size(&self) -> (u16, u16) {
        screen::terminal_size(
            self.area,
            self.session_layout(),
            self.zoomed,
            self.pane_sizes(),
        )
    }

    /// Show the welcome tour from its first card. Its "Don't show this on
    /// startup" box starts as the setting is.
    pub fn open_welcome(&mut self) {
        self.mode = AppMode::Welcome(types::WelcomeState {
            card: 0,
            dont_show: self.settings.sessions.welcome_seen,
        });
    }

    /// Close the welcome tour, keeping its "Don't show this on startup" box:
    /// unticked, the tour shows again on the next start.
    pub fn close_welcome(&mut self, dont_show: bool) {
        self.mode = AppMode::Normal;
        if self.settings.sessions.welcome_seen != dont_show {
            self.settings.sessions.welcome_seen = dont_show;
            if let Err(error) = self.save_settings() {
                self.set_status(format!("Couldn't save settings: {error}"));
            }
        }
    }

    /// The marks for folders and branches the user picked.
    pub fn icons(&self) -> crate::icons::Icons {
        self.settings.sessions.icons
    }

    /// What the Settings and Help modals show: the configured keys, the
    /// layout and the General tab's current values.
    pub fn modal_context(&self, in_session: bool) -> ModalContext {
        ModalContext {
            rakumux_key: self.rakumux_key.label(),
            cycle_keys: self.cycle_keys.map(|k| k.short_label()).unwrap_or_default(),
            cycle_previous: self
                .cycle_keys
                .map(|k| k.previous.label())
                .unwrap_or_default(),
            cycle_next: self.cycle_keys.map(|k| k.next.label()).unwrap_or_default(),
            cycle_in_session: self.settings.sessions.cycle_keys_in_session,
            icons: self.icons(),
            layout: self.session_layout(),
            version: app_version().to_string(),
            in_session,
            general: SettingsItem::ALL
                .iter()
                .map(|item| self.setting_row(*item))
                .collect(),
        }
    }

    /// A General tab row: what `item` sets and its current value.
    fn setting_row(&self, item: SettingsItem) -> SettingRow {
        let ai = &self.settings.ai;
        let key_value = |key: &Option<String>| {
            if key.is_some() {
                "set".to_string()
            } else {
                "not set".to_string()
            }
        };
        let naming = "Used for AI session naming and optional activity summaries";
        let (label, value, note) = match item {
            SettingsItem::Layout => {
                let layout = self.session_layout();
                let value = SessionLayout::ALL
                    .iter()
                    .map(|l| {
                        if *l == layout {
                            format!("[{}]", l.label())
                        } else {
                            l.label().to_string()
                        }
                    })
                    .collect::<Vec<_>>()
                    .join(" ");
                ("Layout", value, layout.description())
            }
            SettingsItem::Icons => {
                let current = self.icons();
                let value = crate::icons::Icons::ALL
                    .iter()
                    .map(|i| {
                        if *i == current {
                            format!("[{}]", i.label())
                        } else {
                            i.label().to_string()
                        }
                    })
                    .collect::<Vec<_>>()
                    .join(" ");
                let sample = |i: crate::icons::Icons| format!("{} {}", i.folder(), i.branch());
                use crate::icons::Icons as I;
                return SettingRow {
                    section: item.section(),
                    label: "Icons".into(),
                    value,
                    note: format!(
                        "Marks for folders and branches. Plain {} works in any font. Emoji {} \
                         needs a colour emoji font (macOS and Windows have one). Nerd Font {} \
                         needs a Nerd Font set in your terminal. If one shows as boxes here, your \
                         terminal can't show it.",
                        sample(I::Plain),
                        sample(I::Emoji),
                        sample(I::Nerd)
                    ),
                };
            }
            SettingsItem::CycleInSessions => {
                let keys = self
                    .cycle_keys
                    .map(|k| k.short_label())
                    .unwrap_or_else(|| "Option+↑/↓".into());
                let value = if self.settings.sessions.cycle_keys_in_session {
                    "[On] Off"
                } else {
                    "On [Off]"
                };
                return SettingRow {
                    section: item.section(),
                    label: "Option+↑/↓ in sessions".into(),
                    value: value.into(),
                    note: format!(
                        "On: {keys} switch sessions from inside a session too, so the program \
                         there never gets them (OpenCode, Pi and Claude Code use them). Off: they \
                         switch sessions in Rakumux mode only."
                    ),
                };
            }
            SettingsItem::PhoneAccess => {
                let value = match self.phone_enabled {
                    Some(true) => "[On] Off",
                    Some(false) => "On [Off]",
                    None => "…",
                };
                let note = if self.phone_enabled == Some(true) {
                    "Use Rakumux from the Rakumux app on your phone, over Tailscale. Enter \
                     shows the QR code to pair a phone; x there turns phone access off. Kept in \
                     ~/.rakumux/remote.json."
                } else {
                    "Use Rakumux from the Rakumux app on your phone, over Tailscale. Enter \
                     turns it on and shows the QR code to pair a phone. Only devices on your \
                     tailnet with the pairing code get in. Kept in ~/.rakumux/remote.json."
                };
                ("Phone access", value.to_string(), note)
            }
            SettingsItem::AnthropicKey => (
                "Anthropic API key",
                key_value(&ai.anthropic_api_key),
                naming,
            ),
            SettingsItem::OpenAiKey => ("OpenAI API key", key_value(&ai.openai_api_key), naming),
            SettingsItem::PrimaryAiProvider => {
                let value = match normalize_primary_provider(ai.ai_primary_provider.as_deref()) {
                    "openai" => "OpenAI",
                    _ => "Anthropic",
                };
                ("AI provider", value.to_string(), naming)
            }
            SettingsItem::OpenAiModel => (
                "OpenAI model",
                effective_openai_model(ai.openai_model.as_deref()),
                naming,
            ),
            SettingsItem::ActivitySummaries => (
                "Activity summaries",
                if self.settings.sessions.ai_activity_summaries { "[On] Off" } else { "On [Off]" }.to_string(),
                "Opt-in: shares bounded, best-effort-redacted output from non-private sessions with your AI provider. Adds a short current-activity line below each session. Generated once by the daemon for every client; may incur charges. Enter enables with confirmation or disables immediately.",
            ),
        };
        SettingRow {
            section: item.section(),
            label: label.to_string(),
            value,
            note: note.to_string(),
        }
    }

    /// Bytes the event loop must write to the real terminal.
    pub fn take_terminal_output(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.terminal_output)
    }

    /// Whether something animates (spinners) and wants frequent ticks.
    pub fn is_animating(&self) -> bool {
        matches!(self.connection, Connection::Connecting)
            || matches!(self.detail.view, DetailView::Loading(_))
            || match &self.mode {
                AppMode::NewSession(d) => d.busy,
                AppMode::Folders(d) => d.busy,
                AppMode::Renaming(d) => d.busy,
                AppMode::DaemonRestart(d) => d.restarting,
                AppMode::Upgrade(d) => d.checking || d.installing,
                AppMode::Pairing(d) => d.busy(),
                _ => false,
            }
    }

    /// Timers: status expiry and reconnects. Returns whether to redraw.
    pub fn on_tick(&mut self, now: Instant) -> bool {
        let mut changed = self.is_animating();
        changed |= self.offer_phone_access();
        if let Some((_, at)) = &self.status {
            if now.duration_since(*at) >= STATUS_TTL {
                self.status = None;
                changed = true;
            }
        }
        if let Some(at) = self.reconnect_at {
            if now >= at {
                self.reconnect_at = None;
                self.connect();
                changed = true;
            }
        }
        changed
    }

    // ------------------------------------------------------------ events

    pub fn handle_event(&mut self, event: AppEvent) {
        match event {
            AppEvent::Connected(api) => self.on_connected(api),
            AppEvent::ConnectFailed(error) => self.on_connect_failed(error),
            AppEvent::DaemonRestarted(result) => self.on_daemon_restarted(result),
            AppEvent::Synced { generation, result } => self.on_synced(generation, result),
            AppEvent::SessionEvent { generation, event } => {
                if generation == self.sync_generation {
                    self.on_session_event(event);
                }
            }
            AppEvent::SubscriptionLost { generation, error } => {
                if generation == self.sync_generation {
                    self.connection_lost(error);
                }
            }
            AppEvent::Attached { generation, result } => self.on_attached(generation, result),
            AppEvent::Attach { generation, event } => self.on_attach_event(generation, event),
            AppEvent::SavedScreen { generation, result } => {
                self.on_saved_screen(generation, result)
            }
            AppEvent::FoldersLoaded { result } => self.on_folders_loaded(result),
            AppEvent::FolderAdded { result } => self.on_folder_added(result),
            AppEvent::Pinned { id, pinned, result } => self.on_pinned(id, pinned, result),
            AppEvent::Created { result } => self.on_created(result),
            AppEvent::Renamed {
                id,
                result,
                automatic,
            } => self.on_renamed(id, result, automatic),
            AppEvent::Deleted { id, result } => self.on_deleted(id, result),
            AppEvent::Killed { id, result } => self.on_killed(id, result),
            AppEvent::Revived { id, result } => self.on_revived(id, result),
            AppEvent::Touched { result } => {
                if let Err(error) = result {
                    self.set_status(format!("Couldn't mark the session as used: {error}"));
                }
            }
            AppEvent::NameSuggested {
                id,
                expected_name,
                result,
            } => self.on_name_suggested(id, expected_name, result),
            AppEvent::Upgrade(message) => self.handle_upgrade_message(message),
            AppEvent::PhoneChecked { enabled, offer } => self.on_phone_checked(enabled, offer),
            AppEvent::PhonePairing(result) => self.on_phone_pairing(result),
            AppEvent::PhoneOff(result) => self.on_phone_off(result),
            AppEvent::JobFailed(message) => {
                self.set_status(format!("Background task failed: {message}"))
            }
        }
    }

    /// Input from the terminal.
    pub async fn handle_terminal_event(&mut self, event: Event) -> AppResult {
        // Someone is using this window (not just a mouse moving over it):
        // the session it shows takes its size.
        let used = match &event {
            Event::Key(key) => key.kind != KeyEventKind::Release,
            Event::Mouse(mouse) => !matches!(
                mouse.kind,
                crossterm::event::MouseEventKind::Moved | crossterm::event::MouseEventKind::Up(_)
            ),
            Event::Paste(_) | Event::FocusGained => true,
            Event::Resize(..) | Event::FocusLost => false,
        };
        let result = match event {
            Event::Key(key) => self.handle_key(key).await,
            Event::Mouse(mouse) => {
                self.handle_mouse(mouse);
                AppResult::Continue
            }
            Event::Paste(text) => {
                self.handle_paste(&text);
                AppResult::Continue
            }
            Event::Resize(width, height) => {
                self.resize(width, height);
                AppResult::Continue
            }
            Event::FocusGained | Event::FocusLost => AppResult::Continue,
        };
        if used {
            self.claim_terminal_size();
        }
        result
    }

    pub async fn handle_key(&mut self, key: KeyEvent) -> AppResult {
        if key.kind == KeyEventKind::Release {
            return AppResult::Continue;
        }
        if !self.flags.auth
            && matches!(
                self.mode,
                AppMode::Auth(_) | AppMode::AuthEmail(_) | AppMode::AuthCode(_)
            )
        {
            self.mode = AppMode::Normal;
            self.set_status("Auth disabled");
            return AppResult::Continue;
        }
        if !matches!(self.mode, AppMode::Normal) {
            return handlers::dialogs::handle_dialog_key(self, key).await;
        }
        match self.focus {
            Focus::List => handlers::list::handle_list_key(self, key),
            Focus::Session => handlers::session::handle_session_key(self, key),
        }
    }

    /// `-1`/`1` when `key` is a configured cycle key.
    pub fn cycle_direction(&self, key: &KeyEvent) -> Option<i32> {
        self.cycle_keys.as_ref().and_then(|k| k.direction(key))
    }

    pub fn resize(&mut self, width: u16, height: u16) {
        self.area = Rect::new(0, 0, width, height);
        self.sync_terminal_size();
    }

    // ------------------------------------------------------------ daemon

    fn connect(&mut self) {
        self.connection = Connection::Connecting;
        let connector = self.connector.clone();
        self.exec.job(move || {
            // A panicking client must end in a visible error, not a spinner.
            match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| connector())) {
                Ok(Ok(api)) => AppEvent::Connected(api),
                Ok(Err(error)) => AppEvent::ConnectFailed(error),
                Err(_) => AppEvent::ConnectFailed(ApiError::new(
                    session_core::api::ErrorCode::Internal,
                    "the daemon client failed unexpectedly",
                )),
            }
        });
    }

    /// Manual refresh (`Ctrl-r`): reconnect when disconnected, otherwise
    /// reload the list and resubscribe.
    pub fn refresh(&mut self) {
        match self.connection {
            Connection::Connected(_) => self.sync(),
            Connection::Connecting => {}
            Connection::Failed(_) => {
                self.reconnect_at = None;
                self.connect();
            }
        }
    }

    fn on_connected(&mut self, api: Arc<dyn session_core::client::SessionApi>) {
        let missing = missing_features(api.as_ref());
        self.connection = Connection::Connected(api);
        if let Some(restart) = self.restarted_daemon.take() {
            self.set_status(match restart {
                ServerRestart::InPlace => {
                    "Restarted the Rakumux server; your sessions kept running."
                }
                ServerRestart::Replaced => {
                    "Restarted the Rakumux server. Sessions that were running ended; Enter \
                     brings one back."
                }
            });
        } else if self.was_connected {
            self.set_status("Reconnected to the daemon");
        }
        self.was_connected = true;
        self.reconnect_attempts = 0;
        self.reconnect_at = None;
        if !std::mem::replace(&mut self.checked_daemon_features, true) && !missing.is_empty() {
            let in_place = api_restarts_in_place(self.api().as_deref());
            self.offer_daemon_upgrade(missing, in_place);
        }
        self.sync();
    }

    /// The daemon still running from an older Rakumux lacks features this
    /// one shows: explain, and offer to restart it (in place when it can,
    /// keeping the sessions; otherwise `y` only, since it ends them).
    fn offer_daemon_upgrade(&mut self, missing: Vec<String>, in_place: bool) {
        if !matches!(self.mode, AppMode::Normal) {
            self.set_status(format!(
                "The running Rakumux server is older than this app (no {}); \
                 `rakumux daemon restart` updates it.",
                missing.join(", ")
            ));
            return;
        }
        self.mode = AppMode::DaemonRestart(DaemonRestartDialog {
            daemon_version: self.paths.as_ref().and_then(connection::daemon_version),
            in_place,
            detail: String::new(),
            missing,
            restarting: false,
            error: None,
        });
    }

    fn on_connect_failed(&mut self, error: ApiError) {
        if connection::is_protocol_mismatch(&error) {
            self.mode = AppMode::DaemonRestart(DaemonRestartDialog {
                daemon_version: self.paths.as_ref().and_then(connection::daemon_version),
                in_place: false,
                detail: error.message.clone(),
                missing: Vec::new(),
                restarting: false,
                error: None,
            });
        } else if self.was_connected {
            self.schedule_reconnect();
        }
        self.connection = Connection::Failed(error);
    }

    fn schedule_reconnect(&mut self) {
        // A server restarting in place answers again within moments.
        let delay = match self.reconnect_attempts {
            0 => Duration::from_millis(250),
            n => Duration::from_secs(1 << (n - 1).min(4)).min(RECONNECT_MAX),
        };
        self.reconnect_attempts += 1;
        self.reconnect_at = Some(Instant::now() + delay);
    }

    /// The subscription (and so presumably the daemon) went away.
    fn connection_lost(&mut self, reason: String) {
        if let Some(cancel) = self.sync_cancel.take() {
            cancel.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        self.sync_generation += 1;
        self.set_status(format!(
            "Lost connection to the daemon ({reason}); reconnecting…"
        ));
        self.connection = Connection::Failed(ApiError::new(
            session_core::api::ErrorCode::DaemonUnavailable,
            reason,
        ));
        self.schedule_reconnect();
    }

    fn on_daemon_restarted(&mut self, result: Result<ServerRestart, String>) {
        match result {
            Ok(restart) => {
                self.mode = AppMode::Normal;
                self.set_status(match restart {
                    ServerRestart::InPlace => "Restarting the Rakumux server…",
                    ServerRestart::Replaced => "Stopped the old server; starting a new one…",
                });
                self.restarted_daemon = Some(restart);
                self.connect();
            }
            Err(error) => {
                if let AppMode::DaemonRestart(dialog) = &mut self.mode {
                    dialog.restarting = false;
                    dialog.error = Some(error);
                } else {
                    self.set_status(format!("Daemon restart failed: {error}"));
                }
            }
        }
    }

    /// Restart the daemon (after consent in the dialog): in place when it
    /// can, otherwise stop it so a new one starts.
    fn restart_daemon(&mut self) {
        let in_place = matches!(&self.mode, AppMode::DaemonRestart(d) if d.in_place);
        if let (true, Some(api)) = (in_place, self.api()) {
            if let AppMode::DaemonRestart(dialog) = &mut self.mode {
                dialog.restarting = true;
                dialog.error = None;
            }
            self.let_go_of_daemon();
            let binary = session_core::paths::find_ptyd_binary(self.ptyd_path.as_deref())
                .and_then(|path| std::path::absolute(path).ok());
            self.exec.job(move || {
                AppEvent::DaemonRestarted(
                    api.restart_daemon(binary.as_deref())
                        .map(|()| ServerRestart::InPlace)
                        .map_err(|error| error.message),
                )
            });
            return;
        }
        let Some(paths) = self.paths.clone() else {
            if let AppMode::DaemonRestart(dialog) = &mut self.mode {
                dialog.error = Some("Rakumux's data directory is unknown (HOME unset)".into());
            }
            return;
        };
        if let AppMode::DaemonRestart(dialog) = &mut self.mode {
            dialog.restarting = true;
            dialog.error = None;
        }
        // An older daemon we're connected to: let go of it first, so its
        // exit isn't reported as a lost connection.
        self.let_go_of_daemon();
        self.exec.job(move || {
            AppEvent::DaemonRestarted(
                connection::stop_daemon(&paths).map(|()| ServerRestart::Replaced),
            )
        });
    }

    /// Stop listening to the daemon before it goes away on purpose, so that
    /// isn't reported as a lost connection.
    fn let_go_of_daemon(&mut self) {
        if self.connection.is_connected() {
            if let Some(cancel) = self.sync_cancel.take() {
                cancel.store(true, std::sync::atomic::Ordering::Relaxed);
            }
            self.sync_generation += 1;
            self.detail.retarget(DetailView::Empty);
            self.connection = Connection::Connecting;
        }
    }
}

/// Features this TUI shows, by the capability a daemon announces for them.
const DAEMON_FEATURES: [(&str, &str); 4] = [
    ("session.git", "git branches and worktrees"),
    ("projects", "folders"),
    ("session.pin", "pins"),
    (
        session_core::api::CAPABILITY_SESSIONS_ALL,
        "new folders live",
    ),
];

/// The daemon restarts in place, keeping the sessions (`daemon.restart`).
fn api_restarts_in_place(api: Option<&dyn session_core::client::SessionApi>) -> bool {
    api.and_then(|api| api.daemon_capabilities())
        .is_some_and(|capabilities| {
            capabilities
                .iter()
                .any(|c| c == session_core::api::CAPABILITY_DAEMON_RESTART)
        })
}

/// What the connected daemon can't do of [`DAEMON_FEATURES`]; nothing when
/// its capabilities are unknown.
fn missing_features(api: &dyn session_core::client::SessionApi) -> Vec<String> {
    let Some(capabilities) = api.daemon_capabilities() else {
        return Vec::new();
    };
    DAEMON_FEATURES
        .iter()
        .filter(|(capability, _)| !capabilities.iter().any(|c| c == capability))
        .map(|(_, feature)| feature.to_string())
        .collect()
}

/// Both keys empty turns cycling off; an invalid key falls back to its
/// default with a warning.
fn parse_cycle_keys(settings: &Settings, warnings: &mut Vec<String>) -> Option<CycleKeys> {
    let keys = &settings.sessions.cycle_keys;
    if keys.previous.trim().is_empty() && keys.next.trim().is_empty() {
        return None;
    }
    let defaults = CycleKeys::default();
    let mut parse = |spec: &str, fallback: KeyBinding, field: &str| {
        KeyBinding::parse(spec).unwrap_or_else(|e| {
            warnings.push(format!(
                "sessions.cycle_keys.{field}: {e}; using {}",
                fallback.label()
            ));
            fallback
        })
    };
    Some(CycleKeys {
        previous: parse(&keys.previous, defaults.previous, "previous"),
        next: parse(&keys.next, defaults.next, "next"),
    })
}
