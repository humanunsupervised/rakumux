//! What the session list is grouped and filtered by: the tabs on its top
//! row. Status (a section per state, as the user reads it), All folders (a
//! section per folder, the folder filter of `folders.rs`) and App (a
//! section per agent or program). One tab is active at a time: it decides
//! the section headers and the only filter that applies. Each tab keeps its
//! own filter while another one is active; none is saved. (The feed isn't
//! one of them: it's a view of its own, `app/feed`.)
//!
//! Everything here is derived from a `SessionView` alone; nothing draws.

use std::cmp::Ordering;
use std::path::Path;

use session_core::activity::ActivityPhase;
use session_core::model::{Lifecycle, ScopeRoot, SessionStatus, SessionView};

/// A tab of the list's top row: what the list is grouped and filtered by.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Grouping {
    /// All folders: a section per folder, narrowed by the folder filter.
    Folder,
    /// A section per app: the agent or program a session runs.
    App,
    /// A section per status: what a session is doing; where the list
    /// starts.
    #[default]
    Status,
}

impl Grouping {
    /// The tabs, left to right: what wants the user first.
    pub const ALL: [Grouping; 3] = [Grouping::Status, Grouping::Folder, Grouping::App];

    /// The tab `delta` places along, stopping at either end.
    pub fn step(self, delta: isize) -> Grouping {
        let last = Self::ALL.len() as isize - 1;
        let index = Self::ALL.iter().position(|g| *g == self).unwrap_or(0) as isize;
        Self::ALL[(index + delta).clamp(0, last) as usize]
    }

    /// What the tab says while it filters nothing.
    pub fn name(self) -> &'static str {
        match self {
            Grouping::Folder => "Folders",
            Grouping::App => "App",
            Grouping::Status => "Status",
        }
    }

    /// What its sections are called: `3 apps`.
    pub fn plural(self) -> &'static str {
        match self {
            Grouping::Folder => "folders",
            Grouping::App => "apps",
            Grouping::Status => "statuses",
        }
    }
}

/// The linked git worktree a session works in when it isn't its folder's
/// own working tree (one inside the repo, like `worktrees/feature`, or one
/// elsewhere): that worktree's root. Under the Folders tab such sessions
/// go under a header for their worktree, inside their folder. Follows the
/// session's git context, so it moves with the agent.
pub fn linked_worktree(session: &SessionView) -> Option<&Path> {
    let git = session.git.as_ref()?;
    git.worktree.as_ref()?;
    (!session.scope.path().starts_with(&git.repo_root)).then_some(git.repo_root.as_path())
}

/// How a worktree's header names `root` inside `folder`: its path there
/// (`worktrees/feature`), or for one elsewhere its own directory's name.
pub fn worktree_label(folder: &ScopeRoot, root: &Path) -> String {
    match root.strip_prefix(folder.path()) {
        Ok(inside) if !inside.as_os_str().is_empty() => inside.display().to_string(),
        _ => root.file_name().map_or_else(
            || root.display().to_string(),
            |name| name.to_string_lossy().into_owned(),
        ),
    }
}

/// Programs the list shows as "shell", and the App tab groups as `shell`.
const SHELLS: &[&str] = &[
    "sh", "bash", "zsh", "fish", "dash", "ksh", "mksh", "tcsh", "csh", "nu", "xonsh", "elvish",
];

/// Whether `program` is a shell (`bash`, `zsh`, …).
pub fn is_shell(program: &str) -> bool {
    SHELLS.contains(&program)
}

/// The app a session runs, as the App tab groups it, in lowercase: the
/// agent the daemon detected or the session reported (`agent.kind`); else,
/// while it runs, the program in its foreground unless that's a shell; else
/// the program it was started with (`launch.argv`) unless that's a shell;
/// else `shell`. An ended session keeps its agent, so a dead Claude Code
/// session stays under `claude` (reviving it resumes the conversation).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct AppKey(String);

impl AppKey {
    /// What a session that runs nothing else is grouped as.
    pub const SHELL: &'static str = "shell";

