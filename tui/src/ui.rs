//! Frame composition: session list or tab bar, detail pane, the feed, the
//! footer (with the label) on the last row, then the open dialog or
//! the Settings modal on top.

use ratatui::{
    layout::Rect,
    style::Style,
    text::Line,
    widgets::{Block, Paragraph},
    Frame,
};

use crate::app::types::{AppMode, Focus};
use crate::app::{App, DividerState};
use crate::components::{
    auth::draw_auth_menu,
    dialogs::{
        draw_auth_code_dialog, draw_auth_email_dialog, draw_confirm_dialog,
        draw_daemon_restart_dialog, draw_feed_reply_dialog, draw_rename_dialog,
        draw_upgrade_dialog,
    },
    drawer::draw_drawer,
    folder_picker::draw_folder_picker,
    footer::draw_footer,
    left::{draw_left, draw_rail},
    section_picker::draw_section_picker,
    session_detail::draw_session_detail,
    session_tabs::draw_session_tabs,
    terminal_panel::draw_terminal_panel,
};
use crate::theme;

/// A draggable line: between the list and the detail (a column side by
/// side, a row when the list is stacked above the detail), or left of the
/// docked feed. Brighter and heavier while the mouse is on it or dragging
/// it.
fn draw_divider(f: &mut Frame, state: DividerState, area: Rect, stacked: bool) {
    let p = theme::palette();
    let active = state.hover || state.dragging;
    let color = if active {
        p.accent_primary
    } else {
        p.border_dim
    };
    let lines: Vec<Line> = if stacked {
        let glyph = if active { "━" } else { "─" };
        vec![Line::from(glyph.repeat(usize::from(area.width)))]
    } else {
        let glyph = if active { "┃" } else { "│" };
        (0..area.height).map(|_| Line::from(glyph)).collect()
    };
    f.render_widget(
        Paragraph::new(lines).style(Style::default().fg(color).bg(p.background)),
        area,
    );
}

