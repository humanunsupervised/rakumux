//! Folders: the directories sessions belong to (the
//! wire calls them scopes, and its requests `project_*`). The TUI lists and
//! watches the sessions of every folder (`sessions.all`), so sessions in a
//! new worktree or started by an agent appear live, and the folder filter
//! narrows the list locally: no filter shows All folders, and the filter
//! isn't saved. The folder Rakumux starts in is added at startup; new
//! sessions go in [`App::current_folder`].
//!
//! The folder filter is the All folders tab's (`grouping`): it applies
//! while that tab is active. The Folders modal (Enter on the tabs with All
//! folders active, a click on that tab, or `P` from any tab) lists All
//! folders and the folders that have sessions, filters them as you type,
//! shows one or the ticked ones (Tab, Space) under All folders, which
//! becomes the active tab, and adds a typed folder
//! (creating it when it doesn't exist, offering the folders that complete
//! it as you type; `path_complete`).
//!
//! An older daemon without `sessions.all` gets its known folders listed and
//! watched by name (new folders show after a refresh); one from before
//! folders shows the launch folder alone and says so when the user asks for
//! folders.

use std::cmp::Ordering;
use std::path::Path;

use session_core::api::{ApiError, ErrorCode, SubscribeRequest, CAPABILITY_SESSIONS_ALL};
use session_core::client::{EventStream, SessionApi};
use session_core::model::{ProjectView, ScopeRoot, SessionView};

use super::grouping::Grouping;
use super::path_complete::{self, expand, home_dir, MAX_SUGGESTIONS};
use super::runtime::AppEvent;
use super::session_list::Listed;
use super::types::{AppMode, FolderPicker, NewFolder, PickerRow, PickerStage};
use super::App;

/// Status shown when the user asks for folders from a daemon without them.
pub const FOLDERS_UNSUPPORTED: &str =
    "This Rakumux server doesn't support folders yet; showing this folder only.";

/// What the daemon can list and watch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FolderSupport {
    /// Not synced yet.
    #[default]
    Unknown,
    /// Every folder, new ones included (capability `sessions.all`).
    All,
    /// An older daemon: the folders it knew at the last sync, by name.
    Known,
    /// A daemon from before folders: the launch folder only.
    LaunchOnly,
}

/// What the TUI knows about folders. The folder filter itself is the
/// session list's ([`super::SessionList::folder_filter`]).
#[derive(Debug, Clone)]
pub struct Folders {
    /// The folder Rakumux started in.
    pub launch: ScopeRoot,
    /// Every folder the daemon knows, with counts and git context.
    pub known: Vec<ProjectView>,
    pub support: FolderSupport,
}

impl Folders {
    /// Before the first sync: only the launch folder is known by path.
    pub fn new(launch: ScopeRoot) -> Self {
        Self {
            launch,
            known: Vec::new(),
            support: FolderSupport::Unknown,
        }
    }

    pub fn get(&self, path: &ScopeRoot) -> Option<&ProjectView> {
        self.known.iter().find(|p| &p.path == path)
    }

    /// Whether the daemon listed this folder.
    pub fn is_known(&self, path: &ScopeRoot) -> bool {
        self.get(path).is_some()
    }

    /// The folder's display name: the daemon's, else its own name.
    pub fn name_of(&self, path: &ScopeRoot) -> String {
        self.get(path)
            .map(|p| p.name.clone())
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| folder_name(path.path()))
    }

    /// Remember a folder the daemon returned.
    pub fn upsert(&mut self, folder: ProjectView) {
        match self.known.iter_mut().find(|p| p.path == folder.path) {
            Some(known) => *known = folder,
            None => self.known.push(folder),
        }
    }
}

/// A folder's own name (`/` and odd paths: the whole path).
pub fn folder_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

