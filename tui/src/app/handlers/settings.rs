//! The Settings modal (`,` or `o` in Rakumux mode: the settings, then
//! every action) and the Help modal (`?`): the settings it changes, the
//! actions it runs; and the upgrade dialog.

use crossterm::event::{KeyCode, KeyEvent};

use crate::app::types::{AppMode, AppResult, Focus, SettingsItem};
use crate::app::App;
use crate::help::{KeyAction, Modal, ModalAction, ModalTab};

/// Open Settings or Help (whichever has `tab`) on `tab`, from whichever
/// pane has focus.
pub fn open_modal(app: &mut App, tab: ModalTab) {
    app.mode = AppMode::modal(tab, app.focus == Focus::Session);
}

/// Open the Settings modal on its first tab.
pub fn open_settings(app: &mut App) {
    open_modal(app, Modal::Settings.first_tab());
}

/// Open the Help modal on its first tab.
pub fn open_help(app: &mut App) {
    open_modal(app, Modal::Help.first_tab());
}

pub fn handle_settings_key(app: &mut App, key: KeyEvent) -> AppResult {
    match &app.mode {
        AppMode::Modal { .. } => return handle_modal_key(app, key),
        AppMode::Upgrade(_) => handle_upgrade_key(app, key),
        _ => {}
    }
    AppResult::Continue
}

fn handle_modal_key(app: &mut App, key: KeyEvent) -> AppResult {
    let AppMode::Modal { state, .. } = &mut app.mode else {
        return AppResult::Continue;
    };
    match state.handle_key(&key) {
        ModalAction::Close => app.mode = AppMode::Normal,
        ModalAction::Handled => {}
        ModalAction::Change(row) => {
            if let Some(item) = SettingsItem::ALL.get(row) {
                open_item(app, *item);
            }
        }
        ModalAction::Run(action) => {
            app.mode = AppMode::Normal;
            return run_action(app, action);
        }
        ModalAction::Tour => app.open_welcome(),
        // Ctrl/Alt keys: the session cycle keys still work, and the
        // Rakumux key closes the modal.
        ModalAction::Ignored => {
            if let Some(delta) = app.cycle_direction(&key) {
                app.cycle_session(delta);
            } else if app.rakumux_key.matches(&key) {
                app.mode = AppMode::Normal;
            }
        }
    }
    AppResult::Continue
}

/// Run an action from Settings as its keys would in Rakumux mode.
pub fn run_action(app: &mut App, action: KeyAction) -> AppResult {
    let in_session = app.focus == Focus::Session;
    // Actions on the list happen in Rakumux mode.
    let leave = |app: &mut App| {
        if app.focus == Focus::Session {
            app.enter_rakumux_mode();
        }
    };
    match action {
        KeyAction::RakumuxMode => leave(app),
        KeyAction::BackToSession => {
            if !in_session {
                app.return_to_session();
            }
        }
        KeyAction::GoIn => {
            if !in_session {
                app.activate_selected();
            }
        }
        KeyAction::LastSession => app.go_to_last_session(),
        KeyAction::NumberedSession => {
            app.set_status("Press 1 to 9 in Rakumux mode to work in that session.")
        }
        KeyAction::NextSession => app.cycle_session(1),
        KeyAction::PreviousSession => app.cycle_session(-1),
        KeyAction::NextTab | KeyAction::PreviousTab => {
            leave(app);
            app.switch_tab(if action == KeyAction::NextTab { 1 } else { -1 });
        }
        KeyAction::Top | KeyAction::Bottom => {
            leave(app);
            let rows = app.list.row_count() as isize;
            app.move_selection(if action == KeyAction::Top {
                -rows
            } else {
                rows
            });
        }
        KeyAction::HalfPageDown | KeyAction::HalfPageUp => {
            leave(app);
            app.move_half_page(if action == KeyAction::HalfPageDown {
                1
            } else {
                -1
            });
        }
        KeyAction::NextFolder | KeyAction::PreviousFolder => {
            leave(app);
            app.move_to_folder_group(if action == KeyAction::NextFolder {
                1
            } else {
                -1
            });
        }
        KeyAction::ScrollBack | KeyAction::ScrollForward => {
            let page = app.preview_page();
            app.scroll_preview(if action == KeyAction::ScrollBack {
                page
            } else {
                -page
            });
        }
        KeyAction::NewSession => app.open_new_session(),
        KeyAction::FilterFolders => {
            leave(app);
            app.open_folder_picker();
        }
        KeyAction::Rename => app.open_rename(),
        KeyAction::Pin => app.toggle_pin(),
        KeyAction::MoveDown | KeyAction::MoveUp => {
            leave(app);
            app.move_selected_session(if action == KeyAction::MoveDown { 1 } else { -1 });
        }
        KeyAction::Close => app.confirm_close(),
        KeyAction::Delete => app.confirm_delete(),
        KeyAction::Zoom => {
            leave(app);
            app.zoom_into_selected();
        }
        KeyAction::Filter => {
            leave(app);
            app.list.filter_editing = true;
        }
        KeyAction::CycleLayout => {
            leave(app);
            app.set_layout(app.next_layout());
        }
        KeyAction::Help => open_help(app),
        KeyAction::Settings => open_settings(app),
        KeyAction::Tour => app.open_welcome(),
        KeyAction::CheckForUpdate => app.open_upgrade_dialog(),
        KeyAction::TerminalPanel => {
            leave(app);
            app.toggle_terminal_panel();
        }
        KeyAction::Reload => app.refresh(),
        KeyAction::Restart => app.request_restart(),
        KeyAction::RestartServer => app.request_server_restart(),
        KeyAction::QuitAndCd => match app.selected_cwd() {
            Some(cwd) => return AppResult::SwitchDirectory(cwd),
            None => app.set_status("Select a session to cd into its folder."),
        },
        KeyAction::Quit => return AppResult::Exit,
        KeyAction::SendRakumuxKey => app.send_rakumux_key(),
    }
    AppResult::Continue
}

