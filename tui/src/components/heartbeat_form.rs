//! The New heartbeat form (`app/heartbeat_form.rs`): a label and its value a
//! row, the text ones on a surface of their own, the schedule's and the
//! agent's choices side by side or between `‹` and `›`, then the buttons.

use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};
use session_core::heartbeat::Weekday;

use super::dialogs::{dialog_frame, error_line, padded, padding};
use super::format::truncate;
use super::utils::get_spinner_char;
use crate::app::heartbeat_form::{FormField, FormHit, HeartbeatForm, Repeat};
use crate::app::{App, Hit, TextInput};
use crate::layout::str_width;
use crate::printable::printable;
use crate::theme;

/// Widest the form gets.
const WIDTH: u16 = 76;
/// The labels' column.
const LABEL: u16 = 8;
/// The task's rows, while the screen has them.
const TASK_ROWS: u16 = 6;
/// Columns of the interval or time.
const WHEN_COLS: u16 = 7;

/// The form's width on a screen `width` columns wide.
fn popup_cols(width: u16) -> u16 {
    WIDTH.min(width.saturating_sub(2))
}

/// The columns the task is written in on a screen `width` columns wide.
pub fn task_cols(width: u16) -> usize {
    let popup = Rect {
        width: popup_cols(width).saturating_sub(2),
        height: 20,
        ..Rect::default()
    };
    let (x, _) = padding(Rect { width, ..popup });
    usize::from(padded(popup, x, 0).width.saturating_sub(LABEL)).max(1)
}

/// The text a one-line field shows in `cols`, and its cursor's column.
fn shown(input: &TextInput, cols: usize) -> (String, usize) {
    let chars: Vec<char> = input.value().chars().collect();
    let cursor = input.cursor_column();
    let start = (cursor + 1).saturating_sub(cols);
    let text = chars[start.min(chars.len())..].iter().take(cols).collect();
    (text, cursor - start)
}

