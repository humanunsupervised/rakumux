//! Heartbeats in the control layer: the `heartbeat_*` requests, the
//! scheduler that starts a run when one is due, and a new session's
//! [`FirstInput`], which gives a run's agent its task once its prompt shows.
//!
//! - **Approval.** What a session caller makes or changes (or anything that
//!   says `by_agent`, as `rakumux-mcp` does) waits for the user, paused:
//!   creating one, or changing what an enabled one runs, sets
//!   `awaiting_approval` and pauses it. Only the user enables it.
//! - **Runs.** At a due time the daemon starts `<name> #<n>` in the
//!   heartbeat's folder, as the user would: a login shell that types the
//!   heartbeat's command, then its prompt once the agent waits at its
//!   prompt. Earlier runs whose agent waits (idle, done, or back at its
//!   shell) are ended first, so their final screens stay and Enter resumes
//!   them; while one is at work or asking something (or still starting) the
//!   due time is skipped. The newest [`RUNS_KEPT`] runs stay listed; older
//!   ended ones go to the session history.
//! - **Missed due times** (the machine asleep, the daemon down) give one run
//!   when it's back, then the next due time is counted from then.

use super::{
    host_error, invalid, limit_exceeded, registry_of, validate_scope, Caller, Inner, State,
};
use crate::control::audit::{self, Decision};
use crate::control::names;
use crate::control::policy::{self, Op};
use crate::control::registry::RunRow;
use crate::control::schedule;
use crate::host::HostError;
use session_core::api::{
    ApiError, CallerInfo, CreateSession, ErrorCode, FirstInput, Response, FIRST_INPUT_TIMEOUT_MS,
    MAX_FIRST_INPUT_BYTES,
};
use session_core::heartbeat::{
    Heartbeat, HeartbeatEdit, HeartbeatFields, HeartbeatRun, HeartbeatRunRef, HeartbeatSpec,
    RunOutcome, MAX_COMMAND_BYTES, MAX_HEARTBEATS, MAX_PROMPT_BYTES, RUNS_KEPT,
};
use session_core::ids::{HeartbeatId, SessionId, TerminalId};
use session_core::model::{
    Creator, Lifecycle, ResumeSource, ScopeRoot, SessionStatus, SessionView,
};
use std::time::Duration;