/// The order folders are shown in, in the list and the Folders modal: the
/// launch folder first, then by name (ignoring case), then by path. Never
/// by activity.
pub fn compare_folders(launch: &ScopeRoot, a: &ScopeRoot, b: &ScopeRoot) -> Ordering {
    let name = |f: &ScopeRoot| folder_name(f.path());
    (a != launch)
        .cmp(&(b != launch))
        .then_with(|| name(a).to_lowercase().cmp(&name(b).to_lowercase()))
        .then_with(|| a.path().cmp(b.path()))
}

/// Whether the modal's query is a folder path rather than a filter: it
/// starts with `/`, `~` or `.`, or has a `/` in it.
pub fn looks_like_path(query: &str) -> bool {
    let query = query.trim();
    starts_like_path(query) || query.contains('/')
}

/// A path that starts with `/`, `~` or `.`: only a path, never a filter.
fn starts_like_path(query: &str) -> bool {
    matches!(query.trim().chars().next(), Some('/' | '~' | '.'))
}

/// What adding the typed folder means: `Ok(None)` when it's one of `known`
/// already. `~` is expanded and relative paths start at `base`. A folder
/// that doesn't exist can be created when its parent does.
pub fn new_folder(
    input: &str,
    base: &Path,
    known: &[ScopeRoot],
) -> Result<Option<NewFolder>, String> {
    let input = input.trim();
    if input.is_empty() {
        return Err("Type a folder to add.".into());
    }
    folder_to_add(&expand(input, base, home_dir().as_deref()), known)
}

/// What adding the folder at `raw` means; see [`new_folder`].
fn folder_to_add(raw: &Path, known: &[ScopeRoot]) -> Result<Option<NewFolder>, String> {
    match std::fs::canonicalize(raw) {
        Ok(path) if !path.is_dir() => Err(format!("{} is not a folder", path.display())),
        Ok(path) if known.iter().any(|k| k.path() == path) => Ok(None),
        Ok(path) => Ok(Some(NewFolder::Existing(path))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let (Some(parent), Some(name)) = (raw.parent(), raw.file_name()) else {
                return Err(format!("{} can't be created", raw.display()));
            };
            match std::fs::canonicalize(parent) {
                Ok(parent) if parent.is_dir() => Ok(Some(NewFolder::Create(parent.join(name)))),
                _ => Err(format!("No folder {}", parent.display())),
            }
        }
        Err(error) => Err(format!("{} can't be used: {error}", raw.display())),
    }
}

/// What a (re)sync loaded.
pub struct SyncResult {
    /// Known folders; `None` when the daemon doesn't support folders.
    pub known: Option<Vec<ProjectView>>,
    pub support: FolderSupport,
    /// The folders whose sessions were listed and are watched.
    pub listed: Listed,
    pub sessions: Vec<SessionView>,
    pub stream: EventStream,
}

fn is_unsupported(error: &ApiError) -> bool {
    error.code == ErrorCode::Unsupported
}

/// Whether the daemon lists and streams every folder (`sessions.all`).
fn lists_every_folder(api: &dyn SessionApi) -> bool {
    api.daemon_capabilities()
        .is_some_and(|capabilities| capabilities.iter().any(|c| c == CAPABILITY_SESSIONS_ALL))
}

/// Add the launch folder, load the known folders, subscribe, then list the
/// sessions (subscribe first, so no change is missed): every folder's with
/// `sessions.all`; else the known folders' (an older daemon); else the
/// launch folder's alone (a daemon from before folders). Blocking.
pub fn load(api: &dyn SessionApi, launch: &ScopeRoot) -> Result<SyncResult, ApiError> {
    let Some(known) = load_known(api, launch)? else {
        return load_launch_only(api, launch);
    };
    if lists_every_folder(api) {
        let stream = api.subscribe(SubscribeRequest {
            all: true,
            ..Default::default()
        })?;
        let sessions = api.list_all(true)?;
        return Ok(SyncResult {
            known: Some(known),
            support: FolderSupport::All,
            listed: Listed::All,
            sessions,
            stream,
        });
    }
    // Every folder with sessions is a known folder, so listing them all
    // shows every session; folders that appear later need another sync.
    let mut folders: Vec<ScopeRoot> = known.iter().map(|f| f.path.clone()).collect();
    if !folders.contains(launch) {
        folders.insert(0, launch.clone());
    }
    let stream = api.subscribe(SubscribeRequest {
        scopes: folders.clone(),
        ..Default::default()
    })?;
    let sessions = match api.list_in(&folders, true) {
        Err(error) if is_unsupported(&error) => return load_launch_only(api, launch),
        result => result?,
    };
    Ok(SyncResult {
        known: Some(known),
        support: FolderSupport::Known,
        listed: Listed::Folders(folders),
        sessions,
        stream,
    })
}

