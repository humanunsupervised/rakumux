//! Keys in session mode: everything goes to the session except the
//! Rakumux key (`Ctrl-\`), which switches to Rakumux mode, and the
//! session cycle keys when `sessions.cycle_keys_in_session` is on.

use crossterm::event::KeyEvent;

use crate::app::types::AppResult;
use crate::app::App;

pub fn handle_session_key(app: &mut App, key: KeyEvent) -> AppResult {
    if app.rakumux_key.matches(&key) {
        app.enter_rakumux_mode();
        return AppResult::Continue;
    }
    if app.settings.sessions.cycle_keys_in_session {
        if let Some(delta) = app.cycle_direction(&key) {
            app.cycle_session(delta);
            return AppResult::Continue;
        }
    }
    app.forward_key(&key);
    AppResult::Continue
}
