//! Frame composition: session list or tab bar, detail pane, the footer
//! (with the mode badge) on the last row, then the open dialog or the
//! Settings modal on top.

use ratatui::{
    layout::Rect,
    style::Style,
    text::Line,
    widgets::{Block, Paragraph},
    Frame,
};

use crate::app::types::{AppMode, Focus, SettingsItem};
use crate::app::App;
use crate::components::{
    auth::draw_auth_menu,
    dialogs::{
        draw_ai_provider_dialog, draw_api_key_dialog, draw_auth_code_dialog,
        draw_auth_email_dialog, draw_confirm_dialog, draw_daemon_restart_dialog,
        draw_openai_model_dialog, draw_rename_dialog, draw_upgrade_dialog,
    },
    folder_picker::draw_folder_picker,
    footer::draw_footer,
    session_detail::draw_session_detail,
    session_grid::draw_session_grid,
    session_list::draw_session_list,
    session_tabs::draw_session_tabs,
};
use crate::help::{ModalState, ModalTab};
use crate::theme;

/// The line between the list and the detail: a column side by side, a row
/// when the list is stacked above the detail. Brighter and heavier while
/// the mouse is on it or dragging it.
fn draw_divider(f: &mut Frame, app: &App, area: Rect, stacked: bool) {
    let p = theme::palette();
    let active = app.divider.hover || app.divider.dragging;
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

/// The Settings modal on its General tab, under the dialog that changes
/// one of its settings (that row selected).
fn draw_settings_under(f: &mut Frame, app: &App, area: Rect) {
    let row = app.mode.editing_setting().map_or(0, SettingsItem::index);
    let state = ModalState::new(ModalTab::General, SettingsItem::ALL.len()).with_selected(row);
    let ctx = app.modal_context(app.focus == Focus::Session);
    crate::help::render(f, area, &state, &ctx);
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
    let screen = app.screen();
    let mut hits = Vec::new();
    let mut tab_hits = Vec::new();

    if let Some(grid) = screen.grid {
        draw_session_grid(f, app, grid, &mut hits);
    }

    if let Some(list) = screen.list {
        draw_session_list(f, app, list, &mut hits);
    }
    if let Some(divider) = screen.divider {
        draw_divider(f, app, divider, screen.stacked);
    }
    if let Some(tabs) = screen.tabs {
        tab_hits = draw_session_tabs(f, app, tabs, &mut hits);
    }
    let cursor = draw_session_detail(f, app, &screen);
    if let Some(footer) = screen.footer {
        draw_footer(f, app, footer, &mut hits);
    }
    if let (Some((x, y)), AppMode::Normal) = (cursor, &app.mode) {
        f.set_cursor(x, y);
    }

    // The Folders modal's own click targets replace the panes'.
    let mut menu_hits = Vec::new();
    match &app.mode {
        AppMode::Normal => {}
        AppMode::Folders(picker) => draw_folder_picker(f, app, picker, area, &mut menu_hits),
        AppMode::Renaming(dialog) => draw_rename_dialog(f, dialog, area),
        AppMode::Confirm(dialog) => draw_confirm_dialog(f, dialog, area),
        AppMode::DaemonRestart(dialog) => draw_daemon_restart_dialog(f, dialog, area),
        AppMode::Modal { state, in_session } => {
            crate::help::render(f, area, state, &app.modal_context(*in_session))
        }
        AppMode::ConfiguringApiKey(dialog) => {
            draw_settings_under(f, app, area);
            draw_api_key_dialog(f, dialog, area)
        }
        AppMode::AiProvider(dialog) => {
            draw_settings_under(f, app, area);
            draw_ai_provider_dialog(f, dialog, area)
        }
        AppMode::OpenAiModel(dialog) => {
            draw_settings_under(f, app, area);
            draw_openai_model_dialog(f, dialog, area)
        }
        AppMode::Upgrade(dialog) => draw_upgrade_dialog(f, dialog, area),
        AppMode::Welcome(state) => crate::components::welcome::draw_welcome(f, app, *state, area),
        AppMode::PhoneOffer => crate::components::phone_access::draw_phone_offer(f, area),
        AppMode::Pairing(dialog) => {
            if dialog.from_settings {
                draw_settings_under(f, app, area);
            }
            crate::components::phone_access::draw_pairing(f, dialog, area)
        }
        AppMode::Auth(menu) => draw_auth_menu(f, menu, area),
        AppMode::AuthEmail(dialog) => draw_auth_email_dialog(f, dialog, area),
        AppMode::AuthCode(dialog) => draw_auth_code_dialog(f, dialog, area),
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::testing::{
        aged, folder, harness, harness_with_folders, on_branch, options, scope, start, view,
        view_in, FakeApi, Harness,
    };
    use crate::app::types::Focus;
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
        // The folder filter heads the list.
        assert!(wide.contains("\n   ▤ All folders "), "{wide}");
        assert!(wide.contains("RAKUMUX"), "{wide}");
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

    #[tokio::test]
    async fn live_session_observed_then_controlled() {
        let mut h = harness(vec![view("A", "api-refactor", 10)]);
        let wide = render(&mut h.app, 160, 48);
        assert!(wide.contains("api-refactor"), "{wide}");
        assert!(wide.contains("hello from the fake"), "{wide}");
        assert!(wide.contains("observing"), "{wide}");
        // Narrow: the session is previewed under the list.
        let narrow = render(&mut h.app, 80, 24);
        assert!(narrow.contains("api-refactor"), "{narrow}");
        assert!(narrow.contains("hello from the fake"), "{narrow}");
        assert!(narrow.contains("observing"), "{narrow}");
        // Too short for both: the list alone.
        let short = render(&mut h.app, 80, 14);
        assert!(short.contains("api-refactor"), "{short}");
        assert!(!short.contains("hello from the fake"), "{short}");

        h.press(KeyCode::Enter).await;
        assert_eq!(h.app.focus, Focus::Session);
        for (width, height) in SIZES {
            let screen = render(&mut h.app, width, height);
            assert!(screen.contains("hello from the fake"), "{screen}");
            assert!(screen.contains("controlling"), "{screen}");
            let footer = screen.lines().last().unwrap();
            assert!(footer.starts_with(" SESSION "), "{footer:?}");
            assert!(footer.contains(" Ctrl-\\ rakumux "), "{footer:?}");
            assert!(footer.trim_end().ends_with("settings"), "{footer:?}");
        }
    }

    #[tokio::test]
    async fn the_footer_badge_uses_the_rakumux_name_in_both_layouts() {
        let badge = |h: &mut Harness, width: u16, height: u16| {
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal.draw(|f| draw(f, &mut h.app)).unwrap();
            let buffer = terminal.backend().buffer();
            let row = height - 1;
            let text: String = (0..12)
                .map(|x| buffer.get(x, row).symbol().to_string())
                .collect();
            (text, buffer.get(1, row).fg, buffer.get(1, row).bg)
        };
        let p = theme::palette();
        for layout in [SessionLayout::Sidebar, SessionLayout::Tabs] {
            let mut h = with_layout(vec![view("A", "api", 10)], layout);
            for (width, height) in [(160, 48), (120, 30), (90, 24), (60, 20), (40, 20)] {
                // Rakumux mode: bright orange, bottom left.
                if h.app.focus == Focus::Session {
                    h.rakumux().await;
                }
                assert_eq!(h.app.focus, Focus::List);
                let (text, fg, bg) = badge(&mut h, width, height);
                assert!(text.starts_with(" RAKUMUX "), "{text:?}");
                assert_eq!((fg, bg), (p.background, p.accent_orange));
                // Dialogs keep the same mode label, even when a small screen
                // lets the modal cover the footer.
                h.press(KeyCode::Char('?')).await;
                let mode = crate::components::footer::mode_badge(&h.app);
                assert_eq!(mode.content, " RAKUMUX ");
                assert_eq!(mode.style.fg, Some(p.background));
                assert_eq!(mode.style.bg, Some(p.accent_orange));
                h.press(KeyCode::Esc).await;
                // A session: a quiet badge that blends in.
                h.press(KeyCode::Enter).await;
                assert_eq!(h.app.focus, Focus::Session);
                let (text, fg, bg) = badge(&mut h, width, height);
                assert!(text.starts_with(" SESSION "), "{text:?}");
                assert_eq!((fg, bg), (p.neutral_gray, p.selection));
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
        // Wide: the detail's title is the first row.
        let wide = render(&mut h.app, 160, 48);
        assert!(wide.lines().next().unwrap().contains("api"), "{wide}");
    }

    #[tokio::test]
    async fn the_footer_is_one_row_of_messages() {
        let mut h = harness(vec![view("A", "api", 10)]);
        for (width, height) in SIZES {
            let screen = render(&mut h.app, width, height);
            let rows: Vec<&str> = screen.lines().collect();
            assert_eq!(rows.len(), usize::from(height));
            let footer = rows[rows.len() - 1];
            assert!(footer.starts_with(" RAKUMUX "), "{footer:?}");
            assert!(
                footer.trim_end().ends_with("? help  , settings"),
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
        assert!(footer.starts_with(" RAKUMUX  Layout: Tabs "), "{footer:?}");
        assert!(
            footer.trim_end().ends_with("? help  , settings"),
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
                assert!(footer.starts_with(" SESSION "), "{footer:?}");
            }
            h.rakumux().await;
            render(&mut h.app, width, height);
            assert_eq!(still(&h), before, "{layout:?}: rakumux mode");
        }
    }

    #[tokio::test]
    async fn settings_has_the_general_tab_and_a_table_of_actions() {
        let mut h = harness(vec![view("A", "api", 10)]);
        h.press(KeyCode::Char('o')).await;
        for (width, height) in SIZES {
            let screen = render(&mut h.app, width, height);
            assert!(screen.contains("Settings"), "{screen}");
            assert!(screen.contains("1 General"), "{screen}");
            assert!(screen.contains("▶ Layout"), "{screen}");
            assert!(screen.contains("[Master detail] Grid Tabs"), "{screen}");
            assert!(screen.contains("Anthropic API key"), "{screen}");
            assert!(screen.contains("not set"), "{screen}");
            // The note follows all settings; a 24-row screen scrolls it
            // below the visible rows once the App section is present.
            if height >= 40 {
                assert!(
                    screen.contains(SessionLayout::Sidebar.description()),
                    "{screen}"
                );
            }
        }
        // Keys tab: every action with its keys.
        h.press(KeyCode::Char('2')).await;
        let screen = render(&mut h.app, 160, 48);
        for hint in [
            "Action",
            "Key",
            "▶ Rakumux mode (from a session)",
            "Esc, Ctrl-\\",
            "↓, j, Alt-↓",
            "gg, Home",
            "Modes",
            "Moving around",
            "Pin or unpin",
            "Enter run",
        ] {
            assert!(screen.contains(hint), "{hint}: {screen}");
        }
        // A General dialog opens over the modal.
        h.press(KeyCode::Char('1')).await;
        for _ in 0..SettingsItem::AnthropicKey.index() {
            h.press(KeyCode::Down).await;
        }
        h.press(KeyCode::Enter).await;
        let screen = render(&mut h.app, 160, 48);
        assert!(screen.contains("1 General"), "{screen}");
        assert!(screen.contains("API key"), "{screen}");
        assert!(screen.contains("console.anthropic.com"), "{screen}");
    }

    #[tokio::test]
    async fn live_preview_title_is_faded_until_session_mode_restores_its_emphasis() {
        let mut h = harness(vec![view("A", "color-check", 10)]);
        let mut terminal = Terminal::new(TestBackend::new(160, 48)).unwrap();
        terminal.draw(|f| draw(f, &mut h.app)).unwrap();
        let title = h.app.screen().detail_title.unwrap();
        let name = terminal.backend().buffer().get(title.x + 3, title.y);
        assert_eq!(name.symbol(), "c");
        let p = theme::palette();
        let amount = crate::components::utils::PREVIEW_FADE;
        match p.faded(p.text_primary, amount) {
            Some(fg) => {
                assert_eq!(name.fg, fg);
                assert!(!name.modifier.contains(ratatui::style::Modifier::DIM));
            }
            None => assert!(name.modifier.contains(ratatui::style::Modifier::DIM)),
        }
        assert_eq!(name.bg, p.faded(p.panel, amount).unwrap_or(p.panel));
        assert!(name.modifier.contains(ratatui::style::Modifier::BOLD));
        h.press(KeyCode::Enter).await;
        terminal.draw(|f| draw(f, &mut h.app)).unwrap();
        let title = h.app.screen().detail_title.unwrap();
        let active_name = terminal.backend().buffer().get(title.x + 3, title.y);
        assert_eq!(active_name.fg, p.text_bright);
        assert!(active_name
            .modifier
            .contains(ratatui::style::Modifier::BOLD));
    }

    #[test]
    fn update_states_are_readable_and_explain_ui_only_restart_at_phone_and_desktop_sizes() {
        use crate::app::types::UpgradeDialog;
        for state in ["checking", "available", "installing", "error", "current"] {
            for (width, height) in [(40, 20), (90, 20), (160, 48)] {
                let mut h = harness(vec![view("A", "api", 10)]);
                h.app.mode = AppMode::Upgrade(UpgradeDialog {
                    current_version: "0.1.4".into(), latest_version: Some("0.1.5".into()),
                    install_method: Some(crate::updater::InstallMethod::Binary),
                    checking: state == "checking", installing: state == "installing",
                    update_available: matches!(state, "available" | "installing"),
                    error_message: (state == "error").then(|| "Cannot write the installation. Use the installer outside Rakumux with your original PREFIX; administrator permission may be required.".into()),
                    info_message: (state == "current").then(|| "You're already on the latest version.".into()), selection: 0,
                });
                let text = render(&mut h.app, width, height);
                assert!(text.contains("Update Rakumux"), "{state}: {text}");
                assert!(
                    text.contains("daemon") && text.contains("sessions"),
                    "{state}: {text}"
                );
                if state == "error" {
                    assert!(
                        text.contains("administrator") && text.contains("PREFIX"),
                        "{text}"
                    );
                }
                if state == "available" {
                    assert!(text.contains("Install update and restart"), "{text}");
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
        let (index, _) = h.last_attach();
        h.attach_event(
            index,
            AttachEvent::Event(StreamEvent::Exited { code: Some(3) }),
        );
        for (width, height) in SIZES {
            let screen = render(&mut h.app, width, height);
            assert!(screen.contains("✕ build"), "{screen}");
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
            // The folder filter sits at the left end of the tab bar.
            let bar = screen.lines().next().unwrap();
            assert!(bar.starts_with(" ▤ All folders ▾   1 ● docs "), "{bar:?}");
            assert!(!h.app.tab_hits.is_empty());
        }
        let mut empty = with_layout(vec![], SessionLayout::Tabs);
        let screen = render(&mut empty.app, 80, 24);
        assert!(screen.contains("No sessions yet"), "{screen}");
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
            assert!(!screen.contains("▤ All folders"), "{screen}");
            assert!(!screen.contains("RAKUMUX"), "{screen}");
            // The terminal starts at the top; the footer keeps the mode, the
            // zoomed session and the way out.
            assert!(screen.starts_with("hello from the fake"), "{screen}");
            let footer = screen.lines().last().unwrap();
            assert!(footer.starts_with(" SESSION  api · zoomed "), "{footer:?}");
            assert!(footer.contains("Ctrl-\\ rakumux"), "{footer:?}");
        }
        // Back in Rakumux mode the list returns, and the badge says so.
        h.rakumux().await;
        let screen = render(&mut h.app, 160, 48);
        assert!(screen.contains("▤ All folders"), "{screen}");
        let footer = screen.lines().last().unwrap();
        assert!(footer.starts_with(" RAKUMUX "), "{footer:?}");
    }

    #[tokio::test]
    async fn modal_dialogs_render() {
        let mut h = harness(vec![view("A", "api", 10)]);
        h.press(KeyCode::Char('o')).await;
        h.press(KeyCode::Char('2')).await; // Keys tab: shows the configured prefix
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
        assert!(screen.contains("[Master detail] Grid Tabs"), "{screen}");
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
            assert!(screen.contains("SESSION"), "{screen}");
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
        // The footer says so after the badge, in both widths.
        for screen in [wide, narrow] {
            let footer = screen.lines().last().unwrap();
            assert!(
                footer.starts_with(" RAKUMUX  ✕ daemon unavailable · can't start"),
                "{footer:?}"
            );
        }
    }

    #[test]
    fn connecting_shows_a_spinner_in_the_footer() {
        let mut h = harness(vec![]);
        h.app.connection = crate::app::Connection::Connecting;
        for (width, height) in SIZES {
            let screen = render(&mut h.app, width, height);
            let footer = screen.lines().last().unwrap();
            assert!(footer.starts_with(" RAKUMUX  "), "{footer:?}");
            assert!(footer.contains(" connecting "), "{footer:?}");
            assert!(footer.trim_end().ends_with("settings"), "{footer:?}");
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
        vec![refactor, docs, front, storybook, server]
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
    fn one_folder_shows_all_folders_branches_and_no_headers() {
        let sessions: Vec<SessionView> = three_folders()
            .into_iter()
            .filter(|s| s.scope == scope())
            .collect();
        let mut h = harness_with_folders(sessions, &known(), SessionLayout::Sidebar);
        let wide = render(&mut h.app, 160, 48);
        let list = list_column(&wide, 40);
        // The folder filter: a padded band above the sessions.
        assert!(list[0].is_empty() && list[2].is_empty(), "{wide}");
        // A folder icon and All folders, and ▾ on the right: it opens the
        // Folders modal.
        assert!(list[1].starts_with("   ▤ All folders"), "{wide}");
        assert!(list[1].ends_with('▾'), "{wide}");
        // Pinned first, with its mark; subtext is program, then git.
        let docs = list.iter().position(|l| l.contains("docs")).unwrap();
        let refactor = list
            .iter()
            .position(|l| l.contains("api-refactor"))
            .unwrap();
        assert!(docs < refactor, "{wide}");
        assert!(list[docs].contains("docs ★"), "{wide}");
        assert!(list[docs + 1].contains("⎇ main"), "{wide}");
        assert!(
            list[refactor + 1].contains("claude · ⎇ sessions-refactor"),
            "{wide}"
        );
        assert!(!wide.contains("worktree"), "branch only: {wide}");
        assert!(!wide.contains("──"), "no folder headers: {wide}");
        // No header above the list: the folder filter says what's shown.
        assert!(wide.lines().last().unwrap().starts_with(" RAKUMUX "));
        let narrow = render(&mut h.app, 80, 24);
        assert!(narrow.contains("▤ All folders"), "{narrow}");
        assert!(narrow.contains("docs ★"), "{narrow}");
    }

    #[test]
    fn the_folder_filter_says_what_is_shown() {
        for layout in [SessionLayout::Sidebar, SessionLayout::Tabs] {
            let mut h = harness_with_folders(three_folders(), &known(), layout);
            let first_lines = |h: &mut Harness| {
                let screen = render(&mut h.app, 160, 48);
                screen.lines().take(2).collect::<Vec<_>>().join("\n")
            };
            assert!(first_lines(&mut h).contains("▤ All folders"), "{layout:?}");
            // One folder: its name, and in the list its branch.
            h.app.show_folders(vec![folder("/code/web")]);
            let top = first_lines(&mut h);
            match layout {
                SessionLayout::Sidebar => {
                    assert!(top.contains("   ▤ web  ⎇ main"), "{top}")
                }
                SessionLayout::Tabs => assert!(top.starts_with(" ▤ web ▾ "), "{top}"),
                SessionLayout::Grid => assert!(top.contains("web"), "{top}"),
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
        assert!(wide.contains("   ▤ empty  ⎇ trunk"), "{wide}");
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
    fn all_folders_are_grouped_under_headers() {
        let mut h = harness_with_folders(three_folders(), &known(), SessionLayout::Sidebar);
        let wide = render(&mut h.app, 160, 48);
        let list = list_column(&wide, 40);
        assert!(list[1].starts_with("   ▤ All folders"), "{wide}");
        let at = |needle: &str| {
            list.iter()
                .position(|l| l.contains(needle))
                .unwrap_or_else(|| panic!("{needle} missing: {wide}"))
        };
        // The launch folder first, then by name (api, then web).
        let (repo, api, web) = (at(" repo  ⎇"), at(" api ──"), at(" web  ⎇ main"));
        assert!(repo < api && api < web, "{wide}");
        assert!(list[repo].ends_with("2/2 live"), "{wide}");
        assert!(list[web].ends_with("1/2 live"), "{wide}");
        // Inside a group: pinned first, then oldest first.
        assert!(at("docs ★") < at("api-refactor"), "{wide}");
        assert!(
            web < at("storybook") && at("storybook") < at("frontend"),
            "{wide}"
        );
        assert!(
            list[at("frontend") + 1].contains("codex · ⎇ feature/login"),
            "{wide}"
        );
        assert!(
            list[at("storybook") + 1].contains("⎇ main · dead"),
            "{wide}"
        );
        assert!(
            list[at("server") + 1].contains("cargo · ⎇ @1a2b3c4"),
            "{wide}"
        );
        // A selected header stands out: its rule turns blue.
        h.app
            .list
            .select(crate::app::Selection::FolderHeader(folder("/code/api")));
        let mut terminal = Terminal::new(TestBackend::new(160, 48)).unwrap();
        terminal.draw(|f| draw(f, &mut h.app)).unwrap();
        let buffer = terminal.backend().buffer();
        let row = (0..48)
            .find(|&y| {
                (0..40)
                    .map(|x| buffer.get(x, y).symbol())
                    .collect::<String>()
                    .contains(" api ──")
            })
            .unwrap();
        let rule = (0..40)
            .find(|&x| buffer.get(x, row).symbol() == "─")
            .unwrap();
        assert_eq!(buffer.get(rule, row).fg, theme::palette().accent_primary);
        // Narrow: still grouped; the stacked list scrolls to the selected
        // header, and the session in view stays below it.
        let narrow = render(&mut h.app, 80, 24);
        assert!(narrow.contains("▤ All folders"), "{narrow}");
        assert!(narrow.contains(" api ──"), "{narrow}");
        assert!(narrow.contains("frontend · waiting for you"), "{narrow}");
        assert!(narrow.contains("hello from the fake"), "{narrow}");
        h.app.list.select(crate::app::Selection::Folders);
        let narrow = render(&mut h.app, 80, 24);
        assert!(narrow.contains(" repo  ⎇ sessions-refactor"), "{narrow}");
    }

    #[tokio::test]
    async fn the_folders_modal_lists_all_folders_and_the_folders_with_sessions() {
        let mut h = harness_with_folders(three_folders(), &known(), SessionLayout::Sidebar);
        h.press(KeyCode::Char('P')).await;
        let wide = render(&mut h.app, 160, 48);
        let lines: Vec<&str> = wide.lines().collect();
        assert!(lines[1].starts_with("   ▤ All folders"), "{wide}");
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
            assert!(bar.starts_with(" ▤ All folders ▾  "), "{bar:?}");
        }
        // Wide enough for every tab: the launch folder's first, pinned
        // first, then the other folders' by name.
        let screen = render(&mut h.app, 160, 48);
        let bar = screen.lines().next().unwrap();
        assert!(bar.starts_with(" ▤ All folders ▾   1 ● docs ★ "), "{bar:?}");
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
        assert!(wide.contains("▤ All folders"), "{wide}");
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
            AiProviderDialog, ApiKeyDialog, ApiProvider, ConfirmAction, ConfirmDialog,
            DaemonRestartDialog, OpenAiModelDialog, RenameDialog, UpgradeDialog,
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
                message: "Delete 'api'? Its process is ended and its saved output removed.".into(),
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
            }),
            "api key" => AppMode::ConfiguringApiKey(ApiKeyDialog {
                provider: ApiProvider::Anthropic,
                input: TextInput::new(""),
                error_message: None,
            }),
            "provider" => AppMode::AiProvider(AiProviderDialog {
                selected: 0,
                current: "anthropic".into(),
            }),
            "model" => AppMode::OpenAiModel(OpenAiModelDialog {
                selected: 0,
                current: "gpt-4o-mini".into(),
                options: vec![
                    "gpt-4o-mini".into(),
                    "gpt-4.1-mini".into(),
                    "gpt-4.1".into(),
                ],
            }),
            other => panic!("no dialog {other}"),
        };
    }

    /// Every dialog fits a phone: its text and its keys show whole.
    #[tokio::test]
    async fn dialogs_fit_a_phone() {
        let mut h = harness(vec![view("A", "api", 10), view("B", "docs", 20)]);
        let expected: [(&str, &[&str]); 8] = [
            ("rename", &["Current: api", "[Enter] Rename • [Esc] Cancel"]),
            ("delete", &["output removed.", "[y/Enter] Yes • [n/Esc] No"]),
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
                    "Install update and restart",
                    "[Enter] Confirm",
                    "[Esc] Cancel",
                ],
            ),
            (
                "api key",
                &[
                    "'sk-ant-'",
                    "remove the key.",
                    "[Enter] Save • [Esc] Cancel",
                ],
            ),
            (
                "provider",
                &["Anthropic (current)", "[↑/↓] Choose", "[Esc] Cancel"],
            ),
            ("model", &["gpt-4.1-mini", "[Enter] Save", "[Esc] Cancel"]),
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
            let screen = render(&mut h.app, width, height);
            assert!(screen.contains("[Master detail] Grid Tabs"), "{screen}");
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
            for name in ["▤ All folders", "● api", "● docs", "+ New session"] {
                assert!(screen.contains(name), "{name}: {screen}");
            }
            // No session on screen.
            assert!(!screen.contains("hello from the fake"), "{screen}");
            assert!(!screen.contains("observing"), "{screen}");
            assert!(screen.lines().last().unwrap().starts_with(" RAKUMUX "));

            // j: only the selection moves.
            h.press(KeyCode::Char('j')).await;
            let screen = render(&mut h.app, width, height);
            assert!(!screen.contains("hello from the fake"), "{screen}");
            assert_eq!(h.app.focus, Focus::List);

            // Enter: tabs on top, the session below, no list.
            h.press(KeyCode::Enter).await;
            assert_eq!(h.app.focus, Focus::Session);
            let screen = render(&mut h.app, width, height);
            assert!(!screen.contains("▤ All folders"), "{screen}");
            let tabs = screen.lines().next().unwrap();
            assert!(tabs.contains("api") && tabs.contains("docs"), "{tabs:?}");
            assert!(screen
                .lines()
                .nth(1)
                .unwrap()
                .starts_with("hello from the fake"));
            assert!(screen.lines().last().unwrap().starts_with(" SESSION "));
            assert_eq!(h.app.terminal_size(), (width, height - 2));

            // The Rakumux key: back to the list, not the session.
            h.rakumux().await;
            assert_eq!(h.app.focus, Focus::List);
            let screen = render(&mut h.app, width, height);
            assert!(screen.contains("▤ All folders"), "{screen}");
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
            for i in 1..=6 {
                h.press(KeyCode::Char('j')).await;
                let screen = render(&mut h.app, width, height);
                let list = h.app.screen().list.unwrap();
                let top = rows(&screen, 0, list.bottom());
                let name = format!("● session-{i}");
                assert!(top.contains(&name), "{name} {width}x{height}: {screen}");
                assert!(top.contains("▤ All folders"), "{screen}");
            }
            // + New session: in view, and the last session stays below.
            h.press(KeyCode::Char('G')).await;
            let screen = render(&mut h.app, width, height);
            let layout = h.app.screen();
            let top = rows(&screen, 0, layout.list.unwrap().bottom());
            assert!(top.contains("+ New session"), "{screen}");
            let detail = layout.detail.unwrap();
            let below = rows(&screen, detail.y, detail.bottom());
            assert!(below.contains("session-6"), "{screen}");
            assert!(below.contains("hello from the fake"), "{screen}");
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
        // Nowhere to go back to yet: the RAKUMUX badge does nothing.
        tap(&mut h, Hit::ModeBadge);
        assert_eq!(h.app.focus, Focus::List);
        assert!(matches!(h.app.mode, AppMode::Normal));

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
        // Both the way out and Help fit 45 columns.
        assert!(
            footer.ends_with("Ctrl-\\ rakumux  Ctrl-\\ ? help "),
            "{footer:?}"
        );

        // The SESSION badge: out to Rakumux mode, remembering the session.
        tap(&mut h, Hit::ModeBadge);
        assert_eq!(h.app.focus, Focus::List);
        assert_eq!(
            h.app.came_from.as_ref().map(|id| id.to_string()),
            Some("B".into())
        );
        // The RAKUMUX badge: back into it.
        render(&mut h.app, width, height);
        tap(&mut h, Hit::ModeBadge);
        assert_eq!(h.app.focus, Focus::Session);
        assert_eq!(h.selected().as_deref(), Some("B"));

        // `Ctrl-\ rakumux` does what the badge does.
        render(&mut h.app, width, height);
        tap(&mut h, Hit::FooterKey(FooterKey::Rakumux));
        assert_eq!(h.app.focus, Focus::List);

        // `? help` and `, settings` open them, in either mode.
        render(&mut h.app, width, height);
        tap(&mut h, Hit::FooterKey(FooterKey::Help));
        assert_eq!(modal_tab(&h), Some(Modal::Help.first_tab()));
        h.press(KeyCode::Esc).await;
        render(&mut h.app, width, height);
        tap(&mut h, Hit::FooterKey(FooterKey::Settings));
        assert_eq!(modal_tab(&h), Some(ModalTab::General));
        h.press(KeyCode::Esc).await;
        h.press(KeyCode::Esc).await; // back into the session
        assert_eq!(h.app.focus, Focus::Session);
        render(&mut h.app, width, height);
        tap(&mut h, Hit::FooterKey(FooterKey::Help));
        assert_eq!(modal_tab(&h), Some(Modal::Help.first_tab()));
        // Like `Ctrl-\ ?`: Rakumux mode under Help.
        assert_eq!(h.app.focus, Focus::List);
        h.press(KeyCode::Esc).await;

        // While a dialog is open the footer isn't a target.
        h.press(KeyCode::Char('r')).await;
        render(&mut h.app, width, height);
        assert!(!h.app.hits.iter().any(|(_, hit)| *hit == Hit::ModeBadge));
    }

    /// The footer's targets line up with what it shows.
    #[tokio::test]
    async fn footer_targets_cover_the_badge_and_the_hints() {
        let mut h = harness(vec![view("A", "api", 10)]);
        for (width, height) in [(40, 20), (45, 40), (160, 48)] {
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
                let badge = if session { " SESSION " } else { " RAKUMUX " };
                assert_eq!(text(targets[0].0), badge);
                assert_eq!(targets[0].1, Hit::ModeBadge);
                for (area, hit) in &targets[1..] {
                    let shown = text(*area);
                    let wanted = match hit {
                        Hit::FooterKey(crate::app::FooterKey::Rakumux) => "rakumux",
                        Hit::FooterKey(crate::app::FooterKey::Help) => "? help",
                        Hit::FooterKey(crate::app::FooterKey::Settings) => ", settings",
                        other => panic!("{other:?}"),
                    };
                    assert!(shown.ends_with(wanted), "{shown:?} {wanted:?}");
                }
                // Help outlasts Settings as the row narrows.
                let has = |key| targets.iter().any(|(_, hit)| *hit == Hit::FooterKey(key));
                use crate::app::FooterKey::{Help, Settings};
                assert!(has(Help) || !has(Settings), "{width} {session}");
                if !session || width >= 45 {
                    assert!(has(Help), "{width} {session}");
                }
            }
        }
    }
}
