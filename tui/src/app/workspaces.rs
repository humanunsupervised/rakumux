//! Workspaces: named groups of this machine's sessions, which the daemon
//! keeps (capability `workspaces`). The list shows one workspace at a time,
//! or the sessions in none ("No workspace", where every start begins until
//! another is picked); the one shown is saved (`sessions.workspace`), so the
//! next start shows it again. New sessions start in it.
//!
//! The workspaces modal (`m`, the footer's workspace button, Settings →
//! Workspace) lists No workspace, every workspace with its sessions' count,
//! then `+ Add new workspace`; Enter (or a click) shows the one under the
//! cursor, or names a new one and shows it. `M` (Settings → Move to
//! workspace) opens it for the selected session: Enter moves the session
//! there (into a new one from `+ New workspace`). `d` twice forgets the
//! workspace under the cursor; its sessions are then in none.

use session_core::api::{ApiError, CAPABILITY_WORKSPACES};
use session_core::client::SessionApi;
use session_core::ids::{SessionId, WorkspaceId};
use session_core::model::SessionView;
use session_core::workspace::{checked_name, Workspace};

use super::runtime::AppEvent;
use super::text_input::TextInput;
use super::types::{AppMode, Focus};
use super::App;

/// What the workspaces modal is open for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkspacePurpose {
    /// Show another workspace.
    Switch,
    /// Move this session to another workspace.
    Move { session: SessionId, name: String },
}

/// A row of the workspaces modal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkspaceRow {
    /// The sessions in no workspace.
    None,
    /// `App::workspaces[i]`.
    Workspace(usize),
    /// Name a new workspace.
    Add,
}

/// The workspaces modal (`AppMode::Workspaces`).
#[derive(Debug, Clone)]
pub struct WorkspacePicker {
    pub purpose: WorkspacePurpose,
    /// Row under the cursor.
    pub cursor: usize,
    /// The new workspace's name being typed (`+ Add new workspace`).
    pub naming: Option<TextInput>,
    /// `d` was pressed once on this workspace: a second `d` forgets it.
    pub deleting: Option<WorkspaceId>,
    /// What went wrong last (a name taken, the daemon's error).
    pub error: Option<String>,
}

impl App {
    /// The connected daemon keeps workspaces.
    pub fn has_workspaces(&self) -> bool {
        self.api()
            .and_then(|api| api.daemon_capabilities())
            .is_some_and(|capabilities| capabilities.iter().any(|c| c == CAPABILITY_WORKSPACES))
    }

    /// The name of the workspace shown, or `No workspace`.
    pub fn workspace_name(&self) -> String {
        self.list
            .workspace()
            .and_then(|id| self.workspaces.iter().find(|w| &w.id == id))
            .map_or_else(|| "No workspace".to_owned(), |w| w.name.clone())
    }

    /// The name of workspace `id`, or `No workspace`.
    fn name_of(&self, id: Option<&WorkspaceId>) -> String {
        id.and_then(|id| self.workspaces.iter().find(|w| &w.id == id))
            .map_or_else(|| "No workspace".to_owned(), |w| w.name.clone())
    }

    /// A sync read the daemon's workspaces (`None`: it keeps none). A saved
    /// workspace that's gone shows the sessions in none.
    pub(super) fn apply_workspaces(&mut self, workspaces: Option<Vec<Workspace>>) {
        self.workspaces = workspaces.unwrap_or_default();
        let gone = self
            .list
            .workspace()
            .is_some_and(|id| !self.workspaces.iter().any(|w| &w.id == id));
        if gone {
            self.show_workspace(None);
        }
    }

    /// The rows of the modal: No workspace, every workspace, then add one.
    pub fn workspace_rows(&self) -> Vec<WorkspaceRow> {
        std::iter::once(WorkspaceRow::None)
            .chain((0..self.workspaces.len()).map(WorkspaceRow::Workspace))
            .chain(std::iter::once(WorkspaceRow::Add))
            .collect()
    }

    /// The listed sessions in workspace `id` (`None`: in none).
    pub fn workspace_count(&self, id: Option<&WorkspaceId>) -> usize {
        self.list
            .every_session()
            .filter(|s| s.workspace.as_ref() == id)
            .count()
    }

