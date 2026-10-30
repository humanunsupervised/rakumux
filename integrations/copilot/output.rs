//! GitHub Copilot CLI's latest output, from its conversation's event log:
//! `$COPILOT_HOME/session-state/<id>/events.jsonl`, one JSON event a line
//! (`{type, data, id, timestamp, parentId, agentId?}`; a subagent's events
//! carry its `agentId` and are left out). The conversation is the one its
//! process shows ([`Foreground`]), found again on every read: `/clear`, the
//! sessions sidebar and `--resume` switch it. Copilot rewrites the log when
//! a turn is taken back (`/undo`, Esc Esc on a prompt it hasn't answered),
//! and it's then read again from its start.
use super::{contains, first_line, prompt_line, short_path, tool_line, Step};
use crate::host::ProcessInfo;
use crate::resume::{
    copilot_argv_conversation, copilot_log_dir, copilot_open_sessions, copilot_pids,
    copilot_process_log, is_copilot_id, read_process_log, Shown,
};
use serde_json::Value;
use std::path::{Path, PathBuf};

/// Thinking and replies longer than this are cut, in bytes, before they're
/// shaped into a line or a summary's message.
const MAX_BLOCK: usize = 8 * 1024;

/// The event log of conversation `id` under `home`.
pub(super) fn log(home: &Path, id: &str) -> PathBuf {
    home.join("session-state").join(id).join("events.jsonl")
}

/// Which conversation a Copilot process shows, followed as it switches.
#[derive(Debug)]
pub(crate) struct Foreground {
    home: PathBuf,
    process: ProcessInfo,
    /// Its process log, once found, and how far it was read.
    process_log: Option<(PathBuf, u64)>,
    shown: Shown,
    /// The followed event log's file (device, inode), to tell a rewrite.
    file: Option<(u64, u64)>,
}

impl Foreground {
    pub(super) fn new(process: &ProcessInfo) -> Option<Self> {
        Some(Self::under(crate::resume::copilot_home()?, process))
    }

    /// For the Copilot whose home is `home`.
    fn under(home: PathBuf, process: &ProcessInfo) -> Self {
        Self {
            home,
            process: process.clone(),
            process_log: None,
            shown: Shown::default(),
            file: None,
        }
    }

    /// The conversation on screen now, if it's known: its process log's,
    /// else (no log, or one that names none) the one it holds open, its
    /// hooks' (`bound`), the newest it holds open, or its `--resume`. `None`
    /// keeps what was followed (a switch half logged).
    fn current(&mut self, bound: Option<&str>) -> Option<String> {
        if self.process_log.is_none() {
            let dir = copilot_log_dir(&self.process, &self.home);
            self.process_log =
                copilot_process_log(&dir, &copilot_pids(&self.process)).map(|(path, _)| (path, 0));
        }
        if let Some((path, offset)) = &mut self.process_log {
            if let Some(next) = read_process_log(path, *offset, &mut self.shown) {
                *offset = next;
            }
            // A log that names no conversation (a level without them) says nothing.
            if self.shown.current.is_some() || !self.shown.earlier.is_empty() {
                return self.shown.current.clone();
            }
        }
        let open = copilot_open_sessions(&self.home, &copilot_pids(&self.process));
        match open.as_slice() {
            [only] => Some(only.clone()),
            _ => bound
                .filter(|id| is_copilot_id(id))
                .map(String::from)
                .or_else(|| open.into_iter().next())
                .or_else(|| copilot_argv_conversation(&self.process)),
        }
    }

    /// Follow the conversation shown now (`conversation` at `path`), and
    /// read its log from the start when it changed or was rewritten.
    /// Returns whether it did.
    pub(super) fn switch(
        &mut self,
        bound: Option<&str>,
        conversation: &mut Option<String>,
        path: &mut PathBuf,
        offset: &mut u64,
        partial: &mut Vec<u8>,
    ) -> bool {
        let mut restart = false;
        if let Some(current) = self
            .current(bound)
            .filter(|current| conversation.as_deref() != Some(current.as_str()))
        {
            *path = log(&self.home, &current);
            *conversation = Some(current);
            restart = true;
        }
        let metadata = std::fs::metadata(&*path).ok();
        let file = metadata.as_ref().map(identity);
        let shorter = metadata.is_some_and(|metadata| metadata.len() < *offset);
        if !restart && *offset > 0 && (file != self.file || shorter) {
            restart = true;
        }
        self.file = file;
        if restart {
            *offset = 0;
            partial.clear();
        }
        restart
    }

