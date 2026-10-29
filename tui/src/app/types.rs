//! Modes, dialogs and small shared types of the TUI.

use std::path::{Path, PathBuf};

use session_core::ids::SessionId;
use session_core::model::ScopeRoot;

use super::grouping::{Grouping, Section};
use super::path_complete::DirSuggestion;
use super::text_input::TextInput;
use crate::help::{ModalState, ModalTab};
use crate::updater::InstallMethod;

/// What the event loop should do after handling input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppResult {
    Continue,
    Exit,
    /// Quit and let the shell wrapper `cd` into this directory.
    SwitchDirectory(String),
}

/// Keyboard target when no dialog is open: Rakumux, the selected session,
/// or the independent terminal panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Focus {
    /// Rakumux mode: keys drive Rakumux (the list or tab bar); the
    /// detail pane observes the selected session.
    #[default]
    List,
    /// Session mode: keys go to the selected session, which this client
    /// controls, except the Rakumux key.
    Session,
    /// Keys go to the independent shell underneath the main detail pane.
    Terminal,
}

/// The Sprite dialog (`AppMode::Sprite`).
#[derive(Debug, Clone)]
pub struct SpriteDialog {
    pub state: SpriteState,
}

#[derive(Debug, Clone)]
pub enum SpriteState {
    /// A Sprites token was found on this computer: use it?
    Found {
        token: String,
        source: &'static str,
    },
    /// No token: where to get one, and the field to paste it in.
    Paste {
        input: TextInput,
        error: Option<String>,
    },
    /// Signed in: are replies from rakumux.com typed into its agents?
    Replies {
        token: String,
    },
    /// Setting up; the step it's at.
    Working {
        step: crate::sprites::Step,
    },
    Failed {
        message: String,
    },
    /// Set up: what's there and what's left to do in its first session.
    Ready {
        sprite: String,
        /// The programs the card names that it has.
        tools: Vec<String>,
        feed: crate::sprites::FeedSetUp,
    },
    /// Copy this folder's `.env` files to the Sprite? Asked once per folder.
    Env {
        folder: ScopeRoot,
        /// The files, as paths in the folder.
        files: Vec<std::path::PathBuf>,
    },
}

#[derive(Debug, Clone)]
pub struct RenameDialog {
    pub session_id: SessionId,
    pub current_name: String,
    pub input: TextInput,
    pub error_message: Option<String>,
    /// The rename request is in flight.
    pub busy: bool,
}

/// Writing back to a feed entry, or to the session of a prompt still waiting
/// for its answer (`r` in the Feed): typed into the session on its machine
/// once its agent waits.
#[derive(Debug, Clone)]
pub struct FeedReplyDialog {
    pub to: super::feed::ReplyTo,
    /// Where it goes: the entry's session and machine (`None`: this one).
    pub session_name: String,
    pub machine: Option<String>,
    /// The start of what's answered.
    pub answering: String,
    pub input: TextInput,
    pub error_message: Option<String>,
    /// The reply is being stored.
    pub busy: bool,
    /// The id and text of the last send: sending the same text again (after
    /// a time-out, say, when rakumux.com may have stored it all the same)
    /// reuses the id, so it's stored and typed in once.
    pub sent: Option<(String, String)>,
}

/// Actions that need a yes/no confirmation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfirmAction {
    /// Reload only this UI, leaving the daemon and sessions running.
    RestartUi,
    /// Terminate, then remove the record and its scrollback.
    DeleteSession { id: SessionId, name: String },
    /// Terminate the process; the record stays (Exited, restartable).
    CloseSession { id: SessionId, name: String },
}

#[derive(Debug, Clone)]
pub struct ConfirmDialog {
    pub title: String,
    pub message: String,
    pub action: ConfirmAction,
}

/// What the Folders modal is doing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PickerStage {
    /// Typing a filter or a folder path and moving through the rows.
    Browse,
    /// Enter on "+ Create folder": asking before creating this folder.
    Creating(PathBuf),
}

