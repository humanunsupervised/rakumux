//! Modal dialogs: confirmations, rename, daemon restart consent, the
//! Settings modal's dialogs (AI provider keys and models), upgrade, account
//! sign-in.

use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Wrap},
    Frame,
};

use crate::app::types::{
    AiProviderDialog, ApiKeyDialog, ApiProvider, AuthCodeDialog, AuthEmailDialog, ConfirmAction,
    ConfirmDialog, DaemonRestartDialog, OpenAiModelDialog, RenameDialog, UpgradeDialog,
};
use crate::components::utils::{
    draw_text_input, get_spinner_char, popup_area, popup_area_fixed_height, popup_width,
    wrapped_rows,
};
use crate::theme;

fn palette() -> &'static theme::Palette {
    theme::palette()
}

/// A cleared popup with a titled border; returns the inner area.
pub(crate) fn dialog_frame(
    f: &mut Frame,
    popup: Rect,
    title: &str,
    accent: ratatui::style::Color,
) -> Rect {
    f.render_widget(Clear, popup);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(accent))
        .title(Span::styled(
            format!(" {title} "),
            Style::default().fg(accent).add_modifier(Modifier::BOLD),
        ))
        .style(palette().base());
    let inner = block.inner(popup);
    f.render_widget(block, popup);
    inner
}

/// `[key] action • [key] action` help line.
fn key_help(pairs: &[(&str, &str)]) -> Line<'static> {
    let mut spans = Vec::new();
    for (i, (key, action)) in pairs.iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled(
                " • ",
                Style::default().fg(palette().text_muted),
            ));
        }
        spans.push(Span::styled(
            format!("[{key}]"),
            Style::default()
                .fg(palette().accent_primary)
                .add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::styled(
            format!(" {action}"),
            Style::default().fg(palette().text_muted),
        ));
    }
    Line::from(spans)
}

/// [`key_help`] on as many lines as it needs to fit `width` columns (a
/// phone), never splitting a key from its action.
fn key_help_lines(pairs: &[(&str, &str)], width: u16) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut current: Vec<(&str, &str)> = Vec::new();
    for pair in pairs {
        current.push(*pair);
        if current.len() > 1 && key_help(&current).width() > usize::from(width) {
            current.pop();
            lines.push(key_help(&current));
            current = vec![*pair];
        }
    }
    if !current.is_empty() || lines.is_empty() {
        lines.push(key_help(&current));
    }
    lines
}

/// Rows [`key_help_lines`] takes in a dialog `popup_width` columns wide.
pub(crate) fn key_rows(pairs: &[(&str, &str)], popup_width: u16) -> u16 {
    let lines = key_help_lines(pairs, popup_width.saturating_sub(2)).len();
    u16::try_from(lines).unwrap_or(1)
}

/// Draw the keys centred at the bottom of `inner`, on as many lines as
/// they need; returns the rows above them.
pub(crate) fn draw_keys(f: &mut Frame, inner: Rect, pairs: &[(&str, &str)]) -> Rect {
    let lines = key_help_lines(pairs, inner.width);
    let rows = u16::try_from(lines.len()).unwrap_or(1).min(inner.height);
    f.render_widget(
        Paragraph::new(lines).alignment(Alignment::Center),
        Rect {
            y: inner.bottom() - rows,
            height: rows,
            ..inner
        },
    );
    Rect {
        height: inner.height - rows,
        ..inner
    }
}

/// Rows `lines` take word-wrapped to `width` columns.
pub(crate) fn text_rows(lines: &[Line], width: u16) -> u16 {
    let rows: usize = lines
        .iter()
        .map(|line| {
            let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
            wrapped_rows(&text, usize::from(width))
        })
        .sum();
    u16::try_from(rows).unwrap_or(u16::MAX)
}

pub(crate) fn error_line(message: &str) -> Line<'static> {
    Line::from(Span::styled(
        format!("✕ {message}"),
        Style::default().fg(palette().accent_pink),
    ))
}

