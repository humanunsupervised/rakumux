//! Session screen layouts: how the session list and the selected session's
//! detail share the screen.
//!
//! - **Master detail** (`Sidebar`, default): the session list in a column on the left, a
//!   one-column divider, the selected session's detail on the right. The
//!   divider can be dragged; the chosen list width is a setting. Below
//!   [`SINGLE_PANE_BELOW`] columns (phones, split windows) the panes are
//!   stacked instead: in Rakumux mode ([`PaneFocus::List`]) the list
//!   sits on top, a one-row divider below it, and the detail fills the
//!   rest, so the session stays in view while you pick one. That divider
//!   can be dragged up and down; the chosen list height is a setting.
//!   With a session focused ([`PaneFocus::Detail`]) the detail fills the
//!   area. When the area is shorter than [`STACK_MIN_ROWS`] there's no
//!   room for both, and Rakumux mode shows the list alone.
//! - **Tabs**: one row of session tabs across the top; the selected tab's
//!   detail fills the rest. Works at any width.
//! - **Grid**: a bounded page of read-only live terminal cards. Session
//!   focus uses the full-screen terminal under tabs; no detail terminal is
//!   on screen while browsing, so it cannot claim any terminal's size.
//!
//! The layout is a user setting (`sessions.layout` in
//! `~/.rakumux/config.json`) and applies live. This module only computes
//! rectangles and draws the tab bar; the caller owns state, key routing and
//! the contents of each pane.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::Span;
use serde::{Deserialize, Serialize};

use crate::theme;

pub mod grid;

/// Screens narrower than this are phones (about 40–60 columns in portrait,
/// whatever the layout setting): one thing at a time. Rakumux mode shows
/// the session list alone; a session fills the screen under a row of tabs,
/// one per session. `Ctrl-\` goes back to the list.
pub const PHONE_BELOW: u16 = 80;

/// Screens narrower than this (and at least [`PHONE_BELOW`]) don't put the
/// Sidebar list beside the detail: the list is stacked above it in
/// Rakumux mode, and a focused session fills the area.
pub const SINGLE_PANE_BELOW: u16 = 100;

/// Fewest rows a stacked list gets: the tabs (three rows), a folder's
/// header (one) and two sessions (three rows each).
pub const LIST_MIN_ROWS: u16 = 10;
/// Fewest rows a stacked detail gets: four rows of the session.
pub const PREVIEW_MIN_ROWS: u16 = 4;
/// Shortest area that stacks the list above the detail on a narrow screen:
/// 15 rows, the list's and the detail's minimums and the divider (a
/// 16-row terminal, with the footer). Shorter ones show the list alone in
/// Rakumux mode.
pub const STACK_MIN_ROWS: u16 = LIST_MIN_ROWS + 1 + PREVIEW_MIN_ROWS;
/// Share of the area a stacked list gets until its divider is dragged, in
/// percent.
const STACKED_LIST_PERCENT: u32 = 40;

/// Sidebar column width at the single-pane threshold: room for the list's
/// tabs (`▤ Folders ▾   ◇ App   ◔ Status`).
const SIDEBAR_MIN_COLS: u16 = 40;
/// Sidebar column width once there's room, until 30% of the screen is
/// wider: the tabs with a filter's name, a branch or the text filter.
const SIDEBAR_COLS: u16 = 48;
/// Narrowest list a dragged divider leaves.
pub const LIST_MIN_COLS: u16 = 20;
/// Narrowest detail a dragged divider leaves.
pub const DETAIL_MIN_COLS: u16 = 40;

/// Longest session name drawn on a tab before it's cut with `…`.
pub const TAB_NAME_MAX: usize = 24;
/// Blank columns between two tabs.
const TAB_GAP: u16 = 1;
/// Columns taken by an overflow marker (`‹ ` or ` ›`).
const MARKER_COLS: u16 = 2;
const ELLIPSIS: &str = "…";

/// How sessions are arranged on screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionLayout {
    /// Session list on the left, detail on the right.
    #[default]
    Sidebar,
    /// Paged read-only live terminal cards; opening one enters session mode.
    Grid,
    /// A tab bar of sessions across the top, detail below.
    Tabs,
}

impl SessionLayout {
    /// Every layout, in settings-menu order.
    pub const ALL: [SessionLayout; 3] = [Self::Sidebar, Self::Grid, Self::Tabs];

    /// Name shown in the settings menu and help.
    pub fn label(self) -> &'static str {
        match self {
            SessionLayout::Sidebar => "Master detail",
            SessionLayout::Grid => "Grid",
            SessionLayout::Tabs => "Tabs",
        }
    }

    /// One-line explanation for the settings menu.
    pub fn description(self) -> &'static str {
        match self {
            SessionLayout::Sidebar => {
                "Session list on the left (on top when narrow), the session beside it"
            }
            SessionLayout::Tabs => "Sessions as tabs across the top, the selected one below",
            SessionLayout::Grid => {
                "Read-only live terminal cards; Enter opens a session, PgUp/PgDn change page"
            }
        }
    }

    /// The following layout, wrapping around (for a settings toggle).
    pub fn next(self) -> Self {
        match self {
            SessionLayout::Sidebar => SessionLayout::Grid,
            SessionLayout::Grid => SessionLayout::Tabs,
            SessionLayout::Tabs => SessionLayout::Sidebar,
        }
    }

    /// The following layout a screen offers: phones have no Grid.
    pub fn next_on(self, phone: bool) -> Self {
        match self.next() {
            SessionLayout::Grid if phone => SessionLayout::Grid.next(),
            next => next,
        }
    }

    /// The layouts a screen offers: phones have no Grid.
    pub fn offered(phone: bool) -> impl Iterator<Item = SessionLayout> {
        Self::ALL
            .into_iter()
            .filter(move |layout| !(phone && *layout == SessionLayout::Grid))
    }
}

/// Where the user is, which decides what a narrow screen shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum PaneFocus {
    /// The session list (or tab bar) has focus. A narrow Sidebar screen
    /// stacks the list above the detail (the list alone when it's short).
    #[default]
    List,
    /// A session has focus (keys go to it). A narrow Sidebar screen shows
    /// only the detail.
    Detail,
    /// `prefix z`: the session fills the whole area; no list, no tab bar.
    Zoomed,
}

