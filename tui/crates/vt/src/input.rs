//! Encode keys, pastes and mouse events as the bytes a program expects,
//! honouring the modes it has switched on (application cursor, bracketed
//! paste, mouse reporting).

use crate::Modes;
use vt100::{MouseProtocolEncoding, MouseProtocolMode};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Char(char),
    Enter,
    Tab,
    BackTab,
    Backspace,
    Esc,
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    PageUp,
    PageDown,
    Insert,
    Delete,
    F(u8),
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Modifiers {
    pub shift: bool,
    pub alt: bool,
    pub ctrl: bool,
}

impl Modifiers {
    pub const NONE: Self = Self {
        shift: false,
        alt: false,
        ctrl: false,
    };

    /// xterm modifier parameter: 1 + shift + 2*alt + 4*ctrl.
    fn param(self) -> u8 {
        1 + self.shift as u8 + 2 * self.alt as u8 + 4 * self.ctrl as u8
    }

    fn any(self) -> bool {
        self.shift || self.alt || self.ctrl
    }
}

fn ctrl_byte(c: char) -> Option<u8> {
    match c {
        'a'..='z' => Some(c as u8 - b'a' + 1),
        'A'..='Z' => Some(c as u8 - b'A' + 1),
        ' ' | '@' | '2' => Some(0x00),
        '[' | '3' => Some(0x1b),
        '\\' | '4' => Some(0x1c),
        ']' | '5' => Some(0x1d),
        '^' | '6' => Some(0x1e),
        '_' | '/' | '7' => Some(0x1f),
        '?' | '8' => Some(0x7f),
        _ => None,
    }
}

/// `CSI 1 ; m X` when modified, otherwise `SS3 X` / `CSI X` depending on
/// application cursor mode.
fn cursor_key(out: &mut Vec<u8>, final_byte: u8, mods: Modifiers, application: bool) {
    if mods.any() {
        out.extend_from_slice(format!("\x1b[1;{}", mods.param()).as_bytes());
        out.push(final_byte);
    } else if application {
        out.extend_from_slice(&[0x1b, b'O', final_byte]);
    } else {
        out.extend_from_slice(&[0x1b, b'[', final_byte]);
    }
}

/// `CSI n ~` or `CSI n ; m ~`.
fn tilde_key(out: &mut Vec<u8>, number: u8, mods: Modifiers) {
    if mods.any() {
        out.extend_from_slice(format!("\x1b[{number};{}~", mods.param()).as_bytes());
    } else {
        out.extend_from_slice(format!("\x1b[{number}~").as_bytes());
    }
}

pub fn encode_key(key: Key, mods: Modifiers, modes: &Modes) -> Vec<u8> {
    let mut out = Vec::new();
    match key {
        Key::Char(c) => {
            if mods.alt {
                out.push(0x1b);
            }
            match (mods.ctrl, ctrl_byte(c)) {
                (true, Some(byte)) => out.push(byte),
                _ => {
                    let mut buf = [0u8; 4];
                    out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
                }
            }
        }
        Key::Enter => {
            if mods.alt {
                out.push(0x1b);
            }
            out.push(b'\r');
        }
        Key::Tab => {
            if mods.shift {
                out.extend_from_slice(b"\x1b[Z");
            } else {
                if mods.alt {
                    out.push(0x1b);
                }
                out.push(b'\t');
            }
        }
        Key::BackTab => out.extend_from_slice(b"\x1b[Z"),
        Key::Backspace => {
            if mods.alt {
                out.push(0x1b);
            }
            out.push(if mods.ctrl { 0x08 } else { 0x7f });
        }
        Key::Esc => {
            if mods.alt {
                out.push(0x1b);
            }
            out.push(0x1b);
        }
        Key::Up => cursor_key(&mut out, b'A', mods, modes.application_cursor),
        Key::Down => cursor_key(&mut out, b'B', mods, modes.application_cursor),
        Key::Right => cursor_key(&mut out, b'C', mods, modes.application_cursor),
        Key::Left => cursor_key(&mut out, b'D', mods, modes.application_cursor),
        Key::Home => cursor_key(&mut out, b'H', mods, modes.application_cursor),
        Key::End => cursor_key(&mut out, b'F', mods, modes.application_cursor),
        Key::Insert => tilde_key(&mut out, 2, mods),
        Key::Delete => tilde_key(&mut out, 3, mods),
        Key::PageUp => tilde_key(&mut out, 5, mods),
        Key::PageDown => tilde_key(&mut out, 6, mods),
        Key::F(n @ 1..=4) => {
            let final_byte = b'P' + (n - 1);
            if mods.any() {
                out.extend_from_slice(format!("\x1b[1;{}", mods.param()).as_bytes());
                out.push(final_byte);
            } else {
                out.extend_from_slice(&[0x1b, b'O', final_byte]);
            }
        }
        Key::F(n @ 5..=12) => {
            let number = [15, 17, 18, 19, 20, 21, 23, 24][(n - 5) as usize];
            tilde_key(&mut out, number, mods);
        }
        Key::F(_) => {}
    }
    out
}

