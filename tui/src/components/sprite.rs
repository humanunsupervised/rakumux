//! The Sprite dialog (`AppMode::Sprite`): sets up your own Fly Sprite the
//! first time a session should run on it.

use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Paragraph, Wrap},
    Frame,
};

use crate::app::sprite::TOKEN_PAGE;
use crate::app::types::{SpriteDialog, SpriteState};
use crate::components::dialogs::{dialog_frame, draw_keys, error_line, key_rows, text_rows};
use crate::components::phone_access::qr_lines;
use crate::components::utils::{get_spinner_char, popup_area_fixed_height, popup_width};
use crate::qr::Qr;
use crate::sprites::{FeedSetUp, Step};
use crate::theme;

const STEPS: [Step; 5] = [
    Step::Create,
    Step::Install,
    Step::Start,
    Step::Connect,
    Step::Feed,
];

/// How many of a folder's `.env` files the question lists.
const ENV_SHOWN: usize = 8;

/// The ready card: the Sprite, what runs there and what's left to do in its
/// first session in a folder.
fn ready_lines(
    sprite: &str,
    tools: &[String],
    feed: &FeedSetUp,
    strong: Style,
    done: Style,
) -> Vec<Line<'static>> {
    let mut there = match feed {
        FeedSetUp::On => "Rakumux runs there, and its agents' replies reach your feed.".to_owned(),
        FeedSetUp::Failed(error) => {
            format!("Rakumux runs there; its feed couldn't be connected: {error}.")
        }
        FeedSetUp::SignedOut | FeedSetUp::Skipped => "Rakumux runs there.".to_owned(),
    };
    if let Some(names) = listed(tools) {
        let verb = if tools.len() == 1 { "is" } else { "are" };
        there.push_str(&format!(" {names} {verb} installed."));
    }
    let agents: Vec<String> = tools.iter().filter(|t| *t != "gh").cloned().collect();
    let mut steps = vec![
        "Enter signs in to GitHub and clones it".to_owned(),
        "Rakumux asks to copy your .env files".to_owned(),
    ];
    if !agents.is_empty() {
        steps.push(format!("Sign in to your agent ({})", agents.join(", ")));
    }
    let mut lines = vec![
        Line::from(vec![
            Span::styled("✓ ", done),
            Span::styled(format!("{sprite} is ready"), strong),
        ]),
        Line::from(""),
        Line::from(there),
        Line::from(""),
        Line::from("In its first session in a folder:"),
    ];
    lines.extend(
        steps
            .into_iter()
            .enumerate()
            .map(|(i, step)| Line::from(format!(" {}  {step}", i + 1))),
    );
    lines
}

/// `a`, `a and b`, `a, b and c`.
fn listed(names: &[String]) -> Option<String> {
    match names {
        [] => None,
        [one] => Some(one.clone()),
        [rest @ .., last] => Some(format!("{} and {last}", rest.join(", "))),
    }
}

