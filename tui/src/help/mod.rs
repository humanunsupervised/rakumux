//! The Settings and Help modals, which share their frame and keys.
//!
//! - **Settings** (`,` or `o` in Rakumux mode): one scrolling page, sized
//!   to its contents, under a search field that narrows it as you type: the
//!   settings (layout, icons, session keys, update, terminal panel, the
//!   account and what it offers) for the caller to change, then every
//!   action and its keys, built from the user's configured keys, where
//!   `Enter` runs the selected action. `→` shows what the selected row does
//!   in a popover.
//! - **Help** (`?` in Rakumux mode): sessions and
//!   folders (the folder filter, All folders, multi-select, grouping, order,
//!   pinning, git branch and worktree), the screen, moving around, the mouse, layouts, observer vs
//!   controller, exited/dead sessions and revive, the single background
//!   server shared by several windows, the `rakumux` CLI and agent
//!   commands, AI naming, and the license.
//!
//! The caller keeps a [`ModalState`] while a modal is open (its tab says
//! which modal), routes keys to [`ModalState::handle_key`] (changing a
//! setting on [`ModalAction::Change`], running an action on
//! [`ModalAction::Run`]) and draws with [`render`] on top of the screen.

use std::cell::Cell;

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::block::Title;
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};
use ratatui::Frame;

use crate::layout::{self, char_width, str_width, truncate, SessionLayout, TabItem};
use crate::theme;

/// Opens Settings in Rakumux mode, besides `,`.
pub const SETTINGS_KEY: &str = "o";
/// The key the footer shows for Settings in Rakumux mode.
pub const SETTINGS_KEY_PRIMARY: &str = ",";

/// Help keeps room for long-form documentation; Settings hugs its rows.
const MIN_MODAL_WIDTH: u16 = 80;
const MIN_MODAL_HEIGHT: u16 = 24;
const MIN_SETTINGS_WIDTH: usize = 48;
const MAX_SETTINGS_WIDTH: usize = 100;

/// Columns Settings adds to its widest row, so its two columns (pinned
/// left and right) have room between them.
const SETTINGS_SPACE: usize = 16;

/// The most of the screen's height Settings takes, in percent.
const SETTINGS_MAX_HEIGHT: u32 = 80;

/// Lines a Help tab scrolls per notch of the mouse wheel.
const WHEEL_LINES: i32 = 3;

/// Body columns not used for text: one of padding on the left, two of gap
/// and one of scrollbar on the right.
const BODY_MARGIN: u16 = 4;

/// Lines moved by PgUp/PgDn before the first render tells us the body height.
const DEFAULT_PAGE: u16 = 10;

/// Settings: how far a value sits in when it goes on the line under its
/// label (a narrow screen).
const VALUE_INDENT: usize = 4;

/// Stands in for a literal backtick inside help markup, where backticks
/// delimit key names, so a configured key containing a backtick still
/// prints correctly.
const LITERAL_BACKTICK: char = '\u{E000}';

/// The two modals.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Modal {
    /// What you can change and do: the settings, then every action.
    Settings,
    /// How Rakumux works: Overview, Sessions, Layouts, Agents & CLI,
    /// License.
    Help,
}

impl Modal {
    /// The modal's tabs, in order.
    pub fn tabs(self) -> &'static [ModalTab] {
        match self {
            Modal::Settings => &[ModalTab::Settings],
            Modal::Help => &[
                ModalTab::Overview,
                ModalTab::Sessions,
                ModalTab::Layouts,
                ModalTab::AgentsCli,
                ModalTab::License,
            ],
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            Modal::Settings => "Settings",
            Modal::Help => "Help",
        }
    }

    /// The tab the modal opens on.
    pub fn first_tab(self) -> ModalTab {
        self.tabs()[0]
    }
}

/// Tabs of the two modals; each belongs to one ([`ModalTab::modal`]).
/// Settings has one page and shows no tab strip.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum ModalTab {
    /// The Settings page: the settings, then the actions and their keys.
    #[default]
    Settings,
    Overview,
    Sessions,
    Layouts,
    AgentsCli,
    License,
}

impl ModalTab {
    /// Every tab of both modals.
    #[cfg(test)]
    pub const ALL: [ModalTab; 6] = [
        ModalTab::Settings,
        ModalTab::Overview,
        ModalTab::Sessions,
        ModalTab::Layouts,
        ModalTab::AgentsCli,
        ModalTab::License,
    ];

    pub fn label(self) -> &'static str {
        match self {
            ModalTab::Settings => "Settings",
            ModalTab::Overview => "Overview",
            ModalTab::Sessions => "Sessions",
            ModalTab::Layouts => "Layouts",
            ModalTab::AgentsCli => "Agents & CLI",
            ModalTab::License => "License",
        }
    }

    /// Label for a tab strip too narrow for every [`ModalTab::label`]
    /// (an 80-column screen).
    pub fn short_label(self) -> &'static str {
        match self {
            ModalTab::AgentsCli => "CLI",
            tab => tab.label(),
        }
    }

    /// The modal this tab is in.
    pub fn modal(self) -> Modal {
        match self {
            ModalTab::Settings => Modal::Settings,
            _ => Modal::Help,
        }
    }

    /// Position among its modal's tabs (the number key is `index + 1`).
    pub fn index(self) -> usize {
        let tabs = self.modal().tabs();
        tabs.iter().position(|tab| *tab == self).unwrap_or(0)
    }

    /// The tab at `index` in the same modal as `self`.
    pub fn sibling(self, index: usize) -> Option<Self> {
        self.modal().tabs().get(index).copied()
    }

    pub fn next(self) -> Self {
        let tabs = self.modal().tabs();
        tabs[layout::cycle(tabs.len(), self.index(), 1)]
    }

    pub fn prev(self) -> Self {
        let tabs = self.modal().tabs();
        tabs[layout::cycle(tabs.len(), self.index(), -1)]
    }
}

/// An action listed in Settings with its keys, run with `Enter` there or
/// with its keys in Rakumux mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum KeyAction {
    /// From a session to Rakumux mode (the Rakumux key).
    RakumuxMode,
    /// Back into the session Rakumux mode was entered from.
    BackToSession,
    /// Work in the selected session (take control).
    GoIn,
    LastSession,
    /// `1`-`9`: into the session at that position.
    NumberedSession,
    NextSession,
    PreviousSession,
    /// `f`: show the feed with the keys, give it the keys, or hide it.
    Feed,
    /// `→`: the list's next tab (Status, Folders, App).
    NextTab,
    /// `←`: the previous tab.
    PreviousTab,
    Top,
    Bottom,
    HalfPageDown,
    HalfPageUp,
    /// `}`: the next folder group.
    NextFolder,
    /// `{`: the previous folder group.
    PreviousFolder,
    ScrollBack,
    ScrollForward,
    NewSession,
    /// `P`: the Folders modal (the folder filter).
    FilterFolders,
    Rename,
    Pin,
    /// `s`: a new AI summary of the selected session.
    Summarize,
    /// `w`: where the selected session has worked, under its row.
    Places,
    /// `W`: where every session has worked.
    AllPlaces,
    /// `y`: the path of the open place the list keys are on, else of where
    /// the selected session works now, on the clipboard.
    CopyPlace,
    /// `J`: one place down in the list (dragging does the same).
    MoveDown,
    /// `K`: one place up.
    MoveUp,
    Close,
    Delete,
    Zoom,
    Filter,
    /// Cycle Master detail, Grid and Tabs in Rakumux mode.
    CycleLayout,
    /// `b`: hide Master detail's list beside the session, or show it.
    HideList,
    /// Reveal/focus the independent shell below the detail pane.
    TerminalPanel,
    Help,
    Settings,
    /// Sign in to the Rakumux account, or see who's signed in.
    Account,
    /// The welcome tour carousel.
    Tour,
    CheckForUpdate,
    Reload,
    /// Reload only this UI, leaving the daemon and sessions running.
    Restart,
    /// Restart the server in place with this build; sessions keep running.
    RestartServer,
    QuitAndCd,
    Quit,
    /// Into the session and send it the Rakumux key itself.
    SendRakumuxKey,
}

impl KeyAction {
    /// The actions as Settings lists them, in order.
    pub const ALL: [KeyAction; 46] = [
        KeyAction::RakumuxMode,
        KeyAction::BackToSession,
        KeyAction::GoIn,
        KeyAction::LastSession,
        KeyAction::NumberedSession,
        KeyAction::SendRakumuxKey,
        KeyAction::NextSession,
        KeyAction::PreviousSession,
        KeyAction::Feed,
        KeyAction::NextTab,
        KeyAction::PreviousTab,
        KeyAction::Top,
        KeyAction::Bottom,
        KeyAction::HalfPageDown,
        KeyAction::HalfPageUp,
        KeyAction::NextFolder,
        KeyAction::PreviousFolder,
        KeyAction::ScrollBack,
        KeyAction::ScrollForward,
        KeyAction::NewSession,
        KeyAction::FilterFolders,
        KeyAction::Rename,
        KeyAction::Pin,
        KeyAction::Summarize,
        KeyAction::Places,
        KeyAction::AllPlaces,
        KeyAction::CopyPlace,
        KeyAction::MoveDown,
        KeyAction::MoveUp,
        KeyAction::Close,
        KeyAction::Delete,
        KeyAction::Zoom,
        KeyAction::Filter,
        KeyAction::CycleLayout,
        KeyAction::HideList,
        KeyAction::TerminalPanel,
        KeyAction::Help,
        KeyAction::Settings,
        KeyAction::Account,
        KeyAction::Tour,
        KeyAction::CheckForUpdate,
        KeyAction::Reload,
        KeyAction::Restart,
        KeyAction::RestartServer,
        KeyAction::QuitAndCd,
        KeyAction::Quit,
    ];

    /// The heading Settings lists it under.
    pub fn section(self) -> &'static str {
        match self {
            KeyAction::RakumuxMode
            | KeyAction::BackToSession
            | KeyAction::GoIn
            | KeyAction::LastSession
            | KeyAction::NumberedSession
            | KeyAction::SendRakumuxKey => "Modes",
            KeyAction::NextSession
            | KeyAction::PreviousSession
            | KeyAction::Feed
            | KeyAction::NextTab
            | KeyAction::PreviousTab
            | KeyAction::Top
            | KeyAction::Bottom
            | KeyAction::HalfPageDown
            | KeyAction::HalfPageUp
            | KeyAction::NextFolder
            | KeyAction::PreviousFolder
            | KeyAction::ScrollBack
            | KeyAction::ScrollForward => "Moving around",
            KeyAction::NewSession
            | KeyAction::FilterFolders
            | KeyAction::Rename
            | KeyAction::Pin
            | KeyAction::Summarize
            | KeyAction::Places
            | KeyAction::AllPlaces
            | KeyAction::CopyPlace
            | KeyAction::MoveDown
            | KeyAction::MoveUp
            | KeyAction::Close
            | KeyAction::Delete
            | KeyAction::Zoom
            | KeyAction::Filter
            | KeyAction::CycleLayout
            | KeyAction::HideList
            | KeyAction::TerminalPanel => "Sessions and folders",
            KeyAction::Help
            | KeyAction::Settings
            | KeyAction::Account
            | KeyAction::Tour
            | KeyAction::CheckForUpdate
            | KeyAction::Reload
            | KeyAction::Restart
            | KeyAction::RestartServer
            | KeyAction::QuitAndCd
            | KeyAction::Quit => "Help and app",
        }
    }

    /// What Settings calls it.
    pub fn label(self) -> &'static str {
        match self {
            KeyAction::RakumuxMode => "Rakumux mode (from a session)",
            KeyAction::BackToSession => "Back to the session you came from",
            KeyAction::GoIn => "Go into the selected session",
            KeyAction::LastSession => "Last session",
            KeyAction::NumberedSession => "Go into session 1 to 9",
            KeyAction::NextSession => "Next session",
            KeyAction::PreviousSession => "Previous session",
            KeyAction::Feed => "Feed (show, focus or hide)",
            KeyAction::NextTab => "Next tab (Status, Folders, App)",
            KeyAction::PreviousTab => "Previous tab",
            KeyAction::Top => "Top of the list",
            KeyAction::Bottom => "Bottom of the list",
            KeyAction::HalfPageDown => "Half a page down",
            KeyAction::HalfPageUp => "Half a page up",
            KeyAction::NextFolder => "Next folder",
            KeyAction::PreviousFolder => "Previous folder",
            KeyAction::ScrollBack => "Scroll back (Grid: previous page)",
            KeyAction::ScrollForward => "Scroll forward (Grid: next page)",
            KeyAction::NewSession => "New session",
            KeyAction::FilterFolders => "Filter by folder",
            KeyAction::Rename => "Rename session",
            KeyAction::Pin => "Pin or unpin session",
            KeyAction::Summarize => "Summarise the session (AI)",
            KeyAction::Places => "Where the session has worked",
            KeyAction::AllPlaces => "Where every session has worked",
            KeyAction::CopyPlace => "Copy a place's path",
            KeyAction::MoveDown => "Move session down",
            KeyAction::MoveUp => "Move session up",
            KeyAction::Close => "Close session (end its program)",
            KeyAction::Delete => "Delete session",
            KeyAction::Zoom => "Work in the session full screen",
            KeyAction::Filter => "Filter sessions",
            KeyAction::CycleLayout => "Cycle layout (Master detail, Grid, Tabs)",
            KeyAction::HideList => "Hide or show the session list",
            KeyAction::TerminalPanel => "Terminal panel (show, focus or hide)",
            KeyAction::Help => "Help",
            KeyAction::Settings => "Settings",
            KeyAction::Account => "Sign in, or your account",
            KeyAction::Tour => "Welcome tour",
            KeyAction::CheckForUpdate => "Update Rakumux (sessions keep running)",
            KeyAction::Reload => "Reconnect or reload the list",
            KeyAction::Restart => "Restart Rakumux (sessions keep running)",
            KeyAction::RestartServer => "Restart the server (sessions keep running)",
            KeyAction::QuitAndCd => "Quit and cd to the session's folder",
            KeyAction::Quit => "Quit Rakumux (sessions keep running)",
            KeyAction::SendRakumuxKey => "Send the Rakumux key to the session",
        }
    }

    /// Its keys: the Rakumux key for leaving a session, Rakumux mode's
    /// keys for the rest, from the configured keys.
    fn keys(self, k: &KeyNames) -> String {
        let plain = |keys: &[&str]| keys.join(", ");
        let with_cycle = |cycle: &str, keys: &[&str]| {
            let mut all: Vec<&str> = keys.to_vec();
            if !cycle.is_empty() {
                all.push(cycle);
            }
            all.join(", ")
        };
        match self {
            KeyAction::RakumuxMode => k.key.clone(),
            KeyAction::BackToSession => format!("Esc, {}", k.key),
            KeyAction::GoIn => plain(&["Enter", "i", "l"]),
            KeyAction::LastSession => plain(&["Tab"]),
            KeyAction::NumberedSession => plain(&["1-9"]),
            KeyAction::NextSession => with_cycle(&k.cycle_next, &["↓", "j"]),
            KeyAction::PreviousSession => with_cycle(&k.cycle_previous, &["↑", "k"]),
            KeyAction::Feed => plain(&["f"]),
            KeyAction::NextTab => plain(&["→"]),
            KeyAction::PreviousTab => plain(&["←"]),
            KeyAction::Top => plain(&["gg", "Home"]),
            KeyAction::Bottom => plain(&["G", "End"]),
            KeyAction::HalfPageDown => plain(&["Ctrl-d"]),
            KeyAction::HalfPageUp => plain(&["Ctrl-u"]),
            KeyAction::NextFolder => plain(&["}"]),
            KeyAction::PreviousFolder => plain(&["{"]),
            KeyAction::ScrollBack => plain(&["Ctrl-y", "PgUp"]),
            KeyAction::ScrollForward => plain(&["Ctrl-e", "PgDn"]),
            KeyAction::NewSession => plain(&["n"]),
            KeyAction::FilterFolders => plain(&["P"]),
            KeyAction::Rename => plain(&["r"]),
            KeyAction::Pin => plain(&["p"]),
            KeyAction::Summarize => plain(&["s"]),
            KeyAction::Places => plain(&["w"]),
            KeyAction::AllPlaces => plain(&["W"]),
            KeyAction::CopyPlace => plain(&["y"]),
            KeyAction::MoveDown => plain(&["J"]),
            KeyAction::MoveUp => plain(&["K"]),
            KeyAction::Close => plain(&["x"]),
            KeyAction::Delete => plain(&["d", "dd"]),
            KeyAction::Zoom => plain(&["z"]),
            KeyAction::Filter => plain(&["/"]),
            KeyAction::CycleLayout => plain(&["v"]),
            KeyAction::HideList => plain(&["b"]),
            KeyAction::TerminalPanel => plain(&["T"]),
            KeyAction::Help => plain(&["?"]),
            KeyAction::Settings => format!("comma (,), {SETTINGS_KEY}"),
            KeyAction::Account => plain(&["a"]),
            KeyAction::Tour => "t (in Help)".into(),
            KeyAction::CheckForUpdate => plain(&["U"]),
            KeyAction::Reload => plain(&["Ctrl-r"]),
            KeyAction::Restart => plain(&["R"]),
            KeyAction::RestartServer => plain(&["S"]),
            KeyAction::QuitAndCd => plain(&["c"]),
            KeyAction::Quit => plain(&["q"]),
            KeyAction::SendRakumuxKey => plain(&["\\"]),
        }
    }

    /// Shown under the Settings page while this row is selected, when
    /// there's more to say.
    fn note(self, k: &KeyNames) -> String {
        let p = &k.p;
        match self {
            KeyAction::RakumuxMode => format!(
                "`{p}` is the one key a session's program never gets. In Rakumux mode your \
                 keys drive Rakumux and the session is only shown; every other row here is \
                 a Rakumux mode key."
            ),
            KeyAction::BackToSession => format!(
                "Esc first cancels what's pending (the second key of `gg`, then the \
                 filter); with nothing to cancel it goes back. `{p}` goes back at once."
            ),
            KeyAction::GoIn => "A dead session is revived, an exited one restarted. On the \
                 tabs at the top it opens the active tab's modal (Statuses, Folders or \
                 Apps); on a folder's header or + New session, creates a generated-name \
                 session and enters its shell immediately."
                .into(),
            KeyAction::LastSession => {
                "Into the session you worked in before the current one.".into()
            }
            KeyAction::NextSession | KeyAction::PreviousSession => match &k.cycle {
                Some(cycle) if k.cycle_in_session => format!(
                    "`{cycle}` also works inside a session (see its setting above), so programs \
                     there never get it. If it does nothing, your terminal isn't sending \
                     Option/Alt: in Terminal.app turn on \"Use Option as Meta key\"."
                ),
                Some(cycle) => format!(
                    "`{cycle}` works in Rakumux mode; turn on \"Option+↑/↓ in sessions\" \
                     (above) to use it inside a session too. If it does nothing, your \
                     terminal isn't sending Option/Alt: in Terminal.app turn on \"Use Option as \
                     Meta key\"."
                ),
                None => "Option/Alt+↑/↓ are turned off in your settings; ↑/↓ and j/k move.".into(),
            },
            KeyAction::Top => {
                "The tabs (Status, Folders, App) are the first row; ←/→ switch them.".into()
            }
            KeyAction::Feed => "What your agents said when they finished a turn, on every \
                 machine of your account, newest first: a pane on the right of Master detail \
                 and Grid, a tab of its own in Tabs. Enter reads a reply whole, again goes into \
                 its session when it runs here; r writes back to it; Esc gives the list the \
                 keys back. The footer's Feed button counts the replies that came since you \
                 last looked. Needs your Rakumux account with the Agent feed on (Settings → \
                 Available on sign-in)."
                .into(),
            KeyAction::NextFolder | KeyAction::PreviousFolder => {
                "With several folders shown: the first session under the next or previous \
                 folder's header."
                    .into()
            }
            KeyAction::FilterFolders => {
                "Pick one folder, tick several, or All folders; or type a path to add a folder."
                    .into()
            }
            KeyAction::ScrollBack | KeyAction::ScrollForward => {
                "Scrolls the history of the session shown on the right.".into()
            }
            KeyAction::MoveDown | KeyAction::MoveUp => {
                "Within its folder, among the pinned sessions or the others. Dragging it with \
                 the mouse does the same. The order is kept until you change it."
                    .into()
            }
            KeyAction::Delete => {
                "Press d or dd; both ask before deleting. Confirm with y or Enter.".into()
            }
            KeyAction::Places | KeyAction::AllPlaces => {
                "Each session shows the folder it works in now. w opens the list under it (W \
                 every session's, a click on the line too): every other git working tree it \
                 has worked in, most recent first, with its branch, ✎ where files were \
                 changed, and when. Read on this machine from what the agent did; it stays \
                 here."
                    .into()
            }
            KeyAction::CopyPlace => {
                "In an open list, ↓ and ↑ step through the places too, and y copies the path of \
                 the one marked ⧉ (a click on a place does the same); elsewhere, y copies the \
                 folder the session works in now. Copied through your terminal (OSC 52)."
                    .into()
            }
            KeyAction::Summarize => {
                "Each agent chat gets one AI summary by itself, at its fifth output or when it \
                 finishes. s (or the ↻ over a description) makes a new one: the agent's latest \
                 request and what it has done since, or the screen for other programs. Needs \
                 your Rakumux account with AI session summaries on."
                    .into()
            }
            KeyAction::Restart => {
                "Asks before reloading this UI through its launcher to pick up the latest build. \
                 Confirm with y or Enter; Esc cancels. The daemon and sessions stay up. \
                 Ctrl-r only reconnects or reloads the list."
                    .into()
            }
            KeyAction::RestartServer => {
                "Asks first, then restarts the Rakumux server in place with this build (after an \
                 update, say): sessions and their programs keep running and every window \
                 reconnects. Same as `rakumux daemon restart`. R reloads only this UI."
                    .into()
            }
            KeyAction::HideList => "Master detail, with the list beside the session: the \
                 session takes the list's columns, and keys still move through the sessions. \
                 The « Hide list button at the bottom of the list does the same; the » left \
                 of the session (or b again) brings the list back."
                .into(),
            KeyAction::TerminalPanel => {
                "T opens a standalone shell below the main detail in the current directory. \
                 Click a pane to focus it. Use the Rakumux key to return to Rakumux mode; \
                 then Esc returns to the shell or T hides it. Hiding does not end its \
                 running command. Exit closes the pane and releases its terminal. No session \
                 is created. Closing or reloading this UI ends its temporary shell, not any \
                 sessions; drag the divider to resize the split."
                    .into()
            }
            KeyAction::Account => "Signed out, it asks for a sign-in code to approve in any \
                 browser (the footer's Sign in does too); signed in, it shows the account and \
                 whether AI session summaries are on."
                .into(),
            KeyAction::CheckForUpdate => {
                "Shift+U checks the installed release channel. Enter confirms installation and \
                 restarts only this UI; Esc cancels before installation starts. Binary installs \
                 verify the release SHA-256 checksum. Source builds and admin-owned install \
                 directories may need a manual update. No daemon stop or session revival."
                    .into()
            }
            KeyAction::Zoom => {
                format!("The list or tab bar hides until you press `{p}`.")
            }
            KeyAction::SendRakumuxKey => {
                format!("Goes back into the session and sends it `{p}`, for programs that use it.")
            }
            KeyAction::QuitAndCd => {
                "Quits, then prints `cd \"<folder>\"` for the selected session's folder, for a \
                 shell function to run. Rakumux doesn't ship that function yet."
                    .into()
            }
            _ => String::new(),
        }
    }
}

