//! GitHub Copilot CLI's screen (captured 1.0.105): an input box with a
//! footer under it, approval and trust dialogs in a box, and `ask_user`
//! questions above the footer. The input box is drawn between two rules
//! (`───`), or, in a terminal that answers its colour queries (Rakumux's),
//! between half-block bars (`╻▄▄▄`, `┃ input`, `╹▀▀▀`); the footer is the
//! last line either way. Its hooks report approvals (`notification`), so
//! they keep winning over the screen.
use super::*;

/// `│ ❯ 1. Yes`, `│   2. Yes, and don't ask again …`: a yes option of a boxed
/// dialog (its border, then the selection mark).
pub(super) fn is_boxed_yes_option(line: &str) -> bool {
    let inside = line.trim_start_matches(['│', ' ']);
    if inside.len() == line.len() {
        return false;
    }
    let option = inside.trim_start_matches(['❯', '›', '>', ' ']);
    let Some(rest) = option.strip_prefix(|c: char| c.is_ascii_digit()) else {
        return false;
    };
    rest.strip_prefix(". ").is_some_and(|label| {
        label
            .split(|c: char| !c.is_alphabetic())
            .next()
            .is_some_and(|word| word.eq_ignore_ascii_case("yes"))
    })
}

/// The footer's activity while a turn runs: a spinner glyph, then `Working`,
/// `Executing` or `Cancelling` (the model's own intent may stand in for
/// `Working`, which the `esc` hint beside it covers).
pub(super) fn is_copilot_activity_line(line: &str) -> bool {
    split_glyph(line).is_some_and(|(glyph, rest)| {
        !glyph.is_alphanumeric()
            && ["Working", "Executing", "Cancelling", "Resuming session"]
                .iter()
                .any(|label| rest.starts_with(label))
    })
}

/// The bar-drawn input box's top and bottom.
fn is_input_bar(line: &str) -> bool {
    line.starts_with("╻▄") || line.starts_with("╹▀")
}

pub(super) const PROFILE: Profile = Profile {
    label: "copilot",
    names: &["copilot"],
    // Also the native binary's own package, `@github/copilot-linux-x64/copilot`.
    packages: &[&["@github", "copilot"]],
    title_hint: |title| title == "GitHub Copilot" || title.ends_with(" - GitHub Copilot"),
    blocked: &[
        // Tool approvals and folder trust: `Do you want to run this command?`,
        // `… create <path>?`, `Do you trust the files in this folder?`.
        marker(
            Region::Bottom(25),
            Test::And(
                &Test::Any(&["do you want to", "do you trust the files in this folder"]),
                &Test::Line(is_boxed_yes_option),
            ),
        ),
        // `ask_user`: `Copilot needs information.` and its own hints (a
        // form's every field ends them `ctrl+d decline · esc cancel`, its
        // title further up than the bottom lines).
        marker(
            Region::Bottom(12),
            Test::Any(&[
                "copilot needs information",
                "enter accept · ctrl+d decline",
                "ctrl+d decline · esc cancel",
            ]),
        ),
    ],
    // The footer, under the input box.
    working: &[
        marker(
            Region::Bottom(3),
            Test::Any(&[
                "esc esc interrupt",
                "esc esc stop agents",
                "esc esc edit prompt",
                // Not `esc again to clear input` or `… rewind`: idle.
                "esc again to interrupt",
                "esc again to cancel",
                "esc again to stop agents",
                "esc again to edit prompt",
            ]),
        ),
        marker(Region::Bottom(3), Test::Line(is_copilot_activity_line)),
    ],
    idle: &[
        marker(Region::PromptBox, Test::Line(|line| line.starts_with('❯'))),
        marker(Region::Bottom(4), Test::Line(is_input_bar)),
        // The footer while idle, typing, or with the sessions sidebar open
        // (whose pane border hides the rules).
        marker(
            Region::Bottom(3),
            Test::Any(&["/ commands", "@ files", "→ close sidebar"]),
        ),
    ],
    quiet: Lifecycle::Unknown,
    output_means_working: false,
    approvals_on_screen: false,
};

#[cfg(test)]
mod tests {
    use super::*;

    fn agent_of(name: &str, cmdline: &[&str]) -> Option<&'static str> {
        let process = ProcessInfo {
            pid: 4242,
            name: name.into(),
            cmdline: cmdline.iter().map(|s| s.to_string()).collect(),
            exe: None,
        };
        identify(Some(&process), "").map(|p| p.label)
    }

    #[test]
    fn every_install_is_named_copilot() {
        let native = "/home/me/.nvm/versions/node/v22.20.0/lib/node_modules/@github/copilot/node_modules/@github/copilot-linux-x64/copilot";
        for (name, cmdline) in [
            // npm: the Node loader is the foreground process…
            (
                "node",
                &[
                    "node",
                    "/home/me/.nvm/versions/node/v22.20.0/bin/copilot",
                    "--yolo",
                ][..],
            ),
            // …and the native binary its child, whose Linux name is its main thread's.
            ("MainThread", &[native][..]),
            // Homebrew and the install script run the native binary.
            (
                "MainThread",
                &["/opt/homebrew/bin/copilot", "--resume=x"][..],
            ),
            ("copilot", &["/home/me/.local/bin/copilot"][..]),
            ("npm exec @githu", &["npx", "@github/copilot@latest"][..]),
            (
                "node",
                &[
                    "node",
                    "/usr/lib/node_modules/@github/copilot/npm-loader.js",
                ][..],
            ),
        ] {
            assert_eq!(
                agent_of(name, cmdline),
                Some("copilot"),
                "{name} {cmdline:?}"
            );
        }
        // GitHub's other CLIs aren't.
        assert_eq!(agent_of("gh", &["gh", "copilot", "suggest"]), None);
        assert_eq!(agent_of("node", &["node", "/srv/copilot-proxy.js"]), None);
    }

    #[test]
    fn its_title_names_it_behind_an_opaque_wrapper() {
        assert_eq!(
            identify(None, "GitHub Copilot").map(|p| p.label),
            Some("copilot")
        );
        assert_eq!(
            identify(None, "Fix the parser - GitHub Copilot").map(|p| p.label),
            Some("copilot")
        );
        assert_eq!(
            identify(None, "copilot notes.md - NVIM").map(|p| p.label),
            None
        );
    }

    #[test]
    fn dialogs_and_activity_lines() {
        assert!(is_boxed_yes_option("│ ❯ 1. Yes                     │"));
        assert!(is_boxed_yes_option(
            "│   2. Yes, and don't ask again for `cargo test` in this repo (~/project) │"
        ));
        assert!(is_boxed_yes_option("    │ │ ❯ 1. Yes │ │"));
        assert!(!is_boxed_yes_option(
            "│   3. No, and tell Copilot what to do differently │"
        ));
        // A numbered list in a reply isn't a dialog.
        assert!(!is_boxed_yes_option("1. Yes, the tests pass"));
        assert!(!is_boxed_yes_option("❯ 1. Yes"));
        assert!(is_copilot_activity_line("◎ Working esc esc edit prompt"));
        assert!(is_copilot_activity_line(
            "◉ Working · 24 B esc esc interrupt"
        ));
        assert!(is_copilot_activity_line("● Cancelling"));
        assert!(!is_copilot_activity_line("● Done: the tool ran."));
        assert!(!is_copilot_activity_line("Working on it"));
    }
}
