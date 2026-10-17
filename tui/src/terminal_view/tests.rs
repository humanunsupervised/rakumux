use super::*;
use crossterm::event::{
    KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers, MouseButton, MouseEventKind,
};
use ratatui::style::{Color, Modifier};

/// Default colours left to the outer terminal.
const TERMINAL: DefaultColors = DefaultColors {
    fg: Color::Reset,
    bg: Color::Reset,
};

fn view_with(rows: u16, cols: u16, bytes: &[u8]) -> TerminalView {
    let mut view = TerminalView::new(rows, cols);
    view.feed(bytes);
    view
}

fn render(
    view: &TerminalView,
    width: u16,
    height: u16,
    focused: bool,
) -> (Buffer, Option<(u16, u16)>) {
    let area = Rect::new(0, 0, width, height);
    let mut buf = Buffer::empty(area);
    let cursor = view.render(area, &mut buf, focused, TERMINAL);
    (buf, cursor)
}

/// Symbols of buffer row `y`, one per cell.
fn row(buf: &Buffer, y: u16) -> String {
    (buf.area.x..buf.area.right())
        .map(|x| buf.get(x, y).symbol())
        .collect()
}

fn modes_after(bytes: &[u8]) -> vt::Modes {
    let mut emulator = vt::Emulator::passive(24, 80, 0);
    emulator.feed(bytes);
    emulator.modes()
}

fn lines(count: usize) -> Vec<u8> {
    (0..count)
        .map(|i| format!("line{i}"))
        .collect::<Vec<_>>()
        .join("\r\n")
        .into_bytes()
}

// ---------------------------------------------------------------- rendering

#[test]
fn renders_text_with_sgr_attributes() {
    let view = view_with(
        2,
        8,
        b"\x1b[31mred\x1b[0m \x1b[1;3;4mx\x1b[0m\x1b[2;7my\x1b[0m",
    );
    let (buf, _) = render(&view, 8, 2, false);
    assert_eq!(row(&buf, 0), "red xy  ");

    let red = buf.get(0, 0);
    assert_eq!(
        (red.fg, red.bg, red.modifier),
        (Color::Indexed(1), Color::Reset, Modifier::empty())
    );
    let space = buf.get(3, 0);
    assert_eq!((space.fg, space.bg), (Color::Reset, Color::Reset));
    assert_eq!(
        buf.get(4, 0).modifier,
        Modifier::BOLD | Modifier::ITALIC | Modifier::UNDERLINED
    );
    assert_eq!(buf.get(5, 0).modifier, Modifier::DIM | Modifier::REVERSED);
}

#[test]
fn renders_indexed_and_rgb_colours() {
    let view = view_with(
        1,
        4,
        b"\x1b[38;5;208;48;2;10;20;30mA\x1b[0m\x1b[94;42mB\x1b[39;49mC",
    );
    let (buf, _) = render(&view, 4, 1, false);
    let a = buf.get(0, 0);
    assert_eq!((a.fg, a.bg), (Color::Indexed(208), Color::Rgb(10, 20, 30)));
    let b = buf.get(1, 0);
    assert_eq!((b.fg, b.bg), (Color::Indexed(12), Color::Indexed(2)));
    let c = buf.get(2, 0);
    assert_eq!((c.symbol(), c.fg, c.bg), ("C", Color::Reset, Color::Reset));
}

#[test]
fn default_colours_are_drawn_in_the_given_ones() {
    let colors = DefaultColors {
        fg: Color::Rgb(192, 202, 245),
        bg: Color::Rgb(26, 27, 38),
    };
    // Default, set by the program, default again, reversed default; the
    // pane is wider and taller than the (2×4) terminal.
    let view = view_with(2, 4, b"a\x1b[31;42mb\x1b[39;49mc\x1b[7md\x1b[H");
    let area = Rect::new(0, 0, 6, 3);
    let mut buf = Buffer::empty(area);
    view.render(area, &mut buf, false, colors);
    let cell = |x, y| {
        let cell = buf.get(x, y);
        (cell.fg, cell.bg)
    };
    assert_eq!(cell(0, 0), (colors.fg, colors.bg));
    assert_eq!(cell(1, 0), (Color::Indexed(1), Color::Indexed(2)));
    assert_eq!(cell(2, 0), (colors.fg, colors.bg));
    // Reverse video swaps them on screen.
    assert_eq!(cell(3, 0), (colors.fg, colors.bg));
    assert_eq!(buf.get(3, 0).modifier, Modifier::REVERSED);
    // A cell the program never wrote, and the padding beyond the edges.
    for (x, y) in [(0, 1), (4, 0), (5, 1), (0, 2), (5, 2)] {
        assert_eq!(cell(x, y), (colors.fg, colors.bg), "({x},{y})");
    }
}

