//! What this machine uses now, for rtop on rakumux.com (`contracts/feed/`
//! `resources-upload`). Only while someone has rtop open there: a replies
//! read answering `resources_wanted` starts it, each upload's answer
//! (`wanted`) keeps it going, and it stops at `false` or once neither said
//! so for [`WANTED_FOR`]. Meanwhile the daemon is asked what the machine,
//! Rakumux's own processes and every live session's processes use
//! (`SessionApi::resources`) every [`EVERY`], and each reading after the
//! first is uploaded with rates from the last two.
//!
//! The upload is numbers only: the machine's counters, Rakumux's processes
//! added up by role, and each session the session list has (never a private
//! one or another machine's) by id, its processes added up. Process names
//! match a pid between two readings here and never leave the machine, nor
//! do command lines, pids or paths. Driven by the owner thread with its own
//! clock; nothing is kept but the last reading.

use std::collections::HashMap;
use std::time::Duration;

use serde::Serialize;
use session_core::api::{ErrorCode, MachineUsage, ProcessUsage, ResourceSample};
use session_core::ids::SessionId;

use super::wire::{self, FeedFailure, FeedRequest};

/// How long rakumux.com's last word that it wants readings holds: a replies
/// read's `resources_wanted` or an upload's `wanted`. Replies are read every
/// second while the website is in use, so this outlasts a few missed reads.
const WANTED_FOR: Duration = Duration::from_secs(30);
/// How often the daemon is asked while readings are wanted.
const EVERY: Duration = Duration::from_secs(2);
/// When the second reading, the first with rates, is asked for: the next tick.
const FIRST_RATES_AFTER: Duration = Duration::from_secs(1);
/// The first and longest waits after a failed upload.
const RETRY_MIN: Duration = Duration::from_secs(5);
const RETRY_MAX: Duration = Duration::from_secs(300);
/// The most sessions an upload carries (the contract's bound): the ones
/// using the most memory.
const MAX_SESSIONS: usize = 200;
/// Never before 2020 or after 2100 on the wire (the contract's timestamps).
const EARLIEST_MS: i64 = 1_577_836_800_000;
const LATEST_MS: i64 = 4_102_444_800_000;
/// The contract's bounds on counts and numbers.
const MAX_CPUS: u32 = 4096;
const MAX_ROLE_PROCESSES: u32 = 10_000;
const MAX_SESSION_PROCESSES: u32 = 500;
const MAX_CPU_PERCENT: f64 = 409_600.0;
const MAX_BYTES: u64 = 9_007_199_254_740_991;

/// What the owner should start next.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ResourceWork {
    /// Ask the daemon what everything uses now.
    Read,
    /// Upload a reading.
    Upload(FeedRequest),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Out {
    Read,
    Upload,
}

/// rtop's readings on the owner thread.
pub(crate) struct ResourceSync {
    /// Signed in with the Feed switch on and this machine's name confirmed.
    enabled: bool,
    /// Until when rakumux.com wants readings.
    wanted_until: Option<Duration>,
    /// The daemon can't say (an older one): not asked until it reconnects.
    unsupported: bool,
    /// The last reading, for rates; dropped whenever readings stop.
    previous: Option<ResourceSample>,
    /// An upload made from the last reading, not started yet.
    ready: Option<FeedRequest>,
    out: Option<Out>,
    next_read: Duration,
    retry_at: Duration,
    retry: Duration,
}

impl ResourceSync {
    pub fn new() -> Self {
        Self {
            enabled: false,
            wanted_until: None,
            unsupported: false,
            previous: None,
            ready: None,
            out: None,
            next_read: Duration::ZERO,
            retry_at: Duration::ZERO,
            retry: RETRY_MIN,
        }
    }

    /// Follow the account: signed in with its Feed switch on and this
    /// machine's name confirmed. Off, nothing is wanted or kept.
    pub fn set_enabled(&mut self, enabled: bool, now: Duration) {
        if enabled == self.enabled {
            return;
        }
        self.enabled = enabled;
        self.stop();
        self.retry = RETRY_MIN;
        self.retry_at = now;
    }

