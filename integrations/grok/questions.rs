//! Grok Build's questions (`ask_user_question`, captured 1.0.70), read from
//! its session's `updates.jsonl`, and its dialog, worked from its screen
//! (`numbered.rs`).
//!
//! The log has the call as a `tool_call` (its `rawInput`: `questions`, each a
//! `question` with `options` of `label`s and `multi_select`), and its result
//! as a `tool_call_update` with a `status` (`completed` when answered, also
//! `failed`). The dialog sits above `Tab:next answer │ … │ Shift+x:dismiss`
//! (while the free answer is typed, `Enter:submit │ Esc:back`), each line
//! after the `┃` of its bar: the question, then its options, `1 (○) Red
//! Warm` (the label, its description in a column of its own; `[ ]`/`[x]`
//! for a question that takes several; a label cut short ends in `…`), then
//! `z (○) Type your answer here`, the free answer, and a hint whose right
//! end says what Enter does now. Keys, as checked against the real Grok:
//! - **One choice:** its digit picks it and moves on (after the last
//!   question, answers the dialog: Grok has no review).
//! - **Several:** each digit ticks or unticks its box; Enter (`Enter:next`,
//!   or `Enter:submit` on the last question) moves on.
//! - **Free answer:** `z` opens its box (`z (●) ❯`), the text is typed, and
//!   Enter saves it and moves on.

use super::numbered::{Free, Mark, Page, Row, Style, View};
use super::{listed, squash, Logged, OpenQuestion};
use serde_json::Value;

/// The tool Grok Build asks the user with.
const TOOL: &str = "ask_user_question";
/// The free answer's row, empty.
const PLACEHOLDER: &str = "typeyouranswerhere";

/// What `entry` of Grok Build's log says about questions: an
/// `ask_user_question` call, or a tool's result. A call whose input isn't
/// the tool's shape is still the newest question, with none to answer.
pub(super) fn logged(entry: &Value) -> Vec<Logged> {
    let Some(update) = entry.get("params").and_then(|params| params.get("update")) else {
        return Vec::new();
    };
    let id = || {
        update
            .get("toolCallId")
            .and_then(Value::as_str)
            .map(str::to_owned)
    };
    match update.get("sessionUpdate").and_then(Value::as_str) {
        Some("tool_call") if tool(update) == Some(TOOL) => id()
            .map(|call_id| {
                Logged::Asked(OpenQuestion {
                    call_id,
                    questions: update
                        .get("rawInput")
                        .and_then(|input| input.get("questions"))
                        .and_then(listed)
                        .unwrap_or_default(),
                })
            })
            .into_iter()
            .collect(),
        // Only an update with a status that ends the call is its result.
        Some("tool_call_update") => match update.get("status").and_then(Value::as_str) {
            Some("pending" | "in_progress") | None => Vec::new(),
            Some(_) => id().map(Logged::Answered).into_iter().collect(),
        },
        _ => Vec::new(),
    }
}

/// A tool call's name, as Grok names it.
fn tool(update: &Value) -> Option<&str> {
    update
        .get("_meta")
        .and_then(|meta| meta.get("x.ai/tool"))
        .and_then(|meta| meta.get("name"))
        .and_then(Value::as_str)
        .or_else(|| update.get("title").and_then(Value::as_str))
}

/// How Grok Build's dialog takes keys.
pub(super) static STYLE: Style = Style {
    read,
    free_key: Some("z"),
    leave: "enter",
    free_leaves: true,
    submit: "enter",
    back: "left",
    reviews: |_| false,
};

/// A row's mark and the rest: `(○) Red`, `[x] Apple`.
fn marked(rest: &str) -> Option<(Mark, &str)> {
    for (mark, shown) in [
        ("(○) ", Mark::Radio(false)),
        ("(●) ", Mark::Radio(true)),
        ("[ ] ", Mark::Box(false)),
        ("[x] ", Mark::Box(true)),
    ] {
        if let Some(rest) = rest.strip_prefix(mark) {
            return Some((shown, rest));
        }
    }
    // A row with nothing after its mark (an open, empty box).
    match rest {
        "(○)" => Some((Mark::Radio(false), "")),
        "(●)" => Some((Mark::Radio(true), "")),
        "[ ]" => Some((Mark::Box(false), "")),
        "[x]" => Some((Mark::Box(true), "")),
        _ => None,
    }
}

/// Grok Build's question dialog as its screen shows it.
pub(super) fn read(screen: &str) -> View {
    read_page(screen).map_or(View::Gone, View::Question)
}