    /// The app `session` runs (see [`AppKey`]).
    pub fn of(session: &SessionView) -> AppKey {
        let agent = session
            .agent
            .as_ref()
            .map(|agent| agent.kind.trim())
            .filter(|kind| !kind.is_empty());
        let foreground = session
            .terminal
            .as_ref()
            .filter(|_| session.status == SessionStatus::Live)
            .and_then(|terminal| terminal.foreground_process.as_deref())
            .filter(|program| !program.is_empty() && !is_shell(program));
        let launched = session
            .launch
            .argv
            .as_ref()
            .and_then(|argv| argv.first())
            .map(|program| program.rsplit('/').next().unwrap_or(program))
            .filter(|program| !program.is_empty() && !is_shell(program));
        AppKey::new(agent.or(foreground).or(launched).unwrap_or(Self::SHELL))
    }

    /// The app called `name` (any case).
    pub fn new(name: &str) -> AppKey {
        AppKey(name.to_lowercase())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// What a session is doing as the user reads it: the Status tab's
/// sections, in the order they're shown (what wants the user first,
/// unread replies among it, then what's busy, then what's over, and ready
/// sessions last). Derived from a running session's
/// lifecycle, with the agent's activity phase telling a wait for the model
/// apart from other work; else its status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum StatusGroup {
    /// `Blocked`: a question or a permission prompt.
    WaitingForYou,
    /// `Done`: a finished turn nobody has looked at (an unread reply).
    Unread,
    /// `Working`, running a tool included.
    Working,
    /// `Working`, waiting for the model's response.
    WaitingForResponse,
    /// Live with nothing to say (no agent, or one whose state can't be
    /// told): a shell or another program.
    Running,
    Exited,
    Dead,
    Unknown,
    /// Ready: `Idle`, nothing to do until the user says so (a reply that's
    /// been read). Listed by when each was last read, not in an order the
    /// user sets ([`StatusGroup::by_last_read`]).
    Ready,
}

impl StatusGroup {
    /// Every status, in display order.
    pub const ALL: [StatusGroup; 9] = [
        StatusGroup::WaitingForYou,
        StatusGroup::Unread,
        StatusGroup::Working,
        StatusGroup::WaitingForResponse,
        StatusGroup::Running,
        StatusGroup::Exited,
        StatusGroup::Dead,
        StatusGroup::Unknown,
        StatusGroup::Ready,
    ];

    /// The status `session` is in (see [`StatusGroup`]).
    pub fn of(session: &SessionView) -> StatusGroup {
        match session.status {
            SessionStatus::Live => {
                let phase = session.activity.as_ref().map(|activity| activity.phase);
                match session.lifecycle {
                    Lifecycle::Working if phase == Some(ActivityPhase::WaitingForResponse) => {
                        StatusGroup::WaitingForResponse
                    }
                    Lifecycle::Working => StatusGroup::Working,
                    Lifecycle::Blocked => StatusGroup::WaitingForYou,
                    Lifecycle::Done => StatusGroup::Unread,
                    Lifecycle::Idle => StatusGroup::Ready,
                    Lifecycle::Unknown | Lifecycle::None => StatusGroup::Running,
                }
            }
            SessionStatus::Exited => StatusGroup::Exited,
            SessionStatus::Dead => StatusGroup::Dead,
            SessionStatus::Unknown => StatusGroup::Unknown,
        }
    }

    /// Whether its sessions are listed by when each was last read
    /// (`last_active_at`, which going into a session sets), most recent
    /// first, instead of in an order the user sets: ready.
    pub fn by_last_read(self) -> bool {
        self == StatusGroup::Ready
    }

    /// How its order is saved (`sessions.section_order`): the name it had
    /// when orders were first saved, so renaming it keeps them.
    pub fn saved_name(self) -> &'static str {
        match self {
            StatusGroup::Unread => "done",
            StatusGroup::Ready => "idle",
            status => status.label(),
        }
    }