/// Rectangles for one frame. Panes that aren't shown are `None`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LayoutAreas {
    /// Full-body session cards (Grid, Rakumux mode only).
    pub grid: Option<Rect>,
    /// Session list: a column on the left, or the top of a stacked screen
    /// (Sidebar only).
    pub list: Option<Rect>,
    /// The draggable line between the list and the detail (Sidebar only):
    /// one column between them side by side, or one row between them when
    /// `stacked`.
    pub divider: Option<Rect>,
    /// The list is above the detail (a narrow Sidebar screen in Rakumux
    /// mode), so the divider is a row, dragged up and down.
    pub stacked: bool,
    /// One-row tab bar (Tabs only).
    pub tabs: Option<Rect>,
    /// Selected session's detail. Zero width when hidden (a narrow Sidebar
    /// screen too short to stack, with the list focused); check
    /// `detail.is_empty()` before drawing or resizing a terminal to it.
    pub detail: Rect,
}

/// The pane sizes the user chose by dragging a divider; `None` sizes that
/// pane to the screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PaneSizes {
    /// Sidebar list width, side by side (see [`list_width`]).
    pub list_cols: Option<u16>,
    /// Sidebar list height, stacked: a share of the height in percent (see
    /// [`list_rows`]).
    pub list_percent: Option<u16>,
}

/// A phone-width screen ([`PHONE_BELOW`]): the list or a session, never
/// both.
pub fn is_phone(width: u16) -> bool {
    width < PHONE_BELOW
}

/// True when `layout` doesn't put the list beside the detail at this
/// width: it stacks them, or shows one.
pub fn is_single_pane(width: u16, layout: SessionLayout) -> bool {
    is_phone(width) || (layout == SessionLayout::Sidebar && width < SINGLE_PANE_BELOW)
}

/// True when a narrow Sidebar `area` is tall enough to stack the list
/// above the detail in Rakumux mode.
pub fn can_stack(area: Rect) -> bool {
    area.height >= STACK_MIN_ROWS
}

/// Width of the Sidebar list column for a screen `total` columns wide:
/// 40 columns at the single-pane threshold, growing to 48, then 30% of the
/// screen on wide terminals. Never wider than `total`.
pub fn sidebar_width(total: u16) -> u16 {
    let fixed = total
        .saturating_sub(SINGLE_PANE_BELOW - SIDEBAR_MIN_COLS)
        .clamp(SIDEBAR_MIN_COLS, SIDEBAR_COLS);
    fixed.max(total * 3 / 10).min(total)
}

/// List width for a screen `total` columns wide: `preferred` (a dragged
/// divider) kept between [`LIST_MIN_COLS`] and what leaves the detail
/// [`DETAIL_MIN_COLS`], or [`sidebar_width`] when there's no preference.
pub fn list_width(total: u16, preferred: Option<u16>) -> u16 {
    match preferred {
        Some(width) => {
            let max = total.saturating_sub(DETAIL_MIN_COLS + 1).max(LIST_MIN_COLS);
            width.clamp(LIST_MIN_COLS, max).min(total)
        }
        None => sidebar_width(total),
    }
}

/// Height of a stacked list in an area `total` rows tall: `percent` of it
/// (a dragged divider; 40% when there's none), kept between
/// [`LIST_MIN_ROWS`] and what leaves the detail [`PREVIEW_MIN_ROWS`]. A
/// share rather than rows, so the split follows the screen as a phone's
/// keyboard comes and goes. Never taller than `total`.
pub fn list_rows(total: u16, percent: Option<u16>) -> u16 {
    let rows = match percent {
        Some(percent) => (u32::from(total) * u32::from(percent.min(100)) + 50) / 100,
        None => u32::from(total) * STACKED_LIST_PERCENT / 100,
    };
    clamp_list_rows(total, u16::try_from(rows).unwrap_or(u16::MAX))
}

/// The share of `total` rows (percent) that a stacked list `rows` tall
/// takes, within [`list_rows`]' bounds: what dragging the divider saves.
pub fn list_percent(total: u16, rows: u16) -> u16 {
    let rows = u32::from(clamp_list_rows(total, rows));
    let total = u32::from(total.max(1));
    u16::try_from((rows * 100 + total / 2) / total).unwrap_or(100)
}

fn clamp_list_rows(total: u16, rows: u16) -> u16 {
    let max = total
        .saturating_sub(PREVIEW_MIN_ROWS + 1)
        .max(LIST_MIN_ROWS);
    rows.clamp(LIST_MIN_ROWS, max).min(total)
}

/// Split `area` (everything above the footer) for `layout`. `focus` picks
/// the panes on narrow screens and handles zoom.
#[cfg(test)]
pub fn split(area: Rect, layout: SessionLayout, focus: PaneFocus) -> LayoutAreas {
    split_with(area, layout, focus, PaneSizes::default())
}

