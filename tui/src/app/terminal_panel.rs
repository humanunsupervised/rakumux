//! An independent daemon-owned shell below the main detail pane. Hiding
//! the panel detaches its view, never kills its session or running command.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, MouseEvent, MouseEventKind};
use session_core::api::{
    ApiError, AttachRequest, ErrorCode, ReadRequest, ReadResult, ReadSource, ReviveSession, Role,
    StreamCommand, StreamEvent, TextFormat,
};
use session_core::client::{AttachEvent, AttachStream};
use session_core::ids::SessionId;
use session_core::model::{ScopeRoot, SessionStatus, SessionView};

use super::detail::{classify_output, Attachment, Detail, DetailView, OutputAction};
use super::runtime::{cancel_flag, AppEvent};
use super::types::{AppMode, AppResult, Focus};
use super::App;
use crate::terminal_view::{self, TerminalView};

const DEFAULT_HEIGHT_PERCENT: u16 = 35;
const MAX_CELLS: usize = 500_000;
const MAX_CHECKPOINT: usize = 2 * 1024 * 1024;
const SCROLLBACK: usize = 500;

/// Window-local view state and opaque ids for independent daemon sessions.
/// None of this state owns or terminates a shell process.
pub struct TerminalPanel {
    pub open: bool,
    pub folder: Option<ScopeRoot>,
    pub detail: Detail,
    pub message: Option<String>,
    pub height_percent: u16,
    pub dragging: bool,
    /// Bindings are local to this window. Sessions themselves remain normal,
    /// durable Rakumux sessions, visible to every other client.
    bindings: HashMap<ScopeRoot, SessionId>,
    pending: Option<u64>,
    epoch: u64,
    needs_open: bool,
    reconnect: bool,
    create_missing: bool,
    retry_at: Option<Instant>,
}

impl Default for TerminalPanel {
    fn default() -> Self {
        Self {
            open: false,
            folder: None,
            detail: Detail::default(),
            message: None,
            height_percent: DEFAULT_HEIGHT_PERCENT,
            dragging: false,
            bindings: HashMap::new(),
            pending: None,
            epoch: 0,
            needs_open: false,
            reconnect: false,
            create_missing: true,
            retry_at: None,
        }
    }
}

impl TerminalPanel {
    fn retarget(&mut self, view: DetailView) -> u64 {
        self.epoch = self.epoch.wrapping_add(1);
        self.detail.retarget(view);
        self.detail.generation = self.epoch;
        self.epoch
    }
    pub fn busy(&self) -> bool {
        self.open && self.pending.is_some()
    }
    pub fn session_id(&self) -> Option<&SessionId> {
        self.detail.session_id()
    }
    pub fn is_panel_session(&self, id: &SessionId) -> bool {
        self.bindings.values().any(|target| target == id)
    }
    pub fn clear_attachment(&mut self) {
        self.retarget(DetailView::Empty);
    }
}

impl App {
    /// T opens/focuses the shell; T after leaving it with the Rakumux key
    /// hides it. A visible, unfocused panel can also be focused by clicking.
    pub fn toggle_terminal_panel(&mut self) {
        if self.terminal_panel.open && self.came_from_terminal {
            self.hide_terminal_panel();
            return;
        }
        self.show_terminal_panel();
    }

    /// Settings reveal/focus an existing shell without toggling it away.
    pub fn show_terminal_panel(&mut self) {
        if !self.terminal_panel.open {
            self.terminal_panel.folder = Some(self.current_folder());
            self.terminal_panel.open = true;
            self.terminal_panel.message = None;
            self.terminal_panel.needs_open = true;
            self.terminal_panel.create_missing = true;
        }
        self.focus_terminal_panel();
    }

