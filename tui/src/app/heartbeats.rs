//! The left column's Heartbeats tab: the user's heartbeats, tasks the daemon runs on a
//! clock (each run a new session given the task). Read from the daemon
//! (`heartbeat_list`, off the UI thread) every 2 s while the tab is on
//! screen; the daemon sends no heartbeat events.
//!
//! With Rakumux mode's keys: `↑`/`↓` (`j`/`k`) select, `Enter` reads the
//! selected heartbeat whole (its prompt, command and runs; there `↑`/`↓` pick
//! a run and `Enter` goes into it), `e` approves or enables (from the list it
//! first shows the heartbeat whole, so its prompt and command are read before
//! they run), `p` pauses, `r` runs it now, `c` edits it, `d` twice deletes it, `n` (or Enter or a click on `+ New heartbeat`, the list's last
//! row) opens the New heartbeat form (`heartbeat_form.rs`), `Esc` goes back.
//! The selected heartbeat shows these as buttons ([`BeatAction`]). `B` (or
//! Settings) shows the tab.

use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use session_core::api::{ApiError, ErrorCode};
use session_core::heartbeat::Heartbeat;
use session_core::ids::{HeartbeatId, SessionId};
use session_core::model::SessionView;

use super::drawer::Panel;
use super::left::LeftTab;
use super::runtime::AppEvent;
use super::App;

/// How often the tab reads the heartbeats while it's on screen.
const EVERY: Duration = Duration::from_secs(2);

/// A button of the selected (or read) heartbeat.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BeatAction {
    Run,
    /// Approve or enable it, or (from the list) show it whole first.
    Enable,
    Pause,
    Edit,
    Delete,
}

impl BeatAction {
    /// `heartbeat`'s buttons, in order.
    pub fn of(heartbeat: &Heartbeat) -> Vec<BeatAction> {
        let mut actions = Vec::new();
        if heartbeat.is_complete() && !heartbeat.awaiting_approval {
            actions.push(BeatAction::Run);
        }
        if heartbeat.enabled {
            actions.push(BeatAction::Pause);
        } else if heartbeat.is_complete() {
            actions.push(BeatAction::Enable);
        }
        actions.extend([BeatAction::Edit, BeatAction::Delete]);
        actions
    }

    /// Its key and what the button says.
    pub fn label(self, heartbeat: &Heartbeat) -> (&'static str, &'static str) {
        match self {
            BeatAction::Run => ("r", "run"),
            BeatAction::Enable if heartbeat.awaiting_approval => ("e", "approve"),
            BeatAction::Enable => ("e", "enable"),
            BeatAction::Pause => ("p", "pause"),
            BeatAction::Edit => ("c", "edit"),
            BeatAction::Delete => ("d", "delete"),
        }
    }
}

/// What the Heartbeats tab keeps for this window.
#[derive(Debug, Default)]
pub struct BeatsView {
    /// The heartbeats as last read, in the tab's order.
    pub heartbeats: Vec<Heartbeat>,
    /// The selected heartbeat.
    pub selected: Option<HeartbeatId>,
    /// `+ New heartbeat` is selected, under them.
    pub on_new: bool,
    /// The heartbeat read whole, with the run selected there.
    pub reading: Option<HeartbeatId>,
    pub run: usize,
    /// `d` was pressed once on this heartbeat: a second deletes it.
    pub deleting: Option<HeartbeatId>,
    /// Why the heartbeats can't be read.
    pub error: Option<String>,
    /// Read once at least.
    pub loaded: bool,
    busy: bool,
    due: Option<Instant>,
    /// Bumped when the tab leaves the screen: answers asked for before are
    /// dropped.
    generation: u64,
}

impl BeatsView {
    /// Take a reading: waiting for approval first, then by folder and name.
    fn take(&mut self, mut heartbeats: Vec<Heartbeat>) {
        heartbeats.sort_by(|a, b| {
            (!a.awaiting_approval, a.scope.path(), a.name.to_lowercase()).cmp(&(
                !b.awaiting_approval,
                b.scope.path(),
                b.name.to_lowercase(),
            ))
        });
        self.heartbeats = heartbeats;
        self.loaded = true;
        self.error = None;
        let gone = |id: &Option<HeartbeatId>, list: &[Heartbeat]| {
            id.as_ref()
                .is_some_and(|id| !list.iter().any(|h| &h.id == id))
        };
        if gone(&self.reading, &self.heartbeats) {
            self.reading = None;
        }
        if self.selected.is_none() || gone(&self.selected, &self.heartbeats) {
            self.selected = self.heartbeats.first().map(|h| h.id.clone());
        }
    }

