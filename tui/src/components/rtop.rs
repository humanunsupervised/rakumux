//! rtop (`app/rtop.rs`) in the drawer, under its band, drawn as btop does:
//! first the machine over the last few minutes (`cpu`, `mem` and `net`
//! panels of braille graphs, Rakumux's own share in the accent, the current
//! values in their borders; download up, upload down), then a row for this
//! window, the server, the other clients and each session (its processes
//! under it), each with a graph of the column the sessions are sorted by
//! (marked `▾`), their total pinned under them, and the selected row's
//! `cpu`, `mem` and `net` over time in a panel at the bottom.
//!
//! Narrower, the panels stack (cpu over mem and net) and shrink, the rows'
//! graphs go, then the pid, then sent and received merge into one network
//! column, then it goes. Shorter, the selected row's panel goes first, then
//! the machine's panels become a line a metric, then a line of numbers.

mod graph;

use ratatui::{
    layout::{Alignment, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{
        block::{Position, Title},
        Block, BorderType, Borders, Paragraph, Wrap,
    },
    Frame,
};

use crate::app::rtop::history::{Metric, Series};
use crate::app::rtop::{MachineSummary, RowKey, RtopRow, RtopSort, RtopView, Usage};
use crate::app::{App, Hit};
use crate::components::dialogs::{error_line, text_rows};
use crate::components::utils::get_spinner_char;
use crate::layout::{str_width, truncate};
use crate::theme;
use graph::{peak, Graph};

const PID_COLS: usize = 7;
const CPU_COLS: usize = 7;
const MEMORY_COLS: usize = 9;
const NET_COLS: usize = 11;
/// Columns the name keeps before others go.
const NAME_MIN_COLS: usize = 18;
/// The CPU and memory bars on the machine's line (the shortest overview).
const BAR_CELLS: usize = 10;
/// A row's graph: narrower and it goes; wider only spreads it over more
/// samples, up to this.
const SPARK_MIN: usize = 6;
const SPARK_MAX: usize = 40;
/// From this width the machine's panels sit side by side.
const PANELS_SIDE_BY_SIDE: u16 = 90;
/// Rows the table needs: its header, four rows, the rule and the total.
const TABLE_MIN: u16 = 7;
/// Rows the table keeps besides those before the selected row's panel shows.
const LIST_ROOM: u16 = 4;
/// A metric line's label (`cpu `) and value columns.
const LABEL_COLS: usize = 4;
const VALUE_COLS: usize = 11;
/// Network graphs' smallest top: a quiet line stays low.
const NET_FLOOR: f64 = 1024.0;
/// A row's CPU graph's smallest top, in percent of one CPU.
const CPU_FLOOR: f64 = 10.0;

/// Which columns fit, and the row graphs' cells (0: none).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Columns {
    spark: usize,
    pid: bool,
    /// Received and sent apart, or together.
    net_split: bool,
    net: bool,
}

impl Columns {
    /// The row graphs go first, then the pid, then the network columns
    /// merge, then go.
    fn for_width(width: usize) -> Self {
        let all = Columns {
            spark: 0,
            pid: true,
            net_split: true,
            net: true,
        };
        let spare = width.saturating_sub(NAME_MIN_COLS + all.fixed());
        if spare > SPARK_MIN {
            return Columns {
                spark: (spare / 2).clamp(SPARK_MIN, SPARK_MAX),
                ..all
            };
        }
        let fits = |c: Columns| NAME_MIN_COLS + c.fixed() <= width;
        [
            all,
            Columns { pid: false, ..all },
            Columns {
                pid: false,
                net_split: false,
                ..all
            },
        ]
        .into_iter()
        .find(|c| fits(*c))
        .unwrap_or(Columns {
            spark: 0,
            pid: false,
            net_split: false,
            net: false,
        })
    }

    /// Columns besides the name's (a row graph has a space before it).
    fn fixed(self) -> usize {
        let net = match (self.net, self.net_split) {
            (false, _) => 0,
            (true, true) => 2 * NET_COLS,
            (true, false) => NET_COLS,
        };
        let spark = if self.spark > 0 { self.spark + 1 } else { 0 };
        spark + CPU_COLS + MEMORY_COLS + net + if self.pid { PID_COLS } else { 0 }
    }
}

