//! Kimi Code through the daemon, on Kimi Code 2.4.0's own log format and
//! screens: the conversation its plugin's hooks name (else the one it holds
//! a lock on, else its folder's newest) is followed from
//! `$KIMI_CODE_HOME/sessions/…/agents/main/wire.jsonl` for the session's
//! lines, turn, reply, whole chat, places and resume command, and a reply
//! typed in shows up as a new turn.
#![cfg(target_os = "linux")]

mod control_common;

use control_common::*;
use ptyd::control_api::Control;
use ptyd::host::ProcessInfo;
use serde_json::{json, Value};
use session_core::api::{
    AgentReply, AgentWorkingDirectory, EventEnvelope, Report, Request, Response, SessionEvent,
    SubscribeRequest, TranscriptPart, TranscriptRole,
};
use session_core::ids::SessionId;
use session_core::model::{Lifecycle, ResumeSource};
use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

/// Each test points the daemon at a Kimi home of its own
/// (`KIMI_CODE_HOME`), and Kimi is this process: they take turns.
static KIMI: std::sync::Mutex<()> = std::sync::Mutex::new(());

const IDLE: &str = " ╭──────────────────────────╮\n │ >                        │\n ╰──────────────────────────╯\n mock thinking  ~/project\n                       context: 1% (120/256k)";
const WORKING: &str = "  ⠸ Working… · Tip: /web: use the Web UI\n ╭──────────────────────────╮\n │ >                        │\n ╰──────────────────────────╯\n mock thinking  ~/project\n                       context: 1% (120/256k)";

struct Kimi {
    _turn: std::sync::MutexGuard<'static, ()>,
    _dir: tempfile::TempDir,
    h: Harness,
    home: PathBuf,
    id: SessionId,
}

impl Kimi {
    /// A Rakumux session, and a Kimi home with no conversation yet.
    fn new() -> Self {
        let turn = KIMI.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let home = std::fs::canonicalize(dir.path()).unwrap().join("kimi-home");
        std::fs::create_dir_all(&home).unwrap();
        // SAFETY: tests that read it take turns (`KIMI`).
        unsafe { std::env::set_var("KIMI_CODE_HOME", &home) };
        let h = Harness::new();
        let id = h.create(Some("kimi")).id;
        Self {
            _turn: turn,
            _dir: dir,
            h,
            home,
            id,
        }
    }

    /// Kimi Code's native binary, started as `kimi -r …`: titled
    /// `kimi-code`, its arguments wiped. This process stands in for it.
    fn start(&self) {
        self.h.host.set_foreground(
            &self.id,
            ProcessInfo {
                pid: std::process::id(),
                name: "kimi-code".into(),
                cmdline: vec!["kimi-code".into()],
                exe: Some("/home/me/.kimi-code/bin/kimi".into()),
            },
        );
    }

    /// What the plugin's hooks say: this conversation, in the session's
    /// folder.
    fn hooks_name(&self, conversation: &str) {
        let report = Report {
            working_directory: Some(AgentWorkingDirectory {
                agent: "kimi".into(),
                conversation_id: Some(conversation.into()),
                path: self.h.scope.path().to_path_buf(),
            }),
            ..Report::default()
        };
        let caller = self.h.session_caller(&self.id);
        assert_eq!(
            self.h.handle(&caller, Request::SessionReport(report)),
            Response::Ok
        );
    }

    /// A conversation of the folder `cwd`, updated at `updated`.
    fn conversation(&self, id: &str, cwd: &Path, updated: i64) -> Log {
        let slug = cwd.file_name().unwrap().to_str().unwrap().to_lowercase();
        let dir = self
            .home
            .join("sessions")
            .join(format!("wd_{slug}_0123456789ab"))
            .join(id);
        std::fs::create_dir_all(dir.join("agents/main")).unwrap();
        std::fs::write(
            dir.join("state.json"),
            json!({"id": id, "version": 2, "cwd": cwd, "archived": false,
                "createdAt": 1, "updatedAt": updated})
            .to_string(),
        )
        .unwrap();
        let mut log = Log {
            file: File::create(dir.join("agents/main/wire.jsonl")).unwrap(),
            at: 0,
        };
        log.record(json!({"type": "metadata", "protocol_version": "1.5", "created_at": 1}));
        log
    }

    fn show(&self, screen: &str) {
        self.h.host.show(&self.id, screen);
    }

    /// Keep the screen moving (the log is read while it does) until `f`.
    fn eventually<T>(&self, what: &str, screen: &str, mut f: impl FnMut() -> Option<T>) -> T {
        let mut frame = 0;
        eventually(what, || {
            frame += 1;
            self.show(&format!("{screen}\n{}", " ".repeat(frame % 3)));
            f()
        })
    }

