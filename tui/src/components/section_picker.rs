//! The App and Status modals (their tab on the list's top row, or Enter on
//! one of their headers): a query line, All apps (All statuses), then each
//! app (status) with a tick and how many sessions it has.

use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
    Frame,
};
use session_core::model::SessionStatus;

use super::folder_picker::count_label;
use super::format::{program_color, status_group_glyph, truncate};
use crate::app::grouping::{Grouping, Section};
use crate::app::types::{SectionPicker, SectionRow};
use crate::app::{App, Hit};
use crate::layout::str_width;
use crate::theme;

/// Widest the modal gets.
const MODAL_WIDTH: u16 = 56;
/// Most rows shown at once.
const MAX_ROWS: u16 = 12;

/// `3 sessions`, `1 session`, or `no sessions`.
pub fn sessions_label(total: usize) -> String {
    match total {
        0 => "no sessions".into(),
        1 => "1 session".into(),
        n => format!("{n} sessions"),
    }
}

/// How many sessions an app or a status has, as its header and the modal
/// say it: `2/3 live` for an app, `3 sessions` for a status.
pub fn section_count(app: &App, section: &Section) -> String {
    let (live, total) = app.list.section_counts(section);
    match section {
        Section::App(_) => count_label(live, total),
        Section::Status(_) => sessions_label(total),
    }
}

/// What Enter does on the row under the cursor, for the keys line.
fn enter_action(picker: &SectionPicker) -> String {
    match picker.current_row() {
        Some(SectionRow::All) => "show all".into(),
        Some(SectionRow::Section(section)) => match picker.sections_for(section).len() {
            1 => "show".into(),
            n => format!("show {n} {}", picker.grouping.plural()),
        },
        None => "show".into(),
    }
}

