//! This machine's session list for rakumux.com (`contracts/feed/`
//! `sessions-upload`): while the account's feed is on, the status
//! service keeps what the TUI's Status and Folders tabs group by for each
//! session it follows (never a private one, and the daemon's list leaves out
//! dead ones) and uploads the whole list whenever it changes, at most every
//! [`SETTLE`], and every [`HEARTBEAT`] meanwhile, so the website can tell a
//! machine that's away from one with nothing running. Each session's places
//! (where it has worked, as names: [`PlaceState`]) go only to a cloud whose
//! answer said it takes them (`accepts`). Driven by the owner thread with
//! its own clock.

use std::collections::BTreeMap;
use std::time::Duration;

use serde::Serialize;
use session_core::activity::ActivityPhase;
use session_core::ids::SessionId;
use session_core::model::{Lifecycle, ScopeRoot, SessionPlace, SessionStatus, SessionView};

use super::wire::{self, FeedFailure, FeedRequest};

/// The shortest time between two uploads of a changing list.
const SETTLE: Duration = Duration::from_secs(3);
/// How often an unchanged list is sent again.
const HEARTBEAT: Duration = Duration::from_secs(60);
/// The first and longest waits after a failed upload.
const RETRY_MIN: Duration = Duration::from_secs(5);
const RETRY_MAX: Duration = Duration::from_secs(300);
/// The most sessions one machine lists (the contract's bound).
const MAX_SESSIONS: usize = 200;
/// The most places a session lists: its most recent (the contract's bound).
const MAX_PLACES: usize = 8;
/// The largest upload the contract takes (`x-maxBodyBytes`): a list that
/// would be larger goes without its places and its agents' latest steps.
const MAX_BODY_BYTES: usize = 262_144;
/// The most characters of an agent's latest step (`output`; the contract's
/// bound).
const MAX_OUTPUT: usize = 300;

/// One session as the website lists it (`contracts/feed/` `sessionState`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct SessionState {
    id: String,
    name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    folder: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    branch: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    agent: Option<&'static str>,
    /// Without one of those agents, what the TUI's App tab files it under.
    #[serde(skip_serializing_if = "Option::is_none")]
    program: Option<String>,
    status: &'static str,
    lifecycle: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    phase: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    conversation_id: Option<String>,
    pinned: bool,
    last_active_at: i64,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    places: Vec<PlaceState>,
    /// A live session's agent's latest step, one line, masked.
    #[serde(skip_serializing_if = "Option::is_none")]
    output: Option<String>,
    /// The prompt with numbered options its agent shows while it asks.
    #[serde(skip_serializing_if = "Option::is_none")]
    choice: Option<ChoiceState>,
    /// The heartbeat this session is a run of.
    #[serde(skip_serializing_if = "Option::is_none")]
    heartbeat: Option<HeartbeatRef>,
}

/// The heartbeat a session is a run of (`contracts/feed/` `heartbeatRunRef`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct HeartbeatRef {
    heartbeat: String,
    run: u32,
}

/// The prompt with numbered options a session's agent shows, as the website
/// lists it (`contracts/feed/` `choicePrompt`): its text cleaned of anything
/// a terminal would act on, its detail masked as chat text is.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct ChoiceState {
    key: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    detail: Option<String>,
    question: String,
    options: Vec<ChoiceOptionState>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
struct ChoiceOptionState {
    number: u8,
    label: String,
}

