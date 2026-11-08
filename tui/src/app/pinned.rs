//! The drawer's Pinned tab (`*` in Rakumux mode, or Settings → Pinned): the
//! pinned sessions alone, whatever the list's tab and filters show, in the
//! order the user gave them (`SessionList::pinned`, saved under
//! `sessions.section_order`). Moving through them previews each in the
//! session's pane, as the list does (one the list's filters hide stays out
//! of it until it's gone into); Enter goes in, `p` unpins, `J`/`K` or a drag
//! reorders, and the keys that act on the list's session act on the row
//! while that's the session in view.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use session_core::ids::SessionId;
use session_core::model::SessionView;

use super::drawer::DrawerTab;
use super::{App, SessionDrag};

/// A row's height: a blank line, the name, its repositories, its app.
pub const PINNED_ROW: u16 = 4;

/// The tab's selection, kept by session as sessions come and go.
#[derive(Debug, Default)]
pub struct PinnedView {
    /// The selected session; before any, the row shows the list's.
    selected: Option<SessionId>,
    /// Its place, which the next row takes when it's unpinned.
    place: usize,
}

impl App {
    /// `*` (or Settings' action): show the drawer on Pinned with the keys,
    /// switch to it, give it the keys, or hide the drawer.
    pub fn toggle_pinned(&mut self) {
        self.toggle_drawer(DrawerTab::Pinned);
    }

    /// Show the drawer on Pinned and give it the keys.
    pub fn open_pinned(&mut self) {
        self.open_drawer(DrawerTab::Pinned);
    }

    /// The selected row among the pinned sessions `rows`.
    pub fn pinned_index(&self, rows: &[&SessionView]) -> usize {
        let of = |id: &SessionId| rows.iter().position(|s| &s.id == id);
        match &self.pinned_view.selected {
            Some(id) => of(id).unwrap_or(self.pinned_view.place.min(rows.len().saturating_sub(1))),
            None => self.list.selected_id().and_then(of).unwrap_or(0),
        }
    }

    /// The selected row's session.
    pub fn pinned_session(&self) -> Option<&SessionView> {
        let rows = self.list.pinned();
        rows.get(self.pinned_index(&rows)).copied()
    }

    /// Select the row at `index` (clamped) and preview its session.
    fn select_pinned(&mut self, index: usize) {
        let rows = self.list.pinned();
        let index = index.min(rows.len().saturating_sub(1));
        let Some(id) = rows.get(index).map(|s| s.id.clone()) else {
            return;
        };
        self.pinned_view.selected = Some(id.clone());
        self.pinned_view.place = index;
        if self.list.can_show(&id) {
            self.select_session(id);
        }
    }

    /// Move the selection `delta` rows (clamped).
    pub fn pinned_move(&mut self, delta: isize) {
        let rows = self.list.pinned();
        let index = self.pinned_index(&rows).saturating_add_signed(delta);
        self.select_pinned(index);
    }

    /// Enter: into the selected row's session (the list gets the keys back).
    pub fn pinned_enter(&mut self) {
        let Some(id) = self.pinned_session().map(|s| s.id.clone()) else {
            return;
        };
        self.drawer_focused = false;
        self.go_into(id);
    }

    /// Unpin the session at row `index`; the next row takes its place.
    pub fn unpin_row(&mut self, index: usize) {
        let id = self.list.pinned().get(index).map(|s| s.id.clone());
        if let Some(id) = id {
            self.pinned_view.place = index;
            self.pin_session(id, false);
        }
    }

    /// `J`/`K`: move the selected row `delta` places.
    fn shift_pinned(&mut self, delta: isize) {
        let rows = self.list.pinned();
        let index = self.pinned_index(&rows);
        let Some(id) = rows.get(index).map(|s| s.id.clone()) else {
            return;
        };
        if self
            .list
            .move_pinned(&id, index.saturating_add_signed(delta))
        {
            self.pinned_view.selected = Some(id);
            self.save_session_order();
        }
    }

    /// A press on the row at `index`: Pinned gets the keys and the row is
    /// selected, ready to be dragged. Let go without moving on a row already
    /// selected goes into its session (a tap on a phone, at once).
    pub fn click_pinned_row(&mut self, index: usize) {
        let rows = self.list.pinned();
        let opens = self.drawer_has_keys()
            && self.drawer_tab() == DrawerTab::Pinned
            && self.pinned_index(&rows) == index;
        let id = rows.get(index).map(|s| s.id.clone());
        self.show_drawer_tab(DrawerTab::Pinned);
        self.select_pinned(index);
        if self.phone() {
            self.pinned_enter();
        } else if let Some(id) = id {
            self.drag = Some(SessionDrag::pinned(id, opens));
        }
    }

    /// The selected row's session is the one the list has selected, so the
    /// list's keys act on what the drawer shows.
    fn pinned_is_selected(&self) -> bool {
        let id = self.pinned_session().map(|s| &s.id);
        id.is_some() && id == self.list.selected_id()
    }

    /// Rakumux mode keys while Pinned has them; `false` leaves the key to
    /// the list's handler.
    pub fn pinned_key(&mut self, key: KeyEvent) -> bool {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let page = self.screen().feed.map_or(1, |area| {
            isize::from((area.height / PINNED_ROW).max(1) as i16)
        });
        match key.code {
            KeyCode::Char('d') if ctrl => self.pinned_move((page / 2).max(1)),
            KeyCode::Char('u') if ctrl => self.pinned_move(-(page / 2).max(1)),
            KeyCode::PageDown => self.pinned_move(page),
            KeyCode::PageUp => self.pinned_move(-page),
            _ if ctrl => return false,
            KeyCode::Down | KeyCode::Char('j') => self.pinned_move(1),
            KeyCode::Up | KeyCode::Char('k') => self.pinned_move(-1),
            KeyCode::Home => self.pinned_move(isize::MIN / 2),
            KeyCode::End | KeyCode::Char('G') => self.pinned_move(isize::MAX / 2),
            KeyCode::Char('g') => self.pending_key = Some('g'),
            KeyCode::Enter | KeyCode::Char('i' | 'l') => self.pinned_enter(),
            KeyCode::Esc => self.unfocus_drawer(),
            KeyCode::Char('p') => {
                let rows = self.list.pinned();
                let index = self.pinned_index(&rows);
                self.unpin_row(index);
            }
            KeyCode::Char('J') => self.shift_pinned(1),
            KeyCode::Char('K') => self.shift_pinned(-1),
            KeyCode::Char('r' | 's' | 'd' | 'x' | 'z') if self.pinned_is_selected() => {
                return false
            }
            KeyCode::Left
            | KeyCode::Right
            | KeyCode::Char(
                'h' | ' ' | 'r' | 's' | 'd' | 'x' | 'z' | 'c' | '/' | '{' | '}' | 'w' | 'W' | 'y',
            ) => {}
            _ => return false,
        }
        true
    }
}
