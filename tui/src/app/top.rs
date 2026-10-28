//! Top (Settings → Top, or `t` in Rakumux mode): what this machine, this
//! window, the Rakumux server, its other clients and every live session's
//! processes use now: CPU, memory and network. Read from the daemon
//! (`SessionApi::resources`, capability `resources`) about every second
//! while it's open, off the UI thread; rates are the differences between the
//! last two samples. Nothing is recorded: closing it forgets them.
//!
//! Rows: this window, the server, the other windows and clients, then the
//! sessions (sorted by `s`), each with its processes as a tree under it when
//! it runs more than one. CPU is a percentage of one CPU, as `top` shows it.

use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use session_core::api::{ApiError, ClientUsage, ErrorCode, ProcessUsage, ResourceSample};
use session_core::ids::SessionId;

use super::grouping::AppKey;
use super::runtime::AppEvent;
use super::types::AppMode;
use super::App;

/// How often a new sample is asked for: a little under the idle tick, so
/// every tick asks.
const EVERY: Duration = Duration::from_millis(900);
/// When the second sample, the first with rates, is asked for.
const FIRST_RATES_AFTER: Duration = Duration::from_millis(500);

/// What sessions are sorted by (`s` goes to the next).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TopSort {
    Memory,
    Cpu,
    Network,
    Name,
}

impl TopSort {
    pub fn next(self) -> Self {
        match self {
            TopSort::Memory => TopSort::Cpu,
            TopSort::Cpu => TopSort::Network,
            TopSort::Network => TopSort::Name,
            TopSort::Name => TopSort::Memory,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            TopSort::Memory => "memory",
            TopSort::Cpu => "CPU",
            TopSort::Network => "network",
            TopSort::Name => "name",
        }
    }
}

/// What a row is, so the selection stays on it as rows come and go.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum RowKey {
    Daemon,
    Client(u32),
    Session(SessionId),
    Process(SessionId, u32),
}

impl RowKey {
    /// The session it is or is in.
    pub fn session(&self) -> Option<&SessionId> {
        match self {
            RowKey::Session(id) | RowKey::Process(id, _) => Some(id),
            RowKey::Daemon | RowKey::Client(_) => None,
        }
    }
}

/// What a row uses.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Usage {
    /// Percent of one CPU; None until two samples have it.
    pub cpu: Option<f64>,
    pub memory: u64,
    /// Bytes received a second; None where it isn't known.
    pub rx: Option<f64>,
    /// Bytes sent a second.
    pub tx: Option<f64>,
}

impl Usage {
    fn add(&mut self, other: &Usage) {
        let sum = |a: Option<f64>, b: Option<f64>| match (a, b) {
            (None, None) => None,
            (a, b) => Some(a.unwrap_or(0.0) + b.unwrap_or(0.0)),
        };
        self.cpu = sum(self.cpu, other.cpu);
        self.memory = self.memory.saturating_add(other.memory);
        self.rx = sum(self.rx, other.rx);
        self.tx = sum(self.tx, other.tx);
    }

    fn network(&self) -> Option<f64> {
        match (self.rx, self.tx) {
            (None, None) => None,
            (rx, tx) => Some(rx.unwrap_or(0.0) + tx.unwrap_or(0.0)),
        }
    }
}

/// A row of Top.
#[derive(Debug, Clone, PartialEq)]
pub struct TopRow {
    pub key: RowKey,
    /// Steps in from the left: a session's processes under it, each
    /// process's children under it.
    pub depth: usize,
    pub name: String,
    /// What it is, quieter after the name.
    pub detail: String,
    pub pid: Option<u32>,
    pub usage: Usage,
    /// A session running more than one process: whether they're shown.
    pub open: Option<bool>,
}

/// The whole machine.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct MachineSummary {
    pub cpus: u32,
    /// Percent of every CPU together.
    pub cpu: Option<f64>,
    pub memory_used: Option<u64>,
    pub memory_total: Option<u64>,
    pub rx: Option<f64>,
    pub tx: Option<f64>,
}

