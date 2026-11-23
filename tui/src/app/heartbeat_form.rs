//! The New heartbeat form (`AppMode::NewHeartbeat`; Beats' `n` and
//! `+ New heartbeat`): its name, task, schedule, folder and agent. `Create`
//! makes it, on its schedule, and Beats shows it whole; `Ask agent` makes it
//! paused and opens a session whose agent is asked to fill it in.
//!
//! `Tab`, `Enter` and `↑`/`↓` go through the fields (in a task of several
//! rows `↑`/`↓` go through those first; Alt-Enter or Ctrl-J starts a new
//! line), `←`/`→` choose, Space ticks a day, Esc closes.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use session_core::api::{ApiError, CreateSession, ErrorCode, FirstInput};
use session_core::heartbeat::{Heartbeat, HeartbeatFields, HeartbeatSpec, Schedule, Weekday};
use session_core::model::ScopeRoot;

use super::folders::compare_folders;
use super::heartbeats::setup_prompt;
use super::runtime::AppEvent;
use super::types::AppMode;
use super::{env_snapshot, App, Hit, TextInput};
use crate::components::heartbeat_form::task_cols;

/// The agents a heartbeat can run, as Rakumux names them.
const AGENTS: [&str; 11] = [
    "claude",
    "codex",
    "opencode",
    "copilot",
    "cursor-agent",
    "pi",
    "grok",
    "kimi",
    "droid",
    "amp",
    "aider",
];

/// What the form's keys are on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FormField {
    Name,
    Task,
    /// How it repeats: every so often, daily, on days.
    Runs,
    /// The interval, or the time of day.
    When,
    /// The days it runs on (only [`Repeat::Days`]).
    Days,
    Folder,
    Agent,
    Create,
    AskAgent,
}

/// How a heartbeat repeats.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Repeat {
    Every,
    Daily,
    Days,
}

impl Repeat {
    pub const ALL: [Repeat; 3] = [Repeat::Every, Repeat::Daily, Repeat::Days];

    pub fn label(self) -> &'static str {
        match self {
            Repeat::Every => "Every",
            Repeat::Daily => "Daily",
            Repeat::Days => "Days",
        }
    }
}

/// A click target of the form.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FormHit {
    Field(FormField),
    Repeat(Repeat),
    /// A day (its place in [`Weekday::ALL`]).
    Day(usize),
    /// The `‹` or `›` of a field: its choice before or after.
    Step(FormField, isize),
    Cancel,
    /// Anywhere else on it.
    Inside,
}

/// The form as it's filled in.
#[derive(Debug, Clone)]
pub struct HeartbeatForm {
    pub name: TextInput,
    pub task: TextInput,
    pub repeat: Repeat,
    /// The interval ([`Repeat::Every`]).
    pub every: TextInput,
    /// The time of day (the others).
    pub at: TextInput,
    /// Ticked, Monday first.
    pub days: [bool; 7],
    /// The day Space ticks.
    pub day: usize,
    pub folders: Vec<ScopeRoot>,
    pub folder: usize,
    pub agents: Vec<String>,
    pub agent: usize,
    pub field: FormField,
    pub error: Option<String>,
    /// Being made.
    pub busy: bool,
}

impl HeartbeatForm {
    fn new(folders: Vec<ScopeRoot>, folder: usize, agents: Vec<String>) -> Self {
        Self {
            name: TextInput::default(),
            task: TextInput::multiline(),
            repeat: Repeat::Every,
            every: TextInput::new("1h"),
            at: TextInput::new("09:00"),
            days: [true, true, true, true, true, false, false],
            day: 0,
            folders,
            folder,
            agents,
            agent: 0,
            field: FormField::Name,
            error: None,
            busy: false,
        }
    }

