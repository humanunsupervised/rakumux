//! Left pane (Sidebar layout): the folder filter at the top (a folder icon
//! and `All folders`, a folder's name or `3 folders`; Enter opens the
//! Folders modal), then the sessions of the folders shown, under a header
//! row per folder when there are several, and "+ New session" last.
//! Each session is a compact cell after a blank row: name (with a pin mark),
//! a padded activity badge and age at the right; then its program, branch and state.
//! The selected item's text is white and the others' muted; nothing has a
//! background except the folder filter. A session being dragged has a blue
//! bar on its left.

use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Paragraph},
    Frame,
};
use session_core::model::{ScopeRoot, SessionView};

use super::folder_picker::{count_label, folder_branch};
use super::format::{
    activity_badge, now_secs, relative_time, secondary_line, status_glyph, truncate, PIN_MARK,
};
use super::utils::get_spinner_char;
use crate::app::types::AppMode;
use crate::app::{App, Connection, Hit, ListRow};
use crate::layout::str_width;
use crate::theme;

/// Rows per list row below the folder filter: a blank row, then the
/// content (one line for "+ New session" and folder headers, two for a
/// session).
fn row_height(row: &ListRow) -> u16 {
    match row {
        ListRow::Folders => 0,
        ListRow::NewSession | ListRow::FolderHeader(_) => 2,
        ListRow::Session(_) => 3,
    }
}

/// Rows of the folder filter: its line with a row of padding above and
/// below, when the pane has room.
fn folder_item_height(pane: u16) -> u16 {
    if pane >= 8 {
        3
    } else {
        1
    }
}

/// Columns left of the content: room for the drag mark, and a gap.
const INDENT: usize = 3;

/// Shown left of a session while it's dragged.
const DRAG_MARK: &str = "▎";

/// First row to draw so that `selected` is fully visible in `height`, given
/// each row's height. A folder header right above the first row is kept
/// when it fits, so a group's first session isn't shown without its header.
pub fn first_visible_row(heights: &[u16], selected: usize, height: u16) -> usize {
    let selected = selected.min(heights.len().saturating_sub(1));
    let used = |first: usize| -> u16 {
        heights
            .get(first..=selected)
            .map(|rows| rows.iter().sum())
            .unwrap_or(0)
    };
    let mut first = 0;
    while first < selected && used(first) > height {
        first += 1;
    }
    first
}

