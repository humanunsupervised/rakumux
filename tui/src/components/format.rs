//! Presentation helpers shared by the session list, tab bar and detail.

use std::path::Path;

use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
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

/// Second line of a list row: the colour-coded app first, then its git
/// branch, state and report. Only the app name gets its identifying colour.
pub fn secondary_line(
    session: &SessionView,
    width: usize,
    icons: Icons,
    detail_style: Style,
) -> Line<'static> {
    let text = secondary_text(session, width, icons);
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

/// Stable app colours from the active terminal palette, not its lifecycle.
fn program_color(program: &str) -> Color {
    let p = theme::palette();
    match program.to_ascii_lowercase().as_str() {
        "claude" => p.accent_orange,
        "codex" => p.accent_green,
        "opencode" => p.accent_purple,
        "pi" => p.accent_gold,
        "gemini" => p.accent_primary,
        _ => p.accent_pink,
    }
}

/// Description fitted to `width` columns. A plain shell with nothing to
/// say shows "shell"; reports are included only when there's room.
fn secondary_text(session: &SessionView, width: usize, icons: Icons) -> String {
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

/// A short, plain-text status suitable for a list row. Cached failures keep
/// the last successful text; details expose the refresh error and age.
pub fn activity_summary_line(summary: &session_core::model::ActivitySummary) -> String {
    let text: String = summary
        .text
        .chars()
        .map(|character| {
            if character.is_control() {
                ' '
            } else {
                character
            }
        })
        .collect();
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if !text.is_empty() {
        text
    } else if summary.error.is_some() {
        "Summary unavailable".into()
    } else {
        "Summary pending".into()
    }
}

/// Full cached status and freshness, shown above the preview rather than
/// taking any rows from an actively controlled terminal.
pub fn activity_summary_details(
    summary: &session_core::model::ActivitySummary,
    now: i64,
) -> String {
    let mut text = format!("AI activity · {}", activity_summary_line(summary));
    if summary.updated_at > 0 && !summary.text.trim().is_empty() {
        let age = relative_time(now, summary.updated_at);
        if age == "now" {
            text.push_str(" · updated now");
        } else {
            text.push_str(&format!(" · updated {age} ago"));
        }
    }
    if let Some(error) = &summary.error {
        let error: String = error
            .chars()
            .map(|character| {
                if character.is_control() {
                    ' '
                } else {
                    character
                }
            })
            .collect();
        text.push_str(&format!(" · {error}"));
    }
    text
}

/// Agent or program, branch, state. (The worktree isn't shown for now.)
fn session_facts(session: &SessionView, icons: Icons) -> Vec<String> {
    let mut parts = Vec::new();
    if let Some(program) = session_program(session) {
        parts.push(program.to_string());
    }
    if let Some(git) = &session.git {
        parts.extend(branch_label(git, icons));
    }
    match session.status {
        SessionStatus::Dead => parts.push("dead · ⏎ revive".into()),
        SessionStatus::Exited => parts.push("exited · ⏎ restart".into()),
        _ => {
            if let Some(label) = lifecycle_label(session.lifecycle) {
                parts.push(label.into());
            }
        }
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
                .filter(|process| !process.is_empty() && !SHELLS.contains(process))
        })
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
    fn secondary_text_shows_program_then_git_then_state_and_never_the_folder() {
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
            "claude · ⎇ @1a2b3c4 · working"
        );
        session.status = SessionStatus::Dead;
        assert_eq!(
            secondary_text(&session, 80, Icons::Plain),
            "⎇ @1a2b3c4 · dead · ⏎ revive"
        );
    }

    #[test]
    fn app_names_are_colour_coded_without_colouring_the_branch_or_state() {
        let p = theme::palette();
        let detail_style = Style::default().fg(p.text_muted);
        let mut session = view("A", "a", 0);
        session.git = Some(git(Some("main"), None, None));
        session.lifecycle = Lifecycle::Working;
        for (program, color) in [
            ("claude", p.accent_orange),
            ("codex", p.accent_green),
            ("opencode", p.accent_purple),
            ("pi", p.accent_gold),
            ("gemini", p.accent_primary),
            ("cargo", p.accent_pink),
            ("OpenCode", p.accent_purple),
        ] {
            session.terminal.as_mut().unwrap().foreground_process = Some(program.into());
            let line = secondary_line(&session, 80, Icons::Plain, detail_style);
            assert_eq!(line.spans[0].content, program);
            assert_eq!(line.spans[0].style.fg, Some(color));
            assert_eq!(line.spans[1].content, " · ⎇ main · working");
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
                assert_eq!(line.spans[0].style.fg, Some(theme::palette().accent_purple));
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
