//! The agent feed's state: what agents said when they finished a turn, on
//! this machine and the account's others, as the account service
//! (`crate::account::AccountAccess::feed`) hands it out. Entries are kept
//! newest first, with the selection, the scroll of the reply being read
//! and where the service's pages are up to, and the whole turns of the
//! entries read lately (`AccountAccess::feed_turn`), and the replies written
//! back to entries (`AccountAccess::feed_reply`, `r`), which with the entries
//! that answer them make threads, and which entries were on screen, to count
//! the new ones. Nothing here draws or knows where it's shown: `pane.rs`
//! puts it on screen, and `components/feed.rs` draws it into any area.

mod pane;
mod reply;
#[cfg(test)]
mod tests;

use std::cmp::Reverse;
use std::collections::{HashSet, VecDeque};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use rakumux_sensagent::feed::{
    FeedEntry, FeedPage, FeedPrompt, FeedReply, FeedStatus, FeedTurn, FeedTurnStatus, OpenTurn,
    ReplyStatus,
};
use session_core::api::TranscriptMessage;

/// Entries kept, newest first (the service keeps as many).
pub const MAX_ENTRIES: usize = 500;
/// Whole turns kept, the most recently read.
const MAX_TURNS: usize = 20;
/// The most messages of one turn held (its start).
const MAX_TURN_MESSAGES: usize = 2000;
/// Replies kept (the service keeps as many).
const MAX_REPLIES: usize = 500;

/// One step of a thread: an entry (an agent's finished turn) or a reply
/// written back to one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThreadItem<'a> {
    Entry(&'a FeedEntry),
    Reply(&'a FeedReply),
}

/// An entry's whole turn, as far as it was read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TurnState {
    /// Asked for, nothing back yet.
    Loading,
    /// Its messages so far, in order; `next` while more are to come.
    Ready {
        messages: Vec<TranscriptMessage>,
        next: Option<u64>,
    },
    /// It couldn't be read; the reply alone is shown.
    Unavailable,
}

/// One card of the feed: a finished turn (its prompt, when known, and its
/// answer), or a prompt still waiting for its answer. A prompt's card
/// becomes its answer's when that arrives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Card<'a> {
    Entry(&'a FeedEntry),
    Open(&'a OpenTurn),
}

impl<'a> Card<'a> {
    /// What the selection holds: an entry's id, or an open prompt's.
    pub fn key(&self) -> String {
        match self {
            Card::Entry(entry) => entry.id.clone(),
            Card::Open(open) => format!("open:{}", open.prompt.id),
        }
    }

    /// When it last changed: its turn finished, or its prompt was sent (an
    /// open one of unknown time counts as now).
    pub fn at(&self) -> i64 {
        match self {
            Card::Entry(entry) => entry.finished_at,
            Card::Open(open) => open.prompt.at.unwrap_or(i64::MAX),
        }
    }

    pub fn entry(&self) -> Option<&'a FeedEntry> {
        match self {
            Card::Entry(entry) => Some(entry),
            Card::Open(_) => None,
        }
    }

    pub fn prompt(&self) -> Option<&'a FeedPrompt> {
        match self {
            Card::Entry(entry) => entry.prompt.as_ref(),
            Card::Open(open) => Some(&open.prompt),
        }
    }

    pub fn machine(&self) -> &'a str {
        match self {
            Card::Entry(entry) => &entry.machine,
            Card::Open(open) => &open.machine,
        }
    }

    pub fn session_id(&self) -> &'a str {
        match self {
            Card::Entry(entry) => &entry.session_id,
            Card::Open(open) => &open.session_id,
        }
    }

    pub fn session_name(&self) -> &'a str {
        match self {
            Card::Entry(entry) => &entry.session_name,
            Card::Open(open) => &open.session_name,
        }
    }

    pub fn agent(&self) -> &'a str {
        match self {
            Card::Entry(entry) => &entry.agent,
            Card::Open(open) => &open.agent,
        }
    }

    /// The branch its agent worked on, else its folder.
    pub fn place(&self) -> Option<&'a str> {
        match self {
            Card::Entry(entry) => entry.branch.as_deref().or(entry.folder.as_deref()),
            Card::Open(open) => open.branch.as_deref().or(open.folder.as_deref()),
        }
    }
}

