//! Presentation helpers shared by the session list, tab bar and detail.

use std::path::Path;

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use session_core::model::{GitInfo, Lifecycle, ScopeRoot, SessionStatus, SessionView};

use crate::app::grouping::{is_shell, ReadPeriod, StatusGroup};
use crate::icons::Icons;
use crate::layout::{char_width, str_width};
use crate::theme;

#[path = "../../../integrations/presentation.rs"]
mod agents;

pub use crate::layout::PIN_MARK;

/// `⎇ main`, or `⎇ @1a2b3c4` when HEAD is detached, with the branch mark of
/// the user's icon style.
pub fn branch_label(git: &GitInfo, icons: Icons) -> Option<String> {
    let mark = icons.branch();
    match (&git.branch, &git.detached_at) {
        (Some(branch), _) if !branch.is_empty() => Some(format!("{mark} {branch}")),
        (_, Some(sha)) if !sha.is_empty() => Some(format!("{mark} @{sha}")),
        _ => None,
    }
}

/// The name of `git`'s repository, as a place shows it before its branch:
/// none when it's the working tree's own folder name (a main checkout
/// named like its repository), ignoring case.
pub fn repo_label(git: &GitInfo) -> Option<&str> {
    let repo = git.repo.as_deref().filter(|repo| !repo.is_empty())?;
    let folder = git.repo_root.file_name().and_then(|name| name.to_str());
    (!folder.is_some_and(|folder| folder.eq_ignore_ascii_case(repo))).then_some(repo)
}

/// Unix seconds now.
pub fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Compact age: `now`, `45s`, `12m`, `3h`, `2d`, `5w`.
pub fn relative_time(now: i64, then: i64) -> String {
    let secs = (now - then).max(0);
    match secs {
        0..=9 => "now".into(),
        10..=59 => format!("{secs}s"),
        60..=3_599 => format!("{}m", secs / 60),
        3_600..=86_399 => format!("{}h", secs / 3_600),
        86_400..=1_209_599 => format!("{}d", secs / 86_400),
        _ => format!("{}w", secs / 604_800),
    }
}

/// When a session last had activity (Unix seconds), for its row's age: its
/// agent conversation's last prompt or message, else when it was last used.
pub fn last_activity(session: &SessionView) -> i64 {
    session.last_message_at.unwrap_or(session.last_active_at)
}

/// Busy spinner; static live/attention/ended markers otherwise.
pub fn status_glyph(session: &SessionView) -> (&'static str, Style) {
    let p = theme::palette();
    match session.status {
        SessionStatus::Dead => ("○", Style::default().fg(p.text_muted)),
        SessionStatus::Exited => ("✕", Style::default().fg(p.accent_pink)),
        SessionStatus::Unknown => ("?", Style::default().fg(p.text_muted)),
        SessionStatus::Live => match session.lifecycle {
            Lifecycle::Blocked => ("◐", Style::default().fg(p.accent_orange)),
            Lifecycle::Working => (
                super::utils::get_spinner_char(),
                Style::default().fg(p.accent_primary),
            ),
            Lifecycle::Done => ("●", Style::default().fg(p.accent_gold)),
            Lifecycle::Idle | Lifecycle::Unknown | Lifecycle::None => {
                ("●", Style::default().fg(p.accent_green))
            }
        },
    }
}

/// The mark of a status in the Status tab's headers and modal: the one
/// its sessions have (see [`status_glyph`] and [`activity_badge`]).
pub fn status_group_glyph(status: StatusGroup) -> (&'static str, Style) {
    let p = theme::palette();
    match status {
        StatusGroup::WaitingForYou => ("◐", Style::default().fg(p.accent_orange)),
        StatusGroup::Unread => ("●", Style::default().fg(p.accent_gold)),
        StatusGroup::Working | StatusGroup::WaitingForResponse => (
            super::utils::get_spinner_char(),
            Style::default().fg(p.accent_primary),
        ),
        StatusGroup::Ready => ("○", read_period_style(ReadPeriod::LastHour)),
        StatusGroup::Dead => ("○", Style::default().fg(p.text_muted)),
        StatusGroup::Running => ("●", Style::default().fg(p.accent_green)),
        StatusGroup::Exited => ("✕", Style::default().fg(p.accent_pink)),
        StatusGroup::Unknown => ("?", Style::default().fg(p.text_muted)),
    }
}

