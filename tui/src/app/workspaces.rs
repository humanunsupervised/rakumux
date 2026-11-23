//! Workspaces: named groups of this machine's sessions, which the daemon
//! keeps (capability `workspaces`). The list shows every session (All, where
//! a first start begins), the sessions in none (No workspace), or one
//! workspace's; the view shown is saved (`sessions.workspace`), so the next
//! start shows it again. New sessions start in the workspace shown (in none
//! under All and No workspace).
//!
//! The footer's tabs (All, each workspace, No workspace, each with its
//! sessions' count) switch the view; its `⊞`, `m` and Settings → Workspace
//! open the workspaces modal: All, every workspace, No workspace, then
//! `+ Add new workspace`. Enter (or a click) shows the row under the cursor,
//! or names a new workspace and shows it. `M` (Settings → Move to
//! workspace) opens it for the selected session, without All: Enter moves
//! the session there (into a new one from `+ New workspace`). `e` renames
//! the workspace under the cursor (its name turns into a field), and `d`
//! twice deletes it: its sessions are then in none. The key line's
//! `e rename` and `d delete` take a click too.

use session_core::api::{ApiError, CAPABILITY_WORKSPACES};
use session_core::client::SessionApi;
use session_core::ids::{SessionId, WorkspaceId};
use session_core::model::SessionView;
use session_core::workspace::{checked_name, Workspace};

use super::runtime::AppEvent;
use super::text_input::TextInput;
use super::types::{AppMode, Focus};
use super::{App, WorkspaceView};

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
    /// Every session (to show only).
    All,
    /// The sessions in no workspace.
    Unassigned,
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
    /// `naming` renames this workspace rather than making one.
    pub renaming: Option<WorkspaceId>,
    /// What went wrong last (a name taken, the daemon's error).
    pub error: Option<String>,
}

/// What the sessions in no workspace are listed under.
pub const NO_WORKSPACE: &str = "No workspace";

impl App {
    /// The connected daemon keeps workspaces.
    pub fn has_workspaces(&self) -> bool {
        self.api()
            .and_then(|api| api.daemon_capabilities())
            .is_some_and(|capabilities| capabilities.iter().any(|c| c == CAPABILITY_WORKSPACES))
    }

    /// What the view shown is called: `All`, `No workspace` or the
    /// workspace's name.
    pub fn workspace_name(&self) -> String {
        self.view_name(self.list.workspace_view())
    }

    /// What `view` is called.
    pub fn view_name(&self, view: &WorkspaceView) -> String {
        match view {
            WorkspaceView::All => "All".to_owned(),
            WorkspaceView::Unassigned => NO_WORKSPACE.to_owned(),
            WorkspaceView::In(id) => self.name_of(Some(id)),
        }
    }

    /// The name of workspace `id`, or `No workspace`.
    fn name_of(&self, id: Option<&WorkspaceId>) -> String {
        id.and_then(|id| self.workspaces.iter().find(|w| &w.id == id))
            .map_or_else(|| NO_WORKSPACE.to_owned(), |w| w.name.clone())
    }

    /// A sync read the daemon's workspaces (`None`: it keeps none). A saved
    /// workspace that's gone shows All.
    pub(super) fn apply_workspaces(&mut self, workspaces: Option<Vec<Workspace>>) {
        self.workspaces = workspaces.unwrap_or_default();
        let gone = match self.list.workspace_view() {
            WorkspaceView::In(id) => !self.workspaces.iter().any(|w| &w.id == id),
            _ => false,
        };
        if gone {
            self.show_view(WorkspaceView::All);
        }
    }

    /// The views in the footer's order: All, every workspace, No workspace.
    pub fn workspace_views(&self) -> Vec<WorkspaceView> {
        std::iter::once(WorkspaceView::All)
            .chain(
                self.workspaces
                    .iter()
                    .map(|w| WorkspaceView::In(w.id.clone())),
            )
            .chain(std::iter::once(WorkspaceView::Unassigned))
            .collect()
    }

