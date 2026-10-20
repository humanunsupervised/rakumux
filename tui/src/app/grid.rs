//! Bounded, read-only terminal previews. This module deliberately holds no
//! input/control attachment: a lease can only detach. All calls use
//! SessionApi, and every async result is checked against a card generation.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use session_core::api::{
    ApiError, AttachRequest, ErrorCode, ReadRequest, ReadResult, ReadSource, Role, StreamCommand,
    StreamEvent, TextFormat,
};
use session_core::client::{AttachEvent, AttachSender, AttachStream};
use session_core::ids::SessionId;
use session_core::model::SessionStatus;

use super::detail::{classify_output, DetailView, OutputAction};
use super::runtime::{cancel_flag, AppEvent, CancelFlag};
use super::types::Focus;
use super::{App, Selection};
use crate::layout::grid::{GridPage, MAX_CARDS};
use crate::layout::SessionLayout;
use crate::terminal_view::TerminalView;

const MAX_CELLS: usize = 250_000;
const MAX_BYTES: usize = 256 * 1024;
const SAVED_ROWS: u32 = 120;
const RETRY: Duration = Duration::from_secs(2);

/// A bounded mailbox prevents a busy terminal from growing the UI event
/// queue indefinitely. Overflow discards the partial screen and reattaches.
#[derive(Clone, Default)]
pub struct PreviewMailbox(Arc<Mutex<Mailbox>>);

#[derive(Default)]
struct Mailbox {
    events: VecDeque<AttachEvent>,
    bytes: usize,
    overflowed: bool,
}

impl PreviewMailbox {
    /// Whether the caller must send a new wake-up notification.
    pub fn push(&self, event: AttachEvent) -> bool {
        if matches!(
            &event,
            AttachEvent::Event(
                StreamEvent::Title { .. }
                    | StreamEvent::Cwd { .. }
                    | StreamEvent::Clipboard { .. }
                    | StreamEvent::Bell
                    | StreamEvent::RoleChanged {
                        role: Role::Observer
                    }
            )
        ) {
            return false;
        }
        let mut state = self.0.lock().unwrap_or_else(|e| e.into_inner());
        let wake = state.events.is_empty();
        if state.overflowed {
            return false;
        }
        let bytes = match &event {
            AttachEvent::Output { bytes, .. } => bytes.len(),
            AttachEvent::Event(StreamEvent::Resized { checkpoint, .. }) => checkpoint.len(),
            _ => 0,
        };
        if state.bytes + bytes > MAX_BYTES || state.events.len() >= 64 {
            state.events.clear();
            state.bytes = 0;
            state.overflowed = true;
            state
                .events
                .push_back(AttachEvent::Event(StreamEvent::Lagged { offset: 0 }));
        } else {
            state.bytes += bytes;
            state.events.push_back(event);
        }
        wake
    }

    pub fn drain(&self) -> VecDeque<AttachEvent> {
        let mut state = self.0.lock().unwrap_or_else(|e| e.into_inner());
        state.bytes = 0;
        state.overflowed = false;
        std::mem::take(&mut state.events)
    }
}

/// Only cancellation/detach is available to a background preview.
struct PreviewLease {
    sender: AttachSender,
    cancel: CancelFlag,
}

impl Drop for PreviewLease {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
        let _ = self.sender.command(StreamCommand::Detach);
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum PreviewState {
    Loading,
    Live,
    Saved,
    Error(String),
}

pub struct Preview {
    pub state: PreviewState,
    pub terminal: Option<TerminalView>,
    source: SessionStatus,
    generation: u64,
    cancel: CancelFlag,
    lease: Option<PreviewLease>,
    offset: u64,
    started: bool,
    retry_at: Option<Instant>,
}

impl Drop for Preview {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

#[derive(Default)]
pub struct GridPreviews {
    cards: HashMap<SessionId, Preview>,
    generation: u64,
    /// Includes cancelled requests until the blocking call returns, so
    /// rapid paging cannot create an unlimited number of attach jobs.
    pending: Arc<AtomicUsize>,
}

impl GridPreviews {
    pub fn get(&self, id: &SessionId) -> Option<&Preview> {
        self.cards.get(id)
    }

    pub fn clear(&mut self) {
        self.cards.clear();
    }
}

struct PendingSlot(Arc<AtomicUsize>);

impl Drop for PendingSlot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}

impl App {
    /// Grid browsing never has a hidden detail terminal claiming its size.
    pub fn browsing_grid(&self) -> bool {
        self.session_layout() == SessionLayout::Grid && self.focus == Focus::List
    }

    pub fn grid_page(&self) -> GridPage {
        let visible = self.list.visible();
        let selected = match self.list.selection() {
            Selection::NewSession => visible.len(),
            Selection::Session(id) => visible.iter().position(|s| &s.id == id).unwrap_or(0),
            _ => 0,
        };
        GridPage::new(
            self.screen().grid.unwrap_or_default(),
            visible.len() + 1,
            selected,
        )
    }

