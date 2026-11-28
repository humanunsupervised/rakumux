//! The drawer (`app/drawer.rs`) where the screen puts it ([`draw_drawer`]):
//! a band naming it, `▥ rtop`, in the accent while it has the keys, over rtop
//! (`rtop.rs`). A click anywhere on it focuses it.

use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Paragraph},
    Frame,
};

use super::rtop::draw_rtop;
use super::session_list::folder_item_height;
use crate::app::drawer::Panel;
use crate::app::{App, Hit};
use crate::layout::str_width;
use crate::theme;

/// Columns left of the band's name, as in the session list.
pub(super) const INDENT: usize = 3;

/// The drawer in `area` (`Screen::feed`): its band and rtop.
pub fn draw_drawer(f: &mut Frame, app: &App, area: Rect, hits: &mut Vec<(Rect, Hit)>) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let p = theme::palette();
    f.render_widget(
        Block::default().style(Style::default().fg(p.text_primary).bg(p.panel)),
        area,
    );
    let band = Rect {
        height: folder_item_height(area.height),
        ..area
    };
    draw_band(f, app, band);
    let body = Rect {
        y: band.bottom(),
        height: area.height - band.height,
        ..area
    };
    draw_rtop(f, app, body, hits);
    hits.push((area, Hit::Drawer));
}

/// `panel`'s mark in the icon style the user picked.
pub(super) fn panel_icon(app: &App, panel: Panel) -> &'static str {
    match panel {
        Panel::Feed => app.icons().feed(),
        Panel::Beats => app.icons().beats(),
        Panel::Rtop => app.icons().rtop(),
    }
}

/// The drawer's band, as tall as the list's tabs so what's under it starts
/// where the sessions do: its mark and name, bold and underlined, in the
/// accent while it has the keys.
fn draw_band(f: &mut Frame, app: &App, area: Rect) {
    draw_panel_band(f, app, Panel::Rtop, app.rtop_has_keys(), false, area);
}

/// `panel`'s band in `area` (the drawer's, or the left column's over the
/// Inbox and Heartbeats): its mark and name, bold and underlined, in the
/// accent while `focused`; with `back`, a `‹` before them while one thing is
/// read whole (a click on the band then goes back).
pub(super) fn draw_panel_band(
    f: &mut Frame,
    app: &App,
    panel: Panel,
    focused: bool,
    back: bool,
    area: Rect,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let p = theme::palette();
    f.render_widget(Block::default().style(Style::default().bg(p.surface)), area);
    let (mark, label) = if focused {
        (p.accent_primary, p.text_bright)
    } else {
        (p.neutral_gray, p.neutral_gray)
    };
    let icon = panel_icon(app, panel);
    // Two after Heartbeats' `♥`, as on its tab: many fonts draw it wider.
    let gap = match panel {
        Panel::Beats if str_width(icon) == 1 => "  ",
        _ => " ",
    };
    let name = panel.label();
    let arrow = if back { "‹ " } else { "" };
    let line = Line::from(vec![
        Span::raw(" ".repeat(INDENT)),
        Span::styled(
            arrow,
            Style::default()
                .fg(p.accent_primary)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(format!("{icon}{gap}"), Style::default().fg(mark)),
        Span::styled(
            name,
            Style::default().fg(label).add_modifier(Modifier::BOLD),
        ),
    ]);
    f.render_widget(
        Paragraph::new(line),
        Rect {
            y: area.y + area.height / 2,
            height: 1,
            ..area
        },
    );
    if area.height >= 3 {
        let under = Rect {
            x: area.x.saturating_add((INDENT + str_width(arrow)) as u16),
            y: area.bottom() - 1,
            width: (str_width(icon) + gap.len() + str_width(name)) as u16,
            height: 1,
        }
        .intersection(area);
        f.render_widget(
            Paragraph::new(Span::styled(
                "▁".repeat(usize::from(under.width)),
                Style::default().fg(mark),
            )),
            under,
        );
    }
}