/// How far [`read_period_style`] fades the unread amber for a done
/// session read within the last hour, and within the last day.
const DONE_FADE: (f32, f32) = (0.3, 0.5);

/// The colour of a ready session's mark (and of ready's sub-headers' clocks)
/// by when it was last read: the amber of an unread reply, quieter within
/// the hour, quieter still within the day, and muted after that.
pub fn read_period_style(period: ReadPeriod) -> Style {
    let p = theme::palette();
    let amber = |amount: f32| match p.faded(p.accent_gold, amount) {
        Some(color) => Style::default().fg(color),
        // 16 colours have no shades in between: the amber, dimmed.
        None => Style::default()
            .fg(p.accent_gold)
            .add_modifier(Modifier::DIM),
    };
    match period {
        // Pinned sessions are any age; their header has the brightest.
        ReadPeriod::Pinned | ReadPeriod::LastHour => amber(DONE_FADE.0),
        ReadPeriod::LastDay => amber(DONE_FADE.1),
        ReadPeriod::Older => Style::default().fg(p.text_muted),
    }
}

/// Right-side agent activity: busy, done, unread reply, or needs input.
/// Terminal presence/exit markers remain separate for non-agent sessions.
pub fn activity_badge(session: &SessionView) -> Option<(&'static str, Style)> {
    activity_badge_at(session, now_secs())
}

/// [`activity_badge`] at `now` (seconds), which a ready session's colour
/// depends on.
pub fn activity_badge_at(session: &SessionView, now: i64) -> Option<(&'static str, Style)> {
    if session.status != SessionStatus::Live
        || (session.agent.is_none() && session.activity.is_none())
    {
        return None;
    }
    match session.lifecycle {
        Lifecycle::Working | Lifecycle::Blocked | Lifecycle::Done => Some(status_glyph(session)),
        Lifecycle::Idle => Some((
            "○",
            read_period_style(ReadPeriod::of(now, session.last_active_at)),
        )),
        Lifecycle::None | Lifecycle::Unknown => None,
    }
}

/// Whether a session wants the user's attention (tab bar marker).
pub fn needs_attention(session: &SessionView) -> bool {
    session.status == SessionStatus::Live
        && matches!(session.lifecycle, Lifecycle::Blocked | Lifecycle::Done)
}

/// The subagents a running agent has working, as an agent mark and their
/// number (`◈ 3`, after the app and branch); nothing without any.
pub fn subagent_badge(session: &SessionView, icons: Icons) -> Option<String> {
    let count = session
        .activity
        .as_ref()
        .filter(|_| session.status == SessionStatus::Live)
        .map_or(0, |activity| activity.active_subagents);
    (count > 0).then(|| format!("{} {count}", icons.agents()))
}

/// `~/…` for paths under the home directory.
pub fn abbreviate_home(path: &Path) -> String {
    if let Some(home) = std::env::var_os("HOME").filter(|h| !h.is_empty()) {
        if let Ok(rest) = path.strip_prefix(Path::new(&home)) {
            return if rest.as_os_str().is_empty() {
                "~".into()
            } else {
                format!("~/{}", rest.display())
            };
        }
    }
    path.display().to_string()
}

/// A place a session worked in, as its row says: its path in the
/// session's folder (`worktrees/fix`), the folder's name for the folder
/// itself, else `~/…` under the home directory, else the whole path.
pub fn place_label(folder: &ScopeRoot, path: &Path) -> String {
    match path.strip_prefix(folder.path()) {
        Ok(inside) if inside.as_os_str().is_empty() => folder.path().file_name().map_or_else(
            || abbreviate_home(path),
            |name| name.to_string_lossy().into_owned(),
        ),
        Ok(inside) => inside.display().to_string(),
        Err(_) => abbreviate_home(path),
    }
}