    pub fn focus_terminal_panel(&mut self) {
        if !self.terminal_panel.open {
            return;
        }
        if self.focus == Focus::Session {
            self.leave_session_focus(true);
        }
        self.focus = Focus::Terminal;
        self.zoomed = false;
        self.came_from_terminal = false;
        self.pending_key = None;
        self.list.filter_editing = false;
        self.grid.clear();
        self.reconcile_detail();
        let size = self.terminal_panel_size();
        if let Some(attachment) = self.terminal_panel.detail.attachment.as_mut() {
            if attachment.role == Role::Observer {
                match attachment.change_role(StreamCommand::Promote {
                    cols: size.0,
                    rows: size.1,
                }) {
                    Ok(()) => {
                        attachment.role = Role::Controller;
                        attachment.sent_size = Some(size);
                    }
                    Err(error) => {
                        self.set_status(format!("Can't focus terminal: {error}"));
                        self.focus = Focus::List;
                    }
                }
            }
        }
        self.sync_terminal_panel();
        self.claim_panel_size();
    }

    pub fn hide_terminal_panel(&mut self) {
        self.terminal_panel.open = false;
        self.terminal_panel.needs_open = false;
        self.terminal_panel.dragging = false;
        self.terminal_panel.clear_attachment();
        self.came_from_terminal = false;
        if self.focus == Focus::Terminal {
            self.focus = Focus::List;
        }
        self.sync_terminal_size();
        self.reconcile_detail();
    }

    pub(super) fn demote_terminal_panel(&mut self) {
        if let Some(attachment) = self.terminal_panel.detail.attachment.as_mut() {
            if attachment.role == Role::Controller {
                if let Err(error) = attachment.change_role(StreamCommand::Demote) {
                    self.set_status(format!("Could not release terminal control: {error}"));
                    self.terminal_panel.detail.attachment = None;
                } else {
                    attachment.role = Role::Observer;
                }
            }
        }
    }

    pub(super) fn sync_terminal_panel(&mut self) {
        if self.terminal_panel.open
            && self.detail.session_id().is_some_and(|id| {
                self.terminal_panel.is_panel_session(id) && self.list.selected_id() != Some(id)
            })
        {
            self.detail.retarget(DetailView::Empty);
        }
        if !self.terminal_panel.open
            || !self.connection.is_connected()
            || self.terminal_panel.pending.is_some()
        {
            return;
        }
        if self.terminal_panel.reconnect {
            if self
                .terminal_panel
                .retry_at
                .is_some_and(|at| at > Instant::now())
            {
                return;
            }
            self.terminal_panel.reconnect = false;
            self.terminal_panel.needs_open = true;
            self.terminal_panel.create_missing = false;
        }
        if self.terminal_panel.needs_open {
            self.resolve_terminal_panel();
            return;
        }
        let Some(id) = self.terminal_panel.session_id().cloned() else {
            return;
        };
        if let Some(session) = self.list.get(&id).cloned() {
            if session.status == SessionStatus::Live
                && matches!(self.terminal_panel.detail.view, DetailView::Saved { .. })
            {
                self.attach_terminal_panel(&session);
            }
        } else if self.list.is_loaded() {
            self.terminal_panel.clear_attachment();
            self.terminal_panel.message =
                Some("Terminal session was removed. Enter opens a new shell.".into());
            if let Some(folder) = self.terminal_panel.folder.as_ref() {
                self.terminal_panel.bindings.remove(folder);
            }
            if self.focus == Focus::Terminal {
                self.focus = Focus::List;
                self.came_from_terminal = true;
            }
        }
    }

