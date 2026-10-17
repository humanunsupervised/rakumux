//! The folder filter at the left end of the tab bar (`▤ All folders ▾`,
//! `▤ web ▾`, `▤ 3 folders ▾`), and the Folders modal it (and the list's
//! folder filter row) opens: a query line, All folders, the folders with
//! sessions (a tick, branch, session counts and path each), the typed
//! folder to add and the folders that complete it.

use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
    Frame,
};

use session_core::model::{ScopeRoot, SessionStatus};

use super::format::{abbreviate_home, branch_label, truncate, truncate_middle};
use super::utils::get_spinner_char;
use crate::app::folders::looks_like_path;
use crate::app::types::{AppMode, FolderPicker, NewFolder, PickerRow, PickerStage};
use crate::app::{App, Hit};
use crate::layout::str_width;
use crate::theme;

/// Widest the modal gets.
const MODAL_WIDTH: u16 = 84;
/// Most rows shown at once.
const MAX_ROWS: u16 = 12;
const NAME_MAX: usize = 24;
const BRANCH_MAX: usize = 22;

/// `2/3 live`, or `no sessions`.
pub fn count_label(live: usize, total: usize) -> String {
    if total == 0 {
        "no sessions".into()
    } else {
        format!("{live}/{total} live")
    }
}

/// The git branch of a folder, as the daemon last read it, with the
/// configured mark (`⎇ main`).
pub fn folder_branch(app: &App, folder: &ScopeRoot) -> Option<String> {
    app.folders
        .get(folder)
        .and_then(|p| p.git.as_ref())
        .and_then(|git| branch_label(git, app.icons()))
}

/// Draw the folder filter at the left of the one-row `area` (the tab bar);
/// returns its click target.
pub fn draw_selector(f: &mut Frame, app: &App, area: Rect, focused: bool) -> Rect {
    let p = theme::palette();
    if area.width == 0 || area.height == 0 {
        return Rect::default();
    }
    let open = matches!(app.mode, AppMode::Folders(_));
    let arrow = if open { "▴" } else { "▾" };
    let label = truncate(&app.folder_label(), (area.width as usize).saturating_sub(6));
    let label_style = if focused || open {
        Style::default()
            .fg(p.accent_primary)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default()
            .fg(p.text_primary)
            .add_modifier(Modifier::BOLD)
    };
    let icon = app.icons().folder();
    let width = ((str_width(&label) + str_width(icon)) as u16 + 5).min(area.width);
    let rect = Rect { width, ..area };
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::raw(" "),
            Span::styled(icon, Style::default().fg(p.accent_primary)),
            Span::raw(" "),
            Span::styled(label, label_style),
            Span::raw(" "),
            Span::styled(arrow, Style::default().fg(p.accent_primary)),
            Span::raw(" "),
        ]))
        .style(Style::default().bg(p.surface)),
        rect,
    );
    rect
}

/// `text` padded with spaces to `width` columns (emoji take two).
fn pad(text: &str, width: usize) -> String {
    format!(
        "{text}{}",
        " ".repeat(width.saturating_sub(str_width(text)))
    )
}

/// One folder row's text pieces.
struct FolderRowText {
    name: String,
    branch: String,
    count: String,
    path: String,
}

fn folder_row_text(app: &App, folder: &ScopeRoot) -> FolderRowText {
    let (live, total) = app.folder_counts(folder);
    FolderRowText {
        name: app.folders.name_of(folder),
        branch: folder_branch(app, folder).unwrap_or_default(),
        count: count_label(live, total),
        path: abbreviate_home(folder.path()),
    }
}

/// What Enter does on the row under the cursor, for the keys line.
fn enter_action(picker: &FolderPicker, app: &App) -> String {
    match picker.current_row() {
        Some(PickerRow::All) => "show all".into(),
        Some(PickerRow::Folder(folder)) => match picker.folders_for(folder).len() {
            1 => "show".into(),
            n => format!("show {n} folders"),
        },
        Some(PickerRow::Add(NewFolder::Existing(_))) => "add and show".into(),
        Some(PickerRow::Add(NewFolder::Create(_))) => "create and show".into(),
        Some(PickerRow::Suggestion(folder)) => {
            let known = std::fs::canonicalize(&folder.path)
                .is_ok_and(|path| app.folders.is_known(&ScopeRoot::from_canonical(path)));
            if known { "show" } else { "add and show" }.into()
        }
        None => "show".into(),
    }
}

