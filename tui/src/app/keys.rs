//! Configurable key bindings: the Orchestra key (default `Ctrl-\\`), the
//! one key a session never receives, which switches from session mode to
//! Orchestra mode; and the session cycling keys (default `Alt-↑`/`Alt-↓`),
//! which work in Orchestra mode and, when the user turns it on, in session
//! mode too. Everything else typed in session mode goes to the session.

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

/// Default Orchestra key, as written in `config.json`
/// (`sessions.orchestra_key`).
pub const DEFAULT_ORCHESTRA_KEY: &str = "ctrl+\\";
/// The in-session prefix before the two modes (`sessions.prefix_key`); a
/// config still naming it gets the new default Orchestra key.
pub const OLD_DEFAULT_PREFIX: &str = "ctrl+b";
/// Default cycling keys (`sessions.cycle_keys`).
pub const DEFAULT_CYCLE_PREVIOUS: &str = "alt+up";
pub const DEFAULT_CYCLE_NEXT: &str = "alt+down";

/// A single key with modifiers, e.g. the prefix.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyBinding {
    pub code: KeyCode,
    pub modifiers: KeyModifiers,
}

impl KeyBinding {
    /// Parse `"ctrl+\\"`, `"ctrl+a"`, `"alt+space"`, `"ctrl+]"`, `"alt+up"`, …
    /// Modifier names: `ctrl`/`control`, `alt`/`meta`/`option`, `shift`. The
    /// key is a single character or a name (`space`, `up`, `down`, `left`,
    /// `right`, `pageup`, `pagedown`, `home`, `end`, `tab`, `f1`–`f12`). A
    /// modifier is required so a binding never clashes with typing.
    pub fn parse(spec: &str) -> Result<Self, String> {
        let spec = spec.trim();
        if spec.is_empty() {
            return Err("empty key binding".into());
        }
        let parts: Vec<&str> = spec.split('+').collect();
        // "ctrl++" would split into ["ctrl", "", ""]; treat a trailing empty
        // pair as the '+' key.
        let (mods, key) = if spec.ends_with("++") {
            (&parts[..parts.len() - 2], "+")
        } else {
            let (key, mods) = parts.split_last().ok_or("empty key binding")?;
            (mods, *key)
        };
        let mut modifiers = KeyModifiers::NONE;
        for m in mods {
            match m.trim().to_ascii_lowercase().as_str() {
                "ctrl" | "control" | "c" => modifiers |= KeyModifiers::CONTROL,
                "alt" | "meta" | "option" | "opt" | "m" => modifiers |= KeyModifiers::ALT,
                "shift" | "s" => modifiers |= KeyModifiers::SHIFT,
                other => return Err(format!("unknown modifier '{other}' in '{spec}'")),
            }
        }
        let key = key.trim();
        let code = match named_key(key) {
            Some(code) => code,
            None => {
                let mut chars = key.chars();
                match (chars.next(), chars.next()) {
                    (Some(c), None) => KeyCode::Char(c.to_ascii_lowercase()),
                    _ => return Err(format!("'{key}' is not a key name in '{spec}'")),
                }
            }
        };
        if modifiers.is_empty() {
            return Err(format!(
                "'{spec}' needs a modifier (e.g. ctrl+b) so it can't clash with typing"
            ));
        }
        Ok(Self { code, modifiers })
    }

    /// Whether `key` is this binding. Letters compare case-insensitively
    /// because terminals differ in how they report Ctrl+letter. Ctrl with
    /// `\\ ] ^ _` also matches Ctrl with `4 5 6 7`: most terminals send
    /// those as the same control bytes, which crossterm reports as digits.
    pub fn matches(&self, key: &KeyEvent) -> bool {
        let relevant = KeyModifiers::CONTROL | KeyModifiers::ALT;
        if key.modifiers & relevant != self.modifiers & relevant {
            return false;
        }
        match (self.code, key.code) {
            (KeyCode::Char(a), KeyCode::Char(b)) => {
                a.eq_ignore_ascii_case(&b)
                    || (self.modifiers.contains(KeyModifiers::CONTROL)
                        && legacy_control_digit(a) == Some(b))
            }
            (a, b) => a == b,
        }
    }

    /// The key event this binding stands for (to send it literally).
    pub fn to_key_event(self) -> KeyEvent {
        KeyEvent::new(self.code, self.modifiers)
    }