pub fn draw(f: &mut Frame, app: &mut App) {
    let area = f.size();
    if app.area != area {
        // Rendering only records geometry. Resize events and ticks own
        // preview synchronization; painting must never open streams.
        app.area = area;
    }
    // Tokyo Night text on its background everywhere, so nothing shows the
    // outer terminal's own colours.
    f.render_widget(Block::default().style(theme::palette().base()), area);
    app.assign_repo_colors();
    let screen = app.screen();
    // Keep the mode bar visible while dialogs are open, including
    // short windows where the modal otherwise reaches the last row.
    let modal_area = screen
        .footer
        .map(|footer| Rect {
            height: footer.y.saturating_sub(area.y),
            ..area
        })
        .unwrap_or(area);
    let mut hits = Vec::new();
    let mut tab_hits = Vec::new();

    if let Some(left) = screen.list.or(screen.panel) {
        draw_left(f, app, left, &mut hits);
    }
    if let Some(divider) = screen.divider {
        draw_divider(f, app.divider, divider, screen.stacked);
    }
    if let Some(rail) = screen.rail {
        draw_rail(f, rail, &mut hits);
    }
    if let Some(tabs) = screen.tabs {
        let (targets, first) = draw_session_tabs(f, app, tabs, &mut hits);
        tab_hits = targets;
        app.tab_first = first;
    }
    let main_cursor = draw_session_detail(f, app, &screen);
    if let Some(feed) = screen.feed {
        draw_drawer(f, app, feed, &mut hits);
    }
    if let Some(line) = screen.feed_divider {
        draw_divider(f, app.feed_divider, line, false);
    }
    let panel_cursor = draw_terminal_panel(f, app, &mut hits);
    let cursor = if app.focus == Focus::Terminal {
        panel_cursor
    } else {
        main_cursor
    };
    if let Some(footer) = screen.footer {
        draw_footer(f, app, footer, &mut hits);
    }
    if let (Some((x, y)), AppMode::Normal) = (cursor, &app.mode) {
        f.set_cursor(x, y);
    }

    // The Folders, App and Status modals' own click targets replace the
    // panes'.
    let mut menu_hits = Vec::new();
    match &app.mode {
        AppMode::Normal => {}
        AppMode::Folders(picker) => draw_folder_picker(f, app, picker, modal_area, &mut menu_hits),
        AppMode::Sections(picker) => {
            draw_section_picker(f, app, picker, modal_area, &mut menu_hits)
        }
        AppMode::History(picker) => {
            crate::components::history::draw_history(f, app, picker, modal_area, &mut menu_hits)
        }
        AppMode::Workspaces(picker) => crate::components::workspaces::draw_workspaces(
            f,
            app,
            picker,
            modal_area,
            &mut menu_hits,
        ),
        AppMode::Renaming(dialog) => draw_rename_dialog(f, dialog, modal_area),
        AppMode::NewHeartbeat(form) => crate::components::heartbeat_form::draw_heartbeat_form(
            f,
            app,
            form,
            modal_area,
            &mut menu_hits,
        ),
        AppMode::FeedReply(dialog) => draw_feed_reply_dialog(f, dialog, modal_area),
        AppMode::Confirm(dialog) => draw_confirm_dialog(f, dialog, modal_area),
        AppMode::DaemonRestart(dialog) => draw_daemon_restart_dialog(f, dialog, modal_area),
        AppMode::Modal { state, in_session } => {
            crate::help::render(f, modal_area, state, &app.modal_context(*in_session))
        }
        AppMode::Upgrade(dialog) => draw_upgrade_dialog(f, dialog, modal_area),
        AppMode::Welcome(state) => {
            crate::components::welcome::draw_welcome(f, app, *state, modal_area)
        }
        AppMode::PhoneOffer => crate::components::phone_access::draw_phone_offer(f, modal_area),
        AppMode::Sprite(dialog) => crate::components::sprite::draw_sprite(f, modal_area, dialog),
        AppMode::Pairing(dialog) => {
            crate::components::phone_access::draw_pairing(f, dialog, modal_area)
        }
        AppMode::Account(dialog) => crate::components::account::draw_account(f, dialog, modal_area),
        AppMode::Devices(dialog) => crate::components::devices::draw_devices(f, dialog, modal_area),
        AppMode::Auth(menu) => draw_auth_menu(f, menu, modal_area),
        AppMode::AuthEmail(dialog) => draw_auth_email_dialog(f, dialog, modal_area),
        AppMode::AuthCode(dialog) => draw_auth_code_dialog(f, dialog, modal_area),
    }
    // Dialogs cover the panes, so clicks only count in normal mode (and on
    // the Folders modal while it's open).
    if matches!(app.mode, AppMode::Normal) {
        app.hits = hits;
        app.tab_hits = tab_hits;
    } else {
        app.hits = menu_hits;
        app.tab_hits.clear();
    }
    // Paths, git names and reports may hold escape sequences: shown, never
    // sent.
    crate::printable::clean_frame(f.buffer_mut());
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::testing::{
        aged, folder, harness, harness_with_folders, on_branch, options, scope, start, view,
        view_in, FakeApi, Harness,
    };
    use crate::app::types::{Focus, SettingsItem};
    use crate::app::Hit;
    use crate::layout::SessionLayout;
    use crossterm::event::{KeyCode, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
    use ratatui::{backend::TestBackend, Terminal};
    use session_core::api::{ApiError, ErrorCode, StreamEvent};
    use session_core::client::AttachEvent;
    use session_core::model::{Lifecycle, SessionStatus, SessionView};

    const SIZES: [(u16, u16); 2] = [(80, 24), (160, 48)];

    fn render(app: &mut App, width: u16, height: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|f| draw(f, app)).unwrap();
        let buffer = terminal.backend().buffer();
        let mut out = String::new();
        for y in 0..buffer.area.height {
            for x in 0..buffer.area.width {
                out.push_str(buffer.get(x, y).symbol());
            }
            out.push('\n');
        }
        out
    }

    fn with_layout(sessions: Vec<SessionView>, layout: SessionLayout) -> Harness {
        let fake = FakeApi::new(sessions);
        let dir = tempfile::tempdir().unwrap();
        let mut options = options(&fake, scope(), &dir);
        options.settings.sessions.layout = layout;
        start(options, fake, dir)
    }

    fn dead(id: &str, name: &str) -> SessionView {
        let mut session = view(id, name, 5);
        session.status = SessionStatus::Dead;
        session.terminal = None;
        session
    }

    #[test]
    fn empty_scope() {
        let mut h = harness(vec![]);
        let wide = render(&mut h.app, 160, 48);
        assert!(wide.contains("+ New session"), "{wide}");
        // All folders, and no sessions anywhere: `n` starts one in the
        // launch folder.
        assert!(wide.contains("No sessions yet"), "{wide}");
        assert!(wide.contains("Press n to start one in repo"), "{wide}");
        // The tabs head the list, Status active: where it starts.
        assert!(
            wide.contains("\n   ★ Pinned    ◔ Status ▾  ▤ Repos    ◇ Agent"),
            "{wide}"
        );
        assert!(wide.lines().last().unwrap().contains(" All "), "{wide}");
        // Narrow: the list, and the detail stacked below it.
        let narrow = render(&mut h.app, 80, 24);
        assert!(narrow.contains("+ New session"), "{narrow}");
        assert!(narrow.contains("no sessions yet"), "{narrow}");
        assert!(narrow.contains("No sessions yet"), "{narrow}");
        // Too short to stack: the list alone.
        let short = render(&mut h.app, 80, 14);
        assert!(short.contains("+ New session"), "{short}");
        assert!(short.contains("no sessions yet"), "{short}");
        assert!(!short.contains("No sessions yet"), "{short}");
    }

    #[test]
    fn right_side_agent_badges_render_in_every_layout_at_phone_and_desktop_sizes() {
        use session_core::model::AgentInfo;
        for layout in SessionLayout::ALL {
            for (width, height) in [(40, 20), (45, 40), (80, 24), (90, 20), (160, 48)] {
                for (lifecycle, mark) in [
                    (Lifecycle::Idle, "○"),
                    (Lifecycle::Done, "●"),
                    (Lifecycle::Blocked, "?"),
                ] {
                    let mut session = view("A", "agent", 10);
                    session.agent = Some(AgentInfo {
                        kind: "opencode".into(),
                        ..Default::default()
                    });
                    session.lifecycle = lifecycle;
                    let mut h = with_layout(vec![session], layout);
                    let screen = render(&mut h.app, width, height);
                    assert!(
                        screen.lines().any(|line| line
                            .find("agent")
                            .is_some_and(|name_at| line[name_at..].contains(mark))),
                        "{layout:?} {width}x{height}: {screen}"
                    );
                }
            }
        }
    }

    #[tokio::test]
    async fn live_session_observed_then_controlled() {
        let mut h = harness(vec![view("A", "api-refactor", 10)]);
        let role =
            |h: &crate::app::testing::Harness| h.app.detail.attachment.as_ref().map(|a| a.role);
        let wide = render(&mut h.app, 160, 48);
        assert!(wide.contains("api-refactor"), "{wide}");
        assert!(wide.contains("hello from the fake"), "{wide}");
        assert_eq!(role(&h), Some(session_core::api::Role::Observer));
        // Narrow: the session is previewed under the list.
        let narrow = render(&mut h.app, 80, 24);
        assert!(narrow.contains("api-refactor"), "{narrow}");
        assert!(narrow.contains("hello from the fake"), "{narrow}");
        // Too short for both: the list alone.
        let short = render(&mut h.app, 80, 14);
        assert!(short.contains("api-refactor"), "{short}");
        assert!(!short.contains("hello from the fake"), "{short}");

        h.press(KeyCode::Enter).await;
        assert_eq!(h.app.focus, Focus::Session);
        for (width, height) in SIZES {
            let screen = render(&mut h.app, width, height);
            assert!(screen.contains("hello from the fake"), "{screen}");
            assert_eq!(role(&h), Some(session_core::api::Role::Controller));
            let footer = screen.lines().last().unwrap();
            assert!(
                footer.contains(" All ") && footer.contains("Ctrl-\\"),
                "{footer:?}"
            );
            // The way to Rakumux mode is the only button.
            assert!(
                crate::components::footer::without_version(footer).ends_with(" Ctrl-\\  "),
                "{footer:?}"
            );
            assert!(!footer.contains("Settings"), "{footer:?}");
        }
    }

    #[tokio::test]
    async fn the_orange_mode_bar_spans_the_app_and_stays_visible_under_settings_and_help() {
        let palette = theme::palette();
        for layout in SessionLayout::ALL {
            for (width, height) in [(40, 20), (80, 24), (160, 48)] {
                let mut h = with_layout(vec![view("A", "api", 10)], layout);
                for key in [None, Some('o'), Some('?')] {
                    if let Some(key) = key {
                        h.press(KeyCode::Char(key)).await;
                    }
                    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
                    terminal.draw(|frame| draw(frame, &mut h.app)).unwrap();
                    // The buttons are a darker orange, while no dialog is
                    // open.
                    let buttons: Vec<Rect> = h
                        .app
                        .hits
                        .iter()
                        .filter(|(area, hit)| {
                            area.y == height - 1 && matches!(hit, Hit::FooterKey(_))
                        })
                        .map(|(area, _)| *area)
                        .collect();
                    assert_eq!(buttons.is_empty(), key.is_some());
                    for x in 0..width {
                        let cell = terminal.backend().buffer().get(x, height - 1);
                        let on_button = buttons.iter().any(|b| x >= b.x && x < b.right());
                        let bg = if on_button {
                            palette
                                .faded(palette.accent_orange, 0.2)
                                .unwrap_or(palette.background)
                        } else {
                            palette.accent_orange
                        };
                        assert_eq!(cell.bg, bg, "{layout:?} {width}x{height} col {x}");
                        if !on_button {
                            assert_eq!(cell.fg, palette.background);
                        }
                    }
                    if key.is_some() {
                        h.press(KeyCode::Esc).await;
                    }
                }
            }
        }
    }

    #[tokio::test]
    async fn the_footer_label_names_the_workspace_in_both_layouts() {
        // The label's text, where it starts and its colours.
        let label = |h: &mut Harness, width: u16, height: u16| {
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal.draw(|f| draw(f, &mut h.app)).unwrap();
            let buffer = terminal.backend().buffer();
            let (area, _) = *h
                .app
                .hits
                .iter()
                .find(|(_, hit)| *hit == Hit::ModeBadge)
                .expect("the label");
            assert_eq!(area.y, height - 1);
            let text: String = (area.x..area.right())
                .map(|x| buffer.get(x, area.y).symbol().to_string())
                .collect();
            let cell = buffer.get(area.x + 1, area.y);
            (text, area.x, cell.fg, cell.bg)
        };
        let p = theme::palette();
        for layout in [SessionLayout::Sidebar, SessionLayout::Tabs] {
            let mut h = with_layout(vec![view("A", "api", 10)], layout);
            for (width, height) in [(160, 48), (120, 30), (90, 24), (60, 20), (40, 20)] {
                // All workspaces, or All where that doesn't fit (a phone).
                let called = if width >= 64 {
                    " All workspaces "
                } else {
                    " All "
                };
                let wide = called.chars().count() as u16;
                // Rakumux mode: bold dark ink on the orange bar, in the
                // centre while it's clear of the buttons and the version
                // (from 160 columns), else just before the buttons.
                if h.app.focus == Focus::Session {
                    h.rakumux().await;
                }
                assert_eq!(h.app.focus, Focus::List);
                let (text, x, fg, bg) = label(&mut h, width, height);
                assert_eq!(text, called);
                assert_eq!((fg, bg), (p.background, p.accent_orange));
                let buttons = h
                    .app
                    .hits
                    .iter()
                    .filter(|(_, hit)| matches!(hit, Hit::FooterKey(_)))
                    .map(|(area, _)| area.x)
                    .min()
                    .expect("buttons");
                // Master detail has no Inbox button: more room.
                let centred = if layout == SessionLayout::Tabs {
                    160
                } else {
                    120
                };
                if width >= centred {
                    assert_eq!(x, (width - wide) / 2, "{width}");
                } else {
                    assert!(x + wide < buttons, "{width}");
                }
                // Help/Settings keep the mode bar visible on short screens too.
                h.press(KeyCode::Char('?')).await;
                let mode = crate::components::footer::centre_label(&h.app, width.into());
                assert_eq!(mode.content, called);
                assert_eq!(mode.style.fg, Some(p.background));
                assert_eq!(mode.style.bg, Some(p.accent_orange));
                h.press(KeyCode::Esc).await;
                // A session: a quiet label that blends in, in the centre.
                h.press(KeyCode::Enter).await;
                assert_eq!(h.app.focus, Focus::Session);
                let (text, x, fg, bg) = label(&mut h, width, height);
                assert_eq!(text, called);
                assert_eq!((fg, bg), (p.neutral_gray, p.selection));
                assert_eq!(x, (width - wide) / 2, "{width}");
            }
        }
    }

    #[tokio::test]
    async fn no_header_the_panes_start_at_the_top() {
        let mut h = harness(vec![view("A", "api", 10)]);
        for (width, height) in SIZES {
            let screen = render(&mut h.app, width, height);
            let first = screen.lines().next().unwrap();
            assert!(!first.contains("RAKUMUX"), "{first:?}");
            assert!(!first.contains("session"), "no count: {first:?}");
            let list = h.app.screen().list.unwrap();
            assert_eq!(list.y, 0);
        }
        // Wide: the session's terminal starts on the first row too.
        let wide = render(&mut h.app, 160, 48);
        let first = wide.lines().next().unwrap();
        assert!(first.contains("hello from the fake"), "{wide}");
    }

    #[tokio::test]
    async fn the_footer_is_one_row_of_messages() {
        let mut h = harness(vec![view("A", "api", 10)]);
        // The account's button has its own test.
        h.app.account = crate::app::types::AccountStatus::Unknown;
        for (width, height) in SIZES {
            let screen = render(&mut h.app, width, height);
            let rows: Vec<&str> = screen.lines().collect();
            assert_eq!(rows.len(), usize::from(height));
            let footer = rows[rows.len() - 1];
            assert!(footer.starts_with(" session: ● api "), "{footer:?}");
            assert!(
                footer.contains(" All ") && !footer.contains("Ctrl-\\"),
                "{footer:?}"
            );
            assert!(
                crate::components::footer::without_version(footer)
                    .ends_with(" ? Help   , Settings  "),
                "{footer:?}"
            );
            // No key hints above it: the panes reach the footer.
            for hint in ["select", "new", "rename", "switch pane"] {
                assert!(!footer.contains(hint), "{footer:?}");
            }
            let detail = h.app.screen().detail.unwrap();
            assert_eq!(detail.y + detail.height, height - 1);
        }
        h.app.set_status("Layout: Tabs");
        let screen = render(&mut h.app, 80, 24);
        let footer = screen.lines().last().unwrap();
        assert!(
            footer.starts_with(" session: ● api  Layout: Tabs "),
            "{footer:?}"
        );
        assert!(
            footer.contains(" All ") && !footer.contains("Ctrl-\\"),
            "{footer:?}"
        );
        assert!(
            crate::components::footer::without_version(footer).ends_with(" ? Help   , Settings  "),
            "{footer:?}"
        );
    }

    /// Mode switches, selection moves and messages coming and going never
    /// move the panes: everything above the footer stays where it was.
    #[tokio::test]
    async fn modes_selection_and_messages_keep_the_layout_still() {
        for layout in [SessionLayout::Sidebar, SessionLayout::Tabs] {
            let mut h = with_layout(
                vec![aged("A", "api", 1, 20), aged("B", "docs", 2, 10)],
                layout,
            );
            let (width, height) = (160, 48);
            render(&mut h.app, width, height);
            let still = |h: &Harness| {
                let screen = h.app.screen();
                (screen.detail, screen.terminal, screen.footer)
            };
            let before = still(&h);
            let size = h.app.terminal_size();
            h.press(KeyCode::Down).await;
            render(&mut h.app, width, height);
            assert_eq!(still(&h), before, "{layout:?}: selection");
            h.press(KeyCode::Enter).await;
            assert_eq!(h.app.focus, Focus::Session);
            render(&mut h.app, width, height);
            assert_eq!(still(&h), before, "{layout:?}: session mode");
            assert_eq!(h.app.terminal_size(), size, "{layout:?}");
            for status in ["short", &"a very long message ".repeat(20)] {
                h.app.set_status(status);
                let screen = render(&mut h.app, width, height);
                assert_eq!(still(&h), before, "{layout:?}: message");
                assert_eq!(screen.lines().count(), usize::from(height));
                let footer = screen.lines().last().unwrap();
                assert!(
                    footer.contains(" All ") && footer.contains("Ctrl-\\"),
                    "{footer:?}"
                );
            }
            h.rakumux().await;
            render(&mut h.app, width, height);
            assert_eq!(still(&h), before, "{layout:?}: rakumux mode");
        }
    }

    #[tokio::test]
    async fn settings_is_one_page_of_settings_then_actions() {
        let mut h = harness(vec![view("A", "api", 10)]);
        h.press(KeyCode::Char('o')).await;
        for (width, height) in SIZES {
            let screen = render(&mut h.app, width, height);
            assert!(screen.contains("Settings"), "{screen}");
            assert!(!screen.contains("1 General"), "no tabs: {screen}");
            // Signing in first, then the rest.
            let at = |text: &str| {
                screen
                    .find(text)
                    .unwrap_or_else(|| panic!("{text}: {screen}"))
            };
            assert!(screen.contains("▶ Account"), "{screen}");
            assert!(at("▶ Account") < at("Appearance"));
            assert!(!screen.contains("Available on sign-in"), "{screen}");
            assert!(screen.contains("[Master detail] Tabs"), "{screen}");
            // The selected setting's note waits for `→`.
            assert!(!screen.contains("Session list on the left"), "{screen}");
            assert!(screen.contains("Enter change"), "{screen}");
            // At most 80% of the screen: a 24-row one shows part of the page.
            let lines: Vec<&str> = screen.lines().collect();
            let top = lines.iter().position(|l| l.contains('╭')).unwrap();
            let bottom = lines.iter().position(|l| l.contains('╰')).unwrap();
            let rows = bottom - top + 1;
            assert!(rows <= usize::from(height) * 4 / 5, "{rows}: {screen}");
            if height >= 40 {
                assert!(screen.contains("Terminal panel"), "{screen}");
            }
        }
        // The actions and their keys follow the settings.
        let screen = render(&mut h.app, 160, 52);
        for hint in [
            "── Keys",
            "Modes",
            "Rakumux mode (from a session)",
            "Esc, Ctrl-\\",
        ] {
            assert!(screen.contains(hint), "{hint}: {screen}");
        }
        // The page scrolls to the selected action.
        h.press(KeyCode::End).await;
        for (width, height) in SIZES {
            let screen = render(&mut h.app, width, height);
            for hint in ["▶ Quit Rakumux", "Enter run"] {
                assert!(screen.contains(hint), "{hint}: {screen}");
            }
        }
        // Its heading too, where the screen has room for the section.
        let screen = render(&mut h.app, 160, 48);
        assert!(screen.contains("Help and app"), "{screen}");
        assert!(
            screen.contains("Terminal panel beside the session"),
            "{screen}"
        );
        // No phone or AI naming sections.
        h.press(KeyCode::Home).await;
        let screen = render(&mut h.app, 160, 48);
        for gone in ["Phone", "AI session naming", "API key", "OpenAI"] {
            assert!(!screen.contains(gone), "{gone}: {screen}");
        }
    }

    #[tokio::test]
    async fn settings_hugs_its_rows_and_the_wheel_moves_through_them() {
        let mut h = harness(vec![view("A", "api", 10)]);
        h.press(KeyCode::Char(',')).await;
        // Taller than the page: the dialog ends where its rows do.
        let mut terminal = Terminal::new(TestBackend::new(120, 200)).unwrap();
        terminal.draw(|f| draw(f, &mut h.app)).unwrap();
        let buffer = terminal.backend().buffer();
        let corners: Vec<u16> = (0..200)
            .filter(|y| (0..120).any(|x| buffer.get(x, *y).symbol() == "╭"))
            .collect();
        let bottom = (0..200)
            .find(|y| (0..120).any(|x| buffer.get(x, *y).symbol() == "╰"))
            .unwrap();
        assert_eq!(corners.len(), 1);
        let height = bottom - corners[0] + 1;
        assert!(height < 120, "{height} rows");
        let screen = render(&mut h.app, 120, 200);
        assert!(screen.contains("Quit Rakumux"), "every row shows: {screen}");
        // In the dialog (the list's divider behind it can be a `┃` too).
        let buffer = terminal.backend().buffer();
        let left = (0..120)
            .find(|x| buffer.get(*x, corners[0]).symbol() == "╭")
            .unwrap();
        let right = (0..120)
            .find(|x| buffer.get(*x, corners[0]).symbol() == "╮")
            .unwrap();
        let scrollbar =
            (corners[0]..=bottom).any(|y| (left..=right).any(|x| buffer.get(x, y).symbol() == "┃"));
        assert!(!scrollbar, "no scrollbar: {screen}");
        // The wheel moves the selection, a row per notch.
        let wheel = |kind| MouseEvent {
            kind,
            column: 60,
            row: 12,
            modifiers: KeyModifiers::NONE,
        };
        h.app.handle_mouse(wheel(MouseEventKind::ScrollDown));
        h.app.handle_mouse(wheel(MouseEventKind::ScrollDown));
        assert!(matches!(&h.app.mode, AppMode::Modal { state, .. } if state.selected == 2));
        h.app.handle_mouse(wheel(MouseEventKind::ScrollUp));
        assert!(matches!(&h.app.mode, AppMode::Modal { state, .. } if state.selected == 1));
        // Help scrolls instead.
        h.press(KeyCode::Esc).await;
        h.press(KeyCode::Char('?')).await;
        render(&mut h.app, 80, 24);
        h.app.handle_mouse(wheel(MouseEventKind::ScrollDown));
        assert!(matches!(&h.app.mode, AppMode::Modal { state, .. } if state.scroll == 3));
    }

    #[tokio::test]
    async fn the_detail_has_no_title_row_and_the_footer_names_the_session_with_its_mark() {
        let mut h = harness(vec![view("A", "color-check", 10)]);
        for focus_session in [false, true] {
            if focus_session {
                h.press(KeyCode::Enter).await;
            }
            let screen = render(&mut h.app, 160, 48);
            let detail = h.app.screen().detail.unwrap();
            assert_eq!(h.app.screen().terminal, Some(detail));
            assert_eq!(detail.y, 0);
            // The program's first row is the screen's first.
            let top = screen.lines().next().unwrap();
            assert!(top.contains("hello from the fake"), "{top:?}");
            let footer = screen.lines().last().unwrap();
            assert!(footer.contains("session: ● color-check"), "{footer:?}");
        }
    }

    #[test]
    fn update_states_are_readable_and_say_sessions_keep_running_at_phone_and_desktop_sizes() {
        use crate::app::types::UpgradeDialog;
        let notes = "## Session list\n\n- Repositories first, then the app.";
        for state in [
            "checking",
            "available",
            "installing",
            "error",
            "current",
            "installed",
        ] {
            for (width, height) in [(40, 20), (90, 20), (160, 48)] {
                let mut h = harness(vec![view("A", "api", 10)]);
                h.app.mode = AppMode::Upgrade(UpgradeDialog {
                    current_version: "0.1.4".into(), latest_version: Some("0.1.5".into()),
                    install_method: Some(crate::updater::InstallMethod::Binary),
                    checking: state == "checking", installing: state == "installing",
                    update_available: matches!(state, "available" | "installing" | "installed"),
                    error_message: (state == "error").then(|| "Cannot write the installation. Use the installer outside Rakumux with your original PREFIX; administrator permission may be required.".into()),
                    info_message: (state == "current").then(|| "You're already on the latest version.".into()),
                    notes: matches!(state, "available" | "installed").then(|| notes.into()),
                    prompted: matches!(state, "available" | "installed"),
                    installed: state == "installed",
                    ..UpgradeDialog::default()
                });
                let text = render(&mut h.app, width, height);
                assert!(text.contains("Rakumux"), "{state}: {text}");
                assert!(text.contains("sessions keep"), "{state}: {text}");
                if state == "error" {
                    assert!(
                        text.contains("administrator") && text.contains("PREFIX"),
                        "{text}"
                    );
                }
                if state == "available" {
                    assert!(
                        text.contains("Update now") && text.contains("Later"),
                        "{text}"
                    );
                    assert!(text.contains("0.1.5 is out"), "{text}");
                }
                if state == "installed" {
                    assert!(
                        text.contains("Restart now") && text.contains("Later"),
                        "{text}"
                    );
                    assert!(text.contains("Updated to 0.1.5"), "{text}");
                }
                if matches!(state, "available" | "installed") && height >= 40 {
                    // What's new, its Markdown drawn.
                    assert!(text.contains("Repositories first, then the app."), "{text}");
                }
            }
        }
    }

    #[test]
    fn dead_session_shows_dimmed_saved_output() {
        let fake = FakeApi::new(vec![dead("D", "old-spike")]);
        fake.set_read("D", "saved line one\nsaved line two");
        let dir = tempfile::tempdir().unwrap();
        let options = options(&fake, scope(), &dir);
        let mut h = start(options, fake, dir);
        let wide = render(&mut h.app, 160, 48);
        assert!(wide.contains("saved line two"), "{wide}");
        assert!(wide.contains("Dead · Enter to revive"), "{wide}");
        assert!(wide.contains("○ old-spike"), "{wide}");
        let narrow = render(&mut h.app, 80, 24);
        assert!(narrow.contains("dead · ⏎ revive"), "{narrow}");
    }

    #[test]
    fn exited_session_offers_a_restart() {
        let mut h = harness(vec![view("A", "build", 10)]);
        // The footer names it whole without the account's button.
        h.app.account = crate::app::types::AccountStatus::Unknown;
        let (index, _) = h.last_attach();
        h.attach_event(
            index,
            AttachEvent::Event(StreamEvent::Exited { code: Some(3) }),
        );
        for (width, height) in SIZES {
            let screen = render(&mut h.app, width, height);
            // The footer names it, then says why.
            let footer = screen.lines().last().unwrap();
            assert!(
                footer.contains("✕ build  build exited with code 3 "),
                "{footer:?}"
            );
        }
        let wide = render(&mut h.app, 160, 48);
        assert!(
            wide.contains("Exited with code 3 · Enter to restart"),
            "{wide}"
        );
        assert!(
            wide.contains("hello from the fake"),
            "final screen kept: {wide}"
        );
    }

    #[test]
    fn tabs_layout_shows_a_tab_bar_and_the_selected_session() {
        let mut h = with_layout(
            vec![view("A", "api", 20), view("B", "docs", 10)],
            SessionLayout::Tabs,
        );
        for (width, height) in SIZES {
            let screen = render(&mut h.app, width, height);
            assert!(screen.contains("api"), "{screen}");
            assert!(screen.contains("docs"), "{screen}");
            assert!(screen.contains("hello from the fake"), "{screen}");
            // The active tab's filter (Status, where the list starts)
            // sits at the left end of the tab bar.
            let bar = screen.lines().next().unwrap();
            assert!(bar.starts_with(" ◔ Status ▾   1 ● docs "), "{bar:?}");
            assert!(!h.app.tab_hits.is_empty());
        }
        let mut empty = with_layout(vec![], SessionLayout::Tabs);
        let screen = render(&mut empty.app, 80, 24);
        assert!(screen.contains("No sessions yet"), "{screen}");
    }

    #[test]
    fn the_list_has_a_hide_button_and_leaves_a_rail_when_hidden() {
        let mut h = harness(vec![view("A", "api", 20), view("B", "docs", 10)]);
        let screen = render(&mut h.app, 160, 48);
        let rows: Vec<&str> = screen.lines().collect();
        let list = h.app.screen().list.unwrap();
        // At the right of the column's tabs, at its bottom.
        assert!(
            list_column(&screen, list.width.into())[45].ends_with(" «"),
            "{:?}",
            rows[45]
        );
        let button = h
            .app
            .hits
            .iter()
            .find(|(_, hit)| *hit == Hit::HideList)
            .map(|(area, _)| *area);
        assert_eq!(button, Some(Rect::new(list.width - 3, 44, 3, 3)));
        let beside = h.app.screen().terminal.unwrap();

        tap(&mut h, Hit::HideList);
        let screen = render(&mut h.app, 160, 48);
        let rows: Vec<&str> = screen.lines().collect();
        assert!(!screen.contains("❯ Workspaces"), "{screen}");
        assert!(!screen.contains("◔ Status"), "{screen}");
        // A column of the list pane with `»` where the button was, the
        // divider's line, then the session from the third column.
        assert!(rows[45].starts_with("»│"), "{:?}", rows[45]);
        assert!(
            rows[0].starts_with(" │hello from the fake"),
            "{:?}",
            rows[0]
        );
        let rail = h
            .app
            .hits
            .iter()
            .find(|(_, hit)| *hit == Hit::ShowList)
            .map(|(area, _)| *area);
        assert_eq!(rail, Some(Rect::new(0, 0, crate::layout::RAIL_COLS, 47)));
        let hidden = h.app.screen().terminal.unwrap();
        assert_eq!(hidden.x, crate::layout::RAIL_COLS);
        assert_eq!(
            hidden.width,
            beside.width + list.width + 1 - crate::layout::RAIL_COLS
        );

        // A click anywhere on the rail shows the list again.
        tap_at(&mut h, 1, 20);
        let screen = render(&mut h.app, 160, 48);
        assert!(screen.contains("❯ Workspaces"), "{screen}");
        assert!(screen.contains("◔ Status"), "{screen}");
        assert_eq!(h.app.screen().terminal, Some(beside));
    }

    #[test]
    fn the_docked_drawer_stays_while_the_list_hides() {
        let mut h = harness(vec![view("A", "api", 20)]);
        h.app.open_rtop();
        render(&mut h.app, 160, 48);
        let session = h.app.screen().terminal.unwrap();
        tap(&mut h, Hit::HideList);
        let screen = render(&mut h.app, 160, 48);
        assert!(
            screen.lines().nth(45).unwrap().starts_with("»│"),
            "{screen}"
        );
        // The session takes the list's columns; the drawer keeps its own.
        let hidden = h.app.screen();
        let feed = hidden.feed.unwrap();
        assert_eq!(feed.right(), 160);
        assert_eq!(hidden.terminal.unwrap().right(), session.right());
        assert!(hidden.terminal.unwrap().width > session.width);
        assert!(
            screen.lines().nth(1).unwrap().contains("▥ rtop"),
            "{screen}"
        );
    }

    #[test]
    fn tabs_that_dont_fit_say_how_many_are_hidden_and_hold_still() {
        let sessions: Vec<SessionView> = (1..=9)
            .map(|i| aged(&format!("S{i}"), &format!("session-{i}"), i, i))
            .collect();
        let mut h = with_layout(sessions, SessionLayout::Tabs);
        let bar = |h: &mut Harness| {
            render(&mut h.app, 80, 24)
                .lines()
                .next()
                .unwrap()
                .to_string()
        };
        let names = |bar: &str| -> Vec<usize> {
            (1..=9)
                .filter(|i| bar.contains(&format!(" session-{i} ")))
                .collect()
        };
        let hidden = |bar: &str, marker: char| -> usize {
            let at = bar.find(marker).map(|at| {
                if marker == '‹' {
                    &bar[at + marker.len_utf8()..]
                } else {
                    &bar[..at]
                }
            });
            at.map(|text| {
                let digits: String = if marker == '‹' {
                    text.chars().take_while(char::is_ascii_digit).collect()
                } else {
                    let rev: String = text
                        .chars()
                        .rev()
                        .take_while(char::is_ascii_digit)
                        .collect();
                    rev.chars().rev().collect()
                };
                digits.parse().unwrap()
            })
            .unwrap_or(0)
        };
        // The most recent session, the last tab, is selected: the ones
        // before it that don't fit are counted on the left.
        assert_eq!(h.selected().as_deref(), Some("S9"));
        let last = bar(&mut h);
        let shown = names(&last);
        assert!(shown.contains(&9), "{last:?}");
        assert_eq!(hidden(&last, '‹') + shown.len(), 9, "{last:?}");
        assert!(!last.contains('›'), "{last:?}");
        // The first: the rest are counted on the right.
        h.app.list.select(crate::app::Selection::Session(
            session_core::ids::SessionId::from_string("S1"),
        ));
        let first = bar(&mut h);
        let shown = names(&first);
        assert_eq!(shown[0], 1, "{first:?}");
        assert_eq!(shown.len() + hidden(&first, '›'), 9, "{first:?}");
        assert!(!first.contains('‹'), "{first:?}");
        // Moving to a tab in view moves none of them.
        h.app.list.select(crate::app::Selection::Session(
            session_core::ids::SessionId::from_string("S2"),
        ));
        assert_eq!(names(&bar(&mut h)), shown);
    }

    #[tokio::test]
    async fn clicking_a_tab_selects_it() {
        let mut h = with_layout(
            vec![view("A", "api", 20), view("B", "docs", 10)],
            SessionLayout::Tabs,
        );
        render(&mut h.app, 160, 48);
        assert_eq!(h.selected().as_deref(), Some("A"));
        // Creation order: docs (B) is the first tab.
        let (_, rect) = h.app.tab_hits[0];
        h.app.handle_mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: rect.x + 1,
            row: rect.y,
            modifiers: KeyModifiers::NONE,
        });
        h.app.process_pending();
        assert_eq!(h.selected().as_deref(), Some("B"));
    }

    #[tokio::test]
    async fn zoom_hides_the_list_and_keeps_the_footer() {
        let mut h = harness(vec![view("A", "api", 10)]);
        h.press(KeyCode::Enter).await;
        h.prefix('z').await;
        for (width, height) in SIZES {
            let screen = render(&mut h.app, width, height);
            assert!(screen.contains("hello from the fake"), "{screen}");
            assert!(!screen.contains("▤ Repos"), "{screen}");
            assert!(!screen.contains("RAKUMUX"), "{screen}");
            // The terminal starts at the top; the footer keeps the zoomed
            // session, the mode and the way out.
            assert!(screen.starts_with("hello from the fake"), "{screen}");
            let footer = screen.lines().last().unwrap();
            assert!(footer.starts_with(" session: ● api  zoomed "), "{footer:?}");
            assert!(
                footer.contains(" All ") && footer.contains("Ctrl-\\"),
                "{footer:?}"
            );
            assert!(
                crate::components::footer::without_version(footer).ends_with(" Ctrl-\\  "),
                "{footer:?}"
            );
        }
        // Back in Rakumux mode the list returns, and the label says so.
        h.rakumux().await;
        let screen = render(&mut h.app, 160, 48);
        assert!(screen.contains("▤ Repos"), "{screen}");
        let footer = screen.lines().last().unwrap();
        assert!(
            footer.contains(" All ") && !footer.contains("Ctrl-\\"),
            "{footer:?}"
        );
    }

    #[tokio::test]
    async fn modal_dialogs_render() {
        let mut h = harness(vec![view("A", "api", 10)]);
        h.press(KeyCode::Char('o')).await;
        // The first action, under the settings, shows the configured prefix.
        for _ in 0..SettingsItem::ALL.len() {
            h.press(KeyCode::Down).await;
        }
        for (width, height) in SIZES {
            let screen = render(&mut h.app, width, height);
            assert!(screen.contains("Keys"), "{screen}");
            assert!(screen.contains("Ctrl-\\"), "{screen}");
        }
        h.press(KeyCode::Esc).await;
        h.press(KeyCode::Char('?')).await;
        for (width, height) in SIZES {
            let screen = render(&mut h.app, width, height);
            assert!(screen.contains("Help"), "{screen}");
            assert!(screen.contains("What Rakumux is"), "{screen}");
        }
        h.press(KeyCode::Esc).await;
        h.press(KeyCode::Char('o')).await;
        let screen = render(&mut h.app, 80, 24);
        assert!(screen.contains("Settings"), "{screen}");
        assert!(screen.contains("[Master detail] Tabs"), "{screen}");
        h.press(KeyCode::Esc).await;
        h.type_text("dd").await;
        let screen = render(&mut h.app, 80, 24);
        assert!(screen.contains("Delete session"), "{screen}");
    }

    #[tokio::test]
    async fn new_sessions_show_the_terminal_immediately_without_a_name_modal() {
        let mut h = harness_with_folders(three_folders(), &known(), SessionLayout::Sidebar);
        // The most recent session is frontend, in /code/web.
        h.press(KeyCode::Char('n')).await;
        assert_eq!(h.app.focus, Focus::Session);
        assert!(matches!(h.app.mode, AppMode::Normal));
        for (width, height) in [(40, 20), (80, 24), (160, 48)] {
            let screen = render(&mut h.app, width, height);
            assert!(!screen.contains("┌ New session "), "{screen}");
            assert!(!screen.contains("┌ Name "), "{screen}");
            assert!(screen.contains("hello from the fake"), "{screen}");
            assert!(screen.contains("Ctrl-\\"), "{screen}");
        }
        assert_eq!(h.fake.creates().last().unwrap().name, None);
    }

    #[tokio::test]
    async fn immediate_creation_uses_the_last_visited_folder_section() {
        let mut h = harness_with_folders(three_folders(), &known(), SessionLayout::Sidebar);
        h.app
            .list
            .select(crate::app::Selection::FolderHeader(folder("/code/api")));
        h.press(KeyCode::End).await;
        h.press(KeyCode::Enter).await;
        assert_eq!(
            h.fake.creates().last().unwrap().scope,
            Some(folder("/code/api"))
        );
        assert_eq!(h.app.focus, Focus::Session);
    }

    #[test]
    fn unreachable_daemon_is_explained() {
        let fake = FakeApi::new(vec![]);
        let dir = tempfile::tempdir().unwrap();
        let mut options = options(&fake, scope(), &dir);
        options.connector = std::sync::Arc::new(|| {
            Err(ApiError::new(
                ErrorCode::DaemonUnavailable,
                "can't start rakumux-ptyd",
            ))
        });
        let mut h = start(options, fake, dir);
        let wide = render(&mut h.app, 160, 48);
        assert!(wide.contains("Can't reach the Rakumux daemon"), "{wide}");
        assert!(wide.contains("can't start rakumux-ptyd"), "{wide}");
        assert!(wide.contains("Ctrl-r"), "{wide}");
        let narrow = render(&mut h.app, 80, 24);
        assert!(narrow.contains("daemon unavailable"), "{narrow}");
        // The footer says so at the left, in both widths (no session to
        // name before it).
        for screen in [wide, narrow] {
            let footer = screen.lines().last().unwrap();
            assert!(
                footer.starts_with(" ✕ daemon unavailable · can't "),
                "{footer:?}"
            );
            assert!(
                footer.contains(" All ") && !footer.contains("Ctrl-\\"),
                "{footer:?}"
            );
        }
    }

    #[test]
    fn connecting_shows_a_spinner_in_the_footer() {
        let mut h = harness(vec![]);
        h.app.connection = crate::app::Connection::Connecting;
        // The account's button has its own test.
        h.app.account = crate::app::types::AccountStatus::Unknown;
        for (width, height) in SIZES {
            let screen = render(&mut h.app, width, height);
            let footer = screen.lines().last().unwrap();
            // At the left: a space, the spinner, then what it's doing.
            let after_spinner: String = footer.chars().skip(2).collect();
            assert!(after_spinner.starts_with(" connecting "), "{footer:?}");
            assert!(
                footer.contains(" All ") && !footer.contains("Ctrl-\\"),
                "{footer:?}"
            );
            assert!(
                crate::components::footer::without_version(footer).ends_with(" , Settings  "),
                "{footer:?}"
            );
        }
    }

    // ------------------------------------------------------------ folders

    /// Unix seconds `secs` ago, so ages render as `2m`, `1h`, …
    fn ago(secs: i64) -> i64 {
        crate::components::format::now_secs() - secs
    }

    fn agent(session: &mut SessionView, kind: &str, lifecycle: Lifecycle) {
        session.agent = Some(session_core::model::AgentInfo {
            kind: kind.into(),
            ..Default::default()
        });
        session.lifecycle = lifecycle;
    }

    /// `/repo` (launch), `/code/web` and `/code/api`, with branches, a
    /// worktree, a pinned session, a dead one and a report.
    fn three_folders() -> Vec<SessionView> {
        let (web, api) = (folder("/code/web"), folder("/code/api"));
        let mut refactor = view("R1", "api-refactor", ago(120));
        refactor.git = on_branch("sessions-refactor");
        agent(&mut refactor, "claude", Lifecycle::Working);
        let mut docs = view("R2", "docs", ago(3_600));
        docs.git = on_branch("main");
        docs.pinned = true;
        let mut front = view_in(&web, "W1", "frontend", ago(30));
        front.git = on_branch("feature/login");
        if let Some(git) = front.git.as_mut() {
            git.worktree = Some("login".into());
        }
        agent(&mut front, "codex", Lifecycle::Blocked);
        let mut storybook = view_in(&web, "W2", "storybook", ago(86_400 * 2));
        storybook.status = SessionStatus::Dead;
        storybook.terminal = None;
        storybook.git = on_branch("main");
        let mut server = view_in(&api, "A1", "server", ago(600));
        server.git = Some(session_core::model::GitInfo {
            repo_root: "/code/api".into(),
            branch: None,
            detached_at: Some("1a2b3c4".into()),
            worktree: None,
            repo: None,
            origin: None,
        });
        if let Some(terminal) = server.terminal.as_mut() {
            terminal.foreground_process = Some("cargo".into());
        }
        server.reported = Some(session_core::model::Reported {
            state: None,
            text: Some("watching for changes".into()),
            result: None,
            at: 0,
        });
        // The web folder's sessions in its own repository (its `origin`).
        for session in [&mut front, &mut storybook] {
            if let Some(git) = session.git.as_mut() {
                git.repo = Some("web".into());
            }
        }
        vec![refactor, docs, front, storybook, server]
    }

    /// `/repo` (launch) and `/code/web`, with Claude Code, Codex and
    /// OpenCode in every state, a shell, a program and a dead session.
    fn agents_and_statuses() -> Vec<SessionView> {
        use session_core::activity::{ActivityPhase, AgentActivity};
        let web = folder("/code/web");
        let mut refactor = view("R1", "api-refactor", ago(120));
        refactor.git = on_branch("sessions-refactor");
        agent(&mut refactor, "claude", Lifecycle::Working);
        let mut review = view("R2", "review-pr-412", ago(300));
        review.git = on_branch("main");
        agent(&mut review, "codex", Lifecycle::Blocked);
        let mut release = view("R3", "release-notes", ago(900));
        agent(&mut release, "claude", Lifecycle::Done);
        let mut docs = view("R4", "docs", ago(3_600));
        docs.pinned = true;
        let mut front = view_in(&web, "W1", "frontend", ago(30));
        front.git = on_branch("feature/login");
        agent(&mut front, "opencode", Lifecycle::Working);
        front.activity = Some(AgentActivity {
            phase: ActivityPhase::WaitingForResponse,
            active_subagents: 0,
            updated_at: 1,
        });
        let mut server = view_in(&web, "W2", "dev-server", ago(600));
        if let Some(terminal) = server.terminal.as_mut() {
            terminal.foreground_process = Some("cargo".into());
        }
        let mut idle = view_in(&web, "W3", "styles", ago(7_200));
        agent(&mut idle, "claude", Lifecycle::Idle);
        let mut storybook = view_in(&web, "W4", "storybook", ago(86_400 * 2));
        storybook.status = SessionStatus::Dead;
        storybook.terminal = None;
        agent(&mut storybook, "codex", Lifecycle::None);
        vec![
            refactor, review, release, docs, front, server, idle, storybook,
        ]
    }

    #[test]
    fn the_app_and_status_tabs_group_sessions_under_headers() {
        use crate::app::grouping::Grouping;
        let mut h = harness_with_folders(agents_and_statuses(), &known(), SessionLayout::Sidebar);
        let width = usize::from(crate::layout::sidebar_width(160));
        h.app.show_tab(Grouping::App);
        // Tall enough for every status row.
        let wide = render(&mut h.app, 160, 80);
        let list = list_column(&wide, width);
        assert!(
            list[1].starts_with("   ★ Pinned    ◔ Status    ▤ Repos    ◇ Agent ▾"),
            "{wide}"
        );
        let at = |list: &[String], needle: &str| {
            list.iter()
                .position(|l| l.contains(needle))
                .unwrap_or_else(|| panic!("{needle} missing: {wide}"))
        };
        // A section's bar: its name, and at its end the count and ▾.
        let bar = |list: &[String], name: &str| {
            list.iter()
                .position(|l| {
                    l.trim_end().ends_with(['▾', '▸']) && l.contains(&format!(" {name} "))
                })
                .unwrap_or_else(|| panic!("{name}'s bar missing: {wide}"))
        };
        // Agents by name, then other programs, then the shell, where docs,
        // pinned, stays with its gold pin; each bar ends in live/total.
        let headers: Vec<usize> = ["claude", "codex", "opencode", "cargo", "shell"]
            .iter()
            .map(|header| bar(&list, header))
            .collect();
        assert!(headers.windows(2).all(|w| w[0] < w[1]), "{wide}");
        assert!(list[headers[0]].ends_with(" 3/3 live ▾"), "{wide}");
        assert!(list[headers[1]].ends_with(" 1/2 live ▾"), "{wide}");
        // Inside an app, by status; the dead Codex session stays under
        // codex.
        assert!(at(&list, "release-notes") < at(&list, "api-refactor"));
        assert!(at(&list, "api-refactor") < at(&list, "styles"));
        assert!(headers[1] < at(&list, "storybook") && at(&list, "storybook") < headers[2]);
        assert!(headers[4] < at(&list, " docs "), "{wide}");
        assert!(list[at(&list, " docs ")].starts_with("  ★ docs "), "{wide}");

        // Tall enough for every status: each session has a place line too.
        h.app.show_tab(Grouping::Status);
        let wide = render(&mut h.app, 160, 80);
        let list = list_column(&wide, width);
        assert!(list[1].starts_with("   ★ Pinned    ◔ Status ▾"), "{wide}");
        let bar = |list: &[String], name: &str| {
            list.iter()
                .position(|l| l.trim_end().ends_with(['▾', '▸']) && l.contains(name))
                .unwrap_or_else(|| panic!("{name}'s bar missing: {wide}"))
        };
        let headers: Vec<usize> = [
            " ? waiting for you ",
            " ● unread ",
            " working ",
            " waiting for response ",
            " ● running ",
            " ○ dead ",
            " ○ ready ",
        ]
        .iter()
        .map(|header| bar(&list, header))
        .collect();
        assert!(headers.windows(2).all(|w| w[0] < w[1]), "{wide}");
        assert!(list[headers[4]].ends_with(" 2 sessions ▾"), "{wide}");
        assert!(list[headers[0]].ends_with(" 1 session ▾"), "{wide}");
        assert!(headers[0] < at(&list, "review-pr-412") && at(&list, "review-pr-412") < headers[1]);
        assert!(headers[3] < at(&list, "frontend") && at(&list, "frontend") < headers[4]);
        assert!(
            !list
                .iter()
                .any(|l| l.contains(" exited ") && l.trim_end().ends_with('▾')),
            "no empty statuses: {wide}"
        );
    }

    #[test]
    fn the_tabs_and_headers_fit_the_list_at_every_width() {
        use crate::app::grouping::{Grouping, Section, StatusGroup};
        for (width, height) in [(40, 30), (80, 24), (120, 40), (160, 48)] {
            for grouping in Grouping::ALL {
                let mut h =
                    harness_with_folders(agents_and_statuses(), &known(), SessionLayout::Sidebar);
                h.app.show_tab(grouping);
                let screen = render(&mut h.app, width, height);
                let list_width = if width < 100 {
                    usize::from(width)
                } else {
                    usize::from(crate::layout::sidebar_width(width))
                };
                let list = list_column(&screen, list_width);
                // Every tab, the active one with its label, and its ▾ where
                // the list has room: four names and their arrows' room take
                // 45 columns.
                for mark in ["★ Pinned", "▤ Repos", "◇ Agent", "◔ Status"] {
                    assert!(list[1].contains(mark), "{width}x{height}: {screen}");
                }
                let active = format!("{} ▾", h.app.tab_label(grouping));
                let arrow = list_width >= 45;
                assert_eq!(
                    list[1].contains(&active),
                    arrow,
                    "{width}x{height}: {screen}"
                );
                assert_eq!(list[1].contains('▾'), arrow, "{width}x{height}: {screen}");
                // Sub-group bars end in their count: nothing is cut off.
                let subgroup_bar = |line: &&String| {
                    let words: String = line.chars().skip(4).collect();
                    match line.chars().nth(2) {
                        Some(' ') | None => false,
                        Some('◷') => true,
                        Some(_) => StatusGroup::ALL
                            .iter()
                            .any(|status| words.starts_with(status.label())),
                    }
                };
                for line in list.iter().filter(subgroup_bar) {
                    assert!(
                        line.ends_with(" session") || line.ends_with(" sessions"),
                        "{width}x{height}: {line:?}\n{screen}"
                    );
                }
            }
        }
        // A long filter name and a text filter on a phone: the other tabs
        // keep their marks and the text filter shows at the right end.
        let mut h = harness_with_folders(agents_and_statuses(), &known(), SessionLayout::Sidebar);
        h.app.show_sections(
            Grouping::Status,
            vec![Section::Status(StatusGroup::WaitingForResponse)],
        );
        h.app.list.filter.set("front");
        let screen = render(&mut h.app, 40, 30);
        let tabs = screen.lines().nth(1).unwrap();
        assert!(
            tabs.starts_with("   ★ ◔ waiting for respo… ▾ ▤ ◇"),
            "{tabs:?}"
        );
        assert!(tabs.ends_with("/front "), "{tabs:?}");
    }

    #[test]
    fn four_tabs_show_marks_and_the_active_name_in_a_28_column_list() {
        use crate::app::grouping::Grouping;
        let mut h = harness_with_folders(agents_and_statuses(), &known(), SessionLayout::Sidebar);
        h.app.settings.sessions.sidebar_width = Some(28);
        // Too short for four names: the active tab's name and arrow, the
        // others' marks alone.
        for (grouping, row) in [
            (Grouping::Pinned, "   ★ Pinned ▾  ◔  ▤  ◇"),
            (Grouping::Status, "   ★  ◔ Status ▾  ▤  ◇"),
            (Grouping::Repo, "   ★  ◔  ▤ Repos ▾  ◇"),
            (Grouping::App, "   ★  ◔  ▤  ◇ Agent ▾"),
        ] {
            h.app.show_tab(grouping);
            let screen = render(&mut h.app, 160, 48);
            assert_eq!(h.app.screen().list.map(|list| list.width), Some(28));
            assert_eq!(list_column(&screen, 28)[1], row, "{grouping:?}: {screen}");
        }
        // The active one is still underlined.
        h.app.show_tab(Grouping::Repo);
        let screen = render(&mut h.app, 160, 48);
        let under = list_column(&screen, 28)[2].clone();
        assert_eq!(under, format!("   {}{}", " ".repeat(6), "▁".repeat(9)));
        // A list of 45 columns has every name and the arrow.
        h.app.settings.sessions.sidebar_width = Some(45);
        h.app.show_tab(Grouping::Repo);
        let screen = render(&mut h.app, 160, 48);
        let tabs = list_column(&screen, 45)[1].clone();
        for name in ["★ Pinned", "◔ Status", "▤ Repos ▾", "◇ Agent"] {
            assert!(tabs.contains(name), "{screen}");
        }
    }

    #[tokio::test]
    async fn the_app_and_status_modals_list_each_one_with_its_sessions() {
        use crate::app::grouping::Grouping;
        let mut h = harness_with_folders(agents_and_statuses(), &known(), SessionLayout::Sidebar);
        h.app.show_tab(Grouping::App);
        h.app.list.select(crate::app::Selection::Folders);
        h.press(KeyCode::Enter).await;
        let wide = render(&mut h.app, 160, 48);
        let lines: Vec<&str> = wide.lines().collect();
        assert!(lines[1].contains("◇ Agent ▴"), "{wide}");
        assert!(wide.contains("┌ Agents ─"), "{wide}");
        assert!(wide.contains("❯ Filter agents"), "{wide}");
        let row = |name: &str| {
            lines
                .iter()
                .position(|l| l.contains(&format!("] {name} ")))
                .unwrap_or_else(|| panic!("{name} missing: {wide}"))
        };
        assert!(
            lines[row("All agents")].contains("▎ [x] All agents  7/8 live"),
            "{wide}"
        );
        assert!(
            lines[row("claude")].contains("[ ] claude      3/3 live"),
            "{wide}"
        );
        assert!(
            lines[row("codex")].contains("[ ] codex       1/2 live"),
            "{wide}"
        );
        let order: Vec<usize> = ["claude", "codex", "opencode", "cargo", "shell"]
            .iter()
            .map(|name| row(name))
            .collect();
        assert!(order.windows(2).all(|w| w[0] < w[1]), "{wide}");
        assert!(
            wide.contains("⏎ show all  Tab/Space tick  Esc close"),
            "{wide}"
        );
        assert!(h.app.hits.iter().any(|(_, hit)| *hit == Hit::SectionRow(5)));
        // Something typed: Space types, so only Tab ticks.
        h.type_text("c").await;
        let wide = render(&mut h.app, 160, 48);
        assert!(wide.contains("⏎ show  Tab tick  Esc close"), "{wide}");
        h.press(KeyCode::Esc).await;

        h.app.show_tab(Grouping::Status);
        h.app.list.select(crate::app::Selection::Folders);
        h.press(KeyCode::Enter).await;
        for (width, height) in [(160, 48), (80, 24), (40, 30)] {
            let screen = render(&mut h.app, width, height);
            assert!(screen.contains("┌ Statuses ─"), "{screen}");
            assert!(screen.contains("[x] All statuses"), "{screen}");
            assert!(screen.contains("[ ] ? waiting for"), "{screen}");
            // Statuses nothing is in yet are listed, to pick them anyway.
            assert!(screen.contains("[ ] ✕ exited"), "{screen}");
            for row in screen.lines().filter(|l| l.contains("] ")) {
                let tick = row.find("] ").unwrap();
                let left = row[..tick].rfind('│').unwrap();
                let inside: String = row[left + '│'.len_utf8()..]
                    .chars()
                    .take_while(|c| *c != '│')
                    .collect();
                assert!(inside.ends_with(' '), "{row:?}\n{screen}");
            }
        }
        let wide = render(&mut h.app, 160, 48);
        assert!(
            wide.contains("✕ exited                no sessions"),
            "{wide}"
        );
        assert!(
            wide.contains("● running               2 sessions"),
            "{wide}"
        );
    }

    #[test]
    fn the_tabs_layout_says_the_active_tabs_filter() {
        use crate::app::grouping::Grouping;
        let mut h = harness_with_folders(agents_and_statuses(), &known(), SessionLayout::Tabs);
        h.app.show_sections(
            Grouping::App,
            vec![crate::app::grouping::Section::App(
                crate::app::grouping::AppKey::new("claude"),
            )],
        );
        let screen = render(&mut h.app, 160, 48);
        let top = screen.lines().next().unwrap();
        assert!(top.starts_with(" ◇ claude ▾ "), "{top:?}");
        assert!(!screen.contains("frontend"), "{screen}");
    }

    /// [`agents_and_statuses`] with dev-server pinned too (docs is), and
    /// the drawer's Pinned tab's order dev-server first.
    fn two_pinned(layout: SessionLayout) -> Harness {
        use session_core::ids::SessionId;
        let mut sessions = agents_and_statuses();
        for session in sessions.iter_mut().filter(|s| s.name == "dev-server") {
            session.pinned = true;
        }
        let mut h = harness_with_folders(sessions, &known(), layout);
        h.app
            .list
            .set_section_order(&crate::app::config::SectionOrder::from([(
                "pinned".to_string(),
                vec![SessionId::from_string("W2"), SessionId::from_string("R4")],
            )]));
        h
    }

    #[test]
    fn the_lists_first_tab_shows_the_pinned_sessions_by_status_or_says_none() {
        use crate::app::grouping::Grouping;
        for (width, height) in [(40, 40), (100, 40), (160, 48)] {
            let mut h = two_pinned(SessionLayout::Sidebar);
            h.app.show_pinned();
            let screen = render(&mut h.app, width, height);
            let list_width = if width < 100 {
                usize::from(width)
            } else {
                usize::from(crate::layout::sidebar_width(width))
            };
            let list = list_column(&screen, list_width);
            assert!(list[1].contains("★ Pinned"), "{width}: {screen}");
            let at = |name: &str| list[1].find(name).unwrap_or(usize::MAX);
            assert!(at("★") < at("◔"), "{width}: {:?}", list[1]);
            // The two, under their statuses; nothing else.
            for name in [" dev-server ", " docs "] {
                assert!(list.iter().any(|l| l.contains(name)), "{width}: {screen}");
            }
            assert!(!list.iter().any(|l| l.contains("frontend")), "{screen}");
            // The other tabs still mark them.
            for grouping in [Grouping::Status, Grouping::App] {
                h.app.show_tab(grouping);
                let screen = render(&mut h.app, width, 80);
                let list = list_column(&screen, list_width);
                assert!(
                    list.iter().any(|l| l.contains("★ docs ")),
                    "{width} {grouping:?}: {screen}"
                );
            }
        }
        // Nothing pinned: a short line, and the drawer has no Pinned.
        let mut h = harness_with_folders(three_folders(), &known(), SessionLayout::Sidebar);
        for session in h.app.list.sessions().to_vec() {
            if session.pinned {
                let mut unpinned = session.clone();
                unpinned.pinned = false;
                h.app.list.upsert(unpinned);
            }
        }
        h.app.show_pinned();
        let screen = render(&mut h.app, 160, 48);
        assert!(screen.contains("   no pinned items"), "{screen}");
    }

    #[test]
    fn the_four_tabs_fit_a_22_28_and_40_column_list() {
        use crate::app::grouping::Grouping;
        for (width, list_width) in [(160, 22), (160, 28), (40, 40)] {
            let mut h =
                harness_with_folders(agents_and_statuses(), &known(), SessionLayout::Sidebar);
            if width >= 100 {
                h.app.settings.sessions.sidebar_width = Some(list_width);
            }
            for grouping in Grouping::ALL {
                h.app.show_tab(grouping);
                let screen = render(&mut h.app, width, 30);
                let row: String = screen
                    .lines()
                    .nth(1)
                    .unwrap()
                    .chars()
                    .take(usize::from(list_width))
                    .collect();
                // Each tab's mark, the active one's name, and a column
                // left before the divider.
                for mark in ["★", "◔", "▤", "◇"] {
                    assert!(row.contains(mark), "{list_width} {grouping:?}: {row:?}");
                }
                assert!(
                    row.contains(&h.app.tab_label(grouping)),
                    "{list_width} {grouping:?}: {row:?}"
                );
                assert!(row.ends_with(' '), "{list_width} {grouping:?}: {row:?}");
                if list_width == 40 {
                    // A phone: every name; four leave no room for the ▾.
                    for name in ["★ Pinned", "◔ Status", "▤ Repos", "◇ Agent"] {
                        assert!(row.contains(name), "{grouping:?}: {row:?}");
                    }
                    assert!(!row.contains('▾'), "{grouping:?}: {row:?}");
                }
            }
        }
    }

    #[test]
    fn the_tabs_layout_keeps_pinned_sessions_in_their_folders() {
        use crate::app::grouping::Grouping;
        // Tabs: the tab bar is in the Folders order, pinned sessions where
        // their folders put them, with ★ after their name; under Status too.
        let mut h = two_pinned(SessionLayout::Tabs);
        let screen = render(&mut h.app, 200, 30);
        let top = screen.lines().next().unwrap();
        assert!(top.starts_with(" ▤ Repos ▾ "), "{top:?}");
        let order = |top: &str, names: &[&str]| {
            let at: Vec<usize> = names
                .iter()
                .map(|name| top.find(name).unwrap_or_else(|| panic!("{name}: {top:?}")))
                .collect();
            at.windows(2).all(|w| w[0] < w[1])
        };
        let folders_order = ["docs ★", "api-refactor", "dev-server ★", "frontend"];
        assert!(order(top, &folders_order), "{top:?}");
        h.app.show_tab(Grouping::Status);
        let screen = render(&mut h.app, 200, 30);
        let top = screen.lines().next().unwrap();
        assert!(top.starts_with(" ◔ Status ▾ "), "{top:?}");
        assert!(order(top, &folders_order), "{top:?}");
    }

    fn known() -> Vec<(&'static str, Option<session_core::model::GitInfo>)> {
        vec![
            ("/repo", on_branch("sessions-refactor")),
            ("/code/web", on_branch("main")),
            ("/code/api", None),
            ("/code/empty", on_branch("trunk")),
        ]
    }

    /// Rows of the list column (the left `width` columns), trimmed.
    fn list_column(screen: &str, width: usize) -> Vec<String> {
        screen
            .lines()
            .map(|l| {
                l.chars()
                    .take(width)
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect()
    }

    #[test]
    fn one_repository_still_has_its_bar_and_no_branch_while_places_are_closed() {
        let sessions: Vec<SessionView> = three_folders()
            .into_iter()
            .filter(|s| s.scope == scope())
            .collect();
        let mut h = harness_with_folders(sessions, &known(), SessionLayout::Sidebar);
        let wide = render(&mut h.app, 160, 48);
        let list = list_column(&wide, crate::layout::sidebar_width(160).into());
        // The tabs: a padded band above the sessions, the active tab
        // underlined on its last row.
        assert!(list[0].is_empty(), "{wide}");
        // `★ Pinned` and `◔ Status`, each with its arrow's room and two
        // columns.
        let under = format!("   {}{}", " ".repeat(24), "▁".repeat(9));
        assert_eq!(list[2], under, "{wide}");
        // The Pinned and Status tabs, then a folder icon and Repos, with
        // ▾: it opens the Folders modal. Then the Agent tab.
        assert!(
            list[1].starts_with("   ★ Pinned    ◔ Status    ▤ Repos ▾"),
            "{wide}"
        );
        assert!(list[1].ends_with("▤ Repos ▾  ◇ Agent"), "{wide}");
        // A pinned session in its place among its status's, its gold pin
        // in the gutter; under its name its repositories, then its app.
        let docs = list.iter().position(|l| l.contains("docs")).unwrap();
        let refactor = list
            .iter()
            .position(|l| l.contains("api-refactor"))
            .unwrap();
        assert!(refactor < docs, "working comes before running: {wide}");
        assert!(list[docs].contains(" docs "), "{wide}");
        assert!(list[docs].starts_with("  ★ docs "), "{wide}");
        // Its repositories, the toggle in the gutter; no folder or branch.
        assert!(list[docs + 1].starts_with("  ▸ repo"), "{wide}");
        assert!(list[refactor + 2].contains("claude"), "{wide}");
        // No branch while the places are closed.
        assert!(!wide.contains("⎇ sessions-refactor"), "{wide}");
        assert!(!wide.contains("worktree"), "{wide}");
        // The lone repository still has its bar, with its name.
        assert!(
            list.iter()
                .any(|l| l.starts_with("   repo ") && l.ends_with(" 2/2 live ▾")),
            "the repository's bar: {wide}"
        );
        // No header above the list: the folder filter says what's shown.
        assert!(wide.lines().last().unwrap().contains(" All "));
        // Narrow, the list keeps the selection in view.
        let narrow = render(&mut h.app, 80, 24);
        assert!(narrow.contains("▤ Repos"), "{narrow}");
        assert!(narrow.contains("api-refactor"), "{narrow}");
    }

    #[test]
    fn a_worktree_goes_under_its_repository_named_by_its_origin() {
        // The folder `/repo` is a checkout of `monorepo` (its origin);
        // one session works in its main checkout, one in a worktree of it.
        let repo = |root: &str, branch: &str, worktree: Option<&str>| {
            Some(session_core::model::GitInfo {
                repo_root: root.into(),
                branch: Some(branch.into()),
                detached_at: None,
                worktree: worktree.map(Into::into),
                repo: Some("monorepo".into()),
                origin: None,
            })
        };
        let mut main = view("A", "main-work", 10);
        main.git = repo("/repo", "main", None);
        let mut feature = view("B", "feature-work", 20);
        feature.git = repo("/repo/worktrees/feature", "feat/login", Some("feature"));
        let mut h = harness(vec![main, feature]).on_repos_tab();
        let wide = render(&mut h.app, 160, 48);
        let list = list_column(&wide, crate::layout::sidebar_width(160).into());
        // One bar, the repository's, with both sessions under it at the
        // same depth; no worktree header, no folder name.
        let bars: Vec<&String> = list.iter().filter(|l| l.ends_with('▾')).collect();
        assert_eq!(bars.len(), 1, "{wide}");
        assert!(bars[0].starts_with("   monorepo "), "{wide}");
        assert!(bars[0].ends_with(" 2/2 live ▾"), "{wide}");
        assert!(!wide.contains("⎇ worktrees/feature"), "{wide}");
        let main = list.iter().position(|l| l.contains("main-work")).unwrap();
        let work = list
            .iter()
            .position(|l| l.contains("feature-work"))
            .unwrap();
        assert!(list[main].starts_with("    main-work "), "{wide}");
        assert!(list[work].starts_with("    feature-work "), "{wide}");
        assert!(list[work + 1].starts_with("  ▸ monorepo"), "{wide}");
    }

    #[test]
    fn the_folder_filter_says_what_is_shown() {
        for layout in [SessionLayout::Sidebar, SessionLayout::Tabs] {
            let mut h = harness_with_folders(three_folders(), &known(), layout);
            let first_lines = |h: &mut Harness| {
                let screen = render(&mut h.app, 160, 48);
                screen.lines().take(2).collect::<Vec<_>>().join("\n")
            };
            assert!(first_lines(&mut h).contains("▤ Repos"), "{layout:?}");
            // One folder: its name.
            h.app.show_folders(vec![folder("/code/web")]);
            let top = first_lines(&mut h);
            match layout {
                SessionLayout::Sidebar => {
                    assert!(top.contains("   ◔ Status    ▤ web ▾  ◇ Agent"), "{top}")
                }
                SessionLayout::Tabs => assert!(top.starts_with(" ▤ web ▾ "), "{top}"),
            }
            // Several: how many.
            h.app
                .show_folders(vec![folder("/code/web"), folder("/code/api")]);
            assert!(first_lines(&mut h).contains("▤ 2 folders"), "{layout:?}");
            // The icon follows the setting.
            h.app.settings.sessions.icons = crate::icons::Icons::Emoji;
            assert!(first_lines(&mut h).contains("📁"), "{layout:?}");
        }
    }

    #[test]
    fn empty_states_say_which_folder() {
        let mut h = harness_with_folders(three_folders(), &known(), SessionLayout::Sidebar);
        // A folder without sessions: its name, and `n` starts one there.
        h.app.show_folders(vec![folder("/code/empty")]);
        let wide = render(&mut h.app, 160, 48);
        assert!(wide.contains("No sessions in empty yet"), "{wide}");
        assert!(wide.contains("Press n to start one in empty"), "{wide}");
        // Its name on the Repos tab, without a branch.
        assert!(wide.contains(" ▤ empty ▾  ◇"), "{wide}");
        assert!(!wide.contains("⎇ trunk"), "{wide}");
        let narrow = render(&mut h.app, 80, 24);
        assert!(narrow.contains("no sessions yet"), "{narrow}");
        // On the folder filter: what it shows, and how to change it.
        h.app.list.select(crate::app::Selection::Folders);
        let wide = render(&mut h.app, 160, 48);
        assert!(wide.contains("empty  /code/empty"), "{wide}");
        assert!(
            wide.contains("Press Enter to filter by folder or add one."),
            "{wide}"
        );
    }

    #[test]
    fn icons_follow_the_setting_and_keep_the_list_aligned() {
        use crate::icons::Icons;
        let mut h = harness_with_folders(three_folders(), &known(), SessionLayout::Sidebar);
        // Branches show in the sessions' places, open.
        h.app.toggle_all_places();
        for icons in Icons::ALL {
            h.app.settings.sessions.icons = icons;
            for (width, height) in SIZES {
                let screen = render(&mut h.app, width, height);
                assert!(screen.contains(icons.folder()), "{icons:?}: {screen}");
                assert!(screen.contains(icons.branch()), "{icons:?}: {screen}");
                if width < 100 {
                    continue;
                }
                // Each cell is one character here, so the divider's
                // position is its column: the same on every row.
                let columns: Vec<usize> = screen
                    .lines()
                    .filter_map(|l| l.chars().position(|c| c == '│'))
                    .collect();
                assert!(
                    columns.windows(2).all(|w| w[0] == w[1]),
                    "{icons:?} {columns:?}: {screen}"
                );
            }
        }
    }

    #[test]
    fn the_repos_tab_groups_sessions_under_a_bar_per_repository() {
        let mut h = harness_with_folders(three_folders(), &known(), SessionLayout::Sidebar);
        let wide = render(&mut h.app, 160, 48);
        let list = list_column(&wide, crate::layout::sidebar_width(160).into());
        assert!(
            list[1].starts_with("   ★ Pinned    ◔ Status    ▤ Repos ▾"),
            "{wide}"
        );
        let at = |needle: &str| {
            list.iter()
                .position(|l| l.contains(needle))
                .unwrap_or_else(|| panic!("{needle} missing: {wide}"))
        };
        // A repository's bar, not a session's line naming it.
        let bar = |name: &str| {
            list.iter()
                .position(|l| l.starts_with(&format!("   {name} ")) && l.ends_with('▾'))
                .unwrap_or_else(|| panic!("{name}'s bar missing: {wide}"))
        };
        // The launch folder's repository first, then by name: `api` (a
        // folder outside any repository, by its name), then `web` (named by
        // its origin).
        let (repo, api, web) = (bar("repo"), bar("api"), bar("web"));
        assert!(repo < api && api < web, "{wide}");
        assert!(list[repo].ends_with(" 2/2 live ▾"), "{wide}");
        assert!(list[web].ends_with(" 1/2 live ▾"), "{wide}");
        // Sessions' text starts a column in from the bars' names, under
        // their status's row (a pinned session's pin in the gutter left of
        // it).
        assert!(list[at(" docs ")].starts_with("  ★ docs "), "{wide}");
        // A pinned session stays in its repository, its gold pin shown; the
        // others' pins show only under the pointer, and the only other ★
        // is the Pinned tab's.
        assert!(repo < at(" docs ") && at(" docs ") < api, "{wide}");
        assert_eq!(wide.matches('★').count(), 2, "{wide}");
        // A worktree's session is under its repository, no header of its
        // own; by status, what waits for you first.
        assert!(
            web < at("frontend") && at("frontend") < at("storybook"),
            "{wide}"
        );
        assert!(!wide.contains("worktree"), "{wide}");
        // Under a name, the repositories, then the program and state.
        assert!(list[at("frontend") + 1].contains(" web"), "{wide}");
        assert!(list[at("frontend") + 2].contains("codex"), "{wide}");
        assert!(list[at("storybook") + 2].contains("dead"), "{wide}");
        assert!(list[at("server") + 2].contains("cargo"), "{wide}");
        // A bar has a background of its own; selected, the selection's.
        let p = theme::palette();
        let bar_bg = |h: &mut crate::app::testing::Harness, name: &str| {
            let mut terminal = Terminal::new(TestBackend::new(160, 48)).unwrap();
            terminal.draw(|f| draw(f, &mut h.app)).unwrap();
            let buffer = terminal.backend().buffer().clone();
            let row = (0..48)
                .find(|&y| {
                    (0..40)
                        .map(|x| buffer.get(x, y).symbol())
                        .collect::<String>()
                        .starts_with(&format!("   {name} "))
                })
                .unwrap();
            (buffer.get(0, row).bg, buffer.get(30, row).bg)
        };
        assert_eq!(bar_bg(&mut h, "api"), (p.surface, p.surface));
        h.app.list.select(crate::app::Selection::SectionHeader(
            crate::app::grouping::Section::Repo(crate::app::grouping::RepoKey::named("api")),
        ));
        assert_eq!(bar_bg(&mut h, "api"), (p.selection, p.selection));
        // Narrow: still grouped; the stacked list scrolls to the selected
        // bar, and the session in view stays below it.
        let narrow = render(&mut h.app, 80, 24);
        assert!(narrow.contains("▤ Repos"), "{narrow}");
        assert!(narrow.contains("   api "), "{narrow}");
        assert!(narrow.contains("hello from the fake"), "{narrow}");
        // The footer names the session in view under the bar.
        let footer = narrow.lines().last().unwrap();
        assert!(footer.starts_with(" session: ? frontend "), "{footer:?}");
    }

    #[tokio::test]
    async fn the_folders_modal_lists_all_folders_and_the_folders_with_sessions() {
        let mut h = harness_with_folders(three_folders(), &known(), SessionLayout::Sidebar);
        h.press(KeyCode::Char('P')).await;
        let wide = render(&mut h.app, 160, 48);
        let lines: Vec<&str> = wide.lines().collect();
        assert!(
            lines[1].starts_with("   ★ Pinned    ◔ Status    ▤ Repos"),
            "{wide}"
        );
        assert!(lines[1].contains("▴"), "{wide}");
        assert!(wide.contains("┌ Folders ─"), "{wide}");
        assert!(
            wide.contains("❯ Filter, or type a folder path to add"),
            "{wide}"
        );
        let row = |name: &str| {
            lines
                .iter()
                .find(|l| l.contains(&format!("] {name} ")))
                .unwrap_or_else(|| panic!("{name} missing: {wide}"))
        };
        // All folders first, ticked: there's no filter.
        assert!(row("All folders").contains("▎ [x] All folders"), "{wide}");
        assert!(row("All folders").contains("4/5 live"), "{wide}");
        assert!(row("repo").contains("[ ] repo"), "{wide}");
        assert!(row("repo").contains("⎇ sessions-refactor"), "{wide}");
        assert!(row("repo").contains("2/2 live"), "{wide}");
        assert!(row("web").contains("/code/web"), "{wide}");
        assert!(row("api").contains("1/1 live"), "{wide}");
        // A folder without sessions isn't listed.
        assert!(!wide.contains("] empty"), "{wide}");
        assert!(
            wide.contains("⏎ show all  Tab/Space tick  Esc close"),
            "{wide}"
        );
        // Clicks land on the rows.
        assert!(h.app.hits.iter().any(|(_, hit)| *hit == Hit::FolderRow(3)));
        // On a folder; after ticking another, Enter shows both.
        h.press(KeyCode::Down).await;
        let wide = render(&mut h.app, 160, 48);
        assert!(wide.contains("⏎ show  Tab/Space tick  Esc close"), "{wide}");
        h.press(KeyCode::Tab).await;
        h.press(KeyCode::Down).await;
        let wide = render(&mut h.app, 160, 48);
        assert!(wide.contains("[x] repo"), "{wide}");
        assert!(wide.contains("[ ] All folders"), "{wide}");
        assert!(wide.contains("⏎ show 2 folders  Tab/Space tick"), "{wide}");
        let narrow = render(&mut h.app, 80, 24);
        assert!(narrow.contains("[ ] api"), "{narrow}");
        assert!(narrow.contains("Esc close"), "{narrow}");
        // A typed folder the daemon doesn't know is offered first.
        let dir = tempfile::tempdir().unwrap();
        h.type_text(dir.path().to_str().unwrap()).await;
        let screen = render(&mut h.app, 160, 48);
        assert!(screen.contains("+ Add folder "), "{screen}");
        assert!(screen.contains("⏎ add and show  Tab complete"), "{screen}");
        h.type_text("/new").await;
        let screen = render(&mut h.app, 160, 48);
        assert!(screen.contains("+ Create folder "), "{screen}");
        assert!(screen.contains("⏎ create and show"), "{screen}");
    }

    #[tokio::test]
    async fn the_modal_lists_the_folders_completing_a_typed_path() {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        for name in ["alpha", "beta", ".hidden"] {
            std::fs::create_dir(root.join(name)).unwrap();
        }
        let mut h = harness_with_folders(three_folders(), &known(), SessionLayout::Sidebar);
        h.press(KeyCode::Char('P')).await;
        h.type_text(&format!("{}/", root.display())).await;
        for (width, height) in SIZES {
            let screen = render(&mut h.app, width, height);
            assert!(screen.contains("▎ + Add folder "), "{screen}");
            // Indented under the typed folder, which has the cursor.
            for name in ["alpha", "beta"] {
                let suffix = format!("/{name}/");
                let row = screen
                    .lines()
                    .find(|line| line.contains(&suffix))
                    .unwrap_or_else(|| panic!("{suffix} missing: {screen}"));
                assert!(row.contains("│       "), "{row}");
            }
            assert!(!screen.contains(".hidden"), "{screen}");
            assert!(
                screen.contains("⏎ add and show  Tab complete  ↑↓ select  Esc close"),
                "{screen}"
            );
        }
        h.press(KeyCode::Down).await;
        let screen = render(&mut h.app, 160, 48);
        let alpha = screen
            .lines()
            .find(|line| line.contains("/alpha/"))
            .unwrap_or_else(|| panic!("alpha missing: {screen}"));
        assert!(alpha.contains("│ ▎     "), "{alpha}");
        assert!(
            screen.contains("⏎ add and show  Tab complete  ↑↓ select  Esc close"),
            "{screen}"
        );
        // The filter keys are back once the query isn't a path.
        h.ctrl('u').await;
        let screen = render(&mut h.app, 160, 48);
        assert!(
            screen.contains("⏎ show all  Tab/Space tick  Esc close"),
            "{screen}"
        );
    }

    #[tokio::test]
    async fn modal_rows_stay_inside_the_border() {
        let long = "/home/someone/code/clients/acme/very-long-project-folder-name/checkout";
        let mut known = known();
        known.push((long, on_branch("feature/a-rather-long-branch-name")));
        let mut sessions = three_folders();
        sessions.push(view_in(&folder(long), "L1", "long", ago(10)));
        let mut h = harness_with_folders(sessions, &known, SessionLayout::Sidebar);
        h.press(KeyCode::Char('P')).await;
        for (width, height) in SIZES {
            let screen = render(&mut h.app, width, height);
            let rows: Vec<&str> = screen.lines().filter(|l| l.contains("] ")).collect();
            assert_eq!(rows.len(), 5, "{screen}");
            for row in rows {
                // Between the modal's borders, with a margin on the right.
                let tick = row.find("] ").unwrap();
                let left = row[..tick].rfind('│').unwrap();
                let inside: String = row[left + '│'.len_utf8()..]
                    .chars()
                    .take_while(|c| *c != '│')
                    .collect();
                assert!(inside.ends_with(' '), "{row:?}\n{screen}");
            }
            // Long paths are shortened in the middle.
            let row = screen.lines().find(|l| l.contains("] checkout")).unwrap();
            assert!(row.contains("/home/som"), "{row}");
            assert!(row.contains("…") && row.contains("/checkout │"), "{row}");
        }
    }

    #[test]
    fn tabs_put_the_folder_filter_first_and_pinned_tabs_lead() {
        let mut h = harness_with_folders(three_folders(), &known(), SessionLayout::Tabs);
        for (width, height) in SIZES {
            let screen = render(&mut h.app, width, height);
            let bar = screen.lines().next().unwrap();
            assert!(bar.starts_with(" ▤ Repos ▾  "), "{bar:?}");
        }
        // Wide enough for every tab: the launch folder's first, pinned
        // first, then the other folders' by name.
        let screen = render(&mut h.app, 160, 48);
        let bar = screen.lines().next().unwrap();
        assert!(bar.starts_with(" ▤ Repos ▾   1 ● docs ★ "), "{bar:?}");
        let at = |name: &str| bar.find(name).unwrap();
        assert!(at("api-refactor") < at("server") && at("server") < at("frontend"));
        assert!(h
            .app
            .hits
            .iter()
            .any(|(_, hit)| *hit == Hit::FolderSelector));
    }

    #[tokio::test]
    async fn an_older_daemon_shows_the_launch_folder_only() {
        let fake = FakeApi::without_folders(vec![view("A", "api", 10)]);
        let dir = tempfile::tempdir().unwrap();
        let options = options(&fake, scope(), &dir);
        let mut h = start(options, fake, dir);
        let wide = render(&mut h.app, 160, 48);
        assert!(wide.contains("Restart the Rakumux daemon?"), "{wide}");
        assert!(wide.contains("from an older Rakumux"), "{wide}");
        assert!(wide.contains("[y] Restart"), "{wide}");
        h.press(KeyCode::Esc).await;
        let wide = render(&mut h.app, 160, 48);
        assert!(wide.contains("▤ Repos"), "{wide}");
        assert!(wide.contains("api"), "{wide}");
        h.press(KeyCode::Char('P')).await;
        let wide = render(&mut h.app, 160, 48);
        assert!(!wide.contains("┌ Folders"), "{wide}");
        assert!(wide.contains("doesn't support folders yet"), "{wide}");
    }

    // ------------------------------------------------------------ phones

    /// Portrait with the keyboard down, and up.
    const PHONES: [(u16, u16); 2] = [(45, 40), (40, 20)];

    /// Open dialog `which` on `h` (the Settings dialogs draw over the
    /// Settings modal).
    fn open_dialog(h: &mut Harness, which: &str) {
        if which == "ui restart" {
            h.app.request_restart();
            return;
        }
        use crate::app::types::{
            ConfirmAction, ConfirmDialog, DaemonRestartDialog, RenameDialog, UpgradeDialog,
        };
        use crate::app::TextInput;
        let id = session_core::ids::SessionId::from_string("A");
        h.app.mode = match which {
            "rename" => AppMode::Renaming(RenameDialog {
                session_id: id,
                current_name: "api".into(),
                input: TextInput::new("api"),
                error_message: None,
                busy: false,
            }),
            "delete" => AppMode::Confirm(ConfirmDialog {
                title: "Delete session".into(),
                message: "Delete 'api'? Its process is ended; Session history keeps it 90 days."
                    .into(),
                action: ConfirmAction::DeleteSession {
                    id,
                    name: "api".into(),
                },
            }),
            "restart" => AppMode::DaemonRestart(DaemonRestartDialog {
                daemon_version: Some("0.5.9".into()),
                in_place: false,
                detail: String::new(),
                missing: vec!["folders".into(), "pinning".into(), "git branches".into()],
                restarting: false,
                error: None,
                requested: false,
            }),
            "upgrade" => AppMode::Upgrade(UpgradeDialog {
                current_version: "0.6.0".into(),
                latest_version: Some("0.7.0".into()),
                install_method: None,
                checking: false,
                installing: false,
                update_available: true,
                error_message: None,
                info_message: Some("Version 0.7.0 is available".into()),
                selection: 0,
                ..UpgradeDialog::default()
            }),
            other => panic!("no dialog {other}"),
        };
    }

    /// Every dialog fits a phone: its text and its keys show whole.
    #[tokio::test]
    async fn dialogs_fit_a_phone() {
        let mut h = harness(vec![view("A", "api", 10), view("B", "docs", 20)]);
        let expected: [(&str, &[&str]); 5] = [
            ("rename", &["Current: api", "[Enter] Rename • [Esc] Cancel"]),
            ("delete", &["90 days.", "[y/Enter] Yes • [n/Esc] No"]),
            ("restart", &["them.", "are.", "[y] Restart", "[q] Quit"]),
            (
                "ui restart",
                &[
                    "Restart Rakumux?",
                    "daemon",
                    "sessions",
                    "[y/Enter] Restart",
                    "[n/Esc] Cancel",
                ],
            ),
            (
                "upgrade",
                &[
                    "Update now",
                    "Later",
                    "sessions keep",
                    "[Enter] Confirm",
                    "[Esc] Later",
                ],
            ),
        ];
        for (width, height) in PHONES {
            for (which, texts) in expected {
                open_dialog(&mut h, which);
                let screen = render(&mut h.app, width, height);
                for text in texts {
                    assert!(screen.contains(text), "{which} {width}x{height}: {screen}");
                }
            }
            h.app.mode = AppMode::Normal;
            // Settings shows every choice; Help and the Folders modal open.
            h.press(KeyCode::Char(',')).await;
            for c in "layout".chars() {
                h.press(KeyCode::Char(c)).await;
            }
            let screen = render(&mut h.app, width, height);
            assert!(screen.contains("[Master detail] Tabs"), "{screen}");
            // Esc clears the search, then closes.
            h.press(KeyCode::Esc).await;
            h.press(KeyCode::Esc).await;
            h.press(KeyCode::Char('?')).await;
            let screen = render(&mut h.app, width, height);
            assert!(screen.contains("Help"), "{screen}");
            h.press(KeyCode::Esc).await;
            h.press(KeyCode::Char('P')).await;
            let screen = render(&mut h.app, width, height);
            assert!(screen.contains("┌ Folders"), "{screen}");
            assert!(screen.contains("] All folders"), "{screen}");
            // Its keys keep Enter and Esc whole.
            assert!(screen.contains("⏎ show all"), "{screen}");
            assert!(screen.contains("Esc close"), "{screen}");
            h.press(KeyCode::Esc).await;
        }
    }

    /// The rows of `screen` from `top` to `bottom` (exclusive), joined.
    fn rows(screen: &str, top: u16, bottom: u16) -> String {
        screen
            .lines()
            .skip(usize::from(top))
            .take(usize::from(bottom - top))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// On a phone, one thing at a time: Rakumux mode is the list alone;
    /// going in gives the session the screen under a row of tabs; the
    /// Rakumux key goes back to the list.
    #[tokio::test]
    async fn a_phone_shows_the_list_then_the_session_under_tabs() {
        for (width, height) in PHONES {
            let mut h = harness(vec![aged("A", "api", 1, 20), aged("B", "docs", 2, 10)]);
            let screen = render(&mut h.app, width, height);
            let layout = h.app.screen();
            assert!(!layout.stacked && layout.detail.is_none(), "{screen}");
            assert_eq!(layout.list.unwrap().height, height - 1);
            // Status marks are on the right, after the age.
            for name in ["▤ Repos", "  api ", "  docs ", "+ New session"] {
                assert!(screen.contains(name), "{name}: {screen}");
            }
            // No session on screen.
            assert!(!screen.contains("hello from the fake"), "{screen}");
            assert!(!screen.contains("observing"), "{screen}");
            assert!(screen.lines().last().unwrap().contains(" All "));

            // j: only the selection moves.
            h.press(KeyCode::Char('j')).await;
            let screen = render(&mut h.app, width, height);
            assert!(!screen.contains("hello from the fake"), "{screen}");
            assert_eq!(h.app.focus, Focus::List);

            // Enter: tabs on top, the session below, no list.
            h.press(KeyCode::Enter).await;
            assert_eq!(h.app.focus, Focus::Session);
            let screen = render(&mut h.app, width, height);
            assert!(!screen.contains("▤ Repos"), "{screen}");
            let tabs = screen.lines().next().unwrap();
            assert!(tabs.contains("api") && tabs.contains("docs"), "{tabs:?}");
            assert!(screen
                .lines()
                .nth(1)
                .unwrap()
                .starts_with("hello from the fake"));
            assert!(screen.lines().last().unwrap().contains(" Ctrl-\\ "));
            assert_eq!(h.app.terminal_size(), (width, height - 2));

            // The Rakumux key: back to the list, not the session.
            h.rakumux().await;
            assert_eq!(h.app.focus, Focus::List);
            let screen = render(&mut h.app, width, height);
            assert!(screen.contains("▤ Repos"), "{screen}");
            assert!(!screen.contains("hello from the fake"), "{screen}");
        }
    }

    /// The short stacked list (a narrow window) scrolls to keep the
    /// selection in view, and + New session keeps the last session below
    /// it.
    #[tokio::test]
    async fn the_stacked_list_keeps_the_selection_in_view() {
        let sessions: Vec<SessionView> = (1..=6)
            .map(|i| aged(&format!("S{i}"), &format!("session-{i}"), i, 60 - i))
            .collect();
        let mut h = harness(sessions);
        for (width, height) in [(90, 40), (90, 20)] {
            h.press(KeyCode::Char('g')).await;
            h.press(KeyCode::Char('g')).await;
            h.press(KeyCode::Char('j')).await;
            for _ in 1..=5 {
                h.press(KeyCode::Char('j')).await;
                let screen = render(&mut h.app, width, height);
                let list = h.app.screen().list.unwrap();
                let top = rows(&screen, 0, list.bottom());
                let selected = h.app.list.selected_session().expect("a session");
                let name = format!("  {} ", selected.name);
                assert!(top.contains(&name), "{name} {width}x{height}: {screen}");
                assert!(top.contains("▤ Repos"), "{screen}");
            }
            // + New session: in view, and the last session stays below.
            h.press(KeyCode::Char('G')).await;
            let screen = render(&mut h.app, width, height);
            let layout = h.app.screen();
            let top = rows(&screen, 0, layout.list.unwrap().bottom());
            assert!(top.contains("+ New session"), "{screen}");
            let detail = layout.detail.unwrap();
            let below = rows(&screen, detail.y, detail.bottom());
            assert!(below.contains("hello from the fake"), "{screen}");
            let footer = screen.lines().last().unwrap();
            assert!(footer.contains("session-6"), "{footer:?}");
        }
    }

    /// A click (a tap on a phone) in the middle of the target `hit`.
    fn tap(h: &mut Harness, hit: Hit) {
        let (area, _) = *h
            .app
            .hits
            .iter()
            .find(|(_, candidate)| *candidate == hit)
            .unwrap_or_else(|| panic!("no {hit:?} in {:?}", h.app.hits));
        tap_at(h, area.x + area.width / 2, area.y + area.height / 2);
    }

    fn tap_at(h: &mut Harness, column: u16, row: u16) {
        for kind in [
            MouseEventKind::Down(MouseButton::Left),
            MouseEventKind::Up(MouseButton::Left),
        ] {
            h.app.handle_mouse(MouseEvent {
                kind,
                column,
                row,
                modifiers: KeyModifiers::NONE,
            });
        }
        h.app.process_pending();
    }

    fn modal_tab(h: &Harness) -> Option<crate::help::ModalTab> {
        match &h.app.mode {
            AppMode::Modal { state, .. } => Some(state.tab),
            _ => None,
        }
    }

    #[tokio::test]
    async fn taps_drive_rakumux_on_a_phone() {
        use crate::app::FooterKey;
        use crate::help::{Modal, ModalTab};
        let mut h = harness(vec![aged("A", "api", 1, 20), aged("B", "docs", 2, 10)]);
        let (width, height) = (45, 40);
        render(&mut h.app, width, height);
        // Nowhere to go back to yet: the label, like Esc, goes into the
        // selected session; in a session it comes back out.
        tap(&mut h, Hit::ModeBadge);
        assert_eq!(h.app.focus, Focus::Session);
        render(&mut h.app, width, height);
        tap(&mut h, Hit::ModeBadge);
        assert_eq!(h.app.focus, Focus::List);
        assert!(matches!(h.app.mode, AppMode::Normal));
        h.app.came_from = None;
        render(&mut h.app, width, height);

        // A session row: straight in, the session on screen under tabs.
        let row = h
            .app
            .list
            .rows()
            .iter()
            .position(|r| matches!(r, crate::app::ListRow::Session(s) if s.name == "docs"))
            .unwrap();
        tap(&mut h, Hit::ListRow(row));
        assert_eq!(h.selected().as_deref(), Some("B"));
        assert_eq!(h.app.focus, Focus::Session);
        assert_eq!(h.last_attach().1.role, session_core::api::Role::Controller);
        let screen = render(&mut h.app, width, height);
        assert!(screen.lines().next().unwrap().contains("docs"), "{screen}");

        // A tab: that session, still in session mode.
        let api = h.app.tab_hits.iter().find(|(i, _)| *i == 0).unwrap().1;
        tap_at(&mut h, api.x + 1, api.y);
        assert_eq!(h.selected().as_deref(), Some("A"));
        assert_eq!(h.app.focus, Focus::Session);
        render(&mut h.app, width, height);
        let back = h.app.tab_hits.iter().find(|(i, _)| *i == 1).unwrap().1;
        tap_at(&mut h, back.x + 1, back.y);
        assert_eq!(h.selected().as_deref(), Some("B"));
        let screen = render(&mut h.app, width, height);
        let footer = screen.lines().last().unwrap();
        // The session, the workspace in the middle, and the way out, the one
        // button in a session.
        assert_eq!(footer, " session: ● docs     All      Ctrl-\\  v0.1.0 ");

        // The label: out to Rakumux mode, remembering the session.
        tap(&mut h, Hit::ModeBadge);
        assert_eq!(h.app.focus, Focus::List);
        assert_eq!(
            h.app.came_from.as_ref().map(|id| id.to_string()),
            Some("B".into())
        );
        // The label again: back into it.
        render(&mut h.app, width, height);
        tap(&mut h, Hit::ModeBadge);
        assert_eq!(h.app.focus, Focus::Session);
        assert_eq!(h.selected().as_deref(), Some("B"));

        // `Ctrl-\` does what the label does.
        render(&mut h.app, width, height);
        tap(&mut h, Hit::FooterKey(FooterKey::Rakumux));
        assert_eq!(h.app.focus, Focus::List);

        // `? Help` and `, Settings` open them.
        render(&mut h.app, width, height);
        tap(&mut h, Hit::FooterKey(FooterKey::Help));
        assert_eq!(modal_tab(&h), Some(Modal::Help.first_tab()));
        h.press(KeyCode::Esc).await;
        render(&mut h.app, width, height);
        tap(&mut h, Hit::FooterKey(FooterKey::Settings));
        assert_eq!(modal_tab(&h), Some(ModalTab::Settings));
        h.press(KeyCode::Esc).await;
        h.press(KeyCode::Esc).await; // back into the session
        assert_eq!(h.app.focus, Focus::Session);
        // In a session they're a tap away, after the way out.
        render(&mut h.app, width, height);
        let help = Hit::FooterKey(FooterKey::Help);
        assert!(!h.app.hits.iter().any(|(_, hit)| *hit == help));
        tap(&mut h, Hit::FooterKey(FooterKey::Rakumux));
        assert_eq!(h.app.focus, Focus::List);
        render(&mut h.app, width, height);
        tap(&mut h, help);
        assert_eq!(modal_tab(&h), Some(Modal::Help.first_tab()));
        h.press(KeyCode::Esc).await;

        // While a dialog is open the footer isn't a target.
        h.press(KeyCode::Char('r')).await;
        render(&mut h.app, width, height);
        assert!(!h.app.hits.iter().any(|(_, hit)| *hit == Hit::ModeBadge));
    }

    /// A click on the footer's tip shows the next one, in Rakumux mode.
    #[tokio::test]
    async fn a_click_on_the_tip_shows_the_next_one() {
        let mut h = harness(vec![view("A", "api", 10)]);
        h.app.settings.sessions.tips = true;
        let day = crate::app::tips::today();
        let shown = |h: &Harness| {
            let tip = h.app.tip(day).unwrap();
            format!("{} {}", tip.keys, tip.text)
        };
        let (width, height) = (240, 40);
        let first = shown(&h);
        let screen = render(&mut h.app, width, height);
        let footer = screen.lines().last().unwrap();
        assert!(footer.contains(&first), "{footer:?}");
        tap(&mut h, Hit::FooterTip);
        assert_eq!(h.app.focus, Focus::List);
        assert!(matches!(h.app.mode, AppMode::Normal));
        let second = shown(&h);
        assert_ne!(first, second);
        let screen = render(&mut h.app, width, height);
        let footer = screen.lines().last().unwrap();
        assert!(footer.contains(&second), "{footer:?}");
        assert!(!footer.contains(&first), "{footer:?}");
    }

    /// The footer's targets line up with what it shows.
    #[tokio::test]
    async fn footer_targets_cover_the_label_and_the_buttons() {
        let mut h = harness(vec![view("A", "api", 10)]);
        for (width, height) in [(40, 20), (45, 40), (60, 40), (160, 48)] {
            for session in [false, true] {
                if session {
                    h.press(KeyCode::Enter).await;
                } else if h.app.focus == Focus::Session {
                    h.rakumux().await;
                }
                let screen = render(&mut h.app, width, height);
                let footer: Vec<char> = screen.lines().last().unwrap().chars().collect();
                let text = |area: Rect| -> String {
                    footer[usize::from(area.x)..usize::from(area.right())]
                        .iter()
                        .collect()
                };
                let targets: Vec<(Rect, Hit)> = h
                    .app
                    .hits
                    .iter()
                    .filter(|(area, _)| area.y == height - 1)
                    .copied()
                    .collect();
                let called = if width >= 64 {
                    " All workspaces "
                } else {
                    " All "
                };
                assert_eq!(text(targets[0].0), called);
                assert_eq!(targets[0].1, Hit::ModeBadge);
                for (area, hit) in &targets[1..] {
                    let shown = text(*area);
                    let wanted = match hit {
                        Hit::FooterKey(crate::app::FooterKey::Rakumux) => " Ctrl-\\ ",
                        Hit::FooterKey(crate::app::FooterKey::Help) => " ? Help ",
                        Hit::FooterKey(crate::app::FooterKey::Settings) => " , Settings ",
                        Hit::FooterKey(crate::app::FooterKey::Account) => " a ✓ Signed in ",
                        Hit::FooterKey(crate::app::FooterKey::Feed) => " f ≡ Inbox ",
                        Hit::FooterKey(crate::app::FooterKey::Workspace) => " m ⊞ No workspace ",
                        other => panic!("{other:?}"),
                    };
                    assert_eq!(shown, wanted);
                }
                // Rakumux mode has Help and Settings (the Inbox is the left
                // column's own tab), a session the way out.
                let has = |key| targets.iter().any(|(_, hit)| *hit == Hit::FooterKey(key));
                use crate::app::FooterKey::{Feed, Help, Rakumux, Settings};
                assert!(!has(Feed), "{width} {session}");
                assert_eq!(has(Help), !session, "{width} {session}");
                assert_eq!(has(Settings), !session, "{width} {session}");
                assert_eq!(has(Rakumux), session, "{width} {session}");
            }
        }
    }

    /// Started with `account`, the feed's next answer `page` (when given),
    /// the feed shown with the keys after its first read.
    fn feed_harness(
        account: crate::app::testing::FakeAccount,
        page: Option<rakumux_sensagent::feed::FeedPage>,
    ) -> Harness {
        let fake = FakeApi::new(vec![view("A", "api", 20), view("B", "docs", 10)]);
        let dir = tempfile::tempdir().unwrap();
        let account = std::sync::Arc::new(account);
        let mut options = options(&fake, scope(), &dir);
        options.account = account.clone();
        let mut h = start(options, fake, dir);
        if let Some(page) = page {
            account.queue_feed(page);
        }
        h.app.toggle_feed();
        h.app
            .on_tick(std::time::Instant::now() + std::time::Duration::from_secs(3));
        h.app.process_pending();
        h
    }

    fn reply(
        id: &str,
        session: &str,
        name: &str,
        machine: &str,
        minutes_ago: i64,
        text: &str,
    ) -> rakumux_sensagent::feed::FeedEntry {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64;
        rakumux_sensagent::feed::FeedEntry {
            id: id.into(),
            seq: Some(1),
            machine: machine.into(),
            session_id: session.into(),
            session_name: name.into(),
            folder: Some("repo".into()),
            branch: None,
            agent: "claude-code".into(),
            conversation_id: None,
            text: text.into(),
            truncated: false,
            finished_at: now - minutes_ago * 60_000,
            position: None,
            reply_to: None,
            thread: None,
            prompt: None,
        }
    }

    fn feed_page(
        status: rakumux_sensagent::feed::FeedStatus,
        entries: Vec<rakumux_sensagent::feed::FeedEntry>,
    ) -> rakumux_sensagent::feed::FeedPage {
        rakumux_sensagent::feed::FeedPage {
            status,
            generation: 1,
            revision: 1,
            entries,
            replies: Vec::new(),
            more: false,
            open: None,
            machine: None,
            release: None,
        }
    }

    #[tokio::test]
    async fn the_inbox_shows_finished_replies_in_the_left_column() {
        use rakumux_sensagent::feed::FeedStatus;
        let machine = rakumux_sensagent::feed::machine_name();
        let mut pending = reply(
            "R2",
            "A",
            "api",
            &machine,
            0,
            "Done: the parser handles quoted commas now, and the regression test covers \
             the case from the bug report.\n\nRun cargo test to check.",
        );
        pending.seq = None;
        let elsewhere = reply(
            "R1",
            "Z",
            "nightly-build",
            "lab-box",
            5,
            "The build is green again \u{1b}[31mafter the cache fix.",
        );
        let mut h = feed_harness(
            crate::app::testing::FakeAccount::with_feed(),
            Some(feed_page(FeedStatus::Ready, vec![pending, elsewhere])),
        );
        let wide = render(&mut h.app, 160, 48);
        let screen = h.app.screen();
        let feed = screen.list.unwrap();
        assert_eq!((feed.x, feed.width), (0, 48));
        let pane = list_column(&wide, 48);
        let rows: Vec<&str> = wide.lines().collect();
        // In the sessions' place; the session stays in view, as wide.
        assert!(!wide.contains("◔ Status"), "{wide}");
        assert!(wide.contains("hello from the fake"), "{wide}");
        assert_eq!(screen.detail.unwrap().right(), 160);
        // The column's tabs at its bottom, the Inbox's in the accent.
        assert_eq!(
            pane[45], "   ❯ Workspaces   ≡ Inbox   ♥  Heartbeats     «",
            "{wide}"
        );
        assert_eq!(pane[44], "                  ▔▔▔▔▔▔▔", "{wide}");
        let mut terminal = Terminal::new(TestBackend::new(160, 48)).unwrap();
        terminal.draw(|f| draw(f, &mut h.app)).unwrap();
        let p = crate::theme::palette();
        let mark = terminal.backend().buffer().get(18, 45).fg;
        assert_eq!(mark, p.accent_primary);
        // A band over it names it, as rtop's: its mark and name, underlined,
        // in the accent while it has the keys, on the surface's colour.
        assert_eq!(pane[0], "", "{wide}");
        assert_eq!(pane[1], "   ≡ Inbox", "{wide}");
        assert_eq!(pane[2], "   ▁▁▁▁▁▁▁", "{wide}");
        let band = terminal.backend().buffer();
        assert_eq!(band.get(3, 1).fg, p.accent_primary, "{wide}");
        assert_eq!(band.get(5, 1).fg, p.text_bright, "{wide}");
        assert_eq!(band.get(40, 0).bg, p.surface, "{wide}");
        assert_eq!(band.get(3, 2).fg, p.accent_primary, "{wide}");
        let pane = &pane[3..];
        // Under its time period, the newest first, each a card: when, its
        // session, agent and folder (no branch known) on the top border,
        // then `sending` while it isn't uploaded, `done` once it is (only
        // the mark, short of room).
        assert!(pane[0].starts_with("   ◷ last hour  "), "{wide}");
        assert!(pane[0].ends_with("  2"), "{wide}");
        // The selected card is drawn heavy all round, the others rounded.
        assert!(pane[1].contains(" · now  api  claude · repo ━"), "{wide}");
        assert!(pane[1].starts_with(" ┏ "), "{wide}");
        assert!(pane[1].ends_with(" ┓"), "{wide}");
        assert!(pane[2].starts_with(" ┃ Done: the parser handles"), "{wide}");
        // As tall as what it says, so nothing to expand; a row between
        // cards, the next under the same period.
        let end = pane
            .iter()
            .position(|row| row.starts_with(" ┗━"))
            .unwrap_or_else(|| panic!("{wide}"));
        assert!(!pane[end].contains("more"), "{wide}");
        assert_eq!(pane[end + 1], "", "{wide}");
        let next = &pane[end + 2];
        assert!(next.starts_with(" ╭ "), "{wide}");
        assert!(next.contains(" · 5m  nightly-build  claude"), "{wide}");
        assert!(next.ends_with(" ● ╮"), "{wide}");
        // Its text isn't muted, selected or not.
        let buffer = terminal.backend().buffer();
        // The selected card's heavy border is the accent's while the Inbox
        // has the keys; the next one's is plain.
        assert_eq!(buffer.get(1, 4).fg, p.accent_primary, "{wide}");
        assert_eq!(buffer.get(1, end as u16 + 5).fg, p.border, "{wide}");
        for row in [5, end + 6] {
            assert_eq!(buffer.get(3, row as u16).fg, p.text_primary, "{wide}");
        }
        assert_eq!(
            pane[1].chars().count(),
            pane[2].trim_end_matches(' ').chars().count(),
            "{wide}"
        );
        assert!(!wide.contains('\u{1b}'), "{wide}");
        // The footer's button is the feed's, and it names the selected
        // card's session at its left.
        let footer = rows[47];
        assert!(footer.starts_with(" session: ● api "), "{footer:?}");
        assert!(
            footer.contains(" All ") && !footer.contains("Ctrl-\\"),
            "{footer:?}"
        );
        assert!(!footer.contains("FEED"), "{footer:?}");
        // Enter: the reply whole in the cards' place, the band's `‹` going
        // back (a click on the band too).
        h.press(KeyCode::Enter).await;
        let wide = render(&mut h.app, 160, 48);
        let pane = list_column(&wide, 48);
        assert_eq!(pane[1], "   ‹ ≡ Inbox", "{wide}");
        assert_eq!(pane[2], "     ▁▁▁▁▁▁▁", "{wide}");
        assert!(
            h.app
                .hits
                .iter()
                .any(|(area, hit)| *hit == Hit::FeedBack && area.y == 0 && area.height == 3),
            "{wide}"
        );
        assert!(wide.contains("Enter goes into its session"), "{wide}");
        assert!(wide.contains("Run cargo test to check."), "{wide}");
        assert!(wide.contains("hello from the fake"), "{wide}");
        // A narrow window: all of it, over the tabs.
        h.press(KeyCode::Esc).await;
        let narrow = render(&mut h.app, 90, 40);
        assert!(h.app.screen().divider.is_none());
        assert!(narrow.contains("api  claude · repo"), "{narrow}");
        assert!(
            narrow.contains("❯ Workspaces (w)   ≡ Inbox (f)   ♥  Heartbeats (B)"),
            "{narrow}"
        );
        assert!(!narrow.contains("«"), "{narrow}");
        // Esc: the sessions again.
        h.press(KeyCode::Esc).await;
        let wide = render(&mut h.app, 160, 48);
        assert!(wide.contains("◔ Status ▾"), "{wide}");
        assert!(!wide.contains("api  claude · repo"), "{wide}");
    }

    #[tokio::test]
    async fn a_narrow_pane_puts_the_reply_button_under_the_way_in() {
        use rakumux_sensagent::feed::FeedStatus;
        let mut done = reply("R1", "A", "api", "lab-box", 1, "Done.");
        done.conversation_id = Some("c".into());
        let mut h = feed_harness(
            crate::app::testing::FakeAccount::with_feed(),
            Some(feed_page(FeedStatus::Ready, vec![done])),
        );
        h.press(KeyCode::Enter).await;
        h.app.settings.sessions.sidebar_width = Some(36);
        let wide = render(&mut h.app, 160, 48);
        let pane = list_column(&wide, h.app.left_pane().unwrap().width.into());
        let at = pane
            .iter()
            .position(|row| row.contains("Enter goes into its session"))
            .unwrap_or_else(|| panic!("{wide}"));
        assert!(!pane[at].contains("Reply"), "{wide}");
        assert!(
            pane[at + 1].trim_start().starts_with("↩ Reply  r"),
            "{wide}"
        );
        tap(&mut h, Hit::FeedReply);
        assert!(matches!(h.app.mode, AppMode::FeedReply(_)));
    }

    #[test]
    fn a_card_names_the_branch_and_what_became_of_a_reply() {
        use rakumux_sensagent::feed::{FeedReply, FeedStatus, ReplyStatus};
        let mut answered = reply("E1", "Z", "parser", "lab-box", 3, "Fixed the parser.");
        answered.branch = Some("fix/csv".into());
        answered.conversation_id = Some("c".into());
        let mut page = feed_page(FeedStatus::Ready, vec![answered]);
        page.replies = vec![FeedReply {
            seq: 2,
            id: "R1".into(),
            entry: "E1".into(),
            thread: "E1".into(),
            machine: "lab-box".into(),
            session_id: "Z".into(),
            session_name: "parser".into(),
            folder: None,
            agent: "claude-code".into(),
            conversation_id: "c".into(),
            text: "Now the docs.".into(),
            attachments: Vec::new(),
            from: None,
            status: ReplyStatus::Delivered,
            failure: None,
            response: Some("E2".into()),
            created_at: 1,
            status_at: 1,
        }];
        let mut h = feed_harness(crate::app::testing::FakeAccount::with_feed(), Some(page));
        // A column dragged wide.
        h.app.settings.sessions.sidebar_width = Some(70);
        let wide = render(&mut h.app, 240, 48);
        let pane = list_column(&wide, 70);
        assert!(
            pane[4].contains(" · 3m  parser  claude · repo ⎇ fix/csv ━"),
            "{wide}"
        );
        // Selected, so heavy.
        assert!(pane[4].ends_with("━ ✓ answered ┓"), "{wide}");
        // Short of room, the status's word goes first: the mark stays.
        let phone = render(&mut h.app, 45, 30);
        let top = phone
            .lines()
            .find(|line| line.contains("  parser  "))
            .unwrap_or_else(|| panic!("{phone}"));
        assert!(top.trim_end().ends_with(" ✓ ┓"), "{phone}");
    }

    #[test]
    fn the_feed_says_what_it_is_waiting_for() {
        use crate::app::testing::FakeAccount;
        use rakumux_sensagent::feed::FeedStatus;
        let mut h = feed_harness(FakeAccount::default(), None);
        let screen = render(&mut h.app, 160, 48);
        assert!(screen.contains("Sign in to see your inbox:"), "{screen}");
        assert!(screen.contains(", Settings → Account"), "{screen}");
        let mut h = feed_harness(FakeAccount::signed_in(), None);
        let screen = render(&mut h.app, 160, 48);
        assert!(
            screen.contains("The inbox isn't on for your account yet."),
            "{screen}"
        );
        // A narrow pane wraps it rather than cutting it off.
        h.app.settings.sessions.feed_width = Some(30);
        let screen = render(&mut h.app, 160, 48);
        assert!(screen.contains("The inbox isn't on for"), "{screen}");
        assert!(screen.contains("your account yet."), "{screen}");
        let mut h = feed_harness(FakeAccount::with_feed(), None);
        let screen = render(&mut h.app, 160, 48);
        assert!(screen.contains("No replies yet"), "{screen}");
        let loading = feed_page(FeedStatus::Loading, Vec::new());
        let mut h = feed_harness(FakeAccount::with_feed(), Some(loading));
        let screen = render(&mut h.app, 160, 48);
        assert!(screen.contains("Loading the inbox…"), "{screen}");
        let kept = feed_page(
            FeedStatus::Unavailable,
            vec![reply("R", "A", "api", "lab", 1, "Kept from before.")],
        );
        let mut h = feed_harness(FakeAccount::with_feed(), Some(kept));
        let screen = render(&mut h.app, 160, 48);
        assert!(screen.contains("can't reach rakumux.com"), "{screen}");
        assert!(screen.contains("Kept from before."), "{screen}");
    }

    /// The terminal panel beside or below the session, on a wide window, a
    /// window with the feed docked and a narrow one: `(name, screen)`.
    async fn terminal_panel_renders() -> Vec<(&'static str, String)> {
        let cases: [(&str, u16, u16, char, bool, bool); 9] = [
            ("200-beside", 200, 48, '|', false, false),
            ("200-below", 200, 48, '-', false, false),
            ("200-feed-beside", 200, 48, '|', true, false),
            ("160-beside", 160, 48, '|', false, false),
            ("160-feed-falls-back-below", 160, 48, '|', true, false),
            ("140-falls-back-below", 140, 48, '|', false, false),
            ("140-feed-beside-list-hidden", 140, 48, '|', true, true),
            ("110-falls-back-below", 110, 40, '|', false, false),
            ("phone-below", 45, 30, '|', false, false),
        ];
        let mut out = Vec::new();
        for (name, width, height, key, feed, list_hidden) in cases {
            let mut h = harness(vec![view("A", "api", 20), view("B", "docs", 10)]);
            h.app.resize(width, height);
            if feed {
                h.app.open_rtop();
            }
            render(&mut h.app, width, height);
            if list_hidden {
                h.press(KeyCode::Char('b')).await;
            }
            h.press(KeyCode::Char(key)).await;
            let panel = h.fake.pane_count() - 1;
            h.fake.send_pane(
                panel,
                AttachEvent::Output {
                    offset: 0,
                    bytes: b"$ ls\r\nCargo.toml  src  tests\r\n$ ".to_vec(),
                },
            );
            h.app.process_pending();
            out.push((name, render(&mut h.app, width, height)));
        }
        out
    }

    #[tokio::test]
    async fn the_terminal_panel_draws_beside_or_below_the_session() {
        for (name, screen) in terminal_panel_renders().await {
            let rows: Vec<&str> = screen.lines().collect();
            let header = rows
                .iter()
                .position(|row| row.contains(" Terminal  1  + · "))
                .unwrap_or_else(|| panic!("{name}: no panel\n{screen}"));
            let beside = name.ends_with("beside") || name.ends_with("hidden");
            if beside {
                // The panel's title on the first row, right of the session,
                // with a column of │ down the divider.
                assert_eq!(header, 0, "{name}\n{screen}");
                let at = rows[0].chars().take_while(|c| *c != 'T').count();
                assert!(at > 60, "{name}\n{screen}");
                // The session keeps 60 columns and the panel 40.
                let panel = rows[0].chars().count() - at + 1;
                assert!(panel >= 40, "{name}: {panel}\n{screen}");
                let column = rows[5].chars().nth(at - 2).unwrap();
                assert_eq!(column, '│', "{name}\n{screen}");
            } else {
                assert!(header > 3, "{name}\n{screen}");
                assert!(rows[header - 1].contains("────"), "{name}\n{screen}");
            }
            assert!(screen.contains("Cargo.toml"), "{name}\n{screen}");
            if name.contains("feed") {
                assert!(screen.contains("▥ rtop"), "{name}\n{screen}");
            }
        }
    }

    #[tokio::test]
    async fn cards_name_the_workspace_first_in_its_colour() {
        use rakumux_sensagent::feed::FeedStatus;
        use session_core::workspace::Workspace;
        let workspace = |id: &str, name: &str| Workspace {
            id: session_core::ids::WorkspaceId::from_string(id),
            name: name.into(),
            created_at: 1,
        };
        let (launch, infra) = (workspace("W1", "Launch"), workspace("W2", "Infrastructure"));
        let mut a = view("A", "api", 30);
        a.workspace = Some(launch.id.clone());
        let mut b = view("B", "docs", 20);
        b.workspace = Some(infra.id.clone());
        let fake = FakeApi::new(vec![a, b, view("C", "loose", 10)]);
        {
            let mut state = fake.state.lock().unwrap();
            state.workspaces = vec![launch.clone(), infra];
            state.keeps_workspaces = true;
        }
        let dir = tempfile::tempdir().unwrap();
        let account = std::sync::Arc::new(crate::app::testing::FakeAccount::with_feed());
        let mut options = options(&fake, scope(), &dir);
        options.account = account.clone();
        // Only Launch's sessions are listed.
        options.settings.sessions.workspace = Some(launch.id.as_str().to_owned());
        let mut h = start(options, fake, dir);
        let machine = rakumux_sensagent::feed::machine_name();
        account.queue_feed(feed_page(
            FeedStatus::Ready,
            vec![
                reply("R1", "A", "api", &machine, 4, "One."),
                reply("R2", "B", "docs", &machine, 3, "Two."),
                reply("R3", "C", "loose", &machine, 2, "Three."),
                // Another machine's: no workspace is known.
                reply("R4", "A", "far", "lab-box", 1, "Four."),
            ],
        ));
        h.app.toggle_feed();
        h.app
            .on_tick(std::time::Instant::now() + std::time::Duration::from_secs(3));
        h.app.process_pending();
        let screen = render(&mut h.app, 160, 48);
        for card in [
            " · 4m  Launch › api  ",
            " · 3m  Infrastructure › ",
            " · 2m  loose  ",
            " · 1m  far  ",
        ] {
            assert!(screen.contains(card), "{card}: {screen}");
        }
        // Each in its own colour, unlike the session's.
        let mut terminal = Terminal::new(TestBackend::new(160, 48)).unwrap();
        terminal.draw(|f| draw(f, &mut h.app)).unwrap();
        let buffer = terminal.backend().buffer().clone();
        let colour = |text: &str| {
            let (row, line) = screen
                .lines()
                .enumerate()
                .find(|(_, line)| line.contains(text))
                .unwrap();
            let at = crate::layout::str_width(&line[..line.find(text).unwrap()]);
            buffer.get(at as u16 + 2, row as u16).fg
        };
        let p = crate::theme::palette();
        let (launch, infra) = (colour("  Launch"), colour("  Infrastructure"));
        assert!(p.repo_labels().contains(&launch) && p.repo_labels().contains(&infra));
        assert_ne!(launch, infra);
        assert_ne!(launch, colour("  far"));
        // Read whole, first too.
        h.press(KeyCode::End).await;
        h.press(KeyCode::Enter).await;
        let whole = render(&mut h.app, 160, 48);
        assert!(whole.contains("Launch › api · "), "{whole}");
        h.press(KeyCode::Esc).await;
        // A session of a workspace that isn't shown: Enter goes into it,
        // showing every workspace's, and the Inbox stays.
        h.press(KeyCode::Up).await;
        h.press(KeyCode::Enter).await;
        h.press(KeyCode::Enter).await;
        assert_eq!(h.app.focus, Focus::Session);
        assert_eq!(h.selected().as_deref(), Some("B"));
        assert_eq!(h.app.list.workspace_view(), &crate::app::WorkspaceView::All);
        assert!(h.app.feed_shown());
        // Narrow, the workspace's name is cut before the session's goes.
        h.app.resize(40, 30);
        h.rakumux().await;
        h.press(KeyCode::Esc).await;
        let phone = render(&mut h.app, 40, 30);
        assert!(phone.contains("› docs"), "{phone}");
    }

    #[tokio::test]
    async fn the_inbox_is_sectioned_by_how_long_ago_as_ready_is() {
        use rakumux_sensagent::feed::FeedStatus;
        let entries = [5, 50, 90, 200, 400, 600, 1_300, 3_000, 9_000]
            .into_iter()
            .enumerate()
            .map(|(i, minutes)| {
                reply(
                    &format!("R{i}"),
                    "Z",
                    &format!("s{i}"),
                    "lab",
                    minutes,
                    "Ok.",
                )
            })
            .collect();
        let mut h = feed_harness(
            crate::app::testing::FakeAccount::with_feed(),
            Some(feed_page(FeedStatus::Ready, entries)),
        );
        let screen = render(&mut h.app, 160, 60);
        let pane = list_column(&screen, 48);
        let at = |text: &str| {
            pane.iter()
                .position(|row| row.contains(text))
                .unwrap_or_else(|| panic!("{text}: {screen}"))
        };
        // Each header once, over its own, with how many.
        let mut last = 0;
        for (period, count, first) in [
            ("◷ last hour", 2, "s0"),
            ("◷ last 2 hours", 1, "s2"),
            ("◷ last 4 hours", 1, "s3"),
            ("◷ last 8 hours", 1, "s4"),
            ("◷ last 12 hours", 1, "s5"),
            ("◷ last 24 hours", 1, "s6"),
            ("◷ older", 2, "s7"),
        ] {
            let header = at(period);
            assert!(header >= last, "{screen}");
            assert!(pane[header].ends_with(&format!(" {count}")), "{screen}");
            assert!(
                pane[header + 1].contains(&format!("  {first}  ")),
                "{screen}"
            );
            last = header;
        }
        // Within a day the time and how long ago; older, the date.
        assert!(pane[at("  s0  ")].contains(" · 5m  s0"), "{screen}");
        let old = &pane[at("  s7  ")];
        assert!(!old.contains(" · 2d") && old.contains(':'), "{screen}");
        // Scrolled, the first card shown still says its period; every card
        // can be reached, the selected one whole on screen.
        for _ in 0..9 {
            h.press(KeyCode::Down).await;
            let screen = render(&mut h.app, 160, 20);
            let pane = list_column(&screen, 48);
            assert!(pane[3].contains("◷ "), "{screen}");
            let name = h.app.feed.selected().unwrap().session_name.clone();
            assert!(
                pane.iter().any(|row| row.contains(&format!("  {name}  "))),
                "{name}: {screen}"
            );
        }
        // A phone's card: the time alone.
        let phone = render(&mut h.app, 40, 30);
        assert!(phone.contains("◷ "), "{phone}");
    }

    #[tokio::test]
    async fn f_shows_the_inbox_in_the_sessions_place_and_again_brings_them_back() {
        let mut h = feed_harness(crate::app::testing::FakeAccount::with_feed(), None);
        let screen = render(&mut h.app, 160, 48);
        assert!(screen.contains("No replies yet"), "{screen}");
        assert!(!screen.contains("◔ Status"), "{screen}");
        assert!(!screen.contains("+ New session"), "{screen}");
        h.press(KeyCode::Char('f')).await;
        let screen = render(&mut h.app, 160, 48);
        assert!(screen.contains("◔ Status ▾"), "{screen}");
        assert!(screen.contains("+ New session"), "{screen}");
        assert!(!screen.contains("No replies yet"), "{screen}");
    }

    #[tokio::test]
    async fn the_footers_feed_button_toggles_the_feed_and_counts_what_came_meanwhile() {
        use crate::app::FooterKey;
        use rakumux_sensagent::feed::FeedStatus;
        let fake = FakeApi::new(vec![view("A", "api", 20), view("B", "docs", 10)]);
        let dir = tempfile::tempdir().unwrap();
        let account = std::sync::Arc::new(crate::app::testing::FakeAccount::with_feed());
        let mut options = options(&fake, scope(), &dir);
        options.account = account.clone();
        let mut h = start(options, fake, dir);
        // The column hidden: the footer has the button.
        h.press(KeyCode::Char('b')).await;
        // Two replies finish while it's hidden; and one from before this
        // window started, which isn't new.
        let new = |id: &str| {
            let mut entry = reply(id, "A", "api", "lab-box", 0, "Done.");
            entry.finished_at += 1_000;
            entry
        };
        let (first, second) = (new("R1"), new("R2"));
        let old = reply("R0", "A", "api", "lab-box", 60, "Old.");
        account.queue_feed(feed_page(FeedStatus::Ready, vec![first, second, old]));
        h.app
            .on_tick(std::time::Instant::now() + std::time::Duration::from_secs(1));
        h.app.process_pending();
        let footer = |h: &mut Harness| {
            render(&mut h.app, 160, 48)
                .lines()
                .last()
                .unwrap()
                .to_string()
        };
        let shown = footer(&mut h);
        assert!(shown.contains(" f ≡ Inbox ●2   ? Help "), "{shown:?}");
        // A tap: the column, on the Inbox, with the keys; nothing is new any
        // more, and its tab takes the button's place.
        tap(&mut h, Hit::FooterKey(FooterKey::Feed));
        assert!(h.app.feed_shown());
        assert!(h.app.feed_has_keys());
        let shown = footer(&mut h);
        assert!(
            shown.contains(" All ") && !shown.contains("Ctrl-\\"),
            "{shown:?}"
        );
        assert!(!shown.contains("≡ Inbox"), "{shown:?}");
        // The label does what Esc does: back to the sessions.
        tap(&mut h, Hit::ModeBadge);
        assert!(!h.app.feed_has_keys() && !h.app.feed_shown());
        assert_eq!(h.app.focus, Focus::List);
        // What comes while the sessions show is counted on its tab.
        account.queue_feed(rakumux_sensagent::feed::FeedPage {
            revision: 2,
            ..feed_page(FeedStatus::Ready, vec![new("R3")])
        });
        h.app
            .on_tick(std::time::Instant::now() + std::time::Duration::from_secs(30));
        h.app.process_pending();
        let screen = render(&mut h.app, 160, 48);
        assert!(
            screen.contains("❯ Workspaces   ≡ Inbox ●1   ♥  Heartbeats"),
            "{screen}"
        );
        let phone = render(&mut h.app, 40, 20);
        assert!(phone.contains("≡ Inbox ●1"), "{phone}");
    }

    #[tokio::test]
    async fn on_a_phone_the_feed_fills_the_screen_and_a_reply_is_read_there() {
        use rakumux_sensagent::feed::FeedStatus;
        let elsewhere = reply(
            "R1",
            "Z",
            "nightly-build",
            "lab-box",
            5,
            "The build is green again after the cache fix.",
        );
        let mut h = feed_harness(
            crate::app::testing::FakeAccount::with_feed(),
            Some(feed_page(FeedStatus::Ready, vec![elsewhere])),
        );
        h.app.resize(45, 40);
        let phone = render(&mut h.app, 45, 40);
        assert_eq!(
            phone.lines().nth(37).unwrap().trim_end(),
            "   ❯ Workspaces   ≡ Inbox   ♥  Heartbeats",
            "{phone}"
        );
        // Its band over the cards, on a phone too.
        assert_eq!(
            phone.lines().nth(1).unwrap().trim_end(),
            "   ≡ Inbox",
            "{phone}"
        );
        assert!(phone.contains("nightly-build"), "{phone}");
        assert!(!phone.contains("◔ Status"), "{phone}");
        h.press(KeyCode::Enter).await;
        let reading = render(&mut h.app, 45, 40);
        assert_eq!(
            reading.lines().nth(1).unwrap().trim_end(),
            "   ‹ ≡ Inbox",
            "{reading}"
        );
        assert!(reading.contains("finished 5m ago"), "{reading}");
        assert!(
            reading.contains("The build is green again after the"),
            "{reading}"
        );
        h.press(KeyCode::Esc).await;
        assert!(!render(&mut h.app, 45, 40).contains("‹"));
        h.press(KeyCode::Esc).await;
        let list = render(&mut h.app, 45, 40);
        assert!(list.contains("◔ Status"), "{list}");
        assert!(!list.contains("nightly-build"), "{list}");
    }

    #[tokio::test]
    async fn the_feed_tab_leads_the_tab_bar_and_fills_the_body_when_selected() {
        use rakumux_sensagent::feed::FeedStatus;
        let fake = FakeApi::new(vec![view("A", "api", 20), view("B", "docs", 10)]);
        let dir = tempfile::tempdir().unwrap();
        let account = std::sync::Arc::new(crate::app::testing::FakeAccount::with_feed());
        let mut options = options(&fake, scope(), &dir);
        options.account = account.clone();
        options.settings.sessions.layout = SessionLayout::Tabs;
        let mut h = start(options, fake, dir);
        let done = reply("R1", "A", "api", "lab-box", 2, "All tests pass.");
        account.queue_feed(feed_page(FeedStatus::Ready, vec![done]));
        h.press(KeyCode::Char('f')).await;
        let screen = render(&mut h.app, 160, 48);
        let rows: Vec<&str> = screen.lines().collect();
        // Before the sessions' tabs and a line, unnumbered.
        assert!(
            rows[0].starts_with(" ◔ Status ▾   ≡ Inbox  │  1 ● docs "),
            "{:?}",
            rows[0]
        );
        // Selected: the cards on the left, the selected reply beside them,
        // over the left column's tabs.
        let width = usize::from(crate::layout::sidebar_width(160));
        let cards = list_column(&screen, width);
        assert_eq!(
            cards[45], "   ❯ Workspaces (w)   ≡ Inbox (f)   ♥  Heartbeat",
            "{screen}"
        );
        // Under the band, which spans both.
        assert_eq!(cards[2], "   ≡ Inbox", "{screen}");
        assert!(cards[4].starts_with("   ◷ last hour"), "{screen}");
        assert!(cards[5].contains(" · 2m  api  claude · repo ━"), "{screen}");
        assert!(rows[2].chars().nth(width) == Some(' '), "{screen}");
        assert!(rows[5].chars().nth(width) == Some('│'), "{screen}");
        assert!(screen.contains("finished 2m ago"), "{screen}");
        assert!(!screen.contains("hello from the fake"), "{screen}");
        // The selected tab is highlighted, no session's is.
        let mut terminal = Terminal::new(TestBackend::new(160, 48)).unwrap();
        terminal.draw(|f| draw(f, &mut h.app)).unwrap();
        let p = crate::theme::palette();
        let buffer = terminal.backend().buffer();
        assert_eq!(buffer.get(15, 0).bg, p.surface);
        assert_ne!(buffer.get(26, 0).bg, p.surface);
        // Esc: the session's tab, the Inbox's gone.
        h.press(KeyCode::Esc).await;
        let screen = render(&mut h.app, 160, 48);
        assert!(
            !screen.lines().next().unwrap().contains("≡ Inbox"),
            "{screen}"
        );
        assert!(screen.contains("hello from the fake"), "{screen}");
    }

    /// What the daemon would say: this window, an API, and session A
    /// running claude under bash with an MCP server, B a lone shell.
    fn resource_sample(at_ms: i64, scale: u64) -> session_core::api::ResourceSample {
        use session_core::api::{ClientUsage, MachineUsage, ProcessUsage, SessionUsage};
        let process =
            |pid: u32, parent: Option<u32>, name: &str, cpu_ms: u64, mb: u64| ProcessUsage {
                pid,
                parent,
                name: name.into(),
                command: format!("{name} --resume"),
                cpu_ms: cpu_ms * scale,
                rss_bytes: mb << 20,
                net_rx_bytes: Some(1000 * scale),
                net_tx_bytes: Some(100 * scale),
            };
        session_core::api::ResourceSample {
            at_ms,
            machine: MachineUsage {
                cpus: 8,
                cpu_busy_ms: Some(1000 * scale),
                cpu_total_ms: Some(8000 * scale),
                memory_total_bytes: Some(16 << 30),
                memory_available_bytes: Some(6 << 30),
                net_rx_bytes: Some(5000 * scale),
                net_tx_bytes: Some(500 * scale),
            },
            daemon: process(10, None, "rakumux-ptyd", 100, 30),
            clients: vec![
                ClientUsage {
                    client: "tui".into(),
                    connections: 3,
                    process: process(std::process::id(), None, "tui", 200, 48),
                },
                ClientUsage {
                    client: "api".into(),
                    connections: 1,
                    process: process(40, None, "rakumux-api", 0, 12),
                },
            ],
            sessions: vec![
                SessionUsage {
                    id: session_core::ids::SessionId::from_string("A"),
                    processes: vec![
                        process(30, None, "bash", 0, 5),
                        process(31, Some(30), "claude", 900, 800),
                        process(32, Some(31), "node", 10, 60),
                    ],
                },
                SessionUsage {
                    id: session_core::ids::SessionId::from_string("B"),
                    processes: vec![process(50, None, "zsh", 0, 4)],
                },
            ],
        }
    }

    /// The drawer's column at `x` of a rendered screen, its lines trimmed.
    fn drawer_text(screen: &str, x: usize) -> String {
        screen
            .lines()
            .map(|line| {
                line.chars()
                    .skip(x)
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[tokio::test]
    async fn rtop_from_settings_shows_in_the_drawer_and_goes_into_a_session() {
        let mut h = harness(vec![view("A", "fix-login", 20), view("B", "notes", 10)]);
        h.fake.state.lock().unwrap().resources =
            vec![resource_sample(1_000, 1), resource_sample(2_000, 2)];
        h.press(KeyCode::Char(',')).await;
        let AppMode::Modal { state, .. } = &mut h.app.mode else {
            panic!("Settings didn't open");
        };
        state.selected = SettingsItem::Rtop.index();
        h.press(KeyCode::Enter).await;
        assert!(matches!(h.app.mode, AppMode::Normal));
        assert!(h.app.rtop_has_keys(), "rtop opens with the keys");
        render(&mut h.app, 160, 48);
        assert!(h.app.rtop_shown());
        // The second sample comes soon, with the first rates.
        h.app
            .on_tick(std::time::Instant::now() + std::time::Duration::from_secs(1));
        h.app.process_pending();
        let calls = |h: &Harness| h.fake.calls().iter().filter(|c| *c == "resources").count();
        assert_eq!(calls(&h), 2);

        // Docked on the right, beside the session, under the band's tabs.
        let screen = render(&mut h.app, 160, 48);
        let drawer = h.app.screen().feed.unwrap();
        assert_eq!((drawer.x, drawer.right()), (116, 160));
        let pane = drawer_text(&screen, usize::from(drawer.x));
        for text in [
            "rtop",
            "this window",
            "rakumux-ptyd",
            "rakumux-api",
            "fix-login",
            "notes",
            "claude",
            "MEMORY▾",
            "Total",
            "10 GB/16 GB",
        ] {
            assert!(pane.contains(text), "{text} missing:\n{pane}");
        }
        // 900 ms of CPU in a second: 90% of one CPU.
        let claude = pane.lines().find(|l| l.contains("claude")).unwrap();
        assert!(
            claude.contains("90%") && claude.contains("800 MB"),
            "{claude}"
        );
        // Narrow windows keep the numbers that matter: in the session's
        // place (80 columns), the screen on a phone.
        for (width, height) in [(90, 30), (44, 30)] {
            let screen = render(&mut h.app, width, height);
            assert!(screen.contains("fix-login"), "{width}:\n{screen}");
            assert!(screen.contains("MEMORY"), "{width}:\n{screen}");
        }
        render(&mut h.app, 160, 48);

        // `s` sorts by CPU; down to claude, fold its session from there,
        // then go into it.
        h.press(KeyCode::Char('s')).await;
        let screen = render(&mut h.app, 160, 48);
        assert!(screen.contains("CPU▾"), "{screen}");
        let rows = |h: &Harness| h.app.rtop_rows();
        let claude_at = rows(&h).iter().position(|r| r.name == "claude").unwrap();
        for _ in 0..claude_at {
            h.press(KeyCode::Down).await;
        }
        h.press(KeyCode::Left).await;
        assert!(rows(&h).iter().all(|r| r.name != "claude"));
        h.press(KeyCode::Enter).await;
        assert_eq!(h.selected().as_deref(), Some("A"));
        assert_eq!(h.app.focus, Focus::Session);
        assert!(!h.app.panel_has_keys());
        render(&mut h.app, 160, 48);
        assert!(h.app.rtop_shown(), "it stays beside the session");

        // `t` gives it the keys again; `t` once more hides it, and nothing
        // more is read.
        h.rakumux().await;
        h.press(KeyCode::Char('t')).await;
        assert!(h.app.rtop_has_keys());
        h.press(KeyCode::Char('t')).await;
        assert!(!h.app.drawer_open());
        render(&mut h.app, 160, 48);
        h.app
            .on_tick(std::time::Instant::now() + std::time::Duration::from_secs(5));
        h.app.process_pending();
        let read = calls(&h);
        h.app
            .on_tick(std::time::Instant::now() + std::time::Duration::from_secs(10));
        h.app.process_pending();
        assert_eq!(calls(&h), read, "hidden, rtop reads nothing");
    }

    #[tokio::test]
    async fn rtop_is_beside_the_session_whatever_the_left_column_shows() {
        let mut h = harness(vec![view("A", "fix-login", 20)]);
        h.fake.state.lock().unwrap().resources = vec![resource_sample(1_000, 1)];
        h.press(KeyCode::Char('f')).await;
        assert!(h.app.feed_has_keys());
        render(&mut h.app, 160, 48);
        h.app.process_pending();
        let calls = |h: &Harness| h.fake.calls().iter().filter(|c| *c == "resources").count();
        assert_eq!(calls(&h), 0, "the Inbox doesn't sample");

        // `t` gives it the keys; the Inbox stays in the left column.
        h.press(KeyCode::Char('t')).await;
        assert!(h.app.rtop_has_keys());
        assert!(h.app.feed_shown());
        let screen = render(&mut h.app, 160, 48);
        assert!(screen.contains("▥ rtop"), "{screen}");
        h.app.process_pending();
        assert!(calls(&h) >= 1);

        // Esc gives the Inbox the keys back; rtop stays beside the session.
        h.press(KeyCode::Esc).await;
        assert!(h.app.feed_has_keys());
        assert!(h.app.rtop_shown());
        // f, from there, goes back to the sessions; from rtop, to the Inbox.
        h.press(KeyCode::Char('f')).await;
        assert!(!h.app.panel_has_keys() && h.app.rtop_shown());
        h.press(KeyCode::Char('t')).await;
        h.press(KeyCode::Char('f')).await;
        assert!(h.app.feed_has_keys());
    }

    #[test]
    fn the_left_columns_tabs_stay_at_its_bottom_at_every_size() {
        use crate::app::left::LeftTab;
        for (width, height) in [(160, 48), (100, 30), (90, 40), (45, 40), (40, 20)] {
            let mut h = harness(vec![view("A", "fix-login", 20)]);
            for tab in LeftTab::ALL {
                h.app.resize(width, height);
                h.app.show_left_tab(tab);
                let screen = render(&mut h.app, width, height);
                let pane = h.app.left_pane().unwrap();
                let (body, tabs) = crate::layout::left_split(pane);
                assert_eq!(tabs.bottom(), pane.bottom());
                let row = list_column(&screen, pane.width.into())
                    [usize::from(tabs.y + tabs.height / 2)]
                .clone();
                for label in ["Workspaces", "Inbox", "Heartbeats"] {
                    assert!(row.contains(label), "{width}x{height}: {row:?}");
                }
                // Only the sessions have their own tabs over them.
                assert_eq!(
                    screen.contains("◔ Status"),
                    tab == LeftTab::Sessions,
                    "{width}x{height} {tab:?}: {screen}"
                );
                // Whole on the row, each its own target.
                let targets: Vec<Rect> = h
                    .app
                    .hits
                    .iter()
                    .filter(|(_, hit)| matches!(hit, Hit::LeftTab(_)))
                    .map(|(area, _)| *area)
                    .collect();
                assert_eq!(targets.len(), 3);
                for (i, area) in targets.iter().enumerate() {
                    assert!(area.right() <= pane.right() && area.y >= body.bottom());
                    for other in &targets[i + 1..] {
                        assert!(area.intersection(*other).is_empty());
                    }
                }
            }
        }
        // The active one's line and name are the accent's while it has the
        // keys, quiet in session mode.
        let mut h = harness(vec![view("A", "fix-login", 20)]);
        let mark = |h: &mut Harness| {
            let mut terminal = Terminal::new(TestBackend::new(160, 48)).unwrap();
            terminal.draw(|f| draw(f, &mut h.app)).unwrap();
            let cell = terminal.backend().buffer().get(3, 44).clone();
            (cell.symbol().to_string(), cell.fg)
        };
        let p = crate::theme::palette();
        assert_eq!(mark(&mut h), ("▔".to_string(), p.accent_primary));
        h.app.activate_selected();
        h.app.process_pending();
        assert_eq!(h.app.focus, Focus::Session);
        assert_eq!(mark(&mut h), ("▔".to_string(), p.neutral_gray));
    }

    #[tokio::test]
    async fn the_tabs_layout_gives_rtop_a_tab_and_a_phone_the_screen() {
        let mut h = harness(vec![view("A", "fix-login", 20)]);
        h.fake.state.lock().unwrap().resources = vec![resource_sample(1_000, 1)];
        h.app.set_layout(SessionLayout::Tabs);
        h.press(KeyCode::Char('t')).await;
        let screen = render(&mut h.app, 160, 48);
        let bar = screen.lines().next().unwrap();
        assert!(bar.contains("rtop") && !bar.contains("Inbox"), "{bar}");
        h.app.process_pending();
        let screen = render(&mut h.app, 160, 48);
        // Full width: every column, the processes' arguments too.
        assert!(screen.contains("↓ NET"), "{screen}");
        h.press(KeyCode::Char('f')).await;
        let bar = render(&mut h.app, 160, 48)
            .lines()
            .next()
            .unwrap()
            .to_string();
        assert!(bar.contains(" ≡ Inbox   ▥ rtop  │ "), "{bar}");

        // A phone: the drawer fills the screen while it has the keys; Esc
        // hides it.
        let mut h = harness(vec![view("A", "fix-login", 20)]);
        h.fake.state.lock().unwrap().resources = vec![resource_sample(1_000, 1)];
        render(&mut h.app, 44, 30);
        h.press(KeyCode::Char('t')).await;
        render(&mut h.app, 44, 30);
        assert_eq!(h.app.screen().feed.map(|a| a.width), Some(44));
        h.press(KeyCode::Esc).await;
        assert!(!h.app.drawer_open());
    }

    #[tokio::test]
    async fn rtop_on_an_older_server_says_to_restart_it() {
        let mut h = harness(vec![view("A", "a", 10)]);
        h.press(KeyCode::Char('t')).await;
        render(&mut h.app, 160, 48);
        h.app.process_pending();
        let screen = render(&mut h.app, 160, 48);
        assert!(screen.contains("restart it (S)"), "{screen}");
    }

    /// Forty samples, then rtop at the drawer's widths: docked (44), a
    /// phone's screen (30 to 60, a short one too) and the Tabs layout's
    /// (120, 160). Frames print with `--nocapture`.
    #[tokio::test]
    async fn rtop_draws_what_was_used_over_time_at_every_size() {
        let mut h = harness(vec![view("A", "fix-login", 20), view("B", "notes", 10)]);
        // Uneven steps, so the graphs have a shape.
        let mut scale = 1;
        let (mut busy, mut received) = (0, 0);
        let samples: Vec<_> = (0..40u64)
            .map(|i| {
                scale += 1 + (i * 3) % 5;
                busy += 600 + (i * 1_700) % 5_000;
                received += 2_000 + (i * 7_919) % 40_000;
                let mut sample = resource_sample(1_000 * (i as i64 + 1), scale);
                sample.machine.cpu_busy_ms = Some(busy);
                sample.machine.cpu_total_ms = Some(8_000 * (i + 1));
                sample.machine.net_rx_bytes = Some(received);
                sample
            })
            .collect();
        h.fake.state.lock().unwrap().resources = samples;
        h.press(KeyCode::Char('t')).await;
        render(&mut h.app, 160, 48);
        // A tick a second until every sample was read (the fake answers
        // its last one again after that, with no time between).
        let calls = |h: &Harness| h.fake.calls().iter().filter(|c| *c == "resources").count();
        let start = std::time::Instant::now();
        for second in 0..60 {
            if calls(&h) >= 40 {
                break;
            }
            h.app
                .on_tick(start + std::time::Duration::from_secs(second));
            h.app.process_pending();
        }
        assert_eq!(h.app.rtop.history.machine().len(), 40);
        let braille = |line: &str| line.chars().any(|c| ('\u{2801}'..='\u{28ff}').contains(&c));

        let screen = render(&mut h.app, 160, 48);
        let drawer = h.app.screen().feed.unwrap();
        let pane = drawer_text(&screen, usize::from(drawer.x));
        eprintln!("docked, {} columns:\n{pane}", drawer.width);
        for text in [
            "╭ cpu",
            "╭ mem",
            "╭ net",
            "rakumux ",
            "↓ ",
            "↑ ",
            "Total",
            "MEMORY▾",
        ] {
            assert!(pane.contains(text), "{text} missing:\n{pane}");
        }
        // The selected row's panel: this window, over time.
        assert!(pane.contains("╭ rakumux · this window"), "{pane}");
        let cpu_panel = pane
            .lines()
            .skip_while(|l| !l.contains("╭ cpu"))
            .nth(4)
            .unwrap();
        assert!(braille(cpu_panel), "the machine's CPU over time:\n{pane}");

        for (width, height) in [(30, 20), (30, 40), (44, 30), (60, 40), (44, 14)] {
            let screen = render(&mut h.app, width, height);
            eprintln!("{width}×{height}:\n{screen}");
            assert_eq!(h.app.screen().feed.map(|a| a.width), Some(width));
            assert!(screen.contains("MEMORY"), "{width}×{height}:\n{screen}");
            if height >= 20 {
                assert!(screen.contains("fix-login"), "{width}×{height}:\n{screen}");
            }
            if height >= 30 {
                assert!(screen.contains("╭ cpu"), "{width}×{height}:\n{screen}");
            }
        }
        // Short: a line a metric, no panels.
        let screen = render(&mut h.app, 44, 20);
        eprintln!("44×20:\n{screen}");
        assert!(!screen.contains("╭ cpu"), "{screen}");
        assert!(
            screen
                .lines()
                .any(|l| l.trim_start().starts_with("cpu") && braille(l)),
            "{screen}"
        );

        // Wide: the panels side by side, each row with its graph.
        h.app.set_layout(SessionLayout::Tabs);
        let bar = render(&mut h.app, 120, 40);
        if !bar.lines().next().unwrap_or_default().contains("rtop") {
            h.press(KeyCode::Char('t')).await;
        }
        for (width, height) in [(120, 40), (160, 48)] {
            let screen = render(&mut h.app, width, height);
            eprintln!("Tabs, {width}×{height}:\n{screen}");
            let panels = screen.lines().find(|l| l.contains("╭ cpu")).unwrap();
            assert!(
                panels.contains("╭ mem") && panels.contains("╭ net"),
                "{screen}"
            );
            let claude = screen
                .lines()
                .find(|l| l.contains("claude  --resume"))
                .unwrap();
            assert!(braille(claude), "{claude}");
            assert!(screen.contains("↓ NET"), "{screen}");
        }
        // `s` puts the rows' graphs on CPU.
        h.press(KeyCode::Char('s')).await;
        let screen = render(&mut h.app, 120, 40);
        assert!(screen.contains("CPU▾"), "{screen}");
    }

    /// Folder names, git names and reports come from files and programs:
    /// an escape sequence in one (a window title, a clipboard write, a
    /// link) is shown as text, never sent to the terminal.
    #[test]
    fn control_characters_in_paths_branches_and_reports_are_shown_not_sent() {
        use session_core::model::{GitInfo, Reported};
        let evil = "/repo/evil\u{1b}]0;PWNED\u{7}dir";
        let mut session = view("A", "fix-login", 20);
        session.cwd = evil.into();
        session.git = Some(GitInfo {
            repo_root: evil.into(),
            branch: Some("main\u{1b}]52;c;SGk=\u{7}\u{9b}31m".into()),
            detached_at: None,
            worktree: Some("wt\u{1b}]8;;http://x\u{1b}\\".into()),
            repo: Some("repo".into()),
            origin: None,
        });
        session.reported = Some(Reported {
            state: None,
            text: Some("done\u{1b}[2J\u{7f}\ttok".into()),
            result: None,
            at: 0,
        });
        for layout in SessionLayout::ALL {
            let mut h = with_layout(vec![session.clone()], layout);
            for (width, height) in SIZES {
                let screen = render(&mut h.app, width, height);
                let control = screen.chars().find(|c| c.is_control() && *c != '\n');
                assert_eq!(control, None, "{layout:?} {width}x{height}:\n{screen}");
            }
        }
        // Each shows as `?`, in its place (its places open).
        let mut h = with_layout(vec![session.clone()], SessionLayout::Sidebar);
        h.app.places_open.insert(session.id.clone());
        let screen = render(&mut h.app, 160, 48);
        assert!(screen.contains("evil?]0;PWNED?dir"), "{screen}");
        assert!(screen.contains("main?]52;c;SGk=??31m"), "{screen}");
    }

    fn heartbeat(
        id: &str,
        name: &str,
        waiting: bool,
        prompt: &str,
        command: &str,
    ) -> session_core::heartbeat::Heartbeat {
        session_core::heartbeat::Heartbeat {
            id: session_core::ids::HeartbeatId::from_string(id),
            scope: scope(),
            name: name.into(),
            agent: Some("claude".into()),
            command: command.into(),
            prompt: prompt.into(),
            schedule: Some(session_core::heartbeat::Schedule::Daily { at: "02:00".into() }),
            enabled: !waiting,
            awaiting_approval: waiting,
            created_by: session_core::model::Creator::User,
            created_at: 0,
            updated_at: 0,
            next_run_at: None,
            runs: 0,
            last_run: None,
        }
    }

    #[tokio::test]
    async fn beats_shows_a_heartbeat_its_agent_sets_up_and_n_starts_its_setup() {
        let mut h = harness(vec![view("A", "fix-login", 20)]);
        h.fake.state.lock().unwrap().heartbeats = vec![
            heartbeat(
                "HB1",
                "nightly",
                true,
                "Run the tests.\nThen push to main.",
                "claude --dangerously-skip-permissions",
            ),
            heartbeat("HB2", "deps", false, "Update the deps.", "codex"),
        ];
        let enables = |h: &Harness| {
            h.fake
                .calls()
                .into_iter()
                .filter(|c| c.starts_with("heartbeat_"))
                .collect::<Vec<_>>()
        };

        // B: the drawer on Heartbeats, by name: the one its agent is
        // setting up among them, marked so.
        h.press(KeyCode::Char('B')).await;
        assert!(h.app.beats_has_keys());
        let screen = render(&mut h.app, 160, 48);
        assert!(screen.contains("♥  Heartbeats"), "{screen}");
        assert!(screen.contains("◌ nightly"), "{screen}");
        assert!(screen.contains("setting up"), "{screen}");
        assert!(!screen.contains("approve"), "{screen}");
        assert!(screen.contains("● deps"), "{screen}");
        h.press(KeyCode::Char('j')).await;
        assert_eq!(
            h.app.beats.selected().map(|b| b.name.as_str()),
            Some("nightly")
        );

        // e from the list shows it whole: its prompt and command, nothing
        // enabled yet; e there enables it.
        h.press(KeyCode::Char('e')).await;
        assert!(enables(&h).is_empty());
        let screen = render(&mut h.app, 160, 48);
        assert!(screen.contains("Then push to main."), "{screen}");
        // In full, wrapped in the column.
        assert!(
            screen.contains("--dangerously-skip-permissions"),
            "{screen}"
        );
        assert!(screen.contains("Its agent is setting it up"), "{screen}");
        assert!(screen.contains("e enable"), "{screen}");
        h.press(KeyCode::Char('e')).await;
        assert_eq!(enables(&h), ["heartbeat_enable HB1 true"]);
        assert!(h
            .app
            .status()
            .is_some_and(|m| m.contains("nightly runs on its schedule")));

        // r runs it now; its run is among its runs, and listed with ♥.
        h.press(KeyCode::Char('r')).await;
        let run = h
            .fake
            .state
            .lock()
            .unwrap()
            .sessions
            .iter()
            .find(|s| s.heartbeat.is_some())
            .cloned()
            .unwrap();
        h.event(
            run.id.as_str(),
            session_core::api::SessionEvent::Created {
                session: run.clone(),
            },
        );
        h.press(KeyCode::Char('B')).await;
        let screen = render(&mut h.app, 160, 48);
        assert!(screen.contains("♥ nightly #1"), "{screen}");
        h.press(KeyCode::Char('B')).await;
        assert_eq!(h.app.beats.runs().map(<[_]>::len), Some(1));
        // The session pane shows the run.
        assert_eq!(h.app.detail.view.session_id(), Some(&run.id));

        // Esc back to the list; d twice deletes the selected one.
        h.press(KeyCode::Esc).await;
        assert!(h.app.beats.reading.is_none());
        h.press(KeyCode::Char('k')).await;
        assert_eq!(
            h.app.beats.selected().map(|b| b.name.as_str()),
            Some("deps")
        );
        h.press(KeyCode::Char('d')).await;
        assert!(!enables(&h)
            .iter()
            .any(|c| c.starts_with("heartbeat_delete")));
        h.press(KeyCode::Char('d')).await;
        assert!(enables(&h).contains(&"heartbeat_delete HB2".to_string()));

        // n: the form; Ask agent makes an empty heartbeat for its agent
        // (on once the agent saves its prompt and schedule), and a session
        // running its command whose agent is asked (typed, not sent) to fill
        // it in.
        h.press(KeyCode::Char('n')).await;
        assert!(matches!(h.app.mode, AppMode::NewHeartbeat(_)));
        assert!(h.fake.creates().is_empty());
        h.press(KeyCode::Up).await;
        h.press(KeyCode::Enter).await;
        assert!(matches!(h.app.mode, AppMode::Normal));
        assert!(enables(&h).contains(&"heartbeat_create Heartbeat".to_string()));
        let made = h
            .fake
            .state
            .lock()
            .unwrap()
            .heartbeats
            .last()
            .cloned()
            .unwrap();
        assert!(!made.enabled && made.awaiting_approval, "{made:?}");
        let setup = h.fake.creates().last().cloned().unwrap();
        assert_eq!(
            setup.startup_cmds,
            ["claude --dangerously-skip-permissions"]
        );
        let first = setup.first_input.unwrap();
        assert!(!first.submit);
        assert!(
            first.text.contains("heartbeat update hb_HB"),
            "{}",
            first.text
        );
        assert_eq!(h.app.focus, Focus::Session);
    }

    /// A heartbeat read whole lists every run the daemon recorded, newest
    /// first, those that started no session too; the session pane shows
    /// the selected one: live, why there's none, or a deleted run's saved
    /// output, and Enter brings that one back and goes into it.
    #[tokio::test]
    async fn every_run_of_a_heartbeat_is_listed_and_the_selected_one_shown() {
        use crate::app::DetailView;
        use crate::components::format::now_secs;
        use session_core::heartbeat::{HeartbeatRun, HeartbeatRunRecord, RunOutcome, RunPlace};
        use session_core::ids::SessionId;
        let mut live = view("R2", "nightly #2", 30);
        live.lifecycle = Lifecycle::Working;
        let mut h = harness(vec![view("A", "fix-login", 20), live.clone()]);
        let id = session_core::ids::HeartbeatId::from_string("HB1");
        let record =
            |seq: u64, run: u32, session: Option<&str>, outcome, detail: Option<&str>, place| {
                (
                    id.clone(),
                    HeartbeatRunRecord {
                        seq,
                        run: HeartbeatRun {
                            run,
                            at: now_secs() - 600 + seq as i64,
                            session_id: session.map(SessionId::from_string),
                            outcome,
                            detail: detail.map(str::to_owned),
                        },
                        place,
                        status: None,
                        lifecycle: None,
                    },
                )
            };
        {
            let mut state = h.fake.state.lock().unwrap();
            state.heartbeats = vec![heartbeat(
                "HB1",
                "nightly",
                false,
                "Run the tests.",
                "claude",
            )];
            state.run_records = vec![
                record(
                    1,
                    1,
                    Some("D1"),
                    RunOutcome::TaskGiven,
                    None,
                    Some(RunPlace::History),
                ),
                record(
                    2,
                    0,
                    None,
                    RunOutcome::Skipped,
                    Some("run #1 is still at work"),
                    None,
                ),
                record(
                    3,
                    2,
                    Some("R2"),
                    RunOutcome::TaskGiven,
                    None,
                    Some(RunPlace::Listed),
                ),
            ];
            state.deleted = vec![view("D1", "nightly #1", 10)];
        }
        h.fake.set_read("D1", "what run one wrote");
        h.press(KeyCode::Char('B')).await;
        h.press(KeyCode::Enter).await;
        let screen = render(&mut h.app, 160, 48);
        assert!(screen.contains("#2 working"), "{screen}");
        assert!(
            screen.contains("skipped: run #1 is still at work"),
            "{screen}"
        );
        assert!(screen.contains("#1 deleted"), "{screen}");
        // The newest is shown, live.
        assert_eq!(h.app.detail.view.session_id(), Some(&live.id));
        assert_eq!(h.last_attach().1.id, live.id);

        // A due time that was skipped started nothing: the pane says why.
        h.press(KeyCode::Down).await;
        assert!(
            matches!(&h.app.detail.view, DetailView::Note(note) if note.contains("Skipped: run #1")),
            "{:?}",
            h.app.detail.view
        );
        // A run in the session history: its saved output.
        h.press(KeyCode::Down).await;
        assert!(h
            .fake
            .calls()
            .iter()
            .any(|c| c.starts_with("history_read D1")));
        let screen = render(&mut h.app, 160, 48);
        assert!(screen.contains("what run one wrote"), "{screen}");

        // A new run doesn't move the selection.
        h.fake.state.lock().unwrap().run_records.push(record(
            4,
            3,
            None,
            RunOutcome::Failed,
            Some("the folder is gone"),
            None,
        ));
        h.app.reread_heartbeats();
        h.app.process_pending();
        assert_eq!(h.app.beats.selected_run().map(|r| r.seq), Some(1));
        let screen = render(&mut h.app, 160, 48);
        assert!(screen.contains("failed: the folder is gone"), "{screen}");

        // Enter brings it back from the history and goes into it.
        h.press(KeyCode::Enter).await;
        assert!(h.fake.calls().contains(&"restore D1".to_string()));
        assert_eq!(h.selected().as_deref(), Some("D1"));
        // Back in Rakumux mode the run is in view again.
        h.rakumux().await;
        assert_eq!(
            h.app.detail.view.session_id(),
            Some(&SessionId::from_string("D1"))
        );
        // Leaving the heartbeat shows the list's selection.
        h.press(KeyCode::Esc).await;
        h.press(KeyCode::Char('B')).await;
        assert!(!h.app.beats_shown());
    }

    /// Click the first place `text` is drawn at, on a `width` × `height`
    /// screen.
    fn click_text(h: &mut Harness, width: u16, height: u16, text: &str) {
        let screen = render(&mut h.app, width, height);
        let (row, column) = screen
            .lines()
            .enumerate()
            .find_map(|(y, line)| {
                line.find(text)
                    .map(|at| (y as u16, crate::layout::str_width(&line[..at]) as u16))
            })
            .unwrap_or_else(|| panic!("{text:?} isn't on screen:\n{screen}"));
        for kind in [
            MouseEventKind::Down(MouseButton::Left),
            MouseEventKind::Up(MouseButton::Left),
        ] {
            h.app.handle_mouse(MouseEvent {
                kind,
                column,
                row,
                modifiers: KeyModifiers::NONE,
            });
        }
        h.app.process_pending();
    }

    #[tokio::test]
    async fn a_new_heartbeat_is_made_in_the_form_then_renamed_and_deleted() {
        let mut h = harness(vec![view("A", "fix-login", 20)]);
        h.press(KeyCode::Char('B')).await;
        // None yet: the one row, selected, makes one (Enter).
        let screen = render(&mut h.app, 160, 48);
        assert!(screen.contains("+ New heartbeat"), "{screen}");
        h.press(KeyCode::Enter).await;
        let screen = render(&mut h.app, 160, 48);
        for text in [
            "New heartbeat",
            "Name",
            "Every",
            "Daily",
            "‹ claude ›",
            "Arguments",
            "--dangerously-skip-permissions",
            "Prompt",
            "$ claude --dangerously-skip-permissions",
            "Create",
            "Ask agent",
        ] {
            assert!(screen.contains(text), "{text}: {screen}");
        }
        // Named, daily at 18:30.
        h.type_text("nightly").await;
        h.press(KeyCode::Enter).await;
        h.press(KeyCode::Right).await;
        h.press(KeyCode::Tab).await;
        h.ctrl('u').await;
        h.type_text("6pm").await;
        h.press(KeyCode::Tab).await;
        // Nothing is listed off the agent; on it, what that agent takes, and
        // what the next one does. Tab goes on to its arguments.
        assert!(!render(&mut h.app, 160, 48).contains("--permission-mode"));
        h.press(KeyCode::Tab).await;
        let screen = render(&mut h.app, 160, 48);
        assert!(screen.contains("--permission-mode <mode>"), "{screen}");
        h.press(KeyCode::Right).await;
        let screen = render(&mut h.app, 160, 48);
        assert!(screen.contains("--sandbox <mode>"), "{screen}");
        assert!(!screen.contains("--permission-mode"), "{screen}");
        assert!(screen.contains("more"), "{screen}");
        h.press(KeyCode::Tab).await;
        // Its own, at first; typing finds one, Enter takes it, then its value.
        h.type_text(" sand").await;
        let screen = render(&mut h.app, 160, 48);
        assert!(!screen.contains("--search"), "{screen}");
        h.press(KeyCode::Enter).await;
        h.type_text("read").await;
        h.press(KeyCode::Tab).await;
        // Esc leaves a suggestion as it is, then Enter goes on.
        h.type_text("--sear").await;
        h.press(KeyCode::Esc).await;
        assert!(matches!(h.app.mode, AppMode::NewHeartbeat(_)));
        h.press(KeyCode::Backspace).await;
        // A click takes one too.
        click_text(&mut h, 160, 48, "Search the web");
        h.press(KeyCode::Enter).await;
        h.type_text("Run the tests.").await;
        h.ctrl('j').await;
        h.type_text("Fix what fails.").await;
        let screen = render(&mut h.app, 160, 48);
        assert!(!screen.contains("Never ask, no sandbox"), "{screen}");
        assert!(
            screen.contains("$ codex --dangerously-bypass-approvals-and-sandbox --sandbox"),
            "{screen}"
        );
        h.press(KeyCode::Tab).await;
        // A time it can't read: said, and the keys are on it.
        h.press(KeyCode::Enter).await;
        let screen = render(&mut h.app, 160, 48);
        assert!(screen.contains("Write the time as HH:MM"), "{screen}");
        assert!(screen.contains("‹ codex ›"), "{screen}");
        assert!(h.fake.state.lock().unwrap().heartbeats.is_empty());
        h.ctrl('u').await;
        h.type_text("18:30").await;
        // A failure keeps what was written.
        h.fake.fail_next(
            "heartbeat_create",
            ApiError::new(
                ErrorCode::InvalidArgument,
                "a heartbeat named nightly exists",
            ),
        );
        click_text(&mut h, 160, 48, " Create ");
        let screen = render(&mut h.app, 160, 48);
        assert!(
            screen.contains("A heartbeat named nightly exists"),
            "{screen}"
        );
        click_text(&mut h, 160, 48, " Create ");
        assert!(matches!(h.app.mode, AppMode::Normal));
        let made = h.fake.state.lock().unwrap().heartbeats[0].clone();
        assert_eq!(made.name, "nightly");
        assert_eq!(made.prompt, "Run the tests.\nFix what fails.");
        assert_eq!(made.schedule.as_ref().unwrap().to_string(), "daily 18:30");
        assert_eq!(made.agent.as_deref(), Some("codex"));
        assert_eq!(
            made.command,
            "codex --dangerously-bypass-approvals-and-sandbox --sandbox read-only --search"
        );
        assert_eq!(made.scope, scope());
        assert!(made.enabled);
        // Heartbeats shows it whole, with its buttons.
        let screen = render(&mut h.app, 160, 48);
        assert!(screen.contains("● nightly"), "{screen}");
        assert!(screen.contains("Fix what fails."), "{screen}");
        assert!(
            screen.contains("r run  p pause  c edit  d delete"),
            "{screen}"
        );

        // c edits it: the form as it is, its folder fixed. An empty name
        // isn't one; Ctrl-S saves from any field, only what changed.
        h.press(KeyCode::Char('c')).await;
        let screen = render(&mut h.app, 160, 48);
        for text in [
            "Edit heartbeat",
            "18:30",
            "‹ codex ›",
            "--sandbox read-only --search",
            "Fix what fails.",
            " Save ",
        ] {
            assert!(screen.contains(text), "{text}: {screen}");
        }
        for text in ["Ask agent", "‹ repo ›", " Create "] {
            assert!(!screen.contains(text), "{text}: {screen}");
        }
        h.ctrl('u').await;
        h.ctrl('s').await;
        let screen = render(&mut h.app, 160, 48);
        assert!(screen.contains("Write its name"), "{screen}");
        h.type_text("evening").await;
        h.ctrl('s').await;
        assert!(matches!(h.app.mode, AppMode::Normal));
        let saved = h.fake.state.lock().unwrap().heartbeats[0].clone();
        assert_eq!(
            (&saved.command, &saved.prompt, &saved.schedule),
            (&made.command, &made.prompt, &made.schedule)
        );
        // Its schedule, agent and prompt, by the keys; Save.
        h.press(KeyCode::Char('c')).await;
        h.press(KeyCode::Tab).await;
        h.press(KeyCode::Left).await;
        h.press(KeyCode::Tab).await;
        h.ctrl('u').await;
        h.type_text("2h").await;
        h.press(KeyCode::Tab).await;
        h.press(KeyCode::Left).await;
        h.press(KeyCode::Tab).await;
        h.press(KeyCode::Tab).await;
        h.ctrl('u').await;
        h.type_text("Only this: ").await;
        // A refusal keeps the form.
        h.fake.fail_next(
            "heartbeat_update",
            ApiError::new(ErrorCode::InvalidArgument, "no such luck"),
        );
        click_text(&mut h, 160, 48, " Save ");
        let screen = render(&mut h.app, 160, 48);
        assert!(screen.contains("No such luck"), "{screen}");
        click_text(&mut h, 160, 48, " Save ");
        assert!(matches!(h.app.mode, AppMode::Normal));
        let saved = h.fake.state.lock().unwrap().heartbeats[0].clone();
        assert_eq!(saved.name, "evening");
        assert_eq!(saved.schedule.unwrap().to_string(), "every 2h");
        assert_eq!(saved.agent.as_deref(), Some("claude"));
        assert_eq!(saved.command, "claude --dangerously-skip-permissions");
        assert!(saved.prompt.starts_with("Only this:"), "{}", saved.prompt);
        // Nothing changed: nothing asked of the daemon.
        let asked = h.fake.calls().len();
        h.press(KeyCode::Char('c')).await;
        h.ctrl('s').await;
        assert!(matches!(h.app.mode, AppMode::Normal));
        assert!(!h.fake.calls()[asked..]
            .iter()
            .any(|call| call.starts_with("heartbeat_update")));
        let screen = render(&mut h.app, 160, 48);
        assert!(screen.contains("● evening"), "{screen}");
        assert!(h
            .fake
            .calls()
            .contains(&format!("heartbeat_update {} evening", made.id)));

        // In the list: its buttons under it, the new row after it. Down and
        // Enter open the form; Esc leaves it.
        h.press(KeyCode::Esc).await;
        let screen = render(&mut h.app, 160, 48);
        assert!(
            screen.contains("r run  p pause  c edit  d delete"),
            "{screen}"
        );
        h.press(KeyCode::Char('j')).await;
        let screen = render(&mut h.app, 160, 48);
        assert!(!screen.contains("c edit"), "{screen}");
        // Nothing to edit or delete on that row.
        h.press(KeyCode::Char('c')).await;
        h.press(KeyCode::Char('d')).await;
        assert!(matches!(h.app.mode, AppMode::Normal));
        h.press(KeyCode::Enter).await;
        assert!(matches!(h.app.mode, AppMode::NewHeartbeat(_)));
        h.press(KeyCode::Esc).await;
        assert!(matches!(h.app.mode, AppMode::Normal));

        // Clicks: the row selects it, a click edits, two delete it.
        click_text(&mut h, 160, 48, "● evening");
        click_text(&mut h, 160, 48, "c edit");
        assert!(matches!(h.app.mode, AppMode::NewHeartbeat(_)));
        h.press(KeyCode::Esc).await;
        click_text(&mut h, 160, 48, "d delete");
        assert_eq!(h.fake.state.lock().unwrap().heartbeats.len(), 1);
        click_text(&mut h, 160, 48, "d again deletes");
        assert!(h.fake.state.lock().unwrap().heartbeats.is_empty());
        let screen = render(&mut h.app, 160, 48);
        assert!(!screen.contains("● evening"), "{screen}");
        click_text(&mut h, 160, 48, "+ New heartbeat");
        assert!(matches!(h.app.mode, AppMode::NewHeartbeat(_)));
    }

    #[tokio::test]
    async fn the_heartbeat_form_fits_phones_and_takes_taps() {
        for (width, height) in PHONES {
            let mut h = harness(vec![view("A", "fix-login", 20)]);
            render(&mut h.app, width, height);
            h.press(KeyCode::Char('B')).await;
            click_text(&mut h, width, height, "+ New heartbeat");
            // The arguments listed leave the labels: a tap takes one, or goes on.
            click_text(&mut h, width, height, "Arguments");
            let screen = render(&mut h.app, width, height);
            assert!(screen.contains("--model <model>"), "{screen}");
            click_text(&mut h, width, height, "--model <");
            click_text(&mut h, width, height, "opus");
            click_text(&mut h, width, height, "Prompt");
            h.app
                .handle_paste("Update the dependencies, run the tests and fix what fails.\nThen say what changed.");
            click_text(&mut h, width, height, "Days");
            click_text(&mut h, width, height, "fri");
            let screen = render(&mut h.app, width, height);
            for text in [
                "Name",
                "Folder",
                "Agent",
                "Create",
                "Ask agent",
                "Esc cancel",
            ] {
                assert!(
                    screen.contains(text),
                    "{text} at {width}x{height}: {screen}"
                );
            }
            click_text(&mut h, width, height, " Create ");
            let made = h.fake.state.lock().unwrap().heartbeats[0].clone();
            assert!(made.prompt.ends_with("\nThen say what changed."));
            assert_eq!(
                made.command,
                "claude --dangerously-skip-permissions --model opus"
            );
            assert_eq!(
                made.schedule.as_ref().unwrap().to_string(),
                "mon,tue,wed,thu 09:00"
            );
            let screen = render(&mut h.app, width, height);
            assert!(screen.contains("● Heartbeat"), "{screen}");
            assert!(screen.contains("c edit"), "{screen}");
        }
    }
}