/// Register the launch folder and list every known folder; `None` when the
/// daemon doesn't know folders.
fn load_known(
    api: &dyn SessionApi,
    launch: &ScopeRoot,
) -> Result<Option<Vec<ProjectView>>, ApiError> {
    match api.add_project(launch.path()) {
        Ok(_) => {}
        Err(error) if is_unsupported(&error) => return Ok(None),
        // Its sessions are still listed; the folder list says the rest.
        Err(_) => {}
    }
    match api.projects() {
        Ok(known) => Ok(Some(known)),
        Err(error) if is_unsupported(&error) => Ok(None),
        Err(error) => Err(error),
    }
}

/// The behaviour from before folders: the launch folder alone.
fn load_launch_only(api: &dyn SessionApi, launch: &ScopeRoot) -> Result<SyncResult, ApiError> {
    let stream = api.subscribe(SubscribeRequest {
        scope: Some(launch.clone()),
        ..Default::default()
    })?;
    let sessions = api.list(Some(launch), true)?;
    Ok(SyncResult {
        known: None,
        support: FolderSupport::LaunchOnly,
        listed: Listed::Folders(vec![launch.clone()]),
        sessions,
        stream,
    })
}

impl App {
    /// Apply what a sync loaded about folders.
    pub(super) fn apply_synced_folders(
        &mut self,
        known: Option<Vec<ProjectView>>,
        support: FolderSupport,
    ) {
        self.folders.known = known.unwrap_or_default();
        self.folders.support = support;
        if support == FolderSupport::LaunchOnly {
            self.list.set_folder_filter(Vec::new());
            if matches!(self.mode, AppMode::Folders(_)) {
                self.mode = AppMode::Normal;
            }
        }
    }

    /// The folder filter's label: `All folders`, the one folder's name, or
    /// `3 folders`.
    pub fn folder_label(&self) -> String {
        match self.list.folder_filter() {
            [] => "All folders".into(),
            [only] => self.folders.name_of(only),
            many => format!("{} folders", many.len()),
        }
    }

    /// `(live, total)` sessions of a folder: from the list when it has the
    /// folder's sessions, else from the daemon's last folder list.
    pub fn folder_counts(&self, folder: &ScopeRoot) -> (usize, usize) {
        if self.list.is_listed(folder) {
            return self.list.counts(folder);
        }
        self.folders
            .get(folder)
            .map(|p| (p.live as usize, p.sessions as usize))
            .unwrap_or((0, 0))
    }

    /// Where new sessions go: the current folder section, retained on the
    /// tabs and "+ New session" rows; the folder filter's only folder takes
    /// precedence while All folders is the active tab. Without folder
    /// context, use the launch folder.
    pub fn current_folder(&self) -> ScopeRoot {
        self.list
            .current_folder()
            .cloned()
            .unwrap_or_else(|| self.folders.launch.clone())
    }

    /// A session showed up in `folder`: ask the daemon about the folder
    /// when it's new to this TUI (its name and branch for the header).
    pub(super) fn learn_folder(&mut self, folder: &ScopeRoot) {
        let asks = matches!(
            self.folders.support,
            FolderSupport::All | FolderSupport::Known
        );
        if asks && !self.folders.is_known(folder) {
            self.refresh_folders();
        }
    }