    /// A replies read said someone has rtop open on rakumux.com: read now,
    /// unless already reading.
    pub fn wanted(&mut self, now: Duration) {
        if !self.enabled {
            return;
        }
        if !self.is_wanted(now) {
            self.next_read = now;
        }
        self.wanted_until = Some(now + WANTED_FOR);
    }

    /// The daemon (re)connected: ask it again, and start the rates afresh
    /// (it may be another daemon, with other processes).
    pub fn connected(&mut self) {
        self.unsupported = false;
        self.previous = None;
        self.ready = None;
    }

    fn is_wanted(&self, now: Duration) -> bool {
        self.enabled && self.wanted_until.is_some_and(|until| now < until)
    }

    /// Nothing wanted: forget the reading (rates never span a pause) and any
    /// upload made from it.
    fn stop(&mut self) {
        self.wanted_until = None;
        self.previous = None;
        self.ready = None;
    }

    /// The work due now, if any: an upload made from the last reading, else
    /// a reading. One at a time.
    pub fn next_work(&mut self, now: Duration) -> Option<ResourceWork> {
        if !self.is_wanted(now) {
            if self.wanted_until.is_some() || self.previous.is_some() {
                self.stop();
            }
            return None;
        }
        if self.out.is_some() || now < self.retry_at {
            return None;
        }
        if let Some(request) = self.ready.take() {
            self.out = Some(Out::Upload);
            return Some(ResourceWork::Upload(request));
        }
        if self.unsupported || now < self.next_read {
            return None;
        }
        self.out = Some(Out::Read);
        Some(ResourceWork::Read)
    }

    /// The daemon answered a reading: with a reading before it, an upload as
    /// `machine` (its sessions only those `listed`) is ready for
    /// [`next_work`](Self::next_work). An older daemon isn't asked again
    /// until it reconnects.
    pub fn read_done(
        &mut self,
        result: Result<ResourceSample, ErrorCode>,
        machine: &str,
        listed: &dyn Fn(&SessionId) -> bool,
        now: Duration,
    ) {
        if self.out == Some(Out::Read) {
            self.out = None;
        }
        let sample = match result {
            Ok(sample) => sample,
            Err(ErrorCode::Unsupported) => {
                self.unsupported = true;
                return;
            }
            Err(_) => {
                self.next_read = now + EVERY;
                return;
            }
        };
        if !self.is_wanted(now) {
            return;
        }
        let first = self.previous.is_none();
        self.ready = self.previous.as_ref().and_then(|previous| {
            wire::resources_upload(&Upload::of(machine, previous, &sample, listed))
        });
        self.previous = Some(sample);
        self.next_read = now + if first { FIRST_RATES_AFTER } else { EVERY };
    }

    /// An upload finished; `current` says whether it was made for the account
    /// still signed in. `wanted` keeps readings going, or stops them; a
    /// failure holds the next one back (`machine_taken` is left to the
    /// session list, which renames the machine).
    pub fn done(&mut self, current: bool, outcome: Result<bool, FeedFailure>, now: Duration) {
        if self.out == Some(Out::Upload) {
            self.out = None;
        }
        if !current || !self.enabled {
            return;
        }
        match outcome {
            Ok(true) => {
                self.retry = RETRY_MIN;
                self.wanted_until = Some(now + WANTED_FOR);
            }
            Ok(false) => {
                self.retry = RETRY_MIN;
                self.stop();
            }
            Err(failure) => {
                let wait = match failure {
                    FeedFailure::RateLimited(Some(delay))
                    | FeedFailure::Unavailable(Some(delay)) => delay,
                    _ => self.retry,
                };
                self.retry = self.retry.saturating_mul(2).min(RETRY_MAX);
                self.retry_at = now + wait.min(Duration::from_secs(86_400));
            }
        }
    }
}

/// `resources-upload`.
#[derive(Debug, Serialize)]
pub(crate) struct Upload<'a> {
    version: u8,
    machine: &'a str,
    at: i64,
    host: Host,
    rakumux: Vec<Role>,
    sessions: Vec<SessionTotal>,
}

