//! OpenCode's screen fallback. Hooks use the V2 CLI plugin instead.
use super::*;

pub(super) fn has_opencode_progress_bar(line: &str) -> bool {
    let mut run = 0;
    for character in line.chars() {
        if character == '■' || character == '⬝' {
            run += 1;
            if run >= 4 {
                return true;
            }
        } else {
            run = 0;
        }
    }
    false
}

pub(super) const PROFILE: Profile = Profile {
    label: "opencode",
    names: &["opencode"],
    packages: &[&["opencode-ai"]],
    title_hint: |title| has_word(title, "opencode"),
    blocked: &[
        marker(
            Region::Screen,
            Test::And(
                &Test::All(&["△ permission required"]),
                &Test::Any(&["allow once", "allow always", "reject"]),
            ),
        ),
        marker(
            Region::Bottom(12),
            Test::And(
                &Test::All(&["esc dismiss"]),
                &Test::Any(&["enter confirm", "enter submit", "enter toggle"]),
            ),
        ),
        marker(
            Region::Screen,
            Test::All(&["update available", "would you like to update"]),
        ),
    ],
    working: &[
        marker(
            Region::Bottom(12),
            Test::Any(&["esc interrupt", "again to interrupt"]),
        ),
        marker(Region::Bottom(12), Test::Line(has_opencode_progress_bar)),
    ],
    idle: &[marker(Region::Screen, Test::Any(&["ctrl+p commands"]))],
    quiet: Lifecycle::Unknown,
    output_means_working: false,
};
