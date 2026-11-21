//! The workspaces modal (`m`, the footer's workspace button, Settings →
//! Workspace; `M` to move a session): No workspace, every workspace with
//! its sessions' count, the one shown (or the session's) marked `✓`, then
//! `+ Add new workspace`, which turns into the new name's field.

use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
    Frame,
};

use super::format::truncate;
use crate::app::workspaces::{WorkspacePicker, WorkspacePurpose, WorkspaceRow};
use crate::app::{App, Hit};
use crate::layout::str_width;
use crate::theme;

/// Widest the modal gets.
const MODAL_WIDTH: u16 = 56;
/// Most rows shown at once.
const MAX_ROWS: u16 = 14;

/// Draw the modal in the middle of `area` and record its click targets.
pub fn draw_workspaces(
    f: &mut Frame,
    app: &App,
    picker: &WorkspacePicker,
    area: Rect,
    hits: &mut Vec<(Rect, Hit)>,
) {
    let p = theme::palette();
    let rows = app.workspace_rows();
    let width = MODAL_WIDTH.min(area.width.saturating_sub(4));
    // Borders, padding, rows, error, blank, keys.
    let list_rows = (rows.len() as u16).min(MAX_ROWS);
    let height = (list_rows + 6).min(area.height);
    if width < 24 || height < 7 {
        return;
    }
    let popup = Rect {
        x: area.x + (area.width - width) / 2,
        y: area.y + (area.height - height) / 3,
        width,
        height,
    };
    let (title, marked) = match &picker.purpose {
        WorkspacePurpose::Switch => (" Workspaces ".to_owned(), app.list.workspace().cloned()),
        WorkspacePurpose::Move { session, name } => (
            format!(
                " Move {} to ",
                truncate(name, width.saturating_sub(12) as usize)
            ),
            app.list
                .get_anywhere(session)
                .and_then(|s| s.workspace.clone()),
        ),
    };
    f.render_widget(Clear, popup);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(p.accent_primary))
        .title(Span::styled(
            title,
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
    let key = Style::default()
        .fg(p.accent_primary)
        .add_modifier(Modifier::BOLD);

    let first = (picker.cursor + 1).saturating_sub(list_rows as usize);
    for (offset, (index, row)) in rows
        .iter()
        .enumerate()
        .skip(first)
        .take(list_rows as usize)
        .enumerate()
    {
        let at_cursor = index == picker.cursor;
        let rect = row_at(offset as u16);
        let bar = Span::styled(
            if at_cursor { "▎ " } else { "  " },
            Style::default().fg(p.accent_primary),
        );
        let mut name_style = Style::default().fg(if at_cursor {
            p.text_bright
        } else {
            p.neutral_gray
        });
        if at_cursor {
            name_style = name_style.add_modifier(Modifier::BOLD);
        }
        let line = match row {
            WorkspaceRow::Add => match (&picker.naming, at_cursor) {
                (Some(naming), true) => {
                    let room = inner_width.saturating_sub(4);
                    let chars: Vec<char> = naming.value().chars().collect();
                    let cursor = naming.cursor_column();
                    let start = (cursor + 1).saturating_sub(room);
                    let shown: String = chars[start.min(chars.len())..].iter().take(room).collect();
                    f.set_cursor(rect.x + 4 + (cursor - start) as u16, rect.y);
                    let text = if chars.is_empty() {
                        Span::styled("Name", muted)
                    } else {
                        Span::styled(shown, Style::default().fg(p.text_bright))
                    };
                    Line::from(vec![bar, Span::styled("❯ ", key), text])
                }
                _ => {
                    let label = match picker.purpose {
                        WorkspacePurpose::Switch => "+ Add new workspace",
                        WorkspacePurpose::Move { .. } => "+ New workspace",
                    };
                    Line::from(vec![
                        bar,
                        Span::styled(label, Style::default().fg(p.accent_primary)),
                    ])
                }
            },
            WorkspaceRow::None | WorkspaceRow::Workspace(_) => {
                let workspace = match row {
                    WorkspaceRow::Workspace(i) => app.workspaces.get(*i),
                    _ => None,
                };
                let id = workspace.map(|w| &w.id);
                let name = workspace.map_or("No workspace", |w| w.name.as_str());
                let deleting = id.is_some() && picker.deleting.as_ref() == id;
                let right = if deleting {
                    "d again deletes".to_owned()
                } else {
                    app.workspace_count(id).to_string()
                };
                let mark = if marked.as_ref() == id { "✓ " } else { "  " };
                let room = inner_width.saturating_sub(4 + str_width(&right) + 2);
                let name = truncate(name, room);
                let pad = room.saturating_sub(str_width(&name));
                let right_style = if deleting {
                    Style::default().fg(p.accent_pink)
                } else {
                    muted
                };
                Line::from(vec![
                    bar,
                    Span::styled(mark, Style::default().fg(p.accent_primary)),
                    Span::styled(name, name_style),
                    Span::raw(" ".repeat(pad + 2)),
                    Span::styled(right, right_style),
                ])
            }
        };
        f.render_widget(Paragraph::new(line), rect);
        hits.push((rect, Hit::WorkspaceRow(index)));
    }

    if let Some(error) = &picker.error {
        f.render_widget(
            Paragraph::new(Span::styled(
                truncate(&format!("  {error}"), inner_width),
                Style::default().fg(p.accent_pink),
            )),
            row_at(list_rows),
        );
    }

    let keys_row = row_at(inner.height.saturating_sub(1));
    let enter = match (&picker.naming, &picker.purpose) {
        (Some(_), _) => " add",
        (None, WorkspacePurpose::Switch) => " show",
        (None, WorkspacePurpose::Move { .. }) => " move",
    };
    let esc = if picker.naming.is_some() {
        " cancel"
    } else {
        " close"
    };
    let spans = vec![
        Span::styled("⏎", key),
        Span::styled(enter, muted),
        Span::raw("  "),
        Span::styled("Esc", key),
        Span::styled(esc, muted),
    ];
    f.render_widget(Paragraph::new(Line::from(spans)), keys_row);
    hits.push((popup, Hit::WorkspaceMenu));
}
