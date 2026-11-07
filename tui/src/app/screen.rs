//! Screen geometry: where the session list or tab bar, detail pane,
//! terminal and footer go for a given size, layout, focus and zoom. Shared
//! by drawing and by the app (terminal size for attach/resize, mouse hits).
//!
//! The footer is always the last row, in every layout and in zoom, so the
//! mode label never leaves it and nothing above it shifts when the mode, the
//! selection or a message changes.
//!
//! The detail is the session's terminal alone, with no title row: the
//! footer names the session in view and its status.
//!
//! Phones (under [`layout::PHONE_BELOW`] columns) show one thing at a
//! time: the list in Rakumux mode, the session under a row of tabs in
//! session mode. Narrow Sidebar screens up to
//! [`layout::SINGLE_PANE_BELOW`] stack the list above the session in
//! Rakumux mode, and give the session everything above the footer in
//! session mode; see [`crate::layout`].

use ratatui::layout::Rect;

use super::types::Focus;
use crate::layout::{self, PaneFocus, PaneSizes, PanelPlacement, SessionLayout};

/// The session, messages, the mode label and the keys for Help and Settings;
/// every other key is listed in Settings.
const FOOTER_ROWS: u16 = 1;
/// Terminal sizes below this make no sense to request.
const MIN_TERMINAL: u16 = 2;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Screen {
    /// Session list (Sidebar).
    pub list: Option<Rect>,
    /// Draggable line between the list and the detail (Sidebar): a column
    /// side by side, a row when `stacked`.
    pub divider: Option<Rect>,
    /// The list is above the detail (a narrow Sidebar screen in Rakumux
    /// mode).
    pub stacked: bool,
    /// The hidden list's rail at the left edge (Sidebar side by side): a
    /// click shows the list again.
    pub rail: Option<Rect>,
    /// Session tab bar (Tabs).
    pub tabs: Option<Rect>,
    /// Detail pane: the session's terminal.
    pub detail: Option<Rect>,
    /// Where the terminal view draws.
    pub terminal: Option<Rect>,
    /// Independent shell panel below or beside the detail; includes its
    /// title row.
    pub terminal_panel: Option<Rect>,
    /// The line between the detail and the panel: a row below, a column
    /// beside.
    pub terminal_panel_divider: Option<Rect>,
    /// The panel is right of the detail (its divider is a column).
    pub terminal_panel_beside: bool,
    /// The agent feed: a docked pane on the right (Master detail), the
    /// session's place (narrower windows, in Rakumux mode), the body under
    /// the tab bar (Tabs, its tab selected) or the screen (a phone).
    pub feed: Option<Rect>,
    /// The draggable line left of a docked feed pane.
    pub feed_divider: Option<Rect>,
    /// The last row: the session, messages, the mode label, keys. Shown in
    /// zoom too.
    pub footer: Option<Rect>,
}

/// Where everything goes on a screen of `area`, with the user's dragged
/// pane `sizes`.
pub fn compute(
    area: Rect,
    session_layout: SessionLayout,
    focus: Focus,
    zoomed: bool,
    sizes: PaneSizes,
) -> Screen {
    let mut screen = Screen::default();
    if area.width == 0 || area.height == 0 {
        return screen;
    }
    let footer_rows = FOOTER_ROWS.min(area.height);
    screen.footer = Some(Rect {
        y: area.y + area.height - footer_rows,
        height: footer_rows,
        ..area
    });
    let body = Rect {
        height: area.height - footer_rows,
        ..area
    };
    if body.height == 0 {
        return screen;
    }
    // Zoom: the session gets everything above the footer.
    if zoomed && focus == Focus::Session {
        screen.terminal = Some(body);
        return screen;
    }

    let pane_focus = match focus {
        Focus::List => PaneFocus::List,
        Focus::Session | Focus::Terminal => PaneFocus::Detail,
    };
    let areas = layout::split_with(body, session_layout, pane_focus, sizes);
    screen.list = areas.list;
    screen.divider = areas.divider;
    screen.stacked = areas.stacked;
    screen.rail = areas.rail;
    screen.tabs = areas.tabs;
    screen.feed = areas.feed;
    screen.feed_divider = areas.feed_divider;
    // A zero-width detail means a short narrow Sidebar shows the list
    // alone.
    if !areas.detail.is_empty() {
        // The footer names the session; every row is its terminal's.
        screen.detail = Some(areas.detail);
        screen.terminal = Some(areas.detail);
    }
    screen
}

