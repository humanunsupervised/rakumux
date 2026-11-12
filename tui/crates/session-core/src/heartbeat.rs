//! Heartbeats: tasks Rakumux runs on a clock. A heartbeat is a folder, an
//! agent command, a task prompt and a schedule; each time it's due the daemon
//! starts a new session (a **run**) in that folder, runs the command and
//! gives the agent the task once its prompt shows (capability
//! [`CAPABILITY_HEARTBEATS`](crate::api::CAPABILITY_HEARTBEATS)).
//!
//! A heartbeat an agent made or changed (a session caller, or anything that
//! came through `rakumux-mcp`) waits for the user's approval, paused, until
//! the user enables it.

use crate::ids::{HeartbeatId, SessionId};
use crate::model::{Creator, ScopeRoot};
use serde::{Deserialize, Serialize};
use std::fmt;

/// Heartbeats a user may have, in every folder together.
pub const MAX_HEARTBEATS: usize = 50;
/// The shortest `every` a schedule takes, in minutes.
pub const MIN_EVERY_MINUTES: u32 = 5;
/// The longest `every`, in minutes (a week).
pub const MAX_EVERY_MINUTES: u32 = 7 * 24 * 60;
/// The longest task prompt, in UTF-8 bytes.
pub const MAX_PROMPT_BYTES: usize = 16 * 1024;
/// The longest command, in UTF-8 bytes.
pub const MAX_COMMAND_BYTES: usize = 1024;
/// Runs of a heartbeat kept in the session list; older ended ones go to the
/// session history.
pub const RUNS_KEPT: u32 = 10;

/// A heartbeat, as every client sees it. Times are Unix seconds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Heartbeat {
    pub id: HeartbeatId,
    /// The folder its runs start in.
    pub scope: ScopeRoot,
    /// Unique within its folder; runs are named `<name> #<n>`.
    pub name: String,
    /// The agent its command runs, as Rakumux names it (`claude`, `codex`,
    /// `cursor-agent`, …), when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    /// What each run types into its login shell to start the agent, flags
    /// included. Empty until set.
    #[serde(default)]
    pub command: String,
    /// The task given to the agent once its prompt shows. Empty until set.
    #[serde(default)]
    pub prompt: String,
    /// When it runs. None until set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schedule: Option<Schedule>,
    /// It runs on its schedule.
    #[serde(default)]
    pub enabled: bool,
    /// An agent made or changed it: paused until the user enables it.
    #[serde(default)]
    pub awaiting_approval: bool,
    pub created_by: Creator,
    pub created_at: i64,
    pub updated_at: i64,
    /// When it runs next; None while paused or incomplete.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_run_at: Option<i64>,
    /// Runs started so far (the last one's number).
    #[serde(default)]
    pub runs: u32,
    /// What its last due time did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_run: Option<HeartbeatRun>,
}

impl Heartbeat {
    /// It has everything a run needs: a command, a prompt and a schedule
    /// Rakumux knows.
    pub fn is_complete(&self) -> bool {
        !self.command.trim().is_empty()
            && !self.prompt.trim().is_empty()
            && self
                .schedule
                .as_ref()
                .is_some_and(|schedule| !matches!(schedule, Schedule::Unknown))
    }

    /// `hb_<id>`, as a person reads and types it.
    pub fn handle(&self) -> String {
        format!("hb_{}", self.id)
    }
}

/// What a heartbeat's due time did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HeartbeatRun {
    /// The run's number (`<name> #<n>`); 0 for a due time that started none.
    pub run: u32,
    /// Unix seconds.
    pub at: i64,
    /// The run's session, when one started.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<SessionId>,
    pub outcome: RunOutcome,
    /// Why it failed or was skipped, for people.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// What became of a run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunOutcome {
    /// Its session started; the agent hasn't had the task yet.
    Started,
    /// The task was typed into the agent and sent.
    TaskGiven,
    /// The agent never showed its prompt; the task wasn't typed.
    NoAgent,
    /// Not started: the previous run was still at work or asking something.
    Skipped,
    /// Not started: its session couldn't be (the folder is gone, …).
    Failed,
    #[serde(other)]
    Unknown,
}

