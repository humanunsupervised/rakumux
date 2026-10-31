//! Droid's questions (`AskUser`, checked against Droid 1.0.0), read from its
//! session file, and its dialog, worked from its screen read with its styles.
//!
//! The tool takes a plain-text questionnaire: `1. [question] … (multi)`, then
//! `[topic] …` and `[option] …` lines, one to four questions of two to four
//! options. The dialog, under `Ask User`, shows one question at a time (with
//! several, a bar of topics first: `○ Colour → ● Fruits → ○ Pet`, `●` once
//! answered), its options numbered (`1. Red`; a `(multi)` question's carry
//! `[ ]`/`[x]` boxes and end with `Submit`, or `Next` before the last
//! question), and its free answer: `Or type your own answer...` (`[ ] Or
//! type …` for a multi one), which shows what's typed in its place. Its
//! cursor is only drawn bold (the free answer's, a reversed text cursor), so
//! the screen is read with its styles, and the hint under it says when the
//! free answer has the keys (`Enter Submit answer • ⇧Enter newline • ↑ back
//! to options`; `Enter Add answer` for a multi one). Keys, as checked
//! against the real Droid:
//! - **One choice:** its number (`1`–`9`) picks it: the dialog moves to the
//!   next question, or, after the last, gives the answers.
//! - **Several choices:** its number ticks or unticks a box, the cursor
//!   staying where it is; `down` to `Submit` (or `Next`), `enter`.
//! - **A free answer:** `down` to its row (`up` back from `Submit`), the text
//!   typed there (which ticks it, for a multi question), then `enter` for one
//!   choice, or `down` to `Submit`/`Next` and `enter` for several. A number
//!   pressed there is typed, so numbers go only from an option's row.
//!
//! There's no review: the last question's answer gives them all. Every step
//! is planned from the screen just read; the driver stops when the screen
//! shows anything it doesn't expect: another question, an option it doesn't
//! know, a free answer someone else typed, or no cursor.

use super::{squash, styled_lines, Answer, Logged, Next, OpenQuestion, Question};
use serde_json::Value;

/// The tool Droid asks the user with.
const TOOL: &str = "AskUser";
/// The free answer's row, empty.
const PLACEHOLDER: &str = "ortypeyourownanswer";
/// The keys that pick (or tick) an option, by its number.
const DIGITS: [&str; 9] = ["1", "2", "3", "4", "5", "6", "7", "8", "9"];

/// What `entry` of Droid's session file says about questions: an `AskUser`
/// call, or a tool result. A call whose questionnaire Rakumux can't read is
/// still the newest question, with none to answer. (A Task subagent's
/// calls are in a session file of its own.)
pub(super) fn logged(entry: &Value) -> Vec<Logged> {
    let Some(message) = entry.get("message") else {
        return Vec::new();
    };
    let role = message.get("role").and_then(Value::as_str);
    let Some(blocks) = message.get("content").and_then(Value::as_array) else {
        return Vec::new();
    };
    blocks
        .iter()
        .filter_map(
            |block| match (role, block.get("type").and_then(Value::as_str)) {
                (Some("assistant"), Some("tool_use"))
                    if block.get("name").and_then(Value::as_str) == Some(TOOL) =>
                {
                    let call_id = block.get("id")?.as_str()?.to_owned();
                    let questions = block
                        .get("input")
                        .and_then(|input| input.get("questionnaire"))
                        .and_then(Value::as_str)
                        .and_then(questionnaire)
                        .unwrap_or_default();
                    Some(Logged::Asked(OpenQuestion { call_id, questions }))
                }
                (Some("user"), Some("tool_result")) => Some(Logged::Answered(
                    block.get("tool_use_id")?.as_str()?.to_owned(),
                )),
                _ => None,
            },
        )
        .collect()
}