    /// Show these folders (none: all folders) under All folders, which
    /// becomes the active tab. The list already has every folder's
    /// sessions; an older daemon is asked for a folder it isn't watching
    /// yet.
    pub fn show_folders(&mut self, folders: Vec<ScopeRoot>) {
        let current = self.list.folder_filter();
        let unchanged = self.list.grouping() == Grouping::Folder
            && folders.len() == current.len()
            && folders.iter().all(|f| current.contains(f));
        if unchanged {
            return;
        }
        self.list.set_grouping(Grouping::Folder);
        self.list.set_folder_filter(folders);
        let unlisted = self
            .list
            .folder_filter()
            .iter()
            .any(|folder| !self.list.is_listed(folder));
        if unlisted {
            self.sync();
        }
        self.reconcile_detail();
        let shown = match self.list.folder_filter() {
            [] => "all folders".to_string(),
            _ => self.folder_label(),
        };
        self.set_status(format!("Showing {shown}"));
    }

    // ------------------------------------------------------------ modal

    /// Enter on the folder filter, `P` or a click on it: open the Folders
    /// modal, ticked like the filter, the cursor on All folders or the
    /// filter's first folder.
    pub fn open_folder_picker(&mut self) {
        if !self.connection.is_connected() {
            self.set_status("Not connected to the daemon yet.");
            return;
        }
        match self.folders.support {
            FolderSupport::All | FolderSupport::Known => {}
            FolderSupport::Unknown => {
                self.set_status("Still loading folders…");
                return;
            }
            FolderSupport::LaunchOnly => {
                self.set_status(FOLDERS_UNSUPPORTED);
                return;
            }
        }
        let filter = self.list.folder_filter().to_vec();
        let focus = match self.list.groups().iter().find(|g| filter.contains(g)) {
            Some(first) => PickerRow::Folder(first.clone()),
            None => PickerRow::All,
        };
        self.mode = AppMode::Folders(FolderPicker::new(filter));
        self.refilter_picker(Some(&focus));
        self.refresh_folders();
    }

    /// Ask the daemon for its folders again (counts, branches, new ones).
    /// The daemon publishes no event when folders are added.
    fn refresh_folders(&mut self) {
        let Some(api) = self.connection.api() else {
            return;
        };
        self.exec.job(move || AppEvent::FoldersLoaded {
            result: api.projects(),
        });
    }

    pub(super) fn on_folders_loaded(&mut self, result: Result<Vec<ProjectView>, ApiError>) {
        match result {
            Ok(known) => {
                self.folders.known = known;
                self.refresh_picker_rows();
            }
            Err(error) if is_unsupported(&error) => {
                self.folders.support = FolderSupport::LaunchOnly;
                if matches!(self.mode, AppMode::Folders(_)) {
                    self.mode = AppMode::Normal;
                    self.set_status(FOLDERS_UNSUPPORTED);
                }
            }
            Err(error) => {
                if let AppMode::Folders(picker) = &mut self.mode {
                    picker.error = Some(format!("Couldn't load folders: {error}"));
                }
            }
        }
    }

    /// The folders the modal lists: every folder with sessions (from the
    /// list, and from the daemon for folders the list doesn't cover) and
    /// the filter's, in display order.
    pub fn picker_folders(&self) -> Vec<ScopeRoot> {
        let mut folders: Vec<ScopeRoot> = Vec::new();
        let unlisted_with_sessions = self
            .folders
            .known
            .iter()
            .filter(|f| f.sessions > 0 && !self.list.is_listed(&f.path))
            .map(|f| &f.path);
        let candidates = self
            .list
            .sessions()
            .iter()
            .map(|s| &s.scope)
            .chain(unlisted_with_sessions)
            .chain(self.list.folder_filter());
        for folder in candidates {
            if !folders.contains(folder) {
                folders.push(folder.clone());
            }
        }
        let launch = &self.folders.launch;
        folders.sort_by(|a, b| compare_folders(launch, a, b));
        folders
    }

