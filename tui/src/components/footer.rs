//! One-line footer, the last row of every screen (zoom too): a full-width
//! bar, orange while keys drive Rakumux and a muted blue while they go to a
//! program. At the left `session: ● <name>` (in Rakumux mode the selected
//! session, else the one kept in view, or the selected card's while the feed
//! has the keys; in session mode the one taking the keys; with the status
//! mark its list row has, since the detail has no title row), then the
//! connection state and a message (a status message, why the daemon is
//! unreachable, zoom, a pending upgrade); the mode label (`RAKUMUX MODE`,
//! `SESSION MODE` or `TERMINAL MODE`) in the centre; and buttons on the
//! right. In Rakumux mode: the feed (`f ≡ Feed`, with `●N` for the entries
//! that arrived since the feed was last on screen), Help (`?`), Settings
//! (`,`), then the Rakumux account: Sign in while signed out, `✓ Signed in`
//! (the account dialog) while signed in. In session mode the Rakumux key
//! alone, the way out (the label already names the mode). Settings lists
//! every key.
//!
//! The footer never wraps or grows: when the row is short the session
//! gives way first (to its mark and name, then nothing), then the message is
//! cut (down to [`MIN_MESSAGE`] columns), then buttons are dropped from the
//! right (the account, Settings, Help, then the Feed's: it carries the
//! count and is the one tap that shows the feed). The label always shows:
//! centred on the row while it clears the left and the buttons, else between
//! them, in its short words (`RAKUMUX`, `SESSION`, `TERMINAL`) on a row too
//! short for the full ones.
//!
//! The label and the buttons are tap targets, for phones and the mouse:
//! `SESSION MODE` switches to Rakumux mode, `RAKUMUX MODE` does what Esc
//! does (back to the session you came from; while the feed has the keys, the
//! feed's Esc), and a button does what its keys do.

use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};
use session_core::model::SessionView;

use super::format::{activity_badge, status_glyph};
use super::utils::get_spinner_char;
use crate::app::types::{AccountStatus, AppMode, Focus};
use crate::app::{App, Connection, FooterKey, Hit};
use crate::layout::{str_width, truncate};
use crate::theme;

/// Columns a message keeps before buttons make way for it.
const MIN_MESSAGE: usize = 30;

/// Columns between two buttons.
const GAP: usize = 1;

/// Columns kept clear on either side of the mode label.
const CLEAR: usize = 1;

/// Columns before the left's text, as after the buttons.
const MARGIN: usize = 1;

/// Between the session and the message.
const SEPARATOR: &str = "  ";

/// How much darker than the orange bar its buttons are (see
/// [`theme::Palette::faded`]).
const BUTTON_SHADE: f32 = 0.2;

/// A footer button: its keys, what it opens, and what a tap on it does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Button {
    pub keys: String,
    pub label: String,
    pub target: FooterKey,
}

impl Button {
    fn new(keys: impl Into<String>, label: impl Into<String>, target: FooterKey) -> Self {
        Self {
            keys: keys.into(),
            label: label.into(),
            target,
        }
    }

    /// Columns it takes: ` keys label `, or ` keys ` without a label.
    fn width(&self) -> usize {
        match str_width(&self.label) {
            0 => str_width(&self.keys) + 2,
            label => str_width(&self.keys) + label + 3,
        }
    }
}

/// The most new entries the Feed button counts (`●99+` beyond).
const MAX_COUNT: usize = 99;

/// The feed's button: `≡ Feed`, `≡ Feed ●3` with entries that arrived
/// since it was last on screen.
fn feed_button(app: &App) -> Button {
    Button::new(FEED_KEY, feed_label(app), FooterKey::Feed)
}

/// `≡ Feed`, with `●N` for the entries that arrived since the feed was last
/// on screen.
fn feed_label(app: &App) -> String {
    let icon = app.icons().feed();
    match feed_count(app) {
        Some(count) => format!("{icon} Feed {count}"),
        None => format!("{icon} Feed"),
    }
}

/// `●N` (`●99+`) for the entries that arrived since the feed was last on
/// screen, none while there are none: the footer's button and the Feed tab
/// say it.
pub fn feed_count(app: &App) -> Option<String> {
    match app.feed_unseen() {
        0 => None,
        n if n > MAX_COUNT => Some(format!("●{MAX_COUNT}+")),
        n => Some(format!("●{n}")),
    }
}

/// The key that shows, focuses and hides the feed in Rakumux mode.
pub const FEED_KEY: &str = "f";

/// A tap target in the footer: `width` columns from column `x` of the row.
pub type FooterTarget = (u16, u16, Hit);

/// The mode label, in the centre of the footer: a bold `RAKUMUX MODE` on
/// the orange bar while the keys drive Rakumux (the mode to notice), a quiet
/// `SESSION MODE` (`TERMINAL MODE` for the terminal panel) on the blue bar
/// while they go to a program; the short words (`RAKUMUX`) when the full
/// ones don't fit `room` columns.
pub fn mode_label(app: &App, room: usize) -> Span<'static> {
    let p = theme::palette();
    let (word, style) = match app.focus {
        Focus::List => (
            "RAKUMUX",
            Style::default().fg(p.background).bg(p.accent_orange),
        ),
        Focus::Session => (
            "SESSION",
            Style::default().fg(p.neutral_gray).bg(p.selection),
        ),
        Focus::Terminal => (
            "TERMINAL",
            Style::default().fg(p.text_primary).bg(p.selection),
        ),
    };
    let full = format!(" {word} MODE ");
    let content = if str_width(&full) <= room {
        full
    } else {
        format!(" {word} ")
    };
    Span::styled(content, style.add_modifier(Modifier::BOLD))
}

