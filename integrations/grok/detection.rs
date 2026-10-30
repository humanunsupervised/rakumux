//! Grok Build screen profile (captured 1.0.70, fullscreen and `--minimal`).
//!
//! One native binary, `~/.grok/downloads/grok-<os>-<arch>`, linked as
//! `~/.grok/bin/grok` and `~/.grok/bin/agent`. Started as `grok` its process
//! is named `grok`; by path, after the file; as `agent`, only the file it runs
//! says it's Grok (Cursor's CLI is called `agent` too).
use super::*;

/// The title while a turn runs: a spinner frame, then what it's doing
/// (`⠼ - Waiting for response… - <session title> - grok`).
fn is_grok_busy_title(title: &str) -> bool {
    let mut chars = title.chars();
    chars
        .next()
        .is_some_and(|glyph| is_braille(glyph) && glyph != '\u{2800}')
        && chars.as_str().starts_with(" - ")
}

/// The title between turns: `grok`, or `<session title> - grok`.
fn is_grok_title(title: &str) -> bool {
    title == "grok" || title.ends_with(" - grok")
}

/// The status row while a turn runs: a spinner frame, then what it's doing
/// with an ellipsis (`⠹ Waiting for response… 1.4s`, `⠴ Responding… 0.6s`).
/// A prompt that waits for the user shows `◆` instead.
pub(super) fn is_grok_status_line(line: &str) -> bool {
    split_glyph(line)
        .is_some_and(|(glyph, rest)| is_braille(glyph) && glyph != '\u{2800}' && rest.contains('…'))
}

/// The input box's first line (`│ ❯ …`), empty, drafted or with a suggestion.
fn is_grok_prompt_line(line: &str) -> bool {
    line.starts_with("│ ❯")
}

pub(super) const PROFILE: Profile = Profile {
    label: "grok",
    names: &[
        "grok",
        "grok-linux-x86_64",
        "grok-linux-aarch64",
        "grok-macos-aarch64",
        "grok-macos-x86_64",
    ],
    packages: &[&[".grok", "downloads"], &[".grok", "bin"]],
    title_hint: |title| is_grok_title(title) || is_grok_busy_title(title),
    blocked: &[
        // A tool's permission prompt (`⚠ Action Required - ⠸ - …`).
        marker(Region::Title, Test::Any(&["action required"])),
        // Under a permission prompt or a question.
        marker(
            Region::Bottom(2),
            Test::Any(&["tab:next option", "tab:next answer"]),
        ),
        marker(
            Region::Bottom(16),
            Test::Any(&[
                "waiting on answers for",
                "no, reject (type to add feedback)",
            ]),
        ),
        marker(
            Region::Screen,
            Test::Any(&["do you trust the contents of this directory?"]),
        ),
    ],
    working: &[
        marker(Region::Title, Test::Line(is_grok_busy_title)),
        marker(Region::Bottom(2), Test::Any(&["ctrl+c:cancel"])),
        marker(Region::Bottom(14), Test::Line(is_grok_status_line)),
    ],
    idle: &[
        marker(Region::Bottom(12), Test::Line(is_grok_prompt_line)),
        // The footer, or `--minimal`'s (`minimal · /help`, then `❯` and the
        // model's line).
        marker(
            Region::Bottom(3),
            Test::Any(&["shift+tab:mode", "minimal · /help"]),
        ),
        marker(Region::Title, Test::Line(is_grok_title)),
    ],
    quiet: Lifecycle::Unknown,
    output_means_working: false,
    approvals_on_screen: false,
};

#[cfg(test)]
mod tests {
    use super::*;

    fn agent(name: &str, cmdline: &[&str], exe: Option<&str>) -> Option<&'static str> {
        let process = ProcessInfo {
            pid: 1,
            name: name.into(),
            cmdline: cmdline.iter().map(|word| word.to_string()).collect(),
            exe: exe.map(std::path::PathBuf::from),
        };
        identify(Some(&process), "").map(|profile| profile.label)
    }

    #[test]
    fn grok_is_named_by_its_name_its_install_path_or_the_file_it_runs() {
        let file = "/home/me/.grok/downloads/grok-linux-x86_64";
        assert_eq!(agent("grok", &["grok", "--resume"], None), Some("grok"));
        // Started by path: the kernel cuts the name to 15 bytes, procinfo widens it.
        assert_eq!(agent("grok-linux-x86_64", &[file], None), Some("grok"));
        assert_eq!(
            agent("agent", &["/home/me/.grok/bin/agent"], None),
            Some("grok")
        );
        // `agent` from `~/.local/bin`: only the file it runs says which agent.
        assert_eq!(agent("agent", &["agent"], None), None);
        assert_eq!(agent("agent", &["agent"], Some(file)), Some("grok"));
        // Over ssh, the title alone.
        let ssh = ProcessInfo {
            pid: 1,
            name: "ssh".into(),
            cmdline: vec!["ssh".into(), "box".into()],
            exe: None,
        };
        let by_title = |title| identify(Some(&ssh), title).map(|profile| profile.label);
        assert_eq!(by_title("Fix the parser - grok"), Some("grok"));
        assert_eq!(
            by_title("⠼ - Responding… - Fix the parser - grok"),
            Some("grok")
        );
    }

    #[test]
    fn grok_titles_and_lines() {
        assert!(is_grok_busy_title(
            "⠼ - Waiting for response… - Say hello - grok"
        ));
        assert!(!is_grok_busy_title("Say hello - grok"));
        assert!(!is_grok_busy_title(
            "⚠ Action Required - ⠸ - Run the requested command… - grok"
        ));
        assert!(is_grok_title("grok") && is_grok_title("Fix the parser - grok"));
        assert!(!is_grok_title("grokking - vim"));
        assert!(is_grok_status_line(
            "⠋ Waiting for response… 3.0s   3.0s ⇣20 [stop]"
        ));
        assert!(is_grok_status_line("⠴ Responding… 0.6s"));
        assert!(!is_grok_status_line("◆ Run the requested command… 2.9s"));
        // The logo is braille too, but never a frame and a word.
        assert!(!is_grok_status_line("⠀⠀⠀⣠⣾⠿⠛⠛⠛⠛⢀⡴⠁⠀   Grok Build  1.0.70"));
        assert!(is_grok_prompt_line("│ ❯ fix the parser   │"));
    }
}
