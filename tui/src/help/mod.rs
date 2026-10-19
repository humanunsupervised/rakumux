//! The Settings and Help modals, which share their frame, tabs and keys.
//!
//! - **Settings** (`,` or `o` in Rakumux mode): the General
//!   tab lists the settings (layout, AI naming provider, API keys) for the
//!   caller to change; the Keys tab is a table of actions and their keys,
//!   built from the user's configured keys, where `Enter` runs the selected
//!   action.
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

/// The modal never shrinks below this size unless the screen does.
const MIN_MODAL_WIDTH: u16 = 80;
const MIN_MODAL_HEIGHT: u16 = 24;

/// Body columns not used for text: one of padding on the left, two of gap
/// and one of scrollbar on the right.
const BODY_MARGIN: u16 = 4;

/// Lines moved by PgUp/PgDn before the first render tells us the body height.
const DEFAULT_PAGE: u16 = 10;

/// General tab: how far a value sits in when it goes on the line under its
/// label (a narrow screen).
const VALUE_INDENT: usize = 4;

/// Stands in for a literal backtick inside help markup, where backticks
/// delimit key names, so a configured key containing a backtick still
/// prints correctly.
const LITERAL_BACKTICK: char = '\u{E000}';

/// The two modals.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Modal {
    /// What you can change and do: General, Keys.
    Settings,
    /// How Rakumux works: Overview, Sessions, Layouts, Agents & CLI,
    /// License.
    Help,
}

impl Modal {
    /// The modal's tabs, in order.
    pub fn tabs(self) -> &'static [ModalTab] {
        match self {
            Modal::Settings => &[ModalTab::General, ModalTab::Keys],
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum ModalTab {
    /// The settings themselves.
    #[default]
    General,
    Keys,
    Overview,
    Sessions,
    Layouts,
    AgentsCli,
    License,
}

impl ModalTab {
    /// Every tab of both modals.
    #[cfg(test)]
    pub const ALL: [ModalTab; 7] = [
        ModalTab::General,
        ModalTab::Keys,
        ModalTab::Overview,
        ModalTab::Sessions,
        ModalTab::Layouts,
        ModalTab::AgentsCli,
        ModalTab::License,
    ];

    pub fn label(self) -> &'static str {
        match self {
            ModalTab::General => "General",
            ModalTab::Keys => "Keys",
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
            ModalTab::General | ModalTab::Keys => Modal::Settings,
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

/// An action on the Settings modal's Keys tab: a row of the table, run with
/// `Enter` there or with its keys in Rakumux mode.
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
    /// `J`: one place down in the list (dragging does the same).
    MoveDown,
    /// `K`: one place up.
    MoveUp,
    Close,
    Delete,
    Zoom,
    Filter,
    Help,
    Settings,
    /// The welcome tour carousel.
    Tour,
    CheckForUpdate,
    Reload,
    /// Reload only this UI, leaving the daemon and sessions running.
    Restart,
    QuitAndCd,
    Quit,
    /// Into the session and send it the Rakumux key itself.
    SendRakumuxKey,
}

impl KeyAction {
    /// The rows of the Keys tab, in order.
    pub const ALL: [KeyAction; 34] = [
        KeyAction::RakumuxMode,
        KeyAction::BackToSession,
        KeyAction::GoIn,
        KeyAction::LastSession,
        KeyAction::NumberedSession,
        KeyAction::SendRakumuxKey,
        KeyAction::NextSession,
        KeyAction::PreviousSession,
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
        KeyAction::MoveDown,
        KeyAction::MoveUp,
        KeyAction::Close,
        KeyAction::Delete,
        KeyAction::Zoom,
        KeyAction::Filter,
        KeyAction::Help,
        KeyAction::Settings,
        KeyAction::Tour,
        KeyAction::CheckForUpdate,
        KeyAction::Reload,
        KeyAction::Restart,
        KeyAction::QuitAndCd,
        KeyAction::Quit,
    ];

    /// The Keys tab heading it's listed under.
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
            | KeyAction::MoveDown
            | KeyAction::MoveUp
            | KeyAction::Close
            | KeyAction::Delete
            | KeyAction::Zoom
            | KeyAction::Filter => "Sessions and folders",
            KeyAction::Help
            | KeyAction::Settings
            | KeyAction::Tour
            | KeyAction::CheckForUpdate
            | KeyAction::Reload
            | KeyAction::Restart
            | KeyAction::QuitAndCd
            | KeyAction::Quit => "Help and app",
        }
    }

    /// The Action column.
    pub fn label(self) -> &'static str {
        match self {
            KeyAction::RakumuxMode => "Rakumux mode (from a session)",
            KeyAction::BackToSession => "Back to the session you came from",
            KeyAction::GoIn => "Go into the selected session",
            KeyAction::LastSession => "Last session",
            KeyAction::NumberedSession => "Go into session 1 to 9",
            KeyAction::NextSession => "Next session",
            KeyAction::PreviousSession => "Previous session",
            KeyAction::Top => "Top of the list",
            KeyAction::Bottom => "Bottom of the list",
            KeyAction::HalfPageDown => "Half a page down",
            KeyAction::HalfPageUp => "Half a page up",
            KeyAction::NextFolder => "Next folder",
            KeyAction::PreviousFolder => "Previous folder",
            KeyAction::ScrollBack => "Scroll the session back",
            KeyAction::ScrollForward => "Scroll the session forward",
            KeyAction::NewSession => "New session",
            KeyAction::FilterFolders => "Filter by folder",
            KeyAction::Rename => "Rename session",
            KeyAction::Pin => "Pin or unpin session",
            KeyAction::MoveDown => "Move session down",
            KeyAction::MoveUp => "Move session up",
            KeyAction::Close => "Close session (end its program)",
            KeyAction::Delete => "Delete session",
            KeyAction::Zoom => "Work in the session full screen",
            KeyAction::Filter => "Filter sessions",
            KeyAction::Help => "Help",
            KeyAction::Settings => "Settings",
            KeyAction::Tour => "Welcome tour",
            KeyAction::CheckForUpdate => "Check for updates",
            KeyAction::Reload => "Reconnect or reload the list",
            KeyAction::Restart => "Restart Rakumux (sessions keep running)",
            KeyAction::QuitAndCd => "Quit and cd to the session's folder",
            KeyAction::Quit => "Quit Rakumux (sessions keep running)",
            KeyAction::SendRakumuxKey => "Send the Rakumux key to the session",
        }
    }

