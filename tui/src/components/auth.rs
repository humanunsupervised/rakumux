use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, Paragraph},
    Frame,
};

use crate::app::types::AuthMenu;
use crate::components::utils::popup_area;
use crate::theme;

fn palette() -> &'static theme::Palette {
    theme::palette()
}

pub fn draw_auth_menu(f: &mut Frame, menu: &AuthMenu, area: Rect) {
    // Centered popup area
    let popup = popup_area(area, 60, 40);

    // Clear background
    f.render_widget(Clear, popup);
    f.render_widget(Block::default().style(palette().base()), popup);

    // Layout: title + list + help
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(3),
            Constraint::Length(3),
        ])
        .split(popup);

    // Title
    let title = Paragraph::new("Account")
        .alignment(Alignment::Center)
        .block(
            Block::default().borders(Borders::ALL).style(
                Style::default()
                    .fg(palette().accent_primary)
                    .bg(palette().background),
            ),
        );
    f.render_widget(title, chunks[0]);

    // Items
    let items: Vec<ListItem> = menu
        .items
        .iter()
        .enumerate()
        .map(|(i, text)| {
            let style = if i == menu.selected {
                Style::default()
                    .fg(palette().background)
                    .bg(palette().accent_primary)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(palette().text_primary)
            };
            ListItem::new(Line::from(Span::styled(*text, style)))
        })
        .collect();

    let list = List::new(items).block(
        Block::default()
            .title("Choose an action")
            .borders(Borders::ALL)
            .style(Style::default().fg(palette().accent_primary)),
    );
    f.render_widget(list, chunks[1]);

    // Help, shorter when the popup is narrow (a phone).
    let full = "[↑/↓] Move • [Enter] Select • [Esc] Close";
    let text = if usize::from(chunks[2].width.saturating_sub(2)) >= full.chars().count() {
        full
    } else {
        "↑/↓ Move • Enter Select • Esc Close"
    };
    let help = Paragraph::new(text).alignment(Alignment::Center).block(
        Block::default()
            .borders(Borders::ALL)
            .style(Style::default().fg(palette().text_muted)),
    );
    f.render_widget(help, chunks[2]);
}
