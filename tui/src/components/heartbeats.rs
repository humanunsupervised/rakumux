//! The drawer's Beats tab (`app/heartbeats.rs`): the heartbeats, two lines
//! each (a mark, the name and when it runs next; its schedule, folder and
//! last run), those waiting for approval first; or one heartbeat whole: how
//! it runs, its command and prompt in full (read before `e` approves it),
//! and its runs, newest first, to go into.

use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Paragraph, Wrap},
    Frame,
};
use session_core::heartbeat::{Heartbeat, RunOutcome};

use crate::app::{App, Hit};
use crate::components::dialogs::{error_line, text_rows};
use crate::components::format::{now_secs, relative_time, status_glyph, truncate};
use crate::components::utils::get_spinner_char;
use crate::layout::str_width;
use crate::printable::printable;
use crate::theme;

/// The tab under its band, in `area`.
pub fn draw_beats(f: &mut Frame, app: &App, area: Rect, hits: &mut Vec<(Rect, Hit)>) {
    let p = theme::palette();
    let view = &app.beats;
    // A row under the band, a column each side, as rtop.
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

/// `?` waiting for approval, `●` on, `○` paused or not filled in yet.
fn mark(heartbeat: &Heartbeat) -> (&'static str, Style) {
    let p = theme::palette();
    match (heartbeat.enabled, heartbeat.awaiting_approval) {
        (_, true) => ("?", Style::default().fg(p.accent_orange)),
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
        (_, true, _) => "approve?".into(),
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
    if view.heartbeats.is_empty() {
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled("No heartbeats · ", Style::default().fg(p.text_muted)),
                Span::styled(
                    "n",
                    Style::default()
                        .fg(p.text_bright)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(" new", Style::default().fg(p.text_muted)),
            ])),
            area,
        );
        return;
    }
    let now = now_secs();
    let selected = view.selected_index();
    let focused = app.beats_has_keys();
    // Two lines a heartbeat and a blank one: as many as fit, the selected
    // one among them.
    let per = 3usize;
    let fit = (usize::from(area.height) / per).max(1);
    let first = selected.saturating_sub(fit.saturating_sub(1));
    for (offset, (index, heartbeat)) in view
        .heartbeats
        .iter()
        .enumerate()
        .skip(first)
        .take(fit)
        .enumerate()
    {
        let y = area.y + (offset * per) as u16;
        let cell = Rect {
            y,
            height: 2.min(area.bottom().saturating_sub(y)),
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
    }
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
    let field = |label: &str, value: String| {
        Line::from(vec![
            Span::styled(format!("{label:<9}"), quiet),
            Span::styled(printable(&value).into_owned(), plain),
        ])
    };
    let mut lines = vec![
        Line::from(vec![
            Span::styled(format!("{glyph} "), glyph_style),
            Span::styled(
                truncate(&printable(&heartbeat.name), width.saturating_sub(2)),
                bold,
            ),
        ]),
        Line::from(Span::styled(heartbeat.handle(), quiet)),
        Line::default(),
    ];
    if heartbeat.awaiting_approval {
        lines.push(Line::from(vec![
            Span::styled(
                "An agent made or changed it · ",
                Style::default().fg(p.accent_orange),
            ),
            Span::styled("e", bold),
            Span::styled(" approves", Style::default().fg(p.accent_orange)),
        ]));
        lines.push(Line::default());
    }
    lines.push(field(
        "schedule",
        heartbeat
            .schedule
            .as_ref()
            .map_or_else(|| "-".into(), ToString::to_string),
    ));
    lines.push(field("next", when(heartbeat, now)));
    lines.push(field(
        "last",
        last(heartbeat, now).unwrap_or_else(|| "-".into()),
    ));
    lines.push(field("folder", app.folders.name_of(&heartbeat.scope)));
    lines.push(field(
        "command",
        match heartbeat.command.is_empty() {
            true => "-".into(),
            false => heartbeat.command.clone(),
        },
    ));
    lines.push(Line::default());
    lines.push(Line::from(Span::styled("prompt", quiet)));
    match heartbeat.prompt.trim() {
        "" => lines.push(Line::from(Span::styled("-", plain))),
        prompt => lines.extend(
            prompt
                .lines()
                .map(|line| Line::from(Span::styled(printable(line).into_owned(), plain))),
        ),
    }
    lines.push(Line::default());
    let runs = app.heartbeat_runs(&heartbeat.id);
    lines.push(Line::from(Span::styled(
        match runs.is_empty() {
            true => "no runs".to_string(),
            false => "runs".to_string(),
        },
        quiet,
    )));
    // The text above the runs wraps: measure it as drawn.
    let head_rows = text_rows(&lines, area.width).min(area.height);
    let head = Paragraph::new(lines).wrap(Wrap { trim: false });
    f.render_widget(
        head,
        Rect {
            height: head_rows,
            ..area
        },
    );
    let mut y = area.y + head_rows;
    for (index, run) in runs.iter().enumerate() {
        if y >= area.bottom() {
            break;
        }
        let (glyph, style) = status_glyph(run);
        let chosen = index == app.beats.run;
        let age = relative_time(now, run.last_active_at);
        let name = truncate(
            &printable(&run.name),
            width.saturating_sub(str_width(&age) + 4),
        );
        let gap = width.saturating_sub(2 + str_width(&name) + str_width(&age));
        let line = Line::from(vec![
            Span::styled(format!("{glyph} "), style),
            Span::styled(name, if chosen { bold } else { plain }),
            Span::raw(" ".repeat(gap)),
            Span::styled(age, quiet),
        ]);
        let cell = Rect {
            y,
            height: 1,
            ..area
        };
        let mut paragraph = Paragraph::new(line);
        if chosen {
            paragraph = paragraph.style(Style::default().bg(p.selection));
        }
        f.render_widget(paragraph, cell);
        hits.push((cell, Hit::BeatRow(index)));
        y += 1;
    }
}
