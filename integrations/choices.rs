//! Prompts an agent shows on its screen that are answered by picking one of
//! their numbered options (`TerminalView::choice`, `session_choose`): a
//! prompt for permission, a plan to approve. Unlike a question dialog
//! (`questions.rs`), nothing in the agent's conversation says what such a
//! prompt offers, so it's read from the screen alone, and answered only
//! while the screen still shows the very prompt the caller saw
//! ([`ChoicePrompt::key`]): one key, the option's number.
//!
//! Provider implementations live beside their hooks in `integrations/<agent>`:
//! - **Claude Code**: its prompts for permission (a heading, what it's
//!   about, `Do you want to proceed?` and `1. Yes` … `3. No`) and its plan
//!   to approve (`Ready to code?`, `Would you like to proceed?`), see
//!   `claude-code/choices.rs`.
//!
//! Another agent is added with a reader of its screen in [`read`], once its
//! prompts are checked against the real agent to take their number as the
//! pick.

use session_core::model::{ChoiceOption, ChoicePrompt, MAX_CHOICE_DETAIL_CHARS};

#[path = "claude-code/choices.rs"]
mod claude;

/// The prompt with numbered options `agent`'s screen shows, if any.
pub fn read(agent: &str, screen: &str) -> Option<ChoicePrompt> {
    let read = match agent {
        "claude" => claude::read(screen)?,
        _ => return None,
    };
    Some(read.prompt())
}

/// A prompt as an agent's reader found it on screen.
#[derive(Debug, PartialEq, Eq)]
struct Read {
    title: Option<String>,
    /// Its lines, as drawn.
    detail: Vec<String>,
    question: String,
    options: Vec<ChoiceOption>,
}

impl Read {
    fn prompt(self) -> ChoicePrompt {
        let detail = detail(&self.detail);
        let mut words = String::new();
        for part in [
            self.title.as_deref(),
            detail.as_deref(),
            Some(&self.question),
        ]
        .into_iter()
        .flatten()
        {
            words.push_str(&squash(part));
            words.push('\n');
        }
        for option in &self.options {
            words.push_str(&format!("{}:{}\n", option.number, squash(&option.label)));
        }
        ChoicePrompt {
            key: format!("{:016x}", fnv1a(words.as_bytes())),
            title: self.title,
            detail,
            question: self.question,
            options: self.options,
        }
    }
}

/// `lines` as one text: their common indent removed, runs of blank lines
/// made one, at most [`MAX_CHOICE_DETAIL_CHARS`] characters.
fn detail(lines: &[String]) -> Option<String> {
    let indent = lines
        .iter()
        .filter(|line| !line.trim().is_empty())
        .map(|line| line.len() - line.trim_start().len())
        .min()?;
    let mut text = String::new();
    let mut blank = false;
    for line in lines {
        if line.trim().is_empty() {
            blank = !text.is_empty();
            continue;
        }
        if blank {
            text.push('\n');
            blank = false;
        }
        if !text.is_empty() {
            text.push('\n');
        }
        text.push_str(line.get(indent..).unwrap_or(line.trim_start()).trim_end());
    }
    let text: String = text.chars().take(MAX_CHOICE_DETAIL_CHARS).collect();
    (!text.is_empty()).then_some(text)
}

/// `text`'s letters and digits, lowercased: what it says, however a narrow
/// screen wraps it.
fn squash(text: &str) -> String {
    text.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

/// FNV-1a, 64 bits: stable across builds and machines.
fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x0100_0000_01b3)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_detail_keeps_its_shape_without_its_indent() {
        let lines = ["  1  one", "", "", "  2 -two", "  2 +2"].map(String::from);
        assert_eq!(detail(&lines).as_deref(), Some("1  one\n\n2 -two\n2 +2"));
        assert_eq!(detail(&[]), None);
    }

    #[test]
    fn the_key_names_the_prompt_however_it_wraps() {
        let read = |question: &str| Read {
            title: Some("Bash command".into()),
            detail: vec!["touch a.txt".into()],
            question: question.into(),
            options: vec![
                ChoiceOption {
                    number: 1,
                    label: "Yes".into(),
                },
                ChoiceOption {
                    number: 2,
                    label: "No".into(),
                },
            ],
        };
        let one = read("Do you want to proceed?").prompt();
        assert_eq!(one.key, read("Do you want to  proceed?").prompt().key);
        assert_ne!(one.key, read("Do you want to go on?").prompt().key);
        let mut other = read("Do you want to proceed?");
        other.detail = vec!["touch b.txt".into()];
        assert_ne!(one.key, other.prompt().key);
        assert_eq!(one.key.len(), 16);
    }
}
