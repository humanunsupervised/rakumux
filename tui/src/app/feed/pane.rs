//! The feed on screen: the drawer's Feed tab (`app/drawer.rs` says where
//! the drawer goes and who has its keys). In the pane, Enter reads the
//! selected card's reply whole in the cards' place (Esc back), and Enter
//! there goes into its session when it runs here; the Feed tab of the Tabs
//! layout, wide enough, shows the cards and the selected reply side by side,
//! and Enter goes in.
//!
//! The service is asked for changes every 2 s while the feed is on screen,
//! and every 10 s otherwise while the account's Feed switch is on (without
//! counting as showing it), so the footer's button and the Feed tab can
//! count the entries that arrived since it was last on screen. `r` writes
//! back to the selected entry (`reply.rs`).

use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use rakumux_sensagent::feed::{FeedPage, FeedTurn};
use ratatui::layout::Rect;
use session_core::ids::SessionId;

use crate::app::drawer::DrawerTab;
use crate::app::runtime::AppEvent;
use crate::app::types::AccountStatus;
use crate::app::{App, DividerState};
use crate::layout::{self, PaneSizes};

/// How often the feed is asked for changes while it's shown.
const FEED_EVERY: Duration = Duration::from_secs(2);
/// How often otherwise, while the account's Feed switch is on: to count
/// what's new.
pub(in crate::app) const FEED_HIDDEN_EVERY: Duration = Duration::from_secs(10);
/// Rows of a card, about, for paging through them.
const CARD_ROWS: isize = 6;

impl App {
    /// The feed has Rakumux mode's keys: the drawer has them on its Feed tab.
    pub fn feed_has_keys(&self) -> bool {
        self.drawer_has_keys() && self.drawer_tab == DrawerTab::Feed
    }

    /// The feed is on screen: the drawer shows its Feed tab (its cards, or a
    /// reply read whole).
    pub fn feed_shown(&self) -> bool {
        self.drawer_shown() && self.drawer_tab == DrawerTab::Feed
    }

    /// The cards, the line and the selected reply side by side: the Tabs
    /// layout's drawer on the Feed, wide enough (`layout::feed_tab_columns`).
    pub fn feed_columns(&self) -> Option<(Rect, Rect, Rect)> {
        if !self.drawer_tab_shown() || self.drawer_tab != DrawerTab::Feed {
            return None;
        }
        self.screen().feed.and_then(layout::feed_tab_columns)
    }

    /// The selected reply is on screen whole: read in the cards' place, or
    /// beside them.
    pub fn feed_reply_shown(&self) -> bool {
        self.feed_shown() && (self.feed.reading || self.feed_columns().is_some())
    }

    /// `f`, the footer's Feed button or Settings: show the drawer on the
    /// Feed with the keys, switch it to the Feed, give it the keys while the
    /// list has them, or hide it while the Feed has them.
    pub fn toggle_feed(&mut self) {
        self.toggle_drawer(DrawerTab::Feed);
    }

    /// Show the drawer on the Feed and give it the keys.
    #[cfg(test)]
    pub fn open_feed(&mut self) {
        self.open_drawer(DrawerTab::Feed);
    }

    /// The feed's Esc: a reply read in the cards' place goes back to them,
    /// else the list gets the keys.
    pub fn feed_back(&mut self) {
        if self.feed.reading && self.feed_columns().is_none() {
            self.feed.reading = false;
        } else {
            self.unfocus_drawer();
        }
    }

    /// When the feed comes on screen or leaves it, what it holds is seen;
    /// coming on, it's asked for changes at once.
    fn note_feed_on_screen(&mut self, now: Instant) {
        let shown = self.feed_shown();
        if shown != self.feed_on_screen {
            self.feed_on_screen = shown;
            self.feed.mark_seen();
            if shown {
                self.feed.due = Some(now);
            }
        }
    }

    /// The line left of the docked drawer dragged to `column`: it takes
    /// the columns right of it, within what docks.
    pub(in crate::app) fn drag_feed_divider(&mut self, column: u16) {
        let wanted = self.area.right().saturating_sub(column.saturating_add(1));
        let sizes = PaneSizes {
            feed_cols: Some(wanted),
            ..self.pane_sizes()
        };
        let Some(width) = layout::docked_feed_width(self.area.width, self.session_layout(), sizes)
        else {
            return;
        };
        if Some(width) != self.settings.sessions.feed_width {
            self.settings.sessions.feed_width = Some(width);
            self.sync_terminal_size();
            self.sync_grid();
        }
    }

