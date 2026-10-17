//! Phone access dialogs: the one-time offer ("Use Rakumux from your
//! phone?") and the pairing dialog with the QR code the Rakumux app
//! scans (`crate::qr`), the URL and the token.

use ratatui::{
    layout::{Alignment, Rect},
    style::{Color, Style},
    text::{Line, Span},
    widgets::{Paragraph, Wrap},
    Frame,
};

use crate::app::types::{PairingDialog, PairingState};
use crate::components::dialogs::{dialog_frame, draw_keys, error_line, key_rows, text_rows};
use crate::components::utils::{get_spinner_char, popup_area_fixed_height, popup_width};
use crate::qr::{Qr, DARK, LIGHT};
use crate::theme;

/// Narrowest the pairing dialog gets while the screen has room.
const PAIRING_MIN_COLS: u16 = 60;

pub fn draw_phone_offer(f: &mut Frame, area: Rect) {
    let p = theme::palette();
    let muted = Style::default().fg(p.text_muted);
    let lines = vec![
        Line::from(
            "Tailscale is running on this machine. Rakumux can listen on your tailnet, so \
             the Rakumux app on your phone can show your sessions and type into them.",
        ),
        Line::from(""),
        Line::from(Span::styled(
            "Only devices on your tailnet that have the pairing code get in. Keep the code to \
             yourself: whoever has it can run commands as you.",
            muted,
        )),
        Line::from(""),
        Line::from(Span::styled(
            "Asked once. Settings (,) → Phone access changes it any time.",
            muted,
        )),
    ];
    let keys = [("y/Enter", "Turn on and pair"), ("n/Esc", "Not now")];
    let width = popup_width(area.width, 50);
    let height = (text_rows(&lines, width.saturating_sub(2)) + key_rows(&keys, width) + 3).max(10);
    let popup = popup_area_fixed_height(area, 50, height);
    let inner = dialog_frame(f, popup, "Use Rakumux from your phone?", p.accent_primary);
    let above = draw_keys(f, inner, &keys);
    f.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: true })
            .style(Style::default().fg(p.text_primary)),
        above,
    );
}

pub fn draw_pairing(f: &mut Frame, dialog: &PairingDialog, area: Rect) {
    let p = theme::palette();
    let muted = Style::default().fg(p.text_muted);
    let spinner = |text: &str| {
        Line::from(Span::styled(
            format!("{} {text}", get_spinner_char()),
            Style::default().fg(p.accent_primary),
        ))
    };
    let close = ("Esc", "Close");
    let (lines, keys): (Vec<Line>, Vec<(&str, &str)>) = match &dialog.state {
        PairingState::Loading { turning_on: true } => {
            (vec![spinner("Turning phone access on…")], vec![])
        }
        PairingState::Loading { turning_on: false } => {
            (vec![spinner("Getting the pairing code…")], vec![])
        }
        PairingState::TurningOff => (vec![spinner("Turning phone access off…")], vec![]),
        PairingState::Off => (
            vec![Line::from("Phone access is off.")],
            vec![("y/Enter", "Turn on"), close],
        ),
        PairingState::Failed(error) => (vec![error_line(error)], vec![close]),
        PairingState::Ready(pairing) => match pairing.link() {
            None => (
                vec![
                    Line::from(
                        "Phone access is on, but this machine has no Tailscale address, so \
                         nothing listens for the phone yet. Start Tailscale, then open this \
                         again (Settings → Phone access).",
                    ),
                    Line::from(""),
                    detail("Token", &pairing.token),
                ],
                vec![close, ("x", "Turn phone access off")],
            ),
            Some(link) => {
                let url = pairing.urls.first().cloned().unwrap_or_default();
                let keys = vec![close, ("x", "Turn phone access off")];
                if draw_with_qr(f, area, &link, &url, &pairing.token, &keys) {
                    return;
                }
                (
                    vec![
                        Line::from(
                            "Open the link on your phone, or enter the URL and token in the \
                             Rakumux app. A larger window shows a QR code to scan.",
                        ),
                        Line::from(""),
                        detail("URL", &url),
                        detail("Token", &pairing.token),
                        detail("Link", &link),
                    ],
                    keys,
                )
            }
        },
    };
    let width = popup_width(area.width, 50);
    let keys_rows = if keys.is_empty() {
        0
    } else {
        key_rows(&keys, width)
    };
    let height = (text_rows(&lines, width.saturating_sub(2)) + keys_rows + 3).max(8);
    let popup = popup_area_fixed_height(area, 50, height);
    let inner = dialog_frame(f, popup, "Pair your phone", p.accent_primary);
    let above = if keys.is_empty() {
        inner
    } else {
        draw_keys(f, inner, &keys)
    };
    f.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .style(muted.fg(p.text_primary)),
        above,
    );
}