    /// Show `id` whole, selected.
    pub(super) fn show(&mut self, id: &HeartbeatId) {
        self.selected = Some(id.clone());
        self.on_new = false;
        self.reading = Some(id.clone());
        self.run = 0;
    }

    /// `+ New heartbeat` is selected (there's nothing else without any).
    pub fn new_selected(&self) -> bool {
        self.on_new || self.heartbeats.is_empty()
    }

    /// The selected heartbeat's index; `+ New heartbeat`'s after them all.
    pub fn selected_index(&self) -> usize {
        if self.new_selected() {
            return self.heartbeats.len();
        }
        self.selected
            .as_ref()
            .and_then(|id| self.heartbeats.iter().position(|h| &h.id == id))
            .unwrap_or(0)
    }

    /// The selected heartbeat.
    pub fn selected(&self) -> Option<&Heartbeat> {
        self.heartbeats.get(self.selected_index())
    }

    /// The heartbeat read whole.
    pub fn read(&self) -> Option<&Heartbeat> {
        let id = self.reading.as_ref()?;
        self.heartbeats.iter().find(|h| &h.id == id)
    }

    /// Waiting for an answer, nothing shown yet: a spinner.
    pub fn waiting(&self) -> bool {
        self.busy && !self.loaded
    }
}

/// What a heartbeat's setup session types for its agent (not sent: the user
/// adds to it or presses Enter): fill `heartbeat` in.
pub fn setup_prompt(heartbeat: &Heartbeat) -> String {
    let handle = heartbeat.handle();
    let todo = match heartbeat.prompt.trim().is_empty() {
        true => "It's empty: ask me what task it should run and how often".to_string(),
        false => format!(
            "Read it with `\"$RAKUMUX_BIN\" heartbeat show {handle}` and ask me what to change \
             about its task and how often it runs"
        ),
    };
    format!(
        "Rakumux heartbeat {handle} (\"{}\") runs a task in this folder on a schedule, each run a \
         new session whose agent gets the task. {todo}, then save both with \
         `\"$RAKUMUX_BIN\" heartbeat update {handle} --prompt '<the task, self-contained>' \
         --every 30m` (or --daily HH:MM, or --days mon,fri --at HH:MM), or Rakumux's \
         update_heartbeat tool.",
        heartbeat.name.replace(['"', '\''], "")
    )
}

impl App {
    /// The Heartbeats tab is on screen.
    pub fn beats_shown(&self) -> bool {
        self.left_tab_shown(LeftTab::Beats)
    }

    /// The Heartbeats tab has Rakumux mode's keys.
    pub fn beats_has_keys(&self) -> bool {
        self.keys_panel() == Some(Panel::Beats)
    }

    /// `B` or Settings: the left column's Heartbeats with the keys, or back to the
    /// sessions while it has them.
    pub fn toggle_beats(&mut self) {
        self.toggle_left_tab(LeftTab::Beats);
    }

    /// The tick's Heartbeats work: coming on screen it reads at once, then every
    /// [`EVERY`] while on screen; leaving it drops answers in flight.
    pub(super) fn beats_timers(&mut self, now: Instant) {
        let shown = self.beats_shown();
        if shown != self.beats_on_screen {
            self.beats_on_screen = shown;
            self.beats.generation += 1;
            self.beats.busy = false;
            self.beats.due = None;
        }
        if shown && !self.beats.busy && self.beats.due.is_none_or(|due| now >= due) {
            self.read_heartbeats(now);
        }
    }

    fn read_heartbeats(&mut self, now: Instant) {
        let api = self.api();
        let view = &mut self.beats;
        view.due = Some(now + EVERY);
        let Some(api) = api else {
            view.error = Some("Not connected to the Rakumux server.".into());
            return;
        };
        view.busy = true;
        let generation = view.generation;
        self.exec.job(move || AppEvent::Heartbeats {
            generation,
            result: api.heartbeats(None),
        });
    }

    /// Read again at once (after a change this window made).
    pub(super) fn reread_heartbeats(&mut self) {
        self.beats.due = None;
        if !self.beats.busy && self.beats_shown() {
            self.read_heartbeats(Instant::now());
        }
    }