/// How a session is named in Top: its name, then its app and folder.
pub type SessionLabel = (String, String);

/// The open Top view (`AppMode::Top`).
#[derive(Debug, Clone)]
pub struct TopView {
    generation: u64,
    latest: Option<ResourceSample>,
    previous: Option<ResourceSample>,
    pub error: Option<String>,
    busy: bool,
    due: Option<Instant>,
    pub sort: TopSort,
    /// The selected row; the first when it's gone.
    selected: Option<RowKey>,
    /// Sessions whose processes are folded under their row.
    folded: HashSet<SessionId>,
    /// The first row in view and how many fit, kept by the drawing.
    pub first: Cell<usize>,
    pub page: Cell<usize>,
}

impl TopView {
    fn new(generation: u64) -> Self {
        Self {
            generation,
            latest: None,
            previous: None,
            error: None,
            busy: false,
            due: None,
            sort: TopSort::Memory,
            selected: None,
            folded: HashSet::new(),
            first: Cell::new(0),
            page: Cell::new(1),
        }
    }

    /// No sample yet, or none with rates: worth ticking fast for.
    pub fn waiting(&self) -> bool {
        self.previous.is_none() && self.error.is_none()
    }

    /// Whether a sample arrived.
    pub fn loaded(&self) -> bool {
        self.latest.is_some()
    }

    fn take(&mut self, sample: ResourceSample) {
        self.previous = self.latest.replace(sample);
        self.error = None;
    }

    /// Seconds between the last two samples, when there are two.
    fn seconds(&self) -> Option<f64> {
        let (now, then) = (self.latest.as_ref()?, self.previous.as_ref()?);
        let ms = now.at_ms.checked_sub(then.at_ms).filter(|ms| *ms > 0)?;
        Some(ms as f64 / 1000.0)
    }

    pub fn machine(&self) -> Option<MachineSummary> {
        let now = &self.latest.as_ref()?.machine;
        let then = self.previous.as_ref().map(|s| &s.machine);
        let seconds = self.seconds();
        let rate = |now: Option<u64>, then: Option<u64>| {
            Some(now?.saturating_sub(then?) as f64 / seconds?)
        };
        let cpu = then.and_then(|then| {
            let busy = now.cpu_busy_ms?.checked_sub(then.cpu_busy_ms?)?;
            let total = now.cpu_total_ms?.checked_sub(then.cpu_total_ms?)?;
            (total > 0).then(|| busy as f64 * 100.0 / total as f64)
        });
        Some(MachineSummary {
            cpus: now.cpus,
            cpu,
            memory_used: now
                .memory_total_bytes
                .zip(now.memory_available_bytes)
                .map(|(total, available)| total.saturating_sub(available)),
            memory_total: now.memory_total_bytes,
            rx: rate(now.net_rx_bytes, then.and_then(|t| t.net_rx_bytes)),
            tx: rate(now.net_tx_bytes, then.and_then(|t| t.net_tx_bytes)),
        })
    }

