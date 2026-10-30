//! Codex CLI composer, approval overlays, status and title.
use super::*;

pub(super) const PROFILE: Profile = Profile {
    label: "codex",
    names: &["codex"],
    packages: &[&["@openai", "codex"]],
    title_hint: |title| has_word(title, "codex"),
    blocked: &[
        marker(Region::Title, Test::Any(&["action required"])),
        marker(
            Region::Bottom(20),
            Test::Any(&[
                "press enter to confirm or esc to cancel",
                "enter to submit answer",
                "enter to submit all",
                "allow command?",
                "would you like to run the following command?",
                "would you like to make the following edits?",
                "do you trust the contents of this directory?",
                "trust this folder?",
            ]),
        ),
        marker(
            Region::Bottom(20),
            Test::All(&["update available", "skip until next version"]),
        ),
    ],
    working: &[
        marker(Region::Title, Test::Line(has_braille_spinner_word)),
        marker(Region::Bottom(12), Test::Any(&["esc to interrupt)"])),
    ],
    idle: &[marker(
        Region::Bottom(8),
        Test::Line(|line| line == "›" || line.starts_with("› ")),
    )],
    quiet: Lifecycle::Unknown,
    output_means_working: false,
    approvals_on_screen: false,
};