impl RunOutcome {
    /// One or two words for a list.
    pub fn label(self) -> &'static str {
        match self {
            RunOutcome::Started => "starting",
            RunOutcome::TaskGiven => "ran",
            RunOutcome::NoAgent => "agent didn't start",
            RunOutcome::Skipped => "skipped",
            RunOutcome::Failed => "failed",
            RunOutcome::Unknown => "unknown",
        }
    }
}

/// Which heartbeat a session is a run of (`SessionView::heartbeat`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HeartbeatRunRef {
    pub heartbeat: HeartbeatId,
    pub run: u32,
}

/// When a heartbeat runs, in this machine's local time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Schedule {
    /// Every `minutes` (at least [`MIN_EVERY_MINUTES`]), counted from the
    /// last due time.
    Every { minutes: u32 },
    /// Every day at `at` (`HH:MM`).
    Daily { at: String },
    /// On `days` at `at` (`HH:MM`).
    Weekdays { days: Vec<Weekday>, at: String },
    /// From a newer Rakumux: never runs here.
    #[serde(other)]
    Unknown,
}

impl Schedule {
    /// The schedule's time of day as (hour, minute), when it has one.
    pub fn time_of_day(&self) -> Option<(u32, u32)> {
        match self {
            Schedule::Daily { at } | Schedule::Weekdays { at, .. } => parse_time(at),
            Schedule::Every { .. } | Schedule::Unknown => None,
        }
    }

    /// Why this schedule can't be used, if it can't.
    pub fn problem(&self) -> Option<String> {
        match self {
            Schedule::Every { minutes } if *minutes < MIN_EVERY_MINUTES => Some(format!(
                "a heartbeat runs at most every {MIN_EVERY_MINUTES} minutes"
            )),
            Schedule::Every { minutes } if *minutes > MAX_EVERY_MINUTES => {
                Some("a heartbeat runs at least once a week".into())
            }
            Schedule::Every { .. } => None,
            Schedule::Daily { at } => parse_time(at).is_none().then(|| bad_time(at)),
            Schedule::Weekdays { days, at } => {
                if days.is_empty() || days.contains(&Weekday::Unknown) {
                    Some("name the days: mon, tue, wed, thu, fri, sat, sun".into())
                } else {
                    parse_time(at).is_none().then(|| bad_time(at))
                }
            }
            Schedule::Unknown => Some("this Rakumux doesn't know that schedule".into()),
        }
    }

    /// Parse an interval as people write it: `30m`, `2h`, `1d`, `45`
    /// (minutes).
    pub fn every(text: &str) -> Option<Schedule> {
        let text = text.trim();
        let (digits, unit) = match text.char_indices().find(|(_, c)| !c.is_ascii_digit()) {
            Some((at, _)) => text.split_at(at),
            None => (text, "m"),
        };
        let n: u32 = digits.parse().ok()?;
        let minutes = match unit.trim() {
            "m" | "min" | "mins" | "minute" | "minutes" => n,
            "h" | "hr" | "hrs" | "hour" | "hours" => n.checked_mul(60)?,
            "d" | "day" | "days" => n.checked_mul(24 * 60)?,
            _ => return None,
        };
        Some(Schedule::Every { minutes })
    }
}

impl fmt::Display for Schedule {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Schedule::Every { minutes } if minutes % (24 * 60) == 0 => {
                write!(f, "every {}d", minutes / (24 * 60))
            }
            Schedule::Every { minutes } if minutes % 60 == 0 => {
                write!(f, "every {}h", minutes / 60)
            }
            Schedule::Every { minutes } => write!(f, "every {minutes}m"),
            Schedule::Daily { at } => write!(f, "daily {at}"),
            Schedule::Weekdays { days, at } => {
                let days: Vec<&str> = days.iter().map(|day| day.short()).collect();
                write!(f, "{} {at}", days.join(","))
            }
            Schedule::Unknown => f.write_str("unknown schedule"),
        }
    }
}

