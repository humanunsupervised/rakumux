//! Session tab bar (Tabs layout): the active list tab's filter at the left
//! end (the folder filter under All folders), then the tabs drawn by
//! `layout::render_tabs_from`: the sessions that filter shows, in the
//! Folders tab's order whatever the active tab (`SessionList::tab_order`),
//! so tabs keep their places as statuses change. While the drawer is shown
//! its tab comes first, named by its active tab (Feed or rtop), before a
//! line, unnumbered and never moved (`draw_drawer_tab`). A phone's
//! session-mode tab row (and Grid's) is the same bar without the filter or
//! the drawer's tab.

use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Paragraph},
    Frame,
};

use super::drawer::tab_icon;
use super::folder_picker::draw_selector;
use super::footer::feed_count;
use super::format::{activity_badge, needs_attention, status_glyph};
use crate::app::drawer::DrawerTab;
use crate::app::types::{AppMode, Focus};
use crate::app::{App, Hit};
use crate::layout::{self, TabItem};
use crate::theme;

/// Columns the folder filter may take from the tab bar.
const SELECTOR_MAX: u16 = 28;

/// Tab items for the visible sessions, in the tabs' order. None is
/// highlighted while the drawer's tab is selected.
pub fn tab_items(app: &App) -> (Vec<TabItem>, usize) {
    let feed = app.drawer_has_keys();
    let visible = app.list.tab_order();
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
                selected: index == selected && !feed,
                attention: needs_attention(session),
                pinned: session.pinned,
                badge: badge.map(|(mark, style)| (mark.to_string(), style)),
            }
        })
        .collect();
    (items, selected)
}

/// Draws the folder filter and the tab bar; records the filter's click target
/// in `hits` and returns the tabs' targets for `layout::tab_at`, and the
/// first tab shown (kept as `App::tab_first` for the next draw).
pub fn draw_session_tabs(
    f: &mut Frame,
    app: &App,
    area: Rect,
    hits: &mut Vec<(Rect, Hit)>,
) -> (Vec<(usize, Rect)>, usize) {
    let p = theme::palette();
    f.render_widget(
        Block::default().style(Style::default().bg(p.background)),
        area,
    );
    let (items, selected) = tab_items(app);
    // A phone shows the tabs in session mode only; the folder filter is the
    // list's first row.
    if layout::is_phone(app.area.width) {
        return layout::render_tabs_from(area, f.buffer_mut(), &items, selected, app.tab_first);
    }
    let focused =
        app.focus == Focus::List && matches!(app.mode, AppMode::Normal) && !app.drawer_has_keys();
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
    let mut tabs = Rect {
        x: selector.right() + 1,
        width: area.width.saturating_sub(selector.width + 1),
        ..area
    };
    if app.drawer_tab_shown() {
        let used = draw_drawer_tab(f, app, tabs, hits);
        tabs.x += used;
        tabs.width = tabs.width.saturating_sub(used);
    }
    if items.is_empty() {
        f.render_widget(
            Paragraph::new(Span::styled(
                " No sessions yet · n new session",
                Style::default().fg(p.text_muted),
            )),
            tabs,
        );
        return (Vec::new(), 0);
    }
    layout::render_tabs_from(tabs, f.buffer_mut(), &items, selected, app.tab_first)
}

/// The drawer's tab at the left of `area`, named by its active tab:
/// ` ≡ Feed ●2 ` (the count of new entries in the unread mark's amber) or
/// ` ▥ rtop `, highlighted as a selected tab while the drawer has the keys,
/// then a line before the sessions' tabs. Records its target; returns the
/// columns used.
fn draw_drawer_tab(f: &mut Frame, app: &App, area: Rect, hits: &mut Vec<(Rect, Hit)>) -> u16 {
    let p = theme::palette();
    let selected = app.drawer_has_keys();
    let base = if selected {
        Style::default().bg(p.surface)
    } else {
        Style::default()
    };
    let name = base
        .fg(if selected {
            p.text_bright
        } else {
            p.text_primary
        })
        .add_modifier(Modifier::BOLD);
    let tab = app.drawer_tab();
    let icon = tab_icon(app, tab);
    let mut spans = vec![
        Span::styled(" ", base),
        Span::styled(icon, base.fg(p.accent_primary)),
        Span::styled(format!(" {}", tab.label()), name),
    ];
    if let Some(count) = feed_count(app).filter(|_| tab == DrawerTab::Feed) {
        spans.push(Span::styled(format!(" {count}"), base.fg(p.accent_gold)));
    }
    spans.push(Span::styled(" ", base));
    let width = spans.iter().map(Span::width).sum::<usize>() as u16;
    let tab = Rect {
        width: width.min(area.width),
        ..area
    };
    f.render_widget(Paragraph::new(Line::from(spans)), tab);
    hits.push((tab, Hit::DrawerBarTab));
    let line = Rect {
        x: tab.right() + 1,
        width: 1,
        ..area
    }
    .intersection(area);
    f.render_widget(
        Paragraph::new(Span::styled("│", Style::default().fg(p.border_dim))),
        line,
    );
    (width + 3).min(area.width)
}
