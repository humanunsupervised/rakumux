//! What replies keep across a restart of the service, in a file (0600) when
//! given one: the replies typed in or decided here (never typed again), those
//! handed to the daemon whose report isn't queued yet with the session each
//! went into (`typed_into`), and the reports rakumux.com hasn't taken yet.
//! (An older release also kept the replies this machine wrote, `own`, which
//! is ignored when read and dropped when saved.) A reply handed to the daemon
//! is followed again after a restart, its session taking nothing else until
//! its agent takes it or it's decided `not_taken`; one from a file without
//! where it went (an older release's) is reported delivered on load. An
//! answer to an agent's question handed to the daemon (`answering`) is
//! reported `not_taken` on load: whether its dialog took it isn't known.
//! Without it a restarted service read its pending replies from the start and
//! typed one again that it had typed but not yet reported (the report waits
//! for the agent to take it, and backs off while rakumux.com can't be
//! reached). Written before the daemon is asked to type, so a stop at any
//! point leaves a reply typed at most once.

use std::collections::{BTreeMap, VecDeque};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use super::super::cache::ReplyFailure;

/// Ids are kept this long: a reply pending longer is expired by rakumux.com,
/// and reads after that never say it's pending.
const KEEP_MS: i64 = 2 * 60 * 60 * 1000;
/// Reports rakumux.com hasn't taken this long after they were decided are
/// dropped: their replies expired long ago.
const REPORT_KEEP_MS: i64 = 24 * 60 * 60 * 1000;
/// At most this many ids of each kind, the oldest dropped first.
const MAX_IDS: usize = 2_000;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct Report {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure: Option<ReplyFailure>,
    /// When it was decided (Unix ms).
    pub at: i64,
}

/// Where a reply handed to the daemon went: its session, its agent's turn
/// and chat just before, and whether the agent worked then (typed
/// mid-turn), so a restarted service sees whether the agent took it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct TypedInto {
    pub session: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conversation: Option<String>,
    #[serde(default)]
    pub mid_turn: bool,
}

#[derive(Default, Serialize, Deserialize)]
struct KeptFile {
    version: u8,
    /// Replies typed in or decided here: never typed again.
    #[serde(default)]
    handled: BTreeMap<String, i64>,
    /// Replies handed to the daemon whose report isn't queued yet.
    #[serde(default)]
    typing: BTreeMap<String, i64>,
    /// Where each of `typing` went (left out by older releases).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    typed_into: BTreeMap<String, TypedInto>,
    /// Answers to agents' questions handed to the daemon whose report isn't
    /// queued yet: a service that stopped meanwhile never learnt whether
    /// the dialog took them.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    answering: BTreeMap<String, i64>,
    /// Reports not yet taken by rakumux.com, oldest first.
    #[serde(default)]
    reports: VecDeque<Report>,
}

pub(super) struct Kept {
    path: Option<PathBuf>,
    file: KeptFile,
}

impl Kept {
    /// What `path` holds (nothing without it, or when it can't be read). A
    /// reply that was being typed when the service stopped is never typed
    /// again: followed where it went ([`Kept::in_flight`]), or, without
    /// that, taken as typed in and reported delivered.
    pub fn load(path: Option<PathBuf>) -> Self {
        let file = path
            .as_ref()
            .and_then(|path| std::fs::read(path).ok())
            .and_then(|bytes| serde_json::from_slice::<KeptFile>(&bytes).ok())
            .filter(|file| file.version == 1)
            .unwrap_or_default();
        let mut kept = Self { path, file };
        let now = now_ms();
        let typing = std::mem::take(&mut kept.file.typing);
        let mut typed_into = std::mem::take(&mut kept.file.typed_into);
        for (id, at) in typing {
            if let Some(into) = typed_into.remove(&id) {
                kept.file.typing.insert(id.clone(), at);
                kept.file.typed_into.insert(id, into);
                continue;
            }
            if !kept.file.reports.iter().any(|report| report.id == id) {
                kept.file.reports.push_back(Report {
                    id: id.clone(),
                    failure: None,
                    at: now,
                });
            }
            kept.file.handled.insert(id, now);
        }
        // An answer handed to the daemon before the stop: it may have gone
        // in, or partly; never given again, and reported not taken (the chat
        // shows the question answered when it was).
        for (id, _) in std::mem::take(&mut kept.file.answering) {
            if !kept.file.reports.iter().any(|report| report.id == id) {
                kept.file.reports.push_back(Report {
                    id: id.clone(),
                    failure: Some(ReplyFailure::NotTaken),
                    at: now,
                });
            }
            kept.file.handled.insert(id, now);
        }
        kept.save();
        kept
    }

    /// Whether the reply `id` was typed in or decided here.
    pub fn handled(&self, id: &str) -> bool {
        self.file.handled.contains_key(id)
    }

    /// The replies handed to the daemon before this service started and not
    /// decided yet, with where each went: still to be followed.
    pub fn in_flight(&self) -> impl Iterator<Item = (&String, &TypedInto)> {
        self.file.typed_into.iter()
    }

    /// The reply `id` is about to be handed to the daemon, `into` where it
    /// goes: from now on it's never typed again, whatever happens to this
    /// service.
    pub fn typing(&mut self, id: &str, into: TypedInto) {
        let now = now_ms();
        self.file.typing.insert(id.to_owned(), now);
        self.file.typed_into.insert(id.to_owned(), into);
        self.file.handled.insert(id.to_owned(), now);
        self.save();
    }

    /// The answer `id` to an agent's question is about to be handed to the
    /// daemon: from now on it's never given again, whatever happens to this
    /// service.
    pub fn answering(&mut self, id: &str) {
        let now = now_ms();
        self.file.answering.insert(id.to_owned(), now);
        self.file.handled.insert(id.to_owned(), now);
        self.save();
    }

    /// The daemon didn't type it (someone was typing, or another program
    /// had the foreground): it may be typed later.
    pub fn untyped(&mut self, id: &str) {
        self.file.typing.remove(id);
        self.file.typed_into.remove(id);
        self.file.answering.remove(id);
        self.file.handled.remove(id);
        self.save();
    }

    /// Handed to the daemon before a restart, it's no longer pending on
    /// rakumux.com (expired): never typed again, and nothing to report.
    pub fn no_longer_pending(&mut self, id: &str) {
        self.file.typing.remove(id);
        self.file.typed_into.remove(id);
        self.save();
    }

    /// Decided: never typed (again), and rakumux.com is to hear `failure`
    /// (none: delivered).
    pub fn decided(&mut self, id: &str, failure: Option<ReplyFailure>) {
        let now = now_ms();
        self.file.typing.remove(id);
        self.file.typed_into.remove(id);
        self.file.answering.remove(id);
        self.file.handled.insert(id.to_owned(), now);
        self.file.reports.push_back(Report {
            id: id.to_owned(),
            failure,
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

    #[cfg(test)]
    pub fn reports(&self) -> Vec<Report> {
        self.file.reports.iter().cloned().collect()
    }

    fn save(&mut self) {
        let now = now_ms();
        for ids in [
            &mut self.file.handled,
            &mut self.file.typing,
            &mut self.file.answering,
        ] {
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
        }
        let typing = &self.file.typing;
        self.file.typed_into.retain(|id, _| typing.contains_key(id));
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
            eprintln!("rakumux-status: saving which replies were typed in failed");
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
