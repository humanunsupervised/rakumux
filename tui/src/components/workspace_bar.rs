//! The workspaces' buttons at the bottom of the left column's Workspaces tab
//! (`app/workspaces.rs`): `⊞` (the workspaces modal), All, each workspace and
//! No workspace, each with its sessions' count, the one shown lit. They wrap
//! to as many rows as the column spares; past those, the one shown and as
//! many others as fit stay beside `+N`, which opens the modal.

use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Paragraph},
    Frame,
};

use super::footer::{workspace_tabs, WorkspaceTab};
use crate::app::{App, FooterKey, Hit};
use crate::layout::str_width;
use crate::theme;

/// Columns left of the first button, and kept right of the last.
const INDENT: usize = 1;
/// Columns between two buttons.
const GAP: usize = 1;

/// One button: what it says (a tab's name, then its count), and what a click
/// on it does.
struct Chip {
    name: String,
    count: Option<usize>,
    selected: bool,
    hit: Hit,
}

impl Chip {
    /// ` name count `.
    fn width(&self) -> usize {
        let count = self
            .count
            .map_or(0, |count| str_width(&count.to_string()) + 1);
        str_width(&self.name) + count + 2
    }
}

/// The chips in rows at most `cols` wide.
fn rows(chips: Vec<Chip>, cols: usize) -> Vec<Vec<Chip>> {
    let mut rows: Vec<Vec<Chip>> = Vec::new();
    let mut used = 0;
    for chip in chips {
        let width = chip.width();
        match rows.last_mut() {
            Some(row) if used + GAP + width <= cols => {
                used += GAP + width;
                row.push(chip);
            }
            _ => {
                used = width;
                rows.push(vec![chip]);
            }
        }
    }
    rows
}

/// The buttons for `cols` columns and at most `most` rows, as rows.
fn fit(tabs: &[WorkspaceTab], icon: &'static str, cols: usize, most: usize) -> Vec<Vec<Chip>> {
    let chips = |kept: &[usize]| -> Vec<Chip> {
        let mut chips = vec![Chip {
            name: icon.to_string(),
            count: None,
            selected: false,
            hit: Hit::FooterKey(FooterKey::Workspace),
        }];
        chips.extend(kept.iter().map(|index| {
            let tab = &tabs[*index];
            Chip {
                name: tab.name.clone(),
                count: Some(tab.count),
                selected: tab.selected,
                hit: Hit::WorkspaceTab(*index),
            }
        }));
        let hidden = tabs.len() - kept.len();
        if hidden > 0 {
            chips.push(Chip {
                name: format!("+{hidden}"),
                count: None,
                selected: false,
                hit: Hit::FooterKey(FooterKey::Workspace),
            });
        }
        chips
    };
    let mut kept: Vec<usize> = (0..tabs.len()).collect();
    loop {
        let laid = rows(chips(&kept), cols);
        if laid.len() <= most {
            return laid;
        }
        // The last one that isn't shown makes way.
        match kept.iter().rposition(|index| !tabs[*index].selected) {
            Some(last) => {
                kept.remove(last);
            }
            None => return laid.into_iter().take(most).collect(),
        }
    }
}

/// Rows the buttons may take of a view `height` rows tall.
fn most_rows(height: u16) -> usize {
    match height {
        0..=7 => 0,
        8..=23 => 1,
        24..=35 => 2,
        _ => 3,
    }
}

/// Draw the buttons at the bottom of `area` and record their targets; the
/// rest of `area` is what's left for the sessions (all of it with a daemon
/// without workspaces).
pub fn draw_workspace_bar(
    f: &mut Frame,
    app: &App,
    area: Rect,
    hits: &mut Vec<(Rect, Hit)>,
) -> Rect {
    if !app.has_workspaces() {
        return area;
    }
    let cols = usize::from(area.width).saturating_sub(2 * INDENT);
    let icon = app.icons().workspace();
    let laid = fit(&workspace_tabs(app), icon, cols, most_rows(area.height));
    let height = (laid.len() as u16).min(area.height);
    let (body, bar) = (
        Rect {
            height: area.height - height,
            ..area
        },
        Rect {
            y: area.bottom() - height,
            height,
            ..area
        },
    );
    let p = theme::palette();
    f.render_widget(Block::default().style(Style::default().bg(p.panel)), bar);
    for (index, row) in laid.into_iter().enumerate() {
        let y = bar.y + index as u16;
        let mut x = area.x + INDENT as u16;
        for chip in row {
            let width = (chip.width() as u16).min(area.right().saturating_sub(x));
            let cell = Rect {
                x,
                y,
                width,
                height: 1,
            };
            let base = if chip.selected {
                Style::default()
                    .fg(p.background)
                    .bg(p.accent_primary)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(p.text_primary).bg(p.surface)
            };
            let mut spans = vec![Span::styled(format!(" {}", chip.name), base)];
            if let Some(count) = chip.count {
                let style = if chip.selected {
                    base
                } else {
                    base.fg(p.text_muted)
                };
                spans.push(Span::styled(format!(" {count}"), style));
            }
            spans.push(Span::styled(" ", base));
            f.render_widget(Paragraph::new(Line::from(spans)), cell);
            hits.push((cell, chip.hit));
            x += width + GAP as u16;
        }
    }
    body
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tabs(names: &[&str], selected: usize) -> Vec<WorkspaceTab> {
        names
            .iter()
            .enumerate()
            .map(|(index, name)| WorkspaceTab {
                name: name.to_string(),
                count: index,
                selected: index == selected,
            })
            .collect()
    }

    fn text(laid: &[Vec<Chip>]) -> Vec<String> {
        laid.iter()
            .map(|row| {
                row.iter()
                    .map(|chip| match chip.count {
                        Some(count) => format!("{} {count}", chip.name),
                        None => chip.name.clone(),
                    })
                    .collect::<Vec<_>>()
                    .join("|")
            })
            .collect()
    }

    #[test]
    fn the_buttons_wrap_then_the_one_shown_stays() {
        let names = ["All", "Launch", "Infrastructure", "Docs", "No workspace"];
        let all = tabs(&names, 2);
        assert_eq!(
            text(&fit(&all, "⊞", 46, 3)),
            ["⊞|All 0|Launch 1|Infrastructure 2", "Docs 3|No workspace 4"]
        );
        let one = text(&fit(&all, "⊞", 46, 1));
        assert_eq!(one, ["⊞|All 0|Launch 1|Infrastructure 2|+2"]);
        let narrow = text(&fit(&tabs(&names, 4), "⊞", 26, 1));
        assert_eq!(narrow, ["⊞|No workspace 4|+4"]);
        // No row is wider than it may be.
        for cols in 26..80 {
            for most in 1..=3 {
                let laid = fit(&all, "⊞", cols, most);
                assert!(laid.len() <= most);
                for row in &laid {
                    let width: usize =
                        row.iter().map(Chip::width).sum::<usize>() + GAP * (row.len() - 1);
                    assert!(width <= cols, "{cols} {most}");
                }
                assert!(laid.iter().flatten().any(|chip| chip.selected));
            }
        }
    }
}
