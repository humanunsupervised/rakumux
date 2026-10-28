//! The left pane's state: every session the daemon listed, kept live from
//! its event stream, grouped and filtered by the active tab of the top row
//! (`grouping`: All folders, App or Status) and narrowed by the text
//! filter. Under All folders the folder filter applies (none: all
//! folders), and with more than one folder shown the sessions are grouped
//! under a header row per folder: the launch folder first, then by folder
//! name, then by path. Under App or Status the app or status filter applies
//! instead, with a header row per app or status when there are several.
//! Activity never moves a group.
//!
//! The order never changes by itself: activity doesn't move a session.
//! Inside a folder, pinned sessions come first. Both parts follow the
//! order the user gave them (a drag, or `J`/`K`; `sessions.session_order`),
//! and sessions without a place go after the others, oldest first, so a
//! new session appears at the bottom of its folder. Each app and status has
//! an order of its own (`sessions.section_order`), set the same ways under
//! the App and Status tabs: pinned sessions first, then that order, then
//! the rest folder by folder, so a drag there never changes the folders'
//! order. Whenever the list is grouped, pinned sessions leave their folder,
//! app or status for one section above every other, under its own header
//! (`ListRow::PinnedHeader`), in an order of their own (under
//! [`PINNED_ORDER`], set the same ways) that every tab shares; there the
//! active tab's filter still applies. The list and Grid use the active
//! tab's order; a row of tabs ([`ShownAs::Tabs`]) shows the same sessions
//! in the Folders tab's order whatever the active tab, so a tab keeps its
//! place (and number) as statuses change and sessions are read. Selection is tracked by session id (or header) so it
//! survives moves and updates made by other clients; under App or Status
//! the selected session stays shown when it stops matching the filter
//! (its status changed), until something else is selected.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use session_core::api::{EventEnvelope, SessionEvent};
use session_core::ids::SessionId;
use session_core::model::{ScopeRoot, SessionStatus, SessionView};

use super::config::{SectionOrder, SessionOrder};
use super::folders::compare_folders;
use super::grouping::{
    linked_worktree, sort_sections, worktree_label, AppKey, Grouping, ReadPeriod, Section,
    StatusGroup,
};
use super::text_input::TextInput;

/// The key `sessions.section_order` keeps the pinned section's order under,
/// next to the apps' and statuses' (which are `app:…` and `status:…`).
pub const PINNED_ORDER: &str = "pinned";

/// What is selected in the list: the top row (the tabs), a folder's or a
/// section's header, a session, or "+ New session" (the last row).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Selection {
    /// The top row: the tabs (Folders, App, Status). ←/→ switch tabs,
    /// Enter opens the active tab's modal (the Folders modal for All
    /// folders).
    Folders,
    /// A folder's header row: new sessions go in that folder.
    FolderHeader(ScopeRoot),
    /// The header of a linked worktree inside a folder (its root): new
    /// sessions go in that folder, starting in the worktree when it's
    /// inside it.
    WorktreeHeader(ScopeRoot, PathBuf),
    /// An app's or a status's header row (App or Status tab, several
    /// shown): Enter opens that tab's modal there.
    SectionHeader(Section),
    /// The header of one of ready's sub-sections under Status (when its
    /// sessions were last read): Enter opens the Status modal on ready.
    ReadPeriodHeader(ReadPeriod),
    /// The pinned section's header, above every group: Enter does nothing.
    PinnedHeader,
    NewSession,
    Session(SessionId),
}

/// How the sessions are laid out on screen, which decides their order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShownAs {
    /// The list (and Grid's cards): the active tab's order, under its
    /// headers ([`SessionList::visible`]).
    List,
    /// A row of tabs (the Tabs layout, a session under tabs on a phone or
    /// from Grid): the same sessions in the Folders tab's order
    /// ([`SessionList::tab_order`]).
    Tabs,
}