    /// The words the list uses for it.
    pub fn label(self) -> &'static str {
        match self {
            StatusGroup::WaitingForYou => "waiting for you",
            StatusGroup::Unread => "unread",
            StatusGroup::Working => "working",
            StatusGroup::WaitingForResponse => "waiting for response",
            StatusGroup::Ready => "ready",
            StatusGroup::Running => "running",
            StatusGroup::Exited => "exited",
            StatusGroup::Dead => "dead",
            StatusGroup::Unknown => "unknown",
        }
    }
}

/// Ready's sub-sections under Status, in order: pinned sessions first, then
/// the others by when each was last read, most recent first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ReadPeriod {
    /// Pinned, however long ago it was read.
    Pinned,
    /// Within the last hour.
    LastHour,
    /// Within the last day.
    LastDay,
    /// Longer ago.
    Older,
}

impl ReadPeriod {
    /// The period, at `now`, of a session last read at `last_read` (both
    /// in seconds); a clock behind the read counts it as just now.
    pub fn of(now: i64, last_read: i64) -> ReadPeriod {
        let age = now.saturating_sub(last_read);
        if age <= 3_600 {
            ReadPeriod::LastHour
        } else if age <= 86_400 {
            ReadPeriod::LastDay
        } else {
            ReadPeriod::Older
        }
    }

    /// The sub-section of `session` at `now` when it's ready: pinned, else by
    /// its last read (`last_active_at`, which going in sets); `None` for any
    /// other status.
    pub fn of_done(now: i64, session: &SessionView) -> Option<ReadPeriod> {
        StatusGroup::of(session).by_last_read().then(|| {
            if session.pinned {
                ReadPeriod::Pinned
            } else {
                ReadPeriod::of(now, session.last_active_at)
            }
        })
    }

    /// Its name in its header.
    pub fn label(self) -> &'static str {
        match self {
            ReadPeriod::Pinned => "pinned",
            ReadPeriod::LastHour => "last hour",
            ReadPeriod::LastDay => "last day",
            ReadPeriod::Older => "older",
        }
    }
}

/// A section of the list under the App or Status tab.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Section {
    App(AppKey),
    Status(StatusGroup),
}

impl Section {
    /// Whether its sessions are listed by when each was last read
    /// ([`StatusGroup::by_last_read`]), so they can't be moved.
    pub fn by_last_read(&self) -> bool {
        matches!(self, Section::Status(status) if status.by_last_read())
    }

    /// How the order of its sessions is saved (`sessions.section_order`):
    /// `app:claude`, `status:working`.
    pub fn key(&self) -> String {
        match self {
            Section::App(app) => format!("app:{}", app.as_str()),
            Section::Status(status) => format!("status:{}", status.saved_name()),
        }
    }

    /// The section `session` is in under `grouping`; `None` under All
    /// folders, whose sections are folders.
    pub fn of(grouping: Grouping, session: &SessionView) -> Option<Section> {
        match grouping {
            Grouping::Folder => None,
            Grouping::App => Some(Section::App(AppKey::of(session))),
            Grouping::Status => Some(Section::Status(StatusGroup::of(session))),
        }
    }

    /// The tab it belongs to.
    pub fn grouping(&self) -> Grouping {
        match self {
            Section::App(_) => Grouping::App,
            Section::Status(_) => Grouping::Status,
        }
    }

    /// Its name in the header, the modal and the tab: `claude`, `working`.
    pub fn label(&self) -> &str {
        match self {
            Section::App(app) => app.as_str(),
            Section::Status(status) => status.label(),
        }
    }
}