impl ChoiceState {
    /// `prompt` within the contract's bounds; `None` when its question or
    /// an option has nothing left to show, or it's off the contract.
    fn of(prompt: &session_core::model::ChoicePrompt) -> Option<Self> {
        let one_line = |text: &str, max: usize| {
            let text = crate::cloud::chat_text(text)?;
            let line = text.split_whitespace().collect::<Vec<_>>().join(" ");
            let line: String = line.chars().take(max).collect();
            (!line.is_empty()).then_some(line)
        };
        let key = prompt.key.len() == 16
            && prompt
                .key
                .chars()
                .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c));
        let options: Option<Vec<ChoiceOptionState>> = prompt
            .options
            .iter()
            .take(9)
            .map(|option| {
                (1..=9).contains(&option.number).then_some(())?;
                Some(ChoiceOptionState {
                    number: option.number,
                    label: one_line(&option.label, 1000)?,
                })
            })
            .collect();
        let options = options.filter(|options| !options.is_empty())?;
        if !key {
            return None;
        }
        Some(Self {
            key: prompt.key.clone(),
            title: prompt
                .title
                .as_deref()
                .and_then(|title| one_line(title, 200)),
            detail: prompt.detail.as_deref().and_then(|detail| {
                let text = crate::cloud::chat_text(detail)?;
                let text: String = text.trim().chars().take(MAX_CHOICE_DETAIL).collect();
                (!text.is_empty()).then_some(text)
            }),
            question: one_line(&prompt.question, 1000)?,
            options,
        })
    }
}

/// The most characters a prompt's detail carries (contract).
const MAX_CHOICE_DETAIL: usize = 2000;

/// A place a session worked in, as the website lists it (`contracts/feed/`
/// `sessionPlace`): its path in the session's folder (`.` for the folder
/// itself), else its folder's name; never a path outside the session's
/// folder.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct PlaceState {
    name: String,
    inside: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    branch: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    worktree: Option<String>,
    edited: bool,
    last_at: i64,
}

impl PlaceState {
    /// `place` of a session in `folder`, or `None` when it can't be named
    /// within the contract.
    fn of(folder: &ScopeRoot, place: &SessionPlace) -> Option<Self> {
        let (name, inside) = match place.path.strip_prefix(folder.path()) {
            Ok(rest) if rest.as_os_str().is_empty() => (".".to_owned(), true),
            Ok(rest) => (rest.to_str()?.to_owned(), true),
            Err(_) => (place.path.file_name()?.to_str()?.to_owned(), false),
        };
        let name = wire::label(&name, 256)?;
        if name.starts_with(['/', '~']) || name.split('/').any(|part| part == "..") {
            return None;
        }
        let git = place.git.as_ref();
        Some(Self {
            name,
            inside,
            branch: git
                .and_then(|git| git.branch.as_deref())
                .and_then(|branch| wire::label(branch, 128)),
            worktree: git
                .and_then(|git| git.worktree.as_deref())
                .filter(|worktree| !worktree.contains('/'))
                .and_then(|worktree| wire::label(worktree, 128)),
            edited: place.edited,
            // Unix seconds on the daemon's side; never before 2020 on the wire.
            last_at: place.last_at.saturating_mul(1000).max(1_577_836_800_000),
        })
    }
}

