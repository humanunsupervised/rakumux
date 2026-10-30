//! rtop (`app/rtop.rs`) in the drawer, under its band: the machine's CPU,
//! memory and network, then a row for this window, the server, the other
//! clients and each session (its processes under it), and their total
//! pinned at the bottom. The column the sessions are sorted by has a `▾`.
//! Columns give way on narrow drawers: the pid first, then sent and
//! received merge into one network column, then it goes.

use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Paragraph, Wrap},
    Frame,
};

use crate::app::rtop::{MachineSummary, RowKey, RtopRow, RtopSort, RtopView, Usage};
use crate::app::{App, Hit};
use crate::components::dialogs::{error_line, text_rows};
use crate::components::utils::get_spinner_char;
use crate::layout::{str_width, truncate};
use crate::theme;

/// The widest rtop's columns get: wider only spreads them apart.
const MAX_COLS: u16 = 124;
const PID_COLS: usize = 7;
const CPU_COLS: usize = 7;
const MEMORY_COLS: usize = 9;
const NET_COLS: usize = 11;
/// Columns the name keeps before others go.
const NAME_MIN_COLS: usize = 18;
/// The CPU and memory bars on the machine's line.
const BAR_CELLS: usize = 10;

/// Which columns fit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Columns {
    pid: bool,
    /// Received and sent apart, or together.
    net_split: bool,
    net: bool,
}

impl Columns {
    fn for_width(width: usize) -> Self {
        let fits = |c: Columns| NAME_MIN_COLS + c.fixed() <= width;
        [
            Columns {
                pid: true,
                net_split: true,
                net: true,
            },
            Columns {
                pid: false,
                net_split: true,
                net: true,
            },
            Columns {
                pid: false,
                net_split: false,
                net: true,
            },
        ]
        .into_iter()
        .find(|c| fits(*c))
        .unwrap_or(Columns {
            pid: false,
            net_split: false,
            net: false,
        })
    }

    /// Columns besides the name's.
    fn fixed(self) -> usize {
        let net = match (self.net, self.net_split) {
            (false, _) => 0,
            (true, true) => 2 * NET_COLS,
            (true, false) => NET_COLS,
        };
        CPU_COLS + MEMORY_COLS + net + if self.pid { PID_COLS } else { 0 }
    }
}

/// rtop in `area` (the drawer under its band). Records a target per row
/// shown; the selected row is highlighted while rtop has the keys.
pub fn draw_rtop(f: &mut Frame, app: &App, area: Rect, hits: &mut Vec<(Rect, Hit)>) {
    let p = theme::palette();
    let view = &app.rtop;
    // A row between the band and the machine's line, a column each side, so
    // the names start under the band's text, their `▾` left of it.
    let body = Rect {
        x: area.x.saturating_add(1),
        y: area.y.saturating_add(1),
        width: area.width.saturating_sub(2).min(MAX_COLS),
        height: area.height.saturating_sub(1),
    };
    if body.height < 2 || body.width < 8 {
        return;
    }
    let width = usize::from(body.width);
    let columns = Columns::for_width(width);

    let summary = match (&view.error, view.machine()) {
        (Some(error), _) => vec![error_line(error)],
        (None, Some(machine)) => machine_lines(&machine, width),
        (None, None) => vec![Line::from(Span::styled(
            format!("{} Reading…", get_spinner_char()),
            Style::default().fg(p.accent_primary),
        ))],
    };
    let summary_rows = text_rows(&summary, body.width).min(body.height / 3).max(1);
    f.render_widget(
        Paragraph::new(summary).wrap(Wrap { trim: true }),
        Rect {
            height: summary_rows,
            ..body
        },
    );
    let header_at = body.y + summary_rows + 1;
    if !view.loaded() || header_at + 4 > body.bottom() {
        return;
    }

    let rows = app.rtop_rows();
    let selected = view.selected_index(&rows);
    f.render_widget(
        Paragraph::new(header_line(columns, width, view.sort)),
        Rect {
            y: header_at,
            height: 1,
            ..body
        },
    );
    // The total and the rule over it stay at the bottom.
    let list = Rect {
        y: header_at + 1,
        height: body.bottom().saturating_sub(header_at + 1 + 2),
        ..body
    };
    let highlighted = app.rtop_has_keys().then_some(selected);
    let lines = row_lines(&rows, highlighted, columns, width);
    let height = usize::from(list.height);
    view.page.set(height.max(1));
    let line_of_selected = lines
        .iter()
        .position(|(row, _)| *row == Some(selected))
        .unwrap_or(0);
    let mut first = view.first.get().min(lines.len().saturating_sub(height));
    if line_of_selected < first {
        first = line_of_selected;
    } else if line_of_selected >= first + height {
        first = line_of_selected + 1 - height;
    }
    view.first.set(first);
    let mut shown = Vec::with_capacity(height);
    for (offset, (row, line)) in lines.into_iter().skip(first).take(height).enumerate() {
        if let Some(index) = row {
            let cell = Rect {
                x: area.x,
                y: list.y + offset as u16,
                width: area.width,
                height: 1,
            };
            hits.push((cell, Hit::RtopRow(index)));
        }
        shown.push(line);
    }
    f.render_widget(Paragraph::new(shown), list);

    let rule_at = list.bottom();
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "─".repeat(width),
            Style::default().fg(p.border_dim),
        ))),
        Rect {
            y: rule_at,
            height: 1,
            ..body
        },
    );
    let total = Line::from(cells(
        "  Total",
        "",
        None,
        &RtopView::total(&rows),
        columns,
        width,
        Style::default()
            .fg(p.text_bright)
            .add_modifier(Modifier::BOLD),
    ));
    f.render_widget(
        Paragraph::new(total),
        Rect {
            y: rule_at + 1,
            height: 1,
            ..body
        },
    );
}

