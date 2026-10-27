//! Right pane: the selected session's terminal (running, or its saved
//! screen), with placeholders for no sessions, loading, the session
//! Rakumux runs in, errors and the daemon being unreachable; in the Feed
//! view in Rakumux mode, the selected reply (`feed.rs`). There's no
//! title row: the footer names the session and its status. Live previews
//! recede in Rakumux mode; dialogs and saved output use a stronger fade.
//! The cursor appears only while the session takes keys.

use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Paragraph, Wrap},
    Frame,
};
use session_core::model::SessionStatus;

use super::format::abbreviate_home;
use super::utils::{fade, fade_preview, get_spinner_char};
use crate::app::grouping::Grouping;
use crate::app::screen::Screen;
use crate::app::types::{AppMode, Focus};
use crate::app::{App, Connection, DetailView, Selection};
use crate::terminal_view::DefaultColors;
use crate::theme;

/// Draws the detail pane; returns the cursor position when a focused
/// session shows its cursor.
pub fn draw_session_detail(
    f: &mut Frame,
    app: &App,
    screen: &Screen,
    hits: &mut Vec<(Rect, crate::app::Hit)>,
) -> Option<(u16, u16)> {
    let p = theme::palette();
    let focused = app.focus == Focus::Session && matches!(app.mode, AppMode::Normal);

    if let Some(detail) = screen.detail {
        f.render_widget(
            Block::default().style(Style::default().bg(p.background)),
            detail,
        );
    }
    let area = screen.terminal?;
    if app.feed_in_detail() {
        super::feed::draw_feed_reply(f, app, area, hits);
        if !matches!(app.mode, AppMode::Normal) {
            fade(f.buffer_mut(), area);
        }
        return None;
    }
    draw_terminal(f, app, area, focused)
}

/// Apply one consistent inactive treatment to the terminal.
/// Each freshly rendered area is faded once, never layered on a prior fade.
fn fade_inactive_detail(f: &mut Frame, app: &App, area: Rect, focused: bool) {
    if !matches!(app.mode, AppMode::Normal) || matches!(app.detail.view, DetailView::Saved { .. }) {
        fade(f.buffer_mut(), area);
    } else if !focused && matches!(app.detail.view, DetailView::Live(_)) {
        fade_preview(f.buffer_mut(), area);
    }
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
    // Programs keep the outer terminal's colours, not the theme's.
    let colors = DefaultColors::NATIVE;
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
            let grouping = app.list.grouping();
            let lines = if app.list.selection() == &Selection::Folders {
                let mut lines = vec![
                    Line::from(Span::styled(app.filter_label(), strong)),
                    Line::from(""),
                ];
                let filtering = match grouping {
                    Grouping::Folder => {
                        let filter = app.list.folder_filter();
                        lines.extend(filter.iter().map(|folder| {
                            Line::from(vec![
                                Span::styled(app.folders.name_of(folder), key),
                                Span::styled(
                                    format!("  {}", abbreviate_home(folder.path())),
                                    muted,
                                ),
                            ])
                        }));
                        !filter.is_empty()
                    }
                    Grouping::App | Grouping::Status => false,
                };
                if filtering {
                    lines.push(Line::from(""));
                }
                let what = match grouping {
                    Grouping::Folder => " to filter by folder or add one.",
                    Grouping::App => " to filter by app.",
                    Grouping::Status => " to filter by status.",
                };
                lines.push(Line::from(vec![
                    Span::styled("Press ", muted),
                    Span::styled("Enter", key),
                    Span::styled(what, muted),
                ]));
                lines.push(Line::from(vec![
                    Span::styled("← →", key),
                    Span::styled(" switch between Status, Folders and App.", muted),
                ]));
                lines
            } else if app.list.is_loaded() && app.list.filter.is_empty() {
                // Nothing in the folders (apps, statuses) shown.
                let title = match (grouping, app.list.folder_filter()) {
                    (Grouping::App | Grouping::Status, _)
                        if !app.list.section_filter(grouping).is_empty() =>
                    {
                        format!("No sessions in {}", app.filter_label())
                    }
                    (Grouping::Folder, [only]) => {
                        format!("No sessions in {} yet", app.folders.name_of(only))
                    }
                    (Grouping::Folder, many) if many.len() > 1 => {
                        format!("No sessions in these {} folders yet", many.len())
                    }
                    _ => "No sessions yet".to_string(),
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
            fade_inactive_detail(f, app, area, focused);
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
