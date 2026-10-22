//! Terminal emulation shared by the Rakumux daemon and TUI.
//!
//! [`Emulator`] wraps a `vt100::Parser`. The daemon owns the authoritative
//! emulator for each terminal and runs it with the query responder enabled, so
//! programs get answers to terminal queries (cursor position, device
//! attributes, colours, …) even when no client is attached. Client emulators
//! are passive: they only mirror the screen, so replies are never sent twice.

use std::path::PathBuf;

pub mod input;

pub use vt100;

const VERSION_REPLY: &str = concat!("rakumux ", env!("CARGO_PKG_VERSION"));

/// Side effects of terminal output that are not part of the screen itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// OSC 0/2 window title.
    Title(String),
    /// OSC 7 working directory report.
    Cwd(PathBuf),
    /// OSC 52 clipboard write; `data` is base64 as sent by the program.
    Clipboard {
        selection: String,
        data: String,
    },
    Bell,
}

/// Result of feeding bytes to an [`Emulator`].
#[derive(Debug, Default)]
pub struct Output {
    /// Bytes to write back to the PTY (query replies). Always empty for
    /// passive emulators.
    pub replies: Vec<u8>,
    pub events: Vec<Event>,
}

/// Default colours reported to programs that ask (OSC 10/11).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Colors {
    pub foreground: (u8, u8, u8),
    pub background: (u8, u8, u8),
}

/// Tokyo Night's text and background: the Rakumux theme. The TUI itself
/// draws programs' default colours in the outer terminal's own.
impl Default for Colors {
    fn default() -> Self {
        Self {
            foreground: (0xc0, 0xca, 0xf5),
            background: (0x1a, 0x1b, 0x26),
        }
    }
}

/// Input-affecting modes the program has switched on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Modes {
    pub application_cursor: bool,
    pub application_keypad: bool,
    pub bracketed_paste: bool,
    pub alternate_screen: bool,
    pub hide_cursor: bool,
    pub mouse_mode: vt100::MouseProtocolMode,
    pub mouse_encoding: vt100::MouseProtocolEncoding,
}

struct Hooks {
    responder: Option<Colors>,
    replies: Vec<u8>,
    events: Vec<Event>,
}

impl Hooks {
    fn reply(&mut self, text: &str) {
        self.replies.extend_from_slice(text.as_bytes());
    }
}

fn rgb16((r, g, b): (u8, u8, u8)) -> String {
    format!("rgb:{r:02x}{r:02x}/{g:02x}{g:02x}/{b:02x}{b:02x}")
}

/// DECRQM state for a private mode: 1 = set, 2 = reset, 0 = not recognised.
fn private_mode_state(screen: &vt100::Screen, mode: u16) -> u8 {
    use vt100::{MouseProtocolEncoding as Enc, MouseProtocolMode as Mouse};
    let flag = |on: bool| if on { 1 } else { 2 };
    match mode {
        1 => flag(screen.application_cursor()),
        25 => flag(!screen.hide_cursor()),
        47 | 1047 | 1049 => flag(screen.alternate_screen()),
        1000 => flag(screen.mouse_protocol_mode() == Mouse::PressRelease),
        1002 => flag(screen.mouse_protocol_mode() == Mouse::ButtonMotion),
        1003 => flag(screen.mouse_protocol_mode() == Mouse::AnyMotion),
        1006 => flag(screen.mouse_protocol_encoding() == Enc::Sgr),
        2004 => flag(screen.bracketed_paste()),
        _ => 0,
    }
}

impl vt100::Callbacks for Hooks {
    fn audible_bell(&mut self, _: &mut vt100::Screen) {
        self.events.push(Event::Bell);
    }

    fn set_window_title(&mut self, _: &mut vt100::Screen, title: &[u8]) {
        self.events
            .push(Event::Title(String::from_utf8_lossy(title).into_owned()));
    }

    fn copy_to_clipboard(&mut self, _: &mut vt100::Screen, ty: &[u8], data: &[u8]) {
        self.events.push(Event::Clipboard {
            selection: String::from_utf8_lossy(ty).into_owned(),
            data: String::from_utf8_lossy(data).into_owned(),
        });
    }

