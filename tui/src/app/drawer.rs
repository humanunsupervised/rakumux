//! The drawer: what shows beside the sessions on the right, with two tabs,
//! the Feed (`feed/`) and rtop (`rtop.rs`). Every start hides it; `f` shows
//! it on the Feed, `t` on rtop (or Settings for either). Master detail and
//! Grid dock it as a pane on the right, a draggable line left of it, and the
//! session's pane narrows (the session on screen takes the new size, as a
//! dragged divider gives it). A window too narrow to keep the session usable
//! beside it shows it in the session's place in Rakumux mode only. Tabs
//! shows it as a tab of its own before the sessions', named by its active
//! tab; a phone shows it alone, full screen (`layout::split_with` decides
//! where). Its band names both tabs, the active one underlined.
//!
//! It takes Rakumux mode's keys from the list (`drawer_focused`), and its
//! active tab handles them (`App::feed_key`, `App::rtop_key`): `f` (`t`)
//! shows the drawer on the Feed (rtop) with the keys, switches to that tab,
//! gives it the keys while the list has them, and hides the drawer while
//! that tab has them; a click focuses the side it lands on, a click on a
//! band's tab shows that tab; Esc gives the keys back to the list, hiding
//! the drawer where it would vanish (a phone, Grid's cards on a narrow
//! window). Session mode gives them back too. Only the active tab works:
//! the feed is read fast and rtop samples only while their tab is on
//! screen.

use std::time::Instant;

use crate::app::types::Focus;
use crate::app::{App, DividerState};
use crate::layout::{FeedShown, SessionLayout};

/// The drawer's tabs, in the band's order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DrawerTab {
    /// What agents said when they finished a turn (`app/feed`).
    #[default]
    Feed,
    /// What this machine, Rakumux and every session use now (`app/rtop.rs`).
    Rtop,
}

impl DrawerTab {
    pub const ALL: [DrawerTab; 2] = [DrawerTab::Feed, DrawerTab::Rtop];

    /// Its name on the band and the Tabs layout's tab.
    pub fn label(self) -> &'static str {
        match self {
            DrawerTab::Feed => "Feed",
            DrawerTab::Rtop => "rtop",
        }
    }
}

impl App {
    /// The drawer is shown (a pane, a tab or the screen), whether or not
    /// it's on screen in this mode.
    #[cfg(test)]
    pub fn drawer_open(&self) -> bool {
        self.drawer_open
    }

    /// The drawer's active tab.
    pub fn drawer_tab(&self) -> DrawerTab {
        self.drawer_tab
    }

    /// The drawer has Rakumux mode's keys: its pane is focused, its tab
    /// selected.
    pub fn drawer_has_keys(&self) -> bool {
        self.drawer_open && self.drawer_focused && self.focus == Focus::List
    }

    /// The drawer as the layout places it.
    pub(in crate::app) fn drawer_layout(&self) -> FeedShown {
        match (self.drawer_open, self.drawer_focused) {
            (false, _) => FeedShown::Hidden,
            (true, false) => FeedShown::Shown,
            (true, true) => FeedShown::Focused,
        }
    }

    /// The drawer is on screen.
    pub fn drawer_shown(&self) -> bool {
        self.screen().feed.is_some()
    }

    /// The Tabs layout's tab bar has the drawer's tab (phones show no tab
    /// bar in Rakumux mode; the drawer fills the screen there).
    pub fn drawer_tab_shown(&self) -> bool {
        self.drawer_open && self.session_layout() == SessionLayout::Tabs && !self.phone()
    }

    /// `f` or `t` (or Settings, or the footer's Feed button): show the
    /// drawer on `tab` with the keys, switch to `tab`, give it the keys
    /// while the list has them, or hide the drawer while `tab` has them.
    pub fn toggle_drawer(&mut self, tab: DrawerTab) {
        if !self.drawer_open {
            self.open_drawer(tab);
        } else if self.drawer_tab != tab {
            self.show_drawer_tab(tab);
        } else if self.drawer_has_keys() {
            self.close_drawer();
        } else {
            self.focus_drawer();
        }
    }

