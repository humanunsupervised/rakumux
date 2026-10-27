//! The control service proper: request handlers over the registry, the live
//! terminal map, session tokens, policy, waits, lifecycle, projects, git
//! context and events.
//!
//! Concurrency: one `Mutex<State>` guards the registry connection and all
//! in-memory session state; a `Condvar` paired with it wakes waits whenever
//! session state changes. Host events reach the service through a relay
//! thread (see `host_events`), so the host never blocks on this lock and
//! host calls made while holding it cannot deadlock. Git files are read
//! outside the lock where the directory is known up front (cwd events, the
//! poll, project views); a session view whose context isn't cached yet
//! reads it under the lock.

use super::audit::{self, AuditEntry, Decision};
use super::auth::{self, SessionTokens};
use super::env::{self, Identity};
use super::events::{EventBus, Filter, Scopes};
use super::git_context::{self, GitContexts};
use super::lifecycle::LifecycleTracker;
use super::names;
use super::places::{self, Places};
use super::policy::{self, Op, Target};
use super::registry::{
    self, ProjectRecord, ProjectRemoval, ProjectSessions, Registry, SessionRecord,
};
use super::replies::{ReplyDue, ReplyRing};
use super::saved;
use super::waits::{self, Observed, Outcome};
use super::{ControlConfig, Detector};
use crate::control_api::Caller;
use crate::detect::{DetectInput, Detection};
use crate::git;
use crate::handoff::{BindingHandoff, ControlHandoff};
use crate::host::{HostError, HostEvent, SpawnSpec, TerminalHost, TerminalState};
use crate::persist::ScrollbackStore;
use session_core::api::{
    AgentReply, ApiError, AttachRequest, CallerInfo, CreateSession, ErrorCode, EventEnvelope,
    Hello, ReadRequest, ReadSource, Report, Request, Response, ReviveSession, Role, SendRequest,
    SessionEvent, SubscribeRequest, TextFormat, WaitRequest, WaitResult, WaitUntil,
};
use session_core::ids::{ReplyId, SessionId, TerminalId, Token};
use session_core::model::{
    AgentInfo, Creator, LaunchSpec, Lifecycle, ProjectView, Reported, ResumeSource, ResumeSpec,
    ScopeRoot, SessionStatus, SessionView, TerminalView,
};
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

/// At most one `Activity` event per session per this interval.
const ACTIVITY_INTERVAL_MS: i64 = 1000;
/// How often a session's foreground is looked at for its resume command
/// (`resume::plan`), unless the foreground process changed.
const RESUME_CHECK_MS: i64 = 5_000;
/// How often a running agent's conversation log is read for its latest
/// line, at most (on its terminal's output).
const OUTPUT_CHECK_MS: i64 = 1_000;
/// How often a running agent's conversation is read for where it works.
const PLACES_CHECK_MS: i64 = 1_000;
/// How much of it is read at once: a long one is read from its start over
/// several checks.
const PLACES_BUDGET: usize = 512 * 1024;
/// A shell must sit at its prompt this long before the resume command is
/// forgotten (programs killed during a reboot leave the prompt for a
/// moment).
const SHELL_SETTLE_MS: i64 = 15_000;
/// Sessions that were running, in the Rakumux home (see
/// [`Inner::save_running`]).
const RUNNING_FILE: &str = "running.json";
/// Limits on what a session may report about itself.
const MAX_REPORT_TEXT_BYTES: usize = 4096;
const MAX_SUMMARY_BYTES: usize = 64 * 1024;
const MAX_REPORT_FILES: usize = 256;
/// Conversations bound per session (oldest dropped first) and per request.
const MAX_BINDINGS_PER_SESSION: usize = 256;
const MAX_BINDINGS_PER_REQUEST: usize = 64;
const MAX_CONVERSATION_ID_BYTES: usize = 128;
const MAX_AGENT_KIND_BYTES: usize = 32;

pub(super) fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

/// A session whose terminal the host has (running, or exited and not yet
/// forgotten).
struct LiveSession {
    working_directory: Option<session_core::api::AgentWorkingDirectory>,
    working_directory_pid: Option<u32>,
    terminal: TerminalId,
    scope: ScopeRoot,
    tracker: LifecycleTracker,
    last_activity_ms: i64,
    /// When the foreground was last looked at for the resume command, and
    /// which process it was.
    resume_checked_ms: i64,
    resume_checked_pid: Option<u32>,
    /// What the registry holds as the resume command, once known.
    recorded_resume: Option<Option<ResumeSpec>>,
    /// Since when a shell sits at its prompt.
    shell_since_ms: Option<i64>,
    /// The running agent's conversation log, followed for its latest line.
    output: Option<crate::agent_output::Follower>,
    /// When the log was last read.
    output_checked_ms: i64,
    /// The agent's latest output line (`SessionView::agent_output`).
    agent_output: Option<String>,
    /// The agent was seen working since its last finished turn.
    turn_started: bool,
    /// A turn finished while reply capture was on; its reply isn't captured yet.
    reply_due: Option<ReplyDue>,
    /// The last reply captured (`agent_output::Reply::key`), never twice.
    reply_key: Option<String>,
    /// The user's latest prompt (`SessionView::user_prompt`).
    user_prompt: Option<String>,
    /// The agent's current turn (`session_turn`, `SessionView::agent_turn`).
    turn: Option<crate::agent_output::Turn>,
    /// The agent's conversation, read for where it works
    /// (`SessionView::places`), and when it last was.
    places_scan: Option<crate::agent_output::places::PlaceScan>,
    places_checked_ms: i64,
}

impl LiveSession {
    fn new(terminal: TerminalId, scope: ScopeRoot) -> Self {
        Self {
            working_directory: None,
            working_directory_pid: None,
            terminal,
            scope,
            tracker: LifecycleTracker::default(),
            last_activity_ms: 0,
            resume_checked_ms: 0,
            resume_checked_pid: None,
            recorded_resume: None,
            shell_since_ms: None,
            output: None,
            output_checked_ms: 0,
            agent_output: None,
            turn_started: false,
            reply_due: None,
            reply_key: None,
            user_prompt: None,
            turn: None,
            places_scan: None,
            places_checked_ms: 0,
        }
    }
}

struct State {
    /// None once the daemon is shutting down.
    registry: Option<Registry>,
    live: HashMap<SessionId, LiveSession>,
    tokens: SessionTokens,
    /// Environment each session's program was spawned with (memory only);
    /// sessions it creates inherit it when they send no snapshot.
    spawn_env: HashMap<SessionId, Vec<(String, String)>>,
    /// Reports received per session in this daemon's lifetime (`wait
    /// --until reported`).
    report_seq: HashMap<SessionId, u64>,
    /// Last view published per session (with `is_self` false), to emit
    /// `Updated`/`Lifecycle` only on change.
    views: HashMap<SessionId, SessionView>,
    /// Git context per session, for the directory it was read from.
    git: GitContexts,
    /// Where each session has worked (`SessionView::places`), as the
    /// registry holds it.
    places: Places,
    /// When each session last created sessions, within the past minute
    /// (`Limits::max_creates_per_minute`).
    recent_creates: HashMap<SessionId, Vec<Instant>>,
    /// Agent conversations each session bound (`SessionBind`), oldest
    /// first; memory only. `Auth::Delegated` resolves to the running
    /// session with the newest binding of a conversation.
    bindings: HashMap<SessionId, VecDeque<Binding>>,
    binding_seq: u64,
    /// Reply capture and the replies captured (memory only).
    replies: ReplyRing,
}

struct Binding {
    agent: String,
    conversation: String,
    seq: u64,
}

impl State {
    fn is_current(&self, session_id: &SessionId, terminal: TerminalId) -> bool {
        self.live
            .get(session_id)
            .is_some_and(|live| live.terminal == terminal)
    }
}

/// Status of a record combined with the host's view of its terminal.
struct Runtime {
    status: SessionStatus,
    exit_code: Option<i32>,
    /// The terminal while the host still has it (running or exited).
    terminal: Option<TerminalId>,
    /// Terminal details while running.
    view: Option<TerminalView>,
}

pub(super) struct Inner {
    config: ControlConfig,
    host: Arc<dyn TerminalHost>,
    store: ScrollbackStore,
    detector: Detector,
    state: Mutex<State>,
    changed: Condvar,
    events: EventBus,
    pub(super) stop: Arc<AtomicBool>,
    /// The user stopped the daemon: nothing is revived on the next start.
    stopped_by_user: AtomicBool,
    /// Directories that are never places.
    places_excluded: places::Excluded,
}

