//! What new sessions keep across a restart of the service, in a file (0600)
//! when given one: the requests handled here (started, being started, or
//! decided: never started again) and the reports rakumux.com hasn't taken
//! yet. Written before the daemon is asked to start a session, so a stop at
//! any point leaves a request started at most once. One the service stopped
//! starting before the daemon answered is never reported: rakumux.com
//! expires it, and the session, if the daemon made it, is listed as any.

use std::collections::{BTreeMap, VecDeque};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use super::Outcome;

/// Ids are kept this long: rakumux.com expires a request pending 10 minutes,
/// and reads after that never bring it.
const KEEP_MS: i64 = 60 * 60 * 1000;
/// Reports rakumux.com hasn't taken this long after they were decided are
/// dropped: their requests expired long ago.
const REPORT_KEEP_MS: i64 = 60 * 60 * 1000;
/// At most this many ids, the oldest dropped first.
const MAX_IDS: usize = 500;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct Report {
    pub id: String,
    pub outcome: Outcome,
    /// When it was decided (Unix ms).
    pub at: i64,
}

#[derive(Default, Serialize, Deserialize)]
struct KeptFile {
    version: u8,
    /// Requests started, being started or decided here, by id: when (Unix ms).
    #[serde(default)]
    handled: BTreeMap<String, i64>,
    /// Reports not yet taken by rakumux.com, oldest first.
    #[serde(default)]
    reports: VecDeque<Report>,
}

pub(super) struct Kept {
    path: Option<PathBuf>,
    file: KeptFile,
}

impl Kept {
    /// What `path` holds (nothing without it, or when it can't be read).
    pub fn load(path: Option<PathBuf>) -> Self {
        let file = path
            .as_ref()
            .and_then(|path| std::fs::read(path).ok())
            .and_then(|bytes| serde_json::from_slice::<KeptFile>(&bytes).ok())
            .filter(|file| file.version == 1)
            .unwrap_or_default();
        Self { path, file }
    }

    /// Whether request `id` was started, is being started or was decided here.
    pub fn handled(&self, id: &str) -> bool {
        self.file.handled.contains_key(id)
    }

    /// Request `id` is about to be handed to the daemon: from now on it's
    /// never started again, whatever happens to this service.
    pub fn creating(&mut self, id: &str) {
        self.file.handled.insert(id.to_owned(), now_ms());
        self.save();
    }

    /// Decided: never started (again), and rakumux.com is to hear `outcome`.
    pub fn decided(&mut self, id: &str, outcome: Outcome) {
        let now = now_ms();
        self.file.handled.insert(id.to_owned(), now);
        self.file.reports.retain(|report| report.id != id);
        self.file.reports.push_back(Report {
            id: id.to_owned(),
            outcome,
            at: now,
        });
        self.save();
    }

    /// The next report to send, if any.
    pub fn next_report(&self) -> Option<&Report> {
        self.file.reports.front()
    }

    /// The next report was taken, or refused for good.
    pub fn reported(&mut self) {
        self.file.reports.pop_front();
        self.save();
    }

    fn save(&mut self) {
        let now = now_ms();
        let ids = &mut self.file.handled;
        ids.retain(|_, at| now.saturating_sub(*at) < KEEP_MS);
        while ids.len() > MAX_IDS {
            let Some(oldest) = ids
                .iter()
                .min_by_key(|(_, at)| **at)
                .map(|(id, _)| id.clone())
            else {
                break;
            };
            ids.remove(&oldest);
        }
        self.file
            .reports
            .retain(|report| now.saturating_sub(report.at) < REPORT_KEEP_MS);
        let Some(path) = &self.path else {
            return;
        };
        self.file.version = 1;
        let Ok(bytes) = serde_json::to_vec(&self.file) else {
            return;
        };
        if crate::feed::write_private(path, &bytes).is_err() {
            eprintln!("rakumux-status: saving which new sessions were started failed");
        }
    }
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| {
            i64::try_from(since.as_millis()).unwrap_or(i64::MAX)
        })
}
