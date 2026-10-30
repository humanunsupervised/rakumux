//! Kimi Code's latest output, from its main agent's log
//! (`sessions/<folder>/<session>/agents/main/wire.jsonl`, one JSON record
//! per line, each with its `time` in Unix milliseconds).
//!
//! What the user typed is a `context.append_message` whose origin is the
//! user's (a steer typed during a turn too); Kimi's own insertions
//! (reminders, hook results, subagents' prompts) have other origins. The
//! agent's reasoning, text and tool calls are `context.append_loop_event`s
//! (`content.part`, `tool.call`), written as each step ends. A turn starts
//! at `turn.prompt` and ends at `turn.ended`, whose `reason` says whether it
//! completed. Subagents write logs of their own, never read here.
//!
//! The conversation is looked up on every read: one process switches
//! between them (`/new`, `/sessions`).
use super::{contains, first_line, prompt_line, short_path, tool_line, Step};
use serde_json::Value;
use std::path::PathBuf;

/// Follow the Kimi Code process `pid` to the conversation it runs now (see
/// `resume::kimi_conversation`; the one followed is kept while nothing
/// names another): a new log is read from its tail. Returns whether it
/// switched.
pub(super) fn switch(
    pid: u32,
    bound: Option<&str>,
    conversation: &mut Option<String>,
    path: &mut PathBuf,
    offset: &mut u64,
    partial: &mut Vec<u8>,
) -> bool {
    let Some(home) = crate::resume::kimi_home() else {
        return false;
    };
    let current = match (bound, crate::resume::kimi_locked_session(&home, pid)) {
        (Some(bound), _) => Some(bound.to_owned()),
        (None, Some(locked)) => Some(locked),
        (None, None) => match conversation {
            Some(followed) => Some(followed.clone()),
            None => crate::procinfo::cwd_of(pid)
                .and_then(|cwd| crate::resume::kimi_recent_session(&home, &cwd)),
        },
    };
    let Some(current) = current.filter(|id| crate::resume::is_conversation_id(id)) else {
        return false;
    };
    if conversation.as_deref() == Some(current.as_str()) {
        return false;
    }
    let Some(log) = crate::resume::kimi_wire_log(&home, &current) else {
        return false;
    };
    *path = log;
    *offset = 0;
    partial.clear();
    *conversation = Some(current);
    true
}

/// When a record was written (Unix milliseconds).
pub(super) fn at(entry: &Value) -> Option<i64> {
    entry.get("time")?.as_i64()
}

/// The main agent's (a record without an agent is its log's).
fn main_agent(entry: &Value) -> bool {
    entry
        .get("agentId")
        .and_then(Value::as_str)
        .is_none_or(|agent| agent == "main")
}

/// A loop event of the main agent: `step.begin`, `content.part`,
/// `tool.call`, `tool.result`, `step.end`.
pub(super) fn loop_event(entry: &Value) -> Option<&Value> {
    (entry.get("type")?.as_str()? == "context.append_loop_event" && main_agent(entry))
        .then(|| entry.get("event"))
        .flatten()
}

/// Its reasoning, tool calls and text, as one line.
pub(super) fn line(entry: &Value) -> Option<String> {
    messages(entry)
        .iter()
        .rev()
        .find_map(|message| first_line(message))
}

/// A record's messages: reasoning and text as written, a tool call as a
/// line. Hidden or encrypted reasoning says nothing.
pub(super) fn messages(entry: &Value) -> Vec<String> {
    let Some(event) = loop_event(entry) else {
        return Vec::new();
    };
    let message = match event.get("type").and_then(Value::as_str) {
        Some("content.part") => part_text(event.get("part")),
        Some("tool.call") => call(event),
        _ => None,
    };
    message.into_iter().collect()
}

/// A content part's text: a `think` part's visible reasoning, a `text`
/// part's text.
fn part_text(part: Option<&Value>) -> Option<String> {
    let part = part?;
    let text = match part.get("type")?.as_str()? {
        "think" if part.get("hidden").and_then(Value::as_bool) != Some(true) => {
            part.get("think")?.as_str()?
        }
        "text" => part.get("text")?.as_str()?,
        _ => return None,
    };
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_owned())
}

