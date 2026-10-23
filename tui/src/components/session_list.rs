//! Left pane (Sidebar layout): the tabs at the top (All folders, App,
//! Status: what the list is grouped and filtered by; the active one names
//! its filter, `▤ web ▾`, and Enter or a click on it opens its modal), then
//! the sessions shown, under a header row per folder (app, status) when
//! there are several, and "+ New session" last.
//! Each session is a compact cell after a blank row: name (with a pin mark),
//! its age and activity mark at the right; then its program, branch and state;
//! then, when signed in to the Rakumux account, its AI description.
//! The selected item's text is white and the others' muted; nothing has a
//! background except the tabs' band. A session being dragged has a blue
//! bar on its left.

use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Paragraph},
    Frame,
};
use session_core::model::{ScopeRoot, SessionView};

use super::folder_picker::{count_label, folder_branch, tab_icon, tab_picker_open};
use super::format::{
    activity_badge_at, now_secs, read_period_style, relative_time, secondary_line, status_glyph,
    status_group_glyph, truncate, PIN_MARK,
};
use super::section_picker::{section_count, sessions_label};
use super::utils::get_spinner_char;
use crate::app::grouping::{Grouping, ReadPeriod, Section};
use crate::app::{App, Connection, Hit, ListRow};
use crate::layout::str_width;
use crate::theme;

/// Rows per list row below the tabs: a blank row, then the content (one
/// line for "+ New session" and headers, two for a session).
fn row_height(row: &ListRow) -> u16 {
    match row {
        ListRow::Folders => 0,
        ListRow::NewSession
        | ListRow::FolderHeader(_)
        | ListRow::WorktreeHeader(..)
        | ListRow::SectionHeader(_)
        | ListRow::ReadPeriodHeader(_) => 2,
        ListRow::Session(_) => 3,
    }
}

/// [`row_height`], plus a line for a session's AI description.
fn row_height_in(app: &App, row: &ListRow) -> u16 {
    match row {
        ListRow::Session(session) if app.description(&session.id).is_some() => 4,
        row => row_height(row),
    }
}

/// Rows of the tabs' band: its line with a row of padding above and below
/// (the active tab underlined on it), when the pane has room.
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

/// Shown in the indent of a hovered unpinned session's first line: a tap
/// pins it.
const PIN_TOGGLE: &str = "☆";

