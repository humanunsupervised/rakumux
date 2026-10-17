//! `session.read` for sessions without a terminal: the saved scrollback
//! (`persist::ScrollbackStore`) turned into a `ReadResult`.
//!
//! Saved rows are ANSI-formatted. For `Text`, each row is fed to a one-row
//! passive emulator so escape sequences are interpreted exactly as a
//! terminal would, without allocating a full-height screen for thousands of
//! rows. The saved screen size isn't known, so `Visible` returns the last
//! [`SAVED_VISIBLE_ROWS`] rows (or `lines`), and `RecentUnwrapped` equals
//! `Recent` (soft-wrap flags aren't saved).

use crate::persist::SavedSession;
use session_core::api::{ReadResult, ReadSource, TextFormat};

/// Rows returned for a `Visible` read of saved output.
pub(crate) const SAVED_VISIBLE_ROWS: usize = 24;

/// Width of the row emulator; wider than any real terminal row.
const ROW_COLS: u16 = 2048;

pub(crate) fn read(
    saved: Option<&SavedSession>,
    source: ReadSource,
    lines: Option<u32>,
    format: TextFormat,
) -> ReadResult {
    let raw: Vec<&[u8]> = match saved {
        Some(saved) => split_rows(&saved.scrollback),
        None => Vec::new(),
    };
    let plain = plain_rows(&raw);
    // Trailing blank rows are the unused bottom of the last screen.
    let end = plain
        .iter()
        .rposition(|row| !row.is_empty())
        .map_or(0, |i| i + 1);
    let want = match (source, lines) {
        (_, Some(lines)) => lines as usize,
        (ReadSource::Visible, None) => SAVED_VISIBLE_ROWS,
        (ReadSource::Recent | ReadSource::RecentUnwrapped, None) => end,
    };
    let start = end.saturating_sub(want);
    let text = match format {
        TextFormat::Text => plain[start..end].join("\n"),
        TextFormat::Ansi => raw[start..end]
            .iter()
            .map(|row| String::from_utf8_lossy(row).into_owned())
            .collect::<Vec<_>>()
            .join("\r\n"),
    };
    let rows = end - start;
    let cols = plain[start..end]
        .iter()
        .map(|row| row.chars().count())
        .max()
        .unwrap_or(0);
    let last_len = if rows > 0 {
        plain[end - 1].chars().count()
    } else {
        0
    };
    ReadResult {
        text,
        rows: clamp_u16(rows),
        cols: clamp_u16(cols),
        cursor: (clamp_u16(rows.saturating_sub(1)), clamp_u16(last_len)),
        truncated: start > 0,
        untrusted: true,
    }
}

fn split_rows(bytes: &[u8]) -> Vec<&[u8]> {
    if bytes.is_empty() {
        return Vec::new();
    }
    bytes
        .split(|b| *b == b'\n')
        .map(|row| row.strip_suffix(b"\r").unwrap_or(row))
        .collect()
}

fn plain_rows(raw: &[&[u8]]) -> Vec<String> {
    let mut emulator = vt::Emulator::passive(1, ROW_COLS, 0);
    raw.iter()
        .map(|row| {
            // Reset attributes, home the cursor and clear the row.
            emulator.feed(b"\x1b[0m\x1b[H\x1b[2K");
            emulator.feed(row);
            emulator.text().trim_end().to_string()
        })
        .collect()
}

fn clamp_u16(n: usize) -> u16 {
    u16::try_from(n).unwrap_or(u16::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::persist::SessionMeta;

    fn saved(rows: &[&str]) -> SavedSession {
        SavedSession {
            scrollback: rows.join("\r\n").into_bytes(),
            meta: SessionMeta::default(),
        }
    }

    #[test]
    fn ansi_rows_become_plain_text() {
        let s = saved(&["\x1b[1;31mred\x1b[0m plain", "\x1b[32mgreen", "", ""]);
        let r = read(Some(&s), ReadSource::Recent, None, TextFormat::Text);
        assert_eq!(r.text, "red plain\ngreen");
        assert_eq!((r.rows, r.cols), (2, 9));
        assert!(!r.truncated);
        assert!(r.untrusted);
        let ansi = read(Some(&s), ReadSource::Recent, None, TextFormat::Ansi);
        assert_eq!(ansi.text, "\x1b[1;31mred\x1b[0m plain\r\n\x1b[32mgreen");
    }

    #[test]
    fn line_limits_and_visible_window() {
        let rows: Vec<String> = (0..40).map(|i| format!("line {i}")).collect();
        let refs: Vec<&str> = rows.iter().map(String::as_str).collect();
        let s = saved(&refs);
        let visible = read(Some(&s), ReadSource::Visible, None, TextFormat::Text);
        assert_eq!(visible.rows as usize, SAVED_VISIBLE_ROWS);
        assert!(visible.text.starts_with("line 16\n"));
        assert!(visible.truncated);
        let last = read(Some(&s), ReadSource::Recent, Some(2), TextFormat::Text);
        assert_eq!(last.text, "line 38\nline 39");
    }

    #[test]
    fn nothing_saved_is_an_empty_read() {
        let r = read(None, ReadSource::Visible, None, TextFormat::Text);
        assert_eq!(r.text, "");
        assert_eq!(r.rows, 0);
        assert!(!r.truncated);
    }
}
