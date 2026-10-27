//! The Feed view: Rakumux mode's other view besides the sessions (`f`, the
//! footer's Feed button, or Settings). The list's place shows the feed
//! (`components/feed.rs`) and, in Rakumux mode, the detail shows the
//! selected reply's whole turn instead of a session (read from the service
//! once it's shown, then kept). It's laid out as Master detail whatever the
//! layout: Grid and Tabs give way to it in Rakumux mode and come back in a
//! session and when it's left. It stays until it's left (`f`, Esc, the
//! footer, a grouping chosen) or a reply's session is opened (Enter), so
//! Rakumux mode comes back to it from a session; in Master detail it stays
//! beside the session too.
//!
//! The service is asked for changes every 2 s while the feed is on screen,
//! and every 10 s otherwise while the account's Feed switch is on (without
//! counting as showing it), so the footer's button can count the entries
//! that arrived since it was last on screen. Enter on a reply made here
//! goes into its session, and `r` writes back to it (`reply.rs`).

use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use rakumux_sensagent::feed::{FeedPage, FeedTurn};
use session_core::ids::SessionId;

use crate::app::runtime::AppEvent;
use crate::app::types::{AccountStatus, Focus};
use crate::app::App;
use crate::layout::SessionLayout;

/// How often the feed is asked for changes while it's shown.
const FEED_EVERY: Duration = Duration::from_secs(2);
/// How often otherwise, while the account's Feed switch is on: to count
/// what's new.
pub(in crate::app) const FEED_HIDDEN_EVERY: Duration = Duration::from_secs(10);

impl App {
    /// The feed is on screen: the Feed view in Rakumux mode, and beside a
    /// session in Master detail (Grid and Tabs show the session alone).
    pub fn feed_shown(&self) -> bool {
        self.feed_shown_for(self.focus)
    }

    fn feed_shown_for(&self, focus: Focus) -> bool {
        self.feed_view && (focus == Focus::List || self.session_layout() == SessionLayout::Sidebar)
    }

    /// The Feed view was chosen: Rakumux mode shows it rather than the
    /// sessions.
    #[cfg(test)]
    pub fn feed_view(&self) -> bool {
        self.feed_view
    }

    /// The layout the screen takes with `focus`: Master detail while the
    /// feed shows there, else the one in use.
    pub(in crate::app) fn shown_layout(&self, focus: Focus) -> SessionLayout {
        if self.feed_shown_for(focus) {
            SessionLayout::Sidebar
        } else {
            self.session_layout()
        }
    }

    /// The feed is on screen and the detail shows the selected reply rather
    /// than a session: Rakumux mode.
    pub fn feed_in_detail(&self) -> bool {
        self.feed_shown() && self.focus == Focus::List
    }

    /// `f`, the footer's Feed button or Settings: the Feed view, or back to
    /// the sessions.
    pub fn toggle_feed_view(&mut self) {
        if self.feed_view {
            self.close_feed_view();
        } else {
            self.open_feed_view();
        }
    }

    /// Show the Feed view: what was held is on screen now, and changes since
    /// are asked for at once.
    pub fn open_feed_view(&mut self) {
        if self.feed_view {
            return;
        }
        self.feed_view = true;
        self.feed.reading = false;
        self.feed.mark_seen();
        self.feed.due = Some(Instant::now());
        // Grid's previews stop while the feed covers them.
        self.sync_grid();
        self.feed_timers(Instant::now());
    }

    /// Back to the sessions, as the layout shows them.
    pub fn close_feed_view(&mut self) {
        if self.feed_view {
            self.leave_feed_view();
            self.sync_grid();
        }
    }

    /// Leave the Feed view (a session is being opened, or a grouping
    /// chosen, which bring the sessions back themselves).
    pub(in crate::app) fn leave_feed_view(&mut self) {
        if self.feed_view {
            self.feed.mark_seen();
        }
        self.feed_view = false;
        self.feed.reading = false;
    }

    /// Esc while the feed shows (the mode badge too): a reply read full
    /// screen goes back to the replies, else back to the sessions.
    pub fn feed_back(&mut self) {
        if self.feed.reading {
            self.feed.reading = false;
        } else {
            self.close_feed_view();
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
    /// footer's Feed button counts.
    pub fn feed_unseen(&self) -> usize {
        if self.feed_shown() || !self.feed_switch_on() {
            0
        } else {
            self.feed.unseen()
        }
    }

    /// The tick's feed work: ask for changes when due (while it's shown, or
    /// while the switch is on, to count what's new), and for the selected
    /// reply's turn while it's read.
    pub(in crate::app) fn feed_timers(&mut self, now: Instant) {
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

    /// Ask for the selected reply's turn while it's read (in the detail, or
    /// full screen on a phone), unless one is being asked for.
    fn follow_turn(&mut self) {
        if (self.feed_in_detail() || self.feed.reading) && !self.feed.turn_busy {
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

    /// Rakumux mode keys while the feed is shown; `false` leaves the key to
    /// the list's handler (`f` back to the sessions among them). Keys that
    /// act on the list's selected session do nothing here: no session is
    /// selected on screen.
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
            KeyCode::Enter | KeyCode::Char('i' | 'l') => self.open_feed_entry(),
            KeyCode::Char('r') => self.open_feed_reply(),
            KeyCode::Esc => self.feed_back(),
            // The feed has no tabs to switch.
            KeyCode::Left | KeyCode::Right | KeyCode::Char('h') => {}
            KeyCode::Char('p' | 's' | 'J' | 'K' | 'd' | 'x' | 'z' | 'c' | '/' | '{' | '}') => {}
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
        self.follow_turn();
    }

    /// A click on the reply at `index`: in Rakumux mode select it, and open
    /// it (as Enter does) when it was selected already or on a phone; from
    /// session mode (the feed beside the session) open it at once, so the
    /// click goes into its session rather than leaving the user in Rakumux
    /// mode with the reply where their session was.
    pub fn click_feed_entry(&mut self, index: usize) {
        let from_session = self.focus != Focus::List;
        if from_session {
            self.enter_rakumux_mode();
        }
        let again = self.feed.selected_index() == Some(index);
        self.feed.select(index);
        if again || from_session || self.phone() {
            self.open_feed_entry();
        }
        self.follow_turn();
    }

    /// Enter on a reply: into its session when it runs here (the sessions
    /// come back as they were grouped); on a phone, a reply from elsewhere
    /// fills the screen to be read; otherwise say where it's from.
    pub fn open_feed_entry(&mut self) {
        let Some((session, machine)) = self.feed.selected_card().map(|card| {
            (
                SessionId::from_string(card.session_id().to_owned()),
                card.machine().to_owned(),
            )
        }) else {
            return;
        };
        if self.list.get(&session).is_some() {
            self.leave_feed_view();
            self.go_into(session);
            return;
        }
        if self.phone() {
            self.feed.reading = true;
            self.follow_turn();
            return;
        }
        let place = if machine == self.feed.machine() {
            "It came from a session that's gone.".to_owned()
        } else {
            format!("It came from {machine}; its session runs there.")
        };
        self.set_status(place);
    }

    /// The feed entry's session when it runs here: what the footer names
    /// while the feed is shown.
    pub fn feed_session(&self) -> Option<&session_core::model::SessionView> {
        let card = self.feed.selected_card()?;
        self.list
            .get(&SessionId::from_string(card.session_id().to_owned()))
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
