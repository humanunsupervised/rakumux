//! Keys while the workspaces modal is open: `↑↓` move, Enter shows (or
//! moves the session to) the workspace under the cursor or names a new one,
//! `e` renames a workspace, `d` twice deletes one, Esc stops naming, then closes. While a
//! name is typed, keys type.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::app::types::AppMode;
use crate::app::App;

pub fn handle_workspaces_key(app: &mut App, key: KeyEvent) {
    let naming = matches!(&app.mode, AppMode::Workspaces(picker) if picker.naming.is_some());
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    match key.code {
        KeyCode::Esc => app.workspace_escape(),
        KeyCode::Enter => app.workspace_enter(),
        KeyCode::Up => app.move_workspace_cursor(-1),
        KeyCode::Down => app.move_workspace_cursor(1),
        KeyCode::Char('p') if ctrl => app.move_workspace_cursor(-1),
        KeyCode::Char('n') if ctrl => app.move_workspace_cursor(1),
        _ if naming => {
            app.workspace_type(&key);
        }
        KeyCode::Char('k') => app.move_workspace_cursor(-1),
        KeyCode::Char('j') => app.move_workspace_cursor(1),
        KeyCode::Char('d') => app.workspace_delete_key(),
        KeyCode::Char('e') => app.workspace_rename_key(),
        _ => {}
    }
}
