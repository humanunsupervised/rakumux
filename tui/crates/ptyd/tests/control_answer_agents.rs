//! `session_answer` for GitHub Copilot CLI's `ask_user` and Droid's
//! `AskUser`: the daemon follows the agent's log as it does for the agent,
//! finds the question call open there, and works the dialog key by key. The
//! dialogs are stand-ins that show the screens the real agents showed for
//! each key (`integrations/<agent>/fixtures/questions/`, Copilot 1.0.105 and
//! Droid 1.0.0), take exactly the keys the real dialog took, and on the
//! last one write the tool's result into the log.
//!
//! Its own test binary: it points `COPILOT_HOME` and `FACTORY_HOME_OVERRIDE`
//! at folders of its own (each test its own pid and conversation in them),
//! and nothing else in this process reads the environment.
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
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

/// The folders the agents' logs are in, for every test here.
fn homes() -> &'static (PathBuf, PathBuf) {
    static HOMES: OnceLock<(tempfile::TempDir, (PathBuf, PathBuf))> = OnceLock::new();
    &HOMES
        .get_or_init(|| {
            let folder = tempfile::tempdir().unwrap();
            let copilot = folder.path().join("copilot");
            let factory = folder.path().join("factory");
            std::env::set_var("COPILOT_HOME", &copilot);
            std::env::set_var("FACTORY_HOME_OVERRIDE", &factory);
            (folder, (copilot, factory))
        })
        .1
}

/// The agents' pids, one per test (no process here has them).
static PIDS: AtomicU32 = AtomicU32::new(4_000_100_000);

fn copilot_detector(input: &DetectInput<'_>) -> Detection {
    Detection {
        agent: Some("copilot".into()),
        lifecycle: if input.screen.contains("Copilot needs information") {
            Lifecycle::Blocked
        } else {
            Lifecycle::Working
        },
    }
}

fn droid_detector(input: &DetectInput<'_>) -> Detection {
    Detection {
        agent: Some("droid".into()),
        lifecycle: if input.screen.contains("ESC stop agent") {
            Lifecycle::Blocked
        } else {
            Lifecycle::Working
        },
    }
}

fn append(path: &Path, entry: &Value) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .unwrap();
    writeln!(file, "{entry}").unwrap();
}

fn fixture(agent: &str, name: &str) -> String {
    let extension = if agent == "droid" { "ansi" } else { "txt" };
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../integrations")
        .join(agent)
        .join(format!("fixtures/questions/{name}.{extension}"));
    std::fs::read_to_string(&path).unwrap_or_else(|error| panic!("{path:?}: {error}"))
}

/// One write the stand-in dialog takes (named keys, or text), and the
/// screen it then shows.
struct Step {
    keys: &'static [&'static str],
    text: Option<&'static str>,
    then: &'static str,
}

const fn keys(keys: &'static [&'static str], then: &'static str) -> Step {
    Step {
        keys,
        text: None,
        then,
    }
}

const fn text(text: &'static str, then: &'static str) -> Step {
    Step {
        keys: &[],
        text: Some(text),
        then,
    }
}

/// An agent asking in its dialog: its session, its log, and the stand-in.
struct Asking {
    h: Harness,
    id: SessionId,
    stop: Arc<AtomicBool>,
    /// The steps taken so far, and whether the dialog gave its result.
    taken: Arc<Mutex<(usize, bool)>>,
}

