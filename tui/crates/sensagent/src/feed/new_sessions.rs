//! New sessions asked for on rakumux.com (`contracts/feed/` `newSession`):
//! while the account's feed is on, the replies read brings the
//! requests pending for this machine (the cloud sends them only because its
//! session list says `new_sessions`), and the status service starts each one
//! through the daemon as the user (`session_create`), then reports it
//! `created` with the session it started, or `failed` with why.
//!
//! A request names a folder by its name and, optionally, an agent from a
//! fixed list; never a path or a command. The folder is one of those of the
//! sessions this machine lists to rakumux.com (live or exited, never private
//! or another machine's), found by name, and `folder_session` (a session
//! rakumux.com listed there) picks one of two folders with the same name;
//! none, or several without that, fails `folder_unknown`. The session is a
//! login shell in the folder, with the agent's own command typed in as a
//! startup command (`claude`, `codex`, `opencode`: what `rakumux new --cmd`
//! would run), and the daemon's environment. (`not_accepted`, which older
//! releases report while a setting of theirs is off, is never sent.)
//!
//! Each request is started at most once: its id is kept as handled before
//! the daemon is asked, and with the reports rakumux.com hasn't taken yet,
//! across a restart of the service (`kept`). Driven by the owner thread with
//! its own clock.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use session_core::api::{CreateSession, ErrorCode};
use session_core::ids::SessionId;
use session_core::model::{ScopeRoot, SessionStatus, SessionView};

use super::wire::{self, FeedFailure, FeedRequest};
use crate::cloud::FeedHttpReply;

mod kept;
use kept::Kept;

/// What this machine says with its session list when it takes new sessions
/// asked for on rakumux.com (`capabilities`), so the cloud sends it those.
pub(crate) const NEW_SESSIONS: &str = "new_sessions";
/// The most requests one read carries (contract).
const MAX_ASKED: usize = 20;
/// The first and longest waits after a failed report.
const RETRY_MIN: Duration = Duration::from_secs(5);
const RETRY_MAX: Duration = Duration::from_secs(300);
/// Reports rakumux.com asks to hold back wait at most this long: a request
/// expires 10 minutes after it was written.
const RATE_LIMIT_MAX: Duration = Duration::from_secs(60);

/// An agent a new session may start: the contract's fixed list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Agent {
    ClaudeCode,
    Codex,
    OpenCode,
}

impl Agent {
    fn of(name: &str) -> Option<Self> {
        match name {
            "claude-code" => Some(Self::ClaudeCode),
            "codex" => Some(Self::Codex),
            "opencode" => Some(Self::OpenCode),
            _ => None,
        }
    }

    /// The command that starts it, typed into the new session's shell as
    /// `rakumux new --cmd` would: this machine's own, never text from the cloud.
    fn command(self) -> &'static str {
        match self {
            Self::ClaudeCode => "claude",
            Self::Codex => "codex",
            Self::OpenCode => "opencode",
        }
    }
}

/// A new session asked of this machine, as read: pending, within the contract.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct NewSessionAsk {
    pub id: String,
    pub machine: String,
    /// The folder's name, as this machine's session list gives it.
    pub folder: String,
    /// A session rakumux.com listed in that folder.
    pub folder_session: Option<SessionId>,
    /// The agent to start in its shell; none: the shell alone.
    pub agent: Option<Agent>,
}

#[derive(Deserialize)]
struct WireAsk {
    id: String,
    machine: String,
    folder: String,
    #[serde(default)]
    folder_session: Option<String>,
    #[serde(default)]
    agent: Option<String>,
    status: String,
}

impl WireAsk {
    fn checked(self) -> Option<NewSessionAsk> {
        // Kept as sent, to compare with this machine's own names, once they're
        // one visible line within the contract's bounds; never used as a path.
        let folder = self.folder;
        let fits = one_line(&folder, 128, 512)
            && !folder.contains('/')
            && !matches!(folder.trim(), "." | "..");
        let machine = self.machine;
        if !fits || !one_line(&machine, 64, 256) {
            return None;
        }
        let agent = match self.agent {
            Some(name) => Some(Agent::of(&name)?),
            None => None,
        };
        let folder_session = match self.folder_session {
            Some(id) if crate::feed::is_entry_id(&id) => Some(SessionId::from_string(id)),
            Some(_) => return None,
            None => None,
        };
        (self.status == "pending" && crate::feed::is_entry_id(&self.id)).then_some(NewSessionAsk {
            id: self.id,
            machine,
            folder,
            folder_session,
            agent,
        })
    }
}

/// `text` is one visible line (the contract's `label`) of at most
/// `characters` characters and `bytes` UTF-8 bytes.
fn one_line(text: &str, characters: usize, bytes: usize) -> bool {
    text.len() <= bytes
        && text.chars().count() <= characters
        && wire::label(text, characters).is_some_and(|clean| clean == text.trim())
}

