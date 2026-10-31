//! `session_answer`: a question Claude Code asks in its `AskUserQuestion`
//! dialog, answered key by key from its screen. A stand-in dialog draws the
//! screens Claude Code 2.1.314 draws, takes the keys the daemon sends, and
//! on submit writes the tool's result into the conversation's log, which the
//! daemon follows.
//!
//! Its own test binary: it points `CLAUDE_CONFIG_DIR` at a folder of its own
//! (each test its own Claude Code pid and conversation in it), and nothing
//! else in this process reads the environment.
#![cfg(target_os = "linux")]

mod control_common;

use control_common::*;
use ptyd::detect::{DetectInput, Detection};
use ptyd::host::ProcessInfo;
use serde_json::{json, Value};
use session_core::activity::{ActivityEvent, ActivityPhase, ActivityUpdate, ACTIVITY_VERSION};
use session_core::api::{ErrorCode, QuestionAnswer, Report, Request, Response};
use session_core::ids::SessionId;
use session_core::model::Lifecycle;
use std::fs::File;
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

/// Claude Code's config folder for every test here.
fn config() -> &'static PathBuf {
    static CONFIG: OnceLock<(tempfile::TempDir, PathBuf)> = OnceLock::new();
    &CONFIG
        .get_or_init(|| {
            let folder = tempfile::tempdir().unwrap();
            let path = folder.path().to_path_buf();
            std::env::set_var("CLAUDE_CONFIG_DIR", &path);
            (folder, path)
        })
        .1
}

/// Claude Code's pids as the stand-ins show them, one per test.
static PIDS: AtomicU32 = AtomicU32::new(5100);

/// The fake agent's screen read by markers, as Claude Code always.
fn claude_detector(input: &DetectInput<'_>) -> Detection {
    Detection {
        agent: Some("claude".into()),
        ..marker_detector(input)
    }
}

const RULE: &str = "────────────────────────────────────────────────────────────────────────";

#[derive(Clone)]
struct Asked {
    question: &'static str,
    header: &'static str,
    options: &'static [&'static str],
    multi: bool,
}

const COLOUR: Asked = Asked {
    question: "Which colour do you prefer?",
    header: "Colour",
    options: &["Red", "Green", "Blue"],
    multi: false,
};
const FRUITS: Asked = Asked {
    question: "Which fruits do you like?",
    header: "Fruits",
    options: &["Apple", "Banana", "Cherry"],
    multi: true,
};
const PET: Asked = Asked {
    question: "Which pet would you keep?",
    header: "Pet",
    options: &["Cat", "Dog"],
    multi: false,
};

/// Claude Code's `AskUserQuestion` dialog, as it behaves: one page per
/// question, then a review when it asks several or takes several choices.
struct Stand {
    asked: Vec<Asked>,
    /// The page shown: a question's, or `asked.len()` for the review.
    page: usize,
    cursor: Vec<usize>,
    ticked: Vec<Vec<bool>>,
    typed: Vec<String>,
    /// A question's chosen option (one choice), or its free answer.
    chosen: Vec<Option<usize>>,
    /// Answered (its result written) or dismissed.
    closed: bool,
    /// The keys it takes before it stops answering any.
    frozen_after: Option<usize>,
    steps: usize,
}

impl Stand {
    fn new(asked: Vec<Asked>) -> Self {
        let n = asked.len();
        Self {
            ticked: asked.iter().map(|a| vec![false; a.options.len()]).collect(),
            asked,
            page: 0,
            cursor: vec![0; n + 1],
            typed: vec![String::new(); n],
            chosen: vec![None; n],
            closed: false,
            frozen_after: None,
            steps: 0,
        }
    }

    /// A question's rows: its options, the free answer, `Next` for one that
    /// takes several, and `Chat about this`.
    fn rows(&self, page: usize) -> usize {
        let asked = &self.asked[page];
        asked.options.len() + 2 + usize::from(asked.multi)
    }

    fn reviews(&self) -> bool {
        self.asked.len() > 1 || self.asked[0].multi
    }

