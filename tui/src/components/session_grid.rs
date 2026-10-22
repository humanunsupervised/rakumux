//! Full-body terminal cards. Rendering is side-effect free: no attach,
//! input, resize, fit, cursor, bell or clipboard operations happen here.

use ratatui::{
    layout::{Alignment, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{block::Title, Block, Borders, Paragraph, Wrap},
    Frame,
};

use super::format::{activity_badge, secondary_line, status_glyph};
use super::utils::{fade, fade_preview};
use crate::app::grid::PreviewState;
use crate::app::types::AppMode;
use crate::app::{App, Connection, Hit, Selection};
use crate::layout::{str_width, truncate, PIN_MARK};
use crate::terminal_view::DefaultColors;
use crate::theme;

/// Draw the visible page and record card/header hit targets.
pub fn draw_session_grid(f: &mut Frame, app: &App, area: Rect, hits: &mut Vec<(Rect, Hit)>) {
    let p = theme::palette();
    let page = app.grid_page();
    let visible = app.list.visible();
    let header = Rect {
        height: area.height.min(1),
        ..area
    };
    let paging = format!("‹ {}/{} ›", page.start / page.capacity + 1, page.pages);
    let page_width = (str_width(&paging) as u16).min(header.width);
    let left = Rect {
        width: header.width.saturating_sub(page_width),
        ..header
    };
    let label = if app.list.filter_editing || !app.list.filter.is_empty() {
        format!(" {} · /{}", app.filter_label(), app.list.filter.value())
    } else {
        format!(" {} · Grid · n new · v layout", app.filter_label())
    };
    f.render_widget(
        Paragraph::new(label).style(Style::default().fg(p.text_primary).bg(p.panel)),
        left,
    );
    hits.push((left, Hit::FolderSelector));
    let right = Rect {
        x: left.right(),
        width: page_width,
        ..header
    };
    f.render_widget(
        Paragraph::new(paging).style(Style::default().fg(p.accent_primary).bg(p.panel)),
        right,
    );
    let half = page_width / 2;
    hits.push((
        Rect {
            width: half,
            ..right
        },
        Hit::GridPage(-1),
    ));
    hits.push((
        Rect {
            x: right.x + half,
            width: page_width - half,
            ..right
        },
        Hit::GridPage(1),
    ));

    for (index, card_area) in page.cards {
        hits.push((card_area, Hit::GridCell(index)));
        let Some(session) = visible.get(index) else {
            let selected = app.list.selection() == &Selection::NewSession;
            let block = card_block(selected).title(" + New session ");
            let inner = block.inner(card_area);
            f.render_widget(block, card_area);
            let message = if !app.connection.is_connected() {
                match app.connection {
                    Connection::Connecting => "Connecting to the daemon…",
                    _ => "Daemon unavailable · Ctrl-r retry",
                }
            } else if visible.is_empty() && !app.list.filter.is_empty() {
                "No matching sessions · Esc clears the filter\nEnter or n starts a session"
            } else if visible.is_empty() {
                "No sessions yet\nEnter or n starts a session"
            } else {
                "Enter or n starts a session"
            };
            f.render_widget(
                Paragraph::new(message)
                    .wrap(Wrap { trim: true })
                    .style(Style::default().fg(p.text_muted)),
                inner,
            );
            continue;
        };
        let selected = app.list.selected_id() == Some(&session.id);
        let (glyph, glyph_style) = status_glyph(session);
        let badge = activity_badge(session);
        let name = truncate(
            &session.name,
            usize::from(
                card_area
                    .width
                    .saturating_sub(if badge.is_some() { 12 } else { 9 }),
            ),
        );
        let mut title = vec![
            Span::raw(" "),
            Span::styled(if badge.is_some() { " " } else { glyph }, glyph_style),
            Span::raw(" "),
            Span::styled(
                name,
                Style::default()
                    .fg(if selected {
                        p.text_bright
                    } else {
                        p.text_primary
                    })
                    .add_modifier(Modifier::BOLD),
            ),
        ];
        if session.pinned {
            title.push(Span::styled(
                format!(" {PIN_MARK}"),
                Style::default().fg(p.accent_gold),
            ));
        }
        title.push(Span::raw(" "));
        let preview = app.grid.get(&session.id);
        let state = if app.is_self_session(&session.id) {
            "this session"
        } else {
            match preview.map(|c| &c.state) {
                Some(PreviewState::Live) => "observing · read-only",
                Some(PreviewState::Saved) => "saved · Enter to revive",
                Some(PreviewState::Error(_)) => "preview unavailable",
                _ if !app.connection.is_connected() => "disconnected",
                _ => "loading…",
            }
        };
        let mut block = card_block(selected).title(Line::from(title)).title(
            Title::from(format!(" {state} ")).position(ratatui::widgets::block::Position::Bottom),
        );
        if let Some((mark, style)) = badge {
            block = block.title(
                Title::from(Line::from(vec![
                    Span::raw(" "),
                    Span::styled(mark, style),
                    Span::raw(" "),
                ]))
                .alignment(Alignment::Right),
            );
        }
        let inner = block.inner(card_area);
        f.render_widget(block, card_area);
        if inner.is_empty() {
            continue;
        }
        // Folder context is retained even though Grid has no group headers.
        let mut metadata = vec![Span::styled(
            format!("{} · ", app.folders.name_of(&session.scope)),
            Style::default().fg(p.text_muted),
        )];
        metadata.extend(
            secondary_line(
                session,
                usize::from(inner.width),
                app.icons(),
                Style::default().fg(p.text_muted),
            )
            .spans,
        );
        f.render_widget(
            Paragraph::new(Line::from(metadata)),
            Rect { height: 1, ..inner },
        );
        let output = Rect {
            y: inner.y + 1,
            height: inner.height.saturating_sub(1),
            ..inner
        };
        if output.is_empty() {
            continue;
        }
        if let Some(terminal) = preview.and_then(|c| c.terminal.as_ref()) {
            terminal.render(output, f.buffer_mut(), false, DefaultColors::NATIVE);
        } else {
            let message = if app.is_self_session(&session.id) {
                "This session cannot be nested inside itself."
            } else if !app.connection.is_connected() {
                "Daemon unavailable · Ctrl-r retry"
            } else {
                match preview.map(|c| &c.state) {
                    Some(PreviewState::Error(message)) => message.as_str(),
                    Some(PreviewState::Saved) => "No saved output · Enter to revive",
                    _ => "Loading terminal…",
                }
            };
            f.render_widget(
                Paragraph::new(message)
                    .wrap(Wrap { trim: true })
                    .style(Style::default().fg(p.text_muted)),
                output,
            );
        }
        if !matches!(app.mode, AppMode::Normal)
            || preview.is_some_and(|c| c.state == PreviewState::Saved)
        {
            fade(f.buffer_mut(), output);
        } else {
            fade_preview(f.buffer_mut(), output);
        }
    }
}

fn card_block(selected: bool) -> Block<'static> {
    let p = theme::palette();
    Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(if selected {
            p.accent_primary
        } else {
            p.border_dim
        }))
        .style(p.base())
}