/// The replies read's `new_sessions`: those within the contract, at most
/// [`MAX_ASKED`]; a row off the contract is left out on its own, never the
/// read (it stays pending, and expires).
pub(crate) fn asks(value: Option<serde_json::Value>) -> Vec<NewSessionAsk> {
    let Some(serde_json::Value::Array(rows)) = value else {
        return Vec::new();
    };
    rows.into_iter()
        .take(MAX_ASKED)
        .filter_map(|row| serde_json::from_value::<WireAsk>(row).ok()?.checked())
        .collect()
}

/// Why this machine didn't start a session asked for (contract `newSessionFailure`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum NewSessionFailure {
    /// A setting of an older release was off. Never decided now; read back
    /// from a report kept before an upgrade.
    NotAccepted,
    /// No folder of that name is listed here now, or several and the request
    /// doesn't say which.
    FolderUnknown,
    /// The daemon couldn't start it.
    Refused,
}

/// The session started for a request (contract `createdSession`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Started {
    pub id: String,
    pub name: String,
}

/// What became of a request, as reported (contract `new-session-status`,
/// whose `status` tags it).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub(crate) enum Outcome {
    Created { session: Started },
    Failed { failure: NewSessionFailure },
}

#[derive(Serialize)]
struct StatusBody<'a> {
    version: u8,
    id: &'a str,
    machine: &'a str,
    #[serde(flatten)]
    outcome: &'a Outcome,
}

/// This machine's report on request `id`.
fn status_request(id: &str, machine: &str, outcome: &Outcome) -> Option<FeedRequest> {
    if !crate::feed::is_entry_id(id) {
        return None;
    }
    let body = serde_json::to_vec(&StatusBody {
        version: 1,
        id,
        machine,
        outcome,
    })
    .ok()?;
    Some(FeedRequest::NewSessionStatus { body })
}

#[derive(Deserialize)]
struct StatusAnswer {
    version: u8,
    new_session: StoredId,
}

#[derive(Deserialize)]
struct StoredId {
    id: String,
}

/// The answer to a report: the stored request (changed or not).
pub(crate) fn reported(reply: &FeedHttpReply) -> Result<(), FeedFailure> {
    if reply.status != 200 {
        return Err(wire::failure(reply));
    }
    let answer: StatusAnswer =
        serde_json::from_slice(&reply.body).map_err(|_| FeedFailure::Invalid)?;
    if answer.version != 1 || !crate::feed::is_entry_id(&answer.new_session.id) {
        return Err(FeedFailure::Invalid);
    }
    Ok(())
}

/// What to do next for new sessions.
#[derive(Debug, PartialEq)]
pub(crate) enum NewSessionWork {
    /// Ask the daemon to start the session for request `id`.
    Create { id: String, request: CreateSession },
    /// Report a request to rakumux.com.
    Report(FeedRequest),
}

/// A folder this machine lists to rakumux.com, through one of its sessions.
struct Listed {
    scope: ScopeRoot,
    /// Its name as the session list gives it.
    folder: String,
}

impl Listed {
    /// `view`'s folder, while `view` is listed (as the session list lists it).
    fn of(view: &SessionView) -> Option<Self> {
        let listed = !crate::shared_elsewhere(view)
            && matches!(view.status, SessionStatus::Live | SessionStatus::Exited);
        listed.then_some(())?;
        Some(Self {
            scope: view.scope.clone(),
            folder: super::sync::folder_of(view)?,
        })
    }
}

/// New sessions' state on the owner thread.
pub(crate) struct NewSessionSync {
    machine: String,
    enabled: bool,
    /// The folders of the sessions this machine lists, by session.
    listed: HashMap<SessionId, Listed>,
    /// The requests pending for this machine, as last read, oldest first.
    asked: Vec<NewSessionAsk>,
    /// The request whose session the daemon is starting.
    creating: Option<String>,
    kept: Kept,
    /// A report is out.
    reporting: bool,
    retry_at: Duration,
    retry: Duration,
}

impl NewSessionSync {
    /// New sessions for `machine`; `kept` is the file that keeps which were
    /// handled across a restart (memory only without it).
    pub fn new(machine: String, kept: Option<PathBuf>) -> Self {
        Self {
            machine,
            enabled: false,
            listed: HashMap::new(),
            asked: Vec::new(),
            creating: None,
            kept: Kept::load(kept),
            reporting: false,
            retry_at: Duration::ZERO,
            retry: RETRY_MIN,
        }
    }

    /// Follow the account: signed in with its feed on and this
    /// machine's name confirmed. Off, nothing is started; reports not yet
    /// taken are kept for when it's on again.
    pub fn set_enabled(&mut self, enabled: bool, now: Duration) {
        if enabled == self.enabled {
            return;
        }
        self.enabled = enabled;
        self.asked.clear();
        self.retry = RETRY_MIN;
        self.retry_at = now;
    }

    /// This machine goes by `machine` now: requests to the old name aren't its.
    pub fn rename(&mut self, machine: String) {
        self.machine = machine;
        self.asked.clear();
    }

    /// The daemon (re)connected: these are its sessions.
    pub fn connected(&mut self, views: &[SessionView]) {
        self.listed = views
            .iter()
            .filter_map(|view| Some((view.id.clone(), Listed::of(view)?)))
            .collect();
    }

