//! Cursor's agent CLI (`agent`, `cursor-agent`; 2026.10.22): its prompts,
//! busy line and input box, from the bundle's UI strings and its signed-out
//! screen. Its launcher runs a bundled Node under
//! `cursor-agent/versions/<v>/` with `exec -a agent`, which Linux shows as
//! `MainThread`: the file it runs says which agent (see `profile_for_process`).
use super::*;

/// What it asks before a tool runs, with the options under it, each with its
/// key: `Run (once) (y)`, `Skip (esc or n)`, `Delete (y)` / `Keep (n)`.
const QUESTIONS: Test = Test::Any(&[
    "run this command?",
    "run this command outside the sandbox?",
    "run this mcp tool?",
    "delete this file?",
    "write to this file?",
    "read this file?",
    "allow this web search?",
    "allow this web fetch?",
    "proceed with this edit?",
]);
const OPTION_KEYS: Test = Test::Any(&["(y)", "(esc or n)"]);

pub(super) const PROFILE: Profile = Profile {
    label: "cursor-agent",
    names: &["cursor-agent"],
    packages: &[&["cursor-agent", "versions"]],
    title_hint: |title| contains_ci(title, "cursor agent") || has_word(title, "cursor-agent"),
    blocked: &[
        // An approval: the question and its options.
        marker(Region::Bottom(16), Test::And(&QUESTIONS, &OPTION_KEYS)),
        // The input box while something else waits for the user: a decision,
        // a question, a mode switch, a plan to revise, what to do instead.
        marker(
            Region::Bottom(8),
            Test::Any(&[
                "waiting for decision (y/n/p)",
                "answer questions (enter to select/next, esc to skip)",
                "approve mode switch (y/n)",
                "describe how to revise the plan",
                "tell the agent what to do instead (enter to send",
                "edit the image prompt, then press enter to generate",
            ]),
        ),
        // A tool row waiting for its approval.
        marker(Region::Bottom(16), Test::Any(&["waiting for approval..."])),
        // Its first screens: workspace trust, and signed out.
        marker(
            Region::Screen,
            Test::Any(&[
                "workspace trust required",
                "do you trust the contents of this directory",
                "press any key to log in",
            ]),
        ),
    ],
    working: &[
        // The input box's right-hand hint while a turn runs.
        marker(Region::Bottom(6), Test::Any(&["ctrl+c to stop"])),
        marker(Region::Bottom(10), Test::Line(is_cursor_activity_line)),
    ],
    idle: &[marker(
        Region::Bottom(8),
        Test::Any(&[
            "plan, search, build anything",
            "add a follow-up",
            "run a command — e.g.",
        ]),
    )],
    quiet: Lifecycle::Idle,
    output_means_working: false,
    // Its hooks have no approval event: `preToolUse` fires before the prompt
    // it may draw, and nothing after.
    approvals_on_screen: true,
};

/// Cursor's busy line: its spinner (two braille cells, `⠰⠳`; older releases
/// one, or `⬡`), then what it does: `⠰⠳ Generating…`, `⠘⠆ Reading files`.
pub(super) fn is_cursor_activity_line(line: &str) -> bool {
    let mut words = line.split_whitespace();
    let (Some(spinner), Some(word)) = (words.next(), words.next()) else {
        return false;
    };
    let mut glyphs = spinner.chars();
    let glyph = |c: char| c == '⬡' || c == '⬢' || is_braille(c);
    let spinner =
        glyphs.next().is_some_and(glyph) && glyphs.clone().count() <= 1 && glyphs.all(glyph);
    let word = word
        .split(|c: char| !c.is_alphabetic())
        .next()
        .unwrap_or("");
    spinner && word.len() > 4 && word.ends_with("ing")
}