    fn render(&self) -> String {
        if self.closed {
            return "[working]\n✻ Thinking…\n".into();
        }
        let mut out = vec![
            "[blocked]".to_owned(),
            "❯ Ask me".to_owned(),
            RULE.to_owned(),
        ];
        let boxes: Vec<String> = self
            .asked
            .iter()
            .enumerate()
            .map(|(i, a)| {
                let done = self.chosen[i].is_some() || self.ticked[i].contains(&true);
                format!("{} {}", if done { "☒" } else { "☐" }, a.header)
            })
            .collect();
        out.push(if self.reviews() {
            format!("←  {}  ✔ Submit  →", boxes.join("  "))
        } else {
            format!(" {}", boxes[0])
        });
        out.push(String::new());
        let mark = |on: bool| if on { "❯" } else { " " };
        if self.page == self.asked.len() {
            out.push("Review your answers".into());
            out.push(String::new());
            for (i, a) in self.asked.iter().enumerate() {
                out.push(format!(" ● {}", a.question));
                let mut said: Vec<String> = a
                    .options
                    .iter()
                    .enumerate()
                    .filter(|(o, _)| self.ticked[i][*o] || self.chosen[i] == Some(*o))
                    .map(|(_, label)| label.to_string())
                    .collect();
                if !self.typed[i].is_empty() {
                    said.push(self.typed[i].clone());
                }
                out.push(format!("   → {}", said.join(", ")));
            }
            out.push(String::new());
            out.push("Ready to submit your answers?".into());
            out.push(String::new());
            let cursor = self.cursor[self.page];
            out.push(format!("{} 1. Submit answers", mark(cursor == 0)));
            out.push(format!("{} 2. Cancel", mark(cursor == 1)));
            return out.join("\n");
        }
        let a = &self.asked[self.page];
        let cursor = self.cursor[self.page];
        out.push(a.question.into());
        out.push(String::new());
        let free = a.options.len();
        for (o, label) in a.options.iter().enumerate() {
            let tick = if a.multi {
                if self.ticked[self.page][o] {
                    "[✔] "
                } else {
                    "[ ] "
                }
            } else {
                ""
            };
            out.push(format!("{} {}. {tick}{label}", mark(cursor == o), o + 1));
            out.push(format!("     About {label}"));
        }
        let typed = &self.typed[self.page];
        let label = match (a.multi, typed.is_empty()) {
            (false, true) => "Type something.".to_owned(),
            (false, false) => typed.clone(),
            (true, true) => "[ ] Type something".to_owned(),
            (true, false) => format!("[✔] {typed}"),
        };
        out.push(format!("{} {}. {label}", mark(cursor == free), free + 1));
        if a.multi {
            let next = if self.page + 1 == self.asked.len() {
                "Submit"
            } else {
                "Next"
            };
            out.push(format!("{}    {next}", mark(cursor == free + 1)));
        }
        out.push(RULE.into());
        let chat = self.rows(self.page) - 1;
        out.push(format!(
            "{} {}. Chat about this",
            mark(cursor == chat),
            free + 2
        ));
        out.push(String::new());
        out.push("Enter to select · ↑/↓ to navigate · Esc to cancel".into());
        out.join("\n")
    }

    /// Take one write of keys or text; whether it closed the dialog.
    fn take(&mut self, keys: &[String], text: Option<&str>) -> bool {
        self.steps += 1;
        if self.closed || self.frozen_after.is_some_and(|after| self.steps > after) {
            return false;
        }
        let review = self.page == self.asked.len();
        let rows = if review { 2 } else { self.rows(self.page) };
        if let Some(text) = text {
            let a = &self.asked[self.page];
            if !review && self.cursor[self.page] == a.options.len() {
                self.typed[self.page].push_str(text);
            }
            return false;
        }
        for key in keys {
            let cursor = &mut self.cursor[self.page];
            match key.as_str() {
                "down" => *cursor = (*cursor + 1).min(rows - 1),
                "up" => *cursor = cursor.saturating_sub(1),
                "left" => self.page = self.page.saturating_sub(1),
                "space" if !review && self.asked[self.page].multi => {
                    if let Some(tick) = self.ticked[self.page].get_mut(*cursor) {
                        *tick = !*tick;
                    }
                }
                "enter" if review => {
                    if *cursor == 0 {
                        self.closed = true;
                        return true;
                    }
                }
                "enter" => {
                    let a = &self.asked[self.page];
                    let free = a.options.len();
                    let at = *cursor;
                    let leaves = if a.multi {
                        if at < free {
                            self.ticked[self.page][at] = !self.ticked[self.page][at];
                        }
                        at == free + 1
                    } else {
                        self.chosen[self.page] = Some(at).filter(|at| *at < free);
                        at <= free && (at < free || !self.typed[self.page].is_empty())
                    };
                    if leaves {
                        if !self.reviews() {
                            self.closed = true;
                            return true;
                        }
                        self.page += 1;
                    }
                }
                _ => {}
            }
        }
        false
    }

