//! crossterm key / mouse / paste events → the bytes the program in the
//! terminal expects, via `vt::input` and the program's current modes.

use crossterm::event::{
    KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers, MouseButton as CtButton,
    MouseEvent, MouseEventKind,
};
use ratatui::layout::Rect;
use vt::input::{encode_key, encode_mouse, Key, Modifiers, MouseAction, MouseButton};

fn modifiers(mods: KeyModifiers) -> Modifiers {
    Modifiers {
        shift: mods.contains(KeyModifiers::SHIFT),
        alt: mods.contains(KeyModifiers::ALT),
        ctrl: mods.contains(KeyModifiers::CONTROL),
    }
}

/// Keypad keys in application keypad mode (DECKPAM) send `SS3` sequences.
/// crossterm only marks keypad keys (`KeyEventState::KEYPAD`) when the
/// kitty keyboard protocol is on; otherwise they arrive as ordinary keys.
fn application_keypad(code: KeyCode, mods: Modifiers) -> Option<Vec<u8>> {
    if mods.ctrl || mods.alt {
        return None;
    }
    let final_byte = match code {
        KeyCode::Char(c @ '0'..='9') => b'p' + (c as u8 - b'0'),
        KeyCode::Char('*') => b'j',
        KeyCode::Char('+') => b'k',
        KeyCode::Char(',') => b'l',
        KeyCode::Char('-') => b'm',
        KeyCode::Char('.') => b'n',
        KeyCode::Char('/') => b'o',
        KeyCode::Char('=') => b'X',
        KeyCode::Enter => b'M',
        _ => return None,
    };
    Some(vec![0x1b, b'O', final_byte])
}

/// See [`super::key_to_bytes`].
pub(super) fn encode_key_event(key: &KeyEvent, modes: &vt::Modes) -> Option<Vec<u8>> {
    if key.kind == KeyEventKind::Release {
        return None;
    }
    let mods = modifiers(key.modifiers);
    if modes.application_keypad && key.state.contains(KeyEventState::KEYPAD) {
        if let Some(bytes) = application_keypad(key.code, mods) {
            return Some(bytes);
        }
    }
    let encoded = match key.code {
        // The kitty keyboard protocol can report Shift+a as 'a' + SHIFT.
        KeyCode::Char(c) if mods.shift && c.is_ascii_lowercase() => {
            Key::Char(c.to_ascii_uppercase())
        }
        KeyCode::Char(c) => Key::Char(c),
        KeyCode::Enter => Key::Enter,
        KeyCode::Tab => Key::Tab,
        KeyCode::BackTab => Key::BackTab,
        KeyCode::Backspace => Key::Backspace,
        KeyCode::Esc => Key::Esc,
        KeyCode::Up => Key::Up,
        KeyCode::Down => Key::Down,
        KeyCode::Left => Key::Left,
        KeyCode::Right => Key::Right,
        KeyCode::Home => Key::Home,
        KeyCode::End => Key::End,
        KeyCode::PageUp => Key::PageUp,
        KeyCode::PageDown => Key::PageDown,
        KeyCode::Insert => Key::Insert,
        KeyCode::Delete => Key::Delete,
        KeyCode::F(n) => Key::F(n),
        KeyCode::Null => return Some(vec![0x00]),
        KeyCode::CapsLock
        | KeyCode::ScrollLock
        | KeyCode::NumLock
        | KeyCode::PrintScreen
        | KeyCode::Pause
        | KeyCode::Menu
        | KeyCode::KeypadBegin
        | KeyCode::Media(_)
        | KeyCode::Modifier(_) => return None,
    };
    let bytes = encode_key(encoded, mods, modes);
    (!bytes.is_empty()).then_some(bytes)
}

/// Zero-based `(col, row)` of the event inside `pane`, or `None` outside it.
pub(super) fn pane_position(event: &MouseEvent, pane: Rect) -> Option<(u16, u16)> {
    let inside = event.column >= pane.x
        && event.column < pane.right()
        && event.row >= pane.y
        && event.row < pane.bottom();
    inside.then(|| (event.column - pane.x, event.row - pane.y))
}

/// Encode `event` as happening at terminal cell `(col, row)`.
pub(super) fn encode_mouse_at(
    event: &MouseEvent,
    col: u16,
    row: u16,
    modes: &vt::Modes,
) -> Option<Vec<u8>> {
    let button = |button: CtButton| match button {
        CtButton::Left => MouseButton::Left,
        CtButton::Middle => MouseButton::Middle,
        CtButton::Right => MouseButton::Right,
    };
    let action = match event.kind {
        MouseEventKind::Down(b) => MouseAction::Press(button(b)),
        MouseEventKind::Up(b) => MouseAction::Release(button(b)),
        MouseEventKind::Drag(b) => MouseAction::Drag(button(b)),
        MouseEventKind::Moved => MouseAction::Move,
        MouseEventKind::ScrollUp => MouseAction::Press(MouseButton::WheelUp),
        MouseEventKind::ScrollDown => MouseAction::Press(MouseButton::WheelDown),
        // `vt::input` has no horizontal wheel buttons.
        MouseEventKind::ScrollLeft | MouseEventKind::ScrollRight => return None,
    };
    encode_mouse(action, col, row, modifiers(event.modifiers), modes)
}