/// Open-modal state: the tab shown (which says the modal), how far a Help
/// tab is scrolled, and the row selected on the Settings page.
#[derive(Debug, Clone)]
pub struct ModalState {
    pub tab: ModalTab,
    /// Lines scrolled from the top of a Help tab's body.
    pub scroll: u16,
    /// Row selected on the Settings page: a setting (`0..general_rows`),
    /// then an action (into [`KeyAction::ALL`]).
    pub selected: usize,
    /// Settings listed before the actions.
    general_rows: usize,
    /// Largest useful scroll for the current tab, recorded by `render`.
    max_scroll: Cell<u16>,
    /// Body height in lines, recorded by `render` (PgUp/PgDn step).
    page: Cell<u16>,
    /// The Settings page's first line in view, kept by `render` so the
    /// selected row stays visible without the page jumping.
    top: Cell<u16>,
    /// The selected Settings row's note shows in a popover (`→`).
    pub note_open: bool,
    /// Typed in Settings' search field: only the rows it matches show.
    pub query: String,
    /// The settings' [`search_text`], in order (the actions' is known here).
    settings: Vec<String>,
}

/// What the caller should do after [`ModalState::handle_key`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModalAction {
    /// The key was used; redraw.
    Handled,
    /// Not a Settings key (including anything with Ctrl or Alt held, such
    /// as the session cycle keys); the caller may use it or drop it.
    Ignored,
    /// Close the modal.
    Close,
    /// `Enter` or `Space` on a setting: change the setting in this row.
    Change(usize),
    /// `Enter` or `Space` on an action: close the modal and run this.
    Run(KeyAction),
    /// `t` in Help: show the welcome tour.
    Tour,
}

impl ModalState {
    /// A modal opened on `tab`, with these settings (their [`search_text`])
    /// before the actions on the Settings page (the first one selected).
    pub fn new(tab: ModalTab, settings: Vec<String>) -> Self {
        Self {
            tab,
            scroll: 0,
            selected: 0,
            general_rows: settings.len(),
            max_scroll: Cell::new(u16::MAX),
            page: Cell::new(DEFAULT_PAGE),
            top: Cell::new(0),
            note_open: false,
            query: String::new(),
            settings,
        }
    }

    /// The Settings rows the search shows, in order: every row while
    /// nothing is typed.
    pub fn shown_rows(&self) -> Vec<usize> {
        (0..self.rows())
            .filter(|row| {
                let text = match self.settings.get(*row) {
                    Some(text) => text.clone(),
                    None => KeyAction::ALL
                        .get(row - self.general_rows)
                        .map(|action| search_text(action.section(), action.label()))
                        .unwrap_or_default(),
                };
                matches_query(&text, &self.query)
            })
            .collect()
    }

    /// The search changed: the first row it shows is selected.
    fn searched(&mut self) {
        self.note_open = false;
        if let Some(first) = self.shown_rows().first() {
            self.selected = *first;
        }
        self.top.set(0);
    }

    /// Select Settings row `row` (clamped to the rows there are).
    #[cfg(test)]
    pub fn with_selected(mut self, row: usize) -> Self {
        self.selected = row.min(self.rows() - 1);
        self
    }

    /// Rows on the Settings page: the settings, then the actions.
    fn rows(&self) -> usize {
        self.general_rows + KeyAction::ALL.len()
    }

    /// The selected Settings row, when it's an action.
    pub fn selected_action(&self) -> Option<KeyAction> {
        let index = self.selected.checked_sub(self.general_rows)?;
        KeyAction::ALL.get(index).copied()
    }

    /// Select `action`'s row on the Settings page.
    #[cfg(test)]
    pub fn select_action(&mut self, action: KeyAction) {
        if let Some(index) = KeyAction::ALL.iter().position(|a| *a == action) {
            self.selected = self.general_rows + index;
        }
    }

    /// Show `tab` from its top.
    pub fn select_tab(&mut self, tab: ModalTab) {
        if tab != self.tab {
            self.tab = tab;
            self.scroll = 0;
            // Unknown until the new tab is drawn.
            self.max_scroll.set(u16::MAX);
        }
    }

    /// Scroll a Help tab's body by `delta` lines (positive = down).
    pub fn scroll_by(&mut self, delta: i32) {
        let max = i32::from(self.max_scroll.get());
        let current = i32::from(self.scroll).min(max);
        self.scroll = (current + delta).clamp(0, max) as u16;
    }

    /// The mouse wheel turned `notches` (positive = down): Settings moves
    /// its selection a row per notch, Help scrolls.
    pub fn wheel(&mut self, notches: i32) {
        if self.tab == ModalTab::Settings {
            self.move_selection(notches);
        } else {
            self.scroll_by(notches.saturating_mul(WHEEL_LINES));
        }
    }

    /// Move the Settings selection `delta` of the rows shown, stopping at
    /// either end. A note in a popover closes.
    fn move_selection(&mut self, delta: i32) {
        self.note_open = false;
        let shown = self.shown_rows();
        let Some(last) = shown.len().checked_sub(1) else {
            return;
        };
        let at = shown
            .iter()
            .position(|row| *row >= self.selected)
            .unwrap_or(last);
        let to = (at as i64 + i64::from(delta)).clamp(0, last as i64) as usize;
        self.selected = shown[to];
    }

    /// In Settings typing searches (`Backspace` deletes, `Esc` clears),
    /// `↑`/`↓` (`PgUp`/`PgDn`, `Home`/`End`) select one of the rows shown,
    /// `Enter` (and `Space` while nothing is typed) changes the setting or
    /// runs the action, `→` shows the row's note in a popover that `←` or
    /// `Esc` closes, as does moving or `Enter`, and `Esc` closes. In Help
    /// `Esc`/`q` and `?` close, and
    /// `Tab`/`→` and `Shift-Tab`/`←` switch tabs, number keys jump to a tab,
    /// and `↑`/`↓`, `PgUp`/`PgDn`, `Home`/`End` scroll (`j`/`k`/`g`/`G`
    /// too). Keys with Ctrl or Alt held are ignored.
    pub fn handle_key(&mut self, key: &KeyEvent) -> ModalAction {
        if key.kind == KeyEventKind::Release {
            return ModalAction::Ignored;
        }
        if !(key.modifiers - KeyModifiers::SHIFT).is_empty() {
            return ModalAction::Ignored;
        }
        let page = i32::from(self.page.get().max(1));
        if self.tab == ModalTab::Settings {
            match key.code {
                KeyCode::Up => self.move_selection(-1),
                KeyCode::Down => self.move_selection(1),
                KeyCode::PageUp => self.move_selection(-page),
                KeyCode::PageDown => self.move_selection(page),
                KeyCode::Home => self.move_selection(i32::MIN),
                KeyCode::End => self.move_selection(i32::MAX),
                KeyCode::Right => self.note_open = true,
                KeyCode::Left => self.note_open = false,
                KeyCode::Esc if self.note_open => self.note_open = false,
                KeyCode::Esc if !self.query.is_empty() => {
                    self.query.clear();
                    self.searched();
                }
                KeyCode::Esc => return ModalAction::Close,
                KeyCode::Backspace => {
                    if self.query.pop().is_some() {
                        self.searched();
                    }
                }
                KeyCode::Enter => return self.choose(),
                KeyCode::Char(' ') if self.query.is_empty() => return self.choose(),
                KeyCode::Char(c) => {
                    self.query.push(c);
                    self.searched();
                }
                _ => return self.handle_common_key(key),
            }
            return ModalAction::Handled;
        }
        match key.code {
            KeyCode::Down | KeyCode::Char('j') => self.scroll_by(1),
            KeyCode::Up | KeyCode::Char('k') => self.scroll_by(-1),
            KeyCode::PageDown | KeyCode::Char(' ') => self.scroll_by(page),
            KeyCode::PageUp => self.scroll_by(-page),
            KeyCode::Home | KeyCode::Char('g') => self.scroll = 0,
            KeyCode::End | KeyCode::Char('G') => self.scroll = self.max_scroll.get(),
            _ => return self.handle_common_key(key),
        }
        ModalAction::Handled
    }

    /// `Enter` in Settings: change the selected setting or run the action,
    /// when the search shows it.
    fn choose(&mut self) -> ModalAction {
        self.note_open = false;
        if !self.shown_rows().contains(&self.selected) {
            return ModalAction::Handled;
        }
        match self.selected_action() {
            Some(action) => ModalAction::Run(action),
            None => ModalAction::Change(self.selected),
        }
    }

    /// Keys that work on every tab: closing, and in Help switching tabs.
    fn handle_common_key(&mut self, key: &KeyEvent) -> ModalAction {
        let own_keys: &[char] = match self.tab.modal() {
            Modal::Help => &['?'],
            Modal::Settings => &[',', 'o'],
        };
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => return ModalAction::Close,
            KeyCode::Char(c) if own_keys.contains(&c) => return ModalAction::Close,
            KeyCode::Char('t') if self.tab.modal() == Modal::Help => return ModalAction::Tour,
            // One page: nothing to switch to.
            _ if self.tab.modal() == Modal::Settings => return ModalAction::Ignored,
            KeyCode::Tab | KeyCode::Right | KeyCode::Char('l') => self.select_tab(self.tab.next()),
            KeyCode::BackTab | KeyCode::Left | KeyCode::Char('h') => {
                self.select_tab(self.tab.prev())
            }
            KeyCode::Char(digit @ '1'..='9') => {
                let index = digit as usize - '1' as usize;
                match self.tab.sibling(index) {
                    Some(tab) => self.select_tab(tab),
                    None => return ModalAction::Ignored,
                }
            }
            _ => return ModalAction::Ignored,
        }
        ModalAction::Handled
    }
}

/// What the modal needs to know about the user's setup, so shortcuts show
/// the configured keys and Settings the current settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModalContext {
    /// The Rakumux key as the user should read it, e.g. `Ctrl-\\`.
    pub rakumux_key: String,
    /// Session cycle keys as the user should read them, e.g. `Alt-↑/↓`.
    /// Empty when cycling is turned off.
    pub cycle_keys: String,
    /// The previous / next session keys alone, e.g. `Alt-↑`; empty when
    /// cycling is turned off.
    pub cycle_previous: String,
    pub cycle_next: String,
    /// The cycle keys also work inside sessions.
    pub cycle_in_session: bool,
    /// The marks for folders and branches.
    pub icons: crate::icons::Icons,
    /// Layout currently in use.
    pub layout: SessionLayout,
    /// Rakumux version, shown in the modal's title.
    pub version: String,
    /// Opened while a session had the keys rather than Rakumux mode.
    pub in_session: bool,
    /// The settings, in order.
    pub general: Vec<SettingRow>,
}

/// One setting on the Settings page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingRow {
    /// The heading it's listed under, e.g. `Appearance`.
    pub section: &'static str,
    /// What it sets, e.g. `Layout`.
    pub label: String,
    /// Its current value, e.g. `[Master detail] Grid Tabs` or `not set`.
    pub value: String,
    /// Shown below the rows while this one is selected.
    pub note: String,
    /// Greyed out: it can't be changed now (signed out).
    pub disabled: bool,
}

/// What Settings' search looks through for a row: its section and label.
pub fn search_text(section: &str, label: &str) -> String {
    format!("{section} {label}").to_lowercase()
}

/// Whether every word typed in `query` is in `text` (a [`search_text`]),
/// ignoring case; an empty query matches everything.
fn matches_query(text: &str, query: &str) -> bool {
    let query = query.to_lowercase();
    query.split_whitespace().all(|word| text.contains(word))
}

/// Draw Settings at its contents' size, or Help at its reading width. Both
/// stay screen-bounded and scroll what doesn't fit.
pub fn render(f: &mut Frame, area: Rect, state: &ModalState, ctx: &ModalContext) {
    match state.tab.modal() {
        Modal::Settings => render_settings(f, area, state, ctx),
        Modal::Help => render_help(f, area, state, ctx),
    }
}

/// Clear `popup` and draw the modal's frame on it: its title, and the
/// version on the right. Returns the area inside, unless too small to use.
fn draw_frame(f: &mut Frame, popup: Rect, modal: Modal, ctx: &ModalContext) -> Option<Rect> {
    if popup.width < 3 || popup.height < 3 {
        return None;
    }
    let palette = theme::palette();
    f.render_widget(Clear, popup);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(palette.accent_primary))
        .style(
            Style::default()
                .bg(palette.background)
                .fg(palette.text_primary),
        )
        .title(Span::styled(
            format!(" {} ", modal.title()),
            Style::default()
                .fg(palette.accent_primary)
                .add_modifier(Modifier::BOLD),
        ))
        .title(
            Title::from(Span::styled(
                format!(" Rakumux {} ", ctx.version),
                Style::default().fg(palette.neutral_gray),
            ))
            .alignment(Alignment::Right),
        );
    let inner = block.inner(popup);
    f.render_widget(block, popup);
    (inner.width > 0 && inner.height > 0).then_some(inner)
}

/// `lines` from `scroll` in `body`, after a column of padding, with a
/// scrollbar on the right when they don't all fit.
fn draw_body(f: &mut Frame, body: Rect, lines: Vec<Line<'static>>, scroll: u16, max_scroll: u16) {
    let total = u16::try_from(lines.len()).unwrap_or(u16::MAX);
    let visible: Vec<Line> = lines
        .into_iter()
        .skip(usize::from(scroll))
        .take(usize::from(body.height))
        .collect();
    let text_area = Rect {
        x: body.x + 1,
        width: body.width.saturating_sub(1),
        ..body
    };
    f.render_widget(Paragraph::new(visible), text_area);
    if max_scroll > 0 && body.width >= 3 && body.height > 0 {
        draw_scrollbar(f, body, scroll, max_scroll, total);
    }
}