impl Inner {
    pub(super) fn new(
        config: ControlConfig,
        host: Arc<dyn TerminalHost>,
        registry: Registry,
        detector: Detector,
    ) -> Self {
        let saved_places = registry.places().unwrap_or_else(|error| {
            crate::log_warn!("control: reading places: {error}");
            Default::default()
        });
        Self {
            places_excluded: places::Excluded::of_this_machine(),
            store: ScrollbackStore::new(config.layout.clone()),
            config,
            host,
            detector,
            state: Mutex::new(State {
                registry: Some(registry),
                live: HashMap::new(),
                tokens: SessionTokens::default(),
                spawn_env: HashMap::new(),
                report_seq: HashMap::new(),
                recent_creates: HashMap::new(),
                bindings: HashMap::new(),
                binding_seq: 0,
                replies: ReplyRing::default(),
                views: HashMap::new(),
                git: GitContexts::default(),
                places: Places::new(saved_places),
            }),
            changed: Condvar::new(),
            events: EventBus::default(),
            stop: Arc::new(AtomicBool::new(false)),
            stopped_by_user: AtomicBool::new(false),
        }
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        // A handler that panicked mid-update leaves consistent-enough state
        // (every write is a single registry statement); keep serving.
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn stopping(&self) -> bool {
        self.stop.load(Ordering::SeqCst)
    }

    /// Adopt terminals the host already runs: none after a daemon start (so
    /// every stored session is Dead or Exited), the handed-over ones after
    /// a restart in place. `handed` brings back their sessions' tokens,
    /// spawn environments and conversation bindings; without it an adopted
    /// terminal's processes can't authenticate as the session.
    pub(super) fn adopt_host_terminals(&self, handed: Option<ControlHandoff>) {
        let terminals = self.host.list();
        if terminals.is_empty() {
            return;
        }
        let mut state = self.lock();
        for info in terminals {
            let record = registry(&state.registry)
                .ok()
                .and_then(|r| r.get(&info.session_id).ok().flatten());
            match record {
                Some(record) => {
                    if handed.is_none() {
                        crate::log_warn!(
                            "control: adopting terminal {} of session {} without a session token",
                            info.id,
                            info.session_id
                        );
                    }
                    state
                        .live
                        .insert(record.id.clone(), LiveSession::new(info.id, record.scope));
                }
                None => crate::log_warn!(
                    "control: terminal {} belongs to unknown session {}",
                    info.id,
                    info.session_id
                ),
            }
        }
        let Some(handed) = handed else {
            return;
        };
        for (id, context, pid) in handed.working_directories {
            if let (Ok(context), Some(live)) = (
                super::working_directory::canonicalize(&context),
                state.live.get_mut(&id),
            ) {
                live.working_directory = Some(context);
                live.working_directory_pid = pid;
            }
        }
        let mut restored = 0;
        for (id, token) in handed.tokens {
            if state.live.contains_key(&id) {
                state.tokens.restore(&id, Token::from_string(token));
                restored += 1;
            }
        }
        for (id, env) in handed.spawn_env {
            if state.live.contains_key(&id) {
                state.spawn_env.insert(id, env);
            }
        }
        for binding in handed.bindings {
            if state.live.contains_key(&binding.session_id) {
                state
                    .bindings
                    .entry(binding.session_id)
                    .or_default()
                    .push_back(Binding {
                        agent: binding.agent,
                        conversation: binding.conversation,
                        seq: binding.seq,
                    });
            }
        }
        state.binding_seq = state.binding_seq.max(handed.binding_seq);
        for (id, update, received_ms) in handed.activity {
            if super::activity::validate(&update).is_ok()
                && now_ms().saturating_sub(received_ms) < super::activity::FRESH_MS
            {
                if let Some(live) = state.live.get_mut(&id) {
                    live.tracker.on_activity(&update, received_ms);
                    if handed.activity_seen.contains(&id) {
                        live.tracker.on_touch(received_ms);
                    }
                }
            }
        }
        crate::log_info!(
            "control: {} running session(s) carried over, {restored} with their token",
            state.live.len()
        );
    }

    /// Hand the sessions' memory to the next daemon image, then shut down
    /// like [`Inner::shutdown`].
    pub(super) fn handoff(&self) -> ControlHandoff {
        let handed = {
            let state = self.lock();
            ControlHandoff {
                activity_seen: state
                    .live
                    .iter()
                    .filter(|(_, live)| live.tracker.current() == Lifecycle::Idle)
                    .map(|(id, _)| id.clone())
                    .collect(),
                working_directories: state
                    .live
                    .iter()
                    .filter_map(|(id, live)| {
                        live.working_directory
                            .clone()
                            .map(|context| (id.clone(), context, live.working_directory_pid))
                    })
                    .collect(),
                activity: state
                    .live
                    .iter()
                    .filter_map(|(id, live)| {
                        live.tracker
                            .activity_handoff()
                            .map(|(update, received_ms)| (id.clone(), update, received_ms))
                    })
                    .collect(),
                tokens: state.tokens.export(),
                spawn_env: state
                    .spawn_env
                    .iter()
                    .map(|(id, env)| (id.clone(), env.clone()))
                    .collect(),
                bindings: state
                    .bindings
                    .iter()
                    .flat_map(|(id, bindings)| {
                        bindings.iter().map(move |b| BindingHandoff {
                            session_id: id.clone(),
                            agent: b.agent.clone(),
                            conversation: b.conversation.clone(),
                            seq: b.seq,
                        })
                    })
                    .collect(),
                binding_seq: state.binding_seq,
            }
        };
        self.shutdown();
        handed
    }

    // ------------------------------------------------------------ Control

    pub(super) fn authenticate(&self, hello: &Hello) -> Result<Caller, ApiError> {
        let state = self.lock();
        auth::authenticate(
            hello,
            &self.config.user_token,
            &state.tokens,
            |id| {
                registry(&state.registry)
                    .ok()?
                    .get(id)
                    .ok()?
                    .map(|record| record.scope)
            },
            |agent, conversation| self.bound_session(&state, agent, conversation),
        )
    }

    /// The running session with the newest binding of `conversation`.
    fn bound_session(&self, state: &State, agent: &str, conversation: &str) -> Option<SessionId> {
        state
            .bindings
            .iter()
            .filter(|(id, _)| self.running_terminal(state, id).is_some())
            .flat_map(|(id, bindings)| {
                bindings
                    .iter()
                    .filter(|b| b.agent == agent && b.conversation == conversation)
                    .map(move |b| (b.seq, id))
            })
            .max_by_key(|(seq, _)| *seq)
            .map(|(_, id)| id.clone())
    }

    pub(super) fn handle(&self, caller: &Caller, request: Request) -> Response {
        let result = match request {
            Request::Ping => Ok(Response::Pong),
            Request::SessionList {
                scope,
                scopes,
                include_dead,
                all,
            } => self.list(caller, scope, scopes, all, include_dead),
            Request::SessionGet { id } => self.get(caller, &id),
            Request::SessionCreate(request) => self.create(caller, request),
            Request::SessionImport { source } => self.import_sessions(caller, &source),
            Request::SessionRename { id, name } => self.rename(caller, &id, &name),
            Request::SessionKill { id, tree } => self.kill(caller, &id, tree),
            Request::SessionDelete { id } => self.delete(caller, &id),
            Request::SessionRevive(request) => self.revive(caller, request),
            Request::SessionReport(report) => self.report(caller, report),
            Request::SessionRead(request) => self.read(caller, request),
            Request::SessionWait(request) => self.wait(caller, request),
            Request::SessionSend(request) => self.send(caller, request),
            Request::SessionSubmit { id, text, quiet_ms } => {
                self.submit(caller, &id, &text, quiet_ms.map(Duration::from_millis))
            }
            Request::SessionTouch { id } => self.touch(caller, &id),
            Request::SessionTurn { id } => self.turn(caller, &id),
            Request::SessionTranscript {
                id,
                after,
                max_bytes,
            } => self.transcript(caller, &id, after.as_ref(), max_bytes),
            Request::SessionPin { id, pinned } => self.pin(caller, &id, pinned),
            Request::SessionBind {
                agent,
                conversations,
            } => self.bind(caller, &agent, conversations),
            Request::ProjectList => self.projects(caller),
            Request::ProjectAdd { path } => self.add_project(caller, &path),
            Request::ProjectRemove { path } => self.remove_project(caller, &path),
            Request::ProjectRelocate { from, to } => self.relocate_project(caller, &from, &to),
            Request::ReplyCapture { enabled } => self.reply_capture(caller, enabled),
            Request::ReplyList { after } => self.reply_list(caller, after.as_ref()),
            Request::Hello(_)
            | Request::Attach(_)
            | Request::TerminalOpen(_)
            | Request::TerminalAttach(_)
            | Request::Subscribe(_)
            | Request::DaemonStatus
            | Request::DaemonShutdown
            | Request::DaemonRestart { .. } => Err(invalid(
                "this request is handled by the connection, not the control layer",
            )),
        };
        result.unwrap_or_else(|error| Response::Error { error })
    }

    pub(super) fn prepare_attach(
        &self,
        caller: &Caller,
        request: &AttachRequest,
    ) -> Result<(TerminalId, SessionView), ApiError> {
        let mut state = self.lock();
        let record = self.target(&state, caller, Op::Attach, &request.id)?;
        let terminal = self
            .running_terminal(&state, &record.id)
            .ok_or_else(|| not_live(&record.id))?;
        if request.role == Role::Controller {
            // Taking control of a session counts as having seen it.
            self.mark_seen(&mut state, &record.id);
        }
        let children = registry(&state.registry)?.children(&record.id)?;
        let view = self.view(&state, &record, children, caller.session_id());
        Ok((terminal, view))
    }

    pub(super) fn open_terminal(
        &self,
        caller: &Caller,
        request: &session_core::api::OpenTerminal,
    ) -> Result<crate::host::PaneAttachment, ApiError> {
        // Serialize the count/id check and spawn with the other control ops.
        let state = self.lock();
        let result = policy::authorize(caller, Op::TerminalOpen, None).and_then(|()| {
            if self.stopping() {
                Err(shutting_down())
            } else {
                super::panes::open(
                    self.host.as_ref(),
                    request,
                    self.config.rakumux_bin.as_deref(),
                )
            }
        });
        self.audit_outcome(
            &state,
            caller,
            Op::TerminalOpen,
            None,
            None,
            Some(format!("pane {}", request.id)),
            &result,
        );
        result
    }

    pub(super) fn attach_terminal(
        &self,
        caller: &Caller,
        request: &session_core::api::TerminalAttachRequest,
    ) -> Result<crate::host::PaneAttachment, ApiError> {
        let state = self.lock();
        let result = policy::authorize(caller, Op::TerminalAttach, None)
            .and_then(|()| super::panes::attach(self.host.as_ref(), request));
        self.audit_outcome(
            &state,
            caller,
            Op::TerminalAttach,
            None,
            None,
            Some(format!("pane {}", request.id)),
            &result,
        );
        result
    }

    pub(super) fn subscribe(
        &self,
        caller: &Caller,
        request: &SubscribeRequest,
    ) -> Result<Receiver<EventEnvelope>, ApiError> {
        if request.replies {
            policy::authorize(caller, Op::ReplyList, None)?;
        }
        let state = self.lock();
        let scopes = self.caller_scopes(
            &state,
            caller,
            Op::Subscribe,
            request.scope.clone(),
            request.scopes.clone(),
            request.all,
        )?;
        let registry = registry(&state.registry)?;
        for id in &request.session_ids {
            let record = registry.get(id)?.ok_or_else(|| not_found(id))?;
            if !scopes.contains(&record.scope) {
                let watched = match &scopes {
                    Scopes::Only(only) if only.len() == 1 => only[0].path().display().to_string(),
                    _ => "the watched folders".to_string(),
                };
                return Err(ApiError::new(
                    ErrorCode::OutOfScope,
                    format!("session {id} is not in {watched}"),
                ));
            }
        }
        Ok(self.events.subscribe(Filter {
            scopes,
            session_ids: request.session_ids.clone(),
            kinds: request.kinds.clone(),
            self_id: caller.session_id().cloned(),
            replies: request.replies,
        }))
    }

    pub(super) fn session_count(&self) -> u32 {
        let state = self.lock();
        match registry(&state.registry).and_then(|r| r.count().map_err(ApiError::from)) {
            Ok(count) => count,
            Err(error) => {
                crate::log_warn!("control: counting sessions: {error}");
                0
            }
        }
    }

    pub(super) fn shutdown(&self) {
        self.stop.store(true, Ordering::SeqCst);
        let mut state = self.lock();
        if let Some(registry) = state.registry.take() {
            if let Err(error) = registry.checkpoint() {
                crate::log_warn!("control: registry checkpoint: {error}");
            }
        }
        drop(state);
        self.changed.notify_all();
        self.events.close_all();
    }

    // ------------------------------------------------------------ ops

    fn list(
        &self,
        caller: &Caller,
        scope: Option<ScopeRoot>,
        scopes: Vec<ScopeRoot>,
        all: bool,
        include_dead: bool,
    ) -> Result<Response, ApiError> {
        let mut state = self.lock();
        let scopes = self.caller_scopes(&state, caller, Op::List, scope, scopes, all)?;
        let registry = registry(&state.registry)?;
        let (records, bad) = match &scopes {
            Scopes::All => registry.list_all()?,
            Scopes::Only(scopes) => registry.list_in(scopes)?,
        };
        for error in bad {
            crate::log_warn!("control: skipping registry row: {error}");
        }
        for record in &records {
            let directory = self.git_directory(&state, record);
            state.git.ensure(&record.id, &directory);
        }
        let mut children: HashMap<SessionId, Vec<(i64, SessionId)>> = HashMap::new();
        for record in &records {
            if let Some(parent) = &record.parent_id {
                children
                    .entry(parent.clone())
                    .or_default()
                    .push((record.created_at_ms, record.id.clone()));
            }
        }
        let sessions = records
            .iter()
            .map(|record| {
                let mut kids = children.remove(&record.id).unwrap_or_default();
                kids.sort();
                let kids = kids.into_iter().map(|(_, id)| id).collect();
                self.view(&state, record, kids, caller.session_id())
            })
            .filter(|view| include_dead || view.status != SessionStatus::Dead)
            .collect();
        Ok(Response::Sessions { sessions })
    }

    fn get(&self, caller: &Caller, id: &SessionId) -> Result<Response, ApiError> {
        let state = self.lock();
        let record = self.target(&state, caller, Op::Get, id)?;
        let children = registry(&state.registry)?.children(id)?;
        Ok(Response::Session {
            session: self.view(&state, &record, children, caller.session_id()),
        })
    }

    fn import_sessions(&self, caller: &Caller, source: &Path) -> Result<Response, ApiError> {
        let mut state = self.lock();
        let result = policy::authorize(caller, Op::Import, None)
            .and_then(|()| self.restore_archive(&mut state, source));
        self.audit_outcome(
            &state,
            caller,
            Op::Import,
            None,
            None,
            Some(audit::clip(&source.display().to_string())),
            &result,
        );
        result.map(|sessions| Response::Sessions { sessions })
    }

    fn restore_archive(
        &self,
        state: &mut State,
        source: &Path,
    ) -> Result<Vec<SessionView>, ApiError> {
        if !source.is_absolute() {
            return Err(invalid("archive directory must be absolute"));
        }
        let records = Registry::read_archive(&source.join("registry.sqlite3"))?;
        let registry = registry(&state.registry)?;
        for record in &records {
            if registry.get(&record.id)?.is_some() {
                return Err(invalid(format!("session {} already exists", record.id)));
            }
            if registry.name_taken(&record.scope, &record.name, None)? {
                return Err(name_taken(&record.name));
            }
            names::validate(&record.name)?;
            // Existing sessions may have cd'd outside their original folder;
            // restoring isn't creating a session in a different scope.
            if !record.cwd.is_absolute() {
                return Err(invalid(format!("session {} has a relative cwd", record.id)));
            }
        }
        let staged = super::import::StagedFiles::copy(source, &self.config.layout, &records)
            .map_err(|error| invalid(format!("copying archive files: {error}")))?;
        registry.insert_many(&records)?;
        staged.commit();
        let mut sessions = Vec::with_capacity(records.len());
        for record in &records {
            let children = records
                .iter()
                .filter(|child| child.parent_id.as_ref() == Some(&record.id))
                .map(|child| child.id.clone())
                .collect();
            let view = self.view(state, record, children, None);
            state.views.insert(record.id.clone(), view.clone());
            self.publish(
                &record.scope,
                &record.id,
                SessionEvent::Created {
                    session: view.clone(),
                },
            );
            sessions.push(view);
        }
        self.changed.notify_all();
        Ok(sessions)
    }

    fn create(&self, caller: &Caller, request: CreateSession) -> Result<Response, ApiError> {
        let mut state = self.lock();
        if let Err(error) = policy::authorize(caller, Op::Create, None) {
            self.audit(
                &state,
                caller,
                Op::Create,
                own_scope(caller),
                None,
                Decision::Denied,
                Some(error.message.clone()),
            );
            return Err(error);
        }
        let requested_scope = request.scope.clone();
        let detail = request.name.as_deref().map(audit::clip);
        let result = self.create_session(&mut state, caller, request);
        let (scope, target) = match &result {
            Ok(view) => (Some(view.scope.clone()), Some(view.id.clone())),
            Err(_) => (requested_scope.or_else(|| own_scope(caller).cloned()), None),
        };
        self.audit_outcome(
            &state,
            caller,
            Op::Create,
            scope.as_ref(),
            target.as_ref(),
            detail,
            &result,
        );
        result.map(|session| Response::Session { session })
    }

    fn create_session(
        &self,
        state: &mut State,
        caller: &Caller,
        request: CreateSession,
    ) -> Result<SessionView, ApiError> {
        let CreateSession {
            scope,
            name,
            cwd,
            argv,
            startup_cmds,
            env: request_env,
            cols,
            rows,
        } = request;
        let scope = match &caller.info {
            CallerInfo::User => validate_scope(scope)?,
            // A session creates sessions in its own project only.
            CallerInfo::Session { scope: own, .. } => match scope {
                Some(scope) if scope != *own => {
                    return Err(ApiError::new(
                        ErrorCode::OutOfScope,
                        format!(
                            "a session may only create sessions in its own folder ({}), not {}",
                            own.path().display(),
                            scope.path().display()
                        ),
                    ))
                }
                _ => own.clone(),
            },
        };
        let cwd = resolve_cwd(&scope, cwd.as_deref())?;
        if let Some(argv) = &argv {
            validate_argv(argv)?;
        }
        env::validate(&request_env)?;
        let (cols, rows) = env::spawn_size(cols, rows, self.caller_size(state, caller));
        let base_env = self.inherited_env(state, caller, &request_env);
        let parent = match caller.session_id() {
            Some(id) => registry(&state.registry)?.get(id)?,
            None => None,
        };
        if let Some(parent) = &parent {
            self.check_create_limits(state, parent)?;
        }

        let registry = registry(&state.registry)?;
        let auto_named = name.is_none();
        let name = match name {
            Some(raw) => {
                let name = names::validate(&raw)?;
                if registry.name_taken(&scope, &name, None)? {
                    return Err(name_taken(&name));
                }
                name
            }
            None => {
                let (records, _) = registry.list(&scope)?;
                let taken: HashSet<String> = records.into_iter().map(|r| r.name).collect();
                names::generate(|candidate| taken.contains(candidate))
            }
        };
        let now = now_ms();
        let id = SessionId::generate();
        let record = SessionRecord {
            id: id.clone(),
            scope: scope.clone(),
            name,
            cwd,
            launch: LaunchSpec {
                argv: argv.clone(),
                startup_cmds,
            },
            resume: None,
            created_by: match caller.session_id() {
                Some(parent) => Creator::Session { id: parent.clone() },
                None => Creator::User,
            },
            parent_id: caller.session_id().cloned(),
            depth: parent.map_or(0, |p| p.depth.saturating_add(1)),
            agent: None,
            reported: None,
            exit_code: None,
            created_at_ms: now,
            last_active_at_ms: now,
            ended_at_ms: None,
            description: None,
            notes: None,
            labels: Default::default(),
            private: false,
            auto_named,
            pinned: false,
        };
        // Insert first (this also records the project): the record must
        // exist before the terminal can emit events, and the unique index
        // settles name races.
        registry.insert(&record)?;
        let argv = argv.unwrap_or_else(|| env::login_shell(&base_env));
        if let Err(error) = self.start_terminal(state, &record, argv, base_env, cols, rows, None) {
            if let Err(e) = registry_of(state).and_then(|r| r.delete(&id).map_err(Into::into)) {
                crate::log_error!("control: removing record of failed session {id}: {e}");
            }
            return Err(error);
        }
        if let Some(parent) = &record.parent_id {
            state
                .recent_creates
                .entry(parent.clone())
                .or_default()
                .push(Instant::now());
        }
        let directory = self.git_directory(&state, &record);
        state.git.ensure(&id, &directory);
        let view = self.view(state, &record, Vec::new(), None);
        state.views.insert(id.clone(), view.clone());
        self.publish(
            &scope,
            &id,
            SessionEvent::Created {
                session: view.clone(),
            },
        );
        if let Some(parent) = &record.parent_id {
            self.refresh(state, parent);
        }
        self.changed.notify_all();
        Ok(view)
    }

    /// `Limits` on sessions that `parent` (a session caller) creates in its
    /// project. User callers have none.
    fn check_create_limits(
        &self,
        state: &mut State,
        parent: &SessionRecord,
    ) -> Result<(), ApiError> {
        let limits = &self.config.limits;
        let depth = parent.depth.saturating_add(1);
        if depth > limits.max_depth {
            return Err(limit_exceeded(
                "max_depth",
                u32::from(limits.max_depth),
                format!(
                    "sessions created by sessions nest at most {} deep, and this one is already at depth {}",
                    limits.max_depth, parent.depth
                ),
            ));
        }
        let registry = registry(&state.registry)?;
        let running = |id: &SessionId| self.running_terminal(state, id).is_some();
        let children = registry
            .children(&parent.id)?
            .iter()
            .filter(|id| running(id))
            .count();
        if children >= limits.max_children_per_parent as usize {
            return Err(limit_exceeded(
                "max_children_per_parent",
                limits.max_children_per_parent,
                format!(
                    "this session already has {children} running sessions it created; \
                     end one before creating another"
                ),
            ));
        }
        let (records, _) = registry.list(&parent.scope)?;
        let agent_created = records
            .iter()
            .filter(|r| matches!(r.created_by, Creator::Session { .. }) && running(&r.id))
            .count();
        if agent_created >= limits.max_live_per_scope as usize {
            return Err(limit_exceeded(
                "max_live_per_scope",
                limits.max_live_per_scope,
                format!(
                    "{agent_created} sessions created by sessions are already running in this \
                     folder; end one before creating another"
                ),
            ));
        }
        let minute_ago = Instant::now().checked_sub(Duration::from_secs(60));
        let recent = state.recent_creates.entry(parent.id.clone()).or_default();
        recent.retain(|at| minute_ago.is_none_or(|minute_ago| *at > minute_ago));
        if recent.len() >= limits.max_creates_per_minute as usize {
            return Err(limit_exceeded(
                "max_creates_per_minute",
                limits.max_creates_per_minute,
                format!(
                    "this session created {} sessions in the past minute; try again shortly",
                    recent.len()
                ),
            ));
        }
        Ok(())
    }

    fn rename(&self, caller: &Caller, id: &SessionId, name: &str) -> Result<Response, ApiError> {
        let mut state = self.lock();
        let record = self.target(&state, caller, Op::Rename, id)?;
        let result = self.rename_session(&mut state, &record, name);
        self.audit_outcome(
            &state,
            caller,
            Op::Rename,
            Some(&record.scope),
            Some(id),
            Some(audit::clip(name)),
            &result,
        );
        result?;
        self.current_view(&state, caller, id)
            .map(|session| Response::Session { session })
    }

    fn rename_session(
        &self,
        state: &mut State,
        record: &SessionRecord,
        raw: &str,
    ) -> Result<(), ApiError> {
        let name = names::validate(raw)?;
        if name == record.name {
            return Ok(());
        }
        let registry = registry(&state.registry)?;
        if registry.name_taken(&record.scope, &name, Some(&record.id))? {
            return Err(name_taken(&name));
        }
        registry.rename(&record.id, &name).map_err(|e| match e {
            registry::RegistryError::NameTaken => name_taken(&name),
            other => other.into(),
        })?;
        self.refresh(state, &record.id);
        self.changed.notify_all();
        Ok(())
    }

    fn kill(&self, caller: &Caller, id: &SessionId, tree: bool) -> Result<Response, ApiError> {
        let state = self.lock();
        let record = self.target(&state, caller, Op::Kill, id)?;
        let result = self.kill_sessions(&state, &record, tree);
        self.audit_outcome(
            &state,
            caller,
            Op::Kill,
            Some(&record.scope),
            Some(id),
            tree.then(|| "tree".to_string()),
            &result,
        );
        result.map(|killed| Response::Killed { killed })
    }

    /// Terminate the session (and with `tree` its descendants). Records stay;
    /// they become Exited when the host reports the exit.
    fn kill_sessions(
        &self,
        state: &State,
        record: &SessionRecord,
        tree: bool,
    ) -> Result<Vec<SessionId>, ApiError> {
        let ids = if tree {
            registry(&state.registry)?.subtree(&record.id)?
        } else {
            vec![record.id.clone()]
        };
        let mut killed = Vec::new();
        for id in ids {
            let Some(terminal) = self.running_terminal(state, &id) else {
                continue;
            };
            match self.host.terminate(terminal) {
                Ok(()) => killed.push(id),
                Err(HostError::NotFound | HostError::NotRunning) => {}
                Err(error) if tree => {
                    crate::log_warn!("control: kill --tree: terminating {id}: {error}");
                }
                Err(error) => return Err(host_error(error)),
            }
        }
        if !tree && killed.is_empty() {
            return Err(not_live(&record.id));
        }
        Ok(killed)
    }

    fn delete(&self, caller: &Caller, id: &SessionId) -> Result<Response, ApiError> {
        let mut state = self.lock();
        let record = self.target(&state, caller, Op::Delete, id)?;
        let result = self.delete_session(&mut state, &record);
        self.audit_outcome(
            &state,
            caller,
            Op::Delete,
            Some(&record.scope),
            Some(id),
            Some(audit::clip(&record.name)),
            &result,
        );
        result.map(|()| Response::Ok)
    }

    fn delete_session(&self, state: &mut State, record: &SessionRecord) -> Result<(), ApiError> {
        let orphans = registry(&state.registry)?.delete(&record.id)?;
        if let Some(live) = state.live.remove(&record.id) {
            match self.host.terminate(live.terminal) {
                Ok(()) | Err(HostError::NotFound | HostError::NotRunning) => {}
                Err(error) => {
                    crate::log_warn!("control: delete {}: terminate: {error}", record.id)
                }
            }
            self.host.forget(live.terminal);
        }
        state.tokens.revoke(&record.id);
        state.spawn_env.remove(&record.id);
        state.report_seq.remove(&record.id);
        state.recent_creates.remove(&record.id);
        state.bindings.remove(&record.id);
        state.views.remove(&record.id);
        state.git.remove(&record.id);
        state.places.remove(&record.id);
        self.save_running(state);
        if let Err(error) = self.store.remove(&record.id) {
            crate::log_warn!("control: delete {}: saved output: {error}", record.id);
        }
        let history = self.config.layout.history_file(&record.id);
        match std::fs::remove_file(&history) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => crate::log_warn!("control: delete {}: {}: {e}", record.id, history.display()),
        }
        self.publish(&record.scope, &record.id, SessionEvent::Removed);
        for orphan in orphans {
            self.refresh(state, &orphan);
        }
        self.changed.notify_all();
        Ok(())
    }