    pub(super) fn on_heartbeats(
        &mut self,
        generation: u64,
        result: Result<Vec<Heartbeat>, ApiError>,
    ) {
        let view = &mut self.beats;
        if view.generation != generation {
            return;
        }
        view.busy = false;
        match result {
            Ok(heartbeats) => view.take(heartbeats),
            Err(error) if error.code == ErrorCode::Unsupported => {
                view.error = Some(
                    "The running Rakumux server is older than this app: restart it (S) for \
                     heartbeats."
                        .into(),
                )
            }
            Err(error) => view.error = Some(error.message),
        }
    }

    /// A heartbeat this window made or changed, as the daemon answered it:
    /// listed so at once (a reading under way may not have it).
    pub(super) fn beats_take_changed(&mut self, heartbeat: &Heartbeat) {
        let view = &mut self.beats;
        let mut heartbeats = std::mem::take(&mut view.heartbeats);
        heartbeats.retain(|known| known.id != heartbeat.id);
        heartbeats.push(heartbeat.clone());
        view.take(heartbeats);
        view.generation += 1;
        view.busy = false;
        view.due = None;
    }

    /// A change this window asked for came back: say so and read again.
    pub(super) fn on_heartbeat_done(&mut self, result: Result<String, ApiError>) {
        match result {
            Ok(message) => self.set_status(message),
            Err(error) => self.set_status(format!("Heartbeat: {}", error.message)),
        }
        self.reread_heartbeats();
    }

    /// The runs of `id` this window lists, newest first.
    pub fn heartbeat_runs(&self, id: &HeartbeatId) -> Vec<&SessionView> {
        let mut runs: Vec<&SessionView> = self
            .list
            .sessions()
            .iter()
            .filter(|s| s.heartbeat.as_ref().is_some_and(|r| &r.heartbeat == id))
            .collect();
        runs.sort_by_key(|s| std::cmp::Reverse(s.heartbeat.as_ref().map_or(0, |r| r.run)));
        runs
    }

