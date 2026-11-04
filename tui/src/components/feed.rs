//! The agent feed (`app/feed`) in the drawer ([`draw_feed`]): the drawer's
//! band (`drawer.rs`), in the accent while it has the keys, over the replies
//! newest first, each a card a row apart (its session, then its agent,
//! folder, repository and branch, on the left of its top border; its age and
//! where it's up to on the right; the start of the reply inside, or all of
//! it once expanded: Space, or the `▾ more` on its bottom border), or what
//! the feed is waiting for (a
//! sign-in, the switch, the first read). The reply read whole (Enter) takes
//! the cards' place, a `‹` on the band to go back; a wide Feed tab shows it
//! beside them. It's its whole turn in order once it's read (the prompt,
//! the agent's reasoning, its tool calls with the start of their output, its
//! text), else the reply alone, then its thread when someone wrote back
//! (`r`): the entry that started it, each reply with where it's up to, and
//! the entries that answer them. Text is stripped of anything a terminal
//! would act on before it's drawn.

use rakumux_sensagent::feed::{FeedEntry, FeedReply, FeedStatus, ReplyFailure, ReplyStatus};
use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Paragraph, Wrap},
    Frame,
};
use session_core::api::{TranscriptMessage, TranscriptPart, TranscriptRole};

use super::drawer::draw_band;
use super::format::{now_secs, relative_time, truncate};
use super::markdown;
use super::session_list::{first_visible_row, folder_item_height};
use super::utils::get_spinner_char;
use crate::app::feed::{agent_name, plain, Card, EntryStatus, ThreadItem, TurnState};
use crate::app::types::AccountStatus;
use crate::app::{App, Hit};
use crate::layout::str_width;
use crate::theme;

/// Columns left of the content, as in the session list.
const INDENT: usize = 3;
/// Rows of a card at its usual height: its top border (the header), eight
/// rows inside and its bottom border.
const CARD_ROWS: u16 = 10;
/// Rows between one card and the next.
const CARD_GAP: u16 = 1;
/// Lines of an answered prompt a card shows at its usual height.
const PROMPT_LINES: usize = 1;
/// The fewest lines of the answer an expanded card taller than the feed
/// keeps under its prompt.
const ANSWER_MIN_LINES: usize = 3;
/// A prompt bar's columns before and after its text.
const BAR_INSET: usize = 2;
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

/// The feed in `area` (the drawer's, on its Feed tab): the drawer's band
/// over the cards, or the reply read in their place; in a wide Feed tab,
/// the cards with the selected reply beside them.
pub fn draw_feed(f: &mut Frame, app: &App, area: Rect, hits: &mut Vec<(Rect, Hit)>) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let p = theme::palette();
    let focused = app.drawer_has_keys();
    let (cards, beside) = match app.feed_columns() {
        Some((cards, line, reply)) => {
            let lines: Vec<Line> = (0..line.height).map(|_| Line::from("│")).collect();
            f.render_widget(
                Paragraph::new(lines).style(Style::default().fg(p.border_dim).bg(p.background)),
                line,
            );
            (cards, Some(reply))
        }
        None => (area, None),
    };
    f.render_widget(
        Block::default().style(Style::default().fg(p.text_primary).bg(p.panel)),
        cards,
    );
    let band = Rect {
        height: folder_item_height(cards.height),
        ..cards
    };
    let reading = app.feed.reading && beside.is_none();
    draw_band(f, app, band, reading, hits);
    let below = Rect {
        y: band.bottom(),
        height: cards.height - band.height,
        ..cards
    };
    if reading {
        draw_feed_reply(f, app, below, hits);
    } else {
        draw_feed_list(f, app, below, focused, hits);
    }
    if let Some(reply) = beside {
        draw_feed_reply(f, app, reply, hits);
    }
}

