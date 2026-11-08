//! A session's terminal panel: tabs of terminal panes, each a daemon-owned
//! PTY, not a Rakumux session. Only the shown session's panel shows; the
//! other sessions' tabs, and the shown session's other tabs, keep their
//! streams (`App::parked_panels`). Hiding retains the streams; a tab's shell
//! exit closes that tab and the daemon releases its PTY.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, MouseEvent, MouseEventKind};
use session_core::api::{
    ApiError, ErrorCode, OpenTerminal, Role, StreamCommand, StreamEvent, TerminalAttachRequest,
};
use session_core::client::{AttachEvent, TerminalStream};
use session_core::ids::{PaneId, SessionId};

use super::detail::{classify_output, Attachment, OutputAction};
use super::runtime::{cancel_flag, AppEvent};
use super::types::{AppMode, AppResult, Focus};
use super::App;
use crate::layout::PanelPlacement;
use crate::terminal_view::{self, TerminalView};

const DEFAULT_HEIGHT_PERCENT: u16 = 35;
/// Beside: the session about 60%, the panel 40%.
const DEFAULT_WIDTH_PERCENT: u16 = 40;
const MAX_CELLS: usize = 500_000;
const MAX_CHECKPOINT: usize = 2 * 1024 * 1024;
const SCROLLBACK: usize = 500;

/// Tab ids and attach epochs, unique across every tab of this process, so a
/// stream event always finds its own tab.
static NEXT_ID: AtomicU64 = AtomicU64::new(1);

fn next_id() -> u64 {
    NEXT_ID.fetch_add(1, Ordering::Relaxed)
}

/// One tab of a session's terminal panel: an independent terminal and its
/// view. It has no registry/session state.
pub struct TerminalPanel {
    /// The session whose panel it is a tab of (`None`: no session shown).
    pub owner: Option<SessionId>,
    /// This tab's id, in the order tabs were opened (0 before its shell).
    pub seq: u64,
    /// Parked: its session's panel shows this tab when the session is
    /// shown again.
    active: bool,
    /// The panel is shown (the shown tab), or will be when its session is
    /// shown again (a parked `active` tab).
    pub open: bool,
    pub id: Option<PaneId>,
    pub cwd: Option<PathBuf>,
    pub terminal: Option<TerminalView>,
    pub attachment: Option<Attachment<PaneId>>,
    pub message: Option<String>,
    pub title: Option<String>,
    /// Its share of the detail's height below (a dragged divider).
    pub height_percent: u16,
    /// Its share of the detail's width beside (a dragged divider).
    pub width_percent: u16,
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
            owner: None,
            seq: 0,
            active: false,
            open: false,
            id: None,
            cwd: None,
            terminal: None,
            attachment: None,
            message: None,
            title: None,
            height_percent: DEFAULT_HEIGHT_PERCENT,
            width_percent: DEFAULT_WIDTH_PERCENT,
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
        self.epoch = next_id();
        self.pending = None;
        self.attachment = None;
    }
}

impl App {
    /// Reveal/focus a pane, or hide it after leaving its input mode.
    pub fn toggle_terminal_panel(&mut self) {
        self.follow_panel_owner();
        if self.terminal_panel.open && self.came_from_terminal && !self.terminal_panel.needs_refocus
        {
            self.hide_terminal_panel();
        } else {
            self.show_terminal_panel();
        }
    }

    /// `|` (beside) and `-` (below): open the panel there, or move it there
    /// while it's open elsewhere, focusing it either way. Already open
    /// there, it does what `T` does. Where the detail has no room beside
    /// (see [`crate::app::screen::fits_beside`]) it shows below and says
    /// so; the placement is still kept for `T`.
    pub fn terminal_panel_at(&mut self, placement: PanelPlacement) {
        self.follow_panel_owner();
        if self.terminal_panel.open && self.settings.sessions.terminal_panel == placement {
            self.toggle_terminal_panel();
        } else {
            // Only the panel, focused now, takes its new size.
            self.keep_panel_placement(placement);
            self.show_terminal_panel();
            self.sync_panel_size();
        }
        if self.terminal_panel.open
            && placement == PanelPlacement::Beside
            && !self.screen_for(Focus::Terminal).terminal_panel_beside
        {
            self.set_status("No room beside: shown below");
        }
    }

    /// Settings' placement row: keep `placement` for `T` and move an open
    /// panel there. The terminal with the keys takes its new size, as a
    /// dragged divider gives it.
    pub fn place_terminal_panel(&mut self, placement: PanelPlacement) {
        if !self.keep_panel_placement(placement) || !self.terminal_panel.open {
            return;
        }
        self.sync_panel_size();
        self.sync_terminal_size();
    }

