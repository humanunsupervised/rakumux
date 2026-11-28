//! The workspaces menu (a click on the left column's Workspaces tab, `w`, the
//! Tabs layout's `⊞`, Settings → Workspace; `W` to move a session), a popover
//! over that tab where it's on screen, else a modal in the middle: All (not when moving), every workspace, No workspace, each
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
use crate::app::workspaces::{
    WorkspacePicker, WorkspacePurpose, WorkspaceRow, ALL_WORKSPACES, NO_WORKSPACE,
};
use crate::app::{App, Hit, WorkspaceView};
use crate::layout::{left_split, str_width};
use crate::theme;

/// Widest the modal gets.
const MODAL_WIDTH: u16 = 56;
/// Most rows shown at once.
const MAX_ROWS: u16 = 14;

/// Narrowest the popover gets.
const POPOVER_MIN: u16 = 30;
/// Widest the popover gets.
const POPOVER_MAX: u16 = 44;

/// Where the popover opens: just over the left column's tabs, when the
/// column is on screen and the menu switches workspaces; `rows` of it, `names`
/// columns its widest row.
fn popover(app: &App, picker: &WorkspacePicker, rows: usize, names: usize) -> Option<(Rect, u16)> {
    if picker.purpose != WorkspacePurpose::Switch {
        return None;
    }
    let pane = app.screen().list?;
    let (body, tabs) = left_split(pane);
    // Borders and the row under the menu's.
    let frame = 3;
    let shown = (rows as u16).min(body.height.saturating_sub(frame));
    // The columns a row takes besides its name and its count's: its bar and
    // mark, a gap and the borders with a column either side.
    let wanted = (names + 10) as u16;
    let width = wanted
        .clamp(POPOVER_MIN, POPOVER_MAX)
        .min(pane.width.saturating_sub(2));
    if shown < 3 || width < 20 {
        return None;
    }
    let height = shown + frame;
    Some((
        Rect {
            x: pane.x + 1,
            y: tabs.y - height,
            width,
            height,
        },
        shown,
    ))
}

/// Draw the menu (over its tab, or in the middle of `area`) and record its
/// click targets.
pub fn draw_workspaces(
    f: &mut Frame,
    app: &App,
    picker: &WorkspacePicker,
    area: Rect,
    hits: &mut Vec<(Rect, Hit)>,
) {
    let p = theme::palette();
    let rows = app.workspace_rows();
    let widest = app
        .workspace_views()
        .iter()
        .map(|view| {
            let count = app.view_count(view).to_string();
            str_width(&app.view_name(view)) + str_width(&count)
        })
        .max()
        .unwrap_or(0);
    let over = popover(app, picker, rows.len(), widest);
    let (popup, list_rows) = match over {
        Some(over) => over,
        None => {
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
            (popup, list_rows)
        }
    };
    let (title, marked) = match &picker.purpose {
        // The tab under it names it.
        WorkspacePurpose::Switch if over.is_some() => {
            (String::new(), app.list.workspace_view().clone())
        }
        WorkspacePurpose::Switch => (" Workspaces ".to_owned(), app.list.workspace_view().clone()),
        WorkspacePurpose::Move { session, name } => (
            format!(
                " Move {} to ",
                truncate(name, popup.width.saturating_sub(12) as usize)
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
    let top = u16::from(over.is_none());
    let inner = Rect {
        x: inner.x + 1,
        y: inner.y + top,
        width: inner.width.saturating_sub(2),
        height: inner.height.saturating_sub(top),
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
                    WorkspaceRow::All => (WorkspaceView::All, ALL_WORKSPACES),
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
    // A popover is a menu: only what isn't plain of one.
    if over.is_some() && (picker.error.is_some() || !on_workspace) {
        hits.push((popup, Hit::WorkspaceMenu));
        return;
    }
    let mut spans: Vec<(Span, Option<Hit>)> = Vec::new();
    if over.is_some() {
        spans.push((Span::raw("  "), None));
    } else {
        spans.extend([
            (Span::styled("⏎", key), None),
            (Span::styled(enter, muted), None),
            (Span::raw("  "), None),
        ]);
    }
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
    if over.is_none() {
        spans.extend([
            (Span::styled("Esc", key), None),
            (Span::styled(esc, muted), None),
        ]);
    }
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
