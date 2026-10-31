//! Kimi Code's questions (`AskUserQuestion`, captured 2.4.0), read from its
//! main agent's `wire.jsonl`, and its dialog, worked from its screen
//! (`numbered.rs`).
//!
//! The log has the call as a `tool.call` loop event (`args`: `questions`,
//! each a `question` with a `header`, `options` of `label`s and
//! `multi_select`), and its result as a `tool.result` naming the same
//! `toolCallId`. The dialog replaces the input box between two rules, under
//! its title `question`: a tab per question then `Submit`, `? ` and the
//! question, its options (`→ [1] Red` with the cursor, `[2] Green`, each
//! description under it; `[ ]`/`[✓] Apple`, unnumbered, for a question
//! that takes several), then `[4] Other`, the free answer (`Other: text`
//! once typed). Keys, as checked against the real Kimi Code:
//! - **One choice:** its digit picks it and moves on.
//! - **Several:** each digit ticks or unticks its box; Tab moves on.
//! - **Free answer:** the number after the options opens its box (the hint
//!   says `type answer  ↵ save`), the text is typed, and Enter saves it (and
//!   moves on from a question taking one choice).
//! - **Review:** after the last question, `Review your answer before
//!   submit` lists each `Q` question with `→` its answer, over `[1] Submit`
//!   and `[2] Cancel`: `1` submits.

use super::numbered::{Free, Mark, Page, Row, Style, View};
use super::{listed, squash, Logged, OpenQuestion};
use serde_json::Value;

/// The tool Kimi Code asks the user with.
const TOOL: &str = "AskUserQuestion";
/// What the dialog says while the free answer is typed, under the question.
const TYPING: &str = "Type your answer, then press Enter to save.";

/// What `entry` of Kimi Code's log says about questions: an
/// `AskUserQuestion` call of the main agent, or a tool's result. A call
/// whose input isn't the tool's shape is still the newest question, with
/// none to answer.
pub(super) fn logged(entry: &Value) -> Vec<Logged> {
    let Some(event) = super::super::kimi::loop_event(entry) else {
        return Vec::new();
    };
    let id = || {
        event
            .get("toolCallId")
            .and_then(Value::as_str)
            .map(str::to_owned)
    };
    match event.get("type").and_then(Value::as_str) {
        Some("tool.call") if event.get("name").and_then(Value::as_str) == Some(TOOL) => id()
            .map(|call_id| {
                Logged::Asked(OpenQuestion {
                    call_id,
                    questions: event
                        .get("args")
                        .and_then(|args| args.get("questions"))
                        .and_then(listed)
                        .unwrap_or_default(),
                })
            })
            .into_iter()
            .collect(),
        Some("tool.result") => id().map(Logged::Answered).into_iter().collect(),
        _ => Vec::new(),
    }
}

/// How Kimi Code's dialog takes keys.
pub(super) static STYLE: Style = Style {
    read,
    free_key: None,
    leave: "tab",
    free_leaves: false,
    submit: "1",
    back: "left",
    reviews: |_| true,
};

/// A rule across the screen.
fn is_rule(line: &str) -> bool {
    let line = line.trim();
    line.chars().count() >= 8 && line.chars().all(|c| c == '─')
}

/// Kimi Code's question dialog as its screen shows it.
pub(super) fn read(screen: &str) -> View {
    let lines: Vec<&str> = screen.lines().map(str::trim_end).collect();
    // Its title, right under the rule that opens it, and the rule that
    // closes it.
    let Some(title) = lines
        .iter()
        .enumerate()
        .rposition(|(at, line)| line.trim() == "question" && at > 0 && is_rule(lines[at - 1]))
    else {
        return View::Gone;
    };
    let Some(end) = lines[title..].iter().position(|line| is_rule(line)) else {
        return View::Gone;
    };
    let body = &lines[title + 1..title + end];
    review(body)
        .map(View::Review)
        .or_else(|| page(body).map(View::Question))
        .unwrap_or(View::Gone)
}

/// The review: `Review your answer before submit`, each `Q` question and
/// its `→` answer, then `Ready to submit your answers?` over `[1] Submit`
/// and `[2] Cancel`.
fn review(body: &[&str]) -> Option<Vec<(String, String)>> {
    let title = body
        .iter()
        .position(|line| line.trim() == "Review your answer before submit")?;
    let ready = body
        .iter()
        .position(|line| line.trim() == "Ready to submit your answers?")?;
    let choices: Vec<String> = body[ready + 1..]
        .iter()
        .map(|line| line.trim())
        .filter(|line| !line.is_empty() && !line.starts_with("↑↓"))
        .map(|line| squash(line.trim_start_matches('→')))
        .collect();
    if choices != ["1submit", "2cancel"] {
        return None;
    }
    let mut answers: Vec<(String, String)> = Vec::new();
    let mut answering = false;
    for line in body.get(title + 1..ready)? {
        let text = line.trim();
        if let Some(question) = text.strip_prefix("Q ") {
            answers.push((squash(question), String::new()));
            answering = false;
            continue;
        }
        let Some((question, answer)) = answers.last_mut() else {
            continue;
        };
        if let Some(said) = text.strip_prefix("→ ") {
            answering = true;
            answer.push_str(&squash(said));
        } else if answering {
            answer.push_str(&squash(text));
        } else {
            question.push_str(&squash(text));
        }
    }
    Some(answers)
}