/// Help: a tab strip, a rule, the tab's text and the keys.
fn render_help(f: &mut Frame, area: Rect, state: &ModalState, ctx: &ModalContext) {
    let Some(inner) = draw_frame(f, modal_area(area), Modal::Help, ctx) else {
        return;
    };
    let palette = theme::palette();
    let tabs_row = Rect { height: 1, ..inner };
    let items = tab_items(state.tab, tabs_row.width);
    layout::render_tabs(tabs_row, f.buffer_mut(), &items, state.tab.index());
    if inner.height < 2 {
        return;
    }

    let rule = Rect {
        y: inner.y + 1,
        height: 1,
        ..inner
    };
    f.render_widget(
        Paragraph::new("─".repeat(usize::from(rule.width)))
            .style(Style::default().fg(palette.text_subtle)),
        rule,
    );
    let footer_height = u16::from(inner.height >= 4);
    let body = Rect {
        y: inner.y + 2,
        height: inner.height - 2 - footer_height,
        ..inner
    };

    let text_width = usize::from(body.width.saturating_sub(BODY_MARGIN)).max(1);
    let lines = body_lines(state.tab, ctx, text_width);
    let total = u16::try_from(lines.len()).unwrap_or(u16::MAX);
    let max_scroll = total.saturating_sub(body.height);
    state.max_scroll.set(max_scroll);
    state.page.set(body.height.saturating_sub(1).max(1));
    let scroll = state.scroll.min(max_scroll);
    draw_body(f, body, lines, scroll, max_scroll);

    if footer_height == 1 {
        let footer = Rect {
            y: inner.bottom() - 1,
            height: 1,
            ..inner
        };
        let jump = format!("1-{}", Modal::Help.tabs().len());
        let position = if total > body.height {
            let last = (scroll + body.height).min(total);
            format!("{}-{}/{} ", scroll + 1, last, total)
        } else {
            String::new()
        };
        draw_footer(
            f,
            footer,
            [
                &[
                    ("Tab ←→", "switch"),
                    ("↑↓ PgUp PgDn", "scroll"),
                    (&jump, "jump"),
                    ("t", "tour"),
                    ("Esc", "close"),
                ],
                &[("←→", "tabs"), ("↑↓", "scroll"), ("Esc", "close")],
                &[("←→", "tabs"), ("Esc", "close")],
            ],
            &position,
        );
    }
}

/// Settings: the search field, then one page as tall as its rows (up to 80%
/// of the screen), then the keys. The search narrows the page but not the
/// dialog, so typing doesn't make it jump.
fn render_settings(f: &mut Frame, area: Rect, state: &ModalState, ctx: &ModalContext) {
    let width = settings_width(area, ctx);
    // Space inside the border, where the screen has room for it.
    let (pad_x, pad_y) = crate::components::dialogs::padding(area);
    let text_width = usize::from(width.saturating_sub(2 + BODY_MARGIN + 2 * pad_x)).max(1);
    let page = settings_lines(ctx, state.selected, &state.query, text_width);
    let total = u16::try_from(page.lines.len()).unwrap_or(u16::MAX);
    let whole = if state.query.is_empty() {
        total
    } else {
        let lines = settings_lines(ctx, state.selected, "", text_width).lines;
        u16::try_from(lines.len()).unwrap_or(u16::MAX)
    };
    // The borders, the padding, the search field and its gap and the
    // footer, within most of the screen: a long page scrolls rather than
    // filling it.
    let room = (u32::from(area.height) * SETTINGS_MAX_HEIGHT / 100) as u16;
    let height = whole
        .saturating_add(5 + 2 * pad_y)
        .min(room.max(area.height.min(8)));
    let popup = Rect {
        x: area.x + (area.width - width) / 2,
        y: area.y + (area.height - height) / 2,
        width,
        height,
    };
    let Some(inner) = draw_frame(f, popup, Modal::Settings, ctx) else {
        return;
    };
    let inner = crate::components::dialogs::padded(inner, pad_x, pad_y);
    let footer_height = u16::from(inner.height >= 4);
    // The search field, and a blank line under it when there's room.
    let search_height = 1 + u16::from(inner.height >= 6);
    draw_search(f, Rect { height: 1, ..inner }, &state.query);
    let inner = Rect {
        y: inner.y + search_height.min(inner.height),
        height: inner.height.saturating_sub(search_height),
        ..inner
    };
    let footer_height = footer_height.min(inner.height);
    let body = Rect {
        height: inner.height - footer_height,
        ..inner
    };

    // Scroll only as far as it takes to show the selected row (from its
    // heading when it starts a section).
    let max_scroll = total.saturating_sub(body.height);
    let to_line = |line: usize| u16::try_from(line).unwrap_or(u16::MAX);
    let (start, end) = page.selected;
    let mut top = state.top.get().min(max_scroll);
    if to_line(end) >= top.saturating_add(body.height) {
        top = to_line(end + 1).saturating_sub(body.height);
    }
    top = top.min(to_line(start)).min(max_scroll);
    state.top.set(top);
    state.max_scroll.set(max_scroll);
    state.page.set(body.height.saturating_sub(1).max(1));
    draw_body(f, body, page.lines, top, max_scroll);

    let names = KeyNames::new(ctx);
    let note = if page.has_selected {
        row_note(ctx, &names, state.selected)
    } else {
        String::new()
    };
    let showing = state.note_open && !note.is_empty();
    if showing {
        let row = body.y + to_line(end).saturating_sub(top);
        let label = row_label(ctx, state.selected);
        draw_note(f, body, row, &label, &note);
    }

    if footer_height == 1 {
        let footer = Rect {
            y: inner.bottom() - 1,
            height: 1,
            ..inner
        };
        let enter = if state.selected < ctx.general.len() {
            "change"
        } else {
            "run"
        };
        let about = if showing {
            ("←", "back")
        } else if note.is_empty() {
            ("", "")
        } else {
            ("→", "about")
        };
        let hints = |pairs: &[(&'static str, &'static str)]| -> Vec<(&'static str, &'static str)> {
            pairs
                .iter()
                .copied()
                .filter(|(keys, _)| !keys.is_empty())
                .collect()
        };
        let esc = if state.query.is_empty() {
            ("Esc", "close")
        } else {
            ("Esc", "clear")
        };
        draw_footer(
            f,
            footer,
            [
                &hints(&[("↑↓", "select"), about, ("Enter", enter), esc]),
                &hints(&[("↑↓", "select"), about, ("Enter", enter), esc]),
                &hints(&[about, ("Enter", enter), esc]),
            ],
            "",
        );
    }
}

/// Settings' search field on `row`: what's typed and a cursor, or a hint.
fn draw_search(f: &mut Frame, row: Rect, query: &str) {
    let palette = theme::palette();
    let mark = Span::styled(" ⌕ ", Style::default().fg(palette.accent_primary));
    let text = if query.is_empty() {
        vec![
            mark,
            Span::styled("█", Style::default().fg(palette.text_muted)),
            Span::styled(
                " Type to search settings and keys",
                Style::default().fg(palette.text_muted),
            ),
        ]
    } else {
        // The end of a long query, so the cursor stays in view.
        let room = usize::from(row.width).saturating_sub(5);
        let shown: String = {
            let mut used = 0;
            let mut kept: Vec<char> = Vec::new();
            for c in query.chars().rev() {
                used += char_width(c);
                if used > room {
                    break;
                }
                kept.push(c);
            }
            kept.into_iter().rev().collect()
        };
        vec![
            mark,
            Span::styled(
                shown,
                Style::default()
                    .fg(palette.text_bright)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled("█", Style::default().fg(palette.accent_primary)),
        ]
    };
    f.render_widget(Paragraph::new(fit(text, usize::from(row.width))), row);
}

/// The selected row's note in a popover over the Settings page, just below
/// the row on screen at `row` (above it when there's no room below), inside
/// `body`: the page doesn't move.
fn draw_note(f: &mut Frame, body: Rect, row: u16, label: &str, note: &str) {
    let palette = theme::palette();
    let width = body.width.saturating_sub(4).min(64);
    if width < 8 || body.height < 3 {
        return;
    }
    // Borders and a column of padding on each side.
    let text_width = usize::from(width - 4);
    let mut lines = note_lines(note, text_width);
    let most = usize::from(body.height - 2);
    if lines.len() > most {
        lines.truncate(most);
        if let Some(last) = lines.pop() {
            let mut last = fit(last.spans, text_width.saturating_sub(1));
            last.spans
                .push(Span::styled("…", Styles::from_theme().note));
            lines.push(last);
        }
    }
    let height = lines.len() as u16 + 2;
    let y = if row + 1 + height <= body.bottom() {
        row + 1
    } else if row >= body.y + height {
        row - height
    } else {
        body.bottom() - height
    };
    let popover = Rect {
        x: body.x + 3,
        y,
        width,
        height,
    };
    f.render_widget(Clear, popover);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(palette.accent_primary))
        .style(Style::default().bg(palette.surface))
        .title(Span::styled(
            format!(
                " {} ",
                truncate(label, usize::from(width).saturating_sub(4))
            ),
            Style::default()
                .fg(palette.text_bright)
                .add_modifier(Modifier::BOLD),
        ));
    let inner = block.inner(popover);
    f.render_widget(block, popover);
    let text = Rect {
        x: inner.x + 1,
        width: inner.width.saturating_sub(2),
        ..inner
    };
    f.render_widget(Paragraph::new(lines), text);
}

/// Help's tab strip items: the full labels when they all fit in `width`,
/// otherwise the short ones.
fn tab_items(selected: ModalTab, width: u16) -> Vec<TabItem> {
    let tabs = selected.modal().tabs();
    // A tab is ` N label `; tabs are one column apart.
    let full = tabs
        .iter()
        .map(|tab| str_width(tab.label()) + 4)
        .sum::<usize>()
        + tabs.len()
        - 1;
    let short = full > usize::from(width);
    tabs.iter()
        .map(|tab| TabItem {
            name: if short {
                tab.short_label()
            } else {
                tab.label()
            }
            .to_string(),
            selected: *tab == selected,
            ..TabItem::default()
        })
        .collect()
}

/// Where Help goes within `area`.
fn modal_area(area: Rect) -> Rect {
    let scaled = |size: u16, min: u16| -> u16 {
        let eighty = (u32::from(size) * 4 / 5) as u16;
        eighty.max(min.min(size))
    };
    let width = scaled(area.width, MIN_MODAL_WIDTH);
    let height = scaled(area.height, MIN_MODAL_HEIGHT);
    Rect {
        x: area.x + (area.width - width) / 2,
        y: area.y + (area.height - height) / 2,
        width,
        height,
    }
}

/// How wide Settings is in `area`: its widest row, setting or action, and
/// some space, not the selected note (notes wrap, so moving the selection
/// never makes the dialog grow and shrink); a column of the screen kept
/// free on each side.
fn settings_width(area: Rect, ctx: &ModalContext) -> u16 {
    let values = ctx
        .general
        .iter()
        .map(|row| str_width(&row.value))
        .max()
        .unwrap_or(0);
    let settings = 2 + setting_label_width(ctx) + values;
    let names = KeyNames::new(ctx);
    let labels = KeyAction::ALL
        .iter()
        .map(|action| str_width(action.label()))
        .max()
        .unwrap_or(0);
    let keys = KeyAction::ALL
        .iter()
        .map(|action| str_width(&action.keys(&names)))
        .max()
        .unwrap_or(0);
    let actions = 2 + labels + 3 + keys;
    let title = str_width(" Settings ") + str_width(&format!(" Rakumux {} ", ctx.version)) + 2;
    let padding = 2 * usize::from(crate::components::dialogs::padding(area).0);
    let preferred =
        (settings.max(actions) + SETTINGS_SPACE + usize::from(BODY_MARGIN) + 2 + padding)
            .max(title)
            .clamp(MIN_SETTINGS_WIDTH, MAX_SETTINGS_WIDTH);
    let available = if area.width > 2 {
        area.width - 2
    } else {
        area.width
    };
    (preferred as u16).min(available)
}

fn setting_label_width(ctx: &ModalContext) -> usize {
    ctx.general
        .iter()
        .map(|row| str_width(&row.label))
        .max()
        .unwrap_or(0)
        + 2
}

/// A quiet scrollbar down `body`'s last column: a thin muted thumb on a
/// dim track.
fn draw_scrollbar(f: &mut Frame, body: Rect, scroll: u16, max_scroll: u16, total: u16) {
    let palette = theme::palette();
    let height = u32::from(body.height);
    let thumb = ((height * height) / u32::from(total.max(1))).clamp(1, height);
    let track = height - thumb;
    let offset = u32::from(scroll) * track / u32::from(max_scroll.max(1));
    let x = body.right() - 1;
    let buf = f.buffer_mut();
    for row in 0..height {
        let in_thumb = row >= offset && row < offset + thumb;
        let (symbol, color) = if in_thumb {
            ("┃", palette.text_muted)
        } else {
            ("│", palette.border_dim)
        };
        buf.get_mut(x, body.y + row as u16)
            .set_symbol(symbol)
            .set_fg(color);
    }
}

/// The modal's last row: the first of `hints` (keys and what they do) that
/// fits beside `position` (shown on the right), shortest last, for a phone.
fn draw_footer(f: &mut Frame, footer: Rect, hints: [&[(&str, &str)]; 3], position: &str) {
    let palette = theme::palette();
    let hint_style = Style::default().fg(palette.neutral_gray);
    let key_style = Style::default().fg(palette.accent_green);
    let position_width = str_width(position) as u16;
    let room = footer.width.saturating_sub(position_width + 1);
    let hint = |pairs: &[(&str, &str)]| -> Line<'static> {
        let mut spans = vec![Span::raw(" ")];
        for (i, (keys, action)) in pairs.iter().enumerate() {
            if i > 0 {
                spans.push(Span::raw("  "));
            }
            spans.push(Span::styled(keys.to_string(), key_style));
            spans.push(Span::styled(format!(" {action}"), hint_style));
        }
        Line::from(spans)
    };
    let [full, short, tiny] = hints;
    let mut line = hint(full);
    if line.width() > usize::from(room) {
        line = hint(short);
    }
    if line.width() > usize::from(room) {
        // The way out stays whole.
        line = hint(tiny);
    }
    let hint_area = Rect {
        width: room,
        ..footer
    };
    f.render_widget(Paragraph::new(line), hint_area);
    if position_width > 0 && position_width < footer.width {
        let position_area = Rect {
            x: footer.right() - position_width,
            width: position_width,
            ..footer
        };
        f.render_widget(
            Paragraph::new(position.to_string()).style(hint_style),
            position_area,
        );
    }
}

// ---------------------------------------------------------------------------
// Content
// ---------------------------------------------------------------------------

/// One block of help content, laid out to the body width at render time.
/// Text may mark key names with backticks.
enum Item {
    Heading(String),
    Text(String),
    /// Muted side note.
    Note(String),
    /// Bold paragraph (license).
    Strong(String),
    /// A marked list entry (`•`, `1.`).
    List(String, String),
    /// Two-column table: key names (plain), description (markup).
    Keys(Vec<(String, String)>),
    /// A command line (plain) and its description (markup) below it.
    Command(String, String),
}

#[derive(Default)]
struct Doc(Vec<Item>);

impl Doc {
    fn heading(&mut self, text: impl Into<String>) {
        self.0.push(Item::Heading(text.into()));
    }
    fn text(&mut self, text: impl Into<String>) {
        self.0.push(Item::Text(text.into()));
    }
    fn note(&mut self, text: impl Into<String>) {
        self.0.push(Item::Note(text.into()));
    }
    fn strong(&mut self, text: impl Into<String>) {
        self.0.push(Item::Strong(text.into()));
    }
    fn bullet(&mut self, text: impl Into<String>) {
        self.0.push(Item::List("•".into(), text.into()));
    }
    fn dash(&mut self, text: impl Into<String>) {
        self.0.push(Item::List("-".into(), text.into()));
    }
    fn step(&mut self, number: usize, text: impl Into<String>) {
        self.0.push(Item::List(format!("{number}."), text.into()));
    }
    fn keys(&mut self, rows: Vec<(String, String)>) {
        self.0.push(Item::Keys(rows));
    }
    fn command(&mut self, command: impl Into<String>, description: impl Into<String>) {
        self.0
            .push(Item::Command(command.into(), description.into()));
    }
}

/// Escape user-provided key text for use inside backtick markup.
fn esc(text: &str) -> String {
    text.replace('`', &LITERAL_BACKTICK.to_string())
}

/// Shorthand for a key-table row.
fn row(keys: impl Into<String>, description: impl Into<String>) -> (String, String) {
    (keys.into(), description.into())
}

/// Key names as they appear in the help, from the user's configuration.
struct KeyNames {
    /// The Rakumux key, escaped for markup.
    p: String,
    /// The Rakumux key, raw (key tables).
    key: String,
    /// Cycle keys, escaped for markup; `None` when cycling is off.
    cycle: Option<String>,
    /// Cycle keys, raw.
    cycle_raw: String,
    /// The previous / next session keys alone, raw; empty when off.
    cycle_previous: String,
    cycle_next: String,
    /// The cycle keys also work inside sessions.
    cycle_in_session: bool,
    /// The marks for folders and branches.
    icons: crate::icons::Icons,
}

impl KeyNames {
    fn new(ctx: &ModalContext) -> Self {
        let cycle_raw = ctx.cycle_keys.trim().to_string();
        Self {
            p: esc(ctx.rakumux_key.trim()),
            key: ctx.rakumux_key.trim().to_string(),
            cycle: (!cycle_raw.is_empty()).then(|| esc(&cycle_raw)),
            cycle_raw,
            cycle_previous: ctx.cycle_previous.trim().to_string(),
            cycle_next: ctx.cycle_next.trim().to_string(),
            cycle_in_session: ctx.cycle_in_session,
            icons: ctx.icons,
        }
    }
}

fn content(tab: ModalTab, ctx: &ModalContext) -> Doc {
    let keys = KeyNames::new(ctx);
    let mut doc = Doc::default();
    match tab {
        // Rows, not prose: see `settings_lines`.
        ModalTab::Settings => {}
        ModalTab::Overview => overview(&mut doc, ctx, &keys),
        ModalTab::Sessions => sessions(&mut doc, &keys),
        ModalTab::Layouts => layouts(&mut doc, ctx, &keys),
        ModalTab::AgentsCli => agents_and_cli(&mut doc),
        ModalTab::License => license(&mut doc),
    }
    doc
}

