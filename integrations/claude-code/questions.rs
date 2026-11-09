//! Claude Code's questions (`AskUserQuestion`, captured 2.1.314), read from
//! its conversation log, and its dialog, worked from its screen.
//!
//! The dialog shows one question at a time under a tab bar (`☐ Colour`; with
//! several questions `←  ☐ Colour  ☐ Fruits  ✔ Submit  →`), the question's
//! text, its options numbered (`❯` marks the cursor; a `multiSelect`
//! question's carry `[ ]`/`[✔]` boxes), then `Type something.`, the free
//! answer, which takes what's typed while the cursor is on it. Below a rule
//! comes `Chat about this`, never chosen here. Keys, as checked against the
//! real Claude Code:
//! - **One choice:** `down`/`up` to the option (or to `Type something.`, then
//!   the text), `enter`. With one question that answers the dialog; with
//!   several it moves to the next question.
//! - **Several choices:** `down`/`up` to each option to change, `space`
//!   ticks or unticks it; the free answer is typed on its row (which ticks
//!   it); `Next` (`Submit` on the last question), under the options, and
//!   `enter` move on.
//! - **Review:** after the last question (unless the dialog is one question
//!   taking one choice) a page lists each question with `→` its answer, then
//!   `Ready to submit your answers?` over `1. Submit answers` and
//!   `2. Cancel`: `enter` on the first submits.
//!
//! A dialog taller than the terminal (long questions, a phone's screen) has
//! its top cut off: the rule, the tab bar and the start of the question, or
//! on the review page its title and first answers. Such a page is read from
//! what's left, the options all there or none of it. Captured on 2.1.319.
//!
//! Every step is planned from the screen just read, and moves the dialog one
//! row, box, text or page on; the driver stops when the screen shows anything
//! it doesn't expect: another question, an option it doesn't know, a free
//! answer someone else typed, or answers on the review page that aren't the
//! ones given.

use super::{squash, Answer, Logged, Next, OpenQuestion, Question};
use serde_json::Value;

/// The tool Claude Code asks the user with.
const TOOL: &str = "AskUserQuestion";
/// The free answer's row, empty (`Type something.`; without the full stop
/// for a question that takes several).
const PLACEHOLDER: &str = "typesomething";
/// The row under the rule, never chosen here.
const CHAT: &str = "chataboutthis";