    /// The rows, in order. `own_pid` is this window's process; `label`
    /// names a session (None for one this window doesn't list).
    pub fn rows(
        &self,
        own_pid: u32,
        label: &dyn Fn(&SessionId) -> Option<SessionLabel>,
    ) -> Vec<TopRow> {
        let Some(latest) = &self.latest else {
            return Vec::new();
        };
        let earlier = self
            .previous
            .as_ref()
            .map(processes_by_pid)
            .unwrap_or_default();
        let seconds = self.seconds();
        let usage = |process: &ProcessUsage| usage_of(process, earlier.get(&process.pid), seconds);
        let row = |key: RowKey, process: &ProcessUsage, name: String, detail: String| TopRow {
            key,
            depth: 0,
            name,
            detail,
            pid: Some(process.pid),
            usage: usage(process),
            open: None,
        };

        let mut rows = Vec::new();
        let (own, others): (Vec<&ClientUsage>, Vec<&ClientUsage>) = latest
            .clients
            .iter()
            .partition(|c| c.process.pid == own_pid);
        let client_row = |client: &ClientUsage| {
            let (name, detail) = client_label(client, own_pid);
            row(
                RowKey::Client(client.process.pid),
                &client.process,
                name,
                detail,
            )
        };
        rows.extend(own.into_iter().map(client_row));
        rows.push(row(
            RowKey::Daemon,
            &latest.daemon,
            "rakumux-ptyd".into(),
            "server".into(),
        ));
        // Other windows first, then the rest as the daemon lists them.
        let (windows, rest): (Vec<&ClientUsage>, Vec<&ClientUsage>) =
            others.into_iter().partition(|c| c.client == "tui");
        rows.extend(windows.into_iter().chain(rest).map(client_row));

        let mut sessions: Vec<(SessionLabel, Usage, &session_core::api::SessionUsage)> = latest
            .sessions
            .iter()
            .filter(|s| !s.processes.is_empty())
            .map(|session| {
                let label = label(&session.id)
                    .unwrap_or_else(|| (short_id(&session.id), session.processes[0].name.clone()));
                let mut total = Usage::default();
                for process in &session.processes {
                    total.add(&usage(process));
                }
                (label, total, session)
            })
            .collect();
        sessions.sort_by(|a, b| {
            compare(self.sort, (&a.0 .0, &a.1), (&b.0 .0, &b.1))
                .then_with(|| a.2.id.as_str().cmp(b.2.id.as_str()))
        });
        for ((name, detail), total, session) in sessions {
            let several = session.processes.len() > 1;
            let open = several.then(|| !self.folded.contains(&session.id));
            rows.push(TopRow {
                key: RowKey::Session(session.id.clone()),
                depth: 0,
                name,
                detail,
                pid: (!several).then(|| session.processes[0].pid),
                usage: total,
                open,
            });
            if open == Some(true) {
                let tree = tree_order(&session.processes, self.sort, &usage);
                rows.extend(tree.into_iter().map(|(process, depth)| TopRow {
                    depth: depth + 1,
                    ..row(
                        RowKey::Process(session.id.clone(), process.pid),
                        process,
                        process.name.clone(),
                        arguments(&process.command),
                    )
                }));
            }
        }
        rows
    }

    /// The total of every Rakumux process (sessions' processes included).
    pub fn total(rows: &[TopRow]) -> Usage {
        let mut total = Usage::default();
        for row in rows
            .iter()
            .filter(|r| !matches!(r.key, RowKey::Process(..)))
        {
            total.add(&row.usage);
        }
        total
    }

    /// The selected row's index in `rows`.
    pub fn selected_index(&self, rows: &[TopRow]) -> usize {
        self.selected
            .as_ref()
            .and_then(|key| rows.iter().position(|row| &row.key == key))
            .unwrap_or(0)
    }

    fn select(&mut self, rows: &[TopRow], index: usize) {
        self.selected = rows
            .get(index.min(rows.len().saturating_sub(1)))
            .map(|r| r.key.clone());
    }
}

/// Every process of `sample` by pid.
fn processes_by_pid(sample: &ResourceSample) -> HashMap<u32, ProcessUsage> {
    std::iter::once(&sample.daemon)
        .chain(sample.clients.iter().map(|c| &c.process))
        .chain(sample.sessions.iter().flat_map(|s| s.processes.iter()))
        .map(|p| (p.pid, p.clone()))
        .collect()
}

