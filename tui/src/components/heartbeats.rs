//! The left column's Heartbeats tab (`app/heartbeats.rs`): the heartbeats, two lines
//! each (a mark, the name and when it runs next; its schedule, folder and
//! last run), by folder and name, the selected one's buttons under it, then
//! `+ New heartbeat`; or one heartbeat whole: its buttons, how it runs, its
//! command and prompt in full, and every run, newest first (the session pane
//! shows the selected one).

use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Paragraph, Wrap},
    Frame,
};
use session_core::heartbeat::{Heartbeat, RunOutcome};

use crate::app::heartbeats::{run_state, BeatAction};
use crate::app::{App, Hit, TextInput};
use crate::components::dialogs::error_line;
use crate::components::feed::card_stamp;
use crate::components::format::{now_secs, relative_time, status_glyph, truncate};
use crate::components::scrollbar::draw_scrollbar;
use crate::components::utils::get_spinner_char;
use crate::layout::str_width;
use crate::printable::printable;
use crate::theme;

/// The tab in `area` (the left column's, under its band).
pub fn draw_beats(f: &mut Frame, app: &App, area: Rect, hits: &mut Vec<(Rect, Hit)>) {
    let p = theme::palette();
    let view = &app.beats;
    // A row down, a column each side, as rtop.
    let body = Rect {
        x: area.x.saturating_add(1),
        y: area.y.saturating_add(1),
        width: area.width.saturating_sub(2),
        height: area.height.saturating_sub(1),
    };
    if body.height < 2 || body.width < 8 {
        return;
    }
    if let Some(error) = &view.error {
        f.render_widget(
            Paragraph::new(vec![error_line(error)]).wrap(Wrap { trim: true }),
            body,
        );
        return;
    }
    if !view.loaded {
        f.render_widget(
            Paragraph::new(Span::styled(
                format!("{} Reading…", get_spinner_char()),
                Style::default().fg(p.accent_primary),
            )),
            body,
        );
        return;
    }
    match view.read() {
        Some(heartbeat) => draw_whole(f, app, heartbeat, body, hits),
        None => draw_list(f, app, body, hits),
    }
}

/// `◌` its agent is setting it up, `●` on, `○` paused or not filled in yet.
fn mark(heartbeat: &Heartbeat) -> (&'static str, Style) {
    let p = theme::palette();
    match (heartbeat.enabled, heartbeat.awaiting_approval) {
        (_, true) => ("◌", Style::default().fg(p.accent_purple)),
        (true, false) => ("●", Style::default().fg(p.accent_green)),
        (false, false) => ("○", Style::default().fg(p.text_muted)),
    }
}

/// When it runs next, or why it doesn't.
fn when(heartbeat: &Heartbeat, now: i64) -> String {
    match (
        heartbeat.enabled,
        heartbeat.awaiting_approval,
        heartbeat.next_run_at,
    ) {
        (_, true, _) => "setting up".into(),
        (true, false, Some(at)) if at > now => format!("in {}", relative_time(at, now)),
        (true, false, _) => "due".into(),
        (false, false, _) if !heartbeat.is_complete() => "empty".into(),
        (false, false, _) => "paused".into(),
    }
}

/// Its last run, as a few words.
fn last(heartbeat: &Heartbeat, now: i64) -> Option<String> {
    let run = heartbeat.last_run.as_ref()?;
    let ago = relative_time(now, run.at);
    Some(match run.outcome {
        RunOutcome::Skipped => format!("skipped {ago}"),
        RunOutcome::Failed => format!("failed {ago}"),
        RunOutcome::NoAgent => format!("#{} no agent {ago}", run.run),
        _ => format!("#{} {ago}", run.run),
    })
}