    fn revive(&self, caller: &Caller, request: ReviveSession) -> Result<Response, ApiError> {
        let mut state = self.lock();
        let id = request.id.clone();
        let record = self.target(&state, caller, Op::Revive, &id)?;
        let detail = request.resume.then(|| "resume".to_string());
        let result = self.revive_session(&mut state, caller, &record, request);
        self.audit_outcome(
            &state,
            caller,
            Op::Revive,
            Some(&record.scope),
            Some(&id),
            detail,
            &result,
        );
        result?;
        self.current_view(&state, caller, &id)
            .map(|session| Response::Session { session })
    }

    /// Start a new terminal for a Dead or Exited session: same id, cwd and
    /// launch spec. With `resume` and a stored `ResumeSpec`, the resume
    /// command replaces whichever of the launch command or a startup command
    /// starts the same program; other startup commands still run first.
    fn revive_session(
        &self,
        state: &mut State,
        caller: &Caller,
        record: &SessionRecord,
        request: ReviveSession,
    ) -> Result<(), ApiError> {
        if self.runtime(state, record).status == SessionStatus::Live {
            return Err(invalid(format!("session {} is already running", record.id)));
        }
        env::validate(&request.env)?;
        if !record.cwd.is_dir() {
            return Err(invalid(format!(
                "working directory {} no longer exists",
                record.cwd.display()
            )));
        }
        let (cols, rows) =
            env::spawn_size(request.cols, request.rows, self.caller_size(state, caller));
        let base_env = self.inherited_env(state, caller, &request.env);
        if let Some(old) = state.live.remove(&record.id) {
            self.host.forget(old.terminal);
        }
        // A last command is only typed, and only at a shell prompt that no
        // startup command already starts it from.
        let at_shell = record
            .launch
            .argv
            .as_ref()
            .is_none_or(|argv| crate::detect::is_shell_command(argv));
        let resume = match (&record.resume, request.resume) {
            (Some(spec), true) if spec.source.runs() => Some(spec),
            (Some(spec), true)
                if at_shell && !env::starts_program(&record.launch.startup_cmds, &spec.argv) =>
            {
                Some(spec)
            }
            _ => None,
        };
        let resumed = resume
            .filter(|spec| spec.source.runs())
            .and_then(|spec| spec.argv.first())
            .map(|p| env::program_name(p));
        let argv = match &record.launch.argv {
            // The launch command is the program being resumed: start a login
            // shell and let the resume command take its place.
            Some(argv)
                if resumed.is_some() && argv.first().map(|p| env::program_name(p)) == resumed =>
            {
                env::login_shell(&base_env)
            }
            Some(argv) => argv.clone(),
            None => env::login_shell(&base_env),
        };
        self.start_terminal(state, record, argv, base_env, cols, rows, resume)?;
        registry(&state.registry)?.mark_revived(&record.id, now_ms())?;
        self.refresh(state, &record.id);
        self.changed.notify_all();
        Ok(())
    }

    fn report(&self, caller: &Caller, report: Report) -> Result<Response, ApiError> {
        let mut state = self.lock();
        let Some(id) = caller.session_id().cloned() else {
            let error = invalid("session.report is only available to processes inside a session");
            self.audit(
                &state,
                caller,
                Op::Report,
                None,
                None,
                Decision::Error,
                Some(error.message.clone()),
            );
            return Err(error);
        };
        let record = self.target(&state, caller, Op::Report, &id)?;
        let detail = report_detail(&report);
        let result = self.apply_report(&mut state, &record, report);
        self.audit_outcome(
            &state,
            caller,
            Op::Report,
            Some(&record.scope),
            Some(&id),
            detail,
            &result,
        );
        result.map(|()| Response::Ok)
    }

