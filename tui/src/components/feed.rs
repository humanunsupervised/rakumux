//! The agent feed (`app/feed`) drawn into any area: the replies newest
//! first, each a cell like a session's (a blank row, then its session,
//! folder, agent and machine with its age at the right, then the start of
//! the reply), or what the feed is waiting for (a sign-in, the switch, the
//! first read). The selected reply is drawn by [`draw_feed_reply`], in the
//! detail beside the list or in the list's place on a phone: its whole turn
//! in order once it's read (the prompt, the agent's reasoning, its tool
//! calls with the start of their output, its text), else the reply alone.
//! Text is stripped of anything a terminal would act on before it's drawn.

use rakumux_sensagent::feed::{FeedEntry, FeedStatus};
use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Paragraph},
    Frame,
};
use session_core::api::{TranscriptMessage, TranscriptPart, TranscriptRole};

use super::format::{now_secs, relative_time, truncate};
use super::session_list::first_visible_row;
use super::utils::get_spinner_char;
use crate::app::feed::{agent_name, plain, TurnState};
use crate::app::types::AccountStatus;
use crate::app::{App, Hit};
use crate::layout::str_width;
use crate::theme;

/// Columns left of the content, as in the session list.
const INDENT: usize = 3;
/// Rows per reply: a blank row, its header and two lines of its text.
const CELL_ROWS: u16 = 4;
/// Lines of a reply shown in the list.
const PREVIEW_LINES: usize = 2;
/// Lines of a tool's output shown in a turn.
const RESULT_LINES: usize = 6;

/// Where the feed is up to, as shown: the service's word, else signed out
/// (known before it answers) or loading.
fn status(app: &App) -> FeedStatus {
    match app.feed.status() {
        Some(status) => status,
        None if matches!(app.account, AccountStatus::SignedOut) => FeedStatus::SignedOut,
        None => FeedStatus::Loading,
    }
}

/// The feed's replies (or its state) in `area`, recording a target per
/// reply. A reply being read on a phone fills the area instead.
pub fn draw_feed_list(f: &mut Frame, app: &App, area: Rect, hits: &mut Vec<(Rect, Hit)>) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    if app.feed.reading {
        draw_feed_reply(f, app, area);
        return;
    }
    let p = theme::palette();
    let muted = Style::default().fg(p.text_muted);
    let strong = Style::default()
        .fg(p.text_primary)
        .add_modifier(Modifier::BOLD);
    let key = Style::default()
        .fg(p.accent_primary)
        .add_modifier(Modifier::BOLD);
    let message = |lines: Vec<Line<'static>>| {
        let lines: Vec<Line> = std::iter::once(Line::default()).chain(lines).collect();
        Paragraph::new(lines)
    };
    let mut area = area;
    match status(app) {
        FeedStatus::SignedOut => {
            let lines = vec![
                indented(vec![Span::styled("Sign in to see your feed:", strong)]),
                indented(vec![
                    Span::styled(",", key),
                    Span::styled(" Settings → Account", muted),
                ]),
            ];
            f.render_widget(message(lines), area);
            return;
        }
        FeedStatus::Off => {
            let lines = vec![
                indented(vec![Span::styled("The feed is off.", strong)]),
                indented(vec![
                    Span::styled("Turn it on in ", muted),
                    Span::styled("Settings → Available on sign-in", key),
                ]),
            ];
            f.render_widget(message(lines), area);
            return;
        }
        FeedStatus::Loading if app.feed.entries().is_empty() => {
            let line = indented(vec![Span::styled(
                format!("{} Loading the feed…", get_spinner_char()),
                muted,
            )]);
            f.render_widget(message(vec![line]), area);
            return;
        }
        FeedStatus::Unavailable | FeedStatus::Unknown => {
            // Kept entries stay; a quiet line says they may be old.
            let line = indented(vec![Span::styled("can't reach rakumux.com", muted)]);
            f.render_widget(Paragraph::new(vec![Line::default(), line]), area);
            area.y += 2;
            area.height = area.height.saturating_sub(2);
        }
        FeedStatus::Loading | FeedStatus::Ready => {}
    }
    let entries = app.feed.entries();
    if entries.is_empty() {
        if status(app) == FeedStatus::Ready {
            let lines = vec![
                indented(vec![Span::styled("No replies yet", strong)]),
                indented(vec![Span::styled(
                    "They appear here when an agent finishes a turn.",
                    muted,
                )]),
            ];
            f.render_widget(message(lines), area);
        }
        return;
    }
    let selected = app.feed.selected_index().unwrap_or(0);
    let heights = vec![CELL_ROWS; entries.len()];
    let first = first_visible_row(&heights, selected, area.height);
    let width = (area.width as usize).saturating_sub(INDENT + 1);
    let now = now_secs();
    let mut y = area.y;
    for (index, entry) in entries.iter().enumerate().skip(first) {
        if y + CELL_ROWS > area.bottom() {
            break;
        }
        let cell = Rect {
            y,
            height: CELL_ROWS,
            ..area
        };
        let chosen = index == selected;
        let mut lines = vec![Line::default(), header(app, entry, width, chosen, now)];
        let text_style = Style::default().fg(if chosen { p.text_primary } else { p.text_muted });
        let mut preview = wrap(&plain(&entry.text), width);
        let more = preview.len() > PREVIEW_LINES;
        preview.truncate(PREVIEW_LINES);
        if more {
            if let Some(last) = preview.last_mut() {
                *last = truncate(&format!("{last} …"), width);
            }
        }
        lines.extend(
            preview
                .into_iter()
                .map(|line| indented(vec![Span::styled(line, text_style)])),
        );
        f.render_widget(Paragraph::new(lines), cell);
        hits.push((cell, Hit::FeedEntry(index)));
        y += CELL_ROWS;
    }
}