    /// The answers as Claude Code's result gives them, by question.
    fn answers(&self) -> Value {
        let mut answers = serde_json::Map::new();
        for (i, a) in self.asked.iter().enumerate() {
            let mut said: Vec<String> = a
                .options
                .iter()
                .enumerate()
                .filter(|(o, _)| self.ticked[i][*o] || self.chosen[i] == Some(*o))
                .map(|(_, label)| label.to_string())
                .collect();
            if !self.typed[i].is_empty() {
                said.push(self.typed[i].clone());
            }
            answers.insert(a.question.into(), json!(said.join(", ")));
        }
        Value::Object(answers)
    }
}

/// One test's Claude Code: its session, its log, and its dialog driven by
/// the keys the daemon sends.
struct Claude {
    h: Harness,
    id: SessionId,
    log: Arc<Mutex<File>>,
    conversation: String,
    stand: Arc<Mutex<Stand>>,
    stop: Arc<AtomicBool>,
}

impl Claude {
    /// Claude Code asking `asked` as call `toolu_1`, its dialog on screen
    /// (with `screen` in its place when given) and its hook saying it waits
    /// for input.
    fn asking(asked: Vec<Asked>, screen: Option<&str>) -> Self {
        let config = config();
        let pid = PIDS.fetch_add(1, Ordering::SeqCst);
        let conversation = format!("8d3f6a10-2c4b-4e7f-9a1d-{pid:012}");
        std::fs::create_dir_all(config.join("sessions")).unwrap();
        std::fs::write(
            config.join("sessions").join(format!("{pid}.json")),
            json!({"sessionId": conversation, "pid": pid}).to_string(),
        )
        .unwrap();
        let project = config.join("projects").join("-home-me-project");
        std::fs::create_dir_all(&project).unwrap();
        let log = File::create(project.join(format!("{conversation}.jsonl"))).unwrap();
        let log = Arc::new(Mutex::new(log));
        let h = Harness::with_detector(claude_detector);
        let id = h.create(Some("claude")).id;
        h.host.set_foreground(
            &id,
            ProcessInfo {
                pid,
                name: "claude".into(),
                cmdline: vec!["claude".into()],
                exe: None,
            },
        );
        let write = |entry: Value| writeln!(log.lock().unwrap(), "{entry}").unwrap();
        write(
            json!({"type": "user", "uuid": "u1", "sessionId": conversation,
            "message": {"role": "user", "content": "Ask me"}}),
        );
        write(
            json!({"type": "assistant", "uuid": "a1", "sessionId": conversation,
            "message": {"role": "assistant", "content": [{"type": "tool_use", "id": "toolu_1",
                "name": "AskUserQuestion", "input": {"questions": asked.iter().map(|a| json!({
                    "question": a.question, "header": a.header, "multiSelect": a.multi,
                    "options": a.options.iter().map(|label| json!({"label": label,
                        "description": format!("About {label}")})).collect::<Vec<_>>()
                })).collect::<Vec<_>>()}}]}}),
        );
        let stand = Arc::new(Mutex::new(Stand::new(asked)));
        let claude = Self {
            h,
            id,
            log,
            conversation,
            stand,
            stop: Arc::new(AtomicBool::new(false)),
        };
        let shown = || match screen {
            Some(screen) => screen.to_owned(),
            None => claude.stand.lock().unwrap().render(),
        };
        // Its hook said it waits; the daemon follows the log and sees the call.
        let report = Report {
            activity: Some(ActivityUpdate {
                version: ACTIVITY_VERSION,
                agent: "claude".into(),
                conversation_id: claude.conversation.clone(),
                completion_id: None,
                event: ActivityEvent::Phase {
                    phase: ActivityPhase::NeedsInput,
                },
            }),
            ..Report::default()
        };
        let inside = claude.h.session_caller(&claude.id);
        assert_eq!(
            claude.h.handle(&inside, Request::SessionReport(report)),
            Response::Ok
        );
        eventually("the question followed", || {
            claude.h.host.show(&claude.id, &shown());
            std::thread::sleep(Duration::from_millis(300));
            (claude.h.get(&claude.id).user_prompt.as_deref() == Some("Ask me")).then_some(())
        });
        // A second's read on, the call is read too.
        std::thread::sleep(Duration::from_millis(1_100));
        claude.h.host.show(&claude.id, &shown());
        std::thread::sleep(Duration::from_millis(300));
        assert_eq!(claude.h.get(&claude.id).lifecycle, Lifecycle::Blocked);
        if screen.is_none() {
            claude.drive();
        }
        claude
    }