/// What `entry` of Claude Code's log says about questions: an
/// `AskUserQuestion` call of the main conversation (a subagent's are its
/// own), or a tool result. A call whose input isn't the tool's shape is
/// still the newest question, with none Rakumux can answer.
pub(super) fn logged(entry: &Value) -> Vec<Logged> {
    if entry.get("isSidechain").and_then(Value::as_bool) == Some(true) {
        return Vec::new();
    }
    let kind = entry.get("type").and_then(Value::as_str);
    let Some(blocks) = entry
        .get("message")
        .and_then(|message| message.get("content"))
        .and_then(Value::as_array)
    else {
        return Vec::new();
    };
    blocks
        .iter()
        .filter_map(
            |block| match (kind, block.get("type").and_then(Value::as_str)) {
                (Some("assistant"), Some("tool_use"))
                    if block.get("name").and_then(Value::as_str) == Some(TOOL) =>
                {
                    let call_id = block.get("id")?.as_str()?.to_owned();
                    let questions = block
                        .get("input")
                        .and_then(|input| input.get("questions"))
                        .and_then(questions)
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

/// The tool's `questions`, each a `question` with `options` of `label`s;
/// none when one isn't.
fn questions(value: &Value) -> Option<Vec<Question>> {
    let text = |value: Option<&Value>| {
        value
            .and_then(Value::as_str)
            .filter(|text| !squash(text).is_empty())
            .map(str::to_owned)
    };
    let questions: Option<Vec<Question>> = value
        .as_array()?
        .iter()
        .map(|item| {
            let options: Option<Vec<String>> = item
                .get("options")?
                .as_array()?
                .iter()
                .map(|option| text(option.get("label")))
                .collect();
            Some(Question {
                question: text(item.get("question"))?,
                header: text(item.get("header")),
                options: options.filter(|options| !options.is_empty())?,
                multi_select: item.get("multiSelect").and_then(Value::as_bool) == Some(true),
            })
        })
        .collect();
    questions.filter(|questions| !questions.is_empty())
}

/// A row of a question's page.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Row {
    /// `N. label`: an option, the free answer, or (under the rule) `Chat
    /// about this`; `checked` for a question that takes several. `first` is
    /// its first line's label, `rest` its other lines (a label's wrap, the
    /// description).
    Numbered {
        number: usize,
        checked: Option<bool>,
        first: String,
        rest: String,
    },
    /// `Next`, or `Submit` on the last question (a question that takes several).
    Action,
}

impl Row {
    /// Its words, squashed: its label, wrapped, then its description.
    fn words(&self) -> String {
        match self {
            Row::Numbered { first, rest, .. } => squash(&format!("{first} {rest}")),
            Row::Action => String::new(),
        }
    }

    fn checked(&self) -> Option<bool> {
        match self {
            Row::Numbered { checked, .. } => *checked,
            Row::Action => None,
        }
    }

    /// Whether this row's label is `label`: its first line begins it, and it
    /// begins the row's words (wrapped labels keep their start).
    fn is(&self, label: &str) -> bool {
        let Row::Numbered { first, .. } = self else {
            return false;
        };
        let (first, label) = (squash(first), squash(label));
        !first.is_empty() && label.starts_with(&first) && self.words().starts_with(&label)
    }
}

/// What the screen shows of the dialog.
#[derive(Clone, Debug, PartialEq, Eq)]
enum View {
    /// A question's page: its text (squashed), its rows down to `Chat about
    /// this`, and which row the cursor is on. `cut`: the page's top is above
    /// the screen, so `question` is only the end of its text, maybe nothing.
    Question {
        question: String,
        rows: Vec<Row>,
        cursor: Option<usize>,
        cut: bool,
    },
    /// The review page: each question (squashed) with its answer (squashed),
    /// and whether the cursor is on `Submit answers` (else `Cancel`). `cut`:
    /// the page's top is above the screen, so these are its last answers.
    Review {
        answers: Vec<(String, String)>,
        on_submit: bool,
        cut: bool,
    },
    /// No dialog Rakumux reads.
    Gone,
}

/// A rule across the screen.
fn is_rule(line: &str) -> bool {
    let line = line.trim();
    line.chars().count() >= 8 && line.chars().all(|c| c == '─')
}

/// A line's cursor mark and the rest: `❯ 1. Red` or `  2. Green`.
fn marked(line: &str) -> (bool, &str) {
    match line.strip_prefix('❯') {
        Some(rest) => (true, rest),
        None => (false, line.strip_prefix(' ').unwrap_or("")),
    }
}

/// `line` as a numbered row's start: `N. label` right after the cursor's
/// column (a description, further in, never is).
fn numbered(line: &str) -> Option<(bool, usize, Option<bool>, String)> {
    let (cursor, rest) = marked(line);
    let rest = rest.strip_prefix(' ')?;
    let digits = rest.find(|c: char| !c.is_ascii_digit())?;
    let number: usize = rest[..digits].parse().ok()?;
    let label = rest[digits..].strip_prefix(". ")?;
    let (checked, label) = if let Some(label) = label.strip_prefix("[ ] ") {
        (Some(false), label)
    } else if let Some(label) = label.strip_prefix("[✔] ") {
        (Some(true), label)
    } else {
        (None, label)
    };
    Some((cursor, number, checked, label.trim().to_owned()))
}

/// `line` as `Next` or `Submit`, under a question's options.
fn action(line: &str) -> Option<bool> {
    let (cursor, rest) = marked(line);
    (rest.starts_with("   ") && matches!(rest.trim(), "Next" | "Submit")).then_some(cursor)
}

impl View {
    fn read(screen: &str) -> Self {
        let lines: Vec<&str> = screen.lines().map(str::trim_end).collect();
        Self::question(&lines)
            .or_else(|| Self::review(&lines))
            .unwrap_or(View::Gone)
    }

    /// A question's page: up from `Chat about this`, the rule over it, the
    /// rule over the dialog, its tab bar, the question and its rows; with
    /// the rule over the dialog above the screen, what's left of them.
    fn question(lines: &[&str]) -> Option<Self> {
        let chat = lines.iter().rposition(|line| {
            numbered(line).is_some_and(|(_, _, _, label)| squash(&label) == CHAT)
        })?;
        let under = lines[..chat].iter().rposition(|line| is_rule(line))?;
        let top = lines[..under].iter().rposition(|line| is_rule(line));
        let cut = top.is_none();
        let shown = &lines[top.map_or(0, |top| top + 1)..under];
        let first = shown.iter().position(|line| !line.trim().is_empty())?;
        let tabbed = shown[first].contains(['☐', '☒']);
        if !tabbed && !cut {
            return None;
        }
        // A tab bar too long for a narrow screen wraps: the question starts
        // after the blank line under it. Cut, it may be gone already.
        let body = shown[first..]
            .iter()
            .skip(usize::from(tabbed))
            .skip_while(|line| tabbed && !line.trim().is_empty());
        let mut question = String::new();
        let mut rows: Vec<Row> = Vec::new();
        let mut cursor = None;
        for line in body.chain([&lines[chat]]) {
            if let Some((on, number, checked, label)) = numbered(line) {
                if on {
                    cursor = Some(rows.len());
                }
                rows.push(Row::Numbered {
                    number,
                    checked,
                    first: label,
                    rest: String::new(),
                });
            } else if let Some(on) = action(line).filter(|_| !rows.is_empty()) {
                if on {
                    cursor = Some(rows.len());
                }
                rows.push(Row::Action);
            } else if let Some(Row::Numbered { rest, .. }) = rows.last_mut() {
                rest.push(' ');
                rest.push_str(line.trim());
            } else if rows.is_empty() {
                question.push(' ');
                question.push_str(line);
            } else {
                // Text after `Next`: not a page read here.
                return None;
            }
        }
        // Numbered 1, 2, … down to `Chat about this`.
        let numbers = rows.iter().filter_map(|row| match row {
            Row::Numbered { number, .. } => Some(*number),
            Row::Action => None,
        });
        if !numbers.zip(1..).all(|(number, due)| number == due) || rows.len() < 3 {
            return None;
        }
        Some(View::Question {
            question: squash(&question),
            rows,
            cursor,
            cut,
        })
    }

    /// The review page: `Review your answers`, each `● question` and its `→
    /// answer`, then `Ready to submit your answers?` over `1. Submit answers`
    /// and `2. Cancel`.
    fn review(lines: &[&str]) -> Option<Self> {
        let ready = lines
            .iter()
            .rposition(|line| line.trim() == "Ready to submit your answers?")?;
        let title = lines[..ready]
            .iter()
            .rposition(|line| line.trim() == "Review your answers");
        let options: Vec<(bool, usize, String)> = lines[ready + 1..]
            .iter()
            .filter(|line| !line.trim().is_empty())
            .map(|line| numbered(line).map(|(on, number, _, label)| (on, number, label)))
            .collect::<Option<_>>()?;
        let [(submit, 1, first), (cancel, 2, second)] = options.as_slice() else {
            return None;
        };
        if squash(first) != "submitanswers" || squash(second) != "cancel" || submit == cancel {
            return None;
        }
        let mut answers: Vec<(String, String)> = Vec::new();
        // Whether the lines read go to the last question's answer yet.
        let mut answering = false;
        for line in &lines[title.map_or(0, |title| title + 1)..ready] {
            let text = line.trim_start().trim_start_matches('│').trim();
            if let Some(question) = text.strip_prefix('●') {
                answers.push((squash(question), String::new()));
                answering = false;
                continue;
            }
            let Some((question, answer)) = answers.last_mut() else {
                continue;
            };
            if let Some(said) = text.strip_prefix('→') {
                answering = true;
                answer.push_str(&squash(said));
            } else if answering {
                answer.push_str(&squash(text));
            } else {
                question.push_str(&squash(text));
            }
        }
        Some(View::Review {
            answers,
            on_submit: *submit,
            cut: title.is_none(),
        })
    }
}

/// Answering one `AskUserQuestion` dialog, a step at a time.
#[derive(Debug)]
pub(super) struct Dialog {
    questions: Vec<Question>,
    answers: Vec<Answer>,
    /// The questions answered so far, in order.
    completed: usize,
    /// The last step left this question's page (its `enter`).
    leaving: Option<usize>,
    /// The last step was the `enter` that answers the dialog.
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

    /// The question a page shows, when its rows are that question's. A page
    /// cut at the top shows only the end of its text, so two questions may
    /// fit it: then it's neither, never the wrong one.
    fn which(&self, question: &str, rows: &[Row], cut: bool) -> Option<usize> {
        let mut fits = (0..self.questions.len()).filter(|index| {
            let asked = &self.questions[*index];
            let options = asked.options.len();
            let numbered = rows
                .iter()
                .filter(|row| matches!(row, Row::Numbered { .. }))
                .count();
            // Its options, the free answer, `Chat about this`; with boxes
            // and `Next` for a question that takes several.
            let text = squash(&asked.question);
            (text == question || (cut && text.ends_with(question)))
                && numbered == options + 2
                && rows.len() == numbered + usize::from(asked.multi_select)
                && asked
                    .options
                    .iter()
                    .zip(rows)
                    .all(|(label, row)| row.is(label))
                && rows[..options + 1]
                    .iter()
                    .all(|row| row.checked().is_some() == asked.multi_select)
        });
        let first = fits.next()?;
        (!cut || fits.next().is_none()).then_some(first)
    }

    /// Whether the review page's answers are the ones given, question by
    /// question; `cut`, the last ones, which is what's left to read of them.
    fn reviewed(&self, shown: &[(String, String)], cut: bool) -> bool {
        let skipped = self.questions.len().checked_sub(shown.len());
        let Some(skipped) = skipped.filter(|skipped| *skipped == 0 || (cut && !shown.is_empty()))
        else {
            return false;
        };
        self.questions[skipped..]
            .iter()
            .zip(&self.answers[skipped..])
            .zip(shown)
            .all(|((asked, answer), (question, said))| {
                *question == squash(&asked.question)
                    && answer
                        .choices
                        .iter()
                        .map(|choice| squash(&asked.options[*choice]))
                        .chain(answer.text.iter().map(|text| squash(text)))
                        .all(|piece| said.contains(&piece))
            })
    }

    pub(super) fn shown(&self, screen: &str) -> bool {
        match View::read(screen) {
            View::Question {
                question,
                rows,
                cut,
                ..
            } => self.which(&question, &rows, cut).is_some(),
            View::Review { answers, cut, .. } => self.reviewed(&answers, cut),
            View::Gone => false,
        }
    }

    pub(super) fn moved(&self, before: &str, after: &str) -> bool {
        View::read(before) != View::read(after)
    }

    pub(super) fn next(&mut self, screen: &str) -> Next {
        let view = View::read(screen);
        if std::mem::take(&mut self.submitting) {
            return match view {
                View::Gone => Next::Done,
                _ => Next::Stop("the dialog stayed open after its answers were submitted"),
            };
        }
        if let Some(left) = self.leaving.take() {
            let stayed = match &view {
                View::Question {
                    question,
                    rows,
                    cut,
                    ..
                } => self.which(question, rows, *cut) == Some(left),
                _ => false,
            };
            if stayed {
                return Next::Stop("the dialog stayed on the question");
            }
            self.completed = left + 1;
        }
        match view {
            View::Gone => Next::Stop("no question dialog on screen"),
            View::Review {
                answers,
                on_submit,
                cut,
            } => {
                if self.completed < self.questions.len() || !self.reviewed(&answers, cut) {
                    Next::Stop("the review shows other answers")
                } else if !on_submit {
                    Next::Keys(vec!["up"])
                } else {
                    self.submitting = true;
                    Next::Keys(vec!["enter"])
                }
            }
            View::Question {
                question,
                rows,
                cursor,
                cut,
            } => {
                let Some(shown) = self.which(&question, &rows, cut) else {
                    return Next::Stop("another dialog is on screen");
                };
                if shown < self.completed {
                    return Next::Stop("the dialog went back to an answered question");
                }
                if shown > self.completed {
                    return Next::Keys(vec!["left"]);
                }
                let Some(cursor) = cursor else {
                    return Next::Stop("no cursor in the dialog");
                };
                self.plan(shown, &rows, cursor)
            }
        }
    }

    /// The next step on question `index`'s page.
    fn plan(&mut self, index: usize, rows: &[Row], cursor: usize) -> Next {
        let asked = &self.questions[index];
        let answer = &self.answers[index];
        let free = asked.options.len();
        let to = |row: usize| match cursor.cmp(&row) {
            std::cmp::Ordering::Less => Some(Next::Keys(vec!["down"])),
            std::cmp::Ordering::Greater => Some(Next::Keys(vec!["up"])),
            std::cmp::Ordering::Equal => None,
        };
        // The free answer's row: empty, showing `text`, or someone else's.
        let typed = |text: &str| {
            let shown = rows[free].words();
            if shown == PLACEHOLDER {
                Some(false)
            } else {
                let text = squash(text);
                (!shown.is_empty() && (text.starts_with(&shown) || shown.starts_with(&text)))
                    .then_some(true)
            }
        };
        if !asked.multi_select {
            let target = answer.choices.first().copied().unwrap_or(free);
            if let Some(step) = to(target) {
                return step;
            }
            if let (true, Some(text)) = (target == free, &answer.text) {
                match typed(text) {
                    Some(false) => return Next::Type(text.clone()),
                    Some(true) => {}
                    None => return Next::Stop("the free answer holds other text"),
                }
            }
            if self.questions.len() == 1 {
                self.submitting = true;
            } else {
                self.leaving = Some(index);
            }
            return Next::Keys(vec!["enter"]);
        }
        // Several: each box as answered, the free answer typed, then `Next`.
        for (row, label) in rows.iter().zip(0..free) {
            if row.checked() != Some(answer.choices.contains(&label)) {
                return to(label).unwrap_or(Next::Keys(vec!["space"]));
            }
        }
        match (&answer.text, typed(answer.text.as_deref().unwrap_or(""))) {
            (Some(text), Some(false)) => {
                return to(free).unwrap_or_else(|| Next::Type(text.clone()));
            }
            (Some(_), Some(true)) if rows[free].checked() == Some(true) => {}
            (None, Some(false)) if rows[free].checked() == Some(false) => {}
            _ => return Next::Stop("the free answer isn't as given"),
        }
        let next = free + 1;
        if rows.get(next) != Some(&Row::Action) {
            return Next::Stop("no Next under the options");
        }
        if let Some(step) = to(next) {
            return step;
        }
        self.leaving = Some(index);
        Next::Keys(vec!["enter"])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Screens captured from Claude Code 2.1.314, in
    /// `integrations/claude-code/fixtures/questions/`.
    fn screen(name: &str) -> String {
        let path = format!(
            "{}/../../../integrations/claude-code/fixtures/questions/{name}.txt",
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
            "Which colour do you prefer?",
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

    #[test]
    fn the_log_says_which_question_is_asked_and_when_it_has_its_result() {
        let asked = json!({"type": "assistant", "isSidechain": false, "message": {"content": [
            {"type": "tool_use", "id": "toolu_1", "name": "AskUserQuestion", "input": {"questions": [
                {"question": "Which colour do you prefer?", "header": "Colour", "multiSelect": false,
                 "options": [{"label": "Red", "description": "Warm"}, {"label": "Green"},
                             {"label": "Blue", "description": "Cool"}]}]}}]}});
        assert_eq!(
            logged(&asked),
            vec![Logged::Asked(OpenQuestion {
                call_id: "toolu_1".into(),
                questions: vec![colour()],
            })]
        );
        let result = json!({"type": "user", "message": {"content": [
            {"type": "tool_result", "tool_use_id": "toolu_1", "content": "answered"}]}});
        assert_eq!(logged(&result), vec![Logged::Answered("toolu_1".into())]);
        // A subagent's question is its own; another tool isn't a question.
        let mut sidechain = asked.clone();
        sidechain["isSidechain"] = json!(true);
        assert!(logged(&sidechain).is_empty());
        let bash = json!({"type": "assistant", "message": {"content": [
            {"type": "tool_use", "id": "toolu_2", "name": "Bash", "input": {"command": "ls"}}]}});
        assert!(logged(&bash).is_empty());
        // A call Rakumux can't read is still the newest question, with none to answer.
        let odd = json!({"type": "assistant", "message": {"content": [
            {"type": "tool_use", "id": "toolu_3", "name": "AskUserQuestion", "input": {"questions": [
                {"question": "Which?", "options": []}]}}]}});
        assert_eq!(
            logged(&odd),
            vec![Logged::Asked(OpenQuestion {
                call_id: "toolu_3".into(),
                questions: Vec::new(),
            })]
        );
    }

    #[test]
    fn a_question_page_is_read_with_its_rows_and_cursor() {
        let View::Question {
            question,
            rows,
            cursor,
            cut: false,
        } = View::read(&screen("one-question"))
        else {
            panic!("not a question page");
        };
        assert_eq!(question, "whichcolourdoyouprefer");
        assert_eq!(cursor, Some(0));
        assert_eq!(rows.len(), 5);
        assert!(rows[0].is("Red") && rows[1].is("Green") && rows[2].is("Blue"));
        assert_eq!(rows[3].words(), PLACEHOLDER);
        assert_eq!(rows[4].words(), CHAT);
        // The prompt above shows the question's JSON: the dialog is read
        // below the rule, never from it.
        let dialog = Dialog::new(vec![colour()], vec![answer(&[1], None)]);
        assert!(dialog.shown(&screen("one-question")));
        // Another question, or other options, isn't this dialog.
        let other = Dialog::new(vec![pet()], vec![answer(&[0], None)]);
        assert!(!other.shown(&screen("one-question")));
        let mut renamed = colour();
        renamed.options[2] = "Teal".into();
        assert!(
            !Dialog::new(vec![renamed], vec![answer(&[0], None)]).shown(&screen("one-question"))
        );
    }

    #[test]
    fn a_permission_prompt_and_the_prompt_box_are_no_dialog() {
        let dialog = Dialog::new(vec![colour()], vec![answer(&[0], None)]);
        for name in ["permission", "working"] {
            assert_eq!(View::read(&screen(name)), View::Gone, "{name}");
            assert!(!dialog.shown(&screen(name)), "{name}");
        }
        let mut dialog = dialog;
        assert_eq!(
            dialog.next(&screen("permission")),
            Next::Stop("no question dialog on screen")
        );
    }

    #[test]
    fn one_choice_is_reached_with_down_and_taken_with_enter() {
        let mut dialog = Dialog::new(vec![colour()], vec![answer(&[1], None)]);
        assert_eq!(
            dialog.next(&screen("one-question")),
            Next::Keys(vec!["down"])
        );
        assert_eq!(
            dialog.next(&screen("one-question-green")),
            Next::Keys(vec!["enter"])
        );
        // One question: that answers the dialog.
        assert_eq!(dialog.next(&screen("working")), Next::Done);
    }

    #[test]
    fn a_free_answer_is_typed_on_its_row() {
        let mut dialog = Dialog::new(vec![colour()], vec![answer(&[], Some("Purple, 2 shades"))]);
        assert_eq!(
            dialog.next(&screen("one-question")),
            Next::Keys(vec!["down"])
        );
        assert_eq!(
            dialog.next(&screen("one-question-free")),
            Next::Type("Purple, 2 shades".into())
        );
        assert_eq!(
            dialog.next(&screen("one-question-typed")),
            Next::Keys(vec!["enter"])
        );
        // Someone else's text there is left alone.
        let mut other = Dialog::new(vec![colour()], vec![answer(&[], Some("Orange"))]);
        assert_eq!(
            other.next(&screen("one-question-typed")),
            Next::Stop("the free answer holds other text")
        );
    }

    #[test]
    fn several_questions_are_answered_in_turn_then_reviewed_and_submitted() {
        let mut dialog = Dialog::new(
            vec![colour(), fruits(), pet()],
            vec![answer(&[1], None), answer(&[0], None), answer(&[0], None)],
        );
        // Colour: Green.
        assert_eq!(dialog.next(&screen("multi-q1")), Next::Keys(vec!["down"]));
        assert_eq!(
            dialog.next(&screen("multi-q1-green")),
            Next::Keys(vec!["enter"])
        );
        // Fruits: Apple ticked, then down to Next.
        assert_eq!(dialog.next(&screen("multi-q2")), Next::Keys(vec!["space"]));
        assert_eq!(
            dialog.next(&screen("multi-q2-apple")),
            Next::Keys(vec!["down"])
        );
        assert_eq!(
            dialog.next(&screen("multi-q2-ticked")),
            Next::Keys(vec!["down"])
        );
        assert_eq!(
            dialog.next(&screen("multi-q2-next")),
            Next::Keys(vec!["enter"])
        );
        // Pet: Cat, where the cursor starts.
        assert_eq!(dialog.next(&screen("multi-q3")), Next::Keys(vec!["enter"]));
        // The review shows the answers given: submitted.
        assert_eq!(
            dialog.next(&screen("multi-review")),
            Next::Keys(vec!["enter"])
        );
        assert_eq!(dialog.next(&screen("working")), Next::Done);
    }

    #[test]
    fn a_review_with_other_answers_is_never_submitted() {
        let mut dialog = Dialog::new(
            vec![colour(), fruits(), pet()],
            vec![answer(&[2], None), answer(&[0], None), answer(&[0], None)],
        );
        dialog.completed = 3;
        assert_eq!(
            dialog.next(&screen("multi-review")),
            Next::Stop("the review shows other answers")
        );
        // Nor before every question was answered here.
        let mut early = Dialog::new(
            vec![colour(), fruits(), pet()],
            vec![answer(&[1], None), answer(&[0], None), answer(&[0], None)],
        );
        assert_eq!(
            early.next(&screen("multi-review")),
            Next::Stop("the review shows other answers")
        );
    }

    #[test]
    fn a_page_further_on_goes_back_to_the_question_due() {
        let mut dialog = Dialog::new(
            vec![colour(), fruits(), pet()],
            vec![answer(&[1], None), answer(&[0], None), answer(&[0], None)],
        );
        assert_eq!(dialog.next(&screen("multi-q3")), Next::Keys(vec!["left"]));
    }

    #[test]
    fn long_questions_and_labels_wrap_and_are_read_whole() {
        let subagents = question(
            "Ship subagent activity? It needs: migration 0024 on production (adds nullable \
             columns and an index; safe under the live site), deploying rakumux.com, and a \
             Rakumux release for machines to start sending subagents.",
            "Subagents",
            &[
                "Ship web now, Rakumux in 0.1.35 (Recommended)",
                "Ship everything now",
                "Hold",
            ],
            false,
        );
        let fixes = question(
            "Which audit fixes should I start now, in parallel, on branches, for the next \
             release? Pick every one you want.",
            "Audit fixes",
            &[
                "The mid-turn guard check for older servers, plus a long tail of words to wrap",
                "Revoking the terminal token",
                "Honest wording",
            ],
            true,
        );
        let text = "Also the FIFO freeze fix, the long one that wraps across the narrow screen";
        let mut dialog = Dialog::new(
            vec![subagents, fixes],
            vec![answer(&[2], None), answer(&[0], Some(text))],
        );
        assert!(dialog.shown(&screen("long-q1")));
        assert_eq!(dialog.next(&screen("long-q1")), Next::Keys(vec!["down"]));
        dialog.completed = 1;
        assert_eq!(dialog.next(&screen("long-q2")), Next::Keys(vec!["space"]));
        assert_eq!(
            dialog.next(&screen("long-q2-text")),
            Next::Keys(vec!["down"])
        );
        dialog.completed = 2;
        assert_eq!(
            dialog.next(&screen("long-review")),
            Next::Keys(vec!["enter"])
        );
    }

    /// The two questions of the dialog captured taller than its terminal.
    fn tall() -> Vec<Question> {
        vec![
            question(
                "Custom script: today rakumux.com can never send a command to a machine. A new-session request names only an agent from a fixed list, and the contract, the Rust service and the privacy notes in AGENTS.md all say so. A custom script ends that rule, so a compromised rakumux.com or a stolen web sign-in could run shell commands on the machine. How should it work?",
                "Script risk",
                &["Machine opts in (Recommended)", "On for every machine", "Leave it out for now"],
                false,
            ),
            question(
                "Custom script and swipe-to-Delete both need the machine to do something new, and rakumux.com has no way to ask a machine to delete a session today. That needs a contract change and Rust work in the public rakumux repo, plus a release. It also needs new SQL migrations, which I would write but not run. You asked for this work in rakumux-cloud, so how far should I go?",
                "Repo scope",
                &["Add a public branch too (Recommended)", "rakumux-cloud only"],
                false,
            ),
        ]
    }

    #[test]
    fn a_dialog_taller_than_the_terminal_is_answered_question_by_question() {
        let mut dialog = Dialog::new(tall(), vec![answer(&[0], None), answer(&[0], None)]);
        assert!(dialog.shown(&screen("tall-q1")));
        assert_eq!(dialog.next(&screen("tall-q1")), Next::Keys(vec!["enter"]));
        assert_eq!(dialog.next(&screen("tall-q2")), Next::Keys(vec!["enter"]));
        assert_eq!(
            dialog.next(&screen("tall-review")),
            Next::Keys(vec!["enter"])
        );
        assert_eq!(dialog.next(&screen("working")), Next::Done);
    }

    #[test]
    fn a_cut_page_goes_to_the_option_given_on_the_question_it_shows() {
        let mut dialog = Dialog::new(tall(), vec![answer(&[1], None), answer(&[0], None)]);
        assert_eq!(dialog.next(&screen("tall-q1")), Next::Keys(vec!["down"]));
        // The second question's page, due only once the first is answered.
        let mut early = Dialog::new(tall(), vec![answer(&[0], None), answer(&[0], None)]);
        assert_eq!(early.next(&screen("tall-q2")), Next::Keys(vec!["left"]));
    }

    #[test]
    fn a_cut_review_is_checked_by_the_answers_left_on_it() {
        let mut dialog = Dialog::new(tall(), vec![answer(&[0], None), answer(&[0], None)]);
        dialog.completed = 2;
        assert_eq!(
            dialog.next(&screen("tall-review-cut")),
            Next::Keys(vec!["enter"])
        );
        let mut other = Dialog::new(tall(), vec![answer(&[0], None), answer(&[1], None)]);
        other.completed = 2;
        assert_eq!(
            other.next(&screen("tall-review-cut")),
            Next::Stop("the review shows other answers")
        );
    }

    #[test]
    fn a_cut_page_two_questions_could_be_is_neither() {
        let same = |text: &str| question(text, "Q", &["Yes", "No"], false);
        let page = "  (a line of the question)\n\n❯ 1. Yes\n  2. No\n  3. Type something.\n──────────\n  4. Chat about this\n";
        let page = page.replace("  (a line of the question)\n", "");
        let dialog = Dialog::new(
            vec![same("First?"), same("Second?")],
            vec![answer(&[0], None), answer(&[0], None)],
        );
        assert!(!dialog.shown(&page));
        let one = Dialog::new(vec![same("First?")], vec![answer(&[0], None)]);
        assert!(one.shown(&page));
    }

    #[test]
    fn a_page_with_its_options_cut_off_is_not_answered() {
        let dialog = Dialog::new(tall(), vec![answer(&[0], None), answer(&[0], None)]);
        assert!(!dialog.shown(&screen("tall-options-cut")));
    }

    #[test]
    fn a_tab_bar_wrapped_on_a_narrow_screen_is_read_past() {
        let wide = screen("multi-q1");
        let narrow = wide.replace(
            "←  ☐ Colour  ☐ Fruits  ☐ Pet  ✔ Submit  →",
            "←  ☐ Colour  ☐ Fruits  ☐ Pet\n✔ Submit  →",
        );
        assert_ne!(wide, narrow);
        let mut dialog = Dialog::new(
            vec![colour(), fruits(), pet()],
            vec![answer(&[1], None), answer(&[0], None), answer(&[0], None)],
        );
        assert!(dialog.shown(&narrow));
        assert_eq!(dialog.next(&narrow), Next::Keys(vec!["down"]));
    }

    #[test]
    fn a_step_is_seen_taken_only_when_the_dialog_moved() {
        let dialog = Dialog::new(vec![colour()], vec![answer(&[1], None)]);
        assert!(dialog.moved(&screen("one-question"), &screen("one-question-green")));
        // The same page, with its spinner or its prompt above changed, didn't move.
        let before = screen("one-question");
        let after = before.replacen("This is a test", "This was a test", 1);
        assert!(!dialog.moved(&before, &after));
    }
}
