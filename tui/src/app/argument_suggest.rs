//! What the New heartbeat form suggests as arguments are typed: the agent's
//! own (`session_core::commands`), or the values of the one before the cursor.

use std::ops::Range;

use session_core::commands::{AgentCommand, Argument};

/// A row of the suggestions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Suggestion {
    /// What taking it writes.
    pub text: &'static str,
    /// What its value is, when it takes one.
    pub value: Option<&'static str>,
    pub about: &'static str,
    /// It can be taken; otherwise it says what's typed there.
    pub takes: bool,
}

impl Suggestion {
    fn of(argument: &'static Argument) -> Self {
        Self {
            text: argument.flag,
            value: argument.value,
            about: argument.about,
            takes: true,
        }
    }
}

/// The suggestions at a place in the arguments.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Suggestions {
    /// The word they replace.
    pub word: Range<usize>,
    pub rows: Vec<Suggestion>,
    /// The row a half-typed word means.
    pub best: Option<usize>,
}

/// What `agent` takes at `cursor` in `line`: the arguments that contain the
/// word there (every one it isn't given yet on an empty word), or the values
/// of the argument before it.
pub fn suggest(agent: &'static AgentCommand, line: &str, cursor: usize) -> Suggestions {
    let start = line[..cursor].rfind(char::is_whitespace).map_or(0, |at| {
        at + line[at..].chars().next().map_or(1, char::len_utf8)
    });
    let end = line[cursor..]
        .find(char::is_whitespace)
        .map_or(line.len(), |at| cursor + at);
    let word = start..end;
    let typed = line[start..cursor].to_lowercase();
    let before = line[..start]
        .split_whitespace()
        .next_back()
        .and_then(|flag| agent.argument(flag))
        .filter(|argument| argument.value.is_some());
    let mut rows: Vec<Suggestion> = match before {
        // A value: one of those known, else what it is.
        Some(argument) if !typed.starts_with('-') => {
            let values: Vec<Suggestion> = argument
                .values
                .iter()
                .filter(|value| value.to_lowercase().contains(&typed))
                .map(|value| Suggestion {
                    text: value,
                    value: None,
                    about: "",
                    takes: true,
                })
                .collect();
            match values.is_empty() {
                true => vec![Suggestion {
                    takes: false,
                    ..Suggestion::of(argument)
                }],
                false => values,
            }
        }
        _ => {
            let needle = typed.trim_start_matches('-');
            let given = |flag: &str| {
                line[..start]
                    .split_whitespace()
                    .chain(line[end..].split_whitespace())
                    .any(|word| word.split('=').next() == Some(flag))
            };
            agent
                .arguments
                .iter()
                .filter(|a| a.flag.to_lowercase().contains(needle))
                .filter(|a| a.repeats || !given(a.flag))
                .map(Suggestion::of)
                .collect()
        }
    };
    // Those that start with what's typed first, in their own order.
    rows.sort_by_key(|row| {
        let text = row.text.to_lowercase();
        !(text.starts_with(&typed) || text.trim_start_matches('-').starts_with(&typed))
    });
    // Whole, and the start of no other: what comes after it.
    let mut starts = rows
        .iter()
        .filter(|row| row.takes && row.text.starts_with(&line[word.clone()]));
    if let (Some(row), None, true) = (starts.next(), starts.next(), cursor == end) {
        if row.text == &line[word.clone()] {
            let next = format!("{} {}", &line[..end], &line[end..]);
            return Suggestions {
                word: end..end,
                ..suggest(agent, &next, end + 1)
            };
        }
    }
    let exact = rows.iter().any(|row| row.text == &line[word.clone()]);
    let best = (!typed.is_empty() && !exact && cursor == end)
        .then(|| rows.iter().position(|row| row.takes))
        .flatten();
    Suggestions { word, rows, best }
}

#[cfg(test)]
mod tests {
    use super::*;
    use session_core::commands::agent;

    fn at(label: &str, line: &str) -> Suggestions {
        suggest(agent(label).unwrap(), line, line.len())
    }

    fn texts(found: &Suggestions) -> Vec<&str> {
        found.rows.iter().map(|row| row.text).collect()
    }

    #[test]
    fn an_empty_word_lists_what_isnt_given_yet() {
        let found = at("claude", "--dangerously-skip-permissions --model=opus ");
        assert!(found.rows.len() > 8 && found.best.is_none());
        assert!(!texts(&found).contains(&"--dangerously-skip-permissions"));
        assert!(!texts(&found).contains(&"--model"));
        // Given once, some may be given again.
        assert!(texts(&at("codex", "--add-dir ~/a --add-dir ~/b ")).contains(&"--add-dir"));
        assert!(!texts(&at("codex", "--search ")).contains(&"--search"));
    }

    #[test]
    fn a_half_typed_word_is_found_anywhere_in_an_argument_and_its_best_is_first() {
        let found = at("claude", "--verbose perm");
        assert_eq!(
            texts(&found),
            ["--permission-mode", "--dangerously-skip-permissions"]
        );
        assert_eq!((found.word.clone(), found.best), (10..14, Some(0)));
        assert_eq!(at("claude", "--verb").best, Some(0));
        // Whole: what comes after it, none of it meant yet.
        let next = at("claude", "--effort low --verbose");
        assert_eq!((next.word.clone(), next.best), (22..22, None));
        assert!(texts(&next).contains(&"--model") && !texts(&next).contains(&"--verbose"));
        assert_eq!(
            texts(&at("claude", "--model")),
            texts(&at("claude", "--model "))
        );
        assert_eq!(texts(&at("copilot", "--allow-all")).len(), 4);
        // Nothing like it: nothing to take.
        assert_eq!(at("claude", "--nothing-like-it"), {
            Suggestions {
                word: 0..17,
                ..Default::default()
            }
        });
    }

    #[test]
    fn a_value_is_one_of_those_known_or_says_what_it_is() {
        let found = at("claude", "--model ");
        assert_eq!(texts(&found), ["fable", "opus", "sonnet", "haiku"]);
        assert_eq!(found.best, None);
        assert_eq!(texts(&at("claude", "--model so")), ["sonnet"]);
        let free = at("claude", "--model claude-x");
        assert_eq!((texts(&free), free.best), (vec!["--model"], None));
        assert!(!free.rows[0].takes);
        // The next argument, where a value was due.
        assert_eq!(texts(&at("claude", "--model --verb")), ["--verbose"]);
    }

    #[test]
    fn the_word_under_the_cursor_is_the_one_replaced() {
        let line = "--eff --verbose";
        let found = suggest(agent("claude").unwrap(), line, 5);
        assert_eq!(found.word, 0..5);
        assert_eq!(texts(&found), ["--effort"]);
        // In the middle of a word, only shown.
        assert_eq!(suggest(agent("claude").unwrap(), line, 4).best, None);
        let wide = "é --v";
        assert_eq!(
            suggest(agent("claude").unwrap(), wide, wide.len()).word,
            3..6
        );
    }
}