    fn apply_report(
        &self,
        state: &mut State,
        record: &SessionRecord,
        report: Report,
    ) -> Result<(), ApiError> {
        validate_report(&report)?;
        let working_directory = report
            .working_directory
            .as_ref()
            .map(super::working_directory::canonicalize)
            .transpose()?;
        if let Some(context) = &working_directory {
            let declared = report
                .activity
                .as_ref()
                .map(|update| update.agent.as_str())
                .or_else(|| report.agent.as_ref().map(|agent| agent.kind.as_str()));
            if declared.is_some_and(|agent| agent != context.agent) {
                return Err(invalid("working-directory agent does not match the report"));
            }
        }
        if working_directory.is_some() && self.runtime(state, record).status != SessionStatus::Live
        {
            return Err(ApiError::new(
                ErrorCode::SessionNotLive,
                "working-directory reports require a live session",
            ));
        }
        if let Some(activity) = &report.activity {
            super::activity::validate(activity)?;
            if self.runtime(state, record).status != SessionStatus::Live {
                return Err(ApiError::new(
                    ErrorCode::SessionNotLive,
                    "activity requires a live session",
                ));
            }
        }
        let Report {
            working_directory: _,
            activity,
            state: lifecycle,
            text,
            result,
            agent,
            resume,
        } = report;
        if let Some(context) = &working_directory {
            let place = self.resolve_places(vec![context.path.clone()]);
            self.note_places(state, &record.id, place);
        }
        if let (Some(context), Some(live)) = (working_directory, state.live.get_mut(&record.id)) {
            live.working_directory_pid = self
                .host
                .info(live.terminal)
                .and_then(|info| info.foreground)
                .filter(|process| {
                    crate::detect::agent_label(process) == Some(context.agent.as_str())
                })
                .map(|process| process.pid);
            live.working_directory = Some(context);
        }
        let now = now_ms();
        let registry = registry(&state.registry)?;
        if let Some(agent) = &agent {
            registry.set_agent(&record.id, agent)?;
        } else if let Some(activity) = &activity {
            let identity = AgentInfo {
                kind: activity.agent.clone(),
                agent_session_id: Some(activity.conversation_id.clone()),
                agent_session_path: record
                    .agent
                    .as_ref()
                    .filter(|agent| {
                        agent.kind == activity.agent
                            && agent.agent_session_id.as_deref()
                                == Some(activity.conversation_id.as_str())
                    })
                    .and_then(|agent| agent.agent_session_path.clone()),
            };
            if record.agent.as_ref() != Some(&identity) {
                registry.set_agent(&record.id, &identity)?;
            }
        }
        if let Some(argv) = resume {
            registry.set_resume(
                &record.id,
                &ResumeSpec {
                    argv,
                    source: ResumeSource::ReportedByAgent,
                },
            )?;
        }
        // Identity-only reports (agent, resume) keep the last semantic report.
        let reported =
            (lifecycle.is_some() || text.is_some() || result.is_some()).then_some(Reported {
                state: lifecycle,
                text,
                result,
                at: now / 1000,
            });
        if let Some(reported) = &reported {
            registry.set_reported(&record.id, reported)?;
            *state.report_seq.entry(record.id.clone()).or_default() += 1;
        }
        if let (Some(lifecycle), Some(live)) = (lifecycle, state.live.get_mut(&record.id)) {
            live.tracker.on_report(lifecycle, now);
        }
        if let (Some(activity), Some(live)) = (&activity, state.live.get_mut(&record.id)) {
            live.tracker.on_activity(activity, now);
        }
        if let Some(reported) = reported {
            self.publish(
                &record.scope,
                &record.id,
                SessionEvent::Reported { reported },
            );
        }
        self.refresh(state, &record.id);
        self.changed.notify_all();
        Ok(())
    }

    /// Live terminal → host read; otherwise the saved scrollback. Never marks
    /// a `done` session as seen.
    fn read(&self, caller: &Caller, request: ReadRequest) -> Result<Response, ApiError> {
        let (record, terminal) = {
            let state = self.lock();
            let record = self.target(&state, caller, Op::Read, &request.id)?;
            let terminal = self.runtime(&state, &record).terminal;
            (record, terminal)
        };
        if let Some(terminal) = terminal {
            match self
                .host
                .read(terminal, request.source, request.lines, request.format)
            {
                Ok(mut result) => {
                    result.untrusted = true;
                    return Ok(Response::Read(result));
                }
                // Forgotten since we looked: fall back to the saved output.
                Err(HostError::NotFound) => {}
                Err(error) => return Err(host_error(error)),
            }
        }
        let saved = self.store.load(&record.id).map_err(|e| {
            ApiError::new(ErrorCode::Internal, format!("reading saved output: {e}"))
        })?;
        Ok(Response::Read(saved::read(
            saved.as_ref(),
            request.source,
            request.lines,
            request.format,
        )))
    }