pub fn draw_confirm_dialog(f: &mut Frame, dialog: &ConfirmDialog, area: Rect) {
    let keys = match &dialog.action {
        ConfirmAction::RestartUi => [("y/Enter", "Restart"), ("n/Esc", "Cancel")],
        _ => [("y/Enter", "Yes"), ("n/Esc", "No")],
    };
    let width = popup_width(area.width, 60);
    let message = [Line::from(dialog.message.clone())];
    // The message, a blank row and the keys: 8 rows unless the message
    // wraps further on a narrow screen.
    let height =
        (text_rows(&message, width.saturating_sub(2)) + 1 + key_rows(&keys, width) + 2).max(8);
    let popup = popup_area_fixed_height(area, 60, height);
    let inner = dialog_frame(f, popup, &dialog.title, palette().accent_gold);
    let above = draw_keys(f, inner, &keys);
    f.render_widget(
        Paragraph::new(dialog.message.clone())
            .alignment(Alignment::Center)
            .wrap(Wrap { trim: true })
            .style(Style::default().fg(palette().text_primary)),
        above,
    );
}

pub fn draw_rename_dialog(f: &mut Frame, dialog: &RenameDialog, area: Rect) {
    let keys = [("Enter", "Rename"), ("Esc", "Cancel")];
    let height = 8 + key_rows(&keys, popup_width(area.width, 60));
    let popup = popup_area_fixed_height(area, 60, height);
    let inner = dialog_frame(f, popup, "Rename session", palette().accent_primary);
    let above = draw_keys(f, inner, &keys);
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Length(3),
            Constraint::Length(1),
            Constraint::Min(0),
        ])
        .split(above);
    f.render_widget(
        Paragraph::new(Span::styled(
            format!(" Current: {}", dialog.current_name),
            Style::default().fg(palette().text_muted),
        )),
        rows[0],
    );
    draw_text_input(
        f,
        rows[1],
        "New name",
        &dialog.input,
        "Enter a new name",
        !dialog.busy,
        false,
    );
    let message = if dialog.busy {
        Line::from(Span::styled(
            format!(" {} Renaming…", get_spinner_char()),
            Style::default().fg(palette().accent_primary),
        ))
    } else if let Some(error) = &dialog.error_message {
        error_line(error)
    } else {
        Line::from("")
    };
    f.render_widget(Paragraph::new(message), rows[2]);
}

/// `a`, `a or b`, `a, b or c`.
fn or_list(items: &[String]) -> String {
    match items {
        [] => String::new(),
        [only] => only.clone(),
        [rest @ .., last] => format!("{} or {last}", rest.join(", ")),
    }
}

pub fn draw_daemon_restart_dialog(f: &mut Frame, dialog: &DaemonRestartDialog, area: Rect) {
    let version = dialog
        .daemon_version
        .as_deref()
        .map(|v| format!(" (version {v})"))
        .unwrap_or_default();
    let outdated = !dialog.missing.is_empty();
    let mut lines = if outdated {
        let consequence = if dialog.in_place {
            Span::styled(
                "Your sessions keep running: the new server takes them over.",
                Style::default().fg(palette().text_muted),
            )
        } else {
            Span::styled(
                "This server can't hand its sessions over, so restarting it ends every running \
                 session; Enter brings one back. Not now keeps the server and your sessions as \
                 they are.",
                Style::default().fg(palette().accent_gold),
            )
        };
        vec![
            Line::from(format!(
                "The running Rakumux server{version} is from an older Rakumux, so it can't \
                 show {}. Restart it to get them.",
                or_list(&dialog.missing)
            )),
            Line::from(""),
            Line::from(consequence),
        ]
    } else {
        vec![
            Line::from(format!(
                "The running Rakumux server{version} speaks a different protocol than this \
                 version of Rakumux ({}).",
                crate::app::app_version()
            )),
            Line::from(""),
            Line::from(Span::styled(
                "Restarting it ends every running session; they show as dead and can be \
                 revived.",
                Style::default().fg(palette().accent_gold),
            )),
            Line::from(Span::styled(
                dialog.detail.clone(),
                Style::default().fg(palette().text_muted),
            )),
        ]
    };
    if dialog.restarting {
        lines.push(Line::from(Span::styled(
            format!("{} Stopping the old server…", get_spinner_char()),
            Style::default().fg(palette().accent_primary),
        )));
    } else if let Some(error) = &dialog.error {
        lines.push(error_line(error));
    }
    let keys = [
        (
            if outdated && !dialog.in_place {
                "y"
            } else {
                "y/Enter"
            },
            "Restart",
        ),
        ("n/Esc", "Not now"),
        ("q", "Quit"),
    ];
    // 12 rows, or what the text needs on a narrow screen.
    let width = popup_width(area.width, 70);
    let height = (text_rows(&lines, width.saturating_sub(2)) + key_rows(&keys, width) + 3).max(12);
    let popup = popup_area_fixed_height(area, 70, height);
    let inner = dialog_frame(
        f,
        popup,
        "Restart the Rakumux daemon?",
        palette().accent_gold,
    );
    let above = draw_keys(f, inner, &keys);
    f.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: true })
            .style(Style::default().fg(palette().text_primary)),
        above,
    );
}

