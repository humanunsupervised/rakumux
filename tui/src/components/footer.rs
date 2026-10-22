//! One-line footer, the last row of every screen (zoom too): a full-width
//! bar, orange while keys drive Rakumux and a muted blue while they go to a
//! program. The mode badge (`RAKUMUX`, `SESSION` or `TERMINAL`) is at the
//! left, then the connection state and a message (a status message, why
//! the daemon is unreachable, zoom, a pending upgrade); `session: <name>`
//! in the centre (the selected session in Rakumux mode, the one taking the
//! keys in session mode); and buttons on the right: Help (`?`) and Settings
//! (`,`) in Rakumux mode, the way to Rakumux mode (the Rakumux key) in
//! session mode. Settings lists every key.
//!
//! The footer never wraps or grows: when the row is short the session
//! gives way first (to its name alone, then nothing), then the message is
//! cut (down to [`MIN_MESSAGE`] columns), then buttons are dropped from the
//! right (Settings first); the badge always shows.
//!
//! The badge and the buttons are tap targets, for phones and the mouse: the
//! `SESSION` badge switches to Rakumux mode, the `RAKUMUX` badge goes back
//! to the session you came from, and a button does what its keys do.

use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};
use session_core::model::SessionView;

use super::utils::get_spinner_char;
use crate::app::types::{AppMode, Focus};
use crate::app::{App, Connection, FooterKey, Hit};
use crate::layout::{str_width, truncate};
use crate::theme;

/// Columns a message keeps before buttons make way for it.
const MIN_MESSAGE: usize = 30;

/// Columns between two buttons.
const GAP: usize = 1;

/// How much darker than the orange bar its buttons are (see
/// [`theme::Palette::faded`]).
const BUTTON_SHADE: f32 = 0.2;

/// A footer button: its keys, what it opens, and what a tap on it does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Button {
    pub keys: String,
    pub label: &'static str,
    pub target: FooterKey,
}

impl Button {
    fn new(keys: impl Into<String>, label: &'static str, target: FooterKey) -> Self {
        Self {
            keys: keys.into(),
            label,
            target,
        }
    }

    /// Columns it takes: ` keys label `.
    fn width(&self) -> usize {
        str_width(&self.keys) + str_width(self.label) + 3
    }
}

/// A tap target in the footer: `width` columns from column `x` of the row.
pub type FooterTarget = (u16, u16, Hit);

/// The mode badge: a bright orange `RAKUMUX` while the keys drive
/// Rakumux (the mode to notice), a quiet `SESSION` in the blue bar while
/// they go to a session.
pub fn mode_badge(app: &App) -> Span<'static> {
    let p = theme::palette();
    match app.focus {
        Focus::List => Span::styled(
            " RAKUMUX ",
            Style::default()
                .fg(p.background)
                .bg(p.accent_orange)
                .add_modifier(Modifier::BOLD),
        ),
        Focus::Session => Span::styled(
            " SESSION ",
            Style::default()
                .fg(p.neutral_gray)
                .bg(p.selection)
                .add_modifier(Modifier::BOLD),
        ),
        Focus::Terminal => Span::styled(
            " TERMINAL ",
            Style::default()
                .fg(p.text_primary)
                .bg(p.selection)
                .add_modifier(Modifier::BOLD),
        ),
    }
}

/// The footer's buttons: Help and Settings in Rakumux mode; in session
/// mode the Rakumux key, the way out (Help and Settings are a tap away
/// from there). Nothing while typing a filter (those keys type) or while a
/// dialog is open.
pub fn buttons(app: &App) -> Vec<Button> {
    if !matches!(app.mode, AppMode::Normal) || app.list.filter_editing {
        return Vec::new();
    }
    match app.focus {
        Focus::List => vec![
            Button::new("?", "Help", FooterKey::Help),
            Button::new(
                crate::help::SETTINGS_KEY_PRIMARY,
                "Settings",
                FooterKey::Settings,
            ),
        ],
        Focus::Session | Focus::Terminal => vec![Button::new(
            app.rakumux_key.label(),
            "Rakumux",
            FooterKey::Rakumux,
        )],
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
        spans.push(Span::styled(format!(" {}", button.keys), key_style));
        spans.push(Span::styled(format!(" {} ", button.label), label_style));
    }
    targets
}

/// The session the footer names in its centre: the selected one in Rakumux
/// mode, the one taking the keys in session mode; none for the terminal
/// panel, which isn't a session.
fn centred_session(app: &App) -> Option<&SessionView> {
    match app.focus {
        Focus::List => app.list.selected_session(),
        Focus::Session => app.detail.session_id().and_then(|id| app.list.get(id)),
        Focus::Terminal => None,
    }
}

