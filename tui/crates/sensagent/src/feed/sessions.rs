//! This machine's session list for rakumux.com (`contracts/feed/`
//! `sessions-upload`): while the account's Feed switch is on, the status
//! service keeps what the TUI's Status and Folders tabs group by for each
//! session it follows (never a private one, and the daemon's list leaves out
//! dead ones) and uploads the whole list whenever it changes, at most every
//! [`SETTLE`], and every [`HEARTBEAT`] meanwhile, so the website can tell a
//! machine that's away from one with nothing running. Driven by the owner
//! thread with its own clock.

use std::collections::BTreeMap;
use std::time::Duration;

use serde::Serialize;
use session_core::activity::ActivityPhase;
use session_core::ids::SessionId;
use session_core::model::{Lifecycle, SessionStatus, SessionView};

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
}

impl SessionState {
    /// `view` as listed, or `None` when it isn't: private, dead, or of an
    /// unknown status.
    fn of(view: &SessionView) -> Option<Self> {
        if view.private {
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
            .and_then(|agent| match agent.kind.as_str() {
                "claude" | "claude-code" => Some("claude-code"),
                "codex" => Some("codex"),
                "opencode" | "opencode2" => Some("opencode"),
                _ => None,
            });
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
        })
    }
}

/// Shells, which the App tab files as `shell` (the TUI's `grouping::is_shell`).
const SHELLS: &[&str] = &[
    "sh", "bash", "zsh", "fish", "dash", "ksh", "mksh", "tcsh", "csh", "nu", "xonsh", "elvish",
];

/// The app the TUI's App tab files `view` under (its `grouping::AppKey`), when it isn't `shell`:
/// the agent the session names, else its foreground program while it runs, else the program it
/// started with; never a shell. Lowercase, a name only, within the contract's 64 characters.
fn program_of(view: &SessionView) -> Option<String> {
    let usable = |program: &str| {
        let program = program.trim();
        (!program.is_empty() && !SHELLS.contains(&program)).then(|| program.to_owned())
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
    enabled: bool,
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
    pub fn new(machine: String) -> Self {
        Self {
            machine,
            enabled: false,
            sessions: BTreeMap::new(),
            sent: None,
            sending: None,
            next_send: Duration::ZERO,
            heartbeat_at: Duration::ZERO,
            retry_at: Duration::ZERO,
            retry: RETRY_MIN,
        }
    }

    /// Follow the account: signed in with its Feed switch on. Turned on
    /// again, the list is sent at once.
    pub fn set_enabled(&mut self, enabled: bool, now: Duration) {
        if enabled == self.enabled {
            return;
        }
        self.enabled = enabled;
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

    /// The list as it stands, the most recently used first, within the
    /// contract's bound.
    fn list(&self) -> Vec<SessionState> {
        let mut list: Vec<SessionState> = self.sessions.values().cloned().collect();
        list.sort_by(|a, b| (b.last_active_at, &a.id).cmp(&(a.last_active_at, &b.id)));
        list.truncate(MAX_SESSIONS);
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
        let request = wire::sessions_upload(&self.machine, &list, wall_ms)?;
        self.sending = Some(list);
        Some(request)
    }

    /// An upload finished; `current` says whether it was made for the
    /// account still signed in.
    pub fn done(&mut self, current: bool, outcome: Result<u32, FeedFailure>, now: Duration) {
        let sent = self.sending.take();
        if !current || !self.enabled {
            return;
        }
        match outcome {
            Ok(_) => {
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

#[cfg(test)]
mod tests;