fn overview(doc: &mut Doc, ctx: &ModalContext, k: &KeyNames) {
    let p = &k.p;
    doc.note("New here? Press `t` for a one-minute tour of the keys.");

    doc.heading("What Rakumux is");
    doc.text(
        "Rakumux runs terminal sessions in the background and shows them together on one \
         screen. A session is a folder plus a terminal: your login shell, a command you choose, \
         or a coding agent such as Claude Code, Codex or OpenCode.",
    );
    doc.text(
        "Sessions keep running when you quit Rakumux, close the window or lose an SSH \
         connection. Start Rakumux again in the same folder and they are all still there.",
    );
    doc.text(
        "They run in a background server, one per user on this machine. Rakumux connects to \
         it from any login (SSH, mosh, your phone) and starts it only when none runs; \
         `rakumux --new-server` replaces it. Restarting the server keeps every program \
         running. After a crash or a reboot, the sessions that were running come back by \
         themselves: Claude Code, Codex and OpenCode in the same conversation, other programs \
         with their command waiting at the prompt.",
    );

    doc.heading("Two modes");
    doc.text(format!(
        "In session mode your keys go to the program in the session, all of them except \
         `{p}`, the Rakumux key. Press it to switch to Rakumux mode, where your keys \
         drive Rakumux: the list, new sessions, folders, Help and Settings. The session \
         keeps showing beside the list (below it on a narrow screen; on a phone you see the \
         list alone) while you look around, \
         with its title and output faded: it's live, but your keys \
         don't reach it. On the folder filter, a folder's header or + New session it keeps \
         the last session in view."
    ));
    doc.text(format!(
        "To get back, press `Enter` (or `i`) on a session to work in it, or `Esc` (or `{p}`) \
         to return to the session you came from. The badge at the bottom left says which mode \
         you're in."
    ));

    doc.heading("Terminal panel");
    doc.text(
        "T in Rakumux mode, or Settings, Terminal panel, opens a separate shell \
         underneath the main detail in the current directory. Click a pane to focus it; the \
         TERMINAL badge identifies shell input. The Rakumux key returns to Rakumux mode, \
         then Esc returns to the shell or T hides it. The hide button never ends its program. \
         Drag the divider to resize. Short windows show the focused pane alone. No session \
         or session-list entry is created. Exit closes the pane and releases its terminal; \
         closing or reloading this UI ends the temporary shell. A daemon restart in place \
         preserves it, reconnecting read-only until refocused.",
    );
    doc.heading("Folders");
    doc.text(
        "Every session belongs to a folder: the one it was started in, such as a git \
         repository, a worktree or any other folder. Rakumux shows the sessions of all \
         folders, under a header per folder. `Enter` on the folder filter at the top of the \
         list (or `P`) narrows the list to one folder or several, or adds a folder. The \
         Sessions tab has the details.",
    );

    doc.heading("The screen");
    match ctx.layout {
        SessionLayout::Sidebar => doc.bullet(format!(
            "Session list, on the left: the tabs on top (Status, {} Folders, App), then the \
             sessions in the order you keep them (a new one goes at the bottom), each with a \
             status mark, a bold title, and its app name in muted blue before the git branch; \
             the selected title is white, the others a softer foreground colour. With \
             several folders, each has a header row; pinned sessions lead their folder.",
            ctx.icons.folder()
        )),
        SessionLayout::Grid => doc.bullet(
            "Grid: read-only live terminal cards. Arrows or h/j/k/l select; PgUp/PgDn change \
             page. Enter or a tap opens a session; the Rakumux key returns to Grid. Folder \
             and text filters and pinned/user ordering apply here too.",
        ),
        SessionLayout::Tabs => doc.bullet(format!(
            "Tab bar, along the top: the folder filter ({} Folders) at the left, then the \
             sessions as numbered tabs, each with a status mark, in the order you keep them. \
             Pinned tabs (★) come first.",
            ctx.icons.folder()
        )),
    }
    doc.bullet("Detail or Grid cards: terminal screens, updating as the programs run.");
    doc.bullet(
        "The feed (`f`): what your agents said when they finished a turn, on every machine of \
         your account, in a pane on the right (a tab in Tabs); `f` again hides it.",
    );
    doc.bullet(format!(
        "Footer, the bottom row: a full-width orange bar in Rakumux mode, with buttons for \
         the feed (`f`, with a count of new replies), this Help (`?`) and Settings (`,`); a \
         muted blue bar with a SESSION or TERMINAL label while a program takes keys, with a `{p}` button, the way to Rakumux mode. The \
         session it's about is named in the middle (session: name). It shows messages and the \
         daemon connection when it isn't up. Settings lists every key."
    ));
    doc.note(format!(
        "Current layout: {}. The Layouts tab explains all three layouts and how to switch.",
        ctx.layout.label()
    ));

    doc.heading("Quick start");
    doc.step(
        1,
        "Press `n` to start a session with a generated name. You're working in it straight \
         away; use `r` in Rakumux mode to rename it later.",
    );
    doc.step(
        2,
        format!(
            "Press `{p}` for Rakumux mode. Move between sessions with `↑`/`↓` or `j`/`k`; the \
             right side shows each one live with a faded, inactive treatment."
        ),
    );
    doc.step(
        3,
        "Press `Enter` to work in the selected session, or `Esc` to go back to the one you \
         came from. `Tab` goes to the session before that.",
    );
    doc.step(
        4,
        "In Rakumux mode, `?` opens this Help and `,` opens Settings: the layout and icons, \
         then every key, and `Enter` runs the one you pick. `q` quits; quitting leaves every \
         session running.",
    );

    moving_around(doc, k);
    mouse(doc);
    modal_keys(doc);
}

/// Rakumux mode's keys for moving around and getting back into a
/// session, and the cycle keys.
fn moving_around(doc: &mut Doc, k: &KeyNames) {
    let p = &k.p;
    doc.heading("Moving around (Rakumux mode)");
    let mut keys = vec![
        row(
            "↑, ↓, j, k",
            "Select a session or a header; the right side shows the session (the tabs, All \
             folders, App and Status, are above the first)",
        ),
        row(
            "←, →",
            "Switch between the tabs, Status, Folders and App (h, l on the tabs too)",
        ),
        row(
            "f",
            "The feed: shows it with the keys, gives it the keys, or hides it. What agents \
             said when they finished a turn, on every machine of your account (turn it on in \
             Settings → Available on sign-in), in a pane on the right (a tab in Tabs). With \
             its keys, ↑/↓ go through the replies; Enter reads one whole (Esc back), again goes \
             into its session; r writes back to it: typed into its session when its agent \
             waits, and shown with the answer as a thread; Esc gives the list the keys",
        ),
        row(
            "gg, G, Home, End",
            "Top (the tabs) / bottom (+ New session)",
        ),
        row("Ctrl-d, Ctrl-u", "Half a page down / up"),
        row("{, }", "Previous / next folder, when several are shown"),
        row(
            "Enter, i, l",
            "Work in the selected session; on the tabs, the active tab's modal; on a \
             folder's header, a new session there",
        ),
        row(
            format!("Esc, {}", k.key),
            "Back to the session you came from (Esc first cancels a pending key or the filter)",
        ),
        row("Tab", "The session you worked in before"),
        row("1-9", "Work in the session at that position"),
        row(
            "Ctrl-y, Ctrl-e, PgUp, PgDn",
            "Scroll the session shown on the right",
        ),
    ];
    if k.cycle.is_some() {
        keys.push(row(
            k.cycle_raw.clone(),
            if k.cycle_in_session {
                "Previous / next session, in Rakumux mode and inside sessions"
            } else {
                "Previous / next session"
            },
        ));
    }
    doc.keys(keys);
    match &k.cycle {
        Some(_) => doc.note(
            "If Option/Alt+↑/↓ do nothing, your terminal isn't sending Option/Alt with the key: \
             iTerm2, Ghostty, kitty, WezTerm and Alacritty do; in Terminal.app turn on \"Use \
             Option as Meta key\". A setting in Settings makes them work inside sessions too.",
        ),
        None => doc.note("Option/Alt+↑/↓ are turned off in your settings; ↑/↓ and j/k move."),
    }
    doc.note(format!(
        "From a session, everything starts with `{p}`: `{p} n` is a new session, `{p} ?` this \
         Help. Every key: Settings (`,`)."
    ));
}

fn mouse(doc: &mut Doc) {
    doc.heading("Mouse");
    doc.bullet("Click a session in the list to show it; click the session to work in it.");
    doc.bullet(
        "Click a tab at the top of the list (Status, Folders, App) to group the list by it; \
         click the active one (▾) to open its modal; click a row there to show it alone, All \
         to show everything, or outside it to close it.",
    );
    doc.bullet(
        "Click the footer's Feed button to show the feed (again to hide it). Click a reply to \
         select it, again to read it; click either side to give it the keys. Drag the line \
         left of the feed to resize it.",
    );
    doc.bullet(
        "Drag a session up or down the list (or a tab left or right) to move it; a blue bar \
         marks it while you hold it. A click without moving just selects it.",
    );
    doc.bullet(
        "Drag the line between the list and the session to resize the list: left and right, \
         or up and down when the list is above the session (narrow screens).",
    );
    doc.bullet(
        "Click the mode badge at the bottom left: SESSION switches to Rakumux mode, RAKUMUX \
         does what Esc does (back to the session you came from; while the feed has the keys, \
         Esc there). Click a button on the right of the footer (`f ≡ Feed`, `? Help`, \
         `, Settings`, or in a session the Rakumux key) to do what it says. Phones send taps \
         as clicks, so all of this works there too.",
    );
    doc.bullet(
        "Wheel: scrolls the session's history, or goes to the program when it uses the mouse \
         itself. In Settings it moves the selection; in Help it scrolls.",
    );
    doc.bullet("Hold Shift while dragging to select and copy text with your terminal.");
    doc.bullet("Tabs layout: click a tab to show that session; click ‹ or › for hidden tabs.");
}

/// Keys inside Help and Settings.
fn modal_keys(doc: &mut Doc) {
    doc.heading("In Help and Settings");
    doc.keys(vec![
        row("Tab, ←, →", "Help: switch tab"),
        row("1-9", "Help: jump to a tab"),
        row(
            "↑, ↓",
            "Settings: choose a setting or an action. Help: scroll",
        ),
        row("→, ←", "Settings: show or hide what the selected row does"),
        row(
            "Enter, Space",
            "Change the selected setting, or run the selected action",
        ),
        row(
            "PgUp, PgDn, Home, End",
            "Scroll, or move a page in Settings",
        ),
        row(
            format!("Esc, q (and ? in Help; comma or {SETTINGS_KEY} in Settings)"),
            "Close",
        ),
    ]);
}

/// Keys while filtering and in the Folders modal.
fn filter_and_switcher_keys(doc: &mut Doc) {
    doc.heading("While filtering (/)");
    doc.keys(vec![
        row("type", "Narrow the list to matching sessions"),
        row("↑, ↓", "Select among the matches"),
        row("Enter", "Keep the filter; the list keys work again"),
        row("Esc", "Clear the filter"),
    ]);
    doc.heading("Folders (Enter on the folder filter, or P)");
    doc.keys(vec![
        row(
            "type",
            "Filter the folders by name, path or branch. A folder path (starting with /, ~ or \
             ., or with a / in it) is offered as + Add folder, with the folders that complete \
             it listed below; one that doesn't exist is created after you confirm with y.",
        ),
        row(
            "↑, ↓",
            "Move between the rows, the folders that complete a typed path included",
        ),
        row(
            "Enter",
            "Show the folder alone; on All folders, show every folder. After ticking, show the \
             ticked folders and the one under the cursor. On + Add folder, + Create folder or a \
             listed folder: add that folder and show it.",
        ),
        row(
            "Tab, Space",
            "Tick or untick a folder to show several; on All folders, clear the ticks. On a \
             typed path Tab completes it with the chosen folder (a / follows, so its folders \
             come next), or as far as the listed folders agree, and Space is typed.",
        ),
        row("→", "At the end of a typed path: complete it, like Tab"),
        row("Esc", "Close without changing anything"),
    ]);
}

fn sessions(doc: &mut Doc, k: &KeyNames) {
    let p = &k.p;
    doc.heading("Sessions");
    doc.text(
        "A session is a folder plus a terminal. It runs your login shell unless you give it a \
         command, and it can type startup commands into the shell once it's ready.",
    );
    doc.text(format!(
        "Every session has a name. Rakumux picks one when you create a session in the TUI; \
         rename it any time with `r` in Rakumux mode (`{p} r` from inside it)."
    ));
    doc.text(
        "`n` (or + New session) creates immediately, selects the new session and enters its \
         login-shell prompt without a naming dialog. It goes in the current folder. Selecting a session or \
         folder header sets this folder; moving to + New session or the folder filter keeps \
         it. A single-folder filter takes precedence. Without folder context, the launch \
         folder is used. For another folder, a command or startup commands, \
         use the command line: `rakumux new --cwd DIR --cmd C -- ARGV`.",
    );

    doc.heading("Folders");
    doc.text(
        "A session belongs to the folder it was started in: a git repository, a git worktree \
         or any other folder (with symlinks resolved). A folder holds any number of sessions. \
         A session's directory must be inside its folder; if you `cd` somewhere else later, \
         the session stays in its folder.",
    );
    doc.text(
        "The list shows the sessions of every folder, including sessions started from another \
         window, the command line or an agent, as they appear. Each folder shown has a header \
         row with its name, git branch and how many of its sessions are running (live/total), \
         even when it's the only one. Sessions whose agent works in a git worktree (one inside \
         the repo, like worktrees/feature, or elsewhere) go under that worktree's own header \
         in the folder, and move as the agent changes worktree; Enter on that header starts a \
         new session in the worktree. The folder you started Rakumux in comes first, then the \
         others by name. `{` and `}` jump between folders.",
    );
    doc.text(
        "The folder filter at the top of the list (at the left end of the tab bar in the Tabs \
         layout) says what's shown: Folders (all of them), one folder's name, or how many. \
         `Enter` on it, `P` or a click opens the Folders modal: All folders, then every folder \
         that has sessions. Type to narrow the rows; `Enter` shows the folder under the cursor \
         alone, and on All folders every folder again. Tick several with `Tab` or `Space` and \
         press `Enter` to see them together. Type a folder path to add a folder (it's created \
         if it doesn't exist) and show it, so you can start sessions there: the folders that \
         complete it are listed as you type; `↑`/`↓` choose one, `Tab` completes the path with \
         it and `Enter` adds it.",
    );
    doc.text(
        "The list's top row has three tabs: Status (where the list starts), a header per \
         state (waiting for you, unread, working, waiting for response, running, exited, \
         dead, then ready: the sessions you've read, pinned ones first, then the rest most \
         recently read first, under last hour, last day and older), Folders, and App, a header per agent or program (claude, codex, \
         opencode, …, then other programs, then shell); `←`/`→` there (or a click) switch \
         them. One tab is active \
         at a time: it decides the headers and the only filter that applies, and each tab \
         keeps its own while another is active. `Enter` on the active tab opens its modal \
         (Apps, Statuses) like the Folders modal: tick several with `Tab` (or `Space` while \
         nothing is typed), `Enter` shows them, `Esc` closes. A session you select stays \
         shown when its status leaves the filter, until you select another.",
    );
    doc.text(
        "The filter only narrows what this window shows; it isn't saved, so Rakumux always \
         starts on every folder, under the Status tab.",
    );

    filter_and_switcher_keys(doc);

    doc.heading("Order");
    doc.text(
        "The list keeps its order: using a session never moves it, and a new session goes at \
         the bottom of its folder, just above + New session.",
    );
    doc.text(
        "To move a session, drag it up or down with the mouse (in the Tabs layout, drag its tab \
         left or right), or press `J` or `K` in Rakumux mode to move the selected one down \
         or up. It stays in its folder, and among the pinned sessions or the others. The \
         order is saved in ~/.rakumux/config.json as `sessions.session_order`.",
    );

    doc.heading("Pinning");
    doc.text(
        "`p` pins the selected session, or unpins it. With the mouse, the session under the \
         pointer shows ☆ left of its name (★ when pinned): a click pins or unpins it. Pinned \
         sessions sit in their own section at the top of the list, under ★ pinned, above every \
         folder, app and status, under every tab (tabs show ★ after their name), in the order \
         you give them there. Unpinned, a session goes back to its place among the others. \
         Every Rakumux window sees the same pins.",
    );

    doc.heading("Git branch");
    let b = k.icons.branch();
    doc.text(format!(
        "The second line of each session shows its git branch ({b} main, or {b} @ and a short \
         commit id when no branch is checked out), then the program and what it's doing. \
         What the session reported follows when there's room; widen the list (drag the line \
         beside it) to see more. Rakumux reads this from the repository's files as the \
         session moves around, so a branch switch shows within seconds; it never runs git. A \
         server started by an older Rakumux can't read it: Rakumux offers to restart it \
         (`rakumux daemon restart` does too)."
    ));
    doc.text(
        "Settings, Icons picks the marks for folders and branches: plain symbols, \
         emoji or Nerd Font icons, whichever your terminal shows.",
    );

    doc.heading("Status");
    doc.keys(vec![
        row("●", "Running"),
        row(
            "◐",
            "Waiting for you: an agent asked a question or wants permission",
        ),
        row("✕", "Exited: the program ended; its last screen is kept"),
        row(
            "○",
            "Dead: the terminal is gone, for example after a reboot",
        ),
    ]);
    doc.text(
        "For coding agents the list also shows what they're doing: working, blocked (waiting \
         for you), unread reply (finished, not opened yet) or idle. The right-side badge is \
         a spinner for working (including subagents), ○ for idle, ● for an unread reply, \
         or ◐ when input is needed. Opening the session clears unread across windows; \
         list and Grid previews do not. A reply that arrives in the foreground open session \
         is already seen. Rakumux uses agent hooks when available, with screen/process \
         detection as the fallback.",
    );

    doc.heading("Watching and controlling");
    doc.text(
        "Selecting a session shows it as an observer: you see its output as it happens, and your keys \
         stay in Rakumux.",
    );
    doc.text(format!(
        "`Enter` makes you the controller: your keys go to the program and its terminal is \
         resized to fit your pane. `{p}` hands control back and switches to Rakumux mode."
    ));
    doc.text(
        "One client controls a session at a time. When another Rakumux window or `rakumux \
         attach` takes control, you become an observer again and the footer says so.",
    );

    doc.heading("Exited and dead sessions");
    doc.text(
        "Ending the program you're working in (`exit`, Ctrl-D, quitting the agent) deletes \
         its session. Exited: the program ended in the background. Its last screen stays \
         visible, and `Enter` restarts it in the same folder.",
    );
    doc.text(
        "Dead: the terminal no longer exists, for example after `rakumux daemon stop`. The \
         saved scrollback is shown dimmed, and `Enter` revives the session: a new terminal in \
         the same folder, its startup commands run again, an agent (Claude Code, Codex, \
         OpenCode) picks up its conversation, and another program's command waits at the \
         prompt.",
    );
    doc.text("Deleting a session (`d`) removes it and its saved scrollback for good.");

    doc.heading("One server, many windows");
    doc.text(
        "A single background server, `rakumux-ptyd`, owns every session for your user. \
         Rakumux starts it when needed. Each Rakumux window, `rakumux attach` and every \
         `rakumux` command is a client of that server.",
    );
    doc.text(
        "Run Rakumux in as many terminals as you like: they show the same sessions and stay \
         in step as sessions are created, renamed or exit. Several windows can watch one \
         session; one of them at a time controls it.",
    );
    doc.text(
        "Quitting Rakumux never stops sessions. `rakumux daemon stop` stops the server and \
         ends every session; they show as dead and can be revived.",
    );
    doc.text(
        "You can run Rakumux inside one of its own sessions. That session is marked as the \
         current one and can't be opened from inside itself.",
    );

    doc.heading("Agent activity hooks");
    doc.text(
        "Optional OpenCode, Claude Code and Codex hooks report tool phases, input requests \
         and active subagent counts without an AI key. Busy markers animate locally; \
         needs-input and ready stay still. Without fresh hooks, Rakumux uses screen/process \
         detection. Running subagents show as a count after the status (◈ 3). See \
         integrations/README.md in the repository for setup. Agent subagents are not Rakumux \
         child sessions.",
    );
    doc.heading("AI naming");
    doc.text(
        "A session you didn't name gets a better name the first time it settles (its program \
         goes idle or finishes a task): Rakumux sends the session's recent commands and \
         screen text to your AI provider and renames the session with its suggestion.",
    );
    doc.text(
        "It uses Anthropic or OpenAI with the API key in ~/.rakumux/config.json \
         (`anthropic_api_key`, `openai_api_key`) or the ANTHROPIC_API_KEY or OPENAI_API_KEY \
         environment variable. Without a key, sessions keep their generated names. Names you \
         choose are never replaced.",
    );
}