/// `process`'s use, its rates from `earlier` (the same pid and name in the
/// sample before, `seconds` ago).
fn usage_of(process: &ProcessUsage, earlier: Option<&ProcessUsage>, seconds: Option<f64>) -> Usage {
    let earlier = earlier.filter(|e| e.name == process.name);
    let rate =
        |now: Option<u64>, then: Option<u64>| Some(now?.saturating_sub(then?) as f64 / seconds?);
    Usage {
        // ms of CPU a second, as a percentage of one CPU.
        cpu: rate(Some(process.cpu_ms), earlier.map(|e| e.cpu_ms)).map(|ms| ms / 10.0),
        memory: process.rss_bytes,
        rx: rate(process.net_rx_bytes, earlier.and_then(|e| e.net_rx_bytes)),
        tx: rate(process.net_tx_bytes, earlier.and_then(|e| e.net_tx_bytes)),
    }
}

/// Larger use first (None last), names alphabetically; by `sort`.
fn compare(sort: TopSort, a: (&String, &Usage), b: (&String, &Usage)) -> std::cmp::Ordering {
    let by = |f: fn(&Usage) -> Option<f64>| {
        let key = |u: &Usage| f(u).unwrap_or(-1.0);
        key(b.1).total_cmp(&key(a.1))
    };
    let order = match sort {
        TopSort::Memory => by(|u| Some(u.memory as f64)),
        TopSort::Cpu => by(|u| u.cpu),
        TopSort::Network => by(Usage::network),
        TopSort::Name => std::cmp::Ordering::Equal,
    };
    order.then_with(|| a.0.to_lowercase().cmp(&b.0.to_lowercase()))
}

/// A session's processes as a tree from its first, each with its depth;
/// siblings by `sort`. Processes whose parent isn't listed hang off the
/// first.
fn tree_order<'a>(
    processes: &'a [ProcessUsage],
    sort: TopSort,
    usage: &dyn Fn(&ProcessUsage) -> Usage,
) -> Vec<(&'a ProcessUsage, usize)> {
    let Some(root) = processes.first() else {
        return Vec::new();
    };
    let listed: HashSet<u32> = processes.iter().map(|p| p.pid).collect();
    let mut children: HashMap<u32, Vec<&ProcessUsage>> = HashMap::new();
    for process in &processes[1..] {
        let parent = process
            .parent
            .filter(|parent| listed.contains(parent) && *parent != process.pid)
            .unwrap_or(root.pid);
        children.entry(parent).or_default().push(process);
    }
    for siblings in children.values_mut() {
        siblings.sort_by(|a, b| {
            compare(sort, (&a.name, &usage(a)), (&b.name, &usage(b))).then(a.pid.cmp(&b.pid))
        });
    }
    let mut order = Vec::with_capacity(processes.len());
    let mut seen = HashSet::new();
    let mut stack = vec![(root, 0usize)];
    while let Some((process, depth)) = stack.pop() {
        if !seen.insert(process.pid) {
            continue;
        }
        order.push((process, depth));
        if let Some(kids) = children.get(&process.pid) {
            stack.extend(kids.iter().rev().map(|kid| (*kid, depth + 1)));
        }
    }
    order
}

/// How a client is named: its program, then which one it is.
fn client_label(client: &ClientUsage, own_pid: u32) -> (String, String) {
    let rakumux = || "rakumux".to_string();
    match client.client.as_str() {
        "tui" if client.process.pid == own_pid => (rakumux(), "this window".into()),
        "tui" => (rakumux(), "another window".into()),
        "api" => ("rakumux-api".into(), String::new()),
        "rakumux-status-service" => (rakumux(), "account service".into()),
        "cli" => (rakumux(), arguments(&client.process.command)),
        other => (client.process.name.clone(), other.to_string()),
    }
}

/// A command line without its program.
fn arguments(command: &str) -> String {
    command
        .split_once(' ')
        .map(|(_, rest)| rest.trim().to_string())
        .unwrap_or_default()
}

/// The end of a session id, for one this window doesn't list.
fn short_id(id: &SessionId) -> String {
    let id = id.as_str();
    id[id.len().saturating_sub(6)..].to_lowercase()
}

impl App {
    /// Open Top and ask for the first sample.
    pub fn open_top(&mut self) {
        self.top_generation += 1;
        self.mode = AppMode::Top(TopView::new(self.top_generation));
        self.read_resources(Instant::now());
    }

