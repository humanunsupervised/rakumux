//! The New heartbeat form (`AppMode::NewHeartbeat`; Heartbeats' `n` and
//! `+ New heartbeat`): its name, schedule and folder, and the command each
//! run is: the agent, its arguments and the prompt. `Create` makes it, on its
//! schedule, and Heartbeats shows it whole; `Ask agent` makes it for its
//! agent (`by_agent`), paused while the agent sets it up, and opens a session
//! whose agent is asked to fill it in: its update saving the prompt and
//! schedule turns it on.
//!
//! Heartbeats' `c` opens it on a heartbeat (`Editing`): `Save` changes what
//! was changed in it, and nothing else, so what an agent changed meanwhile
//! stays. Its folder can't change.
//!
//! Ctrl-S saves (or creates) from any field. `Tab`, `Enter` and `↑`/`↓` go through the fields (in a prompt of several
//! rows `↑`/`↓` go through those first; Alt-Enter or Ctrl-J starts a new
//! line), `←`/`→` choose, Space ticks a day, Esc closes.
//!
//! The prompt grows with its text while the screen has room, then scrolls:
//! the cursor stays in view as it types or moves (`↑`/`↓`, PgUp/PgDn a
//! page, Home/End the start and end of the text), and the wheel over it
//! scrolls it without moving the cursor ([`PromptView`]).
//!
//! Each agent has arguments of its own, at first those with which it doesn't
//! stop to ask (`AgentCommand::unattended`). On the agent or its arguments
//! the form lists what the agent takes (`argument_suggest.rs`); typing
//! narrows it, `↑`/`↓` pick, Tab or Enter (or a click) takes one, Esc leaves
//! what's typed as it is.

use std::cell::Cell;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use session_core::api::{ApiError, CreateSession, ErrorCode, FirstInput};
use session_core::commands::{self, AgentCommand};
use session_core::heartbeat::{
    Heartbeat, HeartbeatEdit, HeartbeatFields, HeartbeatSpec, Schedule, Weekday,
};
use session_core::model::ScopeRoot;

use super::argument_suggest::{suggest, Suggestions};
use super::folders::compare_folders;
use super::heartbeats::setup_prompt;
use super::runtime::AppEvent;
use super::sessions::WHEEL_ROWS;
use super::types::AppMode;
use super::{env_snapshot, App, Hit, TextInput};
use crate::components::heartbeat_form::prompt_cols;

/// What the form's keys are on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FormField {
    Name,
    /// How it repeats: every so often, daily, on days.
    Runs,
    /// The interval, or the time of day.
    When,
    /// The days it runs on (only [`Repeat::Days`]).
    Days,
    Folder,
    Agent,
    Arguments,
    Prompt,
    Create,
    AskAgent,
}

/// What the form was sent for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Submit {
    Create,
    /// Made paused, for a session's agent to fill in.
    AskAgent,
    /// A heartbeat's changes.
    Save,
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
    /// A row of the suggested arguments.
    Suggestion(usize),
    Cancel,
    /// Anywhere else on it.
    Inside,
}

/// An agent and the arguments written for it.
#[derive(Debug, Clone)]
pub struct AgentChoice {
    /// What starts it: the agent's own program, or as an edited heartbeat's
    /// command has it (`FOO=1 ~/bin/claude`, `my-agent`).
    pub program: String,
    /// The agent, when Rakumux knows it.
    pub command: Option<&'static AgentCommand>,
    pub arguments: TextInput,
}

impl AgentChoice {
    fn new(command: &'static AgentCommand) -> Self {
        Self {
            program: command.program().to_string(),
            command: Some(command),
            arguments: TextInput::new(command.unattended),
        }
    }

    /// The choice that makes `line`: what comes before its arguments is its
    /// first word and the environment assignments before that.
    fn of(line: &str) -> Self {
        let line = line.trim();
        let mut end = 0;
        for word in line.split_whitespace() {
            end += line[end..].find(word).unwrap_or(0) + word.len();
            if !word.contains('=') {
                break;
            }
        }
        let (program, arguments) = line.split_at(end);
        Self {
            program: program.to_string(),
            command: commands::agent_of(program),
            arguments: TextInput::new(arguments.trim_start()),
        }
    }