fn read_page(screen: &str) -> Option<Page> {
    let lines: Vec<&str> = screen.lines().map(str::trim_end).collect();
    // The dialog's footer, normal or while the free answer is typed (cut
    // short on a narrow screen).
    let footer = lines.iter().rposition(|line| {
        let line = line.trim();
        line.starts_with("Tab:next answer")
            || (line.starts_with("Enter:submit") && line.contains("Esc:back"))
    })?;
    let typing = lines[footer].trim().starts_with("Enter:submit");
    // The bar's lines above it (a blank line between).
    let end = lines[..footer]
        .iter()
        .rposition(|line| !line.trim().is_empty())?;
    let start = lines[..=end]
        .iter()
        .rposition(|line| !line.trim_start().starts_with('┃'))
        .map_or(0, |at| at + 1);
    let body: Vec<&str> = lines[start..=end]
        .iter()
        .map(|line| {
            let line = line.trim_start().trim_start_matches('┃');
            // The chat's scrollbar runs down the right edge.
            line.trim_end_matches(['█', ' '])
                .strip_prefix("  ")
                .unwrap_or("")
        })
        .collect();
    let hint = body
        .iter()
        .rposition(|line| line.contains("↑/↓ navigate"))?;
    let leaves = matches!(
        body[hint].rsplit("Enter:").next().map(str::trim),
        Some("next" | "submit")
    ) && body[hint].contains("Enter:");
    let mut question = String::new();
    let mut options: Vec<Row> = Vec::new();
    let mut free = None;
    for line in &body[..hint] {
        let (key, rest) = match line.split_once(' ') {
            Some((key, rest)) if key == "z" || key.parse::<usize>().is_ok() => (key, rest),
            _ => ("", *line),
        };
        let row = (!key.is_empty()).then(|| marked(rest)).flatten();
        match row {
            Some((mark, rest)) if key == "z" => {
                let ticked = matches!(mark, Mark::Radio(true) | Mark::Box(true));
                free = Some(match rest.strip_prefix('❯').map(str::trim) {
                    Some(text) if typing => Free::Editing(text.to_owned()),
                    Some(text) => Free::Given(text.to_owned()),
                    None if squash(rest) == PLACEHOLDER => Free::Idle { ticked },
                    None => Free::Given(rest.trim().to_owned()),
                });
            }
            Some((mark, rest)) => {
                if free.is_some() || key.parse::<usize>().ok() != Some(options.len() + 1) {
                    return None;
                }
                // The label, then its description in a column of its own.
                let (label, description) = rest.split_once("  ").unwrap_or((rest, ""));
                let cut = label.ends_with('…');
                let first = squash(label.trim_end_matches('…'));
                options.push(Row {
                    words: format!("{first}{}", squash(description)),
                    first,
                    cut,
                    mark,
                });
            }
            None if free.is_some() => {
                if !line.trim().is_empty() {
                    return None;
                }
            }
            None => match options.last_mut() {
                // A wrapped label or description.
                Some(row) => row.words.push_str(&squash(line)),
                None => question.push_str(&squash(line)),
            },
        }
    }
    let free = free?;
    (!question.is_empty() && !options.is_empty()).then_some(Page {
        question,
        options,
        free,
        leaves,
    })
}

#[cfg(test)]
mod tests {
    use super::super::numbered::Dialog;
    use super::super::{Answer, Next, Question};
    use super::*;
    use serde_json::json;

    /// Screens captured from Grok Build 1.0.70 against a scripted model, in
    /// `integrations/grok/fixtures/questions/`.
    fn screen(name: &str) -> String {
        let path = format!(
            "{}/../../../integrations/grok/fixtures/questions/{name}.txt",
            env!("CARGO_MANIFEST_DIR")
        );
        std::fs::read_to_string(&path).unwrap_or_else(|error| panic!("{path}: {error}"))
    }

    fn question(text: &str, options: &[&str], multi_select: bool) -> Question {
        Question {
            question: text.into(),
            header: None,
            options: options.iter().map(|o| o.to_string()).collect(),
            multi_select,
        }
    }

    fn colour() -> Question {
        question(
            "Which colour do you prefer?",
            &["Red", "Green", "Blue"],
            false,
        )
    }

    fn fruits() -> Question {
        question(
            "Which fruits do you like?",
            &["Apple", "Banana", "Cherry"],
            true,
        )
    }