/// `content` at the bottom of a cell of `height` rows (the blank rows
/// above it separate it from the previous cell), indented.
fn cell_lines(content: Vec<Vec<Span<'_>>>, height: u16) -> Vec<Line<'_>> {
    let mut lines = vec![Line::default(); (height as usize).saturating_sub(content.len())];
    for spans in content {
        let mut line = vec![Span::raw(" ".repeat(INDENT))];
        line.extend(spans);
        lines.push(Line::from(line));
    }
    lines
}

/// The session being dragged: a bar in the indent of its content lines.
fn mark_dragged(lines: &mut [Line<'_>]) {
    let bar = Span::styled(
        format!(" {DRAG_MARK} "),
        Style::default().fg(theme::palette().accent_primary),
    );
    for line in lines.iter_mut().filter(|l| !l.spans.is_empty()) {
        line.spans[0] = bar.clone();
    }
}

/// A folder header: name, branch, a rule, then `live/total`, in `width`
/// columns (the branch goes first when there's no room). Selected, the
/// name is white and the rule blue.
pub fn folder_header_line(
    app: &App,
    folder: &ScopeRoot,
    width: usize,
    selected: bool,
) -> Line<'static> {
    let p = theme::palette();
    let name = app.folders.name_of(folder);
    let branch = folder_branch(app, folder).unwrap_or_default();
    let (live, total) = app.list.counts(folder);
    let count = count_label(live, total);
    // " name  branch ─── count": three spaces, and at least two rule
    // columns.
    let fixed = 3 + str_width(&count);
    let name = truncate(&name, width.saturating_sub(fixed + 2));
    let room = width.saturating_sub(fixed + str_width(&name) + 2);
    let branch_part = if !branch.is_empty() && room >= 6 {
        format!("  {}", truncate(&branch, room - 2))
    } else {
        String::new()
    };
    let rule = width.saturating_sub(fixed + str_width(&name) + str_width(&branch_part));
    let (name_color, rule_color) = if selected {
        (p.text_bright, p.accent_primary)
    } else {
        (p.accent_purple, p.border_dim)
    };
    Line::from(vec![
        Span::raw(" "),
        Span::styled(
            name,
            Style::default().fg(name_color).add_modifier(Modifier::BOLD),
        ),
        Span::styled(branch_part, Style::default().fg(p.text_muted)),
        Span::raw(" "),
        Span::styled("─".repeat(rule), Style::default().fg(rule_color)),
        Span::raw(" "),
        Span::styled(count, Style::default().fg(p.text_muted)),
    ])
}

/// The two content lines of a session cell.
fn session_content(
    app: &App,
    session: &SessionView,
    width: usize,
    selected: bool,
    now: i64,
) -> Vec<Vec<Span<'static>>> {
    let p = theme::palette();
    let badge = activity_badge(session).map(|(mark, style)| {
        Span::styled(
            format!(" {mark} "),
            style.bg(p.surface).add_modifier(Modifier::BOLD),
        )
    });
    let (glyph, glyph_style) = if badge.is_some() {
        (" ", Style::default())
    } else {
        status_glyph(session)
    };
    let pin = if session.pinned { 2 } else { 0 };
    let badge_width = badge.as_ref().map_or(0, |span| span.width() + 1);
    let age = truncate(
        &relative_time(now, session.last_active_at),
        width.saturating_sub(3 + pin + badge_width),
    );
    let right_width = badge_width + str_width(&age);
    let mut name = session.name.clone();
    if app.is_self_session(&session.id) {
        name.push_str(" (this)");
    }
    // Leading marker slot, name, optional pin, at least one space, badge + age.
    let name = truncate(&name, width.saturating_sub(right_width + 3 + pin));
    let pad = width.saturating_sub(2 + str_width(&name) + pin + right_width);
    let (name_style, detail_color, age_color) = if selected {
        (
            Style::default()
                .fg(p.text_bright)
                .add_modifier(Modifier::BOLD),
            p.text_primary,
            p.neutral_gray,
        )
    } else {
        (
            Style::default()
                .fg(p.text_primary)
                .add_modifier(Modifier::BOLD),
            p.text_muted,
            p.text_muted,
        )
    };
    let mut first = vec![
        Span::styled(glyph, glyph_style),
        Span::raw(" "),
        Span::styled(name, name_style),
    ];
    if session.pinned {
        first.push(Span::raw(" "));
        first.push(Span::styled(PIN_MARK, Style::default().fg(p.accent_gold)));
    }
    first.push(Span::raw(" ".repeat(pad)));
    if let Some(badge) = badge {
        first.push(badge);
        first.push(Span::raw(" "));
    }
    first.push(Span::styled(age, Style::default().fg(age_color)));
    let mut second = vec![Span::raw("  ")];
    second.extend(
        secondary_line(
            session,
            width.saturating_sub(2),
            app.icons(),
            Style::default().fg(detail_color),
        )
        .spans,
    );
    vec![first, second]
}

/// The folder filter: `▤ All folders`, `▤ name  branch` or `▤ 3 folders`
/// on a raised band, with `▾` (it opens the Folders modal) or the text
/// filter on the right. White when selected, muted otherwise.
fn draw_folder_item(f: &mut Frame, app: &App, area: Rect, selected: bool) {
    let p = theme::palette();
    f.render_widget(Block::default().style(Style::default().bg(p.surface)), area);
    let line = Rect {
        y: area.y + area.height / 2,
        height: 1,
        ..area
    };
    let open = matches!(app.mode, AppMode::Folders(_));
    let highlighted = selected || open;
    // The right end: the filter while there is one, else the arrow.
    let (right, right_style) = if app.list.filter_editing || !app.list.filter.is_empty() {
        let caret = if app.list.filter_editing { "▏" } else { "" };
        let filter = format!("/{}{caret}", app.list.filter.value());
        (
            truncate(&filter, (area.width as usize / 2).max(4)),
            Style::default().fg(p.accent_gold),
        )
    } else {
        let arrow = if open { "▴" } else { "▾" };
        let color = if highlighted {
            p.accent_primary
        } else {
            p.text_muted
        };
        (arrow.to_string(), Style::default().fg(color))
    };
    let right_width = str_width(&right) as u16 + 2;
    f.render_widget(
        Paragraph::new(Span::styled(right, right_style)),
        Rect {
            x: area.right().saturating_sub(right_width),
            width: right_width,
            ..line
        },
    );
    let icon = format!("{} ", app.icons().folder());
    let icon_color = if highlighted {
        p.accent_primary
    } else {
        p.neutral_gray
    };
    let room =
        (area.width as usize).saturating_sub(INDENT + right_width as usize + 1 + str_width(&icon));
    let label_color = if highlighted {
        p.text_bright
    } else {
        p.neutral_gray
    };
    let label = truncate(&app.folder_label(), room);
    let branch = match app.list.folder_filter() {
        [only] => folder_branch(app, only).unwrap_or_default(),
        _ => String::new(),
    };
    let room = room.saturating_sub(str_width(&label) + 2);
    let mut spans = vec![
        Span::raw(" ".repeat(INDENT)),
        Span::styled(icon, Style::default().fg(icon_color)),
        Span::styled(
            label,
            Style::default()
                .fg(label_color)
                .add_modifier(Modifier::BOLD),
        ),
    ];
    if !branch.is_empty() && room >= 6 {
        spans.push(Span::styled(
            format!("  {}", truncate(&branch, room)),
            Style::default().fg(p.text_muted),
        ));
    }
    f.render_widget(
        Paragraph::new(Line::from(spans)),
        Rect {
            width: area.width.saturating_sub(right_width),
            ..line
        },
    );
}