/// `text` in at most `width` columns, cut at its start (`…/fix`): a path's
/// end says the most.
pub fn truncate_start(text: &str, width: usize) -> String {
    if str_width(text) <= width {
        return text.to_string();
    }
    if width == 0 {
        return String::new();
    }
    let mut tail: Vec<char> = Vec::new();
    let mut used = 0;
    for ch in text.chars().rev() {
        let w = char_width(ch);
        if used + w > width - 1 {
            break;
        }
        used += w;
        tail.push(ch);
    }
    std::iter::once('…').chain(tail.into_iter().rev()).collect()
}

/// Columns the reported text needs before it's worth adding to a row.
const REPORT_MIN: usize = 12;

/// Second line of a list row: the colour-coded app first, then its git
/// branch, state and report. Only the app name gets its identifying colour.
pub fn secondary_line(
    session: &SessionView,
    width: usize,
    icons: Icons,
    detail_style: Style,
) -> Line<'static> {
    facts_line(session, width, icons, detail_style, true)
}

/// [`secondary_line`], with the branch (`branch`) or without (the session
/// list says it on the session's place line).
pub fn facts_line(
    session: &SessionView,
    width: usize,
    icons: Icons,
    detail_style: Style,
    branch: bool,
) -> Line<'static> {
    let text = secondary_text(session, width, icons, branch);
    let Some(program) = session_program(session) else {
        return Line::styled(text, detail_style);
    };
    let program_style = detail_style.fg(program_color(program));
    match text.strip_prefix(program) {
        Some(rest) => Line::from(vec![
            Span::styled(program.to_string(), program_style),
            Span::styled(rest.to_string(), detail_style),
        ]),
        // The app is first, so a line cut before its name ends is all app.
        None => Line::from(Span::styled(text, program_style)),
    }
}

/// Quiet blue app labels, independent of lifecycle and its status colours
/// (rows, and the App tab's headers and modal).
pub fn program_color(program: &str) -> Color {
    let p = theme::palette();
    agents::color(&program.to_ascii_lowercase()).unwrap_or(p.label_blue)
}

/// Description fitted to `width` columns. A plain shell with nothing to
/// say shows "shell"; reports are included only when there's room.
fn secondary_text(session: &SessionView, width: usize, icons: Icons, branch: bool) -> String {
    let base = session_facts(session, icons, branch);
    let report = session
        .reported
        .as_ref()
        .and_then(|r| r.text.as_deref())
        .map(str::trim)
        .filter(|t| !t.is_empty());
    let line = match (base.is_empty(), report) {
        (true, None) => "shell".to_string(),
        (true, Some(report)) => report.to_string(),
        (false, None) => base.join(" · "),
        (false, Some(report)) => {
            let base = base.join(" · ");
            let room = width.saturating_sub(str_width(&base) + 3);
            if room >= REPORT_MIN.min(str_width(report)) {
                format!("{base} · {}", truncate(report, room))
            } else {
                base
            }
        }
    };
    truncate(&line, width)
}

/// Agent or program, branch (when `branch`), state. (The worktree isn't
/// shown for now.)
fn session_facts(session: &SessionView, icons: Icons, branch: bool) -> Vec<String> {
    let mut parts = Vec::new();
    if let Some(program) = session_program(session) {
        parts.push(program.to_string());
    }
    if let Some(git) = session.git.as_ref().filter(|_| branch) {
        parts.extend(branch_label(git, icons));
    }
    match session.status {
        SessionStatus::Dead => parts.push("dead · ⏎ revive".into()),
        SessionStatus::Exited => parts.push("exited · ⏎ restart".into()),
        // A live session's status is its mark on the right; only
        // subagents are said here.
        _ => parts.extend(subagent_badge(session, icons)),
    }
    parts
}

/// Prefer a detected agent over its host process. Ended sessions don't
/// advertise a stale running app, and ordinary shells need no app label.
fn session_program(session: &SessionView) -> Option<&str> {
    if matches!(session.status, SessionStatus::Dead | SessionStatus::Exited) {
        return None;
    }
    session
        .agent
        .as_ref()
        .map(|agent| agent.kind.as_str())
        .filter(|kind| !kind.is_empty())
        .or_else(|| {
            session
                .terminal
                .as_ref()
                .and_then(|terminal| terminal.foreground_process.as_deref())
                .filter(|process| !process.is_empty() && !is_shell(process))
        })
}

