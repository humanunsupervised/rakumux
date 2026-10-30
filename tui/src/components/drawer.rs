//! The drawer (`app/drawer.rs`) where the screen puts it ([`draw_drawer`]):
//! a band naming its tabs, `≡ Feed   ▥ rtop`, the active one underlined
//! and, while the drawer has the keys, in the accent; under it the active
//! tab: the feed's cards or reply (`feed.rs`), or rtop (`rtop.rs`). A click
//! on a tab's name shows it; a click anywhere else on the drawer focuses it.

use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Paragraph},
    Frame,
};

use super::feed::draw_feed;
use super::rtop::draw_rtop;
use super::session_list::folder_item_height;
use crate::app::drawer::DrawerTab;
use crate::app::{App, Hit};
use crate::layout::str_width;
use crate::theme;

/// Columns left of the band's first tab, as in the session list.
pub(super) const INDENT: usize = 3;
/// Columns between the band's tabs.
const TAB_GAP: usize = 3;

/// The drawer in `area` (`Screen::feed`): its band and active tab.
pub fn draw_drawer(f: &mut Frame, app: &App, area: Rect, hits: &mut Vec<(Rect, Hit)>) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    match app.drawer_tab() {
        DrawerTab::Feed => draw_feed(f, app, area, hits),
        DrawerTab::Rtop => {
            let p = theme::palette();
            f.render_widget(
                Block::default().style(Style::default().fg(p.text_primary).bg(p.panel)),
                area,
            );
            let band = Rect {
                height: folder_item_height(area.height),
                ..area
            };
            draw_band(f, app, band, false, hits);
            draw_rtop(
                f,
                app,
                Rect {
                    y: band.bottom(),
                    height: area.height - band.height,
                    ..area
                },
                hits,
            );
        }
    }
    hits.push((area, Hit::Drawer));
}

/// `tab`'s mark in the icon style the user picked.
pub(super) fn tab_icon(app: &App, tab: DrawerTab) -> &'static str {
    match tab {
        DrawerTab::Feed => app.icons().feed(),
        DrawerTab::Rtop => app.icons().rtop(),
    }
}

/// The drawer's band, as tall as the list's tabs so what's under it starts
/// where the sessions do: each tab's mark and name, the active one bold and
/// underlined, in the accent while the drawer has the keys. With `back` (a
/// reply read in the feed's cards' place) it starts with `‹`, and a click
/// there goes back to the cards.
pub(super) fn draw_band(
    f: &mut Frame,
    app: &App,
    area: Rect,
    back: bool,
    hits: &mut Vec<(Rect, Hit)>,
) {
    let p = theme::palette();
    f.render_widget(Block::default().style(Style::default().bg(p.surface)), area);
    let focused = app.drawer_has_keys();
    let active = app.drawer_tab();
    let (mark, label) = if focused {
        (p.accent_primary, p.text_bright)
    } else {
        (p.neutral_gray, p.neutral_gray)
    };
    let row = Rect {
        y: area.y + area.height / 2,
        height: 1,
        ..area
    };
    let mut spans = vec![Span::raw(" ".repeat(INDENT))];
    let mut x = INDENT;
    if back {
        spans.push(Span::styled(
            "‹ ",
            Style::default().fg(mark).add_modifier(Modifier::BOLD),
        ));
        hits.push((
            Rect {
                x: area.x,
                width: (x + 2).min(usize::from(area.width)) as u16,
                ..area
            },
            Hit::FeedBack,
        ));
        x += 2;
    }
    let mut underline = None;
    for (index, tab) in DrawerTab::ALL.into_iter().enumerate() {
        if index > 0 {
            spans.push(Span::raw(" ".repeat(TAB_GAP)));
            x += TAB_GAP;
        }
        let icon = tab_icon(app, tab);
        let name = tab.label();
        let width = str_width(icon) + 1 + str_width(name);
        let (icon_style, name_style) = if tab == active {
            (
                Style::default().fg(mark),
                Style::default().fg(label).add_modifier(Modifier::BOLD),
            )
        } else {
            let quiet = Style::default().fg(p.text_muted);
            (quiet, quiet)
        };
        spans.extend([
            Span::styled(icon, icon_style),
            Span::raw(" "),
            Span::styled(name, name_style),
        ]);
        let target = Rect {
            x: area.x.saturating_add(x as u16),
            width: width as u16,
            ..area
        }
        .intersection(area);
        if tab == active {
            underline = Some(target);
        }
        hits.push((target, Hit::DrawerTab(tab)));
        x += width;
    }
    f.render_widget(Paragraph::new(Line::from(spans)), row);
    if let Some(target) = underline.filter(|_| area.height >= 3) {
        f.render_widget(
            Paragraph::new(Span::styled(
                "▁".repeat(usize::from(target.width)),
                Style::default().fg(mark),
            )),
            Rect {
                y: area.bottom() - 1,
                height: 1,
                ..target
            },
        );
    }
}
