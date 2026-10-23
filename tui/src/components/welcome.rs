//! The welcome tour: a short carousel shown on first start and from Help
//! (`t`). Six cards: what Rakumux is and that it's keyboard driven,
//! folders and sessions, the two modes, moving around, the main shortcuts,
//! and where Help and Settings are. `←`/`→` move, Enter goes on (and finishes), Esc closes.
//! Every card has the "Don't show this on startup" box (Space), so you can
//! opt out at any point.

use ratatui::{
    layout::{Alignment, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Clear, Paragraph, Wrap},
    Frame,
};

use super::utils::{popup_area_fixed_height, popup_width, wrapped_rows};
use crate::app::types::WelcomeState;
use crate::app::App;
use crate::theme;

/// The number of cards.
pub const CARDS: usize = 6;

/// One card: a title and paragraphs whose `backticked` words are keys.
fn card(app: &App, index: usize) -> (&'static str, Vec<String>) {
    let k = app.rakumux_key.label();
    match index {
        0 => (
            "Welcome to Rakumux",
            vec![
                "Rakumux runs your terminal sessions in the background and shows them side by \
                 side: shells, commands, and coding agents like Claude Code, Codex or OpenCode. \
                 They keep running when you quit."
                    .into(),
                "It's driven from the keyboard. This tour shows the few keys you need; it takes \
                 a minute."
                    .into(),
            ],
        ),
        1 => (
            "Folders and sessions",
            vec![
                "A session is a terminal in a folder, like a repo or a worktree: a shell, a dev \
                 server or a coding agent. The list shows each one's git branch."
                    .into(),
                "The list opens on the Status tab, what needs you first; Folders groups by folder."
                    .into(),
                "`P` filters by folder: pick one, tick several with `Tab`, or add a folder. All \
                 folders shows everything again."
                    .into(),
            ],
        ),
        2 => (
            "Two modes",
            vec![
                "SESSION mode: you're working in a session, and every key goes to its program."
                    .into(),
                format!(
                    "RAKUMUX mode: your keys drive Rakumux. Press `{k}` in a session to switch; \
                     it's the one key a program never gets."
                ),
                "The badge at the bottom left always shows which mode you're in: an orange \
                 RAKUMUX, or SESSION."
                    .into(),
            ],
        ),
        3 => (
            "Moving around",
            vec![
                "In Rakumux mode, `↑` `↓` (or `j` `k`) pick a session; it shows live next to \
                 the list while you look."
                    .into(),
                "`Enter` goes into the selected session. `Esc` goes back to the one you came \
                 from, and `Tab` to the one before that."
                    .into(),
                "`n` starts a new session, and `1` to `9` jump straight into one.".into(),
            ],
        ),
        4 => (
            "The main shortcuts",
            vec![
                "`n` new   `r` rename   `p` pin   `x` close   `d`/`dd` delete".into(),
                "`P` folders   `/` filter   `z` full screen   `q` quit".into(),
                format!("From inside a session, press `{k}` first, then the key."),
            ],
        ),
        _ => (
            "Help is always there",
            vec![
                "`?` opens Help: how Rakumux works, every key, the command line.".into(),
                "`,` opens Settings: layout, icons, then every action with its keys; Enter \
                 runs the one you pick."
                    .into(),
                "The footer has a button for each. Press `t` in Help any time to see this \
                 tour again."
                    .into(),
            ],
        ),
    }
}

/// How a card is spaced. Roomy by default; tighter when the roomy card
/// doesn't fit the screen's height (a phone with its keyboard up).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Spacing {
    /// Blank columns inside the border on each side.
    side: u16,
    /// A blank row above the title.
    top: u16,
    /// A blank row between the title and the text.
    title_gap: bool,
    /// A blank row between paragraphs.
    paragraph_gap: bool,
    /// A blank row after the last paragraph.
    trailing_blank: bool,
    /// Rows below the text: the opt-out box, (a blank row,) the dots and
    /// the keys.
    bottom: u16,
}

/// The spacings to try, roomiest first; the last one is used when none
/// fits.
const SPACINGS: [Spacing; 4] = [
    Spacing {
        side: 2,
        top: 1,
        title_gap: true,
        paragraph_gap: true,
        trailing_blank: true,
        bottom: 4,
    },
    Spacing {
        side: 1,
        top: 0,
        title_gap: true,
        paragraph_gap: true,
        trailing_blank: false,
        bottom: 3,
    },
    Spacing {
        side: 1,
        top: 0,
        title_gap: false,
        paragraph_gap: true,
        trailing_blank: false,
        bottom: 3,
    },
    Spacing {
        side: 1,
        top: 0,
        title_gap: false,
        paragraph_gap: false,
        trailing_blank: false,
        bottom: 3,
    },
];

/// Rows of the popup that shows every card `width` columns wide with
/// `spacing`: the tallest card's, so none is cut off and the popup doesn't
/// change height as you move.
fn popup_rows(app: &App, width: u16, spacing: Spacing) -> u16 {
    let text_width = usize::from(width.saturating_sub(2 + 2 * spacing.side));
    let tallest = (0..CARDS)
        .map(|i| {
            let (_, paragraphs) = card(app, i);
            let text: usize = paragraphs
                .iter()
                .map(|p| wrapped_rows(&p.replace('`', ""), text_width))
                .sum();
            // The title, the paragraphs and any optional blank rows.
            1 + usize::from(spacing.title_gap)
                + text
                + paragraphs.len().saturating_sub(1) * usize::from(spacing.paragraph_gap)
                + usize::from(spacing.trailing_blank)
        })
        .max()
        .unwrap_or(0);
    let rows = tallest + usize::from(spacing.top + spacing.bottom) + 2;
    u16::try_from(rows).unwrap_or(u16::MAX)
}