fn draw_list(f: &mut Frame, app: &App, area: Rect, hits: &mut Vec<(Rect, Hit)>) {
    let p = theme::palette();
    let view = &app.beats;
    let width = usize::from(area.width);
    let now = now_secs();
    let selected = view.selected_index();
    let focused = app.beats_has_keys();
    // Two lines a heartbeat and a third (the selected one's buttons, blank
    // otherwise): as many as fit, the selected one among them, over the row
    // that makes a new one.
    let per = 3usize;
    let list = Rect {
        height: area.height.saturating_sub(1),
        ..area
    };
    let fit = (usize::from(list.height) / per).max(1);
    let first = selected
        .min(view.heartbeats.len().saturating_sub(1))
        .saturating_sub(fit.saturating_sub(1));
    let listed = view.heartbeats.len().saturating_sub(first).min(fit);
    let new_row = Rect {
        y: list.y + ((listed * per) as u16).min(list.height),
        height: 1,
        ..area
    };
    let new_chosen = view.new_selected();
    let mut new_line = Paragraph::new(Span::styled(
        "+ New heartbeat",
        Style::default()
            .fg(if new_chosen {
                p.text_bright
            } else {
                p.accent_green
            })
            .add_modifier(Modifier::BOLD),
    ));
    if new_chosen {
        new_line = new_line.style(Style::default().bg(p.selection));
    }
    f.render_widget(new_line, new_row);
    hits.push((new_row, Hit::BeatNew));
    for (offset, (index, heartbeat)) in view
        .heartbeats
        .iter()
        .enumerate()
        .skip(first)
        .take(fit)
        .enumerate()
    {
        let y = list.y + (offset * per) as u16;
        let cell = Rect {
            y,
            height: 2.min(list.bottom().saturating_sub(y)),
            ..area
        };
        let chosen = index == selected;
        let (glyph, glyph_style) = mark(heartbeat);
        let right = when(heartbeat, now);
        let name_width = width.saturating_sub(str_width(&right) + 4);
        let name = truncate(&printable(&heartbeat.name), name_width);
        let gap = width.saturating_sub(2 + str_width(&name) + str_width(&right));
        let mut name_style = Style::default()
            .fg(p.text_primary)
            .add_modifier(Modifier::BOLD);
        if chosen && focused {
            name_style = name_style.fg(p.text_bright);
        }
        let top = Line::from(vec![
            Span::styled(format!("{glyph} "), glyph_style),
            Span::styled(name, name_style),
            Span::raw(" ".repeat(gap)),
            Span::styled(right, Style::default().fg(p.text_muted)),
        ]);
        let folder = app.folders.name_of(&heartbeat.scope);
        let mut facts = vec![
            heartbeat
                .schedule
                .as_ref()
                .map_or_else(|| "no schedule".into(), ToString::to_string),
            folder,
        ];
        facts.extend(last(heartbeat, now));
        let bottom = Line::from(Span::styled(
            truncate(&format!("  {}", printable(&facts.join(" · "))), width),
            Style::default().fg(p.text_muted),
        ));
        let mut paragraph = Paragraph::new(vec![top, bottom]);
        if chosen {
            paragraph = paragraph.style(Style::default().bg(p.selection));
        }
        f.render_widget(paragraph, cell);
        hits.push((cell, Hit::BeatRow(index)));
        if chosen && y + 2 < list.bottom() {
            let cell = Rect {
                x: area.x + 2,
                y: y + 2,
                width: area.width.saturating_sub(2),
                height: 1,
            };
            let mut actions = BeatAction::of(heartbeat);
            // Short of room, those Enter shows with the heartbeat go first.
            while actions.len() > 2 && actions_width(app, heartbeat, &actions) > cell.width {
                actions.remove(0);
            }
            draw_actions(f, app, heartbeat, &actions, cell, hits);
        }
    }
}

/// What a button says: its key and word, or what a second `d` does.
fn action_text(app: &App, heartbeat: &Heartbeat, action: BeatAction) -> (&'static str, String) {
    let (key, word) = action.label();
    let armed = action == BeatAction::Delete && app.beats.deleting.as_ref() == Some(&heartbeat.id);
    (key, if armed { "again deletes" } else { word }.to_string())
}

/// Columns of `actions` side by side.
fn actions_width(app: &App, heartbeat: &Heartbeat, actions: &[BeatAction]) -> u16 {
    let width: usize = actions
        .iter()
        .map(|action| {
            let (key, word) = action_text(app, heartbeat, *action);
            str_width(key) + 1 + str_width(&word) + 2
        })
        .sum();
    width.saturating_sub(2) as u16
}