/// A reply's first line: its session and where it's from, its age (and
/// `sending` while this machine's upload is pending) at the right.
fn header(app: &App, entry: &FeedEntry, width: usize, chosen: bool, now: i64) -> Line<'static> {
    let p = theme::palette();
    let name_style = if chosen {
        Style::default()
            .fg(p.text_bright)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default()
            .fg(p.text_primary)
            .add_modifier(Modifier::BOLD)
    };
    let muted = Style::default().fg(p.text_muted);
    let age = relative_time(now, entry.finished_at / 1000);
    let right = if entry.seq.is_none() {
        format!("sending · {age}")
    } else {
        age
    };
    let room = width.saturating_sub(str_width(&right) + 1);
    let name = truncate(&plain(&entry.session_name), room);
    let place = truncate(
        &format!(" · {}", origin(app, entry)),
        room.saturating_sub(str_width(&name)),
    );
    let gap = width.saturating_sub(str_width(&name) + str_width(&place) + str_width(&right));
    indented(vec![
        Span::styled(name, name_style),
        Span::styled(place, muted),
        Span::raw(" ".repeat(gap)),
        Span::styled(right, muted),
    ])
}

/// Where a reply is from: its folder, its agent, and its machine when it
/// isn't this one.
fn origin(app: &App, entry: &FeedEntry) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(folder) = &entry.folder {
        parts.push(plain(folder));
    }
    parts.push(plain(agent_name(&entry.agent)));
    if entry.machine != app.feed.machine() {
        parts.push(format!("on {}", plain(&entry.machine)));
    }
    parts.join(" · ")
}