    pub fn close_top(&mut self) {
        if matches!(self.mode, AppMode::Top(_)) {
            self.mode = AppMode::Normal;
        }
    }

    /// Ask for a sample when one is due.
    pub(super) fn top_timers(&mut self, now: Instant) {
        let AppMode::Top(view) = &self.mode else {
            return;
        };
        if !view.busy && view.due.is_none_or(|due| now >= due) {
            self.read_resources(now);
        }
    }

    fn read_resources(&mut self, now: Instant) {
        let api = self.api();
        let AppMode::Top(view) = &mut self.mode else {
            return;
        };
        view.due = Some(now + EVERY);
        let Some(api) = api else {
            view.error = Some("Not connected to the Rakumux server.".into());
            return;
        };
        view.busy = true;
        let generation = view.generation;
        self.exec.job(move || AppEvent::Resources {
            generation,
            result: api.resources(),
        });
    }

    pub(super) fn on_resources(
        &mut self,
        generation: u64,
        result: Result<ResourceSample, ApiError>,
    ) {
        let AppMode::Top(view) = &mut self.mode else {
            return;
        };
        if view.generation != generation {
            return;
        }
        view.busy = false;
        match result {
            Ok(sample) => {
                let first = !view.loaded();
                view.take(sample);
                if first {
                    view.due = Some(Instant::now() + FIRST_RATES_AFTER);
                }
            }
            Err(error) if error.code == ErrorCode::Unsupported => {
                view.error = Some(
                    "The running Rakumux server is older than this app: restart it (S) to see \
                     Top."
                        .into(),
                )
            }
            Err(error) => view.error = Some(error.message),
        }
    }

    /// Top's rows as this window names the sessions.
    pub fn top_rows(&self, view: &TopView) -> Vec<TopRow> {
        let label = |id: &SessionId| {
            let session = self.list.get(id)?;
            let app = AppKey::of(session);
            let folder = self.folders.name_of(&session.scope);
            Some((session.name.clone(), format!("{} · {folder}", app.as_str())))
        };
        view.rows(std::process::id(), &label)
    }

    /// Move Top's selection `delta` rows (clamped).
    pub fn top_move(&mut self, delta: isize) {
        self.with_top_rows(|view, rows| {
            let index = view.selected_index(rows).saturating_add_signed(delta);
            view.select(rows, index);
        });
    }

    /// Move Top's selection a page.
    pub fn top_page(&mut self, pages: isize) {
        let page = match &self.mode {
            AppMode::Top(view) => view.page.get().max(1),
            _ => return,
        };
        self.top_move(pages.saturating_mul(isize::try_from(page).unwrap_or(1)));
    }

    /// Fold (`Some(false)`), unfold (`Some(true)`) or toggle (`None`) the
    /// processes of the selected session. Folding from one of its processes
    /// selects the session.
    pub fn top_fold(&mut self, open: Option<bool>) {
        self.with_top_rows(|view, rows| {
            let index = view.selected_index(rows);
            let Some(row) = rows.get(index) else {
                return;
            };
            let Some(id) = row.key.session().cloned() else {
                return;
            };
            let is_open = !view.folded.contains(&id);
            let has_processes = row.open.is_some() || matches!(row.key, RowKey::Process(..));
            if !has_processes {
                return;
            }
            if open.unwrap_or(!is_open) {
                view.folded.remove(&id);
            } else {
                view.folded.insert(id.clone());
                view.selected = Some(RowKey::Session(id));
            }
        });
    }

    pub fn top_sort(&mut self) {
        if let AppMode::Top(view) = &mut self.mode {
            view.sort = view.sort.next();
        }
    }