pub fn draw_api_key_dialog(f: &mut Frame, dialog: &ApiKeyDialog, area: Rect) {
    let p = palette();
    let (title, placeholder, steps): (&str, &str, &[&str]) = match dialog.provider {
        ApiProvider::Anthropic => (
            "Anthropic API key",
            "sk-ant-…",
            &[
                "Visit console.anthropic.com",
                "Open 'API Keys'",
                "Create a key starting with 'sk-ant-'",
            ],
        ),
        ApiProvider::OpenAI => (
            "OpenAI API key",
            "sk-…",
            &[
                "Visit platform.openai.com/api-keys",
                "Create a key starting with 'sk-'",
            ],
        ),
    };
    let mut lines = match &dialog.error_message {
        Some(error) => vec![error_line(error), Line::from("")],
        None => Vec::new(),
    };
    lines.push(Line::from("Used for AI session naming."));
    for step in steps {
        lines.push(Line::from(vec![
            Span::styled("• ", Style::default().fg(p.accent_green)),
            Span::raw(*step),
        ]));
    }
    lines.push(Line::from(Span::styled(
        "Saved locally to ~/.rakumux/config.json with owner-only permissions. Leave empty to remove the key.",
        Style::default().fg(p.text_muted),
    )));
    let keys = [("Enter", "Save"), ("Esc", "Cancel")];
    // 14 rows, or what the text needs on a narrow screen.
    let width = popup_width(area.width, 70);
    let height =
        (3 + text_rows(&lines, width.saturating_sub(2)) + key_rows(&keys, width) + 2).max(14);
    let popup = popup_area_fixed_height(area, 70, height);
    let inner = dialog_frame(f, popup, title, p.accent_primary);
    let above = draw_keys(f, inner, &keys);
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(1)])
        .split(above);
    draw_text_input(
        f,
        rows[0],
        "API key",
        &dialog.input,
        placeholder,
        true,
        true,
    );
    f.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: true })
            .style(Style::default().fg(p.text_primary)),
        rows[1],
    );
}

/// A list of choices in a dialog over `area`: the options, a spare row and
/// the keys.
fn choice_list(
    f: &mut Frame,
    area: Rect,
    title: &str,
    options: &[(String, bool)],
    selected: usize,
) {
    let p = palette();
    let keys = [("↑/↓", "Choose"), ("Enter", "Save"), ("Esc", "Cancel")];
    let options_rows = u16::try_from(options.len()).unwrap_or(u16::MAX);
    let height = options_rows
        .saturating_add(3)
        .saturating_add(key_rows(&keys, popup_width(area.width, 50)));
    let popup = popup_area_fixed_height(area, 50, height);
    let inner = dialog_frame(f, popup, title, p.accent_primary);
    let list_area = draw_keys(f, inner, &keys);
    let items: Vec<ListItem> = options
        .iter()
        .enumerate()
        .map(|(i, (label, current))| {
            let marker = if i == selected { "➤ " } else { "  " };
            let text = if *current {
                format!("{label} (current)")
            } else {
                label.clone()
            };
            let style = if i == selected {
                Style::default()
                    .fg(p.accent_primary)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(p.text_primary)
            };
            ListItem::new(Line::from(vec![
                Span::raw(marker),
                Span::styled(text, style),
            ]))
        })
        .collect();
    f.render_widget(List::new(items), list_area);
}

pub fn draw_ai_provider_dialog(f: &mut Frame, dialog: &AiProviderDialog, area: Rect) {
    let options = [("Anthropic", "anthropic"), ("OpenAI", "openai")]
        .iter()
        .map(|(label, value)| {
            (
                label.to_string(),
                dialog.current.eq_ignore_ascii_case(value),
            )
        })
        .collect::<Vec<_>>();
    choice_list(f, area, "AI naming provider", &options, dialog.selected);
}

pub fn draw_openai_model_dialog(f: &mut Frame, dialog: &OpenAiModelDialog, area: Rect) {
    let options = dialog
        .options
        .iter()
        .map(|model| (model.clone(), *model == dialog.current))
        .collect::<Vec<_>>();
    choice_list(f, area, "OpenAI model", &options, dialog.selected);
}