/// A questionnaire's line: `[question] …`, `[topic] …` or `[option] …`,
/// after an optional `1.` or `1)`; its kind and words.
fn questionnaire_line(line: &str) -> Option<(&str, &str)> {
    let line = line.trim();
    let digits = line
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(line.len());
    let line = if digits > 0 {
        line[digits..]
            .strip_prefix(['.', ')'])
            .map(str::trim_start)
            .unwrap_or(line)
    } else {
        line
    };
    let rest = line.strip_prefix('[')?;
    let (kind, words) = rest.split_once(']')?;
    matches!(kind, "question" | "topic" | "option").then(|| (kind, words.trim()))
}

/// The questionnaire's questions, as rakumux.com reads them too; none when
/// one has no options, or more than the keys pick.
fn questionnaire(text: &str) -> Option<Vec<Question>> {
    let mut questions: Vec<Question> = Vec::new();
    for line in text.lines() {
        let Some((kind, words)) = questionnaire_line(line) else {
            continue;
        };
        if words.is_empty() {
            continue;
        }
        match (kind, questions.last_mut()) {
            ("question", _) => {
                let (question, multi_select) = match words.strip_suffix("(multi)") {
                    Some(question) => (question.trim_end(), true),
                    None => (words, false),
                };
                questions.push(Question {
                    question: question.to_owned(),
                    header: None,
                    options: Vec::new(),
                    multi_select,
                });
            }
            ("topic", Some(current)) => current.header = Some(words.to_owned()),
            ("option", Some(current)) => current.options.push(words.to_owned()),
            _ => {}
        }
    }
    let fits = |question: &Question| (1..=DIGITS.len()).contains(&question.options.len());
    (!questions.is_empty() && questions.iter().all(fits)).then_some(questions)
}

/// A numbered option on screen: its number, its box (a multi question's),
/// its first line's text and the lines its label wrapped onto (with one
/// choice, the free answer's row too, which carries no mark of its own).
#[derive(Clone, Debug, PartialEq, Eq)]
struct OptionRow {
    number: usize,
    checked: Option<bool>,
    lines: Vec<String>,
    /// The cursor is on it (drawn bold).
    cursor: bool,
}

/// A multi question's free answer: its box and what it shows.
#[derive(Clone, Debug, PartialEq, Eq)]
struct OwnRow {
    checked: bool,
    lines: Vec<String>,
}

/// What the screen shows of a question's page.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Page {
    /// The question's text, squashed.
    question: String,
    options: Vec<OptionRow>,
    /// A multi question's free answer.
    own: Option<OwnRow>,
    /// `Submit` or `Next` (squashed), under a multi question's options, and
    /// whether the cursor is on it.
    action: Option<(String, bool)>,
    /// The hint says the free answer has the keys.
    typing: bool,
}

/// Where the cursor is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Cursor {
    Option(usize),
    Own,
    Action,
}

impl Page {
    fn cursor(&self) -> Option<Cursor> {
        if self.typing {
            return Some(Cursor::Own);
        }
        if let Some(at) = self.options.iter().position(|row| row.cursor) {
            return Some(Cursor::Option(at));
        }
        self.action
            .as_ref()
            .filter(|(_, on)| *on)
            .map(|_| Cursor::Action)
    }
}

/// `N. label`, `N. [ ] label` or `N. [x] label`.
fn numbered(text: &str) -> Option<(usize, Option<bool>, &str)> {
    let digits = text.find(|c: char| !c.is_ascii_digit())?;
    let number: usize = text[..digits].parse().ok()?;
    let label = text[digits..].strip_prefix(". ")?;
    Some(match boxed(label) {
        Some((checked, label)) => (number, Some(checked), label),
        None => (number, None, label),
    })
}

/// `[ ] text` or `[x] text`.
fn boxed(text: &str) -> Option<(bool, &str)> {
    if let Some(rest) = text.strip_prefix("[ ] ") {
        Some((false, rest))
    } else {
        text.strip_prefix("[x] ").map(|rest| (true, rest))
    }
}

/// The bar of topics over several questions: `○ Colour → ● Fruits`.
fn is_topics(text: &str) -> bool {
    text.starts_with(['○', '●']) && text.contains(" → ")
}