    /// Enter: into the selected row's session.
    pub fn top_enter(&mut self) {
        let AppMode::Top(view) = &self.mode else {
            return;
        };
        let rows = self.top_rows(view);
        let Some(id) = rows
            .get(view.selected_index(&rows))
            .and_then(|row| row.key.session().cloned())
        else {
            return;
        };
        if self.list.get(&id).is_none() {
            self.set_status("This window doesn't list that session.");
            return;
        }
        self.mode = AppMode::Normal;
        self.go_into(id);
    }

    fn with_top_rows(&mut self, change: impl FnOnce(&mut TopView, &[TopRow])) {
        let AppMode::Top(view) = &self.mode else {
            return;
        };
        let rows = self.top_rows(view);
        if let AppMode::Top(view) = &mut self.mode {
            change(view, &rows);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use session_core::api::{MachineUsage, SessionUsage};

    fn process(pid: u32, parent: Option<u32>, name: &str, cpu_ms: u64, rss: u64) -> ProcessUsage {
        ProcessUsage {
            pid,
            parent,
            name: name.into(),
            command: format!("{name} --x"),
            cpu_ms,
            rss_bytes: rss,
            net_rx_bytes: Some(cpu_ms * 10),
            net_tx_bytes: Some(cpu_ms),
        }
    }

    fn client(client: &str, process: ProcessUsage) -> ClientUsage {
        ClientUsage {
            client: client.into(),
            connections: 1,
            process,
        }
    }

    /// Two sessions: `a` a shell running claude with a child, `b` a shell.
    fn sample(at_ms: i64, scale: u64, a: &SessionId, b: &SessionId) -> ResourceSample {
        ResourceSample {
            at_ms,
            machine: MachineUsage {
                cpus: 8,
                cpu_busy_ms: Some(1000 * scale),
                cpu_total_ms: Some(8000 * scale),
                memory_total_bytes: Some(16_000),
                memory_available_bytes: Some(6_000),
                net_rx_bytes: Some(500 * scale),
                net_tx_bytes: Some(50 * scale),
            },
            daemon: process(10, None, "rakumux-ptyd", 100 * scale, 30),
            clients: vec![
                client("api", process(40, None, "rakumux-api", 0, 12)),
                client("tui", process(20, None, "tui", 200 * scale, 40)),
                client("tui", process(21, None, "tui", 0, 41)),
            ],
            sessions: vec![
                SessionUsage {
                    id: a.clone(),
                    processes: vec![
                        process(30, None, "bash", 0, 5),
                        process(31, Some(30), "claude", 900 * scale, 800),
                        process(32, Some(31), "node", 10 * scale, 25),
                    ],
                },
                SessionUsage {
                    id: b.clone(),
                    processes: vec![process(50, None, "zsh", 0, 900)],
                },
            ],
        }
    }

    fn labels(a: &SessionId) -> impl Fn(&SessionId) -> Option<SessionLabel> + '_ {
        move |id: &SessionId| {
            if id == a {
                Some(("fix-login".into(), "claude · web".into()))
            } else {
                None
            }
        }
    }

    fn names(rows: &[TopRow]) -> Vec<(String, usize)> {
        rows.iter().map(|r| (r.name.clone(), r.depth)).collect()
    }

    #[test]
    fn rows_put_this_window_and_the_server_first_then_sessions_with_their_trees() {
        let (a, b) = (SessionId::generate(), SessionId::generate());
        let mut view = TopView::new(1);
        view.take(sample(1000, 1, &a, &b));
        let rows = view.rows(20, &labels(&a));
        let b_name = short_id(&b);
        assert_eq!(
            names(&rows),
            [
                ("rakumux".into(), 0),
                ("rakumux-ptyd".into(), 0),
                ("rakumux".into(), 0),
                ("rakumux-api".into(), 0),
                // Sorted by memory: b's shell has more.
                (b_name, 0),
                ("fix-login".into(), 0),
                ("bash".into(), 1),
                ("claude".into(), 2),
                ("node".into(), 3),
            ]
        );
        assert_eq!(rows[0].detail, "this window");
        assert_eq!(rows[2].detail, "another window");
        assert_eq!(rows[4].pid, Some(50));
        assert_eq!(rows[4].open, None);
        assert_eq!(rows[5].pid, None);
        assert_eq!(rows[5].open, Some(true));
        assert_eq!(rows[5].usage.memory, 830);
        assert_eq!(rows[7].detail, "--x");
        // One sample: no rates yet.
        assert_eq!(rows[5].usage.cpu, None);
        assert!(view.waiting());
    }

    #[test]
    fn rates_come_from_the_last_two_samples() {
        let (a, b) = (SessionId::generate(), SessionId::generate());
        let mut view = TopView::new(1);
        view.take(sample(1000, 1, &a, &b));
        view.take(sample(3000, 2, &a, &b));
        assert!(!view.waiting());
        let rows = view.rows(20, &labels(&a));
        let claude = rows.iter().find(|r| r.name == "claude").unwrap();
        // 900 ms of CPU over 2 s: 45% of one CPU.
        assert_eq!(claude.usage.cpu, Some(45.0));
        assert_eq!(claude.usage.rx, Some(4500.0));
        assert_eq!(claude.usage.tx, Some(450.0));
        let session = rows.iter().find(|r| r.name == "fix-login").unwrap();
        assert_eq!(session.usage.cpu, Some(45.5));
        let machine = view.machine().unwrap();
        assert_eq!(machine.cpu, Some(12.5));
        assert_eq!(machine.memory_used, Some(10_000));
        assert_eq!(machine.rx, Some(250.0));
        let total = TopView::total(&rows);
        assert_eq!(total.memory, 30 + 40 + 41 + 12 + 830 + 900);
        assert_eq!(total.cpu, Some(5.0 + 10.0 + 45.5));
    }

    #[test]
    fn a_reused_pid_gets_no_rate() {
        let earlier = process(31, None, "claude", 900, 1);
        let now = process(31, None, "sleep", 1000, 1);
        assert_eq!(usage_of(&now, Some(&earlier), Some(1.0)).cpu, None);
        assert_eq!(usage_of(&now, None, Some(1.0)).cpu, None);
        let now = process(31, None, "claude", 1000, 1);
        assert_eq!(usage_of(&now, Some(&earlier), Some(1.0)).cpu, Some(10.0));
    }

    #[test]
    fn sorting_and_folding_reorder_sessions_and_hide_processes() {
        let (a, b) = (SessionId::generate(), SessionId::generate());
        let mut view = TopView::new(1);
        view.take(sample(1000, 1, &a, &b));
        view.take(sample(2000, 2, &a, &b));
        view.sort = TopSort::Cpu;
        let rows = view.rows(20, &labels(&a));
        assert_eq!(rows[4].name, "fix-login");
        // By name, with both named (an unnamed one's id sorts anywhere).
        let both = |id: &SessionId| {
            labels(&a)(id).or_else(|| Some(("zsh-session".into(), "shell · web".into())))
        };
        view.sort = TopSort::Name;
        let rows = view.rows(20, &both);
        assert_eq!(rows[4].name, "fix-login");
        assert_eq!(rows[8].name, "zsh-session");
        view.folded.insert(a.clone());
        let rows = view.rows(20, &both);
        assert_eq!(rows.len(), 6);
        assert_eq!(rows[4].open, Some(false));
    }

    #[test]
    fn a_process_whose_parent_isnt_listed_hangs_off_the_first() {
        let processes = [
            process(30, None, "bash", 0, 1),
            process(35, Some(99), "orphan", 0, 1),
            process(31, Some(30), "claude", 0, 2),
        ];
        let order: Vec<(u32, usize)> = tree_order(&processes, TopSort::Memory, &|p| Usage {
            memory: p.rss_bytes,
            ..Usage::default()
        })
        .into_iter()
        .map(|(p, depth)| (p.pid, depth))
        .collect();
        assert_eq!(order, [(30, 0), (31, 1), (35, 1)]);
    }
}