    fn resolve_terminal_panel(&mut self) {
        let (Some(api), Some(folder)) = (self.api(), self.terminal_panel.folder.clone()) else {
            return;
        };
        let existing = self.terminal_panel.bindings.get(&folder).cloned();
        let create_missing = self.terminal_panel.create_missing;
        let request = super::new_session::request(
            folder.clone(),
            super::env_snapshot(),
            self.terminal_panel_size(),
        );
        let generation = self.terminal_panel.retarget(DetailView::Empty);
        self.terminal_panel.pending = Some(generation);
        self.terminal_panel.needs_open = false;
        self.terminal_panel.message = None;
        self.exec.job(move || AppEvent::PanelResolved {
            generation,
            folder,
            result: match existing {
                Some(id) => match api.get(&id) {
                    Err(error) if error.code == ErrorCode::NotFound && create_missing => {
                        api.create(request)
                    }
                    other => other,
                },
                None if create_missing => api.create(request),
                None => Err(ApiError::new(
                    ErrorCode::NotFound,
                    "Terminal session is gone; open a new terminal explicitly",
                )),
            },
        });
    }

    pub(super) fn on_panel_resolved(
        &mut self,
        generation: u64,
        folder: ScopeRoot,
        result: Result<SessionView, ApiError>,
    ) {
        if self.terminal_panel.pending != Some(generation) {
            return;
        }
        self.terminal_panel.pending = None;
        if result
            .as_ref()
            .is_ok_and(|session| session.scope != folder || self.is_self_session(&session.id))
        {
            self.terminal_panel.bindings.remove(&folder);
        }
        let result = result.and_then(|session| {
            if session.scope != folder || self.is_self_session(&session.id) {
                Err(ApiError::new(
                    ErrorCode::InvalidArgument,
                    "Terminal session does not belong to this folder or cannot be nested",
                ))
            } else {
                Ok(session)
            }
        });
        if let Ok(session) = &result {
            self.terminal_panel
                .bindings
                .insert(folder.clone(), session.id.clone());
            self.list.upsert(session.clone());
        }
        if generation != self.terminal_panel.epoch
            || !self.terminal_panel.open
            || self.terminal_panel.folder.as_ref() != Some(&folder)
        {
            return;
        }
        match result {
            Ok(session) if session.status == SessionStatus::Live => {
                self.attach_terminal_panel(&session)
            }
            Ok(session)
                if matches!(session.status, SessionStatus::Dead | SessionStatus::Exited) =>
            {
                self.read_terminal_panel(&session)
            }
            Ok(_) => {
                self.terminal_panel.message =
                    Some("Unknown terminal status; open it from the session list.".into())
            }
            Err(error) => {
                self.terminal_panel.message =
                    Some(format!("Can't open terminal: {error}. Enter retries."))
            }
        }
    }

    fn attach_terminal_panel(&mut self, session: &SessionView) {
        let Some(api) = self.api() else {
            return;
        };
        let role = if self.focus == Focus::Terminal && self.terminal_panel.open {
            Role::Controller
        } else {
            Role::Observer
        };
        let size = if role == Role::Controller {
            self.terminal_panel_size()
        } else {
            session
                .terminal
                .as_ref()
                .map(|terminal| (terminal.cols, terminal.rows))
                .unwrap_or((80, 24))
        };
        let request = AttachRequest {
            id: session.id.clone(),
            role,
            cols: size.0,
            rows: size.1,
        };
        let generation = self
            .terminal_panel
            .retarget(DetailView::Loading(session.id.clone()));
        self.terminal_panel.pending = Some(generation);
        self.exec.job(move || AppEvent::PanelAttached {
            generation,
            result: api.attach(request),
        });
    }

