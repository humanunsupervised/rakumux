//! The left column (`app/left.rs`): what its tab shows (the sessions, the
//! Inbox or Beats), and its tabs fixed at the bottom, `▴ Launch   ≡ Inbox
//! ●2   ♥ Beats`: a band, the active one bold with a line over it, in the
//! accent while the column has the keys; beside the session, `«` at its right
//! hides the column. A rail is what's left of it then.

use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Paragraph},
    Frame,
};

use super::feed::draw_feed;
use super::footer::feed_count;
use super::heartbeats::draw_beats;
use super::session_list::{draw_session_list, folder_item_height};
use crate::app::left::LeftTab;
use crate::app::types::{AppMode, Focus};
use crate::app::{App, Hit};
use crate::layout::{left_split, str_width, truncate};
use crate::printable::printable;
use crate::theme;

/// Columns left of the first tab, as in the session list.
const INDENT: usize = 3;
/// Columns of `«` at the right of the tabs, a column either side.
const HIDE_COLS: u16 = 3;

/// The column in `area` (`Screen::list`, or the Tabs layout's
/// `Screen::panel`).
pub fn draw_left(f: &mut Frame, app: &App, area: Rect, hits: &mut Vec<(Rect, Hit)>) {
    let (body, tabs) = left_split(area);
    match app.left_tab() {
        LeftTab::Sessions => draw_session_list(f, app, body, hits),
        LeftTab::Inbox | LeftTab::Beats => {
            let p = theme::palette();
            f.render_widget(
                Block::default().style(Style::default().fg(p.text_primary).bg(p.panel)),
                body,
            );
            match app.left_tab() {
                LeftTab::Beats => draw_beats(f, app, body, hits),
                _ => draw_feed(f, app, body, hits),
            }
            hits.push((body, Hit::LeftPane));
        }
    }
    draw_left_tabs(f, app, tabs, hits);
}

/// The text row of the tabs in `area`.
fn text_row(area: Rect) -> Rect {
    Rect {
        y: area.y + area.height / 2,
        height: 1,
        ..area
    }
}

/// The columns a workspace's name gets on its tab.
const NAME_COLS: usize = 16;
/// The fewest it's cut to.
const NAME_MIN: usize = 6;

/// One tab as it's drawn: its mark (when there's room), name and count.
struct Tab {
    tab: LeftTab,
    icon: Option<&'static str>,
    name: String,
    count: Option<String>,
}

impl Tab {
    fn width(&self) -> usize {
        let icon = self.icon.map_or(0, |icon| str_width(icon) + 1);
        let count = self.count.as_ref().map_or(0, |count| str_width(count) + 1);
        icon + str_width(&self.name) + count
    }
}

/// The tabs, the indent and the gap between them, as wide as fits `room`:
/// short of it the gaps narrow, then the marks of the Inbox and Beats go,
/// then the workspace's name is cut, then the count goes.
fn fit(app: &App, room: usize) -> (Vec<Tab>, usize, usize) {
    let count = feed_count(app);
    let shown = app
        .has_workspaces()
        .then(|| truncate(&printable(&app.workspace_name()), NAME_COLS));
    let chevron = match (&shown, &app.mode) {
        (None, _) => None,
        (Some(_), AppMode::Workspaces(_)) => Some("▾"),
        (Some(_), _) => Some("▴"),
    };
    let tabs = |cols: usize, icons: bool, counts: bool| -> Vec<Tab> {
        LeftTab::ALL
            .into_iter()
            .map(|tab| match tab {
                LeftTab::Sessions => Tab {
                    tab,
                    icon: chevron.or(icons.then(|| app.icons().sessions())),
                    // The workspace shown (an older daemon has none).
                    name: match &shown {
                        Some(name) => truncate(name, cols),
                        None => tab.label().to_string(),
                    },
                    count: None,
                },
                LeftTab::Inbox => Tab {
                    tab,
                    icon: icons.then(|| app.icons().feed()),
                    name: tab.label().to_string(),
                    count: count.clone().filter(|_| counts),
                },
                LeftTab::Beats => Tab {
                    tab,
                    icon: icons.then(|| app.icons().beats()),
                    name: tab.label().to_string(),
                    count: None,
                },
            })
            .collect()
    };
    let mut tries = vec![
        (NAME_COLS, true, true, INDENT, 3),
        (NAME_COLS, true, true, INDENT, 2),
        (NAME_COLS, true, true, 1, 1),
        (NAME_COLS, false, true, 1, 2),
        (NAME_COLS, false, true, 1, 1),
    ];
    for counts in [true, false] {
        tries.extend(
            (NAME_MIN..NAME_COLS)
                .rev()
                .map(|cols| (cols, false, counts, 1, 1)),
        );
    }
    for (cols, icons, counts, indent, gap) in tries {
        let tabs = tabs(cols, icons, counts);
        let width: usize = tabs.iter().map(Tab::width).sum();
        if indent + width + gap * (tabs.len() - 1) <= room {
            return (tabs, indent, gap);
        }
    }
    (tabs(NAME_MIN, false, false), 0, 1)
}

