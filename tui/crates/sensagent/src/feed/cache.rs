//! The feed entries the status service holds in memory: the account's newest,
//! this machine's not yet uploaded, and the replies written back to them,
//! with a revision per change so a TUI asks only for what changed since it
//! last asked.

use serde::{Deserialize, Serialize};

/// Entries kept, newest by when they finished.
pub(crate) const MAX_CACHED: usize = 500;
/// Replies kept, the most recently changed.
pub(crate) const MAX_REPLIES: usize = 500;
/// Reply text per page to a TUI: within the owner protocol's message bound,
/// even with every character escaped.
pub(crate) const PAGE_TEXT_BUDGET: usize = 96 * 1024;
/// Open turns kept and how much of each prompt: within a page's room beside
/// [`PAGE_TEXT_BUDGET`] (20 KiB at most).
pub(crate) const MAX_OPEN: usize = 20;
pub(crate) const OPEN_PROMPT_BYTES: usize = 1024;

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
    /// The git branch its agent worked on when the turn finished.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
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
    /// Where the turn ends in its chat; only this machine's know (uploaded,
    /// never read back).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position: Option<u64>,
    /// The reply ([`FeedReply`]) this turn answers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply_to: Option<String>,
    /// With `reply_to`: the entry that started the thread.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread: Option<String>,
    /// What the user asked that started the turn, once rakumux.com has its
    /// chat (never for this machine's not yet uploaded).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<FeedPrompt>,
}

/// The user's message that started a turn (`contracts/feed/` `turnPrompt`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FeedPrompt {
    /// Its id in its chat: an open turn and the entry that answers it share it.
    pub id: String,
    pub text: String,
    /// The prompt was longer; `text` is its start.
    #[serde(default, skip_serializing_if = "is_false")]
    pub truncated: bool,
    /// When it was sent (Unix milliseconds), when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<i64>,
}

/// A prompt still waiting for its answer, on one of the account's machines
/// (`contracts/feed/` `openTurn`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenTurn {
    pub machine: String,
    pub session_id: String,
    pub session_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub folder: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    pub agent: String,
    pub conversation_id: String,
    pub prompt: FeedPrompt,
}

/// What the user wrote back to a feed entry, for the entry's machine to type
/// into its session (`contracts/feed/` `reply`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FeedReply {
    /// The account's order of reply changes.
    pub seq: u64,
    /// A ULID.
    pub id: String,
    /// The entry replied to, and the one that started its thread.
    pub entry: String,
    pub thread: String,
    /// The entry's machine, which types it into the session.
    pub machine: String,
    pub session_id: String,
    pub session_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub folder: Option<String>,
    pub agent: String,
    pub conversation_id: String,
    pub text: String,
    /// Files the user attached on rakumux.com, in the order attached: typed
    /// in as their paths after the text, once fetched to this machine.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<ReplyAttachment>,
    /// The machine it was written on; `None` when written on rakumux.com.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    pub status: ReplyStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure: Option<ReplyFailure>,
    /// The entry that answered it, once one did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response: Option<String>,
    /// Unix milliseconds.
    pub created_at: i64,
    pub status_at: i64,
}

/// A file the user attached to a reply on rakumux.com (`contracts/feed/`
/// `attachment`), fetched from `GET /api/v1/feed/replies/attachments/{id}`
/// while the reply is pending.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplyAttachment {
    /// A ULID.
    pub id: String,
    /// The file's name as the user's browser gave it: never a path.
    pub name: String,
    /// What the browser said it is; informational only.
    pub media_type: String,
    /// Its length in bytes, 1 to 4 MiB.
    pub size: u64,
}

/// Where a reply is up to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReplyStatus {
    /// Waiting for its machine to type it into the session.
    Pending,
    Delivered,
    Failed,
    /// Stayed pending an hour.
    Expired,
    #[serde(other)]
    Unknown,
}

/// Why a reply wasn't typed into its session.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReplyFailure {
    /// The session isn't running on its machine.
    SessionGone,
    /// It shows another conversation now.
    ConversationChanged,
    Private,
    /// Its machine doesn't take replies written elsewhere.
    NotAccepted,
    /// The session refused the text.
    Refused,
    /// A file attached to it couldn't be fetched, or wasn't what the reply said.
    AttachmentUnavailable,
    #[serde(other)]
    Unknown,
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
    /// New or changed replies, in the order they changed.
    #[serde(default)]
    pub replies: Vec<FeedReply>,
    /// More changes than fit one page: ask again now.
    #[serde(default)]
    pub more: bool,
    /// The account's prompts still waiting for answers, newest first, whole,
    /// when they changed since the TUI's revision; absent: as they were.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub open: Option<Vec<OpenTurn>>,
    /// This machine's name in the feed (its entries' `machine`), which may
    /// differ from its hostname; absent from an older service.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub machine: Option<String>,
}