    /// `m`, the footer's button, Settings → Workspace: show another
    /// workspace.
    pub fn open_workspaces(&mut self) {
        if self.refuse_workspaces() {
            return;
        }
        let current = self.list.workspace().cloned();
        self.open_workspace_picker(WorkspacePurpose::Switch, current.as_ref());
    }

    /// `M`, Settings → Move to workspace: move the selected session.
    pub fn open_move_to_workspace(&mut self) {
        if self.refuse_workspaces() {
            return;
        }
        let Some(session) = self.list.selected_session().cloned() else {
            self.set_status("Select a session to move.");
            return;
        };
        if session.machine.is_some() {
            self.set_status("A session on another machine stays in No workspace here.");
            return;
        }
        let purpose = WorkspacePurpose::Move {
            session: session.id.clone(),
            name: session.name.clone(),
        };
        self.open_workspace_picker(purpose, session.workspace.as_ref());
    }

    /// Without a daemon or one that keeps workspaces: say so.
    fn refuse_workspaces(&mut self) -> bool {
        if self.api().is_none() {
            self.set_status("Not connected to the daemon.");
            return true;
        }
        if !self.has_workspaces() {
            self.set_status("This Rakumux server has no workspaces: restart it (S).");
            return true;
        }
        false
    }

    fn open_workspace_picker(&mut self, purpose: WorkspacePurpose, at: Option<&WorkspaceId>) {
        if self.focus == Focus::Session {
            self.enter_rakumux_mode();
        }
        let cursor = at
            .and_then(|id| self.workspaces.iter().position(|w| &w.id == id))
            .map_or(0, |index| index + 1);
        self.mode = AppMode::Workspaces(WorkspacePicker {
            purpose,
            cursor,
            naming: None,
            deleting: None,
            error: None,
        });
        // Another window may have made or forgotten one meanwhile.
        if let Some(api) = self.api() {
            self.exec.job(move || AppEvent::Workspaces {
                result: api.workspaces(),
            });
        }
    }

    /// The daemon's workspaces, read as the modal opened.
    pub(super) fn on_workspaces(&mut self, result: Result<Vec<Workspace>, ApiError>) {
        let Ok(workspaces) = result else {
            return;
        };
        let under = self.picker_row().and_then(|row| match row {
            WorkspaceRow::Workspace(index) => self.workspaces.get(index).map(|w| w.id.clone()),
            _ => None,
        });
        self.apply_workspaces(Some(workspaces));
        let rows = self.workspace_rows();
        let at = under.and_then(|id| self.workspaces.iter().position(|w| w.id == id));
        if let AppMode::Workspaces(picker) = &mut self.mode {
            picker.cursor = match at {
                Some(index) => index + 1,
                None => picker.cursor.min(rows.len() - 1),
            };
        }
    }

    fn picker_row(&self) -> Option<WorkspaceRow> {
        let AppMode::Workspaces(picker) = &self.mode else {
            return None;
        };
        self.workspace_rows().get(picker.cursor).cloned()
    }

    pub(super) fn move_workspace_cursor(&mut self, delta: isize) {
        let last = self.workspace_rows().len().saturating_sub(1) as isize;
        if let AppMode::Workspaces(picker) = &mut self.mode {
            if picker.naming.is_some() {
                return;
            }
            picker.cursor = (picker.cursor as isize + delta).clamp(0, last) as usize;
            picker.deleting = None;
            picker.error = None;
        }
    }

    /// Esc: stop naming, else close.
    pub(super) fn workspace_escape(&mut self) {
        let AppMode::Workspaces(picker) = &mut self.mode else {
            return;
        };
        if picker.naming.take().is_none() {
            self.mode = AppMode::Normal;
        } else {
            picker.error = None;
        }
    }