    /// Whether `conversation` was shown here and isn't now: open in the
    /// sidebar, or closed by `/clear`.
    pub(super) fn in_background(&self, conversation: &str) -> bool {
        self.shown.current.as_deref() != Some(conversation)
            && self
                .shown
                .earlier
                .iter()
                .any(|earlier| earlier == conversation)
    }
}

/// A file's (device, inode).
pub(super) fn identity(metadata: &std::fs::Metadata) -> (u64, u64) {
    use std::os::unix::fs::MetadataExt;
    (metadata.dev(), metadata.ino())
}

/// The event's type, when it's the main agent's (not a subagent's).
fn root_kind(entry: &Value) -> Option<&str> {
    if entry.get("agentId").is_some_and(|agent| !agent.is_null()) {
        return None;
    }
    entry.get("type")?.as_str()
}

/// Its reasoning, tool calls and messages, as one line.
pub(super) fn line(entry: &Value) -> Option<String> {
    messages(entry)
        .iter()
        .rev()
        .find_map(|message| first_line(message))
}

/// An assistant message's messages, oldest first: its reasoning and text as
/// written (cut at [`MAX_BLOCK`]), its tool calls as lines.
pub(super) fn messages(entry: &Value) -> Vec<String> {
    if root_kind(entry) != Some("assistant.message") {
        return Vec::new();
    }
    let Some(data) = entry.get("data") else {
        return Vec::new();
    };
    let text = |key: &str| {
        let text = data.get(key)?.as_str()?.trim();
        let end = (0..=text.len().min(MAX_BLOCK))
            .rev()
            .find(|at| text.is_char_boundary(*at))?;
        (!text.is_empty()).then(|| text[..end].to_string())
    };
    let mut messages: Vec<String> = [text("reasoningText"), text("content")]
        .into_iter()
        .flatten()
        .collect();
    messages.extend(requests(data).iter().filter_map(call_line));
    messages
}

/// An assistant message's tool calls.
pub(super) fn requests(data: &Value) -> &[Value] {
    data.get("toolRequests")
        .and_then(Value::as_array)
        .map_or(&[], Vec::as_slice)
}

/// A tool call (`{name, arguments}`) as a line.
fn call_line(request: &Value) -> Option<String> {
    let name = request.get("name")?.as_str()?;
    let arguments = arguments(request.get("arguments"));
    let field = |key: &str| arguments.get(key).and_then(Value::as_str);
    let (label, subject) = match name {
        "bash" | "powershell" => ("Shell", field("command").map(String::from)),
        "view" => ("Read", field("path").map(short_path)),
        "create" => ("Create", field("path").map(short_path)),
        "edit" | "str_replace_editor" => ("Edit", field("path").map(short_path)),
        "apply_patch" => ("Patch", None),
        "grep" => ("Grep", field("pattern").map(String::from)),
        "glob" => ("Glob", field("pattern").map(String::from)),
        "task" => ("Task", field("description").map(String::from)),
        "web_fetch" => ("Fetch", field("url").map(String::from)),
        "ask_user" => ("Ask", field("message").map(String::from)),
        other => (other, None),
    };
    tool_line(label, subject.as_deref())
}

/// A tool call's arguments as an object (Copilot writes an object; a JSON
/// string of one is read too).
pub(super) fn arguments(value: Option<&Value>) -> Value {
    match value {
        Some(Value::String(text)) => serde_json::from_str(text).unwrap_or(Value::Null),
        Some(value) => value.clone(),
        None => Value::Null,
    }
}

/// What the user typed, as one line.
pub(super) fn prompt(entry: &Value) -> Option<String> {
    prompt_line(&typed(entry)?)
}

