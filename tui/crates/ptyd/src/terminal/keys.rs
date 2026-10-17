//! Named keys for `send_keys`: parsing and encoding.
//!
//! Names are case-insensitive: `enter` (`return`), `esc` (`escape`), `tab`,
//! `backtab`, `backspace`, `space`, `up`, `down`, `left`, `right`, `home`,
//! `end`, `pageup`, `pagedown`, `insert`, `delete`, `f1`…`f12`, or a single
//! character. Modifiers prefix the key with `+`: `ctrl+c`, `alt+x`,
//! `shift+tab`, `ctrl+alt+d`.

use crate::host::HostError;
use vt::input::{encode_key, Key, Modifiers};
use vt::Modes;

/// Parse one key name.
pub(crate) fn parse_key(name: &str) -> Result<(Key, Modifiers), HostError> {
    let invalid = || HostError::InvalidKey(name.to_string());
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err(invalid());
    }
    // A lone or trailing "+" names the plus key itself ("+", "alt++").
    let (prefix, key_part) = if trimmed == "+" {
        (None, "+")
    } else {
        match trimmed.strip_suffix("++") {
            Some(rest) => (Some(rest), "+"),
            None => match trimmed.rsplit_once('+') {
                Some((rest, key)) if !key.is_empty() => (Some(rest), key),
                Some(_) => return Err(invalid()),
                None => (None, trimmed),
            },
        }
    };
    let mut mods = Modifiers::NONE;
    if let Some(prefix) = prefix {
        for part in prefix.split('+') {
            match part.to_ascii_lowercase().as_str() {
                "ctrl" | "control" | "c" => mods.ctrl = true,
                "alt" | "meta" | "option" | "opt" | "m" => mods.alt = true,
                "shift" | "s" => mods.shift = true,
                _ => return Err(invalid()),
            }
        }
    }
    let mut chars = key_part.chars();
    let key = match (chars.next(), chars.next()) {
        (Some(c), None) => Key::Char(c),
        _ => named_key(&key_part.to_ascii_lowercase()).ok_or_else(invalid)?,
    };
    if let Key::Char(c) = key {
        if c.is_control() {
            return Err(invalid());
        }
        if mods.ctrl && !has_ctrl_code(c) {
            return Err(invalid());
        }
    }
    Ok((key, mods))
}

fn named_key(name: &str) -> Option<Key> {
    Some(match name {
        "enter" | "return" | "cr" => Key::Enter,
        "esc" | "escape" => Key::Esc,
        "tab" => Key::Tab,
        "backtab" => Key::BackTab,
        "backspace" | "bs" => Key::Backspace,
        "space" => Key::Char(' '),
        "up" => Key::Up,
        "down" => Key::Down,
        "left" => Key::Left,
        "right" => Key::Right,
        "home" => Key::Home,
        "end" => Key::End,
        "pageup" | "pgup" | "page_up" => Key::PageUp,
        "pagedown" | "pgdn" | "page_down" => Key::PageDown,
        "insert" | "ins" => Key::Insert,
        "delete" | "del" => Key::Delete,
        "plus" => Key::Char('+'),
        _ => {
            let n: u8 = name.strip_prefix('f')?.parse().ok()?;
            if (1..=12).contains(&n) {
                Key::F(n)
            } else {
                return None;
            }
        }
    })
}

/// Characters `Ctrl` turns into a control code (mirrors `vt::input`).
fn has_ctrl_code(c: char) -> bool {
    c.is_ascii_alphabetic() || " @[\\]^_/?2345678".contains(c)
}

/// Literal text may contain tabs but no other control characters: escape
/// sequences and Enter must be sent as named keys.
pub(crate) fn validate_text(text: &str) -> Result<(), HostError> {
    match text.chars().find(|c| c.is_control() && *c != '\t') {
        Some(c) => Err(HostError::InvalidKey(format!(
            "text contains control character U+{:04X}; send it as a named key (e.g. \"enter\", \"esc\", \"ctrl+c\")",
            c as u32
        ))),
        None => Ok(()),
    }
}

/// Validate every key and the text, then encode them in order.
pub(crate) fn encode_keys(
    keys: &[String],
    text: Option<&str>,
    modes: &Modes,
) -> Result<Vec<u8>, HostError> {
    let parsed = keys
        .iter()
        .map(|k| parse_key(k))
        .collect::<Result<Vec<_>, _>>()?;
    if let Some(text) = text {
        validate_text(text)?;
    }
    let mut out = Vec::new();
    for (key, mods) in parsed {
        out.extend(encode_key(key, mods, modes));
    }
    if let Some(text) = text {
        out.extend_from_slice(text.as_bytes());
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn modes(setup: &[u8]) -> Modes {
        let mut emu = vt::Emulator::passive(24, 80, 0);
        emu.feed(setup);
        emu.modes()
    }

    fn keys(names: &[&str]) -> Vec<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn encodes_named_keys_with_modifiers_and_modes() {
        let normal = modes(b"");
        let app = modes(b"\x1b[?1h");
        assert_eq!(
            encode_keys(
                &keys(&["Enter", "esc", "tab", "ctrl+c", "alt+x"]),
                None,
                &normal
            )
            .unwrap(),
            b"\r\x1b\t\x03\x1bx"
        );
        assert_eq!(
            encode_keys(&keys(&["up"]), None, &normal).unwrap(),
            b"\x1b[A"
        );
        assert_eq!(encode_keys(&keys(&["up"]), None, &app).unwrap(), b"\x1bOA");
        assert_eq!(
            encode_keys(
                &keys(&["shift+tab", "pagedown", "f5", "ctrl+right"]),
                None,
                &normal
            )
            .unwrap(),
            b"\x1b[Z\x1b[6~\x1b[15~\x1b[1;5C"
        );
        assert_eq!(
            encode_keys(
                &keys(&["+", "alt++", "space", "A"]),
                Some("hi\tthere"),
                &normal
            )
            .unwrap(),
            b"+\x1b+ Ahi\tthere"
        );
    }

    #[test]
    fn rejects_unknown_keys_before_encoding_anything() {
        let normal = modes(b"");
        for bad in [
            "", "hyper+a", "f13", "ctrl+é", "ctrl++", "enterr", "ctrl+", "\x1b",
        ] {
            assert!(
                matches!(parse_key(bad), Err(HostError::InvalidKey(_))),
                "{bad:?} should be rejected"
            );
        }
        assert_eq!(
            encode_keys(&keys(&["enter", "nope"]), None, &normal),
            Err(HostError::InvalidKey("nope".into()))
        );
    }

    #[test]
    fn text_rejects_control_characters_except_tab() {
        let normal = modes(b"");
        for bad in ["a\nb", "\x1b[A", "x\ry", "\u{9b}", "del\x7f"] {
            let err = encode_keys(&[], Some(bad), &normal).unwrap_err();
            assert!(
                matches!(&err, HostError::InvalidKey(m) if m.contains("named key")),
                "{bad:?}: {err:?}"
            );
        }
        assert!(encode_keys(&[], Some("tab\there é"), &normal).is_ok());
    }
}
