//! The left pane's state: every session the daemon listed, kept live from
//! its event stream, narrowed by the folder filter (none: all folders) and
//! the text filter. With more than one folder shown the sessions are
//! grouped under a header row per folder: the launch folder first, then by
//! folder name, then by path. Activity never moves a group.
//!
//! The order never changes by itself: activity doesn't move a session.
//! Inside a folder, pinned sessions come first. Both parts follow the
//! order the user gave them (a drag, or `J`/`K`; `sessions.session_order`),
//! and sessions without a place go after the others, oldest first, so a
//! new session appears at the bottom of its folder. Both layouts use this
//! order. Selection is tracked by session id (or header folder) so it
//! survives moves and updates made by other clients.

use std::collections::{HashMap, HashSet};

use session_core::api::{EventEnvelope, SessionEvent};
use session_core::ids::SessionId;
use session_core::model::{ScopeRoot, SessionStatus, SessionView};

use super::config::SessionOrder;
use super::folders::compare_folders;
use super::text_input::TextInput;

/// What is selected in the list: the folder filter (the first row), a
/// folder's header, a session, or "+ New session" (the last row).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Selection {
    /// The folder filter: Enter opens the Folders modal.
    Folders,
    /// A folder's header row (several folders shown): new sessions go in
    /// that folder.
    FolderHeader(ScopeRoot),
    NewSession,
    Session(SessionId),
}

/// One row of the list, top to bottom.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ListRow<'a> {
    /// The folder filter at the top: which folders are shown.
    Folders,
    /// A folder's header row (several folders shown).
    FolderHeader(&'a ScopeRoot),
    NewSession,
    Session(&'a SessionView),
}

/// The folders whose sessions the last snapshot brought in full, so a
/// session of theirs missing from it is gone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Listed {
    /// Every folder (`sessions.all`), including ones that get their first
    /// session later.
    All,
    /// These folders only (an older daemon).
    Folders(Vec<ScopeRoot>),
}

/// Effect of an event on the list, as far as the caller cares.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ListChange {
    /// The event changed nothing the list keeps.
    Ignored,
    Changed,
    /// The selected session was removed; the selection moved.
    SelectedRemoved(SessionId),
}

#[derive(Debug)]
pub struct SessionList {
    /// The folder Rakumux started in: its group comes first.
    launch: ScopeRoot,
    /// The folder filter: the folders shown, or every folder when empty.
    folders: Vec<ScopeRoot>,
    /// The folders shown, in display order: the filter's, or with no
    /// filter every folder that has sessions.
    groups: Vec<ScopeRoot>,
    /// Every listed session, of every folder; inside a folder in list
    /// order (pinned first).
    sessions: Vec<SessionView>,
    /// The order the user gave each folder's sessions, first to last.
    order: HashMap<ScopeRoot, Vec<SessionId>>,
    /// What the last snapshot brought in full.
    listed: Listed,
    selection: Selection,
    pub filter: TextInput,
    /// `/` was pressed and keys go to the filter.
    pub filter_editing: bool,
    loaded: bool,
    /// The folder filter left nothing to select: the next snapshot selects
    /// the most recent session, like the first one.
    reselect: bool,
}

impl SessionList {
    /// An empty list for a TUI started in `launch`, showing all folders.
    pub fn new(launch: ScopeRoot) -> Self {
        Self {
            launch,
            folders: Vec::new(),
            groups: Vec::new(),
            sessions: Vec::new(),
            order: HashMap::new(),
            listed: Listed::Folders(Vec::new()),
            selection: Selection::NewSession,
            filter: TextInput::default(),
            filter_editing: false,
            loaded: false,
            reselect: false,
        }
    }

    /// Show these folders (none: all folders). Nothing is fetched: the
    /// list keeps every folder's sessions. A selection the filter hides
    /// goes to the most recent session shown; so does one on the folder
    /// filter or "+ New session".
    pub fn set_folder_filter(&mut self, folders: Vec<ScopeRoot>) {
        let mut unique: Vec<ScopeRoot> = Vec::with_capacity(folders.len());
        for folder in folders {
            if !unique.contains(&folder) {
                unique.push(folder);
            }
        }
        self.folders = unique;
        self.sort_groups();
        let rows = self.rows();
        let kept = match &self.selection {
            Selection::Session(id) => rows
                .iter()
                .any(|r| matches!(r, ListRow::Session(s) if &s.id == id)),
            Selection::FolderHeader(folder) => rows.contains(&ListRow::FolderHeader(folder)),
            Selection::Folders | Selection::NewSession => false,
        };
        drop(rows);
        if !kept {
            self.selection = self.default_selection();
            self.reselect = self.selection == Selection::NewSession;
        }
    }

    /// The folder filter: the folders shown, or none for all folders.
    pub fn folder_filter(&self) -> &[ScopeRoot] {
        &self.folders
    }

    /// Several folders are shown, under header rows.
    pub fn is_grouped(&self) -> bool {
        self.groups.len() > 1
    }

    /// The folders shown, in display order.
    pub fn groups(&self) -> &[ScopeRoot] {
        &self.groups
    }

    /// Whether the last snapshot brought `folder`'s sessions (events keep
    /// them current).
    pub fn is_listed(&self, folder: &ScopeRoot) -> bool {
        match &self.listed {
            Listed::All => true,
            Listed::Folders(folders) => folders.contains(folder),
        }
    }

    /// Use the order the user gave the sessions (from the config).
    pub fn set_session_order(&mut self, order: &SessionOrder) {
        self.order = order
            .iter()
            .map(|(path, ids)| (ScopeRoot::from_canonical(path.clone()), ids.clone()))
            .collect();
        self.sort();
    }