/// Draw the form in the middle of `area` and record its click targets.
pub fn draw_heartbeat_form(
    f: &mut Frame,
    app: &App,
    form: &HeartbeatForm,
    area: Rect,
    hits: &mut Vec<(Rect, Hit)>,
) {
    let p = theme::palette();
    let (pad_x, pad_y) = padding(area);
    // Name, the task, runs, its days, folder, agent, the message, the
    // buttons; blank rows between them while there's room.
    let fixed = 7;
    let borders = 2 + 2 * pad_y;
    let room = area.height.saturating_sub(fixed + borders);
    let task_rows = TASK_ROWS.min(room).max(1);
    let gap = u16::from(room >= task_rows + 2);
    let width = popup_cols(area.width);
    let height = (fixed + borders + task_rows + 2 * gap).min(area.height);
    if width < 24 || height < 6 {
        return;
    }
    let popup = Rect {
        x: area.x + (area.width - width) / 2,
        y: area.y + (area.height - height) / 3,
        width,
        height,
    };
    let inner = padded(
        dialog_frame(f, popup, "New heartbeat", p.accent_primary),
        pad_x,
        pad_y,
    );
    let muted = Style::default().fg(p.text_muted);
    let plain = Style::default().fg(p.text_primary);
    let lit = Style::default()
        .fg(p.accent_primary)
        .add_modifier(Modifier::BOLD);
    let field_bg = Style::default().fg(p.text_bright).bg(p.surface);
    let on = |field: FormField| form.field == field && !form.busy;
    let mut y = inner.y;
    let row = |y: u16, rows: u16| {
        (y < inner.bottom()).then(|| Rect {
            y,
            height: rows.min(inner.bottom() - y),
            ..inner
        })
    };
    let value_of = |rect: Rect| Rect {
        x: rect.x + LABEL,
        width: rect.width.saturating_sub(LABEL),
        ..rect
    };
    let label = |f: &mut Frame, rect: Rect, text: &str, focused: bool| {
        f.render_widget(
            Paragraph::new(Span::styled(
                text.to_string(),
                if focused { lit } else { muted },
            )),
            Rect {
                width: LABEL.min(rect.width),
                height: 1,
                ..rect
            },
        );
    };
    let form_hit = |hit| Hit::HeartbeatForm(hit);

    // Name.
    if let Some(rect) = row(y, 1) {
        label(f, rect, "Name", on(FormField::Name));
        let cell = value_of(rect);
        let (text, cursor) = shown(&form.name, usize::from(cell.width).max(1));
        let text = match form.name.is_empty() {
            true => Span::styled("Heartbeat", Style::default().fg(p.text_muted)),
            false => Span::raw(text),
        };
        f.render_widget(Paragraph::new(text).style(field_bg), cell);
        if on(FormField::Name) {
            f.set_cursor(cell.x + cursor as u16, cell.y);
        }
        hits.push((rect, form_hit(FormHit::Field(FormField::Name))));
    }
    y += 1 + gap;

    // The task, word-wrapped, the cursor's row in view.
    if let Some(rect) = row(y, task_rows) {
        label(f, rect, "Task", on(FormField::Task));
        let cell = value_of(rect);
        let cols = usize::from(cell.width).max(1);
        let rows = form.task.rows(cols);
        let (cursor_row, cursor_col) = form.task.cursor_at(cols);
        let first = (cursor_row + 1).saturating_sub(usize::from(cell.height));
        let lines: Vec<Line> = rows
            .iter()
            .skip(first)
            .take(usize::from(cell.height))
            .map(|range| Line::from(form.task.value()[range.clone()].to_string()))
            .collect();
        f.render_widget(Paragraph::new(lines).style(field_bg), cell);
        if on(FormField::Task) {
            f.set_cursor(
                cell.x + cursor_col as u16,
                cell.y + (cursor_row - first) as u16,
            );
        }
        hits.push((rect, form_hit(FormHit::Field(FormField::Task))));
    }
    y += task_rows + gap;

    // How it repeats, and how often or when.
    if let Some(rect) = row(y, 1) {
        label(
            f,
            rect,
            "Runs",
            on(FormField::Runs) || on(FormField::When) || on(FormField::Days),
        );
        // The words line up with the other values; their chips stick out.
        let mut x = value_of(rect).x.saturating_sub(1);
        for repeat in Repeat::ALL {
            let chosen = repeat == form.repeat;
            let style = match (chosen, on(FormField::Runs)) {
                (true, true) => lit.add_modifier(Modifier::REVERSED),
                (true, false) => lit,
                (false, _) => muted,
            };
            let text = format!(" {} ", repeat.label());
            let cell = Rect {
                x,
                width: (str_width(&text) as u16).min(rect.right().saturating_sub(x)),
                ..rect
            };
            x += cell.width;
            f.render_widget(Paragraph::new(Span::styled(text, style)), cell);
            hits.push((cell, form_hit(FormHit::Repeat(repeat))));
        }
        let cell = Rect {
            x: x + 1,
            width: WHEN_COLS.min(rect.right().saturating_sub(x + 1)),
            ..rect
        };
        if cell.width > 0 {
            let (text, cursor) = shown(form.when(), usize::from(cell.width));
            f.render_widget(Paragraph::new(text).style(field_bg), cell);
            if on(FormField::When) {
                f.set_cursor(cell.x + cursor as u16, cell.y);
            }
            hits.push((cell, form_hit(FormHit::Field(FormField::When))));
        }
        hits.push((rect, form_hit(FormHit::Field(FormField::Runs))));
    }
    y += 1;

    // The days, when it runs on days.
    if let (Some(rect), Repeat::Days) = (row(y, 1), form.repeat) {
        let mut x = value_of(rect).x;
        for (index, day) in Weekday::ALL.into_iter().enumerate() {
            let mut style = if form.days[index] { lit } else { muted };
            if on(FormField::Days) && form.day == index {
                style = style.add_modifier(Modifier::REVERSED);
            }
            let cell = Rect {
                x,
                width: 3.min(rect.right().saturating_sub(x)),
                ..rect
            };
            x += 4;
            if cell.width > 0 {
                f.render_widget(Paragraph::new(Span::styled(day.short(), style)), cell);
                hits.push((cell, form_hit(FormHit::Day(index))));
            }
        }
    }
    y += 1;

    // The folder and the agent: one of several, between arrows.
    let folder = form
        .folders
        .get(form.folder)
        .map(|folder| app.folders.name_of(folder))
        .unwrap_or_default();
    let agent = form.agents.get(form.agent).cloned().unwrap_or_default();
    for (text, field, value) in [
        ("Folder", FormField::Folder, folder),
        ("Agent", FormField::Agent, agent),
    ] {
        if let Some(rect) = row(y, 1) {
            let focused = on(field);
            label(f, rect, text, focused);
            let cell = value_of(rect);
            let value = truncate(
                &printable(&value),
                usize::from(cell.width.saturating_sub(4)),
            );
            let arrow = if focused { lit } else { muted };
            let width = str_width(&value) as u16;
            f.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::styled("‹ ", arrow),
                    Span::styled(value, if focused { lit } else { plain }),
                    Span::styled(" ›", arrow),
                ])),
                cell,
            );
            for (x, delta) in [(cell.x, -1), (cell.x + 2 + width, 1)] {
                let arrow = Rect {
                    x,
                    width: 2,
                    ..cell
                };
                hits.push((arrow, form_hit(FormHit::Step(field, delta))));
            }
            hits.push((rect, form_hit(FormHit::Field(field))));
        }
        y += 1;
    }

    // What's wrong, then the buttons on the last row.
    let message = match (&form.error, form.busy) {
        (Some(error), _) => Some(error_line(&truncate(
            &printable(error),
            usize::from(inner.width.saturating_sub(2)),
        ))),
        (None, true) => Some(Line::from(Span::styled(
            get_spinner_char(),
            Style::default().fg(p.accent_primary),
        ))),
        (None, false) => None,
    };
    let last = inner.bottom().saturating_sub(1);
    if let Some(message) = message {
        let y = last.saturating_sub(1).max(inner.y);
        f.render_widget(
            Paragraph::new(message),
            Rect {
                y,
                height: 1,
                ..inner
            },
        );
    }
    let mut x = inner.x;
    for (text, focused, hit) in [
        (
            " Create ",
            on(FormField::Create),
            FormHit::Field(FormField::Create),
        ),
        (
            " Ask agent ",
            on(FormField::AskAgent),
            FormHit::Field(FormField::AskAgent),
        ),
        (" Esc cancel", false, FormHit::Cancel),
    ] {
        let width = (str_width(text) as u16).min(inner.right().saturating_sub(x));
        if width == 0 {
            break;
        }
        let cell = Rect {
            x,
            y: last,
            width,
            height: 1,
        };
        let style = match (focused, hit) {
            (_, FormHit::Cancel) => muted,
            (true, _) => lit.add_modifier(Modifier::REVERSED),
            (false, _) => field_bg.add_modifier(Modifier::BOLD),
        };
        f.render_widget(Paragraph::new(Span::styled(text, style)), cell);
        hits.push((cell, form_hit(hit)));
        x += width + 2;
    }
    hits.push((popup, form_hit(FormHit::Inside)));
}