/// [`split`] with the user's pane sizes (see [`list_width`] and
/// [`list_rows`]).
pub fn split_with(
    area: Rect,
    layout: SessionLayout,
    focus: PaneFocus,
    sizes: PaneSizes,
) -> LayoutAreas {
    // Only the detail (plus the tab bar in Tabs).
    let only = |detail: Rect| LayoutAreas {
        grid: None,
        list: None,
        divider: None,
        stacked: false,
        tabs: None,
        detail,
    };
    if focus == PaneFocus::Zoomed {
        return only(area);
    }
    // A tab bar row, then the detail.
    let tabs_over = |area: Rect| {
        let bar_height = area.height.min(1);
        LayoutAreas {
            tabs: Some(Rect {
                height: bar_height,
                ..area
            }),
            ..only(Rect {
                y: area.y + bar_height,
                height: area.height - bar_height,
                ..area
            })
        }
    };
    if layout == SessionLayout::Grid {
        return match focus {
            PaneFocus::List => LayoutAreas {
                grid: Some(area),
                ..only(hidden(area))
            },
            PaneFocus::Detail | PaneFocus::Zoomed => tabs_over(area),
        };
    }
    // A phone's Master detail is the list, then a session under tabs; its
    // Tabs keeps the tab bar over the session.
    if is_phone(area.width) && layout == SessionLayout::Sidebar {
        return match focus {
            PaneFocus::List => LayoutAreas {
                list: Some(area),
                ..only(hidden(area))
            },
            PaneFocus::Detail | PaneFocus::Zoomed => tabs_over(area),
        };
    }
    match layout {
        SessionLayout::Sidebar if is_single_pane(area.width, layout) => match focus {
            PaneFocus::List if can_stack(area) => {
                let rows = list_rows(area.height, sizes.list_percent);
                LayoutAreas {
                    grid: None,
                    list: Some(Rect {
                        height: rows,
                        ..area
                    }),
                    divider: Some(Rect {
                        y: area.y + rows,
                        height: 1,
                        ..area
                    }),
                    stacked: true,
                    tabs: None,
                    detail: Rect {
                        y: area.y + rows + 1,
                        height: area.height - rows - 1,
                        ..area
                    },
                }
            }
            PaneFocus::List => LayoutAreas {
                list: Some(area),
                ..only(hidden(area))
            },
            PaneFocus::Detail | PaneFocus::Zoomed => only(area),
        },
        SessionLayout::Sidebar => {
            let width = list_width(area.width, sizes.list_cols);
            LayoutAreas {
                list: Some(Rect { width, ..area }),
                divider: Some(Rect {
                    x: area.x + width,
                    width: 1,
                    ..area
                }),
                ..only(Rect {
                    x: area.x + width + 1,
                    width: area.width - width - 1,
                    ..area
                })
            }
        }
        SessionLayout::Tabs | SessionLayout::Grid => tabs_over(area),
    }
}

/// A zero-width rectangle at the right edge of `area`.
fn hidden(area: Rect) -> Rect {
    Rect {
        x: area.right(),
        width: 0,
        ..area
    }
}

/// Step `delta` places from `current` through `len` items, wrapping at both
/// ends (Option/Alt+↑/↓, `prefix >`/`<`). Returns 0 when `len` is 0.
pub fn cycle(len: usize, current: usize, delta: i32) -> usize {
    if len == 0 {
        return 0;
    }
    let len = len as i64;
    let current = current as i64 % len;
    (current + i64::from(delta)).rem_euclid(len) as usize
}

/// One tab in the tab bar.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TabItem {
    /// Session name; long names are cut with `…`.
    pub name: String,
    /// Status glyph drawn before the name (e.g. `●`); may be empty.
    pub glyph: String,
    /// Style for the glyph, usually just a foreground colour.
    pub glyph_style: Style,
    /// Draw this tab highlighted (normally the item at the `selected`
    /// index passed to [`render_tabs`]).
    pub selected: bool,
    /// The session wants a look (blocked or done); its name is emphasised.
    pub attention: bool,
    /// Pinned: a mark follows the name.
    pub pinned: bool,
    /// Optional activity marker after the name/pin, independent of selection.
    pub badge: Option<(String, Style)>,
}

/// Mark after a pinned session's name (tab bar and session list). One
/// column wide in every common terminal font.
pub const PIN_MARK: &str = "★";

/// Draw a one-row tab bar into the first row of `area` and return click
/// targets as `(item index, rect)`.
///
/// Tabs are numbered `1`–`9` (later ones are unnumbered). When they don't
/// all fit, the bar scrolls so the tab at `selected` is always visible and
/// shows `‹` / `›` where tabs are hidden. Those markers are click targets
/// too, for the nearest hidden tab on that side. The row's existing
/// background is kept, so the bar can sit inside a styled block.
pub fn render_tabs(
    area: Rect,
    buf: &mut Buffer,
    items: &[TabItem],
    selected: usize,
) -> Vec<(usize, Rect)> {
    let area = area.intersection(*buf.area());
    if area.width == 0 || area.height == 0 {
        return Vec::new();
    }
    let row = Rect { height: 1, ..area };
    for x in row.left()..row.right() {
        buf.get_mut(x, row.y).set_symbol(" ");
    }
    if items.is_empty() {
        return Vec::new();
    }

    let selected = selected.min(items.len() - 1);
    let mut labels: Vec<TabLabel> = items
        .iter()
        .enumerate()
        .map(|(index, item)| TabLabel::new(item, index, TAB_NAME_MAX))
        .collect();
    let widths: Vec<u16> = labels.iter().map(TabLabel::width).collect();
    let (start, end) = visible_window(&widths, selected, row.width);

    // A lone tab that's still too wide gets a shorter name.
    let room = row
        .width
        .saturating_sub(markers_width(start, end, items.len()));
    if labels[selected].width() > room {
        let excess = usize::from(labels[selected].width() - room);
        let name_max = str_width(&labels[selected].name).saturating_sub(excess);
        labels[selected] = TabLabel::new(&items[selected], selected, name_max);
    }

    let palette = theme::palette();
    let marker_style = Style::default()
        .fg(palette.accent_primary)
        .add_modifier(Modifier::BOLD);
    let mut hits = Vec::with_capacity(end - start + 2);
    let mut x = row.x;

    if start > 0 {
        buf.set_stringn(x, row.y, "‹", 1, marker_style);
        hits.push((start - 1, Rect { width: 1, ..row }));
        x += MARKER_COLS;
    }
    let tabs_right = if end < items.len() {
        row.right().saturating_sub(MARKER_COLS)
    } else {
        row.right()
    };
    for (index, label) in labels.iter().enumerate().take(end).skip(start) {
        if x >= tabs_right {
            break;
        }
        let drawn = label.draw(buf, x, row.y, tabs_right - x, &items[index]);
        hits.push((
            index,
            Rect {
                x,
                width: drawn,
                ..row
            },
        ));
        x = x.saturating_add(drawn + TAB_GAP);
    }
    if end < items.len() {
        let marker_x = row.right() - 1;
        buf.set_stringn(marker_x, row.y, "›", 1, marker_style);
        hits.push((
            end,
            Rect {
                x: marker_x,
                width: 1,
                ..row
            },
        ));
    }
    hits
}