/// `CPU ▕██░░▏ 23% of 8   Memory ▕██░░▏ 9.1 GB of 31 GB   ↓ 1.2 MB/s  ↑ 40 KB/s`
/// on one line when it fits, else without the bars, the parts on lines of
/// their own as needed.
fn machine_lines(machine: &MachineSummary, width: usize) -> Vec<Line<'static>> {
    let parts = machine_parts(machine, true);
    let one_line: usize = parts.iter().map(|part| spans_width(part)).sum::<usize>()
        + GAP.len() * parts.len().saturating_sub(1);
    let parts = if one_line <= width {
        parts
    } else {
        machine_parts(machine, false)
    };
    let mut lines: Vec<Vec<Span<'static>>> = Vec::new();
    for part in parts {
        match lines.last_mut() {
            Some(line) if spans_width(line) + GAP.len() + spans_width(&part) <= width => {
                line.push(Span::raw(GAP));
                line.extend(part);
            }
            _ => lines.push(part),
        }
    }
    lines.into_iter().map(Line::from).collect()
}

/// Between the machine line's parts.
const GAP: &str = "   ";

fn spans_width(spans: &[Span]) -> usize {
    spans.iter().map(|s| str_width(&s.content)).sum()
}

/// The machine line's parts: CPU, memory, network.
fn machine_parts(machine: &MachineSummary, bars: bool) -> Vec<Vec<Span<'static>>> {
    let p = theme::palette();
    let label = Style::default().fg(p.text_muted);
    let value = Style::default().fg(p.text_bright);
    let bar = |fraction: Option<f64>| -> Vec<Span<'static>> {
        if !bars {
            return Vec::new();
        }
        let filled = fraction
            .map(|f| (f.clamp(0.0, 1.0) * BAR_CELLS as f64).round() as usize)
            .unwrap_or(0);
        vec![
            Span::styled("▕", Style::default().fg(p.border_dim)),
            Span::styled("█".repeat(filled), Style::default().fg(p.accent_primary)),
            Span::styled(
                "░".repeat(BAR_CELLS - filled),
                Style::default().fg(p.border_dim),
            ),
            Span::styled("▏ ", Style::default().fg(p.border_dim)),
        ]
    };
    let mut cpu = vec![Span::styled("CPU ", label)];
    cpu.extend(bar(machine.cpu.map(|c| c / 100.0)));
    cpu.push(Span::styled(percent(machine.cpu), value));
    cpu.push(Span::styled(format!(" of {}", machine.cpus), label));
    let mut parts = vec![cpu];
    if let (Some(used), Some(total)) = (machine.memory_used, machine.memory_total) {
        let mut memory = vec![Span::styled("Memory ", label)];
        memory.extend(bar((total > 0).then(|| used as f64 / total as f64)));
        memory.push(Span::styled(bytes(used), value));
        memory.push(Span::styled(format!(" of {}", bytes(total)), label));
        parts.push(memory);
    }
    if machine.rx.is_some() || machine.tx.is_some() {
        parts.push(vec![
            Span::styled("↓ ", label),
            Span::styled(rate(machine.rx), value),
            Span::styled("  ↑ ", label),
            Span::styled(rate(machine.tx), value),
        ]);
    }
    parts
}

