//! Warnings and errors from this machine's Rakumux programs, for the owner's log viewer on
//! rakumux.com (`POST /api/v1/feed/logs`). The daemon, API, TUI and MCP server only write their local
//! log files; this service follows those files while the account's features read asks for them
//! (`logs`), masks each line and uploads it, so nothing but this service talks to rakumux.com. What it
//! has read up to is kept in `logs.json`; with no record of a file it starts at its end, so a machine
//! never uploads what it logged before it was asked. `tui-input.log` isn't one of the files: it holds
//! keystrokes' events.

use std::collections::{HashMap, VecDeque};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use super::wire::{self, FeedFailure, FeedRequest};
use crate::cloud::LogLevel;

const FILES: &[(&str, Source)] = &[
    ("ptyd.log", Source::Daemon),
    ("api.log", Source::Api),
    ("status-service.log", Source::AccountService),
    ("tui.log", Source::Tui),
    ("mcp.log", Source::Mcp),
];

const EVERY: Duration = Duration::from_secs(15);
/// At most this much of one file a scan.
const MAX_READ: u64 = 256 * 1024;
const MAX_PENDING: usize = 500;
const MAX_BATCH: usize = 100;
const MAX_MESSAGE: usize = 2_048;
const MAX_COMPONENT: usize = 64;
/// The same line again within this long is dropped: a failure that repeats is one event a minute.
const REPEAT: Duration = Duration::from_secs(60);
const MAX_RECENT: usize = 256;
const RETRY_MIN: Duration = Duration::from_secs(30);
const RETRY_MAX: Duration = Duration::from_secs(600);
/// rakumux.com said it doesn't want them although the features read did: ask no sooner than this.
const DECLINED_FOR: Duration = Duration::from_secs(600);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
enum Source {
    Tui,
    Daemon,
    Api,
    Mcp,
    AccountService,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
enum Severity {
    Error,
    Warn,
    Info,
    Debug,
}

impl Severity {
    fn within(self, level: LogLevel) -> bool {
        self <= match level {
            LogLevel::Error => Severity::Error,
            LogLevel::Warn => Severity::Warn,
            LogLevel::Info => Severity::Info,
            LogLevel::Debug => Severity::Debug,
        }
    }
}

#[derive(Debug, Serialize)]
struct Event {
    id: String,
    at: i64,
    source: Source,
    level: Severity,
    #[serde(skip_serializing_if = "Option::is_none")]
    component: Option<String>,
    message: String,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
struct Cursor {
    inode: u64,
    offset: u64,
}

/// One line of a log file, read.
#[derive(Debug, PartialEq, Eq)]
struct Line<'a> {
    at_ms: Option<i64>,
    level: Severity,
    component: Option<&'a str>,
    message: &'a str,
}

/// `<RFC 3339 UTC> <LEVEL> [<component>] <message>` (the component is optional), as the daemon, the API
/// and `session_core::logfile` write them; the account service's own lines have neither time nor
/// level (`rakumux-status: …`, a warning), and a panic's first line is an error. Anything else (a
/// backtrace's lines) isn't an event.
fn parse(line: &str) -> Option<Line<'_>> {
    if let Some((at, rest)) = line.split_once(' ') {
        if let Some(at_ms) = rfc3339_ms(at) {
            let (label, rest) = rest.trim_start().split_once(' ')?;
            let level = match label {
                "ERROR" => Severity::Error,
                "WARN" => Severity::Warn,
                "INFO" => Severity::Info,
                "DEBUG" => Severity::Debug,
                _ => return None,
            };
            let rest = rest.trim_start();
            let (component, message) = match rest
                .strip_prefix('[')
                .and_then(|tail| tail.split_once("] "))
            {
                Some((component, message)) => ((component != "-").then_some(component), message),
                None => (None, rest),
            };
            return Some(Line {
                at_ms: Some(at_ms),
                level,
                component,
                message,
            });
        }
    }
    if line.contains("panicked at") {
        return Some(Line {
            at_ms: None,
            level: Severity::Error,
            component: Some("panic"),
            message: line,
        });
    }
    line.strip_prefix("rakumux-status: ").map(|message| Line {
        at_ms: None,
        level: Severity::Warn,
        component: None,
        message,
    })
}