/// `session: name` in at most `room` columns: the name alone when that's
/// all that fits, nothing under a few columns of it.
fn session_label(name: &str, room: usize, focus: Focus) -> Vec<Span<'static>> {
    let p = theme::palette();
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
    if str_width("session: ") + str_width(name) <= room {
        return vec![
            Span::styled("session: ", label),
            Span::styled(name.to_string(), strong),
        ];
    }
    if room >= 3 {
        return vec![Span::styled(truncate(name, room), strong)];
    }
    Vec::new()
}

/// What follows the badge (and the session in Rakumux mode): the
/// connection state while not connected, then
/// a status message, or why the daemon is unreachable, debug details, the
/// zoomed session or a pending upgrade. Empty when there's nothing to say.
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
        // The session itself is named in the centre.
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

/// How many of `buttons` to keep: as many as leave the message its room
/// (all of it, or [`MIN_MESSAGE`] columns of it) after `lead` columns, with
/// a space on either side of the buttons. Returns `(kept, room for the
/// message)`.
fn keep_buttons(width: usize, lead: usize, message: usize, buttons: &[Button]) -> (usize, usize) {
    for kept in (1..=buttons.len()).rev() {
        let reserve = buttons_width(&buttons[..kept]) + 2;
        if let Some(room) = width.checked_sub(lead + reserve) {
            if room >= message.min(MIN_MESSAGE) {
                return (kept, room);
            }
        }
    }
    (0, width.saturating_sub(lead))
}

/// The footer line for a row `width` columns wide: the mode badge, the
/// message, and the buttons on the right. It always fits the row: the
/// message is cut first, down to [`MIN_MESSAGE`] columns, then the buttons
/// go from the right.
#[cfg(test)]
pub fn footer_line(app: &App, width: usize) -> Line<'static> {
    footer(app, width).0
}