    /// Enter: show (or move to) the workspace under the cursor, start
    /// naming a new one, or make the one named.
    pub(super) fn workspace_enter(&mut self) {
        let AppMode::Workspaces(picker) = &mut self.mode else {
            return;
        };
        if let Some(naming) = &picker.naming {
            let Some(name) = checked_name(naming.value()) else {
                picker.error = Some("A name is one line of 1 to 64 characters.".into());
                return;
            };
            let purpose = picker.purpose.clone();
            self.create_workspace(name, purpose);
            return;
        }
        let purpose = picker.purpose.clone();
        let Some(row) = self.picker_row() else {
            return;
        };
        let target = match row {
            WorkspaceRow::Add => {
                if let AppMode::Workspaces(picker) = &mut self.mode {
                    picker.naming = Some(TextInput::default());
                    picker.deleting = None;
                    picker.error = None;
                }
                return;
            }
            WorkspaceRow::None => None,
            WorkspaceRow::Workspace(index) => self.workspaces.get(index).map(|w| w.id.clone()),
        };
        self.mode = AppMode::Normal;
        match purpose {
            WorkspacePurpose::Switch => self.show_workspace(target),
            WorkspacePurpose::Move { session, .. } => self.move_session_to(session, target),
        }
    }

    /// A click on row `row`: like Enter on it.
    pub(super) fn click_workspace_row(&mut self, row: usize) {
        let rows = self.workspace_rows().len();
        let AppMode::Workspaces(picker) = &mut self.mode else {
            return;
        };
        if row < rows {
            picker.cursor = row;
            picker.naming = None;
            self.workspace_enter();
        }
    }

    /// `d` on a workspace: once marks it, again forgets it.
    pub(super) fn workspace_delete_key(&mut self) {
        let Some(WorkspaceRow::Workspace(index)) = self.picker_row() else {
            return;
        };
        let Some(workspace) = self.workspaces.get(index).cloned() else {
            return;
        };
        let AppMode::Workspaces(picker) = &mut self.mode else {
            return;
        };
        if picker.deleting.as_ref() != Some(&workspace.id) {
            picker.deleting = Some(workspace.id);
            return;
        }
        picker.deleting = None;
        let Some(api) = self.api() else {
            return;
        };
        let id = workspace.id.clone();
        self.exec.job(move || AppEvent::WorkspaceDeleted {
            result: api.workspace_delete(&id).map(|()| workspace),
        });
    }

    /// Typing while naming a new workspace.
    pub(super) fn workspace_type(&mut self, key: &crossterm::event::KeyEvent) -> bool {
        let AppMode::Workspaces(picker) = &mut self.mode else {
            return false;
        };
        match &mut picker.naming {
            Some(naming) => {
                picker.error = None;
                naming.handle_key(key)
            }
            None => false,
        }
    }

    /// A paste while naming.
    pub(super) fn workspace_paste(&mut self, text: &str) -> bool {
        let AppMode::Workspaces(picker) = &mut self.mode else {
            return false;
        };
        match &mut picker.naming {
            Some(naming) => {
                naming.insert_str(&text.replace(['\r', '\n'], " "));
                true
            }
            None => false,
        }
    }

    fn create_workspace(&mut self, name: String, purpose: WorkspacePurpose) {
        let Some(api) = self.api() else {
            return;
        };
        self.exec.job(move || AppEvent::WorkspaceCreated {
            purpose,
            result: api.workspace_create(&name),
        });
    }

    /// A workspace was made from the modal: show it, or move the session
    /// there.
    pub(super) fn on_workspace_created(
        &mut self,
        purpose: WorkspacePurpose,
        result: Result<Workspace, ApiError>,
    ) {
        let workspace = match result {
            Ok(workspace) => workspace,
            Err(error) => {
                match &mut self.mode {
                    AppMode::Workspaces(picker) => picker.error = Some(error.message),
                    _ => self.set_status(format!("Couldn't make the workspace: {error}")),
                }
                return;
            }
        };
        if matches!(self.mode, AppMode::Workspaces(_)) {
            self.mode = AppMode::Normal;
        }
        let id = workspace.id.clone();
        if !self.workspaces.iter().any(|w| w.id == id) {
            self.workspaces.push(workspace);
        }
        match purpose {
            WorkspacePurpose::Switch => self.show_workspace(Some(id)),
            WorkspacePurpose::Move { session, .. } => self.move_session_to(session, Some(id)),
        }
    }

    pub(super) fn on_workspace_deleted(&mut self, result: Result<Workspace, ApiError>) {
        let workspace = match result {
            Ok(workspace) => workspace,
            Err(error) => {
                self.set_status(format!("Couldn't delete the workspace: {error}"));
                return;
            }
        };
        self.workspaces.retain(|w| w.id != workspace.id);
        if self.list.workspace() == Some(&workspace.id) {
            self.show_workspace(None);
        }
        if let AppMode::Workspaces(picker) = &mut self.mode {
            picker.cursor = picker.cursor.min(self.workspaces.len() + 1);
        }
        self.set_status(format!("Deleted workspace {}", workspace.name));
    }