/// `2026-10-13T08:15:02.123Z` (the fraction optional) as Unix milliseconds.
fn rfc3339_ms(text: &str) -> Option<i64> {
    let text = text.strip_suffix('Z')?;
    let (date, time) = text.split_once('T')?;
    let mut date = date.splitn(3, '-').map(|part| part.parse::<i64>().ok());
    let (year, month, day) = (date.next()??, date.next()??, date.next()??);
    let (time, fraction) = time.split_once('.').unwrap_or((time, "0"));
    let mut time = time.splitn(3, ':').map(|part| part.parse::<i64>().ok());
    let (hour, minute, second) = (time.next()??, time.next()??, time.next()??);
    if !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || hour > 23
        || minute > 59
        || second > 60
    {
        return None;
    }
    let millis = format!("{fraction:0<3}").get(..3)?.parse::<i64>().ok()?;
    // Days from civil, Howard Hinnant's.
    let adjusted = if month <= 2 { year - 1 } else { year };
    let era = adjusted.div_euclid(400);
    let of_era = adjusted.rem_euclid(400);
    let of_year = (153 * (month + if month > 2 { -3 } else { 9 }) + 2) / 5 + day - 1;
    let of_cycle = of_era * 365 + of_era / 4 - of_era / 100 + of_year;
    let days = era * 146_097 + of_cycle - 719_468;
    Some((days * 86_400 + hour * 3_600 + minute * 60 + second) * 1_000 + millis)
}

/// `text` as it may leave this machine: controls removed, keys, credential assignments and tokens
/// masked, this user's home directory as `~`, within the contract's length; `None` when nothing is left.
fn masked(text: &str, home: Option<&str>, limit: usize) -> Option<String> {
    let clean = crate::cloud::chat_text(text)?;
    let clean = match home {
        Some(home) if !home.is_empty() => clean.replace(home, "~"),
        _ => clean,
    };
    let clean = clean.split_whitespace().collect::<Vec<_>>().join(" ");
    let cut: String = clean.chars().take(limit).collect();
    (!cut.is_empty()).then_some(cut)
}

fn wall_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Follows the log files and uploads them, one batch at a time. Driven by the owner thread.
pub struct LogSync {
    directory: PathBuf,
    state: PathBuf,
    home: Option<String>,
    machine: String,
    release: String,
    platform: String,
    level: Option<LogLevel>,
    cursors: HashMap<String, Cursor>,
    pending: VecDeque<Event>,
    recent: HashMap<(Source, String), Duration>,
    out: Option<usize>,
    next_scan: Duration,
    retry_at: Duration,
    retry: Duration,
    declined_until: Duration,
}