/// Draw the Folders modal in the middle of `area` and record its click
/// targets: a query line, the rows (All folders, or the typed folder to
/// add, then the matching folders), a message line and the keys.
pub fn draw_folder_picker(
    f: &mut Frame,
    app: &App,
    picker: &FolderPicker,
    area: Rect,
    hits: &mut Vec<(Rect, Hit)>,
) {
    let p = theme::palette();
    let texts: Vec<Option<FolderRowText>> = picker
        .rows
        .iter()
        .map(|row| match row {
            PickerRow::Folder(folder) => Some(folder_row_text(app, folder)),
            PickerRow::All | PickerRow::Add(_) | PickerRow::Suggestion(_) => None,
        })
        .collect();
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
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(p.accent_primary))
        .title(Span::styled(
            " Folders ",
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
    let browsing = picker.stage == PickerStage::Browse && !picker.busy;
    let room = inner_width.saturating_sub(prompt.chars().count() + 1);
    let chars: Vec<char> = picker.query.value().chars().collect();
    let cursor = picker.query.cursor_column();
    let start = (cursor + 1).saturating_sub(room);
    let query = if chars.is_empty() {
        Span::styled(
            truncate("Filter, or type a folder path to add (~/code/app)", room),
            muted,
        )
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
    if browsing {
        f.set_cursor(
            inner.x + (prompt.chars().count() + cursor - start) as u16,
            inner.y,
        );
    }
    f.render_widget(
        Paragraph::new(Span::styled(
            "─".repeat(inner_width),
            Style::default().fg(p.border_dim),
        )),
        row_at(1),
    );

    // The rows, scrolled so the cursor shows. Columns: cursor bar and
    // tick, then name, branch, count and path, each after two spaces.
    let list_height = list_rows as usize;
    let first = (picker.cursor + 1).saturating_sub(list_height);
    let lead = 6;
    let widest = |field: fn(&FolderRowText) -> &str| {
        texts
            .iter()
            .flatten()
            .map(|t| str_width(field(t)))
            .max()
            .unwrap_or(0)
    };
    let gap = |w: usize| if w > 0 { w + 2 } else { 0 };
    let mut branch_w = widest(|t| &t.branch).min(BRANCH_MAX);
    let count_w = widest(|t| &t.count);
    let mut name_w = widest(|t| &t.name).min(NAME_MAX);
    let mut path_w = inner_width.saturating_sub(lead + name_w + gap(branch_w) + gap(count_w) + 2);
    if path_w < 8 {
        path_w = 0;
        if lead + name_w + gap(branch_w) + gap(count_w) > inner_width {
            branch_w = 0;
        }
    }
    name_w = name_w.min(inner_width.saturating_sub(lead + gap(branch_w) + gap(count_w)));
    if picker.rows.is_empty() {
        let note = picker
            .folder_note
            .clone()
            .unwrap_or_else(|| "No folder matches".into());
        f.render_widget(
            Paragraph::new(Span::styled(
                truncate(&format!("  {note}"), inner_width),
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
        let bar = Span::styled(
            if at_cursor { "▎ " } else { "  " },
            Style::default().fg(p.accent_primary),
        );
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
        let mut spans = vec![bar];
        match (row, &texts[index]) {
            (PickerRow::All, _) => {
                spans.push(tick);
                spans.push(Span::styled("All folders", name_style));
                let (live, total) = app.list.sessions().iter().fold((0, 0), |(live, total), s| {
                    let running = s.status == SessionStatus::Live;
                    (live + usize::from(running), total + 1)
                });
                let room = inner_width.saturating_sub(lead + "All folders".len() + 2);
                if room > 0 {
                    spans.push(Span::styled(
                        truncate(&format!("  {}", count_label(live, total)), room),
                        muted,
                    ));
                }
            }
            (PickerRow::Add(folder), _) => {
                let (label, path) = match folder {
                    NewFolder::Existing(path) => ("+ Add folder ", path),
                    NewFolder::Create(path) => ("+ Create folder ", path),
                };
                spans.push(Span::styled(
                    label,
                    Style::default()
                        .fg(p.accent_green)
                        .add_modifier(Modifier::BOLD),
                ));
                let room = inner_width.saturating_sub(2 + label.chars().count());
                spans.push(Span::styled(
                    truncate_middle(&abbreviate_home(path), room),
                    Style::default().fg(text_color).add_modifier(Modifier::BOLD),
                ));
            }
            // Indented like the folder names; the folder's own name stands
            // out from where it is.
            (PickerRow::Suggestion(folder), _) => {
                let known = std::fs::canonicalize(&folder.path)
                    .is_ok_and(|path| app.folders.is_known(&ScopeRoot::from_canonical(path)));
                let tag = if known { "  known" } else { "" };
                let room = inner_width.saturating_sub(lead + tag.len());
                spans.push(Span::raw("    "));
                let full = abbreviate_home(&folder.path);
                let shown = format!("{full}/");
                match full.strip_suffix(folder.name.as_str()) {
                    Some(dir) if str_width(&shown) <= room => {
                        spans.push(Span::styled(
                            dir.to_string(),
                            Style::default().fg(p.text_subtle),
                        ));
                        spans.push(Span::styled(format!("{}/", folder.name), name_style));
                    }
                    _ => spans.push(Span::styled(truncate_middle(&shown, room), name_style)),
                }
                spans.push(Span::styled(tag, muted));
            }
            (PickerRow::Folder(_), Some(text)) => {
                spans.push(tick);
                spans.push(Span::styled(
                    pad(&truncate(&text.name, name_w), name_w),
                    name_style,
                ));
                if branch_w > 0 {
                    spans.push(Span::styled(
                        format!("  {}", pad(&truncate(&text.branch, branch_w), branch_w)),
                        muted,
                    ));
                }
                if count_w > 0 {
                    spans.push(Span::styled(
                        format!("  {}", pad(&text.count, count_w)),
                        muted,
                    ));
                }
                if path_w > 0 {
                    spans.push(Span::styled(
                        format!("  {}", truncate_middle(&text.path, path_w)),
                        Style::default().fg(p.text_subtle),
                    ));
                }
            }
            (PickerRow::Folder(_), None) => {}
        }
        f.render_widget(Paragraph::new(Line::from(spans)), rect);
        hits.push((rect, Hit::FolderRow(index)));
    }

    // The message, then the keys.
    let message_row = row_at(inner.height.saturating_sub(2));
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
    match &picker.stage {
        PickerStage::Browse => {
            let message = if picker.busy {
                Span::styled(format!("{} Adding…", get_spinner_char()), key)
            } else if let Some(text) = &picker.error {
                Span::styled(
                    truncate(&format!("✕ {text}"), inner_width),
                    Style::default().fg(p.accent_pink),
                )
            } else if picker.folder_note.is_some() && !picker.rows.is_empty() {
                Span::styled(
                    truncate(
                        picker.folder_note.as_deref().unwrap_or_default(),
                        inner_width,
                    ),
                    muted,
                )
            } else if picker.more_folders > 0 {
                let noun = if picker.more_folders == 1 {
                    "folder matches"
                } else {
                    "folders match"
                };
                Span::styled(
                    truncate(
                        &format!("{} more {noun}; type more of the name", picker.more_folders),
                        inner_width,
                    ),
                    muted,
                )
            } else {
                Span::raw("")
            };
            f.render_widget(Paragraph::new(message), message_row);
            let enter = enter_action(picker, app);
            // While suggestions complete a typed path, ↑↓ choose one and Tab
            // puts it in; otherwise Tab and Space tick.
            let on_typed_folder = picker.typed_folder && picker.cursor == 0;
            let tab = match picker.current_row() {
                Some(PickerRow::Folder(_)) if !on_typed_folder => "tick",
                Some(PickerRow::All) => "tick",
                Some(_) => "complete",
                None => "tick",
            };
            // Space types in a path, so only Tab ticks there.
            let tab_key = if tab == "tick" && !looks_like_path(picker.query.value()) {
                "Tab/Space"
            } else {
                "Tab"
            };
            let mut pairs: Vec<(&str, &str)> = vec![("⏎", enter.as_str()), (tab_key, tab)];
            if picker.has_suggestions() {
                pairs.push(("↑↓", "select"));
            }
            pairs.push(("Esc", "close"));
            // On a narrow screen (a phone) Enter and Esc stay: Space goes
            // first, then ↑↓, then Tab.
            let fits = |pairs: &[(&str, &str)]| keys(pairs).width() <= usize::from(keys_row.width);
            if !fits(&pairs) && pairs[1].0 == "Tab/Space" {
                pairs[1].0 = "Tab";
            }
            if !fits(&pairs) {
                pairs.retain(|(k, _)| *k != "↑↓");
            }
            if !fits(&pairs) {
                pairs.retain(|(k, _)| *k != "Tab");
            }
            f.render_widget(Paragraph::new(keys(&pairs)), keys_row);
        }
        PickerStage::Creating(path) => {
            let message = if picker.busy {
                Span::styled(format!("{} Creating…", get_spinner_char()), key)
            } else {
                Span::styled(
                    truncate_middle(
                        &format!("Create {} and show it?", abbreviate_home(path)),
                        inner_width,
                    ),
                    Style::default().fg(p.accent_gold),
                )
            };
            f.render_widget(Paragraph::new(message), message_row);
            f.render_widget(
                Paragraph::new(keys(&[("y", "create"), ("n/Esc", "back")])),
                keys_row,
            );
        }
    }
    hits.push((popup, Hit::FolderMenu));
}