    fn settle(&self, screen: &str, lifecycle: Lifecycle) {
        self.eventually(&format!("{lifecycle:?}"), screen, || {
            (self.h.get(&self.id).lifecycle == lifecycle).then_some(())
        });
    }

    fn replies(&self) -> Receiver<EventEnvelope> {
        assert_eq!(
            self.h
                .handle(&self.h.user, Request::ReplyCapture { enabled: true }),
            Response::Ok
        );
        self.h
            .control
            .subscribe(
                &self.h.user,
                &SubscribeRequest {
                    all: true,
                    replies: true,
                    ..Default::default()
                },
            )
            .unwrap()
    }
}

/// A conversation's log, as Kimi writes it: a record per line, each at its
/// byte offset.
struct Log {
    file: File,
    at: u64,
}

impl Log {
    /// Append `record`; returns where it starts.
    fn record(&mut self, mut record: Value) -> u64 {
        if record.get("time").is_none() {
            record["time"] = json!(1_793_338_216_205_i64);
        }
        let line = format!("{record}\n");
        self.file.write_all(line.as_bytes()).unwrap();
        let at = self.at;
        self.at += line.len() as u64;
        at
    }

    fn event(&mut self, event: Value) -> u64 {
        self.record(json!({"type": "context.append_loop_event", "agentId": "main", "event": event}))
    }

    /// The user's prompt, as Kimi logs one typed (or pasted) at its prompt.
    fn prompt(&mut self, text: &str, turn: u32) {
        let id = format!("msg_01M65MK3QGXRCJZBQNYFXV6B{turn:02}");
        let content = json!([{"type": "text", "text": text}]);
        self.record(
            json!({"type": "turn.prompt", "agentId": "main", "input": content,
            "origin": {"kind": "user"}, "promptId": id, "turnId": turn}),
        );
        self.record(
            json!({"type": "context.append_message", "agentId": "main", "message": {
            "role": "user", "content": content, "id": id, "toolCalls": [],
            "origin": {"kind": "user"}}}),
        );
        self.record(json!({"type": "context.append_message", "agentId": "main", "message": {
            "role": "user", "toolCalls": [], "origin": {"kind": "injection", "variant": "date_change"},
            "content": [{"type": "text", "text": "<system-reminder>\nToday's date…\n</system-reminder>"}]}}));
        self.record(json!({"type": "llm.tools_snapshot", "agentId": "main",
            "tools": [{"name": "Write", "description": "x".repeat(4096)}]}));
    }

    fn part(&mut self, uuid: &str, kind: &str, text: &str) -> u64 {
        let part = match kind {
            "think" => json!({"type": "think", "think": text, "reasoningKey": "reasoning_content"}),
            _ => json!({"type": "text", "text": text}),
        };
        self.event(
            json!({"type": "content.part", "uuid": uuid, "turnId": "0", "step": 1,
            "stepUuid": "s", "part": part}),
        )
    }

    fn ended(&mut self, turn: u32, reason: &str) {
        self.record(
            json!({"type": "turn.ended", "agentId": "main", "turnId": turn,
            "reason": reason, "durationMs": 95}),
        );
    }
}

fn replies_in(events: &Receiver<EventEnvelope>, wait: Duration) -> Vec<AgentReply> {
    let deadline = Instant::now() + wait;
    let mut replies = Vec::new();
    while let Some(left) = deadline.checked_duration_since(Instant::now()) {
        match events.recv_timeout(left) {
            Ok(EventEnvelope {
                event: SessionEvent::Reply { reply },
                ..
            }) => replies.push(reply),
            Ok(_) => {}
            Err(_) => break,
        }
    }
    replies
}