/// [`footer_line`] and where its tap targets are: the badge, then each
/// button shown. The badge, buttons and a message come first; the session
/// takes the centre when there's room for it between them.
fn footer(app: &App, width: usize) -> (Line<'static>, Vec<FooterTarget>) {
    let badge = mode_badge(app);
    let badge_width = str_width(&badge.content);
    // The badge, then a space before the message.
    let lead = badge_width + 1;
    let message = message(app);
    let buttons = buttons(app);
    let (kept, room) = keep_buttons(width, lead, spans_width(&message), &buttons);
    let buttons = &buttons[..kept];
    let message = fit(message, room);
    let mut targets = vec![(
        0,
        u16::try_from(badge_width.min(width)).unwrap_or(u16::MAX),
        Hit::ModeBadge,
    )];
    let mut spans = vec![badge];
    if !message.is_empty() {
        spans.push(Span::raw(" "));
        spans.extend(message);
    }
    // Where the buttons start (a column after the centre's room), or the
    // row's end.
    let right = if buttons.is_empty() {
        width
    } else {
        width.saturating_sub(buttons_width(buttons) + 1)
    };
    if let Some(session) = centred_session(app) {
        // A column clear on either side of the session.
        let left = spans_width(&spans);
        let (start, end) = (left + 1, right.saturating_sub(1));
        let label = session_label(&session.name, end.saturating_sub(start), app.focus);
        if !label.is_empty() {
            let label_width = spans_width(&label);
            let x = (width.saturating_sub(label_width) / 2).clamp(start, end - label_width);
            spans.push(Span::raw(" ".repeat(x - left)));
            spans.extend(label);
        }
    }
    if app.focus == Focus::List {
        // Dark ink on the orange bar; alerts stand out in bold.
        let palette = theme::palette();
        for span in &mut spans {
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

/// The bar: orange with dark text in Rakumux mode, muted blue (the badge's
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
/// Records its tap targets (the badge and the buttons) in `hits`.
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
    async fn the_badge_leads_and_buttons_end_the_line() {
        let mut h = harness(vec![view("A", "api", 10)]);
        let p = theme::palette();
        // Rakumux mode: the orange badge, then Help and Settings.
        let line = footer_line(&h.app, 60);
        let shown = text(&line);
        assert!(shown.starts_with(" RAKUMUX "), "{shown:?}");
        assert!(shown.ends_with(" ? Help   , Settings  "), "{shown:?}");
        assert_eq!(str_width(&shown), 60, "{shown:?}");
        assert_eq!(line.spans[0].style.bg, Some(p.accent_orange));
        assert_eq!(line.spans[0].style.fg, Some(p.background));
        let key = line.spans.iter().find(|s| s.content == " ?").unwrap();
        assert_eq!(key.style.fg, Some(p.background));
        assert_eq!(key.style.bg, p.faded(p.accent_orange, BUTTON_SHADE));
        assert!(key.style.add_modifier.contains(Modifier::BOLD));
        // Session mode: a quiet badge, and the way to Rakumux mode alone.
        h.press(KeyCode::Enter).await;
        let line = footer_line(&h.app, 80);
        let shown = text(&line);
        assert!(shown.starts_with(" SESSION "), "{shown:?}");
        assert!(shown.ends_with(" Ctrl-\\ Rakumux  "), "{shown:?}");
        assert!(
            !shown.contains("Help") && !shown.contains("Settings"),
            "{shown:?}"
        );
        assert_eq!(line.spans[0].style.bg, Some(p.selection));
        assert_eq!(line.spans[0].style.fg, Some(p.neutral_gray));
        let key = line.spans.iter().find(|s| s.content == " Ctrl-\\").unwrap();
        assert_eq!(key.style.fg, Some(p.accent_primary));
        assert_eq!(key.style.bg, Some(p.background));
        // While typing a filter those keys type, so none show; the badge
        // and the selected session stay.
        h.rakumux().await;
        h.press(KeyCode::Char('/')).await;
        assert!(buttons(&h.app).is_empty());
        let line = text(&footer_line(&h.app, 60));
        assert!(line.starts_with(" RAKUMUX  "), "{line:?}");
        assert!(centred_in(&line, "session: api", 60), "{line:?}");
    }

    #[tokio::test]
    async fn messages_follow_the_badge_and_are_cut_before_buttons_go() {
        let mut h = harness(vec![view("A", "api", 10)]);
        h.app.set_status("Layout: Tabs");
        let line = text(&footer_line(&h.app, 80));
        assert!(line.starts_with(" RAKUMUX  Layout: Tabs "), "{line:?}");
        assert!(centred(&line, "session: api"), "{line:?}");
        assert!(line.ends_with(" ? Help   , Settings  "), "{line:?}");
        // Short of room the session gives way first: off the centre, then
        // its name alone, then nothing.
        let line = text(&footer_line(&h.app, 52));
        assert!(line.contains("  api  "), "{line:?}");
        assert!(!line.contains("session:"), "{line:?}");
        assert!(line.ends_with(" ? Help   , Settings  "), "{line:?}");
        let line = text(&footer_line(&h.app, 46));
        assert!(!line.contains("api"), "{line:?}");
        assert!(line.starts_with(" RAKUMUX  Layout: Tabs "), "{line:?}");
        // A long message is cut to keep the buttons while it keeps 30
        // columns…
        let long = "A status message long enough to need the whole line here, and then some";
        h.app.set_status(long);
        // (the session label has given way already)…
        let line = text(&footer_line(&h.app, 80));
        assert!(line.starts_with(" RAKUMUX  A status message"), "{line:?}");
        assert!(line.contains("…"), "{line:?}");
        assert!(line.ends_with(" ? Help   , Settings  "), "{line:?}");
        assert_eq!(str_width(&line), 80, "{line:?}");
        // …then the buttons go and the message takes the row.
        let line = text(&footer_line(&h.app, 50));
        assert!(!line.contains("Settings"), "{line:?}");
        assert!(line.starts_with(" RAKUMUX  A status message"), "{line:?}");
        assert_eq!(str_width(&line), 50, "{line:?}");
        // With room to spare the session comes back.
        let line = text(&footer_line(&h.app, 200));
        assert!(line.starts_with(" RAKUMUX  A status message"), "{line:?}");
        assert!(line.contains("session: api"), "{line:?}");
    }

    #[tokio::test]
    async fn the_line_always_fits_and_the_badge_always_shows() {
        let mut h = harness(vec![view("A", "api", 10)]);
        let expire = |h: &mut crate::app::testing::Harness| {
            h.app
                .on_tick(std::time::Instant::now() + std::time::Duration::from_secs(60));
        };
        for focus_session in [false, true] {
            if focus_session {
                h.press(KeyCode::Enter).await;
            }
            for status in [
                None,
                Some("Renamed to a-much-longer-session-name-than-usual"),
            ] {
                match status {
                    Some(status) => h.app.set_status(status),
                    None => expire(&mut h),
                }
                for width in 0..=160 {
                    let line = text(&footer_line(&h.app, width));
                    let badge = if focus_session {
                        " SESSION "
                    } else {
                        " RAKUMUX "
                    };
                    assert!(line.starts_with(badge), "{width}: {line:?}");
                    if width >= str_width(badge) {
                        assert!(str_width(&line) <= width, "{width}: {line:?}");
                    }
                }
            }
        }
        // In a session, a message keeps its room before the way out shows.
        let line = text(&footer_line(&h.app, 40));
        assert!(line.contains("Renamed"), "{line:?}");
        expire(&mut h);
        let line = text(&footer_line(&h.app, 40));
        assert!(line.ends_with(" Ctrl-\\ Rakumux  "), "{line:?}");
    }

    #[tokio::test]
    async fn a_phone_keeps_the_badge_and_help_longest() {
        let mut h = harness(vec![view("A", "api", 10)]);
        // Rakumux mode: both buttons fit 40 columns.
        for width in [40, 45] {
            let line = text(&footer_line(&h.app, width));
            assert!(line.starts_with(" RAKUMUX "), "{line:?}");
            assert!(line.ends_with(" ? Help   , Settings  "), "{line:?}");
        }
        // Then Settings goes before Help.
        let line = text(&footer_line(&h.app, 26));
        assert!(line.ends_with(" ? Help  "), "{line:?}");
        // Session mode: the way out fits a phone.
        h.press(KeyCode::Enter).await;
        let line = text(&footer_line(&h.app, 40));
        assert!(line.starts_with(" SESSION "), "{line:?}");
        assert!(line.ends_with(" Ctrl-\\ Rakumux  "), "{line:?}");
        // The targets match what's shown.
        let (line, targets) = footer(&h.app, 40);
        let shown: Vec<char> = text(&line).chars().collect();
        let at = |(x, width, _): &FooterTarget| -> String {
            shown[usize::from(*x)..usize::from(x + width)]
                .iter()
                .collect()
        };
        assert_eq!(targets.len(), 2);
        assert_eq!(at(&targets[0]), " SESSION ");
        assert_eq!(targets[0].2, Hit::ModeBadge);
        assert_eq!(at(&targets[1]), " Ctrl-\\ Rakumux ");
        assert_eq!(targets[1].2, Hit::FooterKey(FooterKey::Rakumux));
    }

    #[tokio::test]
    async fn connecting_and_an_unreachable_daemon_show_after_the_badge() {
        let mut h = harness(vec![view("A", "api", 10)]);
        h.app.connection = Connection::Connecting;
        let line = text(&footer_line(&h.app, 80));
        assert!(line.starts_with(" RAKUMUX  "), "{line:?}");
        assert!(line.contains(" connecting "), "{line:?}");
        assert!(centred(&line, "session: api"), "{line:?}");
        assert!(line.ends_with(" ? Help   , Settings  "), "{line:?}");
        h.app.connection = Connection::Failed(ApiError::new(
            ErrorCode::DaemonUnavailable,
            "can't start rakumux-ptyd",
        ));
        let line = text(&footer_line(&h.app, 100));
        assert!(
            line.starts_with(" RAKUMUX  ✕ daemon unavailable · can't start rakumux-ptyd "),
            "{line:?}"
        );
        // A status message takes the error's place; the state stays.
        h.app.set_status("Reconnecting…");
        let line = text(&footer_line(&h.app, 100));
        assert!(
            line.starts_with(" RAKUMUX  ✕ daemon unavailable · Reconnecting… "),
            "{line:?}"
        );
    }

    #[tokio::test]
    async fn the_centre_names_the_selected_session_or_the_one_taking_the_keys() {
        let mut h = harness(vec![view("A", "api", 20), view("B", "docs", 10)]);
        let p = theme::palette();
        let line = footer_line(&h.app, 80);
        assert!(centred(&text(&line), "session: api"), "{:?}", text(&line));
        let name = line.spans.iter().find(|s| s.content == "api").unwrap();
        assert!(name.style.add_modifier.contains(Modifier::BOLD));
        assert_eq!(name.style.fg, Some(p.background));
        h.press(KeyCode::Up).await;
        assert!(centred(&text(&footer_line(&h.app, 80)), "session: docs"));
        // No session selected (+ New session): just the badge.
        h.press(KeyCode::Down).await;
        h.press(KeyCode::Down).await;
        assert_eq!(h.app.list.selected_session(), None);
        let shown = text(&footer_line(&h.app, 80));
        assert!(shown.starts_with(" RAKUMUX  "), "{shown:?}");
        assert!(
            !shown.contains("api") && !shown.contains("docs"),
            "{shown:?}"
        );
        // In a session: the one taking the keys, bright on the blue bar.
        h.press(KeyCode::Up).await;
        h.press(KeyCode::Up).await;
        h.press(KeyCode::Enter).await;
        let line = footer_line(&h.app, 80);
        assert!(centred(&text(&line), "session: docs"), "{:?}", text(&line));
        let name = line.spans.iter().find(|s| s.content == "docs").unwrap();
        assert_eq!(name.style.fg, Some(p.text_bright));
        // The terminal panel isn't a session: nothing in the centre.
        h.app.focus = Focus::Terminal;
        assert!(!text(&footer_line(&h.app, 80)).contains("session:"));
    }
}