fn bad_time(at: &str) -> String {
    format!("{at:?} isn't a time of day: write it as HH:MM, 24-hour")
}

/// `HH:MM` (24-hour; `9:05` too) as (hour, minute).
pub fn parse_time(text: &str) -> Option<(u32, u32)> {
    let (hour, minute) = text.trim().split_once(':')?;
    if hour.is_empty() || hour.len() > 2 || minute.len() != 2 {
        return None;
    }
    let hour: u32 = hour.parse().ok()?;
    let minute: u32 = minute.parse().ok()?;
    (hour < 24 && minute < 60).then_some((hour, minute))
}

/// A day of the week.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Weekday {
    Mon,
    Tue,
    Wed,
    Thu,
    Fri,
    Sat,
    Sun,
    #[serde(other)]
    Unknown,
}

impl Weekday {
    /// Monday first.
    pub const ALL: [Weekday; 7] = [
        Weekday::Mon,
        Weekday::Tue,
        Weekday::Wed,
        Weekday::Thu,
        Weekday::Fri,
        Weekday::Sat,
        Weekday::Sun,
    ];

    /// `mon`, `tue`, …
    pub fn short(self) -> &'static str {
        match self {
            Weekday::Mon => "mon",
            Weekday::Tue => "tue",
            Weekday::Wed => "wed",
            Weekday::Thu => "thu",
            Weekday::Fri => "fri",
            Weekday::Sat => "sat",
            Weekday::Sun => "sun",
            Weekday::Unknown => "?",
        }
    }

    /// 0 for Monday … 6 for Sunday.
    pub fn index(self) -> Option<u32> {
        Weekday::ALL
            .iter()
            .position(|day| *day == self)
            .map(|i| i as u32)
    }

    /// A day as people write it (`mon`, `Monday`, `tues`), or a list of
    /// them (`mon,wed,fri`; `weekdays`; `weekends`).
    pub fn parse_list(text: &str) -> Option<Vec<Weekday>> {
        let mut days = Vec::new();
        for word in text.split([',', ' ']).filter(|w| !w.is_empty()) {
            let word = word.to_ascii_lowercase();
            match word.as_str() {
                "weekdays" => days.extend(&Weekday::ALL[..5]),
                "weekends" => days.extend(&Weekday::ALL[5..]),
                _ => days.push(
                    *Weekday::ALL
                        .iter()
                        .find(|day| word.len() >= 3 && day.long().starts_with(&word))?,
                ),
            }
        }
        days.sort();
        days.dedup();
        (!days.is_empty()).then_some(days)
    }

    fn long(self) -> &'static str {
        match self {
            Weekday::Mon => "monday",
            Weekday::Tue => "tuesday",
            Weekday::Wed => "wednesday",
            Weekday::Thu => "thursday",
            Weekday::Fri => "friday",
            Weekday::Sat => "saturday",
            Weekday::Sun => "sunday",
            Weekday::Unknown => "",
        }
    }
}

/// `heartbeat_create`: a new heartbeat. Everything is optional, so an empty
/// one can be made and filled in later (`heartbeat_update`).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct HeartbeatSpec {
    /// Its folder. User callers: required. Session callers: their own when
    /// omitted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<ScopeRoot>,
    /// None → `Heartbeat`, `Heartbeat 2`, … within the folder.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(flatten)]
    pub fields: HeartbeatFields,
    /// Start it enabled (user callers only, and only a complete heartbeat;
    /// one an agent makes always waits for approval).
    #[serde(default, skip_serializing_if = "is_false")]
    pub enabled: bool,
}

/// `heartbeat_update`: what to change; fields left out stay.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HeartbeatEdit {
    pub id: HeartbeatId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(flatten)]
    pub fields: HeartbeatFields,
}

