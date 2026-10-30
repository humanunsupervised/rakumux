//! Text from elsewhere as a terminal should show it: folder and file names,
//! git names, what agents report, the daemon's and other machines'
//! messages. A control character in it (C0, DEL, C1: ESC, BEL, CSI, …)
//! would be acted on, not shown: an escape sequence could set the window
//! title, write the clipboard (OSC 52), make a link (OSC 8) or redraw the
//! screen. Each shows as `?`, as `ls` shows them, and a tab as a space.
//!
//! Only what's drawn or printed changes: stored data, JSON output and paths
//! used as paths keep every character.

use ratatui::buffer::Buffer;
use std::borrow::Cow;

/// What a control character shows as.
const STAND_IN: char = '?';

/// `text` without anything a terminal would act on: each control
/// character, line breaks included, as `?`, a tab as a space.
pub fn printable(text: &str) -> Cow<'_, str> {
    clean(text, false)
}

/// `text` as [`printable`], keeping its line breaks (`\n`).
pub fn printable_lines(text: &str) -> Cow<'_, str> {
    clean(text, true)
}

fn clean(text: &str, keep_lines: bool) -> Cow<'_, str> {
    let acted_on = |c: char| c.is_control() && !(keep_lines && c == '\n');
    if !text.chars().any(acted_on) {
        return Cow::Borrowed(text);
    }
    Cow::Owned(
        text.chars()
            .map(|c| match c {
                '\t' => ' ',
                c if acted_on(c) => STAND_IN,
                c => c,
            })
            .collect(),
    )
}

/// Make every cell of a frame printable before it's drawn: the one place
/// everything the TUI draws passes through. ratatui counts a control
/// character a column wide and gives it a cell of its own, so a one-column
/// stand-in keeps the layout.
pub fn clean_frame(buffer: &mut Buffer) {
    for cell in &mut buffer.content {
        let symbol = cell.symbol();
        if symbol.chars().any(char::is_control) {
            cell.set_char(if symbol == "\t" { ' ' } else { STAND_IN });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::layout::Rect;
    use ratatui::style::Style;

    #[test]
    fn controls_show_as_question_marks_and_tabs_as_spaces() {
        assert!(matches!(printable("~/src/rakumux"), Cow::Borrowed(_)));
        assert_eq!(
            printable("a\u{1b}]0;PWNED\u{7}b\u{9b}31m\u{7f}\tc\nd\r"),
            "a?]0;PWNED?b?31m? c?d?"
        );
        assert_eq!(printable_lines("one\u{1b}[2J\ntwo\r"), "one?[2J\ntwo?");
        // Everything else stays, wide and combining characters included.
        assert_eq!(printable("café ⎇ 名前 e\u{301}"), "café ⎇ 名前 e\u{301}");
    }

    #[test]
    fn a_frame_keeps_its_layout() {
        let mut buffer = Buffer::empty(Rect::new(0, 0, 12, 1));
        buffer.set_string(0, 0, "a\u{1b}]0;x\u{7}\tb\u{9b}\r\nc", Style::default());
        clean_frame(&mut buffer);
        let row: String = (0..12).map(|x| buffer.get(x, 0).symbol()).collect();
        // A carriage return and line break together are no columns wide:
        // ratatui leaves them out.
        assert_eq!(row, "a?]0;x? b?c ");
    }
}