impl SessionState {
    /// `view` as listed, or `None` when it isn't: private, dead, or of an
    /// unknown status.
    fn of(view: &SessionView) -> Option<Self> {
        if crate::shared_elsewhere(view) {
            return None;
        }
        let status = match view.status {
            SessionStatus::Live => "live",
            SessionStatus::Exited => "exited",
            _ => return None,
        };
        let lifecycle = match view.lifecycle {
            Lifecycle::Working => "working",
            Lifecycle::Blocked => "blocked",
            Lifecycle::Done => "done",
            Lifecycle::Idle => "idle",
            Lifecycle::None => "none",
            _ => "unknown",
        };
        let phase = view
            .activity
            .as_ref()
            .and_then(|activity| match activity.phase {
                ActivityPhase::Working => Some("working"),
                ActivityPhase::WaitingForResponse => Some("waiting_for_response"),
                ActivityPhase::RunningTool => Some("running_tool"),
                ActivityPhase::WaitingForAgents => Some("waiting_for_agents"),
                ActivityPhase::NeedsInput => Some("needs_input"),
                ActivityPhase::Idle => Some("idle"),
                ActivityPhase::Done => Some("done"),
                _ => None,
            });
        let agent = view
            .agent
            .as_ref()
            .and_then(|agent| super::agents::feed_agent(&agent.kind));
        let program = if agent.is_none() {
            program_of(view)
        } else {
            None
        };
        let name: String = view
            .name
            .chars()
            .filter(|c| !c.is_control())
            .take(64)
            .collect();
        if name.trim().is_empty() {
            return None;
        }
        Some(Self {
            id: view.id.as_str().to_owned(),
            name,
            folder: super::sync::folder_of(view),
            branch: super::sync::branch_of(view),
            agent,
            program,
            status,
            lifecycle,
            phase,
            conversation_id: view
                .agent_turn
                .as_ref()
                .and_then(|turn| turn.conversation_id.clone())
                .filter(|id| wire::conversation_id(id)),
            pinned: view.pinned,
            // Unix seconds on the daemon's side; never before 2020 on the wire.
            last_active_at: view
                .last_active_at
                .saturating_mul(1000)
                .max(1_577_836_800_000),
            places: view
                .places
                .iter()
                .filter_map(|place| PlaceState::of(&view.scope, place))
                .take(MAX_PLACES)
                .collect(),
            output: if status == "live" && agent.is_some() {
                view.agent_output.as_deref().and_then(output_line)
            } else {
                None
            },
            choice: if status == "live" && view.lifecycle == Lifecycle::Blocked {
                view.terminal
                    .as_ref()
                    .and_then(|terminal| terminal.choice.as_ref())
                    .and_then(ChoiceState::of)
            } else {
                None
            },
            heartbeat: view.heartbeat.as_ref().and_then(|run| {
                let heartbeat = run.heartbeat.as_str().to_owned();
                (run.run >= 1 && crate::feed::is_entry_id(&heartbeat)).then_some(HeartbeatRef {
                    heartbeat,
                    run: run.run,
                })
            }),
        })
    }
}

/// An agent's latest step as the feed takes it (`output`): escapes and
/// controls removed, keys, credential assignments and tokens masked as chat
/// text is, its lines and spaces run into one line, cut to [`MAX_OUTPUT`]
/// characters with `…`; `None` when nothing visible is left.
fn output_line(text: &str) -> Option<String> {
    let text = crate::cloud::chat_text(text)?;
    let line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let line = if line.chars().count() > MAX_OUTPUT {
        let mut cut: String = line.chars().take(MAX_OUTPUT - 1).collect();
        cut.push('…');
        cut
    } else {
        line
    };
    wire::label(&line, MAX_OUTPUT)
}

/// Shells, which the App tab files as `shell` (the TUI's `grouping::is_shell`).
const SHELLS: &[&str] = &[
    "sh", "bash", "zsh", "fish", "dash", "ksh", "mksh", "tcsh", "csh", "nu", "xonsh", "elvish",
];

/// Whether `program` is a shell (`bash`, `zsh`, …).
pub(crate) fn is_shell(program: &str) -> bool {
    SHELLS.contains(&program)
}

/// The app the TUI's App tab files `view` under (its `grouping::AppKey`), when it isn't `shell`:
/// the agent the session names, else its foreground program while it runs, else the program it
/// started with; never a shell. Lowercase, a name only, within the contract's 64 characters.
fn program_of(view: &SessionView) -> Option<String> {
    let usable = |program: &str| {
        let program = program.trim();
        (!program.is_empty() && !is_shell(program)).then(|| program.to_owned())
    };
    let agent = view.agent.as_ref().and_then(|agent| usable(&agent.kind));
    let foreground = view
        .terminal
        .as_ref()
        .filter(|_| view.status == SessionStatus::Live)
        .and_then(|terminal| terminal.foreground_process.as_deref())
        .and_then(usable);
    let launched = view
        .launch
        .argv
        .as_ref()
        .and_then(|argv| argv.first())
        .map(|program| program.rsplit('/').next().unwrap_or(program))
        .and_then(usable);
    let program: String = agent
        .or(foreground)
        .or(launched)?
        .to_lowercase()
        .chars()
        .filter(|c| !c.is_control() && *c != '/')
        .take(64)
        .collect();
    (!program.trim().is_empty()).then_some(program)
}