pub fn draw_session_list(f: &mut Frame, app: &App, area: Rect, hits: &mut Vec<(Rect, Hit)>) {
    let p = theme::palette();
    f.render_widget(
        Block::default().style(Style::default().fg(p.text_primary).bg(p.panel)),
        area,
    );
    if area.width == 0 || area.height == 0 {
        return;
    }
    let rows = app.list.rows();
    let selected = app.list.selected_row();
    // The folder filter stays at the top; the rows below it scroll.
    let item = Rect {
        height: folder_item_height(area.height),
        ..area
    };
    draw_folder_item(f, app, item, selected == 0);
    hits.push((item, Hit::FolderSelector));
    let inner = Rect {
        y: item.bottom(),
        height: area.height - item.height,
        ..area
    };
    if inner.height == 0 {
        return;
    }

    let heights: Vec<u16> = rows.iter().map(row_height).collect();
    let first = first_visible_row(&heights, selected.max(1), inner.height).max(1);
    // Content width: everything right of the indent, less a right margin.
    let width = (inner.width as usize).saturating_sub(INDENT + 1);
    let now = now_secs();
    let mut y = inner.y;
    let bottom = inner.y + inner.height;

    for (index, row) in rows.iter().enumerate().skip(first) {
        let height = heights[index];
        if y + height > bottom {
            break;
        }
        let row_area = Rect {
            x: inner.x,
            y,
            width: inner.width,
            height,
        };
        let is_selected = index == selected;
        let lines = match row {
            ListRow::Folders => Vec::new(),
            ListRow::NewSession => {
                let content = vec![vec![
                    Span::styled(
                        "+ ",
                        Style::default()
                            .fg(p.accent_green)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        truncate("New session", width.saturating_sub(2)),
                        Style::default()
                            .fg(if is_selected {
                                p.text_bright
                            } else {
                                p.accent_green
                            })
                            .add_modifier(Modifier::BOLD),
                    ),
                ]];
                cell_lines(content, height)
            }
            ListRow::FolderHeader(folder) => {
                let mut lines = vec![Line::default(); height as usize - 1];
                lines.push(folder_header_line(
                    app,
                    folder,
                    (inner.width as usize).saturating_sub(1),
                    is_selected,
                ));
                lines
            }
            ListRow::Session(session) => {
                let mut lines = cell_lines(
                    session_content(app, session, width, is_selected, now),
                    height,
                );
                if app.dragged_session() == Some(&session.id) {
                    mark_dragged(&mut lines);
                }
                lines
            }
        };
        f.render_widget(Paragraph::new(lines), row_area);
        hits.push((row_area, Hit::ListRow(index)));
        y += height;
    }

    if y < bottom && app.list.visible().is_empty() {
        let note = match &app.connection {
            Connection::Connecting => format!("   {} connecting…", get_spinner_char()),
            Connection::Connected(_) if !app.list.is_loaded() => {
                format!("   {} loading…", get_spinner_char())
            }
            Connection::Connected(_) if !app.list.filter.is_empty() => "   no matches".into(),
            Connection::Connected(_) => "   no sessions yet".into(),
            Connection::Failed(_) => "   daemon unavailable".into(),
        };
        f.render_widget(
            Paragraph::new(Span::styled(note, Style::default().fg(p.text_muted))),
            Rect {
                y: y + 1,
                height: 1,
                ..inner
            }
            .intersection(inner),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_colours_stay_visible_in_selected_and_unselected_session_items() {
        use crate::app::testing::{harness, on_branch, view};

        let h = harness(vec![]);
        let mut session = view("A", "a", 0);
        session.git = on_branch("main");
        session.agent = Some(session_core::model::AgentInfo {
            kind: "opencode".into(),
            ..Default::default()
        });
        let p = theme::palette();
        for selected in [false, true] {
            let lines = session_content(&h.app, &session, 40, selected, 0);
            let second = &lines[1];
            assert_eq!(second[1].content, "opencode");
            assert_eq!(second[1].style.fg, Some(p.label_slate_blue));
            assert_eq!(second[2].content, " · ⎇ main");
            assert_eq!(
                second[2].style.fg,
                Some(if selected {
                    p.text_primary
                } else {
                    p.text_muted
                })
            );
        }
    }

    #[test]
    fn session_titles_are_prominent_without_increasing_row_height() {
        use crate::app::testing::{harness, view};
        let h = harness(vec![]);
        let mut session = view("A", "A descriptive session title", 0);
        session.agent = Some(session_core::model::AgentInfo {
            kind: "opencode".into(),
            ..Default::default()
        });
        let p = theme::palette();
        for width in [12, 24, 40, 80] {
            for selected in [false, true] {
                let lines = session_content(&h.app, &session, width, selected, 0);
                assert_eq!(lines.len(), 2);
                let title = &lines[0][2];
                assert!(title.style.add_modifier.contains(Modifier::BOLD));
                assert_eq!(
                    title.style.fg,
                    Some(if selected {
                        p.text_bright
                    } else {
                        p.text_primary
                    })
                );
                assert!(!lines[1][1].style.add_modifier.contains(Modifier::BOLD));
                assert!(lines
                    .iter()
                    .all(|line| Line::from(line.clone()).width() <= width));
            }
        }
        assert_eq!(row_height(&ListRow::Session(&session)), 3);
    }

    #[test]
    fn padded_agent_badges_precede_the_right_aligned_time() {
        use crate::app::testing::{harness, view};
        use session_core::model::{AgentInfo, Lifecycle};
        let h = harness(vec![]);
        let mut session = view("A", "A very long 界 session title", 0);
        session.agent = Some(AgentInfo {
            kind: "claude".into(),
            ..Default::default()
        });
        let p = theme::palette();
        for lifecycle in [
            Lifecycle::Working,
            Lifecycle::Idle,
            Lifecycle::Done,
            Lifecycle::Blocked,
        ] {
            session.lifecycle = lifecycle;
            for width in [12, 24, 40, 80] {
                for selected in [false, true] {
                    for pinned in [false, true] {
                        session.pinned = pinned;
                        for now in [0, 45, 1_000_000_000] {
                            let lines = session_content(&h.app, &session, width, selected, now);
                            let first = &lines[0];
                            let (mark, style) = activity_badge(&session).unwrap();
                            let badge = &first[first.len() - 3];
                            assert_eq!(badge.content, format!(" {mark} "));
                            assert_eq!(badge.width(), 3);
                            assert_eq!(badge.style.fg, style.fg);
                            assert_eq!(badge.style.bg, Some(p.surface));
                            assert!(badge.style.add_modifier.contains(Modifier::BOLD));
                            assert_eq!(first[first.len() - 2].content, " ");
                            let age = first.last().unwrap();
                            assert!(!age.content.is_empty());
                            assert!(age
                                .content
                                .chars()
                                .all(|ch| ch.is_ascii_alphanumeric() || ch == '…'));
                            if now == 0 {
                                assert_eq!(age.content, "now");
                            }
                            assert_eq!(Line::from(first.clone()).width(), width);
                            assert!(Line::from(lines[1].clone()).width() <= width);
                            assert_eq!(first[0].content, " ", "no duplicate left-side badge");
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn scrolls_to_keep_the_selection_visible() {
        let cells = |n: usize| -> Vec<u16> {
            std::iter::once(3)
                .chain(std::iter::repeat(4).take(n))
                .collect()
        };
        let heights = cells(10);
        assert_eq!(first_visible_row(&heights, 0, 10), 0);
        // Rows 0..=4 need 3 + 4*4 = 19 lines.
        assert_eq!(first_visible_row(&heights, 4, 19), 0);
        assert_eq!(first_visible_row(&heights, 5, 19), 2);
        assert_eq!(first_visible_row(&heights, 10, 4), 10);
        assert_eq!(first_visible_row(&heights, 3, 1), 3);
        // A one-row header above the selection comes along when it fits.
        let grouped = [3, 1, 4, 4, 1, 4];
        assert_eq!(first_visible_row(&grouped, 5, 5), 4);
        assert_eq!(first_visible_row(&grouped, 5, 4), 5);
    }

    #[test]
    fn cell_content_follows_a_blank_row_and_is_indented() {
        let text = |lines: Vec<Line>| -> Vec<String> {
            lines
                .iter()
                .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
                .collect()
        };
        let lines = cell_lines(vec![vec![Span::raw("a")], vec![Span::raw("b")]], 3);
        assert_eq!(text(lines), vec!["", "   a", "   b"]);
        let lines = cell_lines(vec![vec![Span::raw("new")]], 2);
        assert_eq!(text(lines), vec!["", "   new"]);
    }

    #[test]
    fn sessions_take_three_rows_and_the_folder_filter_three_when_it_fits() {
        let session = crate::app::testing::view("A", "a", 1);
        assert_eq!(row_height(&ListRow::Session(&session)), 3);
        assert_eq!(row_height(&ListRow::NewSession), 2);
        assert_eq!(folder_item_height(20), 3);
        assert_eq!(folder_item_height(5), 1);
    }
}