    /// Folders a typed path may already be: the daemon's and the listed.
    fn known_folder_paths(&self) -> Vec<ScopeRoot> {
        let mut known: Vec<ScopeRoot> = self.folders.known.iter().map(|f| f.path.clone()).collect();
        for folder in self.picker_folders() {
            if !known.contains(&folder) {
                known.push(folder);
            }
        }
        known
    }

    /// The folders changed: rebuild the modal's rows, the cursor staying on
    /// its row.
    fn refresh_picker_rows(&mut self) {
        let AppMode::Folders(picker) = &self.mode else {
            return;
        };
        let current = picker.current_row().cloned();
        self.refilter_picker(current.as_ref());
    }

    /// Rows for the query. A folder path lists the typed folder (to add,
    /// or the folder it already is), the folders completing it, then the
    /// listed folders inside it; a filter lists All folders (while it
    /// matches) and the matching folders. The cursor goes to `focus` when
    /// it's listed, else to the first suggestion when the typed folder
    /// doesn't exist, else to the first row.
    fn refilter_picker(&mut self, focus: Option<&PickerRow>) {
        let base = self.folders.launch.path().to_path_buf();
        let home = home_dir();
        let known = self.known_folder_paths();
        let listable = self.picker_folders();
        let AppMode::Folders(picker) = &self.mode else {
            return;
        };
        let query = picker.query.value().trim().to_string();
        let needle = query.to_lowercase();
        let mut rows = Vec::new();
        let mut note = None;
        let mut typed_known = None;
        let mut suggestions = path_complete::Suggestions::default();
        // A typed folder lists the folders inside it, by its text (a
        // partly typed name) or its canonical path (`..`, links).
        let typed = looks_like_path(&query).then(|| {
            let raw = expand(&query, &base, home.as_deref());
            let canonical = std::fs::canonicalize(&raw).ok();
            (raw.to_string_lossy().into_owned(), canonical)
        });
        if let Some((_, canonical)) = &typed {
            match new_folder(&query, &base, &known) {
                Ok(Some(folder)) => rows.push(PickerRow::Add(folder)),
                Ok(None) => {
                    typed_known = canonical
                        .as_ref()
                        .and_then(|c| known.iter().find(|k| k.path() == c))
                        .cloned();
                    rows.extend(typed_known.clone().map(PickerRow::Folder));
                }
                // `web/app` may be a filter too: no complaint about it.
                Err(error) if starts_like_path(&query) => note = Some(error),
                Err(_) => {}
            }
            suggestions = path_complete::suggest(&query, &base, home.as_deref(), MAX_SUGGESTIONS);
        } else if needle.is_empty() || "all folders".contains(&needle) {
            rows.push(PickerRow::All);
        }
        let typed_folder = typed.is_some() && !rows.is_empty();
        // The typed folder itself is its first row already.
        let typed_text = query.trim_end_matches('/');
        let completing: Vec<_> = suggestions
            .folders
            .into_iter()
            .filter(|s| !(typed_folder && s.value.trim_end_matches('/') == typed_text))
            .collect();
        let shown = |folder: &ScopeRoot| {
            typed_known.as_ref() == Some(folder)
                || completing.iter().any(|s| s.path == folder.path())
        };
        let folders: Vec<ScopeRoot> = listable
            .iter()
            .filter(|folder| !shown(folder))
            .filter(|folder| match &typed {
                Some((raw, canonical)) => {
                    folder.path().to_string_lossy().starts_with(raw.as_str())
                        || canonical
                            .as_ref()
                            .is_some_and(|c| folder.path().starts_with(c))
                        || (!starts_like_path(&query) && self.folder_matches(folder, &needle))
                }
                None => needle.is_empty() || self.folder_matches(folder, &needle),
            })
            .cloned()
            .collect();
        rows.extend(completing.into_iter().map(PickerRow::Suggestion));
        rows.extend(folders.into_iter().map(PickerRow::Folder));
        let AppMode::Folders(picker) = &mut self.mode else {
            return;
        };
        let first_suggestion = rows
            .iter()
            .position(|r| matches!(r, PickerRow::Suggestion(_)));
        picker.cursor = focus
            .and_then(|f| rows.iter().position(|r| r == f))
            .or_else(|| match rows.first() {
                Some(PickerRow::Add(NewFolder::Create(_))) => first_suggestion,
                _ => None,
            })
            .unwrap_or(0);
        picker.rows = rows;
        picker.typed_folder = typed_folder;
        picker.folder_note = note;
        picker.completion = suggestions.completion;
        picker.more_folders = suggestions.more;
    }