    /// Take the keys the daemon sends, draw each step, and on submit write
    /// the tool's result into the log; then keep drawing (a spinner), so
    /// the daemon reads the log again.
    fn drive(&self) {
        let (host, id, stand, log, stop, conversation) = (
            self.h.host.clone(),
            self.id.clone(),
            self.stand.clone(),
            self.log.clone(),
            self.stop.clone(),
            self.conversation.clone(),
        );
        let terminal = host.terminal_of(&id);
        std::thread::spawn(move || {
            let mut seen = 0;
            while !stop.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(20));
                let keys = host.terminal(terminal).keys;
                let mut stand = stand.lock().unwrap();
                for (keys, text) in &keys[seen..] {
                    if stand.take(keys, text.as_deref()) {
                        let result = json!({"type": "user", "uuid": "u2", "sessionId": conversation,
                            "message": {"role": "user", "content": [{"type": "tool_result",
                                "tool_use_id": "toolu_1", "content": "Your questions have been answered"}]},
                            "toolUseResult": {"answers": stand.answers()}});
                        writeln!(log.lock().unwrap(), "{result}").unwrap();
                    }
                }
                seen = keys.len();
                let screen = stand.render();
                drop(stand);
                host.show(&id, &screen);
            }
        });
    }

    fn answer(&self, call_id: &str, answers: Vec<QuestionAnswer>) -> Response {
        self.h.handle(
            &self.h.user,
            Request::SessionAnswer {
                id: self.id.clone(),
                call_id: call_id.into(),
                answers,
                quiet_ms: Some(10_000),
            },
        )
    }

    fn keys(&self) -> Vec<(Vec<String>, Option<String>)> {
        self.h.host.terminal(self.h.host.terminal_of(&self.id)).keys
    }
}

