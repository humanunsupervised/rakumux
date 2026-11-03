//! The session history modal (Settings → Session history, `H`): the
//! sessions deleted on this machine in the last 90 days, most recently
//! deleted first. It reads the whole history once as it opens (off the UI
//! thread, `SessionApi::history` page by page), and typing narrows it here.
//! Enter (or a click) brings the session under the cursor back
//! (`SessionApi::restore`, dead in its folder), revives it and goes into it,
//! as Enter on any dead session does. A daemon without `session.history`
//! gets a footer message to restart the server.

use session_core::api::{ApiError, CAPABILITY_HISTORY, MAX_HISTORY_LIMIT};
use session_core::client::SessionApi;
use session_core::ids::SessionId;
use session_core::model::{DeletedSession, SessionView};

use super::runtime::AppEvent;
use super::session_list::Selection;
use super::types::{AppMode, Focus, HistoryPicker};
use super::App;

/// The most pages of the history read as the modal opens.
const MAX_PAGES: usize = 20;

impl App {
    /// The connected daemon keeps deleted sessions (`session.history`).
    pub fn has_session_history(&self) -> bool {
        self.api()
            .and_then(|api| api.daemon_capabilities())
            .is_some_and(|capabilities| capabilities.iter().any(|c| c == CAPABILITY_HISTORY))
    }

    /// Open the session history and read it.
    pub fn open_history(&mut self) {
        let Some(api) = self.api() else {
            self.set_status("Not connected to the daemon.");
            return;
        };
        if !self.has_session_history() {
            self.set_status("This Rakumux server keeps no session history: restart it (S).");
            return;
        }
        if self.focus == Focus::Session {
            self.enter_rakumux_mode();
        }
        self.mode = AppMode::History(HistoryPicker {
            loading: true,
            ..HistoryPicker::default()
        });
        self.exec.job(move || AppEvent::History {
            result: read_history(api.as_ref()),
        });
    }

    pub(super) fn on_history(&mut self, result: Result<Vec<DeletedSession>, ApiError>) {
        let AppMode::History(picker) = &mut self.mode else {
            return;
        };
        if !picker.loading {
            return;
        }
        picker.loading = false;
        match result {
            Ok(sessions) => picker.sessions = sessions,
            Err(error) => picker.error = Some(error.message),
        }
        self.history_query_changed();
    }

    /// The query changed: filter again from the top.
    pub(super) fn history_query_changed(&mut self) {
        let AppMode::History(picker) = &mut self.mode else {
            return;
        };
        let words: Vec<String> = picker
            .query
            .value()
            .split_whitespace()
            .map(str::to_lowercase)
            .collect();
        let folders = &self.folders;
        picker.rows = picker
            .sessions
            .iter()
            .enumerate()
            .filter(|(_, session)| {
                let fields = search_fields(session, &folders.name_of(&session.scope));
                words
                    .iter()
                    .all(|word| fields.iter().any(|field| field.contains(word.as_str())))
            })
            .map(|(index, _)| index)
            .collect();
        picker.cursor = 0;
    }

    pub(super) fn move_history_cursor(&mut self, delta: isize) {
        if let AppMode::History(picker) = &mut self.mode {
            let last = picker.rows.len().saturating_sub(1) as isize;
            picker.cursor = (picker.cursor as isize + delta).clamp(0, last) as usize;
        }
    }

    /// Esc: clear what's typed, else close.
    pub(super) fn history_escape(&mut self) {
        let AppMode::History(picker) = &mut self.mode else {
            return;
        };
        if picker.query.is_empty() {
            self.mode = AppMode::Normal;
        } else {
            picker.query.set("");
            self.history_query_changed();
        }
    }

    /// Enter: bring the session under the cursor back, then go into it.
    pub(super) fn history_enter(&mut self) {
        let AppMode::History(picker) = &self.mode else {
            return;
        };
        let Some(session) = picker.current() else {
            return;
        };
        let (id, name) = (session.id.clone(), session.name.clone());
        let Some(api) = self.api() else {
            self.set_status("Not connected to the daemon.");
            return;
        };
        self.mode = AppMode::Normal;
        self.set_status(format!("Restoring {name}…"));
        self.exec.job(move || AppEvent::Restored {
            result: api.restore(&id),
        });
    }

    /// A click on a row: like Enter on it.
    pub(super) fn click_history_row(&mut self, row: usize) {
        let AppMode::History(picker) = &mut self.mode else {
            return;
        };
        if row < picker.rows.len() {
            picker.cursor = row;
            self.history_enter();
        }
    }

    /// Back in the list, dead: shown, selected and revived (its resume
    /// command brings its agent's conversation back), then gone into.
    pub(super) fn on_restored(&mut self, result: Result<SessionView, ApiError>) {
        let session = match result {
            Ok(session) => session,
            Err(error) => {
                self.set_status(format!("Restore failed: {error}"));
                return;
            }
        };
        let id: SessionId = session.id.clone();
        self.learn_folder(&session.scope);
        if !self.list.is_listed(&session.scope) {
            // An older daemon watches the folders it knew: watch this one too.
            self.sync();
        }
        self.list.upsert(session);
        self.reveal(&id);
        self.list.select(Selection::Session(id));
        self.activate_selected();
    }
}

/// The whole history, page after page.
fn read_history(api: &dyn SessionApi) -> Result<Vec<DeletedSession>, ApiError> {
    let mut sessions: Vec<DeletedSession> = Vec::new();
    for _ in 0..MAX_PAGES {
        let before = sessions.last().map(|session| session.id.clone());
        let page = api.history(None, Some(MAX_HISTORY_LIMIT), before.as_ref())?;
        sessions.extend(page.sessions);
        if !page.more {
            break;
        }
    }
    Ok(sessions)
}

