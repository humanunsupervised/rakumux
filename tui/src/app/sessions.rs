//! Session pipelines: keeping the list in sync with the daemon, showing the
//! selected session (observer attach or saved screen), focus (controller),
//! cycling, and the results of create/rename/delete/kill/revive.

use std::cmp::Ordering;
use std::path::Path;
use std::sync::Arc;

use base64::Engine;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;
use session_core::api::{
    ApiError, AttachRequest, ErrorCode, EventEnvelope, ReadRequest, ReadResult, ReadSource,
    ReviveSession, Role, SessionEvent, StreamCommand, StreamEvent, TextFormat,
};
use session_core::client::{AttachEvent, AttachStream, SessionApi};
use session_core::ids::SessionId;
use session_core::model::{Lifecycle, SessionStatus, SessionView};

use super::detail::{classify_output, Attachment, DetailView, OutputAction};
use super::folders::{self, SyncResult};
use super::grouping::{Grouping, Section, StatusGroup};
use super::runtime::{cancel_flag, AppEvent};
use super::screen::Screen;
use super::session_list::{ListChange, ListRow, Selection};
use super::types::*;
use super::{env_snapshot, App, DividerState, FooterKey, Hit, SessionDrag};
use crate::layout::{self, SessionLayout};
use crate::terminal_view::{self, TerminalView, DEFAULT_SCROLLBACK_ROWS};

/// Rows scrolled per mouse wheel step.
const WHEEL_ROWS: i32 = 3;

pub(super) fn decode_checkpoint(checkpoint: &str) -> Result<Vec<u8>, String> {
    base64::engine::general_purpose::STANDARD
        .decode(checkpoint)
        .map_err(|e| format!("invalid checkpoint from the daemon: {e}"))
}

/// How often a window that's being used asks for its size again while the
/// daemon refuses (someone types in the session elsewhere).
const CLAIM_INTERVAL: std::time::Duration = std::time::Duration::from_secs(1);

/// How long after its stream closed a session is gone back into when the
/// connection returns.
const RESUME_FOCUS_WITHIN: std::time::Duration = std::time::Duration::from_secs(120);

impl App {
    pub(super) fn api(&self) -> Option<Arc<dyn SessionApi>> {
        self.connection.api()
    }

    /// Whether `id` is the session this TUI runs in (nesting guard).
    pub fn is_self_session(&self, id: &SessionId) -> bool {
        self.self_session.as_ref() == Some(id) || self.list.get(id).is_some_and(|s| s.is_self)
    }

    // ------------------------------------------------------------ sync

