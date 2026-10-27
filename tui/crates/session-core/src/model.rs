//! Session domain model as seen on the wire.
//!
//! A session is a durable record (owned by the daemon's registry) plus, while
//! it runs, a live terminal. Sessions are grouped by scope: the canonical
//! folder Rakumux was launched from. Sessions have no git knowledge.

use crate::ids::SessionId;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Canonical absolute folder that groups sessions. Clients only ever see the
/// sessions of one scope.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ScopeRoot(PathBuf);

impl ScopeRoot {
    /// Canonicalize `dir` (resolving symlinks) to form a scope.
    pub fn resolve(dir: &Path) -> std::io::Result<Self> {
        Ok(Self(std::fs::canonicalize(dir)?))
    }

    /// Wrap an already-canonical path (registry, wire).
    pub fn from_canonical(path: PathBuf) -> Self {
        Self(path)
    }

    pub fn path(&self) -> &Path {
        &self.0
    }
}

/// Whether the session's terminal exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionStatus {
    /// Terminal running.
    Live,
    /// Process ended; final screen and scrollback retained.
    Exited,
    /// Record exists but the terminal is gone (reboot, daemon restart).
    Dead,
    #[serde(other)]
    Unknown,
}

/// What the program in the session (usually a coding agent) is doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Lifecycle {
    Working,
    /// Waiting for the user (permission prompt, question).
    Blocked,
    /// Finished a turn and nobody has looked yet.
    Done,
    Idle,
    /// An agent is running but its state can't be determined.
    #[default]
    Unknown,
    /// No agent detected (plain shell).
    None,
}

/// Who created a session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Creator {
    User,
    Session { id: SessionId },
}

/// Coding agent detected or reported in a session.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct AgentInfo {
    /// e.g. "claude", "codex", "opencode", "pi".
    pub kind: String,
    /// The agent's own session/conversation id, when reported.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_session_path: Option<String>,
}

/// State a session reported about itself (`session.report`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reported {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<Lifecycle>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<ReportResult>,
    /// Unix seconds.
    pub at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReportResult {
    pub summary: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<String>,
}

/// How to bring a session's program back after its terminal is lost.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResumeSpec {
    pub argv: Vec<String>,
    pub source: ResumeSource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResumeSource {
    StartupCommand,
    /// The daemon found the conversation of an agent it knows (Claude Code,
    /// Codex, OpenCode); the command resumes it.
    KnownAgent,
    ReportedByAgent,
    /// The program the session last ran in the foreground (not an agent the
    /// daemon can resume). A revive types it at the prompt without running
    /// it.
    LastCommand,
    #[serde(other)]
    Unknown,
}

impl ResumeSource {
    /// A revive runs the command (`false`: it is only typed).
    pub fn runs(self) -> bool {
        self != ResumeSource::LastCommand
    }
}

/// What a session runs. `argv: None` means the user's login shell.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct LaunchSpec {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub argv: Option<Vec<String>>,
    /// Typed into the shell after it starts.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub startup_cmds: Vec<String>,
}

/// A live agent's current turn, as `SessionView::agent_turn` shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentTurn {
    /// Tells this turn from the next one (opaque).
    pub id: u64,
    /// The messages the agent wrote in it: reasoning, replies, tool calls.
    pub messages: u32,
    /// The agent's own conversation the turn belongs to, when known: one
    /// chat, for its one automatic AI summary.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conversation_id: Option<String>,
}

/// A session as returned by the API.

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionView {
    pub id: SessionId,
    pub name: String,
    pub scope: ScopeRoot,
    /// True when the caller is this session.
    #[serde(default)]
    pub is_self: bool,
    pub cwd: PathBuf,
    pub status: SessionStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    #[serde(default)]
    pub lifecycle: Lifecycle,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<AgentInfo>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reported: Option<Reported>,
    /// Fresh hook activity. Absent when unavailable, expired or not live.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activity: Option<crate::activity::AgentActivity>,
    /// The agent's latest output as one line (its reasoning, a tool call or
    /// its reply), read by the daemon from the agent's conversation log on
    /// this machine. Live sessions running a known agent only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_output: Option<String>,
    /// What the user last typed to the agent, as one line, read the same
    /// way as `agent_output`. Live sessions running a known agent only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_prompt: Option<String>,
    /// The agent's current turn: which one, and how many messages it has
    /// written since the user's last prompt. Its content is
    /// `session_turn`'s (capability `session.turn`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_turn: Option<AgentTurn>,
    pub created_by: Creator,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<SessionId>,
    #[serde(default)]
    pub depth: u8,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<SessionId>,
    pub launch: LaunchSpec,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resume: Option<ResumeSpec>,
    /// The name was generated by Rakumux (no user or agent chose it), so
    /// clients may replace it with an AI-suggested one.
    #[serde(default)]
    pub auto_named: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    /// Free-form key/value labels, e.g. `{"role": "review"}`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub labels: BTreeMap<String, String>,
    /// Other sessions' agents can see this session's metadata but not read
    /// its output.
    #[serde(default)]
    pub private: bool,
    /// Unix seconds.
    pub created_at: i64,
    pub last_active_at: i64,
    /// Live terminal details; absent unless `status == Live`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal: Option<TerminalView>,
    /// Pinned by the user: listed first in its project.
    #[serde(default)]
    pub pinned: bool,
    /// Git context of the session's current directory (read from `.git`
    /// files; no git commands). `None` outside a repository.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git: Option<GitInfo>,
    /// Where the session has worked, most recent first (capability
    /// `session.places`): the git working trees its agent worked in, read
    /// files from or changed, as its conversation shows them, and its
    /// terminal's directories in them. Read by the
    /// daemon on this machine; at most [`MAX_PLACES`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub places: Vec<SessionPlace>,
}

