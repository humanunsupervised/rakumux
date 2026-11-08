//! Left pane (Sidebar layout): the tabs at the top (Status, Folders, App and
//! Pinned: what the list is grouped and filtered by; the active one names
//! its filter, `▤ web ▾`, and Enter or a click on it opens its modal, all
//! but Pinned), then the sessions shown, under a header row per folder
//! (app, status) when there are several, and "+ New session" last. Beside
//! the session (Master detail) its last row is `« Hide list`; hidden, a rail
//! at the left edge (`draw_list_rail`) shows it again.
//! Each session is a compact cell after a blank row: its name (its AI title
//! with AI session titles on), then its age and its mark (an agent's
//! activity, else its status) at the right; then its program and state; then
//! the folder it works in now, one line (its path, its repository's name,
//! its branch), with `▸` when it has worked in other places, which open
//! under it as pairs of lines a step further in (`app::places`), and after
//! its text how many places it has, that one included. With AI
//! session titles on, a live session's name keeps two columns after it: a
//! spinner while a new title is being made, else the ↻ under the pointer,
//! which makes one. A row's text starts where its header's does, and the
//! gutter left of it, where the header's mark sits, holds the row's marks:
//! the gold `★` of a pinned session (but under Pinned), the pin toggle
//! under the pointer, the places' `▸`/`▾` and the copy mark.
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
use session_core::model::{ScopeRoot, SessionStatus, SessionView};

use super::folder_picker::{count_label, folder_branch, tab_icon, tab_picker_open};
use super::format::{
    activity_badge_at, branch_label, facts_line, last_activity, place_label, read_period_style,
    relative_time, repo_label, session_repos, status_glyph, status_group_glyph, truncate,
    truncate_start, PIN_MARK,
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
        | ListRow::ReadPeriodHeader(_) => 2,
        ListRow::Session(_) => 3,
    }
}

/// [`row_height`], plus a session's place lines (`width` columns of
/// content).
fn row_height_in(app: &App, row: &ListRow, width: usize, now: i64) -> u16 {
    match row {
        ListRow::Session(session) => {
            let places = place_content(app, session, width, false, None, now)
                .lines
                .len();
            row_height(row) + u16::try_from(places).unwrap_or(u16::MAX)
        }
        row => row_height(row),
    }
}

/// Columns kept after a session's name for its title mark: a space and the
/// ↻ (or the spinner while a title is being made).
const TITLE_SLOT: usize = 2;