    /// The agent's command line, its prompt left out.
    pub fn line(&self) -> String {
        let program = &self.program;
        match self.arguments.value().trim() {
            "" => program.clone(),
            arguments => format!("{program} {arguments}"),
        }
    }
}

/// The prompt's rows on screen, kept by the drawing: the first one, and how
/// many show. It follows the cursor, unless the wheel moved it since the
/// last key.
///
/// ```text
/// 200 rows, 8 on screen, the cursor on row 150: top 143, rows 8
/// ```
#[derive(Debug, Clone, Default)]
pub struct PromptView {
    pub top: Cell<usize>,
    pub rows: Cell<usize>,
    /// The columns its text is wrapped to.
    pub cols: Cell<usize>,
    pub wheeled: Cell<bool>,
}

/// The heartbeat being edited, with what the form showed of it at first.
#[derive(Debug, Clone)]
pub struct Editing {
    pub heartbeat: Heartbeat,
    schedule: Result<Schedule, String>,
    line: String,
    prompt: String,
}

/// The form as it's filled in.
#[derive(Debug, Clone)]
pub struct HeartbeatForm {
    pub name: TextInput,
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
    pub agents: Vec<AgentChoice>,
    pub agent: usize,
    /// The suggested argument Tab and Enter take.
    pub pick: Option<usize>,
    pub prompt: TextInput,
    pub prompt_view: PromptView,
    pub field: FormField,
    pub error: Option<String>,
    /// Being made.
    pub busy: bool,
    /// The heartbeat it changes; None makes a new one.
    pub editing: Option<Editing>,
}

impl HeartbeatForm {
    fn new(folders: Vec<ScopeRoot>, folder: usize, agents: Vec<AgentChoice>) -> Self {
        Self {
            name: TextInput::default(),
            repeat: Repeat::Every,
            every: TextInput::new("1h"),
            at: TextInput::new("09:00"),
            days: [true, true, true, true, true, false, false],
            day: 0,
            folders,
            folder,
            agents,
            agent: 0,
            pick: None,
            prompt: TextInput::multiline(),
            prompt_view: PromptView::default(),
            field: FormField::Name,
            error: None,
            busy: false,
            editing: None,
        }
    }

    /// The form on `heartbeat`, `agents` its other choices.
    fn edit(heartbeat: Heartbeat, agents: Vec<AgentChoice>) -> Self {
        let mut form = Self::new(vec![heartbeat.scope.clone()], 0, agents);
        form.name.set(heartbeat.name.clone());
        form.prompt.insert_str(&heartbeat.prompt);
        if !heartbeat.command.trim().is_empty() {
            let own = AgentChoice::of(&heartbeat.command);
            let same = |choice: &AgentChoice| choice.program == own.program;
            match form.agents.iter().position(same) {
                Some(at) => {
                    form.agent = at;
                    form.agents[at] = own;
                }
                None => form.agents.insert(0, own),
            }
        }
        match &heartbeat.schedule {
            Some(every @ Schedule::Every { .. }) => {
                let text = every.to_string();
                form.every.set(text.trim_start_matches("every "));
            }
            Some(Schedule::Daily { at }) => {
                form.repeat = Repeat::Daily;
                form.at.set(at.clone());
            }
            Some(Schedule::Weekdays { days, at }) => {
                form.repeat = Repeat::Days;
                form.days = Weekday::ALL.map(|day| days.contains(&day));
                form.at.set(at.clone());
            }
            Some(Schedule::Unknown) | None => {}
        }
        form.editing = Some(Editing {
            schedule: form.schedule(),
            line: form.choice().map(AgentChoice::line).unwrap_or_default(),
            prompt: form.prompt.value().to_string(),
            heartbeat,
        });
        form
    }

