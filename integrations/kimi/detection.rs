//! Kimi Code CLI 2.x (`kimi`): its process, panels and activity line.
//!
//! The native binary sets its process title to `kimi-code`, cut to the room
//! its arguments had (`kimi` started bare stays `kimi`, `kimi -c` shows as
//! `kimi-co`) and wipes the arguments; its executable
//! (`~/.kimi-code/bin/kimi`) names it whatever the title says. An npm
//! install runs `node …/@moonshot-ai/kimi-code/dist/main.mjs`. The archived
//! Python `kimi-cli` (1.x) shows as `Kimi Code`, `kimi-legacy` or `kimi_cli`.
//!
//! Kimi reports what it does itself (OSC 7501), which outranks the screen;
//! the screen covers a Kimi started before its terminal answered.
//! Approval, question and trust panels replace the input box, drawn between
//! two `───` rules (the input box is `╭─…─╮`), so a reply quoting their
//! words above the input box is no prompt. While busy, a braille frame and
//! `Working…` (`Compacting…`), or a moon frame while a tool runs, sit above
//! the input box. The `context: N% (…)` line stays on screen in every state:
//! idle only once nothing else matched.
use super::*;

pub(super) const PROFILE: Profile = Profile {
    label: "kimi",
    names: &["kimi", "kimi-code", "kimi-legacy", "kimi-cli", "kimi_cli"],
    packages: &[&["@moonshot-ai", "kimi-code"], &["kimi_cli"]],
    // Kimi Code 2.x titles the terminal with the conversation's title;
    // only the legacy CLI names itself.
    title_hint: |title| title == "Kimi Code",
    blocked: &[marker(
        Region::PromptBox,
        Test::Any(&[
            // An approval (Run this command? Write this file? …), its
            // feedback and its preview; a question's review step.
            "↵ confirm",
            "type feedback · ↵ submit",
            "esc back to approval",
            // A question, one of several answers, its own answer.
            "↵ choose",
            "↵ toggle",
            "type answer",
            "trust this folder?",
        ]),
    )],
    working: &[marker(Region::Bottom(12), Test::Line(is_kimi_activity_line))],
    idle: &[marker(Region::Bottom(2), Test::Line(has_kimi_context_usage))],
    quiet: Lifecycle::Unknown,
    output_means_working: false,
    approvals_on_screen: false,
};

/// The moon frames Kimi shows while a tool runs.
const MOON: &[char] = &['🌑', '🌒', '🌓', '🌔', '🌕', '🌖', '🌗', '🌘'];

/// Kimi's activity line: `⠸ Working… · Tip: …`, `⠋ Compacting…`, or a moon
/// frame alone or before a tip (`🌕 · Tip: …`).
pub(super) fn is_kimi_activity_line(line: &str) -> bool {
    let mut chars = line.chars();
    let Some(glyph) = chars.next() else {
        return false;
    };
    let rest = chars.as_str().trim_start();
    if MOON.contains(&glyph) {
        return rest.is_empty() || rest.starts_with('·');
    }
    is_braille(glyph)
        && glyph != '\u{2800}'
        && (rest.starts_with("Working…") || rest.starts_with("Compacting…"))
}

/// Kimi's context line: `context: 1% (120/256k)`.
pub(super) fn has_kimi_context_usage(line: &str) -> bool {
    line.strip_prefix("context: ")
        .and_then(|rest| rest.split_once("% ("))
        .is_some_and(|(percent, _)| {
            !percent.is_empty() && percent.chars().all(|c| c.is_ascii_digit() || c == '.')
        })
}