impl Drop for Claude {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
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
fn three_questions_are_answered_through_the_dialog_and_its_result_read() {
    let claude = Claude::asking(vec![COLOUR, FRUITS, PET], None);
    let answered = claude.answer(
        "toolu_1",
        vec![
            answer(COLOUR.question, &["Blue"], None),
            answer(FRUITS.question, &["Cherry", "Apple"], Some("Mango")),
            answer(PET.question, &[], Some("A parrot")),
        ],
    );
    assert_eq!(answered, Response::Ok);
    let stand = claude.stand.lock().unwrap();
    assert!(stand.closed);
    assert_eq!(
        stand.answers(),
        json!({
            "Which colour do you prefer?": "Blue",
            "Which fruits do you like?": "Apple, Cherry, Mango",
            "Which pet would you keep?": "A parrot",
        })
    );
    drop(stand);
    // Every step one write: arrows, boxes, the free answers, the enters.
    let keys = claude.keys();
    assert!(keys
        .iter()
        .all(|(keys, text)| keys.len() + usize::from(text.is_some()) == 1));
    assert!(keys
        .iter()
        .all(|(keys, _)| !keys.iter().any(|key| key == "esc" || key == "tab")));
    // The audit says how many answers went in, never what they said.
    let row = claude
        .h
        .audit()
        .into_iter()
        .find(|row| row.method == "session.answer")
        .unwrap();
    assert_eq!(row.decision, "allowed");
    let detail = row.detail.unwrap_or_default();
    assert!(detail.contains("toolu_1: 3 answers"), "{detail}");
    assert!(
        !detail.contains("parrot") && !detail.contains("Mango"),
        "{detail}"
    );
}

#[test]
fn one_question_taking_one_choice_is_answered_without_a_review() {
    let claude = Claude::asking(vec![COLOUR], None);
    assert_eq!(
        claude.answer("toolu_1", vec![answer(COLOUR.question, &["Green"], None)]),
        Response::Ok
    );
    assert_eq!(
        claude.stand.lock().unwrap().answers(),
        json!({"Which colour do you prefer?": "Green"})
    );
    let keys: Vec<Vec<String>> = claude.keys().into_iter().map(|(keys, _)| keys).collect();
    assert_eq!(keys, [vec!["down".to_owned()], vec!["enter".to_owned()]]);
    // Answered: another answer finds no such question, and types nothing.
    let again = error(claude.answer("toolu_1", vec![answer(COLOUR.question, &["Red"], None)]));
    assert_eq!(again.code, ErrorCode::QuestionGone);
    assert_eq!(claude.keys().len(), 2);
}

#[test]
fn nothing_is_typed_into_a_prompt_for_permission_or_another_question() {
    // The log has the question open, but the screen shows a permission prompt.
    let permission = "[blocked]\n Bash command\n\n Do you want to proceed?\n ❯ 1. Yes\n   2. No\n\n Esc to cancel";
    let claude = Claude::asking(vec![COLOUR], Some(permission));
    let refused = error(claude.answer("toolu_1", vec![answer(COLOUR.question, &["Red"], None)]));
    assert_eq!(refused.code, ErrorCode::QuestionGone);
    // Another call than the one open, or answers that don't fit it: nothing either.
    let other = error(claude.answer("toolu_9", vec![answer(COLOUR.question, &["Red"], None)]));
    assert_eq!(other.code, ErrorCode::QuestionGone);
    let unfit = error(claude.answer("toolu_1", vec![answer(COLOUR.question, &["Teal"], None)]));
    assert_eq!(unfit.code, ErrorCode::InvalidArgument);
    assert!(claude.keys().is_empty());
    // From inside a session, never.
    let inside = claude.h.session_caller(&claude.id);
    let denied = error(claude.h.handle(
        &inside,
        Request::SessionAnswer {
            id: claude.id.clone(),
            call_id: "toolu_1".into(),
            answers: vec![answer(COLOUR.question, &["Red"], None)],
            quiet_ms: None,
        },
    ));
    assert_eq!(denied.code, ErrorCode::PermissionDenied);
    assert!(claude.keys().is_empty());
}

#[test]
fn nothing_is_typed_while_someone_types_in_the_session() {
    let claude = Claude::asking(vec![COLOUR], None);
    claude.h.host.set_typing(&claude.id, true);
    let busy = error(claude.answer("toolu_1", vec![answer(COLOUR.question, &["Red"], None)]));
    assert_eq!(busy.code, ErrorCode::SessionInUse);
    assert!(claude.keys().is_empty());
    claude.h.host.set_typing(&claude.id, false);
    assert_eq!(
        claude.answer("toolu_1", vec![answer(COLOUR.question, &["Red"], None)]),
        Response::Ok
    );
}

#[test]
fn a_dialog_that_stops_taking_keys_stops_the_answer() {
    let claude = Claude::asking(vec![COLOUR, FRUITS], None);
    claude.stand.lock().unwrap().frozen_after = Some(2);
    let stopped = error(claude.answer(
        "toolu_1",
        vec![
            answer(COLOUR.question, &["Blue"], None),
            answer(FRUITS.question, &["Apple"], None),
        ],
    ));
    assert_eq!(stopped.code, ErrorCode::NotTaken);
    // The third key went in, showed nothing, and nothing followed it.
    assert_eq!(claude.keys().len(), 3);
    assert!(!claude.stand.lock().unwrap().closed);
}
