//! Keys while the Folders modal is open: typing filters (or names a folder
//! to add, offering the folders that complete it), `↑↓` move, Enter shows
//! the folder under the cursor (All folders: every folder; after ticking,
//! the ticked ones) or adds the typed or chosen folder, Tab ticks a folder
//! (or completes the typed path; → at the end of the query too), Space
//! ticks while the query isn't a path, Esc closes without changes.
//! Creating a folder is confirmed with `y`.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::app::folders::looks_like_path;
use crate::app::types::{AppMode, PickerStage};
use crate::app::App;

pub fn handle_folder_picker_key(app: &mut App, key: KeyEvent) {
    let AppMode::Folders(picker) = &mut app.mode else {
        return;
    };
    if picker.busy {
        return;
    }
    match &picker.stage {
        PickerStage::Browse => handle_browse_key(app, key),
        // Only `y`: a second Enter is too easy to send by mistake.
        PickerStage::Creating(_) => match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') => app.create_picker_folder(),
            KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Char('q') => {
                picker.stage = PickerStage::Browse;
            }
            _ => {}
        },
    }
}

fn handle_browse_key(app: &mut App, key: KeyEvent) {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    match key.code {
        KeyCode::Esc => app.mode = AppMode::Normal,
        KeyCode::Char('c') if ctrl => app.mode = AppMode::Normal,
        KeyCode::Up => app.move_picker_cursor(-1),
        KeyCode::Down => app.move_picker_cursor(1),
        KeyCode::Char('p') if ctrl => app.move_picker_cursor(-1),
        KeyCode::Char('n') if ctrl => app.move_picker_cursor(1),
        KeyCode::PageUp => app.move_picker_cursor(-10),
        KeyCode::PageDown => app.move_picker_cursor(10),
        KeyCode::Enter => app.picker_enter(),
        KeyCode::Tab => app.picker_tab(),
        // Space ticks, except in a typed path (folder names have spaces).
        KeyCode::Char(' ') if !ctrl && !typing_a_path(app) => app.toggle_picker_tick(),
        // → at the end of a typed folder completes it, like Tab.
        KeyCode::Right if key.modifiers.is_empty() && at_end(app) => {
            app.complete_picker_folder();
        }
        _ => {
            let AppMode::Folders(picker) = &mut app.mode else {
                return;
            };
            let before = picker.query.value().to_string();
            if picker.query.handle_key(&key) && picker.query.value() != before {
                app.picker_query_changed();
            }
        }
    }
}

/// The query is a folder path.
fn typing_a_path(app: &App) -> bool {
    match &app.mode {
        AppMode::Folders(picker) => looks_like_path(picker.query.value()),
        _ => false,
    }
}

/// The cursor is at the end of the query.
fn at_end(app: &App) -> bool {
    match &app.mode {
        AppMode::Folders(picker) => {
            picker.query.cursor_column() == picker.query.value().chars().count()
        }
        _ => false,
    }
}