    fn unhandled_csi(
        &mut self,
        screen: &mut vt100::Screen,
        i1: Option<u8>,
        i2: Option<u8>,
        params: &[&[u16]],
        c: char,
    ) {
        if self.responder.is_none() {
            return;
        }
        let first = params.first().and_then(|p| p.first()).copied();
        match (i1, i2, c, first) {
            // Device status report.
            (None, None, 'n', Some(5)) => self.reply("\x1b[0n"),
            // Cursor position report.
            (None, None, 'n', Some(6)) => {
                let (row, col) = screen.cursor_position();
                self.reply(&format!("\x1b[{};{}R", row + 1, col + 1));
            }
            (Some(b'?'), None, 'n', Some(6)) => {
                let (row, col) = screen.cursor_position();
                self.reply(&format!("\x1b[?{};{}R", row + 1, col + 1));
            }
            // Primary device attributes: VT220 with ANSI colour.
            (None, None, 'c', None | Some(0)) => self.reply("\x1b[?62;22c"),
            // Secondary device attributes.
            (Some(b'>'), None, 'c', None | Some(0)) => self.reply("\x1b[>1;10;0c"),
            // XTVERSION.
            (Some(b'>'), None, 'q', None | Some(0)) => {
                self.reply(&format!("\x1bP>|{VERSION_REPLY}\x1b\\"));
            }
            // Text area size in characters.
            (None, None, 't', Some(18)) => {
                let (rows, cols) = screen.size();
                self.reply(&format!("\x1b[8;{rows};{cols}t"));
            }
            // DECRQM for private modes.
            (Some(b'?'), Some(b'$'), 'p', Some(mode)) => {
                let state = private_mode_state(screen, mode);
                self.reply(&format!("\x1b[?{mode};{state}$y"));
            }
            // DECRQM for ANSI modes: none are tracked.
            (None, Some(b'$'), 'p', Some(mode)) => self.reply(&format!("\x1b[{mode};0$y")),
            _ => {}
        }
    }

    fn unhandled_osc(&mut self, _: &mut vt100::Screen, params: &[&[u8]]) {
        match params {
            [b"7", url, ..] => {
                if let Some(path) = parse_osc7(url) {
                    self.events.push(Event::Cwd(path));
                }
            }
            [b"10", b"?"] => {
                if let Some(colors) = self.responder {
                    self.reply(&format!("\x1b]10;{}\x1b\\", rgb16(colors.foreground)));
                }
            }
            [b"11", b"?"] => {
                if let Some(colors) = self.responder {
                    self.reply(&format!("\x1b]11;{}\x1b\\", rgb16(colors.background)));
                }
            }
            _ => {}
        }
    }
}

/// Parse an OSC 7 `file://host/path` URL into a path.
fn parse_osc7(url: &[u8]) -> Option<PathBuf> {
    let url = std::str::from_utf8(url).ok()?;
    let rest = url.strip_prefix("file://")?;
    let path = &rest[rest.find('/')?..];
    let mut bytes = Vec::with_capacity(path.len());
    let raw = path.as_bytes();
    let mut i = 0;
    while i < raw.len() {
        if raw[i] == b'%' && i + 2 < raw.len() {
            if let Ok(byte) = u8::from_str_radix(&path[i + 1..i + 3], 16) {
                bytes.push(byte);
                i += 3;
                continue;
            }
        }
        bytes.push(raw[i]);
        i += 1;
    }
    Some(PathBuf::from(String::from_utf8(bytes).ok()?))
}

/// One retained row (history or screen).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row<T> {
    pub content: T,
    /// The row continues on the next row (soft wrap).
    pub wrapped: bool,
}

/// Rendition of a cell, as far as SGR can express it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct Style {
    fg: Option<vt100::Color>,
    bg: Option<vt100::Color>,
    bold: bool,
    dim: bool,
    italic: bool,
    underline: bool,
    inverse: bool,
}