/// A row's start: `→ [1] Red` or `[2] Green` (one choice), `[ ] Apple` or
/// `[✓] Apple` (several); its number (for one choice), mark and label.
fn row(line: &str) -> Option<(Option<usize>, Mark, &str)> {
    let line = line.trim_start();
    let line = line.strip_prefix("→ ").unwrap_or(line);
    if let Some(label) = line.strip_prefix("[ ] ") {
        return Some((None, Mark::Box(false), label));
    }
    if let Some(label) = line.strip_prefix("[✓] ") {
        return Some((None, Mark::Box(true), label));
    }
    let rest = line.strip_prefix('[')?;
    let (number, label) = rest.split_once("] ")?;
    Some((Some(number.parse().ok()?), Mark::None, label))
}

/// A question's page: `? ` and its text, its rows, the free answer last,
/// and the hint under them.
fn page(body: &[&str]) -> Option<Page> {
    let hint = body.iter().rposition(|line| !line.trim().is_empty())?;
    let typing = match body[hint].trim() {
        hint if hint.starts_with("type answer") => true,
        hint if hint.starts_with("↑↓ select") => false,
        _ => return None,
    };
    let asks = body
        .iter()
        .position(|line| line.trim_start().starts_with("? "))?;
    let mut question = String::new();
    let mut lines = body[asks..hint].iter();
    for line in lines.by_ref() {
        let text = line.trim();
        if text.is_empty() {
            break;
        }
        if text != TYPING {
            question.push_str(&squash(text.strip_prefix("? ").unwrap_or(text)));
        }
    }
    // The rows, each line under one (a description, a wrap) its own.
    let mut rows: Vec<(Mark, String, String)> = Vec::new();
    for line in lines {
        if line.trim().is_empty() {
            continue;
        }
        match row(line) {
            Some((number, mark, label)) => {
                if number.is_some_and(|number| number != rows.len() + 1) {
                    return None;
                }
                rows.push((mark, label.trim().to_owned(), String::new()));
            }
            None => rows.last_mut()?.2.push_str(&squash(line)),
        }
    }
    let (mark, label, _) = rows.pop()?;
    // The free answer: `Other`, or `Other: text` once typed.
    let text = match label.strip_prefix("Other") {
        Some("") => None,
        Some(rest) => Some(rest.strip_prefix(':')?.trim().to_owned()),
        None => return None,
    };
    let ticked = mark == Mark::Box(true);
    let free = match (typing, text) {
        (true, text) => Free::Editing(text.unwrap_or_default()),
        (false, Some(text)) if !text.is_empty() => Free::Given(text),
        (false, _) => Free::Idle { ticked },
    };
    let options = rows
        .into_iter()
        .map(|(mark, label, rest)| {
            let first = squash(&label);
            Row {
                words: format!("{first}{rest}"),
                first,
                cut: false,
                mark,
            }
        })
        .collect::<Vec<_>>();
    (!question.is_empty() && !options.is_empty()).then_some(Page {
        question,
        options,
        free,
        leaves: !typing,
    })
}

#[cfg(test)]
mod tests {
    use super::super::numbered::Dialog;
    use super::super::{Answer, Next, Question};
    use super::*;
    use serde_json::json;