    /// The filter matches the folder's name, path or branch.
    fn folder_matches(&self, folder: &ScopeRoot, needle: &str) -> bool {
        self.folders.name_of(folder).to_lowercase().contains(needle)
            || folder
                .path()
                .to_string_lossy()
                .to_lowercase()
                .contains(needle)
            || self
                .folders
                .get(folder)
                .and_then(|p| p.git.as_ref())
                .and_then(|g| g.branch.as_deref())
                .is_some_and(|b| b.to_lowercase().contains(needle))
    }

    /// The query changed: filter again from the top.
    pub(super) fn picker_query_changed(&mut self) {
        if let AppMode::Folders(picker) = &mut self.mode {
            picker.error = None;
        }
        self.refilter_picker(None);
    }

    pub(super) fn move_picker_cursor(&mut self, delta: isize) {
        if let AppMode::Folders(picker) = &mut self.mode {
            let last = picker.rows.len().saturating_sub(1) as isize;
            picker.cursor = (picker.cursor as isize + delta).clamp(0, last) as usize;
            picker.error = None;
        }
    }

    /// Tab: complete the typed folder, or tick the row under the cursor.
    pub(super) fn picker_tab(&mut self) {
        if !self.complete_picker_folder() {
            self.toggle_picker_tick();
        }
    }

    /// Complete the typed folder with the row under the cursor: a
    /// suggestion goes in whole; on the typed folder, a `/` goes after it
    /// (its folders are offered next); otherwise the typed path is completed
    /// as far as the matching folders agree, and when that adds nothing the
    /// cursor goes to the first suggestion. Returns `false` when the query
    /// isn't a path or the cursor is on another folder (Tab ticks it).
    pub(super) fn complete_picker_folder(&mut self) -> bool {
        let AppMode::Folders(picker) = &mut self.mode else {
            return false;
        };
        let query = picker.query.value().trim().to_string();
        if !looks_like_path(&query) {
            return false;
        }
        let on_typed_folder = picker.typed_folder && picker.cursor == 0;
        let value = match picker.current_row() {
            Some(PickerRow::Suggestion(folder)) => Some(folder.value.clone()),
            Some(PickerRow::Folder(_)) if !on_typed_folder => return false,
            // The typed folder exists: go into it.
            Some(PickerRow::Add(NewFolder::Existing(_)) | PickerRow::Folder(_))
                if !query.ends_with('/') =>
            {
                Some(format!("{query}/"))
            }
            _ => picker.completion.clone(),
        };
        match value {
            Some(value) if value != picker.query.value() => {
                picker.query.set(value);
                self.picker_query_changed();
            }
            _ => {
                if let Some(first) = picker
                    .rows
                    .iter()
                    .position(|r| matches!(r, PickerRow::Suggestion(_)))
                {
                    picker.cursor = first;
                }
                picker.error = None;
            }
        }
        true
    }

    /// A click on a modal row: a suggestion goes into the query (like
    /// Tab), any other row is picked (like Enter).
    pub(super) fn click_picker_row(&mut self, row: usize) {
        let AppMode::Folders(picker) = &mut self.mode else {
            return;
        };
        if row >= picker.rows.len() {
            return;
        }
        picker.cursor = row;
        if matches!(picker.current_row(), Some(PickerRow::Suggestion(_))) {
            self.complete_picker_folder();
        } else {
            self.picker_enter();
        }
    }

