//! A window-local terminal pane, not a Rakumux session. Hiding retains its
//! stream; shell exit closes the pane and the daemon releases the PTY.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, MouseEvent, MouseEventKind};
use session_core::api::{
    ApiError, ErrorCode, OpenTerminal, Role, StreamCommand, StreamEvent, TerminalAttachRequest,
};
use session_core::client::{AttachEvent, TerminalStream};
use session_core::ids::PaneId;

use super::detail::{classify_output, Attachment, OutputAction};
use super::runtime::{cancel_flag, AppEvent};
use super::types::{AppMode, AppResult, Focus};
use super::App;
use crate::terminal_view::{self, TerminalView};

const DEFAULT_HEIGHT_PERCENT: u16 = 35;
const MAX_CELLS: usize = 500_000;
const MAX_CHECKPOINT: usize = 2 * 1024 * 1024;
const SCROLLBACK: usize = 500;

/// An independent terminal and its view. It has no registry/session state.
pub struct TerminalPanel {
    pub open: bool,
    pub id: Option<PaneId>,
    pub cwd: Option<PathBuf>,
    pub terminal: Option<TerminalView>,
    pub attachment: Option<Attachment<PaneId>>,
    pub message: Option<String>,
    pub title: Option<String>,
    pub height_percent: u16,
    pub dragging: bool,
    pending: Option<u64>,
    epoch: u64,
    needs_open: bool,
    creating: bool,
    reconnect: bool,
    retry_at: Option<Instant>,
    awaiting_checkpoint: bool,
    needs_refocus: bool,
}

impl Default for TerminalPanel {
    fn default() -> Self {
        Self {
            open: false,
            id: None,
            cwd: None,
            terminal: None,
            attachment: None,
            message: None,
            title: None,
            height_percent: DEFAULT_HEIGHT_PERCENT,
            dragging: false,
            pending: None,
            epoch: 0,
            needs_open: false,
            creating: false,
            reconnect: false,
            retry_at: None,
            awaiting_checkpoint: false,
            needs_refocus: false,
        }
    }
}

impl TerminalPanel {
    pub fn busy(&self) -> bool {
        self.pending.is_some()
    }

    fn clear_attachment(&mut self) {
        self.epoch = self.epoch.wrapping_add(1);
        self.pending = None;
        self.attachment = None;
    }
}

impl App {
    /// Reveal/focus a pane, or hide it after leaving its input mode.
    pub fn toggle_terminal_panel(&mut self) {
        if self.terminal_panel.open && self.came_from_terminal && !self.terminal_panel.needs_refocus
        {
            self.hide_terminal_panel();
        } else {
            self.show_terminal_panel();
        }
    }

