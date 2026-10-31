//! OpenCode's questions (its `question` tool, captured 1.18.16 and 2.2.0),
//! read from its database, and its dialog, worked from its screen
//! (`numbered.rs`).
//!
//! The newest `question` call of the conversation is its question: OpenCode
//! 2 keeps it as a `tool` block of an `assistant` row of `session_message`,
//! 1.x as a `tool` row of `part`; either holds the tool's `state` (`input`:
//! `questions`, each a `question` with a `header`, `options` of `label`s and
//! `multiple`; `status`, `completed` once answered, `error` once dismissed).
//!
//! The dialog sits on the left bar (`┃`) over the input box: a tab per
//! question's header then `Confirm` (2: `Submit`; no tabs for one question
//! taking one choice), the question (1.x adds `(select all that apply)` to
//! one that takes several), its options numbered (`1. Red`, its
//! description under it, `2. Green ✓` once chosen; `1. [ ] Apple`/`[✓]` for
//! one that takes several), then `Type your own answer`, the free answer,
//! and a hint ending `esc dismiss` (`esc close` while 2's box is open). Keys,
//! as checked against the real OpenCode:
//! - **One choice:** its digit picks it and moves on (with one question,
//!   answers the dialog).
//! - **Several:** each digit ticks or unticks its box; Tab moves on.
//! - **Free answer:** the number after the options opens its box (1.x under
//!   the row, 2 in its place), the text is typed, and Enter saves it (and
//!   moves on from a question taking one choice).
//! - **Review:** after the last question, each `Header: answer` (1.x under
//!   `Review`): Enter submits.

use super::numbered::{Free, Mark, Page, Row, Style, View};
use super::{listed, squash, Logged, OpenQuestion, Question};
use serde_json::Value;
use std::path::Path;

/// The tool OpenCode asks the user with.
const TOOL: &str = "question";
/// The free answer's row, and its box while empty.
const PLACEHOLDER: &str = "typeyourownanswer";
/// What 1.x adds to a question that takes several.
const SEVERAL: &str = "selectallthatapply";
/// The newest OpenCode 2 replies, or 1.x pieces, looked at for the newest
/// question: one that waits for its answer is among the newest.
const RECENT_REPLIES: usize = 8;
const RECENT_PARTS: usize = 64;

/// What `conversation` says about its questions: its newest `question`
/// call, then its result when it has one. Nothing when it asked none
/// lately.
pub(crate) fn logged(database: &Path, conversation: &str) -> Vec<Logged> {
    let Some(connection) = crate::resume::open_read_only(database) else {
        return Vec::new();
    };
    let Some((call_id, state)) =
        newest_v2(&connection, conversation).or_else(|| newest_v1(&connection, conversation))
    else {
        return Vec::new();
    };
    let questions = state
        .get("input")
        .and_then(|input| input.get("questions"))
        .and_then(listed)
        .unwrap_or_default();
    let ended = matches!(
        state.get("status").and_then(Value::as_str),
        Some("completed" | "error")
    );
    let mut said = vec![Logged::Asked(OpenQuestion {
        call_id: call_id.clone(),
        questions,
    })];
    if ended {
        said.push(Logged::Answered(call_id));
    }
    said
}

/// OpenCode 2: the newest `question` block of the newest replies, its id
/// and state.
fn newest_v2(connection: &rusqlite::Connection, conversation: &str) -> Option<(String, Value)> {
    let mut statement = connection
        .prepare(&format!(
            "SELECT data FROM session_message WHERE session_id = ?1 AND type = 'assistant' \
             ORDER BY seq DESC LIMIT {RECENT_REPLIES}"
        ))
        .ok()?;
    let rows = statement
        .query_map([conversation], |row| row.get::<_, String>(0))
        .ok()?;
    let found = rows
        .filter_map(Result::ok)
        .filter_map(|data| serde_json::from_str::<Value>(&data).ok())
        .find_map(|reply| {
            reply
                .get("content")?
                .as_array()?
                .iter()
                .rev()
                .find_map(|block| {
                    let field = |key: &str| block.get(key).and_then(Value::as_str);
                    (field("type")? == "tool" && field("name")? == TOOL)
                        .then(|| Some((field("id")?.to_owned(), block.get("state")?.clone())))?
                })
        });
    found
}