    /// Rakumux mode keys while Heartbeats has them; `false` leaves the key to the
    /// list's handler (`f`, `t`, `B` among them).
    pub fn beats_key(&mut self, key: KeyEvent) -> bool {
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            return false;
        }
        let deleting = self.beats.deleting.take();
        match key.code {
            KeyCode::Down | KeyCode::Char('j') => self.beats_move(1),
            KeyCode::Up | KeyCode::Char('k') => self.beats_move(-1),
            KeyCode::Home => self.beats_move(isize::MIN / 2),
            KeyCode::End | KeyCode::Char('G') => self.beats_move(isize::MAX / 2),
            KeyCode::Char('g') => self.pending_key = Some('g'),
            KeyCode::Enter | KeyCode::Char('i') | KeyCode::Char('l') => self.beats_enter(),
            KeyCode::Esc | KeyCode::Char('h') | KeyCode::Left => self.beats_back(),
            KeyCode::Char('e') => self.beats_enable(),
            KeyCode::Char('p') => self.beats_set_enabled(false),
            KeyCode::Char('r') => self.beats_run(),
            KeyCode::Char('n') => self.new_heartbeat(),
            KeyCode::Char('c') => self.beats_edit(),
            KeyCode::Char('d') => self.beats_delete(deleting),
            // Keys that act on the list's session do nothing here.
            KeyCode::Char('J' | 'K' | 'x' | 'z' | '/' | '{' | '}' | 'E' | 'W' | 'y' | 's') => {}
            _ => return false,
        }
        true
    }

    /// Move the selection (or, reading one, the selected run) `delta`.
    pub fn beats_move(&mut self, delta: isize) {
        if let Some(id) = self.beats.reading.clone() {
            let runs = self.heartbeat_runs(&id).len();
            let run = self.beats.run.saturating_add_signed(delta);
            self.beats.run = run.min(runs.saturating_sub(1));
            return;
        }
        let count = self.beats.heartbeats.len();
        let index = self
            .beats
            .selected_index()
            .saturating_add_signed(delta)
            .min(count);
        self.beats.on_new = index == count;
        if let Some(heartbeat) = self.beats.heartbeats.get(index) {
            self.beats.selected = Some(heartbeat.id.clone());
        }
    }

    /// Enter: read the selected heartbeat whole (or make a new one); reading
    /// one, go into its selected run.
    pub fn beats_enter(&mut self) {
        let Some(id) = self.beats.reading.clone() else {
            if self.beats.new_selected() {
                self.new_heartbeat();
            } else if let Some(heartbeat) = self.beats.selected() {
                self.beats.reading = Some(heartbeat.id.clone());
                self.beats.run = 0;
            }
            return;
        };
        let Some(run) = self
            .heartbeat_runs(&id)
            .get(self.beats.run)
            .map(|s| s.id.clone())
        else {
            self.set_status("No runs yet: r runs it now.");
            return;
        };
        self.go_into(run);
    }

    /// Esc: from a heartbeat read whole back to the list; from the list, the
    /// left column goes back to the sessions.
    pub fn beats_back(&mut self) {
        if self.beats.reading.take().is_none() {
            self.show_sessions_tab();
        }
    }

    /// The heartbeat the keys act on: the one read whole, else the
    /// selected one.
    fn beats_target(&self) -> Option<Heartbeat> {
        self.beats.read().or_else(|| self.beats.selected()).cloned()
    }

    /// `e`: read whole, approve or enable it; from the list, show it whole
    /// first (its prompt and command), so nothing runs unread.
    pub fn beats_enable(&mut self) {
        if self.beats.reading.is_none() {
            if let Some(heartbeat) = self.beats.selected() {
                self.beats.reading = Some(heartbeat.id.clone());
                self.beats.run = 0;
            }
            return;
        }
        self.beats_set_enabled(true);
    }

    fn beats_set_enabled(&mut self, enabled: bool) {
        let Some(heartbeat) = self.beats_target() else {
            return;
        };
        let Some(api) = self.api() else {
            self.set_status("Not connected to the daemon.");
            return;
        };
        let name = heartbeat.name.clone();
        let approving = heartbeat.awaiting_approval;
        self.exec.job(move || AppEvent::HeartbeatDone {
            result: api.heartbeat_enable(&heartbeat.id, enabled).map(|_| {
                match (enabled, approving) {
                    (true, true) => format!("Approved {name}: it runs on its schedule"),
                    (true, false) => format!("{name} runs on its schedule"),
                    (false, _) => format!("Paused {name}"),
                }
            }),
        });
    }

    /// `r`: start a run of it now.
    pub fn beats_run(&mut self) {
        let Some(heartbeat) = self.beats_target() else {
            return;
        };
        let Some(api) = self.api() else {
            self.set_status("Not connected to the daemon.");
            return;
        };
        self.exec.job(move || AppEvent::HeartbeatDone {
            result: api
                .heartbeat_run(&heartbeat.id)
                .map(|session| format!("Started {}", session.name)),
        });
    }

    /// `c`: change it, in the form.
    pub fn beats_edit(&mut self) {
        if let Some(heartbeat) = self.beats_target() {
            self.edit_heartbeat(heartbeat);
        }
    }

    /// A click on one of the selected (or read) heartbeat's buttons.
    pub fn click_beat_action(&mut self, action: BeatAction) {
        let deleting = self.beats.deleting.take();
        self.show_left_tab(LeftTab::Beats);
        match action {
            BeatAction::Run => self.beats_run(),
            BeatAction::Enable => self.beats_enable(),
            BeatAction::Pause => self.beats_set_enabled(false),
            BeatAction::Edit => self.beats_edit(),
            BeatAction::Delete => self.beats_delete(deleting),
        }
    }

    /// `d`: the first asks, a second (next key) deletes it.
    fn beats_delete(&mut self, armed: Option<HeartbeatId>) {
        let Some(heartbeat) = self.beats_target() else {
            return;
        };
        if armed.as_ref() != Some(&heartbeat.id) {
            self.set_status(format!(
                "Delete heartbeat {}? Again deletes it (its runs stay).",
                heartbeat.name
            ));
            self.beats.deleting = Some(heartbeat.id);
            return;
        }
        let Some(api) = self.api() else {
            self.set_status("Not connected to the daemon.");
            return;
        };
        self.beats.reading = None;
        let name = heartbeat.name.clone();
        self.exec.job(move || AppEvent::HeartbeatDone {
            result: api
                .heartbeat_delete(&heartbeat.id)
                .map(|()| format!("Deleted heartbeat {name}")),
        });
    }

    /// The session the footer names while Heartbeats has the keys: the selected
    /// run's, reading a heartbeat whole.
    pub fn beats_session(&self) -> Option<&SessionView> {
        let id = self.beats.reading.as_ref()?;
        let run: Option<SessionId> = self
            .heartbeat_runs(id)
            .get(self.beats.run)
            .map(|s| s.id.clone());
        self.list.get(&run?)
    }

    /// A click on row `index` (a heartbeat, or reading one, a run): Heartbeats
    /// gets the keys and it's selected; again (a tap on a phone), Enter.
    pub fn click_beat_row(&mut self, index: usize) {
        let again = self.beats_has_keys()
            && match &self.beats.reading {
                Some(_) => self.beats.run == index,
                None => self.beats.selected_index() == index,
            };
        self.show_left_tab(LeftTab::Beats);
        self.beats.deleting = None;
        match self.beats.reading.clone() {
            Some(_) => self.beats.run = index,
            None => {
                self.beats.on_new = false;
                if let Some(heartbeat) = self.beats.heartbeats.get(index) {
                    self.beats.selected = Some(heartbeat.id.clone());
                }
            }
        }
        if again || self.phone() {
            self.beats_enter();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use session_core::heartbeat::Schedule;
    use session_core::model::{Creator, ScopeRoot};

    fn heartbeat(name: &str, waiting: bool) -> Heartbeat {
        Heartbeat {
            id: HeartbeatId::from_string(name),
            scope: ScopeRoot::from_canonical("/w".into()),
            name: name.into(),
            agent: Some("claude".into()),
            command: "claude".into(),
            prompt: "do it".into(),
            schedule: Some(Schedule::Every { minutes: 30 }),
            enabled: !waiting,
            awaiting_approval: waiting,
            created_by: Creator::User,
            created_at: 0,
            updated_at: 0,
            next_run_at: None,
            runs: 0,
            last_run: None,
        }
    }

    #[test]
    fn heartbeats_waiting_for_approval_come_first_and_the_selection_stays() {
        let mut view = BeatsView::default();
        view.take(vec![
            heartbeat("b", false),
            heartbeat("z", true),
            heartbeat("a", false),
        ]);
        let names: Vec<&str> = view.heartbeats.iter().map(|h| h.name.as_str()).collect();
        assert_eq!(names, ["z", "a", "b"]);
        assert_eq!(view.selected().map(|h| h.name.as_str()), Some("z"));
        view.selected = Some(HeartbeatId::from_string("b"));
        view.reading = Some(HeartbeatId::from_string("a"));
        view.take(vec![heartbeat("b", false)]);
        assert_eq!(view.selected().map(|h| h.name.as_str()), Some("b"));
        assert!(view.reading.is_none(), "a heartbeat gone is no longer read");
    }

    #[test]
    fn new_heartbeat_is_the_row_after_the_last() {
        let mut view = BeatsView::default();
        assert!(view.new_selected(), "nothing else to select");
        view.take(vec![heartbeat("a", false)]);
        assert!(!view.new_selected());
        view.on_new = true;
        assert_eq!(view.selected_index(), 1);
        assert!(view.selected().is_none());
        // It stays selected as the heartbeats are read again.
        view.take(vec![heartbeat("a", false), heartbeat("b", false)]);
        assert_eq!(view.selected_index(), 2);
    }

    #[test]
    fn a_heartbeats_buttons_are_what_it_can_do() {
        let on = heartbeat("a", false);
        assert_eq!(
            BeatAction::of(&on),
            [
                BeatAction::Run,
                BeatAction::Pause,
                BeatAction::Edit,
                BeatAction::Delete
            ]
        );
        // One waiting for approval can't be run unread from its row.
        let waiting = heartbeat("b", true);
        assert_eq!(
            BeatAction::of(&waiting),
            [BeatAction::Enable, BeatAction::Edit, BeatAction::Delete]
        );
        assert_eq!(BeatAction::Enable.label(&waiting), ("e", "approve"));
        let mut empty = heartbeat("c", false);
        empty.enabled = false;
        empty.prompt.clear();
        assert_eq!(
            BeatAction::of(&empty),
            [BeatAction::Edit, BeatAction::Delete]
        );
    }

    #[test]
    fn the_setup_prompt_is_one_line_naming_the_heartbeat() {
        let mut empty = heartbeat("Heartbeat \"2\"", false);
        empty.id = HeartbeatId::from_string("01ABC");
        empty.prompt.clear();
        let text = setup_prompt(&empty);
        assert!(text.contains("It's empty"), "{text}");
        let mut begun = empty.clone();
        begun.prompt = "Run the tests.\nFix them.".into();
        let begun = setup_prompt(&begun);
        assert!(begun.contains("heartbeat show hb_01ABC"), "{begun}");
        assert!(!begun.chars().any(char::is_control), "{begun}");
        assert!(!text.chars().any(char::is_control), "{text}");
        assert!(text.contains("hb_01ABC (\"Heartbeat 2\")"), "{text}");
        assert!(
            text.contains("heartbeat update hb_01ABC --prompt"),
            "{text}"
        );
    }
}