impl Style {
    fn of(cell: &vt100::Cell) -> Self {
        let color = |c: vt100::Color| (c != vt100::Color::Default).then_some(c);
        Self {
            fg: color(cell.fgcolor()),
            bg: color(cell.bgcolor()),
            bold: cell.bold(),
            dim: cell.dim(),
            italic: cell.italic(),
            underline: cell.underline(),
            inverse: cell.inverse(),
        }
    }

    /// A blank cell with this style is still visible (coloured background).
    fn visible_when_blank(&self) -> bool {
        self.bg.is_some() || self.inverse
    }

    /// `CSI 0 ; … m`: reset, then set every attribute of this style.
    fn write_sgr(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(b"\x1b[0");
        for (on, code) in [
            (self.bold, "1"),
            (self.dim, "2"),
            (self.italic, "3"),
            (self.underline, "4"),
            (self.inverse, "7"),
        ] {
            if on {
                out.push(b';');
                out.extend_from_slice(code.as_bytes());
            }
        }
        if let Some(color) = self.fg {
            write_sgr_color(out, color, 30, 90, 38);
        }
        if let Some(color) = self.bg {
            write_sgr_color(out, color, 40, 100, 48);
        }
        out.push(b'm');
    }
}

fn write_sgr_color(out: &mut Vec<u8>, color: vt100::Color, base: u8, bright: u8, extended: u8) {
    let code = match color {
        vt100::Color::Default => return,
        vt100::Color::Idx(n @ 0..=7) => format!(";{}", base + n),
        vt100::Color::Idx(n @ 8..=15) => format!(";{}", bright + n - 8),
        vt100::Color::Idx(n) => format!(";{extended};5;{n}"),
        vt100::Color::Rgb(r, g, b) => format!(";{extended};2;{r};{g};{b}"),
    };
    out.extend_from_slice(code.as_bytes());
}

/// Plain text of visible row `row`; trailing blanks are kept only when the
/// row soft-wraps.
fn plain_row(screen: &vt100::Screen, row: u16) -> Row<String> {
    let (_, cols) = screen.size();
    let wrapped = screen.row_wrapped(row);
    let mut text = String::new();
    for col in 0..cols {
        let Some(cell) = screen.cell(row, col) else {
            break;
        };
        if cell.is_wide_continuation() {
            continue;
        }
        if cell.has_contents() {
            text.push_str(cell.contents());
        } else {
            text.push(' ');
        }
    }
    if !wrapped {
        text.truncate(text.trim_end_matches(' ').len());
    }
    Row {
        content: text,
        wrapped,
    }
}

/// SGR-formatted visible row `row`, blanks as spaces, ending with a reset
/// when any attribute was set.
fn formatted_row(screen: &vt100::Screen, row: u16) -> Row<Vec<u8>> {
    let (_, cols) = screen.size();
    let wrapped = screen.row_wrapped(row);
    let cells: Vec<&vt100::Cell> = (0..cols).map_while(|col| screen.cell(row, col)).collect();
    let end = if wrapped {
        cells.len()
    } else {
        cells
            .iter()
            .rposition(|c| c.has_contents() || Style::of(c).visible_when_blank())
            .map_or(0, |i| i + 1)
    };
    let mut out = Vec::new();
    let mut current = Style::default();
    for cell in &cells[..end] {
        if cell.is_wide_continuation() {
            continue;
        }
        let style = Style::of(cell);
        if style != current {
            style.write_sgr(&mut out);
            current = style;
        }
        if cell.has_contents() {
            out.extend_from_slice(cell.contents().as_bytes());
        } else {
            out.push(b' ');
        }
    }
    if current != Style::default() {
        out.extend_from_slice(b"\x1b[m");
    }
    Row {
        content: out,
        wrapped,
    }
}

/// A terminal screen fed from a PTY byte stream.
pub struct Emulator {
    parser: vt100::Parser<Hooks>,
}

impl Emulator {
    /// A mirror that never answers queries. Use this in clients.
    pub fn passive(rows: u16, cols: u16, scrollback: usize) -> Self {
        Self::build(rows, cols, scrollback, None)
    }

