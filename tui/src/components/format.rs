//! Presentation helpers shared by the session list, tab bar and detail.

use std::path::Path;

use ratatui::style::Style;
use session_core::model::{GitInfo, Lifecycle, SessionStatus, SessionView};

use crate::icons::Icons;
use crate::layout::{char_width, str_width};
use crate::theme;

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

/// Status glyph as documented in help: live ● (coloured by lifecycle),
/// waiting ◐, exited ✕, dead ○.
pub fn status_glyph(session: &SessionView) -> (&'static str, Style) {
    let p = theme::palette();
    match session.status {
        SessionStatus::Dead => ("○", Style::default().fg(p.text_muted)),
        SessionStatus::Exited => ("✕", Style::default().fg(p.accent_pink)),
        SessionStatus::Unknown => ("?", Style::default().fg(p.text_muted)),
        SessionStatus::Live => match session.lifecycle {
            Lifecycle::Blocked => ("◐", Style::default().fg(p.accent_orange)),
            Lifecycle::Working => ("●", Style::default().fg(p.accent_primary)),
            Lifecycle::Done => ("●", Style::default().fg(p.accent_gold)),
            Lifecycle::Idle | Lifecycle::Unknown | Lifecycle::None => {
                ("●", Style::default().fg(p.accent_green))
            }
        },
    }
}

/// Whether a session wants the user's attention (tab bar marker).
pub fn needs_attention(session: &SessionView) -> bool {
    session.status == SessionStatus::Live
        && matches!(session.lifecycle, Lifecycle::Blocked | Lifecycle::Done)
}

pub fn lifecycle_label(lifecycle: Lifecycle) -> Option<&'static str> {
    match lifecycle {
        Lifecycle::Working => Some("working"),
        Lifecycle::Blocked => Some("waiting for you"),
        Lifecycle::Done => Some("done"),
        Lifecycle::Idle => Some("idle"),
        Lifecycle::Unknown | Lifecycle::None => None,
    }
}

