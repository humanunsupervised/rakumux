//! The feed entries the status service holds in memory: the account's newest,
//! this machine's not yet uploaded, and a revision per change so a TUI asks
//! only for what changed since it last asked.

use serde::{Deserialize, Serialize};

/// Entries kept, newest by when they finished.
pub(crate) const MAX_CACHED: usize = 500;
/// Reply text per page to a TUI: within the owner protocol's message bound,
/// even with every character escaped.
pub(crate) const PAGE_TEXT_BUDGET: usize = 96 * 1024;

/// What an agent said when it finished a turn, on one of the account's machines.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FeedEntry {
    /// The id the capturing daemon gave the reply (a ULID).
    pub id: String,
    /// The cloud's arrival order; absent while this machine's upload is pending.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seq: Option<u64>,
    /// The machine it was made on.
    pub machine: String,
    pub session_id: String,
    pub session_name: String,
    /// The session's folder name (never a path).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub folder: Option<String>,
    /// `claude-code`, `codex` or `opencode`.
    pub agent: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conversation_id: Option<String>,
    pub text: String,
    /// The reply was longer; `text` is its start.
    #[serde(default, skip_serializing_if = "is_false")]
    pub truncated: bool,
    /// When the turn finished (Unix milliseconds).
    pub finished_at: i64,
}

fn is_false(value: &bool) -> bool {
    !*value
}

/// Where the feed is up to, for the TUI to say.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FeedStatus {
    /// Not signed in.
    SignedOut,
    /// The account's Feed switch is off, or not read yet.
    Off,
    /// On; the account's feed hasn't been read yet.
    Loading,
    /// On and read.
    Ready,
    /// The feed can't be reached or refused this machine; what's shown may be old.
    Unavailable,
    #[serde(other)]
    Unknown,
}

/// The entries that changed since a TUI's revision.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FeedPage {
    pub status: FeedStatus,
    /// Changes whenever the feed is emptied (sign-out, the switch turned off):
    /// a TUI with another generation drops what it has.
    pub generation: u64,
    /// The revision these entries bring a TUI to: ask after it next time.
    pub revision: u64,
    /// New or changed entries, in the order they changed.
    pub entries: Vec<FeedEntry>,
    /// More changes than fit one page: ask again now.
    #[serde(default)]
    pub more: bool,
}

struct Cached {
    entry: FeedEntry,
    revision: u64,
}

/// The newest [`MAX_CACHED`] entries, by id.
#[derive(Default)]
pub(crate) struct FeedCache {
    entries: Vec<Cached>,
    revision: u64,
    generation: u64,
    latest_seq: u64,
}

impl FeedCache {
    /// Forget every entry (sign-out, the switch turned off).
    pub fn clear(&mut self) {
        if self.entries.is_empty() && self.latest_seq == 0 {
            return;
        }
        self.entries.clear();
        self.latest_seq = 0;
        self.generation += 1;
    }

    /// The account's newest seq read so far: reads continue after it.
    pub fn latest_seq(&self) -> u64 {
        self.latest_seq
    }

    /// This machine's new entry, to upload. False when it's known already.
    pub fn insert_local(&mut self, entry: FeedEntry) -> bool {
        if self.position(&entry.id).is_some() {
            return false;
        }
        self.push(entry);
        self.evict();
        true
    }

    /// The uploaded ids now have these seqs.
    pub fn confirm(&mut self, accepted: &[(String, u64)]) {
        for (id, seq) in accepted {
            if let Some(index) = self.position(id) {
                if self.entries[index].entry.seq != Some(*seq) {
                    self.revision += 1;
                    let cached = &mut self.entries[index];
                    cached.entry.seq = Some(*seq);
                    cached.revision = self.revision;
                }
            }
        }
    }

    /// Entries read from the account's feed.
    pub fn merge(&mut self, entries: Vec<FeedEntry>, latest_seq: u64) {
        for entry in entries {
            self.latest_seq = self.latest_seq.max(entry.seq.unwrap_or(0));
            match self.position(&entry.id) {
                Some(index) if self.entries[index].entry == entry => {}
                Some(index) => {
                    self.revision += 1;
                    self.entries[index] = Cached {
                        entry,
                        revision: self.revision,
                    };
                }
                None => self.push(entry),
            }
        }
        self.latest_seq = self.latest_seq.max(latest_seq);
        self.evict();
    }

    /// This machine's entries waiting for upload, oldest first.
    pub fn pending(&self) -> Vec<&FeedEntry> {
        let mut pending: Vec<&FeedEntry> = self
            .entries
            .iter()
            .map(|cached| &cached.entry)
            .filter(|entry| entry.seq.is_none())
            .collect();
        pending.sort_by_key(|entry| entry.finished_at);
        pending
    }