fn open_item(app: &mut App, item: SettingsItem) {
    match item {
        SettingsItem::Layout => {
            // Applies live and stays in the modal so the change is visible.
            let next = app.next_layout();
            app.set_layout(next);
            app.set_status(format!("Layout: {}", next.label()));
        }
        SettingsItem::Icons => {
            let next = app.icons().next();
            app.settings.sessions.icons = next;
            save_config(app, format!("Icons: {}", next.label()));
        }
        SettingsItem::Update => app.open_upgrade_dialog(),
        SettingsItem::Account => app.open_account_dialog(),
        SettingsItem::RestartUi => app.request_restart(),
        SettingsItem::RestartServer => app.request_server_restart(),
        SettingsItem::TerminalPanel => {
            app.mode = AppMode::Normal;
            app.show_terminal_panel();
        }
        SettingsItem::CycleInSessions => {
            let on = !app.settings.sessions.cycle_keys_in_session;
            app.settings.sessions.cycle_keys_in_session = on;
            let state = if on { "on" } else { "off" };
            save_config(app, format!("Option+↑/↓ in sessions: {state}"));
        }
    }
}

fn save_config(app: &mut App, success: String) {
    match app.save_settings() {
        Ok(()) => app.set_status(success),
        Err(e) => app.set_status(format!("Failed to save config: {e}")),
    }
}

fn handle_upgrade_key(app: &mut App, key: KeyEvent) {
    let AppMode::Upgrade(dialog) = &mut app.mode else {
        return;
    };
    if dialog.installing {
        return;
    }
    if dialog.checking {
        if key.code == KeyCode::Esc {
            app.close_upgrade_dialog();
        }
        return;
    }
    match key.code {
        KeyCode::Esc => app.close_upgrade_dialog(),
        KeyCode::Up | KeyCode::Char('k') => dialog.selection = dialog.selection.saturating_sub(1),
        KeyCode::Down | KeyCode::Char('j') => dialog.selection = (dialog.selection + 1).min(1),
        KeyCode::Enter => {
            if !dialog.update_available || dialog.selection != 0 {
                app.close_upgrade_dialog();
                return;
            }
            match (dialog.latest_version.clone(), dialog.install_method) {
                (Some(latest), Some(method)) => {
                    dialog.installing = true;
                    dialog.error_message = None;
                    dialog.info_message = Some(format!(
                        "Installing version {} via {}...",
                        latest,
                        crate::app::upgrade::install_method_display(method)
                    ));
                    app.start_upgrade_install(latest, method);
                }
                _ => {
                    dialog.error_message =
                        Some("Cannot determine install method for upgrade".to_string())
                }
            }
        }
        _ => {}
    }
}
