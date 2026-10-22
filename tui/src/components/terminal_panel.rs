//! The utility shell's title, focus cue and passive terminal mirror.
use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Paragraph, Wrap},
    Frame,
};
use session_core::api::Role;

use super::format::{abbreviate_home, truncate};
use super::utils::{fade, fade_preview, get_spinner_char};
use crate::app::terminal_panel::panel_output;
use crate::app::types::{AppMode, Focus};
use crate::app::{App, Hit};
use crate::terminal_view::DefaultColors;
use crate::theme;

/// Returns a cursor only while this shell, not the agent, takes keys.
pub fn draw_terminal_panel(
    frame: &mut Frame,
    app: &App,
    hits: &mut Vec<(Rect, Hit)>,
) -> Option<(u16, u16)> {
    let screen = app.screen();
    let area = screen.terminal_panel?;
    let palette = theme::palette();
    let focused = app.focus == Focus::Terminal
        && matches!(app.mode, AppMode::Normal)
        && app
            .terminal_panel
            .attachment
            .as_ref()
            .is_some_and(|attachment| attachment.role == Role::Controller);
    if let Some(divider) = screen.terminal_panel_divider {
        frame.render_widget(
            Paragraph::new("─".repeat(usize::from(divider.width))).style(Style::default().fg(
                if app.terminal_panel.dragging {
                    palette.accent_primary
                } else {
                    palette.border_dim
                },
            )),
            divider,
        );
        hits.push((divider, Hit::TerminalPanelDivider));
    }
    let header = Rect { height: 1, ..area };
    let hide_width = area.width.min(8);
    let hide = Rect {
        x: area.right().saturating_sub(hide_width),
        width: hide_width,
        ..header
    };
    let folder = app
        .terminal_panel
        .cwd
        .as_ref()
        .map(|cwd| abbreviate_home(cwd))
        .unwrap_or_default();
    let status = if app.terminal_panel.busy() {
        format!("{} starting…", get_spinner_char())
    } else if focused {
        "typing here".into()
    } else {
        "click to focus".into()
    };
    let title = format!(" Terminal · {folder} · {status}");
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            truncate(&title, usize::from(area.width.saturating_sub(hide.width))),
            Style::default()
                .fg(if focused {
                    palette.text_bright
                } else {
                    palette.text_primary
                })
                .bg(palette.surface)
                .add_modifier(Modifier::BOLD),
        ))),
        header,
    );
    frame.render_widget(
        Paragraph::new(" × hide ").style(
            Style::default()
                .fg(palette.neutral_gray)
                .bg(palette.surface),
        ),
        hide,
    );
    hits.push((hide, Hit::TerminalPanelHide));
    hits.push((header, Hit::TerminalPanel));
    let output = panel_output(area);
    let cursor = if let Some(terminal) = app.terminal_panel.terminal.as_ref() {
        terminal.render(output, frame.buffer_mut(), focused, DefaultColors::NATIVE)
    } else {
        let message =
            app.terminal_panel
                .message
                .as_deref()
                .unwrap_or(if app.terminal_panel.busy() {
                    "Starting a shell in this folder…"
                } else {
                    "Press Enter to open a shell."
                });
        frame.render_widget(
            Paragraph::new(message)
                .wrap(Wrap { trim: true })
                .style(Style::default().fg(palette.text_muted)),
            output,
        );
        None
    };
    if app.terminal_panel.terminal.is_some() {
        if let Some(message) = app.terminal_panel.message.as_ref() {
            frame.render_widget(
                Paragraph::new(message.as_str()).style(Style::default().fg(palette.accent_pink)),
                Rect {
                    height: output.height.min(1),
                    ..output
                },
            );
        }
    }
    if !matches!(app.mode, AppMode::Normal) {
        fade(frame.buffer_mut(), area);
    } else if !focused {
        fade_preview(frame.buffer_mut(), area);
    }
    cursor
}