    /// The fields the keys go through, in order.
    fn fields(&self) -> Vec<FormField> {
        let mut fields = vec![FormField::Name, FormField::Runs, FormField::When];
        if self.repeat == Repeat::Days {
            fields.push(FormField::Days);
        }
        if self.editing.is_none() {
            fields.push(FormField::Folder);
        }
        fields.extend([
            FormField::Agent,
            FormField::Arguments,
            FormField::Prompt,
            FormField::Create,
        ]);
        if self.editing.is_none() {
            fields.push(FormField::AskAgent);
        }
        fields
    }

    /// The agent chosen, with its arguments.
    pub fn choice(&self) -> Option<&AgentChoice> {
        self.agents.get(self.agent)
    }

    /// What the agent takes, listed while the keys are on it or on its
    /// arguments.
    pub fn suggestions(&self) -> Option<Suggestions> {
        let choice = self.choice()?;
        let command = choice.command?;
        match self.field {
            FormField::Agent => Some(suggest(command, " ", 1)),
            FormField::Arguments => Some(suggest(
                command,
                choice.arguments.value(),
                choice.arguments.cursor(),
            )),
            _ => None,
        }
    }

    /// What's typed changed, or the keys came to it: the suggestion it means
    /// is the one taken.
    fn suggest_again(&mut self) {
        self.pick = match self.field {
            FormField::Arguments => self.suggestions().and_then(|found| found.best),
            _ => None,
        };
    }

    /// `↑`/`↓` in the suggestions; `false` when there's nothing there to go to.
    fn step_pick(&mut self, delta: isize) -> bool {
        let Some(found) = self.suggestions() else {
            return false;
        };
        if self.field != FormField::Arguments || !found.rows.iter().any(|row| row.takes) {
            return false;
        }
        self.pick = match (self.pick, delta) {
            (None, 1..) => Some(0),
            (None, _) => return false,
            (Some(0), ..=-1) => None,
            (Some(at), delta) => Some(at.saturating_add_signed(delta).min(found.rows.len() - 1)),
        };
        true
    }

    /// Write the suggested argument `index` in place of the word it's for.
    fn take_suggestion(&mut self, index: usize) {
        let Some(found) = self.suggestions() else {
            return;
        };
        let Some(row) = found.rows.get(index).filter(|row| row.takes).copied() else {
            return;
        };
        // On the agent, it goes after what its arguments say.
        let word = match self.field {
            FormField::Arguments => Some(found.word),
            _ => None,
        };
        self.field = FormField::Arguments;
        if let Some(choice) = self.agents.get_mut(self.agent) {
            let input = &mut choice.arguments;
            match word {
                Some(word) => {
                    // One space after it, the one there already if any.
                    let spaced = input.value()[word.end..].starts_with(' ');
                    let end = word.end + usize::from(spaced);
                    let joins = input.value()[..word.start]
                        .chars()
                        .next_back()
                        .is_some_and(|c| !c.is_whitespace());
                    let text = format!("{}{} ", if joins { " " } else { "" }, row.text);
                    input.replace(word.start..end, &text);
                }
                None => {
                    let line = format!("{} {} ", input.value().trim_end(), row.text);
                    input.set(line.trim_start());
                }
            }
        }
        self.error = None;
        self.suggest_again();
    }

    /// Move the keys to `field`.
    fn focus(&mut self, field: FormField) {
        self.field = field;
        self.suggest_again();
    }