/// The whole machine.
#[derive(Debug, Serialize)]
struct Host {
    cpus: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    cpu_percent: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    memory_used_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    memory_total_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    net_rx_bytes_per_s: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    net_tx_bytes_per_s: Option<u64>,
}

/// Rakumux's processes with one role, added up.
#[derive(Debug, Serialize)]
struct Role {
    role: &'static str,
    processes: u32,
    usage: Usage,
}

/// A session's processes, added up.
#[derive(Debug, Serialize)]
struct SessionTotal {
    id: String,
    processes: u32,
    usage: Usage,
}

/// What some processes use now (`usage`).
#[derive(Debug, Default, Serialize)]
struct Usage {
    /// Of one CPU; absent until two readings have it.
    #[serde(skip_serializing_if = "Option::is_none")]
    cpu_percent: Option<f64>,
    memory_bytes: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    net_rx_bytes_per_s: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    net_tx_bytes_per_s: Option<u64>,
}

/// Rates as they're added up, before rounding for the wire.
#[derive(Clone, Copy, Debug, Default)]
struct Rates {
    cpu: Option<f64>,
    memory: u64,
    rx: Option<f64>,
    tx: Option<f64>,
}

impl Rates {
    fn add(&mut self, other: &Rates) {
        let sum = |a: Option<f64>, b: Option<f64>| match (a, b) {
            (None, None) => None,
            (a, b) => Some(a.unwrap_or(0.0) + b.unwrap_or(0.0)),
        };
        self.cpu = sum(self.cpu, other.cpu);
        self.memory = self.memory.saturating_add(other.memory);
        self.rx = sum(self.rx, other.rx);
        self.tx = sum(self.tx, other.tx);
    }

    fn usage(self) -> Usage {
        Usage {
            cpu_percent: self.cpu.map(|cpu| tenths(cpu.clamp(0.0, MAX_CPU_PERCENT))),
            memory_bytes: self.memory.min(MAX_BYTES),
            net_rx_bytes_per_s: self.rx.map(per_second),
            net_tx_bytes_per_s: self.tx.map(per_second),
        }
    }
}

/// `value` to one decimal, which is all rtop shows.
fn tenths(value: f64) -> f64 {
    (value * 10.0).round() / 10.0
}

/// A rate in whole bytes a second, within the contract.
fn per_second(value: f64) -> u64 {
    (value.max(0.0).round() as u64).min(MAX_BYTES)
}

/// Rakumux's roles in the order they're listed, by the client name each
/// process said hello with: the daemon is the `server`.
const ROLES: [&str; 5] = ["server", "window", "api", "account_service", "other"];

fn role_of(client: &str) -> &'static str {
    match client {
        "tui" => "window",
        // rakumux-api's own connection, and the ones it makes for other
        // machines reaching this one (`/v1/daemon`).
        "api" | session_core::api::MACHINE_CLIENT => "api",
        "rakumux-status-service" => "account_service",
        _ => "other",
    }
}

impl<'a> Upload<'a> {
    /// `now`'s reading as `machine`'s upload, its rates from `then`, the
    /// reading before it; sessions only those `listed`.
    fn of(
        machine: &'a str,
        then: &ResourceSample,
        now: &ResourceSample,
        listed: &dyn Fn(&SessionId) -> bool,
    ) -> Self {
        let seconds = now
            .at_ms
            .checked_sub(then.at_ms)
            .filter(|ms| *ms > 0)
            .map(|ms| ms as f64 / 1000.0);
        let earlier = by_pid(then);
        let rates =
            |process: &ProcessUsage| rates_of(process, earlier.get(&process.pid).copied(), seconds);

        let mut roles: HashMap<&'static str, (u32, Rates)> = HashMap::new();
        let mut count = |role: &'static str, process: &ProcessUsage| {
            let (processes, total) = roles.entry(role).or_default();
            *processes = processes.saturating_add(1);
            total.add(&rates(process));
        };
        count("server", &now.daemon);
        for client in &now.clients {
            count(role_of(&client.client), &client.process);
        }
        let rakumux = ROLES
            .into_iter()
            .filter_map(|role| {
                let (processes, total) = roles.get(role)?;
                Some(Role {
                    role,
                    processes: (*processes).clamp(1, MAX_ROLE_PROCESSES),
                    usage: total.usage(),
                })
            })
            .collect();

