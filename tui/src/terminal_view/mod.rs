//! Live terminal pane: a passive `vt::Emulator` mirroring a session's
//! terminal, rendered into a ratatui buffer, plus translation of crossterm
//! key/mouse/paste events into the bytes the program expects.

mod input;
mod widget;

#[cfg(test)]
mod tests;

use crossterm::event::{KeyEvent, MouseEvent};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Color;

/// Rows a view keeps above the visible screen for local scrolling, unless
/// built with [`TerminalView::with_scrollback`]. A cell is ~32 bytes, so
/// this is ~3.8 MB at 120 columns when full.
pub const DEFAULT_SCROLLBACK_ROWS: usize = 1_000;

/// The screen as it was when the user scrolled back. Live output keeps
/// updating the emulator but not this copy, so history being read doesn't
/// move under the reader (like tmux copy mode).
struct Frozen {
    screen: vt::vt100::Screen,
    /// Rows of history above the frozen screen.
    history: usize,
}

pub struct TerminalView {
    emulator: vt::Emulator,
    /// Rows scrolled back from the live bottom (0 = live).
    scroll_offset: usize,
    /// Rows of history the emulator retains (see [`DEFAULT_SCROLLBACK_ROWS`]).
    scrollback: usize,
    /// Set while scrolled back.
    frozen: Option<Frozen>,
}

/// What a program's default colours (SGR 39/49, and cells it never
/// coloured) are drawn as. Colours the program sets itself are kept.
/// `Color::Reset` leaves them to the outer terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DefaultColors {
    pub fg: Color,
    pub bg: Color,
}

impl DefaultColors {
    /// The outer terminal's own colours, so a program looks in Rakumux as
    /// it does outside it, without the Rakumux theme.
    pub const NATIVE: DefaultColors = DefaultColors {
        fg: Color::Reset,
        bg: Color::Reset,
    };
}

/// vt100 panics below 2×2 (wrapping on a single row, or a wide character
/// in a single column), so smaller sizes are raised to that.
const MIN_SIZE: u16 = 2;

fn clamp_size(rows: u16, cols: u16) -> (u16, u16) {
    (rows.max(MIN_SIZE), cols.max(MIN_SIZE))
}

impl TerminalView {
    /// An empty `rows`×`cols` screen.
    pub fn new(rows: u16, cols: u16) -> Self {
        Self::with_scrollback(rows, cols, DEFAULT_SCROLLBACK_ROWS)
    }

    /// An empty `rows`×`cols` screen that retains up to `scrollback` rows
    /// of history for [`scroll`](Self::scroll).
    pub fn with_scrollback(rows: u16, cols: u16, scrollback: usize) -> Self {
        let (rows, cols) = clamp_size(rows, cols);
        Self {
            emulator: vt::Emulator::passive(rows, cols, scrollback),
            scroll_offset: 0,
            scrollback,
            frozen: None,
        }
    }

    /// Start over at `rows`×`cols` from a daemon checkpoint (attach,
    /// `Resized`), or from saved scrollback when showing a dead session.
    ///
    /// Local history and scroll position are dropped. When `checkpoint`
    /// holds more lines than the configured scrollback (history fetched with
    /// `SessionApi::read`), the new emulator retains all of them.
    pub fn reset(&mut self, rows: u16, cols: u16, checkpoint: &[u8]) {
        let lines = checkpoint.iter().filter(|&&b| b == b'\n').count();
        self.reset_with_history(rows, cols, checkpoint, self.scrollback.max(lines));
    }

    /// Apply a screen checkpoint without allocating extra history. Used by
    /// bounded, passive Grid previews, which retain no scrollback.
    pub fn reset_screen(&mut self, rows: u16, cols: u16, checkpoint: &[u8]) {
        self.reset_with_history(rows, cols, checkpoint, self.scrollback);
    }

    fn reset_with_history(&mut self, rows: u16, cols: u16, checkpoint: &[u8], history: usize) {
        let (rows, cols) = clamp_size(rows, cols);
        self.emulator = vt::Emulator::passive(rows, cols, history);
        self.emulator.feed(checkpoint);
        self.scroll_to_bottom();
    }

    /// Start over showing `text` at `rows`×`cols`: newline-separated rows as
    /// returned by `SessionApi::read` (`Text` or `Ansi`), e.g. a dead
    /// session's saved scrollback. Rows that don't fit on the screen are
    /// kept as history for [`scroll`](Self::scroll); the cursor is hidden.
    pub fn reset_to_text(&mut self, rows: u16, cols: u16, text: &str) {
        self.reset(rows, cols, &text_checkpoint(text));
    }

    /// Saved text without extra retained history, for bounded Grid cards.
    pub fn reset_screen_to_text(&mut self, rows: u16, cols: u16, text: &str) {
        self.reset_screen(rows, cols, &text_checkpoint(text));
    }

    /// Apply live output. While scrolled back the picture stays frozen;
    /// output shows once the view returns to the bottom.
    pub fn feed(&mut self, bytes: &[u8]) {
        // Passive: no replies; title/cwd/bell arrive as daemon stream events.
        self.emulator.feed(bytes);
    }

    /// `(rows, cols)` of the mirrored terminal.
    pub fn size(&self) -> (u16, u16) {
        self.emulator.size()
    }