/// The footer's buttons: the feed, Help and Settings in Rakumux mode, and
/// the Rakumux account at the right (once it was checked): Sign in while
/// signed out, `✓ Signed in` while signed in; in session mode the Rakumux
/// key alone, the way out (the label names the mode; the rest are a tap
/// away from there). Nothing while typing a filter (those keys type) or
/// while a dialog is open.
pub fn buttons(app: &App) -> Vec<Button> {
    if !matches!(app.mode, AppMode::Normal) || app.list.filter_editing {
        return Vec::new();
    }
    match app.focus {
        Focus::List => {
            let mut buttons = vec![
                feed_button(app),
                Button::new("?", "Help", FooterKey::Help),
                Button::new(
                    crate::help::SETTINGS_KEY_PRIMARY,
                    "Settings",
                    FooterKey::Settings,
                ),
            ];
            match app.account {
                AccountStatus::SignedOut => {
                    buttons.push(Button::new("a", "Sign in", FooterKey::SignIn))
                }
                AccountStatus::SignedIn(_) => {
                    buttons.push(Button::new("a", "✓ Signed in", FooterKey::Account))
                }
                AccountStatus::Unknown => {}
            }
            buttons
        }
        Focus::Session | Focus::Terminal => {
            vec![Button::new(app.rakumux_key.label(), "", FooterKey::Rakumux)]
        }
    }
}

fn buttons_width(buttons: &[Button]) -> usize {
    let items: usize = buttons.iter().map(Button::width).sum();
    items + GAP * buttons.len().saturating_sub(1)
}

/// A button's keys and label styles on the bar: a darker shade of the
/// orange bar with its dark ink (a dark chip where the palette can't
/// shade), or a dark chip on the blue one, its keys in the accent.
fn button_styles(focus: Focus) -> (Style, Style) {
    let p = theme::palette();
    let (chip, key, label) = match focus {
        Focus::List => match p.faded(p.accent_orange, BUTTON_SHADE) {
            Some(shade) => (shade, p.background, p.background),
            None => (p.background, p.accent_orange, p.text_primary),
        },
        Focus::Session | Focus::Terminal => (p.background, p.accent_primary, p.text_primary),
    };
    let chip = Style::default().bg(chip);
    (chip.fg(key).add_modifier(Modifier::BOLD), chip.fg(label))
}

/// Add `buttons` to `spans`, the first at column `x`. Returns where each
/// one is.
fn push_buttons(
    spans: &mut Vec<Span<'static>>,
    buttons: &[Button],
    x: usize,
    focus: Focus,
) -> Vec<FooterTarget> {
    let (key_style, label_style) = button_styles(focus);
    let mut targets = Vec::with_capacity(buttons.len());
    let mut x = x;
    for (i, button) in buttons.iter().enumerate() {
        if i > 0 {
            spans.push(Span::raw(" ".repeat(GAP)));
            x += GAP;
        }
        let width = button.width();
        targets.push((
            u16::try_from(x).unwrap_or(u16::MAX),
            u16::try_from(width).unwrap_or(u16::MAX),
            Hit::FooterKey(button.target),
        ));
        x += width;
        if button.label.is_empty() {
            spans.push(Span::styled(format!(" {} ", button.keys), key_style));
        } else {
            spans.push(Span::styled(format!(" {}", button.keys), key_style));
            spans.push(Span::styled(format!(" {} ", button.label), label_style));
        }
    }
    targets
}

/// The session the footer names at its left: in Rakumux mode the
/// selected one, else the one the detail keeps in view (the tabs, a header
/// or + New session selected), or while the feed has the keys the selected
/// card's when it runs here; the one taking the keys in session mode; none
/// for the terminal panel, which isn't a session.
fn named_session(app: &App) -> Option<&SessionView> {
    let in_view = || app.detail.session_id().and_then(|id| app.list.get(id));
    match app.focus {
        Focus::List if app.feed_has_keys() => app.feed_session(),
        Focus::List => app.list.selected_session().or_else(in_view),
        Focus::Session => in_view(),
        Focus::Terminal => None,
    }
}

