//! The New heartbeat form (`app/heartbeat_form.rs`): a label and its value a
//! row, the text ones on a surface of their own, the schedule's and the
//! agent's choices side by side or between `‹` and `›`, the command they
//! make, then the buttons. On the agent or its arguments what the agent takes
//! is listed over the rows under them. The prompt takes as many rows as its
//! text, from [`PROMPT_ROWS`] to what the screen has, then scrolls, a
//! scrollbar down its last column.

use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Clear, Paragraph},
    Frame,
};
use session_core::commands;
use session_core::heartbeat::Weekday;

use super::dialogs::{dialog_frame, error_line, padded, padding};
use super::format::truncate;
use super::scrollbar::draw_scrollbar;
use super::utils::get_spinner_char;
use crate::app::argument_suggest::Suggestions;
use crate::app::heartbeat_form::{FormField, FormHit, HeartbeatForm, Repeat};
use crate::app::{App, Hit, TextInput};
use crate::layout::str_width;
use crate::printable::printable;
use crate::theme;

/// Widest the form gets.
const WIDTH: u16 = 92;
/// The labels' column.
const LABEL: u16 = 10;
/// The prompt's rows at least, while the screen has them; more as its text
/// takes them.
const PROMPT_ROWS: u16 = 8;
/// Fewer than these and the blank rows and the command go.
const PROMPT_MIN_ROWS: u16 = 3;
/// The rows of the command they make.
const COMMAND_ROWS: u16 = 2;
/// Columns of the interval or time.
const WHEN_COLS: u16 = 7;

/// The form's width on a screen `width` columns wide.
fn popup_cols(width: u16) -> u16 {
    WIDTH.min(width.saturating_sub(2))
}