    /// The Key column: the Rakumux key for leaving a session, Rakumux
    /// mode's keys for the rest, from the configured keys.
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
            KeyAction::GoIn => plain(&["Enter", "i", "l", "→"]),
            KeyAction::LastSession => plain(&["Tab"]),
            KeyAction::NumberedSession => plain(&["1-9"]),
            KeyAction::NextSession => with_cycle(&k.cycle_next, &["↓", "j"]),
            KeyAction::PreviousSession => with_cycle(&k.cycle_previous, &["↑", "k"]),
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
            KeyAction::MoveDown => plain(&["J"]),
            KeyAction::MoveUp => plain(&["K"]),
            KeyAction::Close => plain(&["x"]),
            KeyAction::Delete => plain(&["d", "dd"]),
            KeyAction::Zoom => plain(&["z"]),
            KeyAction::Filter => plain(&["/"]),
            KeyAction::Help => plain(&["?"]),
            KeyAction::Settings => format!("comma (,), {SETTINGS_KEY}"),
            KeyAction::Tour => "t (in Help)".into(),
            KeyAction::CheckForUpdate => plain(&["u"]),
            KeyAction::Reload => plain(&["Ctrl-r"]),
            KeyAction::Restart => plain(&["R"]),
            KeyAction::QuitAndCd => plain(&["c"]),
            KeyAction::Quit => plain(&["q"]),
            KeyAction::SendRakumuxKey => plain(&["\\"]),
        }
    }

    /// Shown under the table while this row is selected, when there's
    /// more to say.
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
                 folder filter it opens the Folders modal; on a folder's header or + New \
                 session, the new-session dialog."
                .into(),
            KeyAction::LastSession => {
                "Into the session you worked in before the current one.".into()
            }
            KeyAction::NextSession | KeyAction::PreviousSession => match &k.cycle {
                Some(cycle) if k.cycle_in_session => format!(
                    "`{cycle}` also works inside a session (Settings, General), so programs \
                     there never get it. If it does nothing, your terminal isn't sending \
                     Option/Alt: in Terminal.app turn on \"Use Option as Meta key\"."
                ),
                Some(cycle) => format!(
                    "`{cycle}` works in Rakumux mode; turn on \"Option+↑/↓ in sessions\" \
                     (General) to use it inside a session too. If it does nothing, your \
                     terminal isn't sending Option/Alt: in Terminal.app turn on \"Use Option as \
                     Meta key\"."
                ),
                None => "Option/Alt+↑/↓ are turned off in your settings; ↑/↓ and j/k move.".into(),
            },
            KeyAction::Top => "The folder filter is the first row.".into(),
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
            KeyAction::Restart => {
                "Asks before reloading this UI through its launcher to pick up the latest build. \
                 Confirm with y or Enter; Esc cancels. The daemon and sessions stay up. \
                 Ctrl-r only reconnects or reloads the list."
                    .into()
            }
            KeyAction::Zoom => {
                format!("The list or tab bar hides until you press `{p}`.")
            }
            KeyAction::SendRakumuxKey => {
                format!("Goes back into the session and sends it `{p}`, for programs that use it.")
            }
            KeyAction::QuitAndCd => {
                "Needs the shell wrapper described at the top of rakumux.sh.".into()
            }
            _ => String::new(),
        }
    }
}

/// Open-modal state: the tab shown (which says the modal), how far its
/// body is scrolled, the setting selected on the General tab and the action
/// selected on the Keys tab.
#[derive(Debug, Clone)]
pub struct ModalState {
    pub tab: ModalTab,
    /// Lines scrolled from the top of the tab's body.
    pub scroll: u16,
    /// Row selected on the General tab.
    pub selected: usize,
    /// Row selected on the Keys tab (into [`KeyAction::ALL`]).
    pub action: usize,
    /// Rows on the General tab.
    general_rows: usize,
    /// Largest useful `scroll` for the current tab, recorded by `render`.
    max_scroll: Cell<u16>,
    /// Body height in lines, recorded by `render` (PgUp/PgDn step).
    page: Cell<u16>,
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
    /// `Enter` or `Space` on the General tab: change the setting in this
    /// row.
    Change(usize),
    /// `Enter` or `Space` on the Keys tab: close the modal and run this.
    Run(KeyAction),
    /// `t` in Help: show the welcome tour.
    Tour,
}

impl ModalState {
    /// Settings opened on `tab`, with `general_rows` settings on the
    /// General tab (the first one selected).
    pub fn new(tab: ModalTab, general_rows: usize) -> Self {
        Self {
            tab,
            scroll: 0,
            selected: 0,
            action: 0,
            general_rows,
            max_scroll: Cell::new(u16::MAX),
            page: Cell::new(DEFAULT_PAGE),
        }
    }