/// The feed's cards (or its state) in `area`, recording a target per card;
/// the selected card's border is the accent's while the feed has the keys.
fn draw_feed_list(
    f: &mut Frame,
    app: &App,
    area: Rect,
    focused: bool,
    hits: &mut Vec<(Rect, Hit)>,
) {
    if area.width == 0 || area.height == 0 {
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
    // What the feed waits for, a row down, wrapped at the indent so a
    // narrow pane keeps all of it.
    let message = |f: &mut Frame, area: Rect, lines: Vec<Line<'static>>| {
        let inner = Rect {
            x: area.x + INDENT as u16,
            y: area.y + 1,
            width: area.width.saturating_sub(INDENT as u16 + 1),
            height: area.height.saturating_sub(1),
        };
        f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: true }), inner);
    };
    let mut area = area;
    match status(app) {
        FeedStatus::SignedOut => {
            let lines = vec![
                Line::from(Span::styled("Sign in to see your feed:", strong)),
                Line::from(vec![
                    Span::styled(",", key),
                    Span::styled(" Settings → Account", muted),
                ]),
            ];
            message(f, area, lines);
            return;
        }
        FeedStatus::Off => {
            let line = Line::from(Span::styled(
                "The feed isn't on for your account yet.",
                strong,
            ));
            message(f, area, vec![line]);
            return;
        }
        FeedStatus::Loading if app.feed.cards().is_empty() => {
            let line = Line::from(Span::styled(
                format!("{} Loading the feed…", get_spinner_char()),
                muted,
            ));
            message(f, area, vec![line]);
            return;
        }
        FeedStatus::Unavailable | FeedStatus::Unknown => {
            // Kept entries stay; a quiet line says they may be old.
            let line = Line::from(Span::styled("can't reach rakumux.com", muted));
            message(f, area, vec![line]);
            area.y += 2;
            area.height = area.height.saturating_sub(2);
        }
        FeedStatus::Loading | FeedStatus::Ready => {}
    }
    let cards = app.feed.cards();
    if cards.is_empty() {
        if status(app) == FeedStatus::Ready {
            message(
                f,
                area,
                vec![Line::from(Span::styled("No replies yet", strong))],
            );
        }
        return;
    }
    // A row between the band and the first card.
    area.y += 1;
    area.height = area.height.saturating_sub(1);
    let selected = app.feed.selected_index().unwrap_or(0);
    // Each card's rows and the gap after it; only an expanded card's depend
    // on what it says.
    let card_width = area.width.saturating_sub(2);
    let heights: Vec<u16> = cards
        .iter()
        .map(|card| card_rows(app, *card, card_width, area.height) + CARD_GAP)
        .collect();
    // The last card shown needs no gap after it.
    let first = first_visible_row(&heights, selected, area.height + CARD_GAP);
    let now = now_secs();
    let mut y = area.y;
    for (index, card) in cards.iter().enumerate().skip(first) {
        let rows = heights[index] - CARD_GAP;
        if y + rows > area.bottom() {
            break;
        }
        let cell = Rect {
            y,
            height: rows,
            ..area
        };
        let chosen = index == selected;
        // The toggle's target goes first: the first target a click is in wins.
        if let Some(toggle) = draw_card(f, app, *card, cell, chosen, focused, now) {
            hits.push((toggle, Hit::FeedExpand(index)));
        }
        hits.push((cell, Hit::FeedEntry(index)));
        y += heights[index];
    }
}

/// A card's rows: [`CARD_ROWS`], or an expanded one's whole body and its
/// borders (never fewer, and at most `max` rows: the feed's).
fn card_rows(app: &App, card: Card, width: u16, max: u16) -> u16 {
    if !app.feed.is_expanded(card) {
        return CARD_ROWS.min(max.max(1));
    }
    let body = card_body(card, width, true, false, max);
    (body.rows() + 2).max(CARD_ROWS).min(max.max(1))
}

/// What a card shows inside its borders: the prompt's bar, then the answer
/// (or that it's coming), and whether there's more than it shows.
struct CardBody {
    bar: Vec<Line<'static>>,
    answer: Vec<Line<'static>>,
    more: bool,
}

impl CardBody {
    fn rows(&self) -> u16 {
        (self.bar.len() + self.answer.len()) as u16
    }
}

/// `card`'s body for a card `width` columns wide: at its usual height, its
/// prompt's first line (a waiting prompt as much as leaves a line for the
/// note under it) and the start of the answer; expanded, all of both, cut
/// to fit `max` rows with its borders.
fn card_body(card: Card, width: u16, expanded: bool, chosen: bool, max: u16) -> CardBody {
    let p = theme::palette();
    let inner_rows = if expanded {
        (max as usize).saturating_sub(2)
    } else {
        (CARD_ROWS as usize).saturating_sub(2)
    };
    // The bar runs from border to border, the text a column further in.
    let bar_width = (width as usize).saturating_sub(2);
    let text_width = (width as usize).saturating_sub(4);
    let text_style = Style::default().fg(if chosen { p.text_primary } else { p.text_muted });
    let mut more = false;
    let bar = match card.prompt() {
        Some(prompt) => {
            let whole = prompt_bar(&prompt.text, bar_width, None, chosen).len() - 2;
            // A waiting prompt's note, or a few lines of an expanded answer.
            let under = match card {
                Card::Open(_) => 1,
                Card::Entry(_) if expanded => ANSWER_MIN_LINES,
                Card::Entry(_) => inner_rows.saturating_sub(PROMPT_LINES + 2),
            };
            let rows = match card {
                Card::Entry(_) if !expanded => PROMPT_LINES,
                _ => inner_rows.saturating_sub(under + 2).max(1),
            };
            more |= whole > rows;
            let rows = rows.min(whole.max(1));
            // An expanded card keeps the prompt's own line breaks.
            if expanded && whole <= rows {
                prompt_bar(&prompt.text, bar_width, None, chosen)
            } else {
                prompt_bar(&prompt.text, bar_width, Some(rows), chosen)
            }
        }
        None => Vec::new(),
    };
    let room = inner_rows.saturating_sub(bar.len());
    let answer = match card {
        Card::Entry(entry) => {
            let (lines, cut) = preview(&entry.text, text_width, room, text_style, expanded);
            more |= cut;
            lines
        }
        Card::Open(_) => vec![Line::from(Span::styled(
            "waiting for the answer…",
            Style::default()
                .fg(p.text_muted)
                .add_modifier(Modifier::ITALIC),
        ))],
    };
    CardBody { bar, answer, more }
}