#[test]
fn pads_a_smaller_terminal_and_overwrites_the_whole_area() {
    let view = view_with(2, 3, b"ab");
    let area = Rect::new(1, 1, 5, 4);
    let stale = {
        let mut cell = ratatui::buffer::Cell::default();
        cell.set_symbol("x").set_bg(Color::Red).modifier = Modifier::BOLD;
        cell
    };
    let mut buf = Buffer::filled(Rect::new(0, 0, 7, 6), &stale);
    view.render(area, &mut buf, false, TERMINAL);

    assert_eq!(row(&buf, 1), "xab   x");
    for y in 1..5 {
        for x in 1..6 {
            let cell = buf.get(x, y);
            assert_eq!(cell.bg, Color::Reset, "bg at ({x},{y})");
            assert_eq!(cell.modifier, Modifier::empty(), "modifier at ({x},{y})");
        }
    }
    // Outside the area nothing changes.
    assert_eq!(buf.get(0, 0).symbol(), "x");
    assert_eq!(buf.get(6, 5).bg, Color::Red);
}

#[test]
fn wide_characters_span_two_cells() {
    let view = view_with(1, 6, "中a文".as_bytes());
    let (buf, _) = render(&view, 6, 1, false);
    assert_eq!(buf.get(0, 0).symbol(), "中");
    assert_eq!(buf.get(1, 0).symbol(), " ");
    assert_eq!(buf.get(2, 0).symbol(), "a");
    assert_eq!(buf.get(3, 0).symbol(), "文");
    assert_eq!(buf.get(5, 0).symbol(), " ");

    // ratatui skips the cell a wide symbol covers when it writes the frame.
    let updates = Buffer::empty(buf.area).diff(&buf);
    let columns: Vec<u16> = updates.iter().map(|(x, _, _)| *x).collect();
    assert_eq!(columns, vec![0, 2, 3]);
}

#[test]
fn wide_character_cut_by_the_pane_edge_is_blanked() {
    // Cursor home so the view isn't panned.
    let view = view_with(1, 10, "abc\x1b[41m中\x1b[0m\x1b[H".as_bytes());
    let (buf, _) = render(&view, 4, 1, false);
    assert_eq!(row(&buf, 0), "abc ");
    // The blank keeps the character's colours.
    assert_eq!(buf.get(3, 0).bg, Color::Indexed(1));
}

#[test]
fn wide_character_cut_on_the_left_by_panning_is_blanked() {
    // 7 columns: a, 中 (1-2), b..e; cursor ends on the last column.
    let view = view_with(1, 7, "a\x1b[44m中\x1b[0mbcde".as_bytes());
    let (buf, _) = render(&view, 5, 1, false);
    assert_eq!(row(&buf, 0), " bcde");
    assert_eq!(buf.get(0, 0).bg, Color::Indexed(4));
}

#[test]
fn larger_terminal_is_cropped_around_the_cursor() {
    // Cursor on the last row: the bottom rows show.
    let view = view_with(10, 20, &lines(10));
    let (buf, cursor) = render(&view, 10, 4, true);
    assert_eq!(row(&buf, 0), "line6     ");
    assert_eq!(row(&buf, 3), "line9     ");
    assert_eq!(cursor, Some((5, 3)));

    // Cursor at the top: the top-left part shows.
    let view = view_with(10, 20, &[lines(10), b"\x1b[H".to_vec()].concat());
    let (buf, cursor) = render(&view, 10, 4, true);
    assert_eq!(row(&buf, 0), "line0     ");
    assert_eq!(cursor, Some((0, 0)));

    // Cursor beyond the right edge: panned horizontally.
    let view = view_with(2, 20, b"0123456789abcdef");
    let (buf, cursor) = render(&view, 10, 2, true);
    assert_eq!(row(&buf, 0), "789abcdef ");
    assert_eq!(cursor, Some((9, 0)));
}

