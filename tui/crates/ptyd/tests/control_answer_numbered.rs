//! `session_answer` for the dialogs whose options are picked by number:
//! OpenCode (1.x and 2), Grok Build and Kimi Code. Each test writes the
//! agent's own log (or OpenCode's database) with a question call, shows the
//! screens the real agent drew (captured into
//! `integrations/<agent>/fixtures/questions/`), and replays its dialog: each
//! key the daemon sends must be the one the real agent took at that step,
//! and draws the screen it drew next; the last writes the tool's result,
//! which the daemon reads before it answers `ok`. Detection is the
//! daemon's own, from those screens.
//!
//! Its own test binary: it points `GROK_HOME`, `KIMI_CODE_HOME` and
//! `XDG_DATA_HOME` at folders of its own, and nothing else in this process
//! reads the environment.
#![cfg(target_os = "linux")]

mod control_common;

use control_common::*;
use ptyd::host::ProcessInfo;
use serde_json::{json, Value};
use session_core::api::{
    AgentWorkingDirectory, ErrorCode, QuestionAnswer, Report, Request, Response,
};
use session_core::ids::SessionId;
use session_core::model::Lifecycle;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

/// The agents' homes for every test here.
struct Homes {
    _dir: tempfile::TempDir,
    grok: PathBuf,
    kimi: PathBuf,
    /// OpenCode's database.
    opencode: PathBuf,
}

fn homes() -> &'static Homes {
    static HOMES: OnceLock<Homes> = OnceLock::new();
    HOMES.get_or_init(|| {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let (grok, kimi, data) = (root.join("grok"), root.join("kimi"), root.join("data"));
        std::fs::create_dir_all(data.join("opencode")).unwrap();
        std::fs::create_dir_all(&grok).unwrap();
        std::fs::create_dir_all(&kimi).unwrap();
        let opencode = data.join("opencode/opencode.db");
        rusqlite::Connection::open(&opencode)
            .unwrap()
            .execute_batch(
                "CREATE TABLE session_message (id TEXT, session_id TEXT, seq INTEGER, \
                 type TEXT, time_updated INTEGER, data TEXT); \
                 CREATE TABLE message (id TEXT, session_id TEXT, time_created INTEGER, \
                 time_updated INTEGER, data TEXT); \
                 CREATE TABLE part (id TEXT, message_id TEXT, session_id TEXT, \
                 time_created INTEGER, time_updated INTEGER, data TEXT);",
            )
            .unwrap();
        std::env::set_var("GROK_HOME", &grok);
        std::env::set_var("KIMI_CODE_HOME", &kimi);
        std::env::set_var("XDG_DATA_HOME", &data);
        Homes {
            _dir: dir,
            grok,
            kimi,
            opencode,
        }
    })
}