/// How the machine is shown over the rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Overview {
    /// `cpu`, `mem` and `net` panels, `graph` rows inside, stacked (cpu
    /// over mem and net) or side by side.
    Panels { stacked: bool, graph: u16 },
    /// A line a metric: its label, graph and value.
    Lines,
    /// The numbers alone.
    Text,
}

impl Overview {
    /// What fits `width` × `height`: the panels take up to about two
    /// fifths of the height, as long as the table keeps its rows.
    fn for_size(width: u16, height: u16) -> Self {
        let stacked = width < PANELS_SIDE_BY_SIDE;
        let (panel_rows, most) = if stacked { (2, 4) } else { (1, 6) };
        let graph = ((height * 2 / 5) / panel_rows).saturating_sub(2).min(most);
        if graph >= 2 && panel_rows * (graph + 2) + 1 + TABLE_MIN <= height {
            return Overview::Panels { stacked, graph };
        }
        if 3 + 1 + TABLE_MIN <= height {
            return Overview::Lines;
        }
        Overview::Text
    }
}

/// Rows a metric takes in the selected row's panel when `rows` are left
/// under the overview: 2 (download up, upload down), 1, or no panel.
fn detail_rows(rows: u16) -> u16 {
    let left = rows.saturating_sub(TABLE_MIN + LIST_ROOM);
    if left >= 2 + 3 * 2 {
        2
    } else if left >= 2 + 3 {
        1
    } else {
        0
    }
}

