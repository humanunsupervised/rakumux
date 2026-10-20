//! One-line footer, the last row of every screen (zoom too): the mode
//! badge at the left (a full-width orange bar while keys drive
//! Rakumux, a quiet `SESSION`/`TERMINAL` while they go to a program), then the
//! connection state and a message (a status message, why the daemon is
//! unreachable, the zoomed session, a pending upgrade), and on the right the
//! keys for Help and Settings (in session mode after the Rakumux key,
//! which is shown too). Settings' Keys tab lists every key.
//!
//! The footer never wraps or grows: when the row is short the message is
//! cut first (down to [`MIN_MESSAGE`] columns), then hints are dropped from
//! the right (Settings first, then Help); the badge always shows.
//!
//! The badge and the hints are tap targets, for phones and the mouse: the
//! `SESSION` badge switches to Rakumux mode, the `RAKUMUX` badge goes
//! back to the session you came from, and a hint does what its keys do.

use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use super::utils::get_spinner_char;
use crate::app::types::{AppMode, Focus};
use crate::app::{App, Connection, FooterKey, Hit};
use crate::layout::{str_width, truncate};
use crate::theme;

/// Columns a message keeps before hints make way for it.
const MIN_MESSAGE: usize = 30;

/// Blank columns between two hints.
const GAP: usize = 2;

/// A key hint: the keys, what they do (empty shows the keys alone), and
/// what a tap on it does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hint {
    pub keys: String,
    pub action: &'static str,
    pub target: FooterKey,
}

impl Hint {
    fn new(keys: impl Into<String>, action: &'static str, target: FooterKey) -> Self {
        Self {
            keys: keys.into(),
            action,
            target,
        }
    }

    /// Columns it takes.
    fn width(&self) -> usize {
        let action = if self.action.is_empty() {
            0
        } else {
            1 + str_width(self.action)
        };
        str_width(&self.keys) + action
    }
}

/// A tap target in the footer: `width` columns from column `x` of the row.
pub type FooterTarget = (u16, u16, Hit);

/// The mode badge: a bright orange `RAKUMUX` while the keys drive
/// Rakumux (the mode to notice), a quiet `SESSION` that blends into the
/// theme while they go to a session.
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

/// The footer's hints: the keys for Help and Settings, after the Rakumux
/// key in session mode (where the Rakumux key itself comes first, as the
/// way out). Nothing while typing a filter (those keys type) or while a
/// dialog is open.
pub fn hints(app: &App) -> Vec<Hint> {
    if !matches!(app.mode, AppMode::Normal) || app.list.filter_editing {
        return Vec::new();
    }
    let settings = crate::help::SETTINGS_KEY_PRIMARY;
    match app.focus {
        Focus::List => vec![
            Hint::new("?", "help", FooterKey::Help),
            Hint::new(settings, "settings", FooterKey::Settings),
        ],
        Focus::Session | Focus::Terminal => {
            let key = app.rakumux_key.label();
            vec![
                Hint::new(key.clone(), "rakumux", FooterKey::Rakumux),
                Hint::new(format!("{key} ?"), "help", FooterKey::Help),
                Hint::new(format!("{key} {settings}"), "settings", FooterKey::Settings),
            ]
        }
    }
}

fn hints_width(hints: &[Hint]) -> usize {
    let items: usize = hints.iter().map(Hint::width).sum();
    items + GAP * hints.len().saturating_sub(1)
}