    /// The order to save: for the folders listed in full, ids of sessions
    /// that are gone are dropped; folders left without any are too.
    pub fn session_order(&self) -> SessionOrder {
        self.order
            .iter()
            .filter_map(|(scope, ids)| {
                let ids: Vec<SessionId> = if self.is_listed(scope) {
                    ids.iter()
                        .filter(|id| self.get(id).is_some())
                        .cloned()
                        .collect()
                } else {
                    ids.clone()
                };
                (!ids.is_empty()).then(|| (scope.path().to_path_buf(), ids))
            })
            .collect()
    }

    /// Where `id` is among the visible sessions.
    pub fn visible_position(&self, id: &SessionId) -> Option<usize> {
        self.visible().iter().position(|s| &s.id == id)
    }

    /// The visible position a drag over list row `row` aims at: a
    /// session's own, or for another row the next session's below it (the
    /// folder filter: the first; "+ New session": past the last).
    pub fn position_at_row(&self, row: usize) -> usize {
        self.rows()
            .iter()
            .take(row)
            .filter(|r| matches!(r, ListRow::Session(_)))
            .count()
    }

    /// Move session `id` to visible position `to`, where a drag drops it or
    /// `J`/`K` take it. It stays in its part of the list (its folder's
    /// pinned sessions, or the others): a `to` outside that part means its
    /// first or last place. Returns whether it moved.
    pub fn move_session(&mut self, id: &SessionId, to: usize) -> bool {
        let visible = self.visible();
        let Some(from) = visible.iter().position(|s| &s.id == id) else {
            return false;
        };
        let moving = visible[from];
        let in_part = |s: &&SessionView| s.scope == moving.scope && s.pinned == moving.pinned;
        // A part is contiguous in the list.
        let first = visible.iter().position(in_part).unwrap_or(from);
        let last = visible.iter().rposition(in_part).unwrap_or(from);
        let to = to.clamp(first, last);
        if to == from {
            return false;
        }
        let scope = moving.scope.clone();
        let target = visible[to].id.clone();
        let mut order: Vec<SessionId> = self
            .sessions
            .iter()
            .filter(|s| s.scope == scope)
            .map(|s| s.id.clone())
            .collect();
        let (Some(from), Some(to)) = (
            order.iter().position(|s| s == id),
            order.iter().position(|s| s == &target),
        ) else {
            return false;
        };
        // Taking it out moves a target below it up one, so it lands after
        // a target below and before one above.
        let moved = order.remove(from);
        order.insert(to, moved);
        self.order.insert(scope, order);
        self.sort();
        true
    }

    /// A list snapshot has been received at least once.
    pub fn is_loaded(&self) -> bool {
        self.loaded
    }

    /// Replace everything with a fresh snapshot of the `listed` folders.
    /// The first snapshot selects the most recently active session shown;
    /// later ones keep the selection while it's still there.
    pub fn replace_all(&mut self, sessions: Vec<SessionView>, listed: Listed) {
        let first = !self.loaded || std::mem::take(&mut self.reselect);
        self.loaded = true;
        self.sessions = sessions;
        self.listed = listed;
        self.sort();
        self.sort_groups();
        match &self.selection {
            Selection::Session(id) if self.index_of(id).is_some() => {}
            Selection::Session(_) => self.selection = self.default_selection(),
            Selection::NewSession if first => self.selection = self.default_selection(),
            Selection::Folders | Selection::FolderHeader(_) | Selection::NewSession => {}
        }
        self.ensure_selection_visible();
    }

    /// Apply one subscription event. The daemon sends only the folders
    /// subscribed to, so every event counts, whatever the folder filter.
    pub fn apply(&mut self, envelope: &EventEnvelope) -> ListChange {
        let id = &envelope.session_id;
        match &envelope.event {
            SessionEvent::Created { session } | SessionEvent::Updated { session } => {
                self.upsert(session.clone());
                ListChange::Changed
            }
            SessionEvent::Removed => match self.remove(id) {
                Some(change) => change,
                None => ListChange::Ignored,
            },
            SessionEvent::Exited { code } => self.modify(id, |s| {
                s.status = SessionStatus::Exited;
                s.exit_code = *code;
                s.terminal = None;
            }),
            SessionEvent::Lifecycle { lifecycle } => self.modify(id, |s| s.lifecycle = *lifecycle),
            SessionEvent::Reported { reported } => {
                self.modify(id, |s| s.reported = Some(reported.clone()))
            }
            // Output is shown by the attached pane; nothing to update.
            SessionEvent::Activity | SessionEvent::Unknown => ListChange::Ignored,
        }
    }

    /// Insert or replace a session (by id).
    pub fn upsert(&mut self, view: SessionView) {
        match self.index_of(&view.id) {
            Some(i) => self.sessions[i] = view,
            None => self.sessions.push(view),
        }
        self.sort();
        self.sort_groups();
        self.ensure_selection_visible();
    }

    /// Remove a session. When it was selected, the selection moves to the
    /// session that takes its place (or the one before, or "New session").
    pub fn remove(&mut self, id: &SessionId) -> Option<ListChange> {
        let visible_before: Vec<SessionId> = self.visible().iter().map(|s| s.id.clone()).collect();
        let index = self.index_of(id)?;
        self.sessions.remove(index);
        self.sort_groups();
        if self.selection == Selection::Session(id.clone()) {
            let row = visible_before.iter().position(|v| v == id).unwrap_or(0);
            let visible = self.visible();
            self.selection = visible
                .get(row)
                .or_else(|| row.checked_sub(1).and_then(|r| visible.get(r)))
                .map(|s| Selection::Session(s.id.clone()))
                .unwrap_or(Selection::NewSession);
            return Some(ListChange::SelectedRemoved(id.clone()));
        }
        self.ensure_selection_visible();
        Some(ListChange::Changed)
    }