/// Pasted text, wrapped in bracketed-paste markers when the program asked
/// for them. Newlines become carriage returns, as terminals send them.
pub fn encode_paste(text: &str, modes: &Modes) -> Vec<u8> {
    let normalized = text.replace("\r\n", "\r").replace('\n', "\r");
    let mut out = Vec::with_capacity(normalized.len() + 12);
    if modes.bracketed_paste {
        out.extend_from_slice(b"\x1b[200~");
        // A paste must not be able to end the bracket early.
        out.extend_from_slice(normalized.replace("\x1b[201~", "").as_bytes());
        out.extend_from_slice(b"\x1b[201~");
    } else {
        out.extend_from_slice(normalized.as_bytes());
    }
    out
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseButton {
    Left,
    Middle,
    Right,
    WheelUp,
    WheelDown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseAction {
    Press(MouseButton),
    Release(MouseButton),
    /// Motion with a button held.
    Drag(MouseButton),
    /// Motion with no button held.
    Move,
}

/// Encode a mouse event at zero-based `(col, row)` within the terminal, or
/// `None` when the program hasn't asked for this kind of event.
pub fn encode_mouse(
    action: MouseAction,
    col: u16,
    row: u16,
    mods: Modifiers,
    modes: &Modes,
) -> Option<Vec<u8>> {
    let wanted = match (modes.mouse_mode, action) {
        (MouseProtocolMode::None, _) => false,
        (MouseProtocolMode::Press, MouseAction::Press(_)) => true,
        (MouseProtocolMode::Press, _) => false,
        (MouseProtocolMode::PressRelease, MouseAction::Press(_) | MouseAction::Release(_)) => true,
        (MouseProtocolMode::PressRelease, _) => false,
        (MouseProtocolMode::ButtonMotion, MouseAction::Move) => false,
        (MouseProtocolMode::ButtonMotion, _) => true,
        (MouseProtocolMode::AnyMotion, _) => true,
    };
    if !wanted {
        return None;
    }
    let button_code = |button: MouseButton| -> u16 {
        match button {
            MouseButton::Left => 0,
            MouseButton::Middle => 1,
            MouseButton::Right => 2,
            MouseButton::WheelUp => 64,
            MouseButton::WheelDown => 65,
        }
    };
    let sgr = modes.mouse_encoding == MouseProtocolEncoding::Sgr;
    let (mut code, release) = match action {
        MouseAction::Press(b) => (button_code(b), false),
        // Legacy encodings can't say which button was released.
        MouseAction::Release(b) => (if sgr { button_code(b) } else { 3 }, true),
        MouseAction::Drag(b) => (button_code(b) + 32, false),
        MouseAction::Move => (3 + 32, false),
    };
    // X10 (press-only) mode doesn't report modifiers.
    if modes.mouse_mode != MouseProtocolMode::Press {
        code += 4 * mods.shift as u16 + 8 * mods.alt as u16 + 16 * mods.ctrl as u16;
    }
    let (x, y) = (col + 1, row + 1);
    if sgr {
        let suffix = if release { 'm' } else { 'M' };
        return Some(format!("\x1b[<{code};{x};{y}{suffix}").into_bytes());
    }
    let mut out = b"\x1b[M".to_vec();
    let mut push = |value: u16| -> bool {
        let value = value + 32;
        match modes.mouse_encoding {
            MouseProtocolEncoding::Utf8 => match char::from_u32(value as u32) {
                Some(c) if value < 2048 => {
                    let mut buf = [0u8; 4];
                    out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
                    true
                }
                _ => false,
            },
            _ => {
                if value > 255 {
                    return false;
                }
                out.push(value as u8);
                true
            }
        }
    };
    if push(code) && push(x) && push(y) {
        Some(out)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Emulator;

    fn modes_after(setup: &[u8]) -> Modes {
        let mut emu = Emulator::passive(24, 80, 0);
        emu.feed(setup);
        emu.modes()
    }

    #[test]
    fn arrows_follow_application_cursor_mode() {
        let normal = modes_after(b"");
        let app = modes_after(b"\x1b[?1h");
        assert_eq!(encode_key(Key::Up, Modifiers::NONE, &normal), b"\x1b[A");
        assert_eq!(encode_key(Key::Up, Modifiers::NONE, &app), b"\x1bOA");
        let ctrl = Modifiers {
            ctrl: true,
            ..Modifiers::NONE
        };
        assert_eq!(encode_key(Key::Right, ctrl, &app), b"\x1b[1;5C");
    }

    #[test]
    fn control_and_alt_characters() {
        let modes = modes_after(b"");
        let ctrl = Modifiers {
            ctrl: true,
            ..Modifiers::NONE
        };
        let alt = Modifiers {
            alt: true,
            ..Modifiers::NONE
        };
        assert_eq!(encode_key(Key::Char('c'), ctrl, &modes), [0x03]);
        assert_eq!(encode_key(Key::Char('b'), ctrl, &modes), [0x02]);
        assert_eq!(encode_key(Key::Char('x'), alt, &modes), b"\x1bx");
        assert_eq!(
            encode_key(Key::Char('é'), Modifiers::NONE, &modes),
            "é".as_bytes()
        );
        assert_eq!(encode_key(Key::F(5), Modifiers::NONE, &modes), b"\x1b[15~");
        assert_eq!(encode_key(Key::F(1), Modifiers::NONE, &modes), b"\x1bOP");
        assert_eq!(encode_key(Key::Delete, Modifiers::NONE, &modes), b"\x1b[3~");
    }

    #[test]
    fn paste_is_bracketed_only_when_requested() {
        let plain = modes_after(b"");
        let bracketed = modes_after(b"\x1b[?2004h");
        assert_eq!(encode_paste("a\nb", &plain), b"a\rb");
        assert_eq!(encode_paste("a\nb", &bracketed), b"\x1b[200~a\rb\x1b[201~");
        assert_eq!(
            encode_paste("x\x1b[201~y", &bracketed),
            b"\x1b[200~xy\x1b[201~"
        );
    }

    #[test]
    fn mouse_reports_follow_mode_and_encoding() {
        let none = modes_after(b"");
        let press = MouseAction::Press(MouseButton::Left);
        assert_eq!(encode_mouse(press, 0, 0, Modifiers::NONE, &none), None);

        let sgr = modes_after(b"\x1b[?1000h\x1b[?1006h");
        assert_eq!(
            encode_mouse(press, 4, 9, Modifiers::NONE, &sgr).unwrap(),
            b"\x1b[<0;5;10M"
        );
        assert_eq!(
            encode_mouse(
                MouseAction::Release(MouseButton::Left),
                4,
                9,
                Modifiers::NONE,
                &sgr
            )
            .unwrap(),
            b"\x1b[<0;5;10m"
        );
        // Drag isn't reported in press/release mode.
        assert_eq!(
            encode_mouse(
                MouseAction::Drag(MouseButton::Left),
                1,
                1,
                Modifiers::NONE,
                &sgr
            ),
            None
        );

        let legacy = modes_after(b"\x1b[?1002h");
        assert_eq!(
            encode_mouse(
                MouseAction::Release(MouseButton::Right),
                0,
                0,
                Modifiers::NONE,
                &legacy
            )
            .unwrap(),
            vec![0x1b, b'[', b'M', 32 + 3, 33, 33]
        );
        assert_eq!(
            encode_mouse(
                MouseAction::Press(MouseButton::WheelUp),
                2,
                3,
                Modifiers::NONE,
                &legacy
            )
            .unwrap(),
            vec![0x1b, b'[', b'M', 32 + 64, 35, 36]
        );
    }
}