/// Whether `session`'s name keeps [`TITLE_SLOT`] after it: AI session titles
/// are on and it can get one (live, not private).
fn has_title_slot(app: &App, session: &SessionView) -> bool {
    app.titles_on() && session.status == SessionStatus::Live && !session.private
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
/// places' `▸`/`▾`, the copy mark: one and a gap), where its header's mark
/// sits.
const GUTTER: usize = 2;

/// How much further in a row under a worktree's header starts, as that
/// header's text does.
const NEST: usize = 2;

/// How much further in than ready's header one of its read periods' headers
/// (mark and words) and that period's rows start.
const READ_NEST: usize = 1;

/// Columns left of a row's gutter: the drag mark's, in its last one; a
/// nested row's (see [`nesting`]) `nested` further in.
fn row_indent(nested: usize) -> usize {
    INDENT - GUTTER + nested
}

/// For each of `rows`, how many columns further in than a top-level row it
/// starts: [`NEST`] under a worktree's header inside its folder,
/// [`READ_NEST`] under one of ready's read periods; headers none.
fn nesting(rows: &[ListRow]) -> Vec<usize> {
    let mut nested = 0;
    rows.iter()
        .map(|row| {
            match row {
                ListRow::WorktreeHeader(..) => nested = NEST,
                ListRow::ReadPeriodHeader(_) => nested = READ_NEST,
                ListRow::Folders
                | ListRow::FolderHeader(_)
                | ListRow::SectionHeader(_)
                | ListRow::NewSession => nested = 0,
                ListRow::Session(_) => return nested,
            }
            0
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

/// At the right of `+ New session`: the current folder's new sessions run
/// on your Sprite while it's lit (`N`).
pub const SPRITE_CHIP: &str = "☁ Sprite";

/// Before the name of a session that runs on another machine.
pub const MACHINE_MARK: &str = "☁";

/// Shown in the gutter of a hovered unpinned session's first line: a tap
/// pins it.
const PIN_TOGGLE: &str = "☆";

/// A session's pin toggle, in the gutter just left of its name (`indent`
/// columns in), the column of its header's mark: under the pointer, or
/// always for a pinned one but under Pinned. `☆` to pin it, the gold `★`
/// to unpin it. Returns where it is: the mark and the space after it.
fn mark_pin(lines: &mut [Line<'_>], pinned: bool, area: Rect, indent: usize) -> Option<Rect> {
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

/// Whether `session`'s row shows the gold `★` whether hovered or not: it's
/// pinned, under any tab but Pinned (where every row is).
fn shows_pin(app: &App, session: &SessionView) -> bool {
    session.pinned && app.list.grouping() != Grouping::Pinned
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

/// The header of one of ready's sub-sections, [`READ_NEST`] further in than
/// ready's: a clock one column right of ready's mark and when its sessions
/// were last read one right of its name, a rule, then how many it has, in
/// `width` columns.
pub fn read_period_header_line(
    app: &App,
    period: ReadPeriod,
    width: usize,
    selected: bool,
) -> Line<'static> {
    let p = theme::palette();
    let count = sessions_label(app.list.read_period_count(period));
    // "  ◷ label ─── count": a status's header, a step in.
    let clock = format!("{} ", app.icons().clock());
    let lead = 1 + READ_NEST;
    let fixed = lead + 2 + str_width(&clock) + str_width(&count);
    let label = truncate(period.label(), width.saturating_sub(fixed + 2));
    let rule = width.saturating_sub(fixed + str_width(&label));
    let (label_color, rule_color) = if selected {
        (p.text_bright, p.accent_primary)
    } else {
        (p.label_blue, p.border_dim)
    };
    Line::from(vec![
        Span::raw(" ".repeat(lead)),
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

/// A section's header (an app's, a repository's, a status's) as a bar
/// across the list, on a background of its own so sections stand out from
/// their sessions: its mark (a status's; a repository's or an app's name
/// where a status's is, in line with its sessions' text), its name, then at
/// the right how many sessions it has (`2/3 live`, `3 sessions`) and `▾`
/// (open) or `▸` (collapsed: a click, or Space, opens it), in `width`
/// columns. Selected, the bar takes the selection's colour.
pub fn section_header_line(
    app: &App,
    section: &Section,
    width: usize,
    selected: bool,
) -> Line<'static> {
    let p = theme::palette();
    let count = section_count(app, section);
    let collapsed = app.list.is_collapsed(section);
    let toggle = if collapsed {
        SECTION_CLOSED
    } else {
        SECTION_OPEN
    };
    let bar = if selected { p.selection } else { p.surface };
    let on_bar = |style: Style| style.bg(bar);
    let (mark, mark_style) = match section {
        Section::Status(status) => {
            let (glyph, style) = status_group_glyph(*status);
            (format!("{glyph} "), style)
        }
        Section::Repo(_) | Section::App(_) => (" ".repeat(GUTTER), Style::default()),
    };
    // " mark name … count ▾ ": the name gets what's left, at least a column
    // before the count.
    let tail = format!("{count} {toggle} ");
    let fixed = 1 + str_width(&mark) + str_width(&tail) + 1;
    let name = truncate(section.label(), width.saturating_sub(fixed));
    let gap = width.saturating_sub(fixed - 1 + str_width(&name));
    let name_color = if selected {
        p.text_bright
    } else {
        p.accent_purple
    };
    Line::from(vec![
        Span::styled(" ", on_bar(Style::default())),
        Span::styled(mark, on_bar(mark_style)),
        Span::styled(
            name,
            on_bar(Style::default().fg(name_color).add_modifier(Modifier::BOLD)),
        ),
        Span::styled(" ".repeat(gap), on_bar(Style::default())),
        Span::styled(count, on_bar(Style::default().fg(p.neutral_gray))),
        Span::styled(" ", on_bar(Style::default())),
        Span::styled(toggle, on_bar(Style::default().fg(p.accent_primary))),
        Span::styled(" ", on_bar(Style::default())),
    ])
}

/// At the right of a section's bar: its sessions shown (`▾`) or collapsed
/// under it (`▸`).
const SECTION_OPEN: &str = "▾";
const SECTION_CLOSED: &str = "▸";

/// The content lines of a session cell: two, each after its gutter (two
/// columns). The first is its gutter's mark, a space, its name, its title
/// slot (empty without one; see [`mark_title`]), then the age and mark at
/// the right.
fn session_content(
    app: &App,
    session: &SessionView,
    width: usize,
    selected: bool,
    now: i64,
) -> Vec<Vec<Span<'static>>> {
    let p = theme::palette();
    // On the right after the age: an agent's activity, else the status (a
    // program like nvim). The slot left of the name is the pin's.
    let badge = activity_badge_at(session, now)
        .map(|(mark, style)| Span::styled(mark, style.add_modifier(Modifier::BOLD)))
        .or_else(|| {
            let (glyph, style) = status_glyph(session);
            (!glyph.trim().is_empty()).then(|| Span::styled(glyph, style))
        });
    let (glyph, glyph_style) = (" ", Style::default());
    let badge_width = badge.as_ref().map_or(0, |span| span.width() + 1);
    let age = truncate(
        &relative_time(now, last_activity(session)),
        width.saturating_sub(3 + badge_width),
    );
    let right_width = badge_width + str_width(&age);
    let mut name = match &session.machine {
        Some(_) => format!("{MACHINE_MARK} {}", session.name),
        None => session.name.clone(),
    };
    if app.is_self_session(&session.id) {
        name.push_str(" (this)");
    }
    // Leading marker slot, name, its title slot, at least one space, age +
    // badge. The pin shows left of the row for a pinned session but under
    // Pinned, and under the pointer (`mark_pin`); the ↻ under the pointer,
    // in the title slot (`mark_title`).
    let slot = if has_title_slot(app, session) {
        TITLE_SLOT
    } else {
        0
    };
    let name = truncate(&name, width.saturating_sub(right_width + 3 + slot));
    let pad = width.saturating_sub(2 + str_width(&name) + slot + right_width);
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
        Span::raw(" ".repeat(slot)),
    ];
    first.push(Span::raw(" ".repeat(pad)));
    first.push(Span::styled(age, Style::default().fg(age_color)));
    if let Some(badge) = badge {
        first.push(Span::raw(" "));
        first.push(badge);
    }
    // Under the name and its places (`place_content`): the app, its
    // subagents and report (an ended session's way back), ending where the
    // right column starts.
    let column = width.saturating_sub(2 + RIGHT_COLUMN);
    let detail = Style::default().fg(detail_color);
    let mut second = vec![Span::raw("  ")];
    second.extend(facts_line(session, column, app.icons(), detail, false).spans);
    vec![first, second]
}

/// In the gutter of a session's repositories: `▸` while its places are
/// closed, `▾` while they're open.
const PLACES_CLOSED: &str = "▸";
const PLACES_OPEN: &str = "▾";
/// In the gutter of a place under the pointer or the list keys: a click or
/// tap (`y`) copies its path.
const COPY_MARK: &str = "⧉";
/// How much further in than the row's text its places and the rules
/// between them start.
const PLACE_INDENT: usize = 2;
/// Between a place's repository's name and its branch.
const REPO_GAP: &str = " ";
/// The fewest columns a place's repository's name is cut to (`focal…`);
/// with less room, it goes.
const REPO_MIN: usize = 6;
/// Between two places, under their text.
const PLACE_RULE: &str = "─";

/// A session's place lines.
struct PlaceLines {
    lines: Vec<Vec<Span<'static>>>,
    /// Each open place's two lines: where it works now first, then the
    /// others in the order of [`crate::app::places::listed_places`].
    places: Vec<std::ops::Range<usize>>,
    /// The open place the list keys are on (its index in `places`).
    focus: Option<usize>,
}

/// A place's repository's name and branch in `room` columns: the name is
/// cut at its end first, down to [`REPO_MIN`] columns, then goes, then the
/// branch is cut at its end.
fn fit_git(repo: &str, branch: &str, room: usize) -> (String, String) {
    if repo.is_empty() || branch.is_empty() {
        return (truncate(repo, room), truncate(branch, room));
    }
    let repo_room = room.saturating_sub(str_width(REPO_GAP) + str_width(branch));
    if repo_room >= REPO_MIN.min(str_width(repo)) {
        return (truncate(repo, repo_room), branch.to_string());
    }
    (String::new(), truncate(branch, room))
}

/// A place's repository's name (bold) and branch as spans in `style`, with
/// the gap between them.
fn git_spans(repo: String, branch: String, style: Style) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    let both = !repo.is_empty() && !branch.is_empty();
    if !repo.is_empty() {
        spans.push(Span::styled(repo, style.add_modifier(Modifier::BOLD)));
    }
    if both {
        spans.push(Span::raw(REPO_GAP));
    }
    if !branch.is_empty() {
        spans.push(Span::styled(branch, style));
    }
    spans
}

/// A session's place lines, under its name, after the gutter. First the
/// repositories it works in, in bold and comma-separated
/// ([`session_repos`]; outside any, the folder it works in now), with `▸` in
/// the gutter. While they're open (`▾`), its places, each a pair of lines
/// [`PLACE_INDENT`] further in with a rule above: where it works now first
/// (its working tree, else its directory), then where else it changed
/// files, most recent first; its path, then its repository's name, branch
/// and when. The place the list keys are on (in the `selected` session) and
/// the `hovered` one show the copy mark in their gutter and their path
/// brighter; the list keys' path is bold.
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
    // The repositories' room, after the gutter, and the places', a step
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
    let indent = || Span::raw(" ".repeat(PLACE_INDENT));
    let copy_slot = || {
        Span::styled(
            format!("{COPY_MARK:<GUTTER$}"),
            Style::default().fg(p.accent_primary),
        )
    };
    // A place's repository's name (when its folder doesn't say it) and its
    // branch.
    let git_of = |git: Option<&session_core::model::GitInfo>| -> (String, String) {
        git.map(|git| {
            (
                repo_label(git).unwrap_or_default().to_string(),
                branch_label(git, icons).unwrap_or_default(),
            )
        })
        .unwrap_or_default()
    };

    // The repositories, with the toggle.
    let open = app.places_open(&session.id);
    let mark = if open { PLACES_OPEN } else { PLACES_CLOSED };
    let toggle = Span::styled(
        format!("{mark:<GUTTER$}"),
        Style::default().fg(p.accent_primary),
    );
    let folder_git = app
        .folders
        .get(&session.scope)
        .and_then(|folder| folder.git.as_ref());
    let repos = session_repos(session, folder_git).join(", ");
    let current = crate::app::places::current_place(session);
    // Ending where the right column starts, as the app line under it does.
    let summary_room = width.saturating_sub(GUTTER + RIGHT_COLUMN);
    let summary = if repos.is_empty() {
        Span::styled(
            truncate_start(&place_label(&session.scope, current), summary_room),
            label_style,
        )
    } else {
        Span::styled(
            truncate(&repos, summary_room),
            label_style.add_modifier(Modifier::BOLD),
        )
    };
    let mut lines = vec![vec![toggle, summary]];
    let mut places = Vec::new();
    if !open {
        return PlaceLines {
            lines,
            places,
            focus: None,
        };
    }

    // Under it, where it works now, then the others.
    let listed: Vec<_> = crate::app::places::listed_places(session).collect();
    let current_at = session
        .places
        .iter()
        .find(|place| place.path == current)
        .map(|place| place.last_at);
    let all = std::iter::once((current, session.git.as_ref(), current_at)).chain(
        listed.iter().map(|place| {
            (
                place.path.as_path(),
                place.git.as_ref(),
                Some(place.last_at),
            )
        }),
    );
    let focus = selected
        .then(|| app.list.selected_place())
        .flatten()
        .and_then(|focus| {
            std::iter::once(current)
                .chain(listed.iter().map(|place| place.path.as_path()))
                .position(|path| path == focus)
        });
    for (index, (path, git, last_at)) in all.enumerate() {
        lines.push(vec![
            Span::raw(" ".repeat(GUTTER + PLACE_INDENT)),
            Span::styled(PLACE_RULE.repeat(inner), Style::default().fg(p.border_dim)),
        ]);
        let start = lines.len();
        let place_ref = match index {
            0 => PlaceRef::Current,
            index => PlaceRef::Listed(index - 1),
        };
        let marked = focus == Some(index) || hovered == Some(place_ref);
        let path_style = if focus == Some(index) {
            marked_style.add_modifier(Modifier::BOLD)
        } else if marked {
            marked_style
        } else {
            label_style
        };
        let label = place_label(&session.scope, path);
        lines.push(vec![
            if marked { copy_slot() } else { blank_slot() },
            indent(),
            Span::styled(truncate_start(&label, inner), path_style),
        ]);
        // Under it its repository's name and branch, and at the right its
        // age; short of room the name is cut first, then the branch, then
        // the age.
        let age = last_at
            .map(|at| truncate(&relative_time(now, at), inner))
            .unwrap_or_default();
        let (repo, branch) = git_of(git);
        let room = inner.saturating_sub(str_width(&age) + usize::from(!age.is_empty()));
        let (repo, branch) = fit_git(&repo, &branch, room);
        let git = git_spans(repo, branch, branch_style);
        let used: usize = git.iter().map(|span| str_width(&span.content)).sum();
        let gap = inner.saturating_sub(used + str_width(&age));
        let mut second = vec![blank_slot(), indent()];
        second.extend(git);
        second.push(Span::raw(" ".repeat(gap)));
        second.push(Span::styled(age, branch_style));
        lines.push(second);
        places.push(start..lines.len());
    }
    PlaceLines {
        lines,
        places,
        focus,
    }
}

/// After a hovered session's name: a click or tap makes a new AI title.
const TITLE_REFRESH: &str = "↻";

/// The title slot after a session's name, on its first line (`line`, after
/// its indent: the gutter's mark, a space, the name, the slot; see
/// `session_content`): a spinner while a new title is being made, else the
/// ↻ when `hovered`. Returns where the ↻ is, its space included.
fn mark_title(
    lines: &mut [Line<'_>],
    line: usize,
    refreshing: bool,
    hovered: bool,
    area: Rect,
) -> Option<Rect> {
    let p = theme::palette();
    let (mark, style) = if refreshing {
        (get_spinner_char(), Style::default().fg(p.accent_primary))
    } else if hovered {
        (TITLE_REFRESH, Style::default().fg(p.accent_primary))
    } else {
        return None;
    };
    let spans = &mut lines.get_mut(line)?.spans;
    let x: usize = spans.get(..4)?.iter().map(Span::width).sum();
    *spans.get_mut(4)? = Span::styled(format!(" {mark}"), style);
    if refreshing {
        return None;
    }
    let x = area.x + u16::try_from(x).ok()?;
    Some(Rect {
        x,
        y: area.y + u16::try_from(line).ok()?,
        width: (TITLE_SLOT as u16).min(area.right().saturating_sub(x)),
        height: 1,
    })
}

/// Columns between two tabs when there's room for them (fewer otherwise),
/// after the two every tab keeps for the active one's ` ▾` (while it fits).
const TAB_GAP: usize = 2;

/// Columns every tab with a filter keeps after its label for the ` ▾` the
/// active one ends in, so switching tabs moves nothing. Pinned, which has
/// no filter, keeps none: it's the last tab, so nothing moves after it.
const ARROW_SLOT: usize = 2;

/// [`ARROW_SLOT`] for `tab`, while the tabs show the ` ▾` (`arrows`).
fn arrow_slot(tab: Grouping, arrows: bool) -> usize {
    if arrows && tab.has_filter() {
        ARROW_SLOT
    } else {
        0
    }
}

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
        let tabs_width: usize = tabs
            .iter()
            .map(|(tab, icon, label)| match (*tab == active, compact) {
                (true, _) => str_width(icon) + 1 + active_label + arrow_slot(*tab, arrows),
                (false, true) => str_width(icon),
                (false, false) => str_width(icon) + 1 + str_width(label) + arrow_slot(*tab, arrows),
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

/// The tabs on a raised band: Status, Folders, App, Pinned. The active one
/// is bold, names its filter (`▤ web  ⎇ main`, `◇ claude`), ends in `▾` (it
/// opens its modal; Pinned has none) and is underlined; the text filter shows at the right end
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
            let slot = arrow_slot(*tab, layout.arrows);
            if slot > 0 {
                spans.push(Span::raw(" "));
                spans.push(Span::styled(
                    if open { "▴" } else { "▾" },
                    Style::default().fg(arrow),
                ));
            }
            x += str_width(icon) + 1 + str_width(&layout.active_label) + str_width(branch) + slot;
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
                    format!(
                        " {tab_label}{branch}{}",
                        " ".repeat(arrow_slot(*tab, layout.arrows))
                    ),
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
                str_width(icon)
                    + 1
                    + str_width(tab_label)
                    + branch
                    + arrow_slot(*tab, layout.arrows)
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
    let width_of = |nested: usize| (inner.width as usize).saturating_sub(row_indent(nested) + 1);
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
                // start; at the right the current folder's `☁ Sprite`
                // toggle, lit while its new sessions run on the Sprite.
                let room = width_of(0).saturating_sub(GUTTER);
                let label = truncate("New session", room);
                let on = app.runs_on_sprite(&app.current_folder());
                let mut spans = vec![
                    Span::styled(
                        format!("{:<GUTTER$}", "+"),
                        Style::default()
                            .fg(p.accent_green)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        label.clone(),
                        Style::default()
                            .fg(if is_selected {
                                p.text_bright
                            } else {
                                p.accent_green
                            })
                            .add_modifier(Modifier::BOLD),
                    ),
                ];
                let chip_width = str_width(SPRITE_CHIP);
                let gap = room.saturating_sub(str_width(&label) + chip_width);
                if gap >= 2 {
                    spans.push(Span::raw(" ".repeat(gap)));
                    spans.push(Span::styled(
                        SPRITE_CHIP,
                        if on {
                            Style::default()
                                .fg(p.accent_primary)
                                .add_modifier(Modifier::BOLD)
                        } else {
                            Style::default().fg(p.text_muted)
                        },
                    ));
                    let x = row_area.x as usize + row_indent(0) + GUTTER + str_width(&label) + gap;
                    if let Ok(x) = u16::try_from(x) {
                        row_hits.push((
                            Rect {
                                x,
                                y: row_area.y + height.saturating_sub(1),
                                width: u16::try_from(chip_width).unwrap_or(0),
                                height: 1,
                            },
                            Hit::SpriteToggle,
                        ));
                    }
                }
                cell_lines(vec![spans], height, row_indent(0))
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
                // Its text in its header's text column, its marks in the
                // gutter before it.
                let indent = row_indent(nested[index]);
                let width = width_of(nested[index]);
                let content = session_content(app, session, width, is_selected, now);
                let hovered = app.hovered_row == Some(index);
                let dragged = app.dragged_session() == Some(&session.id);
                let hovered_place = app
                    .hovered_place
                    .filter(|(row, _)| *row == index && !dragged)
                    .map(|(_, place)| place);
                let places = place_content(app, session, width, is_selected, hovered_place, now);
                // The repositories' line's `▸` and names (not the blank after
                // them, which a tap meant for the row lands on).
                let summary_width: usize = places
                    .lines
                    .first()
                    .map_or(0, |line| line.iter().map(|span| span.width()).sum());
                // The name, its repositories and places, then its app.
                let places_at = 1;
                let mut content = content.into_iter();
                let mut cell: Vec<_> = content.next().into_iter().collect();
                cell.extend(places.lines);
                cell.extend(content);
                let content = cell;
                // The cell's content is at its bottom, after blank rows.
                let top = (height as usize).saturating_sub(content.len());
                let mut lines = cell_lines(content, height, indent);
                // The whole row, of which `row_area` shows `skip` rows on.
                let full = Rect { height, ..row_area };
                // A place's lines, from the column left of its gutter.
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
                    let end = top + places_at + focus.map_or(1, |range| range.end);
                    skip = (end as u16).saturating_sub(row_area.height);
                }
                // After the name, with AI session titles on: the ↻ makes a
                // new title, a spinner while one is being made.
                if has_title_slot(app, session) {
                    let refreshing = app.title_refreshing(&session.id);
                    if let Some(refresh) =
                        mark_title(&mut lines, top, refreshing, hovered && !dragged, full)
                    {
                        row_hits.push((refresh, Hit::TitleRefresh(index)));
                    }
                }
                if !dragged {
                    // The repositories' `▸` and names open and close the
                    // places under them; a click on a place copies its path.
                    let area = Rect {
                        x: full.x + indent as u16,
                        y: full.y.saturating_add((top + places_at) as u16),
                        width: summary_width as u16,
                        height: 1,
                    }
                    .intersection(full);
                    row_hits.push((area, Hit::PlacesToggle(index)));
                    for (place, range) in places.places.iter().enumerate() {
                        let area = line_area(top + places_at + range.start, range.len());
                        let place = match place {
                            0 => PlaceRef::Current,
                            place => PlaceRef::Listed(place - 1),
                        };
                        row_hits.push((area, Hit::Place(index, place)));
                    }
                }
                if dragged {
                    mark_dragged(&mut lines, indent);
                } else if hovered || shows_pin(app, session) {
                    // Before the row's own target, which would take the tap.
                    // Only under the pointer: a phone (no hover) shows the ★
                    // but a tap there opens the session, never unpins it.
                    let pin = mark_pin(&mut lines, session.pinned, full, indent);
                    if let Some(pin) = pin.filter(|_| hovered) {
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
            Connection::Connected(_) if app.list.grouping() == Grouping::Pinned => {
                "   no pinned sessions".into()
            }
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
    fn the_last_line_is_the_app_its_subagents_and_report() {
        use crate::app::testing::{harness, on_branch, view};

        let h = harness(vec![]);
        let mut session = view("A", "a", 0);
        session.git = on_branch("main");
        session.agent = Some(session_core::model::AgentInfo {
            kind: "opencode".into(),
            ..Default::default()
        });
        session.reported = Some(session_core::model::Reported {
            state: None,
            text: Some("tests pass".into()),
            result: None,
            at: 0,
        });
        let p = theme::palette();
        for selected in [false, true] {
            let lines = session_content(&h.app, &session, 40, selected, 0);
            // The app in its colour, then the report; the repositories and
            // branch are on the line above (`place_content`).
            assert_eq!(text(&lines[1..]), ["  opencode · tests pass"]);
            assert_eq!(lines[1][1].style.fg, Some(p.label_slate_blue));
        }
        // An ended session says how to bring it back.
        session.status = SessionStatus::Dead;
        let lines = session_content(&h.app, &session, 40, false, 0);
        assert_eq!(text(&lines[1..]), ["  dead · ⏎ revive · tests pass"]);
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
    fn closed_places_are_the_repositories_alone_in_bold() {
        use crate::app::testing::harness;
        let p = theme::palette();
        let h = harness(vec![]);
        // Working in `focal-md`, having changed files in `other` too.
        let mut session =
            with_places(&[("/repo", "main", 5, true), ("/srv/other", "dev", 300, true)]);
        let named = |git: &mut Option<session_core::model::GitInfo>, repo: &str| {
            if let Some(git) = git.as_mut() {
                git.repo = Some(repo.into());
            }
        };
        named(&mut session.git, "focal-md");
        named(&mut session.places[0].git, "focal-md");
        named(&mut session.places[1].git, "other");
        for selected in [false, true] {
            let PlaceLines { lines, places, .. } =
                place_content(&h.app, &session, 40, selected, None, 1000);
            // No folder or worktree, no branch: the toggle and the names.
            assert_eq!(text(&lines), ["▸ focal-md, other"]);
            assert!(places.is_empty());
            assert_eq!(lines[0][0].style.fg, Some(p.accent_primary));
            assert!(lines[0][1].style.add_modifier.contains(Modifier::BOLD));
        }
        // Cut at their end, never wrapped, where the right column (the age and mark above) starts.
        let lines = place_content(&h.app, &session, 24, false, None, 1000).lines;
        assert_eq!(text(&lines), ["▸ focal-md, o…"]);
        // Outside any repository: the folder it works in, not bold.
        let mut plain = crate::app::testing::view("B", "b", 0);
        plain.cwd = "/repo/notes".into();
        let lines = place_content(&h.app, &plain, 40, false, None, 1000).lines;
        assert_eq!(text(&lines), ["▸ notes"]);
        assert!(!lines[0][1].style.add_modifier.contains(Modifier::BOLD));
        // A session row is its name, its repositories and its app.
        assert_eq!(
            row_height_in(&h.app, &ListRow::Session(&session), 40, 1000),
            4
        );
    }

    #[test]
    fn rows_under_a_worktree_or_a_read_period_are_nested() {
        use crate::app::grouping::{AppKey, ReadPeriod};
        let session = crate::app::testing::view("A", "a", 0);
        let folder = ScopeRoot::from_canonical("/repo".into());
        let root = std::path::PathBuf::from("/repo/worktrees/fix");
        let section = Section::App(AppKey::new("claude"));
        let rows = [
            ListRow::Folders,
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
        // A worktree's rows two columns in, a read period's one.
        assert_eq!(nesting(&rows), [0, 0, 0, 0, 0, NEST, 0, 0, 0, READ_NEST, 0]);
        // A top-level row's text in the header's text column, its gutter
        // where the header's mark is; a nested one's in the worktree
        // header's, or a column right of ready's.
        assert_eq!(row_indent(0) + GUTTER, INDENT);
        assert_eq!(row_indent(NEST) + GUTTER, INDENT + NEST);
        assert_eq!(row_indent(READ_NEST) + GUTTER, INDENT + 1);
    }

    #[test]
    fn open_places_are_pairs_where_it_works_now_first_with_a_rule_between() {
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
            places,
            focus,
        } = place_content(&h.app, &session, 40, false, None, 1000);
        // The repositories, then where it works now (its checkout, `/repo`)
        // and the others, each a pair a step further in.
        let rule = |room: usize| format!("    {}", "─".repeat(room));
        let branch =
            |left: &str, gap: usize, age: &str| format!("    {left}{}{age}", " ".repeat(gap));
        assert_eq!(
            text(&lines),
            [
                "▾ repo".to_string(),
                rule(36),
                "    repo".into(),
                branch("⎇ main", 28, "5m"),
                rule(36),
                "    worktrees/fix".into(),
                branch("⎇ fix", 28, "now"),
                rule(36),
                "    /srv/other".into(),
                branch("⎇ dev", 29, "2h"),
            ]
        );
        assert_eq!(places, [2..4, 5..7, 8..10]);
        assert_eq!(focus, None);
        // Nothing left of any place's text but the gutter and blanks.
        let blank_beside = |lines: &[Vec<Span>], line: usize| {
            let span = &lines[line][1];
            span.content == "  " && span.style == Style::default()
        };
        for line in [2, 3, 5, 6, 8, 9] {
            assert!(blank_beside(&lines, line), "{line}");
        }
        let rule_span = lines[1].iter().find(|span| span.content.starts_with('─'));
        assert_eq!(rule_span.and_then(|span| span.style.fg), Some(p.border_dim));
        // Under the pointer, on either of its lines, a place shows the copy
        // mark in its gutter and its path brighter.
        let lines =
            place_content(&h.app, &session, 40, false, Some(PlaceRef::Listed(1)), 1000).lines;
        let marked = text(&lines);
        assert_eq!(marked[8], "⧉   /srv/other");
        assert!(!marked[5].contains('⧉') && !marked[2].contains('⧉'));
        assert_eq!(lines[8][0].style.fg, Some(p.accent_primary));
        assert_eq!(lines[8][2].style.fg, Some(p.text_bright));
        assert!(!lines[8][2].style.add_modifier.contains(Modifier::BOLD));
        let lines = place_content(&h.app, &session, 40, false, Some(PlaceRef::Current), 1000).lines;
        assert_eq!(text(&lines)[2], "⧉   repo");
        // The one the list keys are on (in the selected session) too, its
        // path bright and bold.
        h.app.list.select(Selection::Session(session.id.clone()));
        h.app.list.select_place(Some("/repo/worktrees/fix".into()));
        let PlaceLines { lines, focus, .. } = place_content(&h.app, &session, 40, true, None, 1000);
        assert_eq!(focus, Some(1));
        assert_eq!(text(&lines)[5], "⧉   worktrees/fix");
        let path = lines[5].last().unwrap();
        assert_eq!(path.style.fg, Some(p.text_bright));
        assert!(path.style.add_modifier.contains(Modifier::BOLD));
        h.app.list.select_place(Some("/repo".into()));
        let PlaceLines { focus, .. } = place_content(&h.app, &session, 40, true, None, 1000);
        assert_eq!(focus, Some(0));
        // Narrow: the same pairs, paths cut at their start.
        let PlaceLines { lines, places, .. } =
            place_content(&h.app, &session, 16, false, None, 1000);
        assert_eq!(
            text(&lines),
            [
                "▾ repo".to_string(),
                rule(12),
                "    repo".into(),
                branch("⎇ main", 4, "5m"),
                rule(12),
                "    …rktrees/fix".into(),
                branch("⎇ fix", 4, "now"),
                rule(12),
                "    /srv/other".into(),
                branch("⎇ dev", 5, "2h"),
            ]
        );
        assert_eq!(places, [2..4, 5..7, 8..10]);
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
            13
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

    /// The tabs with `labels` and `marks` (given for Repos, Agent, Status,
    /// Pinned), in the tabs' order.
    fn tabs(marks: [&'static str; 4], labels: [&str; 4]) -> Vec<(Grouping, &'static str, String)> {
        let given = [
            Grouping::Repo,
            Grouping::App,
            Grouping::Status,
            Grouping::Pinned,
        ];
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
        let shown = tabs(["▤", "◇", "◔", "★"], ["web", "Agent", "Status", "Pinned"]);
        // Two apart at 42 columns, one apart at 37; then without the arrow,
        // two apart at 34 and one at 31. Pinned keeps no room for one.
        for (room, arrows) in [(42, true), (37, true), (34, false), (31, false)] {
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
        let plain = ["▤", "◇", "◔", "★"];
        let shown = tabs(plain, ["Repos", "Agent", "Status", "Pinned"]);
        let at = |room| {
            let layout = lay_out_tabs(&shown, Grouping::App, room);
            (layout.gap, layout.arrows, layout.compact, layout.width)
        };
        // `◔ Status    ▤ Repos    ◇ Agent ▾  ★ Pinned`: 10 + 9 + 9 with
        // every filter's arrow room, and 8, two apart, then one.
        assert_eq!(at(44), (2, true, false, 42));
        assert_eq!(at(40), (1, true, false, 39));
        // `◔ Status  ▤ Repos  ◇ Agent  ★ Pinned`: the names stay, the arrow
        // goes (from every tab: switching still moves nothing).
        assert_eq!(at(38), (2, false, false, 36));
        assert_eq!(at(34), (1, false, false, 33));
        // `◔  ▤  ◇ Agent ▾  ★`.
        assert_eq!(at(32), (2, true, true, 18));
        assert_eq!(at(16), (1, true, true, 15));
        // Then the active label is cut.
        let cut = lay_out_tabs(&shown, Grouping::App, 12);
        assert_eq!((cut.active_label.as_str(), cut.width), ("A…", 12));
        // Pinned active has no `▾` to show.
        let pinned = lay_out_tabs(&shown, Grouping::Pinned, 14);
        assert_eq!((pinned.compact, pinned.width), (true, 14));
        // Whatever the room, marks, labels and their width, the tabs fit
        // it once there's room for the marks and the arrow.
        let emoji = ["📁", "🤖", "🚦", "★"];
        for marks in [plain, emoji] {
            for labels in [
                ["Repos", "Agent", "Status", "Pinned"],
                ["web", "Agent", "Status", "Pinned"],
                ["Repos", "Agent", "waiting for response", "Pinned"],
                ["12 folders", "opencode", "Status", "Pinned"],
            ] {
                let shown = tabs(marks, labels);
                for active in Grouping::ALL {
                    for room in 14..80 {
                        let layout = lay_out_tabs(&shown, active, room);
                        assert!(layout.width <= room, "{labels:?} {active:?} {room}");
                    }
                }
            }
        }
    }

    #[test]
    fn section_headers_are_bars_that_fit_their_width_and_end_in_their_count_and_toggle() {
        use crate::app::grouping::{AppKey, RepoKey, StatusGroup};
        use crate::app::testing::{harness, view};
        let mut h = harness(vec![view("A", "a", 0)]);
        let p = theme::palette();
        let sections = [
            Section::App(AppKey::new("opencode")),
            Section::Repo(RepoKey::of(&view("B", "b", 0))),
            Section::Status(StatusGroup::WaitingForResponse),
        ];
        for section in &sections {
            for width in 24..100 {
                let line = section_header_line(&h.app, section, width, false);
                assert_eq!(line.width(), width, "{section:?} {width}");
                let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
                assert!(
                    text.ends_with(" live ▾ ") || text.ends_with("sessions ▾ "),
                    "{text}"
                );
                // A bar: every cell on the bar's background, no rule.
                assert!(
                    line.spans
                        .iter()
                        .all(|span| span.style.bg == Some(p.surface)),
                    "{text}"
                );
                assert!(!text.contains('─'), "{text}");
            }
        }
        // Selected: the selection's background, the name white.
        let line = section_header_line(&h.app, &sections[2], 40, true);
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(text.starts_with(" "), "{text}");
        assert!(text.contains(" waiting for response "), "{text}");
        assert!(text.ends_with(" no sessions ▾ "), "{text}");
        assert_eq!(line.spans[2].style.fg, Some(p.text_bright));
        assert!(line
            .spans
            .iter()
            .all(|span| span.style.bg == Some(p.selection)));
        // Collapsed: `▸`.
        h.app.list.toggle_collapsed(&sections[2]);
        let line = section_header_line(&h.app, &sections[2], 40, false);
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(text.ends_with(" no sessions ▸ "), "{text}");
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