    /// The fields the keys go through, in order.
    fn fields(&self) -> Vec<FormField> {
        let mut fields = vec![
            FormField::Name,
            FormField::Task,
            FormField::Runs,
            FormField::When,
        ];
        if self.repeat == Repeat::Days {
            fields.push(FormField::Days);
        }
        fields.extend([
            FormField::Folder,
            FormField::Agent,
            FormField::Create,
            FormField::AskAgent,
        ]);
        fields
    }

    fn step_field(&mut self, delta: isize) {
        let fields = self.fields();
        let at = fields.iter().position(|f| *f == self.field).unwrap_or(0) as isize;
        let at = (at + delta).rem_euclid(fields.len() as isize);
        self.field = fields[at as usize];
    }

    /// `←`/`→` on a field with choices.
    fn step_choice(&mut self, field: FormField, delta: isize) {
        let step =
            |at: usize, len: usize| (at as isize + delta).rem_euclid(len.max(1) as isize) as usize;
        match field {
            FormField::Runs => {
                let at = Repeat::ALL
                    .iter()
                    .position(|r| *r == self.repeat)
                    .unwrap_or(0);
                self.repeat = Repeat::ALL[step(at, Repeat::ALL.len())];
            }
            FormField::Days => self.day = step(self.day, self.days.len()),
            FormField::Folder => self.folder = step(self.folder, self.folders.len()),
            FormField::Agent => self.agent = step(self.agent, self.agents.len()),
            FormField::Create => self.field = FormField::AskAgent,
            FormField::AskAgent => self.field = FormField::Create,
            _ => {}
        }
        self.error = None;
    }

    fn toggle_day(&mut self, day: usize) {
        if let Some(on) = self.days.get_mut(day) {
            *on = !*on;
            self.day = day;
            self.error = None;
        }
    }

    /// The text the interval or time is typed in.
    pub fn when(&self) -> &TextInput {
        match self.repeat {
            Repeat::Every => &self.every,
            _ => &self.at,
        }
    }

    /// The text field the keys type in.
    fn text_mut(&mut self) -> Option<&mut TextInput> {
        match self.field {
            FormField::Name => Some(&mut self.name),
            FormField::Task => Some(&mut self.task),
            FormField::When => Some(match self.repeat {
                Repeat::Every => &mut self.every,
                _ => &mut self.at,
            }),
            _ => None,
        }
    }

    /// A paste: the task keeps its lines, the others run on as one.
    pub fn paste(&mut self, text: &str) {
        if self.busy {
            return;
        }
        let one_line = self.field != FormField::Task;
        if let Some(input) = self.text_mut() {
            match one_line {
                true => input.insert_str(&text.replace(['\r', '\n'], " ")),
                false => input.insert_str(text),
            }
            self.error = None;
        }
    }

    /// The schedule filled in, or what's wrong with it.
    pub fn schedule(&self) -> Result<Schedule, String> {
        let at = || self.at.value().trim().to_string();
        let schedule = match self.repeat {
            Repeat::Every => Schedule::every(self.every.value())
                .ok_or_else(|| "Write how often as 30m, 2h or 1d".to_string())?,
            Repeat::Daily => Schedule::Daily { at: at() },
            Repeat::Days => Schedule::Weekdays {
                days: Weekday::ALL
                    .into_iter()
                    .zip(self.days)
                    .filter_map(|(day, on)| on.then_some(day))
                    .collect(),
                at: at(),
            },
        };
        if self.repeat == Repeat::Days && !self.days.contains(&true) {
            return Err("Pick a day".into());
        }
        match schedule.problem() {
            Some(_) if schedule.time_of_day().is_none() && self.repeat != Repeat::Every => {
                Err("Write the time as HH:MM, 24-hour".into())
            }
            Some(problem) => Err(capitalized(&problem)),
            None => Ok(schedule),
        }
    }