    /// Human label, e.g. `Ctrl-b`.
    pub fn label(&self) -> String {
        let mut out = String::new();
        if self.modifiers.contains(KeyModifiers::CONTROL) {
            out.push_str("Ctrl-");
        }
        if self.modifiers.contains(KeyModifiers::ALT) {
            out.push_str("Alt-");
        }
        if self.modifiers.contains(KeyModifiers::SHIFT) {
            out.push_str("Shift-");
        }
        match self.code {
            KeyCode::Char(' ') => out.push_str("Space"),
            KeyCode::Char(c) => out.push(c),
            KeyCode::Up => out.push('↑'),
            KeyCode::Down => out.push('↓'),
            KeyCode::Left => out.push('←'),
            KeyCode::Right => out.push('→'),
            KeyCode::F(n) => out.push_str(&format!("F{n}")),
            other => out.push_str(&format!("{other:?}")),
        }
        out
    }
}

/// The digit a terminal's control byte for Ctrl+`c` reads as (0x1C-0x1F
/// are reported as Ctrl+4..7).
fn legacy_control_digit(c: char) -> Option<char> {
    match c {
        '\\' => Some('4'),
        ']' => Some('5'),
        '^' => Some('6'),
        '_' | '/' => Some('7'),
        _ => None,
    }
}

fn named_key(name: &str) -> Option<KeyCode> {
    let lower = name.to_ascii_lowercase();
    let code = match lower.as_str() {
        "space" => KeyCode::Char(' '),
        "up" => KeyCode::Up,
        "down" => KeyCode::Down,
        "left" => KeyCode::Left,
        "right" => KeyCode::Right,
        "pageup" | "pgup" => KeyCode::PageUp,
        "pagedown" | "pgdn" => KeyCode::PageDown,
        "home" => KeyCode::Home,
        "end" => KeyCode::End,
        "tab" => KeyCode::Tab,
        "backslash" => KeyCode::Char('\\'),
        _ => {
            let n: u8 = lower.strip_prefix('f')?.parse().ok()?;
            if !(1..=12).contains(&n) {
                return None;
            }
            KeyCode::F(n)
        }
    };
    Some(code)
}

/// Keys that select the previous/next session from any focus state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CycleKeys {
    pub previous: KeyBinding,
    pub next: KeyBinding,
}

impl Default for CycleKeys {
    fn default() -> Self {
        Self {
            previous: KeyBinding {
                code: KeyCode::Up,
                modifiers: KeyModifiers::ALT,
            },
            next: KeyBinding {
                code: KeyCode::Down,
                modifiers: KeyModifiers::ALT,
            },
        }
    }
}

impl CycleKeys {
    /// `-1` for previous, `1` for next, `None` for any other key.
    pub fn direction(&self, key: &KeyEvent) -> Option<i32> {
        if key.kind == KeyEventKind::Release {
            return None;
        }
        if self.previous.matches(key) {
            Some(-1)
        } else if self.next.matches(key) {
            Some(1)
        } else {
            None
        }
    }

    /// Human label, e.g. `Alt-↑/Alt-↓`.
    pub fn label(&self) -> String {
        format!("{}/{}", self.previous.label(), self.next.label())
    }

    /// Shorter label, as the Settings modal shows it: `Alt-↑/↓` when both
    /// keys share their modifiers, otherwise [`CycleKeys::label`].
    pub fn short_label(&self) -> String {
        let previous = self.previous.label();
        let next = self.next.label();
        if self.previous.modifiers == self.next.modifiers {
            if let (Some((mods, a)), Some((_, b))) =
                (previous.rsplit_once('-'), next.rsplit_once('-'))
            {
                if !a.is_empty() && !b.is_empty() {
                    return format!("{mods}-{a}/{b}");
                }
            }
        }
        self.label()
    }
}