/// The session list's state on the owner thread.
pub(crate) struct SessionSync {
    machine: String,
    /// What else this machine handles, said with every upload.
    capabilities: Vec<&'static str>,
    /// This Rakumux's version, said with every upload (`rakumux_version`).
    version: Option<String>,
    enabled: bool,
    /// The cloud's last answer said it takes places.
    places: bool,
    /// The cloud's last answer said it takes subagents' messages in chats.
    subagents: bool,
    /// The cloud's last answer said it takes agents' latest steps.
    output: bool,
    /// The cloud's last answer said it takes agents' prompts with numbered
    /// options.
    choices: bool,
    /// The cloud's last answer said it takes heartbeats.
    heartbeats: bool,
    /// rakumux.com answered a list under this name with anything but
    /// `machine_taken` (or a failure for now) since the switch was turned on:
    /// the name is this machine's, and its replies may be read.
    confirmed: bool,
    sessions: BTreeMap<SessionId, SessionState>,
    /// What rakumux.com was last sent; `None` until it was, since the switch
    /// was turned on.
    sent: Option<Vec<SessionState>>,
    /// The list being sent now.
    sending: Option<Vec<SessionState>>,
    next_send: Duration,
    heartbeat_at: Duration,
    retry_at: Duration,
    retry: Duration,
}

impl SessionSync {
    /// The list for `machine`; `reply_attachments` says it saves replies'
    /// files for their agents (`capabilities`), so rakumux.com sends it those.
    /// It says it sends what it uses for rtop (`resources`) only once the
    /// daemon is known to tell it ([`set_resources`](Self::set_resources)).
    pub fn new(machine: String, reply_attachments: bool) -> Self {
        let capabilities = if reply_attachments {
            vec![wire::REPLY_ATTACHMENTS]
        } else {
            Vec::new()
        };
        Self {
            machine,
            capabilities,
            version: None,
            enabled: false,
            places: false,
            subagents: false,
            output: false,
            choices: false,
            heartbeats: false,
            confirmed: false,
            sessions: BTreeMap::new(),
            sent: None,
            sending: None,
            next_send: Duration::ZERO,
            heartbeat_at: Duration::ZERO,
            retry_at: Duration::ZERO,
            retry: RETRY_MIN,
        }
    }

    /// Also say this Rakumux is `version` with every upload.
    pub fn with_version(mut self, version: Option<String>) -> Self {
        self.version = version;
        self
    }

    /// Also say `capability` with every upload (what this machine handles).
    pub fn handling(mut self, capability: &'static str) -> Self {
        if !self.capabilities.contains(&capability) {
            self.capabilities.push(capability);
        }
        self
    }

    /// Follow the account: signed in with its feed on. Turned on
    /// again, the list is sent at once.
    pub fn set_enabled(&mut self, enabled: bool, now: Duration) {
        if enabled == self.enabled {
            return;
        }
        self.enabled = enabled;
        self.places = false;
        self.subagents = false;
        self.output = false;
        self.choices = false;
        self.heartbeats = false;
        self.confirmed = false;
        self.sent = None;
        self.retry = RETRY_MIN;
        self.retry_at = now;
        self.next_send = now;
    }

    /// Whether the daemon answers `resource_usage` (capability `resources`),
    /// so this machine can send what it uses for rtop: the list says
    /// `resources` only then, so rakumux.com tells a machine whose daemon is
    /// too old from one still reading. A change goes with the next upload.
    pub fn set_resources(&mut self, supported: bool) {
        self.set_capability(wire::RESOURCES, supported);
    }