struct Cached {
    entry: FeedEntry,
    revision: u64,
}

struct CachedReply {
    reply: FeedReply,
    revision: u64,
}

/// The newest [`MAX_CACHED`] entries, by id, and [`MAX_REPLIES`] replies.
#[derive(Default)]
pub(crate) struct FeedCache {
    entries: Vec<Cached>,
    replies: Vec<CachedReply>,
    /// The open turns as last read, and the revision they changed at.
    open: Vec<OpenTurn>,
    open_revision: u64,
    revision: u64,
    generation: u64,
    latest_seq: u64,
}

impl FeedCache {
    /// Forget every entry (sign-out, the switch turned off).
    pub fn clear(&mut self) {
        if self.entries.is_empty()
            && self.replies.is_empty()
            && self.open.is_empty()
            && self.latest_seq == 0
        {
            return;
        }
        self.entries.clear();
        self.replies.clear();
        self.open.clear();
        self.open_revision = 0;
        self.latest_seq = 0;
        self.generation += 1;
    }

    /// The open turns as rakumux.com lists them now: TUIs get them whole
    /// when they changed, each prompt cut to [`OPEN_PROMPT_BYTES`], at most
    /// [`MAX_OPEN`].
    pub fn set_open(&mut self, mut open: Vec<OpenTurn>) {
        open.truncate(MAX_OPEN);
        for turn in &mut open {
            if turn.prompt.text.len() > OPEN_PROMPT_BYTES {
                let mut cut = OPEN_PROMPT_BYTES;
                while !turn.prompt.text.is_char_boundary(cut) {
                    cut -= 1;
                }
                turn.prompt.text.truncate(cut);
                turn.prompt.truncated = true;
            }
        }
        if open != self.open {
            self.revision += 1;
            self.open_revision = self.revision;
            self.open = open;
        }
    }

    /// Replies read back, or this machine's just written: a changed one
    /// replaces what was held.
    pub fn merge_replies(&mut self, replies: Vec<FeedReply>) {
        for reply in replies {
            match self
                .replies
                .iter()
                .position(|held| held.reply.id == reply.id)
            {
                Some(index) if self.replies[index].reply == reply => {}
                Some(index) if self.replies[index].reply.seq > reply.seq => {}
                Some(index) => {
                    self.revision += 1;
                    self.replies[index] = CachedReply {
                        reply,
                        revision: self.revision,
                    };
                }
                None => {
                    self.revision += 1;
                    self.replies.push(CachedReply {
                        reply,
                        revision: self.revision,
                    });
                }
            }
        }
        while self.replies.len() > MAX_REPLIES {
            let oldest = self
                .replies
                .iter()
                .enumerate()
                .min_by_key(|(_, held)| held.reply.seq)
                .map(|(index, _)| index);
            match oldest {
                Some(index) => {
                    self.replies.swap_remove(index);
                }
                None => break,
            }
        }
    }

    /// Where reads continue: the greatest seq read so far (the account's
    /// once a read said no more wait).
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

