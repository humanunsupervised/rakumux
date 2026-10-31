//! Claude Code screen profile (captured 2.1.296, a question's review page
//! 2.1.314; older inline prompts retained).
use super::*;

const ACTIVITY_GLYPHS: &[char] = &['·', '✢', '✳', '✶', '✻', '✽', '*'];

pub(super) fn is_claude_activity_line(line: &str) -> bool {
    let Some((glyph, rest)) = split_glyph(line) else {
        return false;
    };
    if !ACTIVITY_GLYPHS.contains(&glyph) {
        return false;
    }
    match rest.find('…') {
        Some(at) => {
            let after = rest[at + '…'.len_utf8()..].trim_start();
            after.is_empty() || after.starts_with('(')
        }
        None => false,
    }
}

/// `✻ Waiting for 2 background agents to finish` (captured 2.1.307): the turn
/// paused until background agents wake it. It has no ellipsis, so it isn't an
/// activity line, and the footer drops "esc to interrupt".
pub(super) fn is_claude_background_wait_line(line: &str) -> bool {
    split_glyph(line).is_some_and(|(glyph, rest)| {
        ACTIVITY_GLYPHS.contains(&glyph)
            && rest.starts_with("Waiting for ")
            && rest.contains(" background agent")
    })
}

fn starts_with_claude_title_spinner(title: &str) -> bool {
    split_glyph(title).is_some_and(|(glyph, _)| is_braille(glyph) || ('◐'..='◓').contains(&glyph))
}

pub(super) fn is_numbered_yes_option(line: &str) -> bool {
    let option = line.trim_start_matches(['❯', '›', '>', ' ']);
    let Some(rest) = option.strip_prefix(|character: char| character.is_ascii_digit()) else {
        return false;
    };
    let Some(label) = rest.strip_prefix(". ") else {
        return false;
    };
    label
        .split(|character: char| !character.is_alphabetic())
        .next()
        .unwrap_or("")
        .eq_ignore_ascii_case("yes")
}

pub(super) const PROFILE: Profile = Profile {
    label: "claude",
    names: &["claude", "claude-code"],
    packages: &[&["@anthropic-ai", "claude-code"]],
    title_hint: |title| title.starts_with("✳ ") || contains_ci(title, "claude code"),
    blocked: &[
        marker(
            Region::AfterLastRule,
            Test::And(
                &Test::All(&["esc to cancel"]),
                &Test::Any(&["enter to confirm", "enter to select", "enter to submit"]),
            ),
        ),
        marker(
            Region::Bottom(20),
            Test::And(
                &Test::Any(&["do you want to", "would you like to"]),
                &Test::Line(is_numbered_yes_option),
            ),
        ),
        marker(Region::Bottom(12), Test::Any(&["waiting for permission"])),
        // A question's review page (captured 2.1.314): its answers over
        // "1. Submit answers", with no footer.
        marker(
            Region::Bottom(6),
            Test::All(&["ready to submit your answers?", "submit answers"]),
        ),
    ],
    working: &[
        marker(Region::Title, Test::Line(starts_with_claude_title_spinner)),
        marker(Region::AfterLastRule, Test::Any(&["esc to interrupt"])),
        marker(Region::Bottom(12), Test::Line(is_claude_activity_line)),
        marker(
            Region::Bottom(12),
            Test::Line(is_claude_background_wait_line),
        ),
    ],
    idle: &[
        marker(Region::PromptBox, Test::Line(|line| line.starts_with('❯'))),
        marker(
            Region::Bottom(8),
            Test::Line(|line| line.starts_with("│ >") || line.starts_with("│ ❯")),
        ),
        marker(Region::Title, Test::Line(|title| title.starts_with("✳ "))),
    ],
    quiet: Lifecycle::Unknown,
    output_means_working: false,
    approvals_on_screen: false,
};