pub fn draw_sprite(f: &mut Frame, area: Rect, dialog: &SpriteDialog) {
    let p = theme::palette();
    let muted = Style::default().fg(p.text_muted);
    let strong = Style::default()
        .fg(p.text_bright)
        .add_modifier(Modifier::BOLD);
    let (lines, keys): (Vec<Line>, Vec<(&str, &str)>) = match &dialog.state {
        SpriteState::Found { source, .. } => (
            vec![
                Line::from("New sessions in this folder run on your own Fly Sprite."),
                Line::from(""),
                Line::from(vec![
                    Span::raw("Set it up with the Sprites token from "),
                    Span::styled(source.to_string(), strong),
                    Span::raw("?"),
                ]),
            ],
            vec![
                ("y/Enter", "Set up"),
                ("n", "Another token"),
                ("Esc", "Cancel"),
            ],
        ),
        SpriteState::Paste { input, error } => {
            let mut lines = vec![
                Line::from("New sessions in this folder run on your own Fly Sprite."),
                Line::from(""),
                Line::from(vec![
                    Span::raw("Sign up with Fly.io and make a token at "),
                    Span::styled(TOKEN_PAGE, strong),
                    Span::raw(", then paste it here."),
                ]),
                Line::from(""),
            ];
            if let Some(qr) = Qr::encode(TOKEN_PAGE) {
                lines.extend(qr_lines(&qr));
                lines.push(Line::from(""));
            }
            let shown = if input.is_empty() {
                Span::styled("paste your token", muted)
            } else {
                Span::styled("•".repeat(input.value().chars().count().min(40)), strong)
            };
            lines.push(Line::from(vec![Span::raw("Token  "), shown]));
            if let Some(error) = error {
                lines.push(error_line(error));
            }
            (
                lines,
                vec![
                    ("Enter", "Set up"),
                    ("Ctrl-o", "Copy the link"),
                    ("Esc", "Cancel"),
                ],
            )
        }
        SpriteState::Working { step } => {
            let at = STEPS.iter().position(|s| s == step).unwrap_or(0);
            let lines = STEPS
                .iter()
                .enumerate()
                .map(|(i, s)| {
                    let (mark, style) = if i < at {
                        ("✓".to_string(), Style::default().fg(p.accent_green))
                    } else if i == at {
                        (get_spinner_char().to_string(), strong)
                    } else {
                        ("·".to_string(), muted)
                    };
                    Line::from(vec![
                        Span::styled(format!("{mark} "), style),
                        Span::styled(s.label(), style),
                    ])
                })
                .collect();
            (lines, Vec::new())
        }
        SpriteState::Failed { message } => (
            vec![
                Line::from(Span::styled("Setting up the Sprite failed.", strong)),
                Line::from(""),
                Line::from(message.clone()),
            ],
            vec![("r", "Try again"), ("Esc", "Close")],
        ),
        SpriteState::Ready {
            sprite,
            tools,
            feed,
        } => (
            ready_lines(
                sprite,
                tools,
                feed,
                strong,
                Style::default().fg(p.accent_green),
            ),
            vec![("Enter", "Open the session"), ("Esc", "Close")],
        ),
        SpriteState::Env { folder, files } => {
            let name = crate::sprites::folder_name(folder.path());
            let mut lines = vec![
                Line::from(vec![
                    Span::raw("Copy "),
                    Span::styled(name, strong),
                    Span::raw("'s .env files to your Sprite?"),
                ]),
                Line::from(""),
            ];
            lines.extend(
                files
                    .iter()
                    .take(ENV_SHOWN)
                    .map(|file| Line::from(format!("  {}", file.display()))),
            );
            if files.len() > ENV_SHOWN {
                lines.push(Line::from(Span::styled(
                    format!("  and {} more", files.len() - ENV_SHOWN),
                    muted,
                )));
            }
            (
                lines,
                vec![("y/Enter", "Copy"), ("n", "Don't copy"), ("Esc", "Cancel")],
            )
        }
    };
    let width = popup_width(area.width, 60);
    let height = text_rows(&lines, width.saturating_sub(2)) + key_rows(&keys, width) + 3;
    let popup = popup_area_fixed_height(area, 60, height.min(area.height));
    let inner = dialog_frame(f, popup, "Your Sprite", p.accent_primary);
    let above = draw_keys(f, inner, &keys);
    f.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .style(Style::default().fg(p.text_primary)),
        above,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(lines: &[Line]) -> Vec<String> {
        lines
            .iter()
            .map(|line| line.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect()
    }

    #[test]
    fn the_ready_card_says_whats_there_and_whats_left() {
        let tools = ["claude", "codex", "gh"].map(String::from);
        let card = ready_lines(
            "my-sprite",
            &tools,
            &FeedSetUp::On,
            Style::default(),
            Style::default(),
        );
        assert_eq!(
            text(&card),
            [
                "✓ my-sprite is ready",
                "",
                "Rakumux runs there, and its agents' replies reach your feed. \
                 claude, codex and gh are installed.",
                "",
                "In its first session in a folder:",
                " 1  Enter signs in to GitHub and clones it",
                " 2  Rakumux asks to copy your .env files",
                " 3  Sign in to your agent (claude, codex)",
            ]
        );
        let bare = ready_lines(
            "s",
            &[],
            &FeedSetUp::SignedOut,
            Style::default(),
            Style::default(),
        );
        assert_eq!(text(&bare)[2], "Rakumux runs there.");
        assert_eq!(text(&bare).len(), 7, "no agent to sign in to");
    }
}