#[test]
fn cursor_only_when_focused_and_visible() {
    let view = view_with(3, 10, b"ab");
    assert_eq!(render(&view, 10, 3, true).1, Some((2, 0)));
    assert_eq!(render(&view, 10, 3, false).1, None);

    // Absolute coordinates.
    let mut buf = Buffer::empty(Rect::new(0, 0, 20, 10));
    assert_eq!(
        view.render(Rect::new(5, 3, 10, 3), &mut buf, true, TERMINAL),
        Some((7, 3))
    );

    let hidden = view_with(3, 10, b"ab\x1b[?25l");
    assert_eq!(render(&hidden, 10, 3, true).1, None);
}

#[test]
fn cursor_with_pending_wrap_sits_on_the_last_column() {
    let view = view_with(2, 4, b"abcd");
    assert_eq!(render(&view, 4, 2, true).1, Some((3, 0)));
}

#[test]
fn render_is_clipped_to_the_buffer() {
    let view = view_with(5, 10, b"hello\x1b[H");
    let mut buf = Buffer::empty(Rect::new(0, 0, 5, 2));
    // Only the part of the area inside the buffer is drawn.
    assert_eq!(
        view.render(Rect::new(3, 1, 10, 5), &mut buf, true, TERMINAL),
        Some((3, 1))
    );
    assert_eq!(row(&buf, 0), "     ");
    assert_eq!(row(&buf, 1), "   he");
    assert_eq!(
        view.render(Rect::new(10, 10, 5, 5), &mut buf, true, TERMINAL),
        None
    );
}

#[test]
fn scrolling_back_shows_history_frozen_with_a_marker() {
    let mut view = TerminalView::with_scrollback(3, 12, 100);
    view.feed(&lines(8));
    view.feed(b"\r\n");
    // 8 lines and an empty row: 6 rows above the 3 visible ones.
    assert_eq!(view.history_len(), 6);

    view.scroll(2);
    assert_eq!(view.scroll_offset(), 2);
    let (buf, cursor) = render(&view, 12, 3, true);
    assert_eq!(row(&buf, 0), "line4  [2/6]");
    assert_eq!(row(&buf, 1), "line5       ");
    assert_eq!(row(&buf, 2), "line6       ");
    assert_eq!(cursor, None);
    assert_eq!(buf.get(7, 0).modifier, Modifier::REVERSED);

    // Live output doesn't move the picture while scrolled back.
    view.feed(b"more\r\n");
    assert_eq!(render(&view, 12, 3, true).0, buf);

    view.scroll(100);
    assert_eq!(view.scroll_offset(), 6);
    let (buf, _) = render(&view, 12, 3, false);
    assert_eq!(row(&buf, 0), "line0  [6/6]");

    view.scroll(-6);
    assert_eq!(view.scroll_offset(), 0);
    let (buf, cursor) = render(&view, 12, 3, true);
    assert_eq!(row(&buf, 0), "line7       ");
    assert_eq!(row(&buf, 1), "more        ");
    assert_eq!(cursor, Some((0, 2)));
    assert_eq!(view.history_len(), 7);
}

#[test]
fn scrolling_is_clamped_to_retained_rows() {
    let mut view = view_with(3, 10, b"a\r\nb");
    view.scroll(5);
    assert_eq!(view.scroll_offset(), 0);
    view.scroll(-3);
    assert_eq!(view.scroll_offset(), 0);

    let mut view = view_with(2, 10, &lines(4));
    view.scroll(1);
    assert_eq!(view.scroll_offset(), 1);
    view.scroll_to_bottom();
    assert_eq!(view.scroll_offset(), 0);
    let (buf, _) = render(&view, 10, 2, false);
    assert_eq!(row(&buf, 1), "line3     ");
}