    /// Save `placement` as `sessions.terminal_panel`; false when it already
    /// was.
    fn keep_panel_placement(&mut self, placement: PanelPlacement) -> bool {
        if self.settings.sessions.terminal_panel == placement {
            return false;
        }
        self.settings.sessions.terminal_panel = placement;
        self.terminal_panel.dragging = false;
        if let Err(error) = self.save_settings() {
            self.set_status(format!("Couldn't save settings: {error}"));
        }
        true
    }

    /// Show the shown session's panel, starting its first shell (in the
    /// session's directory) when it has none.
    pub fn show_terminal_panel(&mut self) {
        self.follow_panel_owner();
        if self.terminal_panel.id.is_none() {
            self.terminal_panel.id = Some(PaneId::generate());
            self.terminal_panel.seq = next_id();
            self.terminal_panel.cwd = Some(
                self.shown_session()
                    .map(|session| session.cwd)
                    .unwrap_or_else(|| self.current_folder().path().to_path_buf()),
            );
            self.terminal_panel.creating = true;
            self.terminal_panel.needs_open = true;
            self.terminal_panel.message = None;
        }
        self.terminal_panel.open = true;
        self.focus_terminal_panel();
    }

    /// The panel's `+`: another terminal in the shown session's directory,
    /// as a new tab, focused.
    pub fn new_terminal_tab(&mut self) {
        self.follow_panel_owner();
        if self.terminal_panel.id.is_some() {
            let next = TerminalPanel {
                owner: self.terminal_panel.owner.clone(),
                ..TerminalPanel::default()
            };
            self.bring_tab_forward(next, false);
        }
        self.show_terminal_panel();
    }

    /// A click on a tab: show it in the panel and give it the keys.
    pub fn show_terminal_tab(&mut self, seq: u64) {
        if self.terminal_panel.seq != seq {
            let owner = self.terminal_panel.owner.clone();
            let Some(index) = self
                .parked_panels
                .iter()
                .position(|tab| tab.seq == seq && tab.owner == owner)
            else {
                return;
            };
            let mut next = self.parked_panels.remove(index);
            next.open = true;
            self.bring_tab_forward(next, false);
        }
        self.focus_terminal_panel();
    }

    /// The shown session's tabs, in the order they were opened.
    pub fn terminal_tabs(&self) -> Vec<u64> {
        let owner = &self.terminal_panel.owner;
        let mut tabs: Vec<u64> = self
            .parked_panels
            .iter()
            .filter(|tab| &tab.owner == owner)
            .map(|tab| tab.seq)
            .chain((self.terminal_panel.id.is_some()).then_some(self.terminal_panel.seq))
            .collect();
        tabs.sort_unstable();
        tabs
    }

    /// Any terminal tab's shell running in this window, shown or not.
    pub fn has_terminal_shells(&self) -> bool {
        self.terminal_panel.id.is_some() || !self.parked_panels.is_empty()
    }

    /// The session the panel belongs to: the one the detail keeps in view.
    fn panel_owner(&self) -> Option<SessionId> {
        self.shown_session().map(|session| session.id)
    }

    /// Show the panel of the session in view, never another's: the panel
    /// shown so far is parked with its tabs and streams, and this session's
    /// comes back as it was left (open or hidden, on the tab it showed).
    pub(super) fn follow_panel_owner(&mut self) {
        let owner = self.panel_owner();
        if self.terminal_panel.owner == owner {
            return;
        }
        if self.focus == Focus::Terminal {
            self.focus = Focus::List;
        }
        self.came_from_terminal = false;
        if self.mouse_grab == Some(super::MouseGrab::Panel) {
            self.mouse_grab = None;
        }
        let next = match self
            .parked_panels
            .iter()
            .position(|tab| tab.owner == owner && tab.active)
        {
            Some(index) => self.parked_panels.remove(index),
            None => TerminalPanel {
                owner,
                ..TerminalPanel::default()
            },
        };
        self.bring_tab_forward(next, true);
    }

    /// Show `next` in the panel and park the tab shown so far: its
    /// session's tab to show again when `session_switch`, else one of the
    /// panel's other tabs. The panel keeps the window's split sizes.
    fn bring_tab_forward(&mut self, mut next: TerminalPanel, session_switch: bool) {
        self.demote_terminal_panel();
        let mut previous = std::mem::take(&mut self.terminal_panel);
        next.height_percent = previous.height_percent;
        next.width_percent = previous.width_percent;
        next.active = false;
        next.dragging = false;
        previous.dragging = false;
        previous.active = session_switch;
        if !session_switch {
            previous.open = false;
        }
        if previous.id.is_some() {
            self.parked_panels.push(previous);
        }
        self.terminal_panel = next;
    }