/// rtop in `area` (the drawer under its band). Records a target per row
/// shown; the selected row is highlighted while rtop has the keys.
pub fn draw_rtop(f: &mut Frame, app: &App, area: Rect, hits: &mut Vec<(Rect, Hit)>) {
    let p = theme::palette();
    let view = &app.rtop;
    // A row between the band and what's under it, a column each side, so
    // the names start under the band's text.
    let body = Rect {
        x: area.x.saturating_add(1),
        y: area.y.saturating_add(1),
        width: area.width.saturating_sub(2),
        height: area.height.saturating_sub(1),
    };
    if body.height < 2 || body.width < 8 {
        return;
    }
    let width = usize::from(body.width);
    let reading = || {
        vec![Line::from(Span::styled(
            format!("{} Reading…", get_spinner_char()),
            Style::default().fg(p.accent_primary),
        ))]
    };
    if let Some(error) = &view.error {
        f.render_widget(
            Paragraph::new(vec![error_line(error)]).wrap(Wrap { trim: true }),
            body,
        );
        return;
    }
    if view.history.machine().is_empty() {
        f.render_widget(Paragraph::new(reading()), body);
        return;
    }

    let overview = Overview::for_size(body.width, body.height);
    let overview_rows = match overview {
        Overview::Panels { stacked, graph } => {
            let rows = if stacked { 2 * (graph + 2) } else { graph + 2 };
            draw_panels(
                f,
                view,
                Rect {
                    height: rows,
                    ..body
                },
                stacked,
                graph,
            );
            rows
        }
        Overview::Lines => {
            let lines = metric_lines(view.history.machine(), Scope::Machine(view), width, 1);
            f.render_widget(Paragraph::new(lines), Rect { height: 3, ..body });
            3
        }
        Overview::Text => {
            let summary = match view.machine() {
                Some(machine) => machine_lines(&machine, width),
                None => reading(),
            };
            let rows = text_rows(&summary, body.width).min(body.height / 3).max(1);
            f.render_widget(
                Paragraph::new(summary).wrap(Wrap { trim: true }),
                Rect {
                    height: rows,
                    ..body
                },
            );
            rows
        }
    };
    let header_at = body.y + overview_rows + 1;
    if header_at + 4 > body.bottom() {
        return;
    }
    if !view.loaded() {
        // Back on screen: the graphs of before while the first sample comes.
        f.render_widget(
            Paragraph::new(reading()),
            Rect {
                y: header_at,
                height: 1,
                ..body
            },
        );
        return;
    }

    let rows = app.rtop_rows();
    let selected = view.selected_index(&rows);
    let per_metric = if rows.is_empty() {
        0
    } else {
        detail_rows(body.bottom() - header_at)
    };
    let detail_height = if per_metric > 0 {
        2 + 3 * per_metric
    } else {
        0
    };
    let table_bottom = body.bottom() - detail_height;
    let columns = Columns::for_width(width);
    f.render_widget(
        Paragraph::new(header_line(columns, width, view.sort)),
        Rect {
            y: header_at,
            height: 1,
            ..body
        },
    );
    // The total and the rule over it stay under the rows.
    let list = Rect {
        y: header_at + 1,
        height: table_bottom.saturating_sub(header_at + 1 + 2),
        ..body
    };
    let highlighted = app.rtop_has_keys().then_some(selected);
    let lines = row_lines(view, &rows, highlighted, columns, width);
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
    let total_spark = spark(Some(view.history.total()), view.sort, columns.spark, None);
    let total = Line::from(cells(
        "  Total",
        "",
        None,
        &RtopView::total(&rows),
        total_spark,
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

    if let Some(row) = rows.get(selected).filter(|_| per_metric > 0) {
        draw_detail(
            f,
            view,
            row,
            Rect {
                y: table_bottom,
                height: detail_height,
                ..body
            },
            per_metric,
        );
    }
}

/// What a set of metric lines shows: the machine (CPU of every CPU, memory
/// of its total) or a row (CPU of one CPU, its memory).
#[derive(Clone, Copy)]
enum Scope<'a> {
    Machine(&'a RtopView),
    Row,
}

/// A graph's colour by level, as rtop colours CPU: half a CPU orange, a
/// CPU or more pink, below that green.
fn cpu_color(percent_of_one: f64) -> Color {
    let p = theme::palette();
    if percent_of_one >= 100.0 {
        p.accent_pink
    } else if percent_of_one >= 50.0 {
        p.accent_orange
    } else {
        p.accent_green
    }
}

/// A share of the whole machine (its CPUs, its memory): half of it orange,
/// four fifths pink.
fn share_color(fraction: f64) -> Color {
    let p = theme::palette();
    if fraction >= 0.8 {
        p.accent_pink
    } else if fraction >= 0.5 {
        p.accent_orange
    } else {
        p.accent_green
    }
}

fn memory_color(_: f64) -> Color {
    theme::palette().accent_purple
}

fn rx_color(_: f64) -> Color {
    theme::palette().label_cyan
}

fn tx_color(_: f64) -> Color {
    theme::palette().accent_gold
}

/// A panel's frame with titles in its corners, each only while it fits
/// beside the one before it on its border.
fn panel(
    width: u16,
    top: [Option<Line<'static>>; 2],
    bottom: [Option<Line<'static>>; 2],
) -> Block<'static> {
    let p = theme::palette();
    let mut block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(p.border_dim));
    let room = usize::from(width).saturating_sub(2);
    for (titles, position) in [(top, Position::Top), (bottom, Position::Bottom)] {
        let [left, right] = titles;
        let mut used = 0;
        if let Some(left) = left.filter(|l| l.width() <= room) {
            used = left.width() + 1;
            block = block.title(Title::from(left).position(position));
        }
        if let Some(right) = right.filter(|r| used + r.width() <= room) {
            block = block.title(
                Title::from(right)
                    .position(position)
                    .alignment(Alignment::Right),
            );
        }
    }
    block
}

/// ` name ` for a panel's border.
fn name_title(name: &str) -> Line<'static> {
    let p = theme::palette();
    Line::from(Span::styled(
        format!(" {name} "),
        Style::default()
            .fg(p.text_bright)
            .add_modifier(Modifier::BOLD),
    ))
}

/// ` value ` for a panel's border, in `color`.
fn value_title(value: String, color: Color) -> Line<'static> {
    Line::from(Span::styled(
        format!(" {value} "),
        Style::default().fg(color),
    ))
}

/// `rakumux value` for a panel's border: the overlay's legend.
fn rakumux_title(value: String) -> Line<'static> {
    let p = theme::palette();
    Line::from(vec![
        Span::raw(" "),
        Span::styled("rakumux ", Style::default().fg(p.accent_primary)),
        Span::styled(format!("{value} "), Style::default().fg(p.text_primary)),
    ])
}