    /// Block until the condition holds, can no longer hold, or the timeout
    /// passes. Never marks a `done` session as seen.
    fn wait(&self, caller: &Caller, request: WaitRequest) -> Result<Response, ApiError> {
        let pattern = match (request.until, request.pattern.as_deref()) {
            (WaitUntil::Output, None | Some("")) => {
                return Err(invalid("waiting for output needs a non-empty pattern"))
            }
            (_, pattern) => pattern,
        };
        // The client's own limit (e.g. an MCP host's tool timeout) caps the
        // wait; the caller is told to wait again rather than being cut off.
        let client_limit = caller.max_blocking_ms.map(Duration::from_millis);
        let wanted = Duration::from_millis(request.timeout_ms).min(waits::MAX_WAIT);
        let capped = client_limit.is_some_and(|limit| limit < wanted);
        let deadline = Instant::now() + client_limit.map_or(wanted, |limit| wanted.min(limit));
        let mut state = self.lock();
        self.target(&state, caller, Op::Wait, &request.id)?;
        let start_seq = state.report_seq.get(&request.id).copied().unwrap_or(0);
        loop {
            if self.stopping() {
                return Err(shutting_down());
            }
            let record = registry(&state.registry)?
                .get(&request.id)?
                .ok_or_else(|| ApiError::new(ErrorCode::NotFound, "the session was deleted"))?;
            let runtime = self.runtime(&state, &record);
            let lifecycle = self.lifecycle(&state, &record.id, runtime.status);
            let screen = match (request.until, runtime.terminal) {
                (WaitUntil::Output, Some(terminal)) => self
                    .host
                    .read(terminal, ReadSource::Visible, None, TextFormat::Text)
                    .ok()
                    .map(|r| r.text),
                _ => None,
            };
            let observed = Observed {
                status: runtime.status,
                lifecycle,
                report_seq: state.report_seq.get(&request.id).copied().unwrap_or(0),
                screen: screen.as_deref(),
            };
            let finish = |matched: bool, reason: String| {
                Ok(Response::Wait(WaitResult {
                    matched,
                    status: runtime.status,
                    lifecycle,
                    reported: record.reported.clone(),
                    reason,
                }))
            };
            match waits::evaluate(request.until, pattern, start_seq, &observed) {
                Outcome::Matched(reason) => return finish(true, reason),
                Outcome::Impossible(reason) => return finish(false, reason),
                Outcome::Pending => {}
            }
            let now = Instant::now();
            if now >= deadline {
                let reason = match client_limit {
                    Some(limit) if capped => format!(
                        "stopped after {} ms (the client's limit); wait again to keep waiting",
                        limit.as_millis()
                    ),
                    _ => format!("timeout after {} ms", request.timeout_ms),
                };
                return finish(false, reason);
            }
            let slice = (deadline - now).min(waits::POLL);
            state = self
                .changed
                .wait_timeout(state, slice)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }

    fn send(&self, caller: &Caller, request: SendRequest) -> Result<Response, ApiError> {
        let state = self.lock();
        let record = self.target(&state, caller, Op::Send, &request.id)?;
        let result = if request.keys.is_empty() && request.text.as_deref().is_none_or(str::is_empty)
        {
            Err(invalid("nothing to send: give keys or text"))
        } else {
            self.running_terminal(&state, &record.id)
                .ok_or_else(|| not_live(&record.id))
                .and_then(|terminal| {
                    self.host
                        .send_keys(terminal, &request.keys, request.text.as_deref())
                        .map_err(host_error)
                })
        };
        let mut detail = request.keys.join(" ");
        if let Some(text) = &request.text {
            if !detail.is_empty() {
                detail.push(' ');
            }
            detail.push_str(text);
        }
        self.audit_outcome(
            &state,
            caller,
            Op::Send,
            Some(&record.scope),
            Some(&record.id),
            Some(audit::clip(&detail)),
            &result,
        );
        result.map(|()| Response::Ok)
    }

    /// `session_submit`; with `quiet`, refused while someone typed in the
    /// session that recently (not audited: the caller tries again).
    fn submit(
        &self,
        caller: &Caller,
        id: &SessionId,
        text: &str,
        quiet: Option<Duration>,
    ) -> Result<Response, ApiError> {
        let state = self.lock();
        let record = self.target(&state, caller, Op::Submit, id)?;
        let terminal = self.running_terminal(&state, id);
        let written = terminal.map(|terminal| match quiet {
            Some(quiet) => self.host.submit_when_quiet(terminal, text, quiet),
            None => self.host.submit(terminal, text),
        });
        if let Some(Err(HostError::Typing)) = written {
            return Err(host_error(HostError::Typing));
        }
        let result = written
            .ok_or_else(|| not_live(id))
            .and_then(|written| written.map_err(host_error));
        self.audit_outcome(
            &state,
            caller,
            Op::Submit,
            Some(&record.scope),
            Some(id),
            Some(audit::clip(text)),
            &result,
        );
        result.map(|()| Response::Ok)
    }

    /// Mark a session as just used: list ordering, and `done` → `idle`.
    /// `session_turn`: the live agent's current turn, for user callers.
    fn turn(&self, caller: &Caller, id: &SessionId) -> Result<Response, ApiError> {
        let state = self.lock();
        let record = self.target(&state, caller, Op::Turn, id)?;
        let turn = state
            .live
            .get(&record.id)
            .and_then(|live| live.turn.clone())
            .unwrap_or_default();
        Ok(Response::Turn {
            turn: session_core::api::TurnContent {
                id: turn.id,
                prompt: turn.prompt,
                messages: turn.messages.into(),
                total: turn.total,
            },
        })
    }

    /// `session_transcript`: the live agent's conversation after `after`, for
    /// user callers. Reads files of other programs, so it reads without the
    /// state lock.
    fn transcript(
        &self,
        caller: &Caller,
        id: &SessionId,
        after: Option<&session_core::api::TranscriptCursor>,
        max_bytes: Option<u32>,
    ) -> Result<Response, ApiError> {
        let source = {
            let state = self.lock();
            let record = self.target(&state, caller, Op::Transcript, id)?;
            let live = state.live.get(&record.id).ok_or_else(|| {
                ApiError::new(ErrorCode::SessionNotLive, "the session isn't running")
            })?;
            live.output
                .as_ref()
                .and_then(|follower| follower.transcript_source())
        };
        let source = source.ok_or_else(|| {
            ApiError::new(
                ErrorCode::NotFound,
                "no agent conversation is known in this session",
            )
        })?;
        let transcript = source.read(after, max_bytes).ok_or_else(|| {
            ApiError::new(
                ErrorCode::Internal,
                "the agent's conversation couldn't be read",
            )
        })?;
        Ok(Response::Transcript { transcript })
    }

    fn touch(&self, caller: &Caller, id: &SessionId) -> Result<Response, ApiError> {
        let mut state = self.lock();
        let record = self.target(&state, caller, Op::Touch, id)?;
        let result = registry(&state.registry)
            .and_then(|r| r.touch(id, now_ms()).map_err(ApiError::from))
            .map(|()| self.mark_seen(&mut state, id));
        // The TUI touches on activation, not observer previews; only agents' touches are audited.
        if !caller.is_user() {
            self.audit_outcome(
                &state,
                caller,
                Op::Touch,
                Some(&record.scope),
                Some(id),
                None,
                &result,
            );
        }
        result.map(|()| Response::Ok)
    }

    fn pin(&self, caller: &Caller, id: &SessionId, pinned: bool) -> Result<Response, ApiError> {
        let mut state = self.lock();
        let record = self.target(&state, caller, Op::Pin, id)?;
        let result = registry(&state.registry)
            .and_then(|r| r.set_pinned(id, pinned).map_err(ApiError::from));
        if result.is_ok() {
            self.refresh(&mut state, id);
        }
        self.audit_outcome(
            &state,
            caller,
            Op::Pin,
            Some(&record.scope),
            Some(id),
            Some(if pinned { "pin" } else { "unpin" }.to_string()),
            &result,
        );
        result?;
        self.current_view(&state, caller, id)
            .map(|session| Response::Session { session })
    }

    /// Bind conversations of the calling session's agent to it.
    fn bind(
        &self,
        caller: &Caller,
        agent: &str,
        conversations: Vec<String>,
    ) -> Result<Response, ApiError> {
        let Some(id) = caller.session_id().cloned() else {
            return Err(invalid(
                "only a session can bind conversations, and only to itself",
            ));
        };
        policy::authorize(caller, Op::Bind, None)?;
        let result = validate_binding(agent, &conversations);
        let mut state = self.lock();
        if result.is_ok() {
            let State {
                bindings,
                binding_seq,
                ..
            } = &mut *state;
            let list = bindings.entry(id.clone()).or_default();
            for conversation in &conversations {
                list.retain(|b| !(b.agent == agent && &b.conversation == conversation));
                *binding_seq += 1;
                list.push_back(Binding {
                    agent: agent.to_owned(),
                    conversation: conversation.clone(),
                    seq: *binding_seq,
                });
            }
            while list.len() > MAX_BINDINGS_PER_SESSION {
                list.pop_front();
            }
        }
        let detail = format!("{agent}: {}", conversations.join(" "));
        self.audit_outcome(
            &state,
            caller,
            Op::Bind,
            own_scope(caller),
            Some(&id),
            Some(audit::clip(&detail)),
            &result,
        );
        result.map(|()| Response::Ok)
    }

    /// Every project for user callers; a session caller's own for sessions.
    fn relocate_project(
        &self,
        caller: &Caller,
        from: &ScopeRoot,
        to: &ScopeRoot,
    ) -> Result<Response, ApiError> {
        let mut state = self.lock();
        let result = policy::authorize(caller, Op::ProjectRelocate, None).and_then(|()| {
            validate_scope(Some(to.clone()))?;
            if !from.path().is_absolute() || from == to {
                return Err(invalid("source must be a different absolute folder"));
            }
            if from.path().exists()
                && from
                    .path()
                    .canonicalize()
                    .map_err(|error| invalid(error.to_string()))?
                    != to.path()
            {
                return Err(invalid(
                    "source still exists and does not point to the moved folder",
                ));
            }
            let changed = registry(&state.registry)?.relocate_project(from, to)?;
            for live in state.live.values_mut() {
                if live.scope == *from {
                    live.scope = to.clone();
                }
            }
            for id in changed {
                state.git.remove(&id);
                self.refresh(&mut state, &id);
            }
            self.changed.notify_all();
            Ok(())
        });
        self.audit_outcome(
            &state,
            caller,
            Op::ProjectRelocate,
            Some(to),
            None,
            Some(audit::clip(&format!(
                "{} -> {}",
                from.path().display(),
                to.path().display()
            ))),
            &result,
        );
        result.map(|()| Response::Ok)
    }

    fn projects(&self, caller: &Caller) -> Result<Response, ApiError> {
        let mut projects = {
            let state = self.lock();
            policy::authorize(caller, Op::ProjectList, None)?;
            let registry = registry(&state.registry)?;
            let sessions = registry.project_sessions()?;
            let live = self.live_counts(&state);
            registry
                .projects()?
                .into_iter()
                .filter(|project| policy::check_scope(caller, &project.path).is_ok())
                .map(|project| project_view(&project, sessions.get(&project.path), &live))
                .collect::<Vec<_>>()
        };
        // Git files are read without holding the state lock.
        for project in &mut projects {
            project.git = git::detect(project.path.path());
        }
        Ok(Response::Projects { projects })
    }

    fn add_project(&self, caller: &Caller, path: &Path) -> Result<Response, ApiError> {
        let mut project = {
            let state = self.lock();
            let detail = Some(audit::clip(&path.display().to_string()));
            if let Err(error) = policy::authorize(caller, Op::ProjectAdd, None) {
                self.audit(
                    &state,
                    caller,
                    Op::ProjectAdd,
                    own_scope(caller),
                    None,
                    Decision::Denied,
                    detail.map(|d| format!("{d}: {}", error.message)),
                );
                return Err(error);
            }
            let result = project_dir(path).and_then(|dir| {
                let registry = registry(&state.registry)?;
                registry.add_project(&dir, now_ms())?;
                registry
                    .project(&dir)?
                    .ok_or_else(|| ApiError::new(ErrorCode::Internal, "the folder was not saved"))
            });
            let scope = result.as_ref().ok().map(|project| project.path.clone());
            self.audit_outcome(
                &state,
                caller,
                Op::ProjectAdd,
                scope.as_ref(),
                None,
                detail,
                &result,
            );
            let project = result?;
            let sessions = registry(&state.registry)?.project_sessions()?;
            project_view(
                &project,
                sessions.get(&project.path),
                &self.live_counts(&state),
            )
        };
        project.git = git::detect(project.path.path());
        Ok(Response::Project { project })
    }

    fn remove_project(&self, caller: &Caller, path: &ScopeRoot) -> Result<Response, ApiError> {
        let state = self.lock();
        if let Err(error) = policy::authorize(caller, Op::ProjectRemove, None) {
            self.audit(
                &state,
                caller,
                Op::ProjectRemove,
                Some(path),
                None,
                Decision::Denied,
                Some(error.message.clone()),
            );
            return Err(error);
        }
        let shown = path.path().display();
        let result = registry(&state.registry).and_then(|r| match r.remove_project(path)? {
            ProjectRemoval::Removed => Ok(()),
            ProjectRemoval::NotFound => Err(ApiError::new(
                ErrorCode::NotFound,
                format!("no folder {shown}"),
            )),
            ProjectRemoval::HasSessions(count) => Err(invalid(format!(
                "folder {shown} still has {count} session{}; delete {} first",
                if count == 1 { "" } else { "s" },
                if count == 1 { "it" } else { "them" },
            ))),
        });
        self.audit_outcome(
            &state,
            caller,
            Op::ProjectRemove,
            Some(path),
            None,
            None,
            &result,
        );
        result.map(|()| Response::Ok)
    }

    // ------------------------------------------------------------ host events

    pub(super) fn on_host_event(&self, event: HostEvent) {
        match event {
            HostEvent::Output { id, session_id } => self.on_output(id, &session_id),
            HostEvent::Exited {
                id,
                session_id,
                code,
            } => self.on_exited(id, &session_id, code),
            HostEvent::Cwd {
                id,
                session_id,
                path,
            } => {
                // Read the git files before taking the lock.
                let git = git::detect(&path);
                let place = self.resolve_places(vec![path.clone()]);
                let mut state = self.lock();
                if !state.is_current(&session_id, id) {
                    return;
                }
                self.note_places(&mut state, &session_id, place);
                if let Err(error) = registry(&state.registry)
                    .and_then(|r| r.set_cwd(&session_id, &path).map_err(ApiError::from))
                {
                    crate::log_warn!("control: cwd of {session_id}: {error}");
                }
                state.git.set(session_id.clone(), path, git);
                self.refresh(&mut state, &session_id);
            }
            HostEvent::Title { id, session_id, .. } => {
                let mut state = self.lock();
                if state.is_current(&session_id, id) {
                    self.refresh(&mut state, &session_id);
                }
            }
            HostEvent::ControllerChanged {
                id,
                session_id,
                controlled,
            } => {
                let mut state = self.lock();
                if !state.is_current(&session_id, id) {
                    return;
                }
                if controlled {
                    // A promoted observer has seen the session too.
                    self.mark_seen(&mut state, &session_id);
                } else {
                    self.refresh(&mut state, &session_id);
                }
            }
            HostEvent::Bell { .. } => {}
        }
    }

    /// Re-evaluate lifecycles whose self-report went stale, and re-read git
    /// contexts when their poll is due.
    pub(super) fn on_tick(&self) {
        let now = now_ms();
        let mut state = self.lock();
        let changed: Vec<SessionId> = state
            .live
            .iter_mut()
            .filter_map(|(id, live)| live.tracker.tick(now).then(|| id.clone()))
            .collect();
        for id in &changed {
            self.refresh(&mut state, id);
        }
        if !changed.is_empty() {
            self.changed.notify_all();
        }
        let due = {
            let inner = &mut *state;
            let live = &inner.live;
            inner.git.poll_due(now, |id| live.contains_key(id))
        };
        let replies: Vec<(SessionId, TerminalId)> = state
            .live
            .iter()
            .filter(|(_, live)| live.reply_due.is_some())
            .map(|(id, live)| (id.clone(), live.terminal))
            .collect();
        // Long conversations are read on while their agent is quiet.
        let behind: Vec<(SessionId, TerminalId)> = state
            .live
            .iter()
            .filter(|(_, live)| {
                live.places_scan
                    .as_ref()
                    .is_some_and(crate::agent_output::places::PlaceScan::behind)
                    && now.saturating_sub(live.places_checked_ms) >= PLACES_CHECK_MS
            })
            .map(|(id, live)| (id.clone(), live.terminal))
            .collect();
        // The files are read without the lock.
        drop(state);
        if let Some(targets) = due {
            self.poll_git(targets);
        }
        for (id, terminal) in behind {
            self.scan_places(terminal, &id, now);
            self.refresh(&mut self.lock(), &id);
        }
        // A finished turn's reply may reach its log after the screen settles.
        for (id, terminal) in replies {
            let process = self.host.info(terminal).and_then(|info| info.foreground);
            self.follow_output(terminal, &id, process.as_ref(), now);
            self.refresh(&mut self.lock(), &id);
        }
    }

    /// Re-read the git context of every running session (a checkout changes
    /// HEAD without a cwd change); sessions whose context changed get
    /// `Updated`. Files are read without holding the state lock.
    fn poll_git(&self, targets: Vec<(SessionId, PathBuf)>) {
        let readings = git_context::read_all(targets);
        let mut state = self.lock();
        for id in state.git.apply(readings) {
            self.refresh(&mut state, &id);
        }
    }

    fn on_output(&self, terminal: TerminalId, session_id: &SessionId) {
        let detection = self.detect(terminal);
        let process = self.host.info(terminal).and_then(|info| info.foreground);
        let now = now_ms();
        let plan = self.resume_plan(terminal, session_id, now);
        self.follow_output(terminal, session_id, process.as_ref(), now);
        self.scan_places(terminal, session_id, now);
        let mut state = self.lock();
        if let Some(plan) = plan {
            self.record_resume(&mut state, session_id, terminal, plan, now);
        }
        let Some(live) = state
            .live
            .get_mut(session_id)
            .filter(|live| live.terminal == terminal)
        else {
            return;
        };
        if let Some(detection) = detection {
            let changed_agent = live.working_directory.as_ref().is_some_and(|context| {
                detection.agent.as_deref() != Some(context.agent.as_str())
                    || matches!((live.working_directory_pid, process.as_ref()), (Some(pid), Some(process)) if pid != process.pid)
            });
            if changed_agent {
                live.working_directory = None;
                live.working_directory_pid = None;
            }
            live.tracker.on_detection(detection, now);
        }
        let activity = now.saturating_sub(live.last_activity_ms) >= ACTIVITY_INTERVAL_MS;
        if activity {
            live.last_activity_ms = now;
        }
        let scope = live.scope.clone();
        if activity {
            self.publish(&scope, session_id, SessionEvent::Activity);
        }
        self.refresh(&mut state, session_id);
        self.changed.notify_all();
    }

    /// Read the running agent's conversation log for its latest line, at
    /// most every [`OUTPUT_CHECK_MS`] (sooner when a finished turn's reply is
    /// due): the log is found again when the foreground process changes, and
    /// dropped when it isn't a known agent.
    /// Reads files of other programs, so it runs without the state lock; a
    /// changed line is published by the caller's `refresh`.
    fn follow_output(
        &self,
        terminal: TerminalId,
        session_id: &SessionId,
        process: Option<&crate::host::ProcessInfo>,
        now: i64,
    ) {
        let (follower, agent, bound) = {
            let mut state = self.lock();
            let Some(live) = state
                .live
                .get_mut(session_id)
                .filter(|live| live.terminal == terminal)
            else {
                return;
            };
            let reply_read = live
                .reply_due
                .is_some_and(|due| live.output_checked_ms < due.since);
            if !reply_read && now.saturating_sub(live.output_checked_ms) < OUTPUT_CHECK_MS {
                return;
            }
            live.output_checked_ms = now;
            let agent = live.tracker.detected_agent().map(String::from);
            let output = live.output.take();
            // The OpenCode conversation its plugin bound here, if any.
            let bound = state.bindings.get(session_id).and_then(|bindings| {
                bindings
                    .iter()
                    .filter(|binding| binding.agent == "opencode")
                    .max_by_key(|binding| binding.seq)
                    .map(|binding| binding.conversation.clone())
            });
            (output, agent, bound)
        };
        let follower = match (process, agent) {
            (Some(process), Some(agent)) => {
                let mut follower = follower
                    .filter(|follower| follower.pid() == process.pid)
                    .or_else(|| crate::agent_output::Follower::locate(process, &agent));
                if let Some(follower) = follower.as_mut() {
                    follower.follow(bound.as_deref());
                }
                follower
            }
            _ => None,
        };
        let mut state = self.lock();
        if let Some(live) = state
            .live
            .get_mut(session_id)
            .filter(|live| live.terminal == terminal)
        {
            live.agent_output = follower
                .as_ref()
                .and_then(|follower| follower.latest().map(String::from));
            live.user_prompt = follower
                .as_ref()
                .and_then(|follower| follower.prompt().map(String::from));
            live.turn = follower
                .as_ref()
                .map(|follower| follower.turn().clone())
                .filter(|turn| turn.prompt.is_some() || turn.total > 0);
            live.output = follower;
        }
        self.capture_reply(&mut state, session_id, now);
    }

    /// Read on in the running agent's conversation for where it works (a
    /// long one from its start, [`PLACES_BUDGET`] at a time), at most every
    /// [`PLACES_CHECK_MS`]: from its start again when the agent shows
    /// another conversation. Reads files of other programs, so it runs
    /// without the state lock; a change is published by the caller's
    /// `refresh`.
    fn scan_places(&self, terminal: TerminalId, session_id: &SessionId, now: i64) {
        let (scan, source, pid) = {
            let mut state = self.lock();
            let Some(live) = state
                .live
                .get_mut(session_id)
                .filter(|live| live.terminal == terminal)
            else {
                return;
            };
            if now.saturating_sub(live.places_checked_ms) < PLACES_CHECK_MS {
                return;
            }
            let follower = live.output.as_ref();
            let Some(source) = follower.and_then(|follower| follower.transcript_source()) else {
                live.places_scan = None;
                return;
            };
            live.places_checked_ms = now;
            let pid = follower.map(|follower| follower.pid());
            (live.places_scan.take(), source, pid)
        };
        let mut scan = match scan {
            Some(scan) if *scan.source() == source => scan,
            _ => crate::agent_output::places::PlaceScan::new(
                source,
                pid.and_then(crate::procinfo::cwd_of),
            ),
        };
        let touches = scan.read(PLACES_BUDGET);
        let worked = places::resolve(touches, &self.places_excluded, now);
        let mut state = self.lock();
        let Some(live) = state
            .live
            .get_mut(session_id)
            .filter(|live| live.terminal == terminal)
        else {
            return;
        };
        live.places_scan = Some(scan);
        self.note_places(&mut state, session_id, worked);
    }

    /// The places of directories worked in now. Reads files.
    fn resolve_places(&self, dirs: Vec<PathBuf>) -> Vec<places::Resolved> {
        let touches = dirs
            .into_iter()
            .map(|dir| crate::agent_output::places::Touch {
                dir,
                edited: false,
                at: None,
            })
            .collect();
        places::resolve(touches, &self.places_excluded, now_ms())
    }

    /// Record that `id` worked in `worked`, saving its list when it
    /// changed (the caller refreshes the view).
    fn note_places(&self, state: &mut State, id: &SessionId, worked: Vec<places::Resolved>) {
        let Some(list) = state.places.apply(id, worked) else {
            return;
        };
        let saved = registry(&state.registry)
            .and_then(|registry| registry.set_places(id, list).map_err(ApiError::from));
        if let Err(error) = saved {
            crate::log_warn!("control: places of {id}: {error}");
        }
    }

    /// What to record as `session_id`'s resume command (`resume::plan`),
    /// when it's time to look again. Reads files of other programs, so it
    /// runs without the state lock.
    fn resume_plan(
        &self,
        terminal: TerminalId,
        session_id: &SessionId,
        now: i64,
    ) -> Option<crate::resume::Plan> {
        let process = self.host.info(terminal)?.foreground?;
        let opencode = {
            let mut state = self.lock();
            let live = state
                .live
                .get_mut(session_id)
                .filter(|live| live.terminal == terminal)?;
            let due = live.resume_checked_pid != Some(process.pid)
                || now.saturating_sub(live.resume_checked_ms) >= RESUME_CHECK_MS;
            if !due {
                return None;
            }
            live.resume_checked_ms = now;
            live.resume_checked_pid = Some(process.pid);
            state.bindings.get(session_id).and_then(|bindings| {
                bindings
                    .iter()
                    .filter(|b| b.agent == "opencode")
                    .max_by_key(|b| b.seq)
                    .map(|b| b.conversation.clone())
            })
        };
        let cwd = crate::procinfo::cwd_of(process.pid);
        Some(crate::resume::plan(&crate::resume::Context {
            process: &process,
            cwd: cwd.as_deref(),
            opencode_conversation: opencode.as_deref(),
        }))
    }

    /// Bring the registry's resume command in line with `plan`. An agent's
    /// own report stays while an agent runs; a shell must have sat at its
    /// prompt for [`SHELL_SETTLE_MS`] before the command is forgotten.
    fn record_resume(
        &self,
        state: &mut State,
        session_id: &SessionId,
        terminal: TerminalId,
        plan: crate::resume::Plan,
        now: i64,
    ) {
        use crate::resume::Plan;
        let Some(live) = state
            .live
            .get_mut(session_id)
            .filter(|live| live.terminal == terminal)
        else {
            return;
        };
        let (wanted, agent) = match plan {
            Plan::Unresolved => {
                live.shell_since_ms = None;
                return;
            }
            Plan::Resume(argv) => (
                Some(ResumeSpec {
                    argv,
                    source: ResumeSource::KnownAgent,
                }),
                true,
            ),
            Plan::Command(argv) => (
                Some(ResumeSpec {
                    argv,
                    source: ResumeSource::LastCommand,
                }),
                false,
            ),
            Plan::Shell => {
                let since = *live.shell_since_ms.get_or_insert(now);
                if now.saturating_sub(since) < SHELL_SETTLE_MS {
                    return;
                }
                (None, false)
            }
        };
        if wanted.is_some() {
            live.shell_since_ms = None;
        }
        if live.recorded_resume.as_ref() == Some(&wanted) {
            return;
        }
        let Ok(registry) = registry(&state.registry) else {
            return;
        };
        let current = match registry.get(session_id) {
            Ok(Some(record)) => record.resume,
            _ => return,
        };
        let reported = current
            .as_ref()
            .is_some_and(|spec| spec.source == ResumeSource::ReportedByAgent);
        let keep = (agent && reported) || current == wanted;
        if !keep {
            let written = match &wanted {
                Some(spec) => registry.set_resume(session_id, spec),
                None => registry.clear_resume(session_id),
            };
            if let Err(error) = written {
                crate::log_warn!("control: resume command of {session_id}: {error}");
                return;
            }
            crate::log_debug!(
                "control: resume command of {session_id}: {:?}",
                wanted.as_ref().map(|spec| (&spec.argv, spec.source))
            );
        }
        if let Some(live) = state.live.get_mut(session_id) {
            live.recorded_resume = Some(if keep { current } else { wanted });
        }
    }

    /// Write which sessions run to `running.json` in the Rakumux home, so
    /// the next daemon start can revive them if this one ends without the
    /// user stopping it (a crash, a reboot).
    fn save_running(&self, state: &State) {
        if self.stopping() || self.stopped_by_user.load(Ordering::SeqCst) {
            return;
        }
        let mut running: Vec<&SessionId> = state
            .live
            .keys()
            .filter(|id| self.running_terminal(state, id).is_some())
            .collect();
        running.sort();
        let path = self.config.layout.home.join(RUNNING_FILE);
        let json = serde_json::json!({ "sessions": running });
        if let Err(error) = crate::persist::write_atomic(&path, json.to_string().as_bytes()) {
            crate::log_warn!("control: {}: {error}", path.display());
        }
    }

    /// The user stopped the daemon (`daemon_shutdown`): the sessions ended
    /// on purpose, so the next start revives none.
    pub(super) fn stopped_by_user(&self) {
        self.stopped_by_user.store(true, Ordering::SeqCst);
        let path = self.config.layout.home.join(RUNNING_FILE);
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => crate::log_warn!("control: {}: {e}", path.display()),
        }
    }