/// `session: ● name`, the mark being the one its list row shows (its
/// activity, else its status), in at most `room` columns: the mark and the
/// name when that's all that fits, nothing under a few columns of it.
fn session_label(session: &SessionView, room: usize, focus: Focus) -> Vec<Span<'static>> {
    let p = theme::palette();
    let (mark, mark_style) = activity_badge(session).unwrap_or_else(|| status_glyph(session));
    // Its own colour on the blue bar, a muted one lifted to stay readable
    // there; the orange bar inks everything dark.
    let mark_style = if mark_style.fg == Some(p.text_muted) {
        mark_style.fg(p.neutral_gray)
    } else {
        mark_style
    };
    let name = match &session.machine {
        Some(_) => format!(
            "{} {}",
            crate::components::session_list::MACHINE_MARK,
            session.name
        ),
        None => session.name.clone(),
    };
    let name = name.as_str();
    let mark_width = str_width(mark) + 1;
    let (label, strong) = if focus == Focus::List {
        (
            Style::default(),
            Style::default().add_modifier(Modifier::BOLD),
        )
    } else {
        (
            Style::default().fg(p.neutral_gray),
            Style::default()
                .fg(p.text_bright)
                .add_modifier(Modifier::BOLD),
        )
    };
    if str_width("session: ") + mark_width + str_width(name) <= room {
        return vec![
            Span::styled("session: ", label),
            Span::styled(mark, mark_style),
            Span::raw(" "),
            Span::styled(name.to_string(), strong),
        ];
    }
    if room >= mark_width + 3 {
        return vec![
            Span::styled(mark, mark_style),
            Span::raw(" "),
            Span::styled(truncate(name, room - mark_width), strong),
        ];
    }
    Vec::new()
}

/// What follows the session at the left: the connection state while not
/// connected, then a status message, or why the daemon is unreachable,
/// debug details, zoom or a pending upgrade. Empty when there's nothing to
/// say.
fn message(app: &App) -> Vec<Span<'static>> {
    let p = theme::palette();
    // Readable on the blue bar; the orange one recolours everything.
    let muted = Style::default().fg(p.neutral_gray);
    let mut spans = match &app.connection {
        Connection::Connected(_) => Vec::new(),
        Connection::Connecting => vec![Span::styled(
            format!("{} connecting", get_spinner_char()),
            Style::default().fg(p.accent_primary),
        )],
        Connection::Failed(_) => vec![Span::styled(
            "✕ daemon unavailable",
            Style::default().fg(p.accent_pink),
        )],
    };
    let detail = if let Some(message) = app.status() {
        vec![Span::styled(
            message.to_string(),
            Style::default().fg(p.text_primary),
        )]
    } else if let Connection::Failed(error) = &app.connection {
        vec![Span::styled(
            error.message.clone(),
            Style::default().fg(p.accent_pink),
        )]
    } else if app.debug {
        let attachment = app
            .detail
            .attachment
            .as_ref()
            .map(|a| format!("{:?} offset {}", a.role, a.offset))
            .unwrap_or_else(|| "none".into());
        let (cols, rows) = app.terminal_size();
        vec![Span::styled(
            format!(
                "debug: view {:?} · generation {} · attach {attachment} · pane {cols}x{rows}",
                app.detail.view, app.detail.generation
            ),
            Style::default().fg(p.accent_orange),
        )]
    } else if app.zoomed && app.focus == Focus::Session {
        // The session itself is named before it.
        vec![Span::styled("zoomed", muted)]
    } else if let Some(version) = &app.pending_update_version {
        vec![Span::styled(
            format!("Update {version} available · U to install"),
            Style::default().fg(p.accent_gold),
        )]
    } else {
        Vec::new()
    };
    if !spans.is_empty() && !detail.is_empty() {
        spans.push(Span::styled(" · ", muted));
    }
    spans.extend(detail);
    spans
}

fn spans_width(spans: &[Span]) -> usize {
    spans.iter().map(|s| str_width(&s.content)).sum()
}

/// `spans` cut to at most `max` columns, the cut one ending in `…`.
fn fit(spans: Vec<Span<'static>>, max: usize) -> Vec<Span<'static>> {
    let mut out = Vec::new();
    let mut used = 0;
    for span in spans {
        let width = str_width(&span.content);
        if used + width <= max {
            used += width;
            out.push(span);
            continue;
        }
        let cut = truncate(&span.content, max - used);
        if !cut.is_empty() {
            out.push(Span::styled(cut, span.style));
        }
        break;
    }
    out
}

/// How many of `buttons` to keep beside a mode label `label` columns wide:
/// as many as leave the message its room at the left (all of it, or
/// [`MIN_MESSAGE`] columns of it). Returns `(kept, room for the left's
/// text)`, the session and the message.
fn keep_buttons(width: usize, label: usize, message: usize, buttons: &[Button]) -> (usize, usize) {
    // The left's text beside `right` columns: after the margin, with a
    // column clear on either side of the label.
    let room = |right: usize| width.checked_sub(MARGIN + CLEAR + label + CLEAR + right);
    for kept in (1..=buttons.len()).rev() {
        // The buttons and a space after them.
        let right = buttons_width(&buttons[..kept]) + 1;
        match room(right) {
            Some(room) if room >= message.min(MIN_MESSAGE) => return (kept, room),
            // Nothing to say: the label and the buttons alone.
            _ if message == 0 && label + CLEAR + right <= width => return (kept, 0),
            _ => {}
        }
    }
    (0, width.saturating_sub(MARGIN + CLEAR + label))
}

/// The footer line for a row `width` columns wide: the session and the
/// message at the left, the mode label in the centre and the buttons on the
/// right. It always fits the row (but for a label wider than the row): the
/// session gives way first, then the message is cut, down to
/// [`MIN_MESSAGE`] columns, then the buttons go from the right.
#[cfg(test)]
pub fn footer_line(app: &App, width: usize) -> Line<'static> {
    footer(app, width).0
}

