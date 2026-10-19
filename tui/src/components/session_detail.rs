//! Right pane: the selected session's terminal (running, or its saved
//! screen), with placeholders for no sessions, loading, the session
//! Rakumux runs in, errors and the daemon being unreachable. Live previews
//! fade lightly in Rakumux mode; dialogs and saved output use a stronger
//! fade. The cursor appears only while the session takes keys.

use ratatui::{
    layout::{Alignment, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{block::Title, Block, Paragraph, Wrap},
    Frame,
};
use session_core::api::Role;
use session_core::model::SessionStatus;

use super::format::{abbreviate_home, status_glyph, status_label, truncate};
use super::utils::{fade, fade_preview, get_spinner_char};
use crate::app::screen::Screen;
use crate::app::types::{AppMode, Focus};
use crate::app::{App, Connection, DetailView, Selection};
use crate::layout::str_width;
use crate::terminal_view::DefaultColors;
use crate::theme;

/// Draws the detail pane; returns the cursor position when a focused
/// session shows its cursor.
pub fn draw_session_detail(f: &mut Frame, app: &App, screen: &Screen) -> Option<(u16, u16)> {
    let p = theme::palette();
    // The session in view: the selected one, or the one kept in view while
    // the folder filter, a folder header or + New session is selected.
    let session = app.detail.session_id().and_then(|id| app.list.get(id));
    let focused = app.focus == Focus::Session && matches!(app.mode, AppMode::Normal);

    if let Some(detail) = screen.detail {
        f.render_widget(
            Block::default().style(Style::default().bg(p.background)),
            detail,
        );
    }
    if let Some(title_area) = screen.detail_title {
        let name_color = if focused {
            p.accent_primary
        } else {
            p.text_primary
        };
        let role = match app.detail.attachment.as_ref().map(|a| a.role) {
            Some(Role::Controller) => Some(("● controlling", p.accent_green)),
            Some(Role::Observer) => Some(("observing", p.text_muted)),
            None => None,
        };
        let title = match session {
            Some(session) => {
                let (glyph, glyph_style) = status_glyph(session);
                let mut spans = vec![
                    Span::raw(" "),
                    Span::styled(glyph, glyph_style),
                    Span::raw(" "),
                    Span::styled(
                        session.name.clone(),
                        Style::default().fg(name_color).add_modifier(Modifier::BOLD),
                    ),
                ];
                if let Some(status) = status_label(session) {
                    spans.push(Span::styled(
                        format!(" · {status}"),
                        Style::default().fg(p.text_muted),
                    ));
                }
                // The program's title in what's left beside the role (a
                // narrow pane keeps the role), when there's room for it.
                let used = Line::from(spans.clone()).width()
                    + role.map_or(0, |(label, _)| str_width(label) + 2);
                let room = usize::from(title_area.width).saturating_sub(used + 3);
                if let Some(title) = app.detail.title.as_deref().filter(|t| !t.is_empty()) {
                    if room >= 8 {
                        spans.push(Span::styled(
                            format!(" · {}", truncate(title, room.min(40))),
                            Style::default().fg(p.text_muted),
                        ));
                    }
                }
                Line::from(spans)
            }
            None => Line::from(Span::styled(" Rakumux", Style::default().fg(p.text_muted))),
        };
        let mut block = Block::default()
            .title(title)
            .style(Style::default().bg(p.panel));
        if let Some((label, color)) = role {
            block = block.title(
                Title::from(Span::styled(
                    format!("{label} "),
                    Style::default().fg(color),
                ))
                .alignment(Alignment::Right),
            );
        }
        f.render_widget(block, title_area);
        if !matches!(app.mode, AppMode::Normal)
            || matches!(app.detail.view, DetailView::Saved { .. })
        {
            fade(f.buffer_mut(), title_area);
        } else if !focused && matches!(app.detail.view, DetailView::Live(_)) {
            fade_preview(f.buffer_mut(), title_area);
        }
    }
    let area = screen.terminal?;
    draw_terminal(f, app, area, focused)
}

fn centered_text(f: &mut Frame, area: Rect, lines: Vec<Line>) {
    let height = lines.len() as u16;
    let top = area.y + area.height.saturating_sub(height) / 2;
    let rect = Rect {
        y: top,
        height: height.min(area.height),
        ..area
    };
    f.render_widget(
        Paragraph::new(lines)
            .alignment(ratatui::layout::Alignment::Center)
            .wrap(Wrap { trim: true }),
        rect,
    );
}

fn draw_terminal(f: &mut Frame, app: &App, area: Rect, focused: bool) -> Option<(u16, u16)> {
    let p = theme::palette();
    // Programs' default colours are the theme's, so the pane matches the
    // rest of the app; colours they set themselves are kept.
    let colors = DefaultColors {
        fg: p.text_primary,
        bg: p.background,
    };
    let muted = Style::default().fg(p.text_muted);
    let strong = Style::default()
        .fg(p.text_primary)
        .add_modifier(Modifier::BOLD);
    let key = Style::default()
        .fg(p.accent_primary)
        .add_modifier(Modifier::BOLD);

    if app.detail.terminal.is_none() {
        match &app.connection {
            Connection::Connecting => {
                centered_text(
                    f,
                    area,
                    vec![Line::from(Span::styled(
                        format!("{} Connecting to the Rakumux daemon…", get_spinner_char()),
                        strong,
                    ))],
                );
                return None;
            }
            Connection::Failed(error) => {
                centered_text(
                    f,
                    area,
                    vec![
                        Line::from(Span::styled(
                            "Can't reach the Rakumux daemon",
                            Style::default()
                                .fg(p.accent_pink)
                                .add_modifier(Modifier::BOLD),
                        )),
                        Line::from(""),
                        Line::from(Span::styled(error.to_string(), muted)),
                        Line::from(""),
                        Line::from(vec![
                            Span::styled("Ctrl-r", key),
                            Span::styled(" retry · ", muted),
                            Span::styled("q", key),
                            Span::styled(" quit · ", muted),
                            Span::styled("rakumux daemon logs", key),
                            Span::styled(" for details", muted),
                        ]),
                    ],
                );
                return None;
            }
            Connection::Connected(_) => {}
        }
    }

    match &app.detail.view {
        DetailView::Empty => {
            // Where `n` starts a session.
            let here = app.folders.name_of(&app.current_folder());
            let lines = if app.list.selection() == &Selection::Folders {
                let filter = app.list.folder_filter();
                let mut lines = vec![
                    Line::from(Span::styled(app.folder_label(), strong)),
                    Line::from(""),
                ];
                lines.extend(filter.iter().map(|folder| {
                    Line::from(vec![
                        Span::styled(app.folders.name_of(folder), key),
                        Span::styled(format!("  {}", abbreviate_home(folder.path())), muted),
                    ])
                }));
                if !filter.is_empty() {
                    lines.push(Line::from(""));
                }
                lines.push(Line::from(vec![
                    Span::styled("Press ", muted),
                    Span::styled("Enter", key),
                    Span::styled(" to filter by folder or add one.", muted),
                ]));
                lines
            } else if app.list.is_loaded() && app.list.filter.is_empty() {
                // Nothing in the folders shown.
                let title = match app.list.folder_filter() {
                    [] => "No sessions yet".to_string(),
                    [only] => format!("No sessions in {} yet", app.folders.name_of(only)),
                    many => format!("No sessions in these {} folders yet", many.len()),
                };
                vec![
                    Line::from(Span::styled(title, strong)),
                    Line::from(""),
                    Line::from(vec![
                        Span::styled("Press ", muted),
                        Span::styled("n", key),
                        Span::styled(
                            format!(" to start one in {here}: your shell, or any command."),
                            muted,
                        ),
                    ]),
                ]
            } else {
                vec![
                    Line::from(Span::styled("New session", strong)),
                    Line::from(""),
                    Line::from(vec![
                        Span::styled("Press ", muted),
                        Span::styled("Enter", key),
                        Span::styled(" or ", muted),
                        Span::styled("n", key),
                        Span::styled(format!(" to start a session in {here}."), muted),
                    ]),
                ]
            };
            centered_text(f, area, lines);
            None
        }
        DetailView::Loading(id) => {
            let name = app
                .list
                .get(id)
                .map(|s| s.name.clone())
                .unwrap_or_else(|| id.to_string());
            centered_text(
                f,
                area,
                vec![Line::from(Span::styled(
                    format!("{} Opening {name}…", get_spinner_char()),
                    muted,
                ))],
            );
            None
        }
        DetailView::Live(_) => {
            let terminal = app.detail.terminal.as_ref()?;
            let cursor = terminal.render(area, f.buffer_mut(), focused, colors);
            if !matches!(app.mode, AppMode::Normal) {
                fade(f.buffer_mut(), area);
            } else if !focused {
                fade_preview(f.buffer_mut(), area);
            }
            cursor
        }
        DetailView::Saved {
            status, exit_code, ..
        } => {
            match &app.detail.terminal {
                Some(terminal) => {
                    // A saved screen never takes keys.
                    terminal.render(area, f.buffer_mut(), false, colors);
                    fade(f.buffer_mut(), area);
                }
                None => centered_text(
                    f,
                    area,
                    vec![Line::from(Span::styled("No saved output.", muted))],
                ),
            }
            let note = match (status, exit_code) {
                (SessionStatus::Dead, _) => " Dead · Enter to revive ".to_string(),
                (_, Some(code)) => format!(" Exited with code {code} · Enter to restart "),
                _ => " Exited · Enter to restart ".to_string(),
            };
            let width = (note.chars().count() as u16).min(area.width);
            f.render_widget(
                Paragraph::new(Span::styled(
                    note,
                    Style::default()
                        .fg(p.background)
                        .bg(p.accent_gold)
                        .add_modifier(Modifier::BOLD),
                )),
                Rect {
                    x: area.right().saturating_sub(width),
                    y: area.bottom().saturating_sub(1),
                    width,
                    height: 1.min(area.height),
                },
            );
            None
        }
        DetailView::SelfSession(_) => {
            centered_text(
                f,
                area,
                vec![
                    Line::from(Span::styled(
                        "This is the session Rakumux is running in",
                        strong,
                    )),
                    Line::from(""),
                    Line::from(Span::styled(
                        "Showing it here would mirror the screen into itself.",
                        muted,
                    )),
                ],
            );
            None
        }
        DetailView::Error { message, .. } => {
            centered_text(
                f,
                area,
                vec![
                    Line::from(Span::styled(
                        message.clone(),
                        Style::default().fg(p.accent_pink),
                    )),
                    Line::from(""),
                    Line::from(vec![
                        Span::styled("Enter", key),
                        Span::styled(" to try again", muted),
                    ]),
                ],
            );
            None
        }
    }
}
