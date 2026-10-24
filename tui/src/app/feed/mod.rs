//! The agent feed's state: what agents said when they finished a turn, on
//! this machine and the account's others, as the account service
//! (`crate::account::AccountAccess::feed`) hands it out. Entries are kept
//! newest first, with the selection, the scroll of the reply being read
//! and where the service's pages are up to, and the whole turns of the
//! entries read lately (`AccountAccess::feed_turn`). Nothing here draws or
//! knows where it's shown: the list's Feed tab hosts it today (`tab.rs`),
//! and `components/feed.rs` draws it into any area.

mod tab;
#[cfg(test)]
mod tests;

use std::cmp::Reverse;
use std::collections::VecDeque;
use std::time::Instant;

use rakumux_sensagent::feed::{FeedEntry, FeedPage, FeedStatus, FeedTurn, FeedTurnStatus};
use session_core::api::TranscriptMessage;

/// Entries kept, newest first (the service keeps as many).
pub const MAX_ENTRIES: usize = 500;
/// Whole turns kept, the most recently read.
const MAX_TURNS: usize = 20;
/// The most messages of one turn held (its start).
const MAX_TURN_MESSAGES: usize = 2000;

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

/// The feed as this window has it.
#[derive(Debug, Clone)]
pub struct FeedStore {
    /// This machine's name, as the service names it in entries it uploads.
    machine: String,
    entries: Vec<FeedEntry>,
    /// `None` until the service first answered.
    status: Option<FeedStatus>,
    /// The service's generation and revision these entries are at.
    generation: Option<u64>,
    revision: Option<u64>,
    /// The selected entry's id; `None` selects the newest.
    selected: Option<String>,
    /// Rows of the selected reply scrolled past (the detail).
    pub scroll: usize,
    /// The selected reply fills the list's area (a phone, which has no
    /// detail beside the list).
    pub reading: bool,
    /// A page was asked for and hasn't come back.
    pub(super) busy: bool,
    /// When to ask again while the feed is shown.
    pub(super) due: Option<Instant>,
    /// Answers for an older request generation are dropped.
    pub(super) request_generation: u64,
    /// Entries' turns, the most recently read last.
    turns: VecDeque<(String, TurnState)>,
    /// A turn is being read.
    pub(super) turn_busy: bool,
}

impl FeedStore {
    pub fn new(machine: String) -> Self {
        Self {
            machine,
            entries: Vec::new(),
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
        }
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
    pub fn entries(&self) -> &[FeedEntry] {
        &self.entries
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
            self.scroll = 0;
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
        // An entry that went away leaves the selection where it was.
        if self.selected_index().is_none() && self.selected.is_some() {
            self.selected = selected
                .map(|index| index.min(self.entries.len().saturating_sub(1)))
                .and_then(|index| self.entries.get(index))
                .map(|entry| entry.id.clone());
        }
        page.more
    }

    /// The selected entry's position; `None` when there are none.
    pub fn selected_index(&self) -> Option<usize> {
        if self.entries.is_empty() {
            return None;
        }
        match &self.selected {
            None => Some(0),
            Some(id) => self.entries.iter().position(|entry| &entry.id == id),
        }
    }

    pub fn selected(&self) -> Option<&FeedEntry> {
        self.selected_index()
            .and_then(|index| self.entries.get(index))
    }

    /// Select the entry at `index` (clamped); its reply is read from the top.
    pub fn select(&mut self, index: usize) {
        let Some(last) = self.entries.len().checked_sub(1) else {
            return;
        };
        let id = self.entries[index.min(last)].id.clone();
        if self.selected.as_ref() != Some(&id) {
            self.scroll = 0;
        }
        self.selected = Some(id);
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

    /// Forget everything (the account signed out elsewhere).
    pub fn clear(&mut self) {
        *self = Self {
            request_generation: self.request_generation + 1,
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
