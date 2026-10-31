//! AI session titles: a finished request's title names its session through
//! the daemon (`session_rename`, as the user), unless the user named it.
//!
//! A session's name is the service's to replace while it is still the one
//! Rakumux generated (`auto_named`) or exactly the title this service last
//! gave it, kept per session in `titles.json` (0600) so a restarted service
//! still knows it. Once the user renames it (the TUI's `r`, `rakumux rename`,
//! the API or MCP), automatic titles leave it alone. A title asked for (`s`,
//! the ↻) always applies, and the session counts as AI-titled again. Names
//! are unique within a folder: a title another session there has gets the
//! first free ` 2`, ` 3`, … after it.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::panic::{self, AssertUnwindSafe};
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use session_core::api::ErrorCode;
use session_core::client::SessionApi;
use session_core::ids::SessionId;

use crate::status::{SessionTitle, MAX_CACHE_ENTRIES};

/// Sessions whose last title is remembered; the least recently titled go first.
const MAX_TITLES: usize = MAX_CACHE_ENTRIES;
/// A session's titled chats remembered: its newest.
const MAX_CHATS: usize = 16;
/// The longest conversation id kept (the daemon's own bound).
const MAX_CHAT_CHARS: usize = 128;
/// The daemon's longest session name, in characters.
const MAX_NAME_CHARS: usize = 64;
/// The highest suffix tried for a title another session in the folder has.
const MAX_SUFFIX: u32 = 99;
/// Renames tried when another session takes the chosen name meanwhile.
const ATTEMPTS: usize = 2;

/// A title to give a session, from a finished request. Run [`Retitle::apply`]
/// off the owner thread and pass its result to `StatusRefresher::retitled`.
pub struct Retitle {
    pub(super) session: SessionId,
    pub(super) title: SessionTitle,
    /// Asked for by the user: it replaces any name.
    pub(super) asked: bool,
    /// The name this service last gave the session, if any.
    pub(super) previous: Option<String>,
}

/// How a [`Retitle`] ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum RetitleOutcome {
    /// The session is named this now (renamed, or it already was).
    Named(String),
    /// The user named the session: its name stays.
    Kept,
    /// The daemon refused or couldn't be read; nothing changed.
    Failed,
}

/// A finished [`Retitle`].
pub struct Retitled {
    pub(super) session: SessionId,
    pub(super) outcome: RetitleOutcome,
}

impl Retitle {
    /// Name the session through `api` (a user client of the daemon). A panic
    /// counts as a failure, so the result always comes back.
    pub fn apply<Api: SessionApi + ?Sized>(self, api: &Api) -> Retitled {
        let outcome = panic::catch_unwind(AssertUnwindSafe(|| self.name(api)))
            .unwrap_or(RetitleOutcome::Failed);
        Retitled {
            session: self.session,
            outcome,
        }
    }

    fn name<Api: SessionApi + ?Sized>(&self, api: &Api) -> RetitleOutcome {
        for _ in 0..ATTEMPTS {
            let Ok(view) = api.get(&self.session) else {
                return RetitleOutcome::Failed;
            };
            let ours = view.auto_named || self.previous.as_deref() == Some(view.name.as_str());
            if !self.asked && !ours {
                return RetitleOutcome::Kept;
            }
            let Ok(sessions) = api.list(Some(&view.scope), true) else {
                return RetitleOutcome::Failed;
            };
            let taken: HashSet<&str> = sessions
                .iter()
                .filter(|other| other.id != view.id)
                .map(|other| other.name.as_str())
                .collect();
            let Some(name) = free_name(self.title.as_str(), &taken) else {
                return RetitleOutcome::Failed;
            };
            if name == view.name {
                return RetitleOutcome::Named(name);
            }
            match api.rename(&view.id, &name) {
                Ok(renamed) => return RetitleOutcome::Named(renamed.name),
                // Another session took that name meanwhile: choose again.
                Err(error) if error.code == ErrorCode::InvalidArgument => continue,
                Err(_) => return RetitleOutcome::Failed,
            }
        }
        RetitleOutcome::Failed
    }
}

/// `title`, else the first of `title 2` … `title 99` no other session in the
/// folder has.
fn free_name(title: &str, taken: &HashSet<&str>) -> Option<String> {
    std::iter::once(title.to_owned())
        .chain((2..=MAX_SUFFIX).map(|n| format!("{title} {n}")))
        .find(|name| !taken.contains(name.as_str()))
}