/// What decides where the terminal panel goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PanelGeometry {
    pub open: bool,
    /// The panel has the keys (short screens show it alone then).
    pub focused: bool,
    /// Below or beside, as the user last placed it.
    pub placement: PanelPlacement,
    /// Its share of the detail's height below, in percent.
    pub height_percent: u16,
    /// Its share of the detail's width beside, in percent.
    pub width_percent: u16,
}

/// Split only the detail, never the sidebar, the feed or the footer.
/// Beside goes right of the session where the detail holds both, the
/// session [`layout::PANEL_BESIDE_SESSION_MIN_COLS`] and the panel
/// [`layout::PANEL_BESIDE_MIN_COLS`], else below. Below, on short
/// screens the shell takes the detail while focused; it is hidden
/// otherwise.
pub fn with_terminal_panel(mut screen: Screen, panel: PanelGeometry) -> Screen {
    let Some(detail) = screen.detail else {
        return screen;
    };
    if !panel.open {
        return screen;
    }
    if panel.placement == PanelPlacement::Beside && fits_beside(detail) {
        let room = detail.width - 1;
        let width = ((u32::from(room) * u32::from(panel.width_percent.clamp(20, 80))) / 100) as u16;
        let width = width.clamp(
            layout::PANEL_BESIDE_MIN_COLS,
            room - layout::PANEL_BESIDE_SESSION_MIN_COLS,
        );
        let divider_x = detail.right() - width - 1;
        screen.detail = Some(Rect {
            width: divider_x - detail.x,
            ..detail
        });
        if let Some(terminal) = &mut screen.terminal {
            terminal.width = divider_x.saturating_sub(terminal.x);
        }
        screen.terminal_panel_divider = Some(Rect {
            x: divider_x,
            width: 1,
            ..detail
        });
        screen.terminal_panel = Some(Rect {
            x: divider_x + 1,
            width,
            ..detail
        });
        screen.terminal_panel_beside = true;
        return screen;
    }
    if detail.height < 14 {
        if panel.focused {
            screen.terminal_panel = Some(detail);
            screen.detail = None;
            screen.terminal = None;
        }
        return screen;
    }
    let height =
        ((u32::from(detail.height) * u32::from(panel.height_percent.clamp(20, 70))) / 100) as u16;
    let height = height.clamp(7, detail.height - 7);
    let divider_y = detail.bottom() - height - 1;
    screen.detail = Some(Rect {
        height: divider_y - detail.y,
        ..detail
    });
    if let Some(terminal) = &mut screen.terminal {
        terminal.height = divider_y.saturating_sub(terminal.y);
    }
    screen.terminal_panel_divider = Some(Rect {
        y: divider_y,
        height: 1,
        ..detail
    });
    screen.terminal_panel = Some(Rect {
        y: divider_y + 1,
        height,
        ..detail
    });
    screen
}

/// The detail holds the session and the panel side by side
/// ([`layout::PANEL_BESIDE_DETAIL_COLS`]), with a title row and two rows of
/// shell.
pub fn fits_beside(detail: Rect) -> bool {
    detail.width >= layout::PANEL_BESIDE_DETAIL_COLS && detail.height >= 3
}