/// A folder typed in the Folders modal that the daemon doesn't know yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NewFolder {
    /// An existing folder (canonical).
    Existing(PathBuf),
    /// A folder to create: its parent exists (canonical parent + name).
    Create(PathBuf),
}

impl NewFolder {
    pub fn path(&self) -> &Path {
        match self {
            NewFolder::Existing(path) | NewFolder::Create(path) => path,
        }
    }
}

/// One row of the Folders modal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PickerRow {
    /// All folders: no filter.
    All,
    /// A folder: the filter can show it.
    Folder(ScopeRoot),
    /// Add the typed folder (creating it if needed), then show it.
    Add(NewFolder),
    /// A folder that completes the typed path: Tab puts it in, Enter adds
    /// it (or shows it when the daemon knows it already).
    Suggestion(DirSuggestion),
}

/// The Folders modal (Enter on the folder filter, `P`, a click on it): All
/// folders, then the folders that have sessions (and those in the filter),
/// narrowed by what's typed; a typed folder path can be added. Enter shows
/// the folder under the cursor alone, All folders clears the filter, and
/// once ticks were changed (Tab, Space) Enter shows the ticked folders.
#[derive(Debug, Clone)]
pub struct FolderPicker {
    /// What's typed: a filter, or a folder path (`/`, `~`, `.`, or with a
    /// `/` in it).
    pub query: TextInput,
    /// The rows for the query, top to bottom: for a path, the typed folder
    /// (to add, or the folder it already is), the folders completing it,
    /// then the listed folders inside it; otherwise All folders and the
    /// matching folders.
    pub rows: Vec<PickerRow>,
    /// The first row is the typed folder.
    pub typed_folder: bool,
    /// Why the typed folder can't be added, when it looks like a path.
    pub folder_note: Option<String>,
    /// What Tab puts in when no suggestion is chosen: the typed path
    /// completed as far as the matching folders agree.
    pub completion: Option<String>,
    /// Matching folders not listed.
    pub more_folders: usize,
    /// Ticked folders, in the order they were ticked; none means All
    /// folders. At first the filter's.
    pub checked: Vec<ScopeRoot>,
    /// Ticks were changed with Tab or Space: Enter shows the ticked
    /// folders (and the one under the cursor).
    pub ticks_changed: bool,
    /// Row under the cursor.
    pub cursor: usize,
    pub stage: PickerStage,
    pub error: Option<String>,
    /// An add request is in flight.
    pub busy: bool,
}

impl FolderPicker {
    /// A modal ticked like `filter`, with nothing typed yet.
    pub fn new(filter: Vec<ScopeRoot>) -> Self {
        Self {
            query: TextInput::default(),
            rows: Vec::new(),
            typed_folder: false,
            folder_note: None,
            completion: None,
            more_folders: 0,
            checked: filter,
            ticks_changed: false,
            cursor: 0,
            stage: PickerStage::Browse,
            error: None,
            busy: false,
        }
    }

    /// The row under the cursor.
    pub fn current_row(&self) -> Option<&PickerRow> {
        self.rows.get(self.cursor)
    }

    /// Whether the row shows a tick: All folders when nothing else is
    /// ticked, a folder when it's ticked.
    pub fn is_ticked(&self, row: &PickerRow) -> bool {
        match row {
            PickerRow::All => self.checked.is_empty(),
            PickerRow::Folder(folder) => self.checked.contains(folder),
            PickerRow::Add(_) | PickerRow::Suggestion(_) => false,
        }
    }

    /// The folders Enter on `folder` shows: it alone, or once ticks were
    /// changed the ticked folders and it.
    pub fn folders_for(&self, folder: &ScopeRoot) -> Vec<ScopeRoot> {
        if !self.ticks_changed {
            return vec![folder.clone()];
        }
        let mut folders = self.checked.clone();
        if !folders.contains(folder) {
            folders.push(folder.clone());
        }
        folders
    }

