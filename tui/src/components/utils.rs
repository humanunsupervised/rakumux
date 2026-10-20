use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Modifier;

use crate::theme;

/// Strongly recede saved output and the session behind a dialog.
pub(crate) const FADE_TEXT: f32 = 0.45;
/// Clearly separate the inactive preview from the session list taking keys.
pub(crate) const PREVIEW_FADE: f32 = 0.30;

/// Fade saved output or the session pane under a dialog. Palettes that can't
/// fade colours (16 colours) dim the text instead.
pub fn fade(buf: &mut Buffer, area: Rect) {
    fade_by(buf, area, FADE_TEXT);
}

/// Fade a live preview while preserving its readable text and colour cues.
pub fn fade_preview(buf: &mut Buffer, area: Rect) {
    fade_by(buf, area, PREVIEW_FADE);
}

fn fade_by(buf: &mut Buffer, area: Rect, amount: f32) {
    let p = theme::palette();
    let area = area.intersection(buf.area);
    for y in area.top()..area.bottom() {
        for x in area.left()..area.right() {
            let cell = buf.get_mut(x, y);
            match p.faded(cell.fg, amount) {
                Some(fg) => cell.fg = fg,
                None => cell.modifier |= Modifier::DIM,
            }
            if let Some(bg) = p.faded(cell.bg, amount) {
                cell.bg = bg;
            }
        }
    }
}

/// Shared 100 ms spinner for busy sessions and background operations.
pub fn get_spinner_char() -> &'static str {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    spinner_frame(now)
}

fn spinner_frame(milliseconds: u128) -> &'static str {
    let spinner_chars = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
    let index = ((milliseconds / 100) % spinner_chars.len() as u128) as usize;
    spinner_chars[index]
}

/// The narrowest a dialog gets while the screen has room for it: on a
/// phone (40–60 columns) a percentage alone leaves too little.
pub const MIN_POPUP_COLS: u16 = 48;
/// The shortest a dialog sized by percentage gets while the screen has
/// room for it.
const MIN_POPUP_ROWS: u16 = 10;

/// Columns of a dialog that takes `percent_x` of a screen `total` columns
/// wide: never less than [`MIN_POPUP_COLS`], or the screen less a column
/// on each side when that's narrower.
pub fn popup_width(total: u16, percent_x: u16) -> u16 {
    let share = u32::from(total) * u32::from(percent_x.min(100)) / 100;
    let share = u16::try_from(share).unwrap_or(total);
    share
        .max(MIN_POPUP_COLS.min(total.saturating_sub(2)))
        .min(total)
}

/// `width` × `height` in the middle of `area` (both kept within it).
fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    Rect {
        x: area.x + (area.width - width) / 2,
        y: area.y + (area.height - height) / 2,
        width,
        height,
    }
}

/// A centred popup taking `percent_x` × `percent_y` of `area`, at least
/// [`popup_width`] wide and [`MIN_POPUP_ROWS`] tall while it fits.
pub fn popup_area(area: Rect, percent_x: u16, percent_y: u16) -> Rect {
    let share = u32::from(area.height) * u32::from(percent_y.min(100)) / 100;
    let height = u16::try_from(share)
        .unwrap_or(area.height)
        .max(MIN_POPUP_ROWS.min(area.height));
    centered(area, popup_width(area.width, percent_x), height)
}

/// Rows `text` takes word-wrapped to `width` columns (as a `Paragraph`
/// with `Wrap` lays it out, close enough to size a dialog); at least one.
pub fn wrapped_rows(text: &str, width: usize) -> usize {
    if width == 0 {
        return 1;
    }
    let mut rows = 1;
    let mut used = 0;
    for word in text.split_whitespace() {
        let w = crate::layout::str_width(word);
        if used == 0 {
            used = w;
        } else if used + 1 + w <= width {
            used += 1 + w;
        } else {
            rows += 1;
            used = w;
        }
        while used > width {
            rows += 1;
            used -= width;
        }
    }
    rows
}

/// A bordered one-line text field. Long values scroll so the cursor stays
/// visible; `mask` keeps the first 7 characters and stars the rest (API
/// keys). Sets the frame cursor when `focused`.
pub fn draw_text_input(
    f: &mut ratatui::Frame,
    area: Rect,
    title: &str,
    input: &crate::app::TextInput,
    placeholder: &str,
    focused: bool,
    mask: bool,
) {
    use ratatui::style::Style;
    use ratatui::text::Span;
    use ratatui::widgets::{Block, Borders, Paragraph};

    let p = crate::theme::palette();
    let border = if focused { p.accent_primary } else { p.border };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border))
        .title(Span::styled(
            format!(" {title} "),
            Style::default().fg(if focused {
                p.accent_primary
            } else {
                p.text_muted
            }),
        ));
    let inner = block.inner(area);
    f.render_widget(block, area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    let chars: Vec<char> = if mask {
        input
            .value()
            .chars()
            .enumerate()
            .map(|(i, c)| if i < 7 { c } else { '*' })
            .collect()
    } else {
        input.value().chars().collect()
    };
    let width = inner.width as usize;
    let cursor = input.cursor_column();
    let start = (cursor + 1).saturating_sub(width);
    let (text, style) = if chars.is_empty() {
        (placeholder.to_string(), Style::default().fg(p.text_muted))
    } else {
        (
            chars[start.min(chars.len())..].iter().take(width).collect(),
            Style::default().fg(p.text_primary),
        )
    };
    f.render_widget(Paragraph::new(Span::styled(text, style)), inner);
    if focused {
        f.set_cursor(inner.x + (cursor - start) as u16, inner.y);
    }
}