/// OpenCode 1.x: the newest `question` piece of the newest pieces, its
/// call id and state.
fn newest_v1(connection: &rusqlite::Connection, conversation: &str) -> Option<(String, Value)> {
    let mut statement = connection
        .prepare(&format!(
            "SELECT data FROM part WHERE session_id = ?1 \
             ORDER BY time_created DESC, id DESC LIMIT {RECENT_PARTS}"
        ))
        .ok()?;
    let rows = statement
        .query_map([conversation], |row| row.get::<_, String>(0))
        .ok()?;
    let found = rows
        .filter_map(Result::ok)
        .filter_map(|data| serde_json::from_str::<Value>(&data).ok())
        .find_map(|part| {
            let field = |key: &str| part.get(key).and_then(Value::as_str);
            (field("type")? == "tool" && field("tool")? == TOOL)
                .then(|| Some((field("callID")?.to_owned(), part.get("state")?.clone())))?
        });
    found
}

/// How OpenCode's dialog takes keys.
pub(super) static STYLE: Style = Style {
    read,
    free_key: None,
    leave: "tab",
    free_leaves: false,
    submit: "enter",
    back: "left",
    reviews,
};

/// Whether the dialog reviews its answers: it does unless it asks one
/// question taking one choice.
fn reviews(questions: &[Question]) -> bool {
    questions.len() > 1 || questions.iter().any(|question| question.multi_select)
}

/// A numbered row's start: `1. Red`, `2. Green ✓`, `1. [ ] Apple`,
/// `1. [✓] Apple`; its number, mark and label.
fn row(line: &str) -> Option<(usize, Mark, &str)> {
    let digits = line.find(|c: char| !c.is_ascii_digit())?;
    let number = line[..digits].parse().ok()?;
    let label = line[digits..].strip_prefix(". ")?;
    if let Some(label) = label.strip_prefix("[ ] ") {
        return Some((number, Mark::Box(false), label));
    }
    if let Some(label) = label.strip_prefix("[✓] ") {
        return Some((number, Mark::Box(true), label));
    }
    match label.strip_suffix(" ✓") {
        Some(label) => Some((number, Mark::Radio(true), label)),
        None => Some((number, Mark::None, label)),
    }
}

/// OpenCode's question dialog as its screen shows it.
pub(super) fn read(screen: &str) -> View {
    let lines: Vec<&str> = screen.lines().map(str::trim_end).collect();
    // A bar line's text, once its two columns of margin are left out.
    let content = |line: &str| -> Option<String> {
        let rest = line.trim_start().strip_prefix('┃')?;
        Some(
            rest.strip_prefix("  ")
                .unwrap_or(rest.trim_start())
                .to_owned(),
        )
    };
    // The hint, the dialog's last line of text, and the bar above it.
    let Some(hint) = lines.iter().rposition(|line| {
        content(line)
            .is_some_and(|text| text.ends_with("esc dismiss") || text.ends_with("esc close"))
    }) else {
        return View::Gone;
    };
    let start = lines[..hint]
        .iter()
        .rposition(|line| content(line).is_none())
        .map_or(0, |at| at + 1);
    let body: Vec<String> = lines[start..hint]
        .iter()
        .filter_map(|line| content(line))
        .collect();
    let hint = content(lines[hint]).unwrap_or_default();
    match body.iter().position(|line| row(line).is_some()) {
        Some(first) => {
            page(&body, first, hint.ends_with("esc close")).map_or(View::Gone, View::Question)
        }
        None if hint.contains("enter submit") && !hint.contains("select") => {
            View::Review(review(&body))
        }
        None => View::Gone,
    }
}

