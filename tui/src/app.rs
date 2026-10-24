//! Application state and the dispatch of input and background events.
//!
//! The TUI is a thin client of the session daemon
//! (`session_core::client::SessionApi`): it lists the sessions of every
//! folder (narrowed by the folder filter), keeps the list live from the
//! daemon's event stream, mirrors the selected session's terminal as an
//! observer and takes control when the user focuses it. Other TUIs and CLI
//! calls may act on the same sessions at any time.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossterm::event::{Event, KeyEvent, KeyEventKind};
use ratatui::layout::Rect;
use session_core::api::ApiError;
use session_core::ids::SessionId;
use session_core::model::ScopeRoot;
use session_core::paths::Layout;
use tokio::sync::mpsc::UnboundedReceiver;

use crate::account::AccountAccess;
use crate::auth::service::AuthService;
use crate::help::{ModalContext, SettingRow};
use crate::layout::SessionLayout;
use crate::remote_access::PhoneAccess;

mod account;
mod auto_name;
pub mod config;
mod connection;
mod detail;
mod feature_flags;
pub mod feed;
pub mod folders;
pub mod grid;
pub mod grouping;
mod handlers;
mod keys;
mod list_tabs;
mod new_session;
pub mod path_complete;
mod phone_access;
mod runtime;
pub mod screen;
mod session_list;
mod sessions;
pub mod terminal_panel;
mod text_input;
pub mod types;
mod upgrade;

/// Seconds since the Unix epoch, what session times are in.
fn wall_clock() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}

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
pub use runtime::{AppEvent, ExecMode};
pub use session_list::{ListRow, Selection, SessionList};
pub use text_input::TextInput;

use runtime::{CancelFlag, Exec};
use types::*;

/// Version helper: single place to resolve version string.
pub fn app_version() -> &'static str {
    option_env!("RAKUMUX_VERSION").unwrap_or(env!("CARGO_PKG_VERSION"))
}

/// How long a status message stays in the footer.
const STATUS_TTL: Duration = Duration::from_secs(6);
const RECONNECT_MAX: Duration = Duration::from_secs(10);

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
    TerminalPanel,
    TerminalPanelHide,
    TerminalPanelDivider,
    /// Session list row (0 = the tabs; headers count).
    ListRow(usize),
    /// The pin left of a hovered session row's name (that list row):
    /// pins or unpins its session.
    PinToggle(usize),
    /// The ↻ at the end of a hovered session row's AI description (that list
    /// row): makes a new summary of its session.
    SummaryRefresh(usize),
    /// Visible Grid card in filtered session order (last is New session).
    GridCell(usize),
    /// Previous/next Grid page.
    GridPage(isize),
    /// The top row of the list around its tabs, the left end of the tab
    /// bar, Grid's header: opens the active tab's modal.
    FolderSelector,
    /// A tab on the list's top row: activates it, or opens its modal when
    /// it's the active one.
    ListTab(grouping::ListTab),
    /// A reply in the feed (its index, newest first): selects it, or opens
    /// it when it's selected already (at once on a phone).
    FeedEntry(usize),
    /// The feed detail's Reply button: writes back to the selected reply.
    FeedReply,
    /// The open Folders modal (clicks inside it don't close it).
    FolderMenu,
    /// A row of the Folders modal.
    FolderRow(usize),
    /// The open App or Status modal (clicks inside it don't close it).
    SectionMenu,
    /// A row of the App or Status modal.
    SectionRow(usize),
    /// The footer's mode badge: `SESSION` switches to Rakumux mode,
    /// `RAKUMUX` goes back to the session you came from.
    ModeBadge,
    /// A key hint on the right of the footer.
    FooterKey(FooterKey),
}

