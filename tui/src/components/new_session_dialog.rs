//! New session dialog: the name, and under it the folder the session goes
//! in (not editable: the current folder).

use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
    Frame,
};

use super::format::{abbreviate_home, truncate, truncate_middle};
use super::utils::{draw_text_input, get_spinner_char, popup_area_fixed_height};
use crate::app::types::AppMode;
use crate::app::{App, NewSessionDialog};
use crate::layout::str_width;
use crate::theme;

pub fn draw_new_session_dialog(f: &mut Frame, app: &App, area: Rect) {
    let AppMode::NewSession(dialog) = &app.mode else {
        return;
    };
    let p = theme::palette();
    let popup = popup_area_fixed_height(area, 60, 8);
    f.render_widget(Clear, popup);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(p.accent_primary))
        .title(Span::styled(
            " New session ",
            Style::default()
                .fg(p.accent_primary)
                .add_modifier(Modifier::BOLD),
        ))
        .style(p.base());
    let inner = block.inner(popup);
    f.render_widget(block, popup);

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // name
            Constraint::Length(1), // folder
            Constraint::Length(1), // error / progress
            Constraint::Min(0),
            Constraint::Length(1), // keys
        ])
        .split(inner);

    let width = inner.width as usize;
    draw_text_input(
        f,
        rows[0],
        "Name",
        &dialog.name,
        "blank: a name is picked for you",
        !dialog.busy,
        false,
    );
    f.render_widget(Paragraph::new(folder_line(app, dialog, width)), rows[1]);
    let message = if dialog.busy {
        Some(Span::styled(
            format!(" {} Starting session…", get_spinner_char()),
            Style::default().fg(p.accent_primary),
        ))
    } else {
        dialog.error.as_ref().map(|error| {
            Span::styled(
                truncate(&format!(" ✕ {error}"), width),
                Style::default().fg(p.accent_pink),
            )
        })
    };
    if let Some(message) = message {
        f.render_widget(Paragraph::new(message), rows[2]);
    }
    let key = Style::default()
        .fg(p.accent_primary)
        .add_modifier(Modifier::BOLD);
    let muted = Style::default().fg(p.text_muted);
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(" Enter", key),
            Span::styled(" create  ", muted),
            Span::styled("Esc", key),
            Span::styled(" cancel", muted),
        ])),
        rows[4],
    );
}

/// ` in ▤ rakumux   ~/repos/rakumux`: where the session goes
/// and starts. The folder is left out when there's no room for it.
fn folder_line(app: &App, dialog: &NewSessionDialog, width: usize) -> Line<'static> {
    let p = theme::palette();
    let muted = Style::default().fg(p.text_muted);
    let prefix = " in ";
    let icon = format!("{} ", app.icons().folder());
    let room = width.saturating_sub(str_width(prefix) + str_width(&icon));
    let name = truncate(&app.folders.name_of(&dialog.folder), room);
    let mut spans = vec![
        Span::styled(prefix, muted),
        Span::styled(icon, Style::default().fg(p.neutral_gray)),
        Span::styled(name.clone(), Style::default().fg(p.neutral_gray)),
    ];
    let room = room.saturating_sub(str_width(&name) + 3);
    if room >= 8 {
        spans.push(Span::styled(
            format!(
                "   {}",
                truncate_middle(&abbreviate_home(dialog.folder.path()), room)
            ),
            muted,
        ));
    }
    Line::from(spans)
}