    /// What changed after `after` for a TUI of `generation` (everything for
    /// another generation), within [`PAGE_TEXT_BUDGET`].
    pub fn page(
        &self,
        status: FeedStatus,
        after: Option<u64>,
        generation: Option<u64>,
    ) -> FeedPage {
        let after = if generation == Some(self.generation) {
            after.unwrap_or(0)
        } else {
            0
        };
        let mut changed: Vec<&Cached> = self
            .entries
            .iter()
            .filter(|cached| cached.revision > after)
            .collect();
        changed.sort_by_key(|cached| cached.revision);
        let mut entries = Vec::new();
        let mut text = 0;
        let mut revision = self.revision;
        let mut more = false;
        for cached in changed {
            text += cached.entry.text.len();
            if !entries.is_empty() && text > PAGE_TEXT_BUDGET {
                more = true;
                break;
            }
            revision = cached.revision;
            entries.push(cached.entry.clone());
        }
        if !more {
            revision = self.revision;
        }
        FeedPage {
            status,
            generation: self.generation,
            revision,
            entries,
            more,
        }
    }

    fn position(&self, id: &str) -> Option<usize> {
        self.entries.iter().position(|cached| cached.entry.id == id)
    }

    fn push(&mut self, entry: FeedEntry) {
        self.revision += 1;
        self.entries.push(Cached {
            entry,
            revision: self.revision,
        });
    }

    /// Keep the newest [`MAX_CACHED`]: uploaded entries go before pending ones.
    fn evict(&mut self) {
        while self.entries.len() > MAX_CACHED {
            let oldest = self
                .entries
                .iter()
                .enumerate()
                .min_by_key(|(_, cached)| {
                    (
                        cached.entry.seq.is_none(),
                        cached.entry.finished_at,
                        cached.entry.seq,
                    )
                })
                .map(|(index, _)| index);
            match oldest {
                Some(index) => {
                    self.entries.swap_remove(index);
                }
                None => return,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(crate) fn entry(id: &str, seq: Option<u64>, at: i64) -> FeedEntry {
        FeedEntry {
            id: id.into(),
            seq,
            machine: "studio".into(),
            session_id: "S".into(),
            session_name: "tests".into(),
            folder: None,
            agent: "codex".into(),
            conversation_id: None,
            text: format!("reply {id}"),
            truncated: false,
            finished_at: at,
        }
    }

    fn ids(page: &FeedPage) -> Vec<&str> {
        page.entries.iter().map(|entry| entry.id.as_str()).collect()
    }

    #[test]
    fn a_tui_gets_only_what_changed_since_its_revision() {
        let mut cache = FeedCache::default();
        assert!(cache.insert_local(entry("A", None, 1)));
        assert!(!cache.insert_local(entry("A", None, 1)), "known already");
        cache.merge(vec![entry("B", Some(5), 2)], 5);
        let first = cache.page(FeedStatus::Ready, None, None);
        assert_eq!(ids(&first), ["A", "B"]);
        assert_eq!(cache.latest_seq(), 5);

        cache.confirm(&[("A".into(), 6)]);
        let next = cache.page(
            FeedStatus::Ready,
            Some(first.revision),
            Some(first.generation),
        );
        assert_eq!(ids(&next), ["A"]);
        assert_eq!(next.entries[0].seq, Some(6));
        let none = cache.page(
            FeedStatus::Ready,
            Some(next.revision),
            Some(next.generation),
        );
        assert!(none.entries.is_empty());

        // Our own entry read back unchanged is no change.
        cache.merge(vec![entry("A", Some(6), 1)], 6);
        assert!(cache
            .page(
                FeedStatus::Ready,
                Some(next.revision),
                Some(next.generation)
            )
            .entries
            .is_empty());
    }

    #[test]
    fn clearing_starts_a_new_generation() {
        let mut cache = FeedCache::default();
        cache.insert_local(entry("A", None, 1));
        let page = cache.page(FeedStatus::Ready, None, None);
        cache.clear();
        cache.insert_local(entry("C", None, 3));
        let after = cache.page(
            FeedStatus::Ready,
            Some(page.revision),
            Some(page.generation),
        );
        assert_ne!(after.generation, page.generation);
        assert_eq!(ids(&after), ["C"], "another generation gets everything");
    }

    #[test]
    fn pages_stay_within_their_text_budget() {
        let mut cache = FeedCache::default();
        for i in 0..10 {
            let mut big = entry(&format!("E{i}"), Some(i + 1), i as i64);
            big.text = "x".repeat(30 * 1024);
            cache.merge(vec![big], 0);
        }
        let first = cache.page(FeedStatus::Ready, None, None);
        assert!(first.more);
        assert_eq!(first.entries.len(), 3);
        let rest = cache.page(
            FeedStatus::Ready,
            Some(first.revision),
            Some(first.generation),
        );
        assert_eq!(rest.entries.len(), 3);
        assert_eq!(rest.entries[0].id, "E3");
    }

    #[test]
    fn the_oldest_uploaded_entries_go_first() {
        let mut cache = FeedCache::default();
        cache.insert_local(entry("pending", None, 0));
        for i in 0..MAX_CACHED as u64 {
            cache.merge(vec![entry(&format!("E{i}"), Some(i + 1), 10 + i as i64)], 0);
        }
        let page = cache.page(FeedStatus::Ready, None, None);
        let kept: Vec<&str> = page.entries.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(cache.entries.len(), MAX_CACHED);
        assert!(kept.contains(&"pending"));
        assert!(!kept.contains(&"E0"));
        assert_eq!(cache.pending().len(), 1);
    }
}