#[test]
fn scroll_marker_does_not_overlap_a_wide_character() {
    let mut view = TerminalView::with_scrollback(2, 10, 10);
    view.feed("abcd中\r\nx\r\ny\r\n".as_bytes());
    view.scroll(2);
    let (buf, _) = render(&view, 10, 2, false);
    assert_eq!(row(&buf, 0), "abcd [2/2]");
}

#[test]
fn scrolled_back_taller_terminal_keeps_its_bottom_rows_in_view() {
    let mut view = TerminalView::with_scrollback(4, 10, 10);
    view.feed(&lines(6));
    view.scroll(1);
    let (buf, _) = render(&view, 10, 2, false);
    // Scrolled one row back: rows 1..5 of history+screen, bottom two shown.
    assert_eq!(row(&buf, 0), "line3[1/2]");
    assert_eq!(row(&buf, 1), "line4     ");
}

#[test]
fn reset_reproduces_a_checkpoint() {
    let mut source = vt::Emulator::passive(5, 20, 0);
    source.feed(b"\x1b[32mgreen\x1b[0m \xe4\xb8\xad\r\n\x1b[?1049h\x1b[3;4Halt\x1b[?1h\x1b[?2004h");

    let mut view = view_with(2, 2, b"old");
    view.scroll(1);
    view.reset(5, 20, &source.checkpoint());

    assert_eq!(view.size(), (5, 20));
    assert_eq!(view.modes(), source.modes());
    assert_eq!(view.scroll_offset(), 0);
    let (buf, cursor) = render(&view, 20, 5, true);
    for y in 0..5 {
        let expected: String = (0..20)
            .map(|x| {
                let cell = source.screen().cell(y, x).unwrap();
                match cell.contents() {
                    "" => " ".to_string(),
                    text => text.to_string(),
                }
            })
            .collect();
        assert_eq!(row(&buf, y), expected, "row {y}");
    }
    let (row_, col) = source.screen().cursor_position();
    assert_eq!(cursor, Some((col, row_)));
}

#[test]
fn reset_keeps_all_rows_of_a_long_payload() {
    let mut view = TerminalView::with_scrollback(3, 10, 2);
    view.reset(3, 10, &lines(50));
    assert_eq!(view.history_len(), 47);
    view.scroll(47);
    let (buf, _) = render(&view, 20, 3, false);
    assert_eq!(row(&buf, 0), format!("{:<13}[47/47]", "line0"));
}

#[test]
fn reset_to_text_shows_read_output() {
    let mut view = TerminalView::new(3, 10);
    view.reset_to_text(3, 10, "a\nb\r\nc\nd\n");
    let (buf, cursor) = render(&view, 10, 3, true);
    assert_eq!(row(&buf, 0), "b         ");
    assert_eq!(row(&buf, 2), "d         ");
    assert_eq!(cursor, None, "cursor hidden");
    assert_eq!(view.history_len(), 1);

    // ANSI rows: attributes don't leak from one row into the next.
    view.reset_to_text(2, 10, "\x1b[31mred\nplain");
    let (buf, _) = render(&view, 10, 2, false);
    assert_eq!(buf.get(0, 0).fg, Color::Indexed(1));
    assert_eq!(buf.get(0, 1).fg, Color::Reset);
}

#[test]
fn tiny_sizes_do_not_panic() {
    let mut view = TerminalView::new(0, 0);
    assert_eq!(view.size(), (2, 2));
    view.feed("中ab\r\n\x1b[5;5Hwrap wrap wrap".as_bytes());
    view.reset(1, 0, "x中y中\r\nz".as_bytes());
    assert_eq!(view.size(), (2, 2));
    let (_, cursor) = render(&view, 3, 3, true);
    assert!(cursor.is_some());
    assert_eq!(render(&view, 0, 0, true).1, None);
    view.reset_to_text(0, 1, "中\n中中");
    render(&view, 1, 1, false);
}

// -------------------------------------------------------------------- keys

fn key(code: KeyCode, mods: KeyModifiers) -> Option<Vec<u8>> {
    key_to_bytes(&KeyEvent::new(code, mods), &modes_after(b""))
}

