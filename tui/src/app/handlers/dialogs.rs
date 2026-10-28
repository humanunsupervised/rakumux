//! Keys while a dialog is open.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::app::types::{AccountDialogState, AppMode, AppResult, PairingState};
use crate::app::App;

pub async fn handle_dialog_key(app: &mut App, key: KeyEvent) -> AppResult {
    if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
        // Ctrl-c always closes the dialog (it never quits from one).
        if matches!(app.mode, AppMode::Account(_)) {
            app.close_account_dialog();
        } else if !dialog_busy(&app.mode) {
            app.mode = AppMode::Normal;
        }
        return AppResult::Continue;
    }
    match &app.mode {
        AppMode::Normal => {}
        AppMode::Folders(_) => super::folders::handle_folder_picker_key(app, key),
        AppMode::Sections(_) => super::sections::handle_section_picker_key(app, key),
        AppMode::Renaming(_) => handle_rename_key(app, key),
        AppMode::FeedReply(_) => handle_feed_reply_key(app, key),
        AppMode::Confirm(_) => handle_confirm_key(app, key),
        AppMode::DaemonRestart(_) => return handle_daemon_restart_key(app, key),
        AppMode::Modal { .. } | AppMode::Upgrade(_) => {
            return super::settings::handle_settings_key(app, key)
        }
        AppMode::Welcome(_) => handle_welcome_key(app, key),
        AppMode::PhoneOffer => handle_phone_offer_key(app, key),
        AppMode::Pairing(_) => handle_pairing_key(app, key),
        AppMode::Account(_) => handle_account_key(app, key),
        AppMode::Top(_) => handle_top_key(app, key),
        AppMode::Auth(_) | AppMode::AuthEmail(_) | AppMode::AuthCode(_) => {
            super::auth::handle_auth_key(app, key).await
        }
    }
    AppResult::Continue
}

/// The welcome tour: `←`/`→` (`h`/`l`, Tab) move, Enter goes on and
/// finishes on the last card, Space (`d`) on any card ticks "Don't show
/// this on startup", Esc (`q`) closes.
fn handle_welcome_key(app: &mut App, key: KeyEvent) {
    let AppMode::Welcome(mut state) = app.mode else {
        return;
    };
    let last = crate::components::welcome::CARDS - 1;
    match key.code {
        KeyCode::Right | KeyCode::Char('l') | KeyCode::Tab => {
            state.card = (state.card + 1).min(last)
        }
        KeyCode::Left | KeyCode::Char('h') | KeyCode::BackTab => {
            state.card = state.card.saturating_sub(1)
        }
        KeyCode::Enter if state.card < last => state.card += 1,
        KeyCode::Char(' ') | KeyCode::Char('d') => state.dont_show = !state.dont_show,
        KeyCode::Enter | KeyCode::Esc | KeyCode::Char('q') => {
            return app.close_welcome(state.dont_show)
        }
        _ => return,
    }
    app.mode = AppMode::Welcome(state);
}

fn dialog_busy(mode: &AppMode) -> bool {
    match mode {
        AppMode::Folders(d) => d.busy,
        AppMode::Renaming(d) => d.busy,
        AppMode::FeedReply(d) => d.busy,
        AppMode::DaemonRestart(d) => d.restarting,
        AppMode::Upgrade(d) => d.installing,
        _ => false,
    }
}

fn handle_rename_key(app: &mut App, key: KeyEvent) {
    let AppMode::Renaming(dialog) = &mut app.mode else {
        return;
    };
    if dialog.busy {
        return;
    }
    match key.code {
        KeyCode::Esc => app.mode = AppMode::Normal,
        KeyCode::Enter => app.submit_rename(),
        _ => {
            if dialog.input.handle_key(&key) {
                dialog.error_message = None;
            }
        }
    }
}

/// The reply box: Enter sends, Esc closes (what was written is dropped).
fn handle_feed_reply_key(app: &mut App, key: KeyEvent) {
    let AppMode::FeedReply(dialog) = &mut app.mode else {
        return;
    };
    if dialog.busy {
        return;
    }
    match key.code {
        KeyCode::Esc => app.mode = AppMode::Normal,
        KeyCode::Enter => app.submit_feed_reply(),
        _ => {
            if dialog.input.handle_key(&key) {
                dialog.error_message = None;
            }
        }
    }
}

fn handle_confirm_key(app: &mut App, key: KeyEvent) {
    let AppMode::Confirm(dialog) = &app.mode else {
        return;
    };
    match key.code {
        KeyCode::Enter | KeyCode::Char('y') | KeyCode::Char('Y') => {
            let action = dialog.action.clone();
            app.run_confirmed(action);
        }
        KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Char('q') => {
            app.mode = AppMode::Normal
        }
        _ => {}
    }
}

/// "Use Rakumux from your phone?": `y`/Enter turns phone access on and
/// shows how to pair, `n`/Esc declines (`rakumux remote on` can turn it on
/// later).
fn handle_phone_offer_key(app: &mut App, key: KeyEvent) {
    match key.code {
        KeyCode::Enter | KeyCode::Char('y') | KeyCode::Char('Y') => app.turn_phone_access_on(),
        KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Char('q') => {
            app.decline_phone_access()
        }
        _ => {}
    }
}