    /// Record that a session's process ended (seen on its attach stream).
    pub fn mark_exited(&mut self, id: &SessionId, code: Option<i32>) {
        self.modify(id, |s| {
            s.status = SessionStatus::Exited;
            s.exit_code = code;
            s.terminal = None;
        });
    }

    pub fn get(&self, id: &SessionId) -> Option<&SessionView> {
        self.sessions.iter().find(|s| &s.id == id)
    }

    /// Every listed session, whatever the filters.
    pub fn sessions(&self) -> &[SessionView] {
        &self.sessions
    }

    /// `(live, total)` sessions of one folder.
    pub fn counts(&self, scope: &ScopeRoot) -> (usize, usize) {
        let mut counts = (0, 0);
        for session in self.sessions.iter().filter(|s| &s.scope == scope) {
            counts.1 += 1;
            if session.status == SessionStatus::Live {
                counts.0 += 1;
            }
        }
        counts
    }

    fn is_filtering(&self) -> bool {
        !self.filter.value().trim().is_empty()
    }

    /// Sessions of the folders shown that match the text filter, in
    /// display order (folder by folder).
    pub fn visible(&self) -> Vec<&SessionView> {
        let needle = self.filter.value().trim().to_lowercase();
        let matching = |s: &&SessionView| needle.is_empty() || self.matches(s, &needle);
        let mut visible = Vec::with_capacity(self.sessions.len());
        for group in &self.groups {
            visible.extend(
                self.sessions
                    .iter()
                    .filter(|s| &s.scope == group)
                    .filter(matching),
            );
        }
        visible
    }