    /// Show workspace `id`'s sessions (`None`: those in none), and keep it
    /// for the next start.
    pub fn show_workspace(&mut self, id: Option<WorkspaceId>) {
        let changed = self.list.workspace() != id.as_ref();
        self.list.set_workspace(id.clone());
        let saved = id.as_ref().map(|id| id.as_str().to_owned());
        if self.settings.sessions.workspace != saved {
            self.settings.sessions.workspace = saved;
            if let Err(error) = self.save_settings() {
                if self.config_path.is_some() {
                    self.set_status(format!("Couldn't save settings: {error}"));
                }
            }
        }
        if changed {
            self.reconcile_detail();
            let name = self.name_of(id.as_ref());
            self.set_status(name);
        }
    }

    /// Ask the daemon to put `session` in `workspace` (`None`: in none).
    fn move_session_to(&mut self, session: SessionId, workspace: Option<WorkspaceId>) {
        let Some(api) = self.api() else {
            return;
        };
        self.exec.job(move || AppEvent::WorkspaceSet {
            result: api.set_workspace(&session, workspace.as_ref()),
        });
    }

    /// The daemon moved a session: it leaves the list shown when it went
    /// to another workspace.
    pub(super) fn on_workspace_set(&mut self, result: Result<SessionView, ApiError>) {
        let session = match result {
            Ok(session) => session,
            Err(error) => {
                self.set_status(format!("Couldn't move the session: {error}"));
                return;
            }
        };
        let to = self.name_of(session.workspace.as_ref());
        let name = session.name.clone();
        self.list.upsert(session);
        self.reconcile_detail();
        self.set_status(format!("Moved {name} to {to}"));
    }

    /// The workspace new sessions start in: the one shown.
    pub fn new_session_workspace(&self) -> Option<WorkspaceId> {
        self.has_workspaces()
            .then(|| self.list.workspace().cloned())
            .flatten()
    }
}