    /// Whether any suggestion rows are listed.
    pub fn has_suggestions(&self) -> bool {
        self.rows
            .iter()
            .any(|row| matches!(row, PickerRow::Suggestion(_)))
    }
}

/// One row of the App or Status modal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SectionRow {
    /// All apps (statuses): no filter.
    All,
    /// An app or a status the filter can show.
    Section(Section),
}

/// The App or Status modal (Enter on the tabs with that tab active, a
/// click on the active tab, Enter on one of its headers): All apps (All
/// statuses), then each app (status), narrowed by what's typed. Enter shows
/// the one under the cursor alone, All clears the filter, and once ticks
/// were changed (Tab, Space) Enter shows the ticked ones.
#[derive(Debug, Clone)]
pub struct SectionPicker {
    /// `Grouping::App` or `Grouping::Status`.
    pub grouping: Grouping,
    /// What's typed: narrows the rows by name.
    pub query: TextInput,
    /// The rows for the query, top to bottom: All (while it matches), then
    /// the matching apps or statuses.
    pub rows: Vec<SectionRow>,
    /// Ticked apps or statuses, in the order they were ticked; none means
    /// all. At first the filter's.
    pub checked: Vec<Section>,
    /// Ticks were changed with Tab or Space: Enter shows the ticked ones
    /// (and the one under the cursor).
    pub ticks_changed: bool,
    /// Row under the cursor.
    pub cursor: usize,
}

impl SectionPicker {
    /// A modal for `grouping`, ticked like `filter`, with nothing typed yet.
    pub fn new(grouping: Grouping, filter: Vec<Section>) -> Self {
        Self {
            grouping,
            query: TextInput::default(),
            rows: Vec::new(),
            checked: filter,
            ticks_changed: false,
            cursor: 0,
        }
    }

    /// The row under the cursor.
    pub fn current_row(&self) -> Option<&SectionRow> {
        self.rows.get(self.cursor)
    }

    /// Whether the row shows a tick: All when nothing else is ticked, an
    /// app or a status when it's ticked.
    pub fn is_ticked(&self, row: &SectionRow) -> bool {
        match row {
            SectionRow::All => self.checked.is_empty(),
            SectionRow::Section(section) => self.checked.contains(section),
        }
    }

    /// What Enter on `section` shows: it alone, or once ticks were changed
    /// the ticked ones and it.
    pub fn sections_for(&self, section: &Section) -> Vec<Section> {
        if !self.ticks_changed {
            return vec![section.clone()];
        }
        let mut sections = self.checked.clone();
        if !sections.contains(section) {
            sections.push(section.clone());
        }
        sections
    }
}

/// Shown when the running daemon speaks another protocol version.
#[derive(Debug, Clone)]
pub struct DaemonRestartDialog {
    /// Version from the daemon's registration file, when readable.
    pub daemon_version: Option<String>,
    /// The daemon restarts in place (`daemon.restart`): sessions keep
    /// running.
    pub in_place: bool,
    pub detail: String,
    /// What this Rakumux shows that the running (older, compatible)
    /// daemon can't, e.g. "git branches and worktrees"; empty when the
    /// daemon speaks another protocol.
    pub missing: Vec<String>,
    pub restarting: bool,
    pub error: Option<String>,
    /// Asked for (`S`, Settings) rather than offered: nothing is missing,
    /// and declining just closes it.
    pub requested: bool,
}

/// How this TUI just restarted the server (the next connection says so).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServerRestart {
    /// In place: the sessions kept running.
    InPlace,
    /// Stopped and started anew: the running sessions ended.
    Replaced,
}