/// A card's border: the accent's on the selected card while the feed has
/// the keys, lighter than the others' on it otherwise.
fn card_border(chosen: bool, focused: bool) -> Style {
    let p = theme::palette();
    Style::default().fg(match (chosen, focused) {
        (true, true) => p.accent_primary,
        (true, false) => p.neutral_gray,
        (false, _) => p.border,
    })
}

/// One card in `cell`, a column in from each side: the top border names its
/// session (then, quieter, its agent, folder, repository and branch) on the
/// left and says its age and status on the right; inside, the prompt on a
/// bar, then the answer (or that it's coming); on the bottom border, at the
/// right, `▾ more` when it shows only part of that (`▴ less` once
/// expanded), whose target it returns.
fn draw_card(
    f: &mut Frame,
    app: &App,
    card_of: Card,
    cell: Rect,
    chosen: bool,
    focused: bool,
    now: i64,
) -> Option<Rect> {
    let p = theme::palette();
    let card = Rect {
        x: cell.x + 1,
        width: cell.width.saturating_sub(2),
        ..cell
    };
    if card.width < 8 {
        return None;
    }
    let border = card_border(chosen, focused);
    f.render_widget(
        Block::default()
            .borders(Borders::LEFT | Borders::RIGHT | Borders::BOTTOM)
            .border_type(BorderType::Rounded)
            .border_style(border),
        Rect {
            y: card.y + 1,
            height: card.height.saturating_sub(1),
            ..card
        },
    );
    f.render_widget(
        Paragraph::new(card_top(
            app,
            card_of,
            card.width as usize,
            chosen,
            focused,
            now,
        )),
        Rect { height: 1, ..card },
    );
    let expanded = app.feed.is_expanded(card_of);
    let body = card_body(card_of, card.width, expanded, chosen, cell.height);
    let inner = Rect {
        x: card.x + 2,
        width: card.width.saturating_sub(4),
        y: card.y + 1,
        height: card.height.saturating_sub(2),
    };
    // The prompt on a bar from border to border, the answer under it.
    let bar_rows = (body.bar.len() as u16).min(inner.height);
    f.render_widget(
        Paragraph::new(body.bar),
        Rect {
            x: card.x + 1,
            width: card.width.saturating_sub(2),
            height: bar_rows,
            ..inner
        },
    );
    f.render_widget(
        Paragraph::new(body.answer),
        Rect {
            y: inner.y + bar_rows,
            height: inner.height - bar_rows,
            ..inner
        },
    );
    let label = match (expanded, body.more) {
        (true, _) => " ▴ less ",
        (false, true) => " ▾ more ",
        (false, false) => return None,
    };
    let width = str_width(label) as u16;
    if card.height < 2 || card.width < width + 4 {
        return None;
    }
    let toggle = Rect {
        x: card.right() - 2 - width,
        y: card.bottom() - 1,
        width,
        height: 1,
    };
    let style = Style::default().fg(if chosen && focused {
        p.accent_primary
    } else {
        p.text_muted
    });
    f.render_widget(Paragraph::new(Span::styled(label, style)), toggle);
    Some(toggle)
}

/// A user's words on a bar across `width` columns (Tokyo Night's storm
/// background; the selection's on a selected card): [`BAR_INSET`] columns in
/// from each side, with half a row of the bar above and below (`▄`, `▀`), so
/// it's padded without touching the lines around it. Wrapped, or run
/// together as one paragraph and cut to `rows` with `…` when given (a
/// card's).
fn prompt_bar(text: &str, width: usize, rows: Option<usize>, chosen: bool) -> Vec<Line<'static>> {
    let p = theme::palette();
    let colour = if chosen { p.selection } else { p.surface };
    let bar = Style::default().bg(colour);
    let style = bar.fg(if chosen || rows.is_none() {
        p.text_bright
    } else {
        p.text_primary
    });
    let room = width.saturating_sub(2 * BAR_INSET).max(1);
    let text = plain(text);
    let mut lines = match rows {
        Some(_) => wrap(&text.split_whitespace().collect::<Vec<_>>().join(" "), room),
        None => wrap(&text, room),
    };
    if let Some(rows) = rows {
        let more = lines.len() > rows;
        lines.truncate(rows);
        if more {
            if let Some(last) = lines.last_mut() {
                *last = truncate(&format!("{last} …"), room);
            }
        }
    }
    if lines.is_empty() {
        lines.push(String::new());
    }
    let edge = |half: &str| {
        Line::from(Span::styled(
            half.repeat(width),
            Style::default().fg(colour),
        ))
    };
    let mut bar_lines = vec![edge("▄")];
    bar_lines.extend(lines.into_iter().map(|line| {
        let pad = width.saturating_sub(BAR_INSET + str_width(&line));
        Line::from(vec![
            Span::styled(" ".repeat(BAR_INSET), bar),
            Span::styled(line, style),
            Span::styled(" ".repeat(pad), bar),
        ])
    }));
    bar_lines.push(edge("▀"));
    bar_lines
}

