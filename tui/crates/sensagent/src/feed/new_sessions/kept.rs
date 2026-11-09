//! What requests from rakumux.com (new sessions, deletions) keep across a
//! restart of the service, in a file (0600) when given one: the requests
//! handled here (carried out, being carried out, or decided: never carried
//! out again) and the reports rakumux.com hasn't taken yet. Written before
//! the daemon is asked, so a stop at any point leaves a request carried out
//! at most once. One the service stopped
//! starting before the daemon answered is never reported: rakumux.com
//! expires it, and the session, if the daemon made it, is listed as any.

use std::collections::{BTreeMap, VecDeque};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

/// Ids are kept this long: rakumux.com expires a request pending 10 minutes,
/// and reads after that never bring it.
const KEEP_MS: i64 = 60 * 60 * 1000;
/// Reports rakumux.com hasn't taken this long after they were decided are
/// dropped: their requests expired long ago.
const REPORT_KEEP_MS: i64 = 60 * 60 * 1000;
/// At most this many ids, the oldest dropped first.
const MAX_IDS: usize = 500;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(in crate::feed) struct Report<O> {
    pub id: String,
    pub outcome: O,
    /// When it was decided (Unix ms).
    pub at: i64,
}

#[derive(Serialize, Deserialize)]
#[serde(bound(serialize = "O: Serialize", deserialize = "O: DeserializeOwned"))]
struct KeptFile<O> {
    version: u8,
    /// Requests started, being started or decided here, by id: when (Unix ms).
    #[serde(default)]
    handled: BTreeMap<String, i64>,
    /// Reports not yet taken by rakumux.com, oldest first.
    #[serde(default)]
    reports: VecDeque<Report<O>>,
}

impl<O> Default for KeptFile<O> {
    fn default() -> Self {
        Self {
            version: 0,
            handled: BTreeMap::new(),
            reports: VecDeque::new(),
        }
    }
}

pub(in crate::feed) struct Kept<O> {
    path: Option<PathBuf>,
    file: KeptFile<O>,
}

impl<O: Serialize + DeserializeOwned> Kept<O> {
    /// What `path` holds (nothing without it, or when it can't be read).
    pub fn load(path: Option<PathBuf>) -> Self {
        let file = path
            .as_ref()
            .and_then(|path| std::fs::read(path).ok())
            .and_then(|bytes| serde_json::from_slice::<KeptFile<O>>(&bytes).ok())
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
    pub fn decided(&mut self, id: &str, outcome: O) {
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
    pub fn next_report(&self) -> Option<&Report<O>> {
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
            eprintln!("rakumux-status: saving which requests from rakumux.com were handled failed");
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
