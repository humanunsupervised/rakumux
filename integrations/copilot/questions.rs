//! GitHub Copilot CLI's questions (`ask_user`, captured 1.0.105), read from
//! its event log, and its dialog, worked from its screen.
//!
//! `ask_user` asks with a form: a `message` and a `requestedSchema` whose
//! `properties` are its fields. Each field is a choice (`enum`, or `oneOf`
//! of `{const, title}`), several choices (`array` of those), yes or no
//! (`boolean`), or text (`string`, `number`, `integer`). Every field is a
//! question: a form of one asks its message (its field's title the label);
//! a form of several asks each field by its title (else its description,
//! else its name), as rakumux.com reads them too. A text field with a
//! `default` shows it typed in; such a form isn't answered here.
//!
//! The dialog sits between two rules: `Copilot needs information.`, the
//! message, the bar of fields (a form of several), then one field at a
//! time: its label (its title, else its name; `Colour:` for choices), its
//! description, and its rows: options (`❯` marks the cursor; several
//! choices carry `✓`/`•`) and `Other (type your answer)`, whose text goes in
//! a box drawn under the dialog once the cursor is on it; or, for text, a box
//! of its own (`❯ text`). The hint under it ends `ctrl+d decline · esc
//! cancel`. Keys, as checked against the real Copilot:
//! - **One choice** (yes or no too): `down`/`up` to the option (or to Other,
//!   then the text), `enter`.
//! - **Several choices:** `down`/`up` to each option to change, `space`
//!   ticks or unticks it; the free answer is typed with the cursor on Other
//!   (a `space` there would be typed); `enter` from any row.
//! - **Text:** typed, `enter`.
//!
//! `enter` moves to the next field, and on the last one gives the answers:
//! there's no review. Its result says `User responded: …`. Every step is
//! planned from the screen just read; the driver stops when the screen shows
//! anything it doesn't expect: another form, a field or option it doesn't
//! know, text someone else typed, or a field already answered.

use super::{squash, Answer, Logged, Next, OpenQuestion, Question};
use serde_json::{Map, Value};

/// The tool Copilot asks the user with.
const TOOL: &str = "ask_user";
/// The dialog's first line.
const TITLE: &str = "Copilot needs information.";
/// The free answer's row (`Other (type your answer)`, `… below` once the
/// cursor is on it), squashed.
const OTHER: &str = "othertypeyouranswer";
/// The free answer's box, empty.
const PLACEHOLDER: &str = "typeyouranswer";

/// What `entry` of Copilot's event log says about questions: an `ask_user`
/// call of the main agent (a subagent's events carry its `agentId`), or a
/// tool's result. A call whose form Rakumux can't answer is still the
/// newest question, with none to answer.
pub(super) fn logged(entry: &Value) -> Vec<Logged> {
    if entry.get("agentId").is_some_and(|agent| !agent.is_null()) {
        return Vec::new();
    }
    let Some(data) = entry.get("data") else {
        return Vec::new();
    };
    match entry.get("type").and_then(Value::as_str) {
        Some("assistant.message") => super::super::copilot::requests(data)
            .iter()
            .filter(|request| request.get("name").and_then(Value::as_str) == Some(TOOL))
            .filter_map(|request| {
                let call_id = request.get("toolCallId")?.as_str()?.to_owned();
                let arguments = super::super::copilot::arguments(request.get("arguments"));
                let questions = form(&arguments).unwrap_or_default();
                Some(Logged::Asked(OpenQuestion { call_id, questions }))
            })
            .collect(),
        Some("tool.execution_complete") => data
            .get("toolCallId")
            .and_then(Value::as_str)
            .map(|call_id| Logged::Answered(call_id.to_owned()))
            .into_iter()
            .collect(),
        _ => Vec::new(),
    }
}

/// A text that says something.
fn some_text(value: Option<&Value>) -> Option<&str> {
    value
        .and_then(Value::as_str)
        .filter(|text| !text.trim().is_empty())
}