impl LogSync {
    /// Follow `directory`'s log files as `machine`, keeping where they're read up to in `state`.
    pub fn new(directory: PathBuf, state: PathBuf, machine: String, release: &str) -> Self {
        let cursors = std::fs::read(&state)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        Self {
            directory,
            state,
            home: std::env::var("HOME").ok(),
            machine,
            release: release.chars().take(32).collect(),
            platform: format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH),
            level: None,
            cursors,
            pending: VecDeque::new(),
            recent: HashMap::new(),
            out: None,
            next_scan: Duration::ZERO,
            retry_at: Duration::ZERO,
            retry: RETRY_MIN,
            declined_until: Duration::ZERO,
        }
    }

    /// This machine goes by `machine` in the feed from now on.
    pub fn rename(&mut self, machine: String) {
        self.machine = machine;
    }

    /// What the account's features read says: from what level to send, or nothing. Off, nothing is
    /// kept or followed; on again, each file is followed from where it ends then.
    pub fn set_level(&mut self, level: Option<LogLevel>, now: Duration) {
        if level == self.level {
            return;
        }
        if level.is_none() {
            self.pending.clear();
            self.cursors.clear();
            self.save();
        }
        self.level = level;
        self.next_scan = now;
    }

    /// The upload due now, if any.
    pub fn next_work(&mut self, now: Duration) -> Option<FeedRequest> {
        self.level?;
        if now < self.declined_until {
            return None;
        }
        if now >= self.next_scan {
            self.scan(now);
            self.next_scan = now + EVERY;
        }
        if self.out.is_some() || now < self.retry_at || self.pending.is_empty() {
            return None;
        }
        // A batch that doesn't fit the contract's body is cut until it does; one event always fits.
        let mut count = self.pending.len().min(MAX_BATCH);
        loop {
            let events: Vec<&Event> = self.pending.iter().take(count).collect();
            match wire::logs_upload(&self.machine, &self.release, &self.platform, &events) {
                Some(request) => {
                    self.out = Some(count);
                    return Some(request);
                }
                None if count > 1 => count /= 2,
                None => {
                    self.pending.pop_front();
                    return None;
                }
            }
        }
    }

    /// An upload finished; `current` says whether it was made for the account still signed in.
    pub fn done(&mut self, current: bool, outcome: Result<bool, FeedFailure>, now: Duration) {
        let Some(count) = self.out.take() else { return };
        if !current {
            return;
        }
        match outcome {
            Ok(wanted) => {
                self.pending.drain(..count.min(self.pending.len()));
                self.retry = RETRY_MIN;
                if !wanted {
                    self.pending.clear();
                    self.declined_until = now + DECLINED_FOR;
                }
            }
            Err(FeedFailure::RateLimited(after) | FeedFailure::Unavailable(after)) => {
                self.back_off(after, now)
            }
            // The contract refused them: they'd be refused again.
            Err(FeedFailure::Invalid) => {
                self.pending.drain(..count.min(self.pending.len()));
            }
            Err(_) => self.back_off(None, now),
        }
    }

    fn back_off(&mut self, after: Option<Duration>, now: Duration) {
        self.retry_at = now + after.unwrap_or(self.retry).max(self.retry);
        self.retry = (self.retry * 2).min(RETRY_MAX);
    }

    /// Read what's new in each file.
    fn scan(&mut self, now: Duration) {
        let Some(level) = self.level else { return };
        let mut changed = false;
        for (name, source) in FILES {
            let path = self.directory.join(name);
            let known = self.cursors.get(*name).copied();
            let Some((cursor, chunk)) = read_new(&path, known) else {
                // Not there yet: whatever it holds when it appears is new.
                if known.is_none() && !path.exists() {
                    self.cursors.insert((*name).to_owned(), Cursor::default());
                    changed = true;
                }
                continue;
            };
            changed |= known != Some(cursor);
            self.cursors.insert((*name).to_owned(), cursor);
            for line in chunk.lines() {
                self.take(line, *source, level, now);
            }
        }
        if changed {
            self.save();
        }
    }

    fn take(&mut self, line: &str, source: Source, level: LogLevel, now: Duration) {
        let Some(parsed) = parse(line) else { return };
        if !parsed.level.within(level) {
            return;
        }
        let home = self.home.as_deref();
        let Some(message) = masked(parsed.message, home, MAX_MESSAGE) else {
            return;
        };
        let key = (source, message.clone());
        if self
            .recent
            .get(&key)
            .is_some_and(|seen| now.saturating_sub(*seen) < REPEAT)
        {
            return;
        }
        if self.recent.len() >= MAX_RECENT {
            self.recent.clear();
        }
        self.recent.insert(key, now);
        if self.pending.len() >= MAX_PENDING {
            self.pending.pop_front();
        }
        self.pending.push_back(Event {
            id: session_core::ids::SessionId::generate().as_str().to_owned(),
            at: parsed.at_ms.unwrap_or_else(wall_ms),
            source,
            level: parsed.level,
            component: parsed
                .component
                .and_then(|component| masked(component, home, MAX_COMPONENT)),
            message,
        });
    }

    fn save(&self) {
        let Ok(bytes) = serde_json::to_vec(&self.cursors) else {
            return;
        };
        if crate::feed::write_private(&self.state, &bytes).is_err() {
            eprintln!("rakumux-status: saving where the logs are read up to failed");
        }
    }
}

