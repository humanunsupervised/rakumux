//! Screen geometry: where the session list or tab bar, detail pane,
//! terminal and footer go for a given size, layout, focus and zoom. Shared
//! by drawing and by the app (terminal size for attach/resize, mouse hits).
//!
//! The footer is always the last row, in every layout and in zoom, so the
//! mode badge never moves and nothing above it shifts when the mode, the
//! selection or a message changes.
//!
//! Phones (under [`layout::PHONE_BELOW`] columns) show one thing at a
//! time: the list in Rakumux mode, the session under a row of tabs (no
//! title row) in session mode. Narrow Sidebar screens up to
//! [`layout::SINGLE_PANE_BELOW`] stack the list above the session in
//! Rakumux mode, and give the session everything above the footer in
//! session mode; see [`crate::layout`].

use ratatui::layout::Rect;

use super::types::Focus;
use crate::layout::{self, PaneFocus, PaneSizes, SessionLayout};

/// The mode badge, messages and the keys for Help and Settings; every other
/// key is listed in Settings.
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
    /// Session tab bar (Tabs).
    pub tabs: Option<Rect>,
    /// Detail pane: its title row, then the terminal.
    pub detail: Option<Rect>,
    /// Top row of the detail: session name, status, controlling/observing.
    pub detail_title: Option<Rect>,
    /// Where the terminal view draws.
    pub terminal: Option<Rect>,
    /// The last row: mode badge, messages, keys. Shown in zoom too.
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
        Focus::Session => PaneFocus::Detail,
    };
    let areas = layout::split_with(body, session_layout, pane_focus, sizes);
    screen.list = areas.list;
    screen.divider = areas.divider;
    screen.stacked = areas.stacked;
    screen.tabs = areas.tabs;
    // A zero-width detail means a short narrow Sidebar shows the list
    // alone.
    if !areas.detail.is_empty() {
        let detail = areas.detail;
        screen.detail = Some(detail);
        // On a phone the tab bar names the session; every row counts.
        if layout::is_phone(area.width) && focus == Focus::Session {
            screen.terminal = Some(detail);
        } else if detail.height > 1 {
            screen.detail_title = Some(Rect {
                height: 1,
                ..detail
            });
            screen.terminal = Some(Rect {
                y: detail.y + 1,
                height: detail.height - 1,
                ..detail
            });
        }
    }
    screen
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
            screen.tabs,
            screen.detail_title,
            screen.terminal,
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
        assert_eq!(terminal.y, detail.y + 1);
        assert_eq!(terminal.height, detail.height - 1);
        assert_eq!(terminal.width, detail.width);
        assert_eq!(screen.divider.unwrap().x, list.width);
        assert_eq!(
            terminal_size(
                area(160, 48),
                SessionLayout::Sidebar,
                false,
                PaneSizes::default()
            ),
            (terminal.width, 46)
        );
    }

    #[test]
    fn a_phone_shows_the_list_or_the_session_under_tabs() {
        for layout in SessionLayout::ALL {
            // A phone in portrait, Rakumux mode: the list, then the footer.
            let screen = at(45, 40, layout, Focus::List);
            assert_eq!(screen.list, Some(Rect::new(0, 0, 45, 39)));
            assert_eq!((screen.divider, screen.tabs), (None, None));
            assert!(!screen.stacked);
            assert!(screen.detail.is_none() && screen.terminal.is_none());
            assert_eq!(screen.footer, Some(Rect::new(0, 39, 45, 1)));
            assert_tiled(&screen, "45x40 list");

            // Session mode: a row of tabs, then the session, no title row.
            let session = at(45, 40, layout, Focus::Session);
            assert_eq!((session.list, session.divider), (None, None));
            assert_eq!(session.tabs, Some(Rect::new(0, 0, 45, 1)));
            assert_eq!(session.detail_title, None);
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
        // session's title row and its terminal, then the footer.
        let screen = at(90, 40, SessionLayout::Sidebar, Focus::List);
        assert!(screen.stacked);
        assert_eq!(screen.list, Some(Rect::new(0, 0, 90, 15)));
        assert_eq!(screen.divider, Some(Rect::new(0, 15, 90, 1)));
        assert_eq!(screen.detail, Some(Rect::new(0, 16, 90, 23)));
        assert_eq!(screen.detail_title, Some(Rect::new(0, 16, 90, 1)));
        assert_eq!(screen.terminal, Some(Rect::new(0, 17, 90, 22)));
        assert_eq!(screen.footer, Some(Rect::new(0, 39, 90, 1)));
        assert_tiled(&screen, "90x40");

        // Session mode: the session fills everything above the footer.
        let session = at(90, 40, SessionLayout::Sidebar, Focus::Session);
        assert!(!session.stacked);
        assert_eq!((session.list, session.divider), (None, None));
        assert_eq!(session.detail, Some(Rect::new(0, 0, 90, 39)));
        assert_eq!(session.terminal, Some(Rect::new(0, 1, 90, 38)));
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
            (90, 38)
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
                    if phone && focus == Focus::List {
                        assert!(screen.terminal.is_none(), "{label}");
                        assert_eq!(screen.list.unwrap().width, width, "{label}");
                        continue;
                    }
                    let terminal = screen.terminal.expect(&label);
                    assert!(terminal.height >= 2, "{label}");
                    let narrow = layout == SessionLayout::Sidebar && !phone && width < 100;
                    assert_eq!(screen.stacked, narrow && focus == Focus::List, "{label}");
                    if narrow && focus == Focus::Session {
                        assert_eq!(terminal, Rect::new(0, 1, width, height - 2), "{label}");
                    }
                    if phone {
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
        };
        let screen = compute(
            area(90, 40),
            SessionLayout::Sidebar,
            Focus::List,
            false,
            sizes(64),
        );
        assert_eq!(screen.list.unwrap().height, 25);
        assert_eq!(screen.terminal.unwrap().height, 39 - 25 - 2);
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
            (90, 38)
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
            // The footer stays, so the mode badge is always on screen.
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

    /// Switching modes (and so the badge) never moves a pane: the footer is
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
            for layout in [SessionLayout::Sidebar, SessionLayout::Tabs] {
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
