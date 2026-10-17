//! The Settings modal (`,` or `o` in Rakumux mode: General and Keys tabs)
//! and the Help modal (`?`): their tabs, the actions the Keys tab runs,
//! and the dialogs of the General tab (session layout, AI provider keys and
//! models); and the upgrade dialog.

use crossterm::event::{KeyCode, KeyEvent};

use crate::app::types::{
    AiProviderDialog, ApiKeyDialog, ApiProvider, AppMode, AppResult, Focus, OpenAiModelDialog,
    SettingsItem,
};
use crate::app::{
    effective_openai_model, normalize_primary_provider, openai_model_options, App, TextInput,
    DEFAULT_OPENAI_MODEL,
};
use crate::help::{KeyAction, Modal, ModalAction, ModalState, ModalTab};

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
        AppMode::ConfiguringApiKey(_) => handle_api_key_key(app, key),
        AppMode::AiProvider(_) => handle_ai_provider_key(app, key),
        AppMode::OpenAiModel(_) => handle_openai_model_key(app, key),
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

/// Run a Keys tab action as its keys would in Rakumux mode.
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
        KeyAction::Help => open_help(app),
        KeyAction::Settings => open_settings(app),
        KeyAction::Tour => app.open_welcome(),
        KeyAction::CheckForUpdate => app.open_upgrade_dialog(),
        KeyAction::Reload => app.refresh(),
        KeyAction::QuitAndCd => match app.selected_cwd() {
            Some(cwd) => return AppResult::SwitchDirectory(cwd),
            None => app.set_status("Select a session to cd into its folder."),
        },
        KeyAction::Quit => return AppResult::Exit,
        KeyAction::SendRakumuxKey => app.send_rakumux_key(),
    }
    AppResult::Continue
}

/// Close a General tab dialog: back to the modal with its setting
/// selected.
pub fn back_to_settings(app: &mut App) {
    let row = app.mode.editing_setting().map_or(0, SettingsItem::index);
    app.mode = AppMode::Modal {
        state: ModalState::new(ModalTab::General, SettingsItem::ALL.len()).with_selected(row),
        in_session: app.focus == Focus::Session,
    };
}

fn open_item(app: &mut App, item: SettingsItem) {
    match item {
        SettingsItem::Layout => {
            // Applies live and stays in the modal so the change is visible.
            let next = app.session_layout().next();
            app.set_layout(next);
            app.set_status(format!("Layout: {}", next.label()));
        }
        SettingsItem::Icons => {
            let next = app.icons().next();
            app.settings.sessions.icons = next;
            save_config(app, format!("Icons: {}", next.label()));
        }
        SettingsItem::PhoneAccess => app.open_phone_access(),
        SettingsItem::CycleInSessions => {
            let on = !app.settings.sessions.cycle_keys_in_session;
            app.settings.sessions.cycle_keys_in_session = on;
            let state = if on { "on" } else { "off" };
            save_config(app, format!("Option+↑/↓ in sessions: {state}"));
        }
        SettingsItem::AnthropicKey | SettingsItem::OpenAiKey => {
            let (provider, current) = if item == SettingsItem::AnthropicKey {
                (
                    ApiProvider::Anthropic,
                    app.settings.ai.anthropic_api_key.clone(),
                )
            } else {
                (ApiProvider::OpenAI, app.settings.ai.openai_api_key.clone())
            };
            app.mode = AppMode::ConfiguringApiKey(ApiKeyDialog {
                provider,
                input: TextInput::new(current.unwrap_or_default()),
                error_message: None,
            });
        }
        SettingsItem::PrimaryAiProvider => {
            let current =
                normalize_primary_provider(app.settings.ai.ai_primary_provider.as_deref())
                    .to_string();
            let selected = usize::from(current == "openai");
            app.mode = AppMode::AiProvider(AiProviderDialog { selected, current });
        }
        SettingsItem::OpenAiModel => {
            let options = openai_model_options();
            let current = effective_openai_model(app.settings.ai.openai_model.as_deref());
            let selected = options.iter().position(|m| m == &current).unwrap_or(0);
            app.mode = AppMode::OpenAiModel(OpenAiModelDialog {
                selected,
                current,
                options,
            });
        }
    }
}