/// The hovered session's pin toggle, just left of its name: in its status
/// mark's place (which it covers while hovered), the column of ready's
/// `★ pinned` header's star. `☆` to pin it, the gold `★` to unpin it.
/// Returns where it is: the mark and the space after it.
fn mark_hovered(lines: &mut [Line<'_>], pinned: bool, area: Rect) -> Option<Rect> {
    let p = theme::palette();
    let first = lines.iter().position(|line| !line.spans.is_empty())?;
    let (mark, style) = if pinned {
        (PIN_MARK, Style::default().fg(p.accent_gold))
    } else {
        (PIN_TOGGLE, Style::default().fg(p.text_muted))
    };
    // After the indent, the status mark (see `session_content`).
    let slot = lines[first].spans.get_mut(1)?;
    *slot = Span::styled(mark, style);
    let x = area.x + u16::try_from(INDENT).ok()?;
    Some(Rect {
        x,
        y: area.y + u16::try_from(first).ok()?,
        width: 2.min(area.right().saturating_sub(x)),
        height: 1,
    })
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

/// A linked worktree's header inside a folder, indented under the folder's
/// own: the branch mark, its path in the folder (`worktrees/feature`), its
/// branch when that says more, a rule, then `live/total`. Selected, the
/// label is white and the rule blue.
pub fn worktree_header_line(
    app: &App,
    folder: &ScopeRoot,
    root: &std::path::Path,
    width: usize,
    selected: bool,
) -> Line<'static> {
    let p = theme::palette();
    let label = format!(
        "{} {}",
        app.icons().branch(),
        crate::app::grouping::worktree_label(folder, root)
    );
    let branch = app
        .list
        .worktree_branch(folder, root)
        .filter(|branch| !label.ends_with(branch))
        .unwrap_or_default()
        .to_string();
    let (live, total) = app.list.worktree_counts(folder, root);
    let count = count_label(live, total);
    // "   label  branch ─── count": the indent, then as for a folder.
    let fixed = 5 + str_width(&count);
    let label = truncate(&label, width.saturating_sub(fixed + 2));
    let room = width.saturating_sub(fixed + str_width(&label) + 2);
    let branch_part = if !branch.is_empty() && room >= 6 {
        format!("  {}", truncate(&branch, room - 2))
    } else {
        String::new()
    };
    let rule = width.saturating_sub(fixed + str_width(&label) + str_width(&branch_part));
    let (label_color, rule_color) = if selected {
        (p.text_bright, p.accent_primary)
    } else {
        (p.label_blue, p.border_dim)
    };
    Line::from(vec![
        Span::raw("   "),
        Span::styled(
            label,
            Style::default()
                .fg(label_color)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(branch_part, Style::default().fg(p.text_muted)),
        Span::raw(" "),
        Span::styled("─".repeat(rule), Style::default().fg(rule_color)),
        Span::raw(" "),
        Span::styled(count, Style::default().fg(p.text_muted)),
    ])
}

/// The header of one of ready's sub-sections, indented under ready like a
/// worktree's under its folder: a clock and when its sessions were last
/// read (a pin for the pinned ones), a rule, then how many it has, in
/// `width` columns.
pub fn read_period_header_line(
    app: &App,
    period: ReadPeriod,
    width: usize,
    selected: bool,
) -> Line<'static> {
    let p = theme::palette();
    let count = sessions_label(app.list.read_period_count(period));
    // "   ◷ label ─── count": the indent, and at least two rule columns.
    let clock = match period {
        ReadPeriod::Pinned => format!("{PIN_MARK} "),
        _ => format!("{} ", app.icons().clock()),
    };
    let fixed = 5 + str_width(&clock) + str_width(&count);
    let label = truncate(period.label(), width.saturating_sub(fixed + 2));
    let rule = width.saturating_sub(fixed + str_width(&label));
    let (label_color, rule_color) = if selected {
        (p.text_bright, p.accent_primary)
    } else {
        (p.label_blue, p.border_dim)
    };
    Line::from(vec![
        Span::raw("   "),
        // The clock in the amber its sessions' marks have.
        Span::styled(clock, read_period_style(period)),
        Span::styled(
            label,
            Style::default()
                .fg(label_color)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" "),
        Span::styled("─".repeat(rule), Style::default().fg(rule_color)),
        Span::raw(" "),
        Span::styled(count, Style::default().fg(p.text_muted)),
    ])
}

/// An app's or a status's header, like a folder's: its name (a status
/// after its mark), a rule, then how many sessions it has (`2/3 live` for
/// an app, `3 sessions` for a status), in `width` columns. Selected, the
/// name is white and the rule blue.
pub fn section_header_line(
    app: &App,
    section: &Section,
    width: usize,
    selected: bool,
) -> Line<'static> {
    let p = theme::palette();
    let count = section_count(app, section);
    let (mark, mark_style) = match section {
        Section::Status(status) => {
            let (glyph, style) = status_group_glyph(*status);
            (format!("{glyph} "), style)
        }
        Section::App(_) => (String::new(), Style::default()),
    };
    // " mark name ─── count": three spaces, and at least two rule columns.
    let fixed = 3 + str_width(&mark) + str_width(&count);
    let name = truncate(section.label(), width.saturating_sub(fixed + 2));
    let rule = width.saturating_sub(fixed + str_width(&name));
    let (name_color, rule_color) = if selected {
        (p.text_bright, p.accent_primary)
    } else {
        (p.accent_purple, p.border_dim)
    };
    Line::from(vec![
        Span::raw(" "),
        Span::styled(mark, mark_style),
        Span::styled(
            name,
            Style::default().fg(name_color).add_modifier(Modifier::BOLD),
        ),
        Span::raw(" "),
        Span::styled("─".repeat(rule), Style::default().fg(rule_color)),
        Span::raw(" "),
        Span::styled(count, Style::default().fg(p.text_muted)),
    ])
}