    pub fn observe(&mut self, view: &SessionView) {
        match Listed::of(view) {
            Some(listed) => {
                self.listed.insert(view.id.clone(), listed);
            }
            None => {
                self.listed.remove(&view.id);
            }
        }
    }

    pub fn forget(&mut self, session: &SessionId) {
        self.listed.remove(session);
    }

    /// A replies read brought `asks`, the requests pending for this machine
    /// now; `current` says whether it was made for the account still signed in.
    pub fn read(&mut self, current: bool, asks: Vec<NewSessionAsk>) {
        if !current || !self.enabled {
            return;
        }
        self.asked = asks
            .into_iter()
            .filter(|ask| ask.machine == self.machine)
            .collect();
    }

    /// The next thing to do, if anything is due: decide the requests read
    /// (start one at a time; fail those that can't be), then report.
    pub fn next_work(&mut self, now: Duration) -> Option<NewSessionWork> {
        if !self.enabled {
            return None;
        }
        if self.creating.is_none() {
            let waiting: Vec<NewSessionAsk> = self
                .asked
                .iter()
                .filter(|ask| !self.kept.handled(&ask.id))
                .cloned()
                .collect();
            for ask in waiting {
                let Some(scope) = self.folder(&ask) else {
                    self.kept
                        .decided(&ask.id, failed(NewSessionFailure::FolderUnknown));
                    continue;
                };
                // Never started again from now on, even if this service stops
                // before the daemon answers.
                self.kept.creating(&ask.id);
                self.creating = Some(ask.id.clone());
                return Some(NewSessionWork::Create {
                    id: ask.id,
                    request: create(scope, ask.agent),
                });
            }
        }
        if self.reporting || now < self.retry_at {
            return None;
        }
        while let Some(report) = self.kept.next_report() {
            match status_request(&report.id, &self.machine, &report.outcome) {
                Some(request) => {
                    self.reporting = true;
                    return Some(NewSessionWork::Report(request));
                }
                // An id the contract can't carry: nothing to tell.
                None => self.kept.reported(),
            }
        }
        None
    }

    /// The folder `ask` names among those listed here: the one of its
    /// `folder_session` when that's listed under the name, else the only one
    /// of that name.
    fn folder(&self, ask: &NewSessionAsk) -> Option<ScopeRoot> {
        let named = ask
            .folder_session
            .as_ref()
            .and_then(|session| self.listed.get(session))
            .filter(|listed| listed.folder == ask.folder);
        if let Some(listed) = named {
            return Some(listed.scope.clone());
        }
        let mut scopes = self
            .listed
            .values()
            .filter(|listed| listed.folder == ask.folder)
            .map(|listed| &listed.scope);
        let first = scopes.next()?;
        scopes.all(|scope| scope == first).then(|| first.clone())
    }

    /// The daemon started the session for request `id` (or refused).
    pub fn created(&mut self, id: &str, result: Result<SessionView, ErrorCode>) {
        if self.creating.as_deref() == Some(id) {
            self.creating = None;
        }
        let outcome = match result {
            Ok(view) => {
                self.observe(&view);
                let name = wire::label(&view.name, 64).unwrap_or_else(|| "session".into());
                Outcome::Created {
                    session: Started {
                        id: view.id.as_str().to_owned(),
                        name,
                    },
                }
            }
            Err(_) => failed(NewSessionFailure::Refused),
        };
        self.kept.decided(id, outcome);
    }

    /// A report finished; `current` says whether it was made for the account
    /// still signed in.
    pub fn reported(&mut self, current: bool, outcome: Result<(), FeedFailure>, now: Duration) {
        self.reporting = false;
        if !current || !self.enabled {
            return;
        }
        match outcome {
            Ok(()) => {
                self.retry = RETRY_MIN;
                self.kept.reported();
            }
            // A report the cloud refuses (an expired request's, say) is
            // dropped, not retried forever.
            Err(FeedFailure::Invalid) => self.kept.reported(),
            Err(failure) => {
                let wait = match failure {
                    FeedFailure::RateLimited(Some(delay)) => delay.min(RATE_LIMIT_MAX),
                    FeedFailure::Unavailable(Some(delay)) => delay.min(RETRY_MAX),
                    _ => self.retry,
                };
                self.retry = self.retry.saturating_mul(2).min(RETRY_MAX);
                self.retry_at = now + wait;
            }
        }
    }
}

fn failed(failure: NewSessionFailure) -> Outcome {
    Outcome::Failed { failure }
}

/// The daemon request for a new session in `scope`: a login shell in the
/// folder with a generated name, `agent`'s command typed in as a startup
/// command, the daemon's environment and size (no terminal asks for one).
fn create(scope: ScopeRoot, agent: Option<Agent>) -> CreateSession {
    CreateSession {
        cwd: Some(scope.path().to_path_buf()),
        scope: Some(scope),
        name: None,
        argv: None,
        startup_cmds: agent
            .map(|agent| vec![agent.command().to_owned()])
            .unwrap_or_default(),
        env: Vec::new(),
        cols: 0,
        rows: 0,
        machine: None,
        prefill: None,
    }
}

#[cfg(test)]
mod tests;