/// The form's fields as questions, in the order the log keeps them (by
/// name, as rakumux.com reads them too); `header` is the label the dialog
/// shows. None when one isn't a field Rakumux answers.
fn form(arguments: &Value) -> Option<Vec<Question>> {
    if arguments
        .get("mode")
        .and_then(Value::as_str)
        .is_some_and(|mode| mode != "form")
    {
        return None;
    }
    let properties = arguments
        .get("requestedSchema")?
        .get("properties")?
        .as_object()?;
    let fields: Vec<(&String, &Map<String, Value>)> = properties
        .iter()
        .filter_map(|(name, field)| Some((name, field.as_object()?)))
        .collect();
    let only = match fields.len() {
        0 => return None,
        1 => some_text(arguments.get("message")),
        _ => None,
    };
    fields
        .into_iter()
        .map(|(name, field)| {
            let title = some_text(field.get("title"));
            let (options, multi_select) = kind(field)?;
            Some(Question {
                question: only
                    .or(title)
                    .or_else(|| some_text(field.get("description")))
                    .unwrap_or(name)
                    .to_owned(),
                header: Some(title.unwrap_or(name).to_owned()),
                options,
                multi_select,
            })
        })
        .collect()
}

/// A field's options and whether it takes several; none for a field this
/// driver doesn't answer: text shown typed in already (a `default`),
/// choices other than the tool's own (`anyOf` for one, any for a number),
/// or another kind.
fn kind(field: &Map<String, Value>) -> Option<(Vec<String>, bool)> {
    let defaulted = field
        .get("default")
        .is_some_and(|value| !matches!(value, Value::Null) && value.as_str() != Some(""));
    match field.get("type").and_then(Value::as_str)? {
        "boolean" => Some((vec!["Yes".into(), "No".into()], false)),
        "array" => {
            let options = choices(field.get("items")?.as_object()?);
            (!options.is_empty()).then_some((options, true))
        }
        "string" if field.contains_key("enum") || field.contains_key("oneOf") => {
            let options = choices(field);
            (!options.is_empty()).then_some((options, false))
        }
        // Choices of another kind than the tool's own are left alone.
        _ if ["enum", "oneOf", "anyOf"]
            .iter()
            .any(|key| field.contains_key(*key)) =>
        {
            None
        }
        "string" | "number" | "integer" if !defaulted => Some((Vec::new(), false)),
        _ => None,
    }
}

/// Choices' labels: `enum`'s values (named by `enumNames`), or `oneOf`'s
/// (or `anyOf`'s) titles, else their values.
fn choices(field: &Map<String, Value>) -> Vec<String> {
    let value = |value: &Value| match value {
        Value::String(text) => Some(text.clone()),
        Value::Number(number) => Some(number.to_string()),
        _ => None,
    };
    if let Some(values) = field.get("enum").and_then(Value::as_array) {
        let names = field.get("enumNames").and_then(Value::as_array);
        return values
            .iter()
            .enumerate()
            .filter_map(|(at, item)| {
                let name = names.and_then(|names| some_text(names.get(at)));
                Some(name.map(String::from).or_else(|| value(item))?)
            })
            .collect();
    }
    let listed = field
        .get("oneOf")
        .or_else(|| field.get("anyOf"))
        .and_then(Value::as_array);
    listed
        .into_iter()
        .flatten()
        .filter_map(|choice| {
            let constant = value(choice.get("const")?)?;
            Some(
                some_text(choice.get("title"))
                    .map(String::from)
                    .unwrap_or(constant),
            )
        })
        .collect()
}

/// A rule across the screen.
fn is_rule(line: &str) -> bool {
    let line = line.trim();
    line.chars().count() >= 8 && line.chars().all(|c| c == '─')
}

/// A row of a field of choices.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Row {
    /// `✓` or `•`, for a field of several choices.
    checked: Option<bool>,
    /// Its label, wrapped lines and all, squashed.
    words: String,
}

/// What a field's page shows.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Field {
    /// Options and Other, the row the cursor is on, and what the box under
    /// the dialog holds, squashed, while it shows (empty: the placeholder).
    Choices {
        rows: Vec<Row>,
        cursor: Option<usize>,
        other: Option<String>,
    },
    /// Text: what its box holds.
    Text(String),
}