    /// The authoritative emulator for a terminal. Use this in the daemon.
    pub fn responding(rows: u16, cols: u16, scrollback: usize, colors: Colors) -> Self {
        Self::build(rows, cols, scrollback, Some(colors))
    }

    fn build(rows: u16, cols: u16, scrollback: usize, responder: Option<Colors>) -> Self {
        let hooks = Hooks {
            responder,
            replies: Vec::new(),
            events: Vec::new(),
        };
        Self {
            parser: vt100::Parser::new_with_callbacks(rows, cols, scrollback, hooks),
        }
    }

    pub fn feed(&mut self, bytes: &[u8]) -> Output {
        self.parser.process(bytes);
        let hooks = self.parser.callbacks_mut();
        Output {
            replies: std::mem::take(&mut hooks.replies),
            events: std::mem::take(&mut hooks.events),
        }
    }

    pub fn resize(&mut self, rows: u16, cols: u16) {
        self.parser.screen_mut().set_size(rows, cols);
    }

    /// `(rows, cols)`.
    pub fn size(&self) -> (u16, u16) {
        self.parser.screen().size()
    }

    pub fn screen(&self) -> &vt100::Screen {
        self.parser.screen()
    }

    pub fn modes(&self) -> Modes {
        let screen = self.parser.screen();
        Modes {
            application_cursor: screen.application_cursor(),
            application_keypad: screen.application_keypad(),
            bracketed_paste: screen.bracketed_paste(),
            alternate_screen: screen.alternate_screen(),
            hide_cursor: screen.hide_cursor(),
            mouse_mode: screen.mouse_protocol_mode(),
            mouse_encoding: screen.mouse_protocol_encoding(),
        }
    }

    /// Plain text of the visible screen.
    pub fn text(&self) -> String {
        self.parser.screen().contents()
    }

    /// Bytes that bring a fresh emulator of the same size to this screen
    /// state: alternate screen, contents, attributes, cursor and input modes.
    pub fn checkpoint(&self) -> Vec<u8> {
        let screen = self.parser.screen();
        let mut out = Vec::new();
        if screen.alternate_screen() {
            out.extend_from_slice(b"\x1b[?1049h");
        }
        out.extend_from_slice(&screen.state_formatted());
        out
    }

    /// Number of rows in the scrollback above the visible screen.
    pub fn history_len(&mut self) -> usize {
        let screen = self.parser.screen_mut();
        screen.set_scrollback(usize::MAX);
        let len = screen.scrollback();
        screen.set_scrollback(0);
        len
    }

    /// Retained rows: history plus the visible screen.
    pub fn total_rows(&mut self) -> usize {
        self.history_len() + self.size().0 as usize
    }

    /// Plain-text rows with their soft-wrap flags, indexed like
    /// [`history_rows`](Self::history_rows). Rows that wrap keep their
    /// trailing blanks, so concatenating a wrapped row with the next one
    /// reproduces the logical line; other rows have trailing blanks trimmed.
    pub fn history_lines(&mut self, start: usize, count: usize) -> Vec<Row<String>> {
        let mut out = Vec::new();
        self.for_each_row(start, count, |screen, row| out.push(plain_row(screen, row)));
        out
    }

    /// ANSI rows indexed like [`history_rows`](Self::history_rows): SGR
    /// attributes and text only (no cursor movement), each ending with its
    /// attributes reset, so rows can be saved or printed line by line.
    pub fn history_rows_formatted(&mut self, start: usize, count: usize) -> Vec<Row<Vec<u8>>> {
        let mut out = Vec::new();
        self.for_each_row(start, count, |screen, row| {
            out.push(formatted_row(screen, row))
        });
        out
    }

