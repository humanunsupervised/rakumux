//! rtop's history: what the machine, Rakumux and each row of rtop used over
//! the last few minutes, one point a sample, in this window's memory only.
//! Every series takes a point with each sample, so they line up from their
//! newest end; a series whose row is gone from a sample is dropped. Leaving
//! the screen marks a gap (a point with nothing known) rather than joining
//! across the time it wasn't sampled.

use std::collections::{HashMap, VecDeque};

use super::{RowKey, Usage};

/// Points each series keeps: about five minutes at a sample a second.
pub const HISTORY_LEN: usize = 300;
/// Rows with a series of their own; the machine's and Rakumux's total come
/// on top. Rows past it (processes come last) get none: 512 series of
/// [`HISTORY_LEN`] points of 16 bytes is under 2.5 MB.
pub const MAX_SERIES: usize = 512;

/// One sample of a series. `NaN` stands for unknown (no rate yet, network on
/// macOS, a gap), so a point stays 16 bytes.
#[derive(Debug, Clone, Copy)]
pub struct Point {
    cpu: f32,
    memory: f32,
    rx: f32,
    tx: f32,
}

/// What a graph shows of a [`Point`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Metric {
    /// Percent of one CPU (the machine's: of every CPU together).
    Cpu,
    /// Bytes.
    Memory,
    /// Bytes received a second.
    Rx,
    /// Bytes sent a second.
    Tx,
    /// Received and sent together.
    Net,
}

fn pack(value: Option<f64>) -> f32 {
    value.map_or(f32::NAN, |v| v as f32)
}

fn unpack(value: f32) -> Option<f64> {
    (!value.is_nan()).then_some(f64::from(value))
}

impl Point {
    /// Nothing known: where rtop wasn't on screen.
    pub const GAP: Point = Point {
        cpu: f32::NAN,
        memory: f32::NAN,
        rx: f32::NAN,
        tx: f32::NAN,
    };

    pub fn new(cpu: Option<f64>, memory: Option<f64>, rx: Option<f64>, tx: Option<f64>) -> Self {
        Point {
            cpu: pack(cpu),
            memory: pack(memory),
            rx: pack(rx),
            tx: pack(tx),
        }
    }

    /// A row's use.
    pub fn of(usage: &Usage) -> Self {
        Point::new(usage.cpu, Some(usage.memory as f64), usage.rx, usage.tx)
    }

    pub fn get(&self, metric: Metric) -> Option<f64> {
        match metric {
            Metric::Cpu => unpack(self.cpu),
            Metric::Memory => unpack(self.memory),
            Metric::Rx => unpack(self.rx),
            Metric::Tx => unpack(self.tx),
            Metric::Net => match (unpack(self.rx), unpack(self.tx)) {
                (None, None) => None,
                (rx, tx) => Some(rx.unwrap_or(0.0) + tx.unwrap_or(0.0)),
            },
        }
    }

    fn is_gap(&self) -> bool {
        [self.cpu, self.memory, self.rx, self.tx]
            .iter()
            .all(|v| v.is_nan())
    }
}

/// A bounded run of points, oldest first.
#[derive(Debug, Clone, Default)]
pub struct Series(VecDeque<Point>);

impl Series {
    fn push(&mut self, point: Point) {
        if self.0.len() == HISTORY_LEN {
            self.0.pop_front();
        }
        self.0.push_back(point);
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// The newest point.
    pub fn last(&self) -> Option<&Point> {
        self.0.back()
    }

    /// The newest `n` values of `metric`, oldest first.
    pub fn values(&self, metric: Metric, n: usize) -> Vec<Option<f64>> {
        self.0
            .iter()
            .skip(self.0.len().saturating_sub(n))
            .map(|p| p.get(metric))
            .collect()
    }
}

/// Every series rtop keeps (`RtopView::history`).
#[derive(Debug, Clone, Default)]
pub struct History {
    /// The machine: CPU of every CPU together, memory used, network.
    machine: Series,
    /// The machine's memory and CPUs at the newest sample.
    pub memory_total: Option<u64>,
    pub cpus: u32,
    /// Every Rakumux process together.
    total: Series,
    rows: HashMap<RowKey, Series>,
}

impl History {
    /// A sample's points: the machine's, Rakumux's total and each row's (in
    /// order of priority for the [`MAX_SERIES`] cap). Rows not in it are
    /// dropped.
    pub fn record(
        &mut self,
        machine: Point,
        memory_total: Option<u64>,
        cpus: u32,
        total: Point,
        rows: &[(RowKey, Usage)],
    ) {
        self.machine.push(machine);
        self.memory_total = memory_total;
        self.cpus = cpus;
        self.total.push(total);
        let mut kept = HashMap::with_capacity(rows.len().min(MAX_SERIES));
        for (key, usage) in rows {
            let series = match self.rows.remove(key) {
                Some(series) => series,
                None if kept.len() < MAX_SERIES => Series::default(),
                None => continue,
            };
            kept.insert(key.clone(), series);
            if let Some(series) = kept.get_mut(key) {
                series.push(Point::of(usage));
            }
        }
        self.rows = kept;
    }