/// The pairing dialog with its QR code, when the screen has room for it;
/// `false` (nothing drawn) when it doesn't.
fn draw_with_qr(
    f: &mut Frame,
    area: Rect,
    link: &str,
    url: &str,
    token: &str,
    keys: &[(&str, &str)],
) -> bool {
    let Some(code) = Qr::encode(link) else {
        return false;
    };
    let p = theme::palette();
    let intro = vec![Line::from(
        "In the Rakumux app, tap Scan QR code, or point the phone's camera at the code.",
    )];
    let details = vec![detail("URL", url), detail("Token", token)];
    let detail_cols = details.iter().map(Line::width).max().unwrap_or(0) as u16;
    let width = (code.width() + 4)
        .max(detail_cols + 4)
        .max(PAIRING_MIN_COLS)
        .min(area.width.saturating_sub(2));
    let inner_cols = width.saturating_sub(2);
    if code.width() > inner_cols {
        return false;
    }
    let intro_rows = text_rows(&intro, inner_cols);
    let detail_rows = text_rows(&details, inner_cols);
    let height = 2 + intro_rows + 1 + code.height() + 1 + detail_rows + key_rows(keys, width);
    if height > area.height {
        return false;
    }
    let popup = Rect {
        x: area.x + (area.width - width) / 2,
        y: area.y + (area.height - height) / 2,
        width,
        height,
    };
    let inner = dialog_frame(f, popup, "Pair your phone", p.accent_primary);
    let above = draw_keys(f, inner, keys);
    let text = Style::default().fg(p.text_primary);
    let mut y = above.y;
    let mut row = |rows: u16| {
        let rect = Rect {
            y,
            height: rows,
            ..above
        };
        y += rows;
        rect
    };
    f.render_widget(
        Paragraph::new(intro).wrap(Wrap { trim: true }).style(text),
        row(intro_rows),
    );
    row(1);
    f.render_widget(
        Paragraph::new(qr_lines(&code)).alignment(Alignment::Center),
        row(code.height()),
    );
    row(1);
    f.render_widget(
        Paragraph::new(details)
            .wrap(Wrap { trim: false })
            .style(text),
        row(detail_rows),
    );
    true
}

/// The code as lines of `▀`, dark and light from the 256-colour palette.
fn qr_lines(code: &Qr) -> Vec<Line<'static>> {
    let colour = |dark: bool| Color::Indexed(if dark { DARK } else { LIGHT });
    code.cells()
        .into_iter()
        .map(|row| {
            let mut spans: Vec<Span> = Vec::new();
            let mut run = String::new();
            let mut last = None;
            for (upper, lower) in row {
                if last.is_some() && last != Some((upper, lower)) {
                    let (u, l) = last.unwrap_or_default();
                    spans.push(Span::styled(
                        std::mem::take(&mut run),
                        Style::default().fg(colour(u)).bg(colour(l)),
                    ));
                }
                last = Some((upper, lower));
                run.push('▀');
            }
            if let Some((u, l)) = last {
                spans.push(Span::styled(
                    run,
                    Style::default().fg(colour(u)).bg(colour(l)),
                ));
            }
            Line::from(spans)
        })
        .collect()
}

/// `Label  value`, the label muted.
fn detail(label: &str, value: &str) -> Line<'static> {
    let p = theme::palette();
    Line::from(vec![
        Span::styled(format!("{label:<7}"), Style::default().fg(p.text_muted)),
        Span::styled(value.to_string(), Style::default().fg(p.text_primary)),
    ])
}
