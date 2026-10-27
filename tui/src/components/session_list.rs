//! Left pane (Sidebar layout): the tabs at the top (Status, Folders and App:
//! what the list is grouped and filtered by; the active one names its
//! filter, `▤ web ▾`, and Enter or a click on it opens its modal), then the
//! sessions shown, under a header row per folder (app, status) when there
//! are several, and "+ New session" last. Beside the session (Master
//! detail) its last row is `« Hide list`; hidden, a rail at the left edge
//! (`draw_list_rail`) shows it again.
//! Each session is a compact cell after a blank row: its name, then its age
//! and its mark (an agent's activity, else its status) at the right; then the
//! agent's latest output, when the daemon has one; then its program and
//! state; then the folder it works in now, one line (its path, its branch),
//! with `▸` when it has worked in other places, which open under it as
//! pairs of lines a step further in (`app::places`). A row's text
//! starts where its header's does, and the gutter left of it, where the
//! header's mark sits, holds the row's marks: the pin toggle under the
//! pointer, the summary's ↻, the places' `▸`/`▾` and the copy mark.
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
    activity_badge_at, branch_label, facts_line, place_label, read_period_style, relative_time,
    status_glyph, status_group_glyph, truncate, truncate_start, PIN_MARK,
};
use super::section_picker::{section_count, sessions_label};
use super::utils::get_spinner_char;
use crate::app::grouping::{Grouping, ReadPeriod, Section};
use crate::app::places::PlaceRef;
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
        | ListRow::ReadPeriodHeader(_)
        | ListRow::PinnedHeader => 2,
        ListRow::Session(_) => 3,
    }
}

/// [`row_height`], plus a line for a session's AI description and its
/// place lines (`width` columns of content).
fn row_height_in(app: &App, row: &ListRow, width: usize, now: i64) -> u16 {
    match row {
        ListRow::Session(session) => {
            let places = place_content(app, session, width, false, None, now)
                .lines
                .len();
            row_height(row)
                + u16::from(description_line(app, session).is_some())
                + u16::try_from(places).unwrap_or(u16::MAX)
        }
        row => row_height(row),
    }
}

/// The line under a session's name: its AI summary, or before one (signed
/// out, or with AI summaries off) your last message to its agent.
fn description_line<'a>(app: &'a App, session: &'a SessionView) -> Option<&'a str> {
    app.description(&session.id)
        .or(session.user_prompt.as_deref())
}

/// Rows of the tabs' band: its line with a row of padding above and below
/// (the active tab underlined on it), when the pane has room.
pub(super) fn folder_item_height(pane: u16) -> u16 {
    if pane >= 8 {
        3
    } else {
        1
    }
}

/// Columns left of the tabs and `« Hide list`, the column every top-level
/// header's and row's text starts in.
const INDENT: usize = 3;

/// Columns a row keeps left of its text for its marks (the pin toggle, the
/// summary's ↻, the places' `▸`/`▾`, the copy mark: one and a gap), where
/// its header's mark sits.
const GUTTER: usize = 2;

/// How much further in a row under a worktree's header starts, as that
/// header's text does.
const NEST: usize = 2;

/// Columns left of a row's gutter: the drag mark's, in its last one. A row
/// under a worktree's header is `nested`.
fn row_indent(nested: bool) -> usize {
    INDENT - GUTTER + if nested { NEST } else { 0 }
}

/// For each of `rows`, whether it's under a worktree's header inside its
/// folder. Ready's read periods are in line with ready's header, and so
/// are their rows.
fn nesting(rows: &[ListRow]) -> Vec<bool> {
    let mut nested = false;
    rows.iter()
        .map(|row| {
            match row {
                ListRow::WorktreeHeader(..) => nested = true,
                ListRow::Folders
                | ListRow::FolderHeader(_)
                | ListRow::SectionHeader(_)
                | ListRow::ReadPeriodHeader(_)
                | ListRow::PinnedHeader
                | ListRow::NewSession => nested = false,
                ListRow::Session(_) => return nested,
            }
            false
        })
        .collect()
}

/// A session's right column: its age (at most four columns, `52w`) and its
/// mark, which the lines under its name stay clear of, with a few columns'
/// gap so a long line doesn't run up against them.
const RIGHT_COLUMN: usize = 10;

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

/// `area`, in a row drawn `skip` rows on in `shown` (a row taller than the
/// list), cut to what shows; `None` when none of it does.
fn scrolled(area: Rect, skip: u16, shown: Rect) -> Option<Rect> {
    let top = (i32::from(area.y) - i32::from(skip)).max(i32::from(shown.y));
    let bottom = (i32::from(area.bottom()) - i32::from(skip)).min(i32::from(shown.bottom()));
    (top < bottom).then(|| Rect {
        y: top as u16,
        height: (bottom - top) as u16,
        ..area
    })
}