    /// Select General row `row` (clamped to the rows there are).
    pub fn with_selected(mut self, row: usize) -> Self {
        self.selected = row.min(self.general_rows.saturating_sub(1));
        self
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

    /// Scroll the body by `delta` lines (positive = down), e.g. for the
    /// mouse wheel.
    pub fn scroll_by(&mut self, delta: i32) {
        let max = i32::from(self.max_scroll.get());
        let current = i32::from(self.scroll).min(max);
        self.scroll = (current + delta).clamp(0, max) as u16;
    }

    /// `Esc`/`q` close, and `?` (Help) or `o` (Settings); `Tab`/`→` and
    /// `Shift-Tab`/`←` switch tabs; number keys jump to a tab. On the
    /// General and Keys tabs `↑`/`↓` (`j`/`k`, `Home`/`End`) select a row
    /// and `Enter`/`Space` change the setting or run the action; on the
    /// others `↑`/`↓`, `PgUp`/`PgDn`, `Home`/`End` scroll (`j`/`k`/`g`/`G`
    /// too). Keys with Ctrl or Alt held are ignored.
    pub fn handle_key(&mut self, key: &KeyEvent) -> ModalAction {
        if key.kind == KeyEventKind::Release {
            return ModalAction::Ignored;
        }
        if !(key.modifiers - KeyModifiers::SHIFT).is_empty() {
            return ModalAction::Ignored;
        }
        if self.tab == ModalTab::General {
            let last = self.general_rows.saturating_sub(1);
            match key.code {
                KeyCode::Up | KeyCode::Char('k') => self.selected = self.selected.saturating_sub(1),
                KeyCode::Down | KeyCode::Char('j') => self.selected = (self.selected + 1).min(last),
                KeyCode::Home | KeyCode::Char('g') => self.selected = 0,
                KeyCode::End | KeyCode::Char('G') => self.selected = last,
                KeyCode::Enter | KeyCode::Char(' ') if self.general_rows > 0 => {
                    return ModalAction::Change(self.selected.min(last))
                }
                _ => return self.handle_common_key(key),
            }
            return ModalAction::Handled;
        }
        if self.tab == ModalTab::Keys {
            let last = KeyAction::ALL.len() - 1;
            match key.code {
                KeyCode::Up | KeyCode::Char('k') => self.action = self.action.saturating_sub(1),
                KeyCode::Down | KeyCode::Char('j') => self.action = (self.action + 1).min(last),
                KeyCode::Home | KeyCode::Char('g') => self.action = 0,
                KeyCode::End | KeyCode::Char('G') => self.action = last,
                KeyCode::Enter | KeyCode::Char(' ') => {
                    return ModalAction::Run(KeyAction::ALL[self.action.min(last)])
                }
                _ => return self.handle_common_key(key),
            }
            return ModalAction::Handled;
        }
        let page = i32::from(self.page.get().max(1));
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

    /// Keys that work on every tab: closing and switching tabs.
    fn handle_common_key(&mut self, key: &KeyEvent) -> ModalAction {
        let own_keys: &[char] = match self.tab.modal() {
            Modal::Help => &['?'],
            Modal::Settings => &[',', 'o'],
        };
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => return ModalAction::Close,
            KeyCode::Char(c) if own_keys.contains(&c) => return ModalAction::Close,
            KeyCode::Char('t') if self.tab.modal() == Modal::Help => return ModalAction::Tour,
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
/// the configured keys and the General tab the current settings.
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
    /// The General tab's rows, in order.
    pub general: Vec<SettingRow>,
}

/// One setting on the General tab.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingRow {
    /// The heading it's listed under, e.g. `Appearance`.
    pub section: &'static str,
    /// What it sets, e.g. `Layout`.
    pub label: String,
    /// Its current value, e.g. `[Sidebar] Tabs` or `not set`.
    pub value: String,
    /// Shown below the rows while this one is selected.
    pub note: String,
}

/// Draw the Settings modal over `area` (normally the whole frame): centred
/// at about 80% × 80%, never smaller than 80×24 unless the screen is (then
/// it fills the screen). Records the scroll range in `state` for key
/// handling.
pub fn render(f: &mut Frame, area: Rect, state: &ModalState, ctx: &ModalContext) {
    let popup = modal_area(area);
    if popup.width < 3 || popup.height < 3 {
        return;
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
            format!(" {} ", state.tab.modal().title()),
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
    if inner.width == 0 || inner.height == 0 {
        return;
    }

    // Tab strip, rule, body, footer.
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
    let (lines, selected_line) = match state.tab {
        ModalTab::General => {
            let (lines, line) = general_lines(ctx, state.selected, text_width);
            (lines, Some(line))
        }
        ModalTab::Keys => {
            let (lines, line) = action_lines(ctx, state.action, text_width);
            (lines, Some(line))
        }
        tab => (body_lines(tab, ctx, text_width), None),
    };
    let total = u16::try_from(lines.len()).unwrap_or(u16::MAX);
    let max_scroll = total.saturating_sub(body.height);
    state.max_scroll.set(max_scroll);
    state.page.set(body.height.saturating_sub(1).max(1));
    let scroll = match selected_line {
        // The selected setting stays in view.
        Some(line) => u16::try_from(line + 1)
            .unwrap_or(u16::MAX)
            .saturating_sub(body.height)
            .min(max_scroll),
        None => state.scroll.min(max_scroll),
    };

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

    if footer_height == 1 {
        let footer = Rect {
            y: inner.bottom() - 1,
            height: 1,
            ..inner
        };
        draw_footer(f, footer, state.tab, scroll, body.height, total);
    }
}

/// The tab strip's items (the open modal's tabs): the full labels when
/// they all fit in `width`, otherwise the short ones.
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

/// Where the modal goes within `area`.
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
            ("█", palette.accent_primary)
        } else {
            ("│", palette.text_subtle)
        };
        buf.get_mut(x, body.y + row as u16)
            .set_symbol(symbol)
            .set_fg(color);
    }
}

fn draw_footer(f: &mut Frame, footer: Rect, tab: ModalTab, scroll: u16, page: u16, total: u16) {
    let palette = theme::palette();
    let hint_style = Style::default().fg(palette.neutral_gray);
    let key_style = Style::default().fg(palette.accent_green);
    let position = if total > page && !matches!(tab, ModalTab::General | ModalTab::Keys) {
        let last = (scroll + page).min(total);
        format!("{}-{}/{} ", scroll + 1, last, total)
    } else {
        String::new()
    };
    let position_width = str_width(&position) as u16;
    let room = footer.width.saturating_sub(position_width + 1);

    let jump = format!("1-{}", tab.modal().tabs().len());
    let rows = matches!(tab, ModalTab::General | ModalTab::Keys);
    let enter = if tab == ModalTab::Keys {
        "run"
    } else {
        "change"
    };
    let full: Vec<(&str, &str)> = if rows {
        vec![
            ("↑↓", "select"),
            ("Enter", enter),
            ("Tab ←→", "switch"),
            (&jump, "jump"),
            ("Esc", "close"),
        ]
    } else {
        vec![
            ("Tab ←→", "switch"),
            ("↑↓ PgUp PgDn", "scroll"),
            (&jump, "jump"),
            ("t", "tour"),
            ("Esc", "close"),
        ]
    };
    let short = if rows {
        [("↑↓", "select"), ("Enter", enter), ("Esc", "close")]
    } else {
        [("←→", "tabs"), ("↑↓", "scroll"), ("Esc", "close")]
    };
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
    // Shortest, for a phone: the way out stays whole.
    let tiny = if rows {
        [("Enter", enter), ("Esc", "close")]
    } else {
        [("←→", "tabs"), ("Esc", "close")]
    };
    let mut line = hint(&full);
    if line.width() > usize::from(room) {
        line = hint(&short);
    }
    if line.width() > usize::from(room) {
        line = hint(&tiny);
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
        f.render_widget(Paragraph::new(position).style(hint_style), position_area);
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
        // Rows, not prose: see `general_lines` and `action_lines`.
        ModalTab::General | ModalTab::Keys => {}
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
         in full colour: it's live, but your keys \
         don't reach it. On the folder filter, a folder's header or + New session it keeps \
         the last session in view."
    ));
    doc.text(format!(
        "To get back, press `Enter` (or `i`) on a session to work in it, or `Esc` (or `{p}`) \
         to return to the session you came from. The badge at the bottom left says which mode \
         you're in."
    ));

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
            "Session list, on the left: the folder filter on top ({} All folders), then the \
             sessions in the order you keep them (a new one goes at the bottom), each with a \
             status mark, its colour-coded app name first, then its git branch; the selected \
             name is white, the others dimmed. With \
             several folders, each has a header row; pinned sessions (★) lead their folder.",
            ctx.icons.folder()
        )),
        SessionLayout::Tabs => doc.bullet(format!(
            "Tab bar, along the top: the folder filter ({} All folders) at the left, then the \
             sessions as numbered tabs, each with a status mark, in the order you keep them. \
             Pinned tabs (★) come first.",
            ctx.icons.folder()
        )),
    }
    doc.bullet("Detail: the selected session's screen, updating as its program runs.");
    doc.bullet(format!(
        "Footer, the bottom row: the mode on the left (an orange RAKUMUX, or a quiet \
         SESSION), then messages and the daemon connection when it isn't up, and on the right \
         the keys for this Help (`?`) and Settings (`,`); in a session they come after `{p}`, \
         the way to Rakumux mode. Settings' Keys tab lists every key."
    ));
    doc.note(format!(
        "Current layout: {}. The Layouts tab explains both layouts and how to switch.",
        ctx.layout.label()
    ));

    doc.heading("Quick start");
    doc.step(
        1,
        "Press `n` to start a session. Leave the name empty and Rakumux picks one. You're \
         working in it straight away.",
    );
    doc.step(
        2,
        format!(
            "Press `{p}` for Rakumux mode. Move between sessions with `↑`/`↓` or `j`/`k`; the \
             right side shows each one live, in full colour."
        ),
    );
    doc.step(
        3,
        "Press `Enter` to work in the selected session, or `Esc` to go back to the one you \
         came from. `Tab` goes to the session before that.",
    );
    doc.step(
        4,
        "In Rakumux mode, `?` opens this Help and `,` opens Settings: the General tab holds \
         the layout and API keys, and the Keys tab lists every key and runs the one you pick. \
         `q` quits; quitting leaves every session running.",
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
            "Select a session or a folder's header; the right side shows the session (the \
             folder filter is above the first)",
        ),
        row(
            "gg, G, Home, End",
            "Top (the folder filter) / bottom (+ New session)",
        ),
        row("Ctrl-d, Ctrl-u", "Half a page down / up"),
        row("{, }", "Previous / next folder, when several are shown"),
        row(
            "Enter, i, l, →",
            "Work in the selected session; on the folder filter, the Folders modal; on a \
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
             Option as Meta key\". Settings, General can make them work inside sessions too.",
        ),
        None => doc.note("Option/Alt+↑/↓ are turned off in your settings; ↑/↓ and j/k move."),
    }
    doc.note(format!(
        "From a session, everything starts with `{p}`: `{p} n` is a new session, `{p} ?` this \
         Help. Every key: Settings (`,`), Keys tab."
    ));
}