/// A tool call as a line: `Bash: <description>`, `Write: src/app.rs`.
fn call(event: &Value) -> Option<String> {
    let name = event.get("name")?.as_str()?;
    let field = |key: &str| event.get("args")?.get(key)?.as_str();
    let subject = match name {
        // Its own one-line description says more than the command.
        "Bash" => field("description")
            .or_else(|| field("command"))
            .map(String::from),
        "Read" | "Write" | "Edit" => field("path").map(short_path),
        "Grep" | "Glob" => field("pattern").map(String::from),
        "FetchURL" => field("url").map(String::from),
        "WebSearch" => field("query").map(String::from),
        "Agent" => field("description").map(String::from),
        _ => None,
    };
    tool_line(name, subject.as_deref())
}

/// What the user typed, as one line.
pub(super) fn prompt(entry: &Value) -> Option<String> {
    prompt_line(&typed(entry)?)
}

/// What the user typed (a prompt, or a steer during a turn), as written:
/// a message to the main agent whose origin is the user's. Its text parts;
/// pictures and other media aren't text.
pub(super) fn typed(entry: &Value) -> Option<String> {
    let message = typed_message(entry)?;
    let texts: Vec<&str> = message
        .get("content")?
        .as_array()?
        .iter()
        .filter(|part| part.get("type").and_then(Value::as_str) == Some("text"))
        .filter_map(|part| part.get("text").and_then(Value::as_str))
        .collect();
    (!texts.is_empty()).then(|| texts.join("\n"))
}

/// A message the user typed to the main agent.
pub(super) fn typed_message(entry: &Value) -> Option<&Value> {
    if entry.get("type")?.as_str()? != "context.append_message" || !main_agent(entry) {
        return None;
    }
    let message = entry.get("message")?;
    let from_user = message.get("role")?.as_str()? == "user"
        && message.get("origin")?.get("kind")?.as_str()? == "user";
    from_user.then_some(message)
}