    /// Whether the daemon answers agents' questions in their own dialogs
    /// (capability `session.answer`): the list says `question_answers` only
    /// then, so rakumux.com offers to answer a question only of a machine
    /// that gives the answer. A change goes with the next upload.
    pub fn set_answers(&mut self, supported: bool) {
        self.set_capability(wire::QUESTION_ANSWERS, supported);
    }

    /// Whether the daemon reads agents' prompts with numbered options and
    /// picks their options (capability `session.choose`): the list says
    /// `choice_answers`, and carries the prompts, only then. A change goes
    /// with the next upload.
    pub fn set_choices(&mut self, supported: bool) {
        self.set_capability(wire::CHOICE_ANSWERS, supported);
    }

    /// Which agents beyond Claude Code the daemon answers the questions of
    /// (`wire::AGENT_ANSWERS`, in order): the list says each one's
    /// `question_answers.<agent>` only then.
    pub fn set_agent_answers(&mut self, supported: &[bool]) {
        for ((_, capability), supported) in wire::AGENT_ANSWERS.iter().zip(supported) {
            self.set_capability(capability, *supported);
        }
    }

    /// Whether the daemon runs heartbeats (capability `heartbeats`): the list
    /// says `heartbeats` only then, so rakumux.com asks actions of them only
    /// of a machine that carries them out. A change goes with the next upload.
    pub fn set_heartbeats(&mut self, supported: bool) {
        self.set_capability(super::HEARTBEATS, supported);
    }

    /// Whether rakumux.com's last answer said it takes heartbeats.
    pub fn heartbeats(&self) -> bool {
        self.heartbeats
    }

    fn set_capability(&mut self, capability: &'static str, supported: bool) {
        let has = self.capabilities.contains(&capability);
        if supported == has {
            return;
        }
        if supported {
            self.capabilities.push(capability);
        } else {
            self.capabilities.retain(|name| *name != capability);
        }
        // The same sessions, said again with what this machine handles now.
        self.sent = None;
    }

    /// The name this machine goes by in the feed now.
    pub fn machine(&self) -> &str {
        &self.machine
    }

    /// Whether rakumux.com took a list under this machine's name (see
    /// `confirmed`): until then its replies aren't read, so a machine sharing
    /// another's name never takes (and fails) that one's replies.
    pub fn confirmed(&self) -> bool {
        self.confirmed
    }

    /// Whether rakumux.com's last answer said it takes subagents' messages
    /// in chats (`accepts` has `subagents`): until then, chats go without.
    pub fn subagents(&self) -> bool {
        self.subagents
    }

    /// Another machine held this one's name: it goes by `machine` now, and
    /// sends its list at once.
    pub fn rename(&mut self, machine: String, now: Duration) {
        self.machine = machine;
        self.confirmed = false;
        self.sent = None;
        self.retry = RETRY_MIN;
        self.retry_at = now;
        self.next_send = now;
    }

    /// The daemon (re)connected: these are its sessions.
    pub fn connected(&mut self, views: &[SessionView]) {
        self.sessions = views
            .iter()
            .filter_map(|view| Some((view.id.clone(), SessionState::of(view)?)))
            .collect();
    }

    pub fn observe(&mut self, view: &SessionView) {
        match SessionState::of(view) {
            Some(state) => {
                self.sessions.insert(view.id.clone(), state);
            }
            None => {
                self.sessions.remove(&view.id);
            }
        }
    }

    pub fn forget(&mut self, session: &SessionId) {
        self.sessions.remove(session);
    }

    /// Whether the list has `session`: never a private one, a dead one or
    /// another machine's. rtop's readings name only these.
    pub fn lists(&self, session: &SessionId) -> bool {
        self.sessions.contains_key(session)
    }