/// The columns the prompt is written in on a screen `width` columns wide:
/// its field's, but for the scrollbar's.
pub fn prompt_cols(width: u16) -> usize {
    let popup = Rect {
        width: popup_cols(width).saturating_sub(2),
        height: 20,
        ..Rect::default()
    };
    let (x, _) = padding(Rect { width, ..popup });
    usize::from(padded(popup, x, 0).width.saturating_sub(LABEL + 1)).max(1)
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
    // Name, runs, its days, folder, agent, arguments, the prompt, the
    // message, the buttons; while there's room a blank row over the agent
    // and the prompt, and the command under a third.
    let fixed = 8;
    let borders = 2 + 2 * pad_y;
    let room = area.height.saturating_sub(fixed + borders);
    let spare = 3 + COMMAND_ROWS;
    let gap = u16::from(room >= PROMPT_MIN_ROWS + spare);
    let text_rows = form.prompt.rows(prompt_cols(area.width)).len();
    let wanted = u16::try_from(text_rows)
        .unwrap_or(u16::MAX)
        .max(PROMPT_ROWS);
    let prompt_rows = wanted.min(room.saturating_sub(spare * gap)).max(1);
    let width = popup_cols(area.width);
    let height = (fixed + borders + prompt_rows + spare * gap).min(area.height);
    if width < 24 || height < 6 {
        return;
    }
    // Where it is with a short prompt; a longer one grows it down, then up.
    let short = (height - prompt_rows + prompt_rows.min(PROMPT_ROWS)).min(area.height);
    let popup = Rect {
        x: area.x + (area.width - width) / 2,
        y: (area.y + (area.height - short) / 3).min(area.bottom() - height),
        width,
        height,
    };
    let title = match form.editing {
        Some(_) => "Edit heartbeat",
        None => "New heartbeat",
    };
    let inner = padded(
        dialog_frame(f, popup, title, p.accent_primary),
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
    y += 1;

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
    let agent = form
        .choice()
        .map(|choice| choice.program.clone())
        .unwrap_or_default();
    for (text, field, value) in [
        ("Folder", FormField::Folder, folder),
        ("Agent", FormField::Agent, agent),
    ] {
        // The command starts with its agent.
        if field == FormField::Agent {
            y += gap;
        }
        if let Some(rect) = row(y, 1) {
            let focused = on(field);
            label(f, rect, text, focused);
            let cell = value_of(rect);
            let value = truncate(
                &printable(&value),
                usize::from(cell.width.saturating_sub(4)),
            );
            // Its folder is where it was made.
            if field == FormField::Folder && form.editing.is_some() {
                f.render_widget(Paragraph::new(Span::styled(value, plain)), cell);
                y += 1;
                continue;
            }
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

    // Its arguments.
    let arguments_y = y;
    if let (Some(rect), Some(choice)) = (row(y, 1), form.choice()) {
        label(f, rect, "Arguments", on(FormField::Arguments));
        let cell = value_of(rect);
        let (text, cursor) = shown(&choice.arguments, usize::from(cell.width).max(1));
        f.render_widget(Paragraph::new(text).style(field_bg), cell);
        if on(FormField::Arguments) {
            f.set_cursor(cell.x + cursor as u16, cell.y);
        }
        hits.push((rect, form_hit(FormHit::Field(FormField::Arguments))));
    }
    y += 1 + gap;
    // The suggestions are over what's under them, for clicks too.
    let under = hits.len();

    // The prompt, word-wrapped, the cursor's row in view (unless the wheel
    // moved it), a scrollbar beside it when it's taller than its field.
    if let Some(rect) = row(y, prompt_rows) {
        label(f, rect, "Prompt", on(FormField::Prompt));
        let cell = value_of(rect);
        let cols = usize::from(cell.width.saturating_sub(1)).max(1);
        let rows = form.prompt.rows(cols);
        let (cursor_row, cursor_col) = form.prompt.cursor_at(cols);
        let height = usize::from(cell.height).max(1);
        let view = &form.prompt_view;
        let mut top = view.top.get();
        if !view.wheeled.get() {
            top = top.clamp((cursor_row + 1).saturating_sub(height), cursor_row);
        }
        let max = rows.len().saturating_sub(height);
        let top = top.min(max);
        view.top.set(top);
        view.rows.set(height);
        let lines: Vec<Line> = rows
            .iter()
            .skip(top)
            .take(height)
            .map(|range| Line::from(form.prompt.value()[range.clone()].to_string()))
            .collect();
        f.render_widget(Paragraph::new(lines).style(field_bg), cell);
        if max > 0 {
            draw_scrollbar(f, cell, top, max, rows.len());
        }
        if on(FormField::Prompt) && (top..top + height).contains(&cursor_row) {
            f.set_cursor(
                cell.x + cursor_col as u16,
                cell.y + (cursor_row - top) as u16,
            );
        }
        hits.push((rect, form_hit(FormHit::Field(FormField::Prompt))));
    }
    y += prompt_rows + gap;

    // The command a run is, as much of it as two rows hold.
    if let (1, Some(rect)) = (gap, row(y, COMMAND_ROWS)) {
        let cell = value_of(rect);
        let room = usize::from(cell.width) * usize::from(cell.height);
        let command = format!("$ {}", command_line(form));
        let mut command = TextInput::new(truncate(&printable(&command), room));
        let mut rows = command.rows(usize::from(cell.width));
        // Wrapped at its words it may take more: cut it again.
        while rows.len() > usize::from(cell.height) {
            let cols = str_width(command.value()).saturating_sub(2);
            command.set(truncate(command.value(), cols));
            rows = command.rows(usize::from(cell.width));
        }
        let lines: Vec<Line> = rows
            .into_iter()
            .map(|range| Line::from(command.value()[range].to_string()))
            .collect();
        f.render_widget(Paragraph::new(lines).style(muted), cell);
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
    let mut buttons = vec![(
        match form.editing {
            Some(_) => " Save ",
            None => " Create ",
        },
        on(FormField::Create),
        FormHit::Field(FormField::Create),
    )];
    if form.editing.is_none() {
        buttons.push((
            " Ask agent ",
            on(FormField::AskAgent),
            FormHit::Field(FormField::AskAgent),
        ));
    }
    buttons.push((" Esc cancel", false, FormHit::Cancel));
    for (text, focused, hit) in buttons {
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

    if let (Some(found), false) = (form.suggestions(), form.busy) {
        let top = arguments_y + 1;
        let list = Rect {
            y: top,
            height: last.saturating_sub(top),
            ..value_of(inner)
        };
        let rows = draw_suggestions(f, &found, form.pick, list);
        hits.splice(under..under, rows);
    }
}

/// The command the form makes: its prompt where its agent takes it, on one
/// line.
fn command_line(form: &HeartbeatForm) -> String {
    let line = form
        .choice()
        .map(|choice| choice.line())
        .unwrap_or_default();
    let words: Vec<&str> = form.prompt.value().split_whitespace().collect();
    if words.is_empty() {
        return line;
    }
    let prompt = format!("\"{}\"", words.join(" ").replace('"', "\\\""));
    commands::with_prompt(&line, &prompt).unwrap_or(line)
}

/// The suggested arguments, as many as fit `area` around the one picked;
/// returns their click targets.
fn draw_suggestions(
    f: &mut Frame,
    found: &Suggestions,
    pick: Option<usize>,
    area: Rect,
) -> Vec<(Rect, Hit)> {
    let p = theme::palette();
    let height = found.rows.len().min(usize::from(area.height));
    if height == 0 || area.width < 8 {
        return Vec::new();
    }
    let first = pick.map_or(0, |pick| (pick + 1).saturating_sub(height));
    let below = found.rows.len() - first - height;
    let named = |row: &crate::app::argument_suggest::Suggestion| match row.value {
        Some(value) => format!("{} <{value}>", row.text),
        None => row.text.to_string(),
    };
    let widest = found.rows.iter().map(|row| str_width(&named(row))).max();
    let about_at = widest.unwrap_or(0) + 3;
    let cols = usize::from(area.width);
    let mut hits = Vec::new();
    for (line, (index, row)) in found
        .rows
        .iter()
        .enumerate()
        .skip(first)
        .take(height)
        .enumerate()
    {
        let cell = Rect {
            y: area.y + line as u16,
            height: 1,
            ..area
        };
        let picked = pick == Some(index);
        let background = Style::default().bg(if picked { p.selection } else { p.surface });
        let name = truncate(&named(row), cols.saturating_sub(2));
        let mut spans = vec![Span::styled(
            format!(" {name}"),
            Style::default()
                .fg(if row.takes {
                    p.text_bright
                } else {
                    p.text_muted
                })
                .add_modifier(if picked {
                    Modifier::BOLD
                } else {
                    Modifier::empty()
                }),
        )];
        // How many more there are, at the end of the last row.
        let more = match below > 0 && line + 1 == height {
            true => format!("{below} more "),
            false => String::new(),
        };
        let used = 1 + str_width(&name);
        let room = cols.saturating_sub(about_at.max(used + 2) + str_width(&more) + 1);
        let muted = Style::default().fg(p.text_muted);
        let mut at = used;
        if room >= 8 && !row.about.is_empty() {
            let gap = about_at.max(used + 2) - used;
            let about = truncate(row.about, room);
            at += gap + str_width(&about);
            spans.push(Span::raw(" ".repeat(gap)));
            spans.push(Span::styled(about, muted));
        }
        if !more.is_empty() && at + str_width(&more) < cols {
            spans.push(Span::raw(" ".repeat(cols - at - str_width(&more))));
            spans.push(Span::styled(more, muted));
        }
        f.render_widget(Clear, cell);
        f.render_widget(Paragraph::new(Line::from(spans)).style(background), cell);
        let hit = match row.takes {
            true => FormHit::Suggestion(index),
            false => FormHit::Inside,
        };
        hits.push((cell, Hit::HeartbeatForm(hit)));
    }
    hits
}
