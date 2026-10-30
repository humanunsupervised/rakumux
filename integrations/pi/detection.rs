//! Pi's screen profile. Captured: 0.75.5 (inline, editor between rules, footer
//! with context usage `0.0%/0`; loader `⠇ Running... (escape/ctrl+c to
//! cancel)`, `⠋ Working... (escape to interrupt)`) and 1.4.0 (fullscreen, the
//! loader drawn inside the rule above the editor: `── ⠼ Working ───`; a `!`
//! command's loader still on a line of its own). Its startup help line
//! mentions "escape interrupt", so bare "interrupt" is not evidence. Pi has
//! no built-in approval prompts: an extension's dialog (`ctx.ui.select`,
//! `confirm`, `input`, `editor`) is what waits for the user, drawn between
//! the editor's rules with its key hints. Pi from 1.1 reports its state
//! itself (OSC 7501), and the Rakumux extension reports it too; the screen is
//! the fallback.
use super::*;

/// Pi's loader on a line of its own: braille frame, then a message with an
/// ellipsis (0.75, and a `!` command's in 1.x).
pub(super) fn is_pi_loader_line(line: &str) -> bool {
    split_glyph(line).is_some_and(|(glyph, rest)| {
        is_braille(glyph) && (rest.contains("...") || rest.contains('…'))
    })
}

/// Pi 1.x's loader, drawn inside the rule above the editor:
/// `── ⠼ Working ───…`, `── ⠋ Compacting context… (escape to cancel) ──`.
pub(super) fn is_pi_rule_loader(line: &str) -> bool {
    line.starts_with('─') && has_braille_spinner_word(line)
}

/// Pi's footer shows context usage as `<percent>%/<window>`.
pub(super) fn has_pi_context_usage(line: &str) -> bool {
    line.split_whitespace().any(|word| {
        word.split_once("%/").is_some_and(|(percent, _)| {
            percent.chars().any(|c| c.is_ascii_digit())
                && percent.chars().all(|c| c.is_ascii_digit() || c == '.')
        })
    })
}

pub(super) const PROFILE: Profile = Profile {
    label: "pi",
    names: &["pi"],
    packages: &[
        &["@earendil-works", "pi-coding-agent"],
        &["@mariozechner", "pi-coding-agent"],
    ],
    title_hint: |title| title == "π" || title.starts_with("π - "),
    // An extension's select or confirm (`↑↓ navigate  enter select
    // escape/ctrl+c cancel`), input or editor (`enter submit  escape
    // cancel`). Pi's own pickers say "Enter to select" or "enter save".
    blocked: &[marker(
        Region::PromptBox,
        Test::And(
            &Test::Any(&["enter select", "enter submit"]),
            &Test::All(&["cancel"]),
        ),
    )],
    working: &[
        marker(Region::Bottom(15), Test::Line(is_pi_rule_loader)),
        marker(Region::Bottom(15), Test::Line(is_pi_loader_line)),
        marker(
            Region::Bottom(15),
            Test::Any(&["working...", " to interrupt)"]),
        ),
    ],
    idle: &[marker(Region::Bottom(4), Test::Line(has_pi_context_usage))],
    quiet: Lifecycle::Unknown,
    output_means_working: false,
    approvals_on_screen: false,
};