impl Asking {
    /// `agent` (`copilot` or `droid`) asking with call `call_1`: its log has
    /// the call, its hook says it waits for input, its screen shows
    /// `first`; then each write that is `steps`' next shows that step's
    /// screen, and after the last the log gets `result`. Any other write
    /// leaves the dialog as it is.
    fn new(agent: &'static str, call: Value, first: &str, steps: Vec<Step>, result: Value) -> Self {
        let (copilot, factory) = homes();
        let pid = PIDS.fetch_add(1, Ordering::SeqCst);
        let conversation = format!("8d3f6a10-2c4b-4e7f-9a1d-{pid:012}");
        let h = Harness::with_detector(if agent == "copilot" {
            copilot_detector
        } else {
            droid_detector
        });
        let id = h.create(Some(agent)).id;
        let log = if agent == "copilot" {
            std::fs::create_dir_all(copilot.join("logs")).unwrap();
            std::fs::write(
                copilot.join(format!("logs/process-1793338015057-{pid}.log")),
                format!("2026-10-31T07:00:00.000Z [INFO] Registering foreground session: {conversation}\n"),
            )
            .unwrap();
            let log = copilot.join(format!("session-state/{conversation}/events.jsonl"));
            append(
                &log,
                &json!({"type": "user.message", "id": "e1", "parentId": null,
                    "timestamp": "2026-10-31T07:00:01.000Z", "data": {"content": "Ask me"}}),
            );
            log
        } else {
            let key = ptyd_droid_key(h.scope.path());
            let log = factory.join(format!(".factory/sessions/{key}/{conversation}.jsonl"));
            append(
                &log,
                &json!({"type": "session_start", "id": conversation, "title": "Ask me",
                    "cwd": h.scope.path(), "version": 2}),
            );
            append(
                &log,
                &json!({"type": "message", "id": "u1", "timestamp": "2026-10-31T07:00:01.000Z",
                    "message": {"role": "user", "content": [{"type": "text", "text": "Ask me"}]}}),
            );
            log
        };
        h.host.set_foreground(
            &id,
            ProcessInfo {
                pid,
                name: if agent == "copilot" {
                    "MainThread"
                } else {
                    "droid"
                }
                .into(),
                cmdline: vec![agent.into()],
                exe: None,
            },
        );
        let report = Report {
            activity: Some(ActivityUpdate {
                version: ACTIVITY_VERSION,
                agent: agent.into(),
                conversation_id: conversation.clone(),
                completion_id: None,
                event: ActivityEvent::Phase {
                    phase: ActivityPhase::NeedsInput,
                },
            }),
            ..Report::default()
        };
        let inside = h.session_caller(&id);
        assert_eq!(
            h.handle(&inside, Request::SessionReport(report)),
            Response::Ok
        );
        eventually("the conversation followed", || {
            h.host.show(&id, first);
            std::thread::sleep(Duration::from_millis(300));
            (h.get(&id).user_prompt.as_deref() == Some("Ask me")).then_some(())
        });
        append(&log, &call);
        // A second's read on, the call is read too.
        for _ in 0..4 {
            std::thread::sleep(Duration::from_millis(400));
            h.host.show(&id, first);
        }
        let asking = Self {
            stop: Arc::new(AtomicBool::new(false)),
            taken: Arc::new(Mutex::new((0, false))),
            h,
            id,
        };
        asking.drive(first.to_owned(), steps, log, result);
        asking
    }