    /// Entries read from the account's feed; reads continue after `read_to`
    /// (`PageRead::read_to`: past the page, not past entries still to read).
    pub fn merge(&mut self, entries: Vec<FeedEntry>, read_to: u64) {
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
        self.latest_seq = self.latest_seq.max(read_to);
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
        // Entries and replies in the order they changed, within the budget.
        enum Changed<'a> {
            Entry(&'a FeedEntry),
            Reply(&'a FeedReply),
        }
        let mut changed: Vec<(u64, Changed)> = self
            .entries
            .iter()
            .filter(|cached| cached.revision > after)
            .map(|cached| (cached.revision, Changed::Entry(&cached.entry)))
            .chain(
                self.replies
                    .iter()
                    .filter(|held| held.revision > after)
                    .map(|held| (held.revision, Changed::Reply(&held.reply))),
            )
            .collect();
        changed.sort_by_key(|(revision, _)| *revision);
        let (mut entries, mut replies) = (Vec::new(), Vec::new());
        let mut text = 0;
        let mut revision = self.revision;
        let mut more = false;
        for (at, change) in changed {
            text += match &change {
                Changed::Entry(entry) => {
                    entry.text.len() + entry.prompt.as_ref().map_or(0, |prompt| prompt.text.len())
                }
                Changed::Reply(reply) => {
                    reply.text.len()
                        + reply
                            .attachments
                            .iter()
                            .map(|file| file.name.len() + file.media_type.len())
                            .sum::<usize>()
                }
            };
            if !(entries.is_empty() && replies.is_empty()) && text > PAGE_TEXT_BUDGET {
                more = true;
                break;
            }
            revision = at;
            match change {
                Changed::Entry(entry) => entries.push(entry.clone()),
                Changed::Reply(reply) => replies.push(reply.clone()),
            }
        }
        if !more {
            revision = self.revision;
        }
        FeedPage {
            status,
            generation: self.generation,
            revision,
            entries,
            replies,
            more,
            open: (self.open_revision > after).then(|| self.open.clone()),
            machine: None,
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
pub(super) mod tests {
    use super::*;

    pub(crate) fn entry(id: &str, seq: Option<u64>, at: i64) -> FeedEntry {
        FeedEntry {
            id: id.into(),
            seq,
            machine: "studio".into(),
            session_id: "S".into(),
            session_name: "tests".into(),
            folder: None,
            branch: None,
            agent: "codex".into(),
            conversation_id: None,
            text: format!("reply {id}"),
            truncated: false,
            finished_at: at,
            position: None,
            reply_to: None,
            thread: None,
            prompt: None,
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

    fn held_reply(id: &str, seq: u64, status: ReplyStatus) -> FeedReply {
        FeedReply {
            seq,
            id: id.into(),
            entry: "E1".into(),
            thread: "E1".into(),
            machine: "studio".into(),
            session_id: "S".into(),
            session_name: "fix-tests".into(),
            folder: None,
            agent: "codex".into(),
            conversation_id: "c".into(),
            text: "And the docs.".into(),
            attachments: Vec::new(),
            from: None,
            status,
            failure: None,
            response: None,
            created_at: 1,
            status_at: 1,
        }
    }

    #[test]
    fn replies_are_held_newest_status_first_and_paged_as_they_change() {
        let mut cache = FeedCache::default();
        cache.merge(vec![entry("E1", Some(1), 1)], 0);
        cache.merge_replies(vec![held_reply("R1", 2, ReplyStatus::Pending)]);
        let first = cache.page(FeedStatus::Ready, None, None);
        assert_eq!((first.entries.len(), first.replies.len()), (1, 1));

        // An older read changes nothing; a newer status replaces it.
        cache.merge_replies(vec![held_reply("R1", 1, ReplyStatus::Failed)]);
        let same = cache.page(
            FeedStatus::Ready,
            Some(first.revision),
            Some(first.generation),
        );
        assert!(same.replies.is_empty());
        cache.merge_replies(vec![held_reply("R1", 3, ReplyStatus::Delivered)]);
        let next = cache.page(
            FeedStatus::Ready,
            Some(first.revision),
            Some(first.generation),
        );
        assert!(next.entries.is_empty());
        assert_eq!(next.replies.len(), 1);
        assert_eq!(next.replies[0].status, ReplyStatus::Delivered);

        cache.clear();
        let cleared = cache.page(
            FeedStatus::Ready,
            Some(next.revision),
            Some(next.generation),
        );
        assert!(cleared.replies.is_empty());
        assert_ne!(cleared.generation, next.generation);
    }

    #[test]
    fn open_turns_reach_a_tui_whole_when_they_change() {
        let open = |id: &str, text: &str| OpenTurn {
            machine: "studio".into(),
            session_id: "S".into(),
            session_name: "tests".into(),
            folder: None,
            branch: None,
            agent: "codex".into(),
            conversation_id: "c".into(),
            prompt: FeedPrompt {
                id: id.into(),
                text: text.into(),
                truncated: false,
                at: Some(1),
            },
        };
        let mut cache = FeedCache::default();
        cache.set_open(vec![open("m1", &"é".repeat(OPEN_PROMPT_BYTES))]);
        let first = cache.page(FeedStatus::Ready, None, None);
        let sent = first.open.unwrap();
        assert!(sent[0].prompt.text.len() <= OPEN_PROMPT_BYTES && sent[0].prompt.truncated);
        // Unchanged: nothing more; changed (or emptied): the whole list.
        cache.set_open(sent.clone());
        let same = cache.page(
            FeedStatus::Ready,
            Some(first.revision),
            Some(first.generation),
        );
        assert_eq!(same.open, None);
        cache.set_open(Vec::new());
        let emptied = cache.page(
            FeedStatus::Ready,
            Some(first.revision),
            Some(first.generation),
        );
        assert_eq!(emptied.open, Some(Vec::new()));
    }
}