/// The content lines of a session cell: two, and its AI description.
fn session_content(
    app: &App,
    session: &SessionView,
    width: usize,
    selected: bool,
    now: i64,
) -> Vec<Vec<Span<'static>>> {
    let p = theme::palette();
    let badge = activity_badge_at(session, now)
        .map(|(mark, style)| Span::styled(mark, style.add_modifier(Modifier::BOLD)));
    let (glyph, glyph_style) = if badge.is_some() {
        (" ", Style::default())
    } else {
        status_glyph(session)
    };
    let badge_width = badge.as_ref().map_or(0, |span| span.width() + 1);
    let age = truncate(
        &relative_time(now, session.last_active_at),
        width.saturating_sub(3 + badge_width),
    );
    let right_width = badge_width + str_width(&age);
    let mut name = session.name.clone();
    if app.is_self_session(&session.id) {
        name.push_str(" (this)");
    }
    // Leading marker slot, name, at least one space, age + badge. A pin
    // shows only under the pointer, left of the row (`mark_hovered`).
    let name = truncate(&name, width.saturating_sub(right_width + 3));
    let pad = width.saturating_sub(2 + str_width(&name) + right_width);
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
                .fg(p.text_unselected)
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
    first.push(Span::raw(" ".repeat(pad)));
    first.push(Span::styled(age, Style::default().fg(age_color)));
    if let Some(badge) = badge {
        first.push(Span::raw(" "));
        first.push(badge);
    }
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
    let mut lines = vec![first, second];
    if let Some(description) = app.description(&session.id) {
        lines.push(vec![
            Span::raw("  "),
            Span::styled(
                truncate(description, width.saturating_sub(2)),
                Style::default()
                    .fg(detail_color)
                    .add_modifier(Modifier::ITALIC),
            ),
        ]);
    }
    lines
}

/// Columns between two tabs when there's room for them (fewer otherwise).
const TAB_GAP: usize = 3;

/// Columns the text filter keeps for itself at the right end of the band.
const FILTER_MAX: usize = 12;

/// How the tabs fit their room.
#[derive(Debug, Clone, PartialEq, Eq)]
struct TabLayout {
    /// Columns between two tabs.
    gap: usize,
    /// The inactive tabs show their mark alone.
    compact: bool,
    /// The active tab's label, cut when nothing else made room.
    active_label: String,
    /// Columns the tabs take.
    width: usize,
}

/// Lay the tabs (`(tab, mark, label)`) out in `room` columns: full labels
/// three columns apart, else two apart, else the inactive tabs as their
/// marks alone, else the active tab's label cut. The active tab ends in
/// ` ▾`.
fn lay_out_tabs(tabs: &[(Grouping, &str, String)], active: Grouping, room: usize) -> TabLayout {
    let width = |gap: usize, compact: bool, active_label: usize| -> usize {
        let tabs_width: usize = tabs
            .iter()
            .map(
                |(grouping, icon, label)| match (*grouping == active, compact) {
                    (true, _) => str_width(icon) + 1 + active_label + 2,
                    (false, true) => str_width(icon),
                    (false, false) => str_width(icon) + 1 + str_width(label),
                },
            )
            .sum();
        tabs_width + gap * tabs.len().saturating_sub(1)
    };
    let active_label = tabs
        .iter()
        .find(|(grouping, _, _)| *grouping == active)
        .map(|(_, _, label)| label.clone())
        .unwrap_or_default();
    let label_width = str_width(&active_label);
    for (gap, compact) in [(TAB_GAP, false), (2, false), (2, true), (1, true)] {
        let used = width(gap, compact, label_width);
        if used <= room {
            return TabLayout {
                gap,
                compact,
                active_label,
                width: used,
            };
        }
    }
    let active_label = truncate(&active_label, room.saturating_sub(width(1, true, 0)));
    TabLayout {
        gap: 1,
        compact: true,
        width: width(1, true, str_width(&active_label)),
        active_label,
    }
}