impl fmt::Debug for Retitle {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Retitle { title: [REDACTED] }")
    }
}

impl fmt::Debug for Retitled {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Retitled { name: [REDACTED] }")
    }
}

/// The name this service last gave each session, kept in a file (0600) when
/// given one.
#[derive(Default)]
pub(crate) struct Titles {
    path: Option<PathBuf>,
    entries: HashMap<String, Remembered>,
    /// Increases with each change, for keeping the most recently titled.
    clock: u64,
}

#[derive(Clone, Serialize, Deserialize)]
struct Remembered {
    name: String,
    used: u64,
}

#[derive(Serialize, Deserialize)]
struct TitleFile {
    version: u8,
    sessions: HashMap<String, Remembered>,
}

impl Titles {
    /// What `path` remembers (nothing when it's missing or unreadable).
    pub(crate) fn load(path: Option<PathBuf>) -> Self {
        let mut entries: HashMap<String, Remembered> = path
            .as_ref()
            .and_then(|path| std::fs::read(path).ok())
            .and_then(|bytes| serde_json::from_slice::<TitleFile>(&bytes).ok())
            .filter(|file| file.version == 1)
            .map(|file| file.sessions)
            .unwrap_or_default();
        entries.retain(|session, entry| {
            !session.is_empty()
                && session.len() <= 256
                && !entry.name.is_empty()
                && entry.name.chars().count() <= MAX_NAME_CHARS
        });
        let clock = entries.values().map(|entry| entry.used).max().unwrap_or(0);
        let mut titles = Self {
            path,
            entries,
            clock,
        };
        titles.bound();
        titles
    }

    /// The name this service last gave `session`.
    pub(crate) fn get(&self, session: &SessionId) -> Option<String> {
        self.entries
            .get(session.as_str())
            .map(|entry| entry.name.clone())
    }

    /// This service named `session` `name`.
    pub(crate) fn set(&mut self, session: &SessionId, name: String) {
        if self.get(session).as_deref() == Some(name.as_str()) {
            return;
        }
        self.clock += 1;
        self.entries.insert(
            session.as_str().to_owned(),
            Remembered {
                name,
                used: self.clock,
            },
        );
        self.bound();
        self.save();
    }

    /// The daemon deleted `session`.
    pub(crate) fn forget(&mut self, session: &SessionId) {
        if self.entries.remove(session.as_str()).is_some() {
            self.save();
        }
    }

    /// Forget every session but `known` (all the daemon has, dead ones too).
    pub(crate) fn keep_only(&mut self, known: &HashSet<&str>) {
        let before = self.entries.len();
        self.entries
            .retain(|session, _| known.contains(session.as_str()));
        if self.entries.len() != before {
            self.save();
        }
    }

    fn bound(&mut self) {
        while self.entries.len() > MAX_TITLES {
            let Some(oldest) = self
                .entries
                .iter()
                .min_by_key(|(_, entry)| entry.used)
                .map(|(id, _)| id.clone())
            else {
                break;
            };
            self.entries.remove(&oldest);
        }
    }

    fn save(&self) {
        let Some(path) = &self.path else {
            return;
        };
        let file = TitleFile {
            version: 1,
            sessions: self.entries.clone(),
        };
        let Ok(bytes) = serde_json::to_vec(&file) else {
            return;
        };
        if crate::feed::write_private(path, &bytes).is_err() {
            eprintln!("rakumux-status: saving session titles failed");
        }
    }
}

impl fmt::Debug for Titles {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Titles")
            .field("sessions", &self.entries.len())
            .finish()
    }
}

/// The sessions whose one automatic title was made, each with the chats
/// (agent conversations) it and titles asked for were made in, kept in a file
/// (0600) when given one, so a restarted service, or one following a daemon
/// restarted in place, doesn't title them again. A session with any chat
/// listed has had its title: a file an older release wrote, which listed every
/// chat it titled, reads the same, and that release reads this one's as
/// before. Conversation ids are the agents' own, the same across restarts; a
/// chat without one, or a program whose turns the daemon doesn't follow, is
/// `""`. A session's newest [`MAX_CHATS`] are kept, and the [`MAX_TITLES`]
/// most recently titled sessions; a session the daemon deleted is forgotten.
#[derive(Default)]
pub(crate) struct TitledChats {
    path: Option<PathBuf>,
    sessions: HashMap<String, Chats>,
    /// Increases with each change, for keeping the most recently titled.
    clock: u64,
}

