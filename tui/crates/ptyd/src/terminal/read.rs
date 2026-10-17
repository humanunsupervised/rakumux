//! Screen and history reads (`TerminalHost::read`) and saved scrollback.

use session_core::api::{ReadResult, ReadSource, TextFormat};
use vt::{Emulator, Row};

/// Read the screen or recent history.
///
/// - `Visible`: the screen rows.
/// - `Recent`: retained history followed by the screen.
/// - `RecentUnwrapped`: like `Recent`, with soft-wrapped rows joined into
///   logical lines.
///
/// Trailing blank rows (below the last output) are omitted. `lines` keeps
/// the last N rows (logical lines for `RecentUnwrapped`); `truncated` says
/// rows were left out because of it. `Text` joins rows with `\n`; `Ansi`
/// returns SGR-formatted rows joined with `\r\n`.
pub(crate) fn read_screen(
    emulator: &mut Emulator,
    source: ReadSource,
    lines: Option<u32>,
    format: TextFormat,
) -> ReadResult {
    let (rows, cols) = emulator.size();
    let cursor = emulator.screen().cursor_position();
    let total = emulator.total_rows();
    let (start, count) = match source {
        ReadSource::Visible => (total - rows as usize, rows as usize),
        ReadSource::Recent | ReadSource::RecentUnwrapped => (0, total),
    };
    let mut collected: Vec<Row<String>> = match format {
        TextFormat::Text => emulator.history_lines(start, count),
        TextFormat::Ansi => emulator
            .history_rows_formatted(start, count)
            .into_iter()
            .map(|row| Row {
                content: String::from_utf8_lossy(&row.content).into_owned(),
                wrapped: row.wrapped,
            })
            .collect(),
    };
    trim_trailing_blank(&mut collected);
    let mut lines_out: Vec<String> = if source == ReadSource::RecentUnwrapped {
        join_wrapped(collected)
    } else {
        collected.into_iter().map(|row| row.content).collect()
    };
    let mut truncated = false;
    if let Some(limit) = lines {
        let limit = limit as usize;
        if lines_out.len() > limit {
            lines_out.drain(..lines_out.len() - limit);
            truncated = true;
        }
    }
    let separator = match format {
        TextFormat::Text => "\n",
        TextFormat::Ansi => "\r\n",
    };
    ReadResult {
        text: lines_out.join(separator),
        rows,
        cols,
        cursor,
        truncated,
        untrusted: true,
    }
}

/// SGR-formatted history and screen, rows joined with `\r\n`, oldest first,
/// trailing blank rows omitted: the `persist::ScrollbackStore` format.
pub(crate) fn formatted_scrollback(emulator: &mut Emulator) -> Vec<u8> {
    let total = emulator.total_rows();
    let mut rows = emulator.history_rows_formatted(0, total);
    while rows
        .last()
        .is_some_and(|row| row.content.is_empty() && !row.wrapped)
    {
        rows.pop();
    }
    rows.into_iter()
        .map(|row| row.content)
        .collect::<Vec<_>>()
        .join(&b"\r\n"[..])
}

fn trim_trailing_blank(rows: &mut Vec<Row<String>>) {
    while rows
        .last()
        .is_some_and(|row| row.content.is_empty() && !row.wrapped)
    {
        rows.pop();
    }
}

fn join_wrapped(rows: Vec<Row<String>>) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut open = false;
    for row in rows {
        current.push_str(&row.content);
        open = row.wrapped;
        if !row.wrapped {
            out.push(std::mem::take(&mut current));
        }
    }
    if open {
        out.push(current);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn emulator(rows: u16, cols: u16, feed: &[u8]) -> Emulator {
        let mut emu = Emulator::passive(rows, cols, 100);
        emu.feed(feed);
        emu
    }

    #[test]
    fn visible_reads_trim_blank_rows_and_report_the_cursor() {
        let mut emu = emulator(5, 20, b"one\r\ntwo\r\n$ ");
        let read = read_screen(&mut emu, ReadSource::Visible, None, TextFormat::Text);
        assert_eq!(read.text, "one\ntwo\n$");
        assert_eq!((read.rows, read.cols), (5, 20));
        assert_eq!(read.cursor, (2, 2));
        assert!(!read.truncated);
        assert!(read.untrusted);
    }

    #[test]
    fn recent_reads_include_history_and_honour_line_limits() {
        let mut feed = Vec::new();
        for i in 0..10 {
            feed.extend_from_slice(format!("line{i}\r\n").as_bytes());
        }
        let mut emu = emulator(3, 20, &feed);
        let all = read_screen(&mut emu, ReadSource::Recent, None, TextFormat::Text);
        assert_eq!(all.text.lines().count(), 10);
        assert!(all.text.starts_with("line0\n"));
        let last = read_screen(&mut emu, ReadSource::Recent, Some(3), TextFormat::Text);
        assert_eq!(last.text, "line7\nline8\nline9");
        assert!(last.truncated);
        let visible = read_screen(&mut emu, ReadSource::Visible, None, TextFormat::Text);
        assert_eq!(visible.text, "line8\nline9");
    }

    #[test]
    fn unwrapped_reads_join_soft_wrapped_rows() {
        let mut emu = emulator(4, 6, b"hello world!\r\nok");
        let wrapped = read_screen(&mut emu, ReadSource::Recent, None, TextFormat::Text);
        assert_eq!(wrapped.text, "hello \nworld!\nok");
        let joined = read_screen(
            &mut emu,
            ReadSource::RecentUnwrapped,
            None,
            TextFormat::Text,
        );
        assert_eq!(joined.text, "hello world!\nok");
        let last = read_screen(
            &mut emu,
            ReadSource::RecentUnwrapped,
            Some(1),
            TextFormat::Text,
        );
        assert_eq!(last.text, "ok");
    }

    #[test]
    fn ansi_reads_and_saved_scrollback_keep_colours() {
        let mut emu = emulator(3, 20, b"\x1b[32mgreen\x1b[m\r\nplain\r\n");
        let read = read_screen(&mut emu, ReadSource::Visible, None, TextFormat::Ansi);
        assert_eq!(read.text, "\x1b[0;32mgreen\x1b[m\r\nplain");
        assert_eq!(
            formatted_scrollback(&mut emu),
            b"\x1b[0;32mgreen\x1b[m\r\nplain".to_vec()
        );
    }
}