    pub fn modes(&self) -> vt::Modes {
        self.emulator.modes()
    }

    pub fn scroll_offset(&self) -> usize {
        self.scroll_offset
    }

    /// Rows of history above the screen that [`scroll`](Self::scroll) can
    /// reach. When `scroll_offset() == history_len()` the caller has to
    /// fetch older rows with `SessionApi::read`.
    pub fn history_len(&mut self) -> usize {
        match &self.frozen {
            Some(frozen) => frozen.history,
            None => self.emulator.history_len(),
        }
    }

    /// Scroll local history by `delta` rows (positive = back in time).
    /// History beyond the visible screen is fetched by the caller via
    /// `SessionApi::read` and shown with `reset`; this only scrolls rows the
    /// emulator holds.
    pub fn scroll(&mut self, delta: i32) {
        let max = self.history_len();
        let target = (self.scroll_offset as i64 + i64::from(delta)).clamp(0, max as i64) as usize;
        if target == 0 {
            self.scroll_to_bottom();
            return;
        }
        let frozen = self.frozen.get_or_insert_with(|| Frozen {
            screen: self.emulator.screen().clone(),
            history: max,
        });
        frozen.screen.set_scrollback(target);
        self.scroll_offset = frozen.screen.scrollback();
    }

    /// Back to the live screen.
    pub fn scroll_to_bottom(&mut self) {
        self.scroll_offset = 0;
        self.frozen = None;
    }

    /// Draw into `area` of `buf`. The terminal is drawn at its own size,
    /// cropped or padded to `area`, with the program's default colours (and
    /// the padding) in `colors`. When `focused` and the cursor is visible,
    /// returns its absolute position for `Frame::set_cursor`.
    ///
    /// A terminal larger than `area` is panned to keep the cursor in view.
    /// While scrolled back, a `[offset/history]` marker shows in the
    /// top-right corner and no cursor is returned.
    pub fn render(
        &self,
        area: Rect,
        buf: &mut Buffer,
        focused: bool,
        colors: DefaultColors,
    ) -> Option<(u16, u16)> {
        let area = area.intersection(buf.area);
        if area.is_empty() {
            return None;
        }
        match &self.frozen {
            Some(frozen) => {
                let view = widget::viewport(&frozen.screen, area, true);
                widget::draw(&frozen.screen, area, buf, view, colors);
                widget::draw_scroll_marker(
                    &frozen.screen,
                    area,
                    buf,
                    view,
                    self.scroll_offset,
                    frozen.history,
                );
                None
            }
            None => {
                let screen = self.emulator.screen();
                let view = widget::viewport(screen, area, false);
                widget::draw(screen, area, buf, view, colors);
                if focused {
                    widget::cursor(screen, area, view)
                } else {
                    None
                }
            }
        }
    }

    /// Bytes for a mouse event over this view drawn in `pane` (the `area`
    /// passed to [`render`](Self::render)), with the program's modes. Maps
    /// through the same panning as `render` and ignores events beyond the
    /// terminal's edge. `None` while scrolled back.
    pub fn mouse_bytes(&self, event: &MouseEvent, pane: Rect) -> Option<Vec<u8>> {
        if self.frozen.is_some() {
            return None;
        }
        let screen = self.emulator.screen();
        let view = widget::viewport(screen, pane, false);
        let (col, row) = input::pane_position(event, pane)?;
        let col = view.col.checked_add(col)?;
        let row = view.row.checked_add(row)?;
        let (rows, cols) = screen.size();
        if row >= rows || col >= cols {
            return None;
        }
        input::encode_mouse_at(event, col, row, &self.modes())
    }
}

fn text_checkpoint(text: &str) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(text.len() + text.len() / 16 + 8);
    for (i, line) in text
        .strip_suffix('\n')
        .unwrap_or(text)
        .split('\n')
        .enumerate()
    {
        if i > 0 {
            bytes.extend_from_slice(b"\r\n");
        }
        // Each row starts from default attributes.
        bytes.extend_from_slice(b"\x1b[m");
        bytes.extend_from_slice(line.strip_suffix('\r').unwrap_or(line).as_bytes());
    }
    bytes.extend_from_slice(b"\x1b[m\x1b[?25l");
    bytes
}

/// Bytes for a key press, honouring the program's modes. `None` for keys
/// that send nothing (releases, lock and media keys, unsupported F-keys).
pub fn key_to_bytes(key: &KeyEvent, modes: &vt::Modes) -> Option<Vec<u8>> {
    input::encode_key_event(key, modes)
}

/// Bytes for a mouse event inside `pane` relative to its top-left corner,
/// without panning (tests; the TUI uses [`TerminalView::mouse_bytes`]).
#[cfg(test)]
pub fn mouse_to_bytes(event: &MouseEvent, pane: Rect, modes: &vt::Modes) -> Option<Vec<u8>> {
    let (col, row) = input::pane_position(event, pane)?;
    input::encode_mouse_at(event, col, row, modes)
}

/// Bytes for pasted text (bracketed when the program asked for it).
pub fn paste_to_bytes(text: &str, modes: &vt::Modes) -> Vec<u8> {
    vt::input::encode_paste(text, modes)
}
