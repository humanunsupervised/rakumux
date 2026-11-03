//! Braille graphs for rtop, as btop draws them: each cell two samples wide
//! and four dots tall, every sample a column of dots from the baseline, the
//! newest at the right edge. A known value, zero too, shows at least its
//! baseline dot; an unknown one (no rate yet, a gap, network on macOS)
//! shows nothing. Colours follow the height: each cell takes the colour of
//! the value its dots reach. An overlay (Rakumux's share of the machine)
//! colours the cells wholly under it.

use ratatui::{
    style::{Color, Style},
    text::Span,
};

/// Dots of a cell's left and right columns, from the bottom up.
const LEFT: [u8; 4] = [0x40, 0x04, 0x02, 0x01];
const RIGHT: [u8; 4] = [0x80, 0x20, 0x10, 0x08];

/// A graph of `values` (oldest first) on a scale from 0 to `max`.
pub struct Graph<'a> {
    pub values: &'a [Option<f64>],
    pub max: f64,
    /// Grows down from the top (upload, under download).
    pub down: bool,
    /// The colour of a value.
    pub color: &'a dyn Fn(f64) -> Color,
    /// Values drawn in its colour where they cover a cell.
    pub overlay: Option<(&'a [Option<f64>], Color)>,
}

/// The largest known value, at least `floor`: an auto-scaled graph's top.
pub fn peak(values: &[Option<f64>], floor: f64) -> f64 {
    values.iter().flatten().fold(floor, |a, b| a.max(*b))
}

/// The newest `slots` of `values`, unknown before the oldest.
fn newest(values: &[Option<f64>], slots: usize) -> Vec<Option<f64>> {
    let missing = slots.saturating_sub(values.len());
    std::iter::repeat_n(None, missing)
        .chain(values[values.len().saturating_sub(slots)..].iter().copied())
        .collect()
}

impl Graph<'_> {
    /// The graph `width` cells wide and `height` tall: its rows from the
    /// top, each as spans of `width` cells.
    pub fn lines(&self, width: usize, height: usize) -> Vec<Vec<Span<'static>>> {
        let dots = height * 4;
        let values = newest(self.values, width * 2);
        let levels: Vec<Option<usize>> = values
            .iter()
            .map(|v| v.map(|v| self.level(v, dots)))
            .collect();
        let overlay = self.overlay.map(|(values, color)| {
            let levels: Vec<Option<usize>> = newest(values, width * 2)
                .iter()
                .map(|v| v.map(|v| self.level(v, dots)))
                .collect();
            (levels, color)
        });
        let mut rows = Vec::with_capacity(height);
        for screen_row in 0..height {
            // Rows counted from the baseline.
            let row = if self.down {
                screen_row
            } else {
                height - 1 - screen_row
            };
            let in_row =
                |level: Option<usize>| level.map_or(0, |l| l.saturating_sub(row * 4).min(4));
            let mut cells: Vec<(char, Option<Color>)> = Vec::with_capacity(width);
            for cell in 0..width {
                let (a, b) = (cell * 2, cell * 2 + 1);
                let (left, right) = (in_row(levels[a]), in_row(levels[b]));
                if left == 0 && right == 0 {
                    cells.push((' ', None));
                    continue;
                }
                let mut bits = 0u8;
                for dot in 0..4 {
                    // `dot` counts from the baseline: the bottom, or the top.
                    let at = if self.down { 3 - dot } else { dot };
                    if dot < left {
                        bits |= LEFT[at];
                    }
                    if dot < right {
                        bits |= RIGHT[at];
                    }
                }
                let symbol = char::from_u32(0x2800 + u32::from(bits)).unwrap_or(' ');
                let covered = overlay.as_ref().and_then(|(levels, color)| {
                    (left <= in_row(levels[a]) && right <= in_row(levels[b])).then_some(*color)
                });
                let color = covered.unwrap_or_else(|| {
                    // The value these dots reach: the higher sample's, cut
                    // at the cell's top.
                    let top = self.max * ((row * 4 + 4) as f64) / dots as f64;
                    let reached = values[a]
                        .unwrap_or(0.0)
                        .max(values[b].unwrap_or(0.0))
                        .min(top);
                    (self.color)(reached)
                });
                cells.push((symbol, Some(color)));
            }
            rows.push(spans(cells));
        }
        rows
    }

    /// Dots a value reaches: at least one for a known value.
    fn level(&self, value: f64, dots: usize) -> usize {
        if self.max <= 0.0 || !value.is_finite() {
            return 1;
        }
        ((value.max(0.0) / self.max * dots as f64).round() as usize).clamp(1, dots)
    }
}

/// Cells as spans, a run of one colour in one span.
fn spans(cells: Vec<(char, Option<Color>)>) -> Vec<Span<'static>> {
    let style = |c: Option<Color>| c.map_or_else(Style::default, |c| Style::default().fg(c));
    let mut out: Vec<Span<'static>> = Vec::new();
    let mut run = String::new();
    let mut color: Option<Option<Color>> = None;
    for (symbol, cell_color) in cells {
        if let Some(previous) = color.filter(|c| *c != cell_color) {
            out.push(Span::styled(std::mem::take(&mut run), style(previous)));
        }
        color = Some(cell_color);
        run.push(symbol);
    }
    if let Some(color) = color {
        out.push(Span::styled(run, style(color)));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(rows: &[Vec<Span>]) -> Vec<String> {
        rows.iter()
            .map(|row| row.iter().map(|s| s.content.as_ref()).collect())
            .collect()
    }

    fn graph<'a>(values: &'a [Option<f64>], down: bool) -> Graph<'a> {
        Graph {
            values,
            max: 8.0,
            down,
            color: &|_| Color::Green,
            overlay: None,
        }
    }

    #[test]
    fn samples_rise_from_the_baseline_newest_at_the_right() {
        // Two rows, eight dots: 8 fills both, 4 the bottom row, 0 a dot.
        let values = [Some(8.0), Some(4.0), Some(0.0), None];
        // Three cells: the first two slots unknown.
        assert_eq!(text(&graph(&values, false).lines(3, 2)), [" ⡇ ", " ⣿⡀"]);
    }

    #[test]
    fn upload_hangs_from_the_top() {
        let values = [Some(8.0), Some(2.0)];
        assert_eq!(text(&graph(&values, true).lines(1, 2)), ["⡟", "⡇"]);
    }

    #[test]
    fn unknown_shows_nothing_and_zero_a_dot() {
        let values = [None, Some(0.0)];
        assert_eq!(text(&graph(&values, false).lines(1, 1)), ["⢀"]);
        let values = [None, None];
        assert_eq!(text(&graph(&values, false).lines(1, 1)), [" "]);
    }

    #[test]
    fn colours_follow_the_height_and_the_overlay() {
        let values = [Some(8.0), Some(8.0)];
        let overlay = [Some(4.0), Some(4.0)];
        let g = Graph {
            values: &values,
            max: 8.0,
            down: false,
            color: &|v| if v > 4.0 { Color::Red } else { Color::Green },
            overlay: Some((&overlay, Color::Blue)),
        };
        let rows = g.lines(1, 2);
        assert_eq!(rows[0][0].style.fg, Some(Color::Red));
        assert_eq!(rows[1][0].style.fg, Some(Color::Blue));
        assert_eq!(peak(&[None, Some(3.0), Some(1.0)], 2.0), 3.0);
        assert_eq!(peak(&[None], 2.0), 2.0);
    }
}