#[test]
fn characters_with_ctrl_alt_and_shift() {
    let none = KeyModifiers::NONE;
    assert_eq!(key(KeyCode::Char('a'), none).unwrap(), b"a");
    assert_eq!(key(KeyCode::Char('A'), KeyModifiers::SHIFT).unwrap(), b"A");
    assert_eq!(key(KeyCode::Char('a'), KeyModifiers::SHIFT).unwrap(), b"A");
    assert_eq!(
        key(KeyCode::Char('c'), KeyModifiers::CONTROL).unwrap(),
        [0x03]
    );
    assert_eq!(
        key(KeyCode::Char(' '), KeyModifiers::CONTROL).unwrap(),
        [0x00]
    );
    assert_eq!(
        key(KeyCode::Char('x'), KeyModifiers::ALT).unwrap(),
        b"\x1bx"
    );
    assert_eq!(
        key(
            KeyCode::Char('c'),
            KeyModifiers::CONTROL | KeyModifiers::ALT
        )
        .unwrap(),
        [0x1b, 0x03]
    );
    assert_eq!(key(KeyCode::Char('é'), none).unwrap(), "é".as_bytes());
    assert_eq!(key(KeyCode::Null, none).unwrap(), [0x00]);
}

#[test]
fn editing_keys() {
    let none = KeyModifiers::NONE;
    assert_eq!(key(KeyCode::Enter, none).unwrap(), b"\r");
    assert_eq!(key(KeyCode::Enter, KeyModifiers::ALT).unwrap(), b"\x1b\r");
    assert_eq!(key(KeyCode::Tab, none).unwrap(), b"\t");
    assert_eq!(
        key(KeyCode::BackTab, KeyModifiers::SHIFT).unwrap(),
        b"\x1b[Z"
    );
    assert_eq!(key(KeyCode::Backspace, none).unwrap(), [0x7f]);
    assert_eq!(key(KeyCode::Esc, none).unwrap(), [0x1b]);
    assert_eq!(key(KeyCode::Insert, none).unwrap(), b"\x1b[2~");
    assert_eq!(key(KeyCode::Delete, none).unwrap(), b"\x1b[3~");
    assert_eq!(key(KeyCode::PageUp, none).unwrap(), b"\x1b[5~");
    assert_eq!(
        key(KeyCode::PageDown, KeyModifiers::SHIFT).unwrap(),
        b"\x1b[6;2~"
    );
}

#[test]
fn function_keys() {
    let none = KeyModifiers::NONE;
    assert_eq!(key(KeyCode::F(1), none).unwrap(), b"\x1bOP");
    assert_eq!(key(KeyCode::F(4), none).unwrap(), b"\x1bOS");
    assert_eq!(
        key(KeyCode::F(1), KeyModifiers::CONTROL).unwrap(),
        b"\x1b[1;5P"
    );
    assert_eq!(key(KeyCode::F(5), none).unwrap(), b"\x1b[15~");
    assert_eq!(
        key(KeyCode::F(5), KeyModifiers::SHIFT).unwrap(),
        b"\x1b[15;2~"
    );
    assert_eq!(key(KeyCode::F(12), none).unwrap(), b"\x1b[24~");
    assert_eq!(key(KeyCode::F(13), none), None);
}

#[test]
fn cursor_keys_follow_application_cursor_mode() {
    let normal = modes_after(b"");
    let app = modes_after(b"\x1b[?1h");
    let press = |code| KeyEvent::new(code, KeyModifiers::NONE);
    assert_eq!(
        key_to_bytes(&press(KeyCode::Up), &normal).unwrap(),
        b"\x1b[A"
    );
    assert_eq!(key_to_bytes(&press(KeyCode::Up), &app).unwrap(), b"\x1bOA");
    assert_eq!(
        key_to_bytes(&press(KeyCode::Home), &app).unwrap(),
        b"\x1bOH"
    );
    assert_eq!(
        key_to_bytes(&press(KeyCode::End), &normal).unwrap(),
        b"\x1b[F"
    );
    let ctrl_right = KeyEvent::new(KeyCode::Right, KeyModifiers::CONTROL);
    assert_eq!(key_to_bytes(&ctrl_right, &app).unwrap(), b"\x1b[1;5C");
    let shift_alt_left = KeyEvent::new(KeyCode::Left, KeyModifiers::SHIFT | KeyModifiers::ALT);
    assert_eq!(
        key_to_bytes(&shift_alt_left, &normal).unwrap(),
        b"\x1b[1;4D"
    );
}

