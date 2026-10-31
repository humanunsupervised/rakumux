//! Questions an agent asks the user in a dialog of its own, and answering
//! one from elsewhere (`session_answer`, for the feed): never a prompt for
//! permission.
//!
//! The conversation the daemon follows says which question is open: the
//! newest question tool call without a result ([`OpenQuestion`]), read as
//! the log grows ([`logged`]). Answering works the agent's dialog key by key
//! from what its screen shows ([`Dialog`]): every step is planned from the
//! screen read just before, and an agent's driver stops as soon as the
//! screen shows anything but the dialog it expects.
//!
//! Provider implementations live beside their hooks in `integrations/<agent>`:
//! - **Claude Code**: `AskUserQuestion` (one to four questions, each with two
//!   to four options, several of them for a `multiSelect` one, and a free
//!   answer, "Type something."), see `claude-code/questions.rs`.
//!
//! Another agent is added with its log's question calls and results in
//! [`logged`] and a driver for its dialog in [`Dialog`].

use super::LogAgent;
use serde_json::Value;
use session_core::api::{QuestionAnswer, MAX_ANSWER_TEXT_CHARS};
use std::collections::VecDeque;

#[path = "claude-code/questions.rs"]
mod claude;

/// One question of an agent's dialog, as its conversation logged the call.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Question {
    pub question: String,
    /// Its short label (Claude Code's chip), if any.
    pub header: Option<String>,
    /// The options' labels, in order.
    pub options: Vec<String>,
    /// Several options may be chosen.
    pub multi_select: bool,
}

/// The newest question a conversation asked without a result yet.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpenQuestion {
    /// The tool call that asked it.
    pub call_id: String,
    pub questions: Vec<Question>,
}

/// What a log entry says about the conversation's questions.
#[derive(Debug, PartialEq, Eq)]
pub enum Logged {
    /// A question was asked.
    Asked(OpenQuestion),
    /// A tool call got its result: a question asked by it is no longer open.
    Answered(String),
}

/// What `entry` of `agent`'s log says about questions, in order.
pub fn logged(agent: LogAgent, entry: &Value) -> Vec<Logged> {
    match agent {
        LogAgent::Claude => claude::logged(entry),
        _ => Vec::new(),
    }
}

/// The most question calls whose results are remembered.
const MAX_ANSWERED: usize = 8;

/// What a conversation's log showed of its questions as it was read: the
/// newest one asked without a result yet, and the question calls whose
/// results were read lately (other tools' results aren't kept).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Asked {
    open: Option<OpenQuestion>,
    answered: VecDeque<String>,
}

impl Asked {
    /// Note what `entry` of `agent`'s log says.
    pub fn note(&mut self, agent: LogAgent, entry: &Value) {
        for logged in logged(agent, entry) {
            match logged {
                Logged::Asked(open) => self.open = Some(open),
                Logged::Answered(call_id)
                    if self
                        .open
                        .as_ref()
                        .is_some_and(|open| open.call_id == call_id) =>
                {
                    self.open = None;
                    if self.answered.len() == MAX_ANSWERED {
                        self.answered.pop_front();
                    }
                    self.answered.push_back(call_id);
                }
                Logged::Answered(_) => {}
            }
        }
    }

    /// The log followed is another conversation's now: no question of it is open.
    pub fn switched(&mut self) {
        self.open = None;
    }

    /// The newest question asked without a result yet.
    pub fn open(&self) -> Option<&OpenQuestion> {
        self.open.as_ref()
    }

    /// Whether the log showed the result of question call `call_id` lately.
    pub fn answered(&self, call_id: &str) -> bool {
        self.answered.iter().any(|answered| answered == call_id)
    }
}

/// The answer to one question, checked against it: the options chosen (by
/// index, increasing) and what's typed as the dialog's free answer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Answer {
    pub choices: Vec<usize>,
    pub text: Option<String>,
}