/// Add `hints` to `spans`, the first at column `x`; returns where each one
/// is.
fn push_hints(spans: &mut Vec<Span<'static>>, hints: &[Hint], x: usize) -> Vec<FooterTarget> {
    let p = theme::palette();
    let key_style = Style::default()
        .fg(p.accent_primary)
        .add_modifier(Modifier::BOLD);
    let action_style = Style::default().fg(p.neutral_gray);
    let mut targets = Vec::with_capacity(hints.len());
    let mut x = x;
    for (i, hint) in hints.iter().enumerate() {
        if i > 0 {
            spans.push(Span::raw(" ".repeat(GAP)));
            x += GAP;
        }
        let width = hint.width();
        targets.push((
            u16::try_from(x).unwrap_or(u16::MAX),
            u16::try_from(width).unwrap_or(u16::MAX),
            Hit::FooterKey(hint.target),
        ));
        x += width;
        let (keys, action) = (&hint.keys, hint.action);
        spans.push(Span::styled(keys.clone(), key_style));
        if !action.is_empty() {
            spans.push(Span::styled(format!(" {action}"), action_style));
        }
    }
    targets
}

/// What follows the badge: the connection state while not connected, then
/// a status message, or why the daemon is unreachable, debug details, the
/// zoomed session or a pending upgrade. Empty when there's nothing to say.
fn message(app: &App) -> Vec<Span<'static>> {
    let p = theme::palette();
    let muted = Style::default().fg(p.text_muted);
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
        let name = app
            .list
            .selected_session()
            .map(|s| s.name.clone())
            .unwrap_or_default();
        vec![
            Span::styled(
                name,
                Style::default()
                    .fg(p.accent_primary)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(" · zoomed", muted),
        ]
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

/// How many of `hints` to keep: as many as leave the message its room (all
/// of it, or [`MIN_MESSAGE`] columns of it) after `lead` columns, with a
/// space on either side of the hints. Returns `(kept, room for the
/// message)`.
fn keep_hints(width: usize, lead: usize, message: usize, hints: &[Hint]) -> (usize, usize) {
    for kept in (1..=hints.len()).rev() {
        let reserve = hints_width(&hints[..kept]) + 2;
        if let Some(room) = width.checked_sub(lead + reserve) {
            if room >= message.min(MIN_MESSAGE) {
                return (kept, room);
            }
        }
    }
    (0, width.saturating_sub(lead))
}

/// The footer line for a row `width` columns wide: the mode badge, the
/// message, and the hints on the right. It always fits the row: the message
/// is cut first, down to [`MIN_MESSAGE`] columns, then the hints go from
/// the right.
#[cfg(test)]
pub fn footer_line(app: &App, width: usize) -> Line<'static> {
    footer(app, width).0
}

/// [`footer_line`] and where its tap targets are: the badge, then each
/// hint shown.
fn footer(app: &App, width: usize) -> (Line<'static>, Vec<FooterTarget>) {
    let badge = mode_badge(app);
    let badge_width = str_width(&badge.content);
    // The badge, then a space before the message.
    let lead = badge_width + 1;
    let message = message(app);
    let hints = hints(app);
    let (kept, room) = keep_hints(width, lead, spans_width(&message), &hints);
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
    if kept > 0 {
        let hints = &hints[..kept];
        let used = spans_width(&spans);
        spans.push(Span::raw(
            " ".repeat(width.saturating_sub(used + hints_width(hints) + 1)),
        ));
        let x = spans_width(&spans);
        targets.extend(push_hints(&mut spans, hints, x));
        spans.push(Span::raw(" "));
    }
    let mut line = Line::from(spans);
    if app.focus == Focus::List {
        let palette = theme::palette();
        for span in &mut line.spans {
            let alert = span.style.fg == Some(palette.accent_pink);
            span.style = span.style.fg(palette.background).bg(palette.accent_orange);
            if alert {
                span.style = span.style.add_modifier(Modifier::BOLD);
            }
        }
    }
    (line, targets)
}

fn footer_style(app: &App) -> Style {
    let palette = theme::palette();
    if app.focus == Focus::List {
        Style::default()
            .fg(palette.background)
            .bg(palette.accent_orange)
    } else {
        Style::default()
            .fg(palette.text_primary)
            .bg(palette.background)
    }
}