/// The tabs on a raised band: Folders, App, Status. The active one is
/// bold, names its filter (`▤ web  ⎇ main`, `◇ claude`), ends in `▾` (it
/// opens its modal) and is underlined; the text filter shows at the right
/// end while there is one. White and blue when selected, muted otherwise.
/// Records a click target per tab.
fn draw_tabs(f: &mut Frame, app: &App, area: Rect, selected: bool, hits: &mut Vec<(Rect, Hit)>) {
    let p = theme::palette();
    f.render_widget(Block::default().style(Style::default().bg(p.surface)), area);
    let line = Rect {
        y: area.y + area.height / 2,
        height: 1,
        ..area
    };
    let active = app.list.grouping();
    let open = tab_picker_open(app);
    let highlighted = selected || open;
    // The text filter at the right end, while there is one.
    let filter = (app.list.filter_editing || !app.list.filter.is_empty()).then(|| {
        let caret = if app.list.filter_editing { "▏" } else { "" };
        format!("/{}{caret}", app.list.filter.value())
    });
    let reserve = filter
        .as_ref()
        .map_or(0, |filter| str_width(filter).min(FILTER_MAX) + 2);
    let room = (area.width as usize).saturating_sub(INDENT + 1 + reserve);
    let tabs: Vec<(Grouping, &str, String)> = Grouping::ALL
        .iter()
        .map(|grouping| {
            (
                *grouping,
                tab_icon(app, *grouping),
                app.tab_label(*grouping),
            )
        })
        .collect();
    let layout = lay_out_tabs(&tabs, active, room);
    // A single folder's branch goes after its name, when there's room.
    let branch = match (active, app.list.folder_filter()) {
        (Grouping::Folder, [only]) if filter.is_none() => {
            folder_branch(app, only).unwrap_or_default()
        }
        _ => String::new(),
    };
    let spare = room.saturating_sub(layout.width);
    let branch = if !branch.is_empty() && spare >= 8 {
        format!("  {}", truncate(&branch, spare - 2))
    } else {
        String::new()
    };

    let (mark, label, arrow, other) = if highlighted {
        (
            p.accent_primary,
            p.text_bright,
            p.accent_primary,
            p.neutral_gray,
        )
    } else {
        (p.neutral_gray, p.neutral_gray, p.text_muted, p.text_muted)
    };
    let mut spans = vec![Span::raw(" ".repeat(INDENT))];
    let mut x = INDENT;
    let mut underline = None;
    for (index, (grouping, icon, tab_label)) in tabs.iter().enumerate() {
        if index > 0 {
            spans.push(Span::raw(" ".repeat(layout.gap)));
            x += layout.gap;
        }
        let start = x;
        if *grouping == active {
            spans.push(Span::styled(*icon, Style::default().fg(mark)));
            spans.push(Span::raw(" "));
            spans.push(Span::styled(
                layout.active_label.clone(),
                Style::default().fg(label).add_modifier(Modifier::BOLD),
            ));
            spans.push(Span::styled(
                branch.clone(),
                Style::default().fg(p.text_muted),
            ));
            spans.push(Span::raw(" "));
            spans.push(Span::styled(
                if open { "▴" } else { "▾" },
                Style::default().fg(arrow),
            ));
            x += str_width(icon) + 1 + str_width(&layout.active_label) + str_width(&branch) + 2;
            underline = Some((start, x - start));
        } else {
            spans.push(Span::styled(*icon, Style::default().fg(other)));
            if !layout.compact {
                spans.push(Span::styled(
                    format!(" {tab_label}"),
                    Style::default().fg(other),
                ));
            }
            x += if layout.compact {
                str_width(icon)
            } else {
                str_width(icon) + 1 + str_width(tab_label)
            };
        }
        let target = Rect {
            x: area.x.saturating_add(start as u16),
            width: (x - start) as u16,
            ..area
        }
        .intersection(area);
        hits.push((target, Hit::ListTab(*grouping)));
    }
    f.render_widget(Paragraph::new(Line::from(spans)), line);
    if let Some(filter) = filter {
        let shown = truncate(&filter, reserve.saturating_sub(2).max(1));
        let width = str_width(&shown) as u16 + 1;
        f.render_widget(
            Paragraph::new(Span::styled(shown, Style::default().fg(p.accent_gold))),
            Rect {
                x: area.right().saturating_sub(width),
                width: width.min(area.width),
                ..line
            },
        );
    }
    // The active tab's underline along the band's bottom row.
    if let Some((start, width)) = underline.filter(|_| area.height >= 3) {
        let color = if highlighted {
            p.accent_primary
        } else {
            p.neutral_gray
        };
        f.render_widget(
            Paragraph::new(Span::styled("▁".repeat(width), Style::default().fg(color))),
            Rect {
                x: area.x.saturating_add(start as u16),
                y: area.bottom() - 1,
                width: width as u16,
                height: 1,
            }
            .intersection(area),
        );
    }
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
    // The tabs stay at the top; the rows below them scroll. A click on the
    // band beside the tabs opens the active tab's modal.
    let item = Rect {
        height: folder_item_height(area.height),
        ..area
    };
    draw_tabs(f, app, item, selected == 0, hits);
    hits.push((item, Hit::FolderSelector));
    let inner = Rect {
        y: item.bottom(),
        height: area.height - item.height,
        ..area
    };
    if inner.height == 0 {
        return;
    }

    let heights: Vec<u16> = rows.iter().map(|row| row_height_in(app, row)).collect();
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
            ListRow::WorktreeHeader(folder, root) => {
                let mut lines = vec![Line::default(); height as usize - 1];
                lines.push(worktree_header_line(
                    app,
                    folder,
                    root,
                    (inner.width as usize).saturating_sub(1),
                    is_selected,
                ));
                lines
            }
            ListRow::SectionHeader(section) => {
                let mut lines = vec![Line::default(); height as usize - 1];
                lines.push(section_header_line(
                    app,
                    section,
                    (inner.width as usize).saturating_sub(1),
                    is_selected,
                ));
                lines
            }
            ListRow::ReadPeriodHeader(period) => {
                let mut lines = vec![Line::default(); height as usize - 1];
                lines.push(read_period_header_line(
                    app,
                    *period,
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
                } else if app.hovered_row == Some(index) {
                    // Before the row's own target, which would take the tap.
                    if let Some(pin) = mark_hovered(&mut lines, session.pinned, row_area) {
                        hits.push((pin, Hit::PinToggle(index)));
                    }
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
                        p.text_unselected
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
    fn agent_badges_follow_the_right_aligned_time_without_a_box() {
        use crate::app::testing::{harness, view};
        use session_core::model::{AgentInfo, Lifecycle};
        let h = harness(vec![]);
        let mut session = view("A", "A very long 界 session title", 0);
        session.agent = Some(AgentInfo {
            kind: "claude".into(),
            ..Default::default()
        });
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
                            let (mark, style) = activity_badge_at(&session, now).unwrap();
                            let badge = first.last().unwrap();
                            assert_eq!(badge.content, mark);
                            assert_eq!(badge.style.fg, style.fg);
                            assert_eq!(badge.style.bg, None, "no box");
                            assert!(badge.style.add_modifier.contains(Modifier::BOLD));
                            assert_eq!(first[first.len() - 2].content, " ");
                            let age = &first[first.len() - 3];
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

    /// The tabs with `labels` and `marks` (given for Folders, App, Status),
    /// in the tabs' order.
    fn tabs(marks: [&'static str; 3], labels: [&str; 3]) -> Vec<(Grouping, &'static str, String)> {
        let given = [Grouping::Folder, Grouping::App, Grouping::Status];
        Grouping::ALL
            .iter()
            .map(|grouping| {
                let i = given.iter().position(|g| g == grouping).unwrap();
                (*grouping, marks[i], labels[i].to_string())
            })
            .collect()
    }

    #[test]
    fn tabs_narrow_their_gaps_then_show_marks_alone_then_cut_the_active_label() {
        let plain = ["▤", "◇", "◔"];
        let shown = tabs(plain, ["Folders", "App", "Status"]);
        let at = |room| lay_out_tabs(&shown, Grouping::App, room);
        // `◔ Status   ▤ Folders   ◇ App ▾`: 8 + 9 + 7, three apart.
        assert_eq!((at(40).gap, at(40).compact, at(40).width), (3, false, 30));
        assert_eq!((at(29).gap, at(29).compact, at(29).width), (2, false, 28));
        // `◔  ▤  ◇ App ▾`.
        assert_eq!((at(27).gap, at(27).compact, at(27).width), (2, true, 13));
        assert_eq!((at(12).gap, at(12).compact, at(12).width), (1, true, 11));
        // Then the active label is cut.
        assert_eq!(at(10).active_label, "A…");
        assert_eq!(at(10).width, 10);
        // Whatever the room, marks, labels and their width, the tabs fit
        // it once there's room for the marks and the arrow.
        let emoji = ["📁", "🤖", "🚦"];
        for marks in [plain, emoji] {
            for labels in [
                ["Folders", "App", "Status"],
                ["web", "App", "Status"],
                ["Folders", "App", "waiting for response"],
                ["12 folders", "opencode", "Status"],
            ] {
                let shown = tabs(marks, labels);
                for active in Grouping::ALL {
                    for room in 12..80 {
                        let layout = lay_out_tabs(&shown, active, room);
                        assert!(layout.width <= room, "{labels:?} {active:?} {room}");
                    }
                }
            }
        }
    }

    #[test]
    fn app_and_status_headers_fit_their_width_and_end_in_their_count() {
        use crate::app::grouping::{AppKey, StatusGroup};
        use crate::app::testing::{harness, view};
        let h = harness(vec![view("A", "a", 0)]);
        let sections = [
            Section::App(AppKey::new("opencode")),
            Section::Status(StatusGroup::WaitingForResponse),
        ];
        for section in &sections {
            for width in 24..100 {
                let line = section_header_line(&h.app, section, width, false);
                assert_eq!(line.width(), width, "{section:?} {width}");
                let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
                assert!(
                    text.ends_with(" live") || text.ends_with("sessions"),
                    "{text}"
                );
            }
        }
        let line = section_header_line(&h.app, &sections[1], 40, true);
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(text.starts_with(" "), "{text}");
        assert!(text.contains(" waiting for response ─"), "{text}");
        assert!(text.ends_with("─ no sessions"), "{text}");
        assert_eq!(line.spans[2].style.fg, Some(theme::palette().text_bright));
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