/// A reply as a card shows it, its Markdown drawn: `rows` of it, `…` when
/// there's more (and whether there was). At a card's usual height its blank
/// lines are left out; expanded, only those at its ends.
fn preview(
    text: &str,
    width: usize,
    rows: usize,
    style: Style,
    expanded: bool,
) -> (Vec<Line<'static>>, bool) {
    let mut lines: Vec<Line<'static>> = markdown::render(text, width, style);
    if expanded {
        while lines.last().is_some_and(|line| line.width() == 0) {
            lines.pop();
        }
        let blank = lines.iter().take_while(|line| line.width() == 0).count();
        lines.drain(..blank);
    } else {
        lines.retain(|line| line.width() > 0);
    }
    let more = lines.len() > rows;
    lines.truncate(rows);
    if more {
        if let Some(last) = lines.last_mut() {
            let room = width.saturating_sub(2);
            while last.width() > room && !last.spans.is_empty() {
                let span = last.spans.pop().unwrap_or_default();
                let keep = room.saturating_sub(last.width());
                if keep > 0 {
                    last.spans
                        .push(Span::styled(truncate(&span.content, keep), span.style));
                    break;
                }
            }
            last.spans.push(Span::styled(" …", style));
        }
    }
    (lines, more)
}

/// A card's top border, `width` columns: `╭ session  agent · folder · repo ⎇
/// branch ─── age · ● status ╮`. Short of room, the status keeps only its
/// mark, then the agent, folder, repository and branch give way from the
/// end, then the session's name.
fn card_top(
    app: &App,
    card: Card,
    width: usize,
    chosen: bool,
    focused: bool,
    now: i64,
) -> Line<'static> {
    let p = theme::palette();
    let border = card_border(chosen, focused);
    let muted = Style::default().fg(p.text_muted);
    let name_style = Style::default()
        .fg(if chosen {
            p.text_bright
        } else {
            p.text_primary
        })
        .add_modifier(Modifier::BOLD);
    let status = match card {
        Card::Entry(entry) => app.feed.status_of(entry),
        Card::Open(_) => EntryStatus::Working,
    };
    let (mark, mark_style) = status_mark(status);
    let at = match card {
        Card::Open(open) => open.prompt.at.unwrap_or(now * 1000),
        Card::Entry(entry) => entry.finished_at,
    };
    let age = relative_time(now, at / 1000);
    let name = plain(card.session_name());
    let mut meta = card_meta(app, card);
    // The status's word goes first when the left part wouldn't fit whole.
    let left_width = str_width(&name) + 2 + str_width(&meta);
    let whole = str_width(&age) + str_width(status.label()) + 7;
    let label = if left_width + whole + 5 <= width {
        status.label()
    } else {
        ""
    };
    let mut right = vec![
        Span::raw(" "),
        Span::styled(age, muted),
        Span::styled(" · ", muted),
        Span::styled(mark, mark_style),
    ];
    if !label.is_empty() {
        right.push(Span::raw(" "));
        right.push(Span::styled(label, mark_style));
    }
    right.push(Span::raw(" "));
    let right_width: usize = right.iter().map(|span| str_width(&span.content)).sum();
    // Corners, the spaces around the left part, and at least one dash.
    let room = width.saturating_sub(right_width + 5);
    let name = truncate(&name, room);
    let meta_room = room.saturating_sub(str_width(&name) + 2);
    meta = if meta_room >= 4 {
        truncate(&meta, meta_room)
    } else {
        String::new()
    };
    let mut spans = vec![
        Span::styled("╭", border),
        Span::raw(" "),
        Span::styled(name.clone(), name_style),
    ];
    let mut used = 2 + str_width(&name);
    if !meta.is_empty() {
        used += 2 + str_width(&meta);
        spans.push(Span::raw("  "));
        spans.push(Span::styled(meta, muted));
    }
    spans.push(Span::raw(" "));
    used += 1;
    let fill = width.saturating_sub(used + right_width + 1);
    spans.push(Span::styled("─".repeat(fill), border));
    spans.extend(right);
    spans.push(Span::styled("╮", border));
    Line::from(spans)
}