fn layouts(doc: &mut Doc, ctx: &ModalContext, k: &KeyNames) {
    let p = &k.p;
    doc.note(format!("Current layout: {}.", ctx.layout.label()));

    doc.heading("Master detail (default)");
    doc.text(
        "The session list sits in a column on the left; the selected session fills the right.",
    );
    doc.text(
        "`b` or the « Hide list button at the bottom of the list hides it, so the session gets \
         its columns; the keys still move through the sessions. `b` again, or a click on the » \
         left of the session, brings it back.",
    );
    doc.text(format!(
        "Below {} columns (a split window) the list goes on top and the selected session \
         below it, so you see it while you pick one; drag the line between them up or down to \
         resize the list. Once you press `Enter` the session fills the screen, and `{p}` \
         brings the list back. Below {} rows only the list shows in Rakumux mode.",
        layout::SINGLE_PANE_BELOW,
        layout::STACK_MIN_ROWS + 1
    ));
    doc.text(format!(
        "On a phone (below {} columns) Master detail and Tabs are offered, not Grid. Master \
         detail shows one thing at a time: Rakumux mode is the session list; tap a session to \
         open it, full screen under a row of tabs (tap a tab to switch); Tabs keeps the tabs \
         over the session in Rakumux mode too; `{p}` or a tap on the SESSION badge at the bottom \
         left goes back to the list, and the RAKUMUX badge returns to the session. Tap \
         `? Help` or `, Settings` in Rakumux mode to open them.",
        layout::PHONE_BELOW
    ));
    doc.text(
        "A session is one terminal, so every window shows the program drawn for one size: \
         the size of the window you're using. Press a key or click in a window (or bring it to \
         the front) and the session takes its size, unless someone is typing in it in another \
          window right now. Grid browsing never claims a terminal's size or control.",
    );

    doc.heading("Grid");
    doc.text(
        "A responsive page of up to six cards shows real live terminal output as read-only \
         observers; phones don't offer Grid. Only visible cards have preview streams. Terminals \
         keep their existing size; cards crop and pan rather than resizing them. Titles and \
         selected borders stay readable while inactive output fades. Saved sessions show \
         their saved output; the session Rakumux runs inside is never attached.",
    );
    doc.text(format!(
        "Arrows or `h`/`j`/`k`/`l` select cards; `PgUp`/`PgDn` change page, `Home`/`End` \
         go to the ends. `Enter` or a tap opens the selected session full screen under tabs; \
         `{p}` returns to Grid. `n` or the + New session card creates and enters a generated-name \
         session immediately. `P` filters folders, `/` filters text, and pins and your saved \
         session order are retained. Browsing, scrolling, focus and window resize send no \
         terminal input, size requests or activity touches."
    ));

    doc.heading("Tabs");
    doc.text(
        "Sessions appear as a row of numbered tabs across the top, and the selected tab's \
         session fills the rest of the screen. Long names are shortened with …, and ‹3 or 5› \
         say how many tabs are hidden on that side. The tabs are the sessions the list's \
         active tab shows, in the Folders order (pinned first, then folder by folder, as you \
         arranged them), so a tab keeps its place and number while its session works and \
         you read it.",
    );
    let cycle = match &k.cycle {
        Some(cycle) => format!(", `{cycle}`"),
        None => String::new(),
    };
    doc.text(format!(
        "In Rakumux mode, switch tabs with `↑`/`↓`, `j`/`k`{cycle} or the number keys, or \
         click a tab. Drag a tab left or right to move it. All other keys work as in the \
          Master detail layout."
    ));

    doc.heading("The feed");
    doc.text(format!(
        "`f` shows the feed: in Master detail and Grid a pane on the right, and the session \
         narrows to make room while it keeps {} columns; a narrower window shows the feed in \
         the session's place in Rakumux mode only. Tabs gives it a tab of its own before the \
         sessions'. On a phone it fills the screen. Every start hides it.",
        layout::FEED_SESSION_MIN_COLS
    ));

    doc.heading("Switching layouts");
    doc.text(format!(
        "Change Layout in Settings (`,` or `{SETTINGS_KEY}` in Rakumux mode). \
         It applies at once to this window; every start opens Master detail. `v` cycles \
          Master detail, Grid and Tabs in Rakumux mode (a phone skips Grid); session mode \
          forwards `v` to the program."
    ));

    doc.heading("Zoom");
    doc.text(format!(
        "`z` in Rakumux mode works in the selected session with the list or tab bar hidden, \
         so it gets the whole screen above the footer, which keeps showing the mode; `{p}` \
          brings the list or Grid back. Works in all layouts."
    ));

    doc.heading("Session cycle keys");
    match &k.cycle {
        Some(cycle) => {
            doc.text(format!(
                "`{cycle}` moves to the previous or next session in list order in Rakumux \
                  mode, in all layouts. It needs a terminal that sends Option/Alt with the key \
                 (iTerm2, Ghostty, kitty, WezTerm, Alacritty; Terminal.app with \"Use Option \
                 as Meta key\")."
            ));
            doc.text(format!(
                "Inside a session these keys normally go to the program: OpenCode, Pi and \
                 Claude Code use them. Turn on \"Option+↑/↓ in sessions\" in Settings \
                 to switch sessions with them from inside one too (`{p}` and \
                 `↑`/`↓` always work)."
            ));
        }
        None => doc.text("Turned off in your settings; `↑`/`↓` and `j`/`k` move."),
    }
    doc.text(
        "The keys are set by `sessions.cycle_keys` in ~/.rakumux/config.json, and the \
         Rakumux key by `sessions.rakumux_key`.",
    );
}

fn agents_and_cli(doc: &mut Doc) {
    doc.heading("Command line");
    doc.text(
        "The `rakumux` command also manages sessions without opening this screen. Names are \
         looked up among the current folder's sessions; an id always works.",
    );
    doc.command("rakumux", "Open this screen for the current folder.");
    doc.command(
        "rakumux new [--name N] [--cwd DIR] [--cmd C]... [--detach] [-- PROGRAM ARGS...]",
        "Create a session and attach to it, or leave it running with --detach. --cmd adds a \
         startup command; after -- give a program to run instead of your shell.",
    );
    doc.command(
        "rakumux ls [--json] [--dead] [--folder DIR]... | --all",
        "List this folder's sessions, those of the folders given, or of every folder with \
         --all; --dead includes dead ones. ^ marks a pinned session.",
    );
    doc.command(
        "rakumux pin <name|id>, rakumux unpin <name|id>",
        "Pin a session into the pinned section at the top of the list, or unpin it.",
    );
    doc.command(
        "rakumux folders [--json], rakumux folders add|rm <dir>",
        "List the folders, add one, or remove one that has no sessions.",
    );
    doc.command(
        "rakumux attach <name|id>",
        "One session full screen, outside this screen (works over SSH). `Ctrl-\\` then `d` \
         detaches, `>` / `<` switch session, `n` starts a new one.",
    );
    doc.command(
        "rakumux rename [<name|id>] <new name>",
        "Rename a session. Inside a session, without a target it renames itself.",
    );
    doc.command(
        "rakumux kill <name|id> [--tree]",
        "End a session's program; --tree also ends the sessions it started.",
    );
    doc.command(
        "rakumux rm <name|id>",
        "Delete a session and its saved scrollback.",
    );
    doc.command(
        "rakumux daemon start|status|stop|restart|logs",
        "Start (if it isn't running), check, stop or restart the background server, or follow \
         its log. Restarting keeps every session running; stopping ends them.",
    );
    doc.command(
        "rakumux --new-server",
        "Replace the running server with this Rakumux's (sessions keep running), or start \
         one, then open the session screen.",
    );
    doc.note("Exit codes: 0 success, 1 error (message on stderr), 2 wrong usage.");

    doc.heading("On your phone");
    doc.text(
        "The Rakumux app shows your sessions on a phone and types into them, over Tailscale. \
         It's off until you turn it on: `rakumux remote on`, or the question Rakumux \
         asks once when it sees Tailscale running. Only devices on your tailnet that have the \
         pairing code get in, and Rakumux listens only on Tailscale's own addresses.",
    );
    doc.command(
        "rakumux remote on|off",
        "Turn phone access on (it stays on) or off.",
    );
    doc.command(
        "rakumux remote pair",
        "Show the URL, the pairing code and a QR code to scan with the app.",
    );
    doc.command(
        "rakumux remote status, rakumux remote token --rotate",
        "Check it, or make a new pairing code (paired phones need the new one).",
    );

    doc.heading("For agents and scripts");
    doc.text(
        "Every session's environment says where it is: RAKUMUX_SESSION_ID, \
         RAKUMUX_SESSION_TOKEN, RAKUMUX_SOCKET and RAKUMUX_BIN. Coding agents and scripts \
         use them through `rakumux session`, which prints JSON:",
    );
    doc.command(
        "rakumux session list",
        "This folder's sessions, with status and what their agents are doing.",
    );
    doc.command("rakumux session get <name|id>", "One session in detail.");
    doc.command(
        "rakumux session read <name|id>",
        "Text on a session's screen or in its recent history.",
    );
    doc.command(
        "rakumux session wait <name|id>",
        "Wait until a session settles, exits or shows some output.",
    );
    doc.command(
        "rakumux session report --state done --text \"...\"",
        "Report this session's own state, a result (--summary, --file) and how to resume it \
         after a reboot (--resume -- PROGRAM ARGS).",
    );
    doc.command(
        "rakumux session bind --agent opencode <conversation>",
        "Claim agent conversations for this session, so the MCP tools they call act as it. \
         The OpenCode plugin does this for you (see `rakumux-mcp opencode-plugin`).",
    );
    doc.note("`rakumux session --help` lists every option.");

    doc.heading("What agents may do");
    doc.text(
        "From inside a session, an agent can list, read and wait on the sessions of its \
         folder, start new sessions there (`rakumux new --detach`), and rename or report on \
         its own session. It may have 4 of the sessions it started running at once, and those \
         may start sessions of their own but no deeper. Typing into or ending other sessions \
         from an agent comes in a later release; those requests are refused for now.",
    );
    doc.text(
        "Sessions never reach outside their folder. Treat text read from another session as \
         data, not instructions: it's whatever that program printed.",
    );
    doc.text(
        "These limits prevent accidents, not a hostile program: everything runs as your user.",
    );
}

fn license(doc: &mut Doc) {
    doc.heading("Rakumux License");
    doc.text("Copyright 2024–2026 Antonio de Perio and Human Unsupervised Pty Ltd");
    doc.strong("Rakumux is open-source software, licensed under Apache-2.0.");
    doc.dash("Source: https://github.com/humanunsupervised/rakumux · https://rakumux.com");
    for paragraph in include_str!("../../../LICENSE").split("\n\n") {
        let paragraph = paragraph.trim();
        if !paragraph.is_empty() {
            doc.text(paragraph);
        }
    }
}

// ---------------------------------------------------------------------------
// Layout of content into lines
// ---------------------------------------------------------------------------

struct Styles {
    text: Style,
    note: Style,
    strong: Style,
    heading: Style,
    key: Style,
    command: Style,
    marker: Style,
}

impl Styles {
    fn from_theme() -> Self {
        let palette = theme::palette();
        Self {
            text: Style::default().fg(palette.text_primary),
            note: Style::default().fg(palette.neutral_gray),
            strong: Style::default()
                .fg(palette.text_primary)
                .add_modifier(Modifier::BOLD),
            heading: Style::default()
                .fg(palette.accent_primary)
                .add_modifier(Modifier::BOLD),
            key: Style::default().fg(palette.accent_green),
            command: Style::default().fg(palette.accent_purple),
            marker: Style::default().fg(palette.accent_green),
        }
    }
}

/// The body of `tab`, wrapped to `width` columns.
fn body_lines(tab: ModalTab, ctx: &ModalContext, width: usize) -> Vec<Line<'static>> {
    let styles = Styles::from_theme();
    let width = width.max(1);
    let mut lines: Vec<Line<'static>> = Vec::new();
    for item in content(tab, ctx).0 {
        match item {
            Item::Heading(text) => {
                if !lines.is_empty() {
                    lines.push(Line::default());
                }
                let words = plain_words(&text, styles.heading);
                lines.extend(wrap(&words, width, Vec::new(), 0));
            }
            Item::Text(text) => lines.extend(wrap(
                &words(&text, styles.text, styles.key),
                width,
                Vec::new(),
                0,
            )),
            Item::Note(text) => lines.extend(wrap(
                &words(&text, styles.note, styles.key),
                width,
                Vec::new(),
                0,
            )),
            Item::Strong(text) => lines.extend(wrap(
                &words(&text, styles.strong, styles.key),
                width,
                Vec::new(),
                0,
            )),
            Item::List(marker, text) => {
                let lead = format!("  {marker} ");
                let indent = str_width(&lead);
                let first = vec![Span::styled(lead, styles.marker)];
                lines.extend(wrap(
                    &words(&text, styles.text, styles.key),
                    width,
                    first,
                    indent,
                ));
            }
            Item::Keys(rows) => lines.extend(key_table(&rows, width, &styles)),
            Item::Command(command, description) => {
                let first = vec![Span::raw("  ")];
                lines.extend(wrap(
                    &plain_words(&command, styles.command),
                    width,
                    first,
                    6,
                ));
                let first = vec![Span::raw("    ")];
                let words = words(&description, styles.text, styles.key);
                lines.extend(wrap(&words, width, first, 4));
            }
        }
    }
    lines
}

/// The Settings page's lines.
struct SettingsPage {
    lines: Vec<Line<'static>>,
    /// The first and last lines of the selected row (from its heading when
    /// it starts a section), which the body keeps in view.
    selected: (usize, usize),
    /// The selected row is on the page (the search shows it).
    has_selected: bool,
}

/// The Settings page: one row per setting (its label, then its current
/// value, or the value on the next line when both don't fit), then under a
/// `Keys` rule one row per [`KeyAction`] with its keys, each under its
/// heading; then where settings are saved. Only the rows `query` matches
/// show. The selected row is marked and white; notes show in a popover
/// ([`draw_note`]), so the page never moves.
fn settings_lines(ctx: &ModalContext, selected: usize, query: &str, width: usize) -> SettingsPage {
    let styles = Styles::from_theme();
    let width = width.max(1);
    let mut page = SettingsPage {
        lines: Vec::new(),
        selected: (0, 0),
        has_selected: false,
    };
    push_setting_rows(&mut page, ctx, selected, query, width, &styles);
    let action = selected.checked_sub(ctx.general.len());
    push_action_rows(&mut page, ctx, action, query, width, &styles);
    if page.lines.is_empty() {
        let none = format!("Nothing matches “{}”.", query.trim());
        page.lines.extend(wrap(
            &words(&none, styles.note, styles.key),
            width,
            Vec::new(),
            0,
        ));
        return page;
    }
    if query.is_empty() {
        page.lines.push(Line::default());
        let saved = words(
            "Saved in ~/.rakumux/config.json. Help (`?`) explains how Rakumux works.",
            styles.note,
            styles.key,
        );
        page.lines.extend(wrap(&saved, width, Vec::new(), 0));
    }
    page
}

/// The settings: label and value side by side, or every value under its
/// label when one pair doesn't fit (a phone), so all the choices show.
fn push_setting_rows(
    page: &mut SettingsPage,
    ctx: &ModalContext,
    selected: usize,
    query: &str,
    width: usize,
    styles: &Styles,
) {
    let palette = theme::palette();
    let lines = &mut page.lines;
    let label_width = setting_label_width(ctx);
    let values_below = ctx
        .general
        .iter()
        .any(|row| 2 + label_width + str_width(&row.value) > width);
    let mut section = "";
    for (i, row) in ctx.general.iter().enumerate() {
        if !matches_query(&search_text(row.section, &row.label), query) {
            continue;
        }
        let mut first = lines.len();
        if row.section != section {
            section = row.section;
            push_section(lines, section, width, styles);
            first = lines.len() - 1;
        }
        let (marker, label_style, value_style) = match (i == selected, row.disabled) {
            (true, false) => (
                "▶ ",
                Style::default()
                    .fg(palette.text_bright)
                    .add_modifier(Modifier::BOLD),
                Style::default().fg(palette.text_primary),
            ),
            (false, false) => (
                "  ",
                Style::default().fg(palette.text_primary),
                Style::default().fg(palette.text_muted),
            ),
            // Greyed out, selected or not.
            (chosen, true) => (
                if chosen { "▶ " } else { "  " },
                Style::default().fg(palette.text_subtle),
                Style::default().fg(palette.text_subtle),
            ),
        };
        let marker = Span::styled(marker, Style::default().fg(palette.accent_primary));
        if !values_below {
            // The label on the left, the value pinned to the right.
            let label = truncate(&row.label, width.saturating_sub(2));
            let value = truncate(&row.value, width.saturating_sub(2 + label_width));
            let pad = width.saturating_sub(2 + str_width(&label) + str_width(&value));
            lines.push(fit(
                vec![
                    marker,
                    Span::styled(label, label_style),
                    Span::raw(" ".repeat(pad)),
                    Span::styled(value, value_style),
                ],
                width,
            ));
        } else {
            lines.push(fit(
                vec![marker, Span::styled(row.label.clone(), label_style)],
                width,
            ));
            lines.push(fit(
                vec![
                    Span::raw(" ".repeat(VALUE_INDENT)),
                    Span::styled(
                        truncate(&row.value, width.saturating_sub(VALUE_INDENT)),
                        value_style,
                    ),
                ],
                width,
            ));
        }
        if i == selected {
            page.selected = (first, lines.len() - 1);
            page.has_selected = true;
        }
    }
}

/// A section heading in a list of rows, after a blank line unless it's
/// the first thing in `lines` (or follows a rule).
fn push_section(lines: &mut Vec<Line<'static>>, title: &str, width: usize, styles: &Styles) {
    let after_rule = lines
        .last()
        .is_some_and(|l| l.spans.first().is_some_and(|s| s.content.starts_with('─')));
    if !lines.is_empty() && !after_rule {
        lines.push(Line::default());
    }
    lines.push(fit(
        vec![Span::styled(title.to_string(), styles.heading)],
        width,
    ));
}