/// The selected reply whole in `area`: its session and where it's from,
/// when it finished, then its text, scrolled by the feed's scroll (stopped
/// at its end).
pub fn draw_feed_reply(f: &mut Frame, app: &App, area: Rect) {
    let p = theme::palette();
    f.render_widget(
        Block::default().style(Style::default().bg(p.background)),
        area,
    );
    if area.width == 0 || area.height == 0 {
        return;
    }
    let muted = Style::default().fg(p.text_muted);
    let key = Style::default()
        .fg(p.accent_primary)
        .add_modifier(Modifier::BOLD);
    let Some(entry) = app.feed.selected() else {
        return;
    };
    let inner = Rect {
        x: area.x + 2,
        width: area.width.saturating_sub(4),
        y: area.y + 1,
        height: area.height.saturating_sub(1),
    };
    let width = inner.width as usize;
    let age = relative_time(now_secs(), entry.finished_at / 1000);
    let mut about = vec![if age == "now" {
        "finished just now".to_owned()
    } else {
        format!("finished {age} ago")
    }];
    if entry.truncated {
        about.push("cut short".into());
    }
    if entry.seq.is_none() {
        about.push("sending".into());
    }
    let here = app.feed_session().is_some();
    let action = if here {
        vec![
            Span::styled("Enter", key),
            Span::styled(" goes into its session", muted),
        ]
    } else if app.feed.reading {
        vec![Span::styled("Esc", key), Span::styled(" back", muted)]
    } else {
        Vec::new()
    };
    let mut lines = vec![
        Line::from(vec![
            Span::styled(
                truncate(&plain(&entry.session_name), width),
                Style::default()
                    .fg(p.text_bright)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                truncate(
                    &format!(" · {}", origin(app, entry)),
                    width.saturating_sub(str_width(&entry.session_name)),
                ),
                muted,
            ),
        ]),
        Line::from(Span::styled(truncate(&about.join(" · "), width), muted)),
    ];
    if !action.is_empty() {
        lines.push(Line::from(action));
    }
    lines.push(Line::default());
    let head = lines.len();
    let reply = || {
        wrap(&plain(&entry.text), width)
            .into_iter()
            .map(|line| Line::from(Span::styled(line, Style::default().fg(p.text_primary))))
            .collect::<Vec<_>>()
    };
    let body = match app.feed.turn(&entry.id) {
        Some(TurnState::Ready { messages, next }) if !messages.is_empty() => {
            let mut body = turn_lines(messages, width);
            if next.is_some() {
                body.push(Line::from(Span::styled(
                    format!("{} more of the turn…", get_spinner_char()),
                    muted,
                )));
            }
            body
        }
        Some(TurnState::Loading) => {
            let mut body = vec![
                Line::from(Span::styled(
                    format!("{} Loading the whole turn…", get_spinner_char()),
                    muted,
                )),
                Line::default(),
            ];
            body.extend(reply());
            body
        }
        _ => reply(),
    };
    let room = (inner.height as usize).saturating_sub(head);
    let scroll = app.feed.scroll.min(body.len().saturating_sub(room));
    lines.extend(body.into_iter().skip(scroll).take(room));
    f.render_widget(Paragraph::new(lines), inner);
}

/// What a line of a turn shows, to put a blank line between kinds.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Prompt,
    Text,
    Reasoning,
    Tool,
}

/// A turn's messages as lines of `width` columns: the user's prompts marked
/// `›`, reasoning quiet and italic, each tool call `●` with what it works on
/// and the start of its output under it, the agent's text as written.
pub fn turn_lines(messages: &[TranscriptMessage], width: usize) -> Vec<Line<'static>> {
    let p = theme::palette();
    let text_style = Style::default().fg(p.text_primary);
    let muted = Style::default().fg(p.text_muted);
    let quiet = muted.add_modifier(Modifier::ITALIC);
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut last: Option<Kind> = None;
    let mut gap = |lines: &mut Vec<Line<'static>>, kind: Kind| {
        if last.is_some_and(|last| last != kind || kind == Kind::Prompt) {
            lines.push(Line::default());
        }
        last = Some(kind);
    };
    for message in messages {
        for part in &message.parts {
            match part {
                TranscriptPart::Text { text, truncated }
                    if message.role == TranscriptRole::User =>
                {
                    gap(&mut lines, Kind::Prompt);
                    let prompt = wrap(&plain(text), width.saturating_sub(2));
                    for (index, line) in prompt.into_iter().enumerate() {
                        let mark = if index == 0 { "› " } else { "  " };
                        lines.push(Line::from(vec![
                            Span::styled(mark, Style::default().fg(p.accent_primary)),
                            Span::styled(
                                line,
                                Style::default()
                                    .fg(p.text_bright)
                                    .add_modifier(Modifier::BOLD),
                            ),
                        ]));
                    }
                    if *truncated {
                        lines.push(Line::from(Span::styled("  …", muted)));
                    }
                }
                TranscriptPart::Text { text, truncated } => {
                    gap(&mut lines, Kind::Text);
                    lines.extend(
                        wrap(&plain(text), width)
                            .into_iter()
                            .map(|line| Line::from(Span::styled(line, text_style))),
                    );
                    if *truncated {
                        lines.push(Line::from(Span::styled("…", muted)));
                    }
                }
                TranscriptPart::Reasoning { text, .. } => {
                    gap(&mut lines, Kind::Reasoning);
                    lines.extend(
                        wrap(&plain(text), width)
                            .into_iter()
                            .map(|line| Line::from(Span::styled(line, quiet))),
                    );
                }
                TranscriptPart::ToolCall { name, input, .. } => {
                    gap(&mut lines, Kind::Tool);
                    let name = truncate(&plain(name), width.saturating_sub(2));
                    let room = width.saturating_sub(2 + str_width(&name) + 1);
                    let subject = truncate(&tool_subject(input), room);
                    lines.push(Line::from(vec![
                        Span::styled("● ", Style::default().fg(p.accent_green)),
                        Span::styled(name, text_style.add_modifier(Modifier::BOLD)),
                        Span::styled(format!(" {subject}"), muted),
                    ]));
                }
                TranscriptPart::ToolResult {
                    output,
                    error,
                    truncated,
                    ..
                } => {
                    gap(&mut lines, Kind::Tool);
                    let style = if *error {
                        Style::default().fg(p.accent_pink)
                    } else {
                        muted
                    };
                    let output = plain(output);
                    let shown: Vec<&str> = output
                        .lines()
                        .filter(|line| !line.trim().is_empty())
                        .collect();
                    if shown.is_empty() {
                        let none = if *error {
                            "  ⎿ failed"
                        } else {
                            "  ⎿ (no output)"
                        };
                        lines.push(Line::from(Span::styled(none, style)));
                        continue;
                    }
                    for (index, line) in shown.iter().take(RESULT_LINES).enumerate() {
                        let mark = if index == 0 { "  ⎿ " } else { "    " };
                        lines.push(Line::from(Span::styled(
                            truncate(&format!("{mark}{}", line.trim_end()), width),
                            style,
                        )));
                    }
                    let hidden = shown.len().saturating_sub(RESULT_LINES);
                    if hidden > 0 || *truncated {
                        let more = match hidden {
                            0 => "    …".to_owned(),
                            1 => "    … 1 more line".to_owned(),
                            n => format!("    … {n} more lines"),
                        };
                        lines.push(Line::from(Span::styled(more, muted)));
                    }
                }
                TranscriptPart::Unknown => {}
            }
        }
    }
    lines
}