    /// Tab or Space: tick or untick the folder under the cursor. On All
    /// folders, clear the ticks (All folders is ticked when nothing else
    /// is).
    pub(super) fn toggle_picker_tick(&mut self) {
        let AppMode::Folders(picker) = &mut self.mode else {
            return;
        };
        match picker.current_row().cloned() {
            Some(PickerRow::All) => {
                if picker.checked.is_empty() {
                    return;
                }
                picker.checked.clear();
            }
            Some(PickerRow::Folder(folder)) => {
                if picker.checked.contains(&folder) {
                    picker.checked.retain(|f| f != &folder);
                } else {
                    picker.checked.push(folder);
                }
            }
            Some(PickerRow::Add(_) | PickerRow::Suggestion(_)) | None => return,
        }
        picker.ticks_changed = true;
        picker.error = None;
    }

    /// Enter: All folders clears the filter; a folder is shown alone, or
    /// once ticks were changed with the ticked ones; a typed or suggested
    /// folder the daemon doesn't know is added (asking first before
    /// creating one) and shown alone.
    pub(super) fn picker_enter(&mut self) {
        let known = self.known_folder_paths();
        let AppMode::Folders(picker) = &mut self.mode else {
            return;
        };
        let row = match picker.current_row().cloned() {
            // A suggestion the daemon knows already is shown, not added.
            Some(PickerRow::Suggestion(folder)) => match folder_to_add(&folder.path, &known) {
                Ok(Some(new)) => Some(PickerRow::Add(new)),
                Ok(None) => std::fs::canonicalize(&folder.path)
                    .ok()
                    .map(|path| PickerRow::Folder(ScopeRoot::from_canonical(path))),
                Err(error) => {
                    picker.error = Some(error);
                    return;
                }
            },
            row => row,
        };
        match row {
            Some(PickerRow::All) => {
                self.mode = AppMode::Normal;
                self.show_folders(Vec::new());
            }
            Some(PickerRow::Folder(folder)) => {
                let folders = picker.folders_for(&folder);
                self.mode = AppMode::Normal;
                self.show_folders(folders);
            }
            // Creating a folder is asked first: a half-typed name is easy
            // to send by mistake.
            Some(PickerRow::Add(NewFolder::Create(path))) => {
                picker.stage = PickerStage::Creating(path);
                picker.error = None;
            }
            Some(PickerRow::Add(folder)) => self.add_folder_from_picker(folder),
            Some(PickerRow::Suggestion(_)) => {}
            None => {
                picker.error = Some(match &picker.folder_note {
                    Some(note) => note.clone(),
                    None => "No folder matches; type a folder path (/, ~, .) to add one.".into(),
                });
            }
        }
    }

    /// `y` after Enter on "+ Create folder": create it and add it.
    pub(super) fn create_picker_folder(&mut self) {
        let AppMode::Folders(picker) = &mut self.mode else {
            return;
        };
        let PickerStage::Creating(path) = &picker.stage else {
            return;
        };
        let path = path.clone();
        picker.stage = PickerStage::Browse;
        self.add_folder_from_picker(NewFolder::Create(path));
    }

    /// Ask the daemon to add the typed folder, creating it first if needed.
    fn add_folder_from_picker(&mut self, folder: NewFolder) {
        let Some(api) = self.connection.api() else {
            return;
        };
        let AppMode::Folders(picker) = &mut self.mode else {
            return;
        };
        if picker.busy {
            return;
        }
        picker.busy = true;
        picker.error = None;
        self.exec.job(move || {
            let result = match &folder {
                NewFolder::Create(path) => std::fs::create_dir(path).map_err(|error| {
                    ApiError::new(
                        ErrorCode::InvalidArgument,
                        format!("couldn't create {}: {error}", path.display()),
                    )
                }),
                NewFolder::Existing(_) => Ok(()),
            }
            .and_then(|()| api.add_project(folder.path()));
            AppEvent::FolderAdded { result }
        });
    }