/// Where a reply written in the feed goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReplyTo {
    /// A feed entry (its id): the reply joins its thread.
    Entry(String),
    /// A session itself on its machine, a session message: typed in once an
    /// agent there waits for input, whatever chat it shows.
    Session { machine: String, session: String },
}

/// Whether `entry` answers `open`: it names the same prompt, or it finished
/// in the same chat after the prompt was sent.
fn answers(entry: &FeedEntry, open: &OpenTurn) -> bool {
    if let Some(prompt) = &entry.prompt {
        return prompt.id == open.prompt.id;
    }
    entry.machine == open.machine
        && entry.session_id == open.session_id
        && entry.conversation_id.as_deref() == Some(open.conversation_id.as_str())
        && open.prompt.at.is_some_and(|at| entry.finished_at >= at)
}

/// Where a feed entry is up to, as its card says at the right of its top
/// border (rakumux.com's Feed says the same words).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryStatus {
    /// A prompt whose answer hasn't come yet.
    Working,
    /// This machine's, not uploaded yet.
    Sending,
    /// The agent finished its turn and nobody wrote back.
    Done,
    /// The newest reply written back to it is waiting to be typed in.
    ReplyWaiting,
    /// It was typed into the session; no answer yet.
    ReplyTypedIn,
    /// The agent answered it.
    Answered,
    /// It failed or expired.
    ReplyNotSent,
}

impl EntryStatus {
    pub fn label(self) -> &'static str {
        match self {
            EntryStatus::Working => "working",
            EntryStatus::Sending => "sending",
            EntryStatus::Done => "done",
            EntryStatus::ReplyWaiting => "reply waiting",
            EntryStatus::ReplyTypedIn => "reply typed in",
            EntryStatus::Answered => "answered",
            EntryStatus::ReplyNotSent => "reply not sent",
        }
    }
}

/// The feed as this window has it.
#[derive(Debug, Clone)]
pub struct FeedStore {
    /// This machine's name, as the service names it in entries it uploads.
    machine: String,
    entries: Vec<FeedEntry>,
    /// Prompts still waiting for their answers, as the service last said.
    open: Vec<OpenTurn>,
    /// Replies written back to entries, by when they were written.
    replies: Vec<FeedReply>,
    /// `None` until the service first answered.
    status: Option<FeedStatus>,
    /// The service's generation and revision these entries are at.
    generation: Option<u64>,
    revision: Option<u64>,
    /// The selected card's key ([`Card::key`]); `None` selects the newest.
    selected: Option<String>,
    /// Rows of the selected reply scrolled past.
    pub scroll: usize,
    /// The selected reply is read whole in the cards' place (Enter on a
    /// card; Esc goes back to them).
    pub reading: bool,
    /// A page was asked for and hasn't come back.
    pub(super) busy: bool,
    /// When to ask the service again.
    pub(super) due: Option<Instant>,
    /// Answers for an older request generation are dropped.
    pub(super) request_generation: u64,
    /// Entries' turns, the most recently read last.
    turns: VecDeque<(String, TurnState)>,
    /// A turn is being read.
    pub(super) turn_busy: bool,
    /// Entries that were in the feed while it was on screen, by id.
    seen: HashSet<String>,
    /// Entries that finished before this (Unix milliseconds: when this
    /// window started) are never new.
    pub(super) since: i64,
}