/// `heartbeat`'s buttons from the top left of `area`, wrapped, each a click
/// target; the rows they took.
fn draw_actions(
    f: &mut Frame,
    app: &App,
    heartbeat: &Heartbeat,
    actions: &[BeatAction],
    area: Rect,
    hits: &mut Vec<(Rect, Hit)>,
) -> u16 {
    let p = theme::palette();
    let (mut x, mut y) = (area.x, area.y);
    for action in actions {
        let (key, word) = action_text(app, heartbeat, *action);
        let width = (str_width(key) + 1 + str_width(&word)) as u16;
        if x > area.x && x + width > area.right() {
            x = area.x;
            y += 1;
        }
        if y >= area.bottom() {
            break;
        }
        let cell = Rect {
            x,
            y,
            width: width.min(area.right().saturating_sub(x)),
            height: 1,
        };
        let armed = word == "again deletes";
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(
                    key,
                    Style::default()
                        .fg(p.accent_primary)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!(" {word}"),
                    Style::default().fg(if armed { p.accent_pink } else { p.text_muted }),
                ),
            ])),
            cell,
        );
        hits.push((cell, Hit::BeatAction(*action)));
        x += width + 2;
    }
    y - area.y + 1
}

fn draw_whole(
    f: &mut Frame,
    app: &App,
    heartbeat: &Heartbeat,
    area: Rect,
    hits: &mut Vec<(Rect, Hit)>,
) {
    let p = theme::palette();
    let now = now_secs();
    let width = usize::from(area.width);
    let quiet = Style::default().fg(p.text_muted);
    let plain = Style::default().fg(p.text_primary);
    let bold = Style::default()
        .fg(p.text_bright)
        .add_modifier(Modifier::BOLD);
    let (glyph, glyph_style) = mark(heartbeat);
    // Each line as its pieces: wrapped here, a row a line, so what's
    // counted is what shows.
    let field = |label: &str, value: String| {
        vec![
            (format!("{label:<9}"), quiet),
            (printable(&value).into_owned(), plain),
        ]
    };
    f.render_widget(
        Paragraph::new(vec![
            Line::from(vec![
                Span::styled(format!("{glyph} "), glyph_style),
                Span::styled(
                    truncate(&printable(&heartbeat.name), width.saturating_sub(2)),
                    bold,
                ),
            ]),
            Line::from(Span::styled(heartbeat.handle(), quiet)),
        ]),
        Rect {
            height: 2.min(area.height),
            ..area
        },
    );
    let buttons = Rect {
        y: area.y + 2,
        height: area.height.saturating_sub(2),
        ..area
    };
    let used = draw_actions(f, app, heartbeat, &BeatAction::of(heartbeat), buttons, hits);
    let area = Rect {
        y: (buttons.y + used + 1).min(area.bottom()),
        height: buttons.height.saturating_sub(used + 1),
        ..area
    };
    let mut pieces: Vec<Vec<(String, Style)>> = Vec::new();
    if heartbeat.awaiting_approval {
        pieces.push(vec![(
            "Its agent is setting it up: it turns on once its prompt and schedule are saved".into(),
            quiet,
        )]);
        pieces.push(Vec::new());
    }
    pieces.push(field(
        "schedule",
        heartbeat
            .schedule
            .as_ref()
            .map_or_else(|| "-".into(), ToString::to_string),
    ));
    pieces.push(field("next", when(heartbeat, now)));
    pieces.push(field(
        "last",
        last(heartbeat, now).unwrap_or_else(|| "-".into()),
    ));
    pieces.push(field("folder", app.folders.name_of(&heartbeat.scope)));
    pieces.push(field(
        "command",
        match heartbeat.command.is_empty() {
            true => "-".into(),
            false => heartbeat.command.clone(),
        },
    ));
    pieces.push(Vec::new());
    pieces.push(vec![("prompt".into(), quiet)]);
    let prompt = match heartbeat.prompt.trim() {
        "" => "-".to_string(),
        prompt => prompt
            .lines()
            .map(|line| printable(line).into_owned())
            .collect::<Vec<_>>()
            .join("\n"),
    };
    pieces.push(vec![(prompt, plain)]);
    let wrap = |cols: u16| -> Vec<Line<'static>> {
        pieces
            .iter()
            .flat_map(|line| wrap_spans(line, usize::from(cols)))
            .collect()
    };
    // It leaves the runs room for a few; taller than its room, it scrolls,
    // a scrollbar down its last column.
    let runs = app.beats.runs();
    let record_error = app.beats.record.as_ref().and_then(|r| r.error.clone());
    let wanted = match runs {
        Some(runs) => (runs.len().max(1) + 2).min(8) as u16,
        _ => 2,
    };
    let room = usize::from(area.height.saturating_sub(wanted));
    let mut lines = wrap(area.width);
    let scrolls = lines.len() > room && area.width > 1;
    if scrolls {
        lines = wrap(area.width - 1);
    }
    let total = lines.len();
    let shown = total.min(room);
    let max = total - shown;
    let top = app.beats.text_top.min(max);
    let head_rows = shown as u16;
    let head = Rect {
        height: head_rows,
        ..area
    };
    app.beats.text_max.set(max);
    app.beats.text_rows.set(shown);
    app.beats.text_area.set(head);
    f.render_widget(
        Paragraph::new(lines.into_iter().skip(top).take(shown).collect::<Vec<_>>()),
        Rect {
            width: area.width - u16::from(scrolls),
            ..head
        },
    );
    if max > 0 {
        draw_scrollbar(f, head, top, max, total);
    }
    let area = Rect {
        y: area.y + head_rows,
        height: area.height - head_rows,
        ..area
    };
    if area.height < 2 {
        return;
    }
    let label = match (runs, &record_error) {
        (_, Some(error)) => error.clone(),
        (None, None) => format!("{} runs", get_spinner_char()),
        (Some([]), None) => "no runs".into(),
        (Some(_), None) => "runs".into(),
    };
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            truncate(&label, width),
            match record_error {
                Some(_) => Style::default().fg(p.accent_pink),
                None => quiet,
            },
        ))),
        Rect {
            y: area.y + 1,
            height: 1,
            ..area
        },
    );
    let Some(runs) = runs else {
        return;
    };
    let list = Rect {
        y: area.y + 2,
        height: area.height - 2,
        ..area
    };
    let selected = app.beats.run_index();
    let fit = usize::from(list.height).max(1);
    let first = selected.saturating_sub(fit - 1);
    for (offset, (index, run)) in runs.iter().enumerate().skip(first).take(fit).enumerate() {
        let view = run
            .run
            .session_id
            .as_ref()
            .and_then(|id| app.list.get_anywhere(id));
        let (glyph, style) = match (view, run.run.outcome) {
            (Some(view), _) => status_glyph(view),
            (None, RunOutcome::Skipped) => ("–", quiet),
            (None, RunOutcome::Failed | RunOutcome::NoAgent) => {
                ("✕", Style::default().fg(p.accent_pink))
            }
            (None, _) => ("○", quiet),
        };
        let chosen = index == selected;
        let number = match run.run.run {
            0 => String::new(),
            n => format!("#{n} "),
        };
        let when = card_stamp(run.run.at, now, width < 36);
        let state = run_state(run, view);
        let left = truncate(
            &printable(&format!("{number}{state}")),
            width.saturating_sub(str_width(&when) + 4),
        );
        let gap = width.saturating_sub(2 + str_width(&left) + str_width(&when));
        let line = Line::from(vec![
            Span::styled(format!("{glyph} "), style),
            Span::styled(left, if chosen { bold } else { plain }),
            Span::raw(" ".repeat(gap)),
            Span::styled(when, quiet),
        ]);
        let cell = Rect {
            y: list.y + offset as u16,
            height: 1,
            ..list
        };
        let mut paragraph = Paragraph::new(line);
        if chosen {
            paragraph = paragraph.style(Style::default().bg(p.selection));
        }
        f.render_widget(paragraph, cell);
        hits.push((cell, Hit::BeatRow(index)));
    }
}

/// `pieces` (text and style) word-wrapped to `cols` as the New heartbeat
/// form wraps its prompt (spaces kept, `\n` a new row), a line a row, each
/// piece keeping its style. Drawn without wrapping, every row counts.
///
/// ```text
/// [("command  ", quiet), ("claude --x", plain)] in 12 columns:
/// "command  " "claude "   (one row)
/// "--x"                   (the next)
/// ```
fn wrap_spans(pieces: &[(String, Style)], cols: usize) -> Vec<Line<'static>> {
    let text: String = pieces.iter().map(|(text, _)| text.as_str()).collect();
    let mut bounds = Vec::with_capacity(pieces.len());
    let mut at = 0;
    for (piece, style) in pieces {
        bounds.push((at, at + piece.len(), *style));
        at += piece.len();
    }
    TextInput::new(text.clone())
        .rows(cols)
        .into_iter()
        .map(|row| {
            let spans: Vec<Span<'static>> = bounds
                .iter()
                .filter_map(|&(start, end, style)| {
                    let (start, end) = (start.max(row.start), end.min(row.end));
                    (start < end).then(|| Span::styled(text[start..end].to_string(), style))
                })
                .collect();
            Line::from(spans)
        })
        .collect()
}