        let mut sessions: Vec<(String, u32, Rates)> = now
            .sessions
            .iter()
            .filter(|session| !session.processes.is_empty() && listed(&session.id))
            .map(|session| {
                let mut total = Rates::default();
                for process in &session.processes {
                    total.add(&rates(process));
                }
                let processes = u32::try_from(session.processes.len()).unwrap_or(u32::MAX);
                (session.id.as_str().to_owned(), processes, total)
            })
            .collect();
        sessions.sort_by(|a, b| b.2.memory.cmp(&a.2.memory).then_with(|| a.0.cmp(&b.0)));
        sessions.truncate(MAX_SESSIONS);

        Upload {
            version: 1,
            machine,
            at: now.at_ms.clamp(EARLIEST_MS, LATEST_MS),
            host: host(&then.machine, &now.machine, seconds),
            rakumux,
            sessions: sessions
                .into_iter()
                .map(|(id, processes, total)| SessionTotal {
                    id,
                    processes: processes.clamp(1, MAX_SESSION_PROCESSES),
                    usage: total.usage(),
                })
                .collect(),
        }
    }
}

/// Every process of `sample` by pid: its name, CPU and network so far.
fn by_pid(sample: &ResourceSample) -> HashMap<u32, &ProcessUsage> {
    std::iter::once(&sample.daemon)
        .chain(sample.clients.iter().map(|client| &client.process))
        .chain(
            sample
                .sessions
                .iter()
                .flat_map(|session| session.processes.iter()),
        )
        .map(|process| (process.pid, process))
        .collect()
}

/// `process`'s use, its rates from `earlier` (the same pid `seconds` ago,
/// when it had the same name: a reused pid gets none).
fn rates_of(process: &ProcessUsage, earlier: Option<&ProcessUsage>, seconds: Option<f64>) -> Rates {
    let earlier = earlier.filter(|earlier| earlier.name == process.name);
    let rate =
        |now: Option<u64>, then: Option<u64>| Some(now?.saturating_sub(then?) as f64 / seconds?);
    Rates {
        // ms of CPU a second, as a percentage of one CPU.
        cpu: rate(Some(process.cpu_ms), earlier.map(|e| e.cpu_ms)).map(|ms| ms / 10.0),
        memory: process.rss_bytes,
        rx: rate(process.net_rx_bytes, earlier.and_then(|e| e.net_rx_bytes)),
        tx: rate(process.net_tx_bytes, earlier.and_then(|e| e.net_tx_bytes)),
    }
}

/// The machine's counters as the contract's `host`.
fn host(then: &MachineUsage, now: &MachineUsage, seconds: Option<f64>) -> Host {
    let rate = |now: Option<u64>, then: Option<u64>| {
        Some(per_second(now?.saturating_sub(then?) as f64 / seconds?))
    };
    let cpu = (|| {
        let busy = now.cpu_busy_ms?.checked_sub(then.cpu_busy_ms?)?;
        let total = now.cpu_total_ms?.checked_sub(then.cpu_total_ms?)?;
        (total > 0).then(|| tenths((busy as f64 * 100.0 / total as f64).clamp(0.0, 100.0)))
    })();
    Host {
        cpus: now.cpus.clamp(1, MAX_CPUS),
        cpu_percent: cpu,
        memory_used_bytes: now
            .memory_total_bytes
            .zip(now.memory_available_bytes)
            .map(|(total, available)| total.saturating_sub(available).min(MAX_BYTES)),
        memory_total_bytes: now.memory_total_bytes.map(|total| total.min(MAX_BYTES)),
        net_rx_bytes_per_s: rate(now.net_rx_bytes, then.net_rx_bytes),
        net_tx_bytes_per_s: rate(now.net_tx_bytes, then.net_tx_bytes),
    }
}

#[cfg(test)]
mod tests;