    fn drive(&self, first: String, steps: Vec<Step>, log: PathBuf, result: Value) {
        let (host, id, stop, taken) = (
            self.h.host.clone(),
            self.id.clone(),
            self.stop.clone(),
            self.taken.clone(),
        );
        let terminal = host.terminal_of(&id);
        std::thread::spawn(move || {
            let mut screen = first;
            let mut seen = 0;
            let mut frozen = false;
            while !stop.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(20));
                let writes = host.terminal(terminal).keys;
                for (keys, typed) in &writes[seen..] {
                    let mut taken = taken.lock().unwrap();
                    let due = steps.get(taken.0).filter(|_| !frozen);
                    let fits = due.is_some_and(|step| {
                        step.keys == keys.as_slice() && step.text == typed.as_deref()
                    });
                    match due {
                        Some(step) if fits => {
                            screen = step.then.to_owned();
                            taken.0 += 1;
                            if taken.0 == steps.len() {
                                append(&log, &result);
                                taken.1 = true;
                            }
                        }
                        _ => frozen = true,
                    }
                }
                seen = writes.len();
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

    fn writes(&self) -> Vec<(Vec<String>, Option<String>)> {
        self.h.host.terminal(self.h.host.terminal_of(&self.id)).keys
    }
}

impl Drop for Asking {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

/// Droid's folder key for `cwd`: its real path with `/` as `-`.
fn ptyd_droid_key(cwd: &Path) -> String {
    let real = std::fs::canonicalize(cwd).unwrap();
    format!(
        "-{}",
        real.to_string_lossy().trim_matches('/').replace('/', "-")
    )
}

fn answer(question: &str, choices: &[&str], text: Option<&str>) -> QuestionAnswer {
    QuestionAnswer {
        question: question.into(),
        choices: choices.iter().map(|c| c.to_string()).collect(),
        text: text.map(String::from),
    }
}

// ------------------------------------------------------------------ Copilot

fn copilot_call(arguments: Value) -> Value {
    json!({"type": "assistant.message", "id": "e2", "parentId": "e1",
        "timestamp": "2026-10-31T07:00:02.000Z", "data": {"messageId": "m1", "content": "",
            "toolRequests": [{"toolCallId": "call_1", "name": "ask_user",
                "arguments": arguments, "type": "function"}]}})
}

fn copilot_result(said: &str) -> Value {
    json!({"type": "tool.execution_complete", "id": "e3", "parentId": "e2",
        "timestamp": "2026-10-31T07:00:20.000Z", "data": {"toolCallId": "call_1",
            "success": true, "result": {"content": format!("User responded: {said}")}}})
}

fn release_form() -> Value {
    json!({"message": "A few choices for the release.", "requestedSchema": {"type": "object",
        "properties": {
            "colour": {"type": "string", "title": "Colour", "enum": ["red", "green", "blue"]},
            "fruits": {"type": "array", "title": "Fruits",
                       "items": {"type": "string", "enum": ["apple", "banana", "cherry"]}},
            "ship": {"type": "boolean", "title": "Ship today"},
            "note": {"type": "string", "title": "Note"}},
        "required": ["colour", "fruits", "ship"]}})
}

#[test]
fn a_copilot_form_is_answered_field_by_field_and_its_result_read() {
    let f = |name| fixture("copilot", name);
    let steps = vec![
        keys(&["down"], leak(f("form-colour-green"))),
        keys(&["enter"], leak(f("form-fruits"))),
        keys(&["down"], leak(f("form-fruits-banana"))),
        keys(&["space"], leak(f("form-fruits-ticked"))),
        keys(&["enter"], leak(f("form-ship"))),
        keys(&["down"], leak(f("form-ship-no"))),
        keys(&["enter"], leak(f("form-note"))),
        text("Ship it on Friday", leak(f("form-note-typed"))),
        keys(&["enter"], leak(f("form-answered"))),
    ];
    let copilot = Asking::new(
        "copilot",
        copilot_call(release_form()),
        &f("form-colour"),
        steps,
        copilot_result("colour=green, fruits=banana, ship=false, note=Ship it on Friday"),
    );
    // The answers in the order rakumux.com reads the form (by field name).
    let answered = copilot.answer(
        "call_1",
        vec![
            answer("Colour", &["green"], None),
            answer("Fruits", &["banana"], None),
            answer("Note", &[], Some("Ship it on Friday")),
            answer("Ship today", &["No"], None),
        ],
    );
    assert_eq!(answered, Response::Ok);
    assert_eq!(*copilot.taken.lock().unwrap(), (9, true));
    // Every step one write, never Esc or a decline.
    let writes = copilot.writes();
    assert_eq!(writes.len(), 9);
    assert!(writes
        .iter()
        .all(|(keys, text)| keys.len() + usize::from(text.is_some()) == 1));
    assert!(writes
        .iter()
        .all(|(keys, _)| !keys.iter().any(|key| key == "esc" || key == "ctrl+d")));
}

#[test]
fn a_copilot_free_answer_goes_in_the_box_under_its_choices() {
    let f = |name| fixture("copilot", name);
    let steps = vec![
        keys(&["down"], leak(f("one-green"))),
        keys(&["down"], leak(f("one-blue"))),
        keys(&["down"], leak(f("one-other"))),
        text("Purple, 2 shades", leak(f("one-other-typed"))),
        keys(&["enter"], leak(f("answered"))),
    ];
    let copilot = Asking::new(
        "copilot",
        copilot_call(json!({"message": "Which colour should the button be?",
            "requestedSchema": {"type": "object", "properties": {"colour": {"type": "string",
                "title": "Colour", "enum": ["red", "green", "blue"]}}, "required": ["colour"]}})),
        &f("one"),
        steps,
        copilot_result("Purple, 2 shades"),
    );
    // A call that isn't the one open, or answers that don't fit: nothing typed.
    let other = error(copilot.answer(
        "call_9",
        vec![answer("Which colour should the button be?", &["red"], None)],
    ));
    assert_eq!(other.code, ErrorCode::QuestionGone);
    let unfit = error(copilot.answer(
        "call_1",
        vec![answer(
            "Which colour should the button be?",
            &["teal"],
            None,
        )],
    ));
    assert_eq!(unfit.code, ErrorCode::InvalidArgument);
    assert!(copilot.writes().is_empty());
    assert_eq!(
        copilot.answer(
            "call_1",
            vec![answer(
                "Which colour should the button be?",
                &[],
                Some("Purple, 2 shades")
            )],
        ),
        Response::Ok
    );
    assert_eq!(*copilot.taken.lock().unwrap(), (5, true));
}

#[test]
fn nothing_is_typed_into_copilots_prompt_for_permission() {
    let f = |name| fixture("copilot", name);
    // The log has the question open, but the screen asks for permission.
    let copilot = Asking::new(
        "copilot",
        copilot_call(json!({"message": "Which colour should the button be?",
            "requestedSchema": {"type": "object", "properties": {"colour": {"type": "string",
                "title": "Colour", "enum": ["red", "green", "blue"]}}}})),
        &f("permission"),
        Vec::new(),
        Value::Null,
    );
    let refused = error(copilot.answer(
        "call_1",
        vec![answer("Which colour should the button be?", &["red"], None)],
    ));
    assert_eq!(refused.code, ErrorCode::QuestionGone);
    assert_eq!(refused.message, "the screen doesn't show the question");
    assert!(copilot.writes().is_empty());
}

// -------------------------------------------------------------------- Droid

fn droid_call(questionnaire: &str) -> Value {
    json!({"type": "message", "id": "a1", "timestamp": "2026-10-31T07:00:02.000Z",
        "message": {"role": "assistant", "content": [{"type": "tool_use", "id": "call_1",
            "name": "AskUser", "input": {"questionnaire": questionnaire}}]}})
}

fn droid_result(content: &str) -> Value {
    json!({"type": "message", "id": "u2", "timestamp": "2026-10-31T07:00:20.000Z",
        "message": {"role": "user", "content": [{"type": "tool_result",
            "tool_use_id": "call_1", "is_error": false, "content": content}]}})
}

const THREE: &str = "1. [question] Which colour should the button be?\n[topic] Colour\n[option] Red\n[option] Green\n[option] Blue\n\n2. [question] Which fruits do you like? (multi)\n[topic] Fruits\n[option] Apple\n[option] Banana\n[option] Cherry\n\n3. [question] Which pet would you keep?\n[topic] Pet\n[option] Cat\n[option] Dog\n";

#[test]
fn a_droid_questionnaire_is_answered_by_number_from_its_styled_screen() {
    let f = |name| fixture("droid", name);
    let steps = vec![
        keys(&["2"], leak(f("three-q2"))),
        keys(&["2"], leak(f("three-q2-banana"))),
        keys(&["down"], leak(f("three-q2-banana-down"))),
        keys(&["down"], leak(f("three-q2-banana-down2"))),
        keys(&["down"], leak(f("three-q2-banana-own"))),
        keys(&["down"], leak(f("three-q2-next"))),
        keys(&["enter"], leak(f("three-q3"))),
        keys(&["1"], leak(f("three-answered"))),
    ];
    let droid = Asking::new(
        "droid",
        droid_call(THREE),
        &f("three-q1"),
        steps,
        droid_result("1. [question] Which colour should the button be?\n[answer] Green\n\n2. [question] Which fruits do you like?\n[answer] Banana\n\n3. [question] Which pet would you keep?\n[answer] Cat"),
    );
    let answered = droid.answer(
        "call_1",
        vec![
            answer("Which colour should the button be?", &["Green"], None),
            answer("Which fruits do you like?", &["Banana"], None),
            answer("Which pet would you keep?", &["Cat"], None),
        ],
    );
    assert_eq!(answered, Response::Ok);
    assert_eq!(*droid.taken.lock().unwrap(), (8, true));
    assert!(droid
        .writes()
        .iter()
        .all(|(keys, _)| !keys.iter().any(|key| key == "esc" || key == "tab")));
}

#[test]
fn a_droid_free_answer_is_typed_on_its_row() {
    let f = |name| fixture("droid", name);
    let steps = vec![
        keys(&["down"], leak(f("one-down"))),
        keys(&["down"], leak(f("one-down2"))),
        keys(&["down"], leak(f("one-own"))),
        text("Purple, 2 shades", leak(f("one-own-typed"))),
        keys(&["enter"], leak(f("one-answered"))),
    ];
    let droid = Asking::new(
        "droid",
        droid_call(
            "1. [question] Which colour should the button be?\n[topic] Colour\n[option] Red\n[option] Green\n[option] Blue\n",
        ),
        &f("one"),
        steps,
        droid_result("1. [question] Which colour should the button be?\n[answer] Purple, 2 shades"),
    );
    assert_eq!(
        droid.answer(
            "call_1",
            vec![answer(
                "Which colour should the button be?",
                &[],
                Some("Purple, 2 shades")
            )],
        ),
        Response::Ok
    );
    assert_eq!(*droid.taken.lock().unwrap(), (5, true));
}

#[test]
fn nothing_is_typed_into_droids_prompt_for_approval_and_a_stuck_dialog_stops_it() {
    let f = |name| fixture("droid", name);
    let droid = Asking::new(
        "droid",
        droid_call(THREE),
        &f("approval"),
        Vec::new(),
        Value::Null,
    );
    let answers = || {
        vec![
            answer("Which colour should the button be?", &["Green"], None),
            answer("Which fruits do you like?", &["Banana"], None),
            answer("Which pet would you keep?", &["Cat"], None),
        ]
    };
    let refused = error(droid.answer("call_1", answers()));
    assert_eq!(refused.code, ErrorCode::QuestionGone);
    assert_eq!(refused.message, "the screen doesn't show the question");
    assert!(droid.writes().is_empty());
    // A dialog that takes the first key and then no other.
    let stuck = Asking::new(
        "droid",
        droid_call(THREE),
        &f("three-q1"),
        vec![keys(&["2"], leak(f("three-q2")))],
        Value::Null,
    );
    let stopped = error(stuck.answer("call_1", answers()));
    assert_eq!(stopped.code, ErrorCode::NotTaken);
    assert_eq!(stuck.writes().len(), 2);
}

/// A fixture's text for a step (the stand-in keeps its screens for the
/// test's whole run).
fn leak(text: String) -> &'static str {
    Box::leak(text.into_boxed_str())
}