fn draw_left_tabs(f: &mut Frame, app: &App, area: Rect, hits: &mut Vec<(Rect, Hit)>) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let p = theme::palette();
    f.render_widget(Block::default().style(Style::default().bg(p.surface)), area);
    let row = text_row(area);
    // Beside the session the column hides.
    let hides = app.list_beside_session() && area.width > HIDE_COLS;
    let room = match hides {
        true => area.width - HIDE_COLS,
        false => area.width,
    };
    let focused = app.focus == Focus::List && !app.rtop_has_keys();
    let mark = if focused {
        p.accent_primary
    } else {
        p.neutral_gray
    };
    let (tabs, indent, gap) = fit(app, usize::from(room));
    let mut spans = vec![Span::raw(" ".repeat(indent))];
    let mut x = indent;
    for (index, tab) in tabs.iter().enumerate() {
        if index > 0 {
            spans.push(Span::raw(" ".repeat(gap)));
            x += gap;
        }
        let active = tab.tab == app.left_tab();
        let (icon_style, name_style) = if active {
            let name = if focused {
                p.text_bright
            } else {
                p.neutral_gray
            };
            (
                Style::default().fg(mark),
                Style::default().fg(name).add_modifier(Modifier::BOLD),
            )
        } else {
            let quiet = Style::default().fg(p.text_muted);
            (quiet, quiet)
        };
        if let Some(icon) = tab.icon {
            spans.push(Span::styled(format!("{icon} "), icon_style));
        }
        spans.push(Span::styled(tab.name.clone(), name_style));
        if let Some(count) = &tab.count {
            spans.push(Span::styled(
                format!(" {count}"),
                Style::default().fg(p.accent_gold),
            ));
        }
        let width = tab.width();
        let at = |x: usize, width: usize, area: Rect| {
            Rect {
                x: area.x.saturating_add(x as u16),
                width: width as u16,
                ..area
            }
            .intersection(area)
        };
        if active && area.height >= 3 {
            let line = Rect { height: 1, ..area };
            let over = at(x, width, line);
            f.render_widget(
                Paragraph::new(Span::styled(
                    "▔".repeat(usize::from(over.width)),
                    Style::default().fg(mark),
                )),
                over,
            );
        }
        // The columns either side count too: a tap needn't be exact.
        let before = x.min(gap / 2);
        hits.push((
            at(x - before, before + width + gap - gap / 2, area),
            Hit::LeftTab(tab.tab),
        ));
        x += width;
    }
    f.render_widget(
        Paragraph::new(Line::from(spans)),
        Rect { width: room, ..row },
    );
    if hides {
        let hide = Rect {
            x: area.x + room,
            width: HIDE_COLS,
            ..area
        };
        f.render_widget(
            Paragraph::new(Span::styled(" «", Style::default().fg(p.neutral_gray))),
            text_row(hide),
        );
        hits.push((hide, Hit::HideList));
    }
}

/// What's left of a hidden column (Master detail): a column of its pane (the
/// band at its top, the tabs' with `»` where `«` was), then the divider's
/// line. A click anywhere on it shows the column.
pub fn draw_rail(f: &mut Frame, area: Rect, hits: &mut Vec<(Rect, Hit)>) {
    let p = theme::palette();
    if area.width == 0 || area.height == 0 {
        return;
    }
    let pane = Rect { width: 1, ..area };
    f.render_widget(Block::default().style(Style::default().bg(p.panel)), pane);
    let (_, tabs) = left_split(pane);
    let band = Rect {
        height: folder_item_height(area.height),
        ..pane
    };
    for band in [band, tabs] {
        f.render_widget(Block::default().style(Style::default().bg(p.surface)), band);
    }
    let mark = match tabs.height {
        0 => Rect {
            y: area.bottom() - 1,
            height: 1,
            ..pane
        },
        _ => text_row(tabs),
    };
    f.render_widget(
        Paragraph::new(Span::styled("»", Style::default().fg(p.neutral_gray)))
            .style(Style::default().bg(p.surface)),
        mark,
    );
    if area.width > 1 {
        let line = Rect {
            x: area.x + 1,
            width: area.width - 1,
            ..area
        };
        let lines: Vec<Line> = (0..line.height).map(|_| Line::from("│")).collect();
        f.render_widget(
            Paragraph::new(lines).style(Style::default().fg(p.border_dim).bg(p.background)),
            line,
        );
    }
    hits.push((area, Hit::ShowList));
}
