//! This machine's heartbeats for rakumux.com (`contracts/feed/`
//! `heartbeats-upload`): once a session list's answer said the cloud takes
//! them (`accepts` has `heartbeats`), while the daemon runs heartbeats
//! (capability `heartbeats`), the status service reads the daemon's
//! heartbeats about every [`READ_EVERY`] and uploads the whole list when it
//! changes, and every [`HEARTBEAT`] meanwhile. Each goes as rakumux.com shows
//! it: its folder's name (never a path), its schedule, whether it's on or
//! waits for approval, and its command and task masked as chat text is (the
//! home directory as `~`). Driven by the owner thread with its own clock.

use std::time::Duration;

use serde::Serialize;
use session_core::api::ErrorCode;
use session_core::heartbeat::{Heartbeat, RunOutcome, Schedule, Weekday};
use session_core::model::Creator;

use super::wire::{self, FeedFailure, FeedRequest};
use crate::cloud::FeedHttpReply;

/// What this machine says with its session list when it uploads its
/// heartbeats and carries out the actions asked of them (`capabilities`).
pub(crate) const HEARTBEATS: &str = "heartbeats";
/// How often the daemon's heartbeats are read.
const READ_EVERY: Duration = Duration::from_secs(5);
/// How often an unchanged list is sent again.
const HEARTBEAT: Duration = Duration::from_secs(600);
/// The first and longest waits after a failed upload.
const RETRY_MIN: Duration = Duration::from_secs(5);
const RETRY_MAX: Duration = Duration::from_secs(300);
/// The contract's bounds.
const MAX_HEARTBEATS: usize = 50;
const MAX_COMMAND_BYTES: usize = 1024;
const MAX_PROMPT_BYTES: usize = 16 * 1024;

