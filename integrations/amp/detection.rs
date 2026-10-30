//! Amp's screen fallback. Its state comes from the Rakumux plugin (Amp's own
//! thread state), which ranks above anything read here.
//!
//! The installer build runs as `amp` (`~/.amp/bin/amp`); the npm build's
//! bin is named `amp` too (`@ampcode/cli/bin/amp`, `@sourcegraph/amp` being
//! a wrapper), and its fallback runs `node …/@ampcode/cli/cli-wrapper.cjs`.
//! Plugins run as `amp …/plugin-runtime.ts` children in the same process
//! group, so the foreground stays `amp`.
//!
//! Signed out, it asks whether to log in before drawing anything. Approval
//! dialogs carry `Allow Once`, `Always Allow for Me`, `Always Allow
//! for Workspace`, `Reject` and `Reject with feedback`, under `Waiting for
//! approval` (Amp's own strings). Its TUI animates while idle, so recent
//! output says nothing; a braille spinner in the title is the only busy
//! sign without the plugin, and nothing marks idle, so a quiet screen is
//! idle.
use super::*;

pub(super) const PROFILE: Profile = Profile {
    label: "amp",
    names: &["amp"],
    packages: &[&["@ampcode", "cli"], &["@sourcegraph", "amp"]],
    title_hint: |_| false,
    blocked: &[
        marker(Region::Bottom(20), Test::Any(&["waiting for approval"])),
        // Signed out, before its TUI: `Would you like to log in to Amp? [(y)es, (n)o]:`.
        marker(
            Region::Bottom(3),
            Test::Any(&["would you like to log in to amp?"]),
        ),
        marker(
            Region::Bottom(20),
            Test::And(
                &Test::Any(&[
                    "allow once",
                    "always allow for me",
                    "always allow for workspace",
                ]),
                &Test::Any(&["reject"]),
            ),
        ),
    ],
    working: &[marker(Region::Title, Test::Line(has_braille_spinner_word))],
    idle: &[],
    quiet: Lifecycle::Idle,
    output_means_working: false,
    approvals_on_screen: false,
};

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn agent_of(name: &str, cmdline: &[&str], exe: Option<&str>) -> Option<&'static str> {
        let process = ProcessInfo {
            pid: 1,
            name: name.into(),
            cmdline: cmdline.iter().map(|word| word.to_string()).collect(),
            exe: exe.map(PathBuf::from),
        };
        agent_label(&process)
    }

    #[test]
    fn every_amp_install_is_amp() {
        // The installer's binary, run through its `~/.local/bin` link.
        assert_eq!(
            agent_of(
                "amp",
                &["/home/me/.local/bin/amp"],
                Some("/home/me/.amp/bin/amp")
            ),
            Some("amp")
        );
        // npm's bin, hard-linked from the platform package.
        assert_eq!(
            agent_of(
                "amp",
                &["amp", "-m", "high"],
                Some("/usr/lib/node_modules/@ampcode/cli/bin/amp")
            ),
            Some("amp")
        );
        assert_eq!(
            agent_of(
                "amp.exe",
                &["/opt/node_modules/@ampcode/cli/bin/amp.exe"],
                None
            ),
            Some("amp")
        );
        // npm without the platform package: the wrapper script under Node.
        for wrapper in [
            "/usr/lib/node_modules/@ampcode/cli/cli-wrapper.cjs",
            "/usr/lib/node_modules/@sourcegraph/amp/node_modules/@ampcode/cli/cli-wrapper.cjs",
        ] {
            assert_eq!(agent_of("node", &["node", wrapper], None), Some("amp"));
        }
        // `npx @ampcode/cli`.
        assert_eq!(
            agent_of("npx", &["npx", "-y", "@ampcode/cli@latest"], None),
            Some("amp")
        );
        // Other programs with amp in their path aren't.
        assert_eq!(agent_of("ampy", &["ampy", "ls"], None), None);
        assert_eq!(
            agent_of("node", &["node", "/srv/amp-site/server.js"], None),
            None
        );
    }

    fn classify_screen(screen: &str, title: &str, ms_since_output: Option<u64>) -> Lifecycle {
        classify(
            &PROFILE,
            &DetectInput {
                screen,
                foreground: None,
                alternate_screen: true,
                title,
                ms_since_output,
            },
        )
    }

    #[test]
    fn approvals_block_and_animation_alone_is_not_work() {
        let dialog = "Bash  rm -rf build\n\nAllow Once\nAlways Allow for Me\nReject with feedback";
        assert_eq!(classify_screen(dialog, "", None), Lifecycle::Blocked);
        assert_eq!(
            classify_screen("Waiting for approval", "", None),
            Lifecycle::Blocked
        );
        // A reply that mentions rejecting isn't a dialog.
        assert_eq!(
            classify_screen("I'd reject that change.", "", Some(50)),
            Lifecycle::Idle
        );
        // Its TUI animates: output a moment ago means nothing.
        assert_eq!(
            classify_screen("› ", "demo - amp", Some(10)),
            Lifecycle::Idle
        );
        assert_eq!(
            classify_screen("", "⠙ demo - amp", None),
            Lifecycle::Working
        );
        assert!(!approvals_on_screen("amp"), "the plugin reports approvals");
    }
}