/// `content` at the bottom of a cell of `height` rows (the blank rows
/// above it separate it from the previous cell), `indent` columns in.
fn cell_lines(content: Vec<Vec<Span<'_>>>, height: u16, indent: usize) -> Vec<Line<'_>> {
    let mut lines = vec![Line::default(); (height as usize).saturating_sub(content.len())];
    for spans in content {
        let mut line = vec![Span::raw(" ".repeat(indent))];
        line.extend(spans);
        lines.push(Line::from(line));
    }
    lines
}

/// Shown in the gutter of a hovered unpinned session's first line: a tap
/// pins it.
const PIN_TOGGLE: &str = "☆";

/// The hovered session's pin toggle, in the gutter just left of its name
/// (`indent` columns in), the column of its header's mark. `☆` to pin it,
/// the gold `★` to unpin it. Returns where it is: the mark and the space
/// after it.
fn mark_hovered(lines: &mut [Line<'_>], pinned: bool, area: Rect, indent: usize) -> Option<Rect> {
    let p = theme::palette();
    let first = lines.iter().position(|line| !line.spans.is_empty())?;
    let (mark, style) = if pinned {
        (PIN_MARK, Style::default().fg(p.accent_gold))
    } else {
        (PIN_TOGGLE, Style::default().fg(p.text_muted))
    };
    // After the indent, the gutter's mark (see `session_content`).
    let slot = lines[first].spans.get_mut(1)?;
    *slot = Span::styled(mark, style);
    let x = area.x + u16::try_from(indent).ok()?;
    Some(Rect {
        x,
        y: area.y + u16::try_from(first).ok()?,
        width: 2.min(area.right().saturating_sub(x)),
        height: 1,
    })
}

/// The session being dragged: a bar in the last column of the `indent`
/// of its content lines, just left of its gutter.
fn mark_dragged(lines: &mut [Line<'_>], indent: usize) {
    let bar = Span::styled(
        format!("{}{DRAG_MARK}", " ".repeat(indent.saturating_sub(1))),
        Style::default().fg(theme::palette().accent_primary),
    );
    for line in lines.iter_mut().filter(|l| !l.spans.is_empty()) {
        line.spans[0] = bar.clone();
    }
}

/// A folder header: name, branch, a rule, then `live/total`, in `width`
/// columns (the branch goes first when there's no room). Its name starts
/// where its sessions' text does, a status's after its mark. Selected, the
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
    // "   name  branch ─── count": the indent, two spaces, and at least two
    // rule columns.
    let fixed = INDENT + 2 + str_width(&count);
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
        Span::raw(" ".repeat(INDENT)),
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

/// The header of one of ready's sub-sections, in line with ready's: a
/// clock where ready's mark is and when its sessions were last read where
/// its name is (a pin for the pinned ones), a rule, then how many it has,
/// in `width` columns.
pub fn read_period_header_line(
    app: &App,
    period: ReadPeriod,
    width: usize,
    selected: bool,
) -> Line<'static> {
    let p = theme::palette();
    let count = sessions_label(app.list.read_period_count(period));
    // " ◷ label ─── count": as a status's header.
    let clock = match period {
        ReadPeriod::Pinned => format!("{PIN_MARK} "),
        _ => format!("{} ", app.icons().clock()),
    };
    let fixed = 3 + str_width(&clock) + str_width(&count);
    let label = truncate(period.label(), width.saturating_sub(fixed + 2));
    let rule = width.saturating_sub(fixed + str_width(&label));
    let (label_color, rule_color) = if selected {
        (p.text_bright, p.accent_primary)
    } else {
        (p.label_blue, p.border_dim)
    };
    Line::from(vec![
        Span::raw(" "),
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
/// after its mark, an app's where that mark would be), a rule, then how
/// many sessions it has (`2/3 live` for an app, `3 sessions` for a status),
/// in `width` columns. Selected, the name is white and the rule blue.
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
        // Its name where a status's is, in line with its sessions' text.
        Section::App(_) => (" ".repeat(GUTTER), Style::default()),
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

/// The pinned section's header, drawn like an app's or a status's: the
/// gold pin, `pinned`, a rule, then how many it shows, in `width` columns.
pub fn pinned_header_line(app: &App, width: usize, selected: bool) -> Line<'static> {
    let p = theme::palette();
    let count = sessions_label(app.list.pinned_count());
    let mark = format!("{PIN_MARK} ");
    // " ★ pinned ─── count": three spaces, and at least two rule columns.
    let fixed = 3 + str_width(&mark) + str_width(&count);
    let name = truncate("pinned", width.saturating_sub(fixed + 2));
    let rule = width.saturating_sub(fixed + str_width(&name));
    let (name_color, rule_color) = if selected {
        (p.text_bright, p.accent_primary)
    } else {
        (p.accent_purple, p.border_dim)
    };
    Line::from(vec![
        Span::raw(" "),
        Span::styled(mark, Style::default().fg(p.accent_gold)),
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

/// The content lines of a session cell: two, and its AI description, each
/// after its gutter (two columns).
fn session_content(
    app: &App,
    session: &SessionView,
    width: usize,
    selected: bool,
    now: i64,
) -> Vec<Vec<Span<'static>>> {
    let p = theme::palette();
    // On the right after the age: an agent's activity, else the status (a
    // program like nvim). The slot left of the name is the hover pin's.
    let badge = activity_badge_at(session, now)
        .map(|(mark, style)| Span::styled(mark, style.add_modifier(Modifier::BOLD)))
        .or_else(|| {
            let (glyph, style) = status_glyph(session);
            (!glyph.trim().is_empty()).then(|| Span::styled(glyph, style))
        });
    let (glyph, glyph_style) = (" ", Style::default());
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
    // The lines under the name end where the right column starts.
    let column = width.saturating_sub(2 + RIGHT_COLUMN);
    let mut second = vec![Span::raw("  ")];
    // The branch is on the place line (`place_content`).
    second.extend(
        facts_line(
            session,
            column,
            app.icons(),
            Style::default().fg(detail_color),
            false,
        )
        .spans,
    );
    let mut lines = vec![first];
    // Under the name: its summary, or your last message.
    if let Some(output) = description_line(app, session) {
        // A terminal has one font size: italic and a shade quieter than the
        // details make it read as the smaller print.
        let quiet = if selected {
            p.neutral_gray
        } else {
            p.text_muted
        };
        lines.push(vec![
            Span::raw("  "),
            Span::styled(
                truncate(output, column),
                Style::default().fg(quiet).add_modifier(Modifier::ITALIC),
            ),
        ]);
    }
    lines.push(second);
    lines
}

/// In the gutter of the place a session works now when it has worked
/// elsewhere too: `▸` while its other places are closed, `▾` while they're
/// open.
const PLACES_CLOSED: &str = "▸";
const PLACES_OPEN: &str = "▾";
/// In the gutter of a place under the pointer or the list keys (just left
/// of the current place's `▸`): a click or tap (`y`) copies its path.
const COPY_MARK: &str = "⧉";
/// How much further in than the row's text (where the current place is)
/// its other places and the rules between them start.
const PLACE_INDENT: usize = 2;
/// Between the current place's path and its branch.
const PLACE_GAP: &str = "  ";
/// The fewest columns the current place's branch is cut to (`⎇ fea…`);
/// with less room, it goes.
const BRANCH_MIN: usize = 6;
/// Between two places, under their text.
const PLACE_RULE: &str = "─";

/// A session's place lines.
struct PlaceLines {
    lines: Vec<Vec<Span<'static>>>,
    /// Where its `▸`/`▾` is (its columns from the gutter's start, on the
    /// first line), which opens and closes its other places; none when there
    /// are none.
    toggle: Option<std::ops::Range<usize>>,
    /// The copy mark goes just left of the `▸`/`▾`, in the column before the
    /// gutter: the current place is under the pointer.
    copy_beside_toggle: bool,
    /// The line of the place it works in now.
    current: std::ops::Range<usize>,
    /// Each open place's two lines, in the order of the session's
    /// [`crate::app::places::listed_places`].
    places: Vec<std::ops::Range<usize>>,
    /// The open place the list keys are on (its index in `places`).
    focus: Option<usize>,
}

/// The current place's `path` and `branch` on one line of `room` columns:
/// the path is cut at its start first, down to half the line, then the
/// branch at its end; a branch it would cut to fewer than [`BRANCH_MIN`]
/// columns goes, and the path takes the line.
fn fit_place(path: &str, branch: &str, room: usize) -> (String, String) {
    let (path_width, branch_width) = (str_width(path), str_width(branch));
    let gap = str_width(PLACE_GAP);
    if branch.is_empty() {
        return (truncate_start(path, room), String::new());
    }
    if path_width + gap + branch_width <= room {
        return (path.to_string(), branch.to_string());
    }
    let path_room = room
        .saturating_sub(gap + branch_width)
        .max(path_width.min(room / 2));
    let cut = truncate_start(path, path_room);
    let branch_room = room.saturating_sub(str_width(&cut) + gap);
    if branch_room < BRANCH_MIN.min(branch_width) {
        return (truncate_start(path, room), String::new());
    }
    (cut, truncate(branch, branch_room))
}

/// A session's place lines, under its facts, after the gutter. First the
/// place it works in now, one line in the row's text column: its path, then
/// its branch (`▸` in the gutter when it has worked in other places). While
/// its places are open (`▾`), each of the others (where it changed files:
/// every place is), most recent first, a pair of lines [`PLACE_INDENT`]
/// further in: its path, then its branch and when; a rule above each. No
/// place has a mark or bar left of it. The place the list keys are on (in
/// the `selected` session) and the `hovered` one show the copy mark in their
/// gutter (the current one's left of its `▸`) and their path brighter; the
/// list keys' path is bold.
fn place_content(
    app: &App,
    session: &SessionView,
    width: usize,
    selected: bool,
    hovered: Option<PlaceRef>,
    now: i64,
) -> PlaceLines {
    let p = theme::palette();
    let icons = app.icons();
    // The current place's room, after the gutter, and the others', a step
    // further in.
    let text = width.saturating_sub(GUTTER);
    let inner = text.saturating_sub(PLACE_INDENT);
    let (text_color, quiet) = if selected {
        (p.text_primary, p.neutral_gray)
    } else {
        (p.text_muted, p.text_muted)
    };
    let label_style = Style::default().fg(text_color);
    let branch_style = Style::default().fg(quiet);
    // A place's path under the pointer or the list keys.
    let marked_style = Style::default().fg(p.text_bright);
    let blank_slot = || Span::raw(" ".repeat(GUTTER));
    // The others' step in.
    let indent = || Span::raw(" ".repeat(PLACE_INDENT));
    let copy_slot = || {
        Span::styled(
            format!("{COPY_MARK:<GUTTER$}"),
            Style::default().fg(p.accent_primary),
        )
    };
    let branch_of = |git: Option<&session_core::model::GitInfo>| {
        git.and_then(|git| branch_label(git, icons))
            .unwrap_or_default()
    };

    // Where it works now, with the toggle when there are others.
    let current = crate::app::places::current_place(session);
    let listed: Vec<_> = crate::app::places::listed_places(session).collect();
    let others = listed.len();
    let open = app.places_open(&session.id) && others > 0;
    let hovered_current = hovered == Some(PlaceRef::Current);
    let slot = if others > 0 {
        let mark = if open { PLACES_OPEN } else { PLACES_CLOSED };
        Span::styled(
            format!("{mark:<GUTTER$}"),
            Style::default().fg(p.accent_primary),
        )
    } else if hovered_current {
        copy_slot()
    } else {
        blank_slot()
    };
    let (path, branch) = fit_place(
        &place_label(&session.scope, current),
        &branch_of(session.git.as_ref()),
        text,
    );
    let path_style = if hovered_current {
        marked_style
    } else {
        label_style
    };
    let mut line = vec![slot, Span::styled(path, path_style)];
    if !branch.is_empty() {
        line.push(Span::raw(PLACE_GAP));
        line.push(Span::styled(branch, branch_style));
    }
    let mut lines = vec![line];
    let toggle = (others > 0).then_some(0..GUTTER);
    let copy_beside_toggle = others > 0 && hovered_current;
    let mut places = Vec::new();
    if !open {
        return PlaceLines {
            lines,
            toggle,
            copy_beside_toggle,
            current: 0..1,
            places,
            focus: None,
        };
    }

    // Under it, the others.
    let focus = selected
        .then(|| app.list.selected_place())
        .flatten()
        .and_then(|focus| listed.iter().position(|p| p.path == focus));
    for (index, place) in listed.into_iter().enumerate() {
        lines.push(vec![
            Span::raw(" ".repeat(GUTTER + PLACE_INDENT)),
            Span::styled(PLACE_RULE.repeat(inner), Style::default().fg(p.border_dim)),
        ]);
        let start = lines.len();
        let marked = focus == Some(index) || hovered == Some(PlaceRef::Listed(index));
        let path_style = if focus == Some(index) {
            marked_style.add_modifier(Modifier::BOLD)
        } else if marked {
            marked_style
        } else {
            label_style
        };
        let label = place_label(&session.scope, &place.path);
        lines.push(vec![
            if marked { copy_slot() } else { blank_slot() },
            indent(),
            Span::styled(truncate_start(&label, inner), path_style),
        ]);
        // Under it its branch, and at the right its age; short of room the
        // branch is cut first, then the age.
        let age = truncate(&relative_time(now, place.last_at), inner);
        let branch = truncate(
            &branch_of(place.git.as_ref()),
            inner.saturating_sub(str_width(&age) + 1),
        );
        let gap = inner.saturating_sub(str_width(&branch) + str_width(&age));
        lines.push(vec![
            blank_slot(),
            indent(),
            Span::styled(branch, branch_style),
            Span::raw(" ".repeat(gap)),
            Span::styled(age, branch_style),
        ]);
        places.push(start..lines.len());
    }
    PlaceLines {
        lines,
        toggle,
        copy_beside_toggle,
        current: 0..1,
        places,
        focus,
    }
}

/// Shown before a hovered session's AI description: a click or tap makes a
/// new summary.
const SUMMARY_REFRESH: &str = "↻";

/// The start of a session's description line (`line`), in its gutter
/// (`indent` columns in), the two columns before its text: a spinner while a
/// new summary is being made, else the ↻ when `hovered`. Returns where the ↻
/// is.
fn mark_summary(
    lines: &mut [Line<'_>],
    line: usize,
    refreshing: bool,
    hovered: bool,
    area: Rect,
    indent: usize,
) -> Option<Rect> {
    let p = theme::palette();
    let (mark, style) = if refreshing {
        (get_spinner_char(), Style::default().fg(p.accent_primary))
    } else if hovered {
        (SUMMARY_REFRESH, Style::default().fg(p.accent_primary))
    } else {
        return None;
    };
    // After the indent, the gutter (see `session_content`).
    let slot = lines.get_mut(line)?.spans.get_mut(1)?;
    *slot = Span::styled(format!("{mark} "), style);
    if refreshing {
        return None;
    }
    let x = area.x + u16::try_from(indent).ok()?;
    Some(Rect {
        x,
        y: area.y + u16::try_from(line).ok()?,
        width: 2.min(area.right().saturating_sub(x)),
        height: 1,
    })
}

/// Columns between two tabs when there's room for them (fewer otherwise),
/// after the two every tab keeps for the active one's ` ▾` (while it fits).
const TAB_GAP: usize = 2;

/// Columns every tab keeps after its label for the ` ▾` the active one ends
/// in, so switching tabs moves nothing.
const ARROW_SLOT: usize = 2;

/// Columns the text filter keeps for itself at the right end of the band.
const FILTER_MAX: usize = 12;

/// How the tabs fit their room.
#[derive(Debug, Clone, PartialEq, Eq)]
struct TabLayout {
    /// Columns between two tabs.
    gap: usize,
    /// The active tab ends in ` ▾` and every tab with its name keeps room
    /// for it; without it (short of room) no tab does, so which one is
    /// active still moves nothing.
    arrows: bool,
    /// The inactive tabs show their mark alone.
    compact: bool,
    /// The active tab's label, cut when nothing else made room.
    active_label: String,
    /// Columns the tabs take.
    width: usize,
}

/// Lay the tabs (`(tab, mark, label)`) out in `room` columns: full labels
/// two columns apart, else one apart, else without the ` ▾` (two apart,
/// then one), else every inactive tab as its mark alone, else the active
/// tab's label cut. The active tab ends in ` ▾`, and every full-label tab
/// keeps room for it (or none does), so which tab is active never moves
/// the others.
fn lay_out_tabs(tabs: &[(Grouping, &str, String)], active: Grouping, room: usize) -> TabLayout {
    let width = |gap: usize, arrows: bool, compact: bool, active_label: usize| -> usize {
        let arrow = if arrows { ARROW_SLOT } else { 0 };
        let tabs_width: usize = tabs
            .iter()
            .map(|(tab, icon, label)| match (*tab == active, compact) {
                (true, _) => str_width(icon) + 1 + active_label + arrow,
                (false, true) => str_width(icon),
                (false, false) => str_width(icon) + 1 + str_width(label) + arrow,
            })
            .sum();
        tabs_width + gap * tabs.len().saturating_sub(1)
    };
    let active_label = tabs
        .iter()
        .find(|(tab, _, _)| *tab == active)
        .map(|(_, _, label)| label.clone())
        .unwrap_or_default();
    let label_width = str_width(&active_label);
    for (gap, arrows, compact) in [
        (TAB_GAP, true, false),
        (1, true, false),
        (TAB_GAP, false, false),
        (1, false, false),
        (TAB_GAP, true, true),
        (1, true, true),
    ] {
        let used = width(gap, arrows, compact, label_width);
        if used <= room {
            return TabLayout {
                gap,
                arrows,
                compact,
                active_label,
                width: used,
            };
        }
    }
    let active_label = truncate(&active_label, room.saturating_sub(width(1, true, true, 0)));
    TabLayout {
        gap: 1,
        arrows: true,
        compact: true,
        width: width(1, true, true, str_width(&active_label)),
        active_label,
    }
}

/// The tabs on a raised band: Status, Folders, App. The active one is bold,
/// names its filter (`▤ web  ⎇ main`, `◇ claude`), ends in `▾` (it opens
/// its modal) and is underlined; the text filter shows at the right end
/// while there is one. White and blue when selected, muted otherwise.
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
        .map(|tab| (*tab, tab_icon(app, *tab), app.tab_label(*tab)))
        .collect();
    let mut layout = lay_out_tabs(&tabs, active, room);
    // A single folder's branch goes after its name on the Folders tab,
    // active or not (so switching tabs moves nothing), when there's room:
    // the gaps narrow for it, but the tabs keep their names and the `▾`.
    let branch = match app.list.folder_filter() {
        [only] if filter.is_none() && !layout.compact => {
            folder_branch(app, only).unwrap_or_default()
        }
        _ => String::new(),
    };
    if !branch.is_empty() && room.saturating_sub(layout.width) < 8 {
        let narrowed = lay_out_tabs(&tabs, active, room.saturating_sub(8));
        if !narrowed.compact && narrowed.arrows == layout.arrows {
            layout = narrowed;
        }
    }
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
    let arrow_slot = if layout.arrows { ARROW_SLOT } else { 0 };
    let mut spans = vec![Span::raw(" ".repeat(INDENT))];
    let mut x = INDENT;
    let mut underline = None;
    for (index, (tab, icon, tab_label)) in tabs.iter().enumerate() {
        if index > 0 {
            spans.push(Span::raw(" ".repeat(layout.gap)));
            x += layout.gap;
        }
        let start = x;
        if *tab == active {
            spans.push(Span::styled(*icon, Style::default().fg(mark)));
            spans.push(Span::raw(" "));
            spans.push(Span::styled(
                layout.active_label.clone(),
                Style::default().fg(label).add_modifier(Modifier::BOLD),
            ));
            let branch = if *tab == Grouping::Folder {
                branch.as_str()
            } else {
                ""
            };
            spans.push(Span::styled(
                branch.to_string(),
                Style::default().fg(p.text_muted),
            ));
            if layout.arrows {
                spans.push(Span::raw(" "));
                spans.push(Span::styled(
                    if open { "▴" } else { "▾" },
                    Style::default().fg(arrow),
                ));
            }
            x += str_width(icon)
                + 1
                + str_width(&layout.active_label)
                + str_width(branch)
                + arrow_slot;
            underline = Some((start, x - start));
        } else {
            spans.push(Span::styled(*icon, Style::default().fg(other)));
            if !layout.compact {
                let branch = if *tab == Grouping::Folder {
                    branch.as_str()
                } else {
                    ""
                };
                // The blank where the active tab's ` ▾` goes.
                spans.push(Span::styled(
                    format!(" {tab_label}{branch}{}", " ".repeat(arrow_slot)),
                    Style::default().fg(other),
                ));
            }
            x += if layout.compact {
                str_width(icon)
            } else {
                let branch = if *tab == Grouping::Folder {
                    str_width(&branch)
                } else {
                    0
                };
                str_width(icon) + 1 + str_width(tab_label) + branch + arrow_slot
            };
        }
        let target = Rect {
            x: area.x.saturating_add(start as u16),
            width: (x - start) as u16,
            ..area
        }
        .intersection(area);
        hits.push((target, Hit::ListTab(*tab)));
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

/// The list pane's last row beside the session: a click hides the list.
fn draw_hide_list(f: &mut Frame, area: Rect, hits: &mut Vec<(Rect, Hit)>) {
    let p = theme::palette();
    let quiet = Style::default().fg(p.neutral_gray);
    let line = Line::from(vec![
        Span::raw(" ".repeat(INDENT)),
        Span::styled("« Hide list", quiet),
    ]);
    f.render_widget(
        Paragraph::new(line).style(Style::default().bg(p.surface)),
        area,
    );
    hits.push((area, Hit::HideList));
}

/// What's left of a hidden list (Master detail): a column of the list pane
/// (its tabs' band at the top, its last row with `»` where `« Hide list`
/// was), then the divider's line. A click anywhere on it shows the list.
pub fn draw_list_rail(f: &mut Frame, area: Rect, hits: &mut Vec<(Rect, Hit)>) {
    let p = theme::palette();
    if area.width == 0 || area.height == 0 {
        return;
    }
    let pane = Rect { width: 1, ..area };
    f.render_widget(Block::default().style(Style::default().bg(p.panel)), pane);
    f.render_widget(
        Block::default().style(Style::default().bg(p.surface)),
        Rect {
            height: folder_item_height(area.height),
            ..pane
        },
    );
    f.render_widget(
        Paragraph::new(Span::styled("»", Style::default().fg(p.neutral_gray)))
            .style(Style::default().bg(p.surface)),
        Rect {
            y: area.bottom() - 1,
            height: 1,
            ..pane
        },
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
    let mut inner = Rect {
        y: item.bottom(),
        height: area.height - item.height,
        ..area
    };
    // Beside the session the pane's last row hides it.
    if inner.height >= 2 && app.list_beside_session() {
        inner.height -= 1;
        draw_hide_list(
            f,
            Rect {
                y: inner.bottom(),
                height: 1,
                ..inner
            },
            hits,
        );
    }
    if inner.height == 0 {
        return;
    }

    // A row's content width: its gutter and text, right of its indent, less
    // a right margin.
    let nested = nesting(&rows);
    let width_of = |nested: bool| (inner.width as usize).saturating_sub(row_indent(nested) + 1);
    // The list's clock (set every tick), which ready's sub-sections are
    // measured from too.
    let now = app.list.now();
    let heights: Vec<u16> = rows
        .iter()
        .zip(&nested)
        .map(|(row, nested)| row_height_in(app, row, width_of(*nested), now))
        .collect();
    let first = first_visible_row(&heights, selected.max(1), inner.height).max(1);
    let mut y = inner.y;
    let bottom = inner.y + inner.height;

    for (index, row) in rows.iter().enumerate().skip(first) {
        let height = heights[index];
        let is_selected = index == selected;
        // A selected session taller than the list (its places open) shows
        // as much of it as fits, scrolled to where the list keys are.
        if y + height > bottom && !(is_selected && y == inner.y) {
            break;
        }
        let row_area = Rect {
            x: inner.x,
            y,
            width: inner.width,
            height: height.min(bottom - y),
        };
        let mut skip = 0;
        let mut row_hits = Vec::new();
        let lines = match row {
            ListRow::Folders => Vec::new(),
            ListRow::NewSession => {
                // The `+` in the gutter, its words where the sessions' names
                // start.
                let content = vec![vec![
                    Span::styled(
                        format!("{:<GUTTER$}", "+"),
                        Style::default()
                            .fg(p.accent_green)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        truncate("New session", width_of(false).saturating_sub(GUTTER)),
                        Style::default()
                            .fg(if is_selected {
                                p.text_bright
                            } else {
                                p.accent_green
                            })
                            .add_modifier(Modifier::BOLD),
                    ),
                ]];
                cell_lines(content, height, row_indent(false))
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
            ListRow::PinnedHeader => {
                let mut lines = vec![Line::default(); height as usize - 1];
                lines.push(pinned_header_line(
                    app,
                    (inner.width as usize).saturating_sub(1),
                    is_selected,
                ));
                lines
            }
            ListRow::Session(session) => {
                // Its text in its header's text column, its marks in the
                // gutter before it.
                let indent = row_indent(nested[index]);
                let width = width_of(nested[index]);
                let mut content = session_content(app, session, width, is_selected, now);
                let hovered = app.hovered_row == Some(index);
                let dragged = app.dragged_session() == Some(&session.id);
                let hovered_place = app
                    .hovered_place
                    .filter(|(row, _)| *row == index && !dragged)
                    .map(|(_, place)| place);
                let places = place_content(app, session, width, is_selected, hovered_place, now);
                let places_at = content.len();
                content.extend(places.lines);
                // The cell's content is at its bottom, after blank rows.
                let top = (height as usize).saturating_sub(content.len());
                let mut lines = cell_lines(content, height, indent);
                // The current place under the pointer: its copy mark just
                // left of its ▸, in the indent's last column.
                if places.copy_beside_toggle {
                    if let Some(line) = lines.get_mut(top + places_at + places.current.start) {
                        line.spans[0] = Span::styled(
                            format!("{}{COPY_MARK}", " ".repeat(indent.saturating_sub(1))),
                            Style::default().fg(p.accent_primary),
                        );
                    }
                }
                // The whole row, of which `row_area` shows `skip` rows on.
                let full = Rect { height, ..row_area };
                // A place's lines, from the column left of its gutter (where
                // the current one's copy mark can be).
                let from = indent.saturating_sub(1) as u16;
                let line_area = |line: usize, count: usize| {
                    Rect {
                        x: full.x + from,
                        y: full.y.saturating_add(line as u16),
                        width: full.width.saturating_sub(from),
                        height: count as u16,
                    }
                    .intersection(full)
                };
                if height > row_area.height {
                    let focus = places.focus.and_then(|at| places.places.get(at));
                    let end = top + places_at + focus.unwrap_or(&places.current).end;
                    skip = (end as u16).saturating_sub(row_area.height);
                }
                // The description is the line under the name; with AI
                // summaries on, its ↻ makes a new one.
                if app.summaries_on() && description_line(app, session).is_some() {
                    let refreshing = app.summary_refreshing(&session.id);
                    let line = top + 1;
                    if let Some(refresh) = mark_summary(
                        &mut lines,
                        line,
                        refreshing,
                        hovered && !dragged,
                        full,
                        indent,
                    ) {
                        row_hits.push((refresh, Hit::SummaryRefresh(index)));
                    }
                }
                if !dragged {
                    // The current place's ▸ opens and closes the places
                    // under it, before its own target.
                    if let Some(columns) = &places.toggle {
                        let area = Rect {
                            x: full.x + (indent + columns.start) as u16,
                            y: full.y.saturating_add((top + places_at) as u16),
                            width: columns.len() as u16,
                            height: 1,
                        }
                        .intersection(full);
                        row_hits.push((area, Hit::PlacesToggle(index)));
                    }
                    // A click on a place copies its path.
                    let current = &places.current;
                    let area = line_area(top + places_at + current.start, current.len());
                    row_hits.push((area, Hit::Place(index, PlaceRef::Current)));
                    for (place, range) in places.places.iter().enumerate() {
                        let area = line_area(top + places_at + range.start, range.len());
                        row_hits.push((area, Hit::Place(index, PlaceRef::Listed(place))));
                    }
                }
                if dragged {
                    mark_dragged(&mut lines, indent);
                } else if hovered {
                    // Before the row's own target, which would take the tap.
                    if let Some(pin) = mark_hovered(&mut lines, session.pinned, full, indent) {
                        row_hits.push((pin, Hit::PinToggle(index)));
                    }
                }
                lines
            }
        };
        f.render_widget(Paragraph::new(lines).scroll((skip, 0)), row_area);
        hits.extend(
            row_hits
                .into_iter()
                .filter_map(|(area, hit)| Some((scrolled(area, skip, row_area)?, hit))),
        );
        hits.push((row_area, Hit::ListRow(index)));
        y += row_area.height;
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
            // The branch is on the place line, in the details' colour.
            let facts: String = second.iter().map(|span| span.content.as_ref()).collect();
            assert_eq!(facts, "  opencode");
            let places = place_content(&h.app, &session, 40, selected, None, 0).lines;
            let branch = places[0].last().unwrap();
            assert_eq!(branch.content, "⎇ main");
            assert_eq!(
                branch.style.fg,
                Some(if selected {
                    p.neutral_gray
                } else {
                    p.text_muted
                })
            );
        }
    }

    /// `session` with `places` (paths in its folder `/repo`, a branch each,
    /// how long ago), working in `/repo` on `main` now.
    fn with_places(places: &[(&str, &str, i64, bool)]) -> SessionView {
        let mut session = crate::app::testing::view("A", "a", 0);
        session.git = crate::app::testing::on_branch("main");
        session.places = places
            .iter()
            .map(
                |(path, branch, ago, edited)| session_core::model::SessionPlace {
                    path: path.into(),
                    git: Some(session_core::model::GitInfo {
                        repo_root: path.into(),
                        branch: Some(branch.to_string()),
                        ..Default::default()
                    }),
                    last_at: 1000 - ago,
                    edited: *edited,
                },
            )
            .collect();
        session
    }

    fn text(lines: &[Vec<Span>]) -> Vec<String> {
        lines
            .iter()
            .map(|line| line.iter().map(|span| span.content.as_ref()).collect())
            .collect()
    }

    #[test]
    fn where_a_session_works_now_is_one_line_in_the_rows_text_column_with_its_toggle() {
        use crate::app::testing::harness;
        let p = theme::palette();
        let mut h = harness(vec![]);
        // Nowhere else: its path and branch on one line, no toggle in the
        // gutter.
        let alone = with_places(&[("/repo", "main", 5, false)]);
        let PlaceLines {
            lines,
            toggle,
            copy_beside_toggle,
            current,
            places,
            ..
        } = place_content(&h.app, &alone, 40, false, None, 1000);
        assert_eq!(text(&lines), ["  repo  ⎇ main"]);
        assert_eq!(toggle, None);
        assert!(!copy_beside_toggle);
        assert_eq!(current, 0..1);
        assert!(places.is_empty());
        // Nothing left of its path but the gutter.
        assert_eq!(lines[0][1].content, "repo");
        assert_eq!(lines[0][1].style.fg, Some(p.text_muted));
        // Under the pointer, the copy mark in the gutter and its path
        // brighter.
        let PlaceLines {
            lines,
            copy_beside_toggle,
            ..
        } = place_content(&h.app, &alone, 40, false, Some(PlaceRef::Current), 1000);
        assert_eq!(text(&lines), ["⧉ repo  ⎇ main"]);
        assert!(!copy_beside_toggle);
        assert_eq!(lines[0][0].style.fg, Some(p.accent_primary));
        assert_eq!(lines[0][1].style.fg, Some(p.text_bright));
        // Opened, it stays alone: there's nothing else to list.
        h.app.places_open.insert(alone.id.clone());
        let lines = place_content(&h.app, &alone, 40, false, None, 1000).lines;
        assert_eq!(text(&lines), ["  repo  ⎇ main"]);
        h.app.places_open.clear();

        // Elsewhere too: ▸ in the gutter, which opens and closes the list;
        // under the pointer, the copy mark goes left of it.
        let mut moved = with_places(&[
            ("/repo/worktrees/fix", "fix", 5, true),
            ("/repo", "main", 300, false),
            ("/srv/other", "dev", 7200, false),
        ]);
        moved.git = Some(session_core::model::GitInfo {
            repo_root: "/repo/worktrees/fix".into(),
            branch: Some("fix".into()),
            ..Default::default()
        });
        let PlaceLines {
            lines,
            toggle,
            copy_beside_toggle,
            ..
        } = place_content(&h.app, &moved, 40, false, None, 1000);
        assert_eq!(text(&lines), ["▸ worktrees/fix  ⎇ fix"]);
        assert_eq!(toggle, Some(0..2));
        assert!(!copy_beside_toggle);
        assert_eq!(lines[0][0].style.fg, Some(p.accent_primary));
        let PlaceLines {
            lines,
            copy_beside_toggle,
            ..
        } = place_content(&h.app, &moved, 40, false, Some(PlaceRef::Current), 1000);
        assert_eq!(text(&lines), ["▸ worktrees/fix  ⎇ fix"]);
        assert!(copy_beside_toggle);
        // Narrow: the path is cut at its start first, then the branch goes.
        let lines = place_content(&h.app, &moved, 20, false, None, 1000).lines;
        assert_eq!(text(&lines), ["▸ …ktrees/fix  ⎇ fix"]);
        let lines = place_content(&h.app, &moved, 14, false, None, 1000).lines;
        assert_eq!(text(&lines), ["▸ …rktrees/fix"]);
        // Working somewhere none of its places is (its terminal's
        // directory, outside a repository): every place is another.
        let mut away = moved.clone();
        away.git = None;
        away.cwd = "/srv/scratch".into();
        let lines = place_content(&h.app, &away, 40, false, None, 1000).lines;
        assert_eq!(text(&lines), ["▸ /srv/scratch"]);
        for width in [8, 12, 16, 24, 40, 80] {
            let lines = place_content(&h.app, &moved, width, true, None, 1000).lines;
            assert_eq!(lines.len(), 1, "{width}");
            for line in &lines {
                assert!(
                    Line::from(line.clone()).width() <= width.max(6),
                    "{width}: {:?}",
                    text(&lines)
                );
            }
        }
        assert_eq!(
            row_height_in(&h.app, &ListRow::Session(&moved), 40, 1000),
            4
        );
    }

    #[test]
    fn the_current_places_path_is_cut_before_its_branch_and_never_wraps() {
        let fit = |path: &str, branch: &str, room| {
            let (path, branch) = fit_place(path, branch, room);
            assert!(
                str_width(&path) + str_width(&branch) + if branch.is_empty() { 0 } else { 2 }
                    <= room,
                "{room}: {path:?} {branch:?}"
            );
            (path, branch)
        };
        let path = "/work/focal-md-pubmed-pmc-audit";
        let branch = "⎇ audit/pubmed-pmc-ingest-20261008";
        // Room for both, whole.
        assert_eq!(fit(path, branch, 80), (path.into(), branch.into()));
        // The path first, from its start, down to half the line.
        assert_eq!(
            fit(path, "⎇ main", 34),
            ("…focal-md-pubmed-pmc-audit".into(), "⎇ main".into())
        );
        // Then the branch, from its end.
        assert_eq!(
            fit(path, branch, 34),
            ("…pubmed-pmc-audit".into(), "⎇ audit/pubmed…".into())
        );
        // Too little left for it, the branch goes and the path takes the
        // line.
        assert_eq!(
            fit(path, branch, 12),
            ("…d-pmc-audit".into(), String::new())
        );
        // Without a branch, the path alone.
        assert_eq!(fit(path, "", 12), ("…d-pmc-audit".into(), String::new()));
        for room in 0..90 {
            fit(path, branch, room);
        }
    }

    #[test]
    fn only_rows_under_a_worktrees_header_are_nested() {
        use crate::app::grouping::{AppKey, ReadPeriod};
        let session = crate::app::testing::view("A", "a", 0);
        let folder = ScopeRoot::from_canonical("/repo".into());
        let root = std::path::PathBuf::from("/repo/worktrees/fix");
        let section = Section::App(AppKey::new("claude"));
        let rows = [
            ListRow::Folders,
            ListRow::PinnedHeader,
            ListRow::Session(&session),
            ListRow::FolderHeader(&folder),
            ListRow::Session(&session),
            ListRow::WorktreeHeader(&folder, &root),
            ListRow::Session(&session),
            ListRow::SectionHeader(&section),
            ListRow::Session(&session),
            ListRow::ReadPeriodHeader(ReadPeriod::LastHour),
            ListRow::Session(&session),
            ListRow::NewSession,
        ];
        // Ready's read periods are in line with ready, and so are their
        // rows.
        assert_eq!(
            nesting(&rows),
            [false, false, false, false, false, false, true, false, false, false, false, false]
        );
        // A top-level row's text in the header's text column, its gutter
        // where the header's mark is; a nested one's in the worktree
        // header's.
        assert_eq!(row_indent(false) + GUTTER, INDENT);
        assert_eq!(row_indent(true) + GUTTER, INDENT + NEST);
    }

    #[test]
    fn open_places_are_pairs_under_the_current_one_with_a_rule_between() {
        use crate::app::testing::harness;
        use crate::app::Selection;
        let p = theme::palette();
        let session = with_places(&[
            ("/repo/worktrees/fix", "fix", 5, true),
            ("/repo", "main", 300, false),
            ("/srv/other", "dev", 7200, false),
        ]);
        let mut h = harness(vec![session.clone()]);
        h.app.places_open.insert(session.id.clone());
        let PlaceLines {
            lines,
            toggle,
            current,
            places,
            focus,
            ..
        } = place_content(&h.app, &session, 40, false, None, 1000);
        // The place it works in now is the first line, not again under it,
        // its path in the row's text column; the others a step further in,
        // with nothing left of them.
        let rule = |room: usize| format!("    {}", "─".repeat(room));
        let branch =
            |left: &str, gap: usize, age: &str| format!("    {left}{}{age}", " ".repeat(gap));
        assert_eq!(
            text(&lines),
            [
                "▾ repo  ⎇ main".to_string(),
                rule(36),
                "    worktrees/fix".into(),
                branch("⎇ fix", 28, "now"),
                rule(36),
                "    /srv/other".into(),
                branch("⎇ dev", 29, "2h"),
            ]
        );
        assert_eq!(toggle, Some(0..2));
        assert_eq!(current, 0..1);
        assert_eq!(places, [2..4, 5..7]);
        assert_eq!(focus, None);
        // Nothing left of any place's text but the gutter and, for the
        // others, blanks.
        let blank_beside = |lines: &[Vec<Span>], line: usize| {
            let span = &lines[line][1];
            span.content == "  " && span.style == Style::default()
        };
        assert_eq!(lines[0].len(), 4, "the toggle, path, gap and branch");
        for line in [2, 3, 5, 6] {
            assert!(blank_beside(&lines, line), "{line}");
        }
        let rule_span = lines[1].iter().find(|span| span.content.starts_with('─'));
        assert_eq!(rule_span.and_then(|span| span.style.fg), Some(p.border_dim));
        // Under the pointer, on either of its lines, a place shows the copy
        // mark in its gutter and its path brighter.
        let lines =
            place_content(&h.app, &session, 40, false, Some(PlaceRef::Listed(1)), 1000).lines;
        let marked = text(&lines);
        assert_eq!(marked[5], "⧉   /srv/other");
        assert_eq!(marked[6], branch("⎇ dev", 29, "2h"));
        assert!(!marked[2].contains('⧉'));
        assert_eq!(lines[5][0].style.fg, Some(p.accent_primary));
        assert_eq!(lines[5][2].style.fg, Some(p.text_bright));
        assert!(!lines[5][2].style.add_modifier.contains(Modifier::BOLD));
        assert!(blank_beside(&lines, 5) && blank_beside(&lines, 6));
        // The one the list keys are on (in the selected session) too, its
        // path bright and bold.
        h.app.list.select(Selection::Session(session.id.clone()));
        h.app.list.select_place(Some("/repo/worktrees/fix".into()));
        let PlaceLines { lines, focus, .. } = place_content(&h.app, &session, 40, true, None, 1000);
        assert_eq!(focus, Some(0));
        assert_eq!(text(&lines)[2], "⧉   worktrees/fix");
        assert!(blank_beside(&lines, 2) && blank_beside(&lines, 3));
        let path = lines[2].last().unwrap();
        assert_eq!(path.style.fg, Some(p.text_bright));
        assert!(path.style.add_modifier.contains(Modifier::BOLD));
        // Narrow: the same pairs, paths cut at their start, the branch cut
        // before the age.
        let PlaceLines { lines, places, .. } =
            place_content(&h.app, &session, 16, false, None, 1000);
        assert_eq!(
            text(&lines),
            [
                "▾ repo  ⎇ main".to_string(),
                rule(12),
                "    …rktrees/fix".into(),
                branch("⎇ fix", 4, "now"),
                rule(12),
                "    /srv/other".into(),
                branch("⎇ dev", 5, "2h"),
            ]
        );
        assert_eq!(places, [2..4, 5..7]);
        for width in [8, 12, 16, 24, 40, 80] {
            let PlaceLines { lines, places, .. } =
                place_content(&h.app, &session, width, false, None, 1000);
            assert!(places.iter().all(|pair| pair.len() == 2), "{width}");
            for line in &lines {
                assert!(
                    Line::from(line.clone()).width() <= width.max(6),
                    "{width}: {:?}",
                    text(&lines)
                );
            }
        }
        assert_eq!(
            row_height_in(&h.app, &ListRow::Session(&session), 40, 1000),
            10
        );
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
            .map(|tab| {
                let i = given.iter().position(|t| t == tab).unwrap();
                (*tab, marks[i], labels[i].to_string())
            })
            .collect()
    }

    #[test]
    fn which_tab_is_active_never_moves_the_tabs() {
        let shown = tabs(["▤", "◇", "◔"], ["web", "App", "Status"]);
        // Two apart at 40 columns, one apart at 27; then without the arrow,
        // two apart at 25 and one at 21.
        for (room, arrows) in [(40, true), (27, true), (25, false), (21, false)] {
            let layouts: Vec<TabLayout> = Grouping::ALL
                .iter()
                .map(|active| lay_out_tabs(&shown, *active, room))
                .collect();
            assert!(
                layouts.iter().all(|layout| layout.arrows == arrows),
                "{room}: {layouts:?}"
            );
            assert!(
                layouts
                    .windows(2)
                    .all(|pair| (pair[0].gap, pair[0].width) == (pair[1].gap, pair[1].width)),
                "{layouts:?}"
            );
        }
    }

    #[test]
    fn tabs_narrow_their_gaps_then_drop_the_arrow_then_show_marks_alone_then_cut_the_label() {
        let plain = ["▤", "◇", "◔"];
        let shown = tabs(plain, ["Folders", "App", "Status"]);
        let at = |room| {
            let layout = lay_out_tabs(&shown, Grouping::App, room);
            (layout.gap, layout.arrows, layout.compact, layout.width)
        };
        // `◔ Status    ▤ Folders    ◇ App ▾`: 10 + 11 + 7 with every
        // tab's arrow room, two apart, then one.
        assert_eq!(at(40), (2, true, false, 32));
        assert_eq!(at(31), (1, true, false, 30));
        // `◔ Status  ▤ Folders  ◇ App`: the names stay, the arrow goes
        // (from every tab: switching still moves nothing).
        assert_eq!(at(29), (2, false, false, 26));
        assert_eq!(at(25), (1, false, false, 24));
        // `◔  ▤  ◇ App ▾`.
        assert_eq!(at(23), (2, true, true, 13));
        assert_eq!(at(12), (1, true, true, 11));
        // Then the active label is cut.
        let cut = lay_out_tabs(&shown, Grouping::App, 10);
        assert_eq!((cut.active_label.as_str(), cut.width), ("A…", 10));
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
        let lines = cell_lines(vec![vec![Span::raw("a")], vec![Span::raw("b")]], 3, 1);
        assert_eq!(text(lines), vec!["", " a", " b"]);
        let lines = cell_lines(vec![vec![Span::raw("new")]], 2, 3);
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