/// One row of the list, top to bottom.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ListRow<'a> {
    /// The tabs at the top: what the list is grouped and filtered by.
    Folders,
    /// A folder's header row.
    FolderHeader(&'a ScopeRoot),
    /// A linked worktree's header inside a folder (its root), above the
    /// folder's sessions working in it.
    WorktreeHeader(&'a ScopeRoot, &'a Path),
    /// An app's or a status's header row (several shown).
    SectionHeader(&'a Section),
    /// A sub-section of ready under Status: sessions last read in that
    /// period.
    ReadPeriodHeader(ReadPeriod),
    /// The pinned sessions' header, first under the tabs whenever the list
    /// is grouped and one is shown.
    PinnedHeader,
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
    /// The active tab: the sections, and the one filter that applies.
    grouping: Grouping,
    /// The folder filter: the folders shown, or every folder when empty.
    folders: Vec<ScopeRoot>,
    /// The App tab's filter: the apps shown, or every app when empty.
    apps: Vec<AppKey>,
    /// The Status tab's filter: the statuses shown, or every one when empty.
    statuses: Vec<StatusGroup>,
    /// The folders shown under All folders, in display order: the filter's,
    /// or with no filter every folder that has sessions.
    groups: Vec<ScopeRoot>,
    /// Every folder that has sessions, in display order: inside an app or a
    /// status, sessions go folder by folder in this order.
    all_folders: Vec<ScopeRoot>,
    /// The apps or statuses shown under the App or Status tab, in display
    /// order; none under All folders.
    sections: Vec<Section>,
    /// Every listed session, of every folder; inside a folder in list
    /// order (pinned first).
    sessions: Vec<SessionView>,
    /// The order the user gave each folder's sessions, first to last.
    order: HashMap<ScopeRoot, Vec<SessionId>>,
    /// The order the user gave each app's or status's sessions (by
    /// [`Section::key`]), first to last.
    section_order: HashMap<String, Vec<SessionId>>,
    /// What the last snapshot brought in full.
    listed: Listed,
    selection: Selection,
    /// The open place of the selected session the list keys moved onto
    /// (`app::places`); selecting anything else drops it.
    place: Option<(SessionId, PathBuf)>,
    /// Last selected section or single-folder filter. Neutral rows keep
    /// this context so "+ New session" targets the folder the user visited.
    folder_context: Option<ScopeRoot>,
    pub filter: TextInput,
    /// `/` was pressed and keys go to the filter.
    pub filter_editing: bool,
    loaded: bool,
    /// The folder filter left nothing to select: the next snapshot selects
    /// the most recent session, like the first one.
    reselect: bool,
    /// The wall clock (seconds) the list is drawn at: its ages and ready's
    /// sub-sections ([`Self::set_clock`]).
    now: i64,
}

impl SessionList {
    /// An empty list for a TUI started in `launch`, showing every folder's
    /// sessions under the Status tab.
    pub fn new(launch: ScopeRoot) -> Self {
        Self {
            launch,
            grouping: Grouping::default(),
            folders: Vec::new(),
            apps: Vec::new(),
            statuses: Vec::new(),
            groups: Vec::new(),
            all_folders: Vec::new(),
            sections: Vec::new(),
            sessions: Vec::new(),
            order: HashMap::new(),
            section_order: HashMap::new(),
            listed: Listed::Folders(Vec::new()),
            selection: Selection::NewSession,
            place: None,
            folder_context: None,
            filter: TextInput::default(),
            filter_editing: false,
            loaded: false,
            reselect: false,
            now: 0,
        }
    }

    /// Set the wall clock (seconds) the list is drawn at: its ages and
    /// ready's sub-sections. Returns whether a session went into another
    /// sub-section, so the rows changed.
    pub fn set_clock(&mut self, now: i64) -> bool {
        let before = std::mem::replace(&mut self.now, now);
        let moved = self.grouping == Grouping::Status
            && self
                .sessions
                .iter()
                .any(|s| ReadPeriod::of_done(before, s) != ReadPeriod::of_done(now, s));
        if moved {
            self.ensure_selection_visible();
        }
        moved
    }

    /// The wall clock (seconds) the list is drawn at ([`Self::set_clock`]).
    pub fn now(&self) -> i64 {
        self.now
    }

    /// The ready sub-section `session` is listed under: under Status, when
    /// it's ready; `None` otherwise.
    fn read_period(&self, session: &SessionView) -> Option<ReadPeriod> {
        if self.grouping == Grouping::Status {
            ReadPeriod::of_done(self.now, session)
        } else {
            None
        }
    }

    /// How many of the sessions shown are under ready's `period`.
    /// How many sessions the pinned section shows.
    pub fn pinned_count(&self) -> usize {
        self.rows()
            .iter()
            .skip_while(|row| !matches!(row, ListRow::PinnedHeader))
            .skip(1)
            .take_while(|row| matches!(row, ListRow::Session(_)))
            .count()
    }

    pub fn read_period_count(&self, period: ReadPeriod) -> usize {
        self.visible()
            .into_iter()
            .filter(|s| self.read_period(s) == Some(period))
            .count()
    }

    /// Show these folders (none: all folders). Nothing is fetched: the
    /// list keeps every folder's sessions. Under All folders, a selection
    /// the filter hides goes to the most recent session shown; so does one
    /// on the tabs or "+ New session". Under another tab the filter is
    /// kept for when All folders is back.
    pub fn set_folder_filter(&mut self, folders: Vec<ScopeRoot>) {
        self.folders = unique(folders);
        self.refilter(self.grouping == Grouping::Folder);
    }

    /// The folder filter: the folders shown, or none for all folders.
    pub fn folder_filter(&self) -> &[ScopeRoot] {
        &self.folders
    }

    /// The folder filter while it applies (All folders is the active tab).
    fn active_folder_filter(&self) -> &[ScopeRoot] {
        match self.grouping {
            Grouping::Folder => &self.folders,
            Grouping::App | Grouping::Status => &[],
        }
    }

    /// The active tab.
    pub fn grouping(&self) -> Grouping {
        self.grouping
    }

    /// Make `grouping` the active tab: its sections and filter apply, and
    /// the other tabs' filters wait. The tabs and "+ New session" stay
    /// selected; a session or header the new tab hides goes to the most
    /// recent session shown.
    pub fn set_grouping(&mut self, grouping: Grouping) {
        if grouping != self.grouping {
            self.grouping = grouping;
            self.refilter(false);
        }
    }

    /// The App or Status tab's filter, as sections (none: all of them; and
    /// always none for All folders, whose filter is `folder_filter`).
    pub fn section_filter(&self, grouping: Grouping) -> Vec<Section> {
        match grouping {
            Grouping::Folder => Vec::new(),
            Grouping::App => self.apps.iter().cloned().map(Section::App).collect(),
            Grouping::Status => self.statuses.iter().copied().map(Section::Status).collect(),
        }
    }

    /// Show these apps or statuses (none: all) when `grouping`'s tab is
    /// active; a selection the filter hides goes to the most recent session
    /// shown, and so does one on the tabs or "+ New session".
    pub fn set_section_filter(&mut self, grouping: Grouping, sections: Vec<Section>) {
        let sections = unique(sections);
        match grouping {
            Grouping::Folder => return,
            Grouping::App => {
                self.apps = sections
                    .into_iter()
                    .filter_map(|section| match section {
                        Section::App(app) => Some(app),
                        Section::Status(_) => None,
                    })
                    .collect();
            }
            Grouping::Status => {
                self.statuses = sections
                    .into_iter()
                    .filter_map(|section| match section {
                        Section::Status(status) => Some(status),
                        Section::App(_) => None,
                    })
                    .collect();
            }
        }
        self.refilter(grouping == self.grouping);
    }

    /// Folder for new sessions: the filter's only folder (under All
    /// folders), otherwise the last selected section when the folder filter
    /// still includes it.
    pub fn current_folder(&self) -> Option<&ScopeRoot> {
        let filter = self.active_folder_filter();
        match filter {
            [only] => Some(only),
            _ => self
                .folder_context
                .as_ref()
                .filter(|folder| filter.is_empty() || filter.contains(folder)),
        }
    }

    /// Where a new session starts when it's made from the selection: the
    /// worktree a selected worktree header names, when it's inside its
    /// folder (a session's working directory must be); else the folder.
    pub fn new_session_cwd(&self) -> Option<&Path> {
        match &self.selection {
            Selection::WorktreeHeader(folder, root) if root.starts_with(folder.path()) => {
                Some(root.as_path())
            }
            _ => None,
        }
    }

    /// The sessions are shown under header rows: under Folders always (a
    /// lone folder's header names it), under App or Status when several
    /// apps or statuses are shown.
    pub fn is_grouped(&self) -> bool {
        match self.grouping {
            // A lone folder still gets its header: its name and branch.
            Grouping::Folder => !self.groups.is_empty(),
            // A lone app or status too, below the pinned section.
            Grouping::App | Grouping::Status => {
                self.sections.len() > 1 || (self.sections.len() == 1 && self.pins_shown())
            }
        }
    }

    /// Whether the active tab's filter shows a pinned session.
    fn pins_shown(&self) -> bool {
        self.sessions
            .iter()
            .any(|s| s.pinned && self.tab_shows(s, None))
    }

    /// The folders shown under All folders, in display order.
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

    /// Use the order the user gave the apps' and statuses' sessions (from
    /// the config).
    pub fn set_section_order(&mut self, order: &SectionOrder) {
        self.section_order = order
            .iter()
            .map(|(key, ids)| (key.clone(), ids.clone()))
            .collect();
    }

    /// The apps' and statuses' order to save: with every folder listed,
    /// ids of sessions that are gone are dropped, and sections left without
    /// any are too.
    pub fn section_order(&self) -> SectionOrder {
        let every_folder = matches!(self.listed, Listed::All);
        self.section_order
            .iter()
            .filter_map(|(key, ids)| {
                let ids: Vec<SessionId> = ids
                    .iter()
                    .filter(|id| !every_folder || self.get(id).is_some())
                    .cloned()
                    .collect();
                (!ids.is_empty()).then(|| (key.clone(), ids))
            })
            .collect()
    }

    /// The sessions shown, in the order `shown` lays them out.
    pub fn shown(&self, shown: ShownAs) -> Vec<&SessionView> {
        match shown {
            ShownAs::List => self.visible(),
            ShownAs::Tabs => self.tab_order(),
        }
    }

    /// Where `id` is among the sessions shown as `shown`.
    pub fn position(&self, shown: ShownAs, id: &SessionId) -> Option<usize> {
        self.shown(shown).iter().position(|s| &s.id == id)
    }

    /// The sessions [`SessionList::visible`] shows (the active tab's filter,
    /// the one kept shown, the text filter), as a row of tabs orders them:
    /// the pinned section's order first, then folder by folder (the launch
    /// folder first, then by name, then path), each in its folder's order.
    /// That's the Folders tab's order under every tab: what a session is
    /// doing never moves its tab, nor does reading it, so stepping through
    /// the tabs reaches every one.
    pub fn tab_order(&self) -> Vec<&SessionView> {
        let shown: HashSet<&SessionId> = self.visible().into_iter().map(|s| &s.id).collect();
        let pinned = self.in_pinned_order();
        let others = self
            .all_folders
            .iter()
            .flat_map(|folder| self.in_folder_order(folder))
            .filter(|s| !s.pinned);
        pinned
            .into_iter()
            .chain(others)
            .filter(|s| shown.contains(&s.id))
            .collect()
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
    /// `J`/`K` take it. It stays in its part of the list: its folder's
    /// pinned sessions, or the others; under App or Status, its app's or
    /// status's (whatever their folders), which keep an order of their own,
    /// except ready's, listed by when each was last read. A `to` outside
    /// that part means its first or last place. Returns whether it moved.
    pub fn move_session(&mut self, id: &SessionId, to: usize) -> bool {
        self.move_shown(ShownAs::List, id, to)
    }

    /// Move session `id` to place `to` among the tabs ([`Self::tab_order`]),
    /// where a dragged tab drops it or `J`/`K` take it in a row of tabs: as
    /// under the Folders tab, it stays among the pinned sessions or in its
    /// folder (and worktree). Returns whether it moved.
    pub fn move_tab(&mut self, id: &SessionId, to: usize) -> bool {
        self.move_shown(ShownAs::Tabs, id, to)
    }

    /// [`Self::move_session`] among the sessions in `shown`'s order.
    fn move_shown(&mut self, shown: ShownAs, id: &SessionId, to: usize) -> bool {
        let visible = self.shown(shown);
        let Some(from) = visible.iter().position(|s| &s.id == id) else {
            return false;
        };
        let moving = visible[from];
        // Tabs are in the Folders tab's order, with its parts.
        let (grouping, pins_first) = match shown {
            ShownAs::List => (self.grouping, self.pins_first()),
            ShownAs::Tabs => (Grouping::Folder, true),
        };
        if pins_first && moving.pinned {
            let shown: Vec<(SessionId, bool)> =
                visible.iter().map(|s| (s.id.clone(), s.pinned)).collect();
            return self.move_pinned(&shown, from, to);
        }
        let section = Section::of(grouping, moving);
        if section.as_ref().is_some_and(Section::by_last_read) {
            return false;
        }
        let in_part = |s: &&SessionView| {
            (section.is_some()
                || (s.scope == moving.scope && linked_worktree(s) == linked_worktree(moving)))
                && s.pinned == moving.pinned
                && Section::of(grouping, s) == section
        };
        // A part is contiguous in the list.
        let first = visible.iter().position(in_part).unwrap_or(from);
        let last = visible.iter().rposition(in_part).unwrap_or(from);
        let to = to.clamp(first, last);
        if to == from {
            return false;
        }
        let target = visible[to].id.clone();
        if let Some(section) = section {
            // Every session of the app or status, in its order now.
            let mut order: Vec<SessionId> = self
                .in_section_order(&section)
                .iter()
                .map(|s| s.id.clone())
                .collect();
            let (Some(from), Some(to)) = (
                order.iter().position(|s| s == id),
                order.iter().position(|s| s == &target),
            ) else {
                return false;
            };
            let moved = order.remove(from);
            order.insert(to, moved);
            self.section_order.insert(section.key(), order);
            return true;
        }
        let scope = moving.scope.clone();
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

    /// Move the pinned session at visible position `from` to `to`, kept
    /// within the pinned section; saved as that section's order, every
    /// pinned session's place, shown or not.
    fn move_pinned(&mut self, shown: &[(SessionId, bool)], from: usize, to: usize) -> bool {
        let last = shown
            .iter()
            .rposition(|(_, pinned)| *pinned)
            .unwrap_or(from);
        let to = to.min(last);
        if to == from {
            return false;
        }
        let (id, target) = (shown[from].0.clone(), shown[to].0.clone());
        let mut order: Vec<SessionId> = self
            .in_pinned_order()
            .iter()
            .map(|s| s.id.clone())
            .collect();
        let (Some(from), Some(to)) = (
            order.iter().position(|s| s == &id),
            order.iter().position(|s| s == &target),
        ) else {
            return false;
        };
        let moved = order.remove(from);
        order.insert(to, moved);
        self.section_order.insert(PINNED_ORDER.to_owned(), order);
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
            Selection::Folders
            | Selection::FolderHeader(_)
            | Selection::WorktreeHeader(..)
            | Selection::SectionHeader(_)
            | Selection::ReadPeriodHeader(_)
            | Selection::PinnedHeader
            | Selection::NewSession => {}
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
            SessionEvent::Activity | SessionEvent::Reply { .. } | SessionEvent::Unknown => {
                ListChange::Ignored
            }
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
            let selection = visible
                .get(row)
                .or_else(|| row.checked_sub(1).and_then(|r| visible.get(r)))
                .map(|s| Selection::Session(s.id.clone()))
                .unwrap_or(Selection::NewSession);
            self.select(selection);
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

    /// `(live, total)` sessions of `folder` working in its linked worktree
    /// `root`, whatever the filters.
    pub fn worktree_counts(&self, folder: &ScopeRoot, root: &Path) -> (usize, usize) {
        let mut counts = (0, 0);
        let mine = self
            .sessions
            .iter()
            .filter(|s| &s.scope == folder && linked_worktree(s) == Some(root));
        for session in mine {
            counts.1 += 1;
            if session.status == SessionStatus::Live {
                counts.0 += 1;
            }
        }
        counts
    }

    /// The branch checked out in `folder`'s linked worktree `root`, as its
    /// sessions see it.
    pub fn worktree_branch(&self, folder: &ScopeRoot, root: &Path) -> Option<&str> {
        self.sessions
            .iter()
            .filter(|s| &s.scope == folder && linked_worktree(s) == Some(root))
            .find_map(|s| s.git.as_ref()?.branch.as_deref())
    }

    /// `(live, total)` sessions of an app or a status, whatever the
    /// filters.
    pub fn section_counts(&self, section: &Section) -> (usize, usize) {
        let mut counts = (0, 0);
        let grouping = section.grouping();
        for session in self
            .sessions
            .iter()
            .filter(|s| Section::of(grouping, s).as_ref() == Some(section))
        {
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

    /// Whether the text filter shows session `id` (it shows everything
    /// while it's empty).
    pub fn matches_text(&self, id: &SessionId) -> bool {
        let needle = self.filter.value().trim().to_lowercase();
        needle.is_empty() || self.get(id).is_some_and(|s| self.matches(s, &needle))
    }

    /// Sessions of the folders (apps, statuses) shown that match the text
    /// filter, in display order: folder by folder, or app by app (status by
    /// status) and inside each folder by folder. Under App or Status the
    /// selected session is shown even when the filter no longer matches it.
    pub fn visible(&self) -> Vec<&SessionView> {
        self.visible_keeping(self.kept())
    }

    /// The session shown whatever the App or Status filter says: the
    /// selected one. All folders keeps none, since a session never changes
    /// folder.
    fn kept(&self) -> Option<&SessionId> {
        match (self.grouping, &self.selection) {
            (Grouping::Folder, _) => None,
            (_, Selection::Session(id)) => Some(id),
            _ => None,
        }
    }

    /// [`SessionList::visible`], showing `kept` whatever the App or Status
    /// filter says.
    fn visible_keeping(&self, kept: Option<&SessionId>) -> Vec<&SessionView> {
        let needle = self.filter.value().trim().to_lowercase();
        let matching = |s: &&SessionView| needle.is_empty() || self.matches(s, &needle);
        let mut visible = Vec::with_capacity(self.sessions.len());
        let pins_first = self.pins_first();
        // The pinned section first, then each group without its pinned ones.
        if pins_first {
            visible.extend(
                self.in_pinned_order()
                    .into_iter()
                    .filter(|s| self.tab_shows(s, kept))
                    .filter(matching),
            );
        }
        let unpinned = |s: &&SessionView| !(pins_first && s.pinned);
        let grouping = self.grouping;
        if grouping == Grouping::Folder {
            for group in &self.groups {
                visible.extend(
                    self.in_folder_order(group)
                        .into_iter()
                        .filter(matching)
                        .filter(unpinned),
                );
            }
            return visible;
        }
        for section in &self.sections {
            let shown = self.section_shown(section);
            visible.extend(
                self.in_section_order(section)
                    .into_iter()
                    .filter(|s| shown || kept == Some(&s.id))
                    .filter(matching)
                    .filter(unpinned),
            );
        }
        visible
    }

    /// Whether pinned sessions have their own section above the groups:
    /// whenever the list is grouped, which it is whenever one is shown.
    fn pins_first(&self) -> bool {
        self.is_grouped()
    }

    /// Whether the active tab's filter shows `session` among its groups:
    /// its folder under Folders, its app or status (or `kept`) under App or
    /// Status.
    fn tab_shows(&self, session: &SessionView, kept: Option<&SessionId>) -> bool {
        match self.grouping {
            Grouping::Folder => self.groups.contains(&session.scope),
            Grouping::App | Grouping::Status => {
                kept == Some(&session.id)
                    || Section::of(self.grouping, session).is_some_and(|section| {
                        self.sections.contains(&section) && self.section_shown(&section)
                    })
            }
        }
    }

    /// Every pinned session in the pinned section's order: the order the
    /// user gave them there, then the rest folder by folder (launch folder
    /// first), each in its folder's order. The same under every tab.
    fn in_pinned_order(&self) -> Vec<&SessionView> {
        let order = self.section_order.get(PINNED_ORDER);
        let place = |s: &SessionView| {
            order
                .and_then(|ids| ids.iter().position(|id| id == &s.id))
                .unwrap_or(usize::MAX)
        };
        let mut pinned: Vec<&SessionView> = self
            .all_folders
            .iter()
            .flat_map(|folder| self.in_folder_order(folder))
            .filter(|s| s.pinned)
            .collect();
        // Stable: those without a place keep their folder by folder order.
        pinned.sort_by_key(|s| place(s));
        pinned
    }

    /// A folder's sessions in display order: those in its own working tree,
    /// then those in each linked worktree (by label), each part in the
    /// folder's order (pinned first).
    fn in_folder_order(&self, folder: &ScopeRoot) -> Vec<&SessionView> {
        let mine: Vec<&SessionView> = self
            .sessions
            .iter()
            .filter(|s| &s.scope == folder)
            .collect();
        let mut roots: Vec<(String, &Path)> = mine
            .iter()
            .filter_map(|s| linked_worktree(s))
            .map(|root| (worktree_label(folder, root), root))
            .collect();
        roots.sort();
        roots.dedup();
        let mut ordered: Vec<&SessionView> = mine
            .iter()
            .copied()
            .filter(|s| linked_worktree(s).is_none())
            .collect();
        for (_, root) in roots {
            ordered.extend(
                mine.iter()
                    .copied()
                    .filter(|s| linked_worktree(s) == Some(root)),
            );
        }
        ordered
    }

    /// Every session of an app or a status, in its order: pinned ones
    /// first, then the order the user gave them there, then the rest folder
    /// by folder (each in its folder's order).
    fn in_section_order(&self, section: &Section) -> Vec<&SessionView> {
        let order = self.section_order.get(&section.key());
        let place = |s: &SessionView| {
            order
                .and_then(|ids| ids.iter().position(|id| id == &s.id))
                .unwrap_or(usize::MAX)
        };
        let mut sessions: Vec<&SessionView> = self
            .all_folders
            .iter()
            .flat_map(|folder| self.sessions.iter().filter(move |s| &s.scope == folder))
            .filter(|s| Section::of(self.grouping, s).as_ref() == Some(section))
            .collect();
        // Stable: unordered sessions keep their folder by folder order.
        if section.by_last_read() {
            // Its sub-section first (pinned, then by when each was last
            // read), then the most recently read.
            let period = |s: &SessionView| ReadPeriod::of_done(self.now, s);
            sessions.sort_by(|a, b| {
                period(a)
                    .cmp(&period(b))
                    .then(b.last_active_at.cmp(&a.last_active_at))
            });
        } else {
            sessions.sort_by(|a, b| b.pinned.cmp(&a.pinned).then(place(a).cmp(&place(b))));
        }
        sessions
    }

    /// Whether `id` is in a section listed by when each session was last
    /// read (ready, under Status), where sessions can't be moved.
    pub fn listed_by_last_read(&self, id: &SessionId) -> bool {
        self.get(id)
            .filter(|session| !(self.pins_first() && session.pinned))
            .and_then(|session| Section::of(self.grouping, session))
            .is_some_and(|section| section.by_last_read())
    }

    /// Whether the App or Status filter shows `section` (an empty one
    /// shows every app or status).
    fn section_shown(&self, section: &Section) -> bool {
        match section {
            Section::App(app) => self.apps.is_empty() || self.apps.contains(app),
            Section::Status(status) => self.statuses.is_empty() || self.statuses.contains(status),
        }
    }

    /// Every row: the tabs, the sessions, under a header per folder (app,
    /// status) when several are shown, then "+ New session" last. While
    /// filtering by text, headers without a match are left out.
    pub fn rows(&self) -> Vec<ListRow<'_>> {
        let visible = self.visible();
        let mut rows =
            Vec::with_capacity(visible.len() + self.groups.len().max(self.sections.len()) + 2);
        rows.push(ListRow::Folders);
        if !self.is_grouped() {
            self.push_sessions(&mut rows, visible);
            rows.push(ListRow::NewSession);
            return rows;
        }
        let grouping = self.grouping;
        let filtering = self.is_filtering();
        // The pinned section above every group; the groups leave them out.
        let pinned: Vec<&SessionView> = visible.iter().copied().filter(|s| s.pinned).collect();
        if !pinned.is_empty() {
            rows.push(ListRow::PinnedHeader);
            rows.extend(pinned.into_iter().map(ListRow::Session));
        }
        let visible: Vec<&SessionView> = visible.into_iter().filter(|s| !s.pinned).collect();
        if grouping == Grouping::Folder {
            // Each folder's header, its own sessions, then a header above
            // each linked worktree's.
            for folder in &self.groups {
                let start = rows.len();
                rows.push(ListRow::FolderHeader(folder));
                let mut worktree: Option<&Path> = None;
                for session in visible.iter().copied().filter(|s| &s.scope == folder) {
                    let root = linked_worktree(session);
                    if let Some(root) = root.filter(|root| Some(*root) != worktree) {
                        rows.push(ListRow::WorktreeHeader(folder, root));
                        worktree = Some(root);
                    }
                    rows.push(ListRow::Session(session));
                }
                if filtering && rows.len() == start + 1 {
                    rows.pop();
                }
            }
            rows.push(ListRow::NewSession);
            return rows;
        }
        let headers: Vec<ListRow<'_>> = match grouping {
            Grouping::Folder => self.groups.iter().map(ListRow::FolderHeader).collect(),
            Grouping::App | Grouping::Status => {
                self.sections.iter().map(ListRow::SectionHeader).collect()
            }
        };
        for header in headers {
            let start = rows.len();
            let had_pinned = matches!(header, ListRow::SectionHeader(section)
                if self.sessions.iter().any(|s| s.pinned && Section::of(grouping, s).as_ref() == Some(section)));
            rows.push(header);
            self.push_sessions(
                &mut rows,
                visible.iter().copied().filter(|s| match header {
                    ListRow::FolderHeader(folder) => &s.scope == folder,
                    ListRow::SectionHeader(section) => {
                        Section::of(grouping, s).as_ref() == Some(section)
                    }
                    ListRow::Folders
                    | ListRow::WorktreeHeader(..)
                    | ListRow::ReadPeriodHeader(_)
                    | ListRow::PinnedHeader
                    | ListRow::NewSession
                    | ListRow::Session(_) => false,
                }),
            );
            // A header whose sessions are all in the pinned section goes too.
            if (filtering || had_pinned) && rows.len() == start + 1 {
                rows.pop();
            }
        }
        rows.push(ListRow::NewSession);
        rows
    }

    /// `sessions` as rows, ready ones under Status after a header for when
    /// they were last read (they come sorted by it).
    fn push_sessions<'a>(
        &'a self,
        rows: &mut Vec<ListRow<'a>>,
        sessions: impl IntoIterator<Item = &'a SessionView>,
    ) {
        let mut period = None;
        for session in sessions {
            if let Some(read) = self.read_period(session) {
                if period != Some(read) {
                    rows.push(ListRow::ReadPeriodHeader(read));
                    period = Some(read);
                }
            }
            rows.push(ListRow::Session(session));
        }
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
            Selection::Folders
            | Selection::FolderHeader(_)
            | Selection::WorktreeHeader(..)
            | Selection::SectionHeader(_)
            | Selection::ReadPeriodHeader(_)
            | Selection::PinnedHeader
            | Selection::NewSession => None,
        }
    }

    pub fn selected_session(&self) -> Option<&SessionView> {
        self.selected_id().and_then(|id| self.get(id))
    }

    /// The open place the list keys moved onto in the selected session
    /// (`app::places`), if they did.
    pub fn selected_place(&self) -> Option<&Path> {
        let (id, path) = self.place.as_ref()?;
        (self.selected_id() == Some(id)).then_some(path.as_path())
    }

    /// Move onto `place`, one of the selected session's places, or with
    /// `None` back onto the session itself.
    pub fn select_place(&mut self, place: Option<PathBuf>) {
        self.place = place.and_then(|path| Some((self.selected_id()?.clone(), path)));
    }

    /// Row of the selection; the tabs are the first row, "+ New session"
    /// the last, which also stands in for a selection that isn't shown.
    pub fn selected_row(&self) -> usize {
        let rows = self.rows();
        let new_session = rows.len() - 1;
        let found = match &self.selection {
            Selection::Folders => Some(0),
            Selection::NewSession => None,
            Selection::FolderHeader(folder) => rows
                .iter()
                .position(|row| matches!(row, ListRow::FolderHeader(f) if *f == folder)),
            Selection::WorktreeHeader(folder, root) => rows.iter().position(
                |row| matches!(row, ListRow::WorktreeHeader(f, r) if *f == folder && *r == root),
            ),
            Selection::SectionHeader(section) => rows
                .iter()
                .position(|row| matches!(row, ListRow::SectionHeader(s) if *s == section)),
            Selection::ReadPeriodHeader(period) => rows
                .iter()
                .position(|row| matches!(row, ListRow::ReadPeriodHeader(p) if p == period)),
            Selection::PinnedHeader => rows
                .iter()
                .position(|row| matches!(row, ListRow::PinnedHeader)),
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
            Some(ListRow::WorktreeHeader(folder, root)) => {
                Selection::WorktreeHeader((*folder).clone(), root.to_path_buf())
            }
            Some(ListRow::SectionHeader(section)) => Selection::SectionHeader((*section).clone()),
            Some(ListRow::ReadPeriodHeader(period)) => Selection::ReadPeriodHeader(*period),
            Some(ListRow::PinnedHeader) => Selection::PinnedHeader,
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

    /// `{`/`}`: what to select `delta` groups (folders, apps, statuses)
    /// from the selected session's or header's (from the tabs, `}` reaches
    /// the first group; from "+ New session", `{` the last): the group's
    /// first session, or its header when it shows none. `None` past either
    /// end, or without headers.
    pub fn group_target(&self, delta: isize) -> Option<Selection> {
        let rows = self.rows();
        let headers: Vec<usize> = rows
            .iter()
            .enumerate()
            .filter(|(_, row)| {
                matches!(
                    row,
                    ListRow::FolderHeader(_)
                        | ListRow::WorktreeHeader(..)
                        | ListRow::SectionHeader(_)
                        | ListRow::ReadPeriodHeader(_)
                        | ListRow::PinnedHeader
                )
            })
            .map(|(index, _)| index)
            .collect();
        if headers.is_empty() {
            return None;
        }
        let selected = self.selected_row();
        let current = match &self.selection {
            Selection::Folders => -1,
            Selection::NewSession => headers.len() as isize,
            // The header at or above the selection.
            _ => headers
                .iter()
                .rposition(|header| *header <= selected)
                .map_or(0, |i| i as isize),
        };
        let header = *headers.get(usize::try_from(current + delta).ok()?)?;
        Some(match (rows.get(header + 1), rows[header]) {
            (Some(ListRow::Session(s)), _) => Selection::Session(s.id.clone()),
            (_, ListRow::SectionHeader(section)) => Selection::SectionHeader(section.clone()),
            (_, ListRow::FolderHeader(folder)) => Selection::FolderHeader(folder.clone()),
            (_, ListRow::WorktreeHeader(folder, root)) => {
                Selection::WorktreeHeader(folder.clone(), root.to_path_buf())
            }
            (_, ListRow::ReadPeriodHeader(period)) => Selection::ReadPeriodHeader(period),
            (_, ListRow::PinnedHeader) => Selection::PinnedHeader,
            (_, ListRow::Folders | ListRow::NewSession | ListRow::Session(_)) => return None,
        })
    }

    pub fn select(&mut self, selection: Selection) {
        if self.selection != selection {
            self.place = None;
        }
        self.selection = selection;
        self.reselect = false;
        // The selected session is shown under App or Status, whatever the
        // filter: its section may have to be.
        if self.grouping != Grouping::Folder {
            self.sort_groups();
        }
        self.remember_selected_folder();
    }

    fn remember_selected_folder(&mut self) {
        let folder = match &self.selection {
            Selection::Session(_) => self.selected_session().map(|session| session.scope.clone()),
            Selection::FolderHeader(folder) | Selection::WorktreeHeader(folder, _) => {
                Some(folder.clone())
            }
            Selection::Folders
            | Selection::SectionHeader(_)
            | Selection::ReadPeriodHeader(_)
            | Selection::PinnedHeader
            | Selection::NewSession => match self.active_folder_filter() {
                [only] => Some(only.clone()),
                _ => None,
            },
        };
        if let Some(folder) = folder {
            self.folder_context = Some(folder);
        }
    }

    /// Re-apply the filters: keep the selection when it's still shown,
    /// otherwise select the first session shown (or "New session").
    pub fn ensure_selection_visible(&mut self) {
        let shown = match &self.selection {
            Selection::Session(id) => self.visible().iter().any(|s| &s.id == id),
            Selection::FolderHeader(_)
            | Selection::WorktreeHeader(..)
            | Selection::SectionHeader(_)
            | Selection::ReadPeriodHeader(_)
            | Selection::PinnedHeader => self.header_shown(),
            Selection::Folders | Selection::NewSession => true,
        };
        if !shown {
            self.selection = self
                .visible_keeping(None)
                .first()
                .map(|s| Selection::Session(s.id.clone()))
                .unwrap_or(Selection::NewSession);
            self.sort_groups();
        }
        self.remember_selected_folder();
    }

    /// Whether the selected header has its row.
    fn header_shown(&self) -> bool {
        let rows = self.rows();
        match &self.selection {
            Selection::FolderHeader(folder) => rows.contains(&ListRow::FolderHeader(folder)),
            Selection::WorktreeHeader(folder, root) => {
                rows.contains(&ListRow::WorktreeHeader(folder, root))
            }
            Selection::SectionHeader(section) => rows.contains(&ListRow::SectionHeader(section)),
            Selection::ReadPeriodHeader(period) => {
                rows.contains(&ListRow::ReadPeriodHeader(*period))
            }
            Selection::PinnedHeader => rows.contains(&ListRow::PinnedHeader),
            Selection::Folders | Selection::NewSession | Selection::Session(_) => true,
        }
    }

    /// A filter or the active tab changed: a selection it hides goes to the
    /// most recent session shown; with `leave_top_row`, so does one on the
    /// tabs or "+ New session".
    fn refilter(&mut self, leave_top_row: bool) {
        self.sort_groups();
        let kept = match &self.selection {
            Selection::Session(id) => self.visible_keeping(None).iter().any(|s| &s.id == id),
            Selection::FolderHeader(_)
            | Selection::WorktreeHeader(..)
            | Selection::SectionHeader(_)
            | Selection::ReadPeriodHeader(_)
            | Selection::PinnedHeader => self.header_shown(),
            Selection::Folders | Selection::NewSession => !leave_top_row,
        };
        if !kept {
            self.selection = self.default_selection();
            self.reselect = self.selection == Selection::NewSession;
            self.sort_groups();
        }
        self.remember_selected_folder();
    }

    /// The most recently active session the filters show, whatever the
    /// order.
    fn default_selection(&self) -> Selection {
        self.visible_keeping(None)
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

    /// Change a session in place; nothing these events change moves it
    /// within its folder. Under App or Status it may change sections (a new
    /// lifecycle is a new status), which can leave a header without
    /// sessions.
    fn modify(&mut self, id: &SessionId, f: impl FnOnce(&mut SessionView)) -> ListChange {
        match self.index_of(id) {
            Some(i) => {
                f(&mut self.sessions[i]);
                if self.grouping != Grouping::Folder {
                    self.sort_groups();
                    self.ensure_selection_visible();
                }
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

    /// The folders shown under All folders: the filter's, or every folder
    /// with sessions; the launch folder first, then by name, then by path.
    /// And the apps or statuses shown under App or Status.
    fn sort_groups(&mut self) {
        let mut seen = HashSet::new();
        let mut with_sessions: Vec<ScopeRoot> = self
            .sessions
            .iter()
            .filter(|s| seen.insert(&s.scope))
            .map(|s| s.scope.clone())
            .collect();
        with_sessions.sort_by(|a, b| compare_folders(&self.launch, a, b));
        self.groups = if self.folders.is_empty() {
            with_sessions.clone()
        } else {
            let mut groups = self.folders.clone();
            groups.sort_by(|a, b| compare_folders(&self.launch, a, b));
            groups
        };
        self.all_folders = with_sessions;
        self.sections = self.shown_sections();
    }

    /// The apps or statuses the active tab shows, in display order: the
    /// filter's (empty ones included) and the selected session's, or with
    /// no filter every one that has sessions. None under All folders.
    fn shown_sections(&self) -> Vec<Section> {
        let grouping = self.grouping;
        if grouping == Grouping::Folder {
            return Vec::new();
        }
        let filter = self.section_filter(grouping);
        let candidates: Vec<Section> = if filter.is_empty() {
            self.sessions
                .iter()
                .filter_map(|s| Section::of(grouping, s))
                .collect()
        } else {
            let kept = self
                .kept()
                .and_then(|id| self.get(id))
                .and_then(|s| Section::of(grouping, s));
            filter.into_iter().chain(kept).collect()
        };
        let mut sections = unique(candidates);
        sort_sections(&mut sections, &self.sessions);
        sections
    }
}

/// `items` without repeats, in their first order.
fn unique<T: PartialEq>(items: Vec<T>) -> Vec<T> {
    let mut unique: Vec<T> = Vec::with_capacity(items.len());
    for item in items {
        if !unique.contains(&item) {
            unique.push(item);
        }
    }
    unique
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

    /// An empty list under the Folders tab, which most of these tests are
    /// about (the list starts under Status).
    fn on_folders() -> SessionList {
        let mut list = SessionList::new(scope());
        list.set_grouping(Grouping::Folder);
        list
    }

    /// A first snapshot of every folder, under the Folders tab.
    fn loaded(sessions: Vec<SessionView>) -> SessionList {
        let mut list = on_folders();
        list.replace_all(sessions, Listed::All);
        list
    }

    fn names(list: &SessionList) -> Vec<String> {
        list.visible().iter().map(|s| s.name.clone()).collect()
    }

    /// Rows as text: `^` for the tabs, `+`, `# /path` for folder headers,
    /// `## name` for app and status headers, `★` for the pinned section's,
    /// session names.
    fn row_names(list: &SessionList) -> Vec<String> {
        list.rows()
            .iter()
            .map(|row| match row {
                ListRow::Folders => "^".to_string(),
                ListRow::NewSession => "+".to_string(),
                ListRow::FolderHeader(scope) => format!("# {}", scope.path().display()),
                ListRow::WorktreeHeader(folder, root) => {
                    format!("  ⎇ {}", worktree_label(folder, root))
                }
                ListRow::SectionHeader(section) => format!("## {}", section.label()),
                ListRow::ReadPeriodHeader(period) => format!("  · {}", period.label()),
                ListRow::PinnedHeader => "★".to_string(),
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
    fn a_new_list_starts_under_the_status_tab_on_the_most_recent_session() {
        let mut list = SessionList::new(scope());
        assert_eq!(list.grouping(), Grouping::Status);
        let mut done = aged("B", "second", 2, 30);
        done.lifecycle = Lifecycle::Done;
        list.replace_all(
            vec![aged("A", "first", 1, 10), done, aged("C", "third", 3, 20)],
            Listed::All,
        );
        assert_eq!(list.selected_id().unwrap().as_str(), "B");
        // Unread comes before running.
        assert_eq!(names(&list), ["second", "first", "third"]);
    }

    #[test]
    fn done_lists_sessions_by_when_they_were_last_read_and_they_cannot_be_moved() {
        let read = |id: &str, name: &str, created: i64, read_at: i64| {
            let mut session = aged(id, name, created, read_at);
            session.lifecycle = Lifecycle::Idle;
            session
        };
        let mut working = aged("W", "working", 4, 5);
        working.lifecycle = Lifecycle::Working;
        let mut list = SessionList::new(scope());
        list.replace_all(
            vec![
                read("A", "a", 1, 30),
                read("B", "b", 2, 10),
                read("C", "c", 3, 20),
                working,
            ],
            Listed::All,
        );
        // Most recently read first, whatever the creation order.
        assert_eq!(names(&list), ["working", "a", "c", "b"]);
        // Going into B (the daemon's touch) puts it on top.
        list.apply(&envelope(
            "B",
            SessionEvent::Updated {
                session: read("B", "b", 2, 40),
            },
        ));
        assert_eq!(names(&list), ["working", "b", "a", "c"]);
        // No order of the user's applies, and none can be made there.
        let id = SessionId::from_string;
        list.set_section_order(&SectionOrder::from([(
            "status:idle".into(),
            vec![id("C"), id("A"), id("B")],
        )]));
        assert_eq!(names(&list), ["working", "b", "a", "c"]);
        assert!(list.listed_by_last_read(&id("A")));
        assert!(!list.listed_by_last_read(&id("W")));
        assert!(!list.move_session(&id("A"), 1));
        assert!(!list.move_session(&id("C"), 0));
        // A pinned session leads every status, in the pinned section, and
        // moves there.
        let mut pinned = read("C", "c", 3, 20);
        pinned.pinned = true;
        list.apply(&envelope("C", SessionEvent::Updated { session: pinned }));
        assert_eq!(names(&list), ["c", "working", "b", "a"]);
        assert!(!list.listed_by_last_read(&id("C")));
        // Under Folders the same sessions move as usual.
        list.set_grouping(Grouping::Folder);
        assert!(!list.listed_by_last_read(&id("A")));
        assert!(list.move_session(&id("A"), 2));
    }

    #[test]
    fn pinned_ready_sessions_go_to_the_pinned_section_above_ready() {
        let now = 1_000_000;
        let read = |id: &str, ago: i64, pinned: bool| {
            let mut session = aged(id, &id.to_lowercase(), 1, now - ago);
            session.lifecycle = Lifecycle::Idle;
            session.pinned = pinned;
            session
        };
        let mut list = SessionList::new(scope());
        list.set_clock(now);
        list.replace_all(
            vec![
                read("A", 10 * 86_400, true),
                read("B", 60, false),
                read("C", 7_200, true),
            ],
            Listed::All,
        );
        // Pinned above ready however long ago they were read, which then
        // shows its header.
        assert_eq!(
            row_names(&list),
            ["^", "★", "a", "c", "## ready", "  · last hour", "b", "+"]
        );
        assert_eq!(list.pinned_count(), 2);
        // Unpinned, a session goes back to when it was read.
        list.apply(&envelope(
            "A",
            SessionEvent::Updated {
                session: read("A", 10 * 86_400, false),
            },
        ));
        assert_eq!(
            row_names(&list),
            [
                "^",
                "★",
                "c",
                "## ready",
                "  · last hour",
                "b",
                "  · older",
                "a",
                "+"
            ]
        );
    }

    #[test]
    fn done_has_a_sub_section_for_the_last_hour_the_last_day_and_older() {
        let now = 1_000_000;
        let read = |id: &str, ago: i64| {
            let mut session = aged(id, &id.to_lowercase(), 1, now - ago);
            session.lifecycle = Lifecycle::Idle;
            session
        };
        let mut list = SessionList::new(scope());
        list.set_clock(now);
        list.replace_all(
            vec![
                read("A", 10 * 86_400),
                read("B", 60),
                read("C", 7_200),
                read("D", 600),
            ],
            Listed::All,
        );
        // Every session is ready, so there's no status header; the periods
        // still have theirs.
        assert_eq!(
            row_names(&list),
            [
                "^",
                "  · last hour",
                "b",
                "d",
                "  · last day",
                "c",
                "  · older",
                "a",
                "+"
            ]
        );
        assert_eq!(list.read_period_count(ReadPeriod::LastHour), 2);
        assert_eq!(list.read_period_count(ReadPeriod::Older), 1);
        // The headers are rows like any other.
        list.select_row(1);
        assert_eq!(
            list.selection(),
            &Selection::ReadPeriodHeader(ReadPeriod::LastHour)
        );
        assert_eq!(list.selected_row(), 1);
        assert_eq!(
            list.group_target(1),
            Some(Selection::Session(SessionId::from_string("C")))
        );
        // Time passing moves D into the last day; nothing else changes.
        assert!(list.set_clock(now + 3_100));
        assert_eq!(
            row_names(&list),
            [
                "^",
                "  · last hour",
                "b",
                "  · last day",
                "d",
                "c",
                "  · older",
                "a",
                "+"
            ]
        );
        assert!(!list.set_clock(now + 3_200));
        // A day on, the last hour is empty: its header goes, and the
        // selection with it.
        assert!(list.set_clock(now + 100_000));
        assert_eq!(
            row_names(&list),
            ["^", "  · older", "b", "d", "c", "a", "+"]
        );
        assert!(matches!(list.selection(), Selection::Session(_)));
        // Only under Status.
        list.set_grouping(Grouping::Folder);
        assert!(!row_names(&list).iter().any(|row| row.starts_with("  ·")));
        assert!(!list.set_clock(now + 200_000));
    }

    #[test]
    fn orders_by_creation_and_selects_the_most_recent_on_first_load() {
        let mut list = on_folders();
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
        // Under the tabs and the folder's header.
        assert_eq!(list.selected_row(), 3);
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
        assert_eq!(list.selected_row(), 3);
    }

    #[test]
    fn new_sessions_go_at_the_bottom_of_their_folder() {
        let mut list = on_folders();
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
        list.select_row(2);
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
        // ^ / # / a / b / +
        assert!(list.move_selection(5));
        assert_eq!(list.selected_row(), 4);
        assert_eq!(list.selection(), &Selection::NewSession);
        assert!(!list.move_selection(1));
        assert!(list.move_selection(-2));
        assert_eq!(list.selected_id().unwrap().as_str(), "A");
        // Up from the first session: the folder's header, then the tabs.
        assert!(list.move_selection(-1));
        assert_eq!(list.selection(), &Selection::FolderHeader(scope()));
        assert!(list.move_selection(-1));
        assert_eq!(list.selection(), &Selection::Folders);
        assert_eq!(list.selected_row(), 0);
        assert_eq!(list.selected_id(), None);
        assert!(!list.move_selection(-1));
        assert!(list.move_selection(2));
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
        let mut list = on_folders();
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
        let mut list = on_folders();
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

    /// `session` working in the linked worktree at `root`, on `branch`.
    fn in_worktree(mut session: SessionView, root: &str, branch: &str) -> SessionView {
        let name = Path::new(root)
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        session.git = Some(session_core::model::GitInfo {
            repo_root: PathBuf::from(root),
            branch: Some(branch.into()),
            detached_at: None,
            worktree: Some(name),
            repo: None,
            origin: None,
        });
        session
    }

    #[test]
    fn worktrees_get_a_section_inside_their_folder() {
        let mut list = loaded(vec![
            view("A", "main-work", 1),
            in_worktree(
                view("B", "feature", 2),
                "/repo/worktrees/feature",
                "feature",
            ),
            in_worktree(
                view("C", "agent", 3),
                "/repo/.claude/worktrees/agent-x",
                "a",
            ),
            in_worktree(view("D", "elsewhere", 4), "/tmp/wt-other", "other"),
            view("E", "more-main", 5),
        ]);
        // The folder's own sessions, then each worktree's under its header,
        // by path (one elsewhere by its own name).
        assert_eq!(
            row_names(&list),
            [
                "^",
                "# /repo",
                "main-work",
                "more-main",
                "  ⎇ .claude/worktrees/agent-x",
                "agent",
                "  ⎇ worktrees/feature",
                "feature",
                "  ⎇ wt-other",
                "elsewhere",
                "+"
            ]
        );
        assert_eq!(
            names(&list),
            ["main-work", "more-main", "agent", "feature", "elsewhere"]
        );
        let feature = Path::new("/repo/worktrees/feature");
        assert_eq!(list.worktree_counts(&scope(), feature), (1, 1));
        assert_eq!(list.worktree_branch(&scope(), feature), Some("feature"));
        // A worktree's header: new sessions go in the folder, starting in
        // the worktree when it's inside it.
        list.select_row(6);
        assert_eq!(
            list.selection(),
            &Selection::WorktreeHeader(scope(), feature.to_path_buf())
        );
        assert_eq!(list.current_folder(), Some(&scope()));
        assert_eq!(list.new_session_cwd(), Some(feature));
        list.select_row(8);
        assert_eq!(list.new_session_cwd(), None);
        assert_eq!(list.current_folder(), Some(&scope()));
        // Moves stay in a worktree's part.
        let b = SessionId::from_string("B");
        assert!(!list.move_session(&b, 0));
        assert!(list.move_session(&SessionId::from_string("E"), 0));
        assert_eq!(names(&list)[..2], ["more-main", "main-work"]);
        // The section follows the agent: back in the main checkout, B
        // joins the folder's own sessions.
        let mut back = list.get(&b).unwrap().clone();
        back.git = None;
        list.upsert(back);
        assert!(!row_names(&list).contains(&"  ⎇ worktrees/feature".to_string()));
        assert_eq!(names(&list)[..3], ["more-main", "main-work", "feature"]);
        // A session started in a worktree has it as its folder: no
        // worktree header there.
        let own = folder("/repo/worktrees/feature");
        list.upsert(in_worktree(
            view_in(&own, "F", "own", 6),
            "/repo/worktrees/feature",
            "feature",
        ));
        let rows = row_names(&list);
        let at = rows
            .iter()
            .position(|r| r == "# /repo/worktrees/feature")
            .unwrap();
        assert_eq!(rows[at + 1], "own");
    }

    #[test]
    fn one_folder_still_has_its_header() {
        let mut list = loaded(vec![view("A", "a", 1)]);
        assert!(list.is_grouped());
        assert_eq!(row_names(&list), ["^", "# /repo", "a", "+"]);
        // Filtered to one folder of several: that one's header.
        list.upsert(view_in(&folder("/web"), "W1", "w", 2));
        assert!(list.is_grouped());
        list.set_folder_filter(vec![folder("/web")]);
        assert!(list.is_grouped());
        assert_eq!(row_names(&list), ["^", "# /web", "w", "+"]);
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
                "★",
                "web-pinned",
                "# /repo",
                "repo-old",
                "repo-new",
                "# /api",
                "api",
                "# /x/api",
                "x-api",
                "# /web",
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
        // The pinned one first, then each folder's.
        assert_eq!(ids(&list), ["W2", "A1", "W1", "W3"]);
        // A folder whose last session goes loses its group.
        list.set_folder_filter(vec![]);
        list.remove(&SessionId::from_string("X1"));
        assert_eq!(list.groups(), [scope(), api, web]);
    }

    #[test]
    fn pinned_sessions_have_a_section_above_every_group_in_every_tab() {
        let web = folder("/web");
        let mut pinned_web = view_in(&web, "W1", "web-pinned", 20);
        pinned_web.pinned = true;
        let mut pinned_repo = view_in(&scope(), "R1", "repo-pinned", 10);
        pinned_repo.pinned = true;
        let mut list = SessionList::new(scope());
        list.replace_all(
            vec![
                pinned_web,
                pinned_repo,
                view_in(&scope(), "R2", "repo", 30),
                view_in(&web, "W2", "web", 40),
            ],
            Listed::All,
        );
        list.set_grouping(Grouping::Folder);
        // Folder by folder, the launch folder's first, until the user moves one.
        assert_eq!(
            row_names(&list),
            [
                "^",
                "★",
                "repo-pinned",
                "web-pinned",
                "# /repo",
                "repo",
                "# /web",
                "web",
                "+"
            ]
        );
        assert_eq!(list.pinned_count(), 2);
        // The header is a row like the others.
        list.select_row(1);
        assert_eq!(list.selection(), &Selection::PinnedHeader);
        assert_eq!(list.selected_id(), None);
        // `J`/`K` and drags move one within the section, whatever its folder,
        // and the order is the same under every tab.
        let id = SessionId::from_string;
        assert!(list.move_session(&id("W1"), 0));
        assert_eq!(ids(&list)[..2], ["W1", "R1"]);
        // Past the section's end is its last place, which R1 has already.
        assert!(!list.move_session(&id("R1"), 3), "it stays in the section");
        assert_eq!(
            list.section_order().get(PINNED_ORDER),
            Some(&vec![id("W1"), id("R1")])
        );
        list.set_grouping(Grouping::App);
        assert_eq!(
            row_names(&list)[..4],
            ["^", "★", "web-pinned", "repo-pinned"]
        );
        assert!(!row_names(&list).contains(&"# /web".to_string()));
        // A filter still applies to it: one folder's pinned sessions only.
        list.set_grouping(Grouping::Folder);
        list.set_folder_filter(vec![web.clone()]);
        assert_eq!(
            row_names(&list),
            ["^", "★", "web-pinned", "# /web", "web", "+"]
        );
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
    fn new_sessions_keep_the_last_selected_section_on_neutral_rows() {
        let web = folder("/web");
        let mut list = loaded(vec![view("R1", "r", 10), view_in(&web, "W1", "w", 50)]);
        assert_eq!(list.current_folder(), Some(&web));
        for selection in [Selection::NewSession, Selection::Folders] {
            list.select(selection);
            assert_eq!(list.current_folder(), Some(&web));
        }
        list.select(Selection::FolderHeader(scope()));
        assert_eq!(list.current_folder(), Some(&scope()));
        list.select(Selection::NewSession);
        assert_eq!(list.current_folder(), Some(&scope()));
        // Row clicks and keyboard navigation update the same folder context.
        list.select_row(3);
        assert_eq!(list.selection(), &Selection::FolderHeader(web.clone()));
        list.move_selection(2);
        assert_eq!(list.selection(), &Selection::NewSession);
        assert_eq!(list.current_folder(), Some(&web));
    }

    #[test]
    fn folder_context_follows_filters_and_empty_folder_headers() {
        let (web, empty) = (folder("/web"), folder("/empty"));
        let mut list = loaded(vec![view("R1", "r", 10), view_in(&web, "W1", "w", 50)]);
        list.set_folder_filter(vec![scope(), empty.clone()]);
        assert_eq!(list.current_folder(), Some(&scope()));
        list.select(Selection::FolderHeader(empty.clone()));
        list.select(Selection::NewSession);
        assert_eq!(list.current_folder(), Some(&empty));
        // A new filter takes precedence over the old section, even empty.
        list.set_folder_filter(vec![web.clone()]);
        list.select(Selection::Folders);
        assert_eq!(list.current_folder(), Some(&web));
        list.set_folder_filter(vec![empty.clone()]);
        assert_eq!(list.current_folder(), Some(&empty));
        // Expanding the filter keeps its current folder when still included.
        list.set_folder_filter(vec![empty.clone(), folder("/other-empty")]);
        assert_eq!(list.current_folder(), Some(&empty));
        // Don't reuse a remembered section excluded by a multi-folder filter.
        list.set_folder_filter(vec![folder("/other-empty"), folder("/third-empty")]);
        assert_eq!(list.current_folder(), None);
    }

    #[test]
    fn refreshes_and_removing_the_last_session_preserve_folder_context() {
        let web = folder("/web");
        let sessions = vec![view_in(&web, "W1", "w", 50)];
        let mut list = loaded(sessions.clone());
        list.select(Selection::NewSession);
        list.replace_all(sessions, Listed::All);
        assert_eq!(list.current_folder(), Some(&web));
        list.select(Selection::Session(SessionId::from_string("W1")));
        list.remove(&SessionId::from_string("W1"));
        assert_eq!(list.selection(), &Selection::NewSession);
        assert_eq!(list.current_folder(), Some(&web));
        list.replace_all(vec![], Listed::All);
        assert_eq!(list.current_folder(), Some(&web));
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
        // Only the empty folder: its header, nothing under it.
        list.filter.set("");
        list.set_folder_filter(vec![web]);
        assert_eq!(row_names(&list), ["^", "# /web", "+"]);
        assert_eq!(list.selection(), &Selection::NewSession);
    }

    /// A session in `scope` running agent `kind` (none: a shell) in
    /// `lifecycle`.
    fn running(
        scope: &ScopeRoot,
        id: &str,
        kind: Option<&str>,
        lifecycle: Lifecycle,
        at: i64,
    ) -> SessionView {
        let mut session = view_in(scope, id, &id.to_lowercase(), at);
        session.agent = kind.map(|kind| session_core::model::AgentInfo {
            kind: kind.into(),
            ..Default::default()
        });
        session.lifecycle = lifecycle;
        session
    }

    /// `/repo` (launch) and `/web`: Claude Code working and done, Codex
    /// waiting for you, a shell, and in `/web` Claude Code idle and a dead
    /// Codex session.
    fn agents() -> SessionList {
        let web = folder("/web");
        let mut dead = running(&web, "W2", Some("codex"), Lifecycle::None, 6);
        dead.status = SessionStatus::Dead;
        dead.terminal = None;
        loaded(vec![
            running(&scope(), "R1", Some("claude"), Lifecycle::Working, 1),
            running(&scope(), "R2", None, Lifecycle::None, 2),
            running(&scope(), "R3", Some("codex"), Lifecycle::Blocked, 3),
            running(&scope(), "R4", Some("claude"), Lifecycle::Done, 4),
            running(&web, "W1", Some("claude"), Lifecycle::Idle, 5),
            dead,
        ])
    }

    fn app(name: &str) -> Section {
        Section::App(AppKey::new(name))
    }

    #[test]
    fn the_app_tab_groups_by_app_folder_by_folder_and_keeps_the_folder_filter_aside() {
        let mut list = agents();
        list.set_folder_filter(vec![folder("/web")]);
        assert_eq!(names(&list), ["w1", "w2"]);
        list.set_grouping(Grouping::App);
        assert!(list.is_grouped());
        // Agents first, then the shell; inside an app the launch folder's
        // sessions first, each folder in its own order. The folder filter
        // doesn't apply here.
        assert_eq!(
            row_names(&list),
            [
                "^",
                "## claude",
                "r1",
                "r4",
                "w1",
                "## codex",
                "r3",
                "w2",
                "## shell",
                "r2",
                "+"
            ]
        );
        assert_eq!(list.section_counts(&app("claude")), (3, 3));
        assert_eq!(list.section_counts(&app("codex")), (1, 2));
        // The app filter: one app, no headers.
        list.set_section_filter(Grouping::App, vec![app("codex")]);
        assert_eq!(row_names(&list), ["^", "r3", "w2", "+"]);
        assert_eq!(list.section_filter(Grouping::App), [app("codex")]);
        // Two apps: their headers, in display order whatever the ticks'.
        list.set_section_filter(Grouping::App, vec![app("shell"), app("claude")]);
        assert_eq!(
            row_names(&list),
            ["^", "## claude", "r1", "r4", "w1", "## shell", "r2", "+"]
        );
        // Back on the Folders tab, its filter applies again; the app filter
        // waits for the App tab.
        list.set_grouping(Grouping::Folder);
        assert_eq!(names(&list), ["w1", "w2"]);
        assert_eq!(list.folder_filter(), [folder("/web")]);
        list.set_grouping(Grouping::App);
        assert_eq!(names(&list), ["r1", "r4", "w1", "r2"]);
        // The text filter still narrows, dropping headers without a match.
        list.filter.set("w");
        assert_eq!(row_names(&list), ["^", "## claude", "w1", "+"]);
    }

    #[test]
    fn the_status_tab_groups_by_status_and_events_move_sessions_between_them() {
        let mut list = agents();
        list.set_grouping(Grouping::Status);
        assert_eq!(
            row_names(&list),
            [
                "^",
                "## waiting for you",
                "r3",
                "## unread",
                "r4",
                "## working",
                "r1",
                "## running",
                "r2",
                "## dead",
                "w2",
                "## ready",
                "  · last hour",
                "w1",
                "+"
            ]
        );
        // A session whose lifecycle changes changes status.
        list.apply(&envelope(
            "R1",
            SessionEvent::Lifecycle {
                lifecycle: Lifecycle::Blocked,
            },
        ));
        assert_eq!(&row_names(&list)[1..4], ["## waiting for you", "r1", "r3"]);
        assert!(!row_names(&list).contains(&"## working".to_string()));
        // Filtered to what wants the user: the selected session stays shown
        // when it stops matching, until something else is selected.
        list.set_section_filter(
            Grouping::Status,
            vec![Section::Status(StatusGroup::WaitingForYou)],
        );
        assert_eq!(names(&list), ["r1", "r3"]);
        list.select(Selection::Session(SessionId::from_string("R3")));
        list.apply(&envelope(
            "R3",
            SessionEvent::Lifecycle {
                lifecycle: Lifecycle::Working,
            },
        ));
        assert_eq!(list.selected_id().unwrap().as_str(), "R3");
        assert_eq!(
            row_names(&list),
            ["^", "## waiting for you", "r1", "## working", "r3", "+"]
        );
        list.select(Selection::Session(SessionId::from_string("R1")));
        assert_eq!(row_names(&list), ["^", "r1", "+"]);
        // An exited session is under exited.
        list.apply(&envelope("R1", SessionEvent::Exited { code: Some(0) }));
        assert_eq!(list.selected_id().unwrap().as_str(), "R1");
        assert_eq!(
            row_names(&list),
            ["^", "## waiting for you", "## exited", "r1", "+"]
        );
    }

    #[test]
    fn tabs_show_what_the_active_tab_shows_in_the_folders_order() {
        let tabs = |list: &SessionList| -> Vec<String> {
            list.tab_order().iter().map(|s| s.name.clone()).collect()
        };
        let mut list = agents();
        // Under Folders the tabs are the list.
        assert_eq!(tabs(&list), names(&list));
        assert_eq!(tabs(&list), ["r1", "r2", "r3", "r4", "w1", "w2"]);
        // Under Status and App the list goes by status or app; the tabs
        // keep the folders' order, so what a session does never moves one.
        for grouping in [Grouping::Status, Grouping::App] {
            list.set_grouping(grouping);
            assert_ne!(names(&list), tabs(&list), "{grouping:?}");
            assert_eq!(tabs(&list), ["r1", "r2", "r3", "r4", "w1", "w2"]);
        }
        list.set_grouping(Grouping::Status);
        list.apply(&envelope(
            "R4",
            SessionEvent::Lifecycle {
                lifecycle: Lifecycle::Idle,
            },
        ));
        assert_eq!(tabs(&list), ["r1", "r2", "r3", "r4", "w1", "w2"]);
        // A pinned session leads, as the pinned section does in the list.
        let mut pinned = list.get(&SessionId::from_string("W1")).unwrap().clone();
        pinned.pinned = true;
        list.upsert(pinned);
        assert_eq!(tabs(&list), ["w1", "r1", "r2", "r3", "r4", "w2"]);
        // The active tab's filter decides which: what wants you, and the
        // selected session kept shown after it was answered, until another
        // is selected (as in the list).
        list.set_section_filter(
            Grouping::Status,
            vec![Section::Status(StatusGroup::WaitingForYou)],
        );
        assert_eq!(tabs(&list), ["r3"]);
        list.select(Selection::Session(SessionId::from_string("R3")));
        list.apply(&envelope(
            "R3",
            SessionEvent::Lifecycle {
                lifecycle: Lifecycle::Idle,
            },
        ));
        assert_eq!(tabs(&list), ["r3"]);
        list.apply(&envelope(
            "R1",
            SessionEvent::Lifecycle {
                lifecycle: Lifecycle::Blocked,
            },
        ));
        assert_eq!(tabs(&list), ["r1", "r3"]);
        list.select(Selection::Session(SessionId::from_string("R1")));
        assert_eq!(tabs(&list), ["r1"]);
    }

    #[test]
    fn a_tab_moves_within_its_folder_or_the_pinned_ones_whatever_the_tab() {
        let mut list = agents();
        list.set_grouping(Grouping::Status);
        let tabs = |list: &SessionList| -> Vec<String> {
            list.tab_order().iter().map(|s| s.id.to_string()).collect()
        };
        let id = SessionId::from_string;
        // Ready can't be reordered in the list, but its tab moves.
        assert!(list.listed_by_last_read(&id("W1")));
        assert!(list.move_tab(&id("W1"), 5));
        assert_eq!(tabs(&list), ["R1", "R2", "R3", "R4", "W2", "W1"]);
        // Not into another folder: past its first place it stays.
        assert!(!list.move_tab(&id("W2"), 0));
        assert!(list.move_tab(&id("W2"), 5));
        assert!(list.move_tab(&id("R4"), 0));
        assert_eq!(tabs(&list), ["R4", "R1", "R2", "R3", "W1", "W2"]);
        // It's the folder's order, the list's under Folders too.
        list.set_grouping(Grouping::Folder);
        assert_eq!(ids(&list), ["R4", "R1", "R2", "R3", "W1", "W2"]);
        assert_eq!(
            list.session_order()[std::path::Path::new("/repo")],
            ["R4", "R1", "R2", "R3"].map(SessionId::from_string)
        );
    }

    #[test]
    fn switching_tabs_keeps_the_top_row_and_a_session_still_shown() {
        let mut list = agents();
        list.select(Selection::Folders);
        list.set_grouping(Grouping::App);
        assert_eq!(list.selection(), &Selection::Folders);
        list.set_grouping(Grouping::Status);
        assert_eq!(list.selection(), &Selection::Folders);
        // A filter picked in a modal goes to the most recent session shown.
        list.set_section_filter(
            Grouping::Status,
            vec![Section::Status(StatusGroup::Working)],
        );
        assert_eq!(list.selected_id().unwrap().as_str(), "R1");
        // A tab that still shows the selected session keeps it; one that
        // hides it (the app filter) picks the most recent session it shows.
        list.set_grouping(Grouping::Folder);
        assert_eq!(list.selected_id().unwrap().as_str(), "R1");
        list.set_section_filter(Grouping::App, vec![app("codex")]);
        assert_eq!(list.selected_id().unwrap().as_str(), "R1");
        list.set_grouping(Grouping::App);
        assert_eq!(list.selected_id().unwrap().as_str(), "W2");
        // A header of a tab that isn't active anymore isn't kept.
        list.set_section_filter(Grouping::App, vec![]);
        list.select(Selection::SectionHeader(app("shell")));
        assert_eq!(list.selected_row(), 8);
        list.set_grouping(Grouping::Folder);
        assert!(matches!(list.selection(), Selection::Session(_)));
    }

    #[test]
    fn moves_and_group_jumps_stay_within_an_app() {
        let mut list = agents();
        let by_folder = names(&list);
        list.set_grouping(Grouping::App);
        let (r1, r4) = (SessionId::from_string("R1"), SessionId::from_string("R4"));
        // Within claude whatever the folder: R4 up past R1, then R1 down
        // past W1 (another folder); out of claude (into codex) it can't go.
        assert!(list.move_session(&r4, 0));
        assert_eq!(names(&list), ["r4", "r1", "w1", "r3", "w2", "r2"]);
        assert!(list.move_session(&r1, 2));
        assert_eq!(names(&list), ["r4", "w1", "r1", "r3", "w2", "r2"]);
        assert!(!list.move_session(&r1, 3));
        // The app keeps an order of its own; the folders' is untouched.
        assert!(list.session_order().is_empty());
        let ids = |ids: &[&str]| ids.iter().map(|id| SessionId::from_string(*id)).collect();
        assert_eq!(
            list.section_order(),
            SectionOrder::from([("app:claude".to_string(), ids(&["R4", "W1", "R1"]))])
        );
        list.set_grouping(Grouping::Folder);
        assert_eq!(names(&list), by_folder);
        list.set_grouping(Grouping::App);
        assert_eq!(names(&list), ["r4", "w1", "r1", "r3", "w2", "r2"]);
        // `{`/`}` go from app to app.
        list.select(Selection::Session(r1));
        let target = |id: &str| Some(Selection::Session(SessionId::from_string(id)));
        assert_eq!(list.group_target(1), target("R3"));
        assert_eq!(list.group_target(-1), None);
        list.select(Selection::NewSession);
        assert_eq!(list.group_target(-1), target("R2"));
        list.select(Selection::Folders);
        assert_eq!(list.group_target(1), target("R4"));
    }

    #[test]
    fn each_status_keeps_its_own_order_saved_and_restored() {
        let web = folder("/web");
        let sessions = || {
            vec![
                running(&scope(), "R1", Some("claude"), Lifecycle::Working, 1),
                running(&scope(), "R2", Some("codex"), Lifecycle::Idle, 2),
                running(&web, "W1", Some("claude"), Lifecycle::Working, 3),
            ]
        };
        let mut list = loaded(sessions());
        list.set_grouping(Grouping::Status);
        assert_eq!(names(&list), ["r1", "w1", "r2"]);
        // W1 (another folder) up past R1 within working; not into idle.
        let w1 = SessionId::from_string("W1");
        assert!(list.move_session(&w1, 0));
        assert_eq!(names(&list), ["w1", "r1", "r2"]);
        // Past the group's end it stops at its last place, never into idle.
        assert!(list.move_session(&w1, 2));
        assert_eq!(names(&list), ["r1", "w1", "r2"]);
        assert!(list.move_session(&w1, 0));
        let saved = list.section_order();
        assert_eq!(saved.keys().collect::<Vec<_>>(), ["status:working"]);
        // The App tab's order is its own: there claude goes folder by
        // folder still.
        list.set_grouping(Grouping::App);
        assert_eq!(names(&list), ["r1", "w1", "r2"]);
        // Restored from the config, the working order comes back.
        let mut again = loaded(sessions());
        again.set_section_order(&saved);
        again.set_grouping(Grouping::Status);
        list.set_grouping(Grouping::Status);
        assert_eq!(names(&again), names(&list));
        // A session that's gone is dropped from the order to save.
        let mut gone = loaded(vec![running(
            &scope(),
            "R1",
            Some("claude"),
            Lifecycle::Working,
            1,
        )]);
        gone.set_section_order(&saved);
        assert_eq!(
            gone.section_order()["status:working"],
            [SessionId::from_string("R1")]
        );
    }

    #[test]
    fn new_sessions_ignore_a_waiting_folder_filter() {
        let mut list = agents();
        list.set_folder_filter(vec![folder("/web")]);
        assert_eq!(list.current_folder(), Some(&folder("/web")));
        list.set_grouping(Grouping::App);
        list.select(Selection::Session(SessionId::from_string("R1")));
        list.select(Selection::Folders);
        // Not /web: that filter isn't applied under App.
        assert_eq!(list.current_folder(), Some(&scope()));
        list.select(Selection::SectionHeader(app("claude")));
        assert_eq!(list.current_folder(), Some(&scope()));
    }

    #[test]
    fn the_folder_filter_narrows_without_a_snapshot() {
        let web = folder("/web");
        let mut list = loaded(vec![view("R1", "r", 10), view_in(&web, "W1", "w", 50)]);
        assert_eq!(list.selected_id().unwrap().as_str(), "W1");
        list.set_folder_filter(vec![scope()]);
        assert_eq!(list.folder_filter(), [scope()]);
        assert!(list.is_grouped());
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