pub fn draw_upgrade_dialog(f: &mut Frame, dialog: &UpgradeDialog, area: Rect) {
    let p = palette();
    let popup = popup_area(area, 60, 40);
    let inner = dialog_frame(f, popup, "Upgrade Rakumux", p.accent_primary);
    let keys: &[(&str, &str)] = if dialog.installing {
        &[]
    } else if dialog.checking {
        &[("Esc", "Cancel")]
    } else if dialog.update_available {
        &[("↑/↓", "Select"), ("Enter", "Confirm"), ("Esc", "Cancel")]
    } else {
        &[("Enter/Esc", "Close")]
    };
    let above = draw_keys(f, inner, keys);
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(2),
            Constraint::Length(3),
        ])
        .split(above);
    f.render_widget(
        Paragraph::new(format!("Current version: {}", dialog.current_version))
            .alignment(Alignment::Center)
            .style(Style::default().fg(p.text_primary)),
        rows[0],
    );
    let status = if dialog.checking {
        Line::from(Span::styled(
            format!("{} Checking for updates...", get_spinner_char()),
            Style::default().fg(p.accent_primary),
        ))
    } else if dialog.installing {
        Line::from(Span::styled(
            format!("{} Installing update...", get_spinner_char()),
            Style::default().fg(p.accent_green),
        ))
    } else if let Some(error) = &dialog.error_message {
        error_line(error)
    } else if let Some(info) = &dialog.info_message {
        Line::from(Span::styled(
            format!("ℹ {info}"),
            Style::default().fg(p.accent_primary),
        ))
    } else {
        Line::from("")
    };
    f.render_widget(
        Paragraph::new(status)
            .alignment(Alignment::Center)
            .wrap(Wrap { trim: true }),
        rows[1],
    );
    if dialog.update_available {
        let enabled = !(dialog.checking || dialog.installing);
        let items: Vec<ListItem> = ["Install update and restart", "Cancel"]
            .iter()
            .enumerate()
            .map(|(i, label)| {
                let style = if !enabled {
                    Style::default().fg(p.text_muted)
                } else if i == dialog.selection {
                    Style::default()
                        .fg(p.accent_green)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(p.text_primary)
                };
                let marker = if enabled && i == dialog.selection {
                    "➤ "
                } else {
                    "  "
                };
                ListItem::new(Line::from(vec![
                    Span::raw(marker),
                    Span::styled(*label, style),
                ]))
            })
            .collect();
        f.render_widget(List::new(items), rows[2]);
    }
}

/// Fixed text of a one-field account dialog.
struct AuthInputText {
    title: &'static str,
    label: &'static str,
    placeholder: &'static str,
    hint: &'static str,
}

fn draw_auth_input(
    f: &mut Frame,
    area: Rect,
    text: AuthInputText,
    input: &crate::app::TextInput,
    error: Option<&String>,
) {
    let AuthInputText {
        title,
        label,
        placeholder,
        hint,
    } = text;
    let keys = [("Enter", "Continue"), ("Esc", "Cancel")];
    let height = 8 + key_rows(&keys, popup_width(area.width, 60));
    let popup = popup_area_fixed_height(area, 60, height);
    let inner = dialog_frame(f, popup, title, palette().accent_primary);
    let above = draw_keys(f, inner, &keys);
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(1)])
        .split(above);
    draw_text_input(f, rows[0], label, input, placeholder, true, false);
    let message = match error {
        Some(error) => error_line(error),
        None => Line::from(Span::styled(
            hint.to_string(),
            Style::default().fg(palette().text_muted),
        )),
    };
    f.render_widget(Paragraph::new(message).wrap(Wrap { trim: true }), rows[1]);
}

pub fn draw_auth_email_dialog(f: &mut Frame, dialog: &AuthEmailDialog, area: Rect) {
    draw_auth_input(
        f,
        area,
        AuthInputText {
            title: "Sign in: Email",
            label: "Email",
            placeholder: "you@example.com",
            hint: "Press Enter to send a code to this email",
        },
        &dialog.input,
        dialog.error_message.as_ref(),
    );
}

pub fn draw_auth_code_dialog(f: &mut Frame, dialog: &AuthCodeDialog, area: Rect) {
    draw_auth_input(
        f,
        area,
        AuthInputText {
            title: "Enter verification code",
            label: "Code",
            placeholder: "6-digit code",
            hint: "Press Enter to verify and sign in",
        },
        &dialog.input,
        dialog.error_message.as_ref(),
    );
}