    /// What it makes: complete and on (`Create`), or for its agent to fill
    /// in (`setup`); else the field that's wrong and why.
    fn spec(&self, setup: bool) -> Result<HeartbeatSpec, (FormField, String)> {
        let task = self.task.value().trim();
        if task.is_empty() && !setup {
            return Err((FormField::Task, "Write the task".into()));
        }
        // With no task, the agent asks for the schedule too.
        let schedule = match (setup, task.is_empty()) {
            (true, true) => None,
            _ => Some(self.schedule().map_err(|error| (FormField::When, error))?),
        };
        let name = self.name.value().trim();
        Ok(HeartbeatSpec {
            scope: self.folders.get(self.folder).cloned(),
            name: (!name.is_empty()).then(|| name.to_string()),
            fields: HeartbeatFields {
                agent: self.agents.get(self.agent).cloned(),
                command: None,
                prompt: (!task.is_empty()).then(|| task.to_string()),
                schedule,
                by_agent: false,
            },
            enabled: !setup,
        })
    }
}

fn capitalized(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

impl App {
    /// `n` on Beats, or its `+ New heartbeat`: the form, in the current
    /// folder with the agent last used there.
    pub fn new_heartbeat(&mut self) {
        if self.creating_session {
            return;
        }
        if self.api().is_none() {
            self.set_status("Not connected to the daemon.");
            return;
        }
        if let Some(error) = self.beats.error.clone() {
            self.set_status(error);
            return;
        }
        let current = self.current_folder();
        let mut folders: Vec<ScopeRoot> =
            self.folders.known.iter().map(|f| f.path.clone()).collect();
        for folder in [&self.folders.launch, &current] {
            if !folders.contains(folder) {
                folders.push(folder.clone());
            }
        }
        folders.sort_by(|a, b| compare_folders(&self.folders.launch, a, b));
        let folder = folders.iter().position(|f| *f == current).unwrap_or(0);
        let agents = self.heartbeat_agents(&current);
        self.mode = AppMode::NewHeartbeat(HeartbeatForm::new(folders, folder, agents));
    }

    /// The agents, those used most recently first: in `folder`, then
    /// anywhere.
    fn heartbeat_agents(&self, folder: &ScopeRoot) -> Vec<String> {
        let mut sessions: Vec<_> = self
            .list
            .sessions()
            .iter()
            .filter(|s| s.heartbeat.is_none())
            .collect();
        sessions.sort_by_key(|s| (&s.scope != folder, std::cmp::Reverse(s.last_active_at)));
        let mut agents: Vec<String> = Vec::new();
        let used = sessions
            .into_iter()
            .filter_map(|s| s.agent.as_ref().map(|agent| agent.kind.as_str()))
            .filter(|kind| AGENTS.contains(kind));
        for agent in used.chain(AGENTS) {
            if !agents.iter().any(|known| known == agent) {
                agents.push(agent.to_string());
            }
        }
        agents
    }

    /// Keys while the form is open.
    pub(super) fn heartbeat_form_key(&mut self, key: KeyEvent) {
        let cols = task_cols(self.area.width);
        let AppMode::NewHeartbeat(form) = &mut self.mode else {
            return;
        };
        if form.busy {
            return;
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        match (key.code, form.field) {
            (KeyCode::Esc, _) => self.mode = AppMode::Normal,
            (KeyCode::Enter, FormField::Task) if alt || shift => form.task.newline(),
            (KeyCode::Char('j'), FormField::Task) if ctrl => form.task.newline(),
            (KeyCode::Enter, FormField::Create) => self.submit_heartbeat(false),
            (KeyCode::Enter, FormField::AskAgent) => self.submit_heartbeat(true),
            (KeyCode::Up, FormField::Task) if form.task.move_rows(-1, cols) => {}
            (KeyCode::Down, FormField::Task) if form.task.move_rows(1, cols) => {}
            (KeyCode::Tab | KeyCode::Enter | KeyCode::Down, _) => form.step_field(1),
            (KeyCode::BackTab | KeyCode::Up, _) => form.step_field(-1),
            (KeyCode::Char(' '), FormField::Days) => form.toggle_day(form.day),
            (KeyCode::Left, field) if form.text_mut().is_none() => form.step_choice(field, -1),
            (KeyCode::Right, field) if form.text_mut().is_none() => form.step_choice(field, 1),
            _ => {
                if let Some(input) = form.text_mut() {
                    if input.handle_key(&key) {
                        form.error = None;
                    }
                }
            }
        }
    }

    /// The mouse while the form is open. A click off it leaves it open: what
    /// was written isn't lost.
    pub(super) fn heartbeat_form_mouse(&mut self, event: MouseEvent) {
        if event.kind != MouseEventKind::Down(MouseButton::Left) {
            return;
        }
        let hit = self.hits.iter().find_map(|(area, hit)| match hit {
            Hit::HeartbeatForm(hit)
                if event.column >= area.x
                    && event.column < area.right()
                    && event.row >= area.y
                    && event.row < area.bottom() =>
            {
                Some(*hit)
            }
            _ => None,
        });
        let AppMode::NewHeartbeat(form) = &mut self.mode else {
            return;
        };
        if form.busy {
            return;
        }
        match hit {
            Some(FormHit::Field(FormField::Create)) => {
                form.field = FormField::Create;
                self.submit_heartbeat(false);
            }
            Some(FormHit::Field(FormField::AskAgent)) => {
                form.field = FormField::AskAgent;
                self.submit_heartbeat(true);
            }
            Some(FormHit::Field(field)) => form.field = field,
            Some(FormHit::Repeat(repeat)) => {
                form.repeat = repeat;
                form.field = FormField::Runs;
                form.error = None;
            }
            Some(FormHit::Day(day)) => {
                form.field = FormField::Days;
                form.toggle_day(day);
            }
            Some(FormHit::Step(field, delta)) => {
                form.field = field;
                form.step_choice(field, delta);
            }
            Some(FormHit::Cancel) => self.mode = AppMode::Normal,
            Some(FormHit::Inside) | None => {}
        }
    }

    /// `Create`, or `Ask agent` (`setup`).
    fn submit_heartbeat(&mut self, setup: bool) {
        let api = self.api();
        let AppMode::NewHeartbeat(form) = &mut self.mode else {
            return;
        };
        let spec = match form.spec(setup) {
            Ok(spec) => spec,
            Err((field, error)) => {
                form.field = field;
                form.error = Some(error);
                return;
            }
        };
        let Some(api) = api else {
            form.error = Some("Not connected to the daemon.".into());
            return;
        };
        form.busy = true;
        form.error = None;
        self.exec.job(move || AppEvent::HeartbeatMade {
            setup,
            result: api.heartbeat_create(spec),
        });
    }

    /// The heartbeat the form asked for: Beats shows it whole, or (`setup`)
    /// its agent's session is started.
    pub(super) fn on_heartbeat_made(&mut self, setup: bool, result: Result<Heartbeat, ApiError>) {
        let heartbeat = match result {
            Ok(heartbeat) => heartbeat,
            Err(error) => {
                let message = match error.code {
                    ErrorCode::Unsupported => {
                        "The running Rakumux server is older than this app: restart it (S).".into()
                    }
                    _ => capitalized(&error.message),
                };
                match &mut self.mode {
                    AppMode::NewHeartbeat(form) => {
                        form.busy = false;
                        form.error = Some(message);
                    }
                    _ => self.set_status(message),
                }
                return;
            }
        };
        if matches!(self.mode, AppMode::NewHeartbeat(_)) {
            self.mode = AppMode::Normal;
        }
        self.beats_take_changed(&heartbeat);
        if setup {
            self.set_up_heartbeat(heartbeat);
            return;
        }
        self.beats.show(&heartbeat.id);
        self.set_status(format!("Made {}: it runs on its schedule", heartbeat.name));
        self.reread_heartbeats();
    }

    /// A session in the heartbeat's folder running its command (the agent's,
    /// with the flags it was last started with there), whose agent is asked,
    /// once its prompt shows, to fill it in: typed, not sent. The new session
    /// is entered as `n` in the list does.
    fn set_up_heartbeat(&mut self, heartbeat: Heartbeat) {
        let Some(api) = self.api() else {
            self.set_status("Not connected to the daemon.");
            return;
        };
        let (cols, rows) = self.terminal_size();
        let request = CreateSession {
            scope: Some(heartbeat.scope.clone()),
            name: None,
            cwd: Some(heartbeat.scope.path().to_path_buf()),
            argv: None,
            startup_cmds: vec![heartbeat.command.clone()],
            env: env_snapshot(),
            cols,
            rows,
            machine: None,
            prefill: None,
            first_input: Some(FirstInput {
                text: setup_prompt(&heartbeat),
                submit: false,
            }),
            workspace: self.new_session_workspace(),
        };
        self.creating_session = true;
        self.drawer_focused = false;
        self.beats.reading = None;
        self.set_status(format!("Made {}: asking its agent", heartbeat.name));
        self.exec.job(move || AppEvent::Created {
            result: api.create(request),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn form() -> HeartbeatForm {
        HeartbeatForm::new(
            vec![ScopeRoot::from_canonical("/w".into())],
            0,
            vec!["claude".into()],
        )
    }

    #[test]
    fn a_form_makes_its_schedule_or_says_what_is_wrong() {
        let mut form = form();
        assert_eq!(form.schedule(), Ok(Schedule::Every { minutes: 60 }));
        form.every.set("soon");
        assert!(form.schedule().unwrap_err().contains("30m, 2h or 1d"));
        form.every.set("1m");
        assert!(form.schedule().unwrap_err().starts_with("A heartbeat runs"));
        form.repeat = Repeat::Daily;
        assert_eq!(form.schedule(), Ok(Schedule::Daily { at: "09:00".into() }));
        form.at.set("9am");
        assert!(form.schedule().unwrap_err().contains("HH:MM"));
        form.at.set("18:30");
        form.repeat = Repeat::Days;
        assert_eq!(
            form.schedule().unwrap().to_string(),
            "mon,tue,wed,thu,fri 18:30"
        );
        form.days = [false; 7];
        assert_eq!(form.schedule(), Err("Pick a day".into()));
    }

    #[test]
    fn create_needs_a_task_and_asking_the_agent_does_not() {
        let mut form = form();
        assert_eq!(form.spec(false).unwrap_err().0, FormField::Task);
        let empty = form.spec(true).unwrap();
        assert!(!empty.enabled && empty.name.is_none());
        assert_eq!((empty.fields.prompt, empty.fields.schedule), (None, None));
        form.task.insert_str("Run the tests.\nFix them.\n");
        form.name.set(" nightly ");
        let spec = form.spec(false).unwrap();
        assert!(spec.enabled && !spec.fields.by_agent);
        assert_eq!(spec.name.as_deref(), Some("nightly"));
        assert_eq!(
            spec.fields.prompt.as_deref(),
            Some("Run the tests.\nFix them.")
        );
        assert_eq!(spec.fields.agent.as_deref(), Some("claude"));
        let asked = form.spec(true).unwrap();
        assert!(!asked.enabled && asked.fields.schedule.is_some());
        form.every.set("x");
        assert_eq!(form.spec(false).unwrap_err().0, FormField::When);
    }

    #[test]
    fn days_are_a_field_only_while_it_runs_on_days() {
        let mut form = form();
        assert!(!form.fields().contains(&FormField::Days));
        form.field = FormField::Runs;
        form.step_choice(FormField::Runs, -1);
        assert_eq!(form.repeat, Repeat::Days);
        assert!(form.fields().contains(&FormField::Days));
        form.field = FormField::AskAgent;
        form.step_field(1);
        assert_eq!(form.field, FormField::Name);
    }
}
