//! Drawing a `vt100::Screen` straight into a ratatui `Buffer`: one terminal
//! cell becomes one buffer cell (symbol + style), wide characters span two
//! cells, and a terminal of a different size than the pane is cropped or
//! padded.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use vt::vt100;

use super::DefaultColors;

/// The terminal cell shown at a pane's top-left corner.
///
/// It is `(0, 0)` whenever the terminal fits the pane. A terminal larger than
/// the pane (an observer watching a bigger controller) is panned just far
/// enough to keep the cursor in view; while scrolled back, the bottom rows
/// stay in view so scrolling moves the picture one row at a time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Viewport {
    pub row: u16,
    pub col: u16,
}

/// Start of the visible window along one axis: 0 when `len` fits in `view`,
/// otherwise the smallest shift that keeps `focus` visible.
fn window_start(len: u16, view: u16, focus: u16) -> u16 {
    if len <= view {
        return 0;
    }
    focus.saturating_add(1).saturating_sub(view).min(len - view)
}

/// Cursor position clamped onto the grid: vt100 reports `col == cols` while
/// a line wrap is pending, which terminals draw on the last column.
fn cursor_cell(screen: &vt100::Screen) -> (u16, u16) {
    let (rows, cols) = screen.size();
    let (row, col) = screen.cursor_position();
    (
        row.min(rows.saturating_sub(1)),
        col.min(cols.saturating_sub(1)),
    )
}

pub(super) fn viewport(screen: &vt100::Screen, pane: Rect, scrolled_back: bool) -> Viewport {
    let (rows, cols) = screen.size();
    let (cursor_row, cursor_col) = cursor_cell(screen);
    let row = if scrolled_back {
        rows.saturating_sub(pane.height)
    } else {
        window_start(rows, pane.height, cursor_row)
    };
    Viewport {
        row,
        col: window_start(cols, pane.width, cursor_col),
    }
}

/// Terminal colour → ratatui colour; the program's default colour becomes
/// `default`.
pub(super) fn color(color: vt100::Color, default: Color) -> Color {
    match color {
        vt100::Color::Default => default,
        vt100::Color::Idx(index) => Color::Indexed(index),
        vt100::Color::Rgb(r, g, b) => Color::Rgb(r, g, b),
    }
}

/// Complete style of a terminal cell (both colours always set). Reverse
/// video is left to the terminal, so a reversed cell in default colours
/// swaps `colors` too.
pub(super) fn cell_style(cell: &vt100::Cell, colors: DefaultColors) -> Style {
    let mut modifier = Modifier::empty();
    if cell.bold() {
        modifier |= Modifier::BOLD;
    }
    if cell.dim() {
        modifier |= Modifier::DIM;
    }
    if cell.italic() {
        modifier |= Modifier::ITALIC;
    }
    if cell.underline() {
        modifier |= Modifier::UNDERLINED;
    }
    if cell.inverse() {
        modifier |= Modifier::REVERSED;
    }
    Style::new()
        .fg(color(cell.fgcolor(), colors.fg))
        .bg(color(cell.bgcolor(), colors.bg))
        .add_modifier(modifier)
}

/// Draw the window of `screen` that starts at `view` into `area` (which must
/// lie inside `buf`). Every cell of `area` is overwritten: cells beyond the
/// terminal's edge are blank, in `colors`.
pub(super) fn draw(
    screen: &vt100::Screen,
    area: Rect,
    buf: &mut Buffer,
    view: Viewport,
    colors: DefaultColors,
) {
    let (rows, cols) = screen.size();
    let blank = Style::new().fg(colors.fg).bg(colors.bg);
    for y in 0..area.height {
        for x in 0..area.width {
            let out = buf.get_mut(area.x + x, area.y + y);
            out.reset();
            out.set_style(blank);
            let (Some(row), Some(col)) = (view.row.checked_add(y), view.col.checked_add(x)) else {
                continue;
            };
            if row >= rows || col >= cols {
                continue;
            }
            let Some(cell) = screen.cell(row, col) else {
                continue;
            };
            if cell.is_wide_continuation() {
                // Covered by the wide character to its left, unless that one
                // is cropped off the pane: then show a blank in its colours.
                if x == 0 {
                    if let Some(lead) = col.checked_sub(1).and_then(|c| screen.cell(row, c)) {
                        out.set_style(cell_style(lead, colors));
                    }
                }
                continue;
            }
            out.set_style(cell_style(cell, colors));
            // A wide character whose right half falls outside the pane (or
            // the terminal) would spill into the neighbouring cell: blank it.
            let fits = !cell.is_wide() || (x + 1 < area.width && col + 1 < cols);
            if fits && cell.has_contents() {
                out.set_symbol(cell.contents());
            }
        }
    }
}

/// Absolute position for `Frame::set_cursor`, when the program shows its
/// cursor and it lies inside the visible window.
pub(super) fn cursor(screen: &vt100::Screen, area: Rect, view: Viewport) -> Option<(u16, u16)> {
    if screen.hide_cursor() {
        return None;
    }
    let (row, col) = cursor_cell(screen);
    let dy = row.checked_sub(view.row)?;
    let dx = col.checked_sub(view.col)?;
    (dy < area.height && dx < area.width).then(|| (area.x + dx, area.y + dy))
}

/// `[offset/history]` in the pane's top-right corner (like tmux copy mode),
/// skipped when the pane is too narrow for it.
pub(super) fn draw_scroll_marker(
    screen: &vt100::Screen,
    area: Rect,
    buf: &mut Buffer,
    view: Viewport,
    offset: usize,
    history: usize,
) {
    let label = format!("[{offset}/{history}]");
    let Ok(width) = u16::try_from(label.len()) else {
        return;
    };
    if area.height == 0 || width > area.width {
        return;
    }
    let x = area.right() - width;
    // A wide character just left of the label would overlap its first cell.
    if x > area.x {
        let wide = view
            .col
            .checked_add(x - 1 - area.x)
            .and_then(|col| screen.cell(view.row, col))
            .is_some_and(|cell| cell.is_wide());
        if wide {
            buf.get_mut(x - 1, area.y).set_symbol(" ");
        }
    }
    let style = Style::new().add_modifier(Modifier::REVERSED);
    buf.set_string(x, area.y, &label, style);
}