/// Put sections in display order, which activity never changes: statuses
/// in [`StatusGroup`]'s order; apps that are agents (some session's
/// `agent.kind`) first, then other programs, then the shell, each by name.
pub fn sort_sections(sections: &mut [Section], sessions: &[SessionView]) {
    let is_agent = |app: &AppKey| {
        sessions.iter().any(|s| {
            s.agent
                .as_ref()
                .is_some_and(|agent| AppKey::new(agent.kind.trim()) == *app)
        })
    };
    let rank = |section: &Section| match section {
        Section::App(app) if app.as_str() == AppKey::SHELL => 2,
        Section::App(app) if is_agent(app) => 0,
        Section::App(_) => 1,
        Section::Status(_) => 0,
    };
    sections.sort_by(|a, b| {
        rank(a).cmp(&rank(b)).then_with(|| match (a, b) {
            (Section::App(a), Section::App(b)) => a.cmp(b),
            (Section::Status(a), Section::Status(b)) => a.cmp(b),
            (Section::App(_), Section::Status(_)) => Ordering::Less,
            (Section::Status(_), Section::App(_)) => Ordering::Greater,
        })
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::testing::view;
    use session_core::activity::AgentActivity;
    use session_core::model::AgentInfo;

    fn agent(kind: &str) -> Option<AgentInfo> {
        Some(AgentInfo {
            kind: kind.into(),
            ..Default::default()
        })
    }

    fn running(program: &str) -> SessionView {
        let mut session = view("A", "a", 0);
        if let Some(terminal) = session.terminal.as_mut() {
            terminal.foreground_process = Some(program.into());
        }
        session
    }

    #[test]
    fn tabs_step_without_wrapping() {
        // Status is the grouping the list starts with, and the first tab.
        assert_eq!(Grouping::default(), Grouping::Status);
        let names: Vec<&str> = Grouping::ALL.iter().map(|g| g.name()).collect();
        assert_eq!(names, ["Status", "Folders", "App"]);
        assert_eq!(Grouping::Status.step(-1), Grouping::Status);
        assert_eq!(Grouping::Status.step(1), Grouping::Folder);
        assert_eq!(Grouping::Folder.step(1), Grouping::App);
        assert_eq!(Grouping::App.step(1), Grouping::App);
        assert_eq!(Grouping::App.step(-1), Grouping::Folder);
        assert_eq!(Grouping::Folder.step(-1), Grouping::Status);
    }

    #[test]
    fn the_app_is_the_agent_then_the_foreground_program_then_the_command_then_shell() {
        // A shell at its prompt.
        assert_eq!(AppKey::of(&running("zsh")).as_str(), "shell");
        // Another program in the foreground.
        assert_eq!(AppKey::of(&running("htop")).as_str(), "htop");
        // The agent wins over its host process, whatever its case.
        let mut session = running("node");
        session.agent = agent("Claude");
        assert_eq!(AppKey::of(&session).as_str(), "claude");
        // An ended session keeps its agent, not a stale foreground program.
        session.status = SessionStatus::Dead;
        session.terminal = None;
        assert_eq!(AppKey::of(&session).as_str(), "claude");
        let mut ended = running("htop");
        ended.status = SessionStatus::Exited;
        assert_eq!(AppKey::of(&ended).as_str(), "shell");
        // What it was started with, when nothing else says.
        ended.launch.argv = Some(vec!["/usr/local/bin/codex".into(), "--yolo".into()]);
        assert_eq!(AppKey::of(&ended).as_str(), "codex");
        ended.launch.argv = Some(vec!["bash".into()]);
        assert_eq!(AppKey::of(&ended).as_str(), "shell");
        // An empty agent kind says nothing.
        let mut blank = running("vim");
        blank.agent = agent(" ");
        assert_eq!(AppKey::of(&blank).as_str(), "vim");
    }

    #[test]
    fn statuses_follow_the_lifecycle_activity_and_status() {
        let mut session = view("A", "a", 0);
        let status = |s: &SessionView| StatusGroup::of(s);
        assert_eq!(status(&session), StatusGroup::Running);
        for (lifecycle, group) in [
            (Lifecycle::Blocked, StatusGroup::WaitingForYou),
            (Lifecycle::Done, StatusGroup::Unread),
            (Lifecycle::Working, StatusGroup::Working),
            (Lifecycle::Idle, StatusGroup::Ready),
            (Lifecycle::Unknown, StatusGroup::Running),
            (Lifecycle::None, StatusGroup::Running),
        ] {
            session.lifecycle = lifecycle;
            assert_eq!(status(&session), group, "{lifecycle:?}");
        }
        session.lifecycle = Lifecycle::Working;
        for (phase, group) in [
            (
                ActivityPhase::WaitingForResponse,
                StatusGroup::WaitingForResponse,
            ),
            (ActivityPhase::RunningTool, StatusGroup::Working),
            (ActivityPhase::Working, StatusGroup::Working),
        ] {
            session.activity = Some(AgentActivity {
                phase,
                active_subagents: 0,
                updated_at: 1,
            });
            assert_eq!(status(&session), group, "{phase:?}");
        }
        // The phase only tells working apart: a blocked agent waits for you.
        session.lifecycle = Lifecycle::Blocked;
        session.activity.as_mut().unwrap().phase = ActivityPhase::WaitingForResponse;
        assert_eq!(status(&session), StatusGroup::WaitingForYou);
        for (state, group) in [
            (SessionStatus::Exited, StatusGroup::Exited),
            (SessionStatus::Dead, StatusGroup::Dead),
            (SessionStatus::Unknown, StatusGroup::Unknown),
        ] {
            session.status = state;
            assert_eq!(status(&session), group);
        }
        assert_eq!(
            StatusGroup::WaitingForResponse.label(),
            "waiting for response"
        );
    }

    #[test]
    fn done_sessions_fall_in_the_last_hour_the_last_day_or_older() {
        let now = 1_000_000;
        assert_eq!(ReadPeriod::of(now, now), ReadPeriod::LastHour);
        assert_eq!(ReadPeriod::of(now, now - 3_600), ReadPeriod::LastHour);
        assert_eq!(ReadPeriod::of(now, now - 3_601), ReadPeriod::LastDay);
        assert_eq!(ReadPeriod::of(now, now - 86_400), ReadPeriod::LastDay);
        assert_eq!(ReadPeriod::of(now, now - 86_401), ReadPeriod::Older);
        assert_eq!(ReadPeriod::of(now, now + 60), ReadPeriod::LastHour);
        let mut session = view("A", "a", now - 7_200);
        assert_eq!(ReadPeriod::of_done(now, &session), None);
        session.lifecycle = Lifecycle::Idle;
        assert_eq!(
            ReadPeriod::of_done(now, &session),
            Some(ReadPeriod::LastDay)
        );
        assert_eq!(ReadPeriod::LastDay.label(), "last day");
    }

    #[test]
    fn unread_and_done_keep_the_orders_saved_as_done_and_idle() {
        assert_eq!(StatusGroup::Unread.label(), "unread");
        assert_eq!(StatusGroup::Ready.label(), "ready");
        assert_eq!(Section::Status(StatusGroup::Unread).key(), "status:done");
        assert_eq!(Section::Status(StatusGroup::Ready).key(), "status:idle");
        assert_eq!(
            Section::Status(StatusGroup::Working).key(),
            "status:working"
        );
        assert_eq!(StatusGroup::ALL.last(), Some(&StatusGroup::Ready));
    }

    #[test]
    fn sections_sort_agents_then_programs_then_the_shell_and_statuses_in_order() {
        let mut claude = running("node");
        claude.agent = agent("claude");
        let mut codex = running("node");
        codex.agent = agent("codex");
        let sessions = vec![claude, codex, running("htop"), running("cargo")];
        let app = |name: &str| Section::App(AppKey::new(name));
        let mut sections = vec![
            app("shell"),
            app("htop"),
            app("codex"),
            app("cargo"),
            app("claude"),
        ];
        sort_sections(&mut sections, &sessions);
        assert_eq!(
            sections,
            [
                app("claude"),
                app("codex"),
                app("cargo"),
                app("htop"),
                app("shell")
            ]
        );
        let mut statuses = vec![
            Section::Status(StatusGroup::Dead),
            Section::Status(StatusGroup::Working),
            Section::Status(StatusGroup::WaitingForYou),
        ];
        sort_sections(&mut statuses, &sessions);
        assert_eq!(
            statuses,
            [
                Section::Status(StatusGroup::WaitingForYou),
                Section::Status(StatusGroup::Working),
                Section::Status(StatusGroup::Dead),
            ]
        );
        assert_eq!(Section::of(Grouping::Folder, &sessions[0]), None);
        assert_eq!(
            Section::of(Grouping::App, &sessions[0]).map(|s| s.label().to_string()),
            Some("claude".into())
        );
    }
}