/// The settings listed in Settings, before the actions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsItem {
    Layout,
    /// Plain, emoji or Nerd Font marks for folders and branches.
    Icons,
    /// Option/Alt+↑/↓ also switch sessions inside a session.
    CycleInSessions,
    /// Install a new release and restart only this UI.
    Update,
    /// Reload this UI with the latest build (`R`).
    RestartUi,
    /// Restart the server in place with this build (`S`).
    RestartServer,
    /// Independent shell below the main detail pane.
    TerminalPanel,
    /// What this machine, Rakumux and every session's processes use now.
    Top,
    /// Sign in to the Rakumux account (AI session titles, the agent feed).
    Account,
    /// The account's AI session titles switch (as on rakumux.com/features).
    AiTitles,
    /// The account's Agent feed switch (likewise).
    AgentFeed,
    /// Type replies written on rakumux.com or other machines into this
    /// machine's sessions (local, `sessions.remote_replies`).
    RemoteReplies,
}

impl SettingsItem {
    /// In Settings' order: signing in first, then what it brings, then the rest.
    pub const ALL: [SettingsItem; 12] = [
        SettingsItem::Account,
        SettingsItem::AiTitles,
        SettingsItem::AgentFeed,
        SettingsItem::RemoteReplies,
        SettingsItem::Layout,
        SettingsItem::Icons,
        SettingsItem::CycleInSessions,
        SettingsItem::Update,
        SettingsItem::RestartUi,
        SettingsItem::RestartServer,
        SettingsItem::Top,
        SettingsItem::TerminalPanel,
    ];

    /// The heading Settings lists it under.
    pub fn section(self) -> &'static str {
        match self {
            SettingsItem::Layout | SettingsItem::Icons => "Appearance",
            SettingsItem::CycleInSessions => "Keyboard",
            SettingsItem::Update
            | SettingsItem::RestartUi
            | SettingsItem::RestartServer
            | SettingsItem::Top
            | SettingsItem::TerminalPanel => "App",
            SettingsItem::Account => "Account",
            SettingsItem::AiTitles | SettingsItem::AgentFeed | SettingsItem::RemoteReplies => {
                "Available on sign-in"
            }
        }
    }

    /// Its row in Settings.
    #[cfg(test)]
    pub fn index(self) -> usize {
        Self::ALL.iter().position(|item| *item == self).unwrap_or(0)
    }
}

#[derive(Debug, Clone)]
pub struct UpgradeDialog {
    pub current_version: String,
    pub latest_version: Option<String>,
    pub install_method: Option<InstallMethod>,
    pub checking: bool,
    pub installing: bool,
    pub update_available: bool,
    pub error_message: Option<String>,
    pub info_message: Option<String>,
    pub selection: usize,
}

#[derive(Debug, Clone)]
pub enum UpgradeMessage {
    CheckError(String),
    CheckNoUpdate,
    CheckAvailable {
        latest_version: String,
        method: InstallMethod,
    },
    InstallSuccess {
        launcher: Option<std::path::PathBuf>,
    },
    InstallError(String),
}

#[derive(Debug, Clone)]
pub struct AuthMenu {
    pub items: Vec<&'static str>,
    pub selected: usize,
}

#[derive(Debug, Clone)]
pub struct AuthEmailDialog {
    pub input: TextInput,
    pub error_message: Option<String>,
}

#[derive(Debug, Clone)]
pub struct AuthCodeDialog {
    pub input: TextInput,
    pub error_message: Option<String>,
}

/// The pairing dialog: what the phone app needs to connect (QR code, URL,
/// token), and turning phone access on or off.
#[derive(Debug, Clone)]
pub struct PairingDialog {
    pub state: PairingState,
}

impl PairingDialog {
    pub fn loading(turning_on: bool) -> Self {
        Self {
            state: PairingState::Loading { turning_on },
        }
    }