    /// Revive the sessions the previous daemon left running when it ended
    /// without the user stopping it (`running.json`), each with its resume
    /// command. Returns how many came back.
    pub(super) fn revive_interrupted(&self) -> usize {
        let path = self.config.layout.home.join(RUNNING_FILE);
        let ids: Vec<SessionId> = match std::fs::read(&path) {
            Ok(raw) => serde_json::from_slice::<serde_json::Value>(&raw)
                .ok()
                .and_then(|value| value.get("sessions").cloned())
                .and_then(|sessions| serde_json::from_value(sessions).ok())
                .unwrap_or_default(),
            Err(_) => return 0,
        };
        let caller = Caller {
            info: session_core::api::CallerInfo::User,
            client: "rakumux-ptyd".into(),
            max_blocking_ms: None,
        };
        let mut state = self.lock();
        let mut revived = 0;
        for id in ids {
            let record = match registry(&state.registry).map(|r| r.get(&id)) {
                Ok(Ok(Some(record))) => record,
                _ => continue,
            };
            if self.runtime(&state, &record).status != SessionStatus::Dead {
                continue;
            }
            let request = ReviveSession {
                id: id.clone(),
                env: Vec::new(),
                cols: 0,
                rows: 0,
                resume: true,
            };
            let result = self.revive_session(&mut state, &caller, &record, request);
            self.audit_outcome(
                &state,
                &caller,
                Op::Revive,
                Some(&record.scope),
                Some(&id),
                Some("after the daemon stopped unexpectedly".into()),
                &result,
            );
            match result {
                Ok(()) => revived += 1,
                Err(error) => crate::log_warn!("control: reviving {id}: {}", error.message),
            }
        }
        self.save_running(&state);
        if revived > 0 {
            crate::log_info!("control: revived {revived} session(s) that were running");
        }
        revived
    }

    fn on_exited(&self, terminal: TerminalId, session_id: &SessionId, code: Option<i32>) {
        let detection = self.detect(terminal);
        let now = now_ms();
        let mut state = self.lock();
        let Some(live) = state
            .live
            .get_mut(session_id)
            .filter(|live| live.terminal == terminal)
        else {
            return;
        };
        if let Some(detection) = detection {
            live.tracker.on_detection(detection, now);
        }
        let scope = live.scope.clone();
        // Its agent is gone; a revived one binds its conversations again.
        state.bindings.remove(session_id);
        if let Err(error) = registry(&state.registry)
            .and_then(|r| r.mark_exited(session_id, code, now).map_err(ApiError::from))
        {
            crate::log_warn!("control: exit of {session_id}: {error}");
        }
        self.publish(&scope, session_id, SessionEvent::Exited { code });
        self.refresh(&mut state, session_id);
        self.save_running(&state);
        self.changed.notify_all();
    }

    /// Run detection on the terminal's visible screen and foreground
    /// process. Called without the state lock held.
    fn detect(&self, terminal: TerminalId) -> Option<Detection> {
        let info = self.host.info(terminal)?;
        let screen = self
            .host
            .read(terminal, ReadSource::Visible, None, TextFormat::Text)
            .map(|r| r.text)
            .unwrap_or_default();
        let ms_since_output = info
            .last_output_ms
            .map(|at| u64::try_from(now_ms().saturating_sub(at)).unwrap_or(0));
        Some((self.detector)(&DetectInput {
            screen: &screen,
            foreground: info.foreground.as_ref(),
            alternate_screen: info.alternate_screen,
            title: &info.title,
            ms_since_output,
        }))
    }

    // ------------------------------------------------------------ helpers

    /// Issue a token, build the child environment and spawn the terminal.
    /// On success the session is live.
    #[allow(clippy::too_many_arguments)]
    fn start_terminal(
        &self,
        state: &mut State,
        record: &SessionRecord,
        argv: Vec<String>,
        base_env: Vec<(String, String)>,
        cols: u16,
        rows: u16,
        resume: Option<&ResumeSpec>,
    ) -> Result<(), ApiError> {
        let token = state.tokens.issue(&record.id);
        let child_env = env::child_env(
            &base_env,
            &Identity {
                session_id: &record.id,
                token: &token,
                socket: &self.config.socket,
                rakumux_bin: self.config.rakumux_bin.as_deref(),
            },
        );
        let spec = SpawnSpec {
            session_id: record.id.clone(),
            argv,
            cwd: record.cwd.clone(),
            env: child_env.clone(),
            cols,
            rows,
            startup_input: env::startup_input(
                &record.launch.startup_cmds,
                resume.map(|spec| (spec.argv.as_slice(), spec.source.runs())),
            ),
        };
        match self.host.spawn(spec) {
            Ok(info) => {
                state.live.insert(
                    record.id.clone(),
                    LiveSession::new(info.id, record.scope.clone()),
                );
                // Callers publish the session afterwards.
                let started = self.resolve_places(vec![record.cwd.clone()]);
                self.note_places(state, &record.id, started);
                state.spawn_env.insert(record.id.clone(), child_env);
                self.save_running(state);
                Ok(())
            }
            Err(error) => {
                state.tokens.revoke(&record.id);
                Err(host_error(error))
            }
        }
    }

    fn inherited_env(
        &self,
        state: &State,
        caller: &Caller,
        request_env: &[(String, String)],
    ) -> Vec<(String, String)> {
        let caller_env = caller
            .session_id()
            .and_then(|id| state.spawn_env.get(id))
            .map(Vec::as_slice);
        env::inherited_env(request_env, caller_env)
    }

    /// The calling session's terminal size, if it has a terminal.
    fn caller_size(&self, state: &State, caller: &Caller) -> Option<(u16, u16)> {
        let live = state.live.get(caller.session_id()?)?;
        let info = self.host.info(live.terminal)?;
        Some((info.cols, info.rows))
    }

    fn running_terminal(&self, state: &State, id: &SessionId) -> Option<TerminalId> {
        let live = state.live.get(id)?;
        let info = self.host.info(live.terminal)?;
        (info.state == TerminalState::Running).then_some(live.terminal)
    }

    /// Running sessions per scope.
    fn live_counts(&self, state: &State) -> HashMap<ScopeRoot, u32> {
        let mut counts = HashMap::new();
        for (id, live) in &state.live {
            if self.running_terminal(state, id).is_some() {
                *counts.entry(live.scope.clone()).or_default() += 1;
            }
        }
        counts
    }

    /// The session was seen (touch, controller attach): `done` → `idle`.
    fn mark_seen(&self, state: &mut State, id: &SessionId) {
        if let Some(live) = state.live.get_mut(id) {
            live.tracker.on_touch(now_ms());
        }
        self.refresh(state, id);
        self.changed.notify_all();
    }

