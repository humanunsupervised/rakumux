//! Modes, dialogs and small shared types of the TUI.

use std::path::{Path, PathBuf};

use session_core::ids::SessionId;
use session_core::model::ScopeRoot;

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

/// Where keys go when no dialog is open: the two modes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Focus {
    /// Rakumux mode: keys drive Rakumux (the list or tab bar); the
    /// detail pane observes the selected session.
    #[default]
    List,
    /// Session mode: keys go to the selected session, which this client
    /// controls, except the Rakumux key.
    Session,
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

/// Actions that need a yes/no confirmation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfirmAction {
    /// Allow sharing bounded session output with the configured AI provider.
    EnableActivitySummaries,
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
}

/// How this TUI just restarted the server (the next connection says so).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServerRestart {
    /// In place: the sessions kept running.
    InPlace,
    /// Stopped and started anew: the running sessions ended.
    Replaced,
}

/// Rows of the Settings modal's General tab.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsItem {
    Layout,
    /// Plain, emoji or Nerd Font marks for folders and branches.
    Icons,
    /// Option/Alt+↑/↓ also switch sessions inside a session.
    CycleInSessions,
    /// Remote access for the phone app (`remote.json`), and pairing.
    PhoneAccess,
    AnthropicKey,
    OpenAiKey,
    PrimaryAiProvider,
    OpenAiModel,
    ActivitySummaries,
}

impl SettingsItem {
    pub const ALL: [SettingsItem; 9] = [
        SettingsItem::Layout,
        SettingsItem::Icons,
        SettingsItem::CycleInSessions,
        SettingsItem::PhoneAccess,
        SettingsItem::AnthropicKey,
        SettingsItem::OpenAiKey,
        SettingsItem::PrimaryAiProvider,
        SettingsItem::OpenAiModel,
        SettingsItem::ActivitySummaries,
    ];

    /// The General tab heading it's listed under.
    pub fn section(self) -> &'static str {
        match self {
            SettingsItem::Layout | SettingsItem::Icons => "Appearance",
            SettingsItem::CycleInSessions => "Keys",
            SettingsItem::PhoneAccess => "Phone",
            SettingsItem::AnthropicKey
            | SettingsItem::OpenAiKey
            | SettingsItem::PrimaryAiProvider
            | SettingsItem::OpenAiModel
            | SettingsItem::ActivitySummaries => "AI",
        }
    }

    /// Row on the General tab.
    pub fn index(self) -> usize {
        Self::ALL.iter().position(|item| *item == self).unwrap_or(0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApiProvider {
    Anthropic,
    OpenAI,
}

#[derive(Clone)]
pub struct ApiKeyDialog {
    pub provider: ApiProvider,
    pub input: TextInput,
    pub error_message: Option<String>,
}

impl std::fmt::Debug for ApiKeyDialog {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ApiKeyDialog")
            .field("provider", &self.provider)
            .field("input", &"<redacted>")
            .field("error_message", &self.error_message)
            .finish()
    }
}

#[derive(Debug, Clone)]
pub struct AiProviderDialog {
    pub selected: usize,
    pub current: String,
}

#[derive(Debug, Clone)]
pub struct OpenAiModelDialog {
    pub selected: usize,
    pub current: String,
    pub options: Vec<String>,
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
    InstallSuccess,
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
    /// Opened from Settings: closing goes back to its Phone access row.
    pub from_settings: bool,
}

impl PairingDialog {
    pub fn loading(from_settings: bool, turning_on: bool) -> Self {
        Self {
            state: PairingState::Loading { turning_on },
            from_settings,
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
    /// The Folders modal: the folder filter.
    Folders(FolderPicker),
    Renaming(RenameDialog),
    Confirm(ConfirmDialog),
    DaemonRestart(DaemonRestartDialog),
    /// The Settings modal (General and Keys tabs) or the Help modal (the
    /// docs tabs); the tab says which.
    Modal {
        state: ModalState,
        /// Opened from a session (with the prefix) rather than the list.
        in_session: bool,
    },
    /// Dialogs of the General tab's rows; they go back to the modal.
    ConfiguringApiKey(ApiKeyDialog),
    AiProvider(AiProviderDialog),
    OpenAiModel(OpenAiModelDialog),
    Upgrade(UpgradeDialog),
    /// The welcome tour (first start, or `t` in Help).
    Welcome(WelcomeState),
    /// "Use Rakumux from your phone?": offered once, when Tailscale runs
    /// here and phone access was never set up.
    PhoneOffer,
    /// The pairing dialog (Settings' Phone access row, or the offer).
    Pairing(PairingDialog),
    Auth(AuthMenu),
    AuthEmail(AuthEmailDialog),
    AuthCode(AuthCodeDialog),
}

impl AppMode {
    /// The Settings or Help modal (whichever has `tab`) opened on `tab`.
    pub fn modal(tab: ModalTab, in_session: bool) -> Self {
        AppMode::Modal {
            state: ModalState::new(tab, SettingsItem::ALL.len()),
            in_session,
        }
    }

    /// The setting the open dialog changes, when it's one of the General
    /// tab's dialogs.
    pub fn editing_setting(&self) -> Option<SettingsItem> {
        match self {
            AppMode::Confirm(ConfirmDialog {
                action: ConfirmAction::EnableActivitySummaries,
                ..
            }) => Some(SettingsItem::ActivitySummaries),
            AppMode::ConfiguringApiKey(dialog) => Some(match dialog.provider {
                ApiProvider::Anthropic => SettingsItem::AnthropicKey,
                ApiProvider::OpenAI => SettingsItem::OpenAiKey,
            }),
            AppMode::AiProvider(_) => Some(SettingsItem::PrimaryAiProvider),
            AppMode::Pairing(dialog) if dialog.from_settings => Some(SettingsItem::PhoneAccess),
            AppMode::OpenAiModel(_) => Some(SettingsItem::OpenAiModel),
            _ => None,
        }
    }
}