/// What the screen shows of the dialog: the text above the field (its
/// title, message and bar of fields), squashed, the field's label,
/// squashed, and the field.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Page {
    above: String,
    /// The paragraph just above the field, squashed: a form of several's
    /// bar of fields, its labels in the dialog's order.
    bar: String,
    label: String,
    field: Field,
}

/// What a box shows: the text after its `❯` or `┃`, squashed.
fn boxed(lines: &[&str], mark: char) -> Option<String> {
    let text: Vec<&str> = lines
        .iter()
        .map(|line| line.trim().strip_prefix(mark).map(str::trim))
        .collect::<Option<_>>()?;
    let text = squash(&text.join(" "));
    Some(if text == PLACEHOLDER {
        String::new()
    } else {
        text
    })
}

impl Page {
    fn read(screen: &str) -> Option<Self> {
        let lines: Vec<&str> = screen.lines().map(str::trim_end).collect();
        let title = lines.iter().rposition(|line| line.trim() == TITLE)?;
        if !lines[..title].last().is_some_and(|line| is_rule(line)) {
            return None;
        }
        let bottom = title + lines[title..].iter().position(|line| is_rule(line))?;
        let mut paragraphs: Vec<Vec<&str>> = vec![Vec::new()];
        for line in &lines[title..bottom] {
            if line.trim().is_empty() {
                if paragraphs.last().is_some_and(|p| !p.is_empty()) {
                    paragraphs.push(Vec::new());
                }
            } else if let Some(paragraph) = paragraphs.last_mut() {
                paragraph.push(line);
            }
        }
        if paragraphs.last().is_some_and(Vec::is_empty) {
            paragraphs.pop();
        }
        let hint = paragraphs
            .pop()?
            .iter()
            .map(|line| line.trim())
            .collect::<Vec<_>>()
            .join(" ")
            .to_lowercase();
        if !hint.contains("enter accept") || !hint.contains("esc cancel") {
            return None;
        }
        let field = paragraphs.pop()?;
        let above = squash(&paragraphs.concat().join(" "));
        let bar = paragraphs
            .last()
            .map(|bar| squash(&bar.join(" ")))
            .unwrap_or_default();
        let (label, rest) = field.split_first()?;
        let label = squash(label.trim().trim_end_matches(':'));
        // A text field: its box (`▄▄▄`, `❯ text`, `▀▀▀`), under its description.
        if let Some(top) = rest.iter().position(|line| line.trim().starts_with('▄')) {
            let input = &rest[top + 1..];
            let (end, input) = input.split_last()?;
            if !end.trim().starts_with('▀') {
                return None;
            }
            return Some(Page {
                above,
                bar,
                label,
                field: Field::Text(boxed(input, '❯')?),
            });
        }
        let mut rows: Vec<Row> = Vec::new();
        let mut cursor = None;
        for line in rest {
            let start = line
                .strip_prefix(" ❯ ")
                .map(|label| (true, label))
                .or_else(|| line.strip_prefix("   ").map(|label| (false, label)));
            match start {
                Some((on, label)) if !label.starts_with(' ') => {
                    if on {
                        cursor = Some(rows.len());
                    }
                    let (checked, label) = if let Some(label) = label.strip_prefix("✓ ") {
                        (Some(true), label)
                    } else if let Some(label) = label.strip_prefix("• ") {
                        (Some(false), label)
                    } else {
                        (None, label)
                    };
                    rows.push(Row {
                        checked,
                        words: squash(label),
                    });
                }
                // A label's wrapped line, or (before the rows) the description.
                _ => {
                    if let Some(row) = rows.last_mut() {
                        row.words.push_str(&squash(line));
                    }
                }
            }
        }
        if !rows.last()?.words.starts_with(OTHER) {
            return None;
        }
        // The box under the dialog, while Other has the cursor.
        let below = &lines[bottom + 1..];
        let other = match below.first() {
            Some(top) if top.trim().starts_with('╻') => {
                let end = below.iter().position(|line| line.trim().starts_with('╹'))?;
                Some(boxed(&below[1..end], '┃')?)
            }
            _ => None,
        };
        Some(Page {
            above,
            bar,
            label,
            field: Field::Choices {
                rows,
                cursor,
                other,
            },
        })
    }
}

