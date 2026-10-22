//! Session tab bar (Tabs layout): the active list tab's filter at the left
//! end (the folder filter under All folders), then the tabs drawn by
//! `layout::render_tabs`.

use ratatui::{
    layout::Rect,
    style::Style,
    text::Span,
    widgets::{Block, Paragraph},
    Frame,
};

use super::folder_picker::draw_selector;
use super::format::{activity_badge, needs_attention, status_glyph};
use crate::app::types::{AppMode, Focus};
use crate::app::{App, Hit};
use crate::layout::{self, TabItem};
use crate::theme;

/// Columns the folder filter may take from the tab bar.
const SELECTOR_MAX: u16 = 28;

/// Tab items for the visible sessions, in list order.
pub fn tab_items(app: &App) -> (Vec<TabItem>, usize) {
    let visible = app.list.visible();
    let selected = app
        .list
        .selected_id()
        .and_then(|id| visible.iter().position(|s| &s.id == id))
        .unwrap_or(0);
    let items = visible
        .iter()
        .enumerate()
        .map(|(index, session)| {
            let (glyph, glyph_style) = status_glyph(session);
            let badge = activity_badge(session);
            TabItem {
                name: session.name.clone(),
                glyph: if badge.is_some() {
                    String::new()
                } else {
                    glyph.to_string()
                },
                glyph_style,
                selected: index == selected,
                attention: needs_attention(session),
                pinned: session.pinned,
                badge: badge.map(|(mark, style)| (mark.to_string(), style)),
            }
        })
        .collect();
    (items, selected)
}

/// Draws the folder filter and the tab bar; records the filter's click target
/// in `hits` and returns the tabs' targets for `layout::tab_at`.
pub fn draw_session_tabs(
    f: &mut Frame,
    app: &App,
    area: Rect,
    hits: &mut Vec<(Rect, Hit)>,
) -> Vec<(usize, Rect)> {
    let p = theme::palette();
    f.render_widget(
        Block::default().style(Style::default().bg(p.background)),
        area,
    );
    let (items, selected) = tab_items(app);
    // A phone shows the tabs in session mode only; the folder filter is the
    // list's first row.
    if layout::is_phone(app.area.width) {
        return layout::render_tabs(area, f.buffer_mut(), &items, selected);
    }
    let focused = app.focus == Focus::List && matches!(app.mode, AppMode::Normal);
    let selector = draw_selector(
        f,
        app,
        Rect {
            width: area.width.min(SELECTOR_MAX),
            ..area
        },
        focused,
    );
    hits.push((selector, Hit::FolderSelector));
    let tabs = Rect {
        x: selector.right() + 1,
        width: area.width.saturating_sub(selector.width + 1),
        ..area
    };
    if items.is_empty() {
        f.render_widget(
            Paragraph::new(Span::styled(
                " No sessions yet · n new session",
                Style::default().fg(p.text_muted),
            )),
            tabs,
        );
        return Vec::new();
    }
    layout::render_tabs(tabs, f.buffer_mut(), &items, selected)
}