    /// Load the folders, subscribe to every folder (or the ones an older
    /// daemon knows), then load the list (`folders::load`).
    pub(super) fn sync(&mut self) {
        let Some(api) = self.api() else { return };
        if let Some(cancel) = self.sync_cancel.take() {
            cancel.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        self.sync_generation += 1;
        let generation = self.sync_generation;
        let launch = self.scope.clone();
        self.exec.job(move || AppEvent::Synced {
            generation,
            result: folders::load(api.as_ref(), &launch),
        });
    }

    pub(super) fn on_synced(&mut self, generation: u64, result: Result<SyncResult, ApiError>) {
        if generation != self.sync_generation {
            return;
        }
        match result {
            Ok(synced) => {
                self.apply_synced_folders(synced.known, synced.support);
                self.list.replace_all(synced.sessions, synced.listed);
                if self.session_layout() == SessionLayout::Tabs {
                    self.select_default_tab();
                }
                let cancel = cancel_flag();
                self.sync_cancel = Some(cancel.clone());
                self.exec.pump_events(generation, synced.stream, cancel);
                // Re-show the selection; a focused live session stays
                // focused. An attach that takes focus (a session just
                // created) is left to land.
                let focusing = self.pending_focus.is_some()
                    && matches!(&self.detail.view, DetailView::Loading(id)
                        if self.list.selected_id() == Some(id));
                // The session the user was typing in when the connection
                // dropped, if it's still the selected one.
                let resume = self
                    .resume_focus
                    .take()
                    .filter(|(_, at)| at.elapsed() < RESUME_FOCUS_WITHIN)
                    .map(|(id, _)| id);
                match self.list.selected_session().cloned() {
                    _ if focusing => {}
                    Some(session)
                        if resume.as_ref() == Some(&session.id)
                            && session.status == SessionStatus::Live
                            && !self.is_self_session(&session.id) =>
                    {
                        self.attach(&session.id, Role::Controller)
                    }
                    Some(session)
                        if self.focus == Focus::Session
                            && session.status == SessionStatus::Live
                            && !self.is_self_session(&session.id) =>
                    {
                        self.attach(&session.id, Role::Controller)
                    }
                    _ => {
                        if self.focus == Focus::Session {
                            self.leave_session_focus(false);
                        }
                        self.show_selected();
                    }
                }
                let ids: Vec<SessionId> =
                    self.list.sessions().iter().map(|s| s.id.clone()).collect();
                for id in ids {
                    self.maybe_auto_name(&id);
                }
            }
            Err(error) => self.connection_lost(error.to_string()),
        }
    }

    pub(super) fn on_session_event(&mut self, envelope: EventEnvelope) {
        let id = envelope.session_id.clone();
        if let SessionEvent::Created { session } = &envelope.event {
            self.learn_folder(&session.scope);
        }
        if self.list.apply(&envelope) == ListChange::Ignored {
            return;
        }
        self.reconcile_detail();
        if self.detail.session_id() == Some(&id) {
            self.sync_terminal_size();
        }
        self.maybe_auto_name(&id);
        self.acknowledge_visible_completion();
    }

    /// Bring the detail pane in line with the selection after list changes:
    /// a removed session, a new selection, or a saved session that came
    /// back to life (revived by another client).
    pub(super) fn reconcile_detail(&mut self) {
        if self.browsing_grid() {
            self.sync_grid();
            return;
        }
        let selected = self.list.selected_session().cloned();
        let shown = self.shown_session();
        // Session mode needs its session selected; without it, back to
        // Rakumux mode, observing again.
        let lost_focus = self.focus == Focus::Session && selected.is_none();
        let retarget = lost_focus
            || match (&shown, &self.detail.view) {
                (None, DetailView::Empty) => false,
                (None, _) => true,
                (Some(session), view) if view.session_id() != Some(&session.id) => true,
                (Some(session), DetailView::Saved { .. }) => session.status == SessionStatus::Live,
                _ => false,
            };
        if retarget {
            if lost_focus {
                self.leave_session_focus(false);
            }
            self.show_selected();
        }
    }

    // ------------------------------------------------------------ detail

    /// The session the detail pane shows: the selected one. While the
    /// folder filter, a folder header or + New session is selected, the
    /// pane keeps a session in view: the one it shows already, else the one
    /// Rakumux mode came from, else the first shown. Only sessions the
    /// filters show count, so `None` when none is shown.
    pub(super) fn shown_session(&self) -> Option<SessionView> {
        if let Some(session) = self.list.selected_session() {
            return Some(session.clone());
        }
        let visible = self.list.visible();
        [self.detail.session_id(), self.came_from.as_ref()]
            .into_iter()
            .flatten()
            .find_map(|id| visible.iter().find(|s| &s.id == id))
            .or_else(|| visible.first())
            .map(|session| (*session).clone())
    }

    /// Show the selected session (or, with no session selected, the one
    /// [`App::shown_session`] keeps in view): observe it when live, its
    /// saved screen otherwise.
    pub(super) fn show_selected(&mut self) {
        if self.browsing_grid() {
            self.sync_grid();
            return;
        }
        match self.shown_session() {
            None => {
                self.detail.retarget(DetailView::Empty);
            }
            Some(session) => self.show_session(&session, Role::Observer),
        }
    }

    fn show_session(&mut self, session: &SessionView, role: Role) {
        if self.is_self_session(&session.id) {
            self.detail
                .retarget(DetailView::SelfSession(session.id.clone()));
            return;
        }
        match session.status {
            SessionStatus::Live => self.attach(&session.id, role),
            SessionStatus::Exited | SessionStatus::Dead => self.load_saved(&session.id),
            SessionStatus::Unknown => {
                self.detail.retarget(DetailView::Error {
                    id: session.id.clone(),
                    message: "The daemon reported a status this version doesn't know.".into(),
                });
            }
        }
    }

    fn attach(&mut self, id: &SessionId, role: Role) {
        let Some(api) = self.api() else {
            self.detail.retarget(DetailView::Error {
                id: id.clone(),
                message: "Not connected to the daemon.".into(),
            });
            return;
        };
        let generation = self.detail.retarget(DetailView::Loading(id.clone()));
        self.pending_focus = (role == Role::Controller).then_some(generation);
        if role == Role::Controller {
            self.grid.clear();
        }
        let (cols, rows) = self.terminal_size();
        let request = AttachRequest {
            id: id.clone(),
            role,
            cols,
            rows,
        };
        self.exec.job(move || AppEvent::Attached {
            generation,
            result: api.attach(request),
        });
    }

    pub(super) fn on_attached(&mut self, generation: u64, result: Result<AttachStream, ApiError>) {
        // A stale stream is dropped here, which detaches it.
        if generation != self.detail.generation {
            return;
        }
        let DetailView::Loading(id) = self.detail.view.clone() else {
            return;
        };
        let wants_focus = self.pending_focus == Some(generation);
        self.pending_focus = None;
        match result {
            Ok(stream) => {
                let attached = stream.attached.clone();
                let checkpoint = match decode_checkpoint(&attached.checkpoint) {
                    Ok(bytes) => bytes,
                    Err(message) => {
                        self.detail.view = DetailView::Error { id, message };
                        if wants_focus {
                            self.leave_session_focus(false);
                        }
                        return;
                    }
                };
                let mut terminal = TerminalView::new(attached.rows, attached.cols);
                terminal.reset(attached.rows, attached.cols, &checkpoint);
                self.list.upsert(attached.session.clone());
                let cancel = cancel_flag();
                let mut attachment = Attachment::new(
                    id.clone(),
                    generation,
                    attached.role,
                    stream.sender(),
                    cancel.clone(),
                    attached.offset,
                );
                if attached.role == Role::Controller {
                    attachment.sent_size = Some((attached.cols, attached.rows));
                }
                self.detail.attachment = Some(attachment);
                self.detail.terminal = Some(terminal);
                self.detail.view = DetailView::Live(id);
                self.exec.pump_attach(generation, stream, cancel);
                if wants_focus {
                    self.enter_session_focus();
                } else {
                    self.sync_terminal_size();
                }
            }
            Err(error) if error.code == ErrorCode::SessionNotLive => {
                if wants_focus {
                    self.leave_session_focus(false);
                }
                self.load_saved(&id);
            }
            Err(error) => {
                if wants_focus {
                    self.leave_session_focus(false);
                }
                self.detail.view = DetailView::Error {
                    id,
                    message: format!("Can't attach: {error}"),
                };
            }
        }
    }

    pub(super) fn on_attach_event(&mut self, generation: u64, event: AttachEvent) {
        let Some(attachment) = self.detail.attachment.as_mut() else {
            return;
        };
        if attachment.generation != generation {
            return;
        }
        match event {
            AttachEvent::Output { offset, bytes } => {
                match classify_output(attachment.offset, offset, bytes.len()) {
                    OutputAction::Feed { skip } => {
                        attachment.offset = offset + bytes.len() as u64;
                        if let Some(terminal) = self.detail.terminal.as_mut() {
                            terminal.feed(&bytes[skip..]);
                        }
                    }
                    OutputAction::Duplicate => {}
                    OutputAction::Gap => self.resync_attachment(),
                }
            }
            AttachEvent::Event(event) => self.on_stream_event(event),
            AttachEvent::Closed(error) => {
                let id = attachment.id.clone();
                self.detail.attachment = None;
                if self.focus == Focus::Session {
                    // A server restart or lost connection: go back in once
                    // the session shows again.
                    self.resume_focus = Some((id.clone(), std::time::Instant::now()));
                }
                if self.detail.view == DetailView::Live(id.clone()) {
                    let name = self.session_name(&id);
                    self.detail.view = DetailView::Error {
                        id,
                        message: match error {
                            Some(error) => format!("Disconnected from {name}: {error}"),
                            None => format!("Disconnected from {name}."),
                        },
                    };
                }
                if self.focus == Focus::Session {
                    self.leave_session_focus(false);
                }
            }
        }
    }

    fn on_stream_event(&mut self, event: StreamEvent) {
        let Some(attachment) = self.detail.attachment.as_mut() else {
            return;
        };
        match event {
            StreamEvent::Resized {
                cols,
                rows,
                checkpoint,
                offset,
            } => match decode_checkpoint(&checkpoint) {
                Ok(bytes) => {
                    attachment.offset = offset;
                    // Another window took the size (its user is the one
                    // working). Nobody fits back by itself, or two windows
                    // would make the program redraw over and over: this one
                    // takes the size back when its own user does something
                    // (`claim_terminal_size`).
                    if let Some(terminal) = self.detail.terminal.as_mut() {
                        terminal.reset(rows, cols, &bytes);
                    }
                }
                Err(message) => self.set_status(message),
            },
            StreamEvent::RoleChanged { role } => {
                // The answer to our own Promote/Demote: the role is already
                // what we asked for (maybe changed again since).
                if attachment.own_role_change(role) {
                    return;
                }
                attachment.role = role;
                let id = attachment.id.clone();
                if role == Role::Observer && self.focus == Focus::Session {
                    self.leave_session_focus(false);
                    let name = self.session_name(&id);
                    self.set_status(format!("Another client took control of {name}"));
                }
            }
            StreamEvent::Title { title } => self.detail.title = Some(title),
            StreamEvent::Clipboard { selection, data } => {
                if attachment.role == Role::Controller {
                    self.terminal_output
                        .extend_from_slice(format!("\x1b]52;{selection};{data}\x07").as_bytes());
                }
            }
            StreamEvent::Exited { code } => {
                let id = attachment.id.clone();
                // The user ended the program they were working in here
                // (`exit`, Ctrl-D, quitting the agent): the session goes
                // with it. One that ends in the background stays, its final
                // screen kept for a restart.
                let worked_in = attachment.role == Role::Controller && self.focus == Focus::Session;
                self.detail.attachment = None;
                self.list.mark_exited(&id, code);
                self.detail.view = DetailView::Saved {
                    id: id.clone(),
                    status: SessionStatus::Exited,
                    exit_code: code,
                };
                if self.focus == Focus::Session {
                    self.leave_session_focus(false);
                }
                let name = self.session_name(&id);
                let exited = match code {
                    Some(code) => format!("{name} exited with code {code}"),
                    None => format!("{name} exited"),
                };
                match self.api().filter(|_| worked_in) {
                    Some(api) => {
                        self.set_status(format!("{exited}; removing it…"));
                        self.exec.job(move || AppEvent::Deleted {
                            result: api.delete(&id),
                            id,
                        });
                    }
                    None => self.set_status(exited),
                }
            }
            StreamEvent::Lagged { .. } => self.resync_attachment(),
            StreamEvent::Cwd { .. } | StreamEvent::Bell => {}
        }
    }

    /// Output went missing: attach again with the same role.
    fn resync_attachment(&mut self) {
        if let Some(attachment) = self.detail.attachment.as_ref() {
            let (id, role) = (attachment.id.clone(), attachment.role);
            self.attach(&id, role);
        }
    }

    fn load_saved(&mut self, id: &SessionId) {
        let Some(api) = self.api() else {
            self.detail.retarget(DetailView::Error {
                id: id.clone(),
                message: "Not connected to the daemon.".into(),
            });
            return;
        };
        let generation = self.detail.retarget(DetailView::Loading(id.clone()));
        let request = ReadRequest {
            id: id.clone(),
            source: ReadSource::Recent,
            lines: Some(DEFAULT_SCROLLBACK_ROWS as u32),
            format: TextFormat::Ansi,
        };
        self.exec.job(move || AppEvent::SavedScreen {
            generation,
            result: api.read(request),
        });
    }

    pub(super) fn on_saved_screen(
        &mut self,
        generation: u64,
        result: Result<ReadResult, ApiError>,
    ) {
        if generation != self.detail.generation {
            return;
        }
        let DetailView::Loading(id) = self.detail.view.clone() else {
            return;
        };
        let (status, exit_code) = self
            .list
            .get(&id)
            .map(|s| (s.status, s.exit_code))
            .unwrap_or((SessionStatus::Dead, None));
        match result {
            Ok(read) => {
                let (cols, rows) = if read.cols > 0 && read.rows > 0 {
                    (read.cols, read.rows)
                } else {
                    self.terminal_size()
                };
                let mut terminal = TerminalView::new(rows, cols);
                terminal.reset_to_text(rows, cols, &read.text);
                self.detail.terminal = Some(terminal);
            }
            Err(error) => {
                self.set_status(format!("No saved output: {error}"));
            }
        }
        self.detail.view = DetailView::Saved {
            id,
            status,
            exit_code,
        };
    }

    // ------------------------------------------------------------ focus

    /// Enter / `l` / `→` / click: open the active tab's modal for the tabs
    /// (the Folders modal for All folders) and the App or Status modal for
    /// one of their headers, create immediately for a folder's or a
    /// worktree's header or "+ New session", focus a live session, revive a
    /// dead one, restart an exited one.
    pub fn activate_selected(&mut self) {
        match self.list.selection().clone() {
            Selection::Folders => return self.open_tab_picker(),
            Selection::SectionHeader(section) => {
                return self.open_section_picker(section.grouping(), Some(section))
            }
            Selection::ReadPeriodHeader(_) => {
                let done = Section::Status(StatusGroup::Done);
                return self.open_section_picker(Grouping::Status, Some(done));
            }
            Selection::FolderHeader(_)
            | Selection::WorktreeHeader(..)
            | Selection::NewSession
            | Selection::Session(_) => {}
        }
        let Some(session) = self.list.selected_session().cloned() else {
            self.open_new_session();
            return;
        };
        if self.is_self_session(&session.id) {
            self.set_status(
                "This is the session Rakumux runs in; it can't be shown inside itself.",
            );
            return;
        }
        match session.status {
            SessionStatus::Live => {
                let size = self.terminal_size();
                let promoted = self.detail.attachment_for_mut(&session.id).map(|a| {
                    a.change_role(StreamCommand::Promote {
                        cols: size.0,
                        rows: size.1,
                    })
                });
                match promoted {
                    Some(Ok(())) => {
                        if let Some(attachment) = self.detail.attachment.as_mut() {
                            attachment.role = Role::Controller;
                            attachment.sent_size = Some(size);
                        }
                        self.enter_session_focus();
                    }
                    Some(Err(_)) | None => self.attach(&session.id, Role::Controller),
                }
            }
            SessionStatus::Dead | SessionStatus::Exited => self.revive(&session),
            SessionStatus::Unknown => self.set_status("Unknown session status; can't open it."),
        }
    }

    fn enter_session_focus(&mut self) {
        self.demote_terminal_panel();
        self.came_from_terminal = false;
        self.grid.clear();
        self.focus = Focus::Session;
        self.pending_key = None;
        self.came_from = None;
        if let Some(id) = self.detail.session_id().cloned() {
            self.recent.entered(&id);
        }
        if let Some(terminal) = self.detail.terminal.as_mut() {
            terminal.scroll_to_bottom();
        }
        if let Some(id) = self.detail.session_id().cloned() {
            self.touch(&id);
        }
        self.sync_terminal_size();
    }

    /// Back to the list; `demote` gives up control.
    pub fn leave_session_focus(&mut self, demote: bool) {
        if demote {
            if let Some(attachment) = self.detail.attachment.as_mut() {
                if attachment.role == Role::Controller {
                    let _ = attachment.change_role(StreamCommand::Demote);
                    // The terminal keeps the size we gave it; only a
                    // different pane size (e.g. leaving zoom) needs a refit.
                    attachment.role = Role::Observer;
                }
            }
        }
        self.focus = Focus::List;
        self.zoomed = false;
        self.pending_focus = None;
        self.sync_terminal_size();
        self.sync_grid();
    }

    /// The Rakumux key in session mode (or a click on the list): give up
    /// control and switch to Rakumux mode, remembering the session to go
    /// back to.
    pub fn enter_rakumux_mode(&mut self) {
        if self.focus == Focus::Terminal {
            self.demote_terminal_panel();
            self.focus = Focus::List;
            self.came_from_terminal = true;
            self.came_from = None;
            self.sync_grid();
            return;
        }
        self.came_from_terminal = false;
        let from = self.detail.session_id().cloned();
        self.leave_session_focus(true);
        self.came_from = from;
    }

    /// Esc (with nothing to cancel) or the Rakumux key in Rakumux mode:
    /// back into the session Rakumux mode was entered from, or without one
    /// into the running session in view.
    pub fn return_to_session(&mut self) {
        if self.came_from_terminal && self.terminal_panel.open {
            self.focus_terminal_panel();
            return;
        }
        // Nothing to go back to (a fresh start, a reloaded UI): into the
        // running session in view instead, as Esc leaves Rakumux mode.
        let in_view = || {
            self.detail
                .session_id()
                .filter(|id| {
                    self.list
                        .get(id)
                        .is_some_and(|s| s.status == SessionStatus::Live)
                })
                .cloned()
        };
        let Some(id) = self.came_from.clone().or_else(in_view) else {
            self.set_status("No session to go back to: select one and press Enter.");
            return;
        };
        match self.list.get(&id).map(|s| s.status) {
            Some(SessionStatus::Live) => {
                // A filter that hides it would move the selection away.
                self.reveal(&id);
                self.list.select(Selection::Session(id));
                self.activate_selected();
            }
            Some(_) => {
                self.came_from = None;
                self.set_status("That session has ended; select it and press Enter to revive it.");
            }
            None => {
                self.came_from = None;
                self.set_status("That session is gone.");
            }
        }
    }

    /// Tab in Rakumux mode: into the session worked in before the
    /// current one.
    pub fn go_to_last_session(&mut self) {
        let last = self
            .recent
            .previous
            .clone()
            .filter(|id| self.list.get(id).is_some());
        match last {
            Some(id) => self.go_into(id),
            None => self.set_status("No other session worked in yet."),
        }
    }

    /// `1`-`9` in Rakumux mode: into the `n`th session shown (1-based).
    pub fn go_into_nth(&mut self, n: usize) {
        let id = self
            .list
            .visible()
            .get(n.wrapping_sub(1))
            .map(|s| s.id.clone());
        if let Some(id) = id {
            self.go_into(id);
        }
    }

    /// Select `id` and work in it (revive or restart it when it isn't live).
    fn go_into(&mut self, id: SessionId) {
        self.reveal(&id);
        self.list.select(Selection::Session(id));
        self.reconcile_detail();
        self.activate_selected();
    }

    /// Make sure session `id` is shown: under All folders a folder filter
    /// that hides its folder shows that folder too (under App or Status,
    /// selecting it shows it), and a text filter that hides it is cleared.
    fn reveal(&mut self, id: &SessionId) {
        let shown = |app: &Self| app.list.visible().iter().any(|s| &s.id == id);
        if shown(self) {
            return;
        }
        let Some(folder) = self.list.get(id).map(|s| s.scope.clone()) else {
            return;
        };
        let filter = self.list.folder_filter();
        let folder_tab = self.list.grouping() == Grouping::Folder;
        if folder_tab && !filter.is_empty() && !filter.contains(&folder) {
            let mut folders = filter.to_vec();
            folders.push(folder);
            self.list.set_folder_filter(folders);
        }
        if !self.list.matches_text(id) {
            self.list.filter.set("");
        }
    }

    /// `z` in Rakumux mode: work in the selected session with the list or
    /// tab bar hidden (the footer stays); leaving the session brings it back.
    pub fn zoom_into_selected(&mut self) {
        if self.list.selected_session().is_none() {
            return;
        }
        self.zoomed = true;
        self.activate_selected();
        self.sync_terminal_size();
    }

    /// `Ctrl-d` / `Ctrl-u`: move the selection half a list page.
    pub fn move_half_page(&mut self, direction: isize) {
        if self.browsing_grid() {
            self.move_grid_selection(direction * (self.grid_page().capacity as isize / 2).max(1));
            return;
        }
        // Sessions are three rows; a page is the list's height in them.
        let rows = self.screen().list.map_or(30, |list| list.height) as isize;
        let step = (rows / 3 / 2).max(1);
        self.move_selection(direction * step);
    }

    /// `{` / `}`: the first session of the previous or next group (folder,
    /// app or status; its header when it shows none).
    pub fn move_to_folder_group(&mut self, delta: isize) {
        if let Some(selection) = self.list.group_target(delta) {
            self.list.select(selection);
            self.reconcile_detail();
        }
    }

    /// `J` / `K`: move the selected session down or up one place, within
    /// its folder and among the pinned sessions or the others; the order
    /// is saved.
    pub fn move_selected_session(&mut self, delta: isize) {
        let Some(id) = self.list.selected_id().cloned() else {
            self.set_status("Select a session to move.");
            return;
        };
        if self.list.listed_by_last_read(&id) {
            self.set_status("Done lists sessions by when you last read them.");
            return;
        }
        let Some(from) = self.list.visible_position(&id) else {
            return;
        };
        let to = from.saturating_add_signed(delta);
        if self.list.move_session(&id, to) {
            self.save_session_order();
            self.sync_grid();
        }
    }

    /// `Ctrl-y`/`Ctrl-e`, PgUp/PgDn: scroll the previewed session's history
    /// (positive = back in time).
    pub fn scroll_preview(&mut self, delta: i32) {
        if self.browsing_grid() {
            self.move_grid_page(if delta > 0 { -1 } else { 1 });
            return;
        }
        if let Some(terminal) = self.detail.terminal.as_mut() {
            terminal.scroll(delta);
        }
    }

    /// Rows a page scroll of the preview moves: the preview's height (a
    /// stacked one is shorter than the session-mode pane).
    pub fn preview_page(&self) -> i32 {
        let rows = match self.screen().terminal {
            Some(terminal) => terminal.height,
            None => self.terminal_size().1,
        };
        i32::from(rows.saturating_sub(1).max(1))
    }

    /// `\\` in Rakumux mode: back into the session and send it the
    /// Rakumux key itself.
    pub fn send_rakumux_key(&mut self) {
        let Some(id) = self
            .came_from
            .clone()
            .or_else(|| self.list.selected_id().cloned())
        else {
            return;
        };
        if self.list.get(&id).map(|s| s.status) != Some(SessionStatus::Live) {
            self.set_status("Select a running session to send the Rakumux key to.");
            return;
        }
        self.reveal(&id);
        self.list.select(Selection::Session(id));
        self.activate_selected();
        if self.focus == Focus::Session {
            let key = self.rakumux_key.to_key_event();
            self.forward_key(&key);
        } else {
            self.set_status("Press it again once the session has opened.");
        }
    }

    /// Keep the session's terminal at this pane's size: always as
    /// controller (`Resize`), and as observer while nobody controls it
    /// (`Fit`), so a session no one is typing in fits the pane showing it
    /// instead of keeping a size from another layout or window.
    pub(super) fn sync_terminal_size(&mut self) {
        if self.focus == Focus::Terminal {
            return;
        }
        // Nothing of the session is on screen (a phone's list): leave its
        // size to the windows showing it.
        if self.screen().terminal.is_none() {
            return;
        }
        let size = self.terminal_size();
        let Some(attachment) = self.detail.attachment.as_mut() else {
            return;
        };
        if attachment.sent_size == Some(size) {
            return;
        }
        let (cols, rows) = size;
        // An observer's Fit applies unless another window's user is typing
        // in the session (the daemon decides).
        let command = match attachment.role {
            Role::Controller => StreamCommand::Resize { cols, rows },
            Role::Observer => StreamCommand::Fit { cols, rows },
        };
        if attachment.command(command).is_ok() {
            attachment.sent_size = Some(size);
        }
    }

    /// The person is using this window (a key, a click, the window coming
    /// to the front): the session it shows takes this window's size if it
    /// has another (another window, on another device, set it). The daemon
    /// refuses while someone types in the session elsewhere; this tries
    /// again at most every [`CLAIM_INTERVAL`].
    pub(super) fn claim_terminal_size(&mut self) {
        if self.focus == Focus::Terminal {
            return;
        }
        if self.screen().terminal.is_none() {
            return;
        }
        let size = self.terminal_size();
        let Some(shown) = self.detail.terminal.as_ref().map(|t| t.size()) else {
            return;
        };
        let Some(attachment) = self.detail.attachment.as_mut() else {
            return;
        };
        let (cols, rows) = size;
        if shown == (rows, cols) {
            return;
        }
        if attachment
            .last_claim
            .is_some_and(|at| at.elapsed() < CLAIM_INTERVAL)
        {
            return;
        }
        let command = match attachment.role {
            Role::Controller => StreamCommand::Resize { cols, rows },
            Role::Observer => StreamCommand::Fit { cols, rows },
        };
        if attachment.command(command).is_ok() {
            attachment.sent_size = Some(size);
            attachment.last_claim = Some(std::time::Instant::now());
        }
    }

    fn touch(&mut self, id: &SessionId) {
        let Some(api) = self.api() else { return };
        let id = id.clone();
        self.exec.job(move || AppEvent::Touched {
            result: api.touch(&id),
        });
    }

    /// Only the foreground, open conversation counts as reading a new reply.
    /// Passive previews, dialogs, the utility shell and scrolled history do not.
    pub(super) fn acknowledge_visible_completion(&mut self) {
        if self.focus != Focus::Session
            || !self.window_focused
            || !matches!(self.mode, AppMode::Normal)
            || !self
                .detail
                .terminal
                .as_ref()
                .is_some_and(|terminal| terminal.scroll_offset() == 0)
        {
            return;
        }
        let Some(attachment) = self.detail.attachment.as_ref() else {
            return;
        };
        if attachment.role == Role::Controller
            && self.list.get(&attachment.id).is_some_and(|session| {
                session.status == SessionStatus::Live && session.lifecycle == Lifecycle::Done
            })
        {
            self.touch(&attachment.id.clone());
        }
    }

    // ------------------------------------------------------------ navigation

    /// Select the previous (`-1`) or next (`1`) session in list order,
    /// wrapping. With session focus, the newly selected session takes focus
    /// too.
    pub fn cycle_session(&mut self, delta: i32) {
        let visible: Vec<SessionId> = self.list.visible().iter().map(|s| s.id.clone()).collect();
        if visible.is_empty() {
            return;
        }
        let current = self
            .list
            .selected_id()
            .and_then(|id| visible.iter().position(|v| v == id));
        let index = match current {
            Some(i) => layout::cycle(visible.len(), i, delta),
            None if delta > 0 => 0,
            None => visible.len() - 1,
        };
        self.select_session(visible[index].clone());
    }

    /// Select a session; keeps session focus when it can (live sessions).
    pub fn select_session(&mut self, id: SessionId) {
        if self.list.selected_id() == Some(&id) {
            return;
        }
        self.list.select(Selection::Session(id.clone()));
        if self.focus != Focus::Session {
            self.reconcile_detail();
            return;
        }
        let Some(session) = self.list.get(&id).cloned() else {
            return;
        };
        if session.status == SessionStatus::Live && !self.is_self_session(&id) {
            // Stay in session focus; the attach takes focus when it lands.
            self.attach(&id, Role::Controller);
        } else {
            self.leave_session_focus(false);
            self.show_selected();
        }
    }

    /// Move the selection with the list keys (`j`/`k`, arrows). Sidebar
    /// includes the "+ New session" row; Tabs only has sessions.
    pub fn move_selection(&mut self, delta: isize) {
        match self.session_layout() {
            SessionLayout::Grid => self.move_grid_selection(delta),
            SessionLayout::Sidebar => {
                if self.list.move_selection(delta) {
                    self.reconcile_detail();
                }
            }
            SessionLayout::Tabs => {
                let visible: Vec<SessionId> =
                    self.list.visible().iter().map(|s| s.id.clone()).collect();
                if visible.is_empty() {
                    return;
                }
                let current = self
                    .list
                    .selected_id()
                    .and_then(|id| visible.iter().position(|v| v == id))
                    .unwrap_or(0) as isize;
                let index = (current + delta).clamp(0, visible.len() as isize - 1) as usize;
                self.select_session(visible[index].clone());
            }
        }
    }

    /// Tabs have no folder filter row, folder headers or "+ New session"
    /// row: select a session when there is one.
    pub(super) fn select_default_tab(&mut self) {
        if self.list.selected_id().is_none() {
            if let Some(first) = self.list.visible().first().map(|s| s.id.clone()) {
                self.list.select(Selection::Session(first));
            }
        }
    }

    pub fn set_layout(&mut self, session_layout: SessionLayout) {
        if self.session_layout() == session_layout {
            return;
        }
        self.settings.sessions.layout = session_layout;
        if session_layout == SessionLayout::Tabs {
            self.select_default_tab();
        }
        self.sync_terminal_size();
        self.reconcile_detail();
        self.sync_grid();
    }

    // ------------------------------------------------------------ input

    /// Send a key to the focused session.
    pub fn forward_key(&mut self, key: &KeyEvent) {
        let (Some(terminal), Some(attachment)) = (
            self.detail.terminal.as_mut(),
            self.detail.attachment.as_ref(),
        ) else {
            return;
        };
        if attachment.role != Role::Controller {
            return;
        }
        let Some(bytes) = terminal_view::key_to_bytes(key, &terminal.modes()) else {
            return;
        };
        terminal.scroll_to_bottom();
        if let Err(error) = attachment.input(bytes) {
            self.set_status(format!("Input not delivered: {error}"));
        }
    }

    pub fn handle_paste(&mut self, text: &str) {
        if self.creating_session || self.pending_focus.is_some() {
            return;
        }
        if let AppMode::Folders(picker) = &mut self.mode {
            if picker.stage == super::types::PickerStage::Browse && !picker.busy {
                picker.query.insert_str(text);
                self.picker_query_changed();
            }
            return;
        }
        if let AppMode::Sections(picker) = &mut self.mode {
            picker.query.insert_str(text);
            self.section_query_changed();
            return;
        }
        match &mut self.mode {
            AppMode::Renaming(dialog) => dialog.input.insert_str(text),
            AppMode::AuthEmail(dialog) => dialog.input.insert_str(text),
            AppMode::AuthCode(dialog) => dialog.input.insert_str(text),
            AppMode::Normal if self.focus == Focus::Terminal => self.paste_terminal_panel(text),
            AppMode::Normal if self.focus == Focus::Session => {
                if let (Some(terminal), Some(attachment)) = (
                    self.detail.terminal.as_mut(),
                    self.detail.attachment.as_ref(),
                ) {
                    if attachment.role == Role::Controller {
                        terminal.scroll_to_bottom();
                        let bytes = terminal_view::paste_to_bytes(text, &terminal.modes());
                        if let Err(error) = attachment.input(bytes) {
                            self.set_status(format!("Paste not delivered: {error}"));
                        }
                    }
                }
            }
            AppMode::Normal if self.list.filter_editing => {
                self.list.filter.insert_str(text);
                self.list.ensure_selection_visible();
                self.reconcile_detail();
            }
            _ => {}
        }
    }

    pub fn handle_mouse(&mut self, event: MouseEvent) {
        if self.creating_session || self.pending_focus.is_some() {
            return;
        }
        if !matches!(self.mode, AppMode::Normal) {
            self.terminal_panel.dragging = false;
            // A dialog opened: a held session is let go.
            self.end_session_drag();
            if matches!(self.mode, AppMode::Folders(_)) {
                self.folder_picker_mouse(event);
                return;
            }
            if matches!(self.mode, AppMode::Sections(_)) {
                self.section_picker_mouse(event);
                return;
            }
            self.divider = DividerState::default();
            // The wheel moves Settings' selection and scrolls Help.
            if let AppMode::Modal { state, .. } = &mut self.mode {
                match event.kind {
                    MouseEventKind::ScrollUp => state.wheel(-1),
                    MouseEventKind::ScrollDown => state.wheel(1),
                    _ => {}
                }
            }
            return;
        }
        let screen = self.screen();
        let inside = |area: Option<Rect>| {
            area.is_some_and(|a| {
                event.column >= a.x
                    && event.column < a.x + a.width
                    && event.row >= a.y
                    && event.row < a.y + a.height
            })
        };
        if self.terminal_panel.dragging {
            match event.kind {
                MouseEventKind::Drag(MouseButton::Left) => self.drag_terminal_panel(event.row),
                MouseEventKind::Up(_) => self.terminal_panel.dragging = false,
                _ => {}
            }
            return;
        }
        if self.divider.dragging {
            match event.kind {
                MouseEventKind::Drag(MouseButton::Left) => self.drag_divider(event, &screen),
                MouseEventKind::Up(_) => self.drop_divider(screen.stacked),
                _ => {}
            }
            return;
        }
        if self.drag.is_some() {
            if event.kind == MouseEventKind::Drag(MouseButton::Left) {
                self.drag_session(event.column, event.row);
                return;
            }
            // Released, or the release went unseen: drop it where it is.
            self.end_session_drag();
            if matches!(event.kind, MouseEventKind::Up(_)) {
                return;
            }
        }
        self.divider.hover = inside(screen.divider);
        if self.divider.hover {
            if event.kind == MouseEventKind::Down(MouseButton::Left) {
                self.divider.dragging = true;
            }
            return;
        }
        if let MouseEventKind::Down(MouseButton::Left) = event.kind {
            if let Some(index) = layout::tab_at(&self.tab_hits, event.column, event.row) {
                let id = self.list.visible().get(index).map(|s| s.id.clone());
                if let Some(id) = id {
                    let from_panel = self.focus == Focus::Terminal;
                    if from_panel {
                        self.demote_terminal_panel();
                        self.focus = Focus::List;
                        self.came_from_terminal = false;
                    }
                    self.select_session(id.clone());
                    if from_panel {
                        self.activate_selected();
                    } else {
                        self.drag = Some(SessionDrag::new(id, event.column));
                    }
                }
                return;
            }
            let hit = self
                .hits
                .iter()
                .find(|(area, _)| inside(Some(*area)))
                .map(|(_, hit)| *hit);
            match hit {
                Some(Hit::TerminalPanelHide) => {
                    self.hide_terminal_panel();
                    return;
                }
                Some(Hit::TerminalPanelDivider) => {
                    self.terminal_panel.dragging = true;
                    return;
                }
                Some(Hit::TerminalPanel) => {
                    self.focus_terminal_panel();
                    return;
                }
                Some(Hit::GridCell(index)) => {
                    let id = self.list.visible().get(index).map(|s| s.id.clone());
                    self.list.select(match id {
                        Some(id) => Selection::Session(id),
                        None => Selection::NewSession,
                    });
                    self.activate_selected();
                    return;
                }
                Some(Hit::GridPage(delta)) => {
                    self.move_grid_page(delta);
                    return;
                }
                Some(Hit::ListRow(row)) => {
                    if self.focus != Focus::List {
                        self.enter_rakumux_mode();
                    }
                    self.list.select_row(row);
                    // New session is an action, not a preview selection:
                    // a click creates and enters it at every screen width.
                    if self.list.selection() == &Selection::NewSession {
                        self.activate_selected();
                        return;
                    }
                    self.reconcile_detail();
                    // A phone shows the list or a session: a tap on a
                    // session opens it rather than only selecting it.
                    if self.phone() {
                        let opens = matches!(self.list.rows().get(row), Some(ListRow::Session(_)));
                        if opens {
                            self.activate_selected();
                        }
                        return;
                    }
                    // A session row can be dragged to another place.
                    let held = match self.list.rows().get(row) {
                        Some(ListRow::Session(s)) => Some(s.id.clone()),
                        _ => None,
                    };
                    self.drag = held.map(|id| SessionDrag::new(id, event.row));
                    return;
                }
                Some(Hit::FolderSelector) => {
                    if self.focus != Focus::List {
                        self.enter_rakumux_mode();
                    }
                    self.open_tab_picker();
                    return;
                }
                // A tab: the active one opens its modal, another becomes
                // active. The top row is selected, so ←/→ go on from it.
                Some(Hit::ListTab(grouping)) => {
                    if self.focus != Focus::List {
                        self.enter_rakumux_mode();
                    }
                    self.list.select(Selection::Folders);
                    if grouping == self.list.grouping() {
                        self.open_tab_picker();
                    } else {
                        self.show_tab(grouping);
                    }
                    return;
                }
                Some(Hit::ModeBadge) => {
                    // Like the Rakumux key in session mode, Esc in
                    // Rakumux mode.
                    match self.focus {
                        Focus::Session | Focus::Terminal => self.enter_rakumux_mode(),
                        Focus::List => self.return_to_session(),
                    }
                    return;
                }
                Some(Hit::FooterKey(key)) => {
                    // What the button's keys do: Help and Settings are
                    // Rakumux mode keys.
                    if self.focus != Focus::List {
                        self.enter_rakumux_mode();
                    }
                    match key {
                        FooterKey::Rakumux => {}
                        FooterKey::Help => super::handlers::settings::open_help(self),
                        FooterKey::Settings => super::handlers::settings::open_settings(self),
                    }
                    return;
                }
                Some(
                    Hit::FolderMenu | Hit::FolderRow(_) | Hit::SectionMenu | Hit::SectionRow(_),
                )
                | None => {}
            }
        }
        if inside(screen.terminal_panel) {
            self.terminal_panel_mouse(event);
        } else if inside(screen.terminal) {
            self.terminal_mouse(event, screen.terminal.unwrap_or_default());
        } else if inside(screen.list) || inside(screen.grid) {
            match event.kind {
                MouseEventKind::ScrollUp => self.move_selection(-1),
                MouseEventKind::ScrollDown => self.move_selection(1),
                _ => {}
            }
        }
    }

    /// The Folders modal: a click on a row picks it (like Enter; a
    /// suggestion goes into the query, like Tab), the wheel moves the
    /// cursor, a click outside closes it.
    fn folder_picker_mouse(&mut self, event: MouseEvent) {
        let hit = self
            .hits
            .iter()
            .find(|(a, _)| {
                event.column >= a.x
                    && event.column < a.x + a.width
                    && event.row >= a.y
                    && event.row < a.y + a.height
            })
            .map(|(_, hit)| *hit);
        let browsing = matches!(&self.mode, AppMode::Folders(p)
            if p.stage == super::types::PickerStage::Browse && !p.busy);
        match event.kind {
            MouseEventKind::Down(MouseButton::Left) => match hit {
                Some(Hit::FolderRow(row)) if browsing => self.click_picker_row(row),
                Some(Hit::FolderRow(_) | Hit::FolderMenu) => {}
                _ => self.mode = AppMode::Normal,
            },
            MouseEventKind::ScrollUp if browsing => self.move_picker_cursor(-1),
            MouseEventKind::ScrollDown if browsing => self.move_picker_cursor(1),
            _ => {}
        }
    }

    /// The App or Status modal: a click on a row picks it (like Enter), the
    /// wheel moves the cursor, a click outside closes it.
    fn section_picker_mouse(&mut self, event: MouseEvent) {
        let hit = self
            .hits
            .iter()
            .find(|(a, _)| {
                event.column >= a.x
                    && event.column < a.x + a.width
                    && event.row >= a.y
                    && event.row < a.y + a.height
            })
            .map(|(_, hit)| *hit);
        match event.kind {
            MouseEventKind::Down(MouseButton::Left) => match hit {
                Some(Hit::SectionRow(row)) => self.click_section_row(row),
                Some(Hit::SectionMenu) => {}
                _ => self.mode = AppMode::Normal,
            },
            MouseEventKind::ScrollUp => self.move_section_cursor(-1),
            MouseEventKind::ScrollDown => self.move_section_cursor(1),
            _ => {}
        }
    }

    /// Move the divider to the pointer: side by side the list ends just
    /// left of it; stacked (narrow screens) just above it.
    fn drag_divider(&mut self, event: MouseEvent, screen: &Screen) {
        let Some(list) = screen.list else {
            return;
        };
        if screen.stacked {
            // The stacked panes fill from the list's top to the detail's
            // bottom.
            let total = screen.detail.map_or(list.bottom(), |d| d.bottom()) - list.y;
            let rows = event.row.saturating_sub(list.y);
            // The session-mode pane doesn't change: nothing to resize.
            self.settings.sessions.stacked_list_percent = Some(layout::list_percent(total, rows));
            return;
        }
        let width = layout::list_width(self.area.width, Some(event.column.saturating_sub(list.x)));
        if Some(width) != self.settings.sessions.sidebar_width {
            self.settings.sessions.sidebar_width = Some(width);
            self.sync_terminal_size();
        }
    }

    fn drop_divider(&mut self, stacked: bool) {
        self.divider = DividerState::default();
        if let Err(error) = self.save_settings() {
            let what = if stacked { "height" } else { "width" };
            self.set_status(format!("List {what} changed but not saved: {error}"));
        }
    }

    /// The session being dragged, once the pointer has moved.
    pub fn dragged_session(&self) -> Option<&SessionId> {
        self.drag.as_ref().filter(|d| d.started).map(|d| &d.id)
    }

    /// The pointer moved with a session held: it takes the place of the
    /// session under the pointer, if that's the way the pointer went, and
    /// stays within its part of the list.
    fn drag_session(&mut self, column: u16, row: u16) {
        let tabs = self.screen().tabs.is_some();
        let Some(drag) = self.drag.as_mut() else {
            return;
        };
        let position = if tabs { column } else { row };
        let direction = position.cmp(&drag.last);
        drag.last = position;
        drag.started = true;
        let id = drag.id.clone();
        let to = if tabs {
            self.tab_drop_position(column)
        } else {
            self.row_drop_position(row)
        };
        let (Some(from), Some(to)) = (self.list.visible_position(&id), to) else {
            return;
        };
        let along = match direction {
            Ordering::Greater => to > from,
            Ordering::Less => to < from,
            Ordering::Equal => false,
        };
        let moved = along && self.list.move_session(&id, to);
        if let Some(drag) = self.drag.as_mut() {
            drag.moved |= moved;
        }
    }

    /// Where a session dragged to screen row `row` goes (Sidebar): the
    /// place of the list row there; above the rows the first place, below
    /// them the last.
    fn row_drop_position(&self, row: u16) -> Option<usize> {
        let rows: Vec<(Rect, usize)> = self
            .hits
            .iter()
            .filter_map(|(area, hit)| match hit {
                Hit::ListRow(index) => Some((*area, *index)),
                _ => None,
            })
            .collect();
        if let Some((_, index)) = rows.iter().find(|(a, _)| row >= a.y && row < a.bottom()) {
            return Some(self.list.position_at_row(*index));
        }
        let top = rows.iter().map(|(a, _)| a.y).min()?;
        Some(if row < top { 0 } else { usize::MAX })
    }

    /// Where a session dragged to screen column `column` goes (Tabs): the
    /// place of the tab there; left of the tabs the first place, right of
    /// them the last; nowhere between two tabs.
    fn tab_drop_position(&self, column: u16) -> Option<usize> {
        let hits = &self.tab_hits;
        if let Some((index, _)) = hits
            .iter()
            .find(|(_, a)| column >= a.x && column < a.right())
        {
            return Some(*index);
        }
        let left = hits.iter().map(|(_, a)| a.x).min()?;
        let right = hits.iter().map(|(_, a)| a.right()).max()?;
        if column < left {
            Some(0)
        } else if column >= right {
            Some(usize::MAX)
        } else {
            None
        }
    }

    /// Let go of a dragged session; the new order is saved.
    fn end_session_drag(&mut self) {
        if self.drag.take().is_some_and(|drag| drag.moved) {
            self.save_session_order();
        }
    }

    /// Save the order of the sessions (`sessions.session_order`, and the
    /// apps' and statuses' `sessions.section_order`).
    fn save_session_order(&mut self) {
        self.settings.sessions.session_order = self.list.session_order();
        self.settings.sessions.section_order = self.list.section_order();
        if let Err(error) = self.save_settings() {
            self.set_status(format!("Order changed but not saved: {error}"));
        }
    }

    /// A click on the previewed session in Rakumux mode: go into it when
    /// it's running.
    fn click_preview(&mut self) {
        let live = self
            .list
            .selected_session()
            .is_some_and(|s| s.status == SessionStatus::Live);
        if live {
            self.activate_selected();
        }
    }

    fn terminal_mouse(&mut self, event: MouseEvent, pane: Rect) {
        let wheel = match event.kind {
            MouseEventKind::ScrollUp => Some(WHEEL_ROWS),
            MouseEventKind::ScrollDown => Some(-WHEEL_ROWS),
            _ => None,
        };
        if self.focus != Focus::Session {
            match (event.kind, wheel) {
                (MouseEventKind::Down(MouseButton::Left), _) => self.click_preview(),
                (_, Some(rows)) => {
                    if let Some(terminal) = self.detail.terminal.as_mut() {
                        terminal.scroll(rows);
                    }
                }
                _ => {}
            }
            return;
        }
        let (Some(terminal), Some(attachment)) = (
            self.detail.terminal.as_mut(),
            self.detail.attachment.as_ref(),
        ) else {
            return;
        };
        if attachment.role != Role::Controller {
            if let Some(rows) = wheel {
                terminal.scroll(rows);
            }
            return;
        }
        if let Some(bytes) = terminal.mouse_bytes(&event, pane) {
            let _ = attachment.input(bytes);
            return;
        }
        let Some(rows) = wheel else { return };
        let modes = terminal.modes();
        if modes.alternate_screen && terminal.scroll_offset() == 0 {
            // Full-screen programs without mouse mode: wheel = arrow keys.
            let code = if rows > 0 { KeyCode::Up } else { KeyCode::Down };
            let key = KeyEvent::new(code, KeyModifiers::NONE);
            if let Some(bytes) = terminal_view::key_to_bytes(&key, &modes) {
                let _ = attachment.input(bytes.repeat(WHEEL_ROWS as usize));
            }
        } else {
            terminal.scroll(rows);
        }
    }

    // ------------------------------------------------------------ actions

    /// Create immediately in the current folder, then enter its shell.
    pub fn open_new_session(&mut self) {
        if self.creating_session {
            return;
        }
        if !self.connection.is_connected() {
            self.set_status("Not connected to the daemon yet.");
            return;
        }
        let Some(api) = self.connection.api() else {
            self.set_status("Not connected to the daemon.");
            return;
        };
        let request = super::new_session::request(
            self.current_folder(),
            self.list.new_session_cwd().map(Path::to_path_buf),
            env_snapshot(),
            self.terminal_size(),
        );
        self.creating_session = true;
        self.grid.clear();
        self.mode = AppMode::Normal;
        self.set_status("Creating session...");
        self.exec.job(move || AppEvent::Created {
            result: std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| api.create(request)))
                .unwrap_or_else(|_| {
                    Err(ApiError::new(
                        ErrorCode::Internal,
                        "session creation failed unexpectedly",
                    ))
                }),
        });
    }

    pub(super) fn on_created(&mut self, result: Result<SessionView, ApiError>) {
        self.creating_session = false;
        match result {
            Ok(session) => {
                let id = session.id.clone();
                self.learn_folder(&session.scope);
                if !self.list.is_listed(&session.scope) {
                    // An older daemon watches the folders it knew: watch
                    // this one too.
                    self.sync();
                }
                self.set_status(format!("Created {}", session.name));
                self.list.upsert(session.clone());
                // Hidden by a filter (its folder, or the text): show it.
                self.reveal(&id);
                self.list.select(Selection::Session(id.clone()));
                self.show_session(&session, Role::Controller);
            }
            Err(error) => self.set_status(format!("Create failed: {error}")),
        }
    }

    /// `p`: pin or unpin the selected session.
    pub fn toggle_pin(&mut self) {
        let Some(session) = self.list.selected_session() else {
            self.set_status("Select a session to pin.");
            return;
        };
        let (id, pinned) = (session.id.clone(), !session.pinned);
        let Some(api) = self.api() else {
            self.set_status("Not connected to the daemon.");
            return;
        };
        self.exec.job(move || AppEvent::Pinned {
            result: api.pin(&id, pinned),
            id,
            pinned,
        });
    }

    pub(super) fn on_pinned(
        &mut self,
        id: SessionId,
        pinned: bool,
        result: Result<SessionView, ApiError>,
    ) {
        match result {
            Ok(session) => {
                let verb = if session.pinned { "Pinned" } else { "Unpinned" };
                self.set_status(format!("{verb} {}", session.name));
                self.list.upsert(session);
            }
            Err(error) if error.code == ErrorCode::Unsupported => {
                self.set_status("This Rakumux server doesn't support pinning yet.")
            }
            Err(error) => {
                let verb = if pinned { "Pin" } else { "Unpin" };
                let name = self.session_name(&id);
                self.set_status(format!("{verb} {name} failed: {error}"));
            }
        }
    }

    /// Rename the selected session (`r`, `prefix r`).
    pub fn open_rename(&mut self) {
        let Some(session) = self.list.selected_session() else {
            self.set_status("Select a session to rename.");
            return;
        };
        self.mode = AppMode::Renaming(RenameDialog {
            session_id: session.id.clone(),
            current_name: session.name.clone(),
            input: super::TextInput::new(session.name.clone()),
            error_message: None,
            busy: false,
        });
    }

    pub(super) fn submit_rename(&mut self) {
        let AppMode::Renaming(dialog) = &mut self.mode else {
            return;
        };
        if dialog.busy {
            return;
        }
        let name = dialog.input.value().trim().to_string();
        if name.is_empty() {
            dialog.error_message = Some("Session name cannot be empty".into());
            return;
        }
        if name == dialog.current_name {
            self.mode = AppMode::Normal;
            return;
        }
        let Some(api) = self.connection.api() else {
            dialog.error_message = Some("Not connected to the daemon.".into());
            return;
        };
        dialog.busy = true;
        dialog.error_message = None;
        let id = dialog.session_id.clone();
        self.exec.job(move || AppEvent::Renamed {
            result: api.rename(&id, &name),
            id,
            automatic: false,
        });
    }

    pub(super) fn rename_automatically(&mut self, id: SessionId, name: String) {
        let Some(api) = self.api() else { return };
        self.exec.job(move || AppEvent::Renamed {
            result: api.rename(&id, &name),
            id,
            automatic: true,
        });
    }

    pub(super) fn on_renamed(
        &mut self,
        id: SessionId,
        result: Result<SessionView, ApiError>,
        automatic: bool,
    ) {
        let in_dialog = matches!(&self.mode, AppMode::Renaming(d) if d.session_id == id && d.busy && !automatic);
        match result {
            Ok(session) => {
                if in_dialog {
                    self.mode = AppMode::Normal;
                }
                let verb = if automatic { "Named" } else { "Renamed to" };
                self.set_status(format!("{verb} {}", session.name));
                self.list.upsert(session);
            }
            Err(error) => {
                if let (true, AppMode::Renaming(dialog)) = (in_dialog, &mut self.mode) {
                    dialog.busy = false;
                    dialog.error_message = Some(error.to_string());
                } else if automatic {
                    self.set_status(format!("AI rename failed: {error}"));
                } else {
                    self.set_status(format!("Rename failed: {error}"));
                }
            }
        }
    }

    /// `d`: delete the selected session (after confirmation).
    pub fn confirm_delete(&mut self) {
        let Some(session) = self.list.selected_session() else {
            return;
        };
        self.mode = AppMode::Confirm(ConfirmDialog {
            title: "Delete session".into(),
            message: format!(
                "Delete '{}'? Its process is ended and its saved output removed.",
                session.name
            ),
            action: ConfirmAction::DeleteSession {
                id: session.id.clone(),
                name: session.name.clone(),
            },
        });
    }

    /// `prefix q` / `x`: end the session's process (after confirmation).
    pub fn confirm_close(&mut self) {
        let Some(session) = self.list.selected_session() else {
            return;
        };
        if session.status != SessionStatus::Live {
            self.set_status(format!("{} isn't running.", session.name));
            return;
        }
        self.mode = AppMode::Confirm(ConfirmDialog {
            title: "Close session".into(),
            message: format!(
                "End the process in '{}'? The session stays in the list and can be restarted.",
                session.name
            ),
            action: ConfirmAction::CloseSession {
                id: session.id.clone(),
                name: session.name.clone(),
            },
        });
    }

    pub(super) fn run_confirmed(&mut self, action: ConfirmAction) {
        self.mode = AppMode::Normal;
        match action {
            ConfirmAction::RestartUi => self.restart_requested = true,
            ConfirmAction::DeleteSession { id, name } => {
                let Some(api) = self.api() else {
                    self.set_status("Not connected to the daemon.");
                    return;
                };
                self.set_status(format!("Deleting {name}…"));
                self.exec.job(move || AppEvent::Deleted {
                    result: api.delete(&id),
                    id,
                });
            }
            ConfirmAction::CloseSession { id, name } => {
                let Some(api) = self.api() else {
                    self.set_status("Not connected to the daemon.");
                    return;
                };
                self.set_status(format!("Closing {name}…"));
                self.exec.job(move || AppEvent::Killed {
                    result: api.kill(&id, false),
                    id,
                });
            }
        }
    }

    pub(super) fn on_deleted(&mut self, id: SessionId, result: Result<(), ApiError>) {
        match result {
            Ok(()) => {
                let name = self.session_name(&id);
                self.list.remove(&id);
                if self.detail.session_id() == Some(&id) && self.focus == Focus::Session {
                    self.leave_session_focus(false);
                }
                self.reconcile_detail();
                self.set_status(format!("Deleted {name}"));
            }
            Err(error) => self.set_status(format!("Delete failed: {error}")),
        }
    }

    pub(super) fn on_killed(&mut self, id: SessionId, result: Result<Vec<SessionId>, ApiError>) {
        match result {
            Ok(_) => {
                let name = self.session_name(&id);
                self.set_status(format!("Closed {name}"));
            }
            Err(error) => self.set_status(format!("Close failed: {error}")),
        }
    }

    fn revive(&mut self, session: &SessionView) {
        let Some(api) = self.api() else {
            self.set_status("Not connected to the daemon.");
            return;
        };
        let (cols, rows) = self.terminal_size();
        // Dead = lost unexpectedly: resume the agent. Exited = restart.
        let resume = session.status == SessionStatus::Dead;
        let request = ReviveSession {
            id: session.id.clone(),
            env: env_snapshot(),
            cols,
            rows,
            resume,
        };
        let verb = if resume { "Reviving" } else { "Restarting" };
        self.set_status(format!("{verb} {}…", session.name));
        let id = session.id.clone();
        self.exec.job(move || AppEvent::Revived {
            result: api.revive(request),
            id,
        });
    }

    pub(super) fn on_revived(&mut self, id: SessionId, result: Result<SessionView, ApiError>) {
        match result {
            Ok(session) => {
                self.set_status(format!("{} is running again", session.name));
                self.list.upsert(session.clone());
                if self.list.selected_id() == Some(&id) {
                    self.show_session(&session, Role::Controller);
                }
            }
            Err(error) => self.set_status(format!("Revive failed: {error}")),
        }
    }

    /// `c`: quit and let the shell wrapper `cd` into the session's folder.
    pub fn selected_cwd(&self) -> Option<String> {
        self.list
            .selected_session()
            .map(|s| s.cwd.to_string_lossy().into_owned())
    }

    pub(super) fn session_name(&self, id: &SessionId) -> String {
        self.list
            .get(id)
            .map(|s| s.name.clone())
            .unwrap_or_else(|| id.to_string())
    }
}