/// What a tool call works on, in a few words: the field of its input that
/// says (a description, a command, a path, a pattern…), else the input's
/// first line.
fn tool_subject(input: &str) -> String {
    const FIELDS: [&str; 10] = [
        "description",
        "command",
        "cmd",
        "file_path",
        "filePath",
        "path",
        "pattern",
        "url",
        "query",
        "prompt",
    ];
    let value: Option<serde_json::Value> = serde_json::from_str(input).ok();
    let field = value.as_ref().and_then(|value| {
        FIELDS.iter().find_map(|key| match value.get(key)? {
            serde_json::Value::String(text) => Some(text.clone()),
            serde_json::Value::Array(parts) => Some(
                parts
                    .iter()
                    .filter_map(serde_json::Value::as_str)
                    .collect::<Vec<_>>()
                    .join(" "),
            ),
            _ => None,
        })
    });
    let text = plain(&field.unwrap_or_else(|| input.to_owned()));
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or_default()
        .to_owned()
}

fn indented(spans: Vec<Span<'static>>) -> Line<'static> {
    let mut line = vec![Span::raw(" ".repeat(INDENT))];
    line.extend(spans);
    Line::from(line)
}

/// `text` word-wrapped to `width` columns: its own line breaks kept, words
/// longer than a line broken.
pub fn wrap(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut lines = Vec::new();
    for paragraph in text.lines() {
        let mut line = String::new();
        let mut used = 0;
        for word in paragraph.split(' ').filter(|word| !word.is_empty()) {
            let w = str_width(word);
            if used > 0 && used + 1 + w > width {
                lines.push(std::mem::take(&mut line));
                used = 0;
            }
            if used > 0 {
                line.push(' ');
                used += 1;
            }
            for c in word.chars() {
                let cw = str_width(c.encode_utf8(&mut [0; 4]));
                if used + cw > width && used > 0 {
                    lines.push(std::mem::take(&mut line));
                    used = 0;
                }
                line.push(c);
                used += cw;
            }
        }
        lines.push(line);
    }
    while lines.last().is_some_and(String::is_empty) {
        lines.pop();
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::wrap;

    #[test]
    fn replies_wrap_at_words_and_keep_their_line_breaks() {
        assert_eq!(
            wrap("The tests pass now.\n\n- fixed the parser", 12),
            ["The tests", "pass now.", "", "- fixed the", "parser"]
        );
        assert_eq!(wrap("abcdefghij", 4), ["abcd", "efgh", "ij"]);
        assert_eq!(wrap("", 10), Vec::<String>::new());
    }
}
