//! A quiet scrollbar for text taller than where it's drawn: a thin muted
//! thumb on a dim track down an area's last column (Help, a heartbeat's
//! prompt in its form and read whole).

use ratatui::{layout::Rect, Frame};

use crate::theme;

/// Draw the scrollbar down `body`'s last column: `total` rows, `first` the
/// first in view, `max_first` the largest it can be.
///
/// ```text
/// line 1      │
/// line 2      ┃   <- the thumb: where the rows in view are
/// line 3      ┃
/// line 4      │
/// ```
pub fn draw_scrollbar(f: &mut Frame, body: Rect, first: usize, max_first: usize, total: usize) {
    if body.width == 0 || body.height == 0 {
        return;
    }
    let palette = theme::palette();
    let height = usize::from(body.height);
    let thumb = ((height * height) / total.max(1)).clamp(1, height);
    let track = height - thumb;
    let offset = first.min(max_first) * track / max_first.max(1);
    let x = body.right() - 1;
    let buf = f.buffer_mut();
    for row in 0..height {
        let in_thumb = row >= offset && row < offset + thumb;
        let (symbol, color) = match in_thumb {
            true => ("┃", palette.text_muted),
            false => ("│", palette.border_dim),
        };
        buf.get_mut(x, body.y + row as u16)
            .set_symbol(symbol)
            .set_fg(color);
    }
}