/// The default Orchestra key, `Ctrl-\\`.
impl Default for KeyBinding {
    fn default() -> Self {
        Self {
            code: KeyCode::Char('\\'),
            modifiers: KeyModifiers::CONTROL,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctrl(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    fn plain(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
    }

    #[test]
    fn the_orchestra_key_defaults_to_ctrl_backslash_and_matches_its_legacy_byte() {
        let key = KeyBinding::parse(DEFAULT_ORCHESTRA_KEY).unwrap();
        assert_eq!(key, KeyBinding::default());
        assert_eq!(key.label(), "Ctrl-\\");
        assert_eq!(KeyBinding::parse("ctrl+backslash").unwrap(), key);
        // Kitty-protocol terminals report the key itself; the others send
        // 0x1C, which crossterm reads as Ctrl-4.
        assert!(key.matches(&ctrl('\\')));
        assert!(key.matches(&ctrl('4')));
        assert!(!key.matches(&plain('4')));
        assert!(!key.matches(&plain('\\')));
        assert!(!key.matches(&ctrl('b')));
        // The same holds for Ctrl-] and Ctrl-_.
        assert!(KeyBinding::parse("ctrl+]").unwrap().matches(&ctrl('5')));
        assert!(KeyBinding::parse("ctrl+_").unwrap().matches(&ctrl('7')));
        // Only for Ctrl: Alt-\ is not Alt-4.
        assert!(!KeyBinding::parse("alt+\\")
            .unwrap()
            .matches(&KeyEvent::new(KeyCode::Char('4'), KeyModifiers::ALT)));
    }

    #[test]
    fn parses_key_specs() {
        assert_eq!(
            KeyBinding::parse("ctrl+b").unwrap(),
            KeyBinding {
                code: KeyCode::Char('b'),
                modifiers: KeyModifiers::CONTROL
            }
        );
        let a = KeyBinding::parse("Ctrl+A").unwrap();
        assert_eq!(a.code, KeyCode::Char('a'));
        assert_eq!(a.label(), "Ctrl-a");
        let space = KeyBinding::parse("alt+space").unwrap();
        assert_eq!(space.code, KeyCode::Char(' '));
        assert_eq!(space.modifiers, KeyModifiers::ALT);
        assert!(KeyBinding::parse("b").is_err());
        assert!(KeyBinding::parse("hyper+b").is_err());
        assert!(KeyBinding::parse("ctrl+bb").is_err());
        assert!(KeyBinding::parse("").is_err());
    }

    #[test]
    fn bindings_match_case_insensitively_and_ignore_shift() {
        let prefix = KeyBinding::parse("ctrl+b").unwrap();
        assert!(prefix.matches(&ctrl('b')));
        assert!(prefix.matches(&KeyEvent::new(
            KeyCode::Char('B'),
            KeyModifiers::CONTROL | KeyModifiers::SHIFT
        )));
        assert!(!prefix.matches(&plain('b')));
        assert!(!prefix.matches(&ctrl('a')));
    }

    #[test]
    fn parses_named_keys_and_cycle_defaults() {
        let up = KeyBinding::parse(DEFAULT_CYCLE_PREVIOUS).unwrap();
        let down = KeyBinding::parse(DEFAULT_CYCLE_NEXT).unwrap();
        let cycle = CycleKeys::default();
        assert_eq!(cycle.previous, up);
        assert_eq!(cycle.next, down);
        assert_eq!(cycle.label(), "Alt-↑/Alt-↓");
        assert_eq!(cycle.short_label(), "Alt-↑/↓");
        let mixed = CycleKeys {
            previous: KeyBinding::parse("ctrl+up").unwrap(),
            next: down,
        };
        assert_eq!(mixed.short_label(), "Ctrl-↑/Alt-↓");
        assert_eq!(
            KeyBinding::parse("option+pagedown").unwrap(),
            KeyBinding {
                code: KeyCode::PageDown,
                modifiers: KeyModifiers::ALT
            }
        );
        assert_eq!(KeyBinding::parse("ctrl+f5").unwrap().code, KeyCode::F(5));
        assert!(KeyBinding::parse("up").is_err());
        assert!(KeyBinding::parse("alt+f13").is_err());
    }

    #[test]
    fn cycle_keys_report_direction() {
        let cycle = CycleKeys::default();
        let alt_up = KeyEvent::new(KeyCode::Up, KeyModifiers::ALT);
        let alt_down = KeyEvent::new(KeyCode::Down, KeyModifiers::ALT);
        assert_eq!(cycle.direction(&alt_up), Some(-1));
        assert_eq!(cycle.direction(&alt_down), Some(1));
        assert_eq!(
            cycle.direction(&KeyEvent::new(KeyCode::Up, KeyModifiers::NONE)),
            None
        );
        assert_eq!(cycle.direction(&plain('j')), None);
    }
}