/// The most places a session keeps: the most recently worked in.
pub const MAX_PLACES: usize = 32;

/// A place a session worked in: a git working tree (a repository's main
/// worktree or a linked one). Earlier daemons also listed directories
/// outside any repository, without `git`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionPlace {
    /// The working tree's top (`GitInfo::repo_root`). Canonical.
    pub path: PathBuf,
    /// Its git context when the session last worked there: the branch, and
    /// the linked worktree, or the commit of a detached HEAD. `None` only
    /// from an older daemon, outside any repository.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git: Option<GitInfo>,
    /// Unix seconds: when the session last worked there, by the agent's own
    /// clock for what its conversation shows.
    pub last_at: i64,
    /// Files there were changed (written or edited), not only read,
    /// searched or worked beside.
    #[serde(default)]
    pub edited: bool,
}

/// Where a directory sits in git: its branch, and the linked worktree when
/// it's not the repository's main one.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct GitInfo {
    /// Top of the working tree (the directory holding `.git`).
    pub repo_root: PathBuf,
    /// Checked-out branch; `None` when HEAD is detached.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    /// Short commit id when HEAD is detached.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detached_at: Option<String>,
    /// Name of the linked worktree (`.git/worktrees/<name>`); `None` in the
    /// main worktree.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree: Option<String>,
}

/// A project: a directory sessions belong to (the wire calls it `scope`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectView {
    pub path: ScopeRoot,
    /// Display name: the directory's own name.
    pub name: String,
    /// Sessions in the project (any status).
    pub sessions: u32,
    /// Of those, how many are running.
    pub live: u32,
    /// Unix seconds of the most recent session activity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_active_at: Option<i64>,
    /// Unix seconds when the project was first seen.
    pub added_at: i64,
    /// Git context of the project directory itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git: Option<GitInfo>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalView {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub foreground_process: Option<String>,
    #[serde(default)]
    pub title: String,
    pub cols: u16,
    pub rows: u16,
    /// A client currently holds input control.
    #[serde(default)]
    pub controlled: bool,
    #[serde(default)]
    pub alternate_screen: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_enum_values_deserialize_to_fallbacks() {
        let status: SessionStatus = serde_json::from_str("\"hibernating\"").unwrap();
        assert_eq!(status, SessionStatus::Unknown);
        let source: ResumeSource = serde_json::from_str("\"magic\"").unwrap();
        assert_eq!(source, ResumeSource::Unknown);
    }

    #[test]
    fn session_view_round_trips_with_optional_fields_omitted() {
        let view = SessionView {
            pinned: false,
            git: None,
            auto_named: false,
            description: None,
            notes: None,
            labels: Default::default(),
            private: false,
            id: SessionId::from_string("01J0000000000000000000000A"),
            name: "shell".into(),
            scope: ScopeRoot::from_canonical("/repo".into()),
            is_self: false,
            cwd: "/repo".into(),
            status: SessionStatus::Live,
            exit_code: None,
            lifecycle: Lifecycle::None,
            agent: None,
            reported: None,
            activity: None,
            created_by: Creator::User,
            parent_id: None,
            depth: 0,
            children: vec![],
            launch: LaunchSpec::default(),
            resume: None,
            created_at: 1,
            last_active_at: 2,
            terminal: None,
            agent_output: None,
            user_prompt: None,
            agent_turn: None,
            places: vec![],
        };
        let json = serde_json::to_string(&view).unwrap();
        assert!(!json.contains("exit_code"));
        assert!(!json.contains("activity"));
        assert!(!json.contains("places"));
        assert!(json.contains(r#""created_by":{"kind":"user"}"#));
        let back: SessionView = serde_json::from_str(&json).unwrap();
        assert_eq!(back, view);
    }
}