/// `working`, `exited (code 1)`, `dead`; nothing for a running session
/// that hasn't said what it's doing.
pub fn status_label(session: &SessionView) -> Option<String> {
    match session.status {
        SessionStatus::Live => lifecycle_label(session.lifecycle).map(Into::into),
        SessionStatus::Exited => Some(match session.exit_code {
            Some(code) => format!("exited (code {code})"),
            None => "exited".into(),
        }),
        SessionStatus::Dead => Some("dead".into()),
        SessionStatus::Unknown => None,
    }
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

/// Columns the reported text needs before it's worth adding to a row.
const REPORT_MIN: usize = 12;

/// Second line of a list row, fitted to `width` columns: the git branch
/// (`⎇ main`, `⎇ @1a2b3c4`) and worktree (`worktree login`), the agent or
/// running program and its state, then what the session reported when
/// there's room. A plain shell with nothing to say shows "shell".
pub fn secondary_text(session: &SessionView, width: usize, icons: Icons) -> String {
    let base = session_facts(session, icons);
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

/// Branch, agent or program, state. (The worktree isn't shown for now.)
fn session_facts(session: &SessionView, icons: Icons) -> Vec<String> {
    let mut parts = Vec::new();
    if let Some(git) = &session.git {
        parts.extend(branch_label(git, icons));
    }
    match session.status {
        SessionStatus::Dead => parts.push("dead · ⏎ revive".into()),
        SessionStatus::Exited => parts.push("exited · ⏎ restart".into()),
        _ => {
            let process = session
                .terminal
                .as_ref()
                .and_then(|t| t.foreground_process.as_deref())
                .filter(|p| !p.is_empty() && !SHELLS.contains(p));
            match (&session.agent, process) {
                (Some(agent), _) => parts.push(agent.kind.clone()),
                (None, Some(process)) => parts.push(process.to_string()),
                (None, None) => {}
            }
            if let Some(label) = lifecycle_label(session.lifecycle) {
                parts.push(label.into());
            }
        }
    }
    parts
}

/// Shells, which the list shows as nothing more than "shell".
const SHELLS: &[&str] = &[
    "sh", "bash", "zsh", "fish", "dash", "ksh", "mksh", "tcsh", "csh", "nu", "xonsh", "elvish",
];

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

    fn git(branch: Option<&str>, detached: Option<&str>, worktree: Option<&str>) -> GitInfo {
        GitInfo {
            repo_root: "/repo".into(),
            branch: branch.map(Into::into),
            detached_at: detached.map(Into::into),
            worktree: worktree.map(Into::into),
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
    fn secondary_text_shows_git_then_program_then_state_and_never_the_folder() {
        let mut session = view("A", "a", 0);
        session.cwd = "/repo/docs".into();
        session.lifecycle = Lifecycle::Working;
        assert_eq!(secondary_text(&session, 80, Icons::Plain), "working");
        // The branch, not the worktree.
        session.git = Some(git(Some("main"), None, None));
        assert_eq!(
            secondary_text(&session, 80, Icons::Plain),
            "⎇ main · working"
        );
        session.git = Some(git(None, Some("1a2b3c4"), Some("feature-x")));
        session.agent = Some(session_core::model::AgentInfo {
            kind: "claude".into(),
            ..Default::default()
        });
        assert_eq!(
            secondary_text(&session, 80, Icons::Plain),
            "⎇ @1a2b3c4 · claude · working"
        );
        session.status = SessionStatus::Dead;
        assert_eq!(
            secondary_text(&session, 80, Icons::Plain),
            "⎇ @1a2b3c4 · dead · ⏎ revive"
        );
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
            secondary_text(&session, 80, Icons::Plain),
            "⎇ main · done · Tests green, ready for review"
        );
        assert_eq!(
            secondary_text(&session, 30, Icons::Plain),
            "⎇ main · done · Tests green, …"
        );
        // Too little room for the report: the facts alone.
        assert_eq!(secondary_text(&session, 19, Icons::Plain), "⎇ main · done");
        assert_eq!(secondary_text(&session, 10, Icons::Plain), "⎇ main · …");
        // With nothing else, the report is the whole line.
        session.git = None;
        session.lifecycle = Lifecycle::None;
        assert_eq!(secondary_text(&session, 12, Icons::Plain), "Tests green…");
    }

    #[test]
    fn labels_and_glyphs() {
        let mut session = view("A", "a", 0);
        assert_eq!(status_label(&session), None);
        session.lifecycle = Lifecycle::Blocked;
        assert_eq!(status_glyph(&session).0, "◐");
        assert!(needs_attention(&session));
        assert_eq!(status_label(&session).as_deref(), Some("waiting for you"));
        session.status = SessionStatus::Exited;
        session.exit_code = Some(2);
        assert_eq!(status_glyph(&session).0, "✕");
        assert_eq!(status_label(&session).as_deref(), Some("exited (code 2)"));
        session.status = SessionStatus::Dead;
        assert_eq!(status_glyph(&session).0, "○");
        assert!(!needs_attention(&session));
        assert_eq!(truncate("abcdef", 4), "abc…");
        assert_eq!(truncate("abc", 4), "abc");
        assert_eq!(truncate_middle("/tmp/one/two/three", 10), "/tmp…three");
        assert_eq!(truncate_middle("short", 10), "short");
    }

    #[test]
    fn secondary_text_falls_back_to_shell_and_shows_the_program() {
        let mut session = view("A", "a", 0);
        session.cwd = "/repo/deep/inside".into();
        assert_eq!(secondary_text(&session, 40, Icons::Plain), "shell");
        session.terminal = None;
        assert_eq!(secondary_text(&session, 40, Icons::Plain), "shell");
        session.terminal = Some(session_core::model::TerminalView {
            pid: Some(1),
            foreground_process: Some("htop".into()),
            title: String::new(),
            cols: 80,
            rows: 24,
            controlled: false,
            alternate_screen: true,
        });
        assert_eq!(secondary_text(&session, 40, Icons::Plain), "htop");
        session.agent = Some(session_core::model::AgentInfo {
            kind: "claude".into(),
            ..Default::default()
        });
        session.lifecycle = Lifecycle::Blocked;
        assert_eq!(
            secondary_text(&session, 40, Icons::Plain),
            "claude · waiting for you"
        );
    }
}