/// One heartbeat as rakumux.com lists it (`heartbeatState`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct HeartbeatState {
    id: String,
    name: String,
    folder: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    agent: Option<&'static str>,
    command: String,
    prompt: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    schedule: Option<ScheduleState>,
    enabled: bool,
    awaiting_approval: bool,
    by_agent: bool,
    updated_at: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    next_run_at: Option<i64>,
    runs: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_run: Option<LastRunState>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum ScheduleState {
    Every { minutes: u32 },
    Daily { at: String },
    Weekdays { days: Vec<&'static str>, at: String },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
struct LastRunState {
    run: u32,
    at: i64,
    outcome: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    session: Option<String>,
}

/// Unix seconds as the contract's milliseconds, within its range.
fn ms(seconds: i64) -> i64 {
    seconds
        .saturating_mul(1000)
        .clamp(1_577_836_800_000, 4_102_444_800_000)
}

/// A heartbeat's text as the feed takes it: escapes and controls removed,
/// keys, credential assignments and tokens masked, the home directory as
/// `~`, cut to `max` bytes on a character boundary.
fn masked(text: &str, home: Option<&str>, max: usize) -> String {
    let mut text = crate::cloud::chat_text(text).unwrap_or_default();
    if let Some(home) = home.filter(|home| home.len() > 1) {
        text = text.replace(home, "~");
    }
    if text.len() > max {
        let mut end = max;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
    }
    text
}

fn is_time(at: &str) -> bool {
    let bytes = at.as_bytes();
    bytes.len() == 5
        && bytes[2] == b':'
        && at[..2].parse::<u32>().is_ok_and(|hour| hour < 24)
        && at[3..].parse::<u32>().is_ok_and(|minute| minute < 60)
        && at[..2]
            .bytes()
            .chain(at[3..].bytes())
            .all(|b| b.is_ascii_digit())
}

impl ScheduleState {
    fn of(schedule: &Schedule) -> Option<Self> {
        match schedule {
            Schedule::Every { minutes } => (5..=10_080)
                .contains(minutes)
                .then_some(Self::Every { minutes: *minutes }),
            Schedule::Daily { at } => is_time(at).then(|| Self::Daily { at: at.clone() }),
            Schedule::Weekdays { days, at } => {
                let mut names: Vec<&'static str> = Vec::new();
                for day in Weekday::ALL {
                    if days.contains(&day) {
                        names.push(day.short());
                    }
                }
                (is_time(at) && !names.is_empty()).then(|| Self::Weekdays {
                    days: names,
                    at: at.clone(),
                })
            }
            Schedule::Unknown => None,
        }
    }
}

impl HeartbeatState {
    /// `heartbeat` within the contract; `None` when its id, name or folder
    /// can't go.
    pub(crate) fn of(heartbeat: &Heartbeat, home: Option<&str>) -> Option<Self> {
        let id = heartbeat.id.as_str().to_owned();
        if !crate::feed::is_entry_id(&id) {
            return None;
        }
        let folder = heartbeat
            .scope
            .path()
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .and_then(|name| wire::label(&name, 128))
            .filter(|name| !name.contains('/'))?;
        let outcome = |outcome: RunOutcome| match outcome {
            RunOutcome::Started => Some("started"),
            RunOutcome::TaskGiven => Some("task_given"),
            RunOutcome::NoAgent => Some("no_agent"),
            RunOutcome::Skipped => Some("skipped"),
            RunOutcome::Failed => Some("failed"),
            RunOutcome::Unknown => None,
        };
        Some(Self {
            id,
            name: wire::label(&heartbeat.name, 64)?,
            folder,
            agent: heartbeat.agent.as_deref().and_then(super::feed_agent),
            command: masked(&heartbeat.command, home, MAX_COMMAND_BYTES),
            prompt: masked(&heartbeat.prompt, home, MAX_PROMPT_BYTES),
            schedule: heartbeat.schedule.as_ref().and_then(ScheduleState::of),
            enabled: heartbeat.enabled,
            awaiting_approval: heartbeat.awaiting_approval,
            by_agent: matches!(heartbeat.created_by, Creator::Session { .. }),
            updated_at: ms(heartbeat.updated_at),
            next_run_at: heartbeat.next_run_at.map(ms),
            runs: heartbeat.runs,
            last_run: heartbeat.last_run.as_ref().and_then(|run| {
                Some(LastRunState {
                    run: (run.run >= 1).then_some(run.run)?,
                    at: ms(run.at),
                    outcome: outcome(run.outcome)?,
                    session: run
                        .session_id
                        .as_ref()
                        .map(|id| id.as_str().to_owned())
                        .filter(|id| crate::feed::is_entry_id(id)),
                })
            }),
        })
    }
}

#[derive(Serialize)]
struct Upload<'a> {
    version: u8,
    machine: &'a str,
    heartbeats: &'a [HeartbeatState],
    at: i64,
}

#[derive(serde::Deserialize)]
struct StoredWire {
    version: u8,
    stored: u32,
}

/// The answer to an upload.
pub(crate) fn stored(reply: &FeedHttpReply) -> Result<(), FeedFailure> {
    if reply.status != 200 {
        return Err(wire::failure(reply));
    }
    let stored: StoredWire =
        serde_json::from_slice(&reply.body).map_err(|_| FeedFailure::Invalid)?;
    if stored.version != 1 || stored.stored as usize > MAX_HEARTBEATS {
        return Err(FeedFailure::Invalid);
    }
    Ok(())
}

/// What to do next for the heartbeats list.
#[derive(Debug, PartialEq)]
pub(crate) enum HeartbeatWork {
    /// Ask the daemon for its heartbeats.
    Read,
    /// Upload the list.
    Upload(FeedRequest),
}

/// The heartbeats list's state on the owner thread.
pub(crate) struct HeartbeatSync {
    machine: String,
    home: Option<String>,
    enabled: bool,
    /// The daemon answered `heartbeat_list` with `unsupported`: not asked
    /// again until it reconnects.
    unsupported: bool,
    reading: bool,
    next_read: Duration,
    /// The list as last read.
    current: Option<Vec<HeartbeatState>>,
    /// What rakumux.com was last sent, since it was enabled.
    sent: Option<Vec<HeartbeatState>>,
    sending: Option<Vec<HeartbeatState>>,
    heartbeat_at: Duration,
    retry_at: Duration,
    retry: Duration,
}

impl HeartbeatSync {
    pub fn new(machine: String, home: Option<String>) -> Self {
        Self {
            machine,
            home,
            enabled: false,
            unsupported: false,
            reading: false,
            next_read: Duration::ZERO,
            current: None,
            sent: None,
            sending: None,
            heartbeat_at: Duration::ZERO,
            retry_at: Duration::ZERO,
            retry: RETRY_MIN,
        }
    }

    /// Follow the account, the cloud and the daemon: signed in with the feed
    /// on, this machine's name confirmed, the cloud taking heartbeats and
    /// the daemon running them. Turned on again, the list goes at once.
    pub fn set_enabled(&mut self, enabled: bool, now: Duration) {
        if enabled == self.enabled {
            return;
        }
        self.enabled = enabled;
        self.sent = None;
        self.current = None;
        self.retry = RETRY_MIN;
        self.retry_at = now;
        self.next_read = now;
    }

    /// The daemon (re)connected: ask it again.
    pub fn connected(&mut self, now: Duration) {
        self.unsupported = false;
        self.next_read = now;
    }

    /// A heartbeat changed here (an action carried out): read them again now.
    pub fn changed(&mut self, now: Duration) {
        self.next_read = now;
    }

    pub fn rename(&mut self, machine: String, now: Duration) {
        self.machine = machine;
        self.sent = None;
        self.retry_at = now;
    }

    /// The next thing to do, if anything is due.
    pub fn next_work(&mut self, now: Duration, wall_ms: i64) -> Option<HeartbeatWork> {
        if !self.enabled || self.unsupported {
            return None;
        }
        if !self.reading && now >= self.next_read {
            self.reading = true;
            return Some(HeartbeatWork::Read);
        }
        let list = self.current.as_ref()?;
        if self.sending.is_some() || now < self.retry_at {
            return None;
        }
        if self.sent.as_ref() == Some(list) && now < self.heartbeat_at {
            return None;
        }
        let body = serde_json::to_vec(&Upload {
            version: 1,
            machine: &self.machine,
            heartbeats: list,
            at: wall_ms.max(1_577_836_800_000),
        })
        .ok()?;
        self.sending = Some(list.clone());
        Some(HeartbeatWork::Upload(FeedRequest::Heartbeats { body }))
    }

    /// The daemon answered a read.
    pub fn read_done(&mut self, result: Result<Vec<Heartbeat>, ErrorCode>, now: Duration) {
        self.reading = false;
        self.next_read = now + READ_EVERY;
        match result {
            Ok(heartbeats) => {
                let mut list: Vec<HeartbeatState> = heartbeats
                    .iter()
                    .filter_map(|heartbeat| HeartbeatState::of(heartbeat, self.home.as_deref()))
                    .collect();
                list.sort_by(|a, b| a.id.cmp(&b.id));
                list.truncate(MAX_HEARTBEATS);
                if self.enabled {
                    self.current = Some(list);
                }
            }
            Err(ErrorCode::Unsupported) => self.unsupported = true,
            Err(_) => {}
        }
    }

    /// An upload finished; `current` says whether it was made for the account
    /// still signed in.
    pub fn done(&mut self, current: bool, outcome: Result<(), FeedFailure>, now: Duration) {
        let sent = self.sending.take();
        if !current || !self.enabled {
            return;
        }
        match outcome {
            Ok(()) => {
                self.sent = sent;
                self.retry = RETRY_MIN;
                self.heartbeat_at = now + HEARTBEAT;
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

#[cfg(test)]
mod tests {
    use super::*;
    use session_core::heartbeat::HeartbeatRun;
    use session_core::ids::{HeartbeatId, SessionId};
    use session_core::model::ScopeRoot;

    const ID: &str = "01K8A2B3C4D5E6F7G8H9J0K1M2";

    fn heartbeat() -> Heartbeat {
        Heartbeat {
            id: HeartbeatId::from_string(ID),
            scope: ScopeRoot::from_canonical("/home/me/repos/web".into()),
            name: "nightly".into(),
            agent: Some("claude".into()),
            command: "claude --add-dir /home/me/notes".into(),
            prompt: "use OPENAI_API_KEY=sk-abcdefghijklmnopqrstuvwxyz0123456789 and\nopen a PR"
                .into(),
            schedule: Some(Schedule::Weekdays {
                days: vec![Weekday::Fri, Weekday::Mon],
                at: "09:30".into(),
            }),
            enabled: false,
            awaiting_approval: true,
            created_by: Creator::Session {
                id: SessionId::generate(),
            },
            created_at: 1_761_220_000,
            updated_at: 1_761_220_001,
            next_run_at: None,
            runs: 2,
            last_run: Some(HeartbeatRun {
                run: 2,
                at: 1_761_200_000,
                session_id: None,
                outcome: RunOutcome::TaskGiven,
                detail: Some("never sent".into()),
            }),
        }
    }

    #[test]
    fn a_heartbeat_goes_with_its_folder_name_and_its_text_masked() {
        let state = HeartbeatState::of(&heartbeat(), Some("/home/me")).expect("state");
        let value = serde_json::to_value(&state).expect("json");
        assert_eq!(value["folder"], "web");
        assert_eq!(value["agent"], "claude-code");
        assert_eq!(value["command"], "claude --add-dir ~/notes");
        assert!(!value["prompt"].as_str().unwrap().contains("sk-abc"));
        assert!(value["prompt"].as_str().unwrap().ends_with("\nopen a PR"));
        assert_eq!(
            value["schedule"],
            serde_json::json!({"kind": "weekdays", "days": ["mon", "fri"], "at": "09:30"})
        );
        assert_eq!(value["by_agent"], true);
        assert_eq!(value["updated_at"], 1_761_220_001_000i64);
        assert_eq!(value["last_run"]["outcome"], "task_given");
        assert!(value["last_run"].get("detail").is_none());
        assert!(value.get("scope").is_none());
    }

    #[test]
    fn the_list_is_read_then_uploaded_when_it_changes_and_every_ten_minutes() {
        let now = Duration::from_secs(10);
        let mut sync = HeartbeatSync::new("studio".into(), None);
        assert_eq!(sync.next_work(now, 0), None);
        sync.set_enabled(true, now);
        assert_eq!(sync.next_work(now, 0), Some(HeartbeatWork::Read));
        sync.read_done(Ok(vec![heartbeat()]), now);
        assert!(matches!(
            sync.next_work(now, 0),
            Some(HeartbeatWork::Upload(FeedRequest::Heartbeats { .. }))
        ));
        sync.done(true, Ok(()), now);
        assert_eq!(sync.next_work(now, 0), None);
        let later = now + READ_EVERY;
        assert_eq!(sync.next_work(later, 0), Some(HeartbeatWork::Read));
        sync.read_done(Ok(vec![heartbeat()]), later);
        assert_eq!(sync.next_work(later, 0), None);
        sync.read_done(Ok(Vec::new()), later);
        assert!(matches!(
            sync.next_work(later, 0),
            Some(HeartbeatWork::Upload(_))
        ));
        sync.done(true, Ok(()), later);
        let read = later + HEARTBEAT;
        assert_eq!(sync.next_work(read, 0), Some(HeartbeatWork::Read));
        sync.read_done(Ok(Vec::new()), read);
        assert!(matches!(
            sync.next_work(read, 0),
            Some(HeartbeatWork::Upload(_))
        ));
    }

    #[test]
    fn a_daemon_without_heartbeats_isnt_asked_again_until_it_reconnects() {
        let now = Duration::from_secs(1);
        let mut sync = HeartbeatSync::new("studio".into(), None);
        sync.set_enabled(true, now);
        sync.next_work(now, 0);
        sync.read_done(Err(ErrorCode::Unsupported), now);
        assert_eq!(sync.next_work(now + HEARTBEAT, 0), None);
        sync.connected(now);
        assert_eq!(sync.next_work(now, 0), Some(HeartbeatWork::Read));
    }
}