    /// Show the drawer on `tab` and give it the keys.
    pub fn open_drawer(&mut self, tab: DrawerTab) {
        if !self.drawer_open {
            self.drawer_open = true;
            self.feed.reading = false;
        }
        self.show_drawer_tab(tab);
    }

    /// Switch the drawer to `tab` (a click on the band's tab) and give it
    /// the keys.
    pub fn show_drawer_tab(&mut self, tab: DrawerTab) {
        self.drawer_tab = tab;
        self.focus_drawer();
    }

    /// Give the drawer Rakumux mode's keys (from a session, in Rakumux mode).
    pub fn focus_drawer(&mut self) {
        if !self.drawer_open {
            return;
        }
        if self.focus != Focus::List {
            self.enter_rakumux_mode();
        }
        self.drawer_focused = true;
        self.pending_key = None;
        self.drawer_moved();
    }

    /// Hide the drawer.
    pub fn close_drawer(&mut self) {
        if !self.drawer_open {
            return;
        }
        self.drawer_open = false;
        self.drawer_focused = false;
        self.feed.reading = false;
        self.feed_divider = DividerState::default();
        self.drawer_moved();
    }

    /// The list takes Rakumux mode's keys back. Where the drawer would then
    /// leave the screen (a phone, Grid's cards on a narrow window) it's
    /// hidden; Tabs keeps its tab.
    pub fn unfocus_drawer(&mut self) {
        if !self.drawer_focused {
            return;
        }
        self.drawer_focused = false;
        if self.drawer_open && !self.drawer_shown() && !self.drawer_tab_shown() {
            self.drawer_open = false;
            self.feed.reading = false;
        }
        self.drawer_moved();
    }

    /// Session mode or the terminal panel took the keys: the list has them
    /// when Rakumux mode comes back.
    pub(in crate::app) fn drawer_lost_keys(&mut self) {
        self.drawer_focused = false;
    }

    /// Esc while the drawer has the keys (the RAKUMUX MODE label too): the
    /// active tab's own Esc.
    pub fn drawer_back(&mut self) {
        match self.drawer_tab {
            DrawerTab::Feed => self.feed_back(),
            DrawerTab::Rtop => self.unfocus_drawer(),
        }
    }

    /// `gg` while the drawer has the keys: the active tab's top.
    pub fn drawer_to_top(&mut self) {
        match self.drawer_tab {
            DrawerTab::Feed => self.feed_to_top(),
            DrawerTab::Rtop => self.rtop_move(isize::MIN / 2),
        }
    }

    /// The cycle keys while the drawer has the keys: through the active
    /// tab's cards or rows.
    pub fn move_drawer_selection(&mut self, delta: isize) {
        match self.drawer_tab {
            DrawerTab::Feed => self.move_feed_selection(delta),
            DrawerTab::Rtop => self.rtop_move(delta),
        }
    }

    /// The session the footer names while the drawer has the keys: the
    /// selected card's, or rtop's selected row's, when it runs here.
    pub fn drawer_session(&self) -> Option<&session_core::model::SessionView> {
        match self.drawer_tab {
            DrawerTab::Feed => self.feed_session(),
            DrawerTab::Rtop => self.rtop_session(),
        }
    }

    /// Rakumux mode keys while the drawer has them; `false` leaves the key
    /// to the list's handler.
    pub fn drawer_key(&mut self, key: crossterm::event::KeyEvent) -> bool {
        match self.drawer_tab {
            DrawerTab::Feed => self.feed_key(key),
            DrawerTab::Rtop => self.rtop_key(key),
        }
    }

    /// The drawer came, went, moved or changed tab: the session on screen
    /// takes its pane's new size (as a dragged divider gives it), Grid's
    /// previews follow their cards, and the tab that came on screen starts
    /// (the feed is asked for changes, rtop for a sample) while the one that
    /// left stops.
    pub(in crate::app) fn drawer_moved(&mut self) {
        self.sync_terminal_size();
        self.sync_grid();
        let now = Instant::now();
        self.feed_timers(now);
        self.rtop_timers(now);
    }
}