/// The machine's `cpu`, `mem` and `net` panels in `area`.
fn draw_panels(f: &mut Frame, view: &RtopView, area: Rect, stacked: bool, graph: u16) {
    let p = theme::palette();
    let history = &view.history;
    let machine = history.machine();
    let total = history.total();
    let now = |series: &Series, metric: Metric| series.last().and_then(|p| p.get(metric));
    let cpus = f64::from(history.cpus.max(1));
    let panel_height = graph + 2;
    let (cpu_area, mem_area, net_area) = if stacked {
        let half = area.width / 2;
        let row = Rect {
            y: area.y + panel_height,
            height: panel_height,
            ..area
        };
        (
            Rect {
                height: panel_height,
                ..area
            },
            Rect { width: half, ..row },
            Rect {
                x: row.x + half,
                width: row.width - half,
                ..row
            },
        )
    } else {
        let cpu = area.width * 2 / 5;
        let mem = (area.width - cpu) / 2;
        (
            Rect { width: cpu, ..area },
            Rect {
                x: area.x + cpu,
                width: mem,
                ..area
            },
            Rect {
                x: area.x + cpu + mem,
                width: area.width - cpu - mem,
                ..area
            },
        )
    };
    let graph_rows = usize::from(graph);

    // CPU: every CPU together, Rakumux's share of them under it.
    let block = panel(
        cpu_area.width,
        [
            Some(name_title("cpu")),
            Some(value_title(
                percent(now(machine, Metric::Cpu)),
                p.text_bright,
            )),
        ],
        [
            Some(rakumux_title(percent(
                now(total, Metric::Cpu).map(|c| c / cpus),
            ))),
            None,
        ],
    );
    let inner = block.inner(cpu_area);
    f.render_widget(block, cpu_area);
    let n = usize::from(inner.width) * 2;
    let rakumux: Vec<Option<f64>> = total
        .values(Metric::Cpu, n)
        .into_iter()
        .map(|c| c.map(|c| c / cpus))
        .collect();
    let lines = Graph {
        values: &machine.values(Metric::Cpu, n),
        max: 100.0,
        down: false,
        color: &|v| share_color(v / 100.0),
        overlay: Some((&rakumux, p.accent_primary)),
    }
    .lines(usize::from(inner.width), graph_rows);
    f.render_widget(Paragraph::new(to_lines(lines)), inner);

    // Memory: used of the machine's, Rakumux's under it.
    let used = now(machine, Metric::Memory);
    let used_text = |with_total: bool| match (used, history.memory_total) {
        (Some(used), Some(of)) if with_total => {
            format!("{}/{}", bytes(used as u64), bytes(of))
        }
        (Some(used), _) => bytes(used as u64),
        (None, _) => "—".into(),
    };
    let room = usize::from(mem_area.width).saturating_sub(2 + 6);
    let value = Some(used_text(true))
        .filter(|v| v.chars().count() + 2 <= room)
        .unwrap_or_else(|| used_text(false));
    let block = panel(
        mem_area.width,
        [
            Some(name_title("mem")),
            Some(value_title(value, p.text_bright)),
        ],
        [
            Some(rakumux_title(
                now(total, Metric::Memory).map_or_else(|| "—".into(), |m| bytes(m as u64)),
            )),
            None,
        ],
    );
    let inner = block.inner(mem_area);
    f.render_widget(block, mem_area);
    let n = usize::from(inner.width) * 2;
    let values = machine.values(Metric::Memory, n);
    let max = history
        .memory_total
        .map(|t| t as f64)
        .unwrap_or_else(|| peak(&values, 1.0));
    let lines = Graph {
        values: &values,
        max,
        down: false,
        color: &|v| share_color(v / max),
        overlay: Some((&total.values(Metric::Memory, n), p.accent_primary)),
    }
    .lines(usize::from(inner.width), graph_rows);
    f.render_widget(Paragraph::new(to_lines(lines)), inner);

    // Network: received up, sent down, on one scale.
    let block = panel(
        net_area.width,
        [
            Some(name_title("net")),
            Some(value_title(
                format!("↓ {}", rate(now(machine, Metric::Rx))),
                p.label_cyan,
            )),
        ],
        [
            None,
            Some(value_title(
                format!("↑ {}", rate(now(machine, Metric::Tx))),
                p.accent_gold,
            )),
        ],
    );
    let inner = block.inner(net_area);
    f.render_widget(block, net_area);
    let n = usize::from(inner.width) * 2;
    let (rx, tx) = (machine.values(Metric::Rx, n), machine.values(Metric::Tx, n));
    let max = peak(&rx, NET_FLOOR).max(peak(&tx, NET_FLOOR));
    let up = graph_rows.div_ceil(2);
    let mut lines = Graph {
        values: &rx,
        max,
        down: false,
        color: &rx_color,
        overlay: Some((&total.values(Metric::Rx, n), p.accent_primary)),
    }
    .lines(usize::from(inner.width), up);
    lines.extend(
        Graph {
            values: &tx,
            max,
            down: true,
            color: &tx_color,
            overlay: Some((&total.values(Metric::Tx, n), p.accent_primary)),
        }
        .lines(usize::from(inner.width), graph_rows - up),
    );
    f.render_widget(Paragraph::new(to_lines(lines)), inner);
}