/// The fields `heartbeat_create` and `heartbeat_update` set.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct HeartbeatFields {
    /// The agent (`claude`, `codex`, …). Without a command, its command is
    /// the one the user last started that agent with in the folder (the
    /// flags that shape a session kept), else the agent's name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    /// What a run types into its shell to start the agent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schedule: Option<Schedule>,
    /// Made or changed for an agent (`rakumux-mcp` sets it): it waits for
    /// the user's approval whoever the caller is.
    #[serde(default, skip_serializing_if = "is_false")]
    pub by_agent: bool,
}

impl HeartbeatFields {
    /// Whether this changes what a run does (and an agent's change then
    /// waits for approval).
    pub fn changes_runs(&self) -> bool {
        self.agent.is_some()
            || self.command.is_some()
            || self.prompt.is_some()
            || self.schedule.is_some()
    }
}

fn is_false(value: &bool) -> bool {
    !*value
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schedules_parse_and_print_as_people_write_them() {
        assert_eq!(
            Schedule::every("30m"),
            Some(Schedule::Every { minutes: 30 })
        );
        assert_eq!(
            Schedule::every("2h"),
            Some(Schedule::Every { minutes: 120 })
        );
        assert_eq!(
            Schedule::every("1d"),
            Some(Schedule::Every { minutes: 1440 })
        );
        assert_eq!(Schedule::every("45"), Some(Schedule::Every { minutes: 45 }));
        assert_eq!(Schedule::every("soon"), None);
        assert_eq!(Schedule::Every { minutes: 120 }.to_string(), "every 2h");
        assert_eq!(Schedule::Every { minutes: 90 }.to_string(), "every 90m");
        assert!(Schedule::Every { minutes: 4 }.problem().is_some());
        assert_eq!(parse_time("9:05"), Some((9, 5)));
        assert_eq!(parse_time("24:00"), None);
        assert_eq!(parse_time("9:5"), None);
        assert_eq!(
            Weekday::parse_list("Mon,wednesday fri"),
            Some(vec![Weekday::Mon, Weekday::Wed, Weekday::Fri])
        );
        assert_eq!(Weekday::parse_list("weekends").map(|d| d.len()), Some(2));
        assert_eq!(Weekday::parse_list("mo"), None);
        let weekdays = Schedule::Weekdays {
            days: vec![Weekday::Mon, Weekday::Fri],
            at: "09:00".into(),
        };
        assert_eq!(weekdays.to_string(), "mon,fri 09:00");
        assert!(Schedule::Daily { at: "9am".into() }.problem().is_some());
    }

    #[test]
    fn schedules_and_outcomes_from_a_newer_rakumux_are_unknown() {
        let schedule: Schedule =
            serde_json::from_str(r#"{"kind":"cron","expr":"* * * * *"}"#).unwrap();
        assert_eq!(schedule, Schedule::Unknown);
        let outcome: RunOutcome = serde_json::from_str(r#""queued""#).unwrap();
        assert_eq!(outcome, RunOutcome::Unknown);
        let json = serde_json::to_value(Schedule::Daily { at: "09:00".into() }).unwrap();
        assert_eq!(json, serde_json::json!({"kind": "daily", "at": "09:00"}));
    }

    #[test]
    fn a_spec_reads_its_fields_flat() {
        let spec: HeartbeatSpec = serde_json::from_str(
            r#"{"name":"deps","prompt":"update","schedule":{"kind":"every","minutes":30},"by_agent":true}"#,
        )
        .unwrap();
        assert_eq!(spec.fields.prompt.as_deref(), Some("update"));
        assert!(spec.fields.by_agent);
        let edit: HeartbeatEdit =
            serde_json::from_str(r#"{"id":"hb_01ABC","command":"claude"}"#).unwrap();
        assert_eq!(edit.id.as_str(), "01ABC");
        assert!(edit.fields.changes_runs());
    }
}
