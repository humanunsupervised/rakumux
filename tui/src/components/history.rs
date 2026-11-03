//! The session history modal (Settings → Session history, `H`): a search
//! line, then the deleted sessions, most recently deleted first, each with
//! its folder, agent and how long ago it was deleted.

use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
    Frame,
};
use session_core::model::DeletedSession;

use super::format::{now_secs, program_color, relative_time, truncate};
use crate::app::types::HistoryPicker;
use crate::app::{App, Hit};
use crate::layout::str_width;
use crate::theme;

/// Widest the modal gets.
const MODAL_WIDTH: u16 = 72;
/// Most rows shown at once.
const MAX_ROWS: u16 = 14;
/// Widest a folder or an agent column gets.
const MAX_COLUMN: usize = 20;
/// The name keeps at least this much before other columns go.
const MIN_NAME: usize = 12;

/// The columns a row shows: the name's width, then the folder's and the
/// agent's (0: left out), then the age's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Columns {
    name: usize,
    folder: usize,
    agent: usize,
    age: usize,
}

/// Fit the columns into `width` (after the cursor bar): the age always,
/// the agent and then the folder go when the name would get too narrow.
fn columns(width: usize, names: usize, folders: usize, agents: usize, age: usize) -> Columns {
    let room = width.saturating_sub(age + 1);
    let folder = folders.min(MAX_COLUMN);
    let agent = agents.min(MAX_COLUMN);
    let fits = |extra: usize| room.saturating_sub(extra) >= MIN_NAME.min(names);
    let (folder, agent) = if agent > 0 && fits(folder + agent + 4) {
        (folder, agent)
    } else if folder > 0 && fits(folder + 2) {
        (folder, 0)
    } else {
        (0, 0)
    };
    let used = |w: usize| if w > 0 { w + 2 } else { 0 };
    let name = room.saturating_sub(used(folder) + used(agent));
    Columns {
        name,
        folder,
        agent,
        age,
    }
}

