//! The Feed tab: the first of the list's tabs. While it's active in Master
//! detail, the list shows the feed (`components/feed.rs`) and, in Rakumux
//! mode, the detail shows the selected reply in full instead of a session.
//! The service is asked for changes every 2 s, only while the feed is on
//! screen; Enter on a reply made here goes into its session.

use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use rakumux_sensagent::feed::FeedPage;
use session_core::ids::SessionId;

use crate::app::grouping::{Grouping, ListTab};
use crate::app::runtime::AppEvent;
use crate::app::types::{AccountStatus, Focus};
use crate::app::App;
use crate::layout::SessionLayout;

/// How often the feed is asked for changes while it's shown.
const FEED_EVERY: Duration = Duration::from_secs(2);

impl App {
    /// The active tab of the list's top row, as shown: the Feed only where
    /// it can show (Master detail); Grid and Tabs show the grouped one.
    pub fn list_tab(&self) -> ListTab {
        if self.feed_shown() {
            ListTab::Feed
        } else {
            ListTab::Grouped(self.list.grouping())
        }
    }

    /// The Feed tab is active where it can show: Master detail (Grid and
    /// Tabs show sessions, and their selector the grouped tab).
    pub fn feed_shown(&self) -> bool {
        self.feed_tab && self.session_layout() == SessionLayout::Sidebar
    }

    /// The feed is on screen and the detail shows the selected reply rather
    /// than a session: Rakumux mode.
    pub fn feed_in_detail(&self) -> bool {
        self.feed_shown() && self.focus == Focus::List
    }

    /// Make `tab` active (the user's choice, so the Feed is never put in
    /// front of them afterwards).
    pub fn show_list_tab(&mut self, tab: ListTab) {
        self.tab_chosen = true;
        match tab {
            ListTab::Feed => self.open_feed_tab(),
            ListTab::Grouped(grouping) => self.show_tab(grouping),
        }
    }

    fn open_feed_tab(&mut self) {
        if self.feed_tab {
            return;
        }
        self.feed_tab = true;
        self.feed.reading = false;
        // Changes since this window last looked, at once.
        self.feed.due = Some(Instant::now());
        self.feed_timers(Instant::now());
    }

    /// Leave the Feed tab for the grouped one (a grouped tab was chosen).
    pub(in crate::app) fn close_feed_tab(&mut self) {
        self.feed_tab = false;
        self.feed.reading = false;
    }

    /// The account was checked: the first time its Feed switch is on, the
    /// feed comes to the front unless a tab was chosen already.
    pub(in crate::app) fn on_feed_switch(&mut self) {
        let on =
            matches!(&self.account, AccountStatus::SignedIn(account) if account.feed == Some(true));
        if on && !self.feed_switch_seen {
            self.feed_switch_seen = true;
            if !self.tab_chosen {
                self.open_feed_tab();
            }
        }
        if matches!(self.account, AccountStatus::SignedOut) {
            self.feed.clear();
        }
    }

    /// The tick's feed work: ask for changes when due, while it's shown.
    pub(in crate::app) fn feed_timers(&mut self, now: Instant) {
        if !self.feed_shown() || self.feed.busy {
            return;
        }
        if self.feed.due.is_none_or(|due| now >= due) {
            self.read_feed(now);
        }
    }

    fn read_feed(&mut self, now: Instant) {
        self.feed.due = Some(now + FEED_EVERY);
        self.feed.busy = true;
        let generation = self.feed.request_generation;
        let (after, held) = self.feed.cursor();
        let access = self.account_access.clone();
        self.exec.job(move || AppEvent::Feed {
            generation,
            result: access.feed(after, held),
        });
    }

    pub(in crate::app) fn on_feed(&mut self, generation: u64, result: Result<FeedPage, String>) {
        if generation != self.feed.request_generation {
            return;
        }
        self.feed.busy = false;
        // A failure (the service restarting) keeps what's shown.
        let Ok(page) = result else { return };
        if self.feed.apply(page) && self.feed_shown() {
            self.read_feed(Instant::now());
        }
    }

