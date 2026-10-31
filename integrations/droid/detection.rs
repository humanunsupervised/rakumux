//! Droid (Factory's CLI, 1.0.0): inline UI, title `⛬ <session title>`. Busy
//! status `⠙ Thinking...  (Press ESC to stop)` (or `(Please wait)`); tool
//! approvals with "Yes, allow" / "No, cancel" and the hint "Alt/Option+E to
//! inspect approval details", while the status says "Waiting for tool
//! confirmation..."; AskUser (its free answer's placeholder, or its hints:
//! `1–9 Select`, `1–9 Toggle`, `↑ back to options`), spec
//! approval, folder trust, Droid-Shield and the signed-out login menu wait
//! for the user too. Idle shows the autonomy
//! footer (`Auto (Off) · all actions require approval`, `Auto (High) · …`),
//! which stays while it works, so it only counts after the others. Strings
//! from the 1.0.0 bundle; the login screen captured signed out.
use super::*;

/// Droid's busy line: a braille spinner frame, then the status (`Thinking...`).
fn is_droid_busy_line(line: &str) -> bool {
    split_glyph(line).is_some_and(|(glyph, rest)| is_braille(glyph) && rest.contains("..."))
}

/// The autonomy footer: `Auto (Off)`, `Auto (Low) · …`, `Auto (Med) · …`, `Auto (High) · …`.
fn is_droid_autonomy_footer(line: &str) -> bool {
    ["Auto (Off)", "Auto (Low)", "Auto (Med)", "Auto (High)"]
        .iter()
        .any(|label| line.starts_with(label))
}

pub(super) const PROFILE: Profile = Profile {
    label: "droid",
    names: &["droid"],
    packages: &[],
    title_hint: |title| title.starts_with("⛬ "),
    blocked: &[
        marker(
            Region::Bottom(20),
            Test::Any(&[
                "alt/option+e to inspect approval details",
                "waiting for tool confirmation",
                "or type your own answer",
                // AskUser's hints, once its free answer holds text.
                "1–9 select",
                "1–9 toggle",
                "↑ back to options",
                "droid-shield approval required",
                "please login with your factory account",
            ]),
        ),
        marker(Region::Bottom(20), Test::All(&["yes, allow", "no, cancel"])),
        marker(
            Region::Bottom(20),
            Test::All(&["trust this folder?", "1. trust this folder"]),
        ),
        marker(
            Region::Bottom(20),
            Test::All(&["proceed with implementation", "no, keep iterating on spec"]),
        ),
    ],
    working: &[
        marker(
            Region::Bottom(12),
            Test::Any(&["(press esc to stop)", "(please wait)"]),
        ),
        marker(Region::Bottom(12), Test::Line(is_droid_busy_line)),
    ],
    idle: &[marker(
        Region::Bottom(8),
        Test::Line(is_droid_autonomy_footer),
    )],
    quiet: Lifecycle::Idle,
    output_means_working: false,
    approvals_on_screen: false,
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn busy_lines_and_the_autonomy_footer() {
        assert!(is_droid_busy_line("⠙ Thinking...  (Press ESC to stop)"));
        assert!(is_droid_busy_line("⣀ Executing..."));
        assert!(!is_droid_busy_line("⠙ done"));
        assert!(!is_droid_busy_line("● Execute (cargo test)..."));
        assert!(is_droid_autonomy_footer(
            "Auto (Off) · all actions require approval"
        ));
        assert!(is_droid_autonomy_footer(
            "Auto (High) · allow all commands     ? for help"
        ));
        assert!(!is_droid_autonomy_footer("Automatic (Off)"));
    }

    #[test]
    fn droid_is_named_by_its_binary_its_npm_shim_and_its_title() {
        let agent = |name: &str, cmdline: &[&str]| {
            let process = ProcessInfo {
                pid: 1,
                name: name.into(),
                cmdline: cmdline.iter().map(|word| word.to_string()).collect(),
                ..Default::default()
            };
            identify(Some(&process), "").map(|profile| profile.label)
        };
        assert_eq!(
            agent("droid", &["/home/me/.local/bin/droid"]),
            Some("droid")
        );
        assert_eq!(
            agent(
                "node",
                &[
                    "node",
                    "/usr/lib/node_modules/droid/bin/droid",
                    "--auto",
                    "high"
                ]
            ),
            Some("droid")
        );
        assert_eq!(agent("droid", &["droid", "exec", "fix it"]), Some("droid"));
        assert_eq!(agent("android", &["android"]), None);
        assert_eq!(
            identify(None, "⛬ Fix the login test").map(|profile| profile.label),
            Some("droid")
        );
        let ssh = ProcessInfo {
            pid: 1,
            name: "ssh".into(),
            cmdline: vec!["ssh".into(), "devbox".into()],
            ..Default::default()
        };
        assert_eq!(
            identify(Some(&ssh), "⛬ Droid").map(|profile| profile.label),
            Some("droid")
        );
        // Its hooks report approvals (`permission_prompt`).
        assert!(!approvals_on_screen("droid"));
    }
}
