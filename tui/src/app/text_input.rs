//! Single-line text field shared by every dialog: insertion, deletion,
//! cursor and word movement with the usual readline keys. Unicode-safe: the
//! cursor always sits on a char boundary.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TextInput {
    value: String,
    /// Byte index into `value`, always on a char boundary.
    cursor: usize,
}

impl TextInput {
    /// A field holding `value` with the cursor at the end.
    pub fn new(value: impl Into<String>) -> Self {
        let value = value.into();
        let cursor = value.len();
        Self { value, cursor }
    }

    pub fn value(&self) -> &str {
        &self.value
    }

    pub fn is_empty(&self) -> bool {
        self.value.is_empty()
    }

    /// Cursor position in chars from the start (for drawing).
    pub fn cursor_column(&self) -> usize {
        self.value[..self.cursor].chars().count()
    }

    /// Replace the content; the cursor moves to the end.
    pub fn set(&mut self, value: impl Into<String>) {
        *self = Self::new(value);
    }

    pub fn insert_str(&mut self, text: &str) {
        let clean: String = text.chars().filter(|c| !c.is_control()).collect();
        self.value.insert_str(self.cursor, &clean);
        self.cursor += clean.len();
    }

    /// Apply an editing key. Returns `true` when the key was an editing key
    /// (whether or not it changed anything), `false` for keys the caller
    /// should handle (Enter, Esc, Tab, arrows up/down, …).
    pub fn handle_key(&mut self, key: &KeyEvent) -> bool {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        match key.code {
            KeyCode::Char('a') if ctrl => self.cursor = 0,
            KeyCode::Char('e') if ctrl => self.cursor = self.value.len(),
            KeyCode::Char('k') if ctrl => self.value.truncate(self.cursor),
            KeyCode::Char('u') if ctrl => {
                self.value.replace_range(..self.cursor, "");
                self.cursor = 0;
            }
            KeyCode::Char('w') if ctrl => self.delete_word_left(),
            KeyCode::Char('b') if alt => self.cursor = self.word_left(),
            KeyCode::Char('f') if alt => self.cursor = self.word_right(),
            KeyCode::Char('d') if alt => self.delete_word_right(),
            KeyCode::Char(_) if ctrl || alt => return false,
            KeyCode::Char(c) => {
                self.value.insert(self.cursor, c);
                self.cursor += c.len_utf8();
            }
            KeyCode::Backspace if alt || ctrl => self.delete_word_left(),
            KeyCode::Backspace => {
                if let Some(prev) = self.prev_boundary() {
                    self.value.replace_range(prev..self.cursor, "");
                    self.cursor = prev;
                }
            }
            KeyCode::Delete if alt || ctrl => self.delete_word_right(),
            KeyCode::Delete => {
                if let Some(next) = self.next_boundary() {
                    self.value.replace_range(self.cursor..next, "");
                }
            }
            KeyCode::Left if alt || ctrl => self.cursor = self.word_left(),
            KeyCode::Right if alt || ctrl => self.cursor = self.word_right(),
            KeyCode::Left => {
                if let Some(prev) = self.prev_boundary() {
                    self.cursor = prev;
                }
            }
            KeyCode::Right => {
                if let Some(next) = self.next_boundary() {
                    self.cursor = next;
                }
            }
            KeyCode::Home => self.cursor = 0,
            KeyCode::End => self.cursor = self.value.len(),
            _ => return false,
        }
        true
    }

    fn prev_boundary(&self) -> Option<usize> {
        self.value[..self.cursor]
            .char_indices()
            .next_back()
            .map(|(i, _)| i)
    }

    fn next_boundary(&self) -> Option<usize> {
        self.value[self.cursor..]
            .chars()
            .next()
            .map(|c| self.cursor + c.len_utf8())
    }

    fn word_left(&self) -> usize {
        let before: Vec<(usize, char)> = self.value[..self.cursor].char_indices().collect();
        let mut idx = before.len();
        while idx > 0 && !is_word_char(before[idx - 1].1) {
            idx -= 1;
        }
        while idx > 0 && is_word_char(before[idx - 1].1) {
            idx -= 1;
        }
        before.get(idx).map(|(i, _)| *i).unwrap_or(self.cursor)
    }

    fn word_right(&self) -> usize {
        let mut chars = self.value[self.cursor..].char_indices().peekable();
        let mut end = self.cursor;
        while let Some((i, c)) = chars.peek().copied() {
            if is_word_char(c) {
                break;
            }
            end = self.cursor + i + c.len_utf8();
            chars.next();
        }
        while let Some((i, c)) = chars.peek().copied() {
            if !is_word_char(c) {
                break;
            }
            end = self.cursor + i + c.len_utf8();
            chars.next();
        }
        end
    }

    fn delete_word_left(&mut self) {
        let start = self.word_left();
        self.value.replace_range(start..self.cursor, "");
        self.cursor = start;
    }

    fn delete_word_right(&mut self) {
        let end = self.word_right();
        self.value.replace_range(self.cursor..end, "");
    }
}

fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn mod_key(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, modifiers)
    }

    #[test]
    fn typing_and_deleting_multibyte_text_keeps_the_cursor_on_char_boundaries() {
        let mut input = TextInput::default();
        for c in "héllo".chars() {
            assert!(input.handle_key(&key(KeyCode::Char(c))));
        }
        assert_eq!(input.value(), "héllo");
        input.handle_key(&key(KeyCode::Left));
        input.handle_key(&key(KeyCode::Left));
        input.handle_key(&key(KeyCode::Left));
        input.handle_key(&key(KeyCode::Backspace));
        assert_eq!(input.value(), "hllo");
        assert_eq!(input.cursor_column(), 1);
        input.handle_key(&key(KeyCode::Delete));
        assert_eq!(input.value(), "hlo");
    }

    #[test]
    fn word_movement_and_kill_keys() {
        let mut input = TextInput::new("cargo test --all");
        input.handle_key(&mod_key(KeyCode::Char('w'), KeyModifiers::CONTROL));
        assert_eq!(input.value(), "cargo test --");
        input.handle_key(&mod_key(KeyCode::Left, KeyModifiers::ALT));
        assert_eq!(input.cursor_column(), 6);
        input.handle_key(&mod_key(KeyCode::Char('k'), KeyModifiers::CONTROL));
        assert_eq!(input.value(), "cargo ");
        input.handle_key(&mod_key(KeyCode::Char('u'), KeyModifiers::CONTROL));
        assert_eq!(input.value(), "");
    }

    #[test]
    fn non_editing_keys_are_left_to_the_caller() {
        let mut input = TextInput::new("x");
        assert!(!input.handle_key(&key(KeyCode::Enter)));
        assert!(!input.handle_key(&key(KeyCode::Esc)));
        assert!(!input.handle_key(&key(KeyCode::Tab)));
        assert!(!input.handle_key(&mod_key(KeyCode::Char('b'), KeyModifiers::CONTROL)));
        assert_eq!(input.value(), "x");
    }

    #[test]
    fn pasted_text_drops_control_characters() {
        let mut input = TextInput::default();
        input.insert_str("a\nb\tc");
        assert_eq!(input.value(), "abc");
    }
}