/// What the user typed (a `user.message` of the main agent), as written:
/// not what Copilot sends itself (`source`: a skill's text, a subagent's
/// task, an autopilot continuation).
pub(super) fn typed(entry: &Value) -> Option<String> {
    if root_kind(entry)? != "user.message" {
        return None;
    }
    let data = entry.get("data")?;
    let injected = data
        .get("source")
        .and_then(Value::as_str)
        .is_some_and(|source| !source.is_empty())
        || data.get("isAutopilotContinuation").and_then(Value::as_bool) == Some(true);
    if injected {
        return None;
    }
    Some(data.get("content")?.as_str()?.to_string())
}

/// Whether a log line may be one [`typed`] takes, before it's parsed.
pub(super) fn may_be_typed(line: &[u8]) -> bool {
    contains(line, br#""user.message""#)
}

/// How an event changes the turn's reply: a prompt or a message with tool
/// calls starts it over, and a message's text adds to it (keyed by its
/// `messageId`).
pub(super) fn reply_step(entry: &Value) -> Step {
    if typed(entry).is_some() {
        return Step::Reset;
    }
    if root_kind(entry) != Some("assistant.message") {
        return Step::Keep;
    }
    let Some(data) = entry.get("data") else {
        return Step::Keep;
    };
    if !requests(data).is_empty() {
        return Step::Reset;
    }
    let text = data
        .get("content")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty());
    match (text, data.get("messageId").and_then(Value::as_str)) {
        (Some(text), Some(key)) => Step::Text {
            key: key.to_owned(),
            text: text.to_owned(),
        },
        _ => Step::Keep,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn event(kind: &str, data: Value) -> Value {
        json!({"type": kind, "data": data, "id": "0d3c9a6e-5b1f-4e2a-9c7d-1f2e3d4c5b6a",
            "timestamp": "2026-10-30T05:27:08.082Z", "parentId": null})
    }

    fn subagent(mut entry: Value) -> Value {
        entry["agentId"] = json!("a8068685-6552-4d55-bfb7-7013dee37803");
        entry
    }

    #[test]
    fn reasoning_tool_calls_and_messages_are_lines() {
        let message = event(
            "assistant.message",
            json!({"messageId": "m1", "content": "", "reasoningText": "**Checking** the parser",
                "toolRequests": [{"toolCallId": "call_1", "name": "bash",
                    "arguments": {"command": "cargo test -p tui", "description": "Run the tests"}}]}),
        );
        assert_eq!(
            messages(&message),
            ["**Checking** the parser", "Shell: cargo test -p tui"]
        );
        assert_eq!(line(&message), Some("Shell: cargo test -p tui".into()));
        let edit = event(
            "assistant.message",
            json!({"messageId": "m2", "content": "Fixing it.", "toolRequests": [
                {"toolCallId": "c2", "name": "edit", "arguments": {"path": "/home/me/project/src/app.rs", "old_str": "secret", "new_str": "x"}},
                {"toolCallId": "c3", "name": "view", "arguments": {"path": "/home/me/project/README.md"}},
                {"toolCallId": "c4", "name": "sql", "arguments": {"query": "secret"}}]}),
        );
        assert_eq!(
            messages(&edit),
            [
                "Fixing it.",
                "Edit: src/app.rs",
                "Read: project/README.md",
                "sql"
            ]
        );
        let reply = event(
            "assistant.message",
            json!({"messageId": "m3", "content": "## Done\nAll tests pass.", "toolRequests": []}),
        );
        assert_eq!(line(&reply), Some("Done".into()));
    }

    #[test]
    fn prompts_tool_output_subagents_and_bookkeeping_are_not_lines() {
        for entry in [
            event("user.message", json!({"content": "my prompt"})),
            event(
                "tool.execution_complete",
                json!({"toolCallId": "c1", "success": true, "result": {"content": "secret output"}}),
            ),
            event(
                "hook.start",
                json!({"hookType": "agentStop", "input": {"prompt": "secret"}}),
            ),
            event(
                "system.message",
                json!({"role": "system", "content": "You are…"}),
            ),
            event("session.usage_record", json!({"usage": {}})),
            subagent(event(
                "assistant.message",
                json!({"messageId": "s1", "content": "A subagent's reply", "toolRequests": []}),
            )),
        ] {
            assert_eq!(line(&entry), None, "{entry}");
        }
    }

    #[test]
    fn what_the_user_typed_is_the_prompt_and_what_copilot_sends_isnt() {
        let typed_message = event(
            "user.message",
            json!({"content": "Fix the flaky test\nthanks", "delivery": "idle",
                "transformedContent": "<current_datetime>…</current_datetime>\n\nFix the flaky test"}),
        );
        assert_eq!(
            typed(&typed_message).as_deref(),
            Some("Fix the flaky test\nthanks")
        );
        assert_eq!(
            prompt(&typed_message).as_deref(),
            Some("Fix the flaky test")
        );
        // Typed while a turn ran.
        let steering = event(
            "user.message",
            json!({"content": "also the docs", "delivery": "steering"}),
        );
        assert_eq!(prompt(&steering).as_deref(), Some("also the docs"));
        for injected in [
            event(
                "user.message",
                json!({"content": "skill text", "source": "skill-pdf"}),
            ),
            event(
                "user.message",
                json!({"content": "go on", "isAutopilotContinuation": true}),
            ),
            subagent(event(
                "user.message",
                json!({"content": "child task", "source": "agent-0471e2d2-e37e-4a26-ab61-7af3f61b8611"}),
            )),
        ] {
            assert_eq!(typed(&injected), None, "{injected}");
        }
        assert!(may_be_typed(typed_message.to_string().as_bytes()));
        assert!(!may_be_typed(
            event("assistant.message", json!({})).to_string().as_bytes()
        ));
    }

    #[test]
    fn the_reply_is_the_text_after_the_last_tool_call() {
        let prompt = event("user.message", json!({"content": "go"}));
        assert_eq!(reply_step(&prompt), Step::Reset);
        let calls = event(
            "assistant.message",
            json!({"messageId": "m1", "content": "Let me look.",
                "toolRequests": [{"toolCallId": "c1", "name": "view", "arguments": {"path": "/a"}}]}),
        );
        assert_eq!(reply_step(&calls), Step::Reset);
        let reply = event(
            "assistant.message",
            json!({"messageId": "m2", "content": "All done.\n", "toolRequests": []}),
        );
        assert_eq!(
            reply_step(&reply),
            Step::Text {
                key: "m2".into(),
                text: "All done.".into()
            }
        );
        // A subagent's reply, its task and other events change nothing.
        for keep in [
            subagent(reply.clone()),
            subagent(event(
                "user.message",
                json!({"content": "child", "source": "agent-x"}),
            )),
            event(
                "tool.execution_complete",
                json!({"toolCallId": "c1", "success": true}),
            ),
            event("assistant.turn_end", json!({"turnId": "1"})),
        ] {
            assert_eq!(reply_step(&keep), Step::Keep, "{keep}");
        }
    }

    use super::super::{Follower, LogAgent, Source, Turn};
    use std::io::Write;

    const A: &str = "0471e2d2-e37e-4a26-ab61-7af3f61b8611";
    const B: &str = "bd0ffe88-2f24-4e0d-8b97-ead37412c634";
    /// A pid no process here has.
    const PID: u32 = 4_000_000_003;

    fn follower(home: &Path) -> Follower {
        let process = ProcessInfo {
            pid: PID,
            name: "MainThread".into(),
            cmdline: vec!["copilot".into()],
            ..ProcessInfo::default()
        };
        Follower {
            pid: PID,
            source: Source::Log {
                agent: LogAgent::Copilot,
                conversation: None,
                path: PathBuf::new(),
                offset: 0,
                partial: Vec::new(),
            },
            latest: None,
            prompt: None,
            turn: Turn::default(),
            reply: None,
            reply_state: Default::default(),
            last_at: None,
            foreground: Some(Foreground::under(home.to_path_buf(), &process)),
        }
    }

    fn append(path: &Path, lines: &[Value]) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .unwrap();
        for line in lines {
            writeln!(file, "{line}").unwrap();
        }
    }

    fn logged(home: &Path, line: &str) {
        let path = home.join(format!("logs/process-1793338015057-{PID}.log"));
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .unwrap();
        writeln!(file, "2026-10-30T05:26:55.083Z [INFO] {line}").unwrap();
    }

    fn turn(prompt: &str, reply: &str, n: u32) -> Vec<Value> {
        vec![
            event(
                "user.message",
                json!({"content": prompt, "messageId": format!("u{n}")}),
            ),
            event(
                "assistant.message",
                json!({"messageId": format!("m{n}"), "content": reply, "toolRequests": []}),
            ),
        ]
    }

    #[test]
    fn the_follower_reads_the_conversation_on_screen_and_switches_with_it() {
        let home = tempfile::tempdir().unwrap();
        let home = home.path();
        let mut follower = follower(home);
        // Nothing known yet: nothing to read.
        assert!(!follower.follow(None));
        assert_eq!(follower.conversation(), None);
        logged(home, &format!("Registering foreground session: {A}"));
        // Known, but its log comes with its first prompt.
        assert!(!follower.follow(None));
        assert_eq!(follower.conversation().as_deref(), Some(A));
        append(&log(home, A), &turn("Fix the parser", "Fixed it.", 1));
        assert!(follower.follow(None));
        assert_eq!(follower.prompt(), Some("Fix the parser"));
        assert_eq!(follower.latest(), Some("Fixed it."));
        assert_eq!(follower.turn().prompt.as_deref(), Some("Fix the parser"));
        assert_eq!(follower.reply().unwrap().key, "m1");
        assert_eq!(follower.agent(), "copilot");
        let source = follower.transcript_source().unwrap();
        assert_eq!(source.agent(), "copilot");
        // The sidebar's new session: B is shown, A is in the background.
        logged(home, &format!("Unregistering foreground session: {A}"));
        logged(home, &format!("Registering foreground session: {B}"));
        append(&log(home, B), &turn("Now the docs", "Writing them.", 2));
        assert!(follower.follow(Some(A)));
        assert_eq!(follower.conversation().as_deref(), Some(B));
        assert_eq!(follower.prompt(), Some("Now the docs"));
        assert_eq!(follower.reply().unwrap().key, "m2");
        assert!(follower.in_background(A));
        assert!(!follower.in_background(B));
        assert!(!follower.in_background("c0ffee00-1111-4222-8333-444455556666"));
        // Back to A: B is the background one.
        logged(home, &format!("Unregistering foreground session: {B}"));
        logged(home, &format!("Registering foreground session: {A}"));
        follower.follow(None);
        assert_eq!(follower.conversation().as_deref(), Some(A));
        assert!(follower.in_background(B) && !follower.in_background(A));
    }

    #[test]
    fn a_turn_its_queued_prompt_ended_keeps_its_reply() {
        let home = tempfile::tempdir().unwrap();
        let home = home.path();
        let mut follower = follower(home);
        logged(home, &format!("Registering foreground session: {A}"));
        follower.follow(None);
        append(&log(home, A), &turn("Fix the parser", "Fixed it.", 1));
        follower.follow(None);
        // As it works on the next prompt, a reply typed meanwhile waits
        // (pending); its answer done, Copilot sends it as the next turn.
        append(
            &log(home, A),
            &[event(
                "user.message",
                json!({"content": "Now the docs", "messageId": "u2"}),
            )],
        );
        follower.follow(None);
        append(
            &log(home, A),
            &[event(
                "assistant.message",
                json!({"messageId": "m2", "content": "Docs written.", "toolRequests": []}),
            )],
        );
        follower.follow(None);
        assert_eq!(follower.take_finished(), [], "none ended yet");
        append(
            &log(home, A),
            &[event(
                "user.message",
                json!({"content": "MIDTURN ping 4242", "messageId": "u3"}),
            )],
        );
        follower.follow(None);
        let finished = follower.take_finished();
        assert_eq!(finished.len(), 1, "{finished:?}");
        assert_eq!(finished[0].reply.key, "m2");
        assert_eq!(finished[0].reply.text, "Docs written.");
        assert_eq!(finished[0].conversation.as_deref(), Some(A));
        // Steered in between tool steps: the turn has no reply of its own.
        append(
            &log(home, A),
            &[
                event(
                    "assistant.message",
                    json!({"messageId": "m3", "content": "Running it.",
                        "toolRequests": [{"toolCallId": "c1", "name": "bash",
                            "arguments": {"command": "sleep 12"}}]}),
                ),
                event(
                    "user.message",
                    json!({"content": "MIDTURN pong 777", "messageId": "u4"}),
                ),
            ],
        );
        follower.follow(None);
        assert_eq!(follower.take_finished(), []);
        // The turn read from the log's past (another conversation shown)
        // isn't one that ended as the log grew.
        logged(home, &format!("Unregistering foreground session: {A}"));
        logged(home, &format!("Registering foreground session: {B}"));
        let mut past = turn("Old", "Old answer.", 5);
        past.extend(turn("Older", "Older answer.", 6));
        append(&log(home, B), &past);
        follower.follow(None);
        assert_eq!(follower.conversation().as_deref(), Some(B));
        assert_eq!(follower.take_finished(), []);
    }

    #[test]
    fn a_process_log_that_names_no_conversation_falls_back_to_the_open_one() {
        let home = tempfile::tempdir().unwrap();
        let home = home.path();
        let mut follower = follower(home);
        logged(home, "[ERROR] something went wrong");
        std::fs::create_dir_all(home.join(format!("session-state/{A}"))).unwrap();
        std::fs::write(
            home.join(format!("session-state/{A}/inuse.{PID}.lock")),
            "x",
        )
        .unwrap();
        append(&log(home, A), &turn("Fix the parser", "Fixed it.", 1));
        assert!(follower.follow(None));
        assert_eq!(follower.conversation().as_deref(), Some(A));
    }

    #[test]
    fn a_rewritten_log_is_read_again_from_its_start() {
        let home = tempfile::tempdir().unwrap();
        let home = home.path();
        let mut follower = follower(home);
        logged(home, &format!("Registering foreground session: {A}"));
        let path = log(home, A);
        let mut lines = turn("First prompt", "First reply.", 1);
        append(&path, &lines);
        assert!(follower.follow(None));
        append(&path, &turn("Second prompt", "Second reply.", 2));
        assert!(follower.follow(None));
        assert_eq!(follower.prompt(), Some("Second prompt"));
        let second = follower.turn().id;
        // The second turn taken back (Esc Esc): Copilot writes the log anew
        // and renames it into place, then a longer prompt than before follows.
        lines.extend(turn(
            &format!("Third {}", "x".repeat(400)),
            "Third reply.",
            3,
        ));
        let rewritten = path.with_extension("tmp");
        append(&rewritten, &lines);
        std::fs::rename(&rewritten, &path).unwrap();
        assert!(follower.follow(None));
        assert!(follower.prompt().unwrap().starts_with("Third"));
        assert_ne!(follower.turn().id, second);
        assert_eq!(follower.reply().unwrap().key, "m3");
        // The transcript reads the new file from its start, whatever the
        // old cursor said.
        let source = follower.transcript_source().unwrap();
        let page = source.read(None, None).unwrap();
        let stale = session_core::api::TranscriptCursor {
            key: Some("1:1".into()),
            ..page.cursor.clone()
        };
        assert_eq!(source.read(Some(&stale), None).unwrap().messages.len(), 4);
        assert!(source
            .read(Some(&page.cursor), None)
            .unwrap()
            .messages
            .is_empty());
    }

    #[test]
    fn the_process_log_says_which_conversation_is_shown() {
        let (a, b) = (
            "0471e2d2-e37e-4a26-ab61-7af3f61b8611",
            "bd0ffe88-2f24-4e0d-8b97-ead37412c634",
        );
        let mut shown = Shown::default();
        shown.line(&format!(
            "2026-10-30T05:26:55.083Z [INFO] Registering foreground session: {a}"
        ));
        assert_eq!(shown.current.as_deref(), Some(a));
        // The sidebar's new session.
        shown.line(&format!(
            "2026-10-30T05:31:13.587Z [INFO] Unregistering foreground session: {a}"
        ));
        assert_eq!(shown.current, None);
        shown.line(&format!(
            "2026-10-30T05:31:13.589Z [INFO] Registering foreground session: {b}"
        ));
        assert_eq!(shown.current.as_deref(), Some(b));
        assert_eq!(shown.earlier, [a]);
        // Back to the first.
        shown.line(&format!("x Registering foreground session: {a}"));
        assert_eq!(shown.current.as_deref(), Some(a));
        assert_eq!(shown.earlier, [b]);
        // Other lines, and ids that aren't Copilot's, change nothing.
        let before = shown.clone();
        shown.line("2026-10-30T05:26:20.567Z [INFO] Workspace initialized: x");
        shown.line("x Registering foreground session: ../../etc");
        assert_eq!(shown, before);
    }
}