/// The item under a mouse position, from the targets [`render_tabs`]
/// returned.
pub fn tab_at(hits: &[(usize, Rect)], column: u16, row: u16) -> Option<usize> {
    hits.iter()
        .find(|(_, rect)| {
            column >= rect.left()
                && column < rect.right()
                && row >= rect.top()
                && row < rect.bottom()
        })
        .map(|(index, _)| *index)
}

/// Text pieces of one tab: ` 1 ● name ★ `.
struct TabLabel {
    number: Option<String>,
    glyph: String,
    name: String,
    pinned: bool,
    badge: Option<String>,
}

impl TabLabel {
    fn new(item: &TabItem, index: usize, name_max: usize) -> Self {
        Self {
            number: (index < 9).then(|| (index + 1).to_string()),
            glyph: item.glyph.clone(),
            name: truncate(&item.name, name_max),
            pinned: item.pinned,
            badge: item.badge.as_ref().map(|(glyph, _)| glyph.clone()),
        }
    }

    fn width(&self) -> u16 {
        let mut width = 2 + str_width(&self.name); // padding on both sides
        if let Some(number) = &self.number {
            width += str_width(number) + 1;
        }
        if !self.glyph.is_empty() {
            width += str_width(&self.glyph) + 1;
        }
        if self.pinned {
            width += str_width(PIN_MARK) + 1;
        }
        if let Some(badge) = &self.badge {
            width += str_width(badge) + 1;
        }
        u16::try_from(width).unwrap_or(u16::MAX)
    }

    /// Draw at `(x, y)` within `max` columns; returns the columns used.
    fn draw(&self, buf: &mut Buffer, x: u16, y: u16, max: u16, item: &TabItem) -> u16 {
        let palette = theme::palette();
        let (base, number_style, name_style) = if item.selected {
            let base = Style::default().bg(palette.surface);
            let accent = base.fg(palette.accent_primary).add_modifier(Modifier::BOLD);
            let name = base.fg(palette.text_bright).add_modifier(Modifier::BOLD);
            (base, accent, name)
        } else if item.attention {
            let base = Style::default();
            (
                base,
                base.fg(palette.text_muted),
                base.fg(palette.accent_orange).add_modifier(Modifier::BOLD),
            )
        } else {
            let base = Style::default();
            (
                base,
                base.fg(palette.text_muted),
                base.fg(palette.text_primary).add_modifier(Modifier::BOLD),
            )
        };

        let mut pieces: Vec<(&str, Style)> = vec![(" ", base)];
        if let Some(number) = &self.number {
            pieces.push((number, number_style));
            pieces.push((" ", base));
        }
        if !self.glyph.is_empty() {
            pieces.push((&self.glyph, base.patch(item.glyph_style)));
            pieces.push((" ", base));
        }
        pieces.push((&self.name, name_style));
        if self.pinned {
            pieces.push((" ", base));
            pieces.push((PIN_MARK, base.fg(palette.accent_gold)));
        }
        if let Some((badge, style)) = &item.badge {
            pieces.push((" ", base));
            pieces.push((badge, base.patch(*style)));
        }
        pieces.push((" ", base));

        let end = x.saturating_add(max);
        let mut cursor = x;
        for (text, style) in pieces {
            if cursor >= end {
                break;
            }
            let (next, _) = buf.set_stringn(cursor, y, text, usize::from(end - cursor), style);
            cursor = next;
        }
        cursor - x
    }
}

/// Which tabs `[start, end)` to show so `selected` is visible within
/// `avail` columns, packing from the left.
fn visible_window(widths: &[u16], selected: usize, avail: u16) -> (usize, usize) {
    let len = widths.len();
    let fits = |start: usize, end: usize| needed_width(widths, start, end) <= u32::from(avail);
    if fits(0, len) {
        return (0, len);
    }
    let mut start = 0;
    while start < selected && !fits(start, selected + 1) {
        start += 1;
    }
    let mut end = selected + 1;
    while end < len && fits(start, end + 1) {
        end += 1;
    }
    (start, end)
}

/// Columns needed to show tabs `[start, end)` with their overflow markers.
fn needed_width(widths: &[u16], start: usize, end: usize) -> u32 {
    let tabs: u32 = widths[start..end].iter().map(|w| u32::from(*w)).sum();
    let gaps = u32::from(TAB_GAP) * (end - start).saturating_sub(1) as u32;
    tabs + gaps + u32::from(markers_width(start, end, widths.len()))
}

fn markers_width(start: usize, end: usize, len: usize) -> u16 {
    let left = if start > 0 { MARKER_COLS } else { 0 };
    let right = if end < len { MARKER_COLS } else { 0 };
    left + right
}

/// Display width of `text` in terminal columns.
pub(crate) fn str_width(text: &str) -> usize {
    Span::raw(text).width()
}

/// Display width of one character.
pub(crate) fn char_width(ch: char) -> usize {
    let mut utf8 = [0u8; 4];
    str_width(ch.encode_utf8(&mut utf8))
}

