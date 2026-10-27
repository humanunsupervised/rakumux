//! Where sessions have worked (`SessionView::places`: the git working trees
//! where the daemon saw them change files), shown in the session list: each
//! row has a pair of lines for the directory it works in now, and under it,
//! when opened, a pair for every other place, most recent first
//! ([`listed_places`]). Closed by default, and per window: `w` opens or
//! closes the selected session's, `W` every one's, a click or tap on its
//! `▸`/`▾` or count too. Not saved. The list keys step through the selected
//! session's open places as if each were a row under it, and `y` copies the
//! path of the one they are on (else of where the session works now); a
//! click or tap on a place, the current one too, copies its path.

use session_core::ids::SessionId;
use session_core::model::{SessionPlace, SessionView};
use std::path::{Path, PathBuf};

use super::App;
use crate::layout::SessionLayout;

/// One of a session row's places, as the list shows them: each a pair of
/// lines.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlaceRef {
    /// Where the session works now ([`current_place`]), under its facts.
    Current,
    /// That index in its [`listed_places`], under the current one while
    /// they're open.
    Listed(usize),
}

/// The directory `session` works in now: its git context's working tree
/// (which follows its agent's), else its terminal's directory.
pub fn current_place(session: &SessionView) -> &Path {
    session
        .git
        .as_ref()
        .map_or(session.cwd.as_path(), |git| git.repo_root.as_path())
}

/// The places `session`'s open list shows: every one but
/// [`current_place`] (its place line says that one), most recent first.
pub fn listed_places(session: &SessionView) -> impl Iterator<Item = &SessionPlace> {
    let current = current_place(session);
    session
        .places
        .iter()
        .filter(move |place| place.path != current)
}

/// How many places `session` has worked in besides [`current_place`].
pub fn other_places(session: &SessionView) -> usize {
    listed_places(session).count()
}

impl App {
    /// Whether the session's places are open under its row.
    pub fn places_open(&self, id: &SessionId) -> bool {
        self.places_open.contains(id)
    }

    /// `w`: open or close where the selected session has worked.
    pub fn toggle_places(&mut self) {
        let Some(session) = self.list.selected_session() else {
            self.set_status("Select a session to see where it has worked.");
            return;
        };
        let id = session.id.clone();
        if !self.places_open(&id) && other_places(session) == 0 {
            let name = session.name.clone();
            self.set_status(format!("{name} hasn't changed files anywhere else yet."));
            return;
        }
        self.toggle_places_of(id);
    }

    /// `W`: open where every session has worked, or close them all when
    /// every one with somewhere else is open.
    pub fn toggle_all_places(&mut self) {
        let elsewhere: Vec<SessionId> = self
            .list
            .sessions()
            .iter()
            .filter(|session| other_places(session) > 0)
            .map(|session| session.id.clone())
            .collect();
        if !elsewhere.is_empty() && elsewhere.iter().all(|id| self.places_open(id)) {
            self.places_open.clear();
        } else if elsewhere.is_empty() && self.places_open.is_empty() {
            self.set_status("No session has changed files anywhere else yet.");
        } else {
            self.places_open.extend(elsewhere);
        }
    }

    /// Open or close `id`'s places (a click on its place line).
    pub(super) fn toggle_places_of(&mut self, id: SessionId) {
        if !self.places_open.remove(&id) {
            self.places_open.insert(id);
        }
    }

    /// The paths of the selected session's places, while they're open in
    /// the list on screen.
    fn open_places(&self) -> Option<Vec<PathBuf>> {
        if self.session_layout() != SessionLayout::Sidebar || self.screen().list.is_none() {
            return None;
        }
        let session = self.list.selected_session()?;
        let paths: Vec<PathBuf> = listed_places(session).map(|p| p.path.clone()).collect();
        (self.places_open(&session.id) && !paths.is_empty()).then_some(paths)
    }

    /// `↓`/`↑` (`j`/`k`): one row, the selected session's open places
    /// counting as rows under it.
    pub fn step(&mut self, delta: isize) {
        if let Some(paths) = self.open_places() {
            let at = self
                .list
                .selected_place()
                .and_then(|place| paths.iter().position(|path| path == place));
            let onto = match at {
                None if delta > 0 => Some(Some(0)),
                Some(0) if delta < 0 => Some(None),
                Some(at) if delta < 0 => Some(Some(at - 1)),
                Some(at) if at + 1 < paths.len() => Some(Some(at + 1)),
                _ => None,
            };
            if let Some(onto) = onto {
                self.list
                    .select_place(onto.and_then(|at| paths.get(at).cloned()));
                return;
            }
        }
        let before = self.list.selected_id().cloned();
        self.move_selection(delta);
        // Up into a session whose places are open: onto its last one.
        if delta < 0 && self.list.selected_id() != before.as_ref() {
            if let Some(mut paths) = self.open_places() {
                self.list.select_place(paths.pop());
            }
        }
    }

    /// `y`: the path of the open place the list keys are on, else of where
    /// the selected session works now, on the clipboard.
    pub fn copy_place(&mut self) {
        let path = match self.list.selected_place() {
            Some(place) => place.to_path_buf(),
            None => match self.list.selected_session() {
                Some(session) => current_place(session).to_path_buf(),
                None => {
                    self.set_status("Select a session to copy where it works.");
                    return;
                }
            },
        };
        self.copy_path(&path);
    }

    /// A click or tap on a place of the session at list row `row`: its
    /// path on the clipboard.
    pub(super) fn copy_place_at(&mut self, row: usize, place: PlaceRef) {
        let path = match (self.list.rows().get(row), place) {
            (Some(super::ListRow::Session(session)), PlaceRef::Current) => {
                Some(current_place(session).to_path_buf())
            }
            (Some(super::ListRow::Session(session)), PlaceRef::Listed(index)) => {
                listed_places(session)
                    .nth(index)
                    .map(|place| place.path.clone())
            }
            _ => None,
        };
        if let Some(path) = path {
            self.copy_path(&path);
        }
    }

    /// `path` on the clipboard of the terminal this window runs in (OSC 52,
    /// also over SSH where the terminal allows it).
    fn copy_path(&mut self, path: &Path) {
        use base64::Engine;
        let text = path.to_string_lossy();
        let data = base64::engine::general_purpose::STANDARD.encode(text.as_bytes());
        self.terminal_output
            .extend_from_slice(format!("\x1b]52;c;{data}\x07").as_bytes());
        self.set_status(format!("Copied {text}"));
    }
}