    fn step_field(&mut self, delta: isize) {
        let fields = self.fields();
        let at = fields.iter().position(|f| *f == self.field).unwrap_or(0) as isize;
        let at = (at + delta).rem_euclid(fields.len() as isize);
        self.focus(fields[at as usize]);
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
            FormField::Create if self.editing.is_none() => self.field = FormField::AskAgent,
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
            FormField::Arguments => {
                let choice = self.agents.get_mut(self.agent)?;
                Some(&mut choice.arguments)
            }
            FormField::Prompt => Some(&mut self.prompt),
            FormField::When => Some(match self.repeat {
                Repeat::Every => &mut self.every,
                _ => &mut self.at,
            }),
            _ => None,
        }
    }

    /// A paste: the prompt keeps its lines, the others run on as one.
    pub fn paste(&mut self, text: &str) {
        if self.busy {
            return;
        }
        self.prompt_view.wheeled.set(false);
        let one_line = self.field != FormField::Prompt;
        if let Some(input) = self.text_mut() {
            match one_line {
                true => input.insert_str(&text.replace(['\r', '\n'], " ")),
                false => input.insert_str(text),
            }
            self.error = None;
            self.suggest_again();
        }
    }

    /// PgUp/PgDn in the prompt: its view and the cursor a page up or down
    /// (`pages` -1 or 1): the rows shown but one, the cursor to the first or
    /// last row at most (its column kept).
    fn page_prompt(&mut self, pages: isize) {
        let cols = self.prompt_view.cols.get();
        let page = self.prompt_view.rows.get().saturating_sub(1).max(1) as isize;
        let last = self.prompt.rows(cols).len().saturating_sub(1);
        let (row, _) = self.prompt.cursor_at(cols);
        let target = row.saturating_add_signed(pages * page).min(last);
        self.prompt.move_rows(target as isize - row as isize, cols);
        let view = &self.prompt_view;
        view.top
            .set(view.top.get().saturating_add_signed(pages * page));
    }

    /// The wheel over the prompt: its rows `delta` up or down; the cursor
    /// stays where it is, out of view if need be, until a key.
    fn scroll_prompt(&mut self, delta: isize, cols: usize) {
        let total = self.prompt.rows(cols).len();
        let view = &self.prompt_view;
        let max = total.saturating_sub(view.rows.get());
        view.top
            .set(view.top.get().saturating_add_signed(delta).min(max));
        view.wheeled.set(true);
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
    /// in (`setup`: made for the agent, it turns on once the agent saves its
    /// prompt and schedule); else the field that's wrong and why.
    fn spec(&self, setup: bool) -> Result<HeartbeatSpec, (FormField, String)> {
        let prompt = self.prompt.value().trim();
        // With no prompt, the agent asks for the schedule too.
        let schedule = match (setup, prompt.is_empty()) {
            (true, true) => None,
            _ => Some(self.schedule().map_err(|error| (FormField::When, error))?),
        };
        let command = self.choice().map(AgentChoice::line);
        if let Some(problem) = command.as_deref().and_then(commands::problem) {
            return Err((FormField::Arguments, format!("Arguments: {problem}")));
        }
        if prompt.is_empty() && !setup {
            return Err((FormField::Prompt, "Write the prompt".into()));
        }
        let name = self.name.value().trim();
        Ok(HeartbeatSpec {
            scope: self.folders.get(self.folder).cloned(),
            name: (!name.is_empty()).then(|| name.to_string()),
            fields: HeartbeatFields {
                agent: self.agent_label(),
                command,
                prompt: (!prompt.is_empty()).then(|| prompt.to_string()),
                schedule,
                by_agent: setup,
            },
            enabled: !setup,
        })
    }

    /// The agent chosen, as Rakumux names it.
    fn agent_label(&self) -> Option<String> {
        Some(self.choice()?.command?.label.to_string())
    }

    /// What's changed of `editing`; None when nothing is, else the field
    /// that's wrong and why.
    fn changes(&self, editing: &Editing) -> Result<Option<HeartbeatEdit>, (FormField, String)> {
        let heartbeat = &editing.heartbeat;
        let mut fields = HeartbeatFields::default();
        let name = self.name.value().trim();
        if name.is_empty() {
            return Err((FormField::Name, "Write its name".into()));
        }
        // One without a schedule gets the one shown.
        let schedule = self.schedule();
        if heartbeat.schedule.is_none() || schedule != editing.schedule {
            fields.schedule = Some(schedule.map_err(|error| (FormField::When, error))?);
        }
        let line = self.choice().map(AgentChoice::line).unwrap_or_default();
        if line != editing.line {
            // One the daemon types the prompt into may be more than one
            // command; it may stay so.
            if let (Some(problem), None) =
                (commands::problem(&line), commands::problem(&editing.line))
            {
                return Err((FormField::Arguments, format!("Arguments: {problem}")));
            }
            fields.agent = self.agent_label();
            fields.command = Some(line);
        }
        if self.prompt.value() != editing.prompt {
            let prompt = self.prompt.value().trim();
            if prompt.is_empty() {
                return Err((FormField::Prompt, "Write the prompt".into()));
            }
            fields.prompt = Some(prompt.to_string());
        }
        let name = (name != heartbeat.name).then(|| name.to_string());
        let changed = name.is_some() || fields.changes_runs();
        Ok(changed.then(|| HeartbeatEdit {
            id: heartbeat.id.clone(),
            name,
            fields,
        }))
    }
}