    /// The rows of the modal: All (to show only), every workspace,
    /// No workspace, then add one.
    pub fn workspace_rows(&self) -> Vec<WorkspaceRow> {
        let moving = matches!(
            &self.mode,
            AppMode::Workspaces(WorkspacePicker {
                purpose: WorkspacePurpose::Move { .. },
                ..
            })
        );
        (!moving)
            .then_some(WorkspaceRow::All)
            .into_iter()
            .chain((0..self.workspaces.len()).map(WorkspaceRow::Workspace))
            .chain([WorkspaceRow::Unassigned, WorkspaceRow::Add])
            .collect()
    }

    /// The view a row shows (none for `+ Add`).
    fn row_view(&self, row: &WorkspaceRow) -> Option<WorkspaceView> {
        match row {
            WorkspaceRow::All => Some(WorkspaceView::All),
            WorkspaceRow::Unassigned => Some(WorkspaceView::Unassigned),
            WorkspaceRow::Workspace(i) => self
                .workspaces
                .get(*i)
                .map(|w| WorkspaceView::In(w.id.clone())),
            WorkspaceRow::Add => None,
        }
    }

    /// The listed sessions `view` shows.
    pub fn view_count(&self, view: &WorkspaceView) -> usize {
        self.list.every_session().filter(|s| view.shows(s)).count()
    }

    /// `m`, the footer's `⊞`, Settings → Workspace: show another view, or
    /// add, rename or delete a workspace.
    pub fn open_workspaces(&mut self) {
        if self.refuse_workspaces() {
            return;
        }
        let current = self.list.workspace_view().clone();
        self.open_workspace_picker(WorkspacePurpose::Switch, &current);
    }

    /// A click on the footer's tab `index` (`workspace_views`' order).
    pub fn click_workspace_tab(&mut self, index: usize) {
        if let Some(view) = self.workspace_views().into_iter().nth(index) {
            self.pick_view(view);
        }
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
            self.set_status("A session on another machine is in no workspace here.");
            return;
        }
        let purpose = WorkspacePurpose::Move {
            session: session.id.clone(),
            name: session.name.clone(),
        };
        let at = match session.workspace {
            Some(id) => WorkspaceView::In(id),
            None => WorkspaceView::Unassigned,
        };
        self.open_workspace_picker(purpose, &at);
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

    fn open_workspace_picker(&mut self, purpose: WorkspacePurpose, at: &WorkspaceView) {
        if self.focus == Focus::Session {
            self.enter_rakumux_mode();
        }
        self.mode = AppMode::Workspaces(WorkspacePicker {
            purpose,
            cursor: 0,
            naming: None,
            deleting: None,
            renaming: None,
            error: None,
        });
        let cursor = self.row_of(at);
        if let AppMode::Workspaces(picker) = &mut self.mode {
            picker.cursor = cursor;
        }
        // Another window may have made or forgotten one meanwhile.
        if let Some(api) = self.api() {
            self.exec.job(move || AppEvent::Workspaces {
                result: api.workspaces(),
            });
        }
    }

    /// The modal's row showing `view` (its first without one).
    fn row_of(&self, view: &WorkspaceView) -> usize {
        self.workspace_rows()
            .iter()
            .position(|row| self.row_view(row).as_ref() == Some(view))
            .unwrap_or(0)
    }

