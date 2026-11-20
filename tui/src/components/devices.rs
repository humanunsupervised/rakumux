//! The paired devices dialog (Settings → Paired devices): the devices that
//! sign what rakumux.com asks of this machine, and pairing one with a code
//! (its link and QR code), confirmed here by the device's name and
//! fingerprint.

use ratatui::{
    layout::{Alignment, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Paragraph, Wrap},
    Frame,
};

use rakumux_sensagent::feed::pairing_failure_text;
use rakumux_sensagent::signing::pairing::PairingState as Signer;
use rakumux_sensagent::signing::KeyFormat;

use super::phone_access::{detail, qr_lines};
use crate::app::types::{DevicesDialog, DevicesState};
use crate::components::dialogs::{
    dialog_frame, draw_keys, error_line, key_rows, padded, padding, text_rows,
};
use crate::components::utils::{get_spinner_char, popup_area_fixed_height, popup_width};
use crate::printable::printable;
use crate::qr::Qr;
use crate::theme;

const TITLE: &str = "Paired devices";

pub fn draw_devices(f: &mut Frame, dialog: &DevicesDialog, area: Rect) {
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
        DevicesState::Loading => (vec![spinner("Reading the paired devices…")], vec![close]),
        DevicesState::Starting => (vec![spinner("Getting a code…")], vec![("Esc", "Cancel")]),
        DevicesState::Failed(error) => (vec![error_line(error)], vec![("Enter", "Back"), close]),
        DevicesState::List if dialog.devices.is_empty() => (
            vec![
                Line::from("No paired devices."),
                Line::from(""),
                Line::from(Span::styled(
                    "Pair a browser or the Rakumux app: from then on this machine carries out \
                     what rakumux.com asks only when a paired device signed it.",
                    muted,
                )),
            ],
            vec![("Enter", "Pair a device"), close],
        ),
        DevicesState::List => {
            let mut lines: Vec<Line> = dialog
                .devices
                .iter()
                .enumerate()
                .map(|(index, device)| {
                    let kind = match device.format {
                        KeyFormat::Webauthn => "passkey",
                        KeyFormat::P256 => "key",
                    };
                    let mut style = Style::default().fg(p.text_primary);
                    if index == dialog.selected {
                        style = style.add_modifier(Modifier::BOLD).fg(p.accent_primary);
                    }
                    Line::from(vec![
                        Span::styled(printable(&device.name), style),
                        Span::styled(format!("  {kind}  {}", device.fingerprint), muted),
                    ])
                })
                .collect();
            lines.push(Line::from(""));
            let remove = if dialog.removing {
                ("d", "Forget it")
            } else {
                ("d d", "Forget")
            };
            if dialog.removing {
                lines.push(error_line("Press d again to forget it."));
            }
            (lines, vec![("n", "Pair another"), remove, close])
        }
        DevicesState::Pairing(view) => match &view.state {
            Signer::Open => {
                let keys = vec![("Esc", "Cancel")];
                if draw_code(f, area, &view.link, &view.code, view.opened, &keys) {
                    return;
                }
                (
                    vec![
                        Line::from(
                            "On the device, open the link, or scan it with the Rakumux app.",
                        ),
                        Line::from(""),
                        detail("Link", &view.link),
                        detail("Code", &view.code),
                        Line::from(""),
                        waiting(view.opened),
                    ],
                    keys,
                )
            }
            Signer::Proposed { name, fingerprint } => (
                vec![
                    Line::from(format!("Pair {}?", printable(name))),
                    Line::from(""),
                    detail("Fingerprint", fingerprint),
                    Line::from(Span::styled("The device shows the same.", muted)),
                ],
                vec![("y", "Pair"), ("n", "Don't"), ("Esc", "Cancel")],
            ),
            Signer::Paired { name } => (
                vec![
                    Line::from(format!("Paired {}.", printable(name))),
                    Line::from(""),
                    Line::from(Span::styled(
                        "This machine now carries out what rakumux.com asks only when a paired \
                         device signed it.",
                        muted,
                    )),
                ],
                vec![("Enter", "Done"), close],
            ),
            Signer::Failed { failure } => (
                vec![error_line(&format!(
                    "Not paired: {}.",
                    pairing_failure_text(*failure)
                ))],
                vec![("Enter", "Back"), close],
            ),
        },
    };
    let (pad_x, pad_y) = padding(area);
    let width = popup_width(area.width, 56);
    let keys_rows = key_rows(&keys, width.saturating_sub(2 * pad_x));
    let text = text_rows(&lines, width.saturating_sub(2 + 2 * pad_x));
    let height = (text + keys_rows + 3 + 2 * pad_y).max(8);
    let popup = popup_area_fixed_height(area, 56, height);
    let inner = padded(
        dialog_frame(f, popup, TITLE, p.accent_primary),
        pad_x,
        pad_y,
    );
    let above = draw_keys(f, inner, &keys);
    f.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .style(muted.fg(p.text_primary)),
        above,
    );
}

fn waiting(opened: bool) -> Line<'static> {
    let p = theme::palette();
    let text = if opened {
        "Waiting for the device…"
    } else {
        "Opening the pairing at rakumux.com…"
    };
    Line::from(Span::styled(
        format!("{} {text}", get_spinner_char()),
        Style::default().fg(p.accent_primary),
    ))
}

/// The code with its link's QR code, when the screen has room; `false`
/// (nothing drawn) when it doesn't.
fn draw_code(
    f: &mut Frame,
    area: Rect,
    link: &str,
    code: &str,
    opened: bool,
    keys: &[(&str, &str)],
) -> bool {
    let Some(qr) = Qr::encode(link) else {
        return false;
    };
    let p = theme::palette();
    let intro = vec![Line::from(
        "On the device to pair, open the link in a browser, or scan the code with the Rakumux app.",
    )];
    let details = vec![detail("Link", link), detail("Code", code)];
    let (pad_x, pad_y) = padding(area);
    let width = (qr.width() + 4 + 2 * pad_x)
        .max(60)
        .min(area.width.saturating_sub(2));
    let inner_cols = width.saturating_sub(2 + 2 * pad_x);
    if qr.width() > inner_cols {
        return false;
    }
    let intro_rows = text_rows(&intro, inner_cols);
    let detail_rows = text_rows(&details, inner_cols);
    let keys_rows = key_rows(keys, width.saturating_sub(2 * pad_x));
    let height = 2 + 2 * pad_y + intro_rows + 1 + qr.height() + 1 + detail_rows + 1 + 1 + keys_rows;
    if height > area.height {
        return false;
    }
    let popup = Rect {
        x: area.x + (area.width - width) / 2,
        y: area.y + (area.height - height) / 2,
        width,
        height,
    };
    let inner = padded(
        dialog_frame(f, popup, TITLE, p.accent_primary),
        pad_x,
        pad_y,
    );
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
        Paragraph::new(qr_lines(&qr)).alignment(Alignment::Center),
        row(qr.height()),
    );
    row(1);
    f.render_widget(
        Paragraph::new(details)
            .wrap(Wrap { trim: false })
            .style(text),
        row(detail_rows),
    );
    row(1);
    f.render_widget(Paragraph::new(waiting(opened)), row(1));
    true
}
