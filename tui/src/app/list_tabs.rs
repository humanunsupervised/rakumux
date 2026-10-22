//! The tabs on the session list's top row (Folders, App, Status;
//! `grouping`) and the App and Status modals. ←/→ on the top row, or a
//! click, switch tabs; Enter there, or a click on the active tab, opens the
//! active tab's modal (the Folders modal for All folders, `folders.rs`).
//! The App and Status modals list All apps (All statuses), then each app
//! (status) with its sessions; typing narrows the rows, Tab (Space, while
//! nothing is typed) ticks several, Enter shows the one under the cursor
//! (after ticking, the ticked ones) and makes the tab active, Esc closes
//! without changes.

use super::grouping::{sort_sections, Grouping, Section, StatusGroup};
use super::types::{AppMode, SectionPicker, SectionRow};
use super::App;

impl App {
    /// What a tab of the top row says. The active one names its filter:
    /// All folders, `web` or `3 folders`; App, `claude` or `2 apps`; Status,
    /// `working` or `2 statuses`. The others say what the list shows of them
    /// while they're inactive: Folders, App, Status.
    pub fn tab_label(&self, grouping: Grouping) -> String {
        if grouping != self.list.grouping() {
            return grouping.name().into();
        }
        match grouping {
            Grouping::Folder => self.folder_label(),
            Grouping::App | Grouping::Status => match self.list.section_filter(grouping).as_slice()
            {
                [] => grouping.name().into(),
                [only] => only.label().into(),
                many => format!("{} {}", many.len(), grouping.plural()),
            },
        }
    }

    /// The active tab's label: what the list shows (the Tabs layout's
    /// selector and Grid's header say it).
    pub fn filter_label(&self) -> String {
        self.tab_label(self.list.grouping())
    }

    /// ←/→ (`h`/`l`) on the top row: the previous or next tab.
    pub fn switch_tab(&mut self, delta: isize) {
        self.show_tab(self.list.grouping().step(delta));
    }

    /// Make `grouping`'s tab active: its sections and filter apply.
    pub fn show_tab(&mut self, grouping: Grouping) {
        if grouping != self.list.grouping() {
            self.list.set_grouping(grouping);
            self.reconcile_detail();
        }
    }

    /// Enter on the top row, or a click on the active tab: its modal.
    pub fn open_tab_picker(&mut self) {
        match self.list.grouping() {
            Grouping::Folder => self.open_folder_picker(),
            grouping => self.open_section_picker(grouping, None),
        }
    }

    /// The App or Status modal, ticked like its filter, the cursor on
    /// `focus` (else the filter's first, else All).
    pub fn open_section_picker(&mut self, grouping: Grouping, focus: Option<Section>) {
        if grouping == Grouping::Folder {
            return self.open_folder_picker();
        }
        let filter = self.list.section_filter(grouping);
        let focus = match focus.or_else(|| filter.first().cloned()) {
            Some(section) => SectionRow::Section(section),
            None => SectionRow::All,
        };
        self.mode = AppMode::Sections(SectionPicker::new(grouping, filter));
        self.refilter_section_picker(Some(&focus));
    }

    /// What the modal lists: every app (status) that has sessions and the
    /// filter's, in display order. Status lists every status but
    /// `unknown`, sessions or not, so one that nothing is in yet can be
    /// picked (to see sessions as they get there).
    pub fn picker_sections(&self, grouping: Grouping) -> Vec<Section> {
        let mut sections: Vec<Section> = match grouping {
            Grouping::Status => StatusGroup::ALL
                .iter()
                .filter(|status| **status != StatusGroup::Unknown)
                .copied()
                .map(Section::Status)
                .collect(),
            Grouping::Folder | Grouping::App => Vec::new(),
        };
        let listed = self
            .list
            .sessions()
            .iter()
            .filter_map(|s| Section::of(grouping, s));
        for section in listed.chain(self.list.section_filter(grouping)) {
            if !sections.contains(&section) {
                sections.push(section);
            }
        }
        sort_sections(&mut sections, self.list.sessions());
        sections
    }

    /// Rows for the query: All (while "all apps" or "all statuses" matches
    /// it), then the apps or statuses whose name has it. The cursor goes to
    /// `focus` when it's listed, else to the first row.
    fn refilter_section_picker(&mut self, focus: Option<&SectionRow>) {
        let AppMode::Sections(picker) = &self.mode else {
            return;
        };
        let grouping = picker.grouping;
        let needle = picker.query.value().trim().to_lowercase();
        let all = format!("all {}", grouping.plural());
        let mut rows = Vec::new();
        if all.contains(&needle) {
            rows.push(SectionRow::All);
        }
        rows.extend(
            self.picker_sections(grouping)
                .into_iter()
                .filter(|section| section.label().to_lowercase().contains(&needle))
                .map(SectionRow::Section),
        );
        let AppMode::Sections(picker) = &mut self.mode else {
            return;
        };
        picker.cursor = focus
            .and_then(|f| rows.iter().position(|r| r == f))
            .unwrap_or(0);
        picker.rows = rows;
    }

    /// The query changed: filter again from the top.
    pub(super) fn section_query_changed(&mut self) {
        self.refilter_section_picker(None);
    }

    pub(super) fn move_section_cursor(&mut self, delta: isize) {
        if let AppMode::Sections(picker) = &mut self.mode {
            let last = picker.rows.len().saturating_sub(1) as isize;
            picker.cursor = (picker.cursor as isize + delta).clamp(0, last) as usize;
        }
    }

    /// Tab or Space: tick or untick the row under the cursor; on All, clear
    /// the ticks (All is ticked when nothing else is).
    pub(super) fn toggle_section_tick(&mut self) {
        let AppMode::Sections(picker) = &mut self.mode else {
            return;
        };
        match picker.current_row().cloned() {
            Some(SectionRow::All) => {
                if picker.checked.is_empty() {
                    return;
                }
                picker.checked.clear();
            }
            Some(SectionRow::Section(section)) => {
                if picker.checked.contains(&section) {
                    picker.checked.retain(|s| s != &section);
                } else {
                    picker.checked.push(section);
                }
            }
            None => return,
        }
        picker.ticks_changed = true;
    }

    /// Enter: All clears the filter; an app or a status is shown alone, or
    /// once ticks were changed with the ticked ones. Either way its tab is
    /// the active one.
    pub(super) fn section_picker_enter(&mut self) {
        let AppMode::Sections(picker) = &self.mode else {
            return;
        };
        let grouping = picker.grouping;
        let sections = match picker.current_row() {
            Some(SectionRow::All) => Vec::new(),
            Some(SectionRow::Section(section)) => picker.sections_for(section),
            // Nothing matches what's typed: nothing to show.
            None => return,
        };
        self.mode = AppMode::Normal;
        self.show_sections(grouping, sections);
    }

    /// A click on a row of the modal: like Enter on it.
    pub(super) fn click_section_row(&mut self, row: usize) {
        let AppMode::Sections(picker) = &mut self.mode else {
            return;
        };
        if row < picker.rows.len() {
            picker.cursor = row;
            self.section_picker_enter();
        }
    }

    /// Show these apps or statuses (none: all of them) under their tab,
    /// which becomes the active one.
    pub fn show_sections(&mut self, grouping: Grouping, sections: Vec<Section>) {
        self.list.set_grouping(grouping);
        self.list.set_section_filter(grouping, sections);
        self.reconcile_detail();
        let shown = match self.list.section_filter(grouping).as_slice() {
            [] => format!("all {}", grouping.plural()),
            _ => self.tab_label(grouping),
        };
        self.set_status(format!("Showing {shown}"));
    }
}
