//! Text field shared by every dialog: insertion, deletion, cursor and word
//! movement with the usual readline keys. One line, or several
//! ([`TextInput::multiline`]), word-wrapped to the columns it's drawn in.
//! Unicode-safe: the cursor always sits on a char boundary.

use std::ops::Range;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TextInput {
    value: String,
    /// Byte index into `value`, always on a char boundary.
    cursor: usize,
    /// Keeps the newlines typed ([`Self::newline`]) and pasted.
    multiline: bool,
}

impl TextInput {
    /// A field holding `value` with the cursor at the end.
    pub fn new(value: impl Into<String>) -> Self {
        let value = value.into();
        let cursor = value.len();
        Self {
            value,
            cursor,
            multiline: false,
        }
    }

    /// An empty field of several lines.
    pub fn multiline() -> Self {
        Self {
            multiline: true,
            ..Self::default()
        }
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

    /// The cursor's place in [`Self::value`].
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// Put `text` in place of `range`; the cursor moves after it.
    pub fn replace(&mut self, range: Range<usize>, text: &str) {
        self.value.replace_range(range.clone(), text);
        self.cursor = range.start + text.len();
    }

    /// Replace the content; the cursor moves to the end.
    pub fn set(&mut self, value: impl Into<String>) {
        *self = Self {
            multiline: self.multiline,
            ..Self::new(value)
        };
    }

    pub fn insert_str(&mut self, text: &str) {
        let clean: String = match self.multiline {
            true => text
                .replace("\r\n", "\n")
                .replace('\r', "\n")
                .replace('\t', "    "),
            false => text.to_owned(),
        }
        .chars()
        .filter(|c| !c.is_control() || (self.multiline && *c == '\n'))
        .collect();
        self.value.insert_str(self.cursor, &clean);
        self.cursor += clean.len();
    }

    /// A new line at the cursor, in a field of several.
    pub fn newline(&mut self) {
        if self.multiline {
            self.insert_str("\n");
        }
    }

    /// The rows the text takes word-wrapped to `cols` columns (at least
    /// one), as ranges of it, newlines left out.
    pub fn rows(&self, cols: usize) -> Vec<Range<usize>> {
        let cols = cols.max(1);
        let mut rows = Vec::new();
        let mut line_start = 0;
        for line in self.value.split('\n') {
            let mut start = line_start;
            // Where the row may break: just after its last space.
            let mut space = None;
            let mut used = 0;
            for (at, c) in line.char_indices() {
                let at = line_start + at;
                let width = char_width(c);
                if used + width > cols && at > start {
                    // A space that doesn't fit hangs off the row's end.
                    if c == ' ' {
                        space = Some(at + 1);
                        continue;
                    }
                    let end = space.take().unwrap_or(at);
                    rows.push(start..end);
                    start = end;
                    used = self.value[start..at].chars().map(char_width).sum();
                }
                used += width;
                if c == ' ' {
                    space = Some(at + 1);
                }
            }
            let end = line_start + line.len();
            rows.push(start..end);
            line_start = end + 1;
        }
        rows
    }

    /// The cursor's row of [`Self::rows`] and its column there.
    pub fn cursor_at(&self, cols: usize) -> (usize, usize) {
        let rows = self.rows(cols);
        let row = rows
            .iter()
            .rposition(|row| row.start <= self.cursor)
            .unwrap_or(0);
        let column = self.value[rows[row].start..self.cursor]
            .chars()
            .map(char_width)
            .sum::<usize>();
        (row, column.min(cols.saturating_sub(1)))
    }

    /// Move the cursor `delta` rows (wrapped to `cols`), keeping its column;
    /// `false` when there's no such row.
    pub fn move_rows(&mut self, delta: isize, cols: usize) -> bool {
        let rows = self.rows(cols);
        let (row, column) = self.cursor_at(cols);
        let Some(target) = row
            .checked_add_signed(delta)
            .and_then(|row| rows.get(row))
            .cloned()
        else {
            return false;
        };
        let mut used = 0;
        let mut cursor = target.start;
        for (at, c) in self.value[target.clone()].char_indices() {
            if used >= column {
                break;
            }
            used += char_width(c);
            cursor = target.start + at + c.len_utf8();
        }
        // The end of a wrapped row is the next one's start: stay on it.
        if cursor == target.end
            && rows
                .iter()
                .any(|row| row.start == cursor && !row.is_empty())
        {
            cursor = self.value[..cursor]
                .char_indices()
                .next_back()
                .map_or(cursor, |(at, _)| at.max(target.start));
        }
        self.cursor = cursor;
        true
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

fn char_width(c: char) -> usize {
    crate::layout::str_width(c.encode_utf8(&mut [0; 4]))
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
    #[test]
    fn a_multiline_field_keeps_newlines_wraps_words_and_moves_by_row() {
        let mut input = TextInput::multiline();
        input.insert_str("run the tests\r\nthen\tpush");
        assert_eq!(input.value(), "run the tests\nthen    push");
        input.set("run the tests\nthen push");
        let text = |input: &TextInput, cols| -> Vec<String> {
            input
                .rows(cols)
                .into_iter()
                .map(|row| input.value()[row].to_string())
                .collect()
        };
        assert_eq!(text(&input, 8), ["run the ", "tests", "then ", "push"]);
        assert_eq!(text(&input, 80), ["run the tests", "then push"]);
        // Longer than the columns: cut where it must be.
        let long = TextInput::new("abcdefgh");
        assert_eq!(text(&long, 3), ["abc", "def", "gh"]);
        assert_eq!(text(&TextInput::default(), 3), [""]);
        assert_eq!(input.cursor_at(8), (3, 4));
        assert!(input.move_rows(-1, 8));
        assert_eq!(input.cursor_at(8), (2, 4));
        assert!(input.move_rows(-1, 8));
        assert_eq!(input.cursor_at(8), (1, 4));
        assert!(input.move_rows(-1, 8));
        assert_eq!(input.cursor_at(8), (0, 4));
        assert!(!input.move_rows(-1, 8));
        input.newline();
        assert_eq!(input.value(), "run \nthe tests\nthen push");
        // One line: a newline is nothing, as ever.
        let mut single = TextInput::new("x");
        single.newline();
        assert_eq!(single.value(), "x");
    }
}