impl Page {
    /// The dialog under the last `Ask User`: its question, its rows, and the
    /// hint at the bottom. None for anything else, the answered question
    /// left in the chat among them (it has no hint under it).
    fn read(screen: &str) -> Option<Self> {
        let lines = styled_lines(screen);
        let end = lines
            .iter()
            .rposition(|line| !line.text.trim().is_empty())?
            + 1;
        let title = lines[..end]
            .iter()
            .rposition(|line| line.text.trim().eq_ignore_ascii_case("ask user"))?;
        let mut paragraphs: Vec<Vec<&super::StyledLine>> = vec![Vec::new()];
        for line in &lines[title + 1..end] {
            if line.text.trim().is_empty() {
                if paragraphs.last().is_some_and(|p| !p.is_empty()) {
                    paragraphs.push(Vec::new());
                }
            } else if let Some(paragraph) = paragraphs.last_mut() {
                paragraph.push(line);
            }
        }
        let words = |paragraph: &[&super::StyledLine]| {
            paragraph
                .iter()
                .map(|line| line.text.trim())
                .collect::<Vec<_>>()
                .join(" ")
        };
        let hint = paragraphs.pop()?;
        let hint = words(&hint).to_lowercase();
        if !hint.contains("esc stop agent") {
            return None;
        }
        let typing = hint.contains("back to options");
        let mut paragraphs = paragraphs.into_iter();
        let mut question = paragraphs.next()?;
        if question.len() == 1 && is_topics(question[0].text.trim()) {
            question = paragraphs.next()?;
        }
        let question = squash(&words(&question));
        let rows = paragraphs.next()?;
        let mut options: Vec<OptionRow> = Vec::new();
        let mut own: Option<OwnRow> = None;
        for line in rows {
            let text = line.text.trim();
            if let (None, Some((number, checked, label))) = (&own, numbered(text)) {
                options.push(OptionRow {
                    number,
                    checked,
                    lines: vec![label.to_owned()],
                    cursor: line.starts_bold(),
                });
            } else if let (None, Some((checked, label)), false) =
                (&own, boxed(text), options.is_empty())
            {
                own = Some(OwnRow {
                    checked,
                    lines: vec![label.to_owned()],
                });
            } else if let Some(own) = own.as_mut() {
                own.lines.push(text.to_owned());
            } else if let Some(option) = options.last_mut() {
                option.lines.push(text.to_owned());
            } else {
                return None;
            }
        }
        let action = match paragraphs.next() {
            None => None,
            Some(lines) => {
                let [line] = lines.as_slice() else {
                    return None;
                };
                let label = squash(&line.text);
                if label != "submit" && label != "next" {
                    return None;
                }
                Some((label, line.starts_bold()))
            }
        };
        if paragraphs.next().is_some() || options.is_empty() {
            return None;
        }
        Some(Page {
            question,
            options,
            own,
            action,
            typing,
        })
    }
}

/// A question's free answer as its page shows it: its box (a multi
/// question's) and its words, squashed (the placeholder while empty).
#[derive(Debug, PartialEq, Eq)]
struct Own {
    checked: Option<bool>,
    words: String,
}

/// Answering one `AskUser` dialog, a step at a time.
#[derive(Debug)]
pub(super) struct Dialog {
    questions: Vec<Question>,
    answers: Vec<Answer>,
    /// The questions answered so far, in order.
    completed: usize,
    /// The last step left this question's page.
    leaving: Option<usize>,
    /// The last step gave the dialog its last answer.
    submitting: bool,
}

impl Dialog {
    pub(super) fn new(questions: Vec<Question>, answers: Vec<Answer>) -> Self {
        Self {
            questions,
            answers,
            completed: 0,
            leaving: None,
            submitting: false,
        }
    }