    /// Apply `f` to the parked tab `seq` as if it were shown, with nothing
    /// it does reaching focus, the footer or the session's size. A tab
    /// whose shell ended goes; its session then shows another of its tabs.
    fn with_parked<R>(&mut self, seq: u64, f: impl FnOnce(&mut App) -> R) -> Option<R> {
        let index = self.parked_panels.iter().position(|tab| tab.seq == seq)?;
        let mut tab = self.parked_panels.swap_remove(index);
        let (active, open) = (tab.active, tab.open);
        std::mem::swap(&mut self.terminal_panel, &mut tab);
        self.panel_in_background = true;
        let result = f(self);
        self.panel_in_background = false;
        std::mem::swap(&mut self.terminal_panel, &mut tab);
        if tab.id.is_some() {
            self.parked_panels.push(tab);
        } else if active {
            if let Some(other) = self
                .parked_panels
                .iter_mut()
                .filter(|other| other.owner == tab.owner)
                .max_by_key(|other| other.seq)
            {
                other.active = true;
                other.open = open;
            }
        }
        Some(result)
    }

    /// An attach result for whichever tab asked for it.
    pub(super) fn route_panel_attached(
        &mut self,
        generation: u64,
        result: Result<TerminalStream, ApiError>,
    ) {
        if generation == self.terminal_panel.epoch {
            return self.on_panel_attached(generation, result);
        }
        match self
            .parked_panels
            .iter()
            .find(|tab| tab.epoch == generation)
            .map(|tab| tab.seq)
        {
            Some(seq) => {
                self.with_parked(seq, |app| app.on_panel_attached(generation, result));
            }
            None => {
                if let Ok(stream) = result {
                    stream.detach();
                }
            }
        }
    }

    /// A stream event for whichever tab it belongs to.
    pub(super) fn route_panel_event(&mut self, generation: u64, event: AttachEvent) {
        if generation == self.terminal_panel.epoch {
            return self.on_panel_attach_event(generation, event);
        }
        if let Some(seq) = self
            .parked_panels
            .iter()
            .find(|tab| tab.epoch == generation)
            .map(|tab| tab.seq)
        {
            self.with_parked(seq, |app| app.on_panel_attach_event(generation, event));
        }
    }

    /// Open or re-attach every tab that waits for it, shown or not.
    pub(super) fn sync_terminal_panels(&mut self) {
        self.sync_terminal_panel();
        let waiting: Vec<u64> = self
            .parked_panels
            .iter()
            .filter(|tab| tab.needs_open || tab.reconnect)
            .map(|tab| tab.seq)
            .collect();
        for seq in waiting {
            self.with_parked(seq, |app| app.sync_terminal_panel());
        }
    }

    /// The daemon went away: every tab re-attaches once it's back.
    pub(super) fn terminal_panels_disconnected(&mut self) {
        self.terminal_panel_disconnected();
        let parked: Vec<u64> = self.parked_panels.iter().map(|tab| tab.seq).collect();
        for seq in parked {
            self.with_parked(seq, |app| app.terminal_panel_disconnected());
        }
    }

    /// A session that's gone ends its panel's shells.
    pub(super) fn end_session_panels(&mut self, id: &SessionId) {
        self.follow_panel_owner();
        self.parked_panels
            .retain(|tab| tab.owner.as_ref() != Some(id));
    }