/// The review: each question's `Header: answer`, its answer wrapped onto
/// the lines under it.
fn review(body: &[String]) -> Vec<(String, String)> {
    let mut answers: Vec<(String, String)> = Vec::new();
    for line in body {
        let text = line.trim();
        if text.is_empty() {
            continue;
        }
        match text.split_once(": ") {
            Some((header, answer)) => answers.push((squash(header), squash(answer))),
            None => {
                // `Questions`, the tabs and `Review` come before any answer.
                if let Some((_, answer)) = answers.last_mut() {
                    answer.push_str(&squash(text));
                }
            }
        }
    }
    answers
}

/// A question's page: the question (the lines over its rows, the tabs and
/// `Questions` before it), its rows, the free answer last. `editing`: the
/// hint says OpenCode 2's box is open.
fn page(body: &[String], first: usize, editing: bool) -> Option<Page> {
    let mut over = body[..first]
        .iter()
        .rev()
        .skip_while(|line| line.trim().is_empty());
    let mut question: Vec<&str> = over
        .by_ref()
        .take_while(|line| !line.trim().is_empty())
        .map(|line| line.trim())
        .collect();
    question.reverse();
    let mut question = squash(&question.join(" "));
    // The rows, each line under one its own: a description, a wrapped label,
    // and under the free answer 1.x's box.
    let mut rows: Vec<(Mark, String, Vec<String>)> = Vec::new();
    for line in &body[first..] {
        if line.trim().is_empty() {
            continue;
        }
        match row(line) {
            Some((number, mark, label)) => {
                if number != rows.len() + 1 {
                    return None;
                }
                rows.push((mark, label.trim().to_owned(), Vec::new()));
            }
            None if line.starts_with(' ') => rows.last_mut()?.2.push(line.trim().to_owned()),
            None => return None,
        }
    }
    let (mark, label, under) = rows.pop()?;
    let boxes = rows.iter().any(|(mark, ..)| matches!(mark, Mark::Box(_)));
    if boxes {
        question = question
            .strip_suffix(SEVERAL)
            .map(str::to_owned)
            .unwrap_or(question);
    }
    let ticked = matches!(mark, Mark::Box(true) | Mark::Radio(true));
    let typed = |text: &str| {
        if squash(text) == PLACEHOLDER {
            String::new()
        } else {
            text.to_owned()
        }
    };
    let free = match under.as_slice() {
        // 1.x: its box under the row, saved once its box is ticked.
        [text] if squash(&label) == PLACEHOLDER => match mark {
            Mark::Box(true) => Free::Given(typed(text)),
            _ => Free::Editing(typed(text)),
        },
        [] if editing => Free::Editing(typed(&label)),
        [] if squash(&label) == PLACEHOLDER => Free::Idle { ticked },
        [] => Free::Given(label),
        _ => return None,
    };
    let options = rows
        .into_iter()
        .map(|(mark, label, under)| {
            let first = squash(&label);
            Row {
                words: format!("{first}{}", squash(&under.join(" "))),
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
        leaves: !editing,
    })
}

#[cfg(test)]
mod tests {
    use super::super::numbered::Dialog;
    use super::super::{Answer, Next};
    use super::*;

    /// Screens captured from OpenCode 1.18.16 (`v1-…`) and 2.2.0 (`v2-…`)
    /// against a scripted model, in `integrations/open-code/fixtures/questions/`.
    fn screen(name: &str) -> String {
        let path = format!(
            "{}/../../../integrations/open-code/fixtures/questions/{name}.txt",
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

    /// A database with both layouts' tables, as OpenCode keeps them.
    fn database(dir: &Path) -> (std::path::PathBuf, rusqlite::Connection) {
        let path = dir.join("opencode.db");
        let connection = rusqlite::Connection::open(&path).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE session_message (id TEXT, session_id TEXT, seq INTEGER, \
                 type TEXT, time_updated INTEGER, data TEXT); \
                 CREATE TABLE part (id TEXT, message_id TEXT, session_id TEXT, \
                 time_created INTEGER, time_updated INTEGER, data TEXT);",
            )
            .unwrap();
        (path, connection)
    }

    const INPUT: &str = r#"{"questions":[{"question":"Which colour do you prefer?","header":"Colour","options":[{"label":"Red","description":"Warm"},{"label":"Green","description":"green"},{"label":"Blue","description":"Cool"}],"multiple":false}]}"#;

    #[test]
    fn the_database_says_which_question_is_asked_and_when_it_has_its_result() {
        let dir = tempfile::tempdir().unwrap();
        let (path, db) = database(dir.path());
        let asked = |call_id: &str| {
            vec![Logged::Asked(OpenQuestion {
                call_id: call_id.into(),
                questions: vec![colour()],
            })]
        };
        // OpenCode 2: a reply's tool block, running until answered.
        let reply = |status: &str| {
            format!(
                r#"{{"content":[{{"type":"reasoning","text":"I should ask."}},{{"type":"tool","id":"call_2","name":"question","state":{{"status":"{status}","input":{INPUT}}}}}]}}"#
            )
        };
        db.execute(
            "INSERT INTO session_message VALUES ('msg_1', 'ses_2', 5, 'assistant', 1, ?1)",
            [reply("running")],
        )
        .unwrap();
        assert_eq!(logged(&path, "ses_2"), asked("call_2"));
        db.execute(
            "UPDATE session_message SET data = ?1 WHERE id = 'msg_1'",
            [reply("completed")],
        )
        .unwrap();
        let mut answered = asked("call_2");
        answered.push(Logged::Answered("call_2".into()));
        assert_eq!(logged(&path, "ses_2"), answered);
        // OpenCode 1.x: a tool piece, dismissed (`error`) or answered.
        let part = |status: &str| {
            format!(
                r#"{{"type":"tool","tool":"question","callID":"call_1","state":{{"status":"{status}","input":{INPUT}}}}}"#
            )
        };
        db.execute(
            "INSERT INTO part VALUES ('prt_1', 'msg_9', 'ses_1', 10, 10, ?1)",
            [part("running")],
        )
        .unwrap();
        db.execute(
            "INSERT INTO part VALUES ('prt_2', 'msg_9', 'ses_1', 11, 11, ?1)",
            [r#"{"type":"step-finish"}"#],
        )
        .unwrap();
        assert_eq!(logged(&path, "ses_1"), asked("call_1"));
        db.execute(
            "UPDATE part SET data = ?1 WHERE id = 'prt_1'",
            [part("error")],
        )
        .unwrap();
        let mut dismissed = asked("call_1");
        dismissed.push(Logged::Answered("call_1".into()));
        assert_eq!(logged(&path, "ses_1"), dismissed);
        // A conversation that asked nothing.
        assert!(logged(&path, "ses_3").is_empty());
    }

    #[test]
    fn a_question_page_is_read_with_its_options_and_free_answer() {
        for name in ["v1-one-question", "v2-one-question"] {
            let View::Question(page) = read(&screen(name)) else {
                panic!("{name}: not a question page");
            };
            assert_eq!(page.question, "whichcolourdoyouprefer", "{name}");
            assert_eq!(page.options.len(), 3, "{name}");
            assert!(
                page.options[0].is("Red") && page.options[2].is("Blue"),
                "{name}"
            );
            assert_eq!(page.free, Free::Idle { ticked: false }, "{name}");
            assert!(driver(vec![colour()], vec![answer(&[1], None)]).shown(&screen(name)));
            assert!(!driver(vec![pet()], vec![answer(&[0], None)]).shown(&screen(name)));
        }
    }

    #[test]
    fn a_permission_prompt_and_the_prompt_box_are_no_dialog() {
        for name in ["v1-permission", "v2-permission", "v1-idle", "v2-idle"] {
            assert_eq!(read(&screen(name)), View::Gone, "{name}");
        }
    }

    #[test]
    fn one_choice_is_its_digit_which_answers_one_question() {
        for (question, idle) in [
            ("v1-one-question", "v1-idle"),
            ("v2-one-question", "v2-idle"),
        ] {
            let mut dialog = driver(vec![colour()], vec![answer(&[1], None)]);
            assert_eq!(dialog.next(&screen(question)), Next::Keys(vec!["2"]));
            assert_eq!(dialog.next(&screen(idle)), Next::Done);
        }
    }

    #[test]
    fn a_free_answer_is_typed_in_its_box_and_saved_with_enter() {
        for v in ["v1", "v2"] {
            let at = |name: &str| screen(&format!("{v}-{name}"));
            let mut dialog = driver(vec![colour()], vec![answer(&[], Some("Purple, 2 shades"))]);
            assert_eq!(dialog.next(&at("one-question")), Next::Keys(vec!["4"]));
            assert_eq!(
                dialog.next(&at("one-other")),
                Next::Type("Purple, 2 shades".into()),
                "{v}"
            );
            assert_eq!(
                dialog.next(&at("one-typed")),
                Next::Keys(vec!["enter"]),
                "{v}"
            );
            assert_eq!(dialog.next(&at("idle")), Next::Done, "{v}");
        }
    }

    #[test]
    fn several_questions_are_answered_in_turn_then_reviewed_and_submitted() {
        for v in ["v1", "v2"] {
            let at = |name: &str| screen(&format!("{v}-{name}"));
            let mut dialog = driver(
                vec![colour(), fruits(), pet()],
                vec![
                    answer(&[1], None),
                    answer(&[0], Some("Mango, ripe")),
                    answer(&[0], None),
                ],
            );
            assert_eq!(dialog.next(&at("multi-q1")), Next::Keys(vec!["2"]), "{v}");
            assert_eq!(dialog.next(&at("multi-q2")), Next::Keys(vec!["1"]), "{v}");
            assert_eq!(
                dialog.next(&at("multi-q2-apple")),
                Next::Keys(vec!["4"]),
                "{v}"
            );
            assert_eq!(
                dialog.next(&at("multi-q2-other")),
                Next::Type("Mango, ripe".into()),
                "{v}"
            );
            assert_eq!(
                dialog.next(&at("multi-q2-typed")),
                Next::Keys(vec!["enter"]),
                "{v}"
            );
            assert_eq!(
                dialog.next(&at("multi-q2-saved")),
                Next::Keys(vec!["tab"]),
                "{v}"
            );
            assert_eq!(dialog.next(&at("multi-q3")), Next::Keys(vec!["1"]), "{v}");
            assert_eq!(
                dialog.next(&at("multi-review")),
                Next::Keys(vec!["enter"]),
                "{v}"
            );
            assert_eq!(dialog.next(&at("idle")), Next::Done, "{v}");
        }
    }

    #[test]
    fn a_chosen_option_and_a_page_further_on() {
        // Going back shows the choice made (`2. Green ✓`): still the question.
        let View::Question(page) = read(&screen("v1-back")) else {
            panic!("not a question page");
        };
        assert_eq!(page.options[1].mark, Mark::Radio(true));
        assert!(page.options[1].is("Green"));
        // A page further on goes back to the question due.
        let mut dialog = driver(
            vec![colour(), fruits(), pet()],
            vec![answer(&[1], None), answer(&[0], None), answer(&[0], None)],
        );
        assert_eq!(
            dialog.next(&screen("v2-multi-q3")),
            Next::Keys(vec!["left"])
        );
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
        for v in ["v1", "v2"] {
            let at = |name: &str| screen(&format!("{v}-{name}"));
            let mut dialog = driver(
                vec![subagents.clone(), fixes.clone()],
                vec![
                    answer(&[2], None),
                    answer(&[0], Some("Also the FIFO freeze fix")),
                ],
            );
            assert!(dialog.shown(&at("long-q1")), "{v}");
            assert_eq!(dialog.next(&at("long-q1")), Next::Keys(vec!["3"]), "{v}");
            assert_eq!(dialog.next(&at("long-q2")), Next::Keys(vec!["1"]), "{v}");
            assert_eq!(
                dialog.next(&at("long-q2-text")),
                Next::Keys(vec!["enter"]),
                "{v}"
            );
            assert!(dialog.shown(&at("long-review")), "{v}");
        }
    }
}
