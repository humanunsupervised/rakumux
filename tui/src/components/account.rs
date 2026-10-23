//! The account dialog (Settings → Account): signing in to the Rakumux
//! account with a code approved in any browser (a link and its QR code),
//! who is signed in and whether AI session summaries are on, signing out.

use ratatui::{
    layout::{Alignment, Rect},
    style::Style,
    text::{Line, Span},
    widgets::{Paragraph, Wrap},
    Frame,
};

use rakumux_sensagent::owner::protocol::SignInView;

use super::phone_access::{detail, qr_lines};
use crate::account::approval_link;
use crate::app::types::{AccountDialog, AccountDialogState};
use crate::components::dialogs::{
    dialog_frame, draw_keys, error_line, key_rows, padded, padding, text_rows,
};
use crate::components::utils::{get_spinner_char, popup_area_fixed_height, popup_width};
use crate::qr::Qr;
use crate::theme;

const TITLE: &str = "Rakumux account";

/// Narrowest the sign-in dialog gets while the screen has room.
const SIGN_IN_MIN_COLS: u16 = 60;

pub fn draw_account(f: &mut Frame, dialog: &AccountDialog, area: Rect) {
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
        AccountDialogState::Loading => (vec![spinner("Checking your account…")], vec![close]),
        AccountDialogState::Starting => (
            vec![spinner("Getting a sign-in code…")],
            vec![("Esc", "Cancel")],
        ),
        AccountDialogState::SigningOut => (vec![spinner("Signing out…")], vec![]),
        AccountDialogState::SignedOut => (
            vec![
                Line::from(
                    "Sign in to see a short AI description under each session, written from \
                     what it's doing.",
                ),
                Line::from(""),
                Line::from(Span::styled(
                    "Live sessions' recent terminal output and agent reports are sent to \
                     rakumux.com to write them (secrets it recognises are removed first). Turn \
                     AI session summaries off any time at rakumux.com/features.",
                    muted,
                )),
            ],
            vec![("Enter", "Sign in"), close],
        ),
        AccountDialogState::SignedIn(account) => {
            let summaries = match account.summaries {
                Some(true) => "on",
                Some(false) => "off",
                None => "checking…",
            };
            (
                vec![
                    Line::from(format!("Signed in as {}.", account.name())),
                    Line::from(""),
                    Line::from(format!("AI session summaries: {summaries}")),
                    Line::from(""),
                    Line::from(Span::styled(
                        "Turn them on or off at rakumux.com/features.",
                        muted,
                    )),
                ],
                vec![("o", "Sign out"), close],
            )
        }
        AccountDialogState::Failed(error) => {
            (vec![error_line(error)], vec![("Enter", "Try again"), close])
        }
        AccountDialogState::Waiting(view) => {
            let keys = vec![("c", "Copy link"), ("Esc", "Cancel")];
            if draw_sign_in(f, area, view, &keys) {
                return;
            }
            let (code, link) = code_and_link(view);
            (
                vec![
                    Line::from("Open the link in any browser, sign in, and approve the code."),
                    Line::from(""),
                    detail("Link", link),
                    detail("Code", code),
                    Line::from(""),
                    spinner("Waiting for approval…"),
                ],
                keys,
            )
        }
    };
    let (pad_x, pad_y) = padding(area);
    let width = popup_width(area.width, 50);
    let keys_rows = if keys.is_empty() {
        0
    } else {
        key_rows(&keys, width.saturating_sub(2 * pad_x))
    };
    let text = text_rows(&lines, width.saturating_sub(2 + 2 * pad_x));
    let height = (text + keys_rows + 3 + 2 * pad_y).max(8);
    let popup = popup_area_fixed_height(area, 50, height);
    let inner = padded(
        dialog_frame(f, popup, TITLE, p.accent_primary),
        pad_x,
        pad_y,
    );
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

fn code_and_link(view: &SignInView) -> (&str, &str) {
    let code = match view {
        SignInView::Waiting { user_code, .. } => user_code.as_str(),
        _ => "",
    };
    (code, approval_link(view).unwrap_or_default())
}

/// A waiting sign-in with the link's QR code, when the screen has room for
/// it; `false` (nothing drawn) when it doesn't.
fn draw_sign_in(f: &mut Frame, area: Rect, view: &SignInView, keys: &[(&str, &str)]) -> bool {
    let (code, link) = code_and_link(view);
    let Some(qr) = Qr::encode(link) else {
        return false;
    };
    let p = theme::palette();
    let intro = vec![Line::from(
        "Open the link in any browser (or scan the code with your phone), sign in, and \
         approve the code.",
    )];
    let details = vec![detail("Link", link), detail("Code", code)];
    let waiting = vec![Line::from(Span::styled(
        format!("{} Waiting for approval…", get_spinner_char()),
        Style::default().fg(p.accent_primary),
    ))];
    let (pad_x, pad_y) = padding(area);
    let width = (qr.width() + 4 + 2 * pad_x)
        .max(SIGN_IN_MIN_COLS)
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
    f.render_widget(Paragraph::new(waiting), row(1));
    true
}