/// The columns' names, the one the sessions are sorted by with a `▾`.
fn header_line(columns: Columns, width: usize, sort: RtopSort) -> Line<'static> {
    let p = theme::palette();
    let style = Style::default()
        .fg(p.text_muted)
        .add_modifier(Modifier::BOLD);
    let name = |label: &str, by: RtopSort| {
        if sort == by {
            format!("{label}▾")
        } else {
            label.to_string()
        }
    };
    let name_cols = width.saturating_sub(columns.fixed());
    let mut text = pad_right(&name("  NAME", RtopSort::Name), name_cols);
    if columns.pid {
        text.push_str(&pad_left("PID", PID_COLS));
    }
    text.push_str(&pad_left(&name("CPU", RtopSort::Cpu), CPU_COLS));
    text.push_str(&pad_left(&name("MEMORY", RtopSort::Memory), MEMORY_COLS));
    match (columns.net, columns.net_split) {
        (true, true) => {
            text.push_str(&pad_left(&name("↓ NET", RtopSort::Network), NET_COLS));
            text.push_str(&pad_left(&name("↑ NET", RtopSort::Network), NET_COLS));
        }
        (true, false) => text.push_str(&pad_left(&name("NET", RtopSort::Network), NET_COLS)),
        (false, _) => {}
    }
    Line::from(Span::styled(text, style))
}

/// Each row's line, with the row it shows; a blank line (no row) sets the
/// sessions apart from Rakumux's own processes.
fn row_lines(
    rows: &[RtopRow],
    selected: Option<usize>,
    columns: Columns,
    width: usize,
) -> Vec<(Option<usize>, Line<'static>)> {
    let p = theme::palette();
    let mut lines = Vec::with_capacity(rows.len() + 1);
    let mut sessions_started = false;
    for (index, row) in rows.iter().enumerate() {
        if matches!(row.key, RowKey::Session(_)) && !sessions_started {
            sessions_started = true;
            lines.push((None, Line::from("")));
        }
        let mark = match row.open {
            Some(true) => "▾ ",
            Some(false) => "▸ ",
            None => "  ",
        };
        let indent = format!("{mark}{}", "  ".repeat(row.depth));
        let name_style = match row.key {
            RowKey::Process(..) => Style::default().fg(p.text_primary),
            _ => Style::default().fg(p.text_bright),
        };
        let mut spans = cells(
            &format!("{indent}{}", row.name),
            &row.detail,
            row.pid,
            &row.usage,
            columns,
            width,
            name_style,
        );
        if Some(index) == selected {
            for span in &mut spans {
                span.style = span.style.bg(p.selection);
            }
            if let Some(name) = spans.first_mut() {
                name.style = name.style.add_modifier(Modifier::BOLD);
            }
        }
        lines.push((Some(index), Line::from(spans)));
    }
    lines
}

/// A row's spans: the name and quieter detail in the name's columns, then
/// the numbers right-aligned in theirs.
fn cells(
    name: &str,
    detail: &str,
    pid: Option<u32>,
    usage: &Usage,
    columns: Columns,
    width: usize,
    name_style: Style,
) -> Vec<Span<'static>> {
    let p = theme::palette();
    let muted = Style::default().fg(p.text_muted);
    let name_cols = width.saturating_sub(columns.fixed());
    let name_text = truncate(name, name_cols.saturating_sub(1));
    let room = name_cols.saturating_sub(str_width(&name_text) + 2);
    let detail_text = if detail.is_empty() || room < 4 {
        String::new()
    } else {
        format!("  {}", truncate(detail, room))
    };
    let used = str_width(&name_text) + str_width(&detail_text);
    let mut spans = vec![
        Span::styled(name_text, name_style),
        Span::styled(detail_text, muted),
        Span::raw(" ".repeat(name_cols.saturating_sub(used))),
    ];
    if columns.pid {
        let pid = pid.map(|pid| pid.to_string()).unwrap_or_default();
        spans.push(Span::styled(pad_left(&pid, PID_COLS), muted));
    }
    spans.push(Span::styled(
        pad_left(&percent(usage.cpu), CPU_COLS),
        cpu_style(usage.cpu),
    ));
    spans.push(Span::styled(
        pad_left(&bytes(usage.memory), MEMORY_COLS),
        Style::default().fg(p.text_primary),
    ));
    let net = Style::default().fg(p.text_primary);
    match (columns.net, columns.net_split) {
        (true, true) => {
            spans.push(Span::styled(pad_left(&rate(usage.rx), NET_COLS), net));
            spans.push(Span::styled(pad_left(&rate(usage.tx), NET_COLS), net));
        }
        (true, false) => {
            let both = match (usage.rx, usage.tx) {
                (None, None) => None,
                (rx, tx) => Some(rx.unwrap_or(0.0) + tx.unwrap_or(0.0)),
            };
            spans.push(Span::styled(pad_left(&rate(both), NET_COLS), net));
        }
        (false, _) => {}
    }
    spans
}

