//! Question dialogs whose options are picked by their number (OpenCode,
//! Grok Build, Kimi Code), answered from what their screen shows.
//!
//! Each of these agents draws one question at a time: its text, its options
//! numbered (boxes for a question that takes several), and a free answer
//! after them (its own key, or the number after the options). A digit picks
//! a question's option and moves on, or ticks a box; the free answer's key
//! opens a box the answer is typed in, and Enter saves it. A page of boxes is
//! left with the agent's own key, and a review of the answers given (where
//! the agent has one) is submitted with another; Left goes back a question.
//! Numbers never depend on where the cursor is, so no step reads it.
//!
//! An agent's reader turns its screen into a [`View`]; this driver plans
//! every step from the view just read, one key or text at a time, and stops
//! at anything it doesn't expect: another question, an option it doesn't
//! know, a box someone else typed in, or a review that shows other answers.

use super::{squash, Answer, Next, Question};

/// Digits as key names, for options 1 to 9.
const DIGITS: [&str; 9] = ["1", "2", "3", "4", "5", "6", "7", "8", "9"];

/// What a reader makes out of a screen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum View {
    /// A question's page.
    Question(Page),
    /// The review of the answers given: each question (or its header), and
    /// the answer shown for it, squashed.
    Review(Vec<(String, String)>),
    /// No dialog the reader knows.
    Gone,
}

/// A question's page as shown.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Page {
    /// The question's text, squashed (with what the agent adds to it, such
    /// as OpenCode's `(select all that apply)`, left out by its reader).
    pub question: String,
    /// The options, in order.
    pub options: Vec<Row>,
    /// The free answer's row.
    pub free: Free,
    /// Whether the agent's key for leaving a page of boxes leaves it now
    /// (Grok Build's Enter does only while its hint says so).
    pub leaves: bool,
}

/// An option's row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Row {
    /// The label as its row's first line shows it, squashed (up to where
    /// the agent cut it).
    pub first: String,
    /// The label and what follows it (a wrap, its description), squashed.
    pub words: String,
    /// The agent cut the label short (`…`).
    pub cut: bool,
    pub mark: Mark,
}

impl Row {
    /// Whether this row's label is `label`: its first line begins it and it
    /// begins the row's words, or (cut short) what's shown begins it.
    pub fn is(&self, label: &str) -> bool {
        let label = squash(label);
        !self.first.is_empty()
            && label.starts_with(&self.first)
            && (self.cut || self.words.starts_with(&label))
    }
}

/// What a row shows of being chosen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Mark {
    /// Nothing (a question taking one choice, as OpenCode and Kimi Code draw it).
    None,
    /// A radio button (Grok Build's `(○)`/`(●)`).
    Radio(bool),
    /// A box (a question that takes several).
    Box(bool),
}

/// The free answer's row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Free {
    /// Nothing typed; `ticked` when its box (or radio) is on.
    Idle { ticked: bool },
    /// Its box is open for typing and holds this text (empty: nothing yet).
    Editing(String),
    /// This text was given.
    Given(String),
}

/// How an agent's dialog takes keys, and what it does after the last page.
#[derive(Debug)]
pub(super) struct Style {
    /// Reads the agent's screen.
    pub read: fn(&str) -> View,
    /// The free answer's key (Grok Build's `z`); none: the number after the
    /// options.
    pub free_key: Option<&'static str>,
    /// Leaves a page of boxes: `enter` (Grok Build) or `tab`.
    pub leave: &'static str,
    /// Saving the free answer on a page of boxes leaves it too (Grok Build);
    /// else the page stays and is left with [`Self::leave`].
    pub free_leaves: bool,
    /// Submits the review: `enter`, or its first choice.
    pub submit: &'static str,
    /// Goes back a question (`left`).
    pub back: &'static str,
    /// Whether the dialog reviews the answers after the last question; when
    /// it doesn't, leaving the last question submits them.
    pub reviews: fn(&[Question]) -> bool,
}

/// Answering one numbered dialog, a step at a time.
#[derive(Debug)]
pub(super) struct Dialog {
    style: &'static Style,
    questions: Vec<Question>,
    answers: Vec<Answer>,
    /// The questions answered so far, in order.
    completed: usize,
    /// The last step left this question's page.
    leaving: Option<usize>,
    /// The last step was the one that submits the answers.
    submitting: bool,
}

impl Dialog {
    pub(super) fn new(
        style: &'static Style,
        questions: Vec<Question>,
        answers: Vec<Answer>,
    ) -> Self {
        Self {
            style,
            questions,
            answers,
            completed: 0,
            leaving: None,
            submitting: false,
        }
    }

    /// The question a page shows, when its rows are that question's options,
    /// boxes for one that takes several.
    fn which(&self, page: &Page) -> Option<usize> {
        self.questions.iter().position(|asked| {
            squash(&asked.question) == page.question
                && asked.options.len() == page.options.len()
                && asked
                    .options
                    .iter()
                    .zip(&page.options)
                    .all(|(label, row)| row.is(label))
                && page
                    .options
                    .iter()
                    .all(|row| matches!(row.mark, Mark::Box(_)) == asked.multi_select)
        })
    }

