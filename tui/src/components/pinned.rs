//! The drawer's Pinned tab (`app/pinned.rs`) under its band: a row per
//! pinned session, as the list draws it, without the gutter's pin (the
//! pointer's row shows the gold `★` that unpins it).

use ratatui::{layout::Rect, style::Style, text::Span, widgets::Paragraph, Frame};

use super::session_list::{first_visible_row, pinned_row_lines};
use crate::app::pinned::PINNED_ROW;
use crate::app::{App, Hit};
use crate::theme;

/// The pinned sessions in `area`, scrolled to the selected one.
pub fn draw_pinned(f: &mut Frame, app: &App, area: Rect, hits: &mut Vec<(Rect, Hit)>) {
    let rows = app.list.pinned();
    if rows.is_empty() {
        let note = Span::styled(
            "   no pinned sessions",
            Style::default().fg(theme::palette().text_muted),
        );
        let row = Rect {
            y: area.y + 1,
            height: 1,
            ..area
        };
        f.render_widget(Paragraph::new(note), row.intersection(area));
        return;
    }
    let selected = app.pinned_index(&rows);
    let keys = app.drawer_has_keys();
    let first = first_visible_row(&vec![PINNED_ROW; rows.len()], selected, area.height);
    let mut y = area.y;
    for (index, session) in rows.iter().enumerate().skip(first) {
        if y + PINNED_ROW > area.bottom() {
            break;
        }
        let row = Rect {
            y,
            height: PINNED_ROW,
            ..area
        };
        let hovered = app.hovered_pinned == Some(index);
        let dragged = app.dragged_pinned() == Some(&session.id);
        let (lines, pin) = pinned_row_lines(
            app,
            session,
            row,
            keys && index == selected,
            hovered,
            dragged,
        );
        f.render_widget(Paragraph::new(lines), row);
        if let Some(pin) = pin {
            hits.push((pin, Hit::PinnedToggle(index)));
        }
        hits.push((row, Hit::PinnedRow(index)));
        y += PINNED_ROW;
    }
}