/// The actions under a `Keys` rule and how to run them: a two-column table
/// (what it does, its keys), `selected` (into [`KeyAction::ALL`]) marked.
fn push_action_rows(
    page: &mut SettingsPage,
    ctx: &ModalContext,
    selected: Option<usize>,
    query: &str,
    width: usize,
    styles: &Styles,
) {
    const MARKER: usize = 2;
    const GAP: usize = 3;
    let palette = theme::palette();
    let lines = &mut page.lines;
    let names = KeyNames::new(ctx);
    let shown =
        |action: &KeyAction| matches_query(&search_text(action.section(), action.label()), query);
    if !KeyAction::ALL.iter().any(shown) {
        return;
    }
    // Sized from every action, so the columns stay put while searching.
    let rows: Vec<(&str, String)> = KeyAction::ALL
        .iter()
        .map(|action| (action.label(), action.keys(&names)))
        .collect();
    let widest = rows
        .iter()
        .map(|(label, _)| str_width(label))
        .max()
        .unwrap_or(0);
    // The action column gives way to the keys on narrow screens.
    let widest_key = rows
        .iter()
        .map(|(_, keys)| str_width(keys))
        .max()
        .unwrap_or(0);
    let column = if MARKER + widest + GAP + widest_key <= width {
        widest
    } else {
        widest.min(width.saturating_sub(MARKER + GAP).saturating_mul(3) / 5)
    };
    let key_room = width.saturating_sub(MARKER + column + GAP);

    if !lines.is_empty() {
        lines.push(Line::default());
    }
    let rule = lines.len();
    let title = "Keys";
    let after = width.saturating_sub(3 + str_width(title) + 1);
    let rule_style = Style::default().fg(palette.border_dim);
    lines.push(fit(
        vec![
            Span::styled("── ", rule_style),
            Span::styled(title, styles.heading),
            Span::styled(format!(" {}", "─".repeat(after)), rule_style),
        ],
        width,
    ));
    let p = &names.p;
    if query.is_empty() {
        let how = words(
            &format!(
                "`Enter` runs the selected action, as do its keys in Rakumux mode. From inside \
                 a session, press `{p}` first."
            ),
            styles.note,
            styles.key,
        );
        lines.extend(wrap(&how, width, Vec::new(), 0));
    }
    let mut section = "";
    let mut leading = true;
    for (i, (label, keys)) in rows.iter().enumerate() {
        let action = KeyAction::ALL[i];
        if !shown(&action) {
            continue;
        }
        let mut first = lines.len();
        if action.section() != section {
            section = action.section();
            push_section(lines, section, width, styles);
            // The first heading keeps the rule above it in view.
            first = if leading { rule } else { lines.len() - 1 };
        }
        leading = false;
        let (marker, label_style, key_style) = if Some(i) == selected {
            (
                "▶ ",
                Style::default()
                    .fg(palette.text_bright)
                    .add_modifier(Modifier::BOLD),
                Style::default()
                    .fg(palette.accent_green)
                    .add_modifier(Modifier::BOLD),
            )
        } else {
            (
                "  ",
                Style::default().fg(palette.text_primary),
                Style::default().fg(palette.accent_green),
            )
        };
        // What it does on the left, its keys pinned to the right.
        let label = truncate(label, column);
        let keys = truncate(keys, key_room);
        let pad = width.saturating_sub(MARKER + str_width(&label) + str_width(&keys));
        lines.push(fit(
            vec![
                Span::styled(marker, Style::default().fg(palette.accent_primary)),
                Span::styled(label, label_style),
                Span::raw(" ".repeat(pad)),
                Span::styled(keys, key_style),
            ],
            width,
        ));
        if Some(i) == selected {
            page.selected = (first, lines.len() - 1);
            page.has_selected = true;
        }
    }
}

/// `text` as a note, wrapped to `width`.
fn note_lines(text: &str, width: usize) -> Vec<Line<'static>> {
    if text.is_empty() {
        return Vec::new();
    }
    let styles = Styles::from_theme();
    wrap(&words(text, styles.text, styles.key), width, Vec::new(), 0)
}

/// What Settings row `row` says when asked (`→`): the setting's note or the
/// action's; empty when there's nothing more to say.
fn row_note(ctx: &ModalContext, names: &KeyNames, row: usize) -> String {
    match ctx.general.get(row) {
        Some(setting) => setting.note.clone(),
        None => KeyAction::ALL
            .get(row - ctx.general.len())
            .map(|action| action.note(names))
            .unwrap_or_default(),
    }
}

/// What Settings row `row` is called.
fn row_label(ctx: &ModalContext, row: usize) -> String {
    match ctx.general.get(row) {
        Some(setting) => setting.label.clone(),
        None => KeyAction::ALL
            .get(row - ctx.general.len())
            .map(|action| action.label().to_string())
            .unwrap_or_default(),
    }
}

/// `spans` as one line cut at `width` columns.
fn fit(spans: Vec<Span<'static>>, width: usize) -> Line<'static> {
    let mut used = 0;
    let mut out = Vec::with_capacity(spans.len());
    for span in spans {
        let mut text = String::new();
        for c in span.content.chars() {
            let w = char_width(c);
            if used + w > width {
                break;
            }
            used += w;
            text.push(c);
        }
        if !text.is_empty() {
            out.push(Span::styled(text, span.style));
        }
    }
    Line::from(out)
}

/// Keys in an aligned column, descriptions wrapped beside them. Rows whose
/// keys don't fit the column (or every row, on very narrow screens) put
/// the description on the next line.
fn key_table(rows: &[(String, String)], width: usize, styles: &Styles) -> Vec<Line<'static>> {
    const INDENT: usize = 2;
    const GAP: usize = 2;
    const MIN_DESCRIPTION: usize = 16;
    let widest = rows
        .iter()
        .map(|(keys, _)| str_width(keys))
        .max()
        .unwrap_or(0);
    let column = widest.min(width * 2 / 5).max(1);
    let stacked = width < INDENT + column + GAP + MIN_DESCRIPTION;

    let mut lines = Vec::new();
    for (keys, description) in rows {
        let description = words(description, styles.text, styles.key);
        let key_width = str_width(keys);
        if !stacked && key_width <= column {
            let first = vec![
                Span::raw(" ".repeat(INDENT)),
                Span::styled(keys.clone(), styles.key),
                Span::raw(" ".repeat(column - key_width + GAP)),
            ];
            lines.extend(wrap(&description, width, first, INDENT + column + GAP));
        } else {
            let keys = truncate(keys, width.saturating_sub(INDENT));
            lines.push(Line::from(vec![
                Span::raw(" ".repeat(INDENT)),
                Span::styled(keys, styles.key),
            ]));
            let indent = (INDENT * 3).min(width.saturating_sub(1));
            let first = vec![Span::raw(" ".repeat(indent))];
            lines.extend(wrap(&description, width, first, indent));
        }
    }
    lines
}

/// A run of text that wraps as a unit, possibly in several styles.
struct Word {
    segments: Vec<(String, Style)>,
    width: usize,
}

impl Word {
    fn push(&mut self, text: &str, style: Style) {
        if text.is_empty() {
            return;
        }
        self.width += str_width(text);
        self.segments.push((text.to_string(), style));
    }
}

/// Split markup into words. Backticks switch to `code` style; text inside
/// them is one unbreakable word (unless it's wider than the line).
fn words(text: &str, base: Style, code: Style) -> Vec<Word> {
    let mut out = Vec::new();
    let mut word = Word {
        segments: Vec::new(),
        width: 0,
    };
    let mut run = String::new();
    let mut in_code = false;
    for ch in text.chars() {
        match ch {
            '`' => {
                word.push(&run, if in_code { code } else { base });
                run.clear();
                in_code = !in_code;
            }
            ch if ch.is_whitespace() && !in_code => {
                word.push(&run, base);
                run.clear();
                if word.width > 0 {
                    out.push(std::mem::replace(
                        &mut word,
                        Word {
                            segments: Vec::new(),
                            width: 0,
                        },
                    ));
                }
            }
            LITERAL_BACKTICK => run.push('`'),
            ch => run.push(ch),
        }
    }
    word.push(&run, if in_code { code } else { base });
    if word.width > 0 {
        out.push(word);
    }
    out
}

/// Split plain text on whitespace, all in one style.
fn plain_words(text: &str, style: Style) -> Vec<Word> {
    text.split_whitespace()
        .map(|part| Word {
            segments: vec![(part.to_string(), style)],
            width: str_width(part),
        })
        .collect()
}