    /// Whether the review's answers are the ones given, question by question:
    /// each shown under its question or its header, saying every choice and
    /// the free answer.
    fn reviewed(&self, shown: &[(String, String)]) -> bool {
        shown.len() == self.questions.len()
            && self.questions.iter().zip(&self.answers).zip(shown).all(
                |((asked, answer), (question, said))| {
                    let header = asked.header.as_deref().map(squash);
                    (*question == squash(&asked.question)
                        || header.is_some_and(|header| !header.is_empty() && *question == header))
                        && answer
                            .choices
                            .iter()
                            .map(|choice| squash(&asked.options[*choice]))
                            .chain(answer.text.iter().map(|text| squash(text)))
                            .all(|piece| said.contains(&piece))
                },
            )
    }

    pub(super) fn shown(&self, screen: &str) -> bool {
        match (self.style.read)(screen) {
            View::Question(page) => self.which(&page).is_some(),
            View::Review(answers) => self.reviewed(&answers),
            View::Gone => false,
        }
    }

    pub(super) fn moved(&self, before: &str, after: &str) -> bool {
        (self.style.read)(before) != (self.style.read)(after)
    }

    pub(super) fn next(&mut self, screen: &str) -> Next {
        let view = (self.style.read)(screen);
        if std::mem::take(&mut self.submitting) {
            return match view {
                View::Gone => Next::Done,
                _ => Next::Stop("the dialog stayed open after its answers were submitted"),
            };
        }
        if let Some(left) = self.leaving.take() {
            let stayed = match &view {
                View::Question(page) => self.which(page) == Some(left),
                _ => false,
            };
            if stayed {
                return Next::Stop("the dialog stayed on the question");
            }
            self.completed = left + 1;
        }
        match view {
            View::Gone => Next::Stop("no question dialog on screen"),
            View::Review(answers) => {
                if self.completed < self.questions.len() || !self.reviewed(&answers) {
                    return Next::Stop("the review shows other answers");
                }
                self.submitting = true;
                Next::Keys(vec![self.style.submit])
            }
            View::Question(page) => {
                let Some(shown) = self.which(&page) else {
                    return Next::Stop("another dialog is on screen");
                };
                if shown < self.completed {
                    return Next::Stop("the dialog went back to an answered question");
                }
                if shown > self.completed {
                    if matches!(page.free, Free::Editing(_)) {
                        return Next::Stop("someone is typing an answer");
                    }
                    return Next::Keys(vec![self.style.back]);
                }
                self.plan(shown, &page)
            }
        }
    }

    /// The step that leaves question `index`'s page with `key`: the answers
    /// are submitted when it's the last question and nothing reviews them.
    fn leave(&mut self, index: usize, key: &'static str) -> Next {
        if index + 1 == self.questions.len() && !(self.style.reviews)(&self.questions) {
            self.submitting = true;
        } else {
            self.leaving = Some(index);
        }
        Next::Keys(vec![key])
    }

    /// The free answer's key on question `index`'s page.
    fn free_key(&self, index: usize) -> Option<&'static str> {
        self.style
            .free_key
            .or_else(|| DIGITS.get(self.questions[index].options.len()).copied())
    }

    /// The next step on question `index`'s page.
    fn plan(&mut self, index: usize, page: &Page) -> Next {
        let asked = &self.questions[index];
        let answer = &self.answers[index];
        let text = answer.text.clone();
        let multi = asked.multi_select;
        // A box open for typing: only ever for this answer's free text, once
        // the boxes are as answered.
        if let Free::Editing(shown) = &page.free {
            let boxes = page.options.iter().enumerate().all(|(choice, row)| {
                !multi || row.mark == Mark::Box(answer.choices.contains(&choice))
            });
            let (Some(text), true) = (text, boxes) else {
                return Next::Stop("someone is typing an answer");
            };
            if shown.is_empty() {
                return Next::Type(text);
            }
            if squash(shown) != squash(&text) {
                return Next::Stop("the free answer holds other text");
            }
            // Saving it answers a question that takes one choice; on a page
            // of boxes it leaves only for an agent whose Enter does.
            if !multi || self.style.free_leaves {
                return self.leave(index, "enter");
            }
            return Next::Keys(vec!["enter"]);
        }
        let Some(free_key) = self.free_key(index) else {
            return Next::Stop("the free answer has no key");
        };
        if !multi {
            return match answer.choices.first() {
                Some(choice) => match DIGITS.get(*choice) {
                    Some(digit) => self.leave(index, digit),
                    None => Next::Stop("the option has no number key"),
                },
                None => match (&page.free, &text) {
                    (Free::Given(shown), Some(text)) if squash(shown) != squash(text) => {
                        Next::Stop("the free answer holds other text")
                    }
                    _ => Next::Keys(vec![free_key]),
                },
            };
        }
        // Several: each box as answered, then the free answer, then leave.
        for (choice, row) in page.options.iter().enumerate() {
            let wanted = answer.choices.contains(&choice);
            if row.mark != Mark::Box(wanted) {
                return match DIGITS.get(choice) {
                    Some(digit) => Next::Keys(vec![digit]),
                    None => Next::Stop("the option has no number key"),
                };
            }
        }
        match (&page.free, &text) {
            (Free::Idle { ticked: false }, Some(_)) => return Next::Keys(vec![free_key]),
            (Free::Given(shown), Some(text)) if squash(shown) == squash(text) => {
                if self.style.free_leaves {
                    // Its Enter would open the box again.
                    return Next::Stop("the free answer was given before");
                }
            }
            (Free::Idle { ticked: false }, None) => {}
            _ => return Next::Stop("the free answer isn't as given"),
        }
        if !page.leaves {
            return Next::Stop("the page can't be left now");
        }
        self.leave(index, self.style.leave)
    }
}