/// Whether a log line may be one [`typed`] takes, before it's parsed.
pub(super) fn may_be_typed(line: &[u8]) -> bool {
    contains(line, br#""type":"context.append_message""#) && contains(line, br#""kind":"user""#)
}

/// How a record changes the turn's reply: a new turn, a prompt or steer, or
/// a tool call starts it over; the main agent's text adds to it (one part
/// per record); a turn that didn't complete (cancelled, failed) has none.
pub(super) fn reply_step(entry: &Value) -> Step {
    if !main_agent(entry) {
        return Step::Keep;
    }
    match entry.get("type").and_then(Value::as_str) {
        Some("turn.prompt") => Step::Reset,
        Some("context.append_message") if typed_message(entry).is_some() => Step::Reset,
        Some("turn.ended") if entry.get("reason").and_then(Value::as_str) != Some("completed") => {
            Step::Reset
        }
        Some("context.append_loop_event") => {
            let Some(event) = entry.get("event") else {
                return Step::Keep;
            };
            match event.get("type").and_then(Value::as_str) {
                Some("tool.call") => Step::Reset,
                Some("content.part") => {
                    let part = event.get("part");
                    let text = part
                        .filter(|part| part.get("type").and_then(Value::as_str) == Some("text"))
                        .and_then(|part| part_text(Some(part)));
                    match (text, event.get("uuid").and_then(Value::as_str)) {
                        (Some(text), Some(key)) => Step::Text {
                            key: key.to_owned(),
                            text,
                        },
                        _ => Step::Keep,
                    }
                }
                _ => Step::Keep,
            }
        }
        _ => Step::Keep,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn event_record(event: Value) -> Value {
        json!({"type": "context.append_loop_event", "agentId": "main", "event": event,
            "time": 1_793_338_216_205_i64})
    }

    fn part(kind: &str, key: &str, text: &str) -> Value {
        let part = match kind {
            "think" => json!({"type": "think", "think": text, "reasoningKey": "reasoning_content"}),
            _ => json!({"type": "text", "text": text}),
        };
        event_record(
            json!({"type": "content.part", "uuid": key, "turnId": "0", "step": 1,
            "stepUuid": "s1", "part": part}),
        )
    }

    fn message(kind: &str, text: &str) -> Value {
        json!({"type": "context.append_message", "agentId": "main", "message": {
            "role": "user", "content": [{"type": "text", "text": text}], "id": "msg_01",
            "toolCalls": [], "origin": {"kind": kind}}, "time": 1})
    }

    #[test]
    fn reasoning_tool_calls_and_text_are_lines() {
        assert_eq!(
            line(&part(
                "think",
                "u1",
                "I will write the file.\nThen test it."
            )),
            Some("I will write the file.".into())
        );
        assert_eq!(
            line(&event_record(json!({"type": "tool.call", "uuid": "u2",
                "toolCallId": "call_1", "name": "Write",
                "args": {"path": "/repo/tui/src/app/account.rs", "content": "secret"},
                "display": {"kind": "file_io", "operation": "write"}}))),
            Some("Write: app/account.rs".into())
        );
        assert_eq!(
            line(&event_record(json!({"type": "tool.call", "name": "Bash",
                "args": {"command": "cargo test", "description": "Run the tests"}}))),
            Some("Bash: Run the tests".into())
        );
        assert_eq!(
            line(&event_record(
                json!({"type": "tool.call", "name": "mcp__rakumux__list_sessions",
                "args": {}})
            )),
            Some("mcp__rakumux__list_sessions".into())
        );
        assert_eq!(
            line(&part("text", "u3", "## Done\nThe tests pass.")),
            Some("Done".into())
        );
        // Tool output, hidden reasoning and bookkeeping aren't lines.
        for entry in [
            event_record(json!({"type": "tool.result", "toolCallId": "call_1",
                "result": {"output": "secret output"}})),
            event_record(json!({"type": "content.part", "uuid": "u4",
                "part": {"type": "think", "think": "hidden", "hidden": true}})),
            json!({"type": "llm.request", "agentId": "main", "model": "x"}),
            json!({"type": "context.append_loop_event", "agentId": "agent-0",
                "event": {"type": "content.part", "part": {"type": "text", "text": "a subagent's"}}}),
        ] {
            assert_eq!(line(&entry), None, "{entry}");
        }
        assert_eq!(at(&part("text", "u3", "x")), Some(1_793_338_216_205));
    }

    #[test]
    fn what_the_user_typed_is_a_message_from_the_user() {
        assert_eq!(
            prompt(&message("user", "Fix the parser\nplease")),
            Some("Fix the parser".into())
        );
        // A steer typed during a turn is typed too.
        let mut steer = message("user", "Use the other test");
        steer["message"]["origin"]["inTurn"] = json!(true);
        assert_eq!(typed(&steer).as_deref(), Some("Use the other test"));
        // Kimi's reminders, hook results and subagents' prompts aren't.
        for kind in ["injection", "system_trigger", "hook_result"] {
            assert_eq!(
                typed(&message(kind, "<system-reminder>x</system-reminder>")),
                None
            );
        }
        // A picture alone has no text.
        let picture = json!({"type": "context.append_message", "agentId": "main",
            "message": {"role": "user", "origin": {"kind": "user"},
            "content": [{"type": "image_url", "imageUrl": {"url": "data:…"}}]}});
        assert_eq!(typed(&picture), None);
        let line = serde_json::to_vec(&message("user", "hi")).unwrap();
        assert!(may_be_typed(&line));
        let line = serde_json::to_vec(&message("injection", "hi")).unwrap();
        assert!(!may_be_typed(&line));
    }

    #[test]
    fn the_reply_is_the_text_after_the_last_tool_call_of_a_completed_turn() {
        assert_eq!(
            reply_step(&part("text", "u1", " All done.\n")),
            Step::Text {
                key: "u1".into(),
                text: "All done.".into()
            }
        );
        assert_eq!(reply_step(&part("think", "u2", "thinking")), Step::Keep);
        assert_eq!(
            reply_step(&event_record(json!({"type": "tool.call", "name": "Bash"}))),
            Step::Reset
        );
        assert_eq!(reply_step(&message("user", "next")), Step::Reset);
        assert_eq!(reply_step(&message("injection", "reminder")), Step::Keep);
        assert_eq!(
            reply_step(&json!({"type": "turn.prompt", "agentId": "main",
                "origin": {"kind": "system_trigger"}})),
            Step::Reset
        );
        for (reason, step) in [
            ("completed", Step::Keep),
            ("cancelled", Step::Reset),
            ("failed", Step::Reset),
        ] {
            assert_eq!(
                reply_step(
                    &json!({"type": "turn.ended", "agentId": "main", "turnId": 0,
                    "reason": reason})
                ),
                step,
                "{reason}"
            );
        }
    }
}