#[test]
fn keypad_follows_application_keypad_mode() {
    let normal = modes_after(b"");
    let app = modes_after(b"\x1b=");
    assert!(app.application_keypad);
    let keypad = |code| {
        KeyEvent::new_with_kind_and_state(
            code,
            KeyModifiers::NONE,
            KeyEventKind::Press,
            KeyEventState::KEYPAD,
        )
    };
    assert_eq!(
        key_to_bytes(&keypad(KeyCode::Char('5')), &normal).unwrap(),
        b"5"
    );
    assert_eq!(
        key_to_bytes(&keypad(KeyCode::Char('5')), &app).unwrap(),
        b"\x1bOu"
    );
    assert_eq!(
        key_to_bytes(&keypad(KeyCode::Char('+')), &app).unwrap(),
        b"\x1bOk"
    );
    assert_eq!(
        key_to_bytes(&keypad(KeyCode::Enter), &app).unwrap(),
        b"\x1bOM"
    );
    // Keypad arrows (num lock off) are cursor keys.
    assert_eq!(key_to_bytes(&keypad(KeyCode::Up), &app).unwrap(), b"\x1b[A");
    // The main keyboard's digits are unaffected.
    let digit = KeyEvent::new(KeyCode::Char('5'), KeyModifiers::NONE);
    assert_eq!(key_to_bytes(&digit, &app).unwrap(), b"5");
}

#[test]
fn releases_and_lock_keys_send_nothing() {
    let modes = modes_after(b"");
    let release = KeyEvent::new_with_kind(
        KeyCode::Char('a'),
        KeyModifiers::NONE,
        KeyEventKind::Release,
    );
    assert_eq!(key_to_bytes(&release, &modes), None);
    let repeat =
        KeyEvent::new_with_kind(KeyCode::Char('a'), KeyModifiers::NONE, KeyEventKind::Repeat);
    assert_eq!(key_to_bytes(&repeat, &modes).unwrap(), b"a");
    assert_eq!(key(KeyCode::CapsLock, KeyModifiers::NONE), None);
    assert_eq!(key(KeyCode::PrintScreen, KeyModifiers::NONE), None);
}

// ------------------------------------------------------------------- mouse

fn mouse(kind: MouseEventKind, column: u16, row: u16, modifiers: KeyModifiers) -> MouseEvent {
    MouseEvent {
        kind,
        column,
        row,
        modifiers,
    }
}

#[test]
fn mouse_is_pane_relative_and_only_when_requested() {
    let pane = Rect::new(10, 5, 20, 10);
    let none = modes_after(b"");
    let sgr = modes_after(b"\x1b[?1000h\x1b[?1006h");
    let down = |col, row| {
        mouse(
            MouseEventKind::Down(MouseButton::Left),
            col,
            row,
            KeyModifiers::NONE,
        )
    };

    assert_eq!(mouse_to_bytes(&down(12, 7), pane, &none), None);
    assert_eq!(
        mouse_to_bytes(&down(12, 7), pane, &sgr).unwrap(),
        b"\x1b[<0;3;3M"
    );
    assert_eq!(
        mouse_to_bytes(&down(10, 5), pane, &sgr).unwrap(),
        b"\x1b[<0;1;1M"
    );
    assert_eq!(
        mouse_to_bytes(&down(29, 14), pane, &sgr).unwrap(),
        b"\x1b[<0;20;10M"
    );
    // Outside the pane.
    assert_eq!(mouse_to_bytes(&down(9, 7), pane, &sgr), None);
    assert_eq!(mouse_to_bytes(&down(30, 7), pane, &sgr), None);
    assert_eq!(mouse_to_bytes(&down(12, 15), pane, &sgr), None);

    let up = mouse(
        MouseEventKind::Up(MouseButton::Right),
        12,
        7,
        KeyModifiers::NONE,
    );
    assert_eq!(mouse_to_bytes(&up, pane, &sgr).unwrap(), b"\x1b[<2;3;3m");
    let middle = mouse(
        MouseEventKind::Down(MouseButton::Middle),
        12,
        7,
        KeyModifiers::SHIFT,
    );
    assert_eq!(
        mouse_to_bytes(&middle, pane, &sgr).unwrap(),
        b"\x1b[<5;3;3M"
    );
}