    pub(super) fn on_panel_attached(
        &mut self,
        generation: u64,
        result: Result<AttachStream, ApiError>,
    ) {
        if self.terminal_panel.pending == Some(generation) {
            self.terminal_panel.pending = None;
        }
        if generation != self.terminal_panel.epoch || !self.terminal_panel.open {
            return;
        }
        let Some(id) = self.terminal_panel.session_id().cloned() else {
            return;
        };
        match result {
            Ok(stream) => {
                let attached = &stream.attached;
                if attached.session.id != id
                    || attached.session.is_self
                    || self.is_self_session(&id)
                    || self.terminal_panel.folder.as_ref() != Some(&attached.session.scope)
                {
                    self.terminal_panel.message = Some("Invalid terminal attachment".into());
                    return;
                }
                let terminal =
                    match panel_terminal(attached.rows, attached.cols, &attached.checkpoint) {
                        Ok(terminal) => terminal,
                        Err(error) => {
                            self.terminal_panel.message = Some(error);
                            return;
                        }
                    };
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
                    if self.focus != Focus::Terminal {
                        if let Err(error) = attachment.change_role(StreamCommand::Demote) {
                            self.terminal_panel.message =
                                Some(format!("Can't release stale terminal control: {error}"));
                            return;
                        }
                        attachment.role = Role::Observer;
                    } else {
                        attachment.sent_size = Some((attached.cols, attached.rows));
                    }
                }
                self.terminal_panel.detail.terminal = Some(terminal);
                self.terminal_panel.detail.attachment = Some(attachment);
                self.terminal_panel.detail.view = DetailView::Live(id);
                self.terminal_panel.message = None;
                self.exec.pump_panel(generation, stream, cancel);
                if self.focus == Focus::Terminal
                    && self
                        .terminal_panel
                        .detail
                        .attachment
                        .as_ref()
                        .is_some_and(|attachment| attachment.role == Role::Observer)
                {
                    self.focus = Focus::List;
                    self.came_from_terminal = false;
                    self.set_status(
                        "Terminal is read-only; click its pane or press T to take control.",
                    );
                }
                self.claim_panel_size();
            }
            Err(error) => {
                self.terminal_panel.message =
                    Some(format!("Can't attach terminal: {error}. Enter retries."));
                self.terminal_panel.detail.view = DetailView::Empty;
            }
        }
    }

    fn read_terminal_panel(&mut self, session: &SessionView) {
        let Some(api) = self.api() else {
            return;
        };
        let request = ReadRequest {
            id: session.id.clone(),
            source: ReadSource::Recent,
            lines: Some(500),
            format: TextFormat::Ansi,
        };
        let generation = self.terminal_panel.retarget(DetailView::Saved {
            id: session.id.clone(),
            status: session.status,
            exit_code: session.exit_code,
        });
        self.terminal_panel.pending = Some(generation);
        self.exec.job(move || AppEvent::PanelSaved {
            generation,
            result: api.read(request),
        });
    }

    pub(super) fn on_panel_saved(&mut self, generation: u64, result: Result<ReadResult, ApiError>) {
        if self.terminal_panel.pending == Some(generation) {
            self.terminal_panel.pending = None;
        }
        if generation != self.terminal_panel.epoch || !self.terminal_panel.open {
            return;
        }
        match result {
            Ok(read)
                if read.text.len() <= MAX_CHECKPOINT
                    && valid_size(read.rows.max(2), read.cols.max(2)) =>
            {
                let mut terminal = TerminalView::with_scrollback(
                    read.rows.max(2),
                    read.cols.max(2),
                    panel_history(read.rows.max(2), read.cols.max(2)),
                );
                let lines: Vec<_> = read.text.lines().rev().take(SCROLLBACK).collect();
                terminal.reset_screen_to_text(
                    read.rows.max(2),
                    read.cols.max(2),
                    &lines.into_iter().rev().collect::<Vec<_>>().join("\n"),
                );
                self.terminal_panel.detail.terminal = Some(terminal);
            }
            Ok(_) => {
                self.terminal_panel.message =
                    Some("Saved terminal screen exceeds display limits".into())
            }
            Err(error) => {
                self.terminal_panel.message = Some(format!("Can't read terminal: {error}"))
            }
        }
    }

    pub(super) fn on_panel_attach_event(&mut self, generation: u64, event: AttachEvent) {
        if generation != self.terminal_panel.epoch
            || self.terminal_panel.detail.attachment.is_none()
        {
            return;
        }
        match event {
            AttachEvent::Output { offset, bytes } => {
                let Some(attachment) = self.terminal_panel.detail.attachment.as_mut() else {
                    return;
                };
                match classify_output(attachment.offset, offset, bytes.len()) {
                    OutputAction::Feed { skip } => {
                        attachment.offset = offset + bytes.len() as u64;
                        if let Some(terminal) = &mut self.terminal_panel.detail.terminal {
                            terminal.feed(&bytes[skip..]);
                        }
                    }
                    OutputAction::Duplicate => {}
                    OutputAction::Gap => self.resync_terminal_panel(),
                }
            }
            AttachEvent::Event(StreamEvent::Resized {
                rows,
                cols,
                checkpoint,
                offset,
            }) => {
                if self
                    .terminal_panel
                    .detail
                    .attachment
                    .as_ref()
                    .is_some_and(|attachment| offset < attachment.offset)
                {
                    return;
                }
                match panel_terminal(rows, cols, &checkpoint) {
                    Ok(terminal) => {
                        self.terminal_panel.detail.terminal = Some(terminal);
                        if let Some(attachment) = self.terminal_panel.detail.attachment.as_mut() {
                            attachment.offset = offset;
                        }
                    }
                    Err(error) => {
                        self.terminal_panel.clear_attachment();
                        self.terminal_panel.message = Some(error);
                    }
                }
            }
            AttachEvent::Event(StreamEvent::RoleChanged { role }) => {
                let Some(attachment) = self.terminal_panel.detail.attachment.as_mut() else {
                    return;
                };
                if attachment.own_role_change(role) {
                    return;
                }
                attachment.role = role;
                if role == Role::Observer && self.focus == Focus::Terminal {
                    self.focus = Focus::List;
                    self.came_from_terminal = true;
                    self.set_status("Another client took control of the terminal panel.");
                } else if role == Role::Controller && self.focus != Focus::Terminal {
                    self.demote_terminal_panel();
                }
            }
            AttachEvent::Event(StreamEvent::Exited { code }) => {
                if let Some(id) = self.terminal_panel.session_id().cloned() {
                    self.terminal_panel.detail.attachment = None;
                    self.list.mark_exited(&id, code);
                    self.terminal_panel.detail.view = DetailView::Saved {
                        id,
                        status: SessionStatus::Exited,
                        exit_code: code,
                    };
                }
            }
            AttachEvent::Event(StreamEvent::Clipboard { selection, data }) => {
                if self.focus == Focus::Terminal
                    && matches!(self.mode, AppMode::Normal)
                    && self
                        .terminal_panel
                        .detail
                        .attachment
                        .as_ref()
                        .is_some_and(|attachment| attachment.role == Role::Controller)
                {
                    self.terminal_output
                        .extend_from_slice(format!("\x1b]52;{selection};{data}\x07").as_bytes());
                }
            }
            AttachEvent::Event(StreamEvent::Lagged { .. }) => self.resync_terminal_panel(),
            AttachEvent::Event(StreamEvent::Title { title }) => {
                self.terminal_panel.detail.title = Some(title)
            }
            AttachEvent::Event(_) => {}
            AttachEvent::Closed(error) => {
                self.terminal_panel.detail.attachment = None;
                if self.focus == Focus::Terminal {
                    self.focus = Focus::List;
                    self.came_from_terminal = true;
                }
                self.terminal_panel.message =
                    Some(error.unwrap_or_else(|| "Terminal disconnected; Enter retries.".into()));
                self.terminal_panel.reconnect = true;
                self.terminal_panel.retry_at = Some(Instant::now() + Duration::from_secs(2));
            }
        }
    }

    fn resync_terminal_panel(&mut self) {
        if let Some(session) = self
            .terminal_panel
            .session_id()
            .and_then(|id| self.list.get(id))
            .cloned()
        {
            self.attach_terminal_panel(&session);
        }
    }

    pub fn handle_terminal_panel_key(&mut self, key: KeyEvent) -> AppResult {
        if self.rakumux_key.matches(&key) {
            self.enter_rakumux_mode();
            return AppResult::Continue;
        }
        if self.settings.sessions.cycle_keys_in_session {
            if let Some(delta) = self.cycle_direction(&key) {
                self.demote_terminal_panel();
                self.focus = Focus::List;
                self.came_from_terminal = false;
                self.cycle_session(delta);
                self.activate_selected();
                return AppResult::Continue;
            }
        }
        if self.terminal_panel.pending.is_some() {
            return AppResult::Continue;
        }
        if key.code == KeyCode::Enter && self.terminal_panel.detail.attachment.is_none() {
            self.restart_terminal_panel();
            return AppResult::Continue;
        }
        if self.screen().terminal_panel.is_none() {
            return AppResult::Continue;
        }
        if let (Some(terminal), Some(attachment)) = (
            &mut self.terminal_panel.detail.terminal,
            &self.terminal_panel.detail.attachment,
        ) {
            if attachment.role == Role::Controller {
                if let Some(bytes) = terminal_view::key_to_bytes(&key, &terminal.modes()) {
                    terminal.scroll_to_bottom();
                    if let Err(error) = attachment.input(bytes) {
                        self.set_status(format!("Terminal input failed: {error}"));
                    }
                }
            }
        }
        AppResult::Continue
    }

    fn restart_terminal_panel(&mut self) {
        let Some(session) = self
            .terminal_panel
            .session_id()
            .and_then(|id| self.list.get(id))
            .cloned()
        else {
            self.terminal_panel.create_missing = true;
            self.terminal_panel.reconnect = false;
            self.terminal_panel.needs_open = true;
            self.sync_terminal_panel();
            return;
        };
        if session.status == SessionStatus::Live {
            self.attach_terminal_panel(&session);
            return;
        }
        let (Some(api), Some(folder)) = (self.api(), self.terminal_panel.folder.clone()) else {
            return;
        };
        let size = self.terminal_panel_size();
        let request = ReviveSession {
            id: session.id.clone(),
            env: super::env_snapshot(),
            cols: size.0,
            rows: size.1,
            resume: true,
        };
        let generation = self
            .terminal_panel
            .retarget(DetailView::Loading(session.id));
        self.terminal_panel.pending = Some(generation);
        self.exec.job(move || AppEvent::PanelResolved {
            generation,
            folder,
            result: api.revive(request),
        });
    }

    pub(super) fn paste_terminal_panel(&mut self, text: &str) {
        if self.terminal_panel.pending.is_some() || self.screen().terminal_panel.is_none() {
            return;
        }
        if let (Some(terminal), Some(attachment)) = (
            &mut self.terminal_panel.detail.terminal,
            &self.terminal_panel.detail.attachment,
        ) {
            if attachment.role == Role::Controller {
                terminal.scroll_to_bottom();
                let bytes = terminal_view::paste_to_bytes(text, &terminal.modes());
                if let Err(error) = attachment.input(bytes) {
                    self.set_status(format!("Terminal paste failed: {error}"));
                }
            }
        }
    }

    pub(super) fn terminal_panel_mouse(&mut self, event: MouseEvent) {
        if self.focus != Focus::Terminal
            && matches!(
                event.kind,
                MouseEventKind::Down(crossterm::event::MouseButton::Left)
            )
        {
            self.focus_terminal_panel();
            return;
        }
        let Some(pane) = self.screen().terminal_panel.map(panel_output) else {
            return;
        };
        if let (Some(terminal), Some(attachment)) = (
            &mut self.terminal_panel.detail.terminal,
            &self.terminal_panel.detail.attachment,
        ) {
            if self.focus == Focus::Terminal && attachment.role == Role::Controller {
                if let Some(bytes) = terminal.mouse_bytes(&event, pane) {
                    if let Err(error) = attachment.input(bytes) {
                        self.set_status(format!("Terminal mouse input failed: {error}"));
                    }
                    return;
                }
            }
            match event.kind {
                MouseEventKind::ScrollUp => terminal.scroll(3),
                MouseEventKind::ScrollDown => terminal.scroll(-3),
                _ => {}
            }
        }
    }

    pub fn terminal_panel_size(&self) -> (u16, u16) {
        self.screen_for(Focus::Terminal)
            .terminal_panel
            .map(panel_output)
            .map(|area| (area.width.max(2), area.height.max(2)))
            .unwrap_or((80, 12))
    }

    pub(super) fn drag_terminal_panel(&mut self, row: u16) {
        let screen = self.screen();
        let (Some(main), Some(panel)) = (screen.detail, screen.terminal_panel) else {
            return;
        };
        let total = panel.bottom().saturating_sub(main.y).max(1);
        let height = panel.bottom().saturating_sub(row).saturating_sub(1);
        self.terminal_panel.height_percent =
            ((u32::from(height) * 100 / u32::from(total)) as u16).clamp(20, 70);
        self.sync_panel_size();
        self.sync_terminal_size();
    }

    pub(super) fn claim_panel_size(&mut self) {
        self.resize_panel(false);
    }

    pub(super) fn sync_panel_size(&mut self) {
        self.resize_panel(true);
    }

    fn resize_panel(&mut self, geometry_changed: bool) {
        if self.focus != Focus::Terminal || self.screen().terminal_panel.is_none() {
            return;
        }
        let size = self.terminal_panel_size();
        let shown = self
            .terminal_panel
            .detail
            .terminal
            .as_ref()
            .map(TerminalView::size);
        let Some(attachment) = self.terminal_panel.detail.attachment.as_mut() else {
            return;
        };
        if attachment.role != Role::Controller
            || (attachment.sent_size == Some(size) && shown == Some((size.1, size.0)))
        {
            return;
        }
        if !geometry_changed
            && attachment
                .last_claim
                .is_some_and(|at| at.elapsed() < Duration::from_millis(100))
        {
            return;
        }
        if let Err(error) = attachment.command(StreamCommand::Resize {
            cols: size.0,
            rows: size.1,
        }) {
            self.set_status(format!("Terminal resize failed: {error}"));
        } else {
            attachment.sent_size = Some(size);
            attachment.last_claim = Some(Instant::now());
        }
    }

    pub(super) fn terminal_panel_disconnected(&mut self) {
        if self.terminal_panel.open {
            self.terminal_panel.clear_attachment();
            self.terminal_panel.reconnect = true;
            self.terminal_panel.retry_at = None;
            self.terminal_panel.message = Some("Terminal disconnected; reconnecting…".into());
            if self.focus == Focus::Terminal {
                self.focus = Focus::List;
                self.came_from_terminal = true;
            }
        }
    }
}

pub fn panel_output(area: ratatui::layout::Rect) -> ratatui::layout::Rect {
    ratatui::layout::Rect {
        y: area.y + area.height.min(1),
        height: area.height.saturating_sub(1),
        ..area
    }
}

fn valid_size(rows: u16, cols: u16) -> bool {
    rows >= 2 && cols >= 2 && usize::from(rows) * usize::from(cols) <= MAX_CELLS
}

fn panel_history(rows: u16, cols: u16) -> usize {
    (MAX_CELLS / usize::from(cols.max(2)))
        .saturating_sub(usize::from(rows))
        .min(SCROLLBACK)
}

fn panel_terminal(rows: u16, cols: u16, checkpoint: &str) -> Result<TerminalView, String> {
    if !valid_size(rows, cols) || checkpoint.len() > MAX_CHECKPOINT {
        return Err("Terminal screen exceeds panel display limits".into());
    }
    let bytes = super::sessions::decode_checkpoint(checkpoint)?;
    let mut terminal = TerminalView::with_scrollback(rows, cols, panel_history(rows, cols));
    terminal.reset_screen(rows, cols, &bytes);
    Ok(terminal)
}

#[cfg(test)]
#[path = "terminal_panel_tests.rs"]
mod tests;