/// `given`, one answer per question of `open` in its order, each naming the
/// question it answers and the labels of the options it chooses (one, or
/// the free answer, for a question that takes one; any, with or without a
/// free answer but at least one of them, for one that takes several); why
/// not, when they don't fit. Texts compare by their letters and digits: the
/// feed sends what its chat upload shows, cleaned of controls.
pub fn check_answers(open: &OpenQuestion, given: &[QuestionAnswer]) -> Result<Vec<Answer>, String> {
    if given.len() != open.questions.len() {
        return Err(format!(
            "{} answers for {} questions",
            given.len(),
            open.questions.len()
        ));
    }
    open.questions
        .iter()
        .zip(given)
        .enumerate()
        .map(|(index, (question, answer))| {
            if !same_text(&question.question, &answer.question) {
                return Err(format!("answer {} is for another question", index + 1));
            }
            let mut choices = Vec::new();
            for choice in &answer.choices {
                let found = question
                    .options
                    .iter()
                    .position(|label| label == choice)
                    .or_else(|| {
                        question
                            .options
                            .iter()
                            .position(|label| same_text(label, choice))
                    })
                    .ok_or_else(|| format!("question {} has no such option", index + 1))?;
                if choices.contains(&found) {
                    return Err(format!("question {} names an option twice", index + 1));
                }
                choices.push(found);
            }
            choices.sort_unstable();
            let text = match answer.text.as_deref().map(str::trim) {
                None => None,
                Some(text) if is_answer_text(text) => Some(text.to_owned()),
                Some(_) => {
                    return Err(format!(
                        "question {}'s free answer is empty, too long or not one line",
                        index + 1
                    ))
                }
            };
            let given = choices.len() + usize::from(text.is_some());
            if given == 0 || (!question.multi_select && given > 1) {
                return Err(format!(
                    "question {} takes {}",
                    index + 1,
                    if question.multi_select {
                        "at least one answer"
                    } else {
                        "one answer"
                    }
                ));
            }
            Ok(Answer { choices, text })
        })
        .collect()
}

/// A free answer a dialog can take as typed: one line that says something,
/// within [`MAX_ANSWER_TEXT_CHARS`], with no control characters (a tab or
/// an Enter would work the dialog itself).
fn is_answer_text(text: &str) -> bool {
    !squash(text).is_empty()
        && text.chars().count() <= MAX_ANSWER_TEXT_CHARS
        && !text.chars().any(char::is_control)
}

/// Whether two texts say the same, by their letters and digits.
fn same_text(a: &str, b: &str) -> bool {
    let a = squash(a);
    !a.is_empty() && a == squash(b)
}

