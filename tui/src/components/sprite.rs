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
use crate::sprites::Step;
use crate::theme;

const STEPS: [Step; 4] = [Step::Create, Step::Install, Step::Start, Step::Connect];

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