    /// Start a standalone shell only when this window has no existing pane.
    pub fn show_terminal_panel(&mut self) {
        if self.terminal_panel.id.is_none() {
            self.terminal_panel.id = Some(PaneId::generate());
            self.terminal_panel.cwd = Some(
                self.list
                    .selected_session()
                    .map(|session| session.cwd.clone())
                    .unwrap_or_else(|| self.current_folder().path().to_path_buf()),
            );
            self.terminal_panel.creating = true;
            self.terminal_panel.needs_open = true;
            self.terminal_panel.message = None;
        }
        self.terminal_panel.open = true;
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
        self.feed_lost_keys();
        self.zoomed = false;
        self.came_from_terminal = false;
        self.terminal_panel.needs_refocus = false;
        self.pending_key = None;
        self.list.filter_editing = false;
        self.grid.clear();
        self.reconcile_detail();
        let size = self.terminal_panel_size();
        if let Some(attachment) = self.terminal_panel.attachment.as_mut() {
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

    /// Hide the view, not its process or stream. Hidden exits still clean up.
    pub fn hide_terminal_panel(&mut self) {
        self.demote_terminal_panel();
        self.terminal_panel.open = false;
        self.terminal_panel.dragging = false;
        self.came_from_terminal = false;
        if self.focus == Focus::Terminal {
            self.focus = Focus::List;
        }
        self.sync_terminal_size();
        self.reconcile_detail();
    }

    pub(super) fn demote_terminal_panel(&mut self) {
        if let Some(attachment) = self.terminal_panel.attachment.as_mut() {
            if attachment.role == Role::Controller {
                match attachment.change_role(StreamCommand::Demote) {
                    Ok(()) => attachment.role = Role::Observer,
                    Err(error) => {
                        self.set_status(format!("Could not release terminal control: {error}"))
                    }
                }
            }
        }
    }

    pub(super) fn sync_terminal_panel(&mut self) {
        if !self.connection.is_connected() || self.terminal_panel.pending.is_some() {
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
            self.terminal_panel.creating = false;
            self.terminal_panel.needs_open = true;
        }
        if !self.terminal_panel.needs_open {
            return;
        }
        let (Some(api), Some(id), Some(cwd)) = (
            self.api(),
            self.terminal_panel.id.clone(),
            self.terminal_panel.cwd.clone(),
        ) else {
            return;
        };
        let creating = self.terminal_panel.creating;
        let size = self.terminal_panel_size();
        self.terminal_panel.epoch = self.terminal_panel.epoch.wrapping_add(1);
        let generation = self.terminal_panel.epoch;
        self.terminal_panel.pending = Some(generation);
        self.terminal_panel.needs_open = false;
        let env = super::env_snapshot();
        self.exec.job(move || AppEvent::PanelAttached {
            generation,
            result: if creating {
                api.open_terminal(OpenTerminal {
                    id,
                    cwd,
                    env,
                    cols: size.0,
                    rows: size.1,
                })
            } else {
                api.attach_terminal(TerminalAttachRequest {
                    id,
                    role: Role::Observer,
                    cols: size.0,
                    rows: size.1,
                })
            },
        });
    }

    pub(super) fn on_panel_attached(
        &mut self,
        generation: u64,
        result: Result<TerminalStream, ApiError>,
    ) {
        if generation != self.terminal_panel.epoch {
            if let Ok(stream) = result {
                stream.detach();
            }
            return;
        }
        self.terminal_panel.pending = None;
        let stream = match result {
            Ok(stream) => stream,
            Err(error) if !self.terminal_panel.creating && error.code == ErrorCode::NotFound => {
                self.finish_terminal_panel();
                self.set_status("Terminal ended while disconnected; open a new pane with T.");
                return;
            }
            Err(error) => {
                self.terminal_panel.message = Some(if error.code == ErrorCode::Unsupported {
                    "The daemon needs an in-place restart to support standalone terminal panes. No session was created.".into()
                } else {
                    format!("Can't open terminal: {error}. Enter retries.")
                });
                return;
            }
        };
        let attached = &stream.attached;
        if Some(&attached.id) != self.terminal_panel.id.as_ref() {
            stream.detach();
            self.terminal_panel.message = Some("Invalid terminal pane attachment".into());
            return;
        }
        if let Some(cwd) = &attached.cwd {
            self.terminal_panel.cwd = Some(cwd.clone());
        }
        let terminal = match panel_terminal(attached.rows, attached.cols, &attached.checkpoint) {
            Ok(terminal) => terminal,
            Err(error) => {
                stream.detach();
                self.terminal_panel.message = Some(error);
                return;
            }
        };
        let cancel = cancel_flag();
        let mut attachment = Attachment::new(
            attached.id.clone(),
            generation,
            attached.role,
            stream.sender(),
            cancel.clone(),
            attached.offset,
        );
        if attached.role == Role::Controller {
            if self.focus != Focus::Terminal || !self.terminal_panel.open {
                if let Err(error) = attachment.change_role(StreamCommand::Demote) {
                    self.terminal_panel.message =
                        Some(format!("Can't release stale terminal control: {error}"));
                    return;
                }
                attachment.role = Role::Observer;
            } else {
                attachment.sent_size = Some((attached.cols, attached.rows));
            }
        } else if self.focus == Focus::Terminal {
            self.focus = Focus::List;
            self.came_from_terminal = true;
            self.set_status("Terminal reconnected read-only; click it or press T to take control.");
        }
        self.terminal_panel.terminal = Some(terminal);
        self.terminal_panel.attachment = Some(attachment);
        self.terminal_panel.creating = false;
        self.terminal_panel.awaiting_checkpoint = false;
        self.terminal_panel.needs_refocus = attached.role == Role::Observer;
        self.terminal_panel.message = None;
        self.exec.pump_panel(generation, stream, cancel);
        self.claim_panel_size();
    }

    /// Shell exit is not a saved session: close the pane and release its lease.
    fn finish_terminal_panel(&mut self) {
        self.terminal_panel.clear_attachment();
        self.terminal_panel.open = false;
        self.terminal_panel.id = None;
        self.terminal_panel.cwd = None;
        self.terminal_panel.terminal = None;
        self.terminal_panel.title = None;
        self.terminal_panel.message = None;
        self.terminal_panel.needs_open = false;
        self.terminal_panel.reconnect = false;
        self.terminal_panel.creating = false;
        self.terminal_panel.dragging = false;
        self.terminal_panel.awaiting_checkpoint = false;
        self.terminal_panel.needs_refocus = false;
        if self.focus == Focus::Terminal {
            self.focus = Focus::List;
        }
        self.came_from_terminal = false;
        // A background shell exit is not activity in the agent terminal.
        // Resize only when that terminal already owns keyboard focus.
        if self.focus == Focus::Session {
            self.sync_terminal_size();
        }
        self.reconcile_detail();
    }

    pub(super) fn on_panel_attach_event(&mut self, generation: u64, event: AttachEvent) {
        if generation != self.terminal_panel.epoch || self.terminal_panel.attachment.is_none() {
            return;
        }
        match event {
            AttachEvent::Output { offset, bytes } => {
                let attachment = self.terminal_panel.attachment.as_mut().unwrap();
                match classify_output(attachment.offset, offset, bytes.len()) {
                    OutputAction::Feed { skip } if !self.terminal_panel.awaiting_checkpoint => {
                        attachment.offset = offset + bytes.len() as u64;
                        if let Some(terminal) = &mut self.terminal_panel.terminal {
                            terminal.feed(&bytes[skip..]);
                        }
                    }
                    OutputAction::Gap => self.request_panel_checkpoint(),
                    _ => {}
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
                    .attachment
                    .as_ref()
                    .is_some_and(|attachment| offset < attachment.offset)
                {
                    return;
                }
                match panel_terminal(rows, cols, &checkpoint) {
                    Ok(terminal) => {
                        self.terminal_panel.terminal = Some(terminal);
                        self.terminal_panel.awaiting_checkpoint = false;
                        self.terminal_panel.message = None;
                        if let Some(attachment) = &mut self.terminal_panel.attachment {
                            attachment.offset = offset;
                        }
                    }
                    Err(error) => {
                        self.finish_terminal_panel();
                        self.set_status(error);
                    }
                }
            }
            AttachEvent::Event(StreamEvent::Exited { .. }) => self.finish_terminal_panel(),
            AttachEvent::Event(StreamEvent::RoleChanged { role }) => {
                let attachment = self.terminal_panel.attachment.as_mut().unwrap();
                if attachment.own_role_change(role) {
                    return;
                }
                attachment.role = role;
                if role == Role::Observer && self.focus == Focus::Terminal {
                    self.focus = Focus::List;
                    self.came_from_terminal = true;
                } else if role == Role::Controller && self.focus != Focus::Terminal {
                    self.demote_terminal_panel();
                }
            }
            AttachEvent::Event(StreamEvent::Title { title }) => {
                self.terminal_panel.title = Some(title)
            }
            AttachEvent::Event(StreamEvent::Cwd { path }) => self.terminal_panel.cwd = Some(path),
            AttachEvent::Event(StreamEvent::Clipboard { selection, data }) => {
                if self.focus == Focus::Terminal
                    && matches!(self.mode, AppMode::Normal)
                    && self
                        .terminal_panel
                        .attachment
                        .as_ref()
                        .is_some_and(|a| a.role == Role::Controller)
                {
                    self.terminal_output
                        .extend_from_slice(format!("\x1b]52;{selection};{data}\x07").as_bytes());
                }
            }
            AttachEvent::Event(StreamEvent::Lagged { .. }) => self.request_panel_checkpoint(),
            AttachEvent::Event(_) => {}
            AttachEvent::Closed(error) => {
                self.terminal_panel_disconnected();
                self.terminal_panel.retry_at = Some(Instant::now() + Duration::from_millis(250));
                if let Some(error) = error {
                    self.terminal_panel.message = Some(error);
                }
            }
        }
    }

    fn request_panel_checkpoint(&mut self) {
        if self.terminal_panel.awaiting_checkpoint {
            return;
        }
        self.terminal_panel.awaiting_checkpoint = true;
        if let Some(attachment) = &self.terminal_panel.attachment {
            if let Err(error) = attachment.command(StreamCommand::Checkpoint) {
                self.terminal_panel.message = Some(format!("Can't refresh terminal: {error}"));
            }
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
        if self.terminal_panel.pending.is_some() || self.terminal_panel.awaiting_checkpoint {
            return AppResult::Continue;
        }
        if key.code == KeyCode::Enter && self.terminal_panel.attachment.is_none() {
            self.terminal_panel.needs_open = true;
            self.sync_terminal_panel();
            return AppResult::Continue;
        }
        if self.screen().terminal_panel.is_some() {
            if let Some(terminal) = &self.terminal_panel.terminal {
                if let Some(bytes) = terminal_view::key_to_bytes(&key, &terminal.modes()) {
                    self.panel_input(bytes);
                }
            }
        }
        AppResult::Continue
    }

    fn panel_input(&mut self, bytes: Vec<u8>) {
        if self.terminal_panel.pending.is_some() || self.terminal_panel.awaiting_checkpoint {
            return;
        }
        if let Some(attachment) = &self.terminal_panel.attachment {
            if attachment.role == Role::Controller {
                if let Some(terminal) = &mut self.terminal_panel.terminal {
                    terminal.scroll_to_bottom();
                }
                if let Err(error) = attachment.input(bytes) {
                    self.set_status(format!("Terminal input failed: {error}"));
                }
            }
        }
    }

    pub(super) fn paste_terminal_panel(&mut self, text: &str) {
        if self.screen().terminal_panel.is_none() {
            return;
        }
        if let Some(terminal) = &self.terminal_panel.terminal {
            self.panel_input(terminal_view::paste_to_bytes(text, &terminal.modes()));
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
        if self.focus == Focus::Terminal && !self.terminal_panel.awaiting_checkpoint {
            if let Some(terminal) = &self.terminal_panel.terminal {
                if let Some(bytes) = terminal.mouse_bytes(&event, pane) {
                    self.panel_input(bytes);
                    return;
                }
            }
        }
        if let Some(terminal) = &mut self.terminal_panel.terminal {
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
            .terminal
            .as_ref()
            .map(TerminalView::size);
        let Some(attachment) = &mut self.terminal_panel.attachment else {
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
        match attachment.command(StreamCommand::Resize {
            cols: size.0,
            rows: size.1,
        }) {
            Ok(()) => {
                attachment.sent_size = Some(size);
                attachment.last_claim = Some(Instant::now());
            }
            Err(error) => self.set_status(format!("Terminal resize failed: {error}")),
        }
    }

    pub(super) fn terminal_panel_disconnected(&mut self) {
        if self.terminal_panel.id.is_none() {
            return;
        }
        self.terminal_panel.clear_attachment();
        self.terminal_panel.creating = false;
        self.terminal_panel.reconnect = true;
        self.terminal_panel.needs_refocus = true;
        self.terminal_panel.retry_at = None;
        self.terminal_panel.message = Some("Terminal disconnected; reconnecting…".into());
        if self.focus == Focus::Terminal {
            self.focus = Focus::List;
            self.came_from_terminal = true;
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

fn panel_history(rows: u16, cols: u16) -> usize {
    (MAX_CELLS / usize::from(cols.max(2)))
        .saturating_sub(usize::from(rows))
        .min(SCROLLBACK)
}

fn panel_terminal(rows: u16, cols: u16, checkpoint: &str) -> Result<TerminalView, String> {
    if rows < 2
        || cols < 2
        || usize::from(rows) * usize::from(cols) > MAX_CELLS
        || checkpoint.len() > MAX_CHECKPOINT
    {
        return Err("Terminal screen exceeds pane display limits".into());
    }
    let bytes = super::sessions::decode_checkpoint(checkpoint)?;
    let mut terminal = TerminalView::with_scrollback(rows, cols, panel_history(rows, cols));
    terminal.reset_screen(rows, cols, &bytes);
    Ok(terminal)
}

#[cfg(test)]
#[path = "terminal_panel_tests.rs"]
mod tests;