    /// rtop left the screen: a gap in every series (once).
    pub fn gap(&mut self) {
        for series in std::iter::once(&mut self.machine)
            .chain(std::iter::once(&mut self.total))
            .chain(self.rows.values_mut())
        {
            if series.last().is_some_and(|p| !p.is_gap()) {
                series.push(Point::GAP);
            }
        }
    }

    pub fn machine(&self) -> &Series {
        &self.machine
    }

    pub fn total(&self) -> &Series {
        &self.total
    }

    /// A row's series, when it has one.
    pub fn row(&self, key: &RowKey) -> Option<&Series> {
        self.rows.get(key)
    }

    /// How many rows have a series.
    #[cfg(test)]
    pub fn rows_kept(&self) -> usize {
        self.rows.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use session_core::ids::SessionId;

    fn usage(cpu: Option<f64>, memory: u64) -> Usage {
        Usage {
            cpu,
            memory,
            rx: None,
            tx: None,
        }
    }

    fn record(history: &mut History, rows: &[(RowKey, Usage)]) {
        history.record(
            Point::new(Some(10.0), Some(5.0), Some(1.0), Some(2.0)),
            Some(100),
            8,
            Point::new(Some(1.0), Some(1.0), None, None),
            rows,
        );
    }

    #[test]
    fn a_series_keeps_only_its_newest_points() {
        let mut history = History::default();
        let rows = [(RowKey::Daemon, usage(Some(1.0), 7))];
        for _ in 0..HISTORY_LEN + 25 {
            record(&mut history, &rows);
        }
        assert_eq!(history.machine().len(), HISTORY_LEN);
        assert_eq!(history.total().len(), HISTORY_LEN);
        let daemon = history.row(&RowKey::Daemon).unwrap();
        assert_eq!(daemon.len(), HISTORY_LEN);
        assert_eq!(daemon.values(Metric::Memory, 3), [Some(7.0); 3]);
        assert_eq!(daemon.values(Metric::Cpu, 1000).len(), HISTORY_LEN);
    }

    #[test]
    fn a_row_that_goes_is_dropped_and_one_that_comes_starts_fresh() {
        let mut history = History::default();
        let session = RowKey::Session(SessionId::generate());
        record(
            &mut history,
            &[
                (RowKey::Daemon, usage(None, 1)),
                (session.clone(), usage(None, 1)),
            ],
        );
        record(&mut history, &[(RowKey::Daemon, usage(Some(2.0), 1))]);
        assert!(history.row(&session).is_none());
        assert_eq!(history.rows_kept(), 1);
        record(
            &mut history,
            &[
                (RowKey::Daemon, usage(Some(3.0), 1)),
                (session.clone(), usage(Some(4.0), 1)),
            ],
        );
        assert_eq!(history.row(&session).unwrap().len(), 1);
        // Unknown stays unknown, never zero.
        assert_eq!(
            history.row(&RowKey::Daemon).unwrap().values(Metric::Cpu, 3),
            [None, Some(2.0), Some(3.0)]
        );
    }

    #[test]
    fn rows_past_the_cap_get_no_series() {
        let mut history = History::default();
        let rows: Vec<(RowKey, Usage)> = (0..MAX_SERIES as u32 + 10)
            .map(|pid| (RowKey::Client(pid), usage(None, 1)))
            .collect();
        record(&mut history, &rows);
        assert_eq!(history.rows_kept(), MAX_SERIES);
        assert!(history.row(&RowKey::Client(0)).is_some());
        assert!(history.row(&RowKey::Client(MAX_SERIES as u32)).is_none());
    }

    #[test]
    fn leaving_the_screen_marks_one_gap() {
        let mut history = History::default();
        history.gap();
        assert!(history.machine().is_empty(), "nothing to mark yet");
        record(&mut history, &[(RowKey::Daemon, usage(Some(1.0), 1))]);
        history.gap();
        history.gap();
        record(&mut history, &[(RowKey::Daemon, usage(Some(5.0), 1))]);
        assert_eq!(
            history.machine().values(Metric::Cpu, 10),
            [Some(10.0), None, Some(10.0)]
        );
        assert_eq!(
            history
                .row(&RowKey::Daemon)
                .unwrap()
                .values(Metric::Memory, 10),
            [Some(1.0), None, Some(1.0)]
        );
    }

    #[test]
    fn network_together_is_known_when_either_is() {
        let p = Point::new(None, None, Some(3.0), None);
        assert_eq!(p.get(Metric::Net), Some(3.0));
        assert_eq!(p.get(Metric::Tx), None);
        assert_eq!(Point::GAP.get(Metric::Net), None);
    }
}
