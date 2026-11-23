//! The left column's tabs, at its bottom: the sessions (with their own tabs,
//! Pinned, Status, Repos and Agent: `list_tabs.rs`), the Inbox (`feed/`) and
//! Beats (`heartbeats.rs`). Master detail shows the one picked in the list's
//! column, a phone and a stacked screen on the whole screen in Rakumux mode.
//! Tabs has no column: the Inbox or Beats takes the session's place under
//! the tab bar in Rakumux mode, with a tab of its own there.
//!
//! The tab shown has Rakumux mode's keys (unless the drawer, rtop, has them:
//! [`App::keys_panel`]). `f` shows the Inbox and `B` Beats; again, or Esc,
//! goes back to the sessions. Session mode leaves the tab as it is. Every
//! start shows the sessions.

use ratatui::layout::Rect;

use super::drawer::Panel;
use super::types::Focus;
use super::App;
use crate::layout;

/// The left column's tabs, in order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LeftTab {
    #[default]
    Sessions,
    /// What agents said when they finished a turn (`app/feed`).
    Inbox,
    /// The heartbeats: tasks run on a clock (`app/heartbeats.rs`).
    Beats,
}

impl LeftTab {
    pub const ALL: [LeftTab; 3] = [LeftTab::Sessions, LeftTab::Inbox, LeftTab::Beats];

    pub fn label(self) -> &'static str {
        match self {
            LeftTab::Sessions => "Sessions",
            LeftTab::Inbox => "Inbox",
            LeftTab::Beats => "Beats",
        }
    }

    /// What it shows besides the sessions.
    pub fn panel(self) -> Option<Panel> {
        match self {
            LeftTab::Sessions => None,
            LeftTab::Inbox => Some(Panel::Feed),
            LeftTab::Beats => Some(Panel::Beats),
        }
    }
}

impl App {
    /// The left column's tab.
    pub fn left_tab(&self) -> LeftTab {
        self.left_tab
    }

    /// The left column, tabs included, where it's on screen (the Tabs
    /// layout's Inbox or Beats).
    pub fn left_pane(&self) -> Option<Rect> {
        let screen = self.screen();
        screen.list.or(screen.panel)
    }

    /// The left column without its tabs.
    pub fn left_body(&self) -> Option<Rect> {
        self.left_pane().map(|pane| layout::left_split(pane).0)
    }

    /// The left column shows `tab`, on screen.
    pub fn left_tab_shown(&self, tab: LeftTab) -> bool {
        self.left_tab == tab && self.left_pane().is_some()
    }

    /// What has Rakumux mode's keys instead of the sessions: the drawer, else
    /// the Inbox or Beats on screen.
    pub fn keys_panel(&self) -> Option<Panel> {
        if self.focus != Focus::List {
            return None;
        }
        if self.drawer_open && self.drawer_focused {
            return Some(Panel::Rtop);
        }
        self.left_tab.panel().filter(|_| self.left_pane().is_some())
    }

    /// Something other than the sessions has Rakumux mode's keys.
    pub fn panel_has_keys(&self) -> bool {
        self.keys_panel().is_some()
    }

    /// Show `tab` in the left column (showing the column if it was hidden)
    /// and give it Rakumux mode's keys.
    pub fn show_left_tab(&mut self, tab: LeftTab) {
        if self.focus != Focus::List {
            self.enter_rakumux_mode();
        }
        self.drawer_focused = false;
        self.pending_key = None;
        if self.list_hidden && tab != LeftTab::Sessions {
            self.toggle_list();
        }
        self.set_left_tab(tab);
    }

    /// `f`, `B`: show `tab` with the keys, or the sessions while it has them.
    pub fn toggle_left_tab(&mut self, tab: LeftTab) {
        if self.panel_has_keys() && self.keys_panel() == tab.panel() {
            self.show_sessions_tab();
        } else {
            self.show_left_tab(tab);
        }
    }

    /// A click on `tab`; on the one shown, what's read whole in it goes back
    /// to its list.
    pub fn click_left_tab(&mut self, tab: LeftTab) {
        match tab {
            LeftTab::Sessions => {}
            LeftTab::Inbox if self.left_tab == tab => self.feed.reading = false,
            LeftTab::Beats if self.left_tab == tab => self.beats.reading = None,
            LeftTab::Inbox | LeftTab::Beats => {}
        }
        self.show_left_tab(tab);
    }

    /// The left column shows the sessions, with the keys in Rakumux mode.
    pub fn show_sessions_tab(&mut self) {
        self.unfocus_drawer();
        self.set_left_tab(LeftTab::Sessions);
    }

    fn set_left_tab(&mut self, tab: LeftTab) {
        if self.left_tab != tab {
            self.left_tab = tab;
            self.feed.reading = false;
            self.hovered_row = None;
            self.hovered_place = None;
        }
        self.drawer_moved();
    }
}