    /// The list as it stands, the most recently used first, within the
    /// contract's bound; with places and agents' latest steps when the
    /// cloud takes them.
    fn list(&self) -> Vec<SessionState> {
        let mut list: Vec<SessionState> = self.sessions.values().cloned().collect();
        list.sort_by(|a, b| (b.last_active_at, &a.id).cmp(&(a.last_active_at, &b.id)));
        list.truncate(MAX_SESSIONS);
        if !self.places {
            without_places(&mut list);
        }
        if !self.output {
            without_output(&mut list);
        }
        if !self.choices || !self.capabilities.contains(&wire::CHOICE_ANSWERS) {
            without_choices(&mut list);
        }
        if !self.heartbeats {
            for session in &mut list {
                session.heartbeat = None;
            }
        }
        list
    }

    /// The upload due now, if any: a changed list once it has settled, or
    /// the same one as a heartbeat. One at a time.
    pub fn next_request(&mut self, now: Duration, wall_ms: i64) -> Option<FeedRequest> {
        if !self.enabled || self.sending.is_some() || now < self.retry_at {
            return None;
        }
        let list = self.list();
        let changed = self.sent.as_ref() != Some(&list);
        let due = (changed && now >= self.next_send) || now >= self.heartbeat_at;
        if !due {
            return None;
        }
        let capabilities = &self.capabilities;
        let mut request = wire::sessions_upload(
            &self.machine,
            &list,
            wall_ms,
            capabilities,
            self.version.as_deref(),
        )?;
        if matches!(&request, FeedRequest::Sessions { body } if body.len() > MAX_BODY_BYTES) {
            // Too large with them: the same list without places or agents'
            // latest steps (and it counts as sent, so it isn't sent again
            // until it changes).
            let mut smaller = list.clone();
            without_places(&mut smaller);
            without_output(&mut smaller);
            without_choices(&mut smaller);
            request = wire::sessions_upload(
                &self.machine,
                &smaller,
                wall_ms,
                capabilities,
                self.version.as_deref(),
            )?;
        }
        self.sending = Some(list);
        Some(request)
    }

    /// An upload finished; `current` says whether it was made for the
    /// account still signed in. The answer says whether the cloud takes
    /// places, for the uploads that follow.
    pub fn done(
        &mut self,
        current: bool,
        outcome: Result<wire::SessionsStored, FeedFailure>,
        now: Duration,
    ) {
        let sent = self.sending.take();
        if !current || !self.enabled {
            return;
        }
        // Any answer but these says the name is this machine's.
        self.confirmed |= !matches!(
            outcome,
            Err(FeedFailure::MachineTaken
                | FeedFailure::Unauthorized
                | FeedFailure::Off
                | FeedFailure::RateLimited(_)
                | FeedFailure::Unavailable(_))
        );
        match outcome {
            Ok(stored) => {
                self.places = stored.places;
                self.subagents = stored.subagents;
                self.output = stored.output;
                self.choices = stored.choices;
                self.heartbeats = stored.heartbeats;
                self.sent = sent;
                self.retry = RETRY_MIN;
                self.next_send = now + SETTLE;
                self.heartbeat_at = now + HEARTBEAT;
            }
            Err(failure) => {
                let wait = match failure {
                    FeedFailure::RateLimited(Some(delay))
                    | FeedFailure::Unavailable(Some(delay)) => delay,
                    _ => self.retry,
                };
                self.retry = self.retry.saturating_mul(2).min(RETRY_MAX);
                self.retry_at = now + wait.min(Duration::from_secs(86_400));
            }
        }
    }
}

/// `list` without its sessions' places.
fn without_places(list: &mut [SessionState]) {
    for session in list {
        session.places.clear();
    }
}

/// `list` without its sessions' agents' prompts.
fn without_choices(list: &mut [SessionState]) {
    for session in list {
        session.choice = None;
    }
}

/// `list` without its sessions' agents' latest steps.
fn without_output(list: &mut [SessionState]) {
    for session in list {
        session.output = None;
    }
}

#[cfg(test)]
mod tests;