/// A fixture screen of `agent` (`grok`, `kimi`, `open-code`).
fn fixture(agent: &str, name: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../integrations")
        .join(agent)
        .join("fixtures/questions")
        .join(format!("{name}.txt"));
    std::fs::read_to_string(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

/// What the daemon sends in one write: a named key, or text.
#[derive(Clone, Debug, PartialEq)]
enum Input {
    Key(String),
    Text(String),
}

/// One step of the real dialog: the input it took, and the screen it drew.
struct Step {
    input: Input,
    screen: &'static str,
}

fn key(key: &str, screen: &'static str) -> Step {
    Step {
        input: Input::Key(key.into()),
        screen,
    }
}

fn text(text: &str, screen: &'static str) -> Step {
    Step {
        input: Input::Text(text.into()),
        screen,
    }
}

/// The dialog replayed: the screen it shows, the steps still due, what came
/// that it didn't expect, and whether the answers were submitted.
struct Replay {
    agent: &'static str,
    screen: String,
    steps: Vec<Step>,
    unexpected: Vec<(Vec<String>, Option<String>)>,
    submitted: bool,
}

/// One test's agent: its session, the dialog replayed on its screen, and
/// what writes the tool's result once the answers are submitted.
struct Agent {
    h: Harness,
    id: SessionId,
    replay: Arc<Mutex<Replay>>,
    stop: Arc<AtomicBool>,
}

impl Agent {
    /// A session running `process`, showing `first` of `agent`'s fixtures.
    fn new(process: &str, exe: Option<&str>, agent: &'static str, first: &str) -> Self {
        let h = Harness::with_detector(ptyd::detect::detect);
        let id = h.create(Some(process)).id;
        h.host.set_foreground(
            &id,
            ProcessInfo {
                pid: std::process::id(),
                name: process.into(),
                cmdline: vec![process.into()],
                exe: exe.map(PathBuf::from),
            },
        );
        let replay = Replay {
            agent,
            screen: fixture(agent, first),
            steps: Vec::new(),
            unexpected: Vec::new(),
            submitted: false,
        };
        Self {
            h,
            id,
            replay: Arc::new(Mutex::new(replay)),
            stop: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Show the dialog until the daemon follows the question and sees it
    /// asked, then replay `steps`, `result` writing the tool's result when
    /// the last is taken.
    fn replay(&self, steps: Vec<Step>, result: impl Fn() + Send + 'static) {
        self.replay.lock().unwrap().steps = steps;
        let screen = || self.replay.lock().unwrap().screen.clone();
        eventually("the question followed and asked", || {
            self.h.host.show(&self.id, &screen());
            std::thread::sleep(Duration::from_millis(300));
            let view = self.h.get(&self.id);
            (view.agent_output.is_some() && view.lifecycle == Lifecycle::Blocked).then_some(())
        });
        // A second's read on, the call is read too.
        std::thread::sleep(Duration::from_millis(1_100));
        let (host, id, replay, stop) = (
            self.h.host.clone(),
            self.id.clone(),
            self.replay.clone(),
            self.stop.clone(),
        );
        let terminal = host.terminal_of(&id);
        std::thread::spawn(move || {
            let mut seen = 0;
            while !stop.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(20));
                let keys = host.terminal(terminal).keys;
                let mut replay = replay.lock().unwrap();
                for (keys, text) in &keys[seen..] {
                    let input = match (keys.as_slice(), text) {
                        ([key], None) => Some(Input::Key(key.clone())),
                        ([], Some(text)) => Some(Input::Text(text.clone())),
                        _ => None,
                    };
                    let due = replay.steps.first().map(|step| step.input.clone());
                    if replay.unexpected.is_empty() && input.is_some() && input == due {
                        let step = replay.steps.remove(0);
                        replay.screen = fixture(replay.agent, step.screen);
                        if replay.steps.is_empty() {
                            replay.submitted = true;
                            result();
                        }
                    } else {
                        replay.unexpected.push((keys.clone(), text.clone()));
                    }
                }
                seen = keys.len();
                let screen = replay.screen.clone();
                drop(replay);
                // Kept drawing (a spinner), so the daemon reads the log again.
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

    /// Answered, every step taken as the real dialog took it.
    fn assert_answered(&self, response: Response) {
        assert_eq!(response, Response::Ok);
        let replay = self.replay.lock().unwrap();
        assert!(
            replay.unexpected.is_empty(),
            "unexpected input: {:?}",
            replay.unexpected
        );
        assert!(replay.steps.is_empty(), "{} steps left", replay.steps.len());
        assert!(replay.submitted);
    }
}

impl Drop for Agent {
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

const COLOUR: &str = "Which colour do you prefer?";
const FRUITS: &str = "Which fruits do you like?";
const PET: &str = "Which pet would you keep?";

/// The questions as the agents' calls ask them: colour (one), fruits
/// (several), pet (one), with `flag` for several.
fn asked(which: &[&str], flag: &str, headers: bool) -> Value {
    let one = |question: &str, header: &str, options: &[&str], multi: bool| {
        let mut asked = json!({"question": question, flag: multi, "options": options.iter()
            .map(|label| json!({"label": label, "description": label.to_lowercase()}))
            .collect::<Vec<_>>()});
        if headers {
            asked["header"] = json!(header);
        }
        asked
    };
    let questions: Vec<Value> = which
        .iter()
        .map(|question| match *question {
            COLOUR => one(COLOUR, "Colour", &["Red", "Green", "Blue"], false),
            FRUITS => one(FRUITS, "Fruits", &["Apple", "Banana", "Cherry"], true),
            _ => one(PET, "Pet", &["Cat", "Dog"], false),
        })
        .collect();
    json!({"questions": questions})
}

fn append(path: &Path, entry: Value) {
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .unwrap();
    writeln!(file, "{entry}").unwrap();
}

/// Grok Build's `updates.jsonl` line.
fn grok_line(session: &str, n: u32, update: Value) -> Value {
    json!({"timestamp": 1_793_429_376, "method": "session/update",
        "params": {"sessionId": session, "update": update,
            "_meta": {"eventId": format!("{session}-{n}"),
                "agentTimestampMs": 1_793_429_376_000_i64 + i64::from(n)}}})
}

#[test]
fn grok_builds_three_questions_are_answered_by_number_and_its_result_read() {
    let homes = homes();
    let session = "01a190b8-5c39-7e60-9c77-dff0aa2f8380";
    let pid = std::process::id();
    std::fs::write(
        homes.grok.join("active_sessions.json"),
        json!([{"session_id": session, "pid": pid, "pid_start": 1, "cwd": "/home/me/proj",
            "opened_at": "2026-10-31T06:49:36.000000000Z"}])
        .to_string(),
    )
    .unwrap();
    let dir = homes
        .grok
        .join("sessions/%2Fhome%2Fme%2Fproj")
        .join(session);
    std::fs::create_dir_all(&dir).unwrap();
    let log = dir.join("updates.jsonl");
    append(
        &log,
        grok_line(
            session,
            1,
            json!({"sessionUpdate": "user_message_chunk",
                "content": {"type": "text", "text": "ASK3 now"}}),
        ),
    );
    append(
        &log,
        grok_line(
            session,
            2,
            json!({"sessionUpdate": "tool_call", "toolCallId": "call_3",
                "title": "ask_user_question", "rawInput": asked(&[COLOUR, FRUITS, PET], "multi_select", false),
                "_meta": {"x.ai/tool": {"name": "ask_user_question", "label": "Ask User"}}}),
        ),
    );
    let grok = Agent::new("grok", None, "grok", "multi-q1");
    let result_log = log.clone();
    grok.replay(
        vec![
            key("2", "multi-q2"),
            key("1", "multi-q2-apple"),
            key("3", "multi-q2-apple-cherry"),
            key("z", "multi-q2-z"),
            text("Mango, ripe", "multi-q2-typed"),
            key("enter", "multi-q3"),
            key("1", "idle"),
        ],
        move || {
            append(
                &result_log,
                grok_line(
                    session,
                    3,
                    json!({"sessionUpdate": "tool_call_update", "toolCallId": "call_3",
                        "status": "completed", "rawOutput": {"type": "AskUserQuestion"}}),
                ),
            );
        },
    );
    let answered = grok.answer(
        "call_3",
        vec![
            answer(COLOUR, &["Green"], None),
            answer(FRUITS, &["Cherry", "Apple"], Some("Mango, ripe")),
            answer(PET, &["Cat"], None),
        ],
    );
    grok.assert_answered(answered);
    // Answered: the call is no longer open, and nothing more is typed.
    let again = error(grok.answer("call_3", vec![answer(COLOUR, &["Red"], None)]));
    assert_eq!(again.code, ErrorCode::QuestionGone);
    assert_eq!(grok.keys().len(), 7);
}

/// A Kimi Code conversation in the folder, named by the plugin's hooks.
fn kimi_conversation(kimi: &Agent, id: &str) -> PathBuf {
    let dir = homes()
        .kimi
        .join("sessions/wd_project_0123456789ab")
        .join(id);
    std::fs::create_dir_all(dir.join("agents/main")).unwrap();
    std::fs::write(
        dir.join("state.json"),
        json!({"id": id, "version": 2, "cwd": kimi.h.scope.path(), "archived": false,
            "createdAt": 1, "updatedAt": 2})
        .to_string(),
    )
    .unwrap();
    let report = Report {
        working_directory: Some(AgentWorkingDirectory {
            agent: "kimi".into(),
            conversation_id: Some(id.into()),
            path: kimi.h.scope.path().to_path_buf(),
        }),
        ..Report::default()
    };
    let caller = kimi.h.session_caller(&kimi.id);
    assert_eq!(
        kimi.h.handle(&caller, Request::SessionReport(report)),
        Response::Ok
    );
    dir.join("agents/main/wire.jsonl")
}

fn kimi_event(event: Value) -> Value {
    json!({"type": "context.append_loop_event", "agentId": "main", "event": event,
        "time": 1_793_429_569_857_i64})
}

#[test]
fn kimi_codes_question_is_answered_by_number_then_its_review_submitted() {
    let kimi = Agent::new(
        "kimi-code",
        Some("/home/me/.kimi-code/bin/kimi"),
        "kimi",
        "one-question",
    );
    let log = kimi_conversation(&kimi, "session_3cf29039-5c52-4f2e-96bc-67073d453aa4");
    append(
        &log,
        json!({"type": "metadata", "protocol_version": "1.5", "created_at": 1}),
    );
    append(
        &log,
        kimi_event(json!({"type": "tool.call", "uuid": "f632", "turnId": "1",
            "toolCallId": "call_1", "name": "AskUserQuestion",
            "args": asked(&[COLOUR], "multi_select", true)})),
    );
    let result_log = log.clone();
    kimi.replay(
        vec![
            key("4", "one-other"),
            text("Purple, 2 shades", "one-typed"),
            key("enter", "one-free-review"),
            key("1", "idle"),
        ],
        move || {
            append(
                &result_log,
                kimi_event(json!({"type": "tool.result", "parentUuid": "f632",
                    "toolCallId": "call_1", "result": {"output": "{\"answers\":{}}"}})),
            );
        },
    );
    let answered = kimi.answer(
        "call_1",
        vec![answer(COLOUR, &[], Some("Purple, 2 shades"))],
    );
    kimi.assert_answered(answered);
}

#[test]
fn nothing_is_typed_into_kimi_codes_approval_panel() {
    let kimi = Agent::new(
        "kimi-code",
        Some("/home/me/.kimi-code/bin/kimi"),
        "kimi",
        "permission",
    );
    let log = kimi_conversation(&kimi, "session_4d1a0000-5c52-4f2e-96bc-67073d453aa4");
    append(
        &log,
        kimi_event(json!({"type": "tool.call", "toolCallId": "call_1",
            "name": "AskUserQuestion", "args": asked(&[COLOUR], "multi_select", true)})),
    );
    kimi.replay(Vec::new(), || {});
    let refused = error(kimi.answer("call_1", vec![answer(COLOUR, &["Red"], None)]));
    assert_eq!(refused.code, ErrorCode::QuestionGone);
    assert!(kimi.keys().is_empty());
}

/// An OpenCode session bound to conversation `conversation`.
fn opencode(first: &str, conversation: &str) -> Agent {
    let agent = Agent::new("opencode", None, "open-code", first);
    let caller = agent.h.session_caller(&agent.id);
    assert_eq!(
        agent.h.handle(
            &caller,
            Request::SessionBind {
                agent: "opencode".into(),
                conversations: vec![conversation.into()],
            },
        ),
        Response::Ok
    );
    agent
}

/// OpenCode 2's reply row asking `input` as call `call`, with `status`.
fn v2_reply(call: &str, input: &Value, status: &str) -> String {
    json!({"content": [{"type": "reasoning", "text": "I should ask."},
        {"type": "tool", "id": call, "name": "question",
         "state": {"status": status, "input": input}}]})
    .to_string()
}

#[test]
fn opencode_2s_three_questions_are_answered_by_number_then_reviewed() {
    let db = rusqlite::Connection::open(&homes().opencode).unwrap();
    let input = asked(&[COLOUR, FRUITS, PET], "multiple", true);
    db.execute(
        "INSERT INTO session_message VALUES ('msg_2u', 'ses_two', 4, 'user', 1, ?1)",
        [json!({"text": "ASK3 now"}).to_string()],
    )
    .unwrap();
    db.execute(
        "INSERT INTO session_message VALUES ('msg_2a', 'ses_two', 5, 'assistant', 2, ?1)",
        [v2_reply("call_2", &input, "running")],
    )
    .unwrap();
    let agent = opencode("v2-multi-q1", "ses_two");
    let database = homes().opencode.clone();
    agent.replay(
        vec![
            key("2", "v2-multi-q2"),
            key("1", "v2-multi-q2-apple"),
            key("4", "v2-multi-q2-other"),
            text("Mango, ripe", "v2-multi-q2-typed"),
            key("enter", "v2-multi-q2-saved"),
            key("tab", "v2-multi-q3"),
            key("1", "v2-multi-review"),
            key("enter", "v2-idle"),
        ],
        move || {
            rusqlite::Connection::open(&database)
                .unwrap()
                .execute(
                    "UPDATE session_message SET data = ?1, time_updated = 3 WHERE id = 'msg_2a'",
                    [v2_reply("call_2", &input, "completed")],
                )
                .unwrap();
        },
    );
    let answered = agent.answer(
        "call_2",
        vec![
            answer(COLOUR, &["Green"], None),
            answer(FRUITS, &["Apple"], Some("Mango, ripe")),
            answer(PET, &["Cat"], None),
        ],
    );
    agent.assert_answered(answered);
}

#[test]
fn opencode_1xs_question_is_answered_by_number_and_a_dismissed_one_never() {
    let db = rusqlite::Connection::open(&homes().opencode).unwrap();
    let part = |call: &str, status: &str| {
        json!({"type": "tool", "tool": "question", "callID": call,
            "state": {"status": status, "input": asked(&[COLOUR], "multiple", true)}})
        .to_string()
    };
    for (id, created) in [("msg_1", 10), ("msg_2", 20)] {
        db.execute(
            "INSERT INTO message VALUES (?1, 'ses_one', ?2, ?2, ?3)",
            rusqlite::params![id, created, json!({"role": "assistant"}).to_string()],
        )
        .unwrap();
    }
    db.execute(
        "INSERT INTO part VALUES ('prt_1', 'msg_1', 'ses_one', 10, 10, ?1)",
        [part("call_1", "running")],
    )
    .unwrap();
    let agent = opencode("v1-one-question", "ses_one");
    let database = homes().opencode.clone();
    let done = part("call_1", "completed");
    agent.replay(vec![key("2", "v1-idle")], move || {
        rusqlite::Connection::open(&database)
            .unwrap()
            .execute(
                "UPDATE part SET data = ?1, time_updated = 11 WHERE id = 'prt_1'",
                [done.clone()],
            )
            .unwrap();
    });
    let answered = agent.answer("call_1", vec![answer(COLOUR, &["Green"], None)]);
    agent.assert_answered(answered);
    // Asked again, then dismissed in the terminal (`error`): nothing typed.
    db.execute(
        "INSERT INTO part VALUES ('prt_2', 'msg_2', 'ses_one', 20, 20, ?1)",
        [part("call_2", "error")],
    )
    .unwrap();
    std::thread::sleep(Duration::from_millis(1_500));
    let refused = error(agent.answer("call_2", vec![answer(COLOUR, &["Red"], None)]));
    assert_eq!(refused.code, ErrorCode::QuestionGone);
    assert_eq!(agent.keys().len(), 1);
}