    /// The question a page shows, when its rows are that question's, and its
    /// free answer.
    fn which(&self, page: &Page) -> Option<(usize, Own)> {
        self.questions
            .iter()
            .enumerate()
            .find_map(|(index, asked)| {
                let options = &page.options;
                let fits = squash(&asked.question) == page.question
                    && options.len() == asked.options.len()
                    && options.iter().zip(1..).all(|(row, n)| row.number == n)
                    && options
                        .iter()
                        .all(|row| row.checked.is_some() == asked.multi_select)
                    && page.own.is_some() == asked.multi_select
                    && page.action.is_some() == asked.multi_select;
                if !fits {
                    return None;
                }
                let (last, before) = options.split_last()?;
                if !before
                    .iter()
                    .zip(&asked.options)
                    .all(|(row, label)| squash(&row.lines.join(" ")) == squash(label))
                {
                    return None;
                }
                let label = squash(asked.options.last()?);
                if let Some(own) = &page.own {
                    return (squash(&last.lines.join(" ")) == label).then(|| {
                        (
                            index,
                            Own {
                                checked: Some(own.checked),
                                words: squash(&own.lines.join(" ")),
                            },
                        )
                    });
                }
                // One choice: the last option's label, then the free answer's row.
                (1..=last.lines.len()).find_map(|split| {
                    (squash(&last.lines[..split].join(" ")) == label).then(|| {
                        (
                            index,
                            Own {
                                checked: None,
                                words: squash(&last.lines[split..].join(" ")),
                            },
                        )
                    })
                })
            })
    }

    pub(super) fn shown(&self, screen: &str) -> bool {
        Page::read(screen).is_some_and(|page| self.which(&page).is_some())
    }

    pub(super) fn moved(&self, before: &str, after: &str) -> bool {
        Page::read(before) != Page::read(after)
    }

    pub(super) fn next(&mut self, screen: &str) -> Next {
        let page = Page::read(screen);
        if std::mem::take(&mut self.submitting) {
            return match page {
                None => Next::Done,
                Some(_) => Next::Stop("the dialog stayed open after its last answer"),
            };
        }
        if let Some(left) = self.leaving.take() {
            if page
                .as_ref()
                .and_then(|page| self.which(page))
                .is_some_and(|(shown, _)| shown == left)
            {
                return Next::Stop("the dialog stayed on the question");
            }
            self.completed = left + 1;
        }
        let Some(page) = page else {
            return Next::Stop("no question dialog on screen");
        };
        let Some((shown, own)) = self.which(&page) else {
            return Next::Stop("another dialog is on screen");
        };
        if shown != self.completed {
            return Next::Stop("the dialog is on another question");
        }
        let Some(cursor) = page.cursor() else {
            return Next::Stop("no cursor in the dialog");
        };
        self.plan(shown, &page, &own, cursor)
    }

    /// Leave question `index`'s page with `keys`: the next question's, or,
    /// after the last, the answers given.
    fn leave(&mut self, index: usize, keys: Vec<&'static str>) -> Next {
        if index + 1 == self.questions.len() {
            self.submitting = true;
        } else {
            self.leaving = Some(index);
        }
        Next::Keys(keys)
    }