/// The pairing dialog: Esc (Enter, `q`) closes it, `x` turns phone access
/// off; when it's off, `y`/Enter turns it on. Nothing while waiting for the
/// server.
fn handle_pairing_key(app: &mut App, key: KeyEvent) {
    let AppMode::Pairing(dialog) = &app.mode else {
        return;
    };
    if dialog.busy() {
        return;
    }
    match (&dialog.state, key.code) {
        (PairingState::Off, KeyCode::Enter | KeyCode::Char('y') | KeyCode::Char('Y')) => {
            app.turn_phone_access_on()
        }
        (_, KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q')) => app.close_pairing(),
        (PairingState::Ready(_), KeyCode::Char('x') | KeyCode::Char('X')) => {
            app.turn_phone_access_off()
        }
        (PairingState::Failed(_), KeyCode::Char('x') | KeyCode::Char('X'))
            if app.phone_enabled == Some(true) =>
        {
            app.turn_phone_access_off()
        }
        _ => {}
    }
}

/// The account dialog: Enter signs in (or tries again), `c` copies the
/// sign-in link, `o` signs out, Esc (`q`) closes and abandons a sign-in under way. Only Esc while waiting
/// for the service.
fn handle_account_key(app: &mut App, key: KeyEvent) {
    let AppMode::Account(dialog) = &app.mode else {
        return;
    };
    let close = matches!(key.code, KeyCode::Esc | KeyCode::Char('q'));
    if dialog.busy() {
        if key.code == KeyCode::Esc {
            app.close_account_dialog();
        }
        return;
    }
    match (&dialog.state, key.code) {
        _ if close => app.close_account_dialog(),
        (AccountDialogState::SignedOut | AccountDialogState::Failed(_), KeyCode::Enter) => {
            app.start_sign_in()
        }
        (AccountDialogState::SignedIn(_), KeyCode::Char('o') | KeyCode::Char('O')) => {
            app.sign_out()
        }
        (AccountDialogState::SignedIn(_), KeyCode::Enter) => app.close_account_dialog(),
        (AccountDialogState::Waiting(_), KeyCode::Char('c') | KeyCode::Char('C')) => {
            app.copy_sign_in_link()
        }
        _ => {}
    }
}

/// Top: `↑`/`↓` (`j`/`k`), PgUp/PgDn, Home/End (`g`/`G`) select, `←`/`→`
/// (`h`/`l`) and Space fold and unfold a session's processes, `s` sorts the
/// sessions, Enter goes into the selected one, Esc (`q`, `t`) closes.
fn handle_top_key(app: &mut App, key: KeyEvent) {
    match key.code {
        KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('t') => app.close_top(),
        KeyCode::Down | KeyCode::Char('j') => app.top_move(1),
        KeyCode::Up | KeyCode::Char('k') => app.top_move(-1),
        KeyCode::PageDown => app.top_page(1),
        KeyCode::PageUp => app.top_page(-1),
        KeyCode::Home | KeyCode::Char('g') => app.top_move(isize::MIN),
        KeyCode::End | KeyCode::Char('G') => app.top_move(isize::MAX),
        KeyCode::Left | KeyCode::Char('h') => app.top_fold(Some(false)),
        KeyCode::Right | KeyCode::Char('l') => app.top_fold(Some(true)),
        KeyCode::Char(' ') => app.top_fold(None),
        KeyCode::Char('s') => app.top_sort(),
        KeyCode::Enter => app.top_enter(),
        _ => {}
    }
}

fn handle_daemon_restart_key(app: &mut App, key: KeyEvent) -> AppResult {
    let AppMode::DaemonRestart(dialog) = &app.mode else {
        return AppResult::Continue;
    };
    if dialog.restarting {
        return AppResult::Continue;
    }
    // An older daemon still works: restarting it ends the running sessions
    // unless it restarts in place, so then only `y` does, never a stray
    // Enter. Asked for, declining (or `q`) only closes it.
    let outdated = !dialog.missing.is_empty();
    if dialog.requested {
        match key.code {
            KeyCode::Enter if !dialog.in_place => {}
            KeyCode::Enter | KeyCode::Char('y') | KeyCode::Char('Y') => app.restart_daemon(),
            KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Char('q') => {
                app.mode = AppMode::Normal;
                app.set_status("The server keeps running as it is.");
            }
            _ => {}
        }
        return AppResult::Continue;
    }
    match key.code {
        KeyCode::Enter if outdated && !dialog.in_place => {}
        KeyCode::Enter | KeyCode::Char('y') | KeyCode::Char('Y') => app.restart_daemon(),
        KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('N') if outdated => {
            app.mode = AppMode::Normal;
            app.set_status(
                "Kept the older server: what it can't show waits for \
                 `rakumux daemon restart`.",
            );
        }
        KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('N') => {
            app.mode = AppMode::Normal;
            app.set_status("Daemon restart declined. Ctrl-r to try again, q to quit.");
        }
        KeyCode::Char('q') => return AppResult::Exit,
        _ => {}
    }
    AppResult::Continue
}
