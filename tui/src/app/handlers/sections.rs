//! Keys while the App or Status modal is open: typing narrows the rows,
//! `↑↓` move, Enter shows the app or status under the cursor (All: every
//! one; after ticking, the ticked ones), Tab ticks, Space ticks too while
//! nothing is typed (status names have spaces), Esc closes without changes.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::app::types::AppMode;
use crate::app::App;

pub fn handle_section_picker_key(app: &mut App, key: KeyEvent) {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    match key.code {
        KeyCode::Esc => app.mode = AppMode::Normal,
        KeyCode::Up => app.move_section_cursor(-1),
        KeyCode::Down => app.move_section_cursor(1),
        KeyCode::Char('p') if ctrl => app.move_section_cursor(-1),
        KeyCode::Char('n') if ctrl => app.move_section_cursor(1),
        KeyCode::PageUp => app.move_section_cursor(-10),
        KeyCode::PageDown => app.move_section_cursor(10),
        KeyCode::Enter => app.section_picker_enter(),
        KeyCode::Tab => app.toggle_section_tick(),
        KeyCode::Char(' ') if !ctrl && !typing(app) => app.toggle_section_tick(),
        _ => {
            let AppMode::Sections(picker) = &mut app.mode else {
                return;
            };
            let before = picker.query.value().to_string();
            if picker.query.handle_key(&key) && picker.query.value() != before {
                app.section_query_changed();
            }
        }
    }
}

/// Something is typed: Space goes into the query.
fn typing(app: &App) -> bool {
    match &app.mode {
        AppMode::Sections(picker) => !picker.query.is_empty(),
        _ => false,
    }
}
