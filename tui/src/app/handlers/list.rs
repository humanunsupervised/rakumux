//! Keys in Rakumux mode: the session list (Sidebar) or tab bar (Tabs)
//! has the keys, and the session is previewed. Arrows and Vim keys both
//! move (←/→ switch the list's tabs); Enter (or `i`) goes into a session;
//! Esc cancels what's pending, then goes back to the session Rakumux mode
//! was entered from. `f` shows the feed and gives it the keys, gives them
//! to it while it's shown, and hides it while it has them; with them, the
//! moves go through its cards, Enter reads one and Esc gives them back
//! (`App::feed_key`). `b` hides Master detail's list beside the session,
//! and shows it again.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::settings::{open_help, open_settings};
use crate::app::types::{AppMode, AppResult, AuthMenu};
use crate::app::{App, Selection};

pub fn handle_list_key(app: &mut App, key: KeyEvent) -> AppResult {
    if app.list.filter_editing {
        handle_filter_key(app, key);
        return AppResult::Continue;
    }
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    // The second key of `gg`; anything else drops the first.
    if let Some(first) = app.pending_key.take() {
        match (first, key.code) {
            ('g', KeyCode::Char('g')) if !ctrl && !alt && app.feed_has_keys() => {
                app.feed_to_top();
                return AppResult::Continue;
            }
            ('g', KeyCode::Char('g')) if !ctrl && !alt => {
                app.move_selection(-(app.list.row_count() as isize));
                return AppResult::Continue;
            }
            // Esc only cancels the pending key.
            (_, KeyCode::Esc) => return AppResult::Continue,
            _ => {}
        }
    }
    if app.rakumux_key.matches(&key) {
        app.return_to_session();
        return AppResult::Continue;
    }
    if let Some(delta) = app.cycle_direction(&key) {
        if app.feed_has_keys() {
            app.move_feed_selection(delta as isize);
        } else {
            app.cycle_session(delta);
        }
        return AppResult::Continue;
    }
    if app.feed_has_keys() && app.feed_key(key) {
        return AppResult::Continue;
    }
    match key.code {
        KeyCode::Char('c') if ctrl => return AppResult::Exit,
        KeyCode::Char('r') if ctrl => app.refresh(),
        KeyCode::Char('d') if ctrl => app.move_half_page(1),
        KeyCode::Char('u') if ctrl => app.move_half_page(-1),
        KeyCode::Char('e') if ctrl => app.scroll_preview(-1),
        KeyCode::Char('y') if ctrl => app.scroll_preview(1),
        // No shortcut uses this Ctrl or Alt: it's the plain key. A phone's
        // key bar (Termius' ctrl and alt) or a held modifier on a hardware
        // keyboard sends them with keys the user means plainly: Ctrl-↑ moves,
        // Alt-q quits.
        _ if ctrl || alt => {
            let plain = KeyEvent::new(
                key.code,
                key.modifiers - (KeyModifiers::CONTROL | KeyModifiers::ALT),
            );
            return handle_list_key(app, plain);
        }
        KeyCode::Char('q') => return AppResult::Exit,
        KeyCode::Up | KeyCode::Char('k') if app.browsing_grid() => {
            app.move_grid_selection(-(app.grid_page().columns as isize))
        }
        KeyCode::Down | KeyCode::Char('j') if app.browsing_grid() => {
            app.move_grid_selection(app.grid_page().columns as isize)
        }
        KeyCode::Left | KeyCode::Char('h') if app.browsing_grid() => app.move_grid_selection(-1),
        KeyCode::Right | KeyCode::Char('l') if app.browsing_grid() => app.move_grid_selection(1),
        // ←/→ go along the list's tabs wherever the selection is (`h`/`l`
        // on the top row, where Enter opens the active one's modal); ↑/↓
        // go through the sessions.
        KeyCode::Left => app.switch_tab(-1),
        KeyCode::Right => app.switch_tab(1),
        KeyCode::Char('h') if on_tabs(app) => app.switch_tab(-1),
        KeyCode::Char('l') if on_tabs(app) => app.switch_tab(1),
        KeyCode::Up | KeyCode::Char('k') => app.move_selection(-1),
        KeyCode::Down | KeyCode::Char('j') => app.move_selection(1),
        KeyCode::Home => app.move_selection(-(app.list.row_count() as isize)),
        KeyCode::End | KeyCode::Char('G') => app.move_selection(app.list.row_count() as isize),
        KeyCode::Char('g') => app.pending_key = Some('g'),
        KeyCode::Char('{') => app.move_to_folder_group(-1),
        KeyCode::Char('}') => app.move_to_folder_group(1),
        KeyCode::PageUp => {
            let page = app.preview_page();
            app.scroll_preview(page);
        }
        KeyCode::PageDown => {
            let page = app.preview_page();
            app.scroll_preview(-page);
        }
        KeyCode::Enter | KeyCode::Char('i') | KeyCode::Char('l') => app.activate_selected(),
        KeyCode::Tab => app.go_to_last_session(),
        KeyCode::Char(digit @ '1'..='9') => app.go_into_nth(digit as usize - '0' as usize),
        KeyCode::Char('n') => app.open_new_session(),
        KeyCode::Char('T') => app.toggle_terminal_panel(),
        KeyCode::Char('b') => app.toggle_list(),
        KeyCode::Char('f') => app.toggle_feed(),
        KeyCode::Char('v') => app.set_layout(app.next_layout()),
        KeyCode::Char('p') => app.toggle_pin(),
        KeyCode::Char('s') => app.summarize_selected(),
        KeyCode::Char('J') => app.move_selected_session(1),
        KeyCode::Char('K') => app.move_selected_session(-1),
        KeyCode::Char('P') => app.open_folder_picker(),
        KeyCode::Char('r') => app.open_rename(),
        KeyCode::Char('R') => app.request_restart(),
        KeyCode::Char('S') => app.request_server_restart(),
        // A second `d` is ignored by the dialog, so `dd` works too without
        // bypassing the explicit deletion confirmation.
        KeyCode::Char('d') => app.confirm_delete(),
        KeyCode::Char('x') => app.confirm_close(),
        KeyCode::Char('z') => app.zoom_into_selected(),
        KeyCode::Char('/') => app.list.filter_editing = true,
        KeyCode::Char('\\') => app.send_rakumux_key(),
        // Cancel first (the filter), then back to the session.
        KeyCode::Esc => {
            if app.list.filter.is_empty() {
                app.return_to_session();
            } else {
                app.list.filter.set("");
                app.reconcile_detail();
            }
        }
        KeyCode::Char('?') => open_help(app),
        KeyCode::Char(',') | KeyCode::Char('o') => open_settings(app),
        KeyCode::Char('U') => app.open_upgrade_dialog(),
        KeyCode::Char('a') if app.flags.auth => {
            let items = if app.auth.has_session() {
                vec!["Manage subscription", "View account", "Sign out"]
            } else {
                vec!["Sign in", "Sign up"]
            };
            app.mode = AppMode::Auth(AuthMenu { items, selected: 0 });
        }
        KeyCode::Char('a') => app.account_key(),
        KeyCode::Char('c') => match app.selected_cwd() {
            Some(cwd) => return AppResult::SwitchDirectory(cwd),
            None => app.set_status("Select a session to cd into its folder."),
        },
        _ => {}
    }
    AppResult::Continue
}

/// The top row (the tabs) is selected.
fn on_tabs(app: &App) -> bool {
    app.list.selection() == &Selection::Folders
}

/// `/` filter: typing narrows the list; Enter keeps the filter, Esc
/// clears it.
fn handle_filter_key(app: &mut App, key: KeyEvent) {
    match key.code {
        KeyCode::Enter => app.list.filter_editing = false,
        KeyCode::Esc => {
            app.list.filter_editing = false;
            app.list.filter.set("");
        }
        KeyCode::Up => app.move_selection(-1),
        KeyCode::Down => app.move_selection(1),
        _ => {
            app.list.filter.handle_key(&key);
        }
    }
    app.list.ensure_selection_visible();
    app.reconcile_detail();
}