    pub fn move_grid_selection(&mut self, delta: isize) {
        let ids: Vec<_> = self.list.visible().iter().map(|s| s.id.clone()).collect();
        let current = match self.list.selection() {
            Selection::NewSession => ids.len(),
            Selection::Session(id) => ids.iter().position(|s| s == id).unwrap_or(0),
            _ => 0,
        };
        let index = current.saturating_add_signed(delta).min(ids.len());
        self.list.select(match ids.get(index) {
            Some(id) => Selection::Session(id.clone()),
            None => Selection::NewSession,
        });
        self.reconcile_detail();
    }

    pub fn move_grid_page(&mut self, delta: isize) {
        self.move_grid_selection(delta * self.grid_page().capacity as isize);
    }

    /// Synchronize at state transitions and ticks, never during card drawing.
    pub(super) fn sync_grid(&mut self) {
        if !self.browsing_grid() || self.pending_focus.is_some() || self.creating_session {
            self.grid.clear();
            return;
        }
        if self.detail.view != DetailView::Empty {
            self.detail.retarget(DetailView::Empty);
        }
        if !self.connection.is_connected() {
            self.grid.clear();
            return;
        }
        if matches!(
            self.list.selection(),
            Selection::Folders | Selection::FolderHeader(_)
        ) {
            let first = self.list.visible().first().map(|s| s.id.clone());
            self.list.select(
                first
                    .map(Selection::Session)
                    .unwrap_or(Selection::NewSession),
            );
        }
        let page = self.grid_page();
        let visible = self.list.visible();
        let wanted: Vec<_> = page
            .cards
            .iter()
            .filter_map(|(index, _)| visible.get(*index))
            .filter(|s| !self.is_self_session(&s.id))
            .map(|s| (*s).clone())
            .collect();
        let now = Instant::now();
        self.grid.cards.retain(|id, card| {
            wanted
                .iter()
                .any(|s| &s.id == id && s.status == card.source)
                && card.retry_at.is_none_or(|at| at > now)
        });
        for session in wanted {
            if !self.grid.cards.contains_key(&session.id) {
                self.grid.generation += 1;
                self.grid.cards.insert(
                    session.id.clone(),
                    Preview {
                        state: PreviewState::Loading,
                        terminal: None,
                        source: session.status,
                        generation: self.grid.generation,
                        cancel: cancel_flag(),
                        lease: None,
                        offset: 0,
                        started: false,
                        retry_at: None,
                    },
                );
            }
            let Some(api) = self.api() else { break };
            let Some(card) = self.grid.cards.get_mut(&session.id) else {
                continue;
            };
            if card.started || self.grid.pending.load(Ordering::Relaxed) >= MAX_CARDS {
                continue;
            }
            card.started = true;
            let id = session.id;
            let generation = card.generation;
            let cancel = card.cancel.clone();
            self.grid.pending.fetch_add(1, Ordering::Relaxed);
            let slot = PendingSlot(self.grid.pending.clone());
            self.exec.job(move || {
                let _slot = slot;
                let cancelled = || ApiError::new(ErrorCode::Internal, "preview cancelled");
                if session.status == SessionStatus::Live {
                    // Observer attaches do not size the PTY. Use its known
                    // dimensions, not the card or this window's dimensions.
                    let (cols, rows) = session
                        .terminal
                        .map(|t| (t.cols, t.rows))
                        .unwrap_or((80, 24));
                    let result = if cancel.load(Ordering::Relaxed) {
                        Err(cancelled())
                    } else {
                        api.attach(AttachRequest {
                            id: id.clone(),
                            role: Role::Observer,
                            cols,
                            rows,
                        })
                    };
                    let result = if cancel.load(Ordering::Relaxed) {
                        drop(result);
                        Err(cancelled())
                    } else {
                        result
                    };
                    AppEvent::PreviewAttached {
                        id,
                        generation,
                        result,
                    }
                } else {
                    let result = if cancel.load(Ordering::Relaxed) {
                        Err(cancelled())
                    } else {
                        api.read(ReadRequest {
                            id: id.clone(),
                            source: ReadSource::Recent,
                            lines: Some(SAVED_ROWS),
                            format: TextFormat::Ansi,
                        })
                    };
                    AppEvent::PreviewSaved {
                        id,
                        generation,
                        result,
                    }
                }
            });
        }
    }

    pub(super) fn on_preview_attached(
        &mut self,
        id: SessionId,
        generation: u64,
        result: Result<AttachStream, ApiError>,
    ) {
        let Some(card) = self
            .grid
            .cards
            .get_mut(&id)
            .filter(|c| c.generation == generation && c.source == SessionStatus::Live)
        else {
            return;
        };
        match result {
            Ok(stream) => {
                let attached = &stream.attached;
                let result = if attached.role != Role::Observer
                    || attached.session.id != id
                    || attached.session.is_self
                {
                    Err("Daemon returned an invalid observer attachment".into())
                } else {
                    preview_terminal(attached.rows, attached.cols, &attached.checkpoint)
                };
                match result {
                    Ok(terminal) => {
                        card.terminal = Some(terminal);
                        card.offset = attached.offset;
                        card.state = PreviewState::Live;
                        card.lease = Some(PreviewLease {
                            sender: stream.sender(),
                            cancel: card.cancel.clone(),
                        });
                        self.exec
                            .pump_preview(id, generation, stream, card.cancel.clone());
                    }
                    Err(message) => card.state = PreviewState::Error(message),
                }
            }
            Err(error) => {
                card.state = PreviewState::Error(format!("Can't preview: {error}"));
                card.retry_at = Some(Instant::now() + RETRY);
            }
        }
    }