/// The daemon's workspaces, when it keeps them.
pub fn read(api: &dyn SessionApi) -> Option<Vec<Workspace>> {
    let supported = api
        .daemon_capabilities()
        .is_some_and(|capabilities| capabilities.iter().any(|c| c == CAPABILITY_WORKSPACES));
    if !supported {
        return None;
    }
    api.workspaces().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::session_list::Selection;
    use crate::app::testing::{options, start, view, FakeApi, Harness};
    use crate::app::types::SettingsItem;
    use crate::app::{FooterKey, Hit};
    use crossterm::event::{KeyCode, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};

    fn launch() -> Workspace {
        Workspace {
            id: WorkspaceId::from_string("W1"),
            name: "Launch".into(),
            created_at: 1,
        }
    }

    /// `A` in Launch, `B` and `C` in no workspace; started on No workspace,
    /// or on `saved` as a previous start left it.
    fn with_workspaces(saved: Option<&str>) -> Harness {
        let mut a = view("A", "landing", 300);
        a.workspace = Some(launch().id);
        let fake = FakeApi::new(vec![a, view("B", "api", 200), view("C", "docs", 100)]);
        fake.state.lock().unwrap().workspaces = vec![launch()];
        let dir = tempfile::tempdir().unwrap();
        let mut options = options(&fake, crate::app::testing::scope(), &dir);
        options.settings.sessions.workspace = saved.map(str::to_owned);
        start(options, fake, dir)
    }

    /// The sessions the list shows, by id, sorted.
    fn shown(h: &Harness) -> Vec<String> {
        let mut ids: Vec<String> = h
            .app
            .list
            .visible()
            .iter()
            .map(|s| s.id.to_string())
            .collect();
        ids.sort();
        ids
    }

    fn saved(h: &Harness) -> Option<String> {
        let raw = std::fs::read_to_string(h.home.path().join("config.json")).ok()?;
        let json: serde_json::Value = serde_json::from_str(&raw).ok()?;
        json["sessions"]["workspace"].as_str().map(str::to_owned)
    }

    fn picker(h: &Harness) -> &WorkspacePicker {
        match &h.app.mode {
            AppMode::Workspaces(picker) => picker,
            other => panic!("expected the workspaces modal, got {other:?}"),
        }
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
    async fn no_workspace_shows_the_sessions_in_none_and_m_switches_to_another() {
        let mut h = with_workspaces(None);
        assert_eq!(shown(&h), ["B", "C"]);
        assert!(draw(&mut h, 160, 40).contains("m ⊞ No workspace"));
        h.press(KeyCode::Char('m')).await;
        assert_eq!(picker(&h).cursor, 0);
        let screen = draw(&mut h, 160, 40);
        assert!(screen.contains("Workspaces"), "{screen}");
        assert!(screen.contains("+ Add new workspace"), "{screen}");
        h.press(KeyCode::Down).await;
        h.press(KeyCode::Enter).await;
        assert!(matches!(h.app.mode, AppMode::Normal));
        assert_eq!(shown(&h), ["A"]);
        assert_eq!(h.selected().as_deref(), Some("A"));
        assert_eq!(saved(&h).as_deref(), Some("W1"));
        assert!(draw(&mut h, 160, 40).contains("m ⊞ Launch"));
        // Back to none: the saved workspace goes from the file.
        h.press(KeyCode::Char('m')).await;
        assert_eq!(picker(&h).cursor, 1, "the cursor starts on the one shown");
        h.press(KeyCode::Up).await;
        h.press(KeyCode::Enter).await;
        assert_eq!(shown(&h), ["B", "C"]);
        assert_eq!(saved(&h), None);
    }

    #[tokio::test]
    async fn the_next_start_shows_the_saved_workspace_and_a_gone_one_shows_none() {
        let h = with_workspaces(Some("W1"));
        assert_eq!(shown(&h), ["A"]);
        let gone = with_workspaces(Some("W9"));
        assert_eq!(shown(&gone), ["B", "C"]);
    }

    #[tokio::test]
    async fn adding_a_workspace_shows_it_and_new_sessions_start_there() {
        let mut h = with_workspaces(None);
        h.press(KeyCode::Char('m')).await;
        h.press(KeyCode::Down).await;
        h.press(KeyCode::Down).await;
        h.press(KeyCode::Enter).await;
        assert!(picker(&h).naming.is_some());
        h.type_text("Research").await;
        assert!(draw(&mut h, 160, 40).contains("❯ Research"));
        h.press(KeyCode::Enter).await;
        assert!(h
            .fake
            .calls()
            .iter()
            .any(|c| c == "workspace_create Research"));
        assert!(matches!(h.app.mode, AppMode::Normal));
        assert_eq!(h.app.workspace_name(), "Research");
        assert!(shown(&h).is_empty());
        h.press(KeyCode::Char('n')).await;
        let creates = h.fake.state.lock().unwrap().creates.clone();
        let research = h
            .app
            .workspaces
            .iter()
            .find(|w| w.name == "Research")
            .unwrap();
        assert_eq!(
            creates.last().unwrap().workspace.as_ref(),
            Some(&research.id)
        );
        assert_eq!(shown(&h).len(), 1);
        // A name taken says so and stays open.
        h.rakumux().await;
        h.press(KeyCode::Char('m')).await;
        h.press(KeyCode::Down).await;
        h.press(KeyCode::Down).await;
        h.press(KeyCode::Down).await;
        h.press(KeyCode::Enter).await;
        h.type_text("Launch").await;
        h.press(KeyCode::Enter).await;
        assert!(picker(&h)
            .error
            .as_deref()
            .unwrap_or("")
            .contains("already"));
        h.press(KeyCode::Esc).await;
        assert!(picker(&h).naming.is_none());
        h.press(KeyCode::Esc).await;
        assert!(matches!(h.app.mode, AppMode::Normal));
    }

    #[tokio::test]
    async fn shift_m_moves_the_selected_session_and_it_leaves_the_view() {
        let mut h = with_workspaces(None);
        h.app
            .list
            .select(Selection::Session(SessionId::from_string("B")));
        h.press(KeyCode::Char('M')).await;
        let screen = draw(&mut h, 160, 40);
        assert!(screen.contains("Move api to"), "{screen}");
        assert!(screen.contains("+ New workspace"), "{screen}");
        h.press(KeyCode::Down).await;
        h.press(KeyCode::Enter).await;
        assert!(h.fake.calls().iter().any(|c| c == "set_workspace B W1"));
        assert_eq!(shown(&h), ["C"]);
        assert_eq!(h.selected().as_deref(), Some("C"));
        assert!(h
            .app
            .status()
            .unwrap_or_default()
            .contains("Moved api to Launch"));
        // Into a new one, from the list of another workspace.
        h.app.show_workspace(Some(launch().id));
        assert_eq!(shown(&h), ["A", "B"]);
        h.app
            .list
            .select(Selection::Session(SessionId::from_string("A")));
        h.press(KeyCode::Char('M')).await;
        h.press(KeyCode::Down).await;
        h.press(KeyCode::Down).await;
        h.press(KeyCode::Enter).await;
        h.type_text("Later").await;
        h.press(KeyCode::Enter).await;
        let calls = h.fake.calls();
        assert!(calls.iter().any(|c| c == "workspace_create Later"));
        assert!(calls.iter().any(|c| c == "set_workspace A W2"));
        assert_eq!(shown(&h), ["B"]);
    }

    #[tokio::test]
    async fn d_twice_deletes_a_workspace_and_its_sessions_show_in_none() {
        let mut h = with_workspaces(Some("W1"));
        h.press(KeyCode::Char('m')).await;
        h.press(KeyCode::Char('d')).await;
        assert!(draw(&mut h, 160, 40).contains("d again deletes"));
        h.press(KeyCode::Char('d')).await;
        assert!(h.fake.calls().iter().any(|c| c == "workspace_delete W1"));
        assert!(h.app.workspaces.is_empty());
        assert_eq!(h.app.list.workspace(), None);
        // No workspace can't be deleted.
        h.press(KeyCode::Char('d')).await;
        h.press(KeyCode::Char('d')).await;
        assert_eq!(
            h.fake
                .calls()
                .iter()
                .filter(|c| c.starts_with("workspace_delete"))
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn the_footer_button_and_a_click_on_a_row_switch_and_fit_a_phone() {
        let mut h = with_workspaces(None);
        let buttons = crate::components::footer::buttons(&h.app);
        assert_eq!(buttons[3].target, FooterKey::Workspace);
        h.app.open_workspaces();
        h.app.process_pending();
        let screen = draw(&mut h, 45, 30);
        assert!(screen.contains("No workspace"), "{screen}");
        let area = h
            .app
            .hits
            .iter()
            .find(|(_, hit)| *hit == Hit::WorkspaceRow(1))
            .map(|(area, _)| *area)
            .expect("a row to click");
        h.app.handle_mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: area.x + 1,
            row: area.y,
            modifiers: KeyModifiers::NONE,
        });
        h.app.process_pending();
        assert_eq!(shown(&h), ["A"]);
    }

    #[tokio::test]
    async fn without_the_capability_there_is_no_button_and_m_points_to_a_restart() {
        let mut h = with_workspaces(None);
        h.fake.state.lock().unwrap().lacks = vec![CAPABILITY_WORKSPACES];
        let buttons = crate::components::footer::buttons(&h.app);
        assert!(buttons.iter().all(|b| b.target != FooterKey::Workspace));
        let row = h.app.modal_context(false).general[SettingsItem::Workspace.index()].clone();
        assert!(row.disabled);
        h.press(KeyCode::Char('m')).await;
        assert!(matches!(h.app.mode, AppMode::Normal));
        assert!(h
            .app
            .status()
            .unwrap_or_default()
            .contains("restart it (S)"));
    }

    #[tokio::test]
    async fn a_session_moved_elsewhere_by_another_client_leaves_the_view() {
        let mut h = with_workspaces(None);
        let mut moved = h
            .app
            .list
            .get(&SessionId::from_string("B"))
            .unwrap()
            .clone();
        moved.workspace = Some(launch().id);
        h.fake.emit(
            "B",
            session_core::api::SessionEvent::Updated { session: moved },
        );
        h.app.process_pending();
        assert_eq!(shown(&h), ["C"]);
        assert_eq!(h.app.workspace_count(Some(&launch().id)), 2);
        h.press(KeyCode::Char('m')).await;
        assert!(draw(&mut h, 160, 40).contains("Launch"));
    }
}