    fn runtime(&self, state: &State, record: &SessionRecord) -> Runtime {
        if let Some(live) = state.live.get(&record.id) {
            if let Some(info) = self.host.info(live.terminal) {
                let terminal = Some(live.terminal);
                return match &info.state {
                    TerminalState::Running => Runtime {
                        status: SessionStatus::Live,
                        exit_code: None,
                        terminal,
                        view: Some(TerminalView {
                            pid: info.pid,
                            foreground_process: info.foreground.as_ref().map(|p| p.name.clone()),
                            title: info.title.clone(),
                            cols: info.cols,
                            rows: info.rows,
                            controlled: info.controlled,
                            alternate_screen: info.alternate_screen,
                        }),
                    },
                    TerminalState::Exited { code } => Runtime {
                        status: SessionStatus::Exited,
                        exit_code: *code,
                        terminal,
                        view: None,
                    },
                    TerminalState::Failed { .. } => Runtime {
                        status: SessionStatus::Exited,
                        exit_code: record.exit_code,
                        terminal,
                        view: None,
                    },
                };
            }
        }
        if record.ended_at_ms.is_some() {
            Runtime {
                status: SessionStatus::Exited,
                exit_code: record.exit_code,
                terminal: None,
                view: None,
            }
        } else {
            Runtime {
                status: SessionStatus::Dead,
                exit_code: None,
                terminal: None,
                view: None,
            }
        }
    }

    fn lifecycle(&self, state: &State, id: &SessionId, status: SessionStatus) -> Lifecycle {
        if status != SessionStatus::Live {
            return Lifecycle::None;
        }
        state
            .live
            .get(id)
            .map_or(Lifecycle::None, |live| live.tracker.current())
    }

    fn view(
        &self,
        state: &State,
        record: &SessionRecord,
        children: Vec<SessionId>,
        self_id: Option<&SessionId>,
    ) -> SessionView {
        let runtime = self.runtime(state, record);
        let lifecycle = self.lifecycle(state, &record.id, runtime.status);
        let detected_agent = match runtime.status {
            SessionStatus::Live => state
                .live
                .get(&record.id)
                .and_then(|live| live.tracker.detected_agent()),
            _ => None,
        };
        // Callers cache the context first (`GitContexts::ensure`); a miss
        // reads it here.
        let directory = self.git_directory(state, record);
        let git = match state.git.get(&record.id, &directory) {
            Some(info) => info.clone(),
            None => git::detect(&directory),
        };
        SessionView {
            agent_output: (runtime.status == SessionStatus::Live)
                .then(|| {
                    state
                        .live
                        .get(&record.id)
                        .and_then(|live| live.agent_output.clone())
                })
                .flatten(),
            agent_turn: (runtime.status == SessionStatus::Live)
                .then(|| {
                    let live = state.live.get(&record.id)?;
                    let turn = live.turn.as_ref()?;
                    Some(session_core::model::AgentTurn {
                        id: turn.id,
                        messages: turn.total,
                        conversation_id: live
                            .output
                            .as_ref()
                            .and_then(|follower| follower.conversation()),
                    })
                })
                .flatten(),
            user_prompt: (runtime.status == SessionStatus::Live)
                .then(|| {
                    state
                        .live
                        .get(&record.id)
                        .and_then(|live| live.user_prompt.clone())
                })
                .flatten(),
            pinned: record.pinned,
            git,
            places: state.places.shown(&record.id),
            id: record.id.clone(),
            name: record.name.clone(),
            scope: record.scope.clone(),
            is_self: self_id == Some(&record.id),
            cwd: record.cwd.clone(),
            status: runtime.status,
            exit_code: runtime.exit_code,
            lifecycle,
            agent: merge_agent(detected_agent, record.agent.as_ref()),
            reported: record.reported.clone(),
            created_by: record.created_by.clone(),
            activity: (runtime.status == SessionStatus::Live)
                .then(|| {
                    state
                        .live
                        .get(&record.id)
                        .and_then(|live| live.tracker.activity())
                })
                .flatten(),
            parent_id: record.parent_id.clone(),
            depth: record.depth,
            children,
            launch: record.launch.clone(),
            resume: record.resume.clone(),
            auto_named: record.auto_named,
            description: record.description.clone(),
            notes: record.notes.clone(),
            labels: record.labels.clone(),
            private: record.private,
            created_at: record.created_at_ms / 1000,
            last_active_at: record.last_active_at_ms / 1000,
            terminal: runtime.view,
        }
    }

    fn current_view(
        &self,
        state: &State,
        caller: &Caller,
        id: &SessionId,
    ) -> Result<SessionView, ApiError> {
        let registry = registry(&state.registry)?;
        let record = registry.get(id)?.ok_or_else(|| not_found(id))?;
        let children = registry.children(id)?;
        Ok(self.view(state, &record, children, caller.session_id()))
    }

    /// Rebuild a session's view and publish `Updated` (and `Lifecycle`) if it
    /// changed since the last publication.
    fn refresh(&self, state: &mut State, id: &SessionId) {
        let lookup = registry(&state.registry).and_then(|r| {
            let record = r.get(id)?;
            let children = r.children(id)?;
            Ok(record.map(|record| (record, children)))
        });
        let (record, children) = match lookup {
            Ok(Some(found)) => found,
            Ok(None) => return,
            Err(error) => {
                crate::log_warn!("control: refreshing {id}: {error}");
                return;
            }
        };
        let directory = self.git_directory(state, &record);
        state.git.ensure(id, &directory);
        let view = self.view(state, &record, children, None);
        let previous = state.views.get(id);
        if previous == Some(&view) {
            return;
        }
        let old_lifecycle = previous.map_or(Lifecycle::None, |v| v.lifecycle);
        let lifecycle = view.lifecycle;
        state.views.insert(id.clone(), view.clone());
        self.publish(&record.scope, id, SessionEvent::Updated { session: view });
        if lifecycle != old_lifecycle {
            self.note_turn(state, id, record.private, lifecycle);
            self.publish(&record.scope, id, SessionEvent::Lifecycle { lifecycle });
        }
    }

    /// Follow turns for reply capture: a turn starts when the agent is seen
    /// working and finishes when its session turns `Done`; a finished turn's
    /// reply is then due, while capture is on and the session isn't private.
    fn note_turn(&self, state: &mut State, id: &SessionId, private: bool, lifecycle: Lifecycle) {
        let capturing = state.replies.enabled() && !private;
        let Some(live) = state.live.get_mut(id) else {
            return;
        };
        match lifecycle {
            Lifecycle::Working => live.turn_started = true,
            Lifecycle::Done if live.turn_started => {
                live.turn_started = false;
                live.reply_due = capturing.then(|| ReplyDue::new(now_ms()));
            }
            _ => {}
        }
    }

    /// Capture the due reply of `id`'s finished turn once its log was read
    /// after the turn finished and shows a reply not captured before; stop
    /// waiting when [`ReplyDue::until`] passes.
    fn capture_reply(&self, state: &mut State, id: &SessionId, now: i64) {
        let State { live, replies, .. } = state;
        let Some(live) = live.get_mut(id) else {
            return;
        };
        let Some(due) = live.reply_due else {
            return;
        };
        if !replies.enabled() {
            live.reply_due = None;
            return;
        }
        let found = live
            .output
            .as_ref()
            .filter(|_| live.output_checked_ms >= due.since)
            .and_then(|follower| {
                let reply = follower.reply()?;
                (live.reply_key.as_deref() != Some(reply.key.as_str()))
                    .then(|| (follower.agent(), follower.conversation(), reply.clone()))
            });
        let Some((agent, conversation_id, reply)) = found else {
            if now >= due.until {
                live.reply_due = None;
            }
            return;
        };
        live.reply_due = None;
        live.reply_key = Some(reply.key);
        let reply = AgentReply {
            id: ReplyId::generate(),
            session_id: id.clone(),
            agent: agent.to_owned(),
            conversation_id,
            text: reply.text,
            truncated: reply.truncated,
            finished_at: due.since,
            position: reply.position,
        };
        let scope = live.scope.clone();
        replies.push(reply.clone(), now);
        self.publish(&scope, id, SessionEvent::Reply { reply });
    }

    /// Turn reply capture on or off (user only); off forgets the replies.
    fn reply_capture(&self, caller: &Caller, enabled: bool) -> Result<Response, ApiError> {
        let mut state = self.lock();
        let result = policy::authorize(caller, Op::ReplyCapture, None);
        if result.is_ok() && state.replies.enabled() != enabled {
            state.replies.set_enabled(enabled);
            if !enabled {
                for live in state.live.values_mut() {
                    live.reply_due = None;
                }
            }
        }
        let detail = Some(if enabled { "on" } else { "off" }.to_owned());
        self.audit_outcome(
            &state,
            caller,
            Op::ReplyCapture,
            None,
            None,
            detail,
            &result,
        );
        result.map(|()| Response::Ok)
    }

    /// The replies captured after `after` (user only).
    fn reply_list(&self, caller: &Caller, after: Option<&ReplyId>) -> Result<Response, ApiError> {
        policy::authorize(caller, Op::ReplyList, None)?;
        let mut state = self.lock();
        let capturing = state.replies.enabled();
        let replies = state.replies.after(after, now_ms());
        Ok(Response::Replies { replies, capturing })
    }

    fn publish(&self, scope: &ScopeRoot, id: &SessionId, event: SessionEvent) {
        self.events.publish(EventEnvelope {
            scope: scope.clone(),
            session_id: id.clone(),
            at: now_ms(),
            event,
        });
    }

    /// An agent's logical worktree wins only while that agent is running.
    /// The registry cwd and authenticated folder remain the terminal's own.
    fn git_directory(&self, state: &State, record: &SessionRecord) -> PathBuf {
        let selected = state.live.get(&record.id).and_then(|live| {
            let context = live.working_directory.as_ref()?;
            let info = self.host.info(live.terminal)?;
            if info.state != TerminalState::Running {
                return None;
            }
            let process = info.foreground;
            let agent = match &process {
                Some(process) => crate::detect::agent_label(process),
                None => live.tracker.detected_agent(),
            };
            let same_process = match (live.working_directory_pid, process.as_ref()) {
                (Some(expected), Some(process)) => expected == process.pid,
                _ => true,
            };
            (agent == Some(context.agent.as_str()) && same_process).then(|| context.path.clone())
        });
        selected.unwrap_or_else(|| record.cwd.clone())
    }

    /// Resolve `id` and check scope + permission. Denials are audited, as
    /// are failed lookups for mutating ops.
    fn target(
        &self,
        state: &State,
        caller: &Caller,
        op: Op,
        id: &SessionId,
    ) -> Result<SessionRecord, ApiError> {
        let Some(record) = registry(&state.registry)?.get(id)? else {
            let error = not_found(id);
            if op.is_mutating() {
                self.audit(
                    state,
                    caller,
                    op,
                    own_scope(caller),
                    Some(id),
                    Decision::Error,
                    Some(error.message.clone()),
                );
            }
            return Err(error);
        };
        let target = Target {
            id: &record.id,
            scope: &record.scope,
        };
        if let Err(error) = policy::authorize(caller, op, Some(target)) {
            self.audit(
                state,
                caller,
                op,
                Some(&record.scope),
                Some(id),
                Decision::Denied,
                Some(error.message.clone()),
            );
            return Err(error);
        }
        Ok(record)
    }

    /// The scopes a list/subscribe applies to: for user callers every scope
    /// with `all` (`scope` and `scopes` are then ignored), else the union of
    /// `scope` and `scopes` (at least one is required); for session callers
    /// their own, and naming any other scope or `all` is `OutOfScope`.
    /// Denials are audited.
    fn caller_scopes(
        &self,
        state: &State,
        caller: &Caller,
        op: Op,
        scope: Option<ScopeRoot>,
        scopes: Vec<ScopeRoot>,
        all: bool,
    ) -> Result<Scopes, ApiError> {
        if all {
            if let Err(error) = policy::check_all_scopes(caller) {
                self.audit(
                    state,
                    caller,
                    op,
                    own_scope(caller),
                    None,
                    Decision::Denied,
                    Some(error.message.clone()),
                );
                return Err(error);
            }
            return Ok(Scopes::All);
        }
        let mut requested: Vec<ScopeRoot> = Vec::new();
        for scope in scope.into_iter().chain(scopes) {
            if !requested.contains(&scope) {
                requested.push(scope);
            }
        }
        if requested.is_empty() {
            match &caller.info {
                CallerInfo::Session { scope, .. } => requested.push(scope.clone()),
                CallerInfo::User => return Err(invalid("scope is required for user clients")),
            }
        }
        for scope in &requested {
            if let Err(error) = policy::check_scope(caller, scope) {
                self.audit(
                    state,
                    caller,
                    op,
                    Some(scope),
                    None,
                    Decision::Denied,
                    Some(error.message.clone()),
                );
                return Err(error);
            }
        }
        Ok(Scopes::Only(requested))
    }