    /// The daemon's workspaces, read as the modal opened.
    pub(super) fn on_workspaces(&mut self, result: Result<Vec<Workspace>, ApiError>) {
        let Ok(workspaces) = result else {
            return;
        };
        let under = self.picker_row().and_then(|row| self.row_view(&row));
        self.apply_workspaces(Some(workspaces));
        let rows = self.workspace_rows().len();
        let at = under.map(|view| self.row_of(&view));
        if let AppMode::Workspaces(picker) = &mut self.mode {
            picker.cursor = at.unwrap_or(picker.cursor).min(rows - 1);
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
        picker.renaming = None;
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
            match picker.renaming.clone() {
                Some(id) => self.rename_workspace(id, name),
                None => {
                    let purpose = picker.purpose.clone();
                    self.create_workspace(name, purpose);
                }
            }
            return;
        }
        let purpose = picker.purpose.clone();
        let Some(row) = self.picker_row() else {
            return;
        };
        let Some(view) = self.row_view(&row) else {
            if let AppMode::Workspaces(picker) = &mut self.mode {
                picker.naming = Some(TextInput::default());
                picker.deleting = None;
                picker.error = None;
            }
            return;
        };
        self.mode = AppMode::Normal;
        match (purpose, view) {
            (WorkspacePurpose::Switch, view) => self.pick_view(view),
            (WorkspacePurpose::Move { session, .. }, WorkspaceView::In(id)) => {
                self.move_session_to(session, Some(id))
            }
            (WorkspacePurpose::Move { session, .. }, _) => self.move_session_to(session, None),
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
            picker.renaming = None;
            self.workspace_enter();
        }
    }

    /// `e` on a workspace: its name turns into a field to rename it.
    pub(super) fn workspace_rename_key(&mut self) {
        let Some(WorkspaceRow::Workspace(index)) = self.picker_row() else {
            return;
        };
        let Some(workspace) = self.workspaces.get(index).cloned() else {
            return;
        };
        if let AppMode::Workspaces(picker) = &mut self.mode {
            picker.naming = Some(TextInput::new(workspace.name));
            picker.renaming = Some(workspace.id);
            picker.deleting = None;
            picker.error = None;
        }
    }

    fn rename_workspace(&mut self, id: WorkspaceId, name: String) {
        let Some(api) = self.api() else {
            return;
        };
        self.exec.job(move || AppEvent::WorkspaceRenamed {
            result: api.workspace_rename(&id, &name),
        });
    }

    /// A workspace was renamed from the modal: the modal shows it so.
    pub(super) fn on_workspace_renamed(&mut self, result: Result<Workspace, ApiError>) {
        let workspace = match result {
            Ok(workspace) => workspace,
            Err(error) => {
                match &mut self.mode {
                    AppMode::Workspaces(picker) => picker.error = Some(error.message),
                    _ => self.set_status(format!("Couldn't rename the workspace: {error}")),
                }
                return;
            }
        };
        if let Some(known) = self.workspaces.iter_mut().find(|w| w.id == workspace.id) {
            known.name = workspace.name.clone();
        }
        if let AppMode::Workspaces(picker) = &mut self.mode {
            if picker.renaming.as_ref() == Some(&workspace.id) {
                picker.naming = None;
                picker.renaming = None;
                picker.error = None;
            }
        }
        self.set_status(format!("Renamed to {}", workspace.name));
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
            WorkspacePurpose::Switch => self.pick_view(WorkspaceView::In(id)),
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
        let shown = self.list.workspace_view() == &WorkspaceView::In(workspace.id.clone());
        self.list.forget_workspace(&workspace.id);
        if shown {
            self.save_view();
            self.reconcile_detail();
        }
        let rows = self.workspace_rows().len();
        if let AppMode::Workspaces(picker) = &mut self.mode {
            picker.cursor = picker.cursor.min(rows - 1);
        }
        self.set_status(format!("Deleted workspace {}", workspace.name));
    }

    /// The user picked `view`: the left column shows its sessions.
    pub fn pick_view(&mut self, view: WorkspaceView) {
        self.show_sessions_tab();
        self.show_view(view);
    }

    /// The workspace session `id` is in, shown or not.
    pub fn workspace_of(&self, id: &SessionId) -> Option<&Workspace> {
        let workspace = self.list.get_anywhere(id)?.workspace.as_ref()?;
        self.workspaces.iter().find(|w| &w.id == workspace)
    }