/// [`footer_line`] and where its tap targets are: the label, then each
/// button shown. The label, the buttons and a message come first; the
/// session takes what's left at the left.
fn footer(app: &App, width: usize) -> (Line<'static>, Vec<FooterTarget>) {
    let mode = mode_label(app, width);
    let mode_width = str_width(&mode.content);
    let message = message(app);
    let buttons = buttons(app);
    let (kept, room) = keep_buttons(width, mode_width, spans_width(&message), &buttons);
    let buttons = &buttons[..kept];
    let message = fit(message, room);
    let mut left = match named_session(app) {
        Some(session) => {
            let taken = match spans_width(&message) {
                0 => 0,
                used => used + str_width(SEPARATOR),
            };
            session_label(session, room.saturating_sub(taken), app.focus)
        }
        None => Vec::new(),
    };
    if !left.is_empty() && !message.is_empty() {
        left.push(Span::raw(SEPARATOR));
    }
    left.extend(message);
    let mut spans = Vec::new();
    if !left.is_empty() {
        spans.push(Span::raw(" ".repeat(MARGIN)));
        spans.extend(left);
    }
    let left = spans_width(&spans);
    // Where the buttons start, or the row's end.
    let right = match buttons_width(buttons) {
        0 => width,
        used => width.saturating_sub(used + 1),
    };
    // The label: centred on the row, else as near it as it gets while
    // clear of the left and the buttons.
    let first = if left == 0 { 0 } else { left + CLEAR };
    let end = if buttons.is_empty() {
        right
    } else {
        right.saturating_sub(CLEAR)
    };
    let last = end.saturating_sub(mode_width).max(first);
    let x = (width.saturating_sub(mode_width) / 2).clamp(first, last);
    if x > left {
        spans.push(Span::raw(" ".repeat(x - left)));
    }
    let mut targets = vec![(
        u16::try_from(x).unwrap_or(u16::MAX),
        u16::try_from(mode_width.min(width.saturating_sub(x))).unwrap_or(u16::MAX),
        Hit::ModeBadge,
    )];
    spans.push(mode);
    if app.focus == Focus::List {
        // Dark ink on the orange bar; alerts stand out in bold.
        let palette = theme::palette();
        for span in spans.iter_mut() {
            let alert = span.style.fg == Some(palette.accent_pink);
            span.style = span.style.fg(palette.background).bg(palette.accent_orange);
            if alert {
                span.style = span.style.add_modifier(Modifier::BOLD);
            }
        }
    }
    if !buttons.is_empty() {
        let used = spans_width(&spans);
        spans.push(Span::raw(" ".repeat(right.saturating_sub(used))));
        let x = spans_width(&spans);
        targets.extend(push_buttons(&mut spans, buttons, x, app.focus));
        spans.push(Span::raw(" "));
    }
    (Line::from(spans), targets)
}

/// The bar: orange with dark text in Rakumux mode, muted blue (the label's
/// own) while a program takes the keys.
fn footer_style(app: &App) -> Style {
    let palette = theme::palette();
    if app.focus == Focus::List {
        Style::default()
            .fg(palette.background)
            .bg(palette.accent_orange)
    } else {
        Style::default()
            .fg(palette.text_primary)
            .bg(palette.selection)
    }
}