/// `text` in at most `width` columns, cut in the middle with `…` (paths
/// keep both ends).
pub fn truncate_middle(text: &str, width: usize) -> String {
    if str_width(text) <= width {
        return text.to_string();
    }
    if width <= 1 {
        return "…".chars().take(width).collect();
    }
    let keep = width - 1;
    let head_room = keep / 2;
    let mut tail_room = keep - head_room;
    let mut head = String::new();
    let mut used = 0;
    for ch in text.chars() {
        if used + char_width(ch) > head_room {
            break;
        }
        used += char_width(ch);
        head.push(ch);
    }
    // Room the head couldn't use (a wide character at the cut) goes to the
    // tail.
    tail_room += head_room - used;
    let mut tail: Vec<char> = Vec::new();
    let mut used = 0;
    for ch in text.chars().rev() {
        if used + char_width(ch) > tail_room {
            break;
        }
        used += char_width(ch);
        tail.push(ch);
    }
    head.push('…');
    head.extend(tail.into_iter().rev());
    head
}

/// `text` in at most `width` columns, ending in `…` when cut.
pub fn truncate(text: &str, width: usize) -> String {
    crate::layout::truncate(text, width)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::testing::view;
    use session_core::model::Reported;

    #[test]
    fn relative_times() {
        assert_eq!(relative_time(100, 95), "now");
        assert_eq!(relative_time(100, 55), "45s");
        assert_eq!(relative_time(10_000, 10_000 - 720), "12m");
        assert_eq!(relative_time(20_000, 20_000 - 3 * 3_600), "3h");
        assert_eq!(relative_time(1_000_000, 1_000_000 - 2 * 86_400), "2d");
        assert_eq!(relative_time(10_000_000, 10_000_000 - 5 * 604_800), "5w");
        assert_eq!(relative_time(0, 50), "now");
    }

    #[test]
    fn a_sessions_activity_is_its_agents_last_message_else_its_last_use() {
        let mut session = view("A", "docker", 1_000);
        assert_eq!(last_activity(&session), 1_000);
        // Gone into (or revived) after the agent's last message: still the message.
        session.last_message_at = Some(400);
        assert_eq!(last_activity(&session), 400);
        session.last_message_at = Some(1_500);
        assert_eq!(last_activity(&session), 1_500);
    }

    fn git(branch: Option<&str>, detached: Option<&str>, worktree: Option<&str>) -> GitInfo {
        GitInfo {
            repo_root: "/repo".into(),
            branch: branch.map(Into::into),
            detached_at: detached.map(Into::into),
            worktree: worktree.map(Into::into),
            repo: None,
            origin: None,
        }
    }

    #[test]
    fn cutting_text_measures_columns_not_characters() {
        // 🌿 takes two columns.
        assert_eq!(truncate("🌿 main", 7), "🌿 main");
        assert_eq!(truncate("🌿 main", 6), "🌿 ma…");
        assert_eq!(crate::layout::str_width(&truncate("🌿🌿🌿🌿", 5)), 5);
        let cut = truncate_middle("~/code/📁/very-long/path", 12);
        assert!(crate::layout::str_width(&cut) <= 12, "{cut}");
        assert!(cut.contains('…') && cut.ends_with("path"), "{cut}");
        assert_eq!(truncate_middle("short", 12), "short");
    }

    #[test]
    fn branch_labels() {
        assert_eq!(
            branch_label(&git(Some("main"), None, None), Icons::Plain).as_deref(),
            Some("⎇ main")
        );
        assert_eq!(
            branch_label(&git(None, Some("1a2b3c4"), None), Icons::Plain).as_deref(),
            Some("⎇ @1a2b3c4")
        );
        assert_eq!(branch_label(&git(None, None, None), Icons::Plain), None);
    }

    #[test]
    fn a_places_repository_is_named_unless_its_folder_says_it() {
        let named = |root: &str, repo: Option<&str>| GitInfo {
            repo_root: root.into(),
            repo: repo.map(Into::into),
            ..git(Some("main"), None, None)
        };
        assert_eq!(
            repo_label(&named("/work/focal-md-paper-links", Some("focal-md"))),
            Some("focal-md")
        );
        assert_eq!(repo_label(&named("/code/rakumux", Some("rakumux"))), None);
        assert_eq!(repo_label(&named("/code/Rakumux", Some("rakumux"))), None);
        assert_eq!(repo_label(&named("/code/rakumux", None)), None);
        assert_eq!(repo_label(&named("/code/rakumux", Some(""))), None);
        assert_eq!(repo_label(&named("/", Some("root"))), Some("root"));
    }

    #[test]
    fn secondary_text_shows_program_then_git_and_never_the_folder_or_a_live_status() {
        let mut session = view("A", "a", 0);
        session.cwd = "/repo/docs".into();
        // Working: the mark on the right says so, the line doesn't.
        session.lifecycle = Lifecycle::Working;
        assert_eq!(secondary_text(&session, 80, Icons::Plain, true), "shell");
        // The branch, not the worktree.
        session.git = Some(git(Some("main"), None, None));
        assert_eq!(secondary_text(&session, 80, Icons::Plain, true), "⎇ main");
        session.git = Some(git(None, Some("1a2b3c4"), Some("feature-x")));
        session.agent = Some(session_core::model::AgentInfo {
            kind: "claude".into(),
            ..Default::default()
        });
        assert_eq!(
            secondary_text(&session, 80, Icons::Plain, true),
            "claude · ⎇ @1a2b3c4"
        );
        // An ended session, with no mark on the right, says so.
        session.status = SessionStatus::Dead;
        assert_eq!(
            secondary_text(&session, 80, Icons::Plain, true),
            "⎇ @1a2b3c4 · dead · ⏎ revive"
        );
    }

    #[test]
    fn places_are_named_in_their_folder_else_from_home_and_cut_at_their_start() {
        let folder = ScopeRoot::from_canonical("/srv/repo".into());
        assert_eq!(place_label(&folder, Path::new("/srv/repo")), "repo");
        assert_eq!(
            place_label(&folder, Path::new("/srv/repo/worktrees/fix")),
            "worktrees/fix"
        );
        assert_eq!(place_label(&folder, Path::new("/opt/other")), "/opt/other");
        assert_eq!(truncate_start("worktrees/fix", 20), "worktrees/fix");
        assert_eq!(truncate_start("worktrees/fix", 6), "…s/fix");
        assert_eq!(truncate_start("界界界", 4), "…界");
        assert_eq!(truncate_start("abc", 0), "");
        let mut session = view("A", "a", 0);
        session.git = Some(git(Some("main"), None, None));
        assert_eq!(secondary_text(&session, 80, Icons::Plain, false), "shell");
    }

    #[test]
    fn app_names_use_muted_blues_without_colouring_the_branch_or_state() {
        let p = theme::palette();
        let detail_style = Style::default().fg(p.text_muted);
        let mut session = view("A", "a", 0);
        session.git = Some(git(Some("main"), None, None));
        session.lifecycle = Lifecycle::Working;
        for (program, color) in [
            ("claude", p.label_blue),
            ("codex", p.label_steel_blue),
            ("opencode", p.label_slate_blue),
            ("pi", p.label_blue),
            ("gemini", p.label_blue),
            ("cargo", p.label_blue),
            ("OpenCode", p.label_slate_blue),
        ] {
            session.terminal.as_mut().unwrap().foreground_process = Some(program.into());
            let line = secondary_line(&session, 80, Icons::Plain, detail_style);
            assert_eq!(line.spans[0].content, program);
            assert_eq!(line.spans[0].style.fg, Some(color));
            assert_eq!(line.spans[1].content, " · ⎇ main");
            assert_eq!(line.spans[1].style, detail_style);
        }
    }

    #[test]
    fn app_first_descriptions_fit_even_when_the_app_name_is_truncated() {
        let mut session = view("A", "a", 0);
        session.git = Some(git(Some("長い-branch-name"), None, None));
        session.agent = Some(session_core::model::AgentInfo {
            kind: "opencode".into(),
            ..Default::default()
        });
        let style = Style::default().fg(theme::palette().text_muted);
        for icons in Icons::ALL {
            for width in 0..80 {
                let line = secondary_line(&session, width, icons, style);
                assert!(line.width() <= width);
                assert_eq!(
                    line.spans[0].style.fg,
                    Some(theme::palette().label_slate_blue)
                );
            }
        }
        let line = secondary_line(&session, 5, Icons::Plain, style);
        assert_eq!(line.spans[0].content, "open…");
    }

    #[test]
    fn reported_text_follows_when_there_is_room() {
        let mut session = view("A", "a", 0);
        session.git = Some(git(Some("main"), None, None));
        session.lifecycle = Lifecycle::Done;
        session.reported = Some(Reported {
            state: None,
            text: Some("Tests green, ready for review".into()),
            result: None,
            at: 0,
        });
        assert_eq!(
            secondary_text(&session, 80, Icons::Plain, true),
            "⎇ main · Tests green, ready for review"
        );
        assert_eq!(
            secondary_text(&session, 23, Icons::Plain, true),
            "⎇ main · Tests green, …"
        );
        // Too little room for the report: the facts alone.
        assert_eq!(secondary_text(&session, 15, Icons::Plain, true), "⎇ main");
        assert_eq!(secondary_text(&session, 5, Icons::Plain, true), "⎇ ma…");
        // With nothing else, the report is the whole line.
        session.git = None;
        session.lifecycle = Lifecycle::None;
        assert_eq!(
            secondary_text(&session, 12, Icons::Plain, true),
            "Tests green…"
        );
    }

    #[test]
    fn labels_and_glyphs() {
        let mut session = view("A", "a", 0);
        session.lifecycle = Lifecycle::Blocked;
        assert_eq!(status_glyph(&session).0, "◐");
        assert!(needs_attention(&session));
        session.status = SessionStatus::Exited;
        session.exit_code = Some(2);
        assert_eq!(status_glyph(&session).0, "✕");
        session.status = SessionStatus::Dead;
        assert_eq!(status_glyph(&session).0, "○");
        assert!(!needs_attention(&session));
        assert_eq!(truncate("abcdef", 4), "abc…");
        assert_eq!(truncate("abc", 4), "abc");
        assert_eq!(truncate_middle("/tmp/one/two/three", 10), "/tmp…three");
        assert_eq!(truncate_middle("short", 10), "short");
    }

    #[test]
    fn busy_phases_spin_and_subagent_counts_do_not_animate_attention_or_done() {
        use session_core::activity::{ActivityPhase, AgentActivity};
        let mut session = view("A", "agent", 0);
        session.lifecycle = Lifecycle::Working;
        session.activity = Some(AgentActivity {
            phase: ActivityPhase::WaitingForResponse,
            active_subagents: 3,
            updated_at: 1,
        });
        assert_eq!(
            subagent_badge(&session, Icons::Plain).as_deref(),
            Some("◈ 3")
        );
        assert!("⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏".contains(status_glyph(&session).0));
        session.lifecycle = Lifecycle::Blocked;
        assert_eq!(status_glyph(&session).0, "◐");
        session.lifecycle = Lifecycle::Done;
        session.activity = None;
        assert_eq!(status_glyph(&session).0, "●");
    }

    #[test]
    fn the_description_says_no_status_and_the_subagent_badge_follows_the_app() {
        use session_core::activity::{ActivityPhase, AgentActivity};
        let mut session = view("A", "agent", 0);
        session.agent = Some(session_core::model::AgentInfo {
            kind: "claude".into(),
            ..Default::default()
        });
        session.lifecycle = Lifecycle::Working;
        let style = Style::default();
        let text = |session: &SessionView, icons: Icons| -> String {
            secondary_line(session, 80, icons, style)
                .spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect()
        };
        // None running: no badge, and no status words (the mark on the
        // right says it).
        assert_eq!(text(&session, Icons::Plain), "claude");
        assert_eq!(subagent_badge(&session, Icons::Plain), None);
        session.activity = Some(AgentActivity {
            phase: ActivityPhase::RunningTool,
            active_subagents: 2,
            updated_at: 1,
        });
        assert_eq!(text(&session, Icons::Plain), "claude · ◈ 2");
        assert_eq!(text(&session, Icons::Emoji), "claude · 🤖 2");
        assert!(text(&session, Icons::Nerd).ends_with("\u{f06a9} 2"));
        // A report comes after it, room allowing.
        session.reported = Some(session_core::model::Reported {
            state: None,
            text: Some("reviewing the diff".into()),
            result: None,
            at: 1,
        });
        assert_eq!(
            text(&session, Icons::Plain),
            "claude · ◈ 2 · reviewing the diff"
        );
        // An ended session has nothing running.
        session.status = SessionStatus::Exited;
        assert_eq!(subagent_badge(&session, Icons::Plain), None);
    }

    #[test]
    fn done_marks_are_a_quieter_amber_than_unread_and_fade_with_the_read() {
        let p = theme::palette();
        let now = 1_000_000;
        let mut session = view("A", "agent", now - 60);
        session.agent = Some(session_core::model::AgentInfo {
            kind: "claude".into(),
            ..Default::default()
        });
        session.lifecycle = Lifecycle::Done;
        let unread = activity_badge_at(&session, now).unwrap().1.fg;
        assert_eq!(unread, Some(p.accent_gold));
        session.lifecycle = Lifecycle::Idle;
        let at = |ago: i64| {
            let mut read = session.clone();
            read.last_active_at = now - ago;
            activity_badge_at(&read, now).unwrap()
        };
        let (hour, day, older) = (at(60), at(7_200), at(3 * 86_400));
        assert!([hour.0, day.0, older.0].iter().all(|mark| *mark == "○"));
        assert_eq!(hour.1, read_period_style(ReadPeriod::LastHour));
        assert_eq!(day.1, read_period_style(ReadPeriod::LastDay));
        assert_eq!(older.1.fg, Some(p.text_muted));
        if let Some(shade) = p.faded(p.accent_gold, 0.3) {
            assert_eq!(hour.1.fg, Some(shade));
            assert_ne!(day.1.fg, hour.1.fg);
            assert_ne!(hour.1.fg, unread);
        }
        // Ready's header has the last hour's amber; dead stays muted.
        assert_eq!(
            status_group_glyph(StatusGroup::Ready).1,
            read_period_style(ReadPeriod::LastHour)
        );
        assert_eq!(
            status_group_glyph(StatusGroup::Dead).1.fg,
            Some(p.text_muted)
        );
    }

    #[test]
    fn agent_badges_distinguish_unread_idle_busy_and_input_without_marking_shells() {
        let mut session = view("A", "agent", 0);
        session.lifecycle = Lifecycle::Idle;
        assert!(activity_badge(&session).is_none());
        session.agent = Some(session_core::model::AgentInfo {
            kind: "opencode".into(),
            ..Default::default()
        });
        assert_eq!(activity_badge(&session).unwrap().0, "○");
        session.lifecycle = Lifecycle::Done;
        assert_eq!(activity_badge(&session).unwrap().0, "●");
        session.lifecycle = Lifecycle::Working;
        assert!("⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏".contains(activity_badge(&session).unwrap().0));
        session.lifecycle = Lifecycle::Blocked;
        assert_eq!(activity_badge(&session).unwrap().0, "◐");
        for status in [SessionStatus::Exited, SessionStatus::Dead] {
            session.status = status;
            assert!(activity_badge(&session).is_none());
        }
    }

    #[test]
    fn secondary_text_falls_back_to_shell_and_shows_the_program() {
        let mut session = view("A", "a", 0);
        session.cwd = "/repo/deep/inside".into();
        assert_eq!(secondary_text(&session, 40, Icons::Plain, true), "shell");
        session.terminal = None;
        assert_eq!(secondary_text(&session, 40, Icons::Plain, true), "shell");
        session.terminal = Some(session_core::model::TerminalView {
            pid: Some(1),
            foreground_process: Some("htop".into()),
            title: String::new(),
            cols: 80,
            rows: 24,
            controlled: false,
            alternate_screen: true,
        });
        assert_eq!(secondary_text(&session, 40, Icons::Plain, true), "htop");
        session.agent = Some(session_core::model::AgentInfo {
            kind: "claude".into(),
            ..Default::default()
        });
        session.lifecycle = Lifecycle::Blocked;
        assert_eq!(secondary_text(&session, 40, Icons::Plain, true), "claude");
    }
}