fn mouse(doc: &mut Doc) {
    doc.heading("Mouse");
    doc.bullet("Click a session in the list to show it; click the session to work in it.");
    doc.bullet(
        "Click the folder filter (▾) to open the Folders modal; click a folder there to show \
         it alone, All folders to show every folder, or outside it to close it.",
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
         goes back to the session you came from. Click a key on the right of the footer (`? \
         help`, `, settings`) to do what it says. Phones send taps as clicks, so all of this \
         works there too.",
    );
    doc.bullet(
        "Wheel: scrolls the session's history, or goes to the program when it uses the mouse \
         itself.",
    );
    doc.bullet("Hold Shift while dragging to select and copy text with your terminal.");
    doc.bullet("Tabs layout: click a tab to show that session; click ‹ or › for hidden tabs.");
}

/// Keys inside Help and Settings.
fn modal_keys(doc: &mut Doc) {
    doc.heading("In Help and Settings");
    doc.keys(vec![
        row("Tab, ←, →", "Switch tab"),
        row("1-9", "Jump to a tab"),
        row(
            "↑, ↓",
            "Settings: choose a setting or an action. Help: scroll",
        ),
        row(
            "Enter, Space",
            "Change the selected setting, or run the selected action",
        ),
        row("PgUp, PgDn, Home, End", "Scroll"),
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
        "Every session has a name. Leave it empty when you create a session and Rakumux picks \
         one; rename it any time with `r` in Rakumux mode (`{p} r` from inside it)."
    ));
    doc.text(
        "`n` (or + New session) asks only for a name. The session goes in the current folder, \
         shown under the name, and starts your login shell there. Selecting a session or \
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
        "The list shows the sessions of All folders, including sessions started from another \
         window, the command line or an agent, as they appear. With more than one folder shown, \
         each folder has a header row with its name, git branch and how many of its sessions \
         are running (live/total). The folder you started Rakumux in comes first, then the \
         others by name. `{` and `}` jump between folders.",
    );
    doc.text(
        "The folder filter at the top of the list (at the left end of the tab bar in the Tabs \
         layout) says what's shown: All folders, one folder's name, or how many folders. \
         `Enter` on it, `P` or a click opens the Folders modal: All folders, then every folder \
         that has sessions. Type to narrow the rows; `Enter` shows the folder under the cursor \
         alone, and on All folders every folder again. Tick several with `Tab` or `Space` and \
         press `Enter` to see them together. Type a folder path to add a folder (it's created \
         if it doesn't exist) and show it, so you can start sessions there: the folders that \
         complete it are listed as you type; `↑`/`↓` choose one, `Tab` completes the path with \
         it and `Enter` adds it.",
    );
    doc.text(
        "The filter only narrows what this window shows; it isn't saved, so Rakumux always \
         starts on All folders.",
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
        "`p` pins the selected session, or unpins it. Pinned sessions show ★ after their name \
         and come first in their folder, in both layouts, in the order you give them. \
         Unpinned, a session goes back to its place among the others. Every Rakumux window \
         sees the same pins.",
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
        "Settings, General, Icons picks the marks for folders and branches: plain symbols, \
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
         for you), done (finished, not looked at yet) or idle. Rakumux reads this from the \
         screen and the running process, and agents can report it themselves.",
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
         attach` takes control, you become an observer again and the pane says so.",
    );

    doc.heading("Exited and dead sessions");
    doc.text(
        "Exited: the program ended, for example after `exit`. Its last screen stays visible, \
         and `Enter` restarts it in the same folder.",
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

    doc.heading("AI naming");
    doc.text(
        "A session you didn't name gets a better name the first time it settles (its program \
         goes idle or finishes a task): Rakumux sends the session's recent commands and \
         screen text to your AI provider and renames the session with its suggestion.",
    );
    doc.text(format!(
        "It uses Anthropic or OpenAI with the API key from the General tab here \
         (`{SETTINGS_KEY}` in the list opens it) or the ANTHROPIC_API_KEY or OPENAI_API_KEY \
         environment variable. Without a key, sessions keep their generated names. Names you \
         choose are never replaced."
    ));
}

fn layouts(doc: &mut Doc, ctx: &ModalContext, k: &KeyNames) {
    let p = &k.p;
    doc.note(format!("Current layout: {}.", ctx.layout.label()));

    doc.heading("Sidebar (default)");
    doc.text(
        "The session list sits in a column on the left; the selected session fills the right.",
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
        "On a phone (below {} columns, in either layout) you see one thing at a time: \
         Rakumux mode is the session list; tap a session to open it, full screen under a \
         row of tabs (tap a tab to switch); `{p}` or a tap on the SESSION badge at the bottom \
         left goes back to the list, and the RAKUMUX badge returns to the session. Tap \
         `? help` or `, settings` to open them.",
        layout::PHONE_BELOW
    ));
    doc.text(
        "A session is one terminal, so every window shows the program drawn for one size: \
         the size of the window you're using. Press a key or click in a window (or bring it to \
         the front) and the session takes its size, unless someone is typing in it in another \
         window right now.",
    );

    doc.heading("Tabs");
    doc.text(
        "Sessions appear as a row of numbered tabs across the top, and the selected tab's \
         session fills the rest of the screen. Long names are shortened with …, and ‹ or › \
         show that more tabs are hidden on that side.",
    );
    let cycle = match &k.cycle {
        Some(cycle) => format!(", `{cycle}`"),
        None => String::new(),
    };
    doc.text(format!(
        "In Rakumux mode, switch tabs with `↑`/`↓`, `j`/`k`{cycle} or the number keys, or \
         click a tab. Drag a tab left or right to move it. All other keys work as in the \
         Sidebar layout."
    ));

    doc.heading("Switching layouts");
    doc.text(format!(
        "Change Layout on the Settings General tab (`,` or `{SETTINGS_KEY}` in Rakumux mode). \
         It applies at once and is saved in ~/.rakumux/config.json as `sessions.layout` \
         (`sidebar` or `tabs`)."
    ));

    doc.heading("Zoom");
    doc.text(format!(
        "`z` in Rakumux mode works in the selected session with the list or tab bar hidden, \
         so it gets the whole screen above the footer, which keeps showing the mode; `{p}` \
         brings the list back. Works in both layouts."
    ));

    doc.heading("Session cycle keys");
    match &k.cycle {
        Some(cycle) => {
            doc.text(format!(
                "`{cycle}` moves to the previous or next session in list order in Rakumux \
                 mode, in both layouts. It needs a terminal that sends Option/Alt with the key \
                 (iTerm2, Ghostty, kitty, WezTerm, Alacritty; Terminal.app with \"Use Option \
                 as Meta key\")."
            ));
            doc.text(format!(
                "Inside a session these keys normally go to the program: OpenCode, Pi and \
                 Claude Code use them. Turn on \"Option+↑/↓ in sessions\" on the Settings \
                 General tab to switch sessions with them from inside one too (`{p}` and \
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
        "Pin a session first in its folder, or unpin it.",
    );
    doc.command(
        "rakumux folders [--json], rakumux folders add|rm <dir>",
        "List the folders, add one, or remove one that has no sessions.",
    );
    doc.command(
        "rakumux attach <name|id>",
        "One session full screen, outside this screen (works over SSH). `Ctrl-b` then `d` \
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
         It's off until you turn it on: Settings (,) → Phone access, or the question Rakumux \
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

/// The General tab: one row per setting (its label, then its current
/// value, or the value on the next line when both don't fit), the selected
/// one highlighted, then the selected setting's note. Also returns the
/// selected row's last line, which the body keeps in view.
fn general_lines(ctx: &ModalContext, selected: usize, width: usize) -> (Vec<Line<'static>>, usize) {
    let palette = theme::palette();
    let styles = Styles::from_theme();
    let width = width.max(1);
    let label_width = ctx
        .general
        .iter()
        .map(|row| str_width(&row.label))
        .max()
        .unwrap_or(0)
        + 2;
    // Too narrow for a label and its value side by side (a phone): every
    // value goes on the line under its label, so all the choices show.
    let values_below = ctx
        .general
        .iter()
        .any(|row| 2 + label_width + str_width(&row.value) > width);
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut selected_line = 0;
    let mut section = "";
    for (i, row) in ctx.general.iter().enumerate() {
        if row.section != section {
            section = row.section;
            push_section(&mut lines, section, width, &styles);
        }
        let (marker, label_style, value_style) = if i == selected {
            selected_line = lines.len();
            (
                "▶ ",
                Style::default()
                    .fg(palette.text_bright)
                    .add_modifier(Modifier::BOLD),
                Style::default().fg(palette.text_primary),
            )
        } else {
            (
                "  ",
                Style::default().fg(palette.text_primary),
                Style::default().fg(palette.text_muted),
            )
        };
        let pad = label_width - str_width(&row.label);
        let label = truncate(
            &format!("{}{}", row.label, " ".repeat(pad)),
            width.saturating_sub(2),
        );
        let marker = Span::styled(marker, Style::default().fg(palette.accent_primary));
        if !values_below {
            let value = truncate(&row.value, width.saturating_sub(2 + str_width(&label)));
            lines.push(fit(
                vec![
                    marker,
                    Span::styled(label, label_style),
                    Span::styled(value, value_style),
                ],
                width,
            ));
        } else {
            lines.push(fit(
                vec![marker, Span::styled(row.label.clone(), label_style)],
                width,
            ));
            if i == selected {
                selected_line = lines.len();
            }
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
    }
    if let Some(row) = ctx.general.get(selected) {
        lines.push(Line::default());
        let note = words(&row.note, styles.note, styles.key);
        lines.extend(wrap(&note, width, Vec::new(), 0));
    }
    lines.push(Line::default());
    let saved = words(
        "Saved in ~/.rakumux/config.json. The Keys tab lists every action and its keys; Help \
         (`?`) explains how Rakumux works.",
        styles.note,
        styles.key,
    );
    lines.extend(wrap(&saved, width, Vec::new(), 0));
    (lines, selected_line)
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

/// The Keys tab: a two-column table (Action, Key) with one row per
/// [`KeyAction`], the selected one marked and white, then where the
/// selected action's keys work. Also returns the selected row's line,
/// which the body keeps in view.
fn action_lines(ctx: &ModalContext, selected: usize, width: usize) -> (Vec<Line<'static>>, usize) {
    const MARKER: usize = 2;
    const GAP: usize = 3;
    let palette = theme::palette();
    let styles = Styles::from_theme();
    let names = KeyNames::new(ctx);
    let width = width.max(1);
    let rows: Vec<(&str, String)> = KeyAction::ALL
        .iter()
        .map(|action| (action.label(), action.keys(&names)))
        .collect();
    let widest = rows
        .iter()
        .map(|(label, _)| str_width(label))
        .max()
        .unwrap_or(0)
        .max(str_width("Action"));
    // The Action column gives way to the keys on narrow screens.
    let column = widest.min(width.saturating_sub(MARKER + GAP).saturating_mul(3) / 5);
    let cell = |text: &str, room: usize| {
        let text = truncate(text, room);
        let pad = room.saturating_sub(str_width(&text));
        format!("{text}{}", " ".repeat(pad))
    };
    let key_room = width.saturating_sub(MARKER + column + GAP);
    let header = Style::default()
        .fg(palette.neutral_gray)
        .add_modifier(Modifier::BOLD);
    let mut lines = vec![
        fit(
            vec![
                Span::raw(" ".repeat(MARKER)),
                Span::styled(cell("Action", column), header),
                Span::raw(" ".repeat(GAP)),
                Span::styled(truncate("Key", key_room), header),
            ],
            width,
        ),
        Line::from(Span::styled(
            "─".repeat(width),
            Style::default().fg(palette.border_dim),
        )),
    ];
    let selected = selected.min(rows.len().saturating_sub(1));
    let mut selected_line = 0;
    let mut section = "";
    for (i, (label, keys)) in rows.iter().enumerate() {
        let row_section = KeyAction::ALL[i].section();
        if row_section != section {
            section = row_section;
            push_section(&mut lines, section, width, &styles);
        }
        let (marker, label_style, key_style) = if i == selected {
            selected_line = lines.len();
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
        lines.push(fit(
            vec![
                Span::styled(marker, Style::default().fg(palette.accent_primary)),
                Span::styled(cell(label, column), label_style),
                Span::raw(" ".repeat(GAP)),
                Span::styled(truncate(keys, key_room), key_style),
            ],
            width,
        ));
    }
    let note = KeyAction::ALL
        .get(selected)
        .map(|action| action.note(&names))
        .unwrap_or_default();
    if !note.is_empty() {
        lines.push(Line::default());
        let note = words(&note, styles.note, styles.key);
        lines.extend(wrap(&note, width, Vec::new(), 0));
    }
    lines.push(Line::default());
    let p = &names.p;
    let hint = words(
        &format!(
            "Press `Enter` to run the selected action, or press its keys in Rakumux mode. \
             From inside a session, press `{p}` first."
        ),
        styles.note,
        styles.key,
    );
    lines.extend(wrap(&hint, width, Vec::new(), 0));
    (lines, selected_line)
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

    /// Rows on the General tab in these tests.
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
                setting("Layout", "[Sidebar] Tabs", "List on the left."),
                setting("Anthropic API key", "not set", "Used for AI session naming"),
                setting("OpenAI model", "gpt-4o-mini", "The model for naming"),
            ],
        }
    }

    fn state(tab: ModalTab) -> ModalState {
        ModalState::new(tab, ROWS)
    }

    fn press(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn line_text(line: &Line) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    /// The Keys tab's lines with `selected` chosen.
    fn keys_text(ctx: &ModalContext, selected: usize, width: usize) -> String {
        action_lines(ctx, selected, width)
            .0
            .iter()
            .map(line_text)
            .collect::<Vec<_>>()
            .join("\n")
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

    // --- tabs -----------------------------------------------------------

    #[test]
    fn each_modal_has_its_tabs_in_order_and_they_wrap() {
        assert_eq!(ModalTab::ALL.len(), 7);
        assert_eq!(ModalTab::default(), ModalTab::General);
        assert_eq!(Modal::Settings.tabs(), [ModalTab::General, ModalTab::Keys]);
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
        assert_eq!(Modal::Settings.first_tab(), ModalTab::General);
        assert_eq!(Modal::Help.first_tab(), ModalTab::Overview);
        assert_eq!(ModalTab::Keys.next(), ModalTab::General);
        assert_eq!(ModalTab::General.prev(), ModalTab::Keys);
        assert_eq!(ModalTab::License.next(), ModalTab::Overview);
        assert_eq!(ModalTab::Overview.prev(), ModalTab::License);
        assert_eq!(ModalTab::AgentsCli.label(), "Agents & CLI");
        assert_eq!(ModalTab::AgentsCli.short_label(), "CLI");
        assert_eq!(ModalTab::Keys.short_label(), "Keys");
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
            for code in [KeyCode::Esc, KeyCode::Char('q')] {
                let mut state = state(tab);
                assert_eq!(state.handle_key(&press(code)), ModalAction::Close);
            }
        }
        // `o` closes Settings, `?` closes Help; each is left alone in the
        // other modal.
        for (tab, closes, other) in [
            (ModalTab::General, 'o', '?'),
            (ModalTab::Keys, 'o', '?'),
            (ModalTab::Overview, '?', 'o'),
        ] {
            let mut state = state(tab);
            assert_eq!(
                state.handle_key(&press(KeyCode::Char(other))),
                ModalAction::Ignored
            );
            assert_eq!(
                state.handle_key(&press(KeyCode::Char(closes))),
                ModalAction::Close
            );
        }
        // `?` usually arrives with Shift held.
        let mut state = state(ModalTab::Sessions);
        let shifted = KeyEvent::new(KeyCode::Char('?'), KeyModifiers::SHIFT);
        assert_eq!(state.handle_key(&shifted), ModalAction::Close);
    }

    #[test]
    fn tab_and_arrows_switch_tabs_within_the_modal_and_reset_scroll() {
        let mut state = state(ModalTab::General);
        assert_eq!(state.handle_key(&press(KeyCode::Tab)), ModalAction::Handled);
        assert_eq!(state.tab, ModalTab::Keys);
        state.handle_key(&press(KeyCode::Right));
        assert_eq!(state.tab, ModalTab::General);
        state.handle_key(&press(KeyCode::Left));
        assert_eq!(state.tab, ModalTab::Keys);
        // `h`/`l` too.
        state.handle_key(&press(KeyCode::Char('l')));
        assert_eq!(state.tab, ModalTab::General);
        state.handle_key(&press(KeyCode::Char('h')));
        assert_eq!(state.tab, ModalTab::Keys);

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
    }

    #[test]
    fn number_keys_jump_to_the_modals_tabs() {
        for modal in [Modal::Settings, Modal::Help] {
            let mut state = state(modal.first_tab());
            for (i, tab) in modal.tabs().iter().enumerate() {
                let digit = char::from(b'1' + i as u8);
                assert_eq!(
                    state.handle_key(&press(KeyCode::Char(digit))),
                    ModalAction::Handled
                );
                assert_eq!(state.tab, *tab);
            }
            let past = char::from(b'1' + modal.tabs().len() as u8);
            assert_eq!(
                state.handle_key(&press(KeyCode::Char(past))),
                ModalAction::Ignored
            );
        }
    }

    #[test]
    fn general_tab_selects_and_changes_settings() {
        let mut state = state(ModalTab::General);
        assert_eq!(state.selected, 0);
        state.handle_key(&press(KeyCode::Up));
        assert_eq!(state.selected, 0);
        state.handle_key(&press(KeyCode::Down));
        state.handle_key(&press(KeyCode::Char('j')));
        assert_eq!(state.selected, 2);
        state.handle_key(&press(KeyCode::Down));
        assert_eq!(state.selected, ROWS - 1);
        state.handle_key(&press(KeyCode::Char('k')));
        assert_eq!(state.selected, 1);
        assert_eq!(
            state.handle_key(&press(KeyCode::Enter)),
            ModalAction::Change(1)
        );
        assert_eq!(
            state.handle_key(&press(KeyCode::Char(' '))),
            ModalAction::Change(1)
        );
        state.handle_key(&press(KeyCode::End));
        assert_eq!(state.selected, ROWS - 1);
        state.handle_key(&press(KeyCode::Home));
        assert_eq!(state.selected, 0);
        // The selection survives a look at the Keys tab.
        state.handle_key(&press(KeyCode::Down));
        state.handle_key(&press(KeyCode::Tab));
        state.handle_key(&press(KeyCode::Down));
        assert_eq!(state.action, 1);
        state.handle_key(&press(KeyCode::BackTab));
        assert_eq!(state.selected, 1);
        // Enter on a Help tab changes nothing.
        state.select_tab(ModalTab::Overview);
        assert_eq!(
            state.handle_key(&press(KeyCode::Enter)),
            ModalAction::Ignored
        );
        assert_eq!(
            ModalState::new(ModalTab::General, 0).handle_key(&press(KeyCode::Enter)),
            ModalAction::Ignored
        );
        assert_eq!(
            ModalState::new(ModalTab::General, ROWS)
                .with_selected(99)
                .selected,
            ROWS - 1
        );
    }

    #[test]
    fn keys_tab_selects_and_runs_actions() {
        let mut state = state(ModalTab::Keys);
        assert_eq!(state.action, 0);
        assert_eq!(
            state.handle_key(&press(KeyCode::Enter)),
            ModalAction::Run(KeyAction::RakumuxMode)
        );
        state.handle_key(&press(KeyCode::Up));
        assert_eq!(state.action, 0);
        state.handle_key(&press(KeyCode::Down));
        state.handle_key(&press(KeyCode::Char('j')));
        assert_eq!(state.action, 2);
        assert_eq!(
            state.handle_key(&press(KeyCode::Char(' '))),
            ModalAction::Run(KeyAction::GoIn)
        );
        state.handle_key(&press(KeyCode::End));
        assert_eq!(state.action, KeyAction::ALL.len() - 1);
        state.handle_key(&press(KeyCode::Down));
        assert_eq!(
            state.handle_key(&press(KeyCode::Enter)),
            ModalAction::Run(KeyAction::Quit)
        );
        state.handle_key(&press(KeyCode::Char('k')));
        state.handle_key(&press(KeyCode::Home));
        assert_eq!(state.action, 0);
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
    }

    #[test]
    fn modified_and_release_keys_are_left_to_the_caller() {
        for tab in [ModalTab::General, ModalTab::Keys] {
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
            assert_eq!(
                state.handle_key(&press(KeyCode::Char('x'))),
                ModalAction::Ignored
            );
        }
    }

    // --- rendering ------------------------------------------------------

    #[test]
    fn renders_at_80x24_full_screen_with_the_configured_prefix() {
        let context = ctx("Ctrl-a");
        let state = state(ModalTab::Keys);
        let buf = draw(80, 24, &state, &context);
        let text = buffer_text(&buf);
        let top = text.lines().next().unwrap();
        assert!(top.contains("Settings"), "{top}");
        assert!(top.contains("Rakumux 0.6.0"), "{top}");
        assert!(text.contains("Ctrl-a"), "{text}");
        assert!(!text.contains("Ctrl-b"), "{text}");
        assert!(text.contains("Esc"), "{text}");
        assert!(text.contains("Enter run"), "{text}");
        // Only Settings' tabs.
        let strip = text.lines().nth(1).unwrap();
        for label in ["1 General", "2 Keys"] {
            assert!(strip.contains(label), "{strip}");
        }
        assert!(!strip.contains("Overview"), "{strip}");
        // Fills the screen: the rounded corner is at the origin.
        assert_eq!(buf.get(0, 0).symbol(), "╭");
        assert_eq!(buf.get(79, 23).symbol(), "╯");

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
        assert!(
            !strip.contains("General") && !strip.contains("Keys"),
            "{strip}"
        );
        assert!(!strip.contains('›') && !strip.contains('‹'), "{strip}");
    }

    #[test]
    fn renders_at_160x48_centred_with_the_configured_prefix() {
        let context = ModalContext {
            in_session: true,
            ..ctx("Ctrl-Space")
        };
        let state = state(ModalTab::Keys);
        let buf = draw(160, 48, &state, &context);
        let text = buffer_text(&buf);
        assert!(text.contains("Esc, Ctrl-Space"), "{text}");
        assert!(text.contains("↓, j, Option/Alt+↓"), "{text}");
        // 80% of 160x48 = 128x38, centred.
        assert_eq!(buf.get(0, 0).symbol(), " ");
        assert_eq!(buf.get(16, 5).symbol(), "╭");
        assert_eq!(buf.get(143, 42).symbol(), "╯");

        let overview = draw(160, 48, &self::state(ModalTab::Overview), &context);
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
    fn general_tab_shows_each_setting_and_the_selected_note() {
        let context = ctx("Ctrl-b");
        let state = state(ModalTab::General).with_selected(1);
        let buf = draw(80, 24, &state, &context);
        let text = buffer_text(&buf);
        assert!(
            text.contains("  Layout             [Sidebar] Tabs"),
            "{text}"
        );
        assert!(text.contains("▶ Anthropic API key  not set"), "{text}");
        assert!(text.contains("  OpenAI model       gpt-4o-mini"), "{text}");
        assert!(text.contains("Used for AI session naming"), "{text}");
        assert!(!text.contains("List on the left."), "{text}");
        assert!(text.contains("~/.rakumux/config.json"), "{text}");
        assert!(text.contains("Enter change"), "{text}");
        // The selected row is white, with no background of its own.
        let p = theme::palette();
        let y = (0..24)
            .find(|y| buffer_text(&buf).lines().nth(*y).unwrap().contains('▶'))
            .unwrap() as u16;
        assert_eq!(buf.get(5, y).fg, p.text_bright);
        assert_eq!(buf.get(5, y).bg, p.background);
        assert_eq!(buf.get(5, y + 1).fg, p.text_primary);
    }

    #[test]
    fn general_rows_stay_in_view_and_inside_the_width() {
        let context = ctx("Ctrl-b");
        for width in [1, 5, 12, 30, 76] {
            for selected in 0..ROWS {
                let (lines, line) = general_lines(&context, selected, width);
                if width > 1 {
                    // The row's last line: the label's, or the value's
                    // under it.
                    let marked = |l: usize| line_text(&lines[l]).starts_with('▶');
                    assert!(marked(line) || (line > 0 && marked(line - 1)));
                }
                for line in &lines {
                    assert!(line.width() <= width, "{width}: {:?}", line_text(line));
                }
            }
        }
        // Too narrow for labels and values side by side (a phone): each
        // value goes under its label, whole.
        let (lines, line) = general_lines(&context, 0, 30);
        let text: Vec<String> = lines.iter().map(line_text).collect();
        let layout = text.iter().position(|l| l == "▶ Layout").unwrap();
        assert_eq!(text[layout + 1], "    [Sidebar] Tabs", "{text:?}");
        assert_eq!(line, layout + 1);
        assert!(text.contains(&"    not set".to_string()), "{text:?}");
        // Wide enough: side by side.
        let (lines, _) = general_lines(&context, 0, 76);
        assert!(line_text(&lines[1]).starts_with("▶ Layout             [Sidebar] Tabs"));
        // A short screen scrolls to the selected row.
        let state = state(ModalTab::General).with_selected(ROWS - 1);
        let buf = draw(80, 6, &state, &context);
        assert!(buffer_text(&buf).contains("▶ OpenAI model"));
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
                let mut state = state(tab);
                state.action = KeyAction::ALL.len() - 1;
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
        assert!(text.contains('█'), "scrollbar thumb: {text}");
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
        for tab in [ModalTab::Overview, ModalTab::General, ModalTab::Keys] {
            let buf = draw(40, 20, &ModalState::new(tab, ROWS), &context);
            let footer: String = (0..40)
                .map(|x| buf.get(x, 18).symbol().to_string())
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
    fn keys_tab_is_a_table_of_actions_and_their_configured_keys() {
        let mut context = ctx("C-a");
        context.cycle_previous = "Alt-k".into();
        context.cycle_next = "Alt-j".into();
        let text = keys_text(&context, 0, 120);
        let lines: Vec<&str> = text.lines().collect();
        assert!(lines[0].starts_with("  Action"), "{text}");
        assert!(lines[0].contains("   Key"), "{text}");
        assert!(lines[1].starts_with('─'), "{text}");
        let row = |action: &str| {
            lines
                .iter()
                .find(|l| l.chars().skip(2).collect::<String>().starts_with(action))
                .unwrap_or_else(|| panic!("no {action} row: {text}"))
        };
        for (action, keys) in [
            ("Rakumux mode", "C-a"),
            ("Back to the session you came from", "Esc, C-a"),
            ("Go into the selected session", "Enter, i, l, →"),
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
            ("Scroll the session back", "Ctrl-y, PgUp"),
            ("Scroll the session forward", "Ctrl-e, PgDn"),
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
            ("Help", "?"),
            ("Settings", "comma (,), o"),
            ("Check for updates", "u"),
            ("Reconnect or reload the list", "Ctrl-r"),
            ("Restart Rakumux", "R"),
            ("Quit and cd", "c"),
            ("Quit Rakumux", "q"),
            ("Send the Rakumux key", "\\"),
        ] {
            assert!(row(action).trim_end().ends_with(keys), "{action}: {text}");
        }
        // The Key column lines up.
        let column = |line: &str, needle: &str| {
            let at = line.find(needle).unwrap();
            line[..at].chars().count()
        };
        assert_eq!(
            column(row("Half a page down"), "Ctrl-d"),
            column(lines[0], "Key"),
            "{text}"
        );
        assert!(!text.contains("Ctrl-b"), "{text}");
        // The first row is selected, with its note below the table.
        assert!(row("Rakumux mode").starts_with("▶ "), "{text}");
        assert!(
            text.contains("the one key a session's program never gets"),
            "{text}"
        );
        assert!(
            text.contains("Press Enter to run the selected action"),
            "{text}"
        );
        // Rows are grouped under headings, in this order.
        let at = |heading: &str| {
            lines
                .iter()
                .position(|l| l.trim() == heading)
                .unwrap_or_else(|| panic!("no {heading} heading: {text}"))
        };
        let headings = [
            at("Modes"),
            at("Moving around"),
            at("Sessions and folders"),
            at("Help and app"),
        ];
        assert!(headings.windows(2).all(|w| w[0] < w[1]), "{text}");
        // The General tab too.
        let general = general_lines(&context, 0, 80)
            .0
            .iter()
            .map(line_text)
            .collect::<Vec<_>>();
        assert_eq!(general[0], "Appearance");
        assert!(general.iter().any(|l| l == "AI session naming"));
    }

    #[test]
    fn keys_tab_notes_follow_the_selection_and_rows_fit_the_width() {
        let context = ctx("Ctrl-b");
        let next = KeyAction::ALL
            .iter()
            .position(|a| *a == KeyAction::NextSession)
            .unwrap();
        let text = keys_text(&context, next, 100);
        assert!(text.contains("▶ Next session"), "{text}");
        assert!(text.contains("Use Option as Meta key"), "{text}");
        for width in [1, 12, 30, 60, 125] {
            for selected in 0..KeyAction::ALL.len() {
                let (lines, line) = action_lines(&context, selected, width);
                if width > 2 {
                    assert!(line_text(&lines[line]).starts_with('▶'));
                }
                for line in &lines {
                    assert!(line.width() <= width, "{width}: {:?}", line_text(line));
                }
            }
        }
        // A short screen scrolls to the selected row.
        let mut state = state(ModalTab::Keys);
        state.action = KeyAction::ALL.len() - 1;
        let buf = draw(80, 12, &state, &context);
        assert!(buffer_text(&buf).contains("▶ Quit Rakumux"));
    }

    #[test]
    fn disabled_cycle_keys_point_to_the_prefix_fallback() {
        let mut context = ctx("Ctrl-b");
        context.cycle_keys = "  ".into();
        context.cycle_previous = String::new();
        context.cycle_next = String::new();
        let overview = tab_text(ModalTab::Overview, &context, 100);
        assert!(overview.contains("turned off"), "{overview}");
        let next = KeyAction::ALL
            .iter()
            .position(|a| *a == KeyAction::NextSession)
            .unwrap();
        let keys = keys_text(&context, next, 100);
        assert!(keys.contains("turned off"), "{keys}");
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
    fn layout_tab_and_overview_follow_the_current_layout() {
        let mut context = ctx("Ctrl-b");
        context.layout = SessionLayout::Tabs;
        assert!(tab_text(ModalTab::Layouts, &context, 100).contains("Current layout: Tabs."));
        assert!(tab_text(ModalTab::Overview, &context, 100).contains("Tab bar, along the top"));
        context.layout = SessionLayout::Sidebar;
        assert!(tab_text(ModalTab::Layouts, &context, 100).contains("Current layout: Sidebar."));
        assert!(tab_text(ModalTab::Overview, &context, 100).contains("Session list, on the left"));
    }

    #[test]
    fn content_covers_the_whole_flow() {
        let context = ctx("Ctrl-b");
        let all: String = ModalTab::ALL
            .iter()
            .map(|tab| tab_text(*tab, &context, 200))
            .chain([keys_text(&context, 0, 200)])
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
            "asks only for a name",
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
            "Sidebar",
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