fn to_lines(rows: Vec<Vec<Span<'static>>>) -> Vec<Line<'static>> {
    rows.into_iter().map(Line::from).collect()
}

/// The selected row's panel: its `cpu`, `mem` and `net` over time.
fn draw_detail(f: &mut Frame, view: &RtopView, row: &RtopRow, area: Rect, per_metric: u16) {
    let p = theme::palette();
    let room = usize::from(area.width).saturating_sub(4);
    // Rakumux's own processes share names: which one it is comes too.
    let name = match row.key {
        RowKey::Daemon | RowKey::Client(_) if !row.detail.is_empty() => {
            format!("{} · {}", row.name, row.detail)
        }
        _ => row.name.clone(),
    };
    let pid = row
        .pid
        .map(|pid| value_title(pid.to_string(), p.text_muted));
    let block = panel(
        area.width,
        [Some(name_title(&truncate(&name, room))), pid],
        [None, None],
    );
    let inner = block.inner(area);
    f.render_widget(block, area);
    let series = view.history.row(&row.key);
    let lines = match series {
        Some(series) => metric_lines(
            series,
            Scope::Row,
            usize::from(inner.width),
            usize::from(per_metric),
        ),
        None => metric_lines(
            &Series::default(),
            Scope::Row,
            usize::from(inner.width),
            usize::from(per_metric),
        ),
    };
    f.render_widget(Paragraph::new(lines), inner);
}

/// `cpu`, `mem` and `net` of `series`, each a label, a graph `per` rows
/// tall and the newest value, in `width` columns. Network two rows or more
/// tall is received up and sent down; one row, both together.
fn metric_lines(series: &Series, scope: Scope, width: usize, per: usize) -> Vec<Line<'static>> {
    let p = theme::palette();
    // Very narrow, the value keeps what's left after the label.
    let label_cols = LABEL_COLS.min(width);
    let value_cols = VALUE_COLS.min(width - label_cols);
    let graph_cols = width.saturating_sub(label_cols + 1 + value_cols);
    let graph_cols = if graph_cols >= 4 { graph_cols } else { 0 };
    let n = graph_cols * 2;
    let now = |metric: Metric| series.last().and_then(|p| p.get(metric));
    let mut lines = Vec::with_capacity(3 * per);
    let mut put = |label: &str, rows: Vec<Vec<Span<'static>>>, values: Vec<(String, Color)>| {
        for (index, row) in rows.into_iter().enumerate() {
            let label = if index == 0 { label } else { "" };
            let mut spans = vec![Span::styled(
                pad_right(&truncate(label, label_cols), label_cols),
                Style::default().fg(p.text_muted),
            )];
            spans.extend(row);
            let (value, color) = values.get(index).cloned().unwrap_or_default();
            let gap = width.saturating_sub(label_cols + graph_cols + value_cols);
            spans.push(Span::raw(" ".repeat(gap)));
            spans.push(Span::styled(
                pad_left(&truncate(&value, value_cols), value_cols),
                Style::default().fg(color),
            ));
            lines.push(Line::from(spans));
        }
    };
    let blank = |rows: usize| vec![Vec::new(); rows];
    let draw = |graph: Graph, rows: usize| {
        if graph_cols == 0 {
            blank(rows)
        } else {
            graph.lines(graph_cols, rows)
        }
    };

    let cpu = series.values(Metric::Cpu, n);
    let cpu_rows = match scope {
        Scope::Machine(_) => draw(
            Graph {
                values: &cpu,
                max: 100.0,
                down: false,
                color: &|v| share_color(v / 100.0),
                overlay: None,
            },
            per,
        ),
        Scope::Row => draw(
            Graph {
                values: &cpu,
                max: peak(&cpu, CPU_FLOOR),
                down: false,
                color: &cpu_color,
                overlay: None,
            },
            per,
        ),
    };
    put(
        "cpu",
        cpu_rows,
        vec![(percent(now(Metric::Cpu)), p.text_bright)],
    );

    let memory = series.values(Metric::Memory, n);
    let total = match scope {
        Scope::Machine(view) => view.history.memory_total.map(|t| t as f64),
        Scope::Row => None,
    };
    let max = total.unwrap_or_else(|| peak(&memory, 1.0));
    let memory_rows = draw(
        Graph {
            values: &memory,
            max,
            down: false,
            color: &|v| match total {
                Some(total) => share_color(v / total),
                None => memory_color(v),
            },
            overlay: None,
        },
        per,
    );
    put(
        "mem",
        memory_rows,
        vec![(
            now(Metric::Memory).map_or_else(|| "—".into(), |m| bytes(m as u64)),
            p.text_bright,
        )],
    );

    if per >= 2 {
        let (rx, tx) = (series.values(Metric::Rx, n), series.values(Metric::Tx, n));
        let max = peak(&rx, NET_FLOOR).max(peak(&tx, NET_FLOOR));
        let up = per.div_ceil(2);
        let mut rows = draw(
            Graph {
                values: &rx,
                max,
                down: false,
                color: &rx_color,
                overlay: None,
            },
            up,
        );
        rows.extend(draw(
            Graph {
                values: &tx,
                max,
                down: true,
                color: &tx_color,
                overlay: None,
            },
            per - up,
        ));
        let mut values = vec![(String::new(), p.text_bright); per];
        values[0] = (format!("↓ {}", rate(now(Metric::Rx))), p.label_cyan);
        values[up] = (format!("↑ {}", rate(now(Metric::Tx))), p.accent_gold);
        put("net", rows, values);
    } else {
        let net = series.values(Metric::Net, n);
        let rows = draw(
            Graph {
                values: &net,
                max: peak(&net, NET_FLOOR),
                down: false,
                color: &rx_color,
                overlay: None,
            },
            per,
        );
        put("net", rows, vec![(rate(now(Metric::Net)), p.text_bright)]);
    }
    lines
}

