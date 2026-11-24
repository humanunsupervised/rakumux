//! The drawer: rtop (`rtop.rs`), beside the sessions on the right. Every
//! start hides it; `t` (or Settings) shows it with the keys. Master detail
//! docks it as a pane on the right, a draggable line left of it, and the
//! session's pane narrows (the session on screen takes the new size, as a
//! dragged divider gives it). A window too narrow to keep the session usable
//! beside it shows it in the session's place in Rakumux mode only. Tabs
//! shows it as a tab of its own before the sessions'; a phone shows it
//! alone, full screen (`layout::split_with` decides where).
//!
//! It takes Rakumux mode's keys from the left column (`drawer_focused`):
//! `t` gives it the keys while the left column has them, and hides it while
//! it has them; Esc (a click beside it) gives them back. Session mode and the
//! terminal panel drop them.
//!
//! What has the keys instead of the sessions, the drawer or the left
//! column's Inbox or Beats (`left.rs`), is a [`Panel`], and handles them
//! ([`App::panel_key`]).

use std::time::Instant;

use super::types::Focus;
use super::{App, DividerState};
use crate::layout::{FeedShown, SessionLayout};

/// What shows besides the sessions and takes their keys.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Panel {
    /// What agents said when they finished a turn (`app/feed`): the left
    /// column's Inbox.
    Feed,
    /// The heartbeats: tasks run on a clock (`app/heartbeats.rs`): the left
    /// column's Beats.
    Beats,
    /// What this machine, Rakumux and every session use now
    /// (`app/rtop.rs`): the drawer.
    Rtop,
}

impl Panel {
    /// Its name on the left column's tabs, the drawer's band and the Tabs
    /// layout's bar.
    pub fn label(self) -> &'static str {
        match self {
            Panel::Feed => "Inbox",
            Panel::Beats => "Beats",
            Panel::Rtop => "rtop",
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

    /// The panels with a tab in the Tabs layout's bar, before the sessions':
    /// the Inbox or Beats while the left column's tab, and the drawer while
    /// it's shown (phones show no tab bar in Rakumux mode).
    pub fn bar_panels(&self) -> Vec<Panel> {
        if self.session_layout() != SessionLayout::Tabs || self.phone() {
            return Vec::new();
        }
        let drawer = self.drawer_open.then_some(Panel::Rtop);
        self.left_tab.panel().into_iter().chain(drawer).collect()
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
        self.feed_divider = DividerState::default();
        self.drawer_moved();
    }

    /// The left column takes Rakumux mode's keys back. Where the drawer would
    /// then leave the screen (a phone) it's hidden; Tabs keeps its tab.
    pub fn unfocus_drawer(&mut self) {
        if !self.drawer_focused {
            return;
        }
        self.drawer_focused = false;
        if self.drawer_open && !self.drawer_shown() && !self.bar_panels().contains(&Panel::Rtop) {
            self.drawer_open = false;
        }
        self.drawer_moved();
    }

    /// Session mode or the terminal panel took the keys: the left column has
    /// them when Rakumux mode comes back.
    pub(in crate::app) fn drawer_lost_keys(&mut self) {
        self.drawer_focused = false;
    }

    /// Esc while a panel has the keys (the footer's label too): its own.
    pub fn panel_back(&mut self) {
        match self.keys_panel() {
            Some(Panel::Feed) => self.feed_back(),
            Some(Panel::Beats) => self.beats_back(),
            Some(Panel::Rtop) => self.unfocus_drawer(),
            None => {}
        }
    }

    /// `gg` while a panel has the keys: its top.
    pub fn panel_to_top(&mut self) {
        match self.keys_panel() {
            Some(Panel::Feed) => self.feed_to_top(),
            Some(Panel::Beats) => self.beats_move(isize::MIN / 2),
            Some(Panel::Rtop) => self.rtop_move(isize::MIN / 2),
            None => {}
        }
    }

    /// The cycle keys while a panel has the keys: through its cards or rows.
    pub fn move_panel_selection(&mut self, delta: isize) {
        match self.keys_panel() {
            Some(Panel::Feed) => self.move_feed_selection(delta),
            Some(Panel::Beats) => self.beats_move(delta),
            Some(Panel::Rtop) => self.rtop_move(delta),
            None => {}
        }
    }

    /// The session the footer names while a panel has the keys: the selected
    /// row's, or the selected card's when it runs here.
    pub fn panel_session(&self) -> Option<&session_core::model::SessionView> {
        match self.keys_panel()? {
            Panel::Feed => self.feed_session(),
            Panel::Beats => self.beats_session(),
            Panel::Rtop => self.rtop_session(),
        }
    }

    /// Rakumux mode keys while a panel has them; `false` leaves the key to
    /// the list's handler.
    pub fn panel_key(&mut self, key: crossterm::event::KeyEvent) -> bool {
        match self.keys_panel() {
            Some(Panel::Feed) => self.feed_key(key),
            Some(Panel::Beats) => self.beats_key(key),
            Some(Panel::Rtop) => self.rtop_key(key),
            None => false,
        }
    }

    /// The drawer or the left column came, went, moved or changed what it
    /// shows: the session on screen takes its pane's new size (as a dragged
    /// divider gives it), and what came on screen starts (the feed is asked
    /// for changes, rtop for a sample) while what left stops.
    pub(in crate::app) fn drawer_moved(&mut self) {
        self.sync_terminal_size();
        let now = Instant::now();
        self.feed_timers(now);
        self.beats_timers(now);
        self.rtop_timers(now);
    }
}