    /// Show `view`'s sessions, and keep it for the next start.
    pub fn show_view(&mut self, view: WorkspaceView) {
        let changed = self.list.workspace_view() != &view;
        self.list.set_workspace_view(view.clone());
        self.save_view();
        if changed {
            self.reconcile_detail();
            let name = self.view_name(&view);
            self.set_status(name);
        }
    }

    /// Keep the view shown for the next start (`sessions.workspace`).
    fn save_view(&mut self) {
        let saved = self.list.workspace_view().saved();
        if self.settings.sessions.workspace != saved {
            self.settings.sessions.workspace = saved;
            if let Err(error) = self.save_settings() {
                if self.config_path.is_some() {
                    self.set_status(format!("Couldn't save settings: {error}"));
                }
            }
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

    /// The workspace new sessions start in: the one shown (none under All
    /// and No workspace).
    pub fn new_session_workspace(&self) -> Option<WorkspaceId> {
        match self.list.workspace_view() {
            WorkspaceView::In(id) if self.has_workspaces() => Some(id.clone()),
            _ => None,
        }
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

    /// `A` in Launch, `B` and `C` in none; started on All, or on `saved` as
    /// a previous start left it.
    fn with_workspaces(saved: Option<&str>) -> Harness {
        let mut a = view("A", "landing", 300);
        a.workspace = Some(launch().id);
        let fake = FakeApi::new(vec![a, view("B", "api", 200), view("C", "docs", 100)]);
        {
            let mut state = fake.state.lock().unwrap();
            state.workspaces = vec![launch()];
            state.keeps_workspaces = true;
        }
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

    fn footer(h: &mut Harness, cols: u16) -> String {
        draw(h, cols, 40).lines().last().unwrap().to_owned()
    }

    fn click(h: &mut Harness, hit: Hit) {
        let area = h
            .app
            .hits
            .iter()
            .find(|(_, candidate)| *candidate == hit)
            .map(|(area, _)| *area)
            .unwrap_or_else(|| panic!("no {hit:?}"));
        h.app.handle_mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: area.x,
            row: area.y,
            modifiers: KeyModifiers::NONE,
        });
        h.app.process_pending();
    }

    #[tokio::test]
    async fn all_shows_every_session_and_the_footer_s_tabs_switch() {
        let mut h = with_workspaces(None);
        assert_eq!(shown(&h), ["A", "B", "C"]);
        let line = footer(&mut h, 160);
        assert!(
            line.starts_with(" workspaces  ⊞   All 3   Launch 1   No workspace 2  "),
            "{line:?}"
        );
        assert!(!line.contains("session:"), "{line:?}");
        // In session mode too, on the blue bar.
        h.press(KeyCode::Enter).await;
        assert_eq!(h.app.focus, Focus::Session);
        let line = footer(&mut h, 160);
        assert!(
            line.starts_with(" workspaces  ⊞   All 3   Launch 1 "),
            "{line:?}"
        );
        assert!(line.contains("SESSION MODE"), "{line:?}");
        h.rakumux().await;
        click(&mut h, Hit::WorkspaceTab(1));
        assert_eq!(shown(&h), ["A"]);
        assert_eq!(saved(&h).as_deref(), Some("W1"));
        footer(&mut h, 160);
        click(&mut h, Hit::WorkspaceTab(2));
        assert_eq!(shown(&h), ["B", "C"]);
        assert_eq!(saved(&h).as_deref(), Some("unassigned"));
        footer(&mut h, 160);
        click(&mut h, Hit::WorkspaceTab(0));
        assert_eq!(shown(&h), ["A", "B", "C"]);
        assert_eq!(saved(&h), None);
        // The ⊞ opens the modal: All, each workspace, No workspace, then add.
        footer(&mut h, 160);
        click(&mut h, Hit::FooterKey(FooterKey::Workspace));
        let screen = draw(&mut h, 160, 40);
        let at = |text: &str| {
            screen
                .find(text)
                .unwrap_or_else(|| panic!("{text}: {screen}"))
        };
        assert!(at("✓ All") < at("Launch") && at("Launch") < at("No workspace"));
        assert!(at("No workspace") < at("+ Add new workspace"));
        h.press(KeyCode::Down).await;
        h.press(KeyCode::Enter).await;
        assert_eq!(shown(&h), ["A"]);
    }

    #[tokio::test]
    async fn the_next_start_shows_the_saved_view_and_a_gone_workspace_shows_all() {
        assert_eq!(shown(&with_workspaces(Some("W1"))), ["A"]);
        assert_eq!(shown(&with_workspaces(Some("unassigned"))), ["B", "C"]);
        assert_eq!(shown(&with_workspaces(Some("W9"))), ["A", "B", "C"]);
    }

    #[tokio::test]
    async fn tabs_that_don_t_fit_fold_into_a_count_that_opens_the_modal() {
        let mut h = with_workspaces(Some("unassigned"));
        for name in ["Research", "Marketing", "Infrastructure"] {
            h.fake.workspace_create(name).unwrap();
        }
        h.app.refresh();
        h.app.process_pending();
        let line = footer(&mut h, 160);
        assert!(
            line.contains("Infrastructure 0   No workspace 2"),
            "{line:?}"
        );
        let narrow = footer(&mut h, 110);
        assert!(
            narrow.contains("No workspace 2"),
            "the one shown stays: {narrow:?}"
        );
        assert!(narrow.contains('+'), "{narrow:?}");
        assert!(!narrow.contains("Infrastructure"), "{narrow:?}");
        click(&mut h, Hit::FooterKey(FooterKey::Workspace));
        assert!(matches!(h.app.mode, AppMode::Workspaces(_)));
    }

    #[tokio::test]
    async fn adding_a_workspace_shows_it_and_new_sessions_start_there() {
        let mut h = with_workspaces(None);
        h.press(KeyCode::Char('m')).await;
        for _ in 0..3 {
            h.press(KeyCode::Down).await;
        }
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
        // Under All and No workspace new sessions are in none.
        h.rakumux().await;
        h.app.show_view(WorkspaceView::All);
        assert_eq!(h.app.new_session_workspace(), None);
        // A name taken says so and stays open.
        h.press(KeyCode::Char('m')).await;
        for _ in 0..4 {
            h.press(KeyCode::Down).await;
        }
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
        let mut h = with_workspaces(Some("unassigned"));
        h.app
            .list
            .select(Selection::Session(SessionId::from_string("B")));
        h.press(KeyCode::Char('M')).await;
        let screen = draw(&mut h, 160, 40);
        assert!(screen.contains("Move api to"), "{screen}");
        assert!(screen.contains("+ New workspace"), "{screen}");
        assert!(
            !h.app.workspace_rows().contains(&WorkspaceRow::All),
            "no All when moving"
        );
        assert_eq!(picker(&h).cursor, 1, "on No workspace, where it is");
        h.press(KeyCode::Up).await;
        h.press(KeyCode::Enter).await;
        assert!(h.fake.calls().iter().any(|c| c == "set_workspace B W1"));
        assert_eq!(shown(&h), ["C"]);
        assert!(h
            .app
            .status()
            .unwrap_or_default()
            .contains("Moved api to Launch"));
        // Into a new one; under All it stays in view.
        h.app.show_view(WorkspaceView::All);
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
        assert_eq!(shown(&h), ["A", "B", "C"]);
    }

    #[tokio::test]
    async fn d_twice_deletes_a_workspace_and_its_sessions_are_unassigned() {
        let mut h = with_workspaces(Some("W1"));
        h.press(KeyCode::Char('m')).await;
        assert_eq!(picker(&h).cursor, 1);
        h.press(KeyCode::Char('d')).await;
        assert!(draw(&mut h, 160, 40).contains("d again deletes"));
        h.press(KeyCode::Char('d')).await;
        assert!(h.fake.calls().iter().any(|c| c == "workspace_delete W1"));
        assert!(h.app.workspaces.is_empty());
        assert_eq!(h.app.list.workspace_view(), &WorkspaceView::Unassigned);
        assert_eq!(shown(&h), ["A", "B", "C"]);
        assert_eq!(saved(&h).as_deref(), Some("unassigned"));
        // All and No workspace can't be deleted or renamed.
        for row in [0, 1] {
            if let AppMode::Workspaces(picker) = &mut h.app.mode {
                picker.cursor = row;
            }
            h.press(KeyCode::Char('d')).await;
            h.press(KeyCode::Char('d')).await;
            h.press(KeyCode::Char('e')).await;
            assert!(picker(&h).naming.is_none());
        }
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
    async fn e_renames_a_workspace_in_place_and_its_tab_follows() {
        let mut h = with_workspaces(Some("W1"));
        h.press(KeyCode::Char('m')).await;
        let screen = draw(&mut h, 160, 40);
        assert!(screen.contains("e rename"), "{screen}");
        assert!(screen.contains("d delete"), "{screen}");
        h.press(KeyCode::Char('e')).await;
        assert!(draw(&mut h, 160, 40).contains("❯ Launch"));
        for _ in 0.."Launch".len() {
            h.press(KeyCode::Backspace).await;
        }
        h.type_text("Ship").await;
        h.press(KeyCode::Enter).await;
        assert!(h
            .fake
            .calls()
            .iter()
            .any(|c| c == "workspace_rename W1 Ship"));
        assert!(picker(&h).naming.is_none(), "the modal stays open, renamed");
        assert_eq!(h.app.workspace_name(), "Ship");
        h.press(KeyCode::Esc).await;
        assert!(footer(&mut h, 160).contains("Ship 1"));
        // Esc while renaming leaves the name as it was.
        h.press(KeyCode::Char('m')).await;
        h.press(KeyCode::Char('e')).await;
        h.type_text(" day").await;
        h.press(KeyCode::Esc).await;
        assert_eq!(h.app.workspace_name(), "Ship");
        // Not on All.
        h.press(KeyCode::Up).await;
        assert!(!draw(&mut h, 160, 40).contains("e rename"));
    }

    #[tokio::test]
    async fn the_key_line_s_rename_and_delete_take_a_click() {
        let mut h = with_workspaces(Some("W1"));
        h.press(KeyCode::Char('m')).await;
        draw(&mut h, 160, 40);
        click(&mut h, Hit::WorkspaceDelete);
        draw(&mut h, 160, 40);
        click(&mut h, Hit::WorkspaceDelete);
        assert!(h.fake.calls().iter().any(|c| c == "workspace_delete W1"));
        assert_eq!(shown(&h), ["A", "B", "C"]);
    }

    #[tokio::test]
    async fn the_modal_fits_a_phone_and_a_click_on_a_row_switches() {
        let mut h = with_workspaces(None);
        h.app.open_workspaces();
        h.app.process_pending();
        let screen = draw(&mut h, 45, 30);
        assert!(screen.contains("No workspace"), "{screen}");
        click(&mut h, Hit::WorkspaceRow(1));
        assert_eq!(shown(&h), ["A"]);
    }

    #[tokio::test]
    async fn without_the_capability_the_footer_names_the_session_and_m_points_to_a_restart() {
        let mut h = with_workspaces(None);
        h.fake.state.lock().unwrap().lacks = vec![CAPABILITY_WORKSPACES];
        assert!(footer(&mut h, 160).starts_with(" session: ●"));
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
        let mut h = with_workspaces(Some("unassigned"));
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
        assert_eq!(h.app.view_count(&WorkspaceView::In(launch().id)), 2);
        assert!(footer(&mut h, 160).contains("Launch 2   No workspace 1"));
    }
}