/// A centred popup `height` rows tall (at most `area`'s), as wide as
/// [`popup_width`] gives for `percent_x`.
pub fn popup_area_fixed_height(area: Rect, percent_x: u16, height: u16) -> Rect {
    centered(area, popup_width(area.width, percent_x), height)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_shared_working_spinner_advances_every_100_ms() {
        assert_eq!(spinner_frame(0), spinner_frame(99));
        for frame in 0..10 {
            assert_ne!(spinner_frame(frame * 100), spinner_frame((frame + 1) * 100));
        }
        assert_eq!(spinner_frame(0), spinner_frame(1000));
    }

    #[test]
    fn dialog_fading_is_pronounced_and_stays_within_the_pane() {
        let p = theme::palette();
        let mut buffer = Buffer::empty(Rect::new(0, 0, 2, 1));
        buffer
            .get_mut(0, 0)
            .set_style(p.base().bg(p.selection).add_modifier(Modifier::BOLD));
        let outside = buffer.get(1, 0).clone();

        fade(&mut buffer, Rect::new(0, 0, 1, 1));

        let cell = buffer.get(0, 0);
        match p.faded(p.text_primary, FADE_TEXT) {
            Some(fg) => {
                assert_eq!(cell.fg, fg);
                assert!(!cell.modifier.contains(Modifier::DIM));
            }
            None => assert!(cell.modifier.contains(Modifier::DIM)),
        }
        assert_eq!(
            cell.bg,
            p.faded(p.selection, FADE_TEXT).unwrap_or(p.selection)
        );
        assert!(cell.modifier.contains(Modifier::BOLD));
        assert_eq!(buffer.get(1, 0), &outside);
    }

    #[test]
    fn preview_fading_is_lighter_than_dialog_fading_and_stays_within_the_pane() {
        assert_eq!(PREVIEW_FADE, 0.30);
        assert_eq!(FADE_TEXT, 0.45);
        let p = theme::palette();
        let mut preview = Buffer::empty(Rect::new(0, 0, 2, 1));
        preview
            .get_mut(0, 0)
            .set_style(p.base().bg(p.selection).add_modifier(Modifier::BOLD));
        let outside = preview.get(1, 0).clone();
        let mut dialog = preview.clone();

        fade_preview(&mut preview, Rect::new(0, 0, 1, 1));
        fade(&mut dialog, Rect::new(0, 0, 1, 1));

        let cell = preview.get(0, 0);
        match p.faded(p.text_primary, PREVIEW_FADE) {
            Some(fg) => {
                assert_eq!(cell.fg, fg);
                assert_ne!(cell.fg, dialog.get(0, 0).fg);
                assert!(!cell.modifier.contains(Modifier::DIM));
            }
            None => assert!(cell.modifier.contains(Modifier::DIM)),
        }
        assert_eq!(
            cell.bg,
            p.faded(p.selection, PREVIEW_FADE).unwrap_or(p.selection)
        );
        assert!(cell.modifier.contains(Modifier::BOLD));
        assert_eq!(preview.get(1, 0), &outside);
    }

    #[test]
    fn dialogs_keep_a_usable_width_on_narrow_screens() {
        // Wide screens: the percentage.
        assert_eq!(popup_width(160, 60), 96);
        assert_eq!(popup_width(80, 60), 48);
        // Narrow ones: 48 columns, or all but a column on each side.
        assert_eq!(popup_width(80, 50), 48);
        assert_eq!(popup_width(60, 60), 48);
        assert_eq!(popup_width(45, 60), 43);
        assert_eq!(popup_width(40, 60), 38);
        assert_eq!(popup_width(2, 60), 1);
        assert_eq!(popup_width(0, 60), 0);

        let popup = popup_area_fixed_height(Rect::new(0, 0, 45, 40), 60, 8);
        assert_eq!(popup, Rect::new(1, 16, 43, 8));
        let popup = popup_area_fixed_height(Rect::new(0, 0, 40, 5), 60, 8);
        assert_eq!(popup, Rect::new(1, 0, 38, 5));
        // A percentage height keeps 10 rows while they fit.
        let popup = popup_area(Rect::new(0, 0, 40, 20), 60, 40);
        assert_eq!(popup, Rect::new(1, 5, 38, 10));
        let popup = popup_area(Rect::new(0, 0, 160, 48), 60, 40);
        assert_eq!(popup, Rect::new(32, 14, 96, 19));
        let popup = popup_area(Rect::new(0, 0, 30, 6), 60, 40);
        assert_eq!(popup, Rect::new(1, 0, 28, 6));
    }

    #[test]
    fn wrapped_rows_counts_word_wrapped_lines() {
        assert_eq!(wrapped_rows("", 10), 1);
        assert_eq!(wrapped_rows("one two three", 20), 1);
        assert_eq!(wrapped_rows("one two three", 7), 2);
        assert_eq!(wrapped_rows("abcdefghij", 4), 3);
        assert_eq!(wrapped_rows("anything", 0), 1);
    }
}