    /// Let go of the line left of the pane: its width is saved.
    pub(in crate::app) fn drop_feed_divider(&mut self) {
        self.feed_divider = DividerState::default();
        if let Err(error) = self.save_settings() {
            self.set_status(format!("Feed width changed but not saved: {error}"));
        }
    }

    /// The account was checked or its Feed switch changed: signed out, the
    /// feed is forgotten (but what was seen). With the switch on, the next
    /// tick starts keeping it, to count what's new.
    pub(in crate::app) fn on_feed_switch(&mut self) {
        if matches!(self.account, AccountStatus::SignedOut) {
            self.feed.clear();
        }
    }

    /// Signed in with the account's Feed switch on.
    fn feed_switch_on(&self) -> bool {
        matches!(&self.account, AccountStatus::SignedIn(account) if account.feed == Some(true))
    }

    /// Entries that arrived since the feed was last on screen (none while
    /// it is, or while the switch is off and it isn't kept): what the
    /// footer's Feed button and the Feed tab count.
    pub fn feed_unseen(&self) -> usize {
        if self.feed_shown() || !self.feed_switch_on() {
            0
        } else {
            self.feed.unseen()
        }
    }

    /// The tick's feed work: note whether it's on screen, ask for changes
    /// when due (while it's shown, or while the switch is on, to count
    /// what's new), and for the selected reply's turn while it's read.
    pub(in crate::app) fn feed_timers(&mut self, now: Instant) {
        self.note_feed_on_screen(now);
        if self.feed_shown() {
            self.follow_turn();
        } else if !self.feed_switch_on() {
            return;
        }
        if self.feed.busy {
            return;
        }
        if self.feed.due.is_none_or(|due| now >= due) {
            self.read_feed(now);
        }
    }

    /// Ask for the selected reply's turn while it's on screen whole, unless
    /// one is being asked for.
    fn follow_turn(&mut self) {
        if self.feed_reply_shown() && !self.feed.turn_busy {
            self.read_turn();
        }
    }

    /// Ask for the selected reply's turn, or the rest of it.
    fn read_turn(&mut self) {
        let Some((entry, after)) = self.feed.turn_due() else {
            return;
        };
        self.feed.turn_asked(&entry);
        self.feed.turn_busy = true;
        let generation = self.feed.request_generation;
        let access = self.account_access.clone();
        self.exec.job(move || {
            let result = access.feed_turn(&entry, after);
            AppEvent::FeedTurn {
                generation,
                entry,
                appended: after.is_some(),
                result,
            }
        });
    }

    pub(in crate::app) fn on_feed_turn(
        &mut self,
        generation: u64,
        entry: String,
        appended: bool,
        result: Result<FeedTurn, String>,
    ) {
        if generation != self.feed.request_generation {
            return;
        }
        self.feed.turn_busy = false;
        let turn = result.unwrap_or_else(|_| {
            FeedTurn::failed(rakumux_sensagent::feed::FeedFailure::Unavailable(None))
        });
        self.feed.apply_turn(&entry, turn, appended);
        // The rest of it at once, or the turn selected meanwhile.
        self.follow_turn();
    }

    /// Ask for the changes since what's held; the service reads the cloud
    /// more often only while the feed is on screen.
    fn read_feed(&mut self, now: Instant) {
        let shown = self.feed_shown();
        let every = if shown { FEED_EVERY } else { FEED_HIDDEN_EVERY };
        self.feed.due = Some(now + every);
        self.feed.busy = true;
        let generation = self.feed.request_generation;
        let (after, held) = self.feed.cursor();
        let access = self.account_access.clone();
        self.exec.job(move || AppEvent::Feed {
            generation,
            result: access.feed(after, held, shown),
        });
    }

    pub(in crate::app) fn on_feed(&mut self, generation: u64, result: Result<FeedPage, String>) {
        if generation != self.feed.request_generation {
            return;
        }
        self.feed.busy = false;
        // A failure (the service restarting) keeps what's shown.
        let Ok(page) = result else { return };
        let more = self.feed.apply(page);
        if self.feed_shown() {
            // On screen: none of it is new.
            self.feed.mark_seen();
        }
        if more {
            self.read_feed(Instant::now());
        }
        self.follow_turn();
    }

