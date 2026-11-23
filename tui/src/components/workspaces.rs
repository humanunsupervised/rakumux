//! The workspaces modal (`m`, the footer's `⊞`, Settings → Workspace; `M` to
//! move a session): All (not when moving), every workspace, No workspace, each
//! with its sessions' count, the one shown (or the session's) marked `✓`,
//! then `+ Add new workspace`, which turns into the new name's field; a
//! workspace being renamed turns into one in its place.

use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
    Frame,
};

use super::format::truncate;
use crate::app::workspaces::{WorkspacePicker, WorkspacePurpose, WorkspaceRow, NO_WORKSPACE};
use crate::app::{App, Hit, WorkspaceView};
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
        WorkspacePurpose::Switch => (" Workspaces ".to_owned(), app.list.workspace_view().clone()),
        WorkspacePurpose::Move { session, name } => (
            format!(
                " Move {} to ",
                truncate(name, width.saturating_sub(12) as usize)
            ),
            match app
                .list
                .get_anywhere(session)
                .and_then(|s| s.workspace.clone())
            {
                Some(id) => WorkspaceView::In(id),
                None => WorkspaceView::Unassigned,
            },
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
        // The name field, on the row it adds or renames.
        let row_id = match row {
            WorkspaceRow::Workspace(i) => app.workspaces.get(*i).map(|w| &w.id),
            _ => None,
        };
        let editing = match (&picker.naming, row) {
            (Some(_), WorkspaceRow::Add) => at_cursor && picker.renaming.is_none(),
            (Some(_), WorkspaceRow::Workspace(_)) => {
                row_id.is_some() && picker.renaming.as_ref() == row_id
            }
            _ => false,
        };
        if let (true, Some(naming)) = (editing, &picker.naming) {
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
            let line = Line::from(vec![bar, Span::styled("❯ ", key), text]);
            f.render_widget(Paragraph::new(line), rect);
            hits.push((rect, Hit::WorkspaceRow(index)));
            continue;
        }
        let line = match row {
            WorkspaceRow::Add => {
                let label = match picker.purpose {
                    WorkspacePurpose::Switch => "+ Add new workspace",
                    WorkspacePurpose::Move { .. } => "+ New workspace",
                };
                Line::from(vec![
                    bar,
                    Span::styled(label, Style::default().fg(p.accent_primary)),
                ])
            }
            WorkspaceRow::All | WorkspaceRow::Unassigned | WorkspaceRow::Workspace(_) => {
                let (view, name) = match row {
                    WorkspaceRow::Workspace(i) => match app.workspaces.get(*i) {
                        Some(w) => (WorkspaceView::In(w.id.clone()), w.name.as_str()),
                        None => continue,
                    },
                    WorkspaceRow::All => (WorkspaceView::All, "All"),
                    _ => (WorkspaceView::Unassigned, NO_WORKSPACE),
                };
                let deleting = row_id.is_some() && picker.deleting.as_ref() == row_id;
                let right = if deleting {
                    "d again deletes".to_owned()
                } else {
                    app.view_count(&view).to_string()
                };
                let mark = if marked == view { "✓ " } else { "  " };
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
    let enter = match (&picker.naming, &picker.renaming, &picker.purpose) {
        (Some(_), Some(_), _) => " rename",
        (Some(_), None, _) => " add",
        (None, _, WorkspacePurpose::Switch) => " show",
        (None, _, WorkspacePurpose::Move { .. }) => " move",
    };
    let esc = if picker.naming.is_some() {
        " cancel"
    } else {
        " close"
    };
    // On a workspace: rename and delete it, each a tap target too.
    let on_workspace = picker.naming.is_none()
        && matches!(
            app.workspace_rows().get(picker.cursor),
            Some(WorkspaceRow::Workspace(_))
        );
    let mut spans: Vec<(Span, Option<Hit>)> = vec![
        (Span::styled("⏎", key), None),
        (Span::styled(enter, muted), None),
        (Span::raw("  "), None),
    ];
    if on_workspace {
        spans.extend([
            (Span::styled("e", key), Some(Hit::WorkspaceRename)),
            (Span::styled(" rename", muted), Some(Hit::WorkspaceRename)),
            (Span::raw("  "), None),
            (Span::styled("d", key), Some(Hit::WorkspaceDelete)),
            (Span::styled(" delete", muted), Some(Hit::WorkspaceDelete)),
            (Span::raw("  "), None),
        ]);
    }
    spans.extend([
        (Span::styled("Esc", key), None),
        (Span::styled(esc, muted), None),
    ]);
    let mut x = keys_row.x;
    for (span, hit) in &spans {
        let width = str_width(&span.content) as u16;
        if let Some(hit) = hit {
            if x + width <= keys_row.right() {
                hits.push((
                    Rect {
                        x,
                        width,
                        ..keys_row
                    },
                    *hit,
                ));
            }
        }
        x += width;
    }
    let line = Line::from(spans.into_iter().map(|(span, _)| span).collect::<Vec<_>>());
    f.render_widget(Paragraph::new(line), keys_row);
    hits.push((popup, Hit::WorkspaceMenu));
}