/// A heartbeat named without one: `Heartbeat`, `Heartbeat 2`, …
const NEW_HEARTBEAT: &str = "Heartbeat";
/// The command of a heartbeat whose folder never ran an agent and names none.
const DEFAULT_AGENT: &str = "claude";
/// The agents a heartbeat's command is known to start, by the word that
/// starts them (`opencode2` is OpenCode's).
const AGENTS: &[&str] = &[
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
/// How long a first input waits after its agent's screen went quiet: an
/// agent whose prompt detection reads is at it once it shows; one whose
/// screen shows none (Cursor, Droid, Amp) reads idle as soon as it draws
/// anything, so it gets longer to finish starting.
const SETTLE_KNOWN_MS: i64 = 1_000;
const SETTLE_UNKNOWN_MS: i64 = 3_000;
/// Nobody typed in the session for this long before a first input goes in.
const FIRST_INPUT_QUIET: Duration = Duration::from_secs(2);

/// A new session's [`FirstInput`] while it waits for its agent's prompt.
pub(super) struct PendingInput {
    text: String,
    submit: bool,
    /// When the session started (Unix milliseconds).
    since: i64,
    /// Since when an agent has been in its foreground.
    agent_since: Option<i64>,
}

impl PendingInput {
    pub(super) fn new(input: FirstInput, now: i64) -> Self {
        Self {
            text: input.text,
            submit: input.submit,
            since: now,
            agent_since: None,
        }
    }
}

/// Check a [`FirstInput`] before the session starts.
pub(super) fn validate_first_input(input: &FirstInput) -> Result<(), ApiError> {
    if input.text.trim().is_empty() {
        return Err(invalid("first_input is empty"));
    }
    if input.text.len() > MAX_FIRST_INPUT_BYTES {
        return Err(invalid(format!(
            "first_input is longer than {MAX_FIRST_INPUT_BYTES} bytes"
        )));
    }
    let allowed = |c: char| input.submit && (c == '\n' || c == '\t');
    if input.text.chars().any(|c| c.is_control() && !allowed(c)) {
        return Err(invalid(
            "first_input may not contain control characters (a newline only when it's submitted)",
        ));
    }
    Ok(())
}

/// Why a due time started no run.
enum NotStarted {
    /// An earlier run is at work, asking something or starting.
    Skipped(String),
    Failed(ApiError),
}

/// The daemon acting for the user: runs it starts, earlier runs it ends.
fn daemon() -> Caller {
    Caller {
        info: CallerInfo::User,
        client: "rakumux-ptyd".into(),
        max_blocking_ms: None,
    }
}

impl Inner {
    // ------------------------------------------------------------ requests

    pub(super) fn heartbeat_list(
        &self,
        caller: &Caller,
        scope: Option<&ScopeRoot>,
    ) -> Result<Response, ApiError> {
        policy::authorize(caller, Op::HeartbeatList, None)?;
        let state = self.lock();
        let heartbeats = state
            .heartbeats
            .values()
            .filter(|heartbeat| scope.map_or(true, |scope| &heartbeat.scope == scope))
            .cloned()
            .collect();
        Ok(Response::Heartbeats { heartbeats })
    }

    pub(super) fn heartbeat_get(
        &self,
        caller: &Caller,
        id: &HeartbeatId,
    ) -> Result<Response, ApiError> {
        policy::authorize(caller, Op::HeartbeatGet, None)?;
        let state = self.lock();
        let heartbeat = state
            .heartbeats
            .get(id)
            .cloned()
            .ok_or_else(|| missing(id))?;
        Ok(Response::Heartbeat { heartbeat })
    }

    pub(super) fn heartbeat_create(
        &self,
        caller: &Caller,
        spec: HeartbeatSpec,
    ) -> Result<Response, ApiError> {
        let mut state = self.lock();
        let scope = spec.scope.clone();
        let result = self.create_heartbeat(&mut state, caller, spec);
        let scope = match &result {
            Ok(heartbeat) => Some(heartbeat.scope.clone()),
            Err(_) => scope.or_else(|| super::own_scope(caller).cloned()),
        };
        self.audit_heartbeat(&state, caller, Op::HeartbeatCreate, scope.as_ref(), &result);
        result.map(|heartbeat| Response::Heartbeat { heartbeat })
    }

    pub(super) fn heartbeat_update(
        &self,
        caller: &Caller,
        edit: HeartbeatEdit,
    ) -> Result<Response, ApiError> {
        let mut state = self.lock();
        let scope = state.heartbeats.get(&edit.id).map(|h| h.scope.clone());
        let result = self.update_heartbeat(&mut state, caller, edit);
        self.audit_heartbeat(&state, caller, Op::HeartbeatUpdate, scope.as_ref(), &result);
        result.map(|heartbeat| Response::Heartbeat { heartbeat })
    }

    pub(super) fn heartbeat_enable(
        &self,
        caller: &Caller,
        id: &HeartbeatId,
        enabled: bool,
    ) -> Result<Response, ApiError> {
        let mut state = self.lock();
        let scope = state.heartbeats.get(id).map(|h| h.scope.clone());
        let result = self.enable_heartbeat(&mut state, caller, id, enabled);
        self.audit_heartbeat(&state, caller, Op::HeartbeatEnable, scope.as_ref(), &result);
        result.map(|heartbeat| Response::Heartbeat { heartbeat })
    }

    pub(super) fn heartbeat_delete(
        &self,
        caller: &Caller,
        id: &HeartbeatId,
    ) -> Result<Response, ApiError> {
        let mut state = self.lock();
        let found = state.heartbeats.get(id).cloned();
        let result = self.delete_heartbeat(&mut state, caller, id);
        let scope = found.as_ref().map(|h| h.scope.clone());
        let audited = result
            .clone()
            .and_then(|()| found.ok_or_else(|| missing(id)));
        self.audit_heartbeat(
            &state,
            caller,
            Op::HeartbeatDelete,
            scope.as_ref(),
            &audited,
        );
        result.map(|()| Response::Ok)
    }

    pub(super) fn heartbeat_run(
        &self,
        caller: &Caller,
        id: &HeartbeatId,
    ) -> Result<Response, ApiError> {
        let mut state = self.lock();
        let found = state.heartbeats.get(id).cloned();
        let result = policy::authorize(caller, Op::HeartbeatRun, None).and_then(|()| {
            if found.is_none() {
                return Err(missing(id));
            }
            let now = self.now();
            let started = self.start_run(&mut state, id, true, now);
            let view = match &started {
                Ok((view, _)) => Ok(view.clone()),
                Err(NotStarted::Skipped(why)) => Err(invalid(why.clone())),
                Err(NotStarted::Failed(error)) => Err(error.clone()),
            };
            self.record_run(&mut state, id, started, now);
            view
        });
        let audited = result
            .as_ref()
            .map(|_| found.clone())
            .map_err(Clone::clone)
            .and_then(|found| found.ok_or_else(|| missing(id)));
        let scope = found.as_ref().map(|h| h.scope.clone());
        self.audit_heartbeat(&state, caller, Op::HeartbeatRun, scope.as_ref(), &audited);
        result.map(|session| Response::Session { session })
    }

    fn create_heartbeat(
        &self,
        state: &mut State,
        caller: &Caller,
        spec: HeartbeatSpec,
    ) -> Result<Heartbeat, ApiError> {
        policy::authorize(caller, Op::HeartbeatCreate, None)?;
        if state.heartbeats.len() >= MAX_HEARTBEATS {
            return Err(limit_exceeded(
                "max_heartbeats",
                MAX_HEARTBEATS as u32,
                format!("at most {MAX_HEARTBEATS} heartbeats; delete one first"),
            ));
        }
        let by_agent = caller.session_id().is_some() || spec.fields.by_agent;
        let scope = match (&caller.info, spec.scope) {
            (CallerInfo::Session { scope: own, .. }, None) => own.clone(),
            (_, scope) => validate_scope(scope)?,
        };
        validate_fields(&spec.fields)?;
        let name = match &spec.name {
            Some(raw) => {
                let name = validate_name(raw)?;
                if name_taken(state, &scope, &name, None) {
                    return Err(name_in_use(&name));
                }
                name
            }
            None => (1u32..)
                .map(|n| match n {
                    1 => NEW_HEARTBEAT.to_string(),
                    n => format!("{NEW_HEARTBEAT} {n}"),
                })
                .find(|name| !name_taken(state, &scope, name, None))
                .unwrap_or_else(|| NEW_HEARTBEAT.to_string()),
        };
        let now = self.now().div_euclid(1000);
        let mut heartbeat = Heartbeat {
            id: HeartbeatId::generate(),
            scope,
            name,
            agent: None,
            command: String::new(),
            prompt: String::new(),
            schedule: None,
            enabled: false,
            awaiting_approval: by_agent,
            created_by: match caller.session_id() {
                Some(id) => Creator::Session { id: id.clone() },
                None => Creator::User,
            },
            created_at: now,
            updated_at: now,
            next_run_at: None,
            runs: 0,
            last_run: None,
        };
        self.apply_fields(state, &mut heartbeat, &spec.fields, true);
        if spec.enabled && !by_agent {
            if !heartbeat.is_complete() {
                return Err(incomplete(&heartbeat));
            }
            heartbeat.enabled = true;
            heartbeat.next_run_at = next_due(&heartbeat, now);
        }
        registry_of(state)?.save_heartbeat(&heartbeat)?;
        state
            .heartbeats
            .insert(heartbeat.id.clone(), heartbeat.clone());
        Ok(heartbeat)
    }

    fn update_heartbeat(
        &self,
        state: &mut State,
        caller: &Caller,
        edit: HeartbeatEdit,
    ) -> Result<Heartbeat, ApiError> {
        policy::authorize(caller, Op::HeartbeatUpdate, None)?;
        let mut heartbeat = state
            .heartbeats
            .get(&edit.id)
            .cloned()
            .ok_or_else(|| missing(&edit.id))?;
        validate_fields(&edit.fields)?;
        if let Some(raw) = &edit.name {
            let name = validate_name(raw)?;
            if name_taken(state, &heartbeat.scope, &name, Some(&heartbeat.id)) {
                return Err(name_in_use(&name));
            }
            heartbeat.name = name;
        }
        let by_agent = caller.session_id().is_some() || edit.fields.by_agent;
        let schedule_before = heartbeat.schedule.clone();
        self.apply_fields(state, &mut heartbeat, &edit.fields, false);
        let now = self.now().div_euclid(1000);
        if by_agent && edit.fields.changes_runs() {
            heartbeat.enabled = false;
            heartbeat.awaiting_approval = true;
        }
        if heartbeat.enabled && !heartbeat.is_complete() {
            heartbeat.enabled = false;
        }
        heartbeat.next_run_at = match heartbeat.enabled {
            false => None,
            true if heartbeat.schedule != schedule_before || heartbeat.next_run_at.is_none() => {
                next_due(&heartbeat, now)
            }
            true => heartbeat.next_run_at,
        };
        heartbeat.updated_at = now;
        registry_of(state)?.save_heartbeat(&heartbeat)?;
        state
            .heartbeats
            .insert(heartbeat.id.clone(), heartbeat.clone());
        Ok(heartbeat)
    }

    fn enable_heartbeat(
        &self,
        state: &mut State,
        caller: &Caller,
        id: &HeartbeatId,
        enabled: bool,
    ) -> Result<Heartbeat, ApiError> {
        policy::authorize(caller, Op::HeartbeatEnable, None)?;
        let mut heartbeat = state
            .heartbeats
            .get(id)
            .cloned()
            .ok_or_else(|| missing(id))?;
        let now = self.now().div_euclid(1000);
        if enabled {
            if !heartbeat.is_complete() {
                return Err(incomplete(&heartbeat));
            }
            heartbeat.enabled = true;
            heartbeat.awaiting_approval = false;
            heartbeat.next_run_at = next_due(&heartbeat, now);
        } else {
            heartbeat.enabled = false;
            heartbeat.next_run_at = None;
        }
        heartbeat.updated_at = now;
        registry_of(state)?.save_heartbeat(&heartbeat)?;
        state.heartbeats.insert(id.clone(), heartbeat.clone());
        Ok(heartbeat)
    }

    fn delete_heartbeat(
        &self,
        state: &mut State,
        caller: &Caller,
        id: &HeartbeatId,
    ) -> Result<(), ApiError> {
        policy::authorize(caller, Op::HeartbeatDelete, None)?;
        if !state.heartbeats.contains_key(id) {
            return Err(missing(id));
        }
        registry_of(state)?.delete_heartbeat(id)?;
        state.heartbeats.remove(id);
        let runs: Vec<SessionId> = state
            .runs
            .iter()
            .filter(|(_, run)| run.heartbeat == *id)
            .map(|(session, _)| session.clone())
            .collect();
        for session in runs {
            state.runs.remove(&session);
            self.refresh(state, &session);
        }
        self.changed.notify_all();
        Ok(())
    }

    /// Set what `fields` names. A new heartbeat's command (and a changed
    /// agent's, without a command) is the one the user last started that
    /// agent with in the folder, else the agent's own name.
    fn apply_fields(
        &self,
        state: &State,
        heartbeat: &mut Heartbeat,
        fields: &HeartbeatFields,
        creating: bool,
    ) {
        if let Some(agent) = &fields.agent {
            let agent = agent.trim();
            heartbeat.agent = (!agent.is_empty()).then(|| agent.to_owned());
        }
        match &fields.command {
            Some(command) => {
                heartbeat.command = command.trim().to_owned();
                if fields.agent.is_none() {
                    heartbeat.agent = agent_of(&heartbeat.command);
                }
            }
            None if creating || fields.agent.is_some() => {
                heartbeat.command =
                    self.default_command(state, &heartbeat.scope, heartbeat.agent.as_deref());
                if heartbeat.agent.is_none() {
                    heartbeat.agent = agent_of(&heartbeat.command);
                }
            }
            None => {}
        }
        if let Some(prompt) = &fields.prompt {
            heartbeat.prompt = prompt.clone();
        }
        if let Some(schedule) = &fields.schedule {
            heartbeat.schedule = Some(schedule.clone());
        }
    }

    /// The command of the agent (`agent`, or any) the user last ran in
    /// `scope`, from its session's resume command (which keeps the flags
    /// that shape a session) without what names its conversation; else the
    /// agent's name.
    fn default_command(&self, state: &State, scope: &ScopeRoot, agent: Option<&str>) -> String {
        let records = match registry_of(state).and_then(|r| r.list(scope).map_err(Into::into)) {
            Ok((records, _)) => records,
            Err(error) => {
                crate::log_warn!("control: heartbeat command: {}", error.message);
                Vec::new()
            }
        };
        let mut records: Vec<_> = records
            .into_iter()
            .filter(|record| !state.runs.contains_key(&record.id))
            .collect();
        records.sort_by_key(|record| std::cmp::Reverse(record.last_active_at_ms));
        let found = records.iter().find_map(|record| {
            let resume = record.resume.as_ref()?;
            if !matches!(
                resume.source,
                ResumeSource::KnownAgent | ResumeSource::ReportedByAgent
            ) {
                return None;
            }
            let argv = crate::resume::start_command(&resume.argv)?;
            let label = agent_of(argv.first()?)?;
            agent
                .map_or(true, |agent| agent == label)
                .then(|| shell_words(&argv))
        });
        found.unwrap_or_else(|| agent.unwrap_or(DEFAULT_AGENT).to_owned())
    }

    // ------------------------------------------------------------ runs

    /// Start every enabled heartbeat whose due time came (on the tick).
    pub(super) fn run_due_heartbeats(&self) {
        let now = self.now();
        let now_s = now.div_euclid(1000);
        let mut state = self.lock();
        let due: Vec<HeartbeatId> = state
            .heartbeats
            .values()
            .filter(|h| h.enabled && h.next_run_at.is_some_and(|at| at <= now_s))
            .map(|h| h.id.clone())
            .collect();
        for id in due {
            let started = self.start_run(&mut state, &id, false, now);
            let caller = daemon();
            let scope = state.heartbeats.get(&id).map(|h| h.scope.clone());
            let audited = match &started {
                Ok(_) => state
                    .heartbeats
                    .get(&id)
                    .cloned()
                    .ok_or_else(|| missing(&id)),
                Err(NotStarted::Skipped(why)) => Err(invalid(why.clone())),
                Err(NotStarted::Failed(error)) => Err(error.clone()),
            };
            self.audit_heartbeat(&state, &caller, Op::HeartbeatRun, scope.as_ref(), &audited);
            self.record_run(&mut state, &id, started, now);
        }
    }

    /// Record what a due time (or a run asked for) did, and when it's due
    /// next: counted from now, so missed due times give one run.
    fn record_run(
        &self,
        state: &mut State,
        id: &HeartbeatId,
        started: Result<(SessionView, u32), NotStarted>,
        now: i64,
    ) {
        let Some(mut heartbeat) = state.heartbeats.get(id).cloned() else {
            return;
        };
        let now_s = now.div_euclid(1000);
        let run = match started {
            Ok((view, run)) => {
                heartbeat.runs = run;
                crate::log_info!("control: heartbeat {id} run #{run} started as {}", view.id);
                HeartbeatRun {
                    run,
                    at: now_s,
                    session_id: Some(view.id),
                    outcome: RunOutcome::Started,
                    detail: None,
                }
            }
            Err(NotStarted::Skipped(why)) => {
                crate::log_info!("control: heartbeat {id} skipped: {why}");
                HeartbeatRun {
                    run: 0,
                    at: now_s,
                    session_id: None,
                    outcome: RunOutcome::Skipped,
                    detail: Some(why),
                }
            }
            Err(NotStarted::Failed(error)) => {
                crate::log_warn!("control: heartbeat {id} couldn't start: {}", error.message);
                HeartbeatRun {
                    run: 0,
                    at: now_s,
                    session_id: None,
                    outcome: RunOutcome::Failed,
                    detail: Some(error.message),
                }
            }
        };
        heartbeat.last_run = Some(run);
        if heartbeat.enabled {
            heartbeat.next_run_at = next_due(&heartbeat, now_s);
        }
        if let Err(error) =
            registry_of(state).and_then(|r| r.save_heartbeat(&heartbeat).map_err(ApiError::from))
        {
            crate::log_warn!("control: saving heartbeat {id}: {}", error.message);
        }
        state.heartbeats.insert(id.clone(), heartbeat);
    }

    /// Start heartbeat `id`'s next run: end its earlier runs whose agent
    /// waits, unless one is at work, asking something or starting (then
    /// the due time is skipped; a run asked for, `manual`, starts beside
    /// it), and start `<name> #<n>` with the task as its first input.
    fn start_run(
        &self,
        state: &mut State,
        id: &HeartbeatId,
        manual: bool,
        now: i64,
    ) -> Result<(SessionView, u32), NotStarted> {
        let heartbeat = state
            .heartbeats
            .get(id)
            .cloned()
            .ok_or_else(|| NotStarted::Failed(missing(id)))?;
        if !heartbeat.is_complete() {
            return Err(NotStarted::Failed(incomplete(&heartbeat)));
        }
        let mut waiting = Vec::new();
        let mut busy = None;
        let mut earlier: Vec<(u32, SessionId)> = state
            .runs
            .iter()
            .filter(|(_, run)| run.heartbeat == *id)
            .map(|(session, run)| (run.run, session.clone()))
            .collect();
        earlier.sort();
        for (run, session) in &earlier {
            let Some(terminal) = self.running_terminal(state, session) else {
                continue;
            };
            match self.lifecycle(state, session, SessionStatus::Live) {
                lifecycle @ (Lifecycle::Working | Lifecycle::Blocked | Lifecycle::Unknown) => {
                    busy = Some((*run, lifecycle));
                }
                _ => waiting.push((session.clone(), terminal)),
            }
        }
        if let (Some((run, lifecycle)), false) = (busy, manual) {
            let doing = match lifecycle {
                Lifecycle::Working => "still at work",
                Lifecycle::Blocked => "asking something",
                _ => "still starting",
            };
            return Err(NotStarted::Skipped(format!("run #{run} is {doing}")));
        }
        let caller = daemon();
        for (session, terminal) in waiting {
            let result = match self.host.terminate(terminal) {
                Ok(()) | Err(HostError::NotFound | HostError::NotRunning) => Ok(()),
                Err(error) => Err(host_error(error)),
            };
            self.audit_outcome(
                state,
                &caller,
                Op::Kill,
                Some(&heartbeat.scope),
                Some(&session),
                Some(format!("an earlier run of heartbeat hb_{id}")),
                &result,
            );
        }
        let run = heartbeat.runs.saturating_add(1);
        let name = run_name(state, &heartbeat, run).map_err(NotStarted::Failed)?;
        let request = CreateSession {
            scope: Some(heartbeat.scope.clone()),
            name: Some(name.clone()),
            cwd: None,
            argv: None,
            startup_cmds: vec![heartbeat.command.clone()],
            env: Vec::new(),
            cols: 0,
            rows: 0,
            machine: None,
            prefill: None,
            first_input: Some(FirstInput {
                text: heartbeat.prompt.clone(),
                submit: true,
            }),
        };
        let created = self.create_session(state, &caller, request);
        self.audit_outcome(
            state,
            &caller,
            Op::Create,
            Some(&heartbeat.scope),
            created.as_ref().ok().map(|view| &view.id),
            Some(audit::clip(&name)),
            &created,
        );
        let view = created.map_err(NotStarted::Failed)?;
        let row = RunRow {
            session_id: view.id.clone(),
            heartbeat: id.clone(),
            run,
            started_ms: now,
        };
        if let Err(error) =
            registry_of(state).and_then(|r| r.add_heartbeat_run(&row).map_err(ApiError::from))
        {
            crate::log_warn!(
                "control: recording heartbeat run {}: {}",
                view.id,
                error.message
            );
        }
        state.runs.insert(
            view.id.clone(),
            HeartbeatRunRef {
                heartbeat: id.clone(),
                run,
            },
        );
        self.refresh(state, &view.id);
        let view = state.views.get(&view.id).cloned().unwrap_or(view);
        self.trim_runs(state, id);
        self.changed.notify_all();
        Ok((view, run))
    }

    /// Runs of `id` past the newest [`RUNS_KEPT`] that have ended go to the
    /// session history.
    fn trim_runs(&self, state: &mut State, id: &HeartbeatId) {
        let mut runs: Vec<(u32, SessionId)> = state
            .runs
            .iter()
            .filter(|(_, run)| run.heartbeat == *id)
            .map(|(session, run)| (run.run, session.clone()))
            .collect();
        runs.sort_by(|a, b| b.cmp(a));
        let caller = daemon();
        for (_, session) in runs.into_iter().skip(RUNS_KEPT as usize) {
            if self.running_terminal(state, &session).is_some() {
                continue;
            }
            let record = match registry_of(state).and_then(|r| r.get(&session).map_err(Into::into))
            {
                Ok(Some(record)) => record,
                Ok(None) => {
                    state.runs.remove(&session);
                    continue;
                }
                Err(error) => {
                    crate::log_warn!("control: heartbeat run {session}: {}", error.message);
                    continue;
                }
            };
            let result = self.delete_session(state, &record);
            self.audit_outcome(
                state,
                &caller,
                Op::Delete,
                Some(&record.scope),
                Some(&session),
                Some(format!("an old run of heartbeat hb_{id}")),
                &result,
            );
        }
    }

    /// A session that is no heartbeat's run any more (deleted).
    pub(super) fn forget_run(&self, state: &mut State, session: &SessionId) {
        if state.runs.remove(session).is_none() {
            return;
        }
        if let Err(error) =
            registry_of(state).and_then(|r| r.remove_heartbeat_run(session).map_err(Into::into))
        {
            crate::log_warn!(
                "control: forgetting heartbeat run {session}: {}",
                error.message
            );
        }
    }

    // ------------------------------------------------------------ first input

    /// Type each waiting first input into its session once its agent waits
    /// at its prompt (on the tick): an agent in the foreground, its screen
    /// read idle (or done) and quiet a moment, its hooks not saying it's
    /// busy. Given up after [`FIRST_INPUT_TIMEOUT_MS`].
    pub(super) fn give_first_inputs(&self) {
        let waiting: Vec<(SessionId, TerminalId)> = {
            let state = self.lock();
            state
                .live
                .iter()
                .filter(|(_, live)| live.first_input.is_some())
                .map(|(id, live)| (id.clone(), live.terminal))
                .collect()
        };
        for (id, terminal) in waiting {
            self.give_first_input(&id, terminal);
        }
    }

    fn give_first_input(&self, id: &SessionId, terminal: TerminalId) {
        // The screen and process are read without the lock.
        let detection = self.detect(terminal);
        let foreground = self
            .host
            .info(terminal)
            .and_then(|info| info.foreground)
            .map(|process| process.name);
        let now = self.now();
        let (text, submit, agent) = {
            let mut state = self.lock();
            let Some(live) = state
                .live
                .get_mut(id)
                .filter(|live| live.terminal == terminal)
            else {
                return;
            };
            let busy = matches!(
                live.tracker.current(),
                Lifecycle::Working | Lifecycle::Blocked
            );
            let output_ms = live.output_ms;
            let Some(input) = live.first_input.as_mut() else {
                return;
            };
            if now.saturating_sub(input.since) >= FIRST_INPUT_TIMEOUT_MS {
                live.first_input = None;
                crate::log_info!("control: first input for {id} dropped: no agent prompt");
                self.note_first_input(
                    &mut state,
                    id,
                    RunOutcome::NoAgent,
                    Some("its agent never showed its prompt".into()),
                );
                return;
            }
            let agent = detection.as_ref().and_then(|d| d.agent.clone());
            match agent {
                Some(_) => {
                    input.agent_since.get_or_insert(now);
                }
                None => input.agent_since = None,
            }
            let settle = match agent
                .as_deref()
                .is_some_and(crate::detect::knows_its_prompt)
            {
                true => SETTLE_KNOWN_MS,
                false => SETTLE_UNKNOWN_MS,
            };
            let at_prompt = detection
                .as_ref()
                .is_some_and(|d| matches!(d.lifecycle, Lifecycle::Idle | Lifecycle::Done));
            let settled = input
                .agent_since
                .is_some_and(|since| now.saturating_sub(since) >= settle)
                && now.saturating_sub(output_ms) >= settle;
            if agent.is_none() || !at_prompt || busy || !settled {
                return;
            }
            (input.text.clone(), input.submit, agent.unwrap_or_default())
        };
        let written = match submit {
            true => self.host.submit_checked(
                terminal,
                &text,
                Some(FIRST_INPUT_QUIET),
                foreground.as_deref(),
                self.unless_asking(terminal, id),
            ),
            false => self
                .host
                .send_keys_when_quiet(terminal, &[], Some(&text), FIRST_INPUT_QUIET),
        };
        let mut state = self.lock();
        match written {
            Ok(()) => {
                if let Some(live) = state.live.get_mut(id) {
                    live.first_input = None;
                }
                // Never the text.
                crate::log_info!(
                    "control: first input typed into {id} ({agent}{})",
                    if submit { ", sent" } else { "" }
                );
                if submit {
                    self.note_first_input(&mut state, id, RunOutcome::TaskGiven, None);
                }
            }
            // Someone typed, or the agent just left: next tick.
            Err(HostError::Typing | HostError::ForegroundChanged) => {}
            Err(error) => {
                if let Some(live) = state.live.get_mut(id) {
                    live.first_input = None;
                }
                crate::log_warn!("control: first input for {id}: {error}");
                self.note_first_input(&mut state, id, RunOutcome::NoAgent, Some(error.to_string()));
            }
        }
    }

    /// What became of a run's task, on its heartbeat's last run.
    fn note_first_input(
        &self,
        state: &mut State,
        session: &SessionId,
        outcome: RunOutcome,
        detail: Option<String>,
    ) {
        let Some(run) = state.runs.get(session).cloned() else {
            return;
        };
        let Some(mut heartbeat) = state.heartbeats.get(&run.heartbeat).cloned() else {
            return;
        };
        let Some(last) = heartbeat
            .last_run
            .as_mut()
            .filter(|last| last.session_id.as_ref() == Some(session))
        else {
            return;
        };
        last.outcome = outcome;
        last.detail = detail;
        if let Err(error) =
            registry_of(state).and_then(|r| r.save_heartbeat(&heartbeat).map_err(ApiError::from))
        {
            crate::log_warn!(
                "control: saving heartbeat {}: {}",
                run.heartbeat,
                error.message
            );
        }
        state.heartbeats.insert(run.heartbeat, heartbeat);
    }

    /// Audit a heartbeat request: its id and name, never its prompt.
    fn audit_heartbeat<T>(
        &self,
        state: &State,
        caller: &Caller,
        op: Op,
        scope: Option<&ScopeRoot>,
        result: &Result<T, ApiError>,
    ) where
        T: std::borrow::Borrow<Heartbeat>,
    {
        if !op.is_mutating() {
            return;
        }
        let detail = match result {
            Ok(heartbeat) => {
                let heartbeat = heartbeat.borrow();
                Some(audit::clip(&format!(
                    "{} {}",
                    heartbeat.handle(),
                    heartbeat.name
                )))
            }
            Err(_) => None,
        };
        let (decision, detail) = match result {
            Ok(_) => (Decision::Allowed, detail),
            Err(error) => (
                match error.code {
                    ErrorCode::PermissionDenied | ErrorCode::LimitExceeded => Decision::Denied,
                    _ => Decision::Error,
                },
                Some(audit::clip(&error.to_string())),
            ),
        };
        self.audit(state, caller, op, scope, None, decision, detail);
    }
}

/// The heartbeats and runs the registry holds, as the daemon starts; an
/// enabled one without a due time gets one.
pub(super) fn load(
    registry: &crate::control::registry::Registry,
    now_ms: i64,
) -> (
    std::collections::BTreeMap<HeartbeatId, Heartbeat>,
    std::collections::HashMap<SessionId, HeartbeatRunRef>,
) {
    let heartbeats = match registry.heartbeats() {
        Ok((heartbeats, bad)) => {
            for error in bad {
                crate::log_warn!("control: reading heartbeats: {error}");
            }
            heartbeats
        }
        Err(error) => {
            crate::log_warn!("control: reading heartbeats: {error}");
            Vec::new()
        }
    };
    let now = now_ms.div_euclid(1000);
    let heartbeats = heartbeats
        .into_iter()
        .map(|mut heartbeat| {
            if heartbeat.enabled && heartbeat.next_run_at.is_none() {
                heartbeat.next_run_at = next_due(&heartbeat, now);
            }
            (heartbeat.id.clone(), heartbeat)
        })
        .collect();
    let runs = registry
        .heartbeat_runs()
        .unwrap_or_else(|error| {
            crate::log_warn!("control: reading heartbeat runs: {error}");
            Vec::new()
        })
        .into_iter()
        .map(|row| {
            (
                row.session_id,
                HeartbeatRunRef {
                    heartbeat: row.heartbeat,
                    run: row.run,
                },
            )
        })
        .collect();
    (heartbeats, runs)
}

/// When `heartbeat` is due next after `now` (Unix seconds), in local time.
fn next_due(heartbeat: &Heartbeat, now: i64) -> Option<i64> {
    schedule::next_after(heartbeat.schedule.as_ref()?, now, &chrono::Local)
}

/// `<name> #<n>`, unique in its folder (`… 2` when taken).
fn run_name(state: &State, heartbeat: &Heartbeat, run: u32) -> Result<String, ApiError> {
    let registry = registry_of(state)?;
    let base: String = format!("{} #{run}", heartbeat.name);
    for n in 1u32..100 {
        let name = match n {
            1 => base.clone(),
            n => format!("{base} {n}"),
        };
        if !registry.name_taken(&heartbeat.scope, &name, None)? {
            return Ok(name);
        }
    }
    Err(invalid(format!("no free name for {base}")))
}

/// The agent a command starts (`claude --x` → `claude`), when it's one
/// Rakumux knows: the first word that isn't an environment assignment.
fn agent_of(command: &str) -> Option<String> {
    let word = command
        .split_whitespace()
        .find(|word| !word.contains('='))?;
    let program = word.rsplit('/').next().unwrap_or(word);
    let program = match program {
        "opencode2" => "opencode",
        other => other,
    };
    AGENTS.contains(&program).then(|| program.to_owned())
}

/// `argv` as a shell command line.
fn shell_words(argv: &[String]) -> String {
    argv.iter()
        .map(|word| crate::control::env::shell_quote(word))
        .collect::<Vec<_>>()
        .join(" ")
}

fn validate_name(raw: &str) -> Result<String, ApiError> {
    names::validate(raw).map_err(|error| {
        ApiError::new(
            error.code,
            error.message.replace("session name", "heartbeat name"),
        )
    })
}

fn validate_fields(fields: &HeartbeatFields) -> Result<(), ApiError> {
    if let Some(prompt) = &fields.prompt {
        if prompt.len() > MAX_PROMPT_BYTES {
            return Err(invalid(format!(
                "the prompt is longer than {MAX_PROMPT_BYTES} bytes"
            )));
        }
        if prompt
            .chars()
            .any(|c| c.is_control() && c != '\n' && c != '\t')
        {
            return Err(invalid(
                "the prompt may not contain control characters but newlines and tabs",
            ));
        }
    }
    if let Some(command) = &fields.command {
        if command.len() > MAX_COMMAND_BYTES {
            return Err(invalid(format!(
                "the command is longer than {MAX_COMMAND_BYTES} bytes"
            )));
        }
        if command.chars().any(char::is_control) {
            return Err(invalid(
                "the command is one line without control characters",
            ));
        }
    }
    if let Some(agent) = &fields.agent {
        let agent = agent.trim();
        if agent.len() > 32
            || !agent
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        {
            return Err(invalid(format!("{agent:?} isn't an agent's name")));
        }
    }
    if let Some(problem) = fields.schedule.as_ref().and_then(|s| s.problem()) {
        return Err(invalid(problem));
    }
    Ok(())
}

fn name_taken(state: &State, scope: &ScopeRoot, name: &str, except: Option<&HeartbeatId>) -> bool {
    state.heartbeats.values().any(|heartbeat| {
        &heartbeat.scope == scope && heartbeat.name == name && Some(&heartbeat.id) != except
    })
}

fn name_in_use(name: &str) -> ApiError {
    invalid(format!(
        "another heartbeat of this folder is named {name:?}"
    ))
}

fn missing(id: &HeartbeatId) -> ApiError {
    ApiError::new(ErrorCode::NotFound, format!("no heartbeat hb_{id}"))
}

fn incomplete(heartbeat: &Heartbeat) -> ApiError {
    let mut missing = Vec::new();
    if heartbeat.command.trim().is_empty() {
        missing.push("a command");
    }
    if heartbeat.prompt.trim().is_empty() {
        missing.push("a prompt");
    }
    if heartbeat.schedule.is_none() {
        missing.push("a schedule");
    }
    let what = match missing.is_empty() {
        true => "a schedule this Rakumux knows".to_string(),
        false => missing.join(", "),
    };
    invalid(format!(
        "heartbeat {} needs {what} before it runs",
        heartbeat.name
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commands_name_their_agents() {
        assert_eq!(
            agent_of("claude --permission-mode acceptEdits").as_deref(),
            Some("claude")
        );
        assert_eq!(
            agent_of("FOO=1 ~/.local/bin/codex --full-auto").as_deref(),
            Some("codex")
        );
        assert_eq!(agent_of("opencode2 -m x").as_deref(), Some("opencode"));
        assert_eq!(agent_of("npm test"), None);
        assert_eq!(
            shell_words(&[
                "claude".into(),
                "--append-system-prompt".into(),
                "be brief".into()
            ]),
            "claude --append-system-prompt 'be brief'"
        );
    }
}