    pub(super) fn on_preview_saved(
        &mut self,
        id: SessionId,
        generation: u64,
        result: Result<ReadResult, ApiError>,
    ) {
        let Some(card) = self
            .grid
            .cards
            .get_mut(&id)
            .filter(|c| c.generation == generation && c.source != SessionStatus::Live)
        else {
            return;
        };
        match result {
            Ok(read) if read.text.is_empty() => {
                card.terminal = None;
                card.state = PreviewState::Saved;
            }
            Ok(read)
                if valid_size(read.rows.max(2), read.cols.max(2))
                    && read.text.len() <= MAX_BYTES =>
            {
                // Saved reads report the number of nonempty rows, not a
                // PTY size: even a one-row/one-column result is valid.
                let rows = read.rows.max(2);
                let cols = read.cols.max(2);
                let mut terminal = TerminalView::with_scrollback(rows, cols, 0);
                // The read is capped, but an older daemon may ignore lines.
                let lines: Vec<_> = read.text.lines().rev().take(SAVED_ROWS as usize).collect();
                let text = lines.into_iter().rev().collect::<Vec<_>>().join("\n");
                terminal.reset_screen_to_text(rows, cols, &text);
                card.terminal = Some(terminal);
                card.state = PreviewState::Saved;
            }
            Ok(_) => card.state = PreviewState::Error("Saved screen exceeds preview limits".into()),
            Err(error) => card.state = PreviewState::Error(format!("No saved output: {error}")),
        }
    }

    pub(super) fn on_preview_event(&mut self, id: &SessionId, generation: u64, event: AttachEvent) {
        let Some(card) = self
            .grid
            .cards
            .get_mut(id)
            .filter(|c| c.generation == generation && c.lease.is_some())
        else {
            return;
        };
        match event {
            AttachEvent::Output { offset, bytes } => {
                match classify_output(card.offset, offset, bytes.len()) {
                    OutputAction::Feed { skip } => {
                        card.offset = offset + bytes.len() as u64;
                        if let Some(terminal) = card.terminal.as_mut() {
                            terminal.feed(&bytes[skip..]);
                        }
                    }
                    OutputAction::Duplicate => {}
                    OutputAction::Gap => {
                        self.grid.cards.remove(id);
                    }
                }
            }
            AttachEvent::Event(StreamEvent::Resized {
                rows,
                cols,
                checkpoint,
                offset,
            }) => {
                if offset < card.offset {
                    return;
                }
                match preview_terminal(rows, cols, &checkpoint) {
                    Ok(terminal) => {
                        card.terminal = Some(terminal);
                        card.offset = offset;
                    }
                    Err(message) => {
                        card.state = PreviewState::Error(message);
                        card.terminal = None;
                        card.lease = None;
                    }
                }
            }
            AttachEvent::Event(StreamEvent::Lagged { .. }) => {
                self.grid.cards.remove(id);
            }
            AttachEvent::Event(StreamEvent::Exited { code }) => {
                card.lease = None;
                card.source = SessionStatus::Exited;
                card.state = PreviewState::Saved;
                self.list.mark_exited(id, code);
            }
            AttachEvent::Closed(error) => {
                card.lease = None;
                card.terminal = None;
                card.state =
                    PreviewState::Error(error.unwrap_or_else(|| "Preview disconnected".into()));
                card.retry_at = Some(Instant::now() + RETRY);
            }
            // Never relay clipboard/bell/query replies. Even an unsolicited
            // controller role is not accepted: drop this attachment.
            AttachEvent::Event(StreamEvent::RoleChanged {
                role: Role::Controller,
            }) => {
                card.lease = None;
                card.terminal = None;
                card.state = PreviewState::Error("Preview is no longer an observer".into());
            }
            AttachEvent::Event(_) => {}
        }
    }
}

fn valid_size(rows: u16, cols: u16) -> bool {
    rows >= 2 && cols >= 2 && usize::from(rows) * usize::from(cols) <= MAX_CELLS
}

fn preview_terminal(rows: u16, cols: u16, checkpoint: &str) -> Result<TerminalView, String> {
    if !valid_size(rows, cols) || checkpoint.len() > MAX_BYTES {
        return Err("Terminal exceeds preview limits; Enter to open it".into());
    }
    let bytes = super::sessions::decode_checkpoint(checkpoint)?;
    let mut terminal = TerminalView::with_scrollback(rows, cols, 0);
    terminal.reset_screen(rows, cols, &bytes);
    Ok(terminal)
}

#[cfg(test)]
mod tests;