/// `text` cut to at most `max` columns, ending in `…` when cut.
pub(crate) fn truncate(text: &str, max: usize) -> String {
    if str_width(text) <= max {
        return text.to_string();
    }
    if max == 0 {
        return String::new();
    }
    let budget = max - str_width(ELLIPSIS);
    let mut out = String::new();
    let mut used = 0;
    for ch in text.chars() {
        let width = char_width(ch);
        if used + width > budget {
            break;
        }
        out.push(ch);
        used += width;
    }
    out.push_str(ELLIPSIS);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::style::Color;

    fn rect(x: u16, y: u16, width: u16, height: u16) -> Rect {
        Rect {
            x,
            y,
            width,
            height,
        }
    }

    fn row_text(buf: &Buffer, y: u16) -> String {
        let area = buf.area();
        (area.left()..area.right())
            .map(|x| buf.get(x, y).symbol().to_string())
            .collect()
    }

    fn tab(name: &str) -> TabItem {
        TabItem {
            name: name.to_string(),
            glyph: "●".to_string(),
            glyph_style: Style::default().fg(Color::Green),
            ..TabItem::default()
        }
    }

    fn tabs(names: &[&str], selected: usize) -> Vec<TabItem> {
        names
            .iter()
            .enumerate()
            .map(|(i, name)| TabItem {
                selected: i == selected,
                ..tab(name)
            })
            .collect()
    }

    // --- SessionLayout -------------------------------------------------

    #[test]
    fn layout_defaults_to_sidebar_and_serialises_snake_case() {
        assert_eq!(SessionLayout::default(), SessionLayout::Sidebar);
        assert_eq!(
            serde_json::to_string(&SessionLayout::Sidebar).unwrap(),
            "\"sidebar\""
        );
        assert_eq!(
            serde_json::to_string(&SessionLayout::Tabs).unwrap(),
            "\"tabs\""
        );
        let parsed: SessionLayout = serde_json::from_str("\"tabs\"").unwrap();
        assert_eq!(parsed, SessionLayout::Tabs);
        assert_eq!(
            serde_json::from_str::<SessionLayout>("\"grid\"").unwrap(),
            SessionLayout::Grid
        );
        assert_eq!(
            serde_json::to_string(&SessionLayout::Grid).unwrap(),
            "\"grid\""
        );
    }

    #[test]
    fn layout_next_wraps_and_labels_are_distinct() {
        assert_eq!(SessionLayout::Sidebar.next(), SessionLayout::Grid);
        assert_eq!(SessionLayout::Grid.next(), SessionLayout::Tabs);
        assert_eq!(SessionLayout::Tabs.next(), SessionLayout::Sidebar);
        assert_eq!(SessionLayout::Sidebar.label(), "Master detail");
        assert_eq!(SessionLayout::Tabs.label(), "Tabs");
        for layout in SessionLayout::ALL {
            assert!(!layout.description().is_empty());
        }
    }

    // --- split ----------------------------------------------------------

    #[test]
    fn sidebar_width_follows_the_documented_curve() {
        assert_eq!(sidebar_width(100), 40);
        assert_eq!(sidebar_width(103), 43);
        assert_eq!(sidebar_width(108), 48);
        assert_eq!(sidebar_width(120), 48);
        assert_eq!(sidebar_width(160), 48);
        assert_eq!(sidebar_width(170), 51);
        assert_eq!(sidebar_width(240), 72);
        assert_eq!(sidebar_width(20), 20);
    }

    #[test]
    fn sidebar_wide_splits_list_and_detail_side_by_side() {
        let area = rect(0, 1, 160, 46);
        for focus in [PaneFocus::List, PaneFocus::Detail] {
            let areas = split(area, SessionLayout::Sidebar, focus);
            assert_eq!(areas.list, Some(rect(0, 1, 48, 46)));
            assert_eq!(areas.divider, Some(rect(48, 1, 1, 46)));
            assert_eq!(areas.tabs, None);
            assert_eq!(areas.detail, rect(49, 1, 111, 46));
        }
    }

    #[test]
    fn a_dragged_list_width_is_kept_within_bounds() {
        let area = rect(0, 1, 160, 46);
        let at = |width| {
            let sizes = PaneSizes {
                list_cols: Some(width),
                // A stacked height changes nothing side by side.
                list_percent: Some(70),
            };
            split_with(area, SessionLayout::Sidebar, PaneFocus::List, sizes)
        };
        assert!(!at(70).stacked);
        assert_eq!(at(70).list, Some(rect(0, 1, 70, 46)));
        assert_eq!(at(70).detail, rect(71, 1, 89, 46));
        assert_eq!(at(5).list.unwrap().width, LIST_MIN_COLS);
        assert_eq!(at(150).detail.width, DETAIL_MIN_COLS);
        assert_eq!(list_width(160, None), sidebar_width(160));
    }

    #[test]
    fn sidebar_at_threshold_and_offset_origin() {
        let areas = split(rect(5, 2, 100, 30), SessionLayout::Sidebar, PaneFocus::List);
        assert_eq!(areas.list, Some(rect(5, 2, 40, 30)));
        assert_eq!(areas.detail, rect(46, 2, 59, 30));
    }

    #[test]
    fn sidebar_narrow_stacks_the_list_above_the_detail() {
        assert!(is_single_pane(80, SessionLayout::Sidebar));
        assert!(is_single_pane(99, SessionLayout::Sidebar));
        assert!(!is_single_pane(100, SessionLayout::Sidebar));

        // 90×40, a narrow window: 39 rows above the footer, 40% for the list.
        let area = rect(0, 0, 90, 39);
        let list = split(area, SessionLayout::Sidebar, PaneFocus::List);
        assert!(list.stacked);
        assert_eq!(list.list, Some(rect(0, 0, 90, 15)));
        assert_eq!(list.divider, Some(rect(0, 15, 90, 1)));
        assert_eq!(list.detail, rect(0, 16, 90, 23));
        assert_eq!(list.tabs, None);

        // A session fills the area.
        let detail = split(area, SessionLayout::Sidebar, PaneFocus::Detail);
        assert_eq!(detail.list, None);
        assert_eq!(detail.divider, None);
        assert!(!detail.stacked);
        assert_eq!(detail.detail, area);

        // Offset origins and other narrow sizes: the panes tile the area.
        for (width, height) in [(80, 19), (85, 19), (90, 29), (95, 23), (99, 29)] {
            let area = rect(3, 2, width, height);
            let split = split(area, SessionLayout::Sidebar, PaneFocus::List);
            let (list, divider) = (split.list.unwrap(), split.divider.unwrap());
            assert!(split.stacked);
            assert_eq!((list.x, list.y, list.width), (3, 2, width));
            assert!(list.height >= LIST_MIN_ROWS, "{width}x{height}");
            assert_eq!((divider.y, divider.height), (list.bottom(), 1));
            assert_eq!(split.detail.y, divider.bottom());
            assert_eq!(split.detail.bottom(), area.bottom());
            assert!(split.detail.height >= PREVIEW_MIN_ROWS, "{width}x{height}");
            assert_eq!(split.detail.width, width);
        }
    }

    #[test]
    fn phones_show_the_list_or_a_session_under_tabs() {
        for layout in SessionLayout::ALL {
            for (width, height) in [(40, 19), (45, 39), (59, 52), (79, 30)] {
                assert!(is_phone(width) && is_single_pane(width, layout));
                let area = rect(0, 0, width, height);
                // Rakumux mode: Master detail's list alone (Grid's cards,
                // which the app doesn't offer on phones); Tabs keeps its
                // tab bar over the session.
                let list = split(area, layout, PaneFocus::List);
                match layout {
                    SessionLayout::Grid => {
                        assert_eq!(list.grid, Some(area));
                        assert_eq!(list.list, None);
                    }
                    SessionLayout::Sidebar => {
                        assert_eq!(list.list, Some(area));
                        assert_eq!(list.grid, None);
                    }
                    SessionLayout::Tabs => {
                        assert_eq!(list.list, None);
                        assert_eq!(list.tabs, Some(rect(0, 0, width, 1)));
                        assert_eq!(list.detail, rect(0, 1, width, height - 1));
                    }
                }
                assert_eq!((list.divider, list.stacked), (None, false));
                if layout != SessionLayout::Tabs {
                    assert_eq!(list.tabs, None);
                    assert!(list.detail.is_empty());
                }
                // A session: a row of tabs, the session below.
                let detail = split(area, layout, PaneFocus::Detail);
                assert_eq!(detail.list, None);
                assert_eq!(detail.tabs, Some(rect(0, 0, width, 1)));
                assert_eq!(detail.detail, rect(0, 1, width, height - 1));
            }
        }
        assert!(!is_phone(PHONE_BELOW));
    }

    #[test]
    fn a_short_narrow_screen_shows_the_list_alone() {
        let area = rect(0, 1, 90, STACK_MIN_ROWS - 1);
        assert!(!can_stack(area));
        let list = split(area, SessionLayout::Sidebar, PaneFocus::List);
        assert_eq!(list.list, Some(area));
        assert_eq!(list.divider, None);
        assert!(!list.stacked);
        assert!(list.detail.is_empty());
        assert_eq!(list.detail.x, area.right());
        let detail = split(area, SessionLayout::Sidebar, PaneFocus::Detail);
        assert_eq!(detail.list, None);
        assert_eq!(detail.detail, area);

        // At the threshold both minimums fit.
        let area = rect(0, 0, 90, STACK_MIN_ROWS);
        let list = split(area, SessionLayout::Sidebar, PaneFocus::List);
        assert!(list.stacked);
        assert_eq!(list.list.unwrap().height, LIST_MIN_ROWS);
        assert_eq!(list.detail.height, PREVIEW_MIN_ROWS);
    }

    #[test]
    fn a_dragged_list_height_is_kept_within_bounds() {
        assert_eq!(list_rows(39, None), 15);
        assert_eq!(list_rows(19, None), LIST_MIN_ROWS);
        assert_eq!(list_rows(39, Some(64)), 25);
        assert_eq!(list_rows(39, Some(5)), LIST_MIN_ROWS);
        assert_eq!(list_rows(39, Some(100)), 39 - 1 - PREVIEW_MIN_ROWS);
        assert_eq!(list_rows(STACK_MIN_ROWS, Some(30)), LIST_MIN_ROWS);
        // Never taller than the area, even one too short to stack.
        assert_eq!(list_rows(5, None), 5);
        // A share follows the height: the keyboard coming up keeps the split.
        assert_eq!(list_rows(19, Some(64)), 12);
        // What a drag saves gives the dragged height back, within bounds.
        for rows in LIST_MIN_ROWS..=(39 - 1 - PREVIEW_MIN_ROWS) {
            assert_eq!(list_rows(39, Some(list_percent(39, rows))), rows);
        }
        assert_eq!(list_percent(39, 1), list_percent(39, LIST_MIN_ROWS));

        let area = rect(0, 0, 90, 39);
        let sizes = PaneSizes {
            list_cols: Some(70),
            list_percent: Some(64),
        };
        let split = split_with(area, SessionLayout::Sidebar, PaneFocus::List, sizes);
        assert_eq!(split.list, Some(rect(0, 0, 90, 25)));
        assert_eq!(split.detail, rect(0, 26, 90, 13));
    }

    #[test]
    fn tabs_layout_uses_one_row_at_any_width() {
        for width in [80, 120, 160] {
            assert!(!is_single_pane(width, SessionLayout::Tabs));
            let area = rect(0, 1, width, 22);
            for focus in [PaneFocus::List, PaneFocus::Detail] {
                let areas = split(area, SessionLayout::Tabs, focus);
                assert_eq!(areas.list, None);
                assert_eq!(areas.tabs, Some(rect(0, 1, width, 1)));
                assert_eq!(areas.detail, rect(0, 2, width, 21));
            }
        }
    }

    #[test]
    fn tabs_layout_survives_tiny_heights() {
        let one = split(rect(0, 0, 90, 1), SessionLayout::Tabs, PaneFocus::List);
        assert_eq!(one.tabs, Some(rect(0, 0, 90, 1)));
        assert!(one.detail.is_empty());
        let zero = split(rect(0, 0, 90, 0), SessionLayout::Tabs, PaneFocus::List);
        assert_eq!(zero.tabs, Some(rect(0, 0, 90, 0)));
        assert!(zero.detail.is_empty());
    }

    #[test]
    fn zoom_gives_the_detail_everything() {
        let area = rect(0, 0, 160, 48);
        for layout in SessionLayout::ALL {
            let areas = split(area, layout, PaneFocus::Zoomed);
            assert_eq!(areas.list, None);
            assert_eq!(areas.tabs, None);
            assert_eq!(areas.detail, area);
        }
    }

    // --- cycle ----------------------------------------------------------

    #[test]
    fn cycle_wraps_both_ways() {
        assert_eq!(cycle(3, 0, 1), 1);
        assert_eq!(cycle(3, 2, 1), 0);
        assert_eq!(cycle(3, 0, -1), 2);
        assert_eq!(cycle(3, 1, -1), 0);
        assert_eq!(cycle(3, 1, 7), 2);
        assert_eq!(cycle(3, 1, -7), 0);
        assert_eq!(cycle(1, 0, 1), 0);
        assert_eq!(cycle(1, 0, -1), 0);
    }

    #[test]
    fn cycle_handles_empty_and_stale_indices() {
        assert_eq!(cycle(0, 0, 1), 0);
        assert_eq!(cycle(0, 5, -1), 0);
        // An index past the end (list shrank) is taken modulo the length.
        assert_eq!(cycle(3, 4, 0), 1);
        assert_eq!(cycle(3, 4, 1), 2);
    }

    // --- truncate -------------------------------------------------------

    #[test]
    fn truncate_adds_ellipsis_only_when_needed() {
        assert_eq!(truncate("short", 10), "short");
        assert_eq!(truncate("exactly-10", 10), "exactly-10");
        assert_eq!(truncate("a-much-longer-name", 8), "a-much-…");
        assert_eq!(str_width(&truncate("a-much-longer-name", 8)), 8);
        assert_eq!(truncate("abc", 1), "…");
        assert_eq!(truncate("abc", 0), "");
        // Wide characters count as two columns.
        assert_eq!(truncate("日本語の名前", 5), "日本…");
    }

    // --- render_tabs ----------------------------------------------------

    #[test]
    fn tabs_that_fit_are_numbered_with_hit_areas() {
        let area = rect(0, 0, 60, 1);
        let mut buf = Buffer::empty(area);
        let items = tabs(&["api", "docs", "tests"], 1);
        let hits = render_tabs(area, &mut buf, &items, 1);

        assert_eq!(
            row_text(&buf, 0).trim_end(),
            " 1 ● api   2 ● docs   3 ● tests"
        );
        assert_eq!(
            hits,
            vec![
                (0, rect(0, 0, 9, 1)),
                (1, rect(10, 0, 10, 1)),
                (2, rect(21, 0, 11, 1)),
            ]
        );
        assert_eq!(tab_at(&hits, 12, 0), Some(1));
        assert_eq!(tab_at(&hits, 9, 0), None); // the gap
        assert_eq!(tab_at(&hits, 40, 0), None);
        assert_eq!(tab_at(&hits, 12, 1), None);
    }

    #[test]
    fn selected_tab_is_highlighted_and_glyph_keeps_its_colour() {
        let area = rect(0, 0, 60, 1);
        let mut buf = Buffer::empty(area);
        let items = tabs(&["api", "docs"], 1);
        render_tabs(area, &mut buf, &items, 1);

        let palette = theme::palette();
        let selected_name = buf.get(15, 0); // 'd' of "docs"
        assert_eq!(selected_name.symbol(), "d");
        assert_eq!(selected_name.bg, palette.surface);
        assert_eq!(selected_name.fg, palette.text_bright);
        assert!(selected_name.modifier.contains(Modifier::BOLD));
        let glyph = buf.get(13, 0);
        assert_eq!(glyph.symbol(), "●");
        assert_eq!(glyph.fg, Color::Green);
        assert_eq!(glyph.bg, palette.surface);
        let other = buf.get(5, 0); // 'a' of "api"
        assert_eq!(other.symbol(), "a");
        assert_ne!(other.bg, palette.surface);
        assert_eq!(other.fg, palette.text_primary);
        assert!(other.modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn attention_tabs_are_emphasised() {
        let area = rect(0, 0, 40, 1);
        let mut buf = Buffer::empty(area);
        let mut items = tabs(&["api", "docs"], 0);
        items[1].attention = true;
        render_tabs(area, &mut buf, &items, 0);
        let name = buf.get(15, 0); // 'd' of "docs"
        assert_eq!(name.symbol(), "d");
        assert_eq!(name.fg, theme::palette().accent_orange);
        assert!(name.modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn pinned_tabs_carry_a_mark() {
        let area = rect(0, 0, 60, 1);
        let mut buf = Buffer::empty(area);
        let mut items = tabs(&["api", "docs"], 0);
        items[0].pinned = true;
        let hits = render_tabs(area, &mut buf, &items, 0);
        assert_eq!(row_text(&buf, 0).trim_end(), " 1 ● api ★   2 ● docs");
        assert_eq!(hits[0].1.width, 11);
        assert_eq!(buf.get(9, 0).fg, theme::palette().accent_gold);
    }

    #[test]
    fn trailing_activity_badges_survive_long_tab_names_and_keep_their_state_colour() {
        let palette = theme::palette();
        for width in [12, 24, 40, 80] {
            for selected in [false, true] {
                let area = rect(0, 0, width, 1);
                let mut buf = Buffer::empty(area);
                let item = TabItem {
                    name: "A very long 界 session title".into(),
                    selected,
                    pinned: true,
                    badge: Some(("●".into(), Style::default().fg(palette.accent_gold))),
                    ..Default::default()
                };
                let hits = render_tabs(area, &mut buf, &[item], 0);
                let text = row_text(&buf, 0);
                assert!(text.trim_end().ends_with("★ ●"), "{text:?}");
                let badge = (0..width).find(|&x| buf.get(x, 0).symbol() == "●").unwrap();
                assert_eq!(buf.get(badge, 0).fg, palette.accent_gold);
                assert!(hits[0].1.right() <= area.right());
            }
        }
    }

    #[test]
    fn the_bar_keeps_the_background_it_is_drawn_on() {
        let area = rect(0, 0, 40, 1);
        let mut buf = Buffer::empty(area);
        buf.set_style(area, Style::default().bg(Color::Blue));
        buf.set_string(0, 0, "x".repeat(40), Style::default());
        render_tabs(area, &mut buf, &tabs(&["api", "docs"], 0), 0);
        // Gap between tabs, an unselected tab, and the empty rest of the row.
        for x in [9, 12, 30] {
            assert_eq!(buf.get(x, 0).bg, Color::Blue, "column {x}");
        }
        assert_eq!(buf.get(30, 0).symbol(), " ");
    }

    #[test]
    fn long_names_get_an_ellipsis() {
        let area = rect(0, 0, 80, 1);
        let mut buf = Buffer::empty(area);
        let long = "an-extremely-long-session-name-that-goes-on";
        let items = tabs(&[long], 0);
        let hits = render_tabs(area, &mut buf, &items, 0);
        let text = row_text(&buf, 0);
        let expected = format!(" 1 ● {} ", truncate(long, TAB_NAME_MAX));
        assert!(text.starts_with(&expected), "{text:?}");
        assert!(text.contains('…'));
        assert_eq!(hits[0].1.width as usize, str_width(&expected));
    }

    #[test]
    fn tabs_after_the_ninth_are_unnumbered() {
        let names: Vec<String> = (1..=10).map(|i| format!("s{i}")).collect();
        let refs: Vec<&str> = names.iter().map(String::as_str).collect();
        let area = rect(0, 0, 200, 1);
        let mut buf = Buffer::empty(area);
        render_tabs(area, &mut buf, &tabs(&refs, 0), 0);
        let text = row_text(&buf, 0);
        assert!(text.contains(" 9 ● s9 "), "{text:?}");
        assert!(text.contains(" ● s10 "), "{text:?}");
        assert!(!text.contains("10 ● s10"), "{text:?}");
    }

    #[test]
    fn overflow_scrolls_to_keep_the_selected_tab_visible() {
        let names: Vec<String> = (1..=8).map(|i| format!("session-{i}")).collect();
        let refs: Vec<&str> = names.iter().map(String::as_str).collect();
        // Each tab is " n ● session-n " = 15 columns.
        let area = rect(0, 0, 50, 1);

        // First tab selected: no left marker, right marker present.
        let mut buf = Buffer::empty(area);
        let hits = render_tabs(area, &mut buf, &tabs(&refs, 0), 0);
        let text = row_text(&buf, 0);
        assert!(text.starts_with(" 1 ● session-1"), "{text:?}");
        assert!(!text.contains('‹'), "{text:?}");
        assert_eq!(text.chars().last(), Some('›'));
        let visible: Vec<usize> = hits.iter().map(|(i, _)| *i).collect();
        assert_eq!(visible, vec![0, 1, 2, 3]); // three tabs + the › target for tab 3

        // Last tab selected: left marker, no right marker, tab 8 drawn.
        let mut buf = Buffer::empty(area);
        let hits = render_tabs(area, &mut buf, &tabs(&refs, 7), 7);
        let text = row_text(&buf, 0);
        assert!(text.starts_with('‹'), "{text:?}");
        assert!(!text.contains('›'), "{text:?}");
        assert!(text.contains("8 ● session-8"), "{text:?}");
        let (first_index, first_rect) = hits[0];
        assert_eq!(first_rect, rect(0, 0, 1, 1));
        assert!(first_index < 7);
        assert!(hits.iter().any(|(i, r)| *i == 7 && r.right() <= 50));

        // Middle tab selected: both markers, selected visible.
        let mut buf = Buffer::empty(area);
        let hits = render_tabs(area, &mut buf, &tabs(&refs, 4), 4);
        let text = row_text(&buf, 0);
        assert!(text.starts_with('‹'), "{text:?}");
        assert!(text.ends_with('›'), "{text:?}");
        assert!(text.contains("5 ● session-5"), "{text:?}");
        assert!(hits.iter().any(|(i, r)| *i == 4 && r.width == 15));
    }

    #[test]
    fn every_selection_stays_visible_and_inside_the_area() {
        let names: Vec<String> = (1..=12).map(|i| format!("name-{i:02}-x")).collect();
        let refs: Vec<&str> = names.iter().map(String::as_str).collect();
        for width in [12u16, 20, 37, 64, 100] {
            let area = rect(3, 2, width, 1);
            for selected in 0..refs.len() {
                let mut buf = Buffer::empty(rect(0, 0, 120, 4));
                let hits = render_tabs(area, &mut buf, &tabs(&refs, selected), selected);
                assert!(
                    hits.iter().any(|(i, r)| *i == selected && r.width > 1),
                    "width {width} selected {selected}: {hits:?}"
                );
                for (_, r) in &hits {
                    assert!(r.left() >= area.left() && r.right() <= area.right());
                    assert_eq!(r.y, 2);
                }
                // Nothing drawn outside the bar's row and columns.
                for x in 0..120 {
                    for y in [0, 1, 3] {
                        assert_eq!(buf.get(x, y).symbol(), " ");
                    }
                    if !(3..3 + width).contains(&x) {
                        assert_eq!(buf.get(x, 2).symbol(), " ");
                    }
                }
            }
        }
    }

    #[test]
    fn a_tab_wider_than_the_bar_is_shortened() {
        let area = rect(0, 0, 14, 1);
        let mut buf = Buffer::empty(area);
        let items = tabs(&["first", "a-really-long-session-name"], 1);
        let hits = render_tabs(area, &mut buf, &items, 1);
        let text = row_text(&buf, 0);
        assert!(text.starts_with('‹'), "{text:?}");
        assert!(text.contains('…'), "{text:?}");
        let (index, r) = hits.iter().find(|(i, _)| *i == 1).copied().unwrap();
        assert_eq!(index, 1);
        assert_eq!(r.right(), 14);
    }

    #[test]
    fn empty_glyph_and_empty_items() {
        let area = rect(0, 0, 30, 1);
        let mut buf = Buffer::empty(area);
        let items = vec![TabItem {
            name: "Keys".into(),
            selected: true,
            ..TabItem::default()
        }];
        let hits = render_tabs(area, &mut buf, &items, 0);
        assert_eq!(row_text(&buf, 0).trim_end(), " 1 Keys");
        assert_eq!(hits, vec![(0, rect(0, 0, 8, 1))]);

        let mut buf = Buffer::empty(area);
        assert!(render_tabs(area, &mut buf, &[], 0).is_empty());
        assert!(render_tabs(rect(0, 0, 0, 1), &mut buf, &items, 0).is_empty());
        // A stale selected index is clamped, not a panic.
        let hits = render_tabs(area, &mut buf, &items, 9);
        assert_eq!(hits.len(), 1);
    }
}