/// Answering one `ask_user` form, a field at a time, in the order the
/// dialog shows them.
#[derive(Debug)]
pub(super) struct Dialog {
    questions: Vec<Question>,
    answers: Vec<Answer>,
    /// The fields answered so far (the dialog moved on from them).
    answered: Vec<bool>,
    /// The last step left this field's page (its `enter`).
    leaving: Option<usize>,
    /// The last step was the `enter` that gives the answers.
    submitting: bool,
}

impl Dialog {
    pub(super) fn new(questions: Vec<Question>, answers: Vec<Answer>) -> Self {
        Self {
            answered: vec![false; questions.len()],
            questions,
            answers,
            leaving: None,
            submitting: false,
        }
    }

    /// The field a page shows, when it's one of this form's: its label,
    /// rows and kind are that field's, and the text above names the form (a
    /// form of one: its message; of several: every field's label, its bar).
    fn which(&self, page: &Page) -> Option<usize> {
        let label = |question: &Question| squash(question.header.as_deref().unwrap_or_default());
        let form = if self.questions.len() == 1 {
            self.questions
                .iter()
                .all(|question| page.above.contains(&squash(&question.question)))
        } else {
            self.questions
                .iter()
                .all(|question| page.above.contains(&label(question)))
        };
        if !form {
            return None;
        }
        let mut fits = self.questions.iter().enumerate().filter(|(_, asked)| {
            label(asked) == page.label
                && match &page.field {
                    Field::Text(_) => asked.options.is_empty(),
                    Field::Choices { rows, .. } => {
                        rows.len() == asked.options.len() + 1
                            && asked
                                .options
                                .iter()
                                .zip(rows)
                                .all(|(option, row)| row.words == squash(option))
                            && rows
                                .iter()
                                .all(|row| row.checked.is_some() == asked.multi_select)
                    }
                }
        });
        // Two fields alike: no telling which.
        match (fits.next(), fits.next()) {
            (Some((index, _)), None) => Some(index),
            _ => None,
        }
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
                Some(_) => Next::Stop("the dialog stayed open after its answers were given"),
            };
        }
        if let Some(left) = self.leaving.take() {
            if page.as_ref().and_then(|page| self.which(page)) == Some(left) {
                return Next::Stop("the dialog stayed on the field");
            }
            self.answered[left] = true;
        }
        let Some(page) = page else {
            return Next::Stop("no question dialog on screen");
        };
        let Some(shown) = self.which(&page) else {
            return Next::Stop("another dialog is on screen");
        };
        if self.answered[shown] {
            return Next::Stop("the dialog went back to an answered field");
        }
        // A form of several is answered from its first field on, so its last
        // field's enter comes after every other: never from a field someone
        // moved to in the session (their answers before it would stay).
        let first = !self.answered.contains(&true);
        if first && self.questions.len() > 1 && !page.bar.starts_with(&page.label) {
            return Next::Stop("the dialog isn't on its first field");
        }
        self.plan(shown, &page.field)
    }

    /// Leave field `index`'s page with `enter`: the next field's, or, from
    /// the last one left, the answers given.
    fn leave(&mut self, index: usize) -> Next {
        let rest = self
            .answered
            .iter()
            .enumerate()
            .filter(|(at, answered)| *at != index && !**answered)
            .count();
        if rest == 0 {
            self.submitting = true;
        } else {
            self.leaving = Some(index);
        }
        Next::Keys(vec!["enter"])
    }

    /// The next step on field `index`'s page.
    fn plan(&mut self, index: usize, field: &Field) -> Next {
        let asked = &self.questions[index];
        let answer = self.answers[index].clone();
        // A box: empty, holding the answer's text, or someone else's.
        let typed = |shown: &str| {
            let text = squash(answer.text.as_deref().unwrap_or_default());
            if shown.is_empty() {
                Some(false)
            } else {
                (!text.is_empty() && (text.starts_with(shown) || shown.starts_with(&text)))
                    .then_some(true)
            }
        };
        let (rows, cursor, other) = match field {
            Field::Text(shown) => {
                let Some(text) = answer.text.clone() else {
                    return Next::Stop("no answer for the field");
                };
                return match typed(shown) {
                    Some(false) => Next::Type(text),
                    Some(true) => self.leave(index),
                    None => Next::Stop("the field holds other text"),
                };
            }
            Field::Choices {
                rows,
                cursor,
                other,
            } => (rows, *cursor, other.as_deref()),
        };
        let Some(cursor) = cursor else {
            return Next::Stop("no cursor in the dialog");
        };
        let free = asked.options.len();
        let to = |row: usize| match cursor.cmp(&row) {
            std::cmp::Ordering::Less => Some(Next::Keys(vec!["down"])),
            std::cmp::Ordering::Greater => Some(Next::Keys(vec!["up"])),
            std::cmp::Ordering::Equal => None,
        };
        // The free answer, typed in the box under the dialog once the cursor
        // is on Other: None when it's done.
        let type_free = |text: &str| match other.map(typed) {
            Some(Some(false)) => Some(Next::Type(text.to_owned())),
            Some(Some(true)) => None,
            Some(None) => Some(Next::Stop("the free answer holds other text")),
            None => Some(Next::Stop("no box for the free answer")),
        };
        if !asked.multi_select {
            let target = answer.choices.first().copied().unwrap_or(free);
            if let Some(step) = to(target) {
                return step;
            }
            if let (true, Some(text)) = (target == free, &answer.text) {
                if let Some(step) = type_free(text) {
                    return step;
                }
            }
            return self.leave(index);
        }
        // Several: each box as answered, the free answer typed, then `enter`.
        for (row, at) in rows.iter().zip(0..free) {
            if row.checked != Some(answer.choices.contains(&at)) {
                return to(at).unwrap_or(Next::Keys(vec!["space"]));
            }
        }
        if let Some(text) = &answer.text {
            if let Some(step) = to(free) {
                return step;
            }
            if let Some(step) = type_free(text) {
                return step;
            }
        }
        self.leave(index)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Screens captured from Copilot 1.0.105, in
    /// `integrations/copilot/fixtures/questions/`.
    fn screen(name: &str) -> String {
        let path = format!(
            "{}/../../../integrations/copilot/fixtures/questions/{name}.txt",
            env!("CARGO_MANIFEST_DIR")
        );
        std::fs::read_to_string(&path).unwrap_or_else(|error| panic!("{path}: {error}"))
    }

    fn question(text: &str, label: &str, options: &[&str], multi_select: bool) -> Question {
        Question {
            question: text.into(),
            header: Some(label.into()),
            options: options.iter().map(|o| o.to_string()).collect(),
            multi_select,
        }
    }

    fn colour() -> Question {
        question(
            "Which colour should the button be?",
            "Colour",
            &["red", "green", "blue"],
            false,
        )
    }

    fn fruits() -> Question {
        question(
            "Which fruits do you like?",
            "Fruits",
            &["apple", "banana", "cherry"],
            true,
        )
    }

    fn answer(choices: &[usize], text: Option<&str>) -> Answer {
        Answer {
            choices: choices.to_vec(),
            text: text.map(String::from),
        }
    }

    /// The form of four fields asked in the captures, by name: the log
    /// keeps them so, the dialog as asked (colour, fruits, ship, note).
    fn release() -> Vec<Question> {
        vec![
            question("Colour", "Colour", &["red", "green", "blue"], false),
            question("Fruits", "Fruits", &["apple", "banana", "cherry"], true),
            question("Note", "Note", &[], false),
            question("Ship today", "Ship today", &["Yes", "No"], false),
        ]
    }

    fn call(arguments: Value) -> Value {
        json!({"type": "assistant.message", "id": "e1", "parentId": null, "data": {
            "messageId": "m1", "content": "", "toolRequests": [
                {"toolCallId": "call_1", "name": "ask_user", "arguments": arguments, "type": "function"}]}})
    }

    #[test]
    fn the_log_says_which_question_is_asked_and_when_it_has_its_result() {
        let asked = call(json!({"message": "Which colour should the button be?",
            "requestedSchema": {"type": "object", "properties": {"colour": {"type": "string",
                "title": "Colour", "enum": ["red", "green", "blue"]}}, "required": ["colour"]}}));
        assert_eq!(
            logged(&asked),
            vec![Logged::Asked(OpenQuestion {
                call_id: "call_1".into(),
                questions: vec![colour()],
            })]
        );
        let result = json!({"type": "tool.execution_complete", "id": "e2", "data": {
            "toolCallId": "call_1", "success": true,
            "result": {"content": "User responded: green"}}});
        assert_eq!(logged(&result), vec![Logged::Answered("call_1".into())]);
        // A subagent's question is its own; another tool isn't a question.
        let mut subagent = asked.clone();
        subagent["agentId"] = json!("a1");
        assert!(logged(&subagent).is_empty());
        let bash = json!({"type": "assistant.message", "data": {"toolRequests": [
            {"toolCallId": "call_2", "name": "bash", "arguments": {"command": "ls"}}]}});
        assert!(logged(&bash).is_empty());
    }

    #[test]
    fn a_form_is_read_as_rakumux_com_reads_it() {
        let arguments = json!({"message": "A few choices for the release.", "requestedSchema": {
            "type": "object", "properties": {
                "ship": {"type": "boolean", "title": "Ship today"},
                "colour": {"type": "string", "title": "Colour", "enum": ["red", "green", "blue"]},
                "db": {"type": "string", "description": "Which database.", "enum": ["sqlite", "pg"],
                       "enumNames": ["SQLite", "Postgres"]},
                "auth": {"type": "string", "oneOf": [{"const": "oauth", "title": "OAuth"},
                                                      {"const": "token"}]},
                "targets": {"type": "array", "title": "Targets", "items": {"anyOf": [
                    {"const": "linux", "title": "Linux"}, {"const": "mac", "title": "macOS"}]},
                    "default": ["linux"]},
                "retries": {"type": "integer"}}}});
        let questions = form(&arguments).unwrap();
        let summary: Vec<(&str, &str, Vec<&str>, bool)> = questions
            .iter()
            .map(|q| {
                (
                    q.question.as_str(),
                    q.header.as_deref().unwrap(),
                    q.options.iter().map(String::as_str).collect(),
                    q.multi_select,
                )
            })
            .collect();
        assert_eq!(
            summary,
            vec![
                ("auth", "auth", vec!["OAuth", "token"], false),
                ("Colour", "Colour", vec!["red", "green", "blue"], false),
                ("Which database.", "db", vec!["SQLite", "Postgres"], false),
                ("retries", "retries", vec![], false),
                ("Ship today", "Ship today", vec!["Yes", "No"], false),
                ("Targets", "Targets", vec!["Linux", "macOS"], true),
            ]
        );
        // A form of one asks its message.
        let one = json!({"message": "What should it be called?", "requestedSchema": {
            "type": "object", "properties": {"name": {"type": "string", "title": "Name"}}}});
        assert_eq!(
            form(&one).unwrap(),
            vec![question("What should it be called?", "Name", &[], false)]
        );
        // Text shown typed in already, or a field of another kind: not answered here.
        for field in [
            json!({"type": "string", "title": "Name", "default": "octo"}),
            json!({"type": "number", "default": 5}),
            json!({"type": "string", "anyOf": [{"const": "a", "title": "A"}]}),
            json!({"type": "integer", "enum": [1, 2]}),
            json!({"type": "object"}),
            json!({"title": "No type"}),
        ] {
            let arguments = json!({"message": "?", "requestedSchema": {"type": "object",
                "properties": {"field": field}}});
            assert_eq!(form(&arguments), None, "{field}");
        }
        assert_eq!(
            form(&json!({"message": "Which?", "choices": ["a", "b"]})),
            None
        );
    }

    #[test]
    fn a_page_is_read_with_its_rows_cursor_and_box() {
        let page = Page::read(&screen("one")).expect("a field's page");
        assert_eq!(page.label, "colour");
        let Field::Choices {
            rows,
            cursor,
            other,
        } = &page.field
        else {
            panic!("not choices");
        };
        assert_eq!(rows.len(), 4);
        assert_eq!(*cursor, Some(0));
        assert_eq!(*other, None);
        let dialog = Dialog::new(vec![colour()], vec![answer(&[1], None)]);
        assert!(dialog.shown(&screen("one")));
        // Other options, or another message, aren't this dialog.
        let mut renamed = colour();
        renamed.options[2] = "teal".into();
        assert!(!Dialog::new(vec![renamed], vec![answer(&[0], None)]).shown(&screen("one")));
        let mut asked = colour();
        asked.question = "Which colour is the sky?".into();
        assert!(!Dialog::new(vec![asked], vec![answer(&[0], None)]).shown(&screen("one")));
        // The box under the dialog, once Other has the cursor.
        let Some(Page {
            field: Field::Choices { other, .. },
            ..
        }) = Page::read(&screen("one-other"))
        else {
            panic!("not choices");
        };
        assert_eq!(other.as_deref(), Some(""));
    }

    #[test]
    fn a_permission_prompt_and_the_prompt_box_are_no_dialog() {
        let dialog = Dialog::new(vec![colour()], vec![answer(&[0], None)]);
        for name in ["permission", "answered"] {
            assert_eq!(Page::read(&screen(name)), None, "{name}");
            assert!(!dialog.shown(&screen(name)), "{name}");
        }
    }

    #[test]
    fn one_choice_is_reached_with_down_and_taken_with_enter() {
        let mut dialog = Dialog::new(vec![colour()], vec![answer(&[1], None)]);
        assert_eq!(dialog.next(&screen("one")), Next::Keys(vec!["down"]));
        assert_eq!(dialog.next(&screen("one-green")), Next::Keys(vec!["enter"]));
        assert_eq!(dialog.next(&screen("answered")), Next::Done);
    }

    #[test]
    fn a_free_answer_is_typed_with_the_cursor_on_other() {
        let mut dialog = Dialog::new(vec![colour()], vec![answer(&[], Some("Purple, 2 shades"))]);
        assert_eq!(dialog.next(&screen("one")), Next::Keys(vec!["down"]));
        assert_eq!(dialog.next(&screen("one-green")), Next::Keys(vec!["down"]));
        assert_eq!(dialog.next(&screen("one-blue")), Next::Keys(vec!["down"]));
        assert_eq!(
            dialog.next(&screen("one-other")),
            Next::Type("Purple, 2 shades".into())
        );
        assert_eq!(
            dialog.next(&screen("one-other-typed")),
            Next::Keys(vec!["enter"])
        );
        // Someone else's text there is left alone.
        let mut other = Dialog::new(vec![colour()], vec![answer(&[], Some("Orange"))]);
        assert_eq!(
            other.next(&screen("one-other-typed")),
            Next::Stop("the free answer holds other text")
        );
    }

    #[test]
    fn several_choices_are_ticked_with_space_then_entered() {
        let mut dialog = Dialog::new(vec![fruits()], vec![answer(&[0, 2], Some("Mango"))]);
        assert_eq!(dialog.next(&screen("multi")), Next::Keys(vec!["space"]));
        assert_eq!(
            dialog.next(&screen("multi-apple")),
            Next::Keys(vec!["down"])
        );
        assert_eq!(
            dialog.next(&screen("multi-banana")),
            Next::Keys(vec!["down"])
        );
        assert_eq!(
            dialog.next(&screen("multi-cherry")),
            Next::Keys(vec!["space"])
        );
        assert_eq!(
            dialog.next(&screen("multi-cherry-ticked")),
            Next::Keys(vec!["down"])
        );
        assert_eq!(
            dialog.next(&screen("multi-other")),
            Next::Type("Mango".into())
        );
        assert_eq!(
            dialog.next(&screen("multi-other-typed")),
            Next::Keys(vec!["enter"])
        );
        assert_eq!(dialog.next(&screen("answered")), Next::Done);
    }

    #[test]
    fn a_form_is_answered_field_by_field_in_the_dialogs_order() {
        let mut dialog = Dialog::new(
            release(),
            vec![
                answer(&[1], None),
                answer(&[1], None),
                answer(&[], Some("Ship it on Friday")),
                answer(&[1], None),
            ],
        );
        assert!(dialog.shown(&screen("form-colour")));
        assert_eq!(
            dialog.next(&screen("form-colour")),
            Next::Keys(vec!["down"])
        );
        assert_eq!(
            dialog.next(&screen("form-colour-green")),
            Next::Keys(vec!["enter"])
        );
        assert_eq!(
            dialog.next(&screen("form-fruits")),
            Next::Keys(vec!["down"])
        );
        assert_eq!(
            dialog.next(&screen("form-fruits-banana")),
            Next::Keys(vec!["space"])
        );
        assert_eq!(
            dialog.next(&screen("form-fruits-ticked")),
            Next::Keys(vec!["enter"])
        );
        assert_eq!(dialog.next(&screen("form-ship")), Next::Keys(vec!["down"]));
        assert_eq!(
            dialog.next(&screen("form-ship-no")),
            Next::Keys(vec!["enter"])
        );
        assert_eq!(
            dialog.next(&screen("form-note")),
            Next::Type("Ship it on Friday".into())
        );
        // The last field: its enter gives the answers.
        assert_eq!(
            dialog.next(&screen("form-note-typed")),
            Next::Keys(vec!["enter"])
        );
        assert!(dialog.submitting);
        assert_eq!(dialog.next(&screen("answered")), Next::Done);
        // A form someone moved on in, in the session, isn't answered from there.
        let mut moved = Dialog::new(release(), vec![answer(&[1], None); 4]);
        assert_eq!(
            moved.next(&screen("form-fruits")),
            Next::Stop("the dialog isn't on its first field")
        );
        // A field the dialog went back to is never answered again.
        let mut back = Dialog::new(release(), vec![answer(&[1], None); 4]);
        back.answered[0] = true;
        assert_eq!(
            back.next(&screen("form-colour")),
            Next::Stop("the dialog went back to an answered field")
        );
    }

    #[test]
    fn long_messages_and_labels_wrap_on_a_narrow_screen_and_are_read_whole() {
        let plan = question(
            "Ship subagent activity? It needs migration 0024 on production, deploying \
             rakumux.com, and a Rakumux release.",
            "Plan",
            &[
                "Ship web now, Rakumux in 0.1.35 (Recommended)",
                "Ship everything now, the long way round with every single check",
                "Hold",
            ],
            false,
        );
        let mut dialog = Dialog::new(vec![plan], vec![answer(&[1], None)]);
        assert!(dialog.shown(&screen("narrow-long")));
        assert_eq!(
            dialog.next(&screen("narrow-long")),
            Next::Keys(vec!["down"])
        );
        // A free answer wrapped in the box under the dialog is read whole,
        // and so is a form whose hint wraps.
        let Some(Page {
            field: Field::Choices { other, .. },
            ..
        }) = Page::read(&screen("narrow-multi-other-typed"))
        else {
            panic!("not choices");
        };
        let typed = "A very long free answer that wraps across the narrow screen twice over";
        assert_eq!(other, Some(squash(typed)));
        let form = Page::read(&screen("narrow-form")).expect("a field's page");
        assert_eq!(form.label, "db");
        assert!(form.above.contains("dbtargetsreleasenameretries"));
    }

    #[test]
    fn a_step_is_seen_taken_only_when_the_dialog_moved() {
        let dialog = Dialog::new(vec![colour()], vec![answer(&[1], None)]);
        assert!(dialog.moved(&screen("one"), &screen("one-green")));
        // The same page with the spinner above it changed didn't move.
        let before = screen("one");
        let after = before.replacen("Asking user", "Asking  user", 1);
        assert_ne!(before, after);
        assert!(!dialog.moved(&before, &after));
    }
}