/// Greedy word wrap to `width` columns. The first line starts with `first`
/// (its width counts against the line); later lines are indented by
/// `indent` spaces. Words wider than a line are split.
fn wrap(
    words: &[Word],
    width: usize,
    first: Vec<Span<'static>>,
    indent: usize,
) -> Vec<Line<'static>> {
    let first_width: usize = first.iter().map(Span::width).sum();
    let rest_avail = width.saturating_sub(indent).max(1);
    let mut lines = Vec::new();
    let mut current = first;
    let mut avail = width.saturating_sub(first_width).max(1);
    let mut used = 0;

    let mut new_line = |current: &mut Vec<Span<'static>>, used: &mut usize, avail: &mut usize| {
        lines.push(Line::from(std::mem::take(current)));
        if indent > 0 {
            current.push(Span::raw(" ".repeat(indent)));
        }
        *used = 0;
        *avail = rest_avail;
    };

    for word in words {
        if used > 0 && used + 1 + word.width > avail {
            new_line(&mut current, &mut used, &mut avail);
        }
        if used > 0 {
            current.push(Span::raw(" "));
            used += 1;
        }
        if used + word.width <= avail {
            for (text, style) in &word.segments {
                current.push(Span::styled(text.clone(), *style));
            }
            used += word.width;
            continue;
        }
        // Wider than a whole line: split by character.
        for (text, style) in &word.segments {
            let mut piece = String::new();
            for ch in text.chars() {
                let ch_width = char_width(ch);
                if used + ch_width > avail && used > 0 {
                    if !piece.is_empty() {
                        current.push(Span::styled(std::mem::take(&mut piece), *style));
                    }
                    new_line(&mut current, &mut used, &mut avail);
                }
                piece.push(ch);
                used += ch_width;
            }
            if !piece.is_empty() {
                current.push(Span::styled(piece, *style));
            }
        }
    }
    lines.push(Line::from(current));
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyEventState;
    use ratatui::backend::TestBackend;
    use ratatui::buffer::Buffer;
    use ratatui::Terminal;

    /// Settings before the actions in these tests.
    const ROWS: usize = 3;

    fn ctx(prefix: &str) -> ModalContext {
        let setting = |label: &str, value: &str, note: &str| SettingRow {
            section: if label == "Layout" {
                "Appearance"
            } else {
                "AI session naming"
            },
            label: label.into(),
            value: value.into(),
            note: note.into(),
            disabled: false,
        };
        ModalContext {
            rakumux_key: prefix.to_string(),
            cycle_keys: "Option/Alt+↑/↓".to_string(),
            cycle_previous: "Option/Alt+↑".to_string(),
            cycle_next: "Option/Alt+↓".to_string(),
            cycle_in_session: false,
            icons: crate::icons::Icons::Plain,
            layout: SessionLayout::Sidebar,
            version: "0.6.0".to_string(),
            in_session: false,
            general: vec![
                setting("Layout", "[Master detail] Grid Tabs", "List on the left."),
                setting("Anthropic API key", "not set", "Used for AI session naming"),
                setting("OpenAI model", "gpt-4o-mini", "The model for naming"),
            ],
        }
    }

    fn state(tab: ModalTab) -> ModalState {
        ModalState::new(tab, searched(&ctx("Ctrl-b")))
    }

    /// The search text of `ctx`'s settings.
    fn searched(ctx: &ModalContext) -> Vec<String> {
        ctx.general
            .iter()
            .map(|row| search_text(row.section, &row.label))
            .collect()
    }

    /// The Settings row of `action`.
    fn row_of(action: KeyAction) -> usize {
        ROWS + KeyAction::ALL.iter().position(|a| *a == action).unwrap()
    }

    fn press(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn line_text(line: &Line) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    /// The Settings page's lines with `selected` chosen.
    fn page_text(ctx: &ModalContext, selected: usize, width: usize) -> String {
        settings_lines(ctx, selected, "", width)
            .lines
            .iter()
            .map(line_text)
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// The Settings page's lines as `state` shows them.
    fn page_text_for(ctx: &ModalContext, state: &ModalState) -> String {
        settings_lines(ctx, state.selected, &state.query, 80)
            .lines
            .iter()
            .map(line_text)
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// What `→` shows for Settings row `selected`.
    fn note_text(ctx: &ModalContext, selected: usize) -> String {
        row_note(ctx, &KeyNames::new(ctx), selected)
    }

    fn tab_text(tab: ModalTab, ctx: &ModalContext, width: usize) -> String {
        body_lines(tab, ctx, width)
            .iter()
            .map(line_text)
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn buffer_text(buf: &Buffer) -> String {
        let area = buf.area();
        (area.top()..area.bottom())
            .map(|y| {
                (area.left()..area.right())
                    .map(|x| buf.get(x, y).symbol().to_string())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn draw(width: u16, height: u16, state: &ModalState, ctx: &ModalContext) -> Buffer {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|f| render(f, f.size(), state, ctx)).unwrap();
        terminal.backend().buffer().clone()
    }

    /// Where the dialog is in `buf`, from its rounded corners.
    /// (The outermost corners: a popover inside has its own.)
    fn dialog(buf: &Buffer) -> Rect {
        let area = buf.area();
        let corners = |corner: &'static str| {
            (area.top()..area.bottom())
                .flat_map(|y| (area.left()..area.right()).map(move |x| (x, y)))
                .filter(move |(x, y)| buf.get(*x, *y).symbol() == corner)
        };
        let (left, top) = corners("╭").next().expect("no ╭");
        let (right, bottom) = corners("╯").last().expect("no ╯");
        Rect::new(left, top, right - left + 1, bottom - top + 1)
    }

    // --- tabs -----------------------------------------------------------

    #[test]
    fn settings_has_one_page_and_help_its_tabs_in_order() {
        assert_eq!(ModalTab::ALL.len(), 6);
        assert_eq!(ModalTab::default(), ModalTab::Settings);
        assert_eq!(Modal::Settings.tabs(), [ModalTab::Settings]);
        assert_eq!(
            Modal::Help.tabs(),
            [
                ModalTab::Overview,
                ModalTab::Sessions,
                ModalTab::Layouts,
                ModalTab::AgentsCli,
                ModalTab::License
            ]
        );
        assert_eq!(Modal::Settings.first_tab(), ModalTab::Settings);
        assert_eq!(Modal::Help.first_tab(), ModalTab::Overview);
        assert_eq!(ModalTab::License.next(), ModalTab::Overview);
        assert_eq!(ModalTab::Overview.prev(), ModalTab::License);
        assert_eq!(ModalTab::AgentsCli.label(), "Agents & CLI");
        assert_eq!(ModalTab::AgentsCli.short_label(), "CLI");
        for modal in [Modal::Settings, Modal::Help] {
            for (i, tab) in modal.tabs().iter().enumerate() {
                assert_eq!(tab.modal(), modal);
                assert_eq!(tab.index(), i);
                assert_eq!(modal.first_tab().sibling(i), Some(*tab));
            }
            assert_eq!(modal.first_tab().sibling(modal.tabs().len()), None);
        }
    }

    // --- keys -----------------------------------------------------------

    #[test]
    fn close_keys() {
        for tab in ModalTab::ALL {
            let mut state = state(tab);
            assert_eq!(state.handle_key(&press(KeyCode::Esc)), ModalAction::Close);
        }
        // Help closes with `q` and `?` too, and leaves `o` alone; in
        // Settings they're typed into the search.
        let mut help = state(ModalTab::Overview);
        assert_eq!(
            help.handle_key(&press(KeyCode::Char('o'))),
            ModalAction::Ignored
        );
        for closes in ['q', '?'] {
            assert_eq!(
                help.handle_key(&press(KeyCode::Char(closes))),
                ModalAction::Close
            );
        }
        let mut settings = state(ModalTab::Settings);
        for typed in ['q', 'o', ',', '?'] {
            assert_eq!(
                settings.handle_key(&press(KeyCode::Char(typed))),
                ModalAction::Handled
            );
        }
        assert_eq!(settings.query, "qo,?");
        // `?` usually arrives with Shift held.
        let mut state = state(ModalTab::Sessions);
        let shifted = KeyEvent::new(KeyCode::Char('?'), KeyModifiers::SHIFT);
        assert_eq!(state.handle_key(&shifted), ModalAction::Close);
    }

    #[test]
    fn tab_and_arrows_switch_help_tabs_and_reset_scroll() {
        // Settings has nothing to switch to; its arrows open and close a
        // row's note.
        let mut state = self::state(ModalTab::Settings);
        for code in [KeyCode::Tab, KeyCode::BackTab] {
            assert_eq!(state.handle_key(&press(code)), ModalAction::Ignored);
            assert_eq!(state.tab, ModalTab::Settings);
        }
        for (code, open) in [(KeyCode::Right, true), (KeyCode::Left, false)] {
            assert_eq!(state.handle_key(&press(code)), ModalAction::Handled);
            assert_eq!((state.tab, state.note_open), (ModalTab::Settings, open));
        }

        let mut state = self::state(ModalTab::Overview);
        state.scroll = 3;
        state.handle_key(&press(KeyCode::Tab));
        assert_eq!(state.tab, ModalTab::Sessions);
        assert_eq!(state.scroll, 0);
        state.handle_key(&press(KeyCode::Left));
        state.handle_key(&press(KeyCode::Left));
        assert_eq!(state.tab, ModalTab::License);
        let back_tab = KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT);
        state.handle_key(&back_tab);
        assert_eq!(state.tab, ModalTab::AgentsCli);
        for _ in 0..5 {
            state.handle_key(&press(KeyCode::Tab));
        }
        assert_eq!(state.tab, ModalTab::AgentsCli);
        // `h`/`l` too.
        state.handle_key(&press(KeyCode::Char('l')));
        assert_eq!(state.tab, ModalTab::License);
        state.handle_key(&press(KeyCode::Char('h')));
        assert_eq!(state.tab, ModalTab::AgentsCli);
    }

    #[test]
    fn number_keys_jump_to_helps_tabs() {
        let mut state = state(Modal::Help.first_tab());
        for (i, tab) in Modal::Help.tabs().iter().enumerate() {
            let digit = char::from(b'1' + i as u8);
            assert_eq!(
                state.handle_key(&press(KeyCode::Char(digit))),
                ModalAction::Handled
            );
            assert_eq!(state.tab, *tab);
        }
        let past = char::from(b'1' + Modal::Help.tabs().len() as u8);
        assert_eq!(
            state.handle_key(&press(KeyCode::Char(past))),
            ModalAction::Ignored
        );
    }

    #[test]
    fn settings_select_and_change_settings_then_run_actions() {
        let mut state = state(ModalTab::Settings);
        assert_eq!(state.selected, 0);
        state.handle_key(&press(KeyCode::Up));
        assert_eq!(state.selected, 0);
        state.handle_key(&press(KeyCode::Down));
        state.handle_key(&press(KeyCode::Down));
        assert_eq!(state.selected, 2);
        state.handle_key(&press(KeyCode::Up));
        assert_eq!(state.selected, 1);
        assert_eq!(state.selected_action(), None);
        assert_eq!(
            state.handle_key(&press(KeyCode::Enter)),
            ModalAction::Change(1)
        );
        assert_eq!(
            state.handle_key(&press(KeyCode::Char(' '))),
            ModalAction::Change(1)
        );
        // Past the settings come the actions.
        state.handle_key(&press(KeyCode::Down));
        state.handle_key(&press(KeyCode::Down));
        assert_eq!(state.selected, ROWS);
        assert_eq!(state.selected_action(), Some(KeyAction::RakumuxMode));
        assert_eq!(
            state.handle_key(&press(KeyCode::Enter)),
            ModalAction::Run(KeyAction::RakumuxMode)
        );
        state.handle_key(&press(KeyCode::Down));
        state.handle_key(&press(KeyCode::Down));
        assert_eq!(
            state.handle_key(&press(KeyCode::Char(' '))),
            ModalAction::Run(KeyAction::GoIn)
        );
        state.handle_key(&press(KeyCode::End));
        assert_eq!(state.selected, ROWS + KeyAction::ALL.len() - 1);
        state.handle_key(&press(KeyCode::Down));
        assert_eq!(
            state.handle_key(&press(KeyCode::Enter)),
            ModalAction::Run(KeyAction::Quit)
        );
        state.handle_key(&press(KeyCode::Home));
        assert_eq!(state.selected, 0);
        // PgDn and PgUp move a page of rows (before a render, a default
        // page), stopping at the ends.
        state.handle_key(&press(KeyCode::PageDown));
        assert_eq!(state.selected, usize::from(DEFAULT_PAGE));
        state.handle_key(&press(KeyCode::PageUp));
        state.handle_key(&press(KeyCode::PageUp));
        assert_eq!(state.selected, 0);
        // The wheel moves a row per notch.
        state.wheel(2);
        assert_eq!(state.selected, 2);
        state.wheel(-5);
        assert_eq!(state.selected, 0);
        // Without settings the actions come first.
        assert_eq!(
            ModalState::new(ModalTab::Settings, Vec::new()).handle_key(&press(KeyCode::Enter)),
            ModalAction::Run(KeyAction::RakumuxMode)
        );
        let last = ROWS + KeyAction::ALL.len() - 1;
        assert_eq!(state.clone().with_selected(999).selected, last);
        let mut picked = state.clone();
        picked.select_action(KeyAction::Restart);
        assert_eq!(picked.selected_action(), Some(KeyAction::Restart));
        assert_eq!(picked.selected, row_of(KeyAction::Restart));
        // Enter on a Help tab changes nothing.
        state.select_tab(ModalTab::Overview);
        assert_eq!(
            state.handle_key(&press(KeyCode::Enter)),
            ModalAction::Ignored
        );
    }

    #[test]
    fn typing_in_settings_narrows_its_rows_and_selects_the_first() {
        let context = ctx("Ctrl-b");
        let mut state = state(ModalTab::Settings);
        for c in "openai".chars() {
            state.handle_key(&press(KeyCode::Char(c)));
        }
        assert_eq!(state.query, "openai");
        assert_eq!(state.shown_rows(), [2]);
        assert_eq!(state.selected, 2);
        let page = page_text_for(&context, &state);
        assert!(page.contains("OpenAI model"), "{page}");
        assert!(!page.contains("Layout"), "{page}");
        assert!(!page.contains("Keys"), "{page}");
        assert_eq!(
            state.handle_key(&press(KeyCode::Enter)),
            ModalAction::Change(2)
        );
        // Words in any order, any case, sections too; Space types once
        // something is typed.
        state.query.clear();
        for c in "NAMING ai ".chars() {
            state.handle_key(&press(KeyCode::Char(c)));
        }
        assert_eq!(state.query, "NAMING ai ");
        assert_eq!(state.shown_rows(), [1, 2]);
        state.handle_key(&press(KeyCode::Down));
        assert_eq!(state.selected, 2);
        state.handle_key(&press(KeyCode::Down));
        assert_eq!(state.selected, 2);
        state.handle_key(&press(KeyCode::Home));
        assert_eq!(state.selected, 1);
        // Actions are searched by name.
        state.query.clear();
        for c in "quit".chars() {
            state.handle_key(&press(KeyCode::Char(c)));
        }
        assert_eq!(state.selected_action(), Some(KeyAction::QuitAndCd));
        let page = page_text_for(&context, &state);
        assert!(page.contains("Keys"), "{page}");
        assert!(page.contains("Quit"), "{page}");
        // Nothing matches: Enter does nothing.
        state.handle_key(&press(KeyCode::Char('z')));
        state.handle_key(&press(KeyCode::Char('z')));
        assert!(state.shown_rows().is_empty());
        assert_eq!(
            state.handle_key(&press(KeyCode::Enter)),
            ModalAction::Handled
        );
        assert!(page_text_for(&context, &state).contains("Nothing matches “quitzz”."));
        // Backspace deletes; Esc clears, then closes.
        state.handle_key(&press(KeyCode::Backspace));
        assert_eq!(state.query, "quitz");
        assert_eq!(state.handle_key(&press(KeyCode::Esc)), ModalAction::Handled);
        assert_eq!(state.query, "");
        assert_eq!(state.shown_rows().len(), ROWS + KeyAction::ALL.len());
        assert_eq!(state.handle_key(&press(KeyCode::Esc)), ModalAction::Close);
    }

    #[test]
    fn the_search_field_tops_settings_and_a_greyed_row_stays_listed() {
        let mut context = ctx("Ctrl-b");
        context.general[1].disabled = true;
        let state = state(ModalTab::Settings);
        let text = buffer_text(&draw(80, 30, &state, &context));
        assert!(
            text.contains("⌕ █ Type to search settings and keys"),
            "{text}"
        );
        assert!(text.contains("Anthropic API key"), "{text}");
        let page = settings_lines(&context, 1, "", 60);
        let row = page
            .lines
            .iter()
            .find(|line| line_text(line).contains("Anthropic API key"))
            .unwrap();
        let label = row
            .spans
            .iter()
            .find(|span| span.content.contains("Anthropic"))
            .unwrap();
        assert_eq!(label.style.fg, Some(theme::palette().text_subtle));
        // What's typed shows, with Esc to clear it.
        let mut typed = state.clone();
        typed.handle_key(&press(KeyCode::Char('l')));
        let text = buffer_text(&draw(80, 30, &typed, &context));
        assert!(text.contains("⌕ l█"), "{text}");
        assert!(text.contains("Esc clear"), "{text}");
    }

    #[test]
    fn scrolling_before_and_after_render() {
        let mut state = state(ModalTab::Overview);
        state.handle_key(&press(KeyCode::Up));
        assert_eq!(state.scroll, 0);
        state.handle_key(&press(KeyCode::Down));
        state.handle_key(&press(KeyCode::Down));
        assert_eq!(state.scroll, 2);
        state.handle_key(&press(KeyCode::PageDown));
        assert_eq!(state.scroll, 2 + DEFAULT_PAGE);
        state.handle_key(&press(KeyCode::PageUp));
        assert_eq!(state.scroll, 2);
        state.handle_key(&press(KeyCode::Home));
        assert_eq!(state.scroll, 0);

        // After a render the range is known: End goes to the bottom and
        // Up moves from there at once.
        let context = ctx("Ctrl-b");
        draw(80, 24, &state, &context);
        let max = state.max_scroll.get();
        assert!(max > 0, "overview should overflow 80x24");
        state.handle_key(&press(KeyCode::End));
        assert_eq!(state.scroll, max);
        state.handle_key(&press(KeyCode::Down));
        assert_eq!(state.scroll, max);
        state.handle_key(&press(KeyCode::Up));
        assert_eq!(state.scroll, max - 1);
        let page = state.page.get();
        assert!(page > 1);
        state.handle_key(&press(KeyCode::Home));
        state.handle_key(&press(KeyCode::PageDown));
        assert_eq!(state.scroll, page.min(max));

        state.scroll_by(-1000);
        assert_eq!(state.scroll, 0);
        state.scroll_by(1000);
        assert_eq!(state.scroll, max);
        // The wheel scrolls a few lines per notch.
        state.wheel(-1);
        assert_eq!(state.scroll, max - 3);
    }

    #[test]
    fn modified_and_release_keys_are_left_to_the_caller() {
        for tab in [ModalTab::Settings, ModalTab::Overview] {
            let mut state = state(tab);
            let alt_down = KeyEvent::new(KeyCode::Down, KeyModifiers::ALT);
            assert_eq!(state.handle_key(&alt_down), ModalAction::Ignored);
            assert_eq!((state.scroll, state.selected), (0, 0));
            let ctrl_c = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
            assert_eq!(state.handle_key(&ctrl_c), ModalAction::Ignored);
            let release = KeyEvent {
                code: KeyCode::Esc,
                modifiers: KeyModifiers::NONE,
                kind: KeyEventKind::Release,
                state: KeyEventState::NONE,
            };
            assert_eq!(state.handle_key(&release), ModalAction::Ignored);
            // A key Help has no use for; Settings types it into its search.
            let plain = if tab == ModalTab::Settings {
                ModalAction::Handled
            } else {
                ModalAction::Ignored
            };
            assert_eq!(state.handle_key(&press(KeyCode::Char('x'))), plain);
        }
    }

    // --- rendering ------------------------------------------------------

    #[test]
    fn settings_hug_their_rows_instead_of_growing_with_the_terminal() {
        let context = ctx("Ctrl-\\");
        let narrow = settings_width(Rect::new(0, 0, 120, 40), &context);
        let wide = settings_width(Rect::new(0, 0, 240, 60), &context);
        assert_eq!(narrow, wide);
        assert!(usize::from(wide) <= MAX_SETTINGS_WIDTH);
        // Centred, and on a tall screen exactly as tall as the search field
        // (and a blank line), its rows and the keys.
        let state = state(ModalTab::Settings);
        let buf = draw(240, 120, &state, &context);
        let popup = dialog(&buf);
        assert_eq!(popup.width, wide);
        assert_eq!(popup.x, (240 - wide) / 2);
        // Two columns of padding either side, a row above and below.
        let body = usize::from(wide - 2 - BODY_MARGIN - 4);
        let rows = settings_lines(&context, 0, "", body).lines.len();
        assert_eq!(usize::from(popup.height), 2 + 2 + 2 + rows + 1);
        assert_eq!(popup.y, (120 - popup.height) / 2);
        let text = buffer_text(&buf);
        assert!(!text.contains('┃'), "everything fits: {text}");
        // Nothing needlessly cut.
        let page = page_text(&context, 0, body);
        assert!(page.contains("▶ Layout"), "{page}");
        assert!(page.contains("[Master detail] Grid Tabs"), "{page}");
        for action in KeyAction::ALL {
            assert!(
                page.contains(action.label()),
                "{} was needlessly truncated",
                action.label()
            );
        }
    }

    #[test]
    fn settings_stay_screen_bounded_and_long_values_cannot_make_them_screen_wide() {
        let mut context = ctx("Ctrl-\\");
        context.general[0].value = "long model or layout value ".repeat(100);
        for width in [0, 1, 2, 20, 40, 80, 160, 300] {
            let area = Rect::new(3, 2, width, 40);
            let popup = settings_width(area, &context);
            assert!(usize::from(popup) <= MAX_SETTINGS_WIDTH);
            if width > 2 {
                assert!(popup <= width - 2);
            }
        }
        for (width, height) in [(80, 24), (40, 12), (20, 6), (300, 6)] {
            let buf = draw(width, height, &state(ModalTab::Settings), &context);
            let popup = dialog(&buf);
            assert_eq!(popup.intersection(buf.area), popup);
            // 80% of the screen, or all of a tiny one.
            let most = (height * 4 / 5).max(height.min(8));
            assert!(popup.height <= most, "{width}x{height}");
        }
    }

    #[test]
    fn moving_the_selection_never_resizes_settings() {
        let mut context = ctx("Ctrl-\\");
        context.general[0].note =
            "A long explanation that should wrap rather than stretch the dialog. ".repeat(100);
        for (width, height) in [(160, 48), (160, 120), (80, 24)] {
            let mut state = state(ModalTab::Settings);
            let first = dialog(&draw(width, height, &state, &context));
            for row in [1, ROWS, row_of(KeyAction::TerminalPanel), ROWS + 30] {
                state.selected = row;
                assert_eq!(dialog(&draw(width, height, &state, &context)), first);
            }
        }
    }

    #[test]
    fn settings_render_at_80x24_with_the_configured_prefix() {
        let context = ctx("Ctrl-a");
        let mut state = state(ModalTab::Settings);
        state.selected = ROWS;
        let buf = draw(80, 24, &state, &context);
        let text = buffer_text(&buf);
        let popup = dialog(&buf);
        let top = text.lines().nth(usize::from(popup.y)).unwrap();
        assert!(top.contains("Settings"), "{top}");
        assert!(top.contains("Rakumux 0.6.0"), "{top}");
        assert!(text.contains("Ctrl-a"), "{text}");
        assert!(!text.contains("Ctrl-b"), "{text}");
        assert!(text.contains("Esc close"), "{text}");
        assert!(text.contains("Enter run"), "{text}");
        // No tab strip.
        assert!(
            !text.contains("1 Settings") && !text.contains("Keys tab"),
            "{text}"
        );
        assert!(popup.width < 80);
        assert!(popup.height <= 22);

        // Help: every tab fits, the long label shortened.
        let help = draw(80, 24, &self::state(ModalTab::Overview), &context);
        let text = buffer_text(&help);
        assert!(text.lines().next().unwrap().contains("Help"), "{text}");
        assert!(text.contains("What Rakumux is"), "{text}");
        let strip = text.lines().nth(1).unwrap();
        for label in [
            "1 Overview",
            "2 Sessions",
            "3 Layouts",
            "4 Agents & CLI",
            "5 License",
        ] {
            assert!(strip.contains(label), "{strip}");
        }
        assert!(!strip.contains("Settings"), "{strip}");
        assert!(!strip.contains('›') && !strip.contains('‹'), "{strip}");
    }

    #[test]
    fn renders_at_160x48_centred_with_the_configured_prefix() {
        let context = ModalContext {
            in_session: true,
            ..ctx("Ctrl-Space")
        };
        let mut state = state(ModalTab::Settings);
        state.selected = row_of(KeyAction::NextSession);
        let buf = draw(160, 48, &state, &context);
        let text = buffer_text(&buf);
        assert!(text.contains("Esc, Ctrl-Space"), "{text}");
        assert!(text.contains("↓, j, Option/Alt+↓"), "{text}");
        // Settings hugs its rows; Help retains its long-form reading width.
        let popup = dialog(&buf);
        assert_eq!(buf.get(0, 0).symbol(), " ");
        assert!(popup.width < 128);
        assert_eq!(popup.x, (160 - popup.width) / 2);

        let overview = draw(160, 48, &self::state(ModalTab::Overview), &context);
        assert_eq!(overview.get(16, 5).symbol(), "╭");
        assert_eq!(overview.get(143, 42).symbol(), "╯");
        let text = buffer_text(&overview);
        assert!(text.contains("4 Agents & CLI"), "{text}");
        assert!(text.contains("Two modes"), "{text}");
        let overview = tab_text(ModalTab::Overview, &context, 200);
        assert!(
            overview.contains("all of them except Ctrl-Space"),
            "{overview}"
        );
        assert!(overview.contains("Ctrl-Space n"), "{overview}");
    }

    #[test]
    fn settings_show_each_setting_and_the_selected_note_below() {
        let context = ctx("Ctrl-b");
        let state = state(ModalTab::Settings).with_selected(1);
        let buf = draw(80, 24, &state, &context);
        let text = buffer_text(&buf);
        // Labels on the left, values pinned to the right.
        let row = |label: &str| -> String {
            let line = text.lines().find(|l| l.contains(label)).unwrap();
            line.trim_matches(|c| c == ' ' || c == '│' || c == '┃')
                .to_string()
        };
        for (label, value) in [
            ("  Layout", "[Master detail] Grid Tabs"),
            ("▶ Anthropic API key", "not set"),
            ("  OpenAI model", "gpt-4o-mini"),
        ] {
            let shown = row(label.trim_start());
            assert!(shown.ends_with(value), "{label}: {text}");
            let gap = shown.len() - shown.trim_start_matches(|c: char| c != ' ').len();
            assert!(gap > 0, "{label}: {text}");
        }
        let right = |label| {
            let line = text.lines().find(|l: &&str| l.contains(label)).unwrap();
            line.trim_end_matches(|c| c == ' ' || c == '│' || c == '┃')
                .chars()
                .count()
        };
        assert_eq!(right("Layout"), right("OpenAI model"), "{text}");
        assert_eq!(right("Layout"), right("Anthropic API key"), "{text}");
        // Notes wait for `→`: the rows follow one another.
        assert!(!text.contains("Used for AI session naming"), "{text}");
        assert!(!text.contains("List on the left."), "{text}");
        assert!(text.contains("Enter change"), "{text}");
        assert!(text.contains("→ about"), "{text}");
        let lines: Vec<&str> = text.lines().collect();
        let popup = dialog(&buf);
        let y = lines.iter().position(|l| l.contains('▶')).unwrap();
        assert!(lines[y + 1].contains("  OpenAI model"), "{text}");
        assert!(page_text(&context, 1, 72).contains("~/.rakumux/config.json"));
        // The selected row is white, with no background of its own.
        let p = theme::palette();
        let (pad_x, _) = crate::components::dialogs::padding(buf.area);
        let (label_x, y) = (popup.x + 4 + pad_x, y as u16);
        assert_eq!(buf.get(label_x, y).fg, p.text_bright);
        assert_eq!(buf.get(label_x, y).bg, p.background);
        assert_eq!(buf.get(label_x, y + 1).fg, p.text_primary);
    }

    #[test]
    fn settings_rows_stay_in_view_and_inside_the_width() {
        let context = ctx("Ctrl-b");
        for width in [1, 5, 12, 30, 76, 125] {
            for selected in 0..ROWS + KeyAction::ALL.len() {
                let page = settings_lines(&context, selected, "", width);
                let (lines, (start, end)) = (&page.lines, page.selected);
                assert!(start <= end && end < lines.len());
                if width > 1 {
                    // The marked row is in the span: the label's line, or
                    // the value's under it.
                    let marked = (start..=end).any(|l| line_text(&lines[l]).starts_with('▶'));
                    assert!(marked, "{width} {selected}");
                }
                for line in lines {
                    assert!(line.width() <= width, "{width}: {:?}", line_text(line));
                }
            }
        }
        // Too narrow for labels and values side by side (a phone): each
        // value goes under its label, whole.
        let page = settings_lines(&context, 0, "", 30);
        let text: Vec<String> = page.lines.iter().map(line_text).collect();
        let layout = text.iter().position(|l| l == "▶ Layout").unwrap();
        assert_eq!(
            text[layout + 1],
            "    [Master detail] Grid Tabs",
            "{text:?}"
        );
        // From its heading to its value.
        assert_eq!(page.selected, (layout - 1, layout + 1));
        assert!(text.contains(&"    not set".to_string()), "{text:?}");
        // Wide enough: side by side.
        let lines = settings_lines(&context, 0, "", 76).lines;
        let layout = line_text(&lines[1]);
        assert!(layout.starts_with("▶ Layout  "), "{layout:?}");
        assert!(
            layout.ends_with("  [Master detail] Grid Tabs"),
            "{layout:?}"
        );
        assert_eq!(lines[1].width(), 76);
        // A short screen scrolls to the selected row.
        let state = state(ModalTab::Settings).with_selected(ROWS - 1);
        let buf = draw(80, 9, &state, &context);
        assert!(buffer_text(&buf).contains("▶ OpenAI model"));
        let mut state = state.with_selected(999);
        let buf = draw(80, 12, &state, &context);
        assert!(buffer_text(&buf).contains("▶ Quit Rakumux"));
        // The first action shows with the rule and heading above it.
        state.selected = ROWS;
        let text = buffer_text(&draw(80, 14, &state, &context));
        assert!(text.contains("── Keys"), "{text}");
        assert!(text.contains("Modes"), "{text}");
    }

    #[test]
    fn the_page_scrolls_only_as_far_as_the_selection_needs() {
        let context = ctx("Ctrl-b");
        let mut state = state(ModalTab::Settings);
        let mut last_top = 0;
        for _ in 0..ROWS + KeyAction::ALL.len() {
            draw(80, 20, &state, &context);
            let top = state.top.get();
            assert!(top >= last_top, "moving down never scrolls up");
            last_top = top;
            state.handle_key(&press(KeyCode::Down));
        }
        assert!(last_top > 0);
        // Back up a few rows: the page stays put until the selection
        // reaches its top.
        for _ in 0..3 {
            state.handle_key(&press(KeyCode::Up));
            draw(80, 20, &state, &context);
            assert_eq!(state.top.get(), last_top);
        }
        // A render records the page for PgUp/PgDn.
        assert!(state.page.get() > 1 && state.page.get() < DEFAULT_PAGE + 10);
    }

    #[test]
    fn every_tab_renders_at_small_and_odd_sizes() {
        let context = ctx("Ctrl-b");
        for (w, h) in [
            (80, 24),
            (160, 48),
            (100, 30),
            (40, 12),
            (12, 5),
            (3, 3),
            (1, 1),
        ] {
            for tab in ModalTab::ALL {
                let state = state(tab).with_selected(999);
                let buf = draw(w, h, &state, &context);
                if w >= 40 {
                    let text = buffer_text(&buf);
                    assert!(text.contains(tab.short_label()), "{w}x{h} {tab:?}");
                }
            }
        }
    }

    #[test]
    fn scrolled_render_shows_the_end_and_position() {
        let context = ctx("Ctrl-b");
        let mut state = state(ModalTab::License);
        state.scroll = u16::MAX;
        let buf = draw(80, 24, &state, &context);
        let text = buffer_text(&buf);
        assert!(text.contains("under the License."), "{text}");
        let total = body_lines(
            ModalTab::License,
            &context,
            usize::from(80 - 2 - BODY_MARGIN),
        )
        .len();
        assert!(text.contains(&format!("/{total}")), "{text}");
        // A quiet thumb and track, not the accent.
        let p = theme::palette();
        let (x, thumb) = (80 - 2, buf.area.y..buf.area.bottom());
        let cells: Vec<_> = thumb.map(|y| buf.get(x, y).clone()).collect();
        let thumb = cells.iter().find(|c| c.symbol() == "┃").expect("thumb");
        assert_eq!(thumb.fg, p.text_muted);
        assert!(cells
            .iter()
            .any(|c| c.symbol() == "│" && c.fg == p.border_dim));
        assert!(!text.contains('█'), "{text}");
    }

    #[test]
    fn narrow_footer_shortens_the_hint_instead_of_overlapping() {
        let context = ctx("Ctrl-b");
        let mut state = state(ModalTab::License);
        state.scroll = 30;
        let buf = draw(60, 20, &state, &context);
        let footer: String = (0..60)
            .map(|x| buf.get(x, 18).symbol().to_string())
            .collect();
        assert!(footer.contains("Esc close"), "{footer:?}");
        assert!(!footer.contains("PgUp"), "{footer:?}");
        assert!(footer.contains(" 31-"), "{footer:?}");
        // A phone: fewer keys, Esc close still whole.
        for tab in [ModalTab::Overview, ModalTab::Settings] {
            let buf = draw(40, 20, &self::state(tab), &context);
            let popup = dialog(&buf);
            // Settings has a row of padding under its footer.
            let below = if tab == ModalTab::Settings {
                crate::components::dialogs::padding(buf.area).1
            } else {
                0
            };
            let footer: String = (0..40)
                .map(|x| buf.get(x, popup.bottom() - 2 - below).symbol().to_string())
                .collect();
            assert!(footer.contains("Esc close"), "{tab:?} {footer:?}");
        }
    }

    // --- content --------------------------------------------------------

    #[test]
    fn lines_never_exceed_the_width() {
        let long_prefix = ctx("Ctrl-Shift-Alt-Space");
        for width in [20, 37, 60, 75, 125] {
            for context in [ctx("Ctrl-b"), long_prefix.clone()] {
                for tab in ModalTab::ALL {
                    for line in body_lines(tab, &context, width) {
                        assert!(
                            line.width() <= width,
                            "{tab:?} at {width}: {:?}",
                            line_text(&line)
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn actions_are_a_table_of_actions_and_their_configured_keys() {
        let mut context = ctx("C-a");
        context.cycle_previous = "Alt-k".into();
        context.cycle_next = "Alt-j".into();
        let text = page_text(&context, ROWS, 120);
        let lines: Vec<&str> = text.lines().collect();
        let row = |action: &str| {
            lines
                .iter()
                .find(|l| l.chars().skip(2).collect::<String>().starts_with(action))
                .unwrap_or_else(|| panic!("no {action} row: {text}"))
        };
        for (action, keys) in [
            ("Rakumux mode", "C-a"),
            ("Back to the session you came from", "Esc, C-a"),
            ("Go into the selected session", "Enter, i, l"),
            ("Feed (show, focus or hide)", "f"),
            ("Next tab (Status, Folders, App)", "→"),
            ("Previous tab", "←"),
            ("Last session", "Tab"),
            ("Go into session 1 to 9", "1-9"),
            ("Next session", "↓, j, Alt-j"),
            ("Previous session", "↑, k, Alt-k"),
            ("Top of the list", "gg, Home"),
            ("Bottom of the list", "G, End"),
            ("Half a page down", "Ctrl-d"),
            ("Half a page up", "Ctrl-u"),
            ("Next folder", "}"),
            ("Previous folder", "{"),
            ("Scroll back (Grid: previous page)", "Ctrl-y, PgUp"),
            ("Scroll forward (Grid: next page)", "Ctrl-e, PgDn"),
            ("New session", "n"),
            ("Filter by folder", "P"),
            ("Rename session", "r"),
            ("Pin or unpin session", "p"),
            ("Move session down", "J"),
            ("Move session up", "K"),
            ("Close session", "x"),
            ("Delete session", "d, dd"),
            ("Work in the session full screen", "z"),
            ("Filter sessions", "/"),
            ("Cycle layout (Master detail, Grid, Tabs)", "v"),
            ("Help", "?"),
            ("Settings", "comma (,), o"),
            ("Update Rakumux (sessions keep running)", "U"),
            ("Reconnect or reload the list", "Ctrl-r"),
            ("Restart Rakumux", "R"),
            ("Quit and cd", "c"),
            ("Quit Rakumux", "q"),
            ("Send the Rakumux key", "\\"),
        ] {
            assert!(row(action).trim_end().ends_with(keys), "{action}: {text}");
        }
        // The keys are pinned to the right: every row ends at the edge.
        for action in ["Half a page down", "Next folder", "Rakumux mode"] {
            assert_eq!(row(action).chars().count(), 120, "{action}: {text}");
        }
        assert!(!text.contains("Ctrl-b"), "{text}");
        // The first action is selected, its note under it.
        assert!(row("Rakumux mode").starts_with("▶ "), "{text}");
        assert!(note_text(&context, ROWS).contains("the one key a session's program never gets"),);
        // An action with nothing more to say has no note; the rule says
        // how to run any of them.
        assert_eq!(note_text(&context, row_of(KeyAction::Pin)), "");
        let how = lines
            .iter()
            .position(|l| l.contains("Enter runs the selected action"))
            .unwrap();
        assert!(text.contains("press C-a first"), "{text}");
        // The settings come first, then the actions under a rule, each
        // under its heading, in this order.
        let at = |heading: &str| {
            lines
                .iter()
                .position(|l| l.trim() == heading)
                .unwrap_or_else(|| panic!("no {heading} heading: {text}"))
        };
        assert_eq!(lines[0], "Appearance");
        let rule = lines
            .iter()
            .position(|l| l.starts_with("── Keys ─"))
            .unwrap();
        let headings = [
            at("AI session naming"),
            rule,
            at("Modes"),
            at("Moving around"),
            at("Sessions and folders"),
            at("Help and app"),
        ];
        assert!(headings.windows(2).all(|w| w[0] < w[1]), "{text}");
        assert!(rule < how && how < at("Modes"), "{text}");
    }

    #[test]
    fn right_shows_the_rows_note_in_a_popover_without_moving_the_page() {
        let context = ctx("Ctrl-b");
        let mut state = state(ModalTab::Settings);
        state.selected = row_of(KeyAction::NextSession);
        let closed = draw(100, 44, &state, &context);
        let closed_text = buffer_text(&closed);
        assert!(
            !closed_text.contains("Use Option as Meta key"),
            "{closed_text}"
        );
        assert!(closed_text.contains("→ about"), "{closed_text}");
        state.handle_key(&press(KeyCode::Right));
        assert!(state.note_open);
        let open = draw(100, 44, &state, &context);
        let open_text = buffer_text(&open);
        // Wrapping may split the phrase over two lines.
        let words = open_text.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(words.contains("Use Option as"), "{open_text}");
        assert!(open_text.contains("← back"), "{open_text}");
        // Titled with the row, just below it; the page stays put.
        let lines: Vec<&str> = open_text.lines().collect();
        let row = lines
            .iter()
            .position(|l| l.contains("▶ Next session"))
            .unwrap();
        assert!(lines[row + 1].contains("╭ Next session ─"), "{open_text}");
        assert_eq!(dialog(&open), dialog(&closed));
        let before: Vec<&str> = closed_text.lines().take(row + 1).collect();
        assert_eq!(lines[..=row].to_vec(), before);
        // ← and Esc close it (Esc then closes Settings), and so does moving.
        state.handle_key(&press(KeyCode::Left));
        assert!(!state.note_open);
        state.handle_key(&press(KeyCode::Right));
        assert_eq!(state.handle_key(&press(KeyCode::Esc)), ModalAction::Handled);
        assert!(!state.note_open);
        assert_eq!(state.handle_key(&press(KeyCode::Esc)), ModalAction::Close);
        state.handle_key(&press(KeyCode::Right));
        state.handle_key(&press(KeyCode::Down));
        assert!(!state.note_open);
        state.note_open = true;
        state.wheel(1);
        assert!(!state.note_open);
        // A row with nothing more to say offers no note.
        assert_eq!(note_text(&context, 0), "List on the left.");
        assert_eq!(note_text(&context, row_of(KeyAction::Pin)), "");
        state.select_action(KeyAction::Pin);
        state.note_open = true;
        let text = buffer_text(&draw(100, 40, &state, &context));
        assert!(
            !text.contains("→ about") && !text.contains("← back"),
            "{text}"
        );
        // A long note near the bottom of a short screen goes above the row
        // and stays inside the page.
        state.select_action(KeyAction::TerminalPanel);
        state.note_open = true;
        for (width, height) in [(60, 16), (80, 24), (40, 12)] {
            let buf = draw(width, height, &state, &context);
            let page = dialog(&buf);
            let text = buffer_text(&buf);
            let lines: Vec<&str> = text.lines().collect();
            if let Some(top) = lines.iter().position(|l| l.contains("╭ Terminal panel")) {
                let bottom = lines.iter().rposition(|l| l.contains('╰')).unwrap();
                assert!(top > usize::from(page.y), "{text}");
                assert!(bottom <= usize::from(page.bottom()) - 1, "{text}");
            }
        }
    }

    #[test]
    fn disabled_cycle_keys_point_to_the_prefix_fallback() {
        let mut context = ctx("Ctrl-b");
        context.cycle_keys = "  ".into();
        context.cycle_previous = String::new();
        context.cycle_next = String::new();
        let overview = tab_text(ModalTab::Overview, &context, 100);
        assert!(overview.contains("turned off"), "{overview}");
        let next = row_of(KeyAction::NextSession);
        assert!(note_text(&context, next).contains("turned off"));
        let keys = page_text(&context, next, 100);
        assert!(
            keys.lines()
                .any(|l| l.contains("Next session") && l.trim_end().ends_with("   ↓, j")),
            "{keys}"
        );
        let layouts = tab_text(ModalTab::Layouts, &context, 100);
        assert!(layouts.contains("Turned off"), "{layouts}");
        assert!(!layouts.contains("Option/Alt+"), "{layouts}");
    }

    #[test]
    fn backtick_prefix_prints_literally() {
        let context = ctx("Ctrl-`");
        let text = tab_text(ModalTab::Overview, &context, 100);
        assert!(text.contains("Ctrl-` ?"), "{text}");
        assert!(!text.contains(LITERAL_BACKTICK), "{text}");
        let lines = body_lines(ModalTab::Overview, &context, 100);
        let key_style = Styles::from_theme().key;
        let styled = lines
            .iter()
            .flat_map(|l| l.spans.iter())
            .any(|s| s.content.as_ref() == "Ctrl-` ?" && s.style == key_style);
        assert!(styled, "prefix mention should be one key-styled span");
    }

    #[test]
    fn attach_help_names_its_fixed_prefix_whatever_the_rakumux_key() {
        // `rakumux attach` always uses Ctrl-\ (cli/attach.rs), even with another Rakumux key.
        let text = tab_text(ModalTab::AgentsCli, &ctx("Ctrl-a"), 200);
        assert!(text.contains("Ctrl-\\ then d detaches"), "{text}");
        assert!(!text.contains("Ctrl-b then d"), "{text}");
    }

    #[test]
    fn layout_tab_and_overview_follow_the_current_layout() {
        let mut context = ctx("Ctrl-b");
        context.layout = SessionLayout::Tabs;
        assert!(tab_text(ModalTab::Layouts, &context, 100).contains("Current layout: Tabs."));
        assert!(tab_text(ModalTab::Overview, &context, 100).contains("Tab bar, along the top"));
        context.layout = SessionLayout::Sidebar;
        assert!(
            tab_text(ModalTab::Layouts, &context, 100).contains("Current layout: Master detail.")
        );
        assert!(tab_text(ModalTab::Overview, &context, 100).contains("Session list, on the left"));
    }

    #[test]
    fn content_covers_the_whole_flow() {
        let context = ctx("Ctrl-b");
        let all: String = ModalTab::ALL
            .iter()
            .map(|tab| tab_text(*tab, &context, 200))
            .chain([page_text(&context, 0, 200)])
            .collect::<Vec<_>>()
            .join("\n");
        // Wrapping may split a phrase over two lines.
        let all = all.split_whitespace().collect::<Vec<_>>().join(" ");
        for topic in [
            "folder",
            "All folders",
            "Pinning",
            "★",
            "using a session never moves it",
            "sessions.session_order",
            "Drag a session up or down the list",
            "Drag a tab left or right to move it",
            "Git branch",
            "it never runs git",
            "isn't saved",
            "Folders (Enter on the folder filter, or P)",
            "without a naming dialog",
            "rakumux new --cwd DIR --cmd C -- ARGV",
            "rakumux folders",
            "--folder DIR",
            "rakumux pin <name|id>",
            "--dead",
            "observer",
            "controller",
            "Exited",
            "Dead",
            "revive",
            "rakumux-ptyd",
            "many windows",
            "Master detail",
            "Grid",
            "Tabs",
            "rakumux attach <name|id>",
            "rakumux daemon start|status|stop|restart|logs",
            "rakumux --new-server",
            "rakumux session report",
            "RAKUMUX_SESSION_ID",
            "AI naming",
            "Zoom",
            "Ctrl-r",
            "While filtering",
            "Moving around",
            "Mouse",
            "In Help and Settings",
            "Click a session in the list",
            "Drag the line between the list and the session",
            "Apache-2.0",
            "Grant of Patent License",
            "https://github.com/humanunsupervised/rakumux",
        ] {
            assert!(
                all.contains(topic),
                "Help and Settings never mention {topic:?}"
            );
        }
    }

    #[test]
    fn no_tmux_git_remote_or_merge_content() {
        // Git appears only as read-only context (branch, worktree); the
        // product has no git commands, tmux or git remotes ("remote" is
        // only `rakumux remote`, phone access).
        let context = ctx("Ctrl-b");
        for tab in ModalTab::ALL {
            let text = tab_text(tab, &context, 200).to_lowercase();
            for word in text.split(|c: char| !c.is_alphanumeric()) {
                assert!(
                    !matches!(word, "tmux" | "merge" | "remotes"),
                    "{tab:?} mentions {word:?}"
                );
            }
            assert!(!text.contains("git remote"), "{tab:?} mentions git remotes");
            for (i, _) in text.match_indices("remote") {
                assert!(
                    text[..i].ends_with("rakumux "),
                    "{tab:?} mentions remote outside `rakumux remote`"
                );
            }
        }
    }

    #[test]
    fn wrap_splits_overlong_words_and_indents() {
        let style = Style::default();
        let words = plain_words("aaaaaaaaaaaa bb", style);
        let lines = wrap(&words, 5, vec![Span::raw("- ")], 2);
        let texts: Vec<String> = lines.iter().map(line_text).collect();
        assert_eq!(texts, vec!["- aaa", "  aaa", "  aaa", "  aaa", "  bb"]);
        assert!(lines.iter().all(|l| l.width() <= 5));
        // Empty text still yields one (prefix-only) line.
        let empty = wrap(&[], 10, vec![Span::raw("x")], 0);
        assert_eq!(empty.len(), 1);
    }

    #[test]
    fn markup_keeps_code_spans_together() {
        let base = Style::default();
        let code = Style::default().add_modifier(Modifier::BOLD);
        let parsed = words("press `Ctrl-b l`, then go", base, code);
        assert_eq!(parsed.len(), 4);
        assert_eq!(parsed[1].segments[0], ("Ctrl-b l".to_string(), code));
        assert_eq!(parsed[1].segments[1], (",".to_string(), base));
        assert_eq!(parsed[1].width, 9);
    }
}