/// Draws the footer into the first row of `area`; it never takes more.
/// Records its tap targets (the badge and the hints) in `hits`.
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

    #[test]
    fn rakumux_bar_fills_the_row_with_readable_ink_in_all_message_states() {
        use ratatui::{backend::TestBackend, widgets::Block, Terminal};
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
                let mut terminal = Terminal::new(TestBackend::new(width + 4, 3)).unwrap();
                let mut hits = Vec::new();
                terminal
                    .draw(|frame| {
                        frame.render_widget(
                            Block::default().style(Style::default().bg(palette.background)),
                            frame.size(),
                        );
                        draw_footer(frame, &h.app, Rect::new(2, 1, width, 1), &mut hits);
                    })
                    .unwrap();
                for x in 2..width + 2 {
                    let cell = terminal.backend().buffer().get(x, 1);
                    assert_eq!(cell.bg, palette.accent_orange, "{state}: col {x}");
                    assert_eq!(cell.fg, palette.background, "{state}: col {x}");
                }
                assert_eq!(terminal.backend().buffer().get(2, 0).bg, palette.background);
                assert_eq!(terminal.backend().buffer().get(0, 1).bg, palette.background);
            }
        }
    }

    #[test]
    fn session_and_terminal_modes_keep_the_quiet_footer_background() {
        use ratatui::{backend::TestBackend, Terminal};
        let mut h = harness(vec![view("A", "api", 10)]);
        let palette = theme::palette();
        for focus in [Focus::Session, Focus::Terminal] {
            h.app.focus = focus;
            let mut terminal = Terminal::new(TestBackend::new(160, 1)).unwrap();
            terminal
                .draw(|frame| draw_footer(frame, &h.app, frame.size(), &mut Vec::new()))
                .unwrap();
            let buffer = terminal.backend().buffer();
            assert_eq!(buffer.get(0, 0).bg, palette.selection);
            assert_eq!(buffer.get(159, 0).bg, palette.background);
        }
    }

    #[tokio::test]
    async fn the_badge_leads_and_the_help_and_settings_keys_end_the_line() {
        let mut h = harness(vec![view("A", "api", 10)]);
        let p = theme::palette();
        // Rakumux mode: the orange badge, then the keys themselves.
        let line = footer_line(&h.app, 60);
        let shown = text(&line);
        assert!(shown.starts_with(" RAKUMUX "), "{shown:?}");
        assert!(shown.ends_with("? help  , settings "), "{shown:?}");
        assert_eq!(str_width(&shown), 60, "{shown:?}");
        assert_eq!(line.spans[0].style.bg, Some(p.accent_orange));
        assert_eq!(line.spans[0].style.fg, Some(p.background));
        // Session mode: a quiet badge; the keys after the Rakumux key,
        // which comes first.
        h.press(KeyCode::Enter).await;
        let line = footer_line(&h.app, 80);
        let shown = text(&line);
        assert!(shown.starts_with(" SESSION "), "{shown:?}");
        assert!(
            shown.ends_with("Ctrl-\\ rakumux  Ctrl-\\ ? help  Ctrl-\\ , settings "),
            "{shown:?}"
        );
        assert_eq!(line.spans[0].style.bg, Some(p.selection));
        assert_eq!(line.spans[0].style.fg, Some(p.neutral_gray));
        let key = line
            .spans
            .iter()
            .find(|s| s.content == "Ctrl-\\ ?")
            .unwrap();
        assert_eq!(key.style.fg, Some(p.accent_primary));
        // While typing a filter those keys type, so none show; the badge
        // stays.
        h.rakumux().await;
        h.press(KeyCode::Char('/')).await;
        assert!(hints(&h.app).is_empty());
        assert_eq!(text(&footer_line(&h.app, 60)).trim(), "RAKUMUX");
    }

    #[tokio::test]
    async fn messages_follow_the_badge_and_are_cut_before_hints_go() {
        let mut h = harness(vec![view("A", "api", 10)]);
        h.app.set_status("Layout: Tabs");
        let line = text(&footer_line(&h.app, 60));
        assert!(line.starts_with(" RAKUMUX  Layout: Tabs "), "{line:?}");
        assert!(line.ends_with("? help  , settings "), "{line:?}");
        // A long message is cut to keep the hints while it keeps 30
        // columns…
        let long = "A status message long enough to need the whole line here, and then some";
        h.app.set_status(long);
        let line = text(&footer_line(&h.app, 80));
        assert!(line.starts_with(" RAKUMUX  A status message"), "{line:?}");
        assert!(line.contains("…"), "{line:?}");
        assert!(line.ends_with("? help  , settings "), "{line:?}");
        assert_eq!(str_width(&line), 80, "{line:?}");
        // …then the hints go and the message takes the row.
        let line = text(&footer_line(&h.app, 50));
        assert!(!line.contains("settings"), "{line:?}");
        assert!(line.starts_with(" RAKUMUX  A status message"), "{line:?}");
        assert_eq!(str_width(&line), 50, "{line:?}");
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
        // In a session, narrow rows drop the hints from the right: the
        // way out stays longest.
        let line = text(&footer_line(&h.app, 40));
        assert!(line.contains("Renamed"), "{line:?}");
        expire(&mut h);
        let line = text(&footer_line(&h.app, 40));
        assert!(line.ends_with("Ctrl-\\ rakumux "), "{line:?}");
        assert!(!line.contains("help"), "{line:?}");
        let line = text(&footer_line(&h.app, 50));
        assert!(
            line.ends_with("Ctrl-\\ rakumux  Ctrl-\\ ? help "),
            "{line:?}"
        );
    }

    #[tokio::test]
    async fn a_phone_keeps_the_badge_and_help_longest() {
        let mut h = harness(vec![view("A", "api", 10)]);
        // Rakumux mode: both hints fit 40 columns.
        for width in [40, 45] {
            let line = text(&footer_line(&h.app, width));
            assert!(line.starts_with(" RAKUMUX "), "{line:?}");
            assert!(line.ends_with("? help  , settings "), "{line:?}");
        }
        // Then Settings goes before Help.
        let line = text(&footer_line(&h.app, 26));
        assert!(line.ends_with("? help "), "{line:?}");
        // Session mode: the way out and Help at 45, the way out at 40.
        h.press(KeyCode::Enter).await;
        let line = text(&footer_line(&h.app, 45));
        assert!(line.starts_with(" SESSION "), "{line:?}");
        assert!(
            line.ends_with("Ctrl-\\ rakumux  Ctrl-\\ ? help "),
            "{line:?}"
        );
        let line = text(&footer_line(&h.app, 40));
        assert!(line.ends_with("Ctrl-\\ rakumux "), "{line:?}");
        // The targets match what's shown.
        let (line, targets) = footer(&h.app, 45);
        let shown: Vec<char> = text(&line).chars().collect();
        let at = |(x, width, _): &FooterTarget| -> String {
            shown[usize::from(*x)..usize::from(x + width)]
                .iter()
                .collect()
        };
        assert_eq!(targets.len(), 3);
        assert_eq!(at(&targets[0]), " SESSION ");
        assert_eq!(targets[0].2, Hit::ModeBadge);
        assert_eq!(at(&targets[1]), "Ctrl-\\ rakumux");
        assert_eq!(targets[1].2, Hit::FooterKey(FooterKey::Rakumux));
        assert_eq!(at(&targets[2]), "Ctrl-\\ ? help");
        assert_eq!(targets[2].2, Hit::FooterKey(FooterKey::Help));
    }

    #[tokio::test]
    async fn connecting_and_an_unreachable_daemon_show_after_the_badge() {
        let mut h = harness(vec![view("A", "api", 10)]);
        h.app.connection = Connection::Connecting;
        let line = text(&footer_line(&h.app, 80));
        assert!(line.starts_with(" RAKUMUX  "), "{line:?}");
        assert!(line.contains(" connecting "), "{line:?}");
        assert!(line.ends_with("? help  , settings "), "{line:?}");
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
}