#[test]
fn the_conversation_its_hooks_name_gives_the_lines_turn_reply_chat_places_and_resume() {
    let kimi = Kimi::new();
    let (h, id) = (&kimi.h, &kimi.id);
    // The session's folder is a git checkout, where Kimi will write a file.
    let project = h.scope.path().to_path_buf();
    std::fs::create_dir_all(project.join(".git")).unwrap();
    std::fs::create_dir_all(project.join("src")).unwrap();
    std::fs::write(project.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
    let conversation = "session_9e550655-6a9f-4f15-b633-3839522633c0";
    let mut log = kimi.conversation(conversation, &project, 100);
    kimi.hooks_name(conversation);
    kimi.start();
    let replies = kimi.replies();
    kimi.settle(IDLE, Lifecycle::Idle);
    // Resumed as Kimi prints it.
    let resume = kimi.eventually("its resume command", IDLE, || {
        h.get(id)
            .resume
            .filter(|resume| resume.source == ResumeSource::KnownAgent)
    });
    assert_eq!(resume.argv, ["kimi", "-r", conversation]);

    kimi.settle(WORKING, Lifecycle::Working);
    log.prompt("Fix the parser\nso the tests pass", 0);
    log.part("c1", "think", "I will write the file.");
    log.event(
        json!({"type": "tool.call", "uuid": "t1", "toolCallId": "call_write_1",
        "name": "Write", "args": {"path": project.join("src/lib.rs"), "content": "fn main() {}"},
        "display": {"kind": "file_io", "operation": "write", "path": project.join("src/lib.rs"),
            "content": "fn main() {}"}}),
    );
    log.event(
        json!({"type": "tool.result", "parentUuid": "t1", "toolCallId": "call_write_1",
        "result": {"output": "Wrote 12 bytes", "durationMs": 2}}),
    );
    log.part("c2", "think", "The tool finished; summarising.");
    let reply_at = log.part(
        "c3",
        "text",
        "Fixed: the parser reads the tail.\n\nTests pass.",
    );
    log.ended(0, "completed");

    // Every client sees its lines and the turn's count, never its text.
    let view = kimi.eventually("the turn to be followed", WORKING, || {
        let view = h.get(id);
        (view.agent_turn.as_ref()?.messages == 4).then_some(view)
    });
    assert_eq!(view.agent.as_ref().unwrap().kind, "kimi");
    assert_eq!(view.user_prompt.as_deref(), Some("Fix the parser"));
    assert_eq!(
        view.agent_output.as_deref(),
        Some("Fixed: the parser reads the tail.")
    );
    assert_eq!(
        view.agent_turn.unwrap().conversation_id.as_deref(),
        Some(conversation)
    );
    match h.handle(&h.user, Request::SessionTurn { id: id.clone() }) {
        Response::Turn { turn } => {
            assert_eq!(
                turn.prompt.as_deref(),
                Some("Fix the parser so the tests pass")
            );
            assert_eq!(
                turn.messages,
                [
                    "I will write the file.",
                    "Write: src/lib.rs",
                    "The tool finished; summarising.",
                    "Fixed: the parser reads the tail. Tests pass."
                ]
            );
        }
        other => panic!("expected a turn, got {other:?}"),
    }

    // Finished: its reply, once, with where its turn ends in the chat.
    kimi.settle(IDLE, Lifecycle::Done);
    let got = replies_in(&replies, Duration::from_secs(3));
    assert_eq!(got.len(), 1, "{got:?}");
    assert_eq!(got[0].agent, "kimi");
    assert_eq!(got[0].conversation_id.as_deref(), Some(conversation));
    assert_eq!(
        got[0].text,
        "Fixed: the parser reads the tail.\n\nTests pass."
    );
    assert_eq!(got[0].position, Some(reply_at));

    // The whole chat, from Kimi's log: the prompt (not Kimi's reminder), the
    // reasoning, the call and its result, the reply.
    let page = match h.handle(
        &h.user,
        Request::SessionTranscript {
            id: id.clone(),
            after: None,
            max_bytes: None,
        },
    ) {
        Response::Transcript { transcript } => transcript,
        other => panic!("expected a transcript, got {other:?}"),
    };
    assert_eq!(
        (page.agent.as_str(), page.conversation_id.as_str()),
        ("kimi", conversation)
    );
    let roles: Vec<_> = page.messages.iter().map(|message| message.role).collect();
    assert_eq!(
        roles,
        [
            TranscriptRole::User,
            TranscriptRole::Assistant,
            TranscriptRole::Assistant,
            TranscriptRole::Tool,
            TranscriptRole::Assistant,
            TranscriptRole::Assistant
        ]
    );
    assert_eq!(page.messages[0].id, "msg_01M65MK3QGXRCJZBQNYFXV6B00");
    assert!(matches!(&page.messages[2].parts[0],
        TranscriptPart::ToolCall { call_id: Some(call), name, .. }
            if call == "call_write_1" && name == "Write"));
    assert_eq!(page.messages.last().unwrap().position, reply_at);
    assert!(!page.more);

    // Where it changed files: the checkout.
    let places = kimi.eventually("its places", IDLE, || {
        let places = h.get(id).places;
        (!places.is_empty()).then_some(places)
    });
    assert_eq!(places[0].path, project);
    assert!(places[0].edited);
    assert_eq!(
        places[0].git.as_ref().unwrap().branch.as_deref(),
        Some("main")
    );
}

#[test]
fn a_reply_typed_in_is_a_new_turn_and_a_cancelled_turn_has_no_reply() {
    let kimi = Kimi::new();
    let (h, id) = (&kimi.h, &kimi.id);
    let conversation = "session_0c5f2bb0-7c95-4e88-9e10-df2cd6151e76";
    let mut log = kimi.conversation(conversation, h.scope.path(), 100);
    kimi.hooks_name(conversation);
    kimi.start();
    let replies = kimi.replies();
    kimi.settle(WORKING, Lifecycle::Working);
    log.prompt("Fix the parser", 0);
    log.part("c1", "text", "Fixed.");
    log.ended(0, "completed");
    kimi.settle(IDLE, Lifecycle::Done);
    assert_eq!(replies_in(&replies, Duration::from_secs(3)).len(), 1);
    let first = h.get(id).agent_turn.unwrap();

    // Written back from the feed: pasted and entered while Kimi waits.
    h.handle(&h.user, Request::SessionTouch { id: id.clone() });
    let submit = Request::SessionSubmit {
        id: id.clone(),
        text: "Now the docs".into(),
        quiet_ms: Some(10_000),
        expect_foreground: Some("kimi-code".into()),
    };
    assert_eq!(h.handle(&h.user, submit), Response::Ok);
    let terminal = h.host.terminal_of(id);
    assert_eq!(h.host.terminal(terminal).submits, ["Now the docs"]);
    // Kimi logs it as a prompt: the account service sees it delivered.
    kimi.settle(WORKING, Lifecycle::Working);
    log.prompt("Now the docs", 1);
    log.part("c2", "text", "Half of the docs");
    // Esc: the turn is cancelled.
    log.ended(1, "cancelled");
    let view = kimi.eventually("the typed prompt", WORKING, || {
        let view = h.get(id);
        (view.user_prompt.as_deref() == Some("Now the docs")).then_some(view)
    });
    let turn = view.agent_turn.unwrap();
    assert_ne!(turn.id, first.id, "a new turn");
    assert_eq!(turn.conversation_id.as_deref(), Some(conversation));
    kimi.settle(IDLE, Lifecycle::Done);
    assert!(
        replies_in(&replies, Duration::from_millis(1_500)).is_empty(),
        "a cancelled turn has no reply"
    );
}

#[test]
fn without_hooks_the_lock_names_the_conversation_and_else_the_folders_newest() {
    let kimi = Kimi::new();
    let (h, id) = (&kimi.h, &kimi.id);
    // Kimi never changes its directory: its folder is its process's.
    let folder = std::fs::canonicalize(std::env::current_dir().unwrap()).unwrap();
    let mut older = kimi.conversation("session_older", &folder, 100);
    let mut newer = kimi.conversation("session_newer", &folder, 200);
    older.prompt("An older chat", 0);
    newer.prompt("The newest chat", 0);
    kimi.start();
    let resume = kimi.eventually("its resume command", IDLE, || {
        h.get(id)
            .resume
            .filter(|resume| resume.source == ResumeSource::KnownAgent)
    });
    assert_eq!(resume.argv, ["kimi", "-r", "session_newer"]);
    let chat = |view: session_core::model::SessionView| {
        view.agent_turn.and_then(|turn| turn.conversation_id)
    };
    kimi.eventually("the folder's newest chat", IDLE, || {
        (chat(h.get(id)).as_deref() == Some("session_newer")).then_some(())
    });
    // A turn in the other one: Kimi holds its lock while it runs.
    let locks = kimi.home.join("locks/sessions");
    std::fs::create_dir_all(&locks).unwrap();
    std::fs::write(
        locks.join("session_older.lock"),
        json!({"version": 1, "token": format!("{}:x", std::process::id()),
            "pid": std::process::id(), "hostname": "box", "clientIdentity": "kimi_code_cli"})
        .to_string(),
    )
    .unwrap();
    let view = kimi.eventually("the locked chat", WORKING, || {
        let view = h.get(id);
        (chat(view.clone()).as_deref() == Some("session_older")).then_some(view)
    });
    assert_eq!(view.user_prompt.as_deref(), Some("An older chat"));
    // Its turn over, the lock gone: the chat followed stays.
    std::fs::remove_file(locks.join("session_older.lock")).unwrap();
    older.part("c1", "text", "Still this one.");
    let view = kimi.eventually("the chat to stay", IDLE, || {
        let view = h.get(id);
        (view.agent_output.as_deref() == Some("Still this one.")).then_some(view)
    });
    assert_eq!(chat(view).as_deref(), Some("session_older"));
}