/// What's new in `path` since `known`, as whole lines, and where that leaves the file. With no
/// record, the file's end: nothing from before it was asked for. A file that's another file now, or
/// shorter (rotated), is read from its start.
fn read_new(path: &Path, known: Option<Cursor>) -> Option<(Cursor, String)> {
    let mut file = File::open(path).ok()?;
    let meta = file.metadata().ok()?;
    if !meta.is_file() {
        return None;
    }
    let (inode, size) = (meta.ino(), meta.size());
    let Some(known) = known else {
        return Some((
            Cursor {
                inode,
                offset: size,
            },
            String::new(),
        ));
    };
    let from = if known.inode == inode && known.offset <= size {
        known.offset
    } else {
        0
    };
    if from == size {
        return Some((
            Cursor {
                inode,
                offset: size,
            },
            String::new(),
        ));
    }
    file.seek(SeekFrom::Start(from)).ok()?;
    let mut bytes = Vec::new();
    file.take(MAX_READ).read_to_end(&mut bytes).ok()?;
    let Some(end) = bytes.iter().rposition(|byte| *byte == b'\n') else {
        // A line longer than a whole read is passed over; a shorter one waits for its newline.
        let offset = if bytes.len() as u64 >= MAX_READ {
            from + bytes.len() as u64
        } else {
            from
        };
        return Some((Cursor { inode, offset }, String::new()));
    };
    bytes.truncate(end + 1);
    let offset = from + bytes.len() as u64;
    Some((
        Cursor { inode, offset },
        String::from_utf8_lossy(&bytes).into_owned(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    struct Dir(PathBuf);
    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    fn dir() -> Dir {
        let path = std::env::temp_dir().join(format!(
            "logsync-{}",
            session_core::ids::SessionId::generate().as_str()
        ));
        std::fs::create_dir_all(&path).unwrap();
        Dir(path)
    }
    fn append(path: &Path, text: &str) {
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .unwrap()
            .write_all(text.as_bytes())
            .unwrap();
    }
    fn sync(root: &Dir) -> LogSync {
        std::fs::create_dir_all(root.0.join("state")).unwrap();
        let mut sync = LogSync::new(
            root.0.clone(),
            root.0.join("state/logs.json"),
            "studio".into(),
            "0.1.50",
        );
        sync.home = Some("/home/ada".into());
        sync
    }
    fn events(request: FeedRequest) -> Vec<serde_json::Value> {
        let FeedRequest::Logs { body } = request else {
            panic!("not an upload")
        };
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["machine"], "studio");
        value["events"].as_array().unwrap().clone()
    }
    const NOW: Duration = Duration::from_secs(1_000);

    #[test]
    fn parses_each_program_format() {
        assert_eq!(
            parse("2026-10-13T08:15:02.123Z ERROR [control] disk full"),
            Some(Line {
                at_ms: Some(1_791_879_302_123),
                level: Severity::Error,
                component: Some("control"),
                message: "disk full"
            })
        );
        assert_eq!(
            parse("2026-10-13T08:15:02Z WARN  listener closed"),
            Some(Line {
                at_ms: Some(1_791_879_302_000),
                level: Severity::Warn,
                component: None,
                message: "listener closed"
            })
        );
        assert_eq!(
            parse("2026-10-13T08:15:02.1Z WARN  [-] x"),
            Some(Line {
                at_ms: Some(1_791_879_302_100),
                level: Severity::Warn,
                component: None,
                message: "x"
            })
        );
        assert_eq!(
            parse("rakumux-status: saving titles failed"),
            Some(Line {
                at_ms: None,
                level: Severity::Warn,
                component: None,
                message: "saving titles failed"
            })
        );
        assert_eq!(
            parse("thread 'main' panicked at src/a.rs:1:2:")
                .unwrap()
                .level,
            Severity::Error
        );
        assert_eq!(parse("note: run with RUST_BACKTRACE=1"), None);
        assert_eq!(parse("   0: core::panicking::panic"), None);
    }

    #[test]
    fn starts_at_the_end_then_uploads_only_new_lines_at_or_above_the_level() {
        let root = dir();
        let log = root.0.join("ptyd.log");
        append(&log, "2026-10-13T08:15:02.123Z ERROR [a] from before\n");
        let mut sync = sync(&root);
        assert!(sync.next_work(NOW).is_none(), "not asked for");
        sync.set_level(Some(LogLevel::Warn), NOW);
        assert!(sync.next_work(NOW).is_none(), "nothing new");
        append(&log, concat!(
            "2026-10-13T08:15:03.000Z ERROR [control] cannot save /home/ada/x: token=abc123def456\n",
            "2026-10-13T08:15:04.000Z INFO  [control] chatter\n",
            "2026-10-13T08:15:05.000Z WARN  [-] partial",
        ));
        let events = events(sync.next_work(NOW + EVERY).unwrap());
        assert_eq!(events.len(), 1);
        assert_eq!(events[0]["source"], "daemon");
        assert_eq!(events[0]["level"], "error");
        assert_eq!(events[0]["component"], "control");
        let message = events[0]["message"].as_str().unwrap();
        assert!(message.starts_with("cannot save ~/x"), "{message}");
        assert!(!message.contains("abc123def456"), "{message}");
    }

    #[test]
    fn a_file_that_appears_later_is_read_from_its_start() {
        let root = dir();
        let mut sync = sync(&root);
        sync.set_level(Some(LogLevel::Warn), NOW);
        assert!(sync.next_work(NOW).is_none());
        append(
            &root.0.join("tui.log"),
            "2026-10-13T08:15:03.000Z ERROR [panic] first panic\n",
        );
        let events = events(sync.next_work(NOW + EVERY).unwrap());
        assert_eq!(events[0]["source"], "tui");
        assert_eq!(events[0]["message"], "first panic");
    }

    #[test]
    fn never_reads_the_input_log() {
        let root = dir();
        let mut sync = sync(&root);
        sync.set_level(Some(LogLevel::Debug), NOW);
        sync.next_work(NOW);
        append(
            &root.0.join("tui-input.log"),
            "2026-10-13T08:15:03.000Z ERROR [keys] typed a secret\n",
        );
        assert!(sync.next_work(NOW + EVERY).is_none());
    }

    #[test]
    fn a_rotated_file_is_read_from_its_start_and_cursors_survive_a_restart() {
        let root = dir();
        let log = root.0.join("api.log");
        append(&log, "2026-10-13T08:15:03Z WARN one\n");
        let mut first = sync(&root);
        first.set_level(Some(LogLevel::Warn), NOW);
        first.next_work(NOW);
        append(&log, "2026-10-13T08:15:04Z WARN two\n");
        assert_eq!(
            events(first.next_work(NOW + EVERY).unwrap())[0]["message"],
            "two"
        );
        first.done(true, Ok(true), NOW + EVERY);

        append(&log, "2026-10-13T08:15:05Z WARN three\n");
        let mut restarted = sync(&root);
        restarted.set_level(Some(LogLevel::Warn), NOW);
        assert_eq!(
            events(restarted.next_work(NOW).unwrap())[0]["message"],
            "three"
        );
        restarted.done(true, Ok(true), NOW);

        std::fs::rename(&log, root.0.join("api.log.1")).unwrap();
        append(&log, "2026-10-13T08:15:06Z WARN four\n");
        assert_eq!(
            events(restarted.next_work(NOW + EVERY).unwrap())[0]["message"],
            "four"
        );
    }

    #[test]
    fn a_repeated_line_is_one_event_a_minute() {
        let root = dir();
        let log = root.0.join("ptyd.log");
        let mut sync = sync(&root);
        sync.set_level(Some(LogLevel::Warn), NOW);
        sync.next_work(NOW);
        append(&log, "2026-10-13T08:15:03Z WARN machines: laptop unreachable\n2026-10-13T08:15:04Z WARN machines: laptop unreachable\n");
        assert_eq!(events(sync.next_work(NOW + EVERY).unwrap()).len(), 1);
        sync.done(true, Ok(true), NOW + EVERY);
        append(
            &log,
            "2026-10-13T08:15:05Z WARN machines: laptop unreachable\n",
        );
        assert!(sync.next_work(NOW + EVERY * 2).is_none());
        sync.done(true, Ok(true), NOW + EVERY * 2);
        append(
            &log,
            "2026-10-13T08:16:05Z WARN machines: laptop unreachable\n",
        );
        assert_eq!(events(sync.next_work(NOW + EVERY * 6).unwrap()).len(), 1);
    }

    #[test]
    fn failures_keep_the_batch_and_being_declined_stops_it() {
        let root = dir();
        let log = root.0.join("ptyd.log");
        let mut sync = sync(&root);
        sync.set_level(Some(LogLevel::Warn), NOW);
        sync.next_work(NOW);
        append(&log, "2026-10-13T08:15:03Z WARN first\n");
        assert_eq!(events(sync.next_work(NOW + EVERY).unwrap()).len(), 1);
        assert!(sync.next_work(NOW + EVERY).is_none(), "one at a time");
        sync.done(true, Err(FeedFailure::Unavailable(None)), NOW + EVERY);
        assert!(sync.next_work(NOW + EVERY).is_none(), "holds back");
        let later = NOW + EVERY + RETRY_MIN;
        assert_eq!(events(sync.next_work(later).unwrap()).len(), 1);
        sync.done(true, Ok(false), later);
        append(&log, "2026-10-13T08:15:03Z WARN second\n");
        assert!(sync.next_work(later + EVERY).is_none(), "declined");
        assert!(sync.pending.is_empty());
    }

    #[test]
    fn turned_off_it_forgets_and_starts_again_at_the_end() {
        let root = dir();
        let log = root.0.join("ptyd.log");
        let mut sync = sync(&root);
        sync.set_level(Some(LogLevel::Warn), NOW);
        sync.next_work(NOW);
        sync.set_level(None, NOW);
        append(&log, "2026-10-13T08:15:03Z WARN while off\n");
        assert!(sync.next_work(NOW + EVERY).is_none());
        sync.set_level(Some(LogLevel::Warn), NOW + EVERY);
        assert!(sync.next_work(NOW + EVERY).is_none());
    }
}
