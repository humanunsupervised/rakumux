//! Keys while the session history modal is open: typing narrows the rows,
//! `↑↓` move, Enter brings the session under the cursor back and goes into
//! it, Esc clears what's typed, then closes.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::app::types::AppMode;
use crate::app::App;

pub fn handle_history_key(app: &mut App, key: KeyEvent) {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    match key.code {
        KeyCode::Esc => app.history_escape(),
        KeyCode::Up => app.move_history_cursor(-1),
        KeyCode::Down => app.move_history_cursor(1),
        KeyCode::Char('p') if ctrl => app.move_history_cursor(-1),
        KeyCode::Char('n') if ctrl => app.move_history_cursor(1),
        KeyCode::PageUp => app.move_history_cursor(-10),
        KeyCode::PageDown => app.move_history_cursor(10),
        KeyCode::Enter => app.history_enter(),
        _ => {
            let AppMode::History(picker) = &mut app.mode else {
                return;
            };
            let before = picker.query.value().to_string();
            if picker.query.handle_key(&key) && picker.query.value() != before {
                app.history_query_changed();
            }
        }
    }
}