/// What a footer button does when tapped: what its keys do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FooterKey {
    /// `Ctrl-\ Rakumux` (session mode): switch to Rakumux mode.
    Rakumux,
    /// `? Help` (Rakumux mode): open Help.
    Help,
    /// `, Settings` (Rakumux mode): open Settings.
    Settings,
    /// `Sign in` (Rakumux mode, signed out): sign in to the Rakumux account.
    SignIn,
    /// `✓ Signed in` (Rakumux mode, signed in): the account dialog.
    Account,
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
    /// The Rakumux account and AI descriptions (`LocalAccount` in production).
    pub account: Arc<dyn AccountAccess>,
    /// Update checks and installation, replaced by a fake in tests.
    pub updater: Arc<dyn crate::updater::UpdateService>,
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
    pub came_from_terminal: bool,
    /// The sessions worked in most recently, for Tab ("last session").
    pub recent: RecentSessions,

    exec: Exec,
    rx: Option<UnboundedReceiver<AppEvent>>,
    connector: Connector,
    namer: Namer,
    phone: Arc<dyn PhoneAccess>,
    updater: Arc<dyn crate::updater::UpdateService>,
    update_generation: u64,
    /// Phone access is on; `None` until checked at startup.
    pub phone_enabled: Option<bool>,
    /// The one-time phone access offer waits for nothing else to be on
    /// screen.
    phone_offer_pending: bool,
    account_access: Arc<dyn AccountAccess>,
    /// Who is signed in (Settings → Account).
    pub account: AccountStatus,
    /// The account was asked for and hasn't answered; when to ask again.
    account_checking: bool,
    account_check_at: Option<Instant>,
    /// When to ask where a sign-in is up to; a question is outstanding.
    sign_in_poll_at: Option<Instant>,
    sign_in_polling: bool,
    /// Each session's latest AI description, shown under its name.
    descriptions: HashMap<SessionId, String>,
    descriptions_at: Option<Instant>,
    descriptions_busy: bool,
    descriptions_generation: u64,
    /// Sessions whose new AI summary is being made, as the service said.
    summaries_refreshing: HashSet<SessionId>,
    /// Sessions a summary was asked for whose request hasn't been answered.
    summary_asks: HashSet<SessionId>,
    /// The agent feed (`app/feed`), shown by the list's Feed tab.
    pub feed: feed::FeedStore,
    /// The Feed tab is the active one (where Master detail can show it).
    feed_tab: bool,
    /// A tab was chosen this run: the feed isn't put in front any more.
    tab_chosen: bool,
    /// The account's Feed switch was seen on this run.
    feed_switch_seen: bool,
    /// A switch was changed from Settings; the next account check says if
    /// saving it failed.
    features_saving: bool,
    pub connection: Connection,
    /// The daemon was reachable once; losing it triggers reconnects.
    was_connected: bool,
    /// Whether the daemon was checked for the features this TUI shows
    /// (once per run).
    checked_daemon_features: bool,
    /// The server was just restarted from here; the next connection says
    /// how.
    restarted_daemon: Option<ServerRestart>,
    /// The running server's version, read when it connects (Settings).
    server_version: Option<String>,
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
    pub grid: grid::GridPreviews,
    pub terminal_panel: terminal_panel::TerminalPanel,
    pub focus: Focus,
    /// Terminal focus reporting prevents background windows acknowledging replies.
    window_focused: bool,
    pub zoomed: bool,
    /// The list/detail divider: hovered or being dragged.
    pub divider: DividerState,
    /// A session being dragged to another place in the list.
    pub drag: Option<SessionDrag>,
    /// The list row under the mouse pointer (terminals that report
    /// movement): a session row there shows its pin toggle.
    pub hovered_row: Option<usize>,
    pub mode: AppMode,
    /// Whole terminal area (kept current by the event loop and `draw`).
    pub area: Rect,
    /// Clickable regions from the last draw.
    pub hits: Vec<(Rect, Hit)>,
    /// Session tab click targets from the last draw (`layout::tab_at`).
    pub tab_hits: Vec<(usize, Rect)>,
    /// Detail generation whose attach should take focus when it completes.
    pending_focus: Option<u64>,
    /// Prevent duplicate creates and misrouted input before the new session attaches.
    creating_session: bool,
    status: Option<(String, Instant)>,
    naming_started: HashSet<SessionId>,
    /// Bytes to write to the real terminal (OSC 52 clipboard).
    terminal_output: Vec<u8>,

    pub pending_update_version: Option<String>,
    pub update_install_method: Option<crate::updater::InstallMethod>,
    /// Reload this UI after detaching and restoring the terminal. Does not
    /// stop or restart the daemon.
    pub restart_requested: bool,
    pub restart_launcher: Option<std::path::PathBuf>,
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
        list.set_clock(wall_clock());
        list.set_session_order(&options.settings.sessions.session_order);
        list.set_section_order(&options.settings.sessions.section_order);
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
            came_from_terminal: false,
            recent: RecentSessions::default(),
            exec,
            rx: Some(rx),
            connector: options.connector,
            namer: options.namer,
            phone: options.phone,
            updater: options.updater,
            update_generation: 0,
            phone_enabled: None,
            phone_offer_pending: false,
            account_access: options.account,
            account: AccountStatus::Unknown,
            account_checking: false,
            account_check_at: None,
            sign_in_poll_at: None,
            sign_in_polling: false,
            descriptions: HashMap::new(),
            descriptions_at: None,
            descriptions_busy: false,
            descriptions_generation: 0,
            summaries_refreshing: HashSet::new(),
            summary_asks: HashSet::new(),
            feed: feed::FeedStore::new(rakumux_sensagent::feed::machine_name()),
            feed_tab: false,
            tab_chosen: false,
            feed_switch_seen: false,
            features_saving: false,
            connection: Connection::Connecting,
            was_connected: false,
            checked_daemon_features: false,
            restarted_daemon: None,
            server_version: None,
            resume_focus: None,
            ptyd_path: None,
            reconnect_at: None,
            reconnect_attempts: 0,
            sync_generation: 0,
            sync_cancel: None,
            list,
            detail: Detail::default(),
            grid: grid::GridPreviews::default(),
            terminal_panel: terminal_panel::TerminalPanel::default(),
            focus: Focus::List,
            window_focused: true,
            zoomed: false,
            divider: DividerState::default(),
            drag: None,
            hovered_row: None,
            mode: AppMode::Normal,
            area: Rect::new(0, 0, 80, 24),
            hits: Vec::new(),
            tab_hits: Vec::new(),
            pending_focus: None,
            creating_session: false,
            status: None,
            naming_started: HashSet::new(),
            terminal_output: Vec::new(),
            pending_update_version: None,
            update_install_method: None,
            restart_requested: false,
            restart_launcher: None,
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
            Some(path) => self.settings.save_to(path),
            None => Err(anyhow::anyhow!("no config file location (HOME unset)")),
        }
    }

    // ------------------------------------------------------------ status

    pub fn set_status(&mut self, message: impl Into<String>) {
        self.status = Some((message.into(), Instant::now()));
    }

    pub fn status(&self) -> Option<&str> {
        self.status.as_ref().map(|(m, _)| m.as_str())
    }

    // ------------------------------------------------------------ layout

    /// The layout in use: the one chosen, except on a phone, which has no
    /// Grid (it shows Master detail instead).
    pub fn session_layout(&self) -> SessionLayout {
        match self.settings.sessions.layout {
            SessionLayout::Grid if self.no_grid() => SessionLayout::Sidebar,
            layout => layout,
        }
    }

    /// The layout `v` switches to (phones skip Grid).
    pub fn next_layout(&self) -> SessionLayout {
        self.session_layout().next_on(self.no_grid())
    }

    /// This screen has no Grid: a phone, once its size is known.
    fn no_grid(&self) -> bool {
        self.area.width > 0 && self.phone()
    }

    /// Where keys go now is the session's program (session mode with no
    /// dialog): what's typed there is the user's own and is never logged.
    pub fn keys_go_to_session(&self) -> bool {
        matches!(self.focus, Focus::Session | Focus::Terminal)
            && matches!(self.mode, AppMode::Normal)
    }

    /// Session input, pending shell input and credential dialogs must never
    /// expose typed characters in the input-debug log.
    pub fn input_is_private(&self) -> bool {
        self.keys_go_to_session()
            || (self.came_from_terminal && self.terminal_panel.open)
            || self.creating_session
            || self.pending_focus.is_some()
            || self.terminal_panel.busy()
            || matches!(self.mode, AppMode::AuthEmail(_) | AppMode::AuthCode(_))
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
            Focus::Terminal => "terminal",
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

    /// The list sizes set by dragging the dividers.
    fn pane_sizes(&self) -> crate::layout::PaneSizes {
        crate::layout::PaneSizes {
            list_cols: self.settings.sessions.sidebar_width,
            list_percent: self.settings.sessions.stacked_list_percent,
        }
    }

    pub fn screen(&self) -> screen::Screen {
        self.screen_for(self.focus)
    }

    pub(super) fn screen_for(&self, focus: Focus) -> screen::Screen {
        let screen = screen::compute(
            self.area,
            self.session_layout(),
            focus,
            self.zoomed,
            self.pane_sizes(),
        );
        screen::with_terminal_panel(
            screen,
            self.terminal_panel.open,
            focus == Focus::Terminal,
            self.terminal_panel.height_percent,
        )
    }

    /// `(cols, rows)` for attaching/resizing.
    pub fn terminal_size(&self) -> (u16, u16) {
        if !self.terminal_panel.open {
            return screen::terminal_size(
                self.area,
                self.session_layout(),
                self.zoomed,
                self.pane_sizes(),
            );
        }
        self.screen_for(Focus::Session)
            .terminal
            .map(|area| (area.width.max(2), area.height.max(2)))
            .unwrap_or((80, 24))
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
    /// layout and the settings' current values.
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

    /// A row of Settings: what `item` sets and its current value.
    fn setting_row(&self, item: SettingsItem) -> SettingRow {
        let (label, value, note) = match item {
            SettingsItem::Layout => {
                let layout = self.session_layout();
                let value = SessionLayout::offered(self.no_grid())
                    .map(|l| {
                        if l == layout {
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
                    disabled: false,
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
                    disabled: false,
                };
            }
            SettingsItem::Update => (
                "Update",
                self.pending_update_version.as_ref().map(|version| format!("{version} available · U"))
                    .unwrap_or_else(|| format!("{} · U", app_version())),
                "Check for a new release, confirm installation, then restart only this UI. The daemon and all running sessions stay up. Source builds and admin-owned installations may need a manual update.",
            ),
            SettingsItem::RestartUi => (
                "Restart UI",
                "R".to_string(),
                "Reload this UI with the latest build. The server and every running session keep running.",
            ),
            SettingsItem::RestartServer => {
                let value = match self.server_version.as_deref() {
                    Some(running) if running != app_version() => {
                        format!("{running} → {} · S", app_version())
                    }
                    Some(running) => format!("{running} · S"),
                    None => "S".into(),
                };
                (
                    "Restart server",
                    value,
                    "Restart the Rakumux server in place with this build, e.g. after an update: sessions and their programs keep running and every window reconnects. Same as `rakumux daemon restart`.",
                )
            }
            SettingsItem::Account => {
                let value = match &self.account {
                    AccountStatus::SignedIn(account) => account.name().to_string(),
                    AccountStatus::SignedOut => "Sign in".into(),
                    AccountStatus::Unknown => "…".into(),
                };
                (
                    "Account",
                    value,
                    "Sign in to your Rakumux account for AI summaries under each session and the agent feed (Available on sign-in, below). Same as `rakumux account login`.",
                )
            }
            SettingsItem::AiSummaries | SettingsItem::AgentFeed => {
                let feature = match item {
                    SettingsItem::AiSummaries => AccountFeature::AiSummaries,
                    _ => AccountFeature::AgentFeed,
                };
                let (value, disabled) = match &self.account {
                    AccountStatus::SignedIn(account) => match feature.of(account) {
                        Some(true) => ("[On] Off", false),
                        Some(false) => ("On [Off]", false),
                        None => ("…", false),
                    },
                    _ => ("On Off", true),
                };
                let note = match feature {
                    AccountFeature::AiSummaries => "A short AI summary of each agent chat under the session's name, made once the agent has written 5 messages or finished, then when you ask (the ↻ on a hovered summary, or `s`). Your prompt and the agent's messages since are sent to rakumux.com to write it; private sessions never are. Without one the line shows your last message. Saved to your account, as on rakumux.com/features.",
                    AccountFeature::AgentFeed => "The Feed tab: what your agents said when they finished a turn, from every machine signed in to your account, newest first. Each finished reply is sent to rakumux.com, keys and tokens masked, private sessions never, and kept for 30 days. Saved to your account, as on rakumux.com/features.",
                };
                return SettingRow {
                    section: item.section(),
                    label: feature.label().into(),
                    value: value.into(),
                    note: note.into(),
                    disabled,
                };
            }
            SettingsItem::RemoteReplies => {
                let signed_in = matches!(self.account, AccountStatus::SignedIn(_));
                let value = match (signed_in, self.settings.sessions.remote_replies) {
                    (false, _) => "On Off",
                    (true, true) => "[On] Off",
                    (true, false) => "On [Off]",
                };
                return SettingRow {
                    section: item.section(),
                    label: "Replies from rakumux.com".into(),
                    value: value.into(),
                    note: "Type replies written to the feed on rakumux.com, or on your other machines, into this machine's agent sessions, as if you typed them: only into the reply's own session, while it shows that chat and its agent waits for you. Replies can make your agents run commands here, so leave this off unless you want to steer them from elsewhere. Replies written in this window always go in. This machine only, never changed from the website.".into(),
                    disabled: !signed_in,
                };
            }
            SettingsItem::TerminalPanel => (
                "Terminal panel",
                if self.terminal_panel.open { "Available · Enter focuses" } else { "Open · T" }.into(),
                "Open a temporary shell below the main detail in the current directory, without creating a session. Exit closes the pane; hiding with × or the Rakumux key then T keeps it running. Closing this UI ends its temporary shell, not any sessions.",
            ),
        };
        SettingRow {
            section: item.section(),
            label: label.to_string(),
            value,
            note: note.to_string(),
            disabled: false,
        }
    }

    /// Each setting's search text (Settings' search), in order.
    pub fn settings_search_text(&self) -> Vec<String> {
        SettingsItem::ALL
            .iter()
            .map(|item| {
                let row = self.setting_row(*item);
                crate::help::search_text(row.section, &row.label)
            })
            .collect()
    }

    /// Bytes the event loop must write to the real terminal.
    pub fn take_terminal_output(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.terminal_output)
    }

    /// Whether something animates (spinners) and wants frequent ticks.
    pub fn is_animating(&self) -> bool {
        matches!(self.connection, Connection::Connecting)
            || self.list.visible().iter().any(|session| {
                session.status == session_core::model::SessionStatus::Live
                    && session.lifecycle == session_core::model::Lifecycle::Working
            })
            || self.creating_session
            || self.terminal_panel.busy()
            || matches!(self.detail.view, DetailView::Loading(_))
            || match &self.mode {
                AppMode::Folders(d) => d.busy,
                AppMode::Renaming(d) => d.busy,
                AppMode::FeedReply(d) => d.busy,
                AppMode::DaemonRestart(d) => d.restarting,
                AppMode::Upgrade(d) => d.checking || d.installing,
                AppMode::Pairing(d) => d.busy(),
                AppMode::Account(d) => d.busy(),
                _ => false,
            }
    }

    /// Timers: status expiry, reconnects and done's sub-sections as time
    /// passes. Returns whether to redraw.
    pub fn on_tick(&mut self, now: Instant) -> bool {
        self.sync_grid();
        self.sync_terminal_panel();
        let mut changed = self.is_animating();
        changed |= self.list.set_clock(wall_clock());
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
        self.account_timers(now);
        self.feed_timers(now);
        changed
    }

    // ------------------------------------------------------------ events

    pub fn handle_event(&mut self, event: AppEvent) {
        match event {
            AppEvent::PanelAttached { generation, result } => {
                self.on_panel_attached(generation, result)
            }
            AppEvent::PanelAttach { generation, event } => {
                self.on_panel_attach_event(generation, event)
            }
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
            AppEvent::PreviewAttached {
                id,
                generation,
                result,
            } => self.on_preview_attached(id, generation, result),
            AppEvent::PreviewSaved {
                id,
                generation,
                result,
            } => self.on_preview_saved(id, generation, result),
            AppEvent::PreviewReady {
                id,
                generation,
                mailbox,
            } => {
                for event in mailbox.drain() {
                    self.on_preview_event(&id, generation, event);
                }
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
            AppEvent::Upgrade {
                generation,
                message,
            } => {
                if generation == self.update_generation {
                    self.handle_upgrade_message(message);
                }
            }
            AppEvent::PhoneChecked { enabled, offer } => self.on_phone_checked(enabled, offer),
            AppEvent::PhonePairing(result) => self.on_phone_pairing(result),
            AppEvent::PhoneOff(result) => self.on_phone_off(result),
            AppEvent::AccountChecked(result) => self.on_account_checked(result),
            AppEvent::SignIn(result) => self.on_sign_in(result),
            AppEvent::SignInCancelled(result) => {
                if let Err(error) = result {
                    self.set_status(format!("Couldn't cancel the sign-in: {error}"));
                }
            }
            AppEvent::SignedOut(result) => self.on_signed_out(result),
            AppEvent::Descriptions { generation, result } => {
                self.on_descriptions(generation, result)
            }
            AppEvent::Summarized { session, result } => self.on_summarized(session, result),
            AppEvent::FeaturesSet(result) => self.on_features_set(result),
            AppEvent::FeedTurn {
                generation,
                entry,
                appended,
                result,
            } => self.on_feed_turn(generation, entry, appended, result),
            AppEvent::Feed { generation, result } => self.on_feed(generation, result),
            AppEvent::FeedReplied { generation, result } => {
                self.on_feed_replied(generation, result)
            }
            AppEvent::JobFailed(message) => {
                self.set_status(format!("Background task failed: {message}"))
            }
        }
        self.sync_grid();
        self.sync_terminal_panel();
    }

    /// Input from the terminal.
    pub async fn handle_terminal_event(&mut self, event: Event) -> AppResult {
        let panel_pointer = match &event {
            Event::Mouse(mouse) => {
                let screen = self.screen();
                self.terminal_panel.dragging
                    || [screen.terminal_panel, screen.terminal_panel_divider]
                        .into_iter()
                        .flatten()
                        .any(|area| {
                            mouse.column >= area.x
                                && mouse.column < area.right()
                                && mouse.row >= area.y
                                && mouse.row < area.bottom()
                        })
            }
            _ => false,
        };
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
            Event::FocusGained => {
                self.window_focused = true;
                AppResult::Continue
            }
            Event::FocusLost => {
                self.window_focused = false;
                AppResult::Continue
            }
        };
        if used {
            self.window_focused = true;
            if !panel_pointer {
                self.claim_terminal_size();
            }
            self.claim_panel_size();
            self.acknowledge_visible_completion();
        }
        self.sync_grid();
        result
    }

    pub async fn handle_key(&mut self, key: KeyEvent) -> AppResult {
        if key.kind == KeyEventKind::Release {
            return AppResult::Continue;
        }
        if self.creating_session || self.pending_focus.is_some() {
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
            Focus::Terminal => self.handle_terminal_panel_key(key),
        }
    }

    /// `-1`/`1` when `key` is a configured cycle key.
    pub fn cycle_direction(&self, key: &KeyEvent) -> Option<i32> {
        self.cycle_keys.as_ref().and_then(|k| k.direction(key))
    }

    pub fn resize(&mut self, width: u16, height: u16) {
        self.area = Rect::new(0, 0, width, height);
        self.sync_terminal_size();
        self.sync_panel_size();
        self.sync_grid();
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
        self.server_version = self.paths.as_ref().and_then(connection::daemon_version);
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
        // The account service follows the daemon, so it starts once one runs.
        if self.account == AccountStatus::Unknown {
            self.check_account();
        }
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
            requested: false,
        });
    }

    /// `S` or Settings: restart the server after asking, in place when it
    /// can (sessions keep running), with this build's `rakumux-ptyd`.
    pub fn request_server_restart(&mut self) {
        if !self.connection.is_connected() {
            self.set_status("Not connected to the server; Ctrl-r reconnects.");
            return;
        }
        self.mode = AppMode::DaemonRestart(DaemonRestartDialog {
            daemon_version: self.server_version.clone(),
            in_place: api_restarts_in_place(self.api().as_deref()),
            detail: String::new(),
            missing: Vec::new(),
            restarting: false,
            error: None,
            requested: true,
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
                requested: false,
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
        self.terminal_panel_disconnected();
        self.grid.clear();
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
        self.terminal_panel_disconnected();
        self.grid.clear();
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
const DAEMON_FEATURES: [(&str, &str); 11] = [
    ("session.git", "git branches and worktrees"),
    ("projects", "folders"),
    ("session.pin", "pins"),
    (
        session_core::api::CAPABILITY_SESSIONS_ALL,
        "new folders live",
    ),
    (
        session_core::api::CAPABILITY_TERMINAL_PANE,
        "the terminal panel",
    ),
    (
        session_core::api::CAPABILITY_WORKING_DIRECTORY,
        "the branch an agent works on",
    ),
    (
        session_core::api::CAPABILITY_AGENT_OUTPUT,
        "each agent's latest output",
    ),
    (
        session_core::api::CAPABILITY_USER_PROMPT,
        "your last message in each session",
    ),
    (
        session_core::api::CAPABILITY_TURN,
        "AI summaries of each agent's turn",
    ),
    (session_core::api::CAPABILITY_REPLIES, "the agent feed"),
    (
        session_core::api::CAPABILITY_TRANSCRIPT,
        "whole chats in the agent feed",
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