fn capitalized(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// What the form asks the daemon for.
enum Asked {
    Make(HeartbeatSpec),
    Edit(HeartbeatEdit),
}

impl App {
    /// `n` on Heartbeats, or its `+ New heartbeat`: the form, in the current
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

    /// `c` on Heartbeats: the form on `heartbeat`.
    pub(super) fn edit_heartbeat(&mut self, heartbeat: Heartbeat) {
        if self.api().is_none() {
            self.set_status("Not connected to the daemon.");
            return;
        }
        let agents = self.heartbeat_agents(&heartbeat.scope);
        self.mode = AppMode::NewHeartbeat(HeartbeatForm::edit(heartbeat, agents));
    }

    /// The agents, those used most recently first: in `folder`, then
    /// anywhere.
    fn heartbeat_agents(&self, folder: &ScopeRoot) -> Vec<AgentChoice> {
        let mut sessions: Vec<_> = self
            .list
            .sessions()
            .iter()
            .filter(|s| s.heartbeat.is_none())
            .collect();
        sessions.sort_by_key(|s| (&s.scope != folder, std::cmp::Reverse(s.last_active_at)));
        let mut agents: Vec<AgentChoice> = Vec::new();
        let used = sessions
            .into_iter()
            .filter_map(|s| commands::agent(&s.agent.as_ref()?.kind));
        for agent in used.chain(commands::AGENTS) {
            if !agents.iter().any(|known| known.command == Some(agent)) {
                agents.push(AgentChoice::new(agent));
            }
        }
        agents
    }

    /// Keys while the form is open.
    pub(super) fn heartbeat_form_key(&mut self, key: KeyEvent) {
        let cols = prompt_cols(self.area.width);
        let AppMode::NewHeartbeat(form) = &mut self.mode else {
            return;
        };
        if form.busy {
            return;
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        // A key: the prompt's view follows its cursor again.
        form.prompt_view.wheeled.set(false);
        form.prompt_view.cols.set(cols);
        match (key.code, form.field) {
            // Off the suggestion, then off the form.
            (KeyCode::Esc, _) if form.pick.is_some() => form.pick = None,
            (KeyCode::Esc, _) => self.mode = AppMode::Normal,
            (KeyCode::Enter, FormField::Prompt) if alt || shift => form.prompt.newline(),
            (KeyCode::Char('j'), FormField::Prompt) if ctrl => form.prompt.newline(),
            (KeyCode::Char('s'), _) if ctrl => self.submit_heartbeat(false),
            (KeyCode::Enter, FormField::Create) => self.submit_heartbeat(false),
            (KeyCode::Enter, FormField::AskAgent) => self.submit_heartbeat(true),
            (KeyCode::Up, FormField::Prompt) if form.prompt.move_rows(-1, cols) => {}
            (KeyCode::Down, FormField::Prompt) if form.prompt.move_rows(1, cols) => {}
            (KeyCode::PageUp, FormField::Prompt) => form.page_prompt(-1),
            (KeyCode::PageDown, FormField::Prompt) => form.page_prompt(1),
            (KeyCode::Up, FormField::Arguments) if form.step_pick(-1) => {}
            (KeyCode::Down, FormField::Arguments) if form.step_pick(1) => {}
            (KeyCode::Tab | KeyCode::Enter, FormField::Arguments) if form.pick.is_some() => {
                form.take_suggestion(form.pick.unwrap_or_default())
            }
            (KeyCode::Tab | KeyCode::Enter | KeyCode::Down, _) => form.step_field(1),
            (KeyCode::BackTab | KeyCode::Up, _) => form.step_field(-1),
            (KeyCode::Char(' '), FormField::Days) => form.toggle_day(form.day),
            (KeyCode::Left, field) if form.text_mut().is_none() => form.step_choice(field, -1),
            (KeyCode::Right, field) if form.text_mut().is_none() => form.step_choice(field, 1),
            _ => {
                if let Some(input) = form.text_mut() {
                    if input.handle_key(&key) {
                        form.error = None;
                        form.suggest_again();
                    }
                }
            }
        }
    }

    /// The mouse while the form is open. A click off it leaves it open: what
    /// was written isn't lost.
    pub(super) fn heartbeat_form_mouse(&mut self, event: MouseEvent) {
        let wheel = match event.kind {
            MouseEventKind::ScrollUp => Some(-WHEEL_ROWS as isize),
            MouseEventKind::ScrollDown => Some(WHEEL_ROWS as isize),
            MouseEventKind::Down(MouseButton::Left) => None,
            _ => return,
        };
        let cols = prompt_cols(self.area.width);
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
        // The wheel scrolls the prompt under it.
        if let Some(delta) = wheel {
            if hit == Some(FormHit::Field(FormField::Prompt)) {
                form.scroll_prompt(delta, cols);
            }
            return;
        }
        match hit {
            Some(FormHit::Field(FormField::Create)) => {
                form.focus(FormField::Create);
                self.submit_heartbeat(false);
            }
            Some(FormHit::Field(FormField::AskAgent)) => {
                form.focus(FormField::AskAgent);
                self.submit_heartbeat(true);
            }
            Some(FormHit::Field(field)) => form.focus(field),
            Some(FormHit::Suggestion(index)) => form.take_suggestion(index),
            Some(FormHit::Repeat(repeat)) => {
                form.repeat = repeat;
                form.focus(FormField::Runs);
                form.error = None;
            }
            Some(FormHit::Day(day)) => {
                form.focus(FormField::Days);
                form.toggle_day(day);
            }
            Some(FormHit::Step(field, delta)) => {
                form.focus(field);
                form.step_choice(field, delta);
            }
            Some(FormHit::Cancel) => self.mode = AppMode::Normal,
            Some(FormHit::Inside) | None => {}
        }
    }

    /// `Create` (`Save`), or `Ask agent` (`setup`).
    fn submit_heartbeat(&mut self, setup: bool) {
        let api = self.api();
        let AppMode::NewHeartbeat(form) = &mut self.mode else {
            return;
        };
        let asked = match &form.editing {
            Some(editing) => form.changes(editing).map(|edit| edit.map(Asked::Edit)),
            None => form.spec(setup).map(|spec| Some(Asked::Make(spec))),
        };
        let asked = match asked {
            Ok(Some(asked)) => asked,
            Ok(None) => {
                self.mode = AppMode::Normal;
                return;
            }
            Err((field, error)) => {
                form.focus(field);
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
        self.exec.job(move || match asked {
            Asked::Make(spec) => AppEvent::HeartbeatForm {
                submit: if setup {
                    Submit::AskAgent
                } else {
                    Submit::Create
                },
                result: api.heartbeat_create(spec),
            },
            Asked::Edit(edit) => AppEvent::HeartbeatForm {
                submit: Submit::Save,
                result: api.heartbeat_update(edit),
            },
        });
    }

    /// The heartbeat the form made or changed: Heartbeats shows it (a new one
    /// whole), or its agent's session is started.
    pub(super) fn on_heartbeat_form(
        &mut self,
        submit: Submit,
        result: Result<Heartbeat, ApiError>,
    ) {
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
        match submit {
            Submit::AskAgent => {
                self.set_up_heartbeat(heartbeat);
                return;
            }
            Submit::Create => {
                self.beats.show(&heartbeat.id);
                self.set_status(format!("Made {}: it runs on its schedule", heartbeat.name));
            }
            Submit::Save => self.set_status(format!("Saved {}", heartbeat.name)),
        }
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
            ["claude", "codex", "pi"]
                .map(|label| AgentChoice::new(commands::agent(label).unwrap()))
                .into(),
        )
    }

    fn arguments(form: &HeartbeatForm) -> &str {
        form.choice().unwrap().arguments.value()
    }

    fn write(form: &mut HeartbeatForm, text: &str) {
        form.text_mut().unwrap().insert_str(text);
        form.suggest_again();
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
    fn create_needs_a_prompt_and_asking_the_agent_does_not() {
        let mut form = form();
        assert_eq!(form.spec(false).unwrap_err().0, FormField::Prompt);
        let empty = form.spec(true).unwrap();
        // Made for its agent: on once the agent saves what it's missing.
        assert!(!empty.enabled && empty.fields.by_agent && empty.name.is_none());
        assert_eq!((empty.fields.prompt, empty.fields.schedule), (None, None));
        form.prompt.insert_str("Run the tests.\nFix them.\n");
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
        assert!(!asked.enabled && asked.fields.by_agent && asked.fields.schedule.is_some());
        form.every.set("x");
        assert_eq!(form.spec(false).unwrap_err().0, FormField::When);
    }

    #[test]
    fn each_agent_has_its_own_arguments_at_first_those_that_dont_stop_to_ask() {
        let mut form = form();
        form.prompt.insert_str("Run the tests.");
        let command = |form: &HeartbeatForm| form.spec(false).unwrap().fields.command.unwrap();
        assert_eq!(command(&form), "claude --dangerously-skip-permissions");
        form.focus(FormField::Arguments);
        write(&mut form, " --model opus");
        form.step_choice(FormField::Agent, 1);
        assert_eq!(
            command(&form),
            "codex --dangerously-bypass-approvals-and-sandbox"
        );
        form.step_choice(FormField::Agent, 1);
        assert_eq!(command(&form), "pi");
        form.step_choice(FormField::Agent, 1);
        assert_eq!(
            command(&form),
            "claude --dangerously-skip-permissions --model opus"
        );
        // What a shell would read as more than its arguments.
        write(&mut form, " 'x");
        let (field, error) = form.spec(false).unwrap_err();
        assert_eq!(field, FormField::Arguments);
        assert_eq!(error, "Arguments: a quote isn't closed");
    }

    fn heartbeat(command: &str, schedule: Option<Schedule>) -> Heartbeat {
        let json = serde_json::json!({
            "id": "hb_01", "scope": "/w", "name": "deps", "command": command,
            "prompt": "Update them.", "schedule": schedule, "enabled": true,
            "created_by": {"kind": "user"}, "created_at": 1, "updated_at": 1
        });
        serde_json::from_value(json).unwrap()
    }

    fn edit(command: &str, schedule: Option<Schedule>) -> HeartbeatForm {
        HeartbeatForm::edit(heartbeat(command, schedule), form().agents)
    }

    fn changes(form: &HeartbeatForm) -> Option<HeartbeatEdit> {
        form.changes(form.editing.as_ref().unwrap()).unwrap()
    }

    #[test]
    fn editing_changes_only_what_was_changed() {
        let every = |minutes| Some(Schedule::Every { minutes });
        for schedule in [
            Schedule::Every { minutes: 90 },
            Schedule::Every { minutes: 2880 },
            Schedule::Daily { at: "07:15".into() },
            Schedule::Weekdays {
                days: vec![Weekday::Tue, Weekday::Sun],
                at: "22:00".into(),
            },
        ] {
            let form = edit("claude --model opus", Some(schedule.clone()));
            assert_eq!(form.schedule(), Ok(schedule));
            assert_eq!(changes(&form), None);
        }
        // A command of any shape shows, and stays, as it is.
        for command in [
            "pi",
            "claude --x 'a b'",
            "FOO=1 ~/bin/claude --x",
            "opencode2 --auto",
            "my-agent --x",
            "cd web && claude",
        ] {
            let form = edit(command, every(60));
            assert_eq!(form.choice().unwrap().line(), command);
            assert_eq!(changes(&form), None, "{command}");
        }
        let mut form = edit("FOO=1 ~/bin/claude --x", every(60));
        form.focus(FormField::Arguments);
        assert!(form.suggestions().is_some());
        write(&mut form, " --verbose");
        let edit_of = changes(&form).unwrap();
        assert_eq!((edit_of.name, edit_of.fields.prompt), (None, None));
        assert_eq!(edit_of.fields.schedule, None);
        assert_eq!(edit_of.fields.agent.as_deref(), Some("claude"));
        assert_eq!(
            edit_of.fields.command.as_deref(),
            Some("FOO=1 ~/bin/claude --x --verbose")
        );
        assert!(!edit_of.fields.by_agent);
        // Another agent's, with its own arguments, and back.
        form.step_choice(FormField::Agent, 1);
        assert_eq!(
            changes(&form).unwrap().fields.command.as_deref(),
            Some("claude --dangerously-skip-permissions")
        );
        form.step_choice(FormField::Agent, -1);
        assert_eq!(arguments(&form), "--x --verbose");
        // One Rakumux doesn't know suggests nothing.
        let mut form = edit("my-agent --x", every(60));
        form.focus(FormField::Arguments);
        assert!(form.suggestions().is_none());
        write(&mut form, " --y");
        let fields = changes(&form).unwrap().fields;
        assert_eq!(
            (fields.agent, fields.command.as_deref()),
            (None, Some("my-agent --x --y"))
        );
        // Its schedule is the one shown once it has none; one from a newer
        // Rakumux stays until it's changed.
        assert_eq!(
            changes(&edit("pi", None)).unwrap().fields.schedule,
            every(60)
        );
        assert_eq!(changes(&edit("pi", Some(Schedule::Unknown))), None);
        // What can't be.
        let mut form = edit("pi", every(60));
        form.prompt.set("");
        let error = |form: &HeartbeatForm| form.changes(form.editing.as_ref().unwrap());
        assert_eq!(error(&form).unwrap_err().0, FormField::Prompt);
        form.name.set(" ");
        assert_eq!(error(&form).unwrap_err().0, FormField::Name);
        // Nor its folder, nor asking an agent.
        assert!(!form.fields().contains(&FormField::Folder));
        assert!(!form.fields().contains(&FormField::AskAgent));
        form.step_choice(FormField::Create, 1);
        assert_ne!(form.field, FormField::AskAgent);
    }

    #[test]
    fn suggestions_are_picked_and_taken() {
        let mut form = form();
        assert!(form.suggestions().is_none());
        // On the agent: what it takes, nothing picked, its keys its own.
        form.focus(FormField::Agent);
        assert!(form.suggestions().unwrap().rows.len() > 8);
        assert!(form.pick.is_none() && !form.step_pick(1));
        // A click there adds one.
        let at = |form: &HeartbeatForm, text: &str| {
            let found = form.suggestions().unwrap();
            found.rows.iter().position(|row| row.text == text).unwrap()
        };
        form.take_suggestion(at(&form, "--effort"));
        assert_eq!(form.field, FormField::Arguments);
        assert_eq!(arguments(&form), "--dangerously-skip-permissions --effort ");
        // Then its values: ↓ picks, ↑ from the first leaves them.
        assert!(form.pick.is_none());
        assert!(form.step_pick(1) && form.step_pick(1));
        assert_eq!(form.pick, Some(1));
        assert!(form.step_pick(-1) && form.step_pick(-1) && form.pick.is_none());
        assert!(!form.step_pick(-1));
        form.take_suggestion(at(&form, "high"));
        // After a whole word, it goes after it.
        form.agents[0].arguments.set("--chrome");
        form.take_suggestion(at(&form, "--verbose"));
        assert_eq!(arguments(&form), "--chrome --verbose ");
        form.agents[0]
            .arguments
            .set("--dangerously-skip-permissions --effort high ");
        // Half typed, the best is picked.
        write(&mut form, "--verb");
        assert_eq!(form.pick, Some(0));
        form.take_suggestion(0);
        assert_eq!(
            arguments(&form),
            "--dangerously-skip-permissions --effort high --verbose "
        );
        assert!(form.pick.is_none());
        // ↓ stops at the last.
        for _ in 0..100 {
            form.step_pick(1);
        }
        assert_eq!(form.pick, Some(form.suggestions().unwrap().rows.len() - 1));
        // A value of its own isn't one to take.
        write(&mut form, "--max-budget-usd 5");
        assert!(form.pick.is_none() && !form.step_pick(1));
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