    /// Screens captured from Kimi Code 2.4.0 against a scripted model, in
    /// `integrations/kimi/fixtures/questions/`.
    fn screen(name: &str) -> String {
        let path = format!(
            "{}/../../../integrations/kimi/fixtures/questions/{name}.txt",
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

    fn driver(questions: Vec<Question>, answers: Vec<Answer>) -> Dialog {
        Dialog::new(&STYLE, questions, answers)
    }

    fn event(event: Value) -> Value {
        json!({"type": "context.append_loop_event", "agentId": "main", "event": event,
            "time": 1_793_429_569_857_i64})
    }

    #[test]
    fn the_log_says_which_question_is_asked_and_when_it_has_its_result() {
        let asked = event(json!({"type": "tool.call", "uuid": "f632", "turnId": "1",
            "toolCallId": "call_1", "name": "AskUserQuestion", "args": {"questions": [
                {"question": "Which colour do you prefer?", "header": "Colour",
                 "options": [{"label": "Red", "description": "Warm"}, {"label": "Green"},
                             {"label": "Blue", "description": "Cool"}],
                 "multi_select": false}]}}));
        assert_eq!(
            logged(&asked),
            vec![Logged::Asked(OpenQuestion {
                call_id: "call_1".into(),
                questions: vec![colour()],
            })]
        );
        let result = event(json!({"type": "tool.result", "parentUuid": "f632",
            "toolCallId": "call_1", "result": {"output": "{\"answers\":{}}"}}));
        assert_eq!(logged(&result), vec![Logged::Answered("call_1".into())]);
        // A subagent's question is its own; another tool isn't a question.
        let mut subagent = asked.clone();
        subagent["agentId"] = json!("coder-1");
        assert!(logged(&subagent).is_empty());
        let bash = event(
            json!({"type": "tool.call", "toolCallId": "call_2", "name": "Bash",
            "args": {"command": "ls"}}),
        );
        assert!(logged(&bash).is_empty());
    }

    #[test]
    fn a_question_page_is_read_with_its_options_and_free_answer() {
        let View::Question(page) = read(&screen("one-question")) else {
            panic!("not a question page");
        };
        assert_eq!(page.question, "whichcolourdoyouprefer");
        assert_eq!(page.options.len(), 3);
        assert!(page.options[0].is("Red") && page.options[2].is("Blue"));
        assert_eq!(page.free, Free::Idle { ticked: false });
        assert!(driver(vec![colour()], vec![answer(&[1], None)]).shown(&screen("one-question")));
        assert!(!driver(vec![pet()], vec![answer(&[0], None)]).shown(&screen("one-question")));
    }

    #[test]
    fn an_approval_panel_and_the_prompt_box_are_no_dialog() {
        for name in ["permission", "idle"] {
            assert_eq!(read(&screen(name)), View::Gone, "{name}");
        }
    }

    #[test]
    fn one_choice_is_its_digit_then_the_review_is_submitted() {
        let mut dialog = driver(vec![colour()], vec![answer(&[1], None)]);
        assert_eq!(dialog.next(&screen("one-question")), Next::Keys(vec!["2"]));
        assert_eq!(dialog.next(&screen("one-review")), Next::Keys(vec!["1"]));
        assert_eq!(dialog.next(&screen("idle")), Next::Done);
    }

    #[test]
    fn a_free_answer_is_typed_in_its_box_and_saved_with_enter() {
        let mut dialog = driver(vec![colour()], vec![answer(&[], Some("Purple, 2 shades"))]);
        assert_eq!(dialog.next(&screen("one-question")), Next::Keys(vec!["4"]));
        assert_eq!(
            dialog.next(&screen("one-other")),
            Next::Type("Purple, 2 shades".into())
        );
        assert_eq!(dialog.next(&screen("one-typed")), Next::Keys(vec!["enter"]));
        assert_eq!(
            dialog.next(&screen("one-free-review")),
            Next::Keys(vec!["1"])
        );
        // A review of another answer is never submitted.
        let mut other = driver(vec![colour()], vec![answer(&[0], None)]);
        assert_eq!(other.next(&screen("one-question")), Next::Keys(vec!["1"]));
        assert_eq!(
            other.next(&screen("one-free-review")),
            Next::Stop("the review shows other answers")
        );
    }

    #[test]
    fn several_questions_are_answered_in_turn_then_reviewed_and_submitted() {
        let mut dialog = driver(
            vec![colour(), fruits(), pet()],
            vec![
                answer(&[1], None),
                answer(&[0], Some("Mango, ripe")),
                answer(&[0], None),
            ],
        );
        assert_eq!(dialog.next(&screen("multi-q1")), Next::Keys(vec!["2"]));
        assert_eq!(dialog.next(&screen("multi-q2")), Next::Keys(vec!["1"]));
        assert_eq!(
            dialog.next(&screen("multi-q2-apple")),
            Next::Keys(vec!["4"])
        );
        assert_eq!(
            dialog.next(&screen("multi-q2-other")),
            Next::Type("Mango, ripe".into())
        );
        assert_eq!(
            dialog.next(&screen("multi-q2-typed")),
            Next::Keys(vec!["enter"])
        );
        assert_eq!(
            dialog.next(&screen("multi-q2-saved")),
            Next::Keys(vec!["tab"])
        );
        assert_eq!(dialog.next(&screen("multi-q3")), Next::Keys(vec!["1"]));
        assert_eq!(dialog.next(&screen("multi-review")), Next::Keys(vec!["1"]));
        assert_eq!(dialog.next(&screen("idle")), Next::Done);
    }

    #[test]
    fn long_questions_wrap_and_are_read_whole() {
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
        let text = "Also the FIFO freeze fix";
        let mut dialog = driver(
            vec![subagents, fixes],
            vec![answer(&[2], None), answer(&[0], Some(text))],
        );
        assert!(dialog.shown(&screen("long-q1")));
        assert_eq!(dialog.next(&screen("long-q1")), Next::Keys(vec!["3"]));
        assert_eq!(dialog.next(&screen("long-q2")), Next::Keys(vec!["1"]));
        assert_eq!(
            dialog.next(&screen("long-q2-text")),
            Next::Keys(vec!["enter"])
        );
        // The review of these answers, its lines wrapped, is theirs.
        assert!(dialog.shown(&screen("long-review")));
    }
}