/// What a card names after its session: the agent, then its folder, the
/// repository (when its session is this machine's and listed here, and not
/// named like its folder) and the branch it worked on: `claude · web ·
/// focal-md ⎇ main`.
fn card_meta(app: &App, card: Card) -> String {
    let folder = card.folder().map(plain).filter(|folder| !folder.is_empty());
    let repo = app
        .feed_card_session(card)
        .and_then(|session| session.git.as_ref())
        .and_then(|git| git.repo.as_deref())
        .map(plain)
        .filter(|repo| !repo.is_empty())
        .filter(|repo| {
            !folder
                .as_ref()
                .is_some_and(|f| f.eq_ignore_ascii_case(repo))
        });
    let mut parts = vec![plain(agent_name(card.agent()))];
    parts.extend(folder);
    parts.extend(repo);
    let mut meta = parts.join(" · ");
    if let Some(branch) = card.branch().map(plain).filter(|b| !b.is_empty()) {
        meta.push_str(&format!(" {} {branch}", app.icons().branch()));
    }
    meta
}

/// A status's mark and colour on a card, the colours rakumux.com's Feed
/// gives them.
fn status_mark(status: EntryStatus) -> (String, Style) {
    let p = theme::palette();
    let (mark, colour) = match status {
        EntryStatus::Working => (get_spinner_char().to_string(), p.accent_primary),
        EntryStatus::Sending => (get_spinner_char().to_string(), p.text_muted),
        EntryStatus::Done => ("●".into(), p.accent_green),
        EntryStatus::ReplyWaiting => ("◐".into(), p.accent_gold),
        EntryStatus::ReplyTypedIn => ("●".into(), p.accent_primary),
        EntryStatus::Answered => ("✓".into(), p.accent_purple),
        EntryStatus::ReplyNotSent => ("✕".into(), p.accent_pink),
    };
    (mark, Style::default().fg(colour))
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
fn draw_feed_reply(f: &mut Frame, app: &App, area: Rect, hits: &mut Vec<(Rect, Hit)>) {
    let p = theme::palette();
    f.render_widget(
        Block::default().style(Style::default().bg(p.background)),
        area,
    );
    if area.width == 0 || area.height == 0 {
        return;
    }
    let muted = Style::default().fg(p.text_muted);
    let inner = Rect {
        x: area.x + 2,
        width: area.width.saturating_sub(4),
        y: area.y + 1,
        height: area.height.saturating_sub(1),
    };
    let entry = match app.feed.selected_card() {
        Some(Card::Entry(entry)) => entry,
        Some(Card::Open(open)) => {
            draw_open(f, app, open, inner, hits);
            return;
        }
        None => return,
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
    let can_reply = app.feed.reply_to(Card::Entry(entry)).is_some();
    actions(app, can_reply, inner, &mut lines, hits);
    lines.push(Line::default());
    let head = lines.len();
    // The prompt (when known) and the answer, while the whole turn isn't read.
    let reply = || {
        let mut lines = Vec::new();
        if let Some(prompt) = &entry.prompt {
            lines.extend(prompt_bar(&prompt.text, width, None, false));
            lines.push(Line::default());
        }
        lines.extend(markdown::render(
            &entry.text,
            width,
            Style::default().fg(p.text_primary),
        ));
        lines
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
    let mut body = body;
    body.extend(thread_lines(app, entry, width));
    let room = (inner.height as usize).saturating_sub(head);
    let scroll = app.feed.scroll.min(body.len().saturating_sub(room));
    lines.extend(body.into_iter().skip(scroll).take(room));
    f.render_widget(Paragraph::new(lines), inner);
}

/// What can be done with the selected card, under its heading: `Enter` goes
/// into its session when it runs here, and its `↩ Reply` button (`r`) when
/// it takes a reply; both on one line when they fit (a narrow pane puts the
/// button under).
fn actions(
    app: &App,
    can_reply: bool,
    inner: Rect,
    lines: &mut Vec<Line<'static>>,
    hits: &mut Vec<(Rect, Hit)>,
) {
    let p = theme::palette();
    let muted = Style::default().fg(p.text_muted);
    let key = Style::default()
        .fg(p.accent_primary)
        .add_modifier(Modifier::BOLD);
    let width = inner.width as usize;
    let mut action = if app.feed_session().is_some() {
        vec![
            Span::styled("Enter", key),
            Span::styled(" goes into its session", muted),
        ]
    } else {
        Vec::new()
    };
    if can_reply {
        // A button for a click or a tap, `r` from the keyboard.
        let button = " ↩ Reply  r ";
        let before: usize = action.iter().map(|span| str_width(&span.content)).sum();
        let mut x = before + 3;
        if action.is_empty() {
            x = 0;
        } else if x + str_width(button) <= width {
            action.push(Span::styled(" · ", muted));
        } else {
            lines.push(Line::from(std::mem::take(&mut action)));
            x = 0;
        }
        let at = inner.x.saturating_add(x as u16);
        let rect = Rect {
            x: at,
            y: inner.y + lines.len() as u16,
            width: (str_width(button) as u16).min(inner.right().saturating_sub(at)),
            height: 1,
        };
        if rect.width > 0 && rect.y < inner.bottom() {
            hits.push((rect, Hit::FeedReply));
        }
        action.push(Span::styled(
            button,
            Style::default()
                .fg(p.background)
                .bg(p.accent_primary)
                .add_modifier(Modifier::BOLD),
        ));
    }
    if !action.is_empty() {
        lines.push(Line::from(action));
    }
}

/// A prompt still waiting for its answer, read whole: its session and
/// where it's from, when it was sent, then (scrolled under that heading, as
/// a reply is) the prompt and that the answer comes here.
fn draw_open(
    f: &mut Frame,
    app: &App,
    open: &rakumux_sensagent::feed::OpenTurn,
    inner: Rect,
    hits: &mut Vec<(Rect, Hit)>,
) {
    let p = theme::palette();
    let muted = Style::default().fg(p.text_muted);
    let width = inner.width as usize;
    let mut place = vec![plain(agent_name(&open.agent))];
    if let Some(where_) = open.branch.as_ref().or(open.folder.as_ref()) {
        place.push(plain(where_));
    }
    if open.machine != app.feed.machine() {
        place.push(format!("on {}", plain(&open.machine)));
    }
    let name = plain(&open.session_name);
    let sent = match open.prompt.at {
        Some(at) => match relative_time(now_secs(), at / 1000).as_str() {
            "now" => "asked just now".to_owned(),
            age => format!("asked {age} ago"),
        },
        None => "asked".to_owned(),
    };
    let mut lines = vec![
        Line::from(vec![
            Span::styled(
                truncate(&name, width),
                Style::default()
                    .fg(p.text_bright)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                truncate(
                    &format!(" · {}", place.join(" · ")),
                    width.saturating_sub(str_width(&name)),
                ),
                muted,
            ),
        ]),
        Line::from(Span::styled(
            truncate(&format!("{sent} · working"), width),
            muted,
        )),
    ];
    let can_reply = app.feed.reply_to(Card::Open(open)).is_some();
    actions(app, can_reply, inner, &mut lines, hits);
    lines.push(Line::default());
    let head = lines.len();
    let mut body = prompt_bar(&open.prompt.text, width, None, false);
    body.push(Line::default());
    body.push(Line::from(Span::styled(
        format!("{} Waiting for the answer…", get_spinner_char()),
        muted,
    )));
    let room = (inner.height as usize).saturating_sub(head);
    let scroll = app.feed.scroll.min(body.len().saturating_sub(room));
    lines.extend(body.into_iter().skip(scroll).take(room));
    f.render_widget(Paragraph::new(lines), inner);
}

/// The rows of a reply written back shown in a thread.
const THREAD_REPLY_LINES: usize = 4;

/// `entry`'s thread, under its turn: each step oldest first, `entry` itself
/// marked `▸`; each reply written back with who wrote it, where it's up to,
/// and its start.
fn thread_lines(app: &App, entry: &FeedEntry, width: usize) -> Vec<Line<'static>> {
    let items = app.feed.thread(entry);
    if items.is_empty() {
        return Vec::new();
    }
    let p = theme::palette();
    let muted = Style::default().fg(p.text_muted);
    let text = Style::default().fg(p.text_primary);
    let mark = Style::default()
        .fg(p.accent_primary)
        .add_modifier(Modifier::BOLD);
    let now = now_secs();
    let mut lines = vec![
        Line::default(),
        Line::from(Span::styled(
            "Thread",
            Style::default()
                .fg(p.text_bright)
                .add_modifier(Modifier::BOLD),
        )),
    ];
    for item in items {
        match item {
            ThreadItem::Entry(step) => {
                let about = format!(
                    "{} · {} · ",
                    agent_name(&step.agent),
                    relative_time(now, step.finished_at / 1000)
                );
                let first = plain(&step.text);
                let first = first
                    .lines()
                    .find(|line| !line.trim().is_empty())
                    .unwrap_or("");
                let room = width.saturating_sub(2 + str_width(&about));
                let style = if step.id == entry.id { text } else { muted };
                lines.push(Line::from(vec![
                    Span::styled(if step.id == entry.id { "▸ " } else { "● " }, mark),
                    Span::styled(about, muted),
                    Span::styled(truncate(first.trim(), room), style),
                ]));
            }
            ThreadItem::Reply(reply) => {
                let about = format!(
                    "{} · {} · {}",
                    reply_author(app, reply),
                    relative_time(now, reply.created_at / 1000),
                    reply_state(reply)
                );
                lines.push(Line::from(vec![
                    Span::styled("↩ ", mark),
                    Span::styled(truncate(&about, width.saturating_sub(2)), muted),
                ]));
                lines.extend(prompt_bar(
                    &reply.text,
                    width,
                    Some(THREAD_REPLY_LINES),
                    false,
                ));
            }
        }
    }
    lines
}

/// Who wrote a reply back: you, and where when it wasn't here.
fn reply_author(app: &App, reply: &FeedReply) -> String {
    match reply.from.as_deref() {
        None => "you on rakumux.com".into(),
        Some(machine) if machine == app.feed.machine() => "you".into(),
        Some(machine) => format!("you on {}", plain(machine)),
    }
}

/// Where a reply written back is up to.
fn reply_state(reply: &FeedReply) -> &'static str {
    match (reply.status, reply.failure) {
        (ReplyStatus::Pending, _) => "waiting for its agent",
        (ReplyStatus::Delivered, _) if reply.response.is_some() => "answered",
        (ReplyStatus::Delivered, _) => "typed in",
        (ReplyStatus::Failed, Some(ReplyFailure::SessionGone)) => "not sent: its session is gone",
        (ReplyStatus::Failed, Some(ReplyFailure::ConversationChanged)) => {
            "not sent: the session shows another chat"
        }
        (ReplyStatus::Failed, Some(ReplyFailure::Private)) => "not sent: the session is private",
        (ReplyStatus::Failed, Some(ReplyFailure::NotAccepted)) => {
            "not sent: its machine doesn't take replies from rakumux.com"
        }
        (ReplyStatus::Failed, Some(ReplyFailure::Refused)) => "not sent: the session refused it",
        (ReplyStatus::Failed, Some(ReplyFailure::AttachmentUnavailable)) => {
            "not sent: its files couldn't be fetched"
        }
        (ReplyStatus::Failed, Some(ReplyFailure::NotTaken)) => {
            "not taken: it may still be in the session's input box"
        }
        (ReplyStatus::Failed, _) => "not sent",
        (ReplyStatus::Expired, _) => "not sent: its agent didn't wait for an hour",
        (ReplyStatus::Unknown, _) => "",
    }
}

/// What a line of a turn shows, to put a blank line between kinds.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Prompt,
    Text,
    Reasoning,
    Tool,
}