#[derive(Clone, Serialize, Deserialize)]
struct Chats {
    /// Oldest first.
    chats: Vec<String>,
    used: u64,
}

#[derive(Serialize, Deserialize)]
struct ChatsFile {
    version: u8,
    sessions: HashMap<String, Chats>,
}

impl TitledChats {
    /// What `path` remembers (nothing when it's missing or unreadable).
    pub(crate) fn load(path: Option<PathBuf>) -> Self {
        let mut sessions: HashMap<String, Chats> = path
            .as_ref()
            .and_then(|path| std::fs::read(path).ok())
            .and_then(|bytes| serde_json::from_slice::<ChatsFile>(&bytes).ok())
            .filter(|file| file.version == 1)
            .map(|file| file.sessions)
            .unwrap_or_default();
        sessions.retain(|session, entry| {
            entry
                .chats
                .retain(|chat| chat.chars().count() <= MAX_CHAT_CHARS);
            let excess = entry.chats.len().saturating_sub(MAX_CHATS);
            entry.chats.drain(..excess);
            !session.is_empty() && session.len() <= 256 && !entry.chats.is_empty()
        });
        let clock = sessions.values().map(|entry| entry.used).max().unwrap_or(0);
        let mut titled = Self {
            path,
            sessions,
            clock,
        };
        titled.bound();
        titled
    }

    /// Every session's titled chats.
    pub(crate) fn iter(&self) -> impl Iterator<Item = (SessionId, &str)> + '_ {
        self.sessions.iter().flat_map(|(session, entry)| {
            entry
                .chats
                .iter()
                .map(move |chat| (SessionId::from_string(session.clone()), chat.as_str()))
        })
    }

    /// `session` has its title, made in `chat`.
    pub(crate) fn add(&mut self, session: &SessionId, chat: &str) {
        if chat.chars().count() > MAX_CHAT_CHARS {
            return;
        }
        let entry = self
            .sessions
            .entry(session.as_str().to_owned())
            .or_insert(Chats {
                chats: Vec::new(),
                used: 0,
            });
        if entry.chats.iter().any(|known| known == chat) {
            return;
        }
        entry.chats.push(chat.to_owned());
        let excess = entry.chats.len().saturating_sub(MAX_CHATS);
        entry.chats.drain(..excess);
        self.clock += 1;
        entry.used = self.clock;
        self.bound();
        self.save();
    }

    /// The daemon deleted `session`.
    pub(crate) fn forget(&mut self, session: &SessionId) {
        if self.sessions.remove(session.as_str()).is_some() {
            self.save();
        }
    }

    /// Forget every session but `known` (all the daemon has, dead ones too).
    pub(crate) fn keep_only(&mut self, known: &HashSet<&str>) {
        let before = self.sessions.len();
        self.sessions
            .retain(|session, _| known.contains(session.as_str()));
        if self.sessions.len() != before {
            self.save();
        }
    }

    /// Sharing stopped and its text was dropped: every session gets its
    /// automatic title again once sharing resumes.
    pub(crate) fn clear(&mut self) {
        if !self.sessions.is_empty() {
            self.sessions.clear();
            self.save();
        }
    }

    fn bound(&mut self) {
        while self.sessions.len() > MAX_TITLES {
            let Some(oldest) = self
                .sessions
                .iter()
                .min_by_key(|(_, entry)| entry.used)
                .map(|(id, _)| id.clone())
            else {
                break;
            };
            self.sessions.remove(&oldest);
        }
    }

    fn save(&self) {
        let Some(path) = &self.path else {
            return;
        };
        let file = ChatsFile {
            version: 1,
            sessions: self.sessions.clone(),
        };
        let Ok(bytes) = serde_json::to_vec(&file) else {
            return;
        };
        if crate::feed::write_private(path, &bytes).is_err() {
            eprintln!("rakumux-status: saving titled chats failed");
        }
    }
}

impl fmt::Debug for TitledChats {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TitledChats")
            .field("sessions", &self.sessions.len())
            .finish()
    }
}

#[cfg(test)]
mod tests;