    /// The next step on question `index`'s page.
    fn plan(&mut self, index: usize, page: &Page, own: &Own, cursor: Cursor) -> Next {
        let asked = &self.questions[index];
        let answer = self.answers[index].clone();
        let text = answer.text.as_deref().map(squash);
        // The free answer: empty, showing the answer's text, or someone else's.
        let typed = match text.as_deref() {
            _ if own.words == PLACEHOLDER => Some(false),
            Some(text)
                if !own.words.is_empty()
                    && (text.starts_with(&own.words) || own.words.starts_with(text)) =>
            {
                Some(true)
            }
            _ => None,
        };
        if !asked.multi_select {
            if let Some(&choice) = answer.choices.first() {
                if typed != Some(false) {
                    return Next::Stop("the free answer holds other text");
                }
                // On the free answer's row a number would be typed.
                if !matches!(cursor, Cursor::Option(_)) {
                    return Next::Stop("the free answer has the keys");
                }
                return self.leave(index, vec![DIGITS[choice]]);
            }
            let Some(said) = answer.text else {
                return Next::Stop("no answer for the question");
            };
            return match (cursor, typed) {
                (Cursor::Option(_), _) => Next::Keys(vec!["down"]),
                (Cursor::Own, Some(false)) => Next::Type(said),
                (Cursor::Own, Some(true)) => self.leave(index, vec!["enter"]),
                (Cursor::Own, None) => Next::Stop("the free answer holds other text"),
                (Cursor::Action, _) => Next::Stop("no cursor in the dialog"),
            };
        }
        // Several: each box as answered, by its number from an option's row
        // (or the action's: on the free answer's the number would be typed).
        for (at, row) in page.options.iter().enumerate() {
            if row.checked != Some(answer.choices.contains(&at)) {
                return match cursor {
                    Cursor::Own => Next::Keys(vec!["up"]),
                    _ => Next::Keys(vec![DIGITS[at]]),
                };
            }
        }
        match (&answer.text, typed, own.checked) {
            (Some(said), Some(false), Some(false)) => {
                return match cursor {
                    Cursor::Own => Next::Type(said.clone()),
                    Cursor::Option(_) => Next::Keys(vec!["down"]),
                    Cursor::Action => Next::Keys(vec!["up"]),
                };
            }
            (Some(_), Some(true), Some(true)) | (None, Some(false), Some(false)) => {}
            _ => return Next::Stop("the free answer isn't as given"),
        }
        let due = if index + 1 == self.questions.len() {
            "submit"
        } else {
            "next"
        };
        if page.action.as_ref().map(|(label, _)| label.as_str()) != Some(due) {
            return Next::Stop("no Submit under the options");
        }
        match cursor {
            Cursor::Action => self.leave(index, vec!["enter"]),
            _ => Next::Keys(vec!["down"]),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Screens captured from Droid 1.0.0 with their styles, in
    /// `integrations/droid/fixtures/questions/`.
    fn screen(name: &str) -> String {
        let path = format!(
            "{}/../../../integrations/droid/fixtures/questions/{name}.ansi",
            env!("CARGO_MANIFEST_DIR")
        );
        std::fs::read_to_string(&path).unwrap_or_else(|error| panic!("{path}: {error}"))
    }

    fn question(text: &str, header: &str, options: &[&str], multi_select: bool) -> Question {
        Question {
            question: text.into(),
            header: Some(header.into()),
            options: options.iter().map(|o| o.to_string()).collect(),
            multi_select,
        }
    }

    fn colour() -> Question {
        question(
            "Which colour should the button be?",
            "Colour",
            &["Red", "Green", "Blue"],
            false,
        )
    }

    fn fruits() -> Question {
        question(
            "Which fruits do you like?",
            "Fruits",
            &["Apple", "Banana", "Cherry"],
            true,
        )
    }

    fn pet() -> Question {
        question("Which pet would you keep?", "Pet", &["Cat", "Dog"], false)
    }

    fn answer(choices: &[usize], text: Option<&str>) -> Answer {
        Answer {
            choices: choices.to_vec(),
            text: text.map(String::from),
        }
    }

    const QUESTIONNAIRE: &str = "1. [question] Which colour should the button be?\n[topic] Colour\n[option] Red\n[option] Green\n[option] Blue\n\n2. [question] Which fruits do you like? (multi)\n[topic] Fruits\n[option] Apple\n[option] Banana\n[option] Cherry\n\n3. [question] Which pet would you keep?\n[topic] Pet\n[option] Cat\n[option] Dog\n";

    #[test]
    fn the_log_says_which_question_is_asked_and_when_it_has_its_result() {
        let asked = json!({"type": "message", "id": "m1", "message": {"role": "assistant",
            "content": [{"type": "tool_use", "id": "call_1", "name": "AskUser",
                "input": {"questionnaire": QUESTIONNAIRE}}]}});
        assert_eq!(
            logged(&asked),
            vec![Logged::Asked(OpenQuestion {
                call_id: "call_1".into(),
                questions: vec![colour(), fruits(), pet()],
            })]
        );
        let result = json!({"type": "message", "message": {"role": "user", "content": [
            {"type": "tool_result", "tool_use_id": "call_1", "is_error": false,
             "content": "1. [question] Which colour should the button be?\n[answer] Green"}]}});
        assert_eq!(logged(&result), vec![Logged::Answered("call_1".into())]);
        // Droid's hook rows and other tools aren't questions.
        let hook = json!({"type": "message", "message": {"role": "user", "content": [],
            "hookEventName": "PreToolUse", "hookMatcher": "AskUser"}});
        assert!(logged(&hook).is_empty());
        let execute = json!({"type": "message", "message": {"role": "assistant", "content": [
            {"type": "tool_use", "id": "call_2", "name": "Execute", "input": {"command": "ls"}}]}});
        assert!(logged(&execute).is_empty());
        // A questionnaire Rakumux can't read is still the newest question, with none to answer.
        let odd = json!({"type": "message", "message": {"role": "assistant", "content": [
            {"type": "tool_use", "id": "call_3", "name": "AskUser",
             "input": {"questionnaire": "1. [question] Which?\n"}}]}});
        assert_eq!(
            logged(&odd),
            vec![Logged::Asked(OpenQuestion {
                call_id: "call_3".into(),
                questions: Vec::new(),
            })]
        );
    }

    #[test]
    fn a_page_is_read_with_its_rows_and_the_bold_cursor() {
        let page = Page::read(&screen("one")).expect("a question page");
        assert_eq!(page.question, "whichcolourshouldthebuttonbe");
        assert_eq!(page.options.len(), 3);
        assert_eq!(page.cursor(), Some(Cursor::Option(0)));
        let dialog = Dialog::new(vec![colour()], vec![answer(&[1], None)]);
        assert_eq!(
            dialog.which(&page),
            Some((
                0,
                Own {
                    checked: None,
                    words: PLACEHOLDER.into()
                }
            ))
        );
        // Other options, or another question, aren't this dialog.
        let mut renamed = colour();
        renamed.options[2] = "Teal".into();
        assert!(!Dialog::new(vec![renamed], vec![answer(&[0], None)]).shown(&screen("one")));
        assert!(!Dialog::new(vec![pet()], vec![answer(&[0], None)]).shown(&screen("one")));
        // Without its styles the cursor can't be seen.
        let plain: String = styled_lines(&screen("one"))
            .into_iter()
            .map(|line| line.text + "\n")
            .collect();
        assert_eq!(Page::read(&plain).and_then(|page| page.cursor()), None);
    }

    #[test]
    fn the_answered_question_and_the_prompt_are_no_dialog() {
        let dialog = Dialog::new(vec![colour()], vec![answer(&[1], None)]);
        for name in ["one-answered", "prompt", "approval"] {
            assert_eq!(Page::read(&screen(name)), None, "{name}");
            assert!(!dialog.shown(&screen(name)), "{name}");
        }
    }

    #[test]
    fn one_choice_is_picked_by_its_number() {
        let mut dialog = Dialog::new(vec![colour()], vec![answer(&[1], None)]);
        assert_eq!(dialog.next(&screen("one")), Next::Keys(vec!["2"]));
        assert_eq!(dialog.next(&screen("one-answered")), Next::Done);
    }

    #[test]
    fn a_free_answer_is_typed_on_its_row_and_entered() {
        let mut dialog = Dialog::new(vec![colour()], vec![answer(&[], Some("Purple, 2 shades"))]);
        assert_eq!(dialog.next(&screen("one")), Next::Keys(vec!["down"]));
        assert_eq!(
            dialog.next(&screen("one-own")),
            Next::Type("Purple, 2 shades".into())
        );
        assert_eq!(
            dialog.next(&screen("one-own-typed")),
            Next::Keys(vec!["enter"])
        );
        // Someone else's text there is left alone, and a number isn't typed into it.
        let mut other = Dialog::new(vec![colour()], vec![answer(&[], Some("Orange"))]);
        assert_eq!(
            other.next(&screen("one-own-typed")),
            Next::Stop("the free answer holds other text")
        );
        let mut pick = Dialog::new(vec![colour()], vec![answer(&[0], None)]);
        assert_eq!(
            pick.next(&screen("one-own")),
            Next::Stop("the free answer has the keys")
        );
    }

    #[test]
    fn several_choices_are_ticked_by_number_then_submitted() {
        let mut dialog = Dialog::new(vec![fruits()], vec![answer(&[0, 2], Some("Mango 2"))]);
        assert_eq!(dialog.next(&screen("multi")), Next::Keys(vec!["1"]));
        assert_eq!(dialog.next(&screen("multi-apple")), Next::Keys(vec!["3"]));
        // Both ticked, the cursor still on Apple: down to the free answer.
        assert_eq!(
            dialog.next(&screen("multi-ticked")),
            Next::Keys(vec!["down"])
        );
        assert_eq!(
            dialog.next(&screen("multi-own")),
            Next::Type("Mango 2".into())
        );
        assert_eq!(
            dialog.next(&screen("multi-own-typed")),
            Next::Keys(vec!["down"])
        );
        assert_eq!(
            dialog.next(&screen("multi-submit")),
            Next::Keys(vec!["enter"])
        );
        assert_eq!(dialog.next(&screen("multi-answered")), Next::Done);
    }

    #[test]
    fn three_questions_are_answered_in_turn() {
        let mut dialog = Dialog::new(
            vec![colour(), fruits(), pet()],
            vec![answer(&[1], None), answer(&[1], None), answer(&[0], None)],
        );
        assert!(dialog.shown(&screen("three-q1")));
        assert_eq!(dialog.next(&screen("three-q1")), Next::Keys(vec!["2"]));
        assert_eq!(dialog.next(&screen("three-q2")), Next::Keys(vec!["2"]));
        assert_eq!(
            dialog.next(&screen("three-q2-banana")),
            Next::Keys(vec!["down"])
        );
        assert_eq!(
            dialog.next(&screen("three-q2-next")),
            Next::Keys(vec!["enter"])
        );
        assert_eq!(dialog.next(&screen("three-q3")), Next::Keys(vec!["1"]));
        assert_eq!(dialog.next(&screen("three-answered")), Next::Done);
        // A page further on than the question due is never answered.
        let mut early = Dialog::new(
            vec![colour(), fruits(), pet()],
            vec![answer(&[1], None), answer(&[1], None), answer(&[0], None)],
        );
        assert_eq!(
            early.next(&screen("three-q3")),
            Next::Stop("the dialog is on another question")
        );
    }

    #[test]
    fn long_questions_and_labels_wrap_on_a_narrow_screen_and_are_read_whole() {
        let subagents = question(
            "Ship subagent activity? It needs migration 0024 on production, deploying \
             rakumux.com, and a Rakumux release for machines to start sending subagents.",
            "Subagents",
            &[
                "Ship web now, Rakumux in 0.1.35 (Recommended)",
                "Ship everything now, the long way round with every check",
                "Hold",
            ],
            true,
        );
        let mut dialog = Dialog::new(vec![subagents], vec![answer(&[1], None)]);
        assert!(dialog.shown(&screen("narrow-long")));
        assert_eq!(dialog.next(&screen("narrow-long")), Next::Keys(vec!["2"]));
    }

    #[test]
    fn a_step_is_seen_taken_only_when_the_dialog_moved() {
        let dialog = Dialog::new(vec![fruits()], vec![answer(&[0], None)]);
        // A box ticked, or the cursor moving (only its style changes), moved it.
        assert!(dialog.moved(&screen("multi"), &screen("multi-apple")));
        assert!(dialog.moved(&screen("multi-ticked"), &screen("multi-own")));
        assert!(!dialog.moved(&screen("multi"), &screen("multi")));
    }
}