impl FeedStore {
    pub fn new(machine: String) -> Self {
        let since = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |now| i64::try_from(now.as_millis()).unwrap_or(i64::MAX));
        Self {
            machine,
            entries: Vec::new(),
            open: Vec::new(),
            replies: Vec::new(),
            status: None,
            generation: None,
            revision: None,
            selected: None,
            scroll: 0,
            reading: false,
            busy: false,
            due: None,
            request_generation: 0,
            turns: VecDeque::new(),
            turn_busy: false,
            seen: HashSet::new(),
            since,
        }
    }

    /// Everything held was on screen: none of it is new any more.
    pub fn mark_seen(&mut self) {
        // Ids of entries no longer held go once the set outgrows them.
        if self.seen.len() > 2 * MAX_ENTRIES {
            let held: HashSet<&str> = self.entries.iter().map(|e| e.id.as_str()).collect();
            self.seen.retain(|id| held.contains(id.as_str()));
        }
        self.seen
            .extend(self.entries.iter().map(|entry| entry.id.clone()));
    }

    /// Entries that arrived since the feed was last on screen: finished in
    /// this window's run and never shown.
    pub fn unseen(&self) -> usize {
        self.entries
            .iter()
            .filter(|entry| entry.finished_at > self.since && !self.seen.contains(&entry.id))
            .count()
    }

    /// Entry `id`'s turn, if it was asked for.
    pub fn turn(&self, id: &str) -> Option<&TurnState> {
        self.turns
            .iter()
            .find(|(held, _)| held == id)
            .map(|(_, turn)| turn)
    }

    /// What to read of the selected entry's turn next: from its start when
    /// it wasn't asked for, else after `next` while more are to come.
    pub(super) fn turn_due(&self) -> Option<(String, Option<u64>)> {
        let entry = self.selected()?;
        entry.seq?;
        match self.turn(&entry.id) {
            None => Some((entry.id.clone(), None)),
            Some(TurnState::Ready {
                messages,
                next: Some(next),
            }) if messages.len() < MAX_TURN_MESSAGES => Some((entry.id.clone(), Some(*next))),
            Some(_) => None,
        }
    }

    /// Entry `id`'s turn is being read (from its start).
    pub(super) fn turn_asked(&mut self, id: &str) {
        if self.turn(id).is_none() {
            self.turns.push_back((id.to_owned(), TurnState::Loading));
            while self.turns.len() > MAX_TURNS {
                self.turns.pop_front();
            }
        }
    }

    /// Take in a read of entry `id`'s turn: from its start, or after what's
    /// held (`appended`).
    pub(super) fn apply_turn(&mut self, id: &str, turn: FeedTurn, appended: bool) {
        let Some(at) = self.turns.iter().position(|(held, _)| held == id) else {
            return;
        };
        let state = match (turn.status, &mut self.turns[at].1) {
            (FeedTurnStatus::Ready, TurnState::Ready { messages, next }) if appended => {
                messages.extend(turn.messages);
                messages.truncate(MAX_TURN_MESSAGES);
                *next = turn.next;
                return;
            }
            (FeedTurnStatus::Ready, _) => TurnState::Ready {
                messages: turn.messages,
                next: turn.next,
            },
            // A continuation that failed keeps what was read.
            (_, TurnState::Ready { next, .. }) if appended => {
                *next = None;
                return;
            }
            _ => TurnState::Unavailable,
        };
        self.turns[at].1 = state;
    }

    /// This machine's name in entries.
    pub fn machine(&self) -> &str {
        &self.machine
    }

    /// Where the feed is up to; `None` until the service answered.
    pub fn status(&self) -> Option<FeedStatus> {
        self.status
    }

    /// Newest first.
    #[cfg(test)]
    pub fn entries(&self) -> &[FeedEntry] {
        &self.entries
    }

    /// The cards, newest first: every entry, and each prompt still waiting
    /// for an answer that no entry has given yet.
    pub fn cards(&self) -> Vec<Card<'_>> {
        let mut cards: Vec<Card> = self.entries.iter().map(Card::Entry).collect();
        cards.extend(
            self.open
                .iter()
                .filter(|open| !self.entries.iter().any(|entry| answers(entry, open)))
                .map(Card::Open),
        );
        // Entries are newest first already; open prompts go where their time puts them.
        cards.sort_by_key(|card| Reverse(card.at()));
        cards
    }

    /// What to ask the service next: changes after this revision, of this
    /// generation.
    pub fn cursor(&self) -> (Option<u64>, Option<u64>) {
        (self.revision, self.generation)
    }

    /// Take in a page; returns whether more changes wait (ask again now).
    /// A page of another generation replaces everything held.
    pub fn apply(&mut self, page: FeedPage) -> bool {
        let selected = self.selected_index();
        if self.generation != Some(page.generation) {
            self.entries.clear();
            self.open.clear();
            self.replies.clear();
            self.scroll = 0;
        }
        if let Some(open) = page.open {
            self.open = open;
        }
        // The service's name for this machine (it keeps one, and takes
        // another when a machine of the account had it), not the hostname.
        if let Some(machine) = page.machine {
            self.machine = machine;
        }
        for reply in page.replies {
            self.take_reply(reply);
        }
        self.generation = Some(page.generation);
        self.revision = Some(page.revision);
        self.status = Some(page.status);
        for entry in page.entries {
            match self.entries.iter_mut().find(|held| held.id == entry.id) {
                Some(held) => *held = entry,
                None => self.entries.push(entry),
            }
        }
        // Newest first; this machine's pending one first among equals.
        self.entries
            .sort_by_key(|entry| Reverse((entry.finished_at, entry.seq.unwrap_or(u64::MAX))));
        self.entries.truncate(MAX_ENTRIES);
        // This machine's entry not uploaded yet says which prompt it answers.
        for entry in &mut self.entries {
            if entry.prompt.is_none() {
                if let Some(open) = self.open.iter().find(|open| answers(entry, open)) {
                    entry.prompt = Some(open.prompt.clone());
                }
            }
        }
        // A prompt that was answered stays selected as its answer's card.
        if let Some(prompt) = self
            .selected
            .as_deref()
            .and_then(|key| key.strip_prefix("open:"))
        {
            let gone = !self.open.iter().any(|open| open.prompt.id == prompt);
            let answer = self
                .entries
                .iter()
                .find(|entry| entry.prompt.as_ref().is_some_and(|p| p.id == prompt));
            if let (true, Some(answer)) = (gone, answer) {
                self.selected = Some(answer.id.clone());
            }
        }
        // A card that went away leaves the selection where it was.
        if self.selected_index().is_none() && self.selected.is_some() {
            let cards = self.cards();
            self.selected = selected
                .map(|index| index.min(cards.len().saturating_sub(1)))
                .and_then(|index| cards.get(index))
                .map(Card::key);
        }
        page.more
    }

    /// A reply as written or read back: one with a newer status replaces it.
    pub fn take_reply(&mut self, reply: FeedReply) {
        match self.replies.iter_mut().find(|held| held.id == reply.id) {
            Some(held) if held.seq > reply.seq => {}
            Some(held) => *held = reply,
            None => self.replies.push(reply),
        }
        self.replies
            .sort_by(|a, b| (a.created_at, &a.id).cmp(&(b.created_at, &b.id)));
        if self.replies.len() > MAX_REPLIES {
            let over = self.replies.len() - MAX_REPLIES;
            self.replies.drain(..over);
        }
    }

    /// Whether `entry` can be written back to: uploaded, with a known chat,
    /// and the newest entry of its session (its machine's and session's
    /// latest by when it finished, then its seq), as rakumux.com requires.
    pub fn can_reply(&self, entry: &FeedEntry) -> bool {
        let key = |held: &FeedEntry| (held.finished_at, held.seq.unwrap_or(u64::MAX));
        entry.seq.is_some()
            && entry.conversation_id.is_some()
            && !self.entries.iter().any(|held| {
                held.machine == entry.machine
                    && held.session_id == entry.session_id
                    && key(held) > key(entry)
            })
    }

    /// Where a reply written to `card` goes, when it takes one. Only a
    /// session's newest card does (its machine's and session's first in
    /// [`Self::cards`]). An entry is answered itself when it can be
    /// ([`Self::can_reply`]); a prompt still waiting for its answer answers
    /// the session's newest entry when that's in the prompt's chat, so the
    /// thread goes on, and otherwise writes to the session itself.
    pub fn reply_to(&self, card: Card<'_>) -> Option<ReplyTo> {
        let newest = self.cards().into_iter().find(|other| {
            other.machine() == card.machine() && other.session_id() == card.session_id()
        })?;
        if newest.key() != card.key() {
            return None;
        }
        match card {
            Card::Entry(entry) => self
                .can_reply(entry)
                .then(|| ReplyTo::Entry(entry.id.clone())),
            Card::Open(open) => {
                // Entries are newest first.
                let latest = self.entries.iter().find(|entry| {
                    entry.machine == open.machine && entry.session_id == open.session_id
                });
                Some(match latest {
                    Some(entry)
                        if self.can_reply(entry)
                            && entry.conversation_id.as_deref()
                                == Some(open.conversation_id.as_str()) =>
                    {
                        ReplyTo::Entry(entry.id.clone())
                    }
                    _ => ReplyTo::Session {
                        machine: open.machine.clone(),
                        session: open.session_id.clone(),
                    },
                })
            }
        }
    }

    /// Where `entry` is up to: sending, done, or what became of the newest
    /// reply written back to it.
    pub fn status_of(&self, entry: &FeedEntry) -> EntryStatus {
        if entry.seq.is_none() {
            return EntryStatus::Sending;
        }
        let newest = self
            .replies
            .iter()
            .rev()
            .find(|reply| reply.entry == entry.id);
        match newest.map(|reply| (reply.status, reply.response.is_some())) {
            Some((ReplyStatus::Pending, _)) => EntryStatus::ReplyWaiting,
            Some((ReplyStatus::Delivered, true)) => EntryStatus::Answered,
            Some((ReplyStatus::Delivered, false)) => EntryStatus::ReplyTypedIn,
            Some((ReplyStatus::Failed | ReplyStatus::Expired, _)) => EntryStatus::ReplyNotSent,
            Some((ReplyStatus::Unknown, _)) | None => EntryStatus::Done,
        }
    }

    /// `entry`'s thread, oldest first: the entry that started it, the
    /// replies written back and the entries that answer them. Empty while
    /// nobody replied.
    pub fn thread(&self, entry: &FeedEntry) -> Vec<ThreadItem<'_>> {
        let root = entry.thread.as_deref().unwrap_or(&entry.id);
        let replies: Vec<&FeedReply> = self
            .replies
            .iter()
            .filter(|reply| reply.thread == root)
            .collect();
        if replies.is_empty() && entry.reply_to.is_none() {
            return Vec::new();
        }
        let mut items: Vec<(i64, ThreadItem)> = self
            .entries
            .iter()
            .filter(|held| held.id == root || held.thread.as_deref() == Some(root))
            .map(|held| (held.finished_at, ThreadItem::Entry(held)))
            .chain(
                replies
                    .into_iter()
                    .map(|reply| (reply.created_at, ThreadItem::Reply(reply))),
            )
            .collect();
        items.sort_by_key(|(at, _)| *at);
        items.into_iter().map(|(_, item)| item).collect()
    }

    /// The selected card's position; `None` when there are none.
    pub fn selected_index(&self) -> Option<usize> {
        let cards = self.cards();
        if cards.is_empty() {
            return None;
        }
        match &self.selected {
            None => Some(0),
            Some(key) => cards.iter().position(|card| &card.key() == key),
        }
    }

    pub fn selected_card(&self) -> Option<Card<'_>> {
        let index = self.selected_index()?;
        self.cards().into_iter().nth(index)
    }

    /// The selected card's entry; `None` for a prompt still waiting.
    pub fn selected(&self) -> Option<&FeedEntry> {
        self.selected_card().and_then(|card| card.entry())
    }

    /// Select the card at `index` (clamped); it's read from the top.
    pub fn select(&mut self, index: usize) {
        let cards = self.cards();
        let Some(last) = cards.len().checked_sub(1) else {
            return;
        };
        let key = cards[index.min(last)].key();
        if self.selected.as_ref() != Some(&key) {
            self.scroll = 0;
        }
        self.selected = Some(key);
    }

    /// Move the selection `delta` entries, stopping at either end.
    pub fn move_selection(&mut self, delta: isize) {
        let current = self.selected_index().unwrap_or(0) as isize;
        self.select((current + delta).max(0) as usize);
    }

    /// Scroll the selected reply `delta` rows (down when positive), never
    /// above its top; drawing stops it at its end.
    pub fn scroll_by(&mut self, delta: isize) {
        self.scroll = (self.scroll as isize + delta).max(0) as usize;
    }

    /// Forget everything (the account signed out elsewhere) but what was
    /// seen, so signing in again brings back nothing as new.
    pub fn clear(&mut self) {
        *self = Self {
            request_generation: self.request_generation + 1,
            seen: std::mem::take(&mut self.seen),
            since: self.since,
            ..Self::new(std::mem::take(&mut self.machine))
        };
    }
}

/// `text` without anything a terminal would act on: controls other than a
/// line break or tab, and bidi and zero-width format characters.
pub fn plain(text: &str) -> String {
    text.chars()
        .filter(|c| {
            !((c.is_control() && *c != '\n' && *c != '\t')
                || matches!(c, '\u{200b}'..='\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2060}'..='\u{206f}' | '\u{feff}'))
        })
        .map(|c| if c == '\t' { ' ' } else { c })
        .collect()
}

/// An agent as the feed names it: `claude` for Claude Code.
pub fn agent_name(agent: &str) -> &str {
    match agent {
        "claude-code" => "claude",
        other => other,
    }
}