    /// Call `f` with the screen scrolled so that each requested row is
    /// visible, and that row's index on the visible screen.
    fn for_each_row(&mut self, start: usize, count: usize, mut f: impl FnMut(&vt100::Screen, u16)) {
        let len = self.history_len();
        let (rows, _) = self.size();
        let total = len + rows as usize;
        let end = start.saturating_add(count).min(total);
        let mut index = start;
        while index < end {
            // Scrolling back by `len - index` puts row `index` at the top.
            let offset = len.saturating_sub(index);
            self.parser.screen_mut().set_scrollback(offset);
            let first = len - offset;
            let skip = index - first;
            let take = (end - index).min(rows as usize - skip);
            let screen = self.parser.screen();
            for row in skip..skip + take {
                f(screen, row as u16);
            }
            index += take;
        }
        self.parser.screen_mut().set_scrollback(0);
    }

    /// Plain-text history rows, oldest first. `start` indexes from the oldest
    /// retained row; rows on the visible screen follow the history.
    pub fn history_rows(&mut self, start: usize, count: usize) -> Vec<String> {
        let len = self.history_len();
        let (rows, cols) = self.size();
        let total = len + rows as usize;
        let end = (start + count).min(total);
        let mut out = Vec::with_capacity(end.saturating_sub(start));
        let mut index = start;
        while index < end {
            // Scrolling back by `len - index` puts row `index` at the top.
            let offset = len.saturating_sub(index);
            let screen = self.parser.screen_mut();
            screen.set_scrollback(offset);
            let first = len - offset;
            let skip = index - first;
            let take = (end - index).min(rows as usize - skip);
            out.extend(screen.rows(0, cols).skip(skip).take(take));
            index += take;
        }
        self.parser.screen_mut().set_scrollback(0);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn answers_cursor_position_and_device_attributes() {
        let mut emu = Emulator::responding(24, 80, 0, Colors::default());
        let out = emu.feed(b"hello\x1b[6n\x1b[c");
        assert_eq!(out.replies, b"\x1b[1;6R\x1b[?62;22c");
    }

    #[test]
    fn passive_emulator_never_replies() {
        let mut emu = Emulator::passive(24, 80, 0);
        let out = emu.feed(b"\x1b[6n\x1b[c\x1b]11;?\x1b\\");
        assert!(out.replies.is_empty());
    }

    #[test]
    fn answers_colour_and_mode_queries() {
        let mut emu = Emulator::responding(24, 80, 0, Colors::default());
        let out = emu.feed(b"\x1b]11;?\x07\x1b[?2004h\x1b[?2004$p\x1b[?2026$p");
        assert_eq!(
            String::from_utf8(out.replies).unwrap(),
            "\x1b]11;rgb:1a1a/1b1b/2626\x1b\\\x1b[?2004;1$y\x1b[?2026;0$y"
        );
    }

    #[test]
    fn reports_title_cwd_and_bell() {
        let mut emu = Emulator::passive(24, 80, 0);
        let out = emu.feed(b"\x1b]2;build\x07\x1b]7;file://host/tmp/a%20b\x07\x07");
        assert_eq!(
            out.events,
            vec![
                Event::Title("build".into()),
                Event::Cwd(PathBuf::from("/tmp/a b")),
                Event::Bell
            ]
        );
    }

    #[test]
    fn checkpoint_reproduces_screen_and_modes() {
        let mut source = Emulator::responding(10, 40, 100, Colors::default());
        source.feed(b"\x1b[31mred\x1b[0m line\r\n\xe4\xb8\xad\xe6\x96\x87 wide\r\n");
        source.feed(b"\x1b[?1049h\x1b[2;5Halt screen\x1b[?1h\x1b[?2004h\x1b[?1000h\x1b[?1006h");

        let mut mirror = Emulator::passive(10, 40, 0);
        mirror.feed(&source.checkpoint());

        assert_eq!(mirror.text(), source.text());
        assert_eq!(mirror.modes(), source.modes());
        assert_eq!(
            mirror.screen().cursor_position(),
            source.screen().cursor_position()
        );
        let a = source.screen().cell(1, 4).unwrap();
        let b = mirror.screen().cell(1, 4).unwrap();
        assert_eq!(a.contents(), b.contents());
    }

    #[test]
    fn checkpoint_keeps_colours() {
        let mut source = Emulator::passive(5, 20, 0);
        source.feed(b"\x1b[1;32mok\x1b[0m");
        let mut mirror = Emulator::passive(5, 20, 0);
        mirror.feed(&source.checkpoint());
        let cell = mirror.screen().cell(0, 0).unwrap();
        assert_eq!(cell.fgcolor(), vt100::Color::Idx(2));
        assert!(cell.bold());
    }

    #[test]
    fn history_rows_include_scrollback_then_screen() {
        let mut emu = Emulator::passive(3, 10, 100);
        for i in 0..8 {
            emu.feed(format!("line{i}\r\n").as_bytes());
        }
        // 8 lines plus a trailing empty row: 6 rows scrolled off, 3 visible.
        assert_eq!(emu.history_len(), 6);
        assert_eq!(emu.history_rows(0, 2), vec!["line0", "line1"]);
        assert_eq!(emu.history_rows(5, 4), vec!["line5", "line6", "line7", ""]);
        // Reading history leaves the live view at the bottom.
        assert_eq!(emu.screen().scrollback(), 0);
    }

    #[test]
    fn history_lines_report_soft_wraps_and_keep_wrapped_blanks() {
        let mut emu = Emulator::passive(4, 6, 100);
        // "hello " fills the first row exactly, so the space before the wrap
        // must survive for the logical line to read "hello world".
        emu.feed(b"first\r\nhello world\r\nend");
        assert_eq!(emu.total_rows(), 4);
        let lines = emu.history_lines(0, 10);
        let rows: Vec<(&str, bool)> = lines
            .iter()
            .map(|r| (r.content.as_str(), r.wrapped))
            .collect();
        assert_eq!(
            rows,
            vec![
                ("first", false),
                ("hello ", true),
                ("world", false),
                ("end", false)
            ]
        );
        let joined = format!("{}{}", lines[1].content, lines[2].content);
        assert_eq!(joined, "hello world");
    }

    #[test]
    fn history_lines_span_scrollback_and_screen() {
        let mut emu = Emulator::passive(2, 10, 100);
        emu.feed(b"a\r\nb\r\nc\r\nd");
        assert_eq!(emu.total_rows(), 4);
        let rows: Vec<String> = emu
            .history_lines(1, 3)
            .into_iter()
            .map(|r| r.content)
            .collect();
        assert_eq!(rows, vec!["b", "c", "d"]);
        assert_eq!(emu.screen().scrollback(), 0);
    }

    #[test]
    fn formatted_rows_carry_attributes_without_cursor_movement() {
        let mut emu = Emulator::passive(3, 20, 100);
        emu.feed(b"\x1b[1;31mred\x1b[0m plain  \x1b[48;5;200m  \x1b[0m\r\n");
        emu.feed(b"\x1b[38;2;1;2;3mrgb\x1b[m \xe4\xb8\xad!\r\n");
        let rows = emu.history_rows_formatted(0, 3);
        assert_eq!(
            String::from_utf8(rows[0].content.clone()).unwrap(),
            "\x1b[0;1;31mred\x1b[0m plain  \x1b[0;48;5;200m  \x1b[m"
        );
        assert_eq!(
            String::from_utf8(rows[1].content.clone()).unwrap(),
            "\x1b[0;38;2;1;2;3mrgb\x1b[0m \u{4e2d}!"
        );
        assert!(rows[2].content.is_empty());

        // Printing the rows line by line reproduces the screen.
        let mut mirror = Emulator::passive(3, 20, 0);
        let joined: Vec<u8> = rows
            .iter()
            .map(|r| r.content.clone())
            .collect::<Vec<_>>()
            .join(&b"\r\n"[..]);
        mirror.feed(&joined);
        assert_eq!(mirror.text(), emu.text());
        let cell = mirror.screen().cell(0, 0).unwrap();
        assert!(cell.bold());
        assert_eq!(cell.fgcolor(), vt100::Color::Idx(1));
        assert_eq!(
            mirror.screen().cell(1, 0).unwrap().fgcolor(),
            vt100::Color::Rgb(1, 2, 3)
        );
        assert_eq!(
            mirror.screen().cell(0, 11).unwrap().bgcolor(),
            vt100::Color::Idx(200)
        );
    }
}