    #[allow(clippy::too_many_arguments)]
    fn audit(
        &self,
        state: &State,
        caller: &Caller,
        op: Op,
        scope: Option<&ScopeRoot>,
        target: Option<&SessionId>,
        decision: Decision,
        detail: Option<String>,
    ) {
        let Some(registry) = state.registry.as_ref() else {
            return;
        };
        let entry = AuditEntry {
            ts_ms: now_ms(),
            scope_root: scope.and_then(|s| s.path().to_str()).map(str::to_owned),
            caller: audit::caller_label(caller),
            method: op.method(),
            target_session_id: target.map(|t| t.as_str().to_owned()),
            decision,
            detail,
        };
        if let Err(error) = registry.insert_audit(&entry) {
            crate::log_warn!("control: audit: {error}");
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn audit_outcome<T>(
        &self,
        state: &State,
        caller: &Caller,
        op: Op,
        scope: Option<&ScopeRoot>,
        target: Option<&SessionId>,
        detail: Option<String>,
        result: &Result<T, ApiError>,
    ) {
        let (decision, detail) = match result {
            Ok(_) => (Decision::Allowed, detail),
            Err(error) => {
                let decision = match error.code {
                    ErrorCode::OutOfScope
                    | ErrorCode::PermissionDenied
                    | ErrorCode::LimitExceeded => Decision::Denied,
                    _ => Decision::Error,
                };
                let text = match detail {
                    Some(detail) => format!("{detail}: {error}"),
                    None => error.to_string(),
                };
                (decision, Some(audit::clip(&text)))
            }
        };
        self.audit(state, caller, op, scope, target, decision, detail);
    }
}

// ---------------------------------------------------------------- free helpers

fn registry(registry: &Option<Registry>) -> Result<&Registry, ApiError> {
    registry.as_ref().ok_or_else(shutting_down)
}

fn registry_of(state: &State) -> Result<&Registry, ApiError> {
    registry(&state.registry)
}

fn own_scope(caller: &Caller) -> Option<&ScopeRoot> {
    match &caller.info {
        CallerInfo::Session { scope, .. } => Some(scope),
        CallerInfo::User => None,
    }
}

/// A detected agent wins over a stale reported identity of another kind;
/// a report of the same kind adds the agent's own session id.
fn merge_agent(detected: Option<&str>, reported: Option<&AgentInfo>) -> Option<AgentInfo> {
    match (detected, reported) {
        (Some(kind), Some(reported)) if reported.kind == kind => Some(reported.clone()),
        (Some(kind), _) => Some(AgentInfo {
            kind: kind.to_string(),
            ..AgentInfo::default()
        }),
        (None, reported) => reported.cloned(),
    }
}

/// User callers name the scope: an existing directory given in canonical
/// form (as `ScopeRoot::resolve` produces).
fn validate_scope(scope: Option<ScopeRoot>) -> Result<ScopeRoot, ApiError> {
    let scope = scope.ok_or_else(|| invalid("scope is required for user clients"))?;
    let path = scope.path();
    let canonical = std::fs::canonicalize(path)
        .map_err(|e| invalid(format!("scope {}: {e}", path.display())))?;
    if canonical != path {
        return Err(invalid(format!(
            "scope must be canonical: {} resolves to {}",
            path.display(),
            canonical.display()
        )));
    }
    if !canonical.is_dir() {
        return Err(invalid(format!(
            "scope {} is not a directory",
            path.display()
        )));
    }
    registry::path_text(path)?;
    Ok(scope)
}

/// A directory named by `ProjectAdd`: absolute, existing, canonicalized.
fn project_dir(path: &Path) -> Result<ScopeRoot, ApiError> {
    if !path.is_absolute() {
        return Err(invalid(format!(
            "folder path {} must be absolute",
            path.display()
        )));
    }
    let canonical = std::fs::canonicalize(path)
        .map_err(|e| invalid(format!("folder {}: {e}", path.display())))?;
    if !canonical.is_dir() {
        return Err(invalid(format!(
            "folder {} is not a directory",
            canonical.display()
        )));
    }
    registry::path_text(&canonical)?;
    Ok(ScopeRoot::from_canonical(canonical))
}

/// A project as clients see it, without its git context (read separately,
/// outside the state lock).
fn project_view(
    project: &ProjectRecord,
    sessions: Option<&ProjectSessions>,
    live: &HashMap<ScopeRoot, u32>,
) -> ProjectView {
    let sessions = sessions.copied().unwrap_or_default();
    let last_active_ms = sessions.last_active_at_ms.max(project.last_used_at_ms);
    let path = project.path.path();
    ProjectView {
        name: path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string()),
        path: project.path.clone(),
        sessions: sessions.count,
        live: live.get(&project.path).copied().unwrap_or(0),
        last_active_at: last_active_ms.map(|ms| ms / 1000),
        added_at: project.added_at_ms / 1000,
        git: None,
    }
}

/// The session's start directory: the scope root by default; relative paths
/// are taken from the scope root. Must resolve inside the scope.
fn resolve_cwd(scope: &ScopeRoot, cwd: Option<&Path>) -> Result<PathBuf, ApiError> {
    let Some(cwd) = cwd else {
        return Ok(scope.path().to_path_buf());
    };
    let joined = scope.path().join(cwd);
    let canonical = std::fs::canonicalize(&joined)
        .map_err(|e| invalid(format!("working directory {}: {e}", joined.display())))?;
    if !canonical.is_dir() {
        return Err(invalid(format!(
            "working directory {} is not a directory",
            canonical.display()
        )));
    }
    if !canonical.starts_with(scope.path()) {
        return Err(ApiError::new(
            ErrorCode::OutOfScope,
            format!(
                "working directory {} is outside {}",
                canonical.display(),
                scope.path().display()
            ),
        ));
    }
    registry::path_text(&canonical)?;
    Ok(canonical)
}

fn validate_argv(argv: &[String]) -> Result<(), ApiError> {
    match argv.first() {
        Some(program) if !program.is_empty() => {}
        _ => return Err(invalid("command must name a program")),
    }
    if argv.iter().any(|arg| arg.contains('\0')) {
        return Err(invalid("command arguments must not contain NUL bytes"));
    }
    Ok(())
}

/// An agent kind (`opencode`) and the conversation ids bound to it.
fn validate_binding(agent: &str, conversations: &[String]) -> Result<(), ApiError> {
    let kind_ok = !agent.is_empty()
        && agent.len() <= MAX_AGENT_KIND_BYTES
        && agent
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_');
    if !kind_ok {
        return Err(invalid(
            "agent must be a short lowercase name such as \"opencode\"",
        ));
    }
    if conversations.is_empty() || conversations.len() > MAX_BINDINGS_PER_REQUEST {
        return Err(invalid(format!(
            "bind 1 to {MAX_BINDINGS_PER_REQUEST} conversations at a time"
        )));
    }
    for conversation in conversations {
        let ok = !conversation.is_empty()
            && conversation.len() <= MAX_CONVERSATION_ID_BYTES
            && conversation
                .chars()
                .all(|c| c.is_ascii_graphic() && c != '"' && c != '\\');
        if !ok {
            return Err(invalid(format!(
                "{} is not a conversation id",
                audit::clip(conversation)
            )));
        }
    }
    Ok(())
}

fn validate_report(report: &Report) -> Result<(), ApiError> {
    if let Some(state) = report.state {
        if !matches!(
            state,
            Lifecycle::Working | Lifecycle::Blocked | Lifecycle::Done | Lifecycle::Idle
        ) {
            return Err(invalid(
                "reported state must be working, blocked, done or idle",
            ));
        }
    }
    if report
        .text
        .as_ref()
        .is_some_and(|t| t.len() > MAX_REPORT_TEXT_BYTES)
    {
        return Err(invalid(format!(
            "report text is longer than {MAX_REPORT_TEXT_BYTES} bytes"
        )));
    }
    if let Some(result) = &report.result {
        if result.summary.len() > MAX_SUMMARY_BYTES {
            return Err(invalid(format!(
                "result summary is longer than {MAX_SUMMARY_BYTES} bytes"
            )));
        }
        if result.files.len() > MAX_REPORT_FILES {
            return Err(invalid(format!(
                "result lists more than {MAX_REPORT_FILES} files"
            )));
        }
    }
    if report
        .agent
        .as_ref()
        .is_some_and(|a| a.kind.trim().is_empty())
    {
        return Err(invalid("agent kind must not be empty"));
    }
    if let Some(resume) = &report.resume {
        validate_argv(resume)?;
    }
    Ok(())
}

fn report_detail(report: &Report) -> Option<String> {
    let mut parts = Vec::new();
    if report.activity.is_some() {
        parts.push("activity".into());
    }
    if let Some(state) = report.state {
        parts.push(format!("state={}", waits::lifecycle_name(state)));
    }
    if let Some(agent) = &report.agent {
        parts.push(format!("agent={}", agent.kind));
    }
    if report.resume.is_some() {
        parts.push("resume".to_string());
    }
    if report.result.is_some() {
        parts.push("result".to_string());
    }
    if let Some(text) = &report.text {
        parts.push(format!("text={text}"));
    }
    (!parts.is_empty()).then(|| audit::clip(&parts.join(" ")))
}

fn host_error(error: HostError) -> ApiError {
    match error {
        HostError::NotFound | HostError::NotRunning => {
            ApiError::new(ErrorCode::SessionNotLive, error.to_string())
        }
        HostError::NotController | HostError::Typing => {
            ApiError::new(ErrorCode::SessionInUse, error.to_string())
        }
        HostError::InvalidKey(_) => invalid(error.to_string()),
        HostError::Spawn(message) => invalid(format!("could not start the session: {message}")),
        HostError::Io(_) => ApiError::new(ErrorCode::Internal, error.to_string()),
    }
}

fn invalid(message: impl Into<String>) -> ApiError {
    ApiError::new(ErrorCode::InvalidArgument, message)
}

fn not_found(id: &SessionId) -> ApiError {
    ApiError::new(ErrorCode::NotFound, format!("no session {id}"))
}

fn not_live(id: &SessionId) -> ApiError {
    ApiError::new(
        ErrorCode::SessionNotLive,
        format!("session {id} is not running"),
    )
}

fn name_taken(name: &str) -> ApiError {
    invalid(format!(
        "a session named {name:?} already exists in this folder"
    ))
}

/// `limit` is the `Limits` field that was reached and `max` its value; both
/// go in the error's details.
fn limit_exceeded(limit: &str, max: u32, message: String) -> ApiError {
    let mut error = ApiError::new(ErrorCode::LimitExceeded, message);
    error.details = Some(serde_json::json!({ "limit": limit, "max": max }));
    error
}

fn shutting_down() -> ApiError {
    ApiError::new(ErrorCode::DaemonUnavailable, "the daemon is shutting down")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detected_agent_and_reported_identity_merge() {
        let reported = AgentInfo {
            kind: "claude".into(),
            agent_session_id: Some("abc".into()),
            agent_session_path: None,
        };
        assert_eq!(
            merge_agent(Some("claude"), Some(&reported)),
            Some(reported.clone())
        );
        assert_eq!(
            merge_agent(Some("codex"), Some(&reported)).map(|a| a.kind),
            Some("codex".into())
        );
        assert_eq!(merge_agent(None, Some(&reported)), Some(reported));
        assert_eq!(merge_agent(None, None), None);
    }

    #[test]
    fn cwd_must_stay_inside_the_scope() {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        std::fs::create_dir_all(root.join("project/src")).unwrap();
        let scope = ScopeRoot::from_canonical(root.join("project"));
        assert_eq!(resolve_cwd(&scope, None).unwrap(), scope.path());
        assert_eq!(
            resolve_cwd(&scope, Some(Path::new("src"))).unwrap(),
            scope.path().join("src")
        );
        assert_eq!(
            resolve_cwd(&scope, Some(Path::new(".."))).unwrap_err().code,
            ErrorCode::OutOfScope
        );
        assert_eq!(
            resolve_cwd(&scope, Some(Path::new("missing")))
                .unwrap_err()
                .code,
            ErrorCode::InvalidArgument
        );
        assert!(validate_scope(Some(ScopeRoot::from_canonical(
            root.join("project/../project")
        )))
        .is_err());
        assert!(validate_scope(None).is_err());
    }

    #[test]
    fn project_dirs_are_absolute_existing_directories() {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        std::fs::create_dir_all(root.join("app")).unwrap();
        std::fs::write(root.join("file"), "x").unwrap();
        assert_eq!(
            project_dir(&root.join("app/../app")).unwrap().path(),
            root.join("app")
        );
        for bad in [
            PathBuf::from("relative"),
            root.join("missing"),
            root.join("file"),
        ] {
            assert_eq!(
                project_dir(&bad).unwrap_err().code,
                ErrorCode::InvalidArgument,
                "{}",
                bad.display()
            );
        }
    }

    #[test]
    fn project_views_combine_sessions_and_usage() {
        let path = ScopeRoot::from_canonical("/work/rakumux".into());
        let record = ProjectRecord {
            path: path.clone(),
            added_at_ms: 1_000,
            last_used_at_ms: Some(9_000),
        };
        let live = HashMap::from([(path.clone(), 2)]);
        let view = project_view(
            &record,
            Some(&ProjectSessions {
                count: 3,
                last_active_at_ms: Some(5_000),
            }),
            &live,
        );
        assert_eq!(view.name, "rakumux");
        assert_eq!((view.sessions, view.live), (3, 2));
        assert_eq!(view.last_active_at, Some(9));
        assert_eq!(view.added_at, 1);
        let empty = project_view(
            &ProjectRecord {
                last_used_at_ms: None,
                ..record
            },
            None,
            &HashMap::new(),
        );
        assert_eq!((empty.sessions, empty.live), (0, 0));
        assert_eq!(empty.last_active_at, None);
        let root = project_view(
            &ProjectRecord {
                path: ScopeRoot::from_canonical("/".into()),
                added_at_ms: 0,
                last_used_at_ms: None,
            },
            None,
            &HashMap::new(),
        );
        assert_eq!(root.name, "/");
    }
}