fn save_config(app: &mut App, success: String) {
    match app.save_settings() {
        Ok(()) => app.set_status(success),
        Err(e) => app.set_status(format!("Failed to save config: {e}")),
    }
}

fn handle_api_key_key(app: &mut App, key: KeyEvent) {
    let AppMode::ConfiguringApiKey(dialog) = &mut app.mode else {
        return;
    };
    match key.code {
        KeyCode::Esc => back_to_settings(app),
        KeyCode::Enter => {
            let value = dialog.input.value().trim().to_string();
            let (prefix, label) = match dialog.provider {
                ApiProvider::Anthropic => ("sk-ant-", "Anthropic"),
                ApiProvider::OpenAI => ("sk-", "OpenAI"),
            };
            if !value.is_empty() && !value.starts_with(prefix) {
                dialog.error_message = Some(format!("API key should start with '{prefix}'"));
                return;
            }
            let value = (!value.is_empty()).then_some(value);
            match dialog.provider {
                ApiProvider::Anthropic => app.settings.ai.anthropic_api_key = value,
                ApiProvider::OpenAI => app.settings.ai.openai_api_key = value,
            }
            match app.save_settings() {
                Ok(()) => {
                    back_to_settings(app);
                    app.set_status(format!("{label} API key saved"));
                }
                Err(e) => {
                    if let AppMode::ConfiguringApiKey(dialog) = &mut app.mode {
                        dialog.error_message = Some(format!("Failed to save config: {e}"));
                    }
                }
            }
        }
        _ => {
            if dialog.input.handle_key(&key) {
                dialog.error_message = None;
            }
        }
    }
}

fn handle_ai_provider_key(app: &mut App, key: KeyEvent) {
    let AppMode::AiProvider(dialog) = &mut app.mode else {
        return;
    };
    match key.code {
        KeyCode::Esc => back_to_settings(app),
        KeyCode::Up | KeyCode::Char('k') => dialog.selected = dialog.selected.saturating_sub(1),
        KeyCode::Down | KeyCode::Char('j') => dialog.selected = (dialog.selected + 1).min(1),
        KeyCode::Enter => {
            let provider = if dialog.selected == 1 {
                "openai"
            } else {
                "anthropic"
            };
            app.settings.ai.ai_primary_provider = Some(provider.to_string());
            if provider == "openai" && app.settings.ai.openai_model.is_none() {
                app.settings.ai.openai_model = Some(DEFAULT_OPENAI_MODEL.to_string());
            }
            back_to_settings(app);
            let label = if provider == "openai" {
                "OpenAI"
            } else {
                "Anthropic"
            };
            save_config(app, format!("Primary AI provider set to {label}"));
        }
        _ => {}
    }
}

fn handle_openai_model_key(app: &mut App, key: KeyEvent) {
    let AppMode::OpenAiModel(dialog) = &mut app.mode else {
        return;
    };
    match key.code {
        KeyCode::Esc => back_to_settings(app),
        KeyCode::Up | KeyCode::Char('k') => dialog.selected = dialog.selected.saturating_sub(1),
        KeyCode::Down | KeyCode::Char('j') => {
            if !dialog.options.is_empty() {
                dialog.selected = (dialog.selected + 1).min(dialog.options.len() - 1);
            }
        }
        KeyCode::Enter => {
            let model = dialog
                .options
                .get(dialog.selected)
                .cloned()
                .unwrap_or_else(|| DEFAULT_OPENAI_MODEL.to_string());
            app.settings.ai.openai_model = Some(model.clone());
            back_to_settings(app);
            save_config(app, format!("OpenAI model set to {model}"));
        }
        _ => {}
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
            app.mode = AppMode::Normal;
        }
        return;
    }
    match key.code {
        KeyCode::Esc => app.mode = AppMode::Normal,
        KeyCode::Up | KeyCode::Char('k') => dialog.selection = dialog.selection.saturating_sub(1),
        KeyCode::Down | KeyCode::Char('j') => dialog.selection = (dialog.selection + 1).min(1),
        KeyCode::Enter => {
            if !dialog.update_available || dialog.selection != 0 {
                app.mode = AppMode::Normal;
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