/// Draw the App or Status modal in the middle of `area` and record its
/// click targets: a query line, the rows, a message line and the keys.
pub fn draw_section_picker(
    f: &mut Frame,
    app: &App,
    picker: &SectionPicker,
    area: Rect,
    hits: &mut Vec<(Rect, Hit)>,
) {
    let p = theme::palette();
    let width = MODAL_WIDTH.min(area.width.saturating_sub(4));
    // Borders, padding, query, rule, rows (at least one), blank, message,
    // keys.
    let list_rows = (picker.rows.len() as u16).clamp(1, MAX_ROWS);
    let height = (list_rows + 9).min(area.height);
    if width < 24 || height < 10 {
        return;
    }
    let popup = Rect {
        x: area.x + (area.width - width) / 2,
        y: area.y + (area.height - height) / 3,
        width,
        height,
    };
    f.render_widget(Clear, popup);
    let (title, noun, one) = match picker.grouping {
        Grouping::Status => (" Statuses ", "statuses", "status"),
        Grouping::Folder | Grouping::App => (" Apps ", "apps", "app"),
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(p.accent_primary))
        .title(Span::styled(
            title,
            Style::default()
                .fg(p.accent_primary)
                .add_modifier(Modifier::BOLD),
        ))
        .style(p.base());
    let inner = block.inner(popup);
    f.render_widget(block, popup);
    let inner = Rect {
        x: inner.x + 1,
        y: inner.y + 1,
        width: inner.width.saturating_sub(2),
        height: inner.height.saturating_sub(1),
    };
    let row_at = |offset: u16| Rect {
        y: inner.y + offset,
        height: 1,
        ..inner
    };
    let inner_width = inner.width as usize;
    let key = Style::default()
        .fg(p.accent_primary)
        .add_modifier(Modifier::BOLD);
    let muted = Style::default().fg(p.text_muted);

    // The query.
    let prompt = "❯ ";
    let room = inner_width.saturating_sub(prompt.chars().count() + 1);
    let chars: Vec<char> = picker.query.value().chars().collect();
    let cursor = picker.query.cursor_column();
    let start = (cursor + 1).saturating_sub(room);
    let query = if chars.is_empty() {
        Span::styled(truncate(&format!("Filter {noun}"), room), muted)
    } else {
        Span::styled(
            chars[start.min(chars.len())..]
                .iter()
                .take(room)
                .collect::<String>(),
            Style::default().fg(p.text_bright),
        )
    };
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(prompt, Style::default().fg(p.accent_primary)),
            query,
        ])),
        row_at(0),
    );
    f.set_cursor(
        inner.x + (prompt.chars().count() + cursor - start) as u16,
        inner.y,
    );
    f.render_widget(
        Paragraph::new(Span::styled(
            "─".repeat(inner_width),
            Style::default().fg(p.border_dim),
        )),
        row_at(1),
    );

    // The rows, scrolled so the cursor shows: cursor bar and tick, the
    // name (a status after its mark), then its count.
    let list_height = list_rows as usize;
    let first = (picker.cursor + 1).saturating_sub(list_height);
    let lead = 6;
    let name_width = |row: &SectionRow| match row {
        SectionRow::All => str_width("All ") + str_width(noun),
        SectionRow::Section(Section::App(app)) => str_width(app.as_str()),
        SectionRow::Section(Section::Status(status)) => 2 + str_width(status.label()),
    };
    let count_of = |row: &SectionRow| match row {
        SectionRow::All => {
            let sessions = app.list.sessions();
            let live = sessions
                .iter()
                .filter(|s| s.status == SessionStatus::Live)
                .count();
            match picker.grouping {
                Grouping::Status => sessions_label(sessions.len()),
                Grouping::Folder | Grouping::App => count_label(live, sessions.len()),
            }
        }
        SectionRow::Section(section) => section_count(app, section),
    };
    let name_w = picker.rows.iter().map(name_width).max().unwrap_or(0);
    let name_w = name_w.min(inner_width.saturating_sub(lead + 2 + 10));
    if picker.rows.is_empty() {
        f.render_widget(
            Paragraph::new(Span::styled(
                truncate(&format!("  No {one} matches"), inner_width),
                muted,
            )),
            row_at(2),
        );
    }
    for (offset, (index, row)) in picker
        .rows
        .iter()
        .enumerate()
        .skip(first)
        .take(list_height)
        .enumerate()
    {
        let at_cursor = index == picker.cursor;
        let rect = row_at(2 + offset as u16);
        let text_color = if at_cursor {
            p.text_bright
        } else {
            p.neutral_gray
        };
        let mut name_style = Style::default().fg(text_color);
        if at_cursor {
            name_style = name_style.add_modifier(Modifier::BOLD);
        }
        let tick = if picker.is_ticked(row) {
            Span::styled("[x] ", Style::default().fg(p.accent_green))
        } else {
            Span::styled("[ ] ", muted)
        };
        let mut spans = vec![
            Span::styled(
                if at_cursor { "▎ " } else { "  " },
                Style::default().fg(p.accent_primary),
            ),
            tick,
        ];
        let name = match row {
            SectionRow::All => vec![Span::styled(
                truncate(&format!("All {noun}"), name_w),
                name_style,
            )],
            // An app's name in its colour, white under the cursor.
            SectionRow::Section(Section::App(app)) if !at_cursor => vec![Span::styled(
                truncate(app.as_str(), name_w),
                name_style.fg(program_color(app.as_str())),
            )],
            SectionRow::Section(Section::App(app)) => {
                vec![Span::styled(truncate(app.as_str(), name_w), name_style)]
            }
            SectionRow::Section(Section::Status(status)) => {
                let (glyph, glyph_style) = status_group_glyph(*status);
                vec![
                    Span::styled(glyph, glyph_style),
                    Span::raw(" "),
                    Span::styled(
                        truncate(status.label(), name_w.saturating_sub(2)),
                        name_style,
                    ),
                ]
            }
        };
        let used: usize = name.iter().map(|s| s.width()).sum();
        spans.extend(name);
        spans.push(Span::raw(" ".repeat(name_w.saturating_sub(used))));
        let room = inner_width.saturating_sub(lead + name_w.max(used) + 2);
        if room > 0 {
            spans.push(Span::styled(
                truncate(&format!("  {}", count_of(row)), room + 2),
                muted,
            ));
        }
        f.render_widget(Paragraph::new(Line::from(spans)), rect);
        hits.push((rect, Hit::SectionRow(index)));
    }

    // The keys: Enter, Tab (and Space while nothing is typed), Esc.
    let keys_row = row_at(inner.height.saturating_sub(1));
    let keys = |pairs: &[(&str, &str)]| {
        let mut spans = Vec::new();
        for (i, (k, action)) in pairs.iter().enumerate() {
            if i > 0 {
                spans.push(Span::raw("  "));
            }
            spans.push(Span::styled(k.to_string(), key));
            spans.push(Span::styled(format!(" {action}"), muted));
        }
        Line::from(spans)
    };
    let enter = enter_action(picker);
    let tab_key = if picker.query.is_empty() {
        "Tab/Space"
    } else {
        "Tab"
    };
    let mut pairs: Vec<(&str, &str)> =
        vec![("⏎", enter.as_str()), (tab_key, "tick"), ("Esc", "close")];
    // On a narrow screen (a phone) Enter and Esc stay.
    let fits = |pairs: &[(&str, &str)]| keys(pairs).width() <= usize::from(keys_row.width);
    if !fits(&pairs) {
        pairs[1].0 = "Tab";
    }
    if !fits(&pairs) {
        pairs.remove(1);
    }
    f.render_widget(Paragraph::new(keys(&pairs)), keys_row);
    hits.push((popup, Hit::SectionMenu));
}