    /// The folder was added: show it alone, so new sessions start there.
    pub(super) fn on_folder_added(&mut self, result: Result<ProjectView, ApiError>) {
        match result {
            Ok(folder) => {
                let path = folder.path.clone();
                self.folders.upsert(folder);
                if !matches!(self.mode, AppMode::Folders(_)) {
                    return;
                }
                self.mode = AppMode::Normal;
                self.show_folders(vec![path]);
            }
            Err(error) => match &mut self.mode {
                AppMode::Folders(picker) => {
                    picker.busy = false;
                    picker.error = Some(format!("Couldn't add the folder: {error}"));
                }
                _ => self.set_status(format!("Couldn't add the folder: {error}")),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn folder(path: &str) -> ScopeRoot {
        ScopeRoot::from_canonical(path.into())
    }

    fn view(path: &str) -> ProjectView {
        ProjectView {
            path: folder(path),
            name: folder_name(Path::new(path)),
            sessions: 0,
            live: 0,
            last_active_at: None,
            added_at: 0,
            git: None,
        }
    }

    #[test]
    fn folders_sort_launch_first_then_by_name_then_path() {
        let launch = folder("/code/repo");
        let mut folders = vec![
            folder("/x/web"),
            folder("/code/Api"),
            folder("/code/web"),
            launch.clone(),
            folder("/b/api"),
        ];
        folders.sort_by(|a, b| compare_folders(&launch, a, b));
        assert_eq!(
            folders,
            [
                launch,
                folder("/b/api"),
                folder("/code/Api"),
                folder("/code/web"),
                folder("/x/web"),
            ]
        );
    }

    #[test]
    fn names_come_from_the_daemon_or_the_folder() {
        let mut folders = Folders::new(folder("/repo"));
        assert_eq!(folders.name_of(&folder("/not/known")), "known");
        assert!(!folders.is_known(&folder("/web")));
        let mut web = view("/web");
        web.name = "website".into();
        folders.upsert(web);
        assert!(folders.is_known(&folder("/web")));
        assert_eq!(folders.name_of(&folder("/web")), "website");
        assert_eq!(folder_name(Path::new("/")), "/");
    }

    #[test]
    fn typed_folders_are_added_or_created_unless_already_known() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("web")).unwrap();
        std::fs::write(dir.path().join("file"), "x").unwrap();
        let base = std::fs::canonicalize(dir.path()).unwrap();
        let known = [ScopeRoot::from_canonical(base.clone())];
        assert!(looks_like_path("~/code"));
        assert!(looks_like_path(" ./web"));
        assert!(looks_like_path("/srv"));
        assert!(!looks_like_path("web"));
        assert_eq!(
            new_folder("./web", &base, &known).unwrap(),
            Some(NewFolder::Existing(base.join("web")))
        );
        assert_eq!(
            new_folder(base.join("web/").to_str().unwrap(), &base, &known).unwrap(),
            Some(NewFolder::Existing(base.join("web")))
        );
        // Known already: nothing to add.
        assert_eq!(new_folder(".", &base, &known).unwrap(), None);
        assert_eq!(new_folder("./web/..", &base, &known).unwrap(), None);
        // A missing folder can be created when its parent exists.
        assert_eq!(
            new_folder("./web/app", &base, &known).unwrap(),
            Some(NewFolder::Create(base.join("web/app")))
        );
        assert!(new_folder("./nope/app", &base, &known)
            .unwrap_err()
            .starts_with("No folder"));
        assert!(new_folder("./file", &base, &known)
            .unwrap_err()
            .contains("not a folder"));
        assert!(new_folder("  ", &base, &known).is_err());
    }
}