/// `text`'s letters and digits, lower case: what a text and the screen that
/// shows it have in common, however it was wrapped, marked or quoted.
pub(crate) fn squash(text: &str) -> String {
    text.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

/// The next step answering a dialog takes, from what its screen shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Next {
    /// Send these named keys, as one write.
    Keys(Vec<&'static str>),
    /// Type this text.
    Type(String),
    /// The dialog took the answers and closed: its result is due.
    Done,
    /// The screen shows something else than the step expects: stop, why.
    Stop(&'static str),
}

/// Answering one question dialog: its agent's driver, which plans each step
/// from the screen and stops at anything it doesn't expect.
#[derive(Debug)]
pub struct Dialog(Driver);

#[derive(Debug)]
enum Driver {
    Claude(claude::Dialog),
}

impl Dialog {
    /// A driver for `agent`'s dialog (`claude`, as detected or named by its
    /// hooks) asking `open`, to answer it with `answers`; none for an agent
    /// Rakumux doesn't answer for.
    pub fn new(agent: &str, open: &OpenQuestion, answers: Vec<Answer>) -> Option<Self> {
        match agent {
            "claude" | "claude-code" => Some(Self(Driver::Claude(claude::Dialog::new(
                open.questions.clone(),
                answers,
            )))),
            _ => None,
        }
    }

    /// Whether `screen` shows this dialog, on any of its pages.
    pub fn shown(&self, screen: &str) -> bool {
        match &self.0 {
            Driver::Claude(dialog) => dialog.shown(screen),
        }
    }

    /// The next step from what `screen` shows now.
    pub fn next(&mut self, screen: &str) -> Next {
        match &mut self.0 {
            Driver::Claude(dialog) => dialog.next(screen),
        }
    }

    /// Whether `after` shows the dialog moved on from `before`: the last
    /// step was taken (a spinner or a clock changing doesn't count).
    pub fn moved(&self, before: &str, after: &str) -> bool {
        match &self.0 {
            Driver::Claude(dialog) => dialog.moved(before, after),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn open() -> OpenQuestion {
        OpenQuestion {
            call_id: "toolu_1".into(),
            questions: vec![
                Question {
                    question: "Which colour do you prefer?".into(),
                    header: Some("Colour".into()),
                    options: vec!["Red".into(), "Green".into(), "Blue".into()],
                    multi_select: false,
                },
                Question {
                    question: "Which fruits do you like?".into(),
                    header: Some("Fruits".into()),
                    options: vec!["Apple".into(), "Banana".into(), "Cherry".into()],
                    multi_select: true,
                },
            ],
        }
    }

    fn answer(question: &str, choices: &[&str], text: Option<&str>) -> QuestionAnswer {
        QuestionAnswer {
            question: question.into(),
            choices: choices.iter().map(|c| c.to_string()).collect(),
            text: text.map(String::from),
        }
    }

    #[test]
    fn answers_fit_their_questions_in_order() {
        let checked = check_answers(
            &open(),
            &[
                answer("which colour do you prefer", &["Green"], None),
                answer(
                    "Which fruits do you like?",
                    &["Cherry", "apple"],
                    Some(" Mango "),
                ),
            ],
        )
        .unwrap();
        assert_eq!(
            checked,
            vec![
                Answer {
                    choices: vec![1],
                    text: None
                },
                Answer {
                    choices: vec![0, 2],
                    text: Some("Mango".into())
                },
            ]
        );
    }

    #[test]
    fn answers_that_dont_fit_are_refused() {
        let colour = |choices: &[&str], text| answer("Which colour do you prefer?", choices, text);
        let fruits = answer("Which fruits do you like?", &["Apple"], None);
        let refused = |answers: Vec<QuestionAnswer>| check_answers(&open(), &answers).is_err();
        // One question left out, or answered out of order.
        assert!(refused(vec![colour(&["Red"], None)]));
        assert!(refused(vec![fruits.clone(), colour(&["Red"], None)]));
        // A question that takes one answer, given two, or none.
        assert!(refused(vec![
            colour(&["Red", "Blue"], None),
            fruits.clone()
        ]));
        assert!(refused(vec![
            colour(&["Red"], Some("Purple")),
            fruits.clone()
        ]));
        assert!(refused(vec![colour(&[], None), fruits.clone()]));
        // No such option, an option twice, a free answer that isn't one line.
        assert!(refused(vec![colour(&["Purple"], None), fruits.clone()]));
        assert!(refused(vec![
            colour(&["Red"], None),
            answer("Which fruits do you like?", &["Apple", "Apple"], None)
        ]));
        assert!(refused(vec![
            colour(&[], Some("Purple\nplease")),
            fruits.clone()
        ]));
        assert!(refused(vec![colour(&[], Some("Pur\tple")), fruits.clone()]));
        assert!(refused(vec![colour(&[], Some(" -- ")), fruits.clone()]));
        let long = "a".repeat(MAX_ANSWER_TEXT_CHARS + 1);
        assert!(refused(vec![colour(&[], Some(&long)), fruits.clone()]));
        // A free answer alone fits.
        assert!(!refused(vec![colour(&[], Some("Purple")), fruits]));
    }
}