    /// Every row: the folder filter, the sessions, under a header per
    /// folder when several are shown, then "+ New session" last. While
    /// filtering by text, folders without a match have no header.
    pub fn rows(&self) -> Vec<ListRow<'_>> {
        let visible = self.visible();
        let mut rows = Vec::with_capacity(visible.len() + self.groups.len() + 2);
        rows.push(ListRow::Folders);
        if !self.is_grouped() {
            rows.extend(visible.into_iter().map(ListRow::Session));
            rows.push(ListRow::NewSession);
            return rows;
        }
        let filtering = self.is_filtering();
        for group in &self.groups {
            let start = rows.len();
            rows.push(ListRow::FolderHeader(group));
            rows.extend(
                visible
                    .iter()
                    .filter(|s| &s.scope == group)
                    .map(|s| ListRow::Session(s)),
            );
            if filtering && rows.len() == start + 1 {
                rows.pop();
            }
        }
        rows.push(ListRow::NewSession);
        rows
    }

    fn matches(&self, session: &SessionView, needle: &str) -> bool {
        session.name.to_lowercase().contains(needle)
            || session
                .cwd
                .to_string_lossy()
                .to_lowercase()
                .contains(needle)
            || session
                .git
                .as_ref()
                .and_then(|g| g.branch.as_deref())
                .is_some_and(|b| b.to_lowercase().contains(needle))
            || session
                .reported
                .as_ref()
                .and_then(|r| r.text.as_deref())
                .is_some_and(|t| t.to_lowercase().contains(needle))
    }

    /// Number of rows, including the folder filter, "+ New session" and
    /// folder headers.
    pub fn row_count(&self) -> usize {
        self.rows().len()
    }

    pub fn selection(&self) -> &Selection {
        &self.selection
    }

    pub fn selected_id(&self) -> Option<&SessionId> {
        match &self.selection {
            Selection::Session(id) => Some(id),
            Selection::Folders | Selection::FolderHeader(_) | Selection::NewSession => None,
        }
    }

    pub fn selected_session(&self) -> Option<&SessionView> {
        self.selected_id().and_then(|id| self.get(id))
    }

    /// Row of the selection; the folder filter is the first row, "+ New
    /// session" the last, which also stands in for a selection that isn't
    /// shown.
    pub fn selected_row(&self) -> usize {
        let rows = self.rows();
        let new_session = rows.len() - 1;
        let found = match &self.selection {
            Selection::Folders => Some(0),
            Selection::NewSession => None,
            Selection::FolderHeader(folder) => rows
                .iter()
                .position(|row| matches!(row, ListRow::FolderHeader(f) if *f == folder)),
            Selection::Session(id) => rows
                .iter()
                .position(|row| matches!(row, ListRow::Session(s) if &s.id == id)),
        };
        found.unwrap_or(new_session)
    }

    /// Select the row.
    pub fn select_row(&mut self, row: usize) {
        let selection = match self.rows().get(row) {
            Some(ListRow::Folders) => Selection::Folders,
            Some(ListRow::FolderHeader(folder)) => Selection::FolderHeader((*folder).clone()),
            Some(ListRow::NewSession) => Selection::NewSession,
            Some(ListRow::Session(s)) => Selection::Session(s.id.clone()),
            None => return,
        };
        self.select(selection);
    }

    /// Move the selection by `delta` rows (folder headers included),
    /// clamped. Returns whether it moved.
    pub fn move_selection(&mut self, delta: isize) -> bool {
        let before = self.selection.clone();
        let last = self.row_count().saturating_sub(1) as isize;
        let target = (self.selected_row() as isize + delta).clamp(0, last);
        self.select_row(target as usize);
        self.selection != before
    }

    /// `{`/`}`: what to select `delta` folder groups from the selected
    /// session's or header's (from the folder filter, `}` reaches the first
    /// group; from "+ New session", `{` the last): the group's first
    /// session, or its header when it shows none. `None` past either end,
    /// or without headers.
    pub fn group_target(&self, delta: isize) -> Option<Selection> {
        let rows = self.rows();
        let groups: Vec<&ScopeRoot> = rows
            .iter()
            .filter_map(|row| match row {
                ListRow::FolderHeader(folder) => Some(*folder),
                _ => None,
            })
            .collect();
        if groups.is_empty() {
            return None;
        }
        let position = |found: Option<usize>| found.map_or(0, |i| i as isize);
        let current = match &self.selection {
            Selection::Folders => -1,
            Selection::NewSession => groups.len() as isize,
            Selection::FolderHeader(folder) => position(groups.iter().position(|g| *g == folder)),
            Selection::Session(_) => position(
                self.selected_session()
                    .and_then(|s| groups.iter().position(|g| **g == s.scope)),
            ),
        };
        let group = *groups.get(usize::try_from(current + delta).ok()?)?;
        let first = rows.iter().find_map(|row| match row {
            ListRow::Session(s) if &s.scope == group => Some(s.id.clone()),
            _ => None,
        });
        Some(match first {
            Some(id) => Selection::Session(id),
            None => Selection::FolderHeader(group.clone()),
        })
    }

    pub fn select(&mut self, selection: Selection) {
        self.selection = selection;
        self.reselect = false;
    }

    /// Re-apply the filters: keep the selection when it's still shown,
    /// otherwise select the first session shown (or "New session").
    pub fn ensure_selection_visible(&mut self) {
        let shown = match &self.selection {
            Selection::Session(id) => self.visible().iter().any(|s| &s.id == id),
            Selection::FolderHeader(folder) => self.rows().contains(&ListRow::FolderHeader(folder)),
            Selection::Folders | Selection::NewSession => true,
        };
        if !shown {
            self.selection = self
                .visible()
                .first()
                .map(|s| Selection::Session(s.id.clone()))
                .unwrap_or(Selection::NewSession);
        }
    }

    /// The most recently active visible session, whatever the order.
    fn default_selection(&self) -> Selection {
        self.visible()
            .into_iter()
            .max_by(|a, b| {
                a.last_active_at
                    .cmp(&b.last_active_at)
                    .then(a.created_at.cmp(&b.created_at))
            })
            .map(|s| Selection::Session(s.id.clone()))
            .unwrap_or(Selection::NewSession)
    }

    fn index_of(&self, id: &SessionId) -> Option<usize> {
        self.sessions.iter().position(|s| &s.id == id)
    }

    /// Change a session in place; nothing these events change moves it.
    fn modify(&mut self, id: &SessionId, f: impl FnOnce(&mut SessionView)) -> ListChange {
        match self.index_of(id) {
            Some(i) => {
                f(&mut self.sessions[i]);
                ListChange::Changed
            }
            None => ListChange::Ignored,
        }
    }

    /// Inside a folder: pinned sessions first, then the order the user
    /// gave them, then the rest oldest first. Activity plays no part.
    fn sort(&mut self) {
        let order = &self.order;
        let place = |s: &SessionView| {
            order
                .get(&s.scope)
                .and_then(|ids| ids.iter().position(|id| id == &s.id))
                .unwrap_or(usize::MAX)
        };
        self.sessions.sort_by(|a, b| {
            b.pinned
                .cmp(&a.pinned)
                .then_with(|| place(a).cmp(&place(b)))
                .then(a.created_at.cmp(&b.created_at))
                .then_with(|| a.id.cmp(&b.id))
        });
    }

    /// The folders shown: the filter's, or every folder with sessions; the
    /// launch folder first, then by name, then by path.
    fn sort_groups(&mut self) {
        let mut groups: Vec<ScopeRoot> = if self.folders.is_empty() {
            let mut seen = HashSet::new();
            self.sessions
                .iter()
                .filter(|s| seen.insert(&s.scope))
                .map(|s| s.scope.clone())
                .collect()
        } else {
            self.folders.clone()
        };
        groups.sort_by(|a, b| compare_folders(&self.launch, a, b));
        self.groups = groups;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::testing::{aged, view, view_in};
    use session_core::model::{Lifecycle, Reported};

    fn scope() -> ScopeRoot {
        ScopeRoot::from_canonical("/repo".into())
    }

    fn folder(path: &str) -> ScopeRoot {
        ScopeRoot::from_canonical(path.into())
    }

    fn envelope(id: &str, event: SessionEvent) -> EventEnvelope {
        EventEnvelope {
            scope: scope(),
            session_id: SessionId::from_string(id),
            at: 0,
            event,
        }
    }

    /// A first snapshot of every folder.
    fn loaded(sessions: Vec<SessionView>) -> SessionList {
        let mut list = SessionList::new(scope());
        list.replace_all(sessions, Listed::All);
        list
    }

    fn names(list: &SessionList) -> Vec<String> {
        list.visible().iter().map(|s| s.name.clone()).collect()
    }

    /// Rows as text: `^` for the folder filter, `+`, `# /path` for
    /// headers, session names.
    fn row_names(list: &SessionList) -> Vec<String> {
        list.rows()
            .iter()
            .map(|row| match row {
                ListRow::Folders => "^".to_string(),
                ListRow::NewSession => "+".to_string(),
                ListRow::FolderHeader(scope) => format!("# {}", scope.path().display()),
                ListRow::Session(s) => s.name.clone(),
            })
            .collect()
    }

    fn ids(list: &SessionList) -> Vec<String> {
        list.visible().iter().map(|s| s.id.to_string()).collect()
    }

    fn order_of(path: &str, ids: &[&str]) -> SessionOrder {
        SessionOrder::from([(
            path.into(),
            ids.iter().map(|id| SessionId::from_string(*id)).collect(),
        )])
    }

    #[test]
    fn orders_by_creation_and_selects_the_most_recent_on_first_load() {
        let mut list = SessionList::new(scope());
        assert_eq!(list.selection(), &Selection::NewSession);
        list.replace_all(
            vec![
                aged("C", "third", 3, 20),
                aged("A", "first", 1, 10),
                aged("B", "second", 2, 30),
            ],
            Listed::All,
        );
        assert_eq!(names(&list), ["first", "second", "third"]);
        // Selection, not order: the most recently used.
        assert_eq!(list.selected_id().unwrap().as_str(), "B");
        assert_eq!(list.selected_row(), 2);
    }

    #[test]
    fn activity_never_moves_a_session() {
        let mut list = loaded(vec![
            aged("A", "a", 1, 10),
            aged("B", "b", 2, 20),
            aged("C", "c", 3, 30),
        ]);
        list.select(Selection::Session(SessionId::from_string("B")));
        // A touch (the daemon's update after going in) and agent activity.
        let mut touched = aged("A", "a", 1, 99);
        touched.lifecycle = Lifecycle::Working;
        list.apply(&envelope("A", SessionEvent::Updated { session: touched }));
        list.apply(&envelope(
            "C",
            SessionEvent::Lifecycle {
                lifecycle: Lifecycle::Blocked,
            },
        ));
        list.apply(&envelope("B", SessionEvent::Exited { code: Some(0) }));
        list.replace_all(
            vec![
                aged("C", "c", 3, 500),
                aged("B", "b", 2, 400),
                aged("A", "a", 1, 300),
            ],
            Listed::All,
        );
        assert_eq!(names(&list), ["a", "b", "c"]);
        assert_eq!(list.selected_id().unwrap().as_str(), "B");
        assert_eq!(list.selected_row(), 2);
    }

    #[test]
    fn new_sessions_go_at_the_bottom_of_their_folder() {
        let mut list = SessionList::new(scope());
        list.set_session_order(&order_of("/repo", &["B", "A"]));
        list.replace_all(
            vec![aged("A", "a", 1, 1), aged("B", "b", 2, 2)],
            Listed::All,
        );
        assert_eq!(ids(&list), ["B", "A"]);
        list.apply(&envelope(
            "C",
            SessionEvent::Created {
                session: aged("C", "c", 3, 3),
            },
        ));
        list.upsert(aged("D", "d", 4, 4));
        assert_eq!(ids(&list), ["B", "A", "C", "D"]);
        assert_eq!(row_names(&list).last().unwrap(), "+");
    }

    #[test]
    fn empty_first_load_selects_new_session() {
        let list = loaded(vec![]);
        assert!(list.is_loaded());
        assert_eq!(list.selection(), &Selection::NewSession);
        assert_eq!(list.row_count(), 2);
        assert!(list.groups().is_empty());
    }

    #[test]
    fn every_folder_is_kept_and_the_filter_hides_the_others() {
        let other = folder("/other");
        let mut list = loaded(vec![
            view("A", "here", 1),
            view_in(&other, "X", "elsewhere", 99),
        ]);
        // All folders: both, under headers.
        assert_eq!(
            row_names(&list),
            ["^", "# /repo", "here", "# /other", "elsewhere", "+"]
        );
        list.set_folder_filter(vec![scope()]);
        assert_eq!(names(&list), ["here"]);
        // An event from a folder the filter hides still counts: it shows
        // once that folder is shown.
        let created = EventEnvelope {
            scope: other.clone(),
            session_id: SessionId::from_string("Y"),
            at: 0,
            event: SessionEvent::Created {
                session: view_in(&other, "Y", "new-elsewhere", 100),
            },
        };
        assert_eq!(list.apply(&created), ListChange::Changed);
        assert_eq!(names(&list), ["here"]);
        assert_eq!(list.counts(&other), (2, 2));
        list.set_folder_filter(vec![]);
        assert_eq!(names(&list), ["here", "elsewhere", "new-elsewhere"]);
    }

    #[test]
    fn events_update_status_lifecycle_and_reports() {
        let mut list = loaded(vec![view("A", "a", 1)]);
        list.apply(&envelope(
            "A",
            SessionEvent::Lifecycle {
                lifecycle: Lifecycle::Blocked,
            },
        ));
        list.apply(&envelope(
            "A",
            SessionEvent::Reported {
                reported: Reported {
                    state: None,
                    text: Some("needs review".into()),
                    result: None,
                    at: 5,
                },
            },
        ));
        list.apply(&envelope("A", SessionEvent::Exited { code: Some(3) }));
        let a = list.get(&SessionId::from_string("A")).unwrap();
        assert_eq!(a.lifecycle, Lifecycle::Blocked);
        assert_eq!(
            a.reported.as_ref().unwrap().text.as_deref(),
            Some("needs review")
        );
        assert_eq!(a.status, SessionStatus::Exited);
        assert_eq!(a.exit_code, Some(3));
        assert_eq!(
            list.apply(&envelope("A", SessionEvent::Activity)),
            ListChange::Ignored
        );
    }

    #[test]
    fn removing_the_selected_session_selects_its_neighbour() {
        let mut list = loaded(vec![
            view("A", "a", 10),
            view("B", "b", 20),
            view("C", "c", 30),
        ]);
        list.move_selection(-1);
        assert_eq!(list.selected_id().unwrap().as_str(), "B");
        let change = list.apply(&envelope("B", SessionEvent::Removed));
        assert_eq!(
            change,
            ListChange::SelectedRemoved(SessionId::from_string("B"))
        );
        assert_eq!(list.selected_id().unwrap().as_str(), "C");
        list.apply(&envelope("C", SessionEvent::Removed));
        assert_eq!(list.selected_id().unwrap().as_str(), "A");
        list.apply(&envelope("A", SessionEvent::Removed));
        assert_eq!(list.selection(), &Selection::NewSession);
    }

    #[test]
    fn filter_matches_name_cwd_branch_and_report_text() {
        let mut api = view("A", "api-refactor", 10);
        api.cwd = "/repo/server".into();
        let mut docs = view("B", "docs", 20);
        docs.reported = Some(Reported {
            state: None,
            text: Some("Waiting on API review".into()),
            result: None,
            at: 0,
        });
        let mut spike = view("C", "spike", 30);
        spike.git = Some(session_core::model::GitInfo {
            branch: Some("feature/login".into()),
            ..Default::default()
        });
        let mut list = loaded(vec![api, docs, spike]);
        list.filter.set("API");
        list.ensure_selection_visible();
        assert_eq!(names(&list), ["api-refactor", "docs"]);
        list.filter.set("server");
        assert_eq!(names(&list), ["api-refactor"]);
        list.filter.set("login");
        assert_eq!(names(&list), ["spike"]);
        list.select_row(1);
        list.filter.set("spike");
        list.ensure_selection_visible();
        assert_eq!(list.selected_id().unwrap().as_str(), "C");
        list.filter.set("nothing");
        list.ensure_selection_visible();
        assert_eq!(list.selection(), &Selection::NewSession);
        assert_eq!(list.row_count(), 2);
    }

    #[test]
    fn movement_is_clamped() {
        let mut list = loaded(vec![view("A", "a", 20), view("B", "b", 30)]);
        // ^ / a / b / +
        assert!(list.move_selection(5));
        assert_eq!(list.selected_row(), 3);
        assert_eq!(list.selection(), &Selection::NewSession);
        assert!(!list.move_selection(1));
        assert!(list.move_selection(-2));
        assert_eq!(list.selected_id().unwrap().as_str(), "A");
        // Up from the first session: the folder filter.
        assert!(list.move_selection(-1));
        assert_eq!(list.selection(), &Selection::Folders);
        assert_eq!(list.selected_row(), 0);
        assert_eq!(list.selected_id(), None);
        assert!(!list.move_selection(-1));
        assert!(list.move_selection(1));
        assert_eq!(list.selected_id().unwrap().as_str(), "A");
        assert!(list.move_selection(-10));
        assert_eq!(list.selection(), &Selection::Folders);
    }

    #[test]
    fn the_folder_filter_row_stays_selected_until_the_filter_changes() {
        let web = folder("/web");
        let mut list = loaded(vec![view("A", "a", 30), view_in(&web, "W1", "w", 5)]);
        list.select_row(0);
        assert_eq!(list.selection(), &Selection::Folders);
        // Snapshots and events leave it selected.
        list.replace_all(
            vec![
                view("A", "a", 30),
                view("B", "b", 40),
                view_in(&web, "W1", "w", 5),
            ],
            Listed::All,
        );
        list.upsert(view("C", "c", 50));
        assert_eq!(list.selection(), &Selection::Folders);
        // A filter picked: the most recent session it shows.
        list.set_folder_filter(vec![web.clone()]);
        assert_eq!(list.selected_id().unwrap().as_str(), "W1");
    }

    #[test]
    fn the_saved_order_is_applied_and_sessions_without_a_place_go_last() {
        let mut list = SessionList::new(scope());
        // "X" is gone; C and D have no place, so they follow, oldest first.
        list.set_session_order(&order_of("/repo", &["B", "X", "A"]));
        list.replace_all(
            vec![
                aged("D", "d", 4, 1),
                aged("A", "a", 1, 1),
                aged("C", "c", 3, 1),
                aged("B", "b", 2, 1),
            ],
            Listed::All,
        );
        assert_eq!(ids(&list), ["B", "A", "C", "D"]);
        // Another folder's order doesn't touch this one.
        list.set_session_order(&order_of("/web", &["D", "C", "B", "A"]));
        assert_eq!(ids(&list), ["A", "B", "C", "D"]);
    }

    #[test]
    fn moving_a_session_saves_the_whole_folder_order() {
        let mut list = loaded(vec![
            aged("A", "a", 1, 1),
            aged("B", "b", 2, 1),
            aged("C", "c", 3, 1),
        ]);
        assert!(list.session_order().is_empty());
        let c = SessionId::from_string("C");
        // Up two places, then down one.
        assert!(list.move_session(&c, 0));
        assert_eq!(ids(&list), ["C", "A", "B"]);
        assert!(list.move_session(&c, 1));
        assert_eq!(ids(&list), ["A", "C", "B"]);
        // Already there, or nowhere to go: nothing moves.
        assert!(!list.move_session(&c, 1));
        assert!(!list.move_session(&SessionId::from_string("nope"), 0));
        assert_eq!(list.session_order(), order_of("/repo", &["A", "C", "B"]));
        // A new session goes after them.
        list.upsert(aged("D", "d", 0, 1));
        assert_eq!(ids(&list), ["A", "C", "B", "D"]);
    }

    #[test]
    fn moves_stay_in_the_folder_and_its_pinned_or_unpinned_part() {
        let web = folder("/web");
        let mut pinned = aged("P", "pinned", 1, 1);
        pinned.pinned = true;
        let mut list = loaded(vec![
            pinned,
            aged("A", "a", 2, 1),
            aged("B", "b", 3, 1),
            view_in(&web, "W1", "w1", 4),
            view_in(&web, "W2", "w2", 5),
        ]);
        assert_eq!(ids(&list), ["P", "A", "B", "W1", "W2"]);
        let (a, b, p) = (
            SessionId::from_string("A"),
            SessionId::from_string("B"),
            SessionId::from_string("P"),
        );
        // Past the pinned session: the top of the unpinned part.
        assert!(list.move_session(&b, 0));
        assert_eq!(ids(&list), ["P", "B", "A", "W1", "W2"]);
        assert!(!list.move_session(&b, 0));
        // Into the next folder: the bottom of its own.
        assert!(list.move_session(&b, usize::MAX));
        assert_eq!(ids(&list), ["P", "A", "B", "W1", "W2"]);
        assert!(!list.move_session(&b, 4));
        // The pinned session stays above the others.
        assert!(!list.move_session(&p, 2));
        // The saved order keeps pinned and unpinned in one list per folder.
        assert!(list.move_session(&a, 2));
        assert_eq!(list.session_order(), order_of("/repo", &["P", "B", "A"]));
        // Unpinned, P keeps its place in that order: first.
        let mut unpinned = aged("P", "pinned", 1, 1);
        unpinned.pinned = false;
        list.upsert(unpinned);
        assert_eq!(ids(&list), ["P", "B", "A", "W1", "W2"]);
    }

    #[test]
    fn moves_pass_sessions_the_filter_hides() {
        let mut list = loaded(vec![
            aged("A", "alpha", 1, 1),
            aged("B", "beta", 2, 1),
            aged("C", "alpine", 3, 1),
        ]);
        list.filter.set("al");
        assert_eq!(ids(&list), ["A", "C"]);
        // Down one place among the shown: after C, past the hidden B.
        assert!(list.move_session(&SessionId::from_string("A"), 1));
        assert_eq!(ids(&list), ["C", "A"]);
        list.filter.set("");
        assert_eq!(ids(&list), ["B", "C", "A"]);
    }

    #[test]
    fn rows_aim_drags_at_list_positions() {
        let web = folder("/web");
        let list = loaded(vec![view("R1", "r1", 1), view_in(&web, "W1", "w1", 2)]);
        // ^ / #repo / r1 / #web / w1 / +
        let positions: Vec<usize> = (0..6).map(|row| list.position_at_row(row)).collect();
        assert_eq!(positions, [0, 0, 0, 1, 1, 2]);
    }

    #[test]
    fn the_saved_order_drops_sessions_that_are_gone() {
        let mut list = SessionList::new(scope());
        let mut saved = order_of("/repo", &["B", "A", "GONE"]);
        saved.extend(order_of("/web", &["W9", "W8"]));
        saved.extend(order_of("/elsewhere", &["E1"]));
        list.set_session_order(&saved);
        // Nothing listed yet: nothing is known to be gone.
        assert_eq!(list.session_order(), saved);
        // An older daemon listed /repo and /web only.
        let listed = Listed::Folders(vec![scope(), folder("/web")]);
        list.replace_all(vec![aged("A", "a", 1, 1), aged("B", "b", 2, 1)], listed);
        let mut expected = order_of("/repo", &["B", "A"]);
        expected.extend(order_of("/elsewhere", &["E1"]));
        // /web's sessions are all gone; /elsewhere wasn't listed, so it's kept.
        assert_eq!(list.session_order(), expected);
        list.apply(&envelope("A", SessionEvent::Removed));
        expected.insert("/repo".into(), vec![SessionId::from_string("B")]);
        assert_eq!(list.session_order(), expected);
        // Every folder listed: /elsewhere's session is gone too.
        list.replace_all(vec![aged("B", "b", 2, 1)], Listed::All);
        assert_eq!(list.session_order(), order_of("/repo", &["B"]));
    }

    #[test]
    fn a_later_snapshot_keeps_the_selection() {
        let mut list = loaded(vec![view("A", "a", 30), view("B", "b", 20)]);
        list.select(Selection::Session(SessionId::from_string("B")));
        list.replace_all(vec![view("A", "a", 30), view("B", "b", 40)], Listed::All);
        assert_eq!(list.selected_id().unwrap().as_str(), "B");
        list.select(Selection::NewSession);
        list.replace_all(vec![view("A", "a", 30)], Listed::All);
        assert_eq!(list.selection(), &Selection::NewSession);
    }

    #[test]
    fn pinned_sessions_come_first() {
        let mut pinned = view("C", "newest-pinned", 30);
        pinned.pinned = true;
        let mut list = loaded(vec![
            view("B", "mid", 20),
            pinned.clone(),
            view("A", "old", 10),
        ]);
        assert_eq!(names(&list), ["newest-pinned", "old", "mid"]);
        // The saved order applies within each part.
        list.set_session_order(&order_of("/repo", &["B", "A", "C"]));
        assert_eq!(names(&list), ["newest-pinned", "mid", "old"]);
        // Unpinned, it takes its place in that order.
        pinned.pinned = false;
        list.upsert(pinned);
        assert_eq!(names(&list), ["mid", "old", "newest-pinned"]);
    }

    #[test]
    fn one_folder_has_no_headers() {
        let mut list = loaded(vec![view("A", "a", 1)]);
        assert!(!list.is_grouped());
        assert_eq!(row_names(&list), ["^", "a", "+"]);
        // Filtered to one folder of several: no headers either.
        list.upsert(view_in(&folder("/web"), "W1", "w", 2));
        assert!(list.is_grouped());
        list.set_folder_filter(vec![folder("/web")]);
        assert!(!list.is_grouped());
        assert_eq!(row_names(&list), ["^", "w", "+"]);
    }

    #[test]
    fn folders_are_grouped_launch_first_then_by_name_then_path() {
        let (web, api, other_api) = (folder("/web"), folder("/api"), folder("/x/api"));
        let mut pinned = view_in(&web, "W2", "web-pinned", 80);
        pinned.pinned = true;
        let mut list = loaded(vec![
            view_in(&other_api, "X1", "x-api", 95),
            view("R2", "repo-new", 20),
            view_in(&api, "A1", "api", 90),
            view_in(&web, "W1", "web-old", 5),
            pinned,
            view("R1", "repo-old", 10),
        ]);
        assert!(list.is_grouped());
        // The launch folder leads, then by name (api before web), and two
        // folders with one name by path; whatever was used last.
        assert_eq!(
            row_names(&list),
            [
                "^",
                "# /repo",
                "repo-old",
                "repo-new",
                "# /api",
                "api",
                "# /x/api",
                "x-api",
                "# /web",
                "web-pinned",
                "web-old",
                "+",
            ]
        );
        assert_eq!(list.counts(&web), (2, 2));
        // Activity moves no group.
        list.upsert(view_in(&api, "A1", "api", 999));
        list.upsert(view_in(&web, "W3", "web-new", 1000));
        let groups = [scope(), api.clone(), other_api.clone(), web.clone()];
        assert_eq!(list.groups(), groups);
        // A filter follows the same order, whatever order it was ticked
        // in, and leaves the launch folder out unless it's ticked.
        list.set_folder_filter(vec![web.clone(), api.clone()]);
        assert_eq!(list.groups(), [api.clone(), web.clone()]);
        assert!(list.get(&SessionId::from_string("R1")).is_some());
        assert_eq!(ids(&list), ["A1", "W2", "W1", "W3"]);
        // A folder whose last session goes loses its group.
        list.set_folder_filter(vec![]);
        list.remove(&SessionId::from_string("X1"));
        assert_eq!(list.groups(), [scope(), api, web]);
    }

    #[test]
    fn headers_are_rows_that_can_be_selected() {
        let web = folder("/web");
        let mut list = loaded(vec![view("R1", "r", 10), view_in(&web, "W1", "w", 5)]);
        list.select(Selection::NewSession);
        // ^ / #repo / r / #web / w / +
        assert_eq!(list.selected_row(), 5);
        assert!(list.move_selection(-1));
        assert_eq!(list.selected_id().unwrap().as_str(), "W1");
        assert!(list.move_selection(-1));
        assert_eq!(list.selection(), &Selection::FolderHeader(web.clone()));
        assert_eq!(list.selected_row(), 3);
        assert_eq!(list.selected_session(), None);
        assert!(list.move_selection(-2));
        assert_eq!(list.selection(), &Selection::FolderHeader(scope()));
        assert!(list.move_selection(-1));
        assert_eq!(list.selection(), &Selection::Folders);
        list.select_row(3);
        assert_eq!(list.selection(), &Selection::FolderHeader(web.clone()));
        // Updates keep it; a text filter that hides the header moves on to
        // the first match.
        list.upsert(view_in(&web, "W1", "w", 50));
        assert_eq!(list.selection(), &Selection::FolderHeader(web.clone()));
        list.filter.set("r");
        list.ensure_selection_visible();
        assert_eq!(list.selected_id().unwrap().as_str(), "R1");
        // Cycling order (visible) runs across groups.
        list.filter.set("");
        let ids: Vec<&str> = list.visible().iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, ["R1", "W1"]);
    }

    #[test]
    fn group_target_moves_between_folders() {
        let web = folder("/web");
        let mut list = loaded(vec![view("R1", "r", 10)]);
        assert_eq!(list.group_target(1), None, "one folder: nothing to move to");
        list.replace_all(
            vec![
                view("R1", "r", 5),
                view("R2", "r2", 10),
                view_in(&web, "W1", "w", 50),
            ],
            Listed::All,
        );
        let session = |id: &str| Some(Selection::Session(SessionId::from_string(id)));
        list.select(Selection::Session(SessionId::from_string("R2")));
        assert_eq!(list.group_target(1), session("W1"));
        assert_eq!(list.group_target(-1), None);
        list.select(Selection::Session(SessionId::from_string("W1")));
        assert_eq!(list.group_target(-1), session("R1"));
        assert_eq!(list.group_target(1), None);
        // From a header, and from the ends of the list.
        list.select(Selection::FolderHeader(web.clone()));
        assert_eq!(list.group_target(-1), session("R1"));
        list.select(Selection::Folders);
        assert_eq!(list.group_target(1), session("R1"));
        list.select(Selection::NewSession);
        assert_eq!(list.group_target(-1), session("W1"));
        // A folder without sessions: its header.
        let empty = folder("/empty");
        list.set_folder_filter(vec![scope(), empty.clone()]);
        list.select(Selection::Session(SessionId::from_string("R1")));
        assert_eq!(list.group_target(1), Some(Selection::FolderHeader(empty)));
    }

    #[test]
    fn an_empty_filter_folder_keeps_a_header_until_filtering_by_text() {
        let web = folder("/web");
        let mut list = loaded(vec![view("R1", "alpha", 10)]);
        list.set_folder_filter(vec![web.clone(), scope()]);
        assert_eq!(row_names(&list), ["^", "# /repo", "alpha", "# /web", "+"]);
        list.filter.set("alp");
        assert_eq!(row_names(&list), ["^", "# /repo", "alpha", "+"]);
        // Only the empty folder: no header, nothing to show.
        list.filter.set("");
        list.set_folder_filter(vec![web]);
        assert_eq!(row_names(&list), ["^", "+"]);
        assert_eq!(list.selection(), &Selection::NewSession);
    }

    #[test]
    fn the_folder_filter_narrows_without_a_snapshot() {
        let web = folder("/web");
        let mut list = loaded(vec![view("R1", "r", 10), view_in(&web, "W1", "w", 50)]);
        assert_eq!(list.selected_id().unwrap().as_str(), "W1");
        list.set_folder_filter(vec![scope()]);
        assert_eq!(list.folder_filter(), [scope()]);
        assert!(!list.is_grouped());
        assert_eq!(names(&list), ["r"]);
        assert_eq!(list.selected_id().unwrap().as_str(), "R1");
        // A session that stays shown stays selected.
        list.set_folder_filter(vec![scope(), web.clone()]);
        assert_eq!(list.selected_id().unwrap().as_str(), "R1");
        // A folder with nothing listed yet (an older daemon): the snapshot
        // that brings its sessions selects the most recent.
        let api = folder("/api");
        list.set_folder_filter(vec![api.clone()]);
        assert_eq!(list.selection(), &Selection::NewSession);
        list.replace_all(
            vec![view_in(&api, "A1", "a", 50), view_in(&api, "A3", "a3", 70)],
            Listed::Folders(vec![api]),
        );
        assert_eq!(list.selected_id().unwrap().as_str(), "A3");
        assert!(list.is_listed(&folder("/api")));
        assert!(!list.is_listed(&web));
    }
}