/// Draws the footer into the first row of `area`; it never takes more.
/// Records its tap targets (the label and the buttons) in `hits`.
pub fn draw_footer(f: &mut Frame, app: &App, area: Rect, hits: &mut Vec<(Rect, Hit)>) {
    if area.height == 0 {
        return;
    }
    let row = Rect { height: 1, ..area };
    let (line, targets) = footer(app, area.width as usize);
    f.render_widget(Paragraph::new(line).style(footer_style(app)), row);
    for (x, width, hit) in targets {
        let target = Rect {
            x: row.x.saturating_add(x),
            width,
            ..row
        }
        .intersection(row);
        if !target.is_empty() {
            hits.push((target, hit));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::testing::{harness, view};
    use crossterm::event::KeyCode;
    use session_core::api::{ApiError, ErrorCode};

    fn text(line: &Line) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    /// Whether `needle` is in the middle of `line`, a row `width` columns
    /// wide (a line without buttons isn't padded to its end).
    fn centred_in(line: &str, needle: &str, width: usize) -> bool {
        let Some(at) = line.find(needle) else {
            return false;
        };
        let start = str_width(&line[..at]);
        let middle = (width - str_width(needle)) / 2;
        start.abs_diff(middle) <= 1
    }

    /// [`centred_in`] a full row.
    fn centred(line: &str, needle: &str) -> bool {
        centred_in(line, needle, str_width(line))
    }

    /// Draw the footer `width` columns wide at (2, 1) on a larger screen;
    /// returns the buffer and the button columns.
    fn draw_at(app: &App, width: u16) -> (ratatui::buffer::Buffer, Vec<Rect>) {
        use ratatui::{backend::TestBackend, widgets::Block, Terminal};
        let palette = theme::palette();
        let mut terminal = Terminal::new(TestBackend::new(width + 4, 3)).unwrap();
        let mut hits = Vec::new();
        terminal
            .draw(|frame| {
                frame.render_widget(
                    Block::default().style(Style::default().bg(palette.background)),
                    frame.size(),
                );
                draw_footer(frame, app, Rect::new(2, 1, width, 1), &mut hits);
            })
            .unwrap();
        let buttons = hits
            .iter()
            .filter(|(_, hit)| matches!(hit, Hit::FooterKey(_)))
            .map(|(area, _)| *area)
            .collect();
        (terminal.backend().buffer().clone(), buttons)
    }

    /// A footer row: each `(x, text)` from column `x`, spaces between.
    fn row(parts: &[(usize, &str)]) -> String {
        let mut row = String::new();
        for (x, text) in parts {
            let used = str_width(&row);
            assert!(*x >= used, "{x} overlaps {row:?}");
            row.push_str(&" ".repeat(x - used));
            row.push_str(text);
        }
        row
    }

    /// Rakumux mode's buttons, signed in: 49 columns with the space after.
    const ALL_BUTTONS: &str = " f ≡ Feed   ? Help   , Settings   a ✓ Signed in  ";

    /// The way out of session mode: 9 columns with the space after.
    const WAY_OUT: &str = " Ctrl-\\  ";

    #[tokio::test]
    async fn desktop_narrow_and_phone_rows_in_every_mode() {
        let mut h = harness(vec![view("A", "api", 10)]);
        let line = |h: &crate::app::testing::Harness, width| text(&footer_line(&h.app, width));
        // Rakumux mode: the session at the left, the label in the centre
        // until the buttons reach it, then just before them; a phone keeps
        // the mark and name, the Feed and Help.
        let session = " session: ● api";
        let label = " RAKUMUX MODE ";
        assert_eq!(
            line(&h, 160),
            row(&[(0, session), (73, label), (111, ALL_BUTTONS)])
        );
        assert_eq!(
            line(&h, 100),
            row(&[(0, session), (36, label), (51, ALL_BUTTONS)])
        );
        let phone = " f ≡ Feed   ? Help  ";
        assert_eq!(
            line(&h, 45),
            row(&[(0, " ● api"), (10, label), (25, phone)])
        );
        assert_eq!(line(&h, 40), row(&[(5, label), (20, phone)]));
        // Session mode: the label in the centre, the Rakumux key alone on
        // the right.
        h.press(KeyCode::Enter).await;
        let label = " SESSION MODE ";
        assert_eq!(
            line(&h, 160),
            row(&[(0, session), (73, label), (151, WAY_OUT)])
        );
        assert_eq!(
            line(&h, 100),
            row(&[(0, session), (43, label), (91, WAY_OUT)])
        );
        assert_eq!(
            line(&h, 45),
            row(&[(0, session), (16, label), (36, WAY_OUT)])
        );
        // The terminal panel names no session.
        h.app.focus = Focus::Terminal;
        let label = " TERMINAL MODE ";
        assert_eq!(line(&h, 160), row(&[(72, label), (151, WAY_OUT)]));
        assert_eq!(line(&h, 100), row(&[(42, label), (91, WAY_OUT)]));
        assert_eq!(line(&h, 45), row(&[(15, label), (36, WAY_OUT)]));
    }

    #[test]
    fn rakumux_bar_fills_the_row_with_readable_ink_in_all_message_states() {
        let palette = theme::palette();
        for state in ["empty", "filter", "connecting", "error", "message"] {
            let mut h = harness(vec![view("A", "api", 10)]);
            h.app.list.filter_editing = state == "filter";
            if state == "connecting" {
                h.app.connection = Connection::Connecting;
            }
            if state == "error" {
                h.app.connection = Connection::Failed(ApiError::new(
                    ErrorCode::DaemonUnavailable,
                    "daemon failed",
                ));
            }
            if state == "message" {
                h.app.set_status("A long status message ".repeat(20));
            }
            for width in [1, 9, 40, 80, 160] {
                let (buffer, buttons) = draw_at(&h.app, width);
                for x in 2..width + 2 {
                    let cell = buffer.get(x, 1);
                    if buttons.iter().any(|b| x >= b.x && x < b.right()) {
                        // A darker orange, with the same dark ink.
                        let (key, _) = button_styles(Focus::List);
                        assert_eq!(Some(cell.bg), key.bg, "{state}: col {x}");
                        assert_ne!(cell.bg, palette.background, "{state}: col {x}");
                        assert_eq!(cell.fg, palette.background, "{state}: col {x}");
                        continue;
                    }
                    assert_eq!(cell.bg, palette.accent_orange, "{state}: col {x}");
                    assert_eq!(cell.fg, palette.background, "{state}: col {x}");
                }
                assert_eq!(buffer.get(2, 0).bg, palette.background);
                assert_eq!(buffer.get(0, 1).bg, palette.background);
            }
        }
    }

    #[test]
    fn session_and_terminal_modes_fill_the_row_with_muted_blue() {
        let mut h = harness(vec![view("A", "api", 10)]);
        let palette = theme::palette();
        for focus in [Focus::Session, Focus::Terminal] {
            h.app.focus = focus;
            for width in [9, 40, 160] {
                let (buffer, buttons) = draw_at(&h.app, width);
                assert_eq!(buttons.len(), usize::from(width >= 40), "{focus:?} {width}");
                for x in 2..width + 2 {
                    let on_button = buttons.iter().any(|b| x >= b.x && x < b.right());
                    let bg = if on_button {
                        palette.background
                    } else {
                        palette.selection
                    };
                    assert_eq!(buffer.get(x, 1).bg, bg, "{focus:?} {width}: col {x}");
                }
                // Only the footer's row.
                assert_eq!(buffer.get(2, 0).bg, palette.background);
                assert_eq!(buffer.get(0, 1).bg, palette.background);
            }
        }
    }

    #[tokio::test]
    async fn the_label_takes_the_centre_and_buttons_end_the_line() {
        let mut h = harness(vec![view("A", "api", 10)]);
        // The account's button has its own test.
        h.app.account = crate::app::types::AccountStatus::Unknown;
        let p = theme::palette();
        // Rakumux mode: the session, the label in the centre, then the
        // buttons.
        let line = footer_line(&h.app, 90);
        let shown = text(&line);
        assert!(shown.starts_with(" session: ● api "), "{shown:?}");
        assert!(centred(&shown, " RAKUMUX MODE "), "{shown:?}");
        assert!(shown.ends_with(" ? Help   , Settings  "), "{shown:?}");
        assert_eq!(str_width(&shown), 90, "{shown:?}");
        let label = line
            .spans
            .iter()
            .find(|s| s.content == " RAKUMUX MODE ")
            .unwrap();
        assert_eq!(label.style.bg, Some(p.accent_orange));
        assert_eq!(label.style.fg, Some(p.background));
        assert!(label.style.add_modifier.contains(Modifier::BOLD));
        let key = line.spans.iter().find(|s| s.content == " ?").unwrap();
        assert_eq!(key.style.fg, Some(p.background));
        assert_eq!(key.style.bg, p.faded(p.accent_orange, BUTTON_SHADE));
        assert!(key.style.add_modifier.contains(Modifier::BOLD));
        // Session mode: a quiet label, and the Rakumux key alone: the
        // label already says the mode.
        h.press(KeyCode::Enter).await;
        let line = footer_line(&h.app, 80);
        let shown = text(&line);
        assert!(shown.starts_with(" session: ● api "), "{shown:?}");
        assert!(centred(&shown, " SESSION MODE "), "{shown:?}");
        assert!(shown.ends_with(" Ctrl-\\  "), "{shown:?}");
        for gone in ["Help", "Settings", "Rakumux"] {
            assert!(!shown.contains(gone), "{shown:?}");
        }
        let label = line
            .spans
            .iter()
            .find(|s| s.content == " SESSION MODE ")
            .unwrap();
        assert_eq!(label.style.bg, Some(p.selection));
        assert_eq!(label.style.fg, Some(p.neutral_gray));
        let key = line
            .spans
            .iter()
            .find(|s| s.content == " Ctrl-\\ ")
            .unwrap();
        assert_eq!(key.style.fg, Some(p.accent_primary));
        assert_eq!(key.style.bg, Some(p.background));
        // While typing a filter those keys type, so none show; the label
        // and the selected session stay.
        h.rakumux().await;
        h.press(KeyCode::Char('/')).await;
        assert!(buttons(&h.app).is_empty());
        let line = text(&footer_line(&h.app, 60));
        assert!(line.starts_with(" session: ● api "), "{line:?}");
        assert!(centred_in(&line, " RAKUMUX MODE ", 60), "{line:?}");
    }

    #[tokio::test]
    async fn messages_follow_the_session_and_are_cut_before_buttons_go() {
        let mut h = harness(vec![view("A", "api", 10)]);
        // The account's button has its own test.
        h.app.account = crate::app::types::AccountStatus::Unknown;
        h.app.set_status("Layout: Tabs");
        let line = text(&footer_line(&h.app, 90));
        assert!(
            line.starts_with(" session: ● api  Layout: Tabs "),
            "{line:?}"
        );
        assert!(centred(&line, " RAKUMUX MODE "), "{line:?}");
        assert!(line.ends_with(" ? Help   , Settings  "), "{line:?}");
        // Short of room the label leaves the centre, just before the
        // buttons…
        let line = text(&footer_line(&h.app, 80));
        assert!(
            line.starts_with(" session: ● api  Layout: Tabs    RAKUMUX MODE   f ≡ Feed "),
            "{line:?}"
        );
        // …then the session gives way: its name alone, then nothing.
        let line = text(&footer_line(&h.app, 72));
        assert!(line.starts_with(" ● api  Layout: Tabs "), "{line:?}");
        assert!(line.ends_with(" ? Help   , Settings  "), "{line:?}");
        let line = text(&footer_line(&h.app, 64));
        assert!(
            line.starts_with(" Layout: Tabs    RAKUMUX MODE "),
            "{line:?}"
        );
        assert!(line.ends_with(" ? Help   , Settings  "), "{line:?}");
        // Then the buttons go from the right: Settings, then Help.
        let line = text(&footer_line(&h.app, 50));
        assert!(line.starts_with(" Layout: Tabs "), "{line:?}");
        assert!(line.ends_with(" f ≡ Feed   ? Help  "), "{line:?}");
        // A long message is cut to keep the buttons while it keeps 30
        // columns (the session has given way already)…
        let long = "A status message long enough to need the whole line here, and then some";
        h.app.set_status(long);
        let line = text(&footer_line(&h.app, 100));
        assert!(line.starts_with(" A status message"), "{line:?}");
        assert!(line.contains("…  RAKUMUX MODE "), "{line:?}");
        assert!(line.ends_with(" ? Help   , Settings  "), "{line:?}");
        assert_eq!(str_width(&line), 100, "{line:?}");
        // …then the buttons go…
        let line = text(&footer_line(&h.app, 70));
        assert!(
            line.ends_with(" RAKUMUX MODE   f ≡ Feed   ? Help  "),
            "{line:?}"
        );
        // …and the message takes the row up to the label.
        let line = text(&footer_line(&h.app, 50));
        assert!(line.starts_with(" A status message"), "{line:?}");
        assert!(line.ends_with("…  RAKUMUX MODE "), "{line:?}");
        assert_eq!(str_width(&line), 50, "{line:?}");
        // With room to spare the session comes back.
        let line = text(&footer_line(&h.app, 200));
        assert!(
            line.starts_with(" session: ● api  A status message"),
            "{line:?}"
        );
        assert!(centred(&line, " RAKUMUX MODE "), "{line:?}");
    }

    #[tokio::test]
    async fn the_line_always_fits_and_the_label_always_shows() {
        let mut h = harness(vec![view("A", "api", 10)]);
        let expire = |h: &mut crate::app::testing::Harness| {
            h.app
                .on_tick(std::time::Instant::now() + std::time::Duration::from_secs(60));
        };
        for focus in [Focus::List, Focus::Session, Focus::Terminal] {
            if focus == Focus::Session {
                h.press(KeyCode::Enter).await;
            }
            h.app.focus = focus;
            let word = match focus {
                Focus::List => " RAKUMUX ",
                Focus::Session => " SESSION ",
                Focus::Terminal => " TERMINAL ",
            };
            let full = format!("{} MODE ", word.trim_end());
            for status in [
                None,
                Some("Renamed to a-much-longer-session-name-than-usual"),
            ] {
                match status {
                    Some(status) => h.app.set_status(status),
                    None => expire(&mut h),
                }
                for width in 0..=160 {
                    let (line, targets) = footer(&h.app, width);
                    let line = text(&line);
                    // The full words wherever they fit, else the short.
                    let label = if width >= str_width(&full) {
                        full.as_str()
                    } else {
                        word
                    };
                    let (x, _, hit) = targets[0];
                    assert_eq!(hit, Hit::ModeBadge);
                    let at = line.chars().skip(usize::from(x)).collect::<String>();
                    assert!(at.starts_with(label), "{focus:?} {width}: {line:?}");
                    if width >= str_width(word) {
                        assert!(str_width(&line) <= width, "{width}: {line:?}");
                    }
                }
            }
        }
        // In a session, a message keeps its room before the way out shows.
        h.app.focus = Focus::Session;
        h.app
            .set_status("Renamed to a-much-longer-session-name-than-usual");
        let line = text(&footer_line(&h.app, 40));
        assert!(line.contains("Renamed"), "{line:?}");
        assert!(!line.contains("Ctrl-\\"), "{line:?}");
        expire(&mut h);
        let line = text(&footer_line(&h.app, 40));
        assert!(line.ends_with(" SESSION MODE   Ctrl-\\  "), "{line:?}");
    }

    #[tokio::test]
    async fn a_phone_keeps_the_label_the_feed_and_help_longest() {
        let mut h = harness(vec![view("A", "api", 10)]);
        // Rakumux mode: the Feed, Help and Settings fit 60 columns (the
        // account goes first)…
        let line = text(&footer_line(&h.app, 60));
        assert!(
            line.ends_with(" RAKUMUX MODE   f ≡ Feed   ? Help   , Settings  "),
            "{line:?}"
        );
        // …then Settings, then Help: the Feed's button stays longest…
        for width in [45, 40] {
            let line = text(&footer_line(&h.app, width));
            assert!(line.ends_with(" f ≡ Feed   ? Help  "), "{line:?}");
        }
        let line = text(&footer_line(&h.app, 26));
        assert_eq!(line, " RAKUMUX MODE   f ≡ Feed  ");
        // …and the label longer still, in its short word when even its
        // full ones don't fit.
        assert_eq!(text(&footer_line(&h.app, 20)), "    RAKUMUX MODE ");
        assert_eq!(text(&footer_line(&h.app, 12)), "  RAKUMUX ");
        // Session mode: the session, the label and the way out fit a
        // phone.
        h.press(KeyCode::Enter).await;
        let line = text(&footer_line(&h.app, 40));
        assert_eq!(line, " session: ● api  SESSION MODE   Ctrl-\\  ");
        // The targets match what's shown.
        let (line, targets) = footer(&h.app, 40);
        let shown: Vec<char> = text(&line).chars().collect();
        let at = |(x, width, _): &FooterTarget| -> String {
            shown[usize::from(*x)..usize::from(x + width)]
                .iter()
                .collect()
        };
        assert_eq!(targets.len(), 2);
        assert_eq!(at(&targets[0]), " SESSION MODE ");
        assert_eq!(targets[0].2, Hit::ModeBadge);
        assert_eq!(at(&targets[1]), " Ctrl-\\ ");
        assert_eq!(targets[1].2, Hit::FooterKey(FooterKey::Rakumux));
    }

    #[test]
    fn the_feed_button_counts_new_replies_up_to_ninety_nine() {
        use rakumux_sensagent::feed::{FeedEntry, FeedPage, FeedStatus};
        let mut h = harness(vec![view("A", "api", 10)]);
        h.app.account = AccountStatus::Unknown;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64;
        let entry = |i: i64| FeedEntry {
            id: format!("E{i}"),
            seq: Some(i as u64 + 1),
            machine: "lab".into(),
            session_id: "S".into(),
            session_name: "s".into(),
            folder: None,
            branch: None,
            agent: "codex".into(),
            conversation_id: None,
            text: "Done.".into(),
            truncated: false,
            finished_at: now + 1_000 + i,
            position: None,
            reply_to: None,
            thread: None,
            prompt: None,
        };
        let page = |entries: Vec<FeedEntry>| FeedPage {
            status: FeedStatus::Ready,
            generation: 1,
            revision: entries.len() as u64,
            entries,
            replies: Vec::new(),
            more: false,
            open: None,
            machine: None,
        };
        h.app.feed.apply(page((0..3).map(entry).collect()));
        // Counted only while the account keeps the feed (signed in, on).
        assert_eq!(buttons(&h.app)[0].label, "≡ Feed");
        h.app.account = AccountStatus::SignedIn(crate::account::Account {
            feed: Some(true),
            ..crate::app::testing::FakeAccount::person()
        });
        assert_eq!(buttons(&h.app)[0].label, "≡ Feed ●3");
        h.app.feed.apply(page((0..120).map(entry).collect()));
        assert_eq!(buttons(&h.app)[0].label, "≡ Feed ●99+");
        assert_eq!(buttons(&h.app)[0].target, FooterKey::Feed);
    }

    #[tokio::test]
    async fn connecting_and_an_unreachable_daemon_show_after_the_session() {
        let mut h = harness(vec![view("A", "api", 10)]);
        // The account's button has its own test.
        h.app.account = crate::app::types::AccountStatus::Unknown;
        h.app.connection = Connection::Connecting;
        let line = text(&footer_line(&h.app, 90));
        assert!(line.starts_with(" session: ● api  "), "{line:?}");
        assert!(line.contains(" connecting "), "{line:?}");
        assert!(centred(&line, " RAKUMUX MODE "), "{line:?}");
        assert!(line.ends_with(" ? Help   , Settings  "), "{line:?}");
        h.app.connection = Connection::Failed(ApiError::new(
            ErrorCode::DaemonUnavailable,
            "can't start rakumux-ptyd",
        ));
        let error = " ✕ daemon unavailable · can't start rakumux-ptyd ";
        let line = text(&footer_line(&h.app, 120));
        assert!(
            line.starts_with(&format!(" session: ● api {error}")),
            "{line:?}"
        );
        // Short of room the session goes first.
        let line = text(&footer_line(&h.app, 100));
        assert!(line.starts_with(error), "{line:?}");
        // A status message takes the error's place; the state stays.
        h.app.set_status("Reconnecting…");
        let line = text(&footer_line(&h.app, 120));
        assert!(
            line.starts_with(" session: ● api  ✕ daemon unavailable · Reconnecting… "),
            "{line:?}"
        );
    }

    #[tokio::test]
    async fn the_left_names_the_selected_session_or_the_one_taking_the_keys() {
        let mut h = harness(vec![view("A", "api", 20), view("B", "docs", 10)]);
        // The account's button has its own test.
        h.app.account = crate::app::types::AccountStatus::Unknown;
        let p = theme::palette();
        let line = footer_line(&h.app, 80);
        assert!(
            text(&line).starts_with(" session: ● api "),
            "{:?}",
            text(&line)
        );
        let name = line.spans.iter().find(|s| s.content == "api").unwrap();
        assert!(name.style.add_modifier.contains(Modifier::BOLD));
        assert_eq!(name.style.fg, Some(p.background));
        h.press(KeyCode::Up).await;
        let shown = text(&footer_line(&h.app, 80));
        assert!(shown.starts_with(" session: ● docs "), "{shown:?}");
        // No session selected (+ New session): the one still in view.
        h.press(KeyCode::Down).await;
        h.press(KeyCode::Down).await;
        assert_eq!(h.app.list.selected_session(), None);
        let in_view = h.app.detail.session_id().cloned().expect("in view");
        let name = &h.app.list.get(&in_view).unwrap().name;
        let shown = text(&footer_line(&h.app, 80));
        assert!(
            shown.starts_with(&format!(" session: ● {name} ")),
            "{shown:?}"
        );
        // In a session: the one taking the keys, bright on the blue bar.
        h.press(KeyCode::Up).await;
        h.press(KeyCode::Up).await;
        h.press(KeyCode::Enter).await;
        let line = footer_line(&h.app, 80);
        assert!(
            text(&line).starts_with(" session: ● docs "),
            "{:?}",
            text(&line)
        );
        let name = line.spans.iter().find(|s| s.content == "docs").unwrap();
        assert_eq!(name.style.fg, Some(p.text_bright));
        // The mark keeps its own colour on the blue bar.
        let mark = line.spans.iter().find(|s| s.content == "●").unwrap();
        assert_eq!(mark.style.fg, Some(p.accent_green));
        // The terminal panel isn't a session: nothing at the left.
        h.app.focus = Focus::Terminal;
        let shown = text(&footer_line(&h.app, 80));
        assert!(!shown.contains("session:"), "{shown:?}");
        assert!(centred(&shown, " TERMINAL MODE "), "{shown:?}");
    }
}