    /// Rakumux mode keys while the feed has them; `false` leaves the key to
    /// the list's handler (`f` among them). Keys that act on the list's
    /// selected session do nothing here. A reply read in the cards' place
    /// scrolls with ↑/↓ as the cards move with them.
    pub fn feed_key(&mut self, key: KeyEvent) -> bool {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let page = self.feed_page_rows();
        let reading = self.feed.reading && self.feed_columns().is_none();
        match key.code {
            KeyCode::Char('d') if ctrl => self.feed_page(page / 2),
            KeyCode::Char('u') if ctrl => self.feed_page(-(page / 2)),
            KeyCode::Char('e') if ctrl => self.feed.scroll_by(1),
            KeyCode::Char('y') if ctrl => self.feed.scroll_by(-1),
            KeyCode::PageDown => self.feed_page(page),
            KeyCode::PageUp => self.feed_page(-page),
            _ if ctrl => return false,
            KeyCode::Up | KeyCode::Char('k') if reading => self.feed.scroll_by(-1),
            KeyCode::Down | KeyCode::Char('j') if reading => self.feed.scroll_by(1),
            KeyCode::Up | KeyCode::Char('k') => self.move_feed_selection(-1),
            KeyCode::Down | KeyCode::Char('j') => self.move_feed_selection(1),
            KeyCode::Home => self.feed_to_top(),
            KeyCode::End | KeyCode::Char('G') if !reading => {
                self.move_feed_selection(isize::MAX / 2)
            }
            KeyCode::End | KeyCode::Char('G') => {}
            KeyCode::Char('g') => self.pending_key = Some('g'),
            KeyCode::Enter | KeyCode::Char('i' | 'l') => self.open_feed_entry(),
            KeyCode::Char('r') => self.open_feed_reply(),
            KeyCode::Esc => self.feed_back(),
            // Its cards have no sides to go to.
            KeyCode::Left | KeyCode::Right | KeyCode::Char('h') => {}
            KeyCode::Char('p' | 's' | 'J' | 'K' | 'd' | 'x' | 'z' | 'c' | '/' | '{' | '}') => {}
            _ => return false,
        }
        true
    }

    /// Ctrl-d/Ctrl-u, PgUp/PgDn: scroll the reply on screen `rows`, else
    /// move through the cards about as far.
    fn feed_page(&mut self, rows: isize) {
        if self.feed_reply_shown() {
            self.feed.scroll_by(rows);
        } else {
            let cards = rows / CARD_ROWS;
            self.move_feed_selection(if cards == 0 { rows.signum() } else { cards });
        }
    }

    /// `gg` and Home while the feed has the keys: the newest card, or the
    /// top of the reply read in its place.
    pub fn feed_to_top(&mut self) {
        if self.feed.reading && self.feed_columns().is_none() {
            self.feed.scroll = 0;
        } else {
            self.move_feed_selection(isize::MIN / 2);
        }
    }

    /// Select the card `delta` along (the newest first).
    pub fn move_feed_selection(&mut self, delta: isize) {
        self.feed.move_selection(delta);
        self.follow_turn();
    }

    /// A click on the card at `index`: the feed gets the keys and the card
    /// is selected; a second click (a tap on a phone) opens it as Enter
    /// does.
    pub fn click_feed_entry(&mut self, index: usize) {
        let again = self.feed_has_keys() && self.feed.selected_index() == Some(index);
        self.show_drawer_tab(DrawerTab::Feed);
        self.feed.select(index);
        if again || self.phone() {
            self.open_feed_entry();
        }
        self.follow_turn();
    }

    /// Enter: the selected card's reply whole in the cards' place; from
    /// there (or beside the cards), into its session.
    pub fn open_feed_entry(&mut self) {
        if self.feed_reply_shown() {
            self.go_into_feed_session();
        } else if self.feed.selected_card().is_some() {
            self.feed.reading = true;
            self.follow_turn();
        }
    }

    /// Into the selected entry's session when it runs here (the list gets
    /// the keys back); otherwise say where it's from.
    fn go_into_feed_session(&mut self) {
        let Some((session, machine)) = self.feed.selected_card().map(|card| {
            (
                SessionId::from_string(card.session_id().to_owned()),
                card.machine().to_owned(),
            )
        }) else {
            return;
        };
        if self.list.get(&session).is_some() {
            self.drawer_focused = false;
            self.go_into(session);
            return;
        }
        let place = if machine == self.feed.machine() {
            "It came from a session that's gone.".to_owned()
        } else {
            format!("It came from {machine}; its session runs there.")
        };
        self.set_status(place);
    }

    /// The selected card's session when it runs here: what the footer
    /// names while the feed has the keys.
    pub fn feed_session(&self) -> Option<&session_core::model::SessionView> {
        let card = self.feed.selected_card()?;
        self.list
            .get(&SessionId::from_string(card.session_id().to_owned()))
    }

    /// Rows of the area the selected reply is read in, for paging.
    fn feed_page_rows(&self) -> isize {
        let area = self
            .feed_columns()
            .map(|(_, _, reply)| reply)
            .or(self.screen().feed);
        area.map_or(20, |area| (area.height.saturating_sub(4) as isize).max(1))
    }
}
