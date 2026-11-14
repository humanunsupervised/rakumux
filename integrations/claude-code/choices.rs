//! Claude Code's prompts answered by picking a numbered option, read from its
//! screen (captured 2.1.325): its prompts for permission and its plan to
//! approve.
//!
//! A prompt for permission, under a rule across the screen: its heading
//! (`Bash command`, `Edit file`, `Create file`), a line about it, then
//! between dashed rules (`╌`) what it's about (the command, the file's
//! changes), its question (`Do you want to proceed?`), its options numbered
//! with `❯` at the cursor (a long one wrapped onto lines further in), and
//! `Esc to cancel · Tab to amend` under them. A plan to approve: `Ready to
//! code?` over the plan (scrolled in a box), a rule, `Claude has written up
//! a plan … Would you like to proceed?` and its options, the last of them
//! `Tell Claude what to change`, which opens a field to type in (not picked
//! here), then `ctrl+g to edit in Vim · <plan file>`.
//!
//! An option's number picks it at once, as checked against the real Claude
//! Code. Question dialogs (`AskUserQuestion`, `questions.rs`) aren't read
//! here: their options end with `Type something.` and `Chat about this`.

use super::{squash, Read};
use session_core::model::ChoiceOption;

/// The most lines under the options: the footer, and the status line under
/// it.
const MAX_BELOW: usize = 4;
/// Options that open a field to type in (squashed), never picked here.
const TYPED: &[&str] = &["tellclaudewhattochange"];
/// A hint under such an option, or the start of one (squashed).
const TYPED_HINT: &str = "shifttabtoapprove";
/// Options only a question dialog has (squashed).
const QUESTION_DIALOG: &[&str] = &["typesomething", "chataboutthis", "submitanswers"];

/// The prompt with numbered options Claude Code's `screen` shows, if any.
pub(super) fn read(screen: &str) -> Option<Read> {
    let lines: Vec<&str> = screen.lines().map(str::trim_end).collect();
    // The screen's width, as the rules across it show it: a line that runs
    // to its edge was broken mid-word.
    let width = lines
        .iter()
        .filter(|line| is_rule(line) || is_dashed(line))
        .map(|line| line.chars().count())
        .max()
        .unwrap_or(usize::MAX);
    // The options: the last numbered rows on screen, with the lines each
    // wraps onto.
    let last = lines.iter().rposition(|line| option(line).is_some())?;
    let (column, ..) = option(lines[last])?;
    let further_in = |line: &str| indent(line) > column && !line.trim().is_empty();
    let mut end = last + 1;
    while end < lines.len() && further_in(lines[end]) && option(lines[end]).is_none() {
        end += 1;
    }
    let below = lines[end..].iter().filter(|line| !line.trim().is_empty());
    if below.clone().count() > MAX_BELOW
        || below
            .clone()
            .any(|line| line.trim_start().starts_with('❯') || option(line).is_some())
    {
        return None;
    }
    let mut start = last;
    while start > 0 && {
        let line = lines[start - 1];
        option(line).is_some_and(|(at, ..)| at == column)
            || (further_in(line) && option(line).is_none())
    } {
        start -= 1;
    }
    let mut rows: Vec<(bool, u8, String, Vec<String>)> = Vec::new();
    let mut previous = "";
    for line in &lines[start..end] {
        match option(line) {
            Some((_, cursor, number, label)) => rows.push((cursor, number, label, Vec::new())),
            None => {
                let more = &mut rows.last_mut()?.3;
                if at_edge(previous, width) {
                    more.push(format!("\u{0}{}", line.trim()));
                } else {
                    more.push(line.trim().to_owned());
                }
            }
        }
        previous = line;
    }
    let numbered = rows
        .iter()
        .map(|(_, number, ..)| usize::from(*number))
        .eq(1..=rows.len());
    if !numbered || rows.len() < 2 || rows.iter().filter(|row| row.0).count() != 1 {
        return None;
    }
    let mut options = Vec::new();
    for (_, number, label, more) in rows {
        let squashed = squash(&label);
        if QUESTION_DIALOG.contains(&squashed.as_str()) {
            return None;
        }
        let typed = TYPED.contains(&squashed.as_str())
            || more.iter().any(|line| squash(line).starts_with(TYPED_HINT));
        if !typed {
            let label = joined(std::iter::once(label).chain(more));
            options.push(ChoiceOption { number, label });
        }
    }
    // The question, right above the options.
    let mut top = start;
    while top > 0 && lines[top - 1].trim().is_empty() {
        top -= 1;
    }
    let asked = top;
    while top > 0
        && !lines[top - 1].trim().is_empty()
        && !is_rule(lines[top - 1])
        && !is_dashed(lines[top - 1])
    {
        top -= 1;
    }
    let question = joined(lines[top..asked].iter().enumerate().map(|(at, line)| {
        let broken = at > 0 && at_edge(lines[top + at - 1], width);
        let line = line.trim();
        if broken {
            format!("\u{0}{line}")
        } else {
            line.to_owned()
        }
    }));
    if !question.ends_with('?') || squash(&question).contains("readytosubmityouranswers") {
        return None;
    }
    let (title, detail) = about(&lines[..top]);
    Some(Read {
        title,
        detail,
        question,
        options,
    })
}