/// A turn's messages as lines of `width` columns: the user's prompts on a
/// bar across them ([`prompt_bar`]), reasoning quiet and italic, each tool
/// call `●` with what it works on and the start of its output under it, the
/// agent's text with its Markdown drawn.
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
                    let text = if *truncated {
                        format!("{} …", text.trim_end())
                    } else {
                        text.clone()
                    };
                    lines.extend(prompt_bar(&text, width, None, false));
                }
                TranscriptPart::Text { text, truncated } => {
                    gap(&mut lines, Kind::Text);
                    lines.extend(markdown::render(text, width, text_style));
                    if *truncated {
                        lines.push(Line::from(Span::styled("…", muted)));
                    }
                }
                TranscriptPart::Reasoning { text, .. } => {
                    gap(&mut lines, Kind::Reasoning);
                    lines.extend(markdown::render(text, width, quiet));
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

/// The fields of a tool call's JSON input that say what it works on, in
/// order: a description first (Claude Code's and Kimi Code's Bash, Grok's
/// shell, Droid's `Execute` `summary`), then a command, a file or folder (Grok's
/// and Cursor's `target_*`, Droid's `directory_path`), a pattern (Droid's
/// `patterns`, Cursor's `glob_pattern`, Amp's `filePattern`), a URL or query,
/// and last a patch (Amp's `patchText`, Droid's `input`). rakumux.com's chats
/// (`toolSubject`) read the same fields in the same order, a blank one saying
/// nothing there too.
const SUBJECT_FIELDS: [&str; 22] = [
    "description",
    "summary",
    "command",
    "cmd",
    "file_path",
    "filePath",
    "path",
    "pattern",
    "url",
    "query",
    "prompt",
    "target_file",
    "target_notebook",
    "target_directory",
    "directory_path",
    "patterns",
    "glob_pattern",
    "filePattern",
    "search_term",
    "task",
    "patchText",
    "input",
];

/// What a tool call works on, in a few words: the first of its input's
/// [`SUBJECT_FIELDS`] that says something (a string, or strings joined),
/// else the input itself; its first line, or for a patch (`*** Begin Patch`)
/// the first file it changes.
fn tool_subject(input: &str) -> String {
    let value: Option<serde_json::Value> = serde_json::from_str(input).ok();
    let field = value.as_ref().and_then(|value| {
        SUBJECT_FIELDS.iter().find_map(|key| {
            let text = match value.get(key)? {
                serde_json::Value::String(text) => text.clone(),
                serde_json::Value::Array(parts) => parts
                    .iter()
                    .filter_map(serde_json::Value::as_str)
                    .collect::<Vec<_>>()
                    .join(" "),
                _ => return None,
            };
            (!text.trim().is_empty()).then_some(text)
        })
    });
    let text = plain(&field.unwrap_or_else(|| input.to_owned()));
    let mut lines = text.lines().map(str::trim).filter(|line| !line.is_empty());
    let first = lines.next().unwrap_or_default();
    if first == "*** Begin Patch" {
        let file = lines.find_map(|line| {
            ["*** Add File:", "*** Update File:", "*** Delete File:"]
                .iter()
                .find_map(|mark| line.strip_prefix(mark))
                .map(str::trim)
                .filter(|path| !path.is_empty())
        });
        if let Some(file) = file {
            return file.to_owned();
        }
    }
    first.to_owned()
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
    use super::{reply_state, tool_subject, wrap};
    use rakumux_sensagent::feed::{FeedReply, ReplyFailure, ReplyStatus};

    #[test]
    fn a_tool_call_shows_what_it_works_on_whichever_agent_made_it() {
        let cases = [
            // Claude Code: its own description before the command.
            (
                r#"{"command":"cargo test","description":"Run the tests"}"#,
                "Run the tests",
            ),
            (
                r#"{"file_path":"/repo/src/lib.rs","old_string":"a"}"#,
                "/repo/src/lib.rs",
            ),
            // Droid: Execute's summary, else its command (an empty one says nothing).
            (
                r#"{"command":"npm test","summary":"Run the login tests"}"#,
                "Run the login tests",
            ),
            (r#"{"command":"cargo test","summary":""}"#, "cargo test"),
            (r#"{"directory_path":"/repo/src"}"#, "/repo/src"),
            (r#"{"patterns":["src/*.rs","*.toml"]}"#, "src/*.rs *.toml"),
            (
                r#"{"input":"*** Begin Patch\n*** Update File: /repo/src/lib.rs\n@@\n-a\n+b\n*** End Patch"}"#,
                "/repo/src/lib.rs",
            ),
            // GitHub Copilot: view, create and edit name a path; bash a command.
            (
                r#"{"path":"/repo/README.md","view_range":[1,20]}"#,
                "/repo/README.md",
            ),
            (
                r#"{"path":"/repo/new.rs","file_text":"fn main() {}"}"#,
                "/repo/new.rs",
            ),
            (r#"{"command":"ls -la","description":""}"#, "ls -la"),
            // Cursor: a path, a file or folder to read, a glob, a search.
            (r#"{"path":"src/app.rs","contents":"…"}"#, "src/app.rs"),
            (r#"{"target_file":"src/main.rs"}"#, "src/main.rs"),
            (r#"{"target_directory":"src"}"#, "src"),
            (r#"{"glob_pattern":"**/*.ts"}"#, "**/*.ts"),
            (r#"{"search_term":"parser bug"}"#, "parser bug"),
            // Grok: read_file's target_file.
            (
                r#"{"target_file":"/repo/Cargo.toml","offset":1}"#,
                "/repo/Cargo.toml",
            ),
            // Amp: Bash's cmd, glob's filePattern, a patch's first file, the oracle's task.
            (r#"{"cmd":"make check","cwd":"/repo"}"#, "make check"),
            (r#"{"filePattern":"**/*.go"}"#, "**/*.go"),
            (
                r#"{"patchText":"*** Begin Patch\n*** Add File: docs/new.md\n+hello\n*** End Patch"}"#,
                "docs/new.md",
            ),
            (
                r#"{"task":"Review the parser","files":["a.rs"]}"#,
                "Review the parser",
            ),
            // Pi and Kimi Code: a command, a path, a pattern.
            (r#"{"path":"@src/lib.rs"}"#, "@src/lib.rs"),
            (r#"{"pattern":"TODO"}"#, "TODO"),
            // A patch given whole (Codex), and input that isn't JSON.
            (
                "*** Begin Patch\n*** Delete File: old.txt\n*** End Patch",
                "old.txt",
            ),
            ("\n  ls -la\nmore", "ls -la"),
            // Strings only: a list without any says nothing.
            (r#"{"patterns":[1,2],"query":"rust"}"#, "rust"),
        ];
        for (input, subject) in cases {
            assert_eq!(tool_subject(input), subject, "{input}");
        }
    }

    #[test]
    fn replies_wrap_at_words_and_keep_their_line_breaks() {
        assert_eq!(
            wrap("The tests pass now.\n\n- fixed the parser", 12),
            ["The tests", "pass now.", "", "- fixed the", "parser"]
        );
        assert_eq!(wrap("abcdefghij", 4), ["abcd", "efgh", "ij"]);
        assert_eq!(wrap("", 10), Vec::<String>::new());
    }

    #[test]
    fn a_reply_whose_files_couldnt_be_fetched_says_so() {
        let mut reply = FeedReply {
            seq: 2,
            id: "R1".into(),
            entry: "E1".into(),
            thread: "E1".into(),
            machine: "lab-box".into(),
            session_id: "Z".into(),
            session_name: "parser".into(),
            folder: None,
            agent: "claude-code".into(),
            conversation_id: "c".into(),
            text: "See the screenshot.".into(),
            attachments: Vec::new(),
            from: None,
            status: ReplyStatus::Failed,
            failure: Some(ReplyFailure::AttachmentUnavailable),
            response: None,
            created_at: 1,
            status_at: 1,
        };
        assert_eq!(
            reply_state(&reply),
            "not sent: its files couldn't be fetched"
        );
        reply.failure = Some(ReplyFailure::NotTaken);
        assert_eq!(
            reply_state(&reply),
            "not taken: it may still be in the session's input box"
        );
        // A reason this build doesn't know is a failure without one.
        reply.failure = Some(ReplyFailure::Unknown);
        assert_eq!(reply_state(&reply), "not sent");
    }
}