#[test]
fn wheel_and_motion_follow_the_mouse_mode() {
    let pane = Rect::new(0, 0, 20, 10);
    let press_release = modes_after(b"\x1b[?1000h\x1b[?1006h");
    let button_motion = modes_after(b"\x1b[?1002h\x1b[?1006h");
    let any_motion = modes_after(b"\x1b[?1003h\x1b[?1006h");
    let at = |kind, mods| mouse(kind, 2, 3, mods);

    let wheel_up = at(MouseEventKind::ScrollUp, KeyModifiers::NONE);
    assert_eq!(
        mouse_to_bytes(&wheel_up, pane, &press_release).unwrap(),
        b"\x1b[<64;3;4M"
    );
    let wheel_down = at(MouseEventKind::ScrollDown, KeyModifiers::CONTROL);
    assert_eq!(
        mouse_to_bytes(&wheel_down, pane, &press_release).unwrap(),
        b"\x1b[<81;3;4M"
    );
    let wheel_left = at(MouseEventKind::ScrollLeft, KeyModifiers::NONE);
    assert_eq!(mouse_to_bytes(&wheel_left, pane, &any_motion), None);

    let drag = at(MouseEventKind::Drag(MouseButton::Left), KeyModifiers::NONE);
    assert_eq!(mouse_to_bytes(&drag, pane, &press_release), None);
    assert_eq!(
        mouse_to_bytes(&drag, pane, &button_motion).unwrap(),
        b"\x1b[<32;3;4M"
    );

    let moved = at(MouseEventKind::Moved, KeyModifiers::NONE);
    assert_eq!(mouse_to_bytes(&moved, pane, &button_motion), None);
    assert_eq!(
        mouse_to_bytes(&moved, pane, &any_motion).unwrap(),
        b"\x1b[<35;3;4M"
    );
}

#[test]
fn legacy_mouse_encoding() {
    let pane = Rect::new(4, 2, 20, 10);
    let x10 = modes_after(b"\x1b[?1000h");
    let down = mouse(
        MouseEventKind::Down(MouseButton::Left),
        4,
        2,
        KeyModifiers::NONE,
    );
    assert_eq!(
        mouse_to_bytes(&down, pane, &x10).unwrap(),
        vec![0x1b, b'[', b'M', 32, 33, 33]
    );
}

#[test]
fn view_mouse_bytes_follow_panning_and_terminal_edges() {
    let mut setup = b"\x1b[?1000h\x1b[?1006h".to_vec();
    setup.extend_from_slice(&lines(10));
    let mut view = TerminalView::with_scrollback(10, 20, 10);
    view.feed(&setup);
    let click = |col, row| {
        mouse(
            MouseEventKind::Down(MouseButton::Left),
            col,
            row,
            KeyModifiers::NONE,
        )
    };

    // Pane 4 rows high shows terminal rows 6..10.
    let pane = Rect::new(0, 0, 10, 4);
    assert_eq!(
        view.mouse_bytes(&click(1, 0), pane).unwrap(),
        b"\x1b[<0;2;7M"
    );

    // Pane larger than the terminal: clicks past its edge are dropped.
    let pane = Rect::new(0, 0, 30, 12);
    assert_eq!(
        view.mouse_bytes(&click(19, 9), pane).unwrap(),
        b"\x1b[<0;20;10M"
    );
    assert_eq!(view.mouse_bytes(&click(20, 0), pane), None);
    assert_eq!(view.mouse_bytes(&click(0, 10), pane), None);

    // Nothing reaches the program while history is shown.
    view.feed(b"\r\n");
    view.scroll(1);
    assert_eq!(view.mouse_bytes(&click(0, 0), pane), None);
}

#[test]
fn paste_is_bracketed_when_requested() {
    assert_eq!(paste_to_bytes("a\nb", &modes_after(b"")), b"a\rb");
    assert_eq!(
        paste_to_bytes("a\nb", &modes_after(b"\x1b[?2004h")),
        b"\x1b[200~a\rb\x1b[201~"
    );
}