/// The opt-out box's line, as long as fits `width`: shorter wording, then
/// without the key.
fn opt_out_line(dont_show: bool, width: u16) -> Line<'static> {
    let p = theme::palette();
    let (mark, color) = if dont_show {
        ("[x]", p.accent_orange)
    } else {
        ("[ ]", p.text_muted)
    };
    let key = Style::default()
        .fg(p.accent_primary)
        .add_modifier(Modifier::BOLD);
    let muted = Style::default().fg(p.neutral_gray);
    // The wording, then the key and what it does.
    let variants: [(&str, &str, &str); 4] = [
        (" Don't show this on startup", "Space", " tick"),
        (" Don't show on startup", "Space", " tick"),
        (" Don't show on startup", "Space", ""),
        (" Don't show on startup", "", ""),
    ];
    let line = |(text, keys, action): (&'static str, &'static str, &'static str)| {
        let mut spans = vec![
            Span::styled(
                mark,
                Style::default().fg(color).add_modifier(Modifier::BOLD),
            ),
            Span::styled(text, Style::default().fg(p.text_primary)),
        ];
        if !keys.is_empty() {
            spans.push(Span::raw("  "));
            spans.push(Span::styled(keys, key));
            spans.push(Span::styled(action, muted));
        }
        Line::from(spans)
    };
    variants
        .into_iter()
        .map(line)
        .find(|l| l.width() <= usize::from(width))
        .unwrap_or_else(|| line(variants[3]))
}

/// Styled spans for `text`, with `backticked` parts as keys.
fn spans(text: &str) -> Vec<Span<'static>> {
    let p = theme::palette();
    let key = Style::default()
        .fg(p.accent_orange)
        .add_modifier(Modifier::BOLD);
    let plain = Style::default().fg(p.text_primary);
    text.split('`')
        .enumerate()
        .filter(|(_, part)| !part.is_empty())
        .map(|(i, part)| Span::styled(part.to_string(), if i % 2 == 1 { key } else { plain }))
        .collect()
}

/// Draw card `index` in the middle of `area`.
pub fn draw_welcome(f: &mut Frame, app: &App, state: WelcomeState, area: Rect) {
    let index = state.card;
    let p = theme::palette();
    // Tall enough for the longest card at this width, so none is cut off
    // and the card doesn't change height as you move; tighter when that
    // doesn't fit.
    let width = popup_width(area.width, 60);
    let spacing = SPACINGS
        .into_iter()
        .find(|s| popup_rows(app, width, *s) <= area.height)
        .unwrap_or(SPACINGS[SPACINGS.len() - 1]);
    let popup = popup_area_fixed_height(area, 60, popup_rows(app, width, spacing));
    f.render_widget(Clear, popup);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(p.accent_orange))
        .title(Span::styled(
            " Rakumux tour ",
            Style::default()
                .fg(p.accent_orange)
                .add_modifier(Modifier::BOLD),
        ))
        .style(p.base());
    let inner = block.inner(popup);
    f.render_widget(block, popup);
    if inner.height < 4 || inner.width < 10 {
        return;
    }
    let index = index.min(CARDS - 1);
    let last = index + 1 == CARDS;
    let (title, paragraphs) = card(app, index);
    let mut lines = vec![Line::from(Span::styled(
        title,
        Style::default()
            .fg(p.text_bright)
            .add_modifier(Modifier::BOLD),
    ))];
    if spacing.title_gap {
        lines.push(Line::default());
    }
    for (i, paragraph) in paragraphs.iter().enumerate() {
        if i > 0 && spacing.paragraph_gap {
            lines.push(Line::default());
        }
        lines.push(Line::from(spans(paragraph)));
    }
    let body = Rect {
        x: inner.x + spacing.side,
        y: inner.y + spacing.top,
        width: inner.width.saturating_sub(2 * spacing.side),
        height: inner.height.saturating_sub(spacing.top + spacing.bottom),
    };
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: true }), body);

    // Dots and keys.
    let dots: Vec<Span> = (0..CARDS)
        .flat_map(|i| {
            let (dot, color) = if i == index {
                ("●", p.accent_orange)
            } else {
                ("○", p.text_muted)
            };
            [
                Span::styled(dot, Style::default().fg(color)),
                Span::raw(" "),
            ]
        })
        .collect();
    let key = Style::default()
        .fg(p.accent_primary)
        .add_modifier(Modifier::BOLD);
    let muted = Style::default().fg(p.neutral_gray);
    let enter = if last { " done" } else { " next" };
    let bottom = inner.bottom().saturating_sub(1);
    f.render_widget(
        Paragraph::new(opt_out_line(state.dont_show, inner.width)).alignment(Alignment::Center),
        Rect {
            y: bottom.saturating_sub(spacing.bottom - 1),
            height: 1,
            ..inner
        },
    );
    f.render_widget(
        Paragraph::new(Line::from(dots)).alignment(Alignment::Center),
        Rect {
            y: bottom.saturating_sub(1),
            height: 1,
            ..inner
        },
    );
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("←→", key),
            Span::styled(" move  ", muted),
            Span::styled("Enter", key),
            Span::styled(enter, muted),
            Span::styled("  Esc", key),
            Span::styled(" close", muted),
        ]))
        .alignment(Alignment::Center),
        Rect {
            y: bottom,
            height: 1,
            ..inner
        },
    );
}
