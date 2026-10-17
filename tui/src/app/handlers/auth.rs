//! Account dialogs (behind the `auth` feature flag, off by default).

use std::process::Command;

use crossterm::event::{KeyCode, KeyEvent};

use crate::app::types::{AppMode, AuthCodeDialog, AuthEmailDialog};
use crate::app::{App, TextInput};

pub async fn handle_auth_key(app: &mut App, key: KeyEvent) {
    match &app.mode {
        AppMode::Auth(_) => handle_menu_key(app, key),
        AppMode::AuthEmail(_) => handle_email_key(app, key).await,
        AppMode::AuthCode(_) => handle_code_key(app, key).await,
        _ => {}
    }
}

fn open_portal(app: &mut App, what: &str) {
    match std::env::var("AUTH_PORTAL_URL") {
        Ok(url) => {
            let _ = Command::new("open").arg(url).spawn();
            app.set_status(format!("Opening {what}..."));
        }
        Err(_) => app.set_status("AUTH_PORTAL_URL not set"),
    }
}

fn handle_menu_key(app: &mut App, key: KeyEvent) {
    let AppMode::Auth(menu) = &mut app.mode else {
        return;
    };
    match key.code {
        KeyCode::Esc => app.mode = AppMode::Normal,
        KeyCode::Up | KeyCode::Char('k') => menu.selected = menu.selected.saturating_sub(1),
        KeyCode::Down | KeyCode::Char('j') => {
            if menu.selected + 1 < menu.items.len() {
                menu.selected += 1;
            }
        }
        KeyCode::Enter => {
            let choice = menu.items.get(menu.selected).copied().unwrap_or("");
            app.mode = AppMode::Normal;
            match choice {
                "Sign in" if app.auth.enabled() => {
                    app.mode = AppMode::AuthEmail(AuthEmailDialog {
                        input: TextInput::default(),
                        error_message: None,
                    });
                }
                "Sign in" => app.set_status("Auth not configured (missing Clerk env)"),
                "Sign up" => app.set_status("Sign-up flow not implemented yet"),
                "Manage subscription" => open_portal(app, "subscription portal"),
                "View account" => open_portal(app, "account page"),
                "Sign out" => match app.auth.sign_out() {
                    Ok(()) => app.set_status("Signed out"),
                    Err(e) => app.set_status(format!("Sign out failed: {e}")),
                },
                _ => {}
            }
        }
        _ => {}
    }
}

async fn handle_email_key(app: &mut App, key: KeyEvent) {
    let AppMode::AuthEmail(dialog) = &mut app.mode else {
        return;
    };
    match key.code {
        KeyCode::Esc => app.mode = AppMode::Normal,
        KeyCode::Enter => {
            let email = dialog.input.value().trim().to_string();
            if email.is_empty() || !email.contains('@') {
                dialog.error_message = Some("Enter a valid email".to_string());
                return;
            }
            match app.auth.start_email_sign_in(&email).await {
                Ok(()) => {
                    app.set_status("Code sent (check email)");
                    app.mode = AppMode::AuthCode(AuthCodeDialog {
                        input: TextInput::default(),
                        error_message: None,
                    });
                }
                Err(e) => {
                    if let AppMode::AuthEmail(dialog) = &mut app.mode {
                        dialog.error_message = Some(format!("Failed to start sign-in: {e}"));
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

async fn handle_code_key(app: &mut App, key: KeyEvent) {
    let AppMode::AuthCode(dialog) = &mut app.mode else {
        return;
    };
    match key.code {
        KeyCode::Esc => app.mode = AppMode::Normal,
        KeyCode::Enter => {
            let code = dialog.input.value().trim().to_string();
            if code.is_empty() {
                dialog.error_message = Some("Enter the code".to_string());
                return;
            }
            match app.auth.complete_email_code(&code).await {
                Ok(()) => {
                    app.set_status("Signed in");
                    app.mode = AppMode::Normal;
                }
                Err(e) => {
                    if let AppMode::AuthCode(dialog) = &mut app.mode {
                        dialog.error_message = Some(format!("Verification failed: {e}"));
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