    /// Waiting for the server (a spinner shows).
    pub fn busy(&self) -> bool {
        matches!(
            self.state,
            PairingState::Loading { .. } | PairingState::TurningOff
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PairingState {
    /// Turning phone access on, or reading how to pair.
    Loading {
        turning_on: bool,
    },
    /// Phone access is on: pair with this (no URLs: nothing to listen on).
    Ready(crate::remote_access::Pairing),
    /// Phone access is off (turned off elsewhere meanwhile).
    Off,
    TurningOff,
    Failed(String),
}

/// The account dialog: signing in to the Rakumux account (a code approved
/// in any browser), who is signed in, and signing out.
#[derive(Debug, Clone)]
pub struct AccountDialog {
    pub state: AccountDialogState,
}

impl AccountDialog {
    /// Waiting for the account service (a spinner shows).
    pub fn busy(&self) -> bool {
        matches!(
            self.state,
            AccountDialogState::Loading
                | AccountDialogState::Starting
                | AccountDialogState::SigningOut
        )
    }

    /// A sign-in is under way (its status is polled).
    pub fn signing_in(&self) -> bool {
        matches!(
            self.state,
            AccountDialogState::Starting | AccountDialogState::Waiting(_)
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AccountDialogState {
    /// Asking the service who is signed in.
    Loading,
    SignedOut,
    /// Asking for a sign-in code.
    Starting,
    /// Waiting for the code to be approved: a `SignInView::Waiting`.
    Waiting(rakumux_sensagent::owner::protocol::SignInView),
    SignedIn(crate::account::Account),
    SigningOut,
    Failed(String),
}

/// Who is signed in, as far as this window knows.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum AccountStatus {
    /// Not checked yet.
    #[default]
    Unknown,
    SignedOut,
    SignedIn(crate::account::Account),
}

/// A switch of the Rakumux account that Settings → Available on sign-in
/// turns on and off, as rakumux.com/features does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccountFeature {
    AiTitles,
    AgentFeed,
}

impl AccountFeature {
    /// What Settings calls it.
    pub fn label(self) -> &'static str {
        match self {
            AccountFeature::AiTitles => "AI session titles",
            AccountFeature::AgentFeed => "Agent feed",
        }
    }

    /// Whether `account` has it on; `None` until read.
    pub fn of(self, account: &crate::account::Account) -> Option<bool> {
        match self {
            AccountFeature::AiTitles => account.titles,
            AccountFeature::AgentFeed => account.feed,
        }
    }
}

/// Where the welcome tour is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WelcomeState {
    /// The card shown, from 0.
    pub card: usize,
    /// The "Don't show this on startup" box (on every card).
    pub dont_show: bool,
}

/// Modal state. `Normal` routes keys by [`Focus`].
#[derive(Debug, Clone)]
pub enum AppMode {
    Normal,
    /// Setting up your Sprite for a new session.
    Sprite(SpriteDialog),
    /// The Folders modal: the folder filter.
    Folders(FolderPicker),
    /// The App or Status modal: that tab's filter.
    Sections(SectionPicker),
    Renaming(RenameDialog),
    /// Writing back to a feed entry.
    FeedReply(FeedReplyDialog),
    Confirm(ConfirmDialog),
    DaemonRestart(DaemonRestartDialog),
    /// The Settings modal (one page: settings, then actions) or the Help
    /// modal (the docs tabs); the tab says which.
    Modal {
        state: ModalState,
        /// Opened from a session (with the prefix) rather than the list.
        in_session: bool,
    },
    Upgrade(UpgradeDialog),
    /// The welcome tour (first start, or `t` in Help).
    Welcome(WelcomeState),
    /// "Use Rakumux from your phone?": offered once, when Tailscale runs
    /// here and phone access was never set up.
    PhoneOffer,
    /// The pairing dialog (the offer).
    Pairing(PairingDialog),
    /// The account dialog (Settings → Account).
    Account(AccountDialog),
    /// Top (Settings → Top, `t`): what Rakumux's processes use now.
    Top(super::top::TopView),
    Auth(AuthMenu),
    AuthEmail(AuthEmailDialog),
    AuthCode(AuthCodeDialog),
}

impl AppMode {
    /// The Settings or Help modal (whichever has `tab`) opened on `tab`;
    /// `settings` are the settings' search text, in order.
    pub fn modal(tab: ModalTab, in_session: bool, settings: Vec<String>) -> Self {
        AppMode::Modal {
            state: ModalState::new(tab, settings),
            in_session,
        }
    }
}