/// Busier is warmer: a CPU's worth or more pink, half of one orange.
fn cpu_style(cpu: Option<f64>) -> Style {
    let p = theme::palette();
    let color = match cpu {
        Some(c) if c >= 100.0 => p.accent_pink,
        Some(c) if c >= 50.0 => p.accent_orange,
        Some(_) => p.text_primary,
        None => p.text_muted,
    };
    Style::default().fg(color)
}

/// `0.4%`, `12%`; `…` until there's a rate.
fn percent(value: Option<f64>) -> String {
    match value {
        None => "…".into(),
        Some(v) if v < 9.95 => format!("{v:.1}%"),
        Some(v) => format!("{v:.0}%"),
    }
}

/// `512 B`, `12 KB`, `1.5 MB`, `31.2 GB` (powers of 1024).
fn bytes(n: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = n as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{n} B")
    } else if value < 9.95 {
        format!("{value:.1} {}", UNITS[unit])
    } else {
        format!("{value:.0} {}", UNITS[unit])
    }
}

/// [`bytes`] a second; `—` where it isn't known.
fn rate(value: Option<f64>) -> String {
    match value {
        None => "—".into(),
        Some(v) => format!("{}/s", bytes(v.max(0.0).round() as u64)),
    }
}

fn pad_left(text: &str, width: usize) -> String {
    format!(
        "{}{text}",
        " ".repeat(width.saturating_sub(str_width(text)))
    )
}

fn pad_right(text: &str, width: usize) -> String {
    format!(
        "{text}{}",
        " ".repeat(width.saturating_sub(str_width(text)))
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_and_rates_read_short() {
        assert_eq!(bytes(512), "512 B");
        assert_eq!(bytes(1536), "1.5 KB");
        assert_eq!(bytes(820 * 1024 * 1024), "820 MB");
        assert_eq!(bytes(31 * 1024 * 1024 * 1024 + 200 * 1024 * 1024), "31 GB");
        assert_eq!(rate(None), "—");
        assert_eq!(rate(Some(0.0)), "0 B/s");
        assert_eq!(rate(Some(4608.0)), "4.5 KB/s");
        assert_eq!(percent(None), "…");
        assert_eq!(percent(Some(0.42)), "0.4%");
        assert_eq!(percent(Some(45.5)), "46%");
        assert_eq!(percent(Some(9.96)), "10%");
    }

    #[test]
    fn the_machine_line_drops_its_bars_then_wraps_when_narrow() {
        let machine = MachineSummary {
            cpus: 8,
            cpu: Some(12.5),
            memory_used: Some(10 << 30),
            memory_total: Some(16 << 30),
            rx: Some(5000.0),
            tx: Some(500.0),
        };
        let text = |lines: &[Line]| -> Vec<String> {
            lines
                .iter()
                .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
                .collect()
        };
        let wide = machine_lines(&machine, 120);
        assert_eq!(wide.len(), 1);
        assert!(text(&wide)[0].contains('█'));
        let narrow = text(&machine_lines(&machine, 40));
        assert_eq!(
            narrow,
            [
                "CPU 12% of 8   Memory 10 GB of 16 GB",
                "↓ 4.9 KB/s  ↑ 500 B/s"
            ]
        );
    }

    #[test]
    fn columns_give_way_on_narrow_screens() {
        let wide = Columns::for_width(100);
        assert!(wide.pid && wide.net && wide.net_split);
        let middle = Columns::for_width(NAME_MIN_COLS + CPU_COLS + MEMORY_COLS + 2 * NET_COLS);
        assert!(!middle.pid && middle.net_split);
        let phone = Columns::for_width(46);
        assert!(!phone.pid && !phone.net_split && phone.net);
        let tiny = Columns::for_width(36);
        assert!(!tiny.net);
    }

    #[test]
    fn a_row_fills_its_width_exactly() {
        for width in [36, 46, 60, 100] {
            let columns = Columns::for_width(width);
            let usage = Usage {
                cpu: Some(123.4),
                memory: 5 << 30,
                rx: Some(1e6),
                tx: None,
            };
            let spans = cells(
                "a-very-long-session-name-that-will-not-fit",
                "claude · somewhere",
                Some(4_194_303),
                &usage,
                columns,
                width,
                Style::default(),
            );
            let used: usize = spans.iter().map(|s| str_width(&s.content)).sum();
            assert_eq!(used, width, "{width}: {spans:?}");
        }
    }
}