/// `(cols, rows)` of the terminal pane once a session has focus: the size
/// to attach and resize with. A stacked preview in Rakumux mode is
/// shorter and shows a window of it.
pub fn terminal_size(
    area: Rect,
    session_layout: SessionLayout,
    zoomed: bool,
    sizes: PaneSizes,
) -> (u16, u16) {
    compute(area, session_layout, Focus::Session, zoomed, sizes)
        .terminal
        .map(|t| (t.width.max(MIN_TERMINAL), t.height.max(MIN_TERMINAL)))
        .unwrap_or((80, 24))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn area(width: u16, height: u16) -> Rect {
        Rect::new(0, 0, width, height)
    }

    fn at(width: u16, height: u16, layout: SessionLayout, focus: Focus) -> Screen {
        compute(
            area(width, height),
            layout,
            focus,
            false,
            PaneSizes::default(),
        )
    }

    /// No two of the shown panes overlap, and all of them sit above the
    /// footer.
    fn assert_tiled(screen: &Screen, label: &str) {
        let footer = screen.footer.expect("footer");
        let panes: Vec<Rect> = [
            screen.list,
            screen.divider,
            screen.rail,
            screen.tabs,
            screen.feed,
            screen.feed_divider,
            screen.terminal,
            screen.terminal_panel_divider,
            screen.terminal_panel,
        ]
        .into_iter()
        .flatten()
        .collect();
        for (i, a) in panes.iter().enumerate() {
            assert!(a.bottom() <= footer.y, "{label}: {a:?} under the footer");
            for b in &panes[i + 1..] {
                assert!(a.intersection(*b).is_empty(), "{label}: {a:?} and {b:?}");
            }
        }
    }

    #[test]
    fn wide_sidebar_shows_list_and_detail() {
        let screen = at(160, 48, SessionLayout::Sidebar, Focus::List);
        let list = screen.list.unwrap();
        let detail = screen.detail.unwrap();
        assert!(!screen.stacked);
        assert_eq!(list.x, 0);
        assert_eq!(detail.x, list.width + 1);
        assert_eq!(detail.width + list.width + 1, 160);
        // No header; one footer row: everything above it is panes.
        assert_eq!(screen.footer, Some(Rect::new(0, 47, 160, 1)));
        assert_eq!((list.y, list.height), (0, 47));
        assert_eq!((detail.y, detail.height), (0, 47));
        let terminal = screen.terminal.unwrap();
        // No title row either: the terminal is the whole detail.
        assert_eq!(terminal, detail);
        assert_eq!(screen.divider.unwrap().x, list.width);
        assert_eq!(
            terminal_size(
                area(160, 48),
                SessionLayout::Sidebar,
                false,
                PaneSizes::default()
            ),
            (terminal.width, 47)
        );
    }

    fn below(open: bool, focused: bool) -> PanelGeometry {
        PanelGeometry {
            open,
            focused,
            placement: PanelPlacement::Below,
            height_percent: 35,
            width_percent: 50,
        }
    }

    fn beside(focused: bool, width_percent: u16) -> PanelGeometry {
        PanelGeometry {
            placement: PanelPlacement::Beside,
            width_percent,
            ..below(true, focused)
        }
    }

    /// The panel with the app's pane sizes: a docked feed narrows for it.
    fn with_panel(
        width: u16,
        height: u16,
        focus: Focus,
        sizes: PaneSizes,
        panel: PanelGeometry,
    ) -> Screen {
        let sizes = PaneSizes {
            panel_beside: panel.open && panel.placement == PanelPlacement::Beside,
            ..sizes
        };
        let base = compute(
            area(width, height),
            SessionLayout::Sidebar,
            focus,
            false,
            sizes,
        );
        with_terminal_panel(
            base,
            PanelGeometry {
                focused: focus == Focus::Terminal,
                ..panel
            },
        )
    }

    #[test]
    fn beside_puts_the_panel_right_of_the_session_with_a_column_between() {
        for focus in [Focus::List, Focus::Session, Focus::Terminal] {
            let base = at(200, 48, SessionLayout::Sidebar, focus);
            let detail = base.detail.unwrap();
            let screen = with_terminal_panel(base, beside(focus == Focus::Terminal, 40));
            assert_tiled(&screen, &format!("{focus:?}"));
            assert!(screen.terminal_panel_beside);
            let (main, divider, panel) = (
                screen.detail.unwrap(),
                screen.terminal_panel_divider.unwrap(),
                screen.terminal_panel.unwrap(),
            );
            // Session | divider | panel, filling the detail, each its full height.
            assert_eq!(main.x, detail.x);
            assert_eq!(divider, Rect::new(main.right(), 0, 1, 47));
            assert_eq!(panel.x, divider.right());
            assert_eq!(panel.right(), detail.right());
            assert_eq!((main.height, panel.height), (47, 47));
            assert_eq!(main.width + 1 + panel.width, detail.width);
            // About 60/40 until the divider is dragged: 139 - 1 → 83 | 55.
            assert_eq!(detail.width, 139);
            assert_eq!((main.width, panel.width), (83, 55));
            assert_eq!(screen.terminal, Some(main));
            assert_eq!((screen.list, screen.footer), (base.list, base.footer));
        }
        // A dragged divider's share, each side kept to its minimum.
        let base = at(200, 48, SessionLayout::Sidebar, Focus::Terminal);
        let detail = base.detail.unwrap();
        let widest = detail.width - 1 - layout::PANEL_BESIDE_SESSION_MIN_COLS;
        for (percent, panel_cols) in [
            (20, layout::PANEL_BESIDE_MIN_COLS),
            (50, 69),
            (80, widest),
            (5, layout::PANEL_BESIDE_MIN_COLS),
            (95, widest),
        ] {
            let screen = with_terminal_panel(base, beside(true, percent));
            assert_eq!(
                screen.terminal_panel.unwrap().width,
                panel_cols,
                "{percent}"
            );
            assert!(screen.detail.unwrap().width >= layout::PANEL_BESIDE_SESSION_MIN_COLS);
        }
    }

    #[test]
    fn beside_falls_back_below_where_the_session_and_the_panel_cannot_keep_their_minimums() {
        let hidden = PaneSizes {
            list_hidden: true,
            ..PaneSizes::default()
        };
        // 60 + 1 + 40 = 101 columns of detail: with the list beside, a
        // 150-column window (48 + 1 + 101); with it hidden, 2 rail columns
        // + 101. One column less goes below.
        for (width, sizes, fits) in [
            (200, PaneSizes::default(), true),
            (160, PaneSizes::default(), true),
            (150, PaneSizes::default(), true),
            (149, PaneSizes::default(), false),
            (140, PaneSizes::default(), false),
            (140, hidden, true),
            (103, hidden, true),
            (102, hidden, false),
            (90, hidden, false),
            (45, hidden, false),
        ] {
            for focus in [Focus::List, Focus::Session, Focus::Terminal] {
                let label = format!("{width} {} {focus:?}", sizes.list_hidden);
                let hidden = sizes;
                let screen = with_panel(width, 40, focus, hidden, beside(false, 40));
                assert_tiled(&screen, &label);
                let detail_shown = screen.terminal_panel.is_some();
                if !detail_shown {
                    // A phone's or a narrow window's list: no detail to split.
                    assert!(!fits, "{label}");
                    continue;
                }
                assert_eq!(screen.terminal_panel_beside, fits, "{label}");
                let below = with_panel(width, 40, focus, hidden, below(true, false));
                if !fits {
                    // Exactly the panel below.
                    assert_eq!(screen.terminal_panel, below.terminal_panel, "{label}");
                    assert_eq!(screen.terminal, below.terminal, "{label}");
                } else {
                    let (main, panel) = (screen.terminal.unwrap(), screen.terminal_panel.unwrap());
                    assert!(
                        main.width >= layout::PANEL_BESIDE_SESSION_MIN_COLS,
                        "{label}"
                    );
                    assert!(panel.width >= layout::PANEL_BESIDE_MIN_COLS, "{label}");
                }
            }
        }
    }

    #[test]
    fn beside_with_the_feed_docked_goes_list_session_panel_feed_and_the_feed_narrows_first() {
        use crate::layout::FeedShown;
        let feed = |list_hidden| PaneSizes {
            feed: FeedShown::Shown,
            list_hidden,
            ..PaneSizes::default()
        };
        // Wide enough for everything at the feed's own width.
        let screen = with_panel(300, 48, Focus::Terminal, feed(false), beside(true, 40));
        assert_tiled(&screen, "300");
        let (list, main, panel, docked) = (
            screen.list.unwrap(),
            screen.terminal.unwrap(),
            screen.terminal_panel.unwrap(),
            screen.feed.unwrap(),
        );
        assert!(screen.terminal_panel_beside);
        assert!(list.right() < main.x && main.right() < panel.x);
        assert_eq!(screen.feed_divider.unwrap().x, panel.right());
        assert_eq!(docked.x, panel.right() + 1);
        assert_eq!(docked.width, layout::default_feed_width(300));
        // 200 columns with the list: the feed narrows from 50 to 49 so the
        // detail keeps 101 (60 + 1 + 40).
        let screen = with_panel(200, 48, Focus::Terminal, feed(false), beside(true, 40));
        assert_tiled(&screen, "200");
        assert!(screen.terminal_panel_beside);
        assert_eq!(layout::default_feed_width(200), 50);
        assert_eq!(screen.feed.unwrap().width, 49);
        // 140 columns, the list hidden: the feed narrows from 44 to 36.
        let screen = with_panel(140, 48, Focus::Terminal, feed(true), beside(true, 40));
        assert_tiled(&screen, "140 hidden");
        assert!(screen.terminal_panel_beside);
        assert_eq!(
            screen.feed.unwrap().width,
            140 - layout::RAIL_COLS - 1 - layout::PANEL_BESIDE_DETAIL_COLS
        );
        assert!(screen.feed.unwrap().width >= layout::FEED_MIN_COLS);
        // 160 with the list: no feed width leaves 101, so the feed keeps
        // its width and the panel goes below.
        let screen = with_panel(160, 48, Focus::Terminal, feed(false), beside(true, 40));
        assert_tiled(&screen, "160");
        assert!(!screen.terminal_panel_beside);
        assert_eq!(screen.feed.unwrap().width, layout::default_feed_width(160));
        let panel = screen.terminal_panel.unwrap();
        assert!(panel.y > screen.terminal.unwrap().bottom());
        assert_eq!(panel.width, screen.terminal.unwrap().width);
        // Below, the feed is as it always was.
        let shut = with_panel(160, 48, Focus::Terminal, feed(true), below(true, true));
        assert_eq!(shut.feed.unwrap().width, layout::default_feed_width(160));
    }

    #[test]
    fn beside_applies_to_tabs_and_short_wide_windows_keep_both() {
        let base = at(200, 40, SessionLayout::Tabs, Focus::Terminal);
        let screen = with_terminal_panel(base, beside(true, 40));
        assert_tiled(&screen, "Tabs");
        assert!(screen.terminal_panel_beside);
        assert_eq!(screen.tabs, base.tabs);
        assert_eq!(screen.terminal_panel.unwrap().y, base.detail.unwrap().y);
        // 10 rows: too short below for both, but beside each keeps every row.
        let short = at(200, 11, SessionLayout::Sidebar, Focus::Session);
        let screen = with_terminal_panel(short, beside(false, 40));
        assert!(screen.terminal_panel_beside);
        assert_eq!(screen.terminal.unwrap().height, 10);
        assert_eq!(screen.terminal_panel.unwrap().height, 10);
    }

    #[test]
    fn terminal_panel_splits_only_the_detail_and_tiny_windows_show_the_focused_pane() {
        for (width, height) in [(5, 5), (40, 10), (40, 20), (90, 40), (160, 48)] {
            for layout in SessionLayout::ALL {
                for focus in [Focus::List, Focus::Session, Focus::Terminal] {
                    let base = at(width, height, layout, focus);
                    let screen = with_terminal_panel(base, below(true, focus == Focus::Terminal));
                    assert_tiled(&screen, &format!("{width}x{height} {layout:?} {focus:?}"));
                    assert_eq!(screen.footer, base.footer);
                    assert_eq!(screen.list, base.list);
                    assert_eq!(screen.tabs, base.tabs);
                    if focus == Focus::Terminal && base.detail.is_some() {
                        assert!(screen.terminal_panel.is_some());
                        if base.detail.unwrap().height < 14 {
                            assert!(screen.terminal.is_none());
                        }
                    }
                    if let (Some(main), Some(panel)) = (screen.detail, screen.terminal_panel) {
                        assert!(main.bottom() < panel.y);
                        assert_eq!(panel.bottom(), base.detail.unwrap().bottom());
                    }
                    assert_eq!(with_terminal_panel(base, below(false, false)), base);
                    let beside = PanelGeometry {
                        placement: PanelPlacement::Beside,
                        ..below(false, false)
                    };
                    assert_eq!(with_terminal_panel(base, beside), base);
                }
            }
        }
    }

    #[test]
    fn a_phone_shows_the_list_or_the_session_under_tabs() {
        for layout in SessionLayout::ALL {
            // A phone in portrait, Rakumux mode: Master detail's list, then
            // the footer; Tabs keeps its tab bar over the session.
            let screen = at(45, 40, layout, Focus::List);
            match layout {
                SessionLayout::Sidebar => {
                    assert_eq!(screen.list, Some(Rect::new(0, 0, 45, 39)));
                }
                SessionLayout::Tabs => {
                    assert_eq!(screen.list, None);
                    assert_eq!(screen.tabs, Some(Rect::new(0, 0, 45, 1)));
                    assert_eq!(screen.terminal, Some(Rect::new(0, 1, 45, 38)));
                }
            }
            if layout != SessionLayout::Tabs {
                assert_eq!((screen.divider, screen.tabs), (None, None));
                assert!(screen.detail.is_none() && screen.terminal.is_none());
            }
            assert!(!screen.stacked);
            assert_eq!(screen.footer, Some(Rect::new(0, 39, 45, 1)));
            assert_tiled(&screen, "45x40 list");

            // Session mode: a row of tabs, then the session.
            let session = at(45, 40, layout, Focus::Session);
            assert_eq!((session.list, session.divider), (None, None));
            assert_eq!(session.tabs, Some(Rect::new(0, 0, 45, 1)));
            assert_eq!(session.terminal, Some(Rect::new(0, 1, 45, 38)));
            assert_tiled(&session, "45x40 session");
            assert_eq!(
                terminal_size(area(45, 40), layout, false, PaneSizes::default()),
                (45, 38)
            );
        }
    }

    #[test]
    fn narrow_sidebar_stacks_the_list_over_the_session_in_rakumux_mode() {
        // A narrow window: the list on top, a divider row, then the
        // session's terminal, then the footer.
        let screen = at(90, 40, SessionLayout::Sidebar, Focus::List);
        assert!(screen.stacked);
        assert_eq!(screen.list, Some(Rect::new(0, 0, 90, 15)));
        assert_eq!(screen.divider, Some(Rect::new(0, 15, 90, 1)));
        assert_eq!(screen.detail, Some(Rect::new(0, 16, 90, 23)));
        assert_eq!(screen.terminal, screen.detail);
        assert_eq!(screen.footer, Some(Rect::new(0, 39, 90, 1)));
        assert_tiled(&screen, "90x40");

        // Session mode: the session fills everything above the footer.
        let session = at(90, 40, SessionLayout::Sidebar, Focus::Session);
        assert!(!session.stacked);
        assert_eq!((session.list, session.divider), (None, None));
        assert_eq!(session.detail, Some(Rect::new(0, 0, 90, 39)));
        assert_eq!(session.terminal, session.detail);
        assert_eq!(session.footer, screen.footer);
        // Only the session-mode pane sizes the terminal; the stacked
        // preview shows a window of it.
        assert_eq!(
            terminal_size(
                area(90, 40),
                SessionLayout::Sidebar,
                false,
                PaneSizes::default()
            ),
            (90, 39)
        );
    }

    #[test]
    fn layouts_at_phone_and_split_window_sizes() {
        for (width, height) in [
            (45, 40),
            (45, 20),
            (40, 20),
            (60, 30),
            (80, 30),
            (99, 30),
            (100, 30),
        ] {
            for layout in SessionLayout::ALL {
                for focus in [Focus::List, Focus::Session] {
                    let label = format!("{width}x{height} {layout:?} {focus:?}");
                    let screen = at(width, height, layout, focus);
                    assert_tiled(&screen, &label);
                    let footer = screen.footer.unwrap();
                    assert_eq!(footer, Rect::new(0, height - 1, width, 1), "{label}");
                    let phone = width < layout::PHONE_BELOW;
                    // The session is on screen, except in a phone's list.
                    let list_alone = phone && layout == SessionLayout::Sidebar;
                    if list_alone && focus == Focus::List {
                        assert!(screen.terminal.is_none(), "{label}");
                        assert_eq!(screen.list.unwrap().width, width, "{label}");
                        continue;
                    }
                    let terminal = screen.terminal.expect(&label);
                    assert!(terminal.height >= 2, "{label}");
                    let narrow = layout == SessionLayout::Sidebar && !phone && width < 100;
                    assert_eq!(screen.stacked, narrow && focus == Focus::List, "{label}");
                    if narrow && focus == Focus::Session {
                        assert_eq!(terminal, Rect::new(0, 0, width, height - 1), "{label}");
                    }
                    if phone {
                        // Under a row of tabs.
                        assert_eq!(terminal, Rect::new(0, 1, width, height - 2), "{label}");
                        assert_eq!(screen.tabs, Some(Rect::new(0, 0, width, 1)), "{label}");
                    }
                    if screen.stacked {
                        let list = screen.list.unwrap();
                        assert!(list.height >= layout::LIST_MIN_ROWS, "{label}");
                        assert_eq!(list.width, width, "{label}");
                        assert_eq!(screen.detail.unwrap().bottom(), footer.y, "{label}");
                    }
                }
            }
        }
        // 100 columns is wide enough to put them side by side.
        let wide = at(100, 30, SessionLayout::Sidebar, Focus::List);
        assert_eq!(wide.list.unwrap().height, 29);
        assert_eq!(wide.divider.unwrap().width, 1);
    }

    #[test]
    fn a_hidden_list_gives_the_session_its_columns_in_both_modes() {
        let hidden = PaneSizes {
            list_hidden: true,
            ..PaneSizes::default()
        };
        for focus in [Focus::List, Focus::Session] {
            let screen = compute(area(160, 48), SessionLayout::Sidebar, focus, false, hidden);
            assert_tiled(&screen, &format!("{focus:?}"));
            assert_eq!(screen.rail, Some(Rect::new(0, 0, layout::RAIL_COLS, 47)));
            assert_eq!((screen.list, screen.divider), (None, None));
            let width = 160 - layout::RAIL_COLS;
            assert_eq!(
                screen.terminal,
                Some(Rect::new(layout::RAIL_COLS, 0, width, 47))
            );
        }
        let shown = terminal_size(
            area(160, 48),
            SessionLayout::Sidebar,
            false,
            PaneSizes::default(),
        );
        let wider = terminal_size(area(160, 48), SessionLayout::Sidebar, false, hidden);
        assert_eq!(wider, (160 - layout::RAIL_COLS, 47));
        assert!(wider.0 > shown.0);
    }

    #[test]
    fn the_feed_tiles_with_the_panes_at_every_size_and_never_moves_the_footer() {
        use crate::layout::FeedShown;
        for (width, height) in [(40, 20), (45, 40), (80, 24), (90, 40), (120, 30), (160, 48)] {
            for layout in SessionLayout::ALL {
                for focus in [Focus::List, Focus::Session, Focus::Terminal] {
                    for feed in [FeedShown::Shown, FeedShown::Focused] {
                        for list_hidden in [false, true] {
                            let sizes = PaneSizes {
                                feed,
                                list_hidden,
                                ..PaneSizes::default()
                            };
                            let label = format!(
                                "{width}x{height} {layout:?} {focus:?} {feed:?} {list_hidden}"
                            );
                            let screen = compute(area(width, height), layout, focus, false, sizes);
                            assert_tiled(&screen, &label);
                            assert_eq!(
                                screen.footer,
                                Some(Rect::new(0, height - 1, width, 1)),
                                "{label}"
                            );
                            // Session mode keeps a usable session beside a docked pane.
                            if let (Some(_), Focus::Session) = (screen.feed_divider, focus) {
                                let terminal = screen.terminal.expect(&label);
                                assert!(terminal.width >= layout::FEED_SESSION_MIN_COLS, "{label}");
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn a_short_narrow_screen_shows_the_list_alone() {
        // 90×14 (a short narrow window): 13 rows above the
        // footer is too few for both.
        let rows = layout::STACK_MIN_ROWS;
        let list_focus = at(90, rows, SessionLayout::Sidebar, Focus::List);
        assert!(!list_focus.stacked);
        assert_eq!(list_focus.list, Some(Rect::new(0, 0, 90, rows - 1)));
        assert!(list_focus.detail.is_none() && list_focus.terminal.is_none());
        assert!(list_focus.divider.is_none());
        let session_focus = at(90, rows, SessionLayout::Sidebar, Focus::Session);
        assert!(session_focus.list.is_none());
        assert_eq!(session_focus.detail.unwrap().width, 90);
        // One row more and they stack.
        assert!(at(90, rows + 1, SessionLayout::Sidebar, Focus::List).stacked);
    }

    #[test]
    fn a_dragged_stacked_list_keeps_both_minimums() {
        let sizes = |percent| PaneSizes {
            list_cols: None,
            list_percent: Some(percent),
            list_hidden: false,
            ..PaneSizes::default()
        };
        let screen = compute(
            area(90, 40),
            SessionLayout::Sidebar,
            Focus::List,
            false,
            sizes(64),
        );
        assert_eq!(screen.list.unwrap().height, 25);
        assert_eq!(screen.terminal.unwrap().height, 39 - 25 - 1);
        // The keyboard comes up: the same share, so the session keeps room.
        let screen = compute(
            area(90, 20),
            SessionLayout::Sidebar,
            Focus::List,
            false,
            sizes(64),
        );
        assert_eq!(screen.list.unwrap().height, 12, "{screen:?}");
        assert_eq!(screen.detail.unwrap().height, 6, "{screen:?}");
        let screen = compute(
            area(90, 20),
            SessionLayout::Sidebar,
            Focus::List,
            false,
            sizes(100),
        );
        assert_eq!(
            screen.detail.unwrap().height,
            layout::PREVIEW_MIN_ROWS,
            "{screen:?}"
        );
        // The session-mode size doesn't depend on it.
        assert_eq!(
            terminal_size(area(90, 40), SessionLayout::Sidebar, false, sizes(64)),
            (90, 39)
        );
    }

    #[test]
    fn tabs_layout_has_a_bar_on_top() {
        let screen = at(80, 24, SessionLayout::Tabs, Focus::List);
        let tabs = screen.tabs.unwrap();
        assert_eq!((tabs.y, tabs.height), (0, 1));
        assert!(screen.list.is_none());
        assert!(!screen.stacked);
        assert_eq!(screen.detail.unwrap().y, 1);
        assert_eq!(screen.footer, Some(Rect::new(0, 23, 80, 1)));
    }

    #[test]
    fn zoom_fills_the_screen_with_the_session() {
        for (width, height) in [(120, 40), (45, 40)] {
            let screen = compute(
                area(width, height),
                SessionLayout::Sidebar,
                Focus::Session,
                true,
                PaneSizes::default(),
            );
            assert_eq!(screen.terminal, Some(Rect::new(0, 0, width, height - 1)));
            // The footer stays, so the mode label is always on screen.
            assert_eq!(screen.footer, Some(Rect::new(0, height - 1, width, 1)));
            assert!(screen.list.is_none() && screen.detail.is_none() && screen.tabs.is_none());
            assert_eq!(
                terminal_size(
                    area(width, height),
                    SessionLayout::Sidebar,
                    true,
                    PaneSizes::default()
                ),
                (width, height - 1)
            );
        }
        // Zoom only applies with session focus.
        let list = compute(
            area(120, 40),
            SessionLayout::Sidebar,
            Focus::List,
            true,
            PaneSizes::default(),
        );
        assert!(list.list.is_some());
    }

    /// Switching modes (and so the label) never moves a pane: the footer is
    /// one row in both, and two-pane layouts keep their rectangles.
    #[test]
    fn switching_modes_keeps_the_panes_still() {
        for layout in [SessionLayout::Sidebar, SessionLayout::Tabs] {
            for (width, height) in [(100, 30), (160, 48)] {
                let list = at(width, height, layout, Focus::List);
                let session = at(width, height, layout, Focus::Session);
                assert_eq!(list, session, "{layout:?} {width}x{height}");
                assert_eq!(list.footer.unwrap().height, 1);
                assert_eq!(list.footer.unwrap().y, height - 1);
            }
        }
        // A narrow Sidebar swaps the panes but keeps the footer row, and the
        // session keeps its width.
        let list = at(80, 24, SessionLayout::Sidebar, Focus::List);
        let session = at(80, 24, SessionLayout::Sidebar, Focus::Session);
        assert_eq!(list.footer, session.footer);
        assert_eq!(
            list.terminal.unwrap().width,
            session.terminal.unwrap().width
        );
        assert_eq!(
            list.list.unwrap().height + 1 + list.detail.unwrap().height,
            session.detail.unwrap().height
        );
    }

    #[test]
    fn tiny_screens_do_not_panic() {
        for (w, h) in [(0, 0), (1, 1), (5, 2), (10, 4), (3, 3), (20, 15), (45, 14)] {
            for layout in SessionLayout::ALL {
                for focus in [Focus::List, Focus::Session] {
                    for zoom in [false, true] {
                        let _ = compute(area(w, h), layout, focus, zoom, PaneSizes::default());
                    }
                }
                let (cols, rows) = terminal_size(area(w, h), layout, false, PaneSizes::default());
                assert!(cols >= 2 && rows >= 2);
            }
        }
    }
}