/// A row's graph of the metric the sessions are sorted by, `cells` wide,
/// one row tall: CPU (by name too) and network each on their own scale,
/// memory on `memory_max` (the largest of the rows) so rows compare.
fn spark(
    series: Option<&Series>,
    sort: RtopSort,
    cells: usize,
    memory_max: Option<f64>,
) -> Option<Vec<Span<'static>>> {
    if cells == 0 {
        return None;
    }
    let Some(series) = series else {
        return Some(vec![Span::raw(" ".repeat(cells))]);
    };
    let n = cells * 2;
    let row = match sort {
        RtopSort::Memory => {
            let values = series.values(Metric::Memory, n);
            let max = memory_max.unwrap_or_else(|| peak(&values, 1.0));
            Graph {
                values: &values,
                max,
                down: false,
                color: &memory_color,
                overlay: None,
            }
            .lines(cells, 1)
        }
        RtopSort::Cpu | RtopSort::Name => {
            let values = series.values(Metric::Cpu, n);
            Graph {
                values: &values,
                max: peak(&values, CPU_FLOOR),
                down: false,
                color: &cpu_color,
                overlay: None,
            }
            .lines(cells, 1)
        }
        RtopSort::Network => {
            let values = series.values(Metric::Net, n);
            Graph {
                values: &values,
                max: peak(&values, NET_FLOOR),
                down: false,
                color: &rx_color,
                overlay: None,
            }
            .lines(cells, 1)
        }
    };
    row.into_iter().next()
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