    fn pet() -> Question {
        question("Which pet would you keep?", &["Cat", "Dog"], false)
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

    fn entry(update: Value) -> Value {
        json!({"timestamp": 1_793_429_376, "method": "session/update",
            "params": {"sessionId": "01a1", "update": update, "_meta": {"eventId": "01a1-6"}}})
    }

    #[test]
    fn the_log_says_which_question_is_asked_and_when_it_has_its_result() {
        let asked = entry(json!({"sessionUpdate": "tool_call", "toolCallId": "call_1",
            "title": "ask_user_question", "rawInput": {"questions": [
                {"question": "Which colour do you prefer?", "multi_select": false,
                 "options": [{"label": "Red", "description": "Warm"},
                             {"label": "Green", "description": "green"},
                             {"label": "Blue", "description": "Cool"}]}]},
            "_meta": {"x.ai/tool": {"name": "ask_user_question", "kind": "ask_user"}}}));
        assert_eq!(
            logged(&asked),
            vec![Logged::Asked(OpenQuestion {
                call_id: "call_1".into(),
                questions: vec![colour()],
            })]
        );
        // Answering logs the input again, without a status: not its result.
        let normalized = entry(json!({"sessionUpdate": "tool_call_update",
            "toolCallId": "call_1", "rawInput": {"variant": "AskUserQuestion"}}));
        assert!(logged(&normalized).is_empty());
        let result = entry(json!({"sessionUpdate": "tool_call_update",
            "toolCallId": "call_1", "status": "completed",
            "rawOutput": {"type": "AskUserQuestion", "UserAnswered": {"answers": []}}}));
        assert_eq!(logged(&result), vec![Logged::Answered("call_1".into())]);
        // Another tool isn't a question.
        let shell = entry(json!({"sessionUpdate": "tool_call", "toolCallId": "call_2",
            "title": "run_terminal_command", "rawInput": {"command": "ls"}}));
        assert!(logged(&shell).is_empty());
    }

    #[test]
    fn a_question_page_is_read_with_its_options_and_free_answer() {
        let View::Question(page) = read(&screen("one-question")) else {
            panic!("not a question page");
        };
        assert_eq!(page.question, "whichcolourdoyouprefer");
        assert_eq!(page.options.len(), 3);
        assert!(page.options[0].is("Red") && page.options[2].is("Blue"));
        assert_eq!(page.options[1].mark, Mark::Radio(false));
        assert_eq!(page.free, Free::Idle { ticked: false });
        assert!(driver(vec![colour()], vec![answer(&[1], None)]).shown(&screen("one-question")));
        assert!(!driver(vec![pet()], vec![answer(&[0], None)]).shown(&screen("one-question")));
    }

    #[test]
    fn a_permission_prompt_and_the_prompt_box_are_no_dialog() {
        for name in ["permission", "idle"] {
            assert_eq!(read(&screen(name)), View::Gone, "{name}");
        }
        let mut dialog = driver(vec![colour()], vec![answer(&[0], None)]);
        assert_eq!(
            dialog.next(&screen("permission")),
            Next::Stop("no question dialog on screen")
        );
    }

    #[test]
    fn one_choice_is_its_digit_which_answers_the_dialog() {
        let mut dialog = driver(vec![colour()], vec![answer(&[1], None)]);
        assert_eq!(dialog.next(&screen("one-question")), Next::Keys(vec!["2"]));
        assert_eq!(dialog.next(&screen("idle")), Next::Done);
    }

    #[test]
    fn a_free_answer_is_typed_in_its_box_and_saved_with_enter() {
        let mut dialog = driver(vec![colour()], vec![answer(&[], Some("Purple, 2 shades"))]);
        assert_eq!(dialog.next(&screen("one-question")), Next::Keys(vec!["z"]));
        assert_eq!(
            dialog.next(&screen("one-question-z")),
            Next::Type("Purple, 2 shades".into())
        );
        assert_eq!(
            dialog.next(&screen("one-question-typed")),
            Next::Keys(vec!["enter"])
        );
        assert_eq!(dialog.next(&screen("idle")), Next::Done);
        // Someone else's text in the box is left alone.
        let mut other = driver(vec![colour()], vec![answer(&[], Some("Orange"))]);
        assert_eq!(
            other.next(&screen("one-question-typed")),
            Next::Stop("the free answer holds other text")
        );
        // A box open while the answer is an option: someone is typing.
        let mut option = dialog_of_green();
        assert_eq!(
            option.next(&screen("one-question-z")),
            Next::Stop("someone is typing an answer")
        );
    }

    fn dialog_of_green() -> Dialog {
        driver(vec![colour()], vec![answer(&[1], None)])
    }

    #[test]
    fn several_questions_are_answered_in_turn_and_the_last_answers_the_dialog() {
        let mut dialog = driver(
            vec![colour(), fruits(), pet()],
            vec![
                answer(&[1], None),
                answer(&[0, 2], Some("Mango, ripe")),
                answer(&[0], None),
            ],
        );
        assert_eq!(dialog.next(&screen("multi-q1")), Next::Keys(vec!["2"]));
        // Fruits: Apple and Cherry ticked, then the free answer, which moves on.
        assert_eq!(dialog.next(&screen("multi-q2")), Next::Keys(vec!["1"]));
        assert_eq!(
            dialog.next(&screen("multi-q2-apple")),
            Next::Keys(vec!["3"])
        );
        assert_eq!(
            dialog.next(&screen("multi-q2-apple-cherry")),
            Next::Keys(vec!["z"])
        );
        assert_eq!(
            dialog.next(&screen("multi-q2-z")),
            Next::Type("Mango, ripe".into())
        );
        assert_eq!(
            dialog.next(&screen("multi-q2-typed")),
            Next::Keys(vec!["enter"])
        );
        assert_eq!(dialog.next(&screen("multi-q3")), Next::Keys(vec!["1"]));
        assert_eq!(dialog.next(&screen("idle")), Next::Done);
    }

    #[test]
    fn a_page_of_boxes_is_left_with_enter_once_its_hint_says_so() {
        let mut dialog = driver(
            vec![colour(), fruits(), pet()],
            vec![answer(&[1], None), answer(&[2], None), answer(&[0], None)],
        );
        assert_eq!(dialog.next(&screen("multi-q1")), Next::Keys(vec!["2"]));
        assert_eq!(dialog.next(&screen("multi-q2")), Next::Keys(vec!["3"]));
        assert_eq!(
            dialog.next(&screen("multi-q2-cherry")),
            Next::Keys(vec!["enter"])
        );
        assert_eq!(dialog.next(&screen("multi-q3")), Next::Keys(vec!["1"]));
    }

    #[test]
    fn a_page_further_on_goes_back_to_the_question_due() {
        let mut dialog = driver(
            vec![colour(), fruits(), pet()],
            vec![answer(&[1], None), answer(&[0], None), answer(&[0], None)],
        );
        assert_eq!(dialog.next(&screen("multi-q3")), Next::Keys(vec!["left"]));
    }

    #[test]
    fn long_labels_cut_short_are_read_by_their_start() {
        let subagents = question(
            "Ship subagent activity? It needs: migration 0024 on production (adds nullable \
             columns and an index; safe under the live site), deploying rakumux.com, and a \
             Rakumux release for machines to start sending subagents.",
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
            &[
                "The mid-turn guard check for older servers, plus a long tail of words to wrap",
                "Revoking the terminal token",
                "Honest wording",
            ],
            true,
        );
        let mut dialog = driver(
            vec![subagents, fixes],
            vec![
                answer(&[2], None),
                answer(&[0], Some("Also the FIFO freeze fix")),
            ],
        );
        assert!(dialog.shown(&screen("long-q1")));
        assert_eq!(dialog.next(&screen("long-q1")), Next::Keys(vec!["3"]));
        assert_eq!(dialog.next(&screen("long-q2")), Next::Keys(vec!["1"]));
        // Ticked, its label is cut short: still the option.
        assert_eq!(
            dialog.next(&screen("long-q2-text")),
            Next::Keys(vec!["enter"])
        );
        assert_eq!(dialog.next(&screen("idle")), Next::Done);
    }

    #[test]
    fn a_narrow_screen_cuts_the_footer_and_may_hide_options() {
        // 56 columns: the footer is cut short, the page still read.
        let mut dialog = driver(
            vec![colour(), fruits(), pet()],
            vec![answer(&[1], None), answer(&[2], None), answer(&[0], None)],
        );
        assert!(dialog.shown(&screen("narrow-q2")));
        assert_eq!(dialog.next(&screen("narrow-q2")), Next::Keys(vec!["left"]));
        // A long question there shows two of its three options: not the
        // question as asked, so nothing is typed.
        let subagents = question(
            "Ship subagent activity? It needs: migration 0024 on production (adds nullable \
             columns and an index; safe under the live site), deploying rakumux.com, and a \
             Rakumux release for machines to start sending subagents.",
            &[
                "Ship web now, Rakumux in 0.1.35 (Recommended)",
                "Ship everything now",
                "Hold",
            ],
            false,
        );
        let View::Question(page) = read(&screen("narrow-long-q1")) else {
            panic!("not a question page");
        };
        assert_eq!(page.options.len(), 2);
        assert!(!driver(vec![subagents], vec![answer(&[0], None)]).shown(&screen("narrow-long-q1")));
    }

    #[test]
    fn a_step_is_seen_taken_only_when_the_dialog_moved() {
        let dialog = dialog_of_green();
        assert!(dialog.moved(&screen("one-question"), &screen("one-question-z")));
        let before = screen("one-question");
        let after = before.replacen("0.0s", "0.4s", 1);
        assert!(!dialog.moved(&before, &after));
    }
}