/// The heading and what the prompt is about, from the lines above its
/// question: those under the rule over the prompt (or, when nothing is
/// between that rule and the question, as over a plan's question, under the
/// rule before it). What's between dashed rules is what it's about; without
/// them, whatever follows the heading but tips.
fn about(above: &[&str]) -> (Option<String>, Vec<String>) {
    let rules: Vec<usize> = above
        .iter()
        .enumerate()
        .filter(|(_, line)| is_rule(line))
        .map(|(at, _)| at)
        .collect();
    let shown = |from: usize, to: usize| {
        above[from + 1..to]
            .iter()
            .filter(|line| !line.trim().is_empty())
            .count()
    };
    let content = match rules.as_slice() {
        [.., before, last] if shown(*last, above.len()) == 0 => &above[before + 1..*last],
        [.., last] => &above[last + 1..],
        [] => return (None, Vec::new()),
    };
    let mut lines = content
        .iter()
        .filter(|line| !matches!(line.trim(), "↓" | "↑"))
        .skip_while(|line| line.trim().is_empty());
    let Some(title) = lines.next().map(|line| line.trim().to_owned()) else {
        return (None, Vec::new());
    };
    let rest: Vec<&str> = lines.copied().collect();
    let detail = match rest.iter().position(|line| is_dashed(line)) {
        Some(first) => rest[first + 1..]
            .iter()
            .take_while(|line| !is_dashed(line))
            .map(|line| line.to_string())
            .collect(),
        None => rest
            .iter()
            .filter(|line| !line.trim_start().starts_with("Tip:"))
            .map(|line| line.to_string())
            .collect(),
    };
    (Some(title), detail)
}

/// `line` runs to the screen's edge: the line after it goes on the same word.
fn at_edge(line: &str, width: usize) -> bool {
    line.chars().count() + 1 >= width
}

/// Lines as one: with a space between them, but none before a line that
/// goes on a word broken at the screen's edge (marked with a NUL).
fn joined(lines: impl Iterator<Item = String>) -> String {
    let mut text = String::new();
    for line in lines {
        match line.strip_prefix('\0') {
            Some(rest) => text.push_str(rest),
            None => {
                if !text.is_empty() {
                    text.push(' ');
                }
                text.push_str(&line);
            }
        }
    }
    text
}

/// `line` as an option's row: where its number starts, whether the cursor
/// is on it, its number and its label (`❯ 1. Yes`, `  2. No`).
fn option(line: &str) -> Option<(usize, bool, u8, String)> {
    let rest = line.trim_start();
    let (cursor, rest) = match rest.strip_prefix('❯') {
        Some(rest) => (true, rest.trim_start()),
        None => (false, rest),
    };
    let column = line.chars().count() - rest.chars().count();
    let mut chars = rest.chars();
    let number = chars.next()?.to_digit(10).filter(|n| *n > 0)?;
    let label = chars.as_str().strip_prefix(". ")?.trim();
    if label.is_empty() {
        return None;
    }
    Some((column, cursor, u8::try_from(number).ok()?, label.to_owned()))
}