    pub fn focus_terminal_panel(&mut self) {
        if !self.terminal_panel.open {
            return;
        }
        if self.focus == Focus::Session {
            self.leave_session_focus(true);
        }
        self.focus = Focus::Terminal;
        self.drawer_lost_keys();
        self.zoomed = false;
        self.came_from_terminal = false;
        self.terminal_panel.needs_refocus = false;
        self.pending_key = None;
        self.list.filter_editing = false;
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
        self.terminal_panel.epoch = next_id();
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
                if !self.panel_in_background {
                    self.set_status("Terminal ended while disconnected; open a new pane with T.");
                }
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
            if self.panel_in_background
                || self.focus != Focus::Terminal
                || !self.terminal_panel.open
            {
                if let Err(error) = attachment.change_role(StreamCommand::Demote) {
                    self.terminal_panel.message =
                        Some(format!("Can't release stale terminal control: {error}"));
                    return;
                }
                attachment.role = Role::Observer;
            } else {
                attachment.sent_size = Some((attached.cols, attached.rows));
            }
        } else if self.focus == Focus::Terminal && !self.panel_in_background {
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

    /// Shell exit is not a saved session: close the tab and release its
    /// lease. The panel shows the session's other tab, if it has one; with
    /// none, it closes.
    fn finish_terminal_panel(&mut self) {
        let (was_open, was_focused) = (
            self.terminal_panel.open,
            self.focus == Focus::Terminal && !self.panel_in_background,
        );
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
        if self.panel_in_background {
            return;
        }
        let owner = self.terminal_panel.owner.clone();
        if let Some(index) = self
            .parked_panels
            .iter()
            .enumerate()
            .filter(|(_, tab)| tab.owner == owner)
            .max_by_key(|(_, tab)| tab.seq)
            .map(|(index, _)| index)
        {
            let mut next = self.parked_panels.remove(index);
            next.open = was_open;
            self.bring_tab_forward(next, false);
            if was_focused && was_open {
                self.focus_terminal_panel();
            }
            return;
        }
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
                let focused = self.focus == Focus::Terminal && !self.panel_in_background;
                if role == Role::Observer && focused {
                    self.focus = Focus::List;
                    self.came_from_terminal = true;
                } else if role == Role::Controller && !focused {
                    self.demote_terminal_panel();
                }
            }
            AttachEvent::Event(StreamEvent::Title { title }) => {
                self.terminal_panel.title = Some(title)
            }
            AttachEvent::Event(StreamEvent::Cwd { path }) => self.terminal_panel.cwd = Some(path),
            AttachEvent::Event(StreamEvent::Clipboard { selection, data }) => {
                if self.focus == Focus::Terminal
                    && !self.panel_in_background
                    && matches!(self.mode, AppMode::Normal)
                    && self
                        .terminal_panel
                        .attachment
                        .as_ref()
                        .is_some_and(|a| a.role == Role::Controller)
                {
                    self.write_clipboard(&selection, &data);
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
                    // Its drags and release go to it wherever they are.
                    if matches!(event.kind, MouseEventKind::Down(_)) {
                        self.mouse_grab = Some(super::MouseGrab::Panel);
                    }
                    return;
                }
            }
        }
        if let Some(terminal) = &mut self.terminal_panel.terminal {
            match event.kind {
                MouseEventKind::ScrollUp => terminal.scroll(3),
                MouseEventKind::ScrollDown => terminal.scroll(-3),
                // A program that doesn't take the mouse: a drag selects its text.
                MouseEventKind::Down(crossterm::event::MouseButton::Left) => {
                    terminal.start_selection(&event, pane);
                }
                _ => {}
            }
        }
    }

    /// A drag or release of a press the panel's program took
    /// (`mouse_held_by_program`), wherever it is; `false` once the panel
    /// no longer has the keys.
    pub(super) fn held_panel_mouse(
        &mut self,
        event: MouseEvent,
        screen: &super::screen::Screen,
    ) -> bool {
        let pane = screen.terminal_panel.map(panel_output);
        let bytes = match (&self.terminal_panel.terminal, pane) {
            (Some(terminal), Some(pane))
                if self.focus == Focus::Terminal && !self.terminal_panel.awaiting_checkpoint =>
            {
                terminal.held_mouse_bytes(&event, pane)
            }
            _ => return false,
        };
        if let Some(bytes) = bytes {
            self.panel_input(bytes);
        }
        true
    }

    pub fn terminal_panel_size(&self) -> (u16, u16) {
        self.screen_for(Focus::Terminal)
            .terminal_panel
            .map(panel_output)
            .map(|area| (area.width.max(2), area.height.max(2)))
            .unwrap_or((80, 12))
    }

    pub(super) fn drag_terminal_panel(&mut self, column: u16, row: u16) {
        let screen = self.screen();
        let (Some(main), Some(panel)) = (screen.detail, screen.terminal_panel) else {
            return;
        };
        if screen.terminal_panel_beside {
            // The share of the columns right of the divider; the geometry
            // keeps both sides their minimum.
            let total = panel
                .right()
                .saturating_sub(main.x)
                .saturating_sub(1)
                .max(1);
            let width = panel.right().saturating_sub(column).saturating_sub(1);
            self.terminal_panel.width_percent =
                ((u32::from(width) * 100 / u32::from(total)) as u16).clamp(20, 80);
        } else {
            let total = panel.bottom().saturating_sub(main.y).max(1);
            let height = panel.bottom().saturating_sub(row).saturating_sub(1);
            self.terminal_panel.height_percent =
                ((u32::from(height) * 100 / u32::from(total)) as u16).clamp(20, 70);
        }
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
        if self.panel_in_background
            || self.focus != Focus::Terminal
            || self.screen().terminal_panel.is_none()
        {
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
        if self.focus == Focus::Terminal && !self.panel_in_background {
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