/// Draw the modal in the middle of `area` and record its click targets.
pub fn draw_history(
    f: &mut Frame,
    app: &App,
    picker: &HistoryPicker,
    area: Rect,
    hits: &mut Vec<(Rect, Hit)>,
) {
    let p = theme::palette();
    let width = MODAL_WIDTH.min(area.width.saturating_sub(4));
    // Borders, padding, query, rule, rows (at least one), blank, keys.
    let list_rows = (picker.rows.len() as u16).clamp(1, MAX_ROWS);
    let height = (list_rows + 7).min(area.height);
    if width < 24 || height < 8 {
        return;
    }
    let popup = Rect {
        x: area.x + (area.width - width) / 2,
        y: area.y + (area.height - height) / 3,
        width,
        height,
    };
    f.render_widget(Clear, popup);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(p.accent_primary))
        .title(Span::styled(
            " Session history ",
            Style::default()
                .fg(p.accent_primary)
                .add_modifier(Modifier::BOLD),
        ))
        .style(p.base());
    let inner = block.inner(popup);
    f.render_widget(block, popup);
    let inner = Rect {
        x: inner.x + 1,
        y: inner.y + 1,
        width: inner.width.saturating_sub(2),
        height: inner.height.saturating_sub(1),
    };
    let row_at = |offset: u16| Rect {
        y: inner.y + offset,
        height: 1,
        ..inner
    };
    let inner_width = inner.width as usize;
    let muted = Style::default().fg(p.text_muted);

    // The query.
    let prompt = "❯ ";
    let room = inner_width.saturating_sub(prompt.chars().count() + 1);
    let chars: Vec<char> = picker.query.value().chars().collect();
    let cursor = picker.query.cursor_column();
    let start = (cursor + 1).saturating_sub(room);
    let query = if chars.is_empty() {
        Span::styled(truncate("Search", room), muted)
    } else {
        Span::styled(
            chars[start.min(chars.len())..]
                .iter()
                .take(room)
                .collect::<String>(),
            Style::default().fg(p.text_bright),
        )
    };
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(prompt, Style::default().fg(p.accent_primary)),
            query,
        ])),
        row_at(0),
    );
    f.set_cursor(
        inner.x + (prompt.chars().count() + cursor - start) as u16,
        inner.y,
    );
    f.render_widget(
        Paragraph::new(Span::styled(
            "─".repeat(inner_width),
            Style::default().fg(p.border_dim),
        )),
        row_at(1),
    );

    let empty = if picker.loading {
        Some("Loading…".to_string())
    } else if let Some(error) = &picker.error {
        Some(error.clone())
    } else if picker.sessions.is_empty() {
        Some("No deleted sessions".into())
    } else if picker.rows.is_empty() {
        Some("No session matches".into())
    } else {
        None
    };
    if let Some(line) = empty {
        f.render_widget(
            Paragraph::new(Span::styled(
                truncate(&format!("  {line}"), inner_width),
                muted,
            )),
            row_at(2),
        );
    }

    // The rows, scrolled so the cursor shows.
    let now = now_secs();
    let shown: Vec<(usize, &DeletedSession)> = picker
        .rows
        .iter()
        .enumerate()
        .filter_map(|(row, index)| picker.sessions.get(*index).map(|s| (row, s)))
        .collect();
    let folder_of = |session: &DeletedSession| app.folders.name_of(&session.scope);
    let agent_of = |session: &DeletedSession| {
        session
            .agent
            .as_ref()
            .map(|agent| agent.kind.clone())
            .unwrap_or_default()
    };
    let widest = |f: &dyn Fn(&DeletedSession) -> String| {
        shown
            .iter()
            .map(|(_, s)| str_width(&f(s)))
            .max()
            .unwrap_or(0)
    };
    let lead = 2;
    let cols = columns(
        inner_width.saturating_sub(lead),
        widest(&|s| s.name.clone()),
        widest(&folder_of),
        widest(&agent_of),
        widest(&|s| relative_time(now, s.deleted_at)),
    );
    let list_height = list_rows as usize;
    let first = (picker.cursor + 1).saturating_sub(list_height);
    for (offset, (row, session)) in shown.iter().skip(first).take(list_height).enumerate() {
        let at_cursor = *row == picker.cursor;
        let rect = row_at(2 + offset as u16);
        let mut name_style = Style::default().fg(if at_cursor {
            p.text_bright
        } else {
            p.neutral_gray
        });
        if at_cursor {
            name_style = name_style.add_modifier(Modifier::BOLD);
        }
        let cell = |text: &str, width: usize| {
            let text = truncate(text, width);
            let pad = width.saturating_sub(str_width(&text));
            format!("{text}{}", " ".repeat(pad))
        };
        let mut spans = vec![
            Span::styled(
                if at_cursor { "▎ " } else { "  " },
                Style::default().fg(p.accent_primary),
            ),
            Span::styled(cell(&session.name, cols.name), name_style),
        ];
        if cols.folder > 0 {
            spans.push(Span::raw("  "));
            spans.push(Span::styled(cell(&folder_of(session), cols.folder), muted));
        }
        if cols.agent > 0 {
            let agent = agent_of(session);
            spans.push(Span::raw("  "));
            spans.push(Span::styled(
                cell(&agent, cols.agent),
                Style::default().fg(program_color(&agent)),
            ));
        }
        let age = relative_time(now, session.deleted_at);
        spans.push(Span::raw(" "));
        spans.push(Span::styled(
            format!("{age:>width$}", width = cols.age),
            muted,
        ));
        f.render_widget(Paragraph::new(Line::from(spans)), rect);
        hits.push((rect, Hit::HistoryRow(*row)));
    }

    // The keys: Enter, then Esc (clearing what's typed first).
    let keys_row = row_at(inner.height.saturating_sub(1));
    let key = Style::default()
        .fg(p.accent_primary)
        .add_modifier(Modifier::BOLD);
    let esc = if picker.query.is_empty() {
        " close"
    } else {
        " clear"
    };
    let mut spans = Vec::new();
    if !picker.rows.is_empty() {
        spans.extend([
            Span::styled("⏎", key),
            Span::styled(" restore", muted),
            Span::raw("  "),
        ]);
    }
    spans.extend([Span::styled("Esc", key), Span::styled(esc, muted)]);
    f.render_widget(Paragraph::new(Line::from(spans)), keys_row);
    hits.push((popup, Hit::HistoryMenu));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn narrow_rows_drop_the_agent_then_the_folder() {
        let wide = columns(66, 20, 10, 6, 3);
        assert_eq!((wide.folder, wide.agent), (10, 6));
        assert_eq!(wide.name, 66 - 4 - 12 - 8);
        let phone = columns(34, 20, 10, 6, 3);
        assert_eq!((phone.folder, phone.agent), (10, 0));
        let tiny = columns(20, 20, 10, 6, 3);
        assert_eq!((tiny.folder, tiny.agent), (0, 0));
        assert_eq!(tiny.name, 16);
    }
}