fn indent(line: &str) -> usize {
    line.chars().take_while(|c| c.is_whitespace()).count()
}

/// A rule across the screen over a prompt (`─`).
fn is_rule(line: &str) -> bool {
    let line = line.trim();
    line.chars().count() >= 8 && line.chars().all(|c| c == '─')
}

/// A dashed rule around what a prompt is about (`╌`).
fn is_dashed(line: &str) -> bool {
    let line = line.trim();
    line.chars().count() >= 8 && line.chars().all(|c| c == '╌')
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Screens captured from Claude Code 2.1.325, in
    /// `integrations/claude-code/fixtures/choices/`.
    fn screen(name: &str) -> String {
        let path = format!(
            "{}/../../../integrations/claude-code/fixtures/{name}.txt",
            env!("CARGO_MANIFEST_DIR")
        );
        std::fs::read_to_string(&path).unwrap_or_else(|error| panic!("{path}: {error}"))
    }

    fn labels(read: &Read) -> Vec<(u8, &str)> {
        read.options
            .iter()
            .map(|option| (option.number, option.label.as_str()))
            .collect()
    }

    #[test]
    fn a_prompt_for_a_command_is_read_with_its_command_and_options() {
        let read = read(&screen("choices/bash")).expect("a prompt");
        assert_eq!(read.title.as_deref(), Some("Bash command"));
        assert_eq!(read.detail, vec![" touch made2.txt".to_owned()]);
        assert_eq!(read.question, "Do you want to proceed?");
        let options = labels(&read);
        assert_eq!(options.len(), 4);
        assert_eq!(options[0], (1, "Yes"));
        assert_eq!(
            options[1].1,
            "Yes, and always allow access to /tmp/claude-1000/-home-someuser-repos-rakumux/\
             00000000-0000-0000-0000-000000000000/scratchpad/cap/proj from this project"
        );
        assert!(
            options[1].1.ends_with("from this project"),
            "{}",
            options[1].1
        );
        assert_eq!(options[3], (4, "No"));
    }

    #[test]
    fn a_prompt_for_an_edit_is_read_with_its_changes() {
        let read = read(&screen("choices/edit")).expect("a prompt");
        assert_eq!(read.title.as_deref(), Some("Edit file"));
        assert_eq!(
            read.detail,
            [
                " 1  line one",
                " 2 -line two",
                " 2 +line 2",
                " 3  line three"
            ]
        );
        assert_eq!(read.question, "Do you want to make this edit to notes.txt?");
        assert_eq!(labels(&read)[2], (3, "No"));
        // A new file's, under the plan's own numbered steps in the chat.
        let read = super::read(&screen("choices/create")).expect("a prompt");
        assert_eq!(read.title.as_deref(), Some("Create file"));
        assert_eq!(read.question, "Do you want to create goodbye.py?");
        assert_eq!(read.options.len(), 3);
    }

    #[test]
    fn a_plan_to_approve_is_read_without_the_option_to_type() {
        let read = read(&screen("choices/plan")).expect("a prompt");
        assert_eq!(read.title.as_deref(), Some("Ready to code?"));
        assert_eq!(
            read.detail.first().map(|line| line.trim()),
            Some("Plan: add goodbye.py")
        );
        assert_eq!(
            read.question,
            "Claude has written up a plan and is ready to execute. Would you like to proceed?"
        );
        assert_eq!(
            labels(&read),
            [
                (1, "Yes, and use auto mode"),
                (2, "Yes, manually approve edits")
            ]
        );
    }

    #[test]
    fn the_prompt_box_a_numbered_list_and_question_dialogs_are_no_prompt() {
        for name in [
            "choices/idle-list",
            "questions/working",
            "questions/one-question",
            "questions/multi-q2",
            "questions/multi-review",
            "questions/tall-review",
        ] {
            assert_eq!(read(&screen(name)), None, "{name}");
        }
        // The prompt for permission captured with the question fixtures.
        let read = read(&screen("questions/permission")).expect("a prompt");
        assert_eq!(read.question, "Do you want to proceed?");
    }
}