/// The columns' names, the one the sessions are sorted by with a `▾`; the
/// rows' graphs have none.
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
    if columns.spark > 0 {
        text.push_str(&" ".repeat(columns.spark + 1));
    }
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
    view: &RtopView,
    rows: &[RtopRow],
    selected: Option<usize>,
    columns: Columns,
    width: usize,
) -> Vec<(Option<usize>, Line<'static>)> {
    let p = theme::palette();
    // Memory graphs share one scale, so rows compare.
    let memory_max = (columns.spark > 0 && view.sort == RtopSort::Memory).then(|| {
        rows.iter()
            .filter_map(|row| view.history.row(&row.key))
            .map(|series| peak(&series.values(Metric::Memory, columns.spark * 2), 1.0))
            .fold(1.0, f64::max)
    });
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
            spark(
                view.history.row(&row.key),
                view.sort,
                columns.spark,
                memory_max,
            ),
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

/// A row's spans: the name and quieter detail in the name's columns, its
/// graph, then the numbers right-aligned in theirs.
#[allow(clippy::too_many_arguments)]
fn cells(
    name: &str,
    detail: &str,
    pid: Option<u32>,
    usage: &Usage,
    spark: Option<Vec<Span<'static>>>,
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
    if columns.spark > 0 {
        let graph = spark.unwrap_or_default();
        let drawn: usize = graph.iter().map(|s| str_width(&s.content)).sum();
        spans.extend(graph);
        spans.push(Span::raw(
            " ".repeat(columns.spark.saturating_sub(drawn) + 1),
        ));
    }
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
        assert!(wide.spark >= SPARK_MIN, "{wide:?}");
        assert_eq!(Columns::for_width(400).spark, SPARK_MAX);
        // The rows' graphs go before any number does.
        let numbers =
            Columns::for_width(NAME_MIN_COLS + PID_COLS + CPU_COLS + MEMORY_COLS + 2 * NET_COLS);
        assert!(numbers.spark == 0 && numbers.pid && numbers.net_split);
        let middle = Columns::for_width(NAME_MIN_COLS + CPU_COLS + MEMORY_COLS + 2 * NET_COLS);
        assert!(!middle.pid && middle.net_split);
        let phone = Columns::for_width(46);
        assert!(!phone.pid && !phone.net_split && phone.net);
        let tiny = Columns::for_width(36);
        assert!(!tiny.net);
    }

    #[test]
    fn the_overview_stacks_then_shrinks_and_the_detail_goes_first() {
        assert_eq!(
            Overview::for_size(120, 40),
            Overview::Panels {
                stacked: false,
                graph: 6
            }
        );
        assert_eq!(
            Overview::for_size(42, 43),
            Overview::Panels {
                stacked: true,
                graph: 4
            }
        );
        assert_eq!(
            Overview::for_size(42, 25),
            Overview::Panels {
                stacked: true,
                graph: 3
            }
        );
        assert_eq!(Overview::for_size(42, 15), Overview::Lines);
        assert_eq!(Overview::for_size(42, 9), Overview::Text);
        // Rows under the overview: the table's first, then the panel.
        assert_eq!(detail_rows(TABLE_MIN + LIST_ROOM + 4), 0);
        assert_eq!(detail_rows(TABLE_MIN + LIST_ROOM + 5), 1);
        assert_eq!(detail_rows(TABLE_MIN + LIST_ROOM + 8), 2);
    }

    #[test]
    fn metric_lines_fill_their_width_exactly() {
        use crate::app::rtop::history::{History, Point};
        let mut history = History::default();
        for second in 0..50 {
            let cpu = Some(f64::from(second % 7) * 20.0);
            history.record(
                Point::new(cpu, Some(1e9), Some(5e3), None),
                Some(4_000_000_000),
                4,
                Point::new(cpu, Some(1e8), None, None),
                &[],
            );
        }
        for width in [12, 20, 28, 42, 100] {
            for per in [1, 2, 3] {
                let lines = metric_lines(history.machine(), Scope::Row, width, per);
                assert_eq!(lines.len(), 3 * per);
                for line in &lines {
                    assert_eq!(line.width(), width, "{width}, {per}: {line:?}");
                }
            }
        }
        // Sent isn't known: its graph is empty, its value a dash.
        let lines = metric_lines(history.machine(), Scope::Row, 42, 2);
        let sent: String = lines[5].spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(sent.ends_with("↑ —"), "{sent}");
        assert!(
            !sent.chars().any(|c| ('\u{2801}'..='\u{28ff}').contains(&c)),
            "{sent}"
        );
    }

    #[test]
    fn a_row_fills_its_width_exactly() {
        for width in [36, 46, 60, 100, 160] {
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
                Some(vec![Span::raw("⣀⣤")]),
                columns,
                width,
                Style::default(),
            );
            let used: usize = spans.iter().map(|s| str_width(&s.content)).sum();
            assert_eq!(used, width, "{width}: {spans:?}");
        }
    }
}