/// What typing looks through: the name, the folder's name and path, the
/// agent, the branch and the repository, lowercase.
fn search_fields(session: &DeletedSession, folder: &str) -> Vec<String> {
    let git = session.git.as_ref();
    [
        Some(session.name.as_str()),
        Some(folder),
        session.scope.path().to_str(),
        session.agent.as_ref().map(|agent| agent.kind.as_str()),
        git.and_then(|git| git.branch.as_deref()),
        git.and_then(|git| git.repo.as_deref()),
    ]
    .into_iter()
    .flatten()
    .map(str::to_lowercase)
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::testing::{harness, view, Harness};
    use crate::app::types::SettingsItem;
    use crate::app::Hit;
    use crossterm::event::{KeyCode, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};

    /// Two sessions, `web` (selected: the most recent) and `api`, then
    /// `web` deleted with `d` and `y`.
    async fn with_web_deleted() -> Harness {
        let mut h = harness(vec![view("A", "web", 200), view("B", "api", 100)]);
        h.app
            .list
            .select(Selection::Session(SessionId::from_string("A")));
        h.press(KeyCode::Char('d')).await;
        h.press(KeyCode::Char('y')).await;
        assert!(h.app.list.get(&SessionId::from_string("A")).is_none());
        h
    }

    fn picker(h: &Harness) -> &HistoryPicker {
        match &h.app.mode {
            AppMode::History(picker) => picker,
            other => panic!("expected the session history, got {other:?}"),
        }
    }

    fn rows(h: &Harness) -> Vec<String> {
        let picker = picker(h);
        picker
            .rows
            .iter()
            .map(|index| picker.sessions[*index].name.clone())
            .collect()
    }

    fn draw(h: &mut Harness, cols: u16, rows: u16) -> String {
        let backend = ratatui::backend::TestBackend::new(cols, rows);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|f| crate::ui::draw(f, &mut h.app)).unwrap();
        let buffer = terminal.backend().buffer().clone();
        (0..rows)
            .map(|y| {
                (0..cols)
                    .map(|x| buffer.get(x, y).symbol().to_string())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[tokio::test]
    async fn typing_narrows_esc_clears_then_closes() {
        let mut h = with_web_deleted().await;
        h.press(KeyCode::Char('H')).await;
        assert!(!picker(&h).loading);
        assert_eq!(rows(&h), ["web"]);
        h.type_text("zz").await;
        assert!(rows(&h).is_empty());
        assert!(draw(&mut h, 80, 24).contains("No session matches"));
        h.press(KeyCode::Esc).await;
        assert_eq!(picker(&h).query.value(), "");
        assert_eq!(rows(&h), ["web"]);
        // The folder's name matches too.
        h.type_text("REPO").await;
        assert_eq!(rows(&h), ["web"]);
        h.press(KeyCode::Esc).await;
        h.press(KeyCode::Esc).await;
        assert!(matches!(h.app.mode, AppMode::Normal));
    }

    #[tokio::test]
    async fn enter_restores_revives_and_goes_into_the_session() {
        let mut h = with_web_deleted().await;
        h.press(KeyCode::Char('H')).await;
        h.press(KeyCode::Enter).await;
        assert!(matches!(h.app.mode, AppMode::Normal));
        let calls = h.fake.calls();
        let restore = calls.iter().position(|c| c == "restore A").unwrap();
        let revive = calls
            .iter()
            .position(|c| c.starts_with("revive A resume=true"))
            .unwrap();
        assert!(restore < revive);
        assert_eq!(h.selected().as_deref(), Some("A"));
        let (_, attach) = h.last_attach();
        assert_eq!(attach.id.as_str(), "A");
        assert_eq!(attach.role, session_core::api::Role::Controller);
    }

    #[tokio::test]
    async fn a_click_on_a_row_restores_it_and_the_modal_fits_a_phone() {
        let mut h = with_web_deleted().await;
        h.press(KeyCode::Char('H')).await;
        let screen = draw(&mut h, 45, 40);
        assert!(screen.contains("Session history"));
        assert!(screen.contains("web"));
        let area = h
            .app
            .hits
            .iter()
            .find(|(_, hit)| *hit == Hit::HistoryRow(0))
            .map(|(area, _)| *area)
            .expect("a row to click");
        h.app.handle_mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: area.x + 1,
            row: area.y,
            modifiers: KeyModifiers::NONE,
        });
        h.app.process_pending();
        assert!(h.fake.calls().iter().any(|c| c == "restore A"));
    }

    #[tokio::test]
    async fn an_empty_history_says_so() {
        let mut h = harness(vec![view("B", "api", 100)]);
        h.press(KeyCode::Char('H')).await;
        assert!(draw(&mut h, 80, 24).contains("No deleted sessions"));
        // Enter on nothing does nothing.
        h.press(KeyCode::Enter).await;
        assert!(matches!(h.app.mode, AppMode::History(_)));
    }

    #[tokio::test]
    async fn without_the_capability_the_row_is_greyed_and_points_to_a_restart() {
        let mut h = harness(vec![view("B", "api", 100)]);
        let row = |h: &Harness| {
            h.app.modal_context(false).general[SettingsItem::SessionHistory.index()].clone()
        };
        assert!(!row(&h).disabled);
        h.fake.state.lock().unwrap().lacks = vec![CAPABILITY_HISTORY];
        let greyed = row(&h);
        assert!(greyed.disabled);
        assert!(greyed.note.contains("Restart the server (S)"));
        h.press(KeyCode::Char('H')).await;
        assert!(matches!(h.app.mode, AppMode::Normal));
        assert!(h
            .app
            .status()
            .unwrap_or_default()
            .contains("restart it (S)"));
    }
}