    /// Rakumux mode keys while the feed is shown; `false` leaves the key to
    /// the list's handler. Keys that act on the list's selected session do
    /// nothing here: no session is selected on screen.
    pub fn feed_key(&mut self, key: KeyEvent) -> bool {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let page = self.feed_page_rows();
        match key.code {
            KeyCode::Char('d') if ctrl => self.feed.scroll_by(page / 2),
            KeyCode::Char('u') if ctrl => self.feed.scroll_by(-(page / 2)),
            KeyCode::Char('e') if ctrl => self.feed.scroll_by(1),
            KeyCode::Char('y') if ctrl => self.feed.scroll_by(-1),
            KeyCode::PageDown => self.feed.scroll_by(page),
            KeyCode::PageUp => self.feed.scroll_by(-page),
            _ if ctrl => return false,
            KeyCode::Up | KeyCode::Char('k') => self.move_feed_selection(-1),
            KeyCode::Down | KeyCode::Char('j') => self.move_feed_selection(1),
            KeyCode::Home => self.move_feed_selection(isize::MIN / 2),
            KeyCode::End | KeyCode::Char('G') => self.move_feed_selection(isize::MAX / 2),
            KeyCode::Char('g') => self.pending_key = Some('g'),
            KeyCode::Char('h') => self.switch_tab(-1),
            KeyCode::Char('l') => self.switch_tab(1),
            KeyCode::Enter | KeyCode::Char('i') => self.open_feed_entry(),
            KeyCode::Esc if self.feed.reading => self.feed.reading = false,
            KeyCode::Char(
                'p' | 's' | 'J' | 'K' | 'r' | 'd' | 'x' | 'z' | 'c' | '/' | '{' | '}',
            ) => {}
            _ => return false,
        }
        true
    }

    /// `gg` while the feed is shown: the newest reply.
    pub fn feed_to_top(&mut self) {
        self.move_feed_selection(isize::MIN / 2);
    }

    /// Select the reply `delta` entries along (the newest first).
    pub fn move_feed_selection(&mut self, delta: isize) {
        self.feed.reading = false;
        self.feed.move_selection(delta);
    }

    /// A click on the reply at `index`: select it; on a phone, or when it
    /// was selected already, open it.
    pub fn click_feed_entry(&mut self, index: usize) {
        if self.focus != Focus::List {
            self.enter_rakumux_mode();
        }
        let again = self.feed.selected_index() == Some(index);
        self.feed.select(index);
        if again || self.phone() {
            self.open_feed_entry();
        }
    }

    /// Enter on a reply: into its session when it runs here (from the
    /// Status tab, where it's shown); on a phone, a reply from elsewhere
    /// fills the screen to be read; otherwise say where it's from.
    pub fn open_feed_entry(&mut self) {
        let Some(entry) = self.feed.selected().cloned() else {
            return;
        };
        let session = SessionId::from_string(entry.session_id.clone());
        if self.list.get(&session).is_some() {
            self.close_feed_tab();
            self.list.set_grouping(Grouping::Status);
            self.go_into(session);
            return;
        }
        if self.phone() {
            self.feed.reading = true;
            return;
        }
        let place = if entry.machine == self.feed.machine() {
            "It came from a session that's gone.".to_owned()
        } else {
            format!("It came from {}; its session runs there.", entry.machine)
        };
        self.set_status(place);
    }

    /// The feed entry's session when it runs here: what the footer names
    /// while the feed is shown.
    pub fn feed_session(&self) -> Option<&session_core::model::SessionView> {
        let entry = self.feed.selected()?;
        self.list
            .get(&SessionId::from_string(entry.session_id.clone()))
    }

    /// Rows of the area the selected reply is read in, for paging.
    fn feed_page_rows(&self) -> isize {
        let screen = self.screen();
        let area = if self.feed.reading || screen.detail.is_none() {
            screen.list
        } else {
            screen.detail
        };
        area.map_or(20, |area| (area.height.saturating_sub(3) as isize).max(1))
    }
}
