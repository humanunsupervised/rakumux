//! Grok Build's latest output, from its session's `updates.jsonl`: one JSON
//! line per ACP session update (`{timestamp, method, params: {sessionId,
//! update: {sessionUpdate, …}, _meta: {eventId, agentTimestampMs, promptId}}}`),
//! appended to as the session goes on (a `/rewind` appends a marker too). A
//! streamed block is one line once it ends.
use super::{contains, first_line, prompt_line, short_path, tool_line, Step};
use serde_json::Value;
use std::path::PathBuf;

/// Thinking and replies longer than this are cut, in bytes, before they're
/// shaped into a line or a summary's message.
const MAX_BLOCK: usize = 8 * 1024;

/// The conversation process `pid` shows, with its log: `bound` (the one its
/// hooks named) when it holds that one, else the one written last.
pub(super) fn current(pid: u32, bound: Option<&str>) -> Option<(String, PathBuf)> {
    crate::resume::grok_conversation(&crate::resume::grok_home()?, pid, bound)
}

/// Follow the conversation process `pid` shows now (one Grok holds several:
/// `/new`, forks, the Agent Dashboard): another one's log is read from its
/// tail. Returns whether it switched.
pub(super) fn switch(
    pid: u32,
    bound: Option<&str>,
    conversation: &mut Option<String>,
    path: &mut PathBuf,
    offset: &mut u64,
    partial: &mut Vec<u8>,
) -> bool {
    let Some((current, log)) = current(pid, bound) else {
        return false;
    };
    if conversation.as_deref() == Some(current.as_str()) {
        return false;
    }
    *path = log;
    *offset = 0;
    partial.clear();
    *conversation = Some(current);
    true
}

/// An entry's update and its kind.
pub(super) fn update(entry: &Value) -> Option<(&Value, &str)> {
    let update = entry.get("params")?.get("update")?;
    Some((update, update.get("sessionUpdate")?.as_str()?))
}

/// When an entry was written (Unix milliseconds): its `agentTimestampMs`, else
/// its `timestamp` (Unix seconds).
pub(crate) fn timestamp(entry: &Value) -> Option<i64> {
    let meta = entry.get("params").and_then(|params| params.get("_meta"));
    meta.and_then(|meta| meta.get("agentTimestampMs"))
        .and_then(Value::as_i64)
        .or_else(|| {
            entry
                .get("timestamp")
                .and_then(Value::as_i64)
                .map(|seconds| seconds.saturating_mul(1000))
        })
}

/// The text of an update's `content` block.
fn text(update: &Value) -> Option<&str> {
    let content = update.get("content")?;
    (content.get("type")?.as_str()? == "text")
        .then(|| content.get("text")?.as_str())
        .flatten()
}

/// The thinking or reply of an entry, as written (cut at [`MAX_BLOCK`]).
fn block(update: &Value) -> Option<String> {
    let text = text(update)?.trim();
    let end = (0..=text.len().min(MAX_BLOCK))
        .rev()
        .find(|at| text.is_char_boundary(*at))?;
    (!text.is_empty()).then(|| text[..end].to_string())
}

/// What the user typed (`user_message_chunk`), as written. Grok logs the
/// prompts it wakes itself with (a background subagent finished) there too,
/// as a `<system-reminder>`: those aren't typed.
pub(super) fn typed(entry: &Value) -> Option<String> {
    let (update, kind) = update(entry)?;
    if kind != "user_message_chunk" {
        return None;
    }
    let text = text(update)?;
    (!text.trim_start().starts_with("<system-reminder>")).then(|| text.to_string())
}

/// Whether a log line may be one [`typed`] takes, before it's parsed.
pub(super) fn may_be_typed(line: &[u8]) -> bool {
    contains(line, br#""user_message_chunk""#)
}

/// What the user typed, as one line.
pub(super) fn prompt(entry: &Value) -> Option<String> {
    prompt_line(&typed(entry)?)
}

/// Its thinking, reply or tool call, as one line.
pub(super) fn line(entry: &Value) -> Option<String> {
    messages(entry)
        .iter()
        .rev()
        .find_map(|message| first_line(message))
}

/// An entry's messages: its thinking and reply as written, a tool call as a
/// line. A subagent's work is in its own session's log, never this one.
pub(super) fn messages(entry: &Value) -> Vec<String> {
    let Some((update, kind)) = update(entry) else {
        return Vec::new();
    };
    match kind {
        "agent_thought_chunk" | "agent_message_chunk" => block(update).into_iter().collect(),
        "tool_call" => call(update).into_iter().collect(),
        _ => Vec::new(),
    }
}

/// A tool's name as Grok names it (`run_terminal_command`, `search_replace`,
/// `<server>__<tool>` for MCP), and how its UI labels it (`Run Command`, `Edit`).
pub(super) fn tool(update: &Value) -> (Option<&str>, Option<&str>) {
    let meta = update.get("_meta").and_then(|meta| meta.get("x.ai/tool"));
    let name = meta
        .and_then(|meta| meta.get("name"))
        .and_then(Value::as_str)
        .or_else(|| update.get("title").and_then(Value::as_str));
    let label = meta
        .and_then(|meta| meta.get("label"))
        .and_then(Value::as_str)
        .filter(|label| !label.trim().is_empty());
    (name, label)
}

/// A tool call as a line: its label, and what it works on.
fn call(update: &Value) -> Option<String> {
    let (name, label) = tool(update);
    let name = name?;
    let field = |key: &str| update.get("rawInput")?.get(key)?.as_str();
    let subject = match name {
        // Its own one-line description says more than the command.
        "run_terminal_command" => field("description")
            .or_else(|| field("command"))
            .map(String::from),
        "read_file" => field("target_file").map(short_path),
        "write" | "search_replace" | "hashline_edit" => {
            field("file_path").or_else(|| field("path")).map(short_path)
        }
        "list_dir" => field("target_directory").map(String::from),
        "grep" => field("pattern").map(String::from),
        "web_search" => field("query").map(String::from),
        "web_fetch" => field("url").map(String::from),
        "spawn_subagent" => field("description").map(String::from),
        "ask_user_question" => update
            .get("rawInput")
            .and_then(|input| input.get("questions")?.get(0)?.get("question")?.as_str())
            .map(String::from),
        _ => None,
    };
    tool_line(label.unwrap_or(name), subject.as_deref())
}

/// How an entry changes the turn's reply: anything the user side adds (a
/// prompt, a wake-up) or a tool call starts it over, and each block of text
/// adds to it, keyed by its turn's prompt.
pub(super) fn reply_step(entry: &Value) -> Step {
    let Some((update, kind)) = update(entry) else {
        return Step::Keep;
    };
    match kind {
        "user_message_chunk" | "tool_call" => Step::Reset,
        "agent_message_chunk" => {
            let meta = entry.get("params").and_then(|params| params.get("_meta"));
            let key = ["promptId", "eventId"]
                .iter()
                .find_map(|key| meta?.get(key)?.as_str())
                .filter(|key| !key.is_empty());
            match (text(update).map(str::trim), key) {
                (Some(text), Some(key)) if !text.is_empty() => Step::Text {
                    key: key.to_owned(),
                    text: text.to_owned(),
                },
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

    /// An `updates.jsonl` line of a synthetic session.
    fn entry(n: u32, update: Value, prompt: Option<&str>) -> Value {
        let mut meta = json!({"eventId": format!("01a18b49-8533-7252-8d65-46555f07d0c5-{n}"),
            "agentTimestampMs": 1_793_338_236_000_i64 + i64::from(n)});
        if let Some(prompt) = prompt {
            meta["promptId"] = json!(prompt);
        }
        json!({"timestamp": 1_793_338_236, "method": "session/update",
            "params": {"sessionId": "01a18b49-8533-7252-8d65-46555f07d0c5",
                "update": update, "_meta": meta}})
    }

    fn chunk(kind: &str, text: &str) -> Value {
        json!({"sessionUpdate": kind, "content": {"type": "text", "text": text}})
    }

    fn call(name: &str, label: &str, input: Value) -> Value {
        json!({"sessionUpdate": "tool_call", "toolCallId": "call_1", "title": name,
            "rawInput": input, "_meta": {"x.ai/tool": {"version": 1, "name": name,
                "kind": "execute", "label": label, "read_only": false}}})
    }

    #[test]
    fn its_thinking_tool_calls_and_replies_are_lines_and_what_was_typed_a_prompt() {
        assert_eq!(
            line(&entry(
                3,
                chunk("agent_thought_chunk", "Let me check the parser.\nThen…"),
                Some("p")
            )),
            Some("Let me check the parser.".into())
        );
        assert_eq!(
            line(&entry(
                4,
                call(
                    "run_terminal_command",
                    "Run Command",
                    json!({"command": "cargo test", "description": "Run the tests"})
                ),
                Some("p")
            )),
            Some("Run Command: Run the tests".into())
        );
        assert_eq!(
            line(&entry(
                5,
                call(
                    "search_replace",
                    "Edit",
                    json!({"file_path": "/home/me/proj/src/app.rs", "old_string": "a"})
                ),
                Some("p")
            )),
            Some("Edit: src/app.rs".into())
        );
        assert_eq!(
            line(&entry(
                6,
                call("linear__save_issue", "", json!({})),
                Some("p")
            )),
            Some("linear__save_issue".into())
        );
        let asked = entry(
            2,
            chunk("user_message_chunk", "Fix the parser\nplease"),
            None,
        );
        assert_eq!(prompt(&asked), Some("Fix the parser".into()));
        assert!(may_be_typed(asked.to_string().as_bytes()));
        // Woken by a finished background subagent: Grok's prompt, not the user's.
        let wake = entry(9, chunk("user_message_chunk",
            "<system-reminder>\nWhile you were idle, 1 background subagent completed:\n</system-reminder>"), None);
        assert_eq!(typed(&wake), None);
        assert_eq!(reply_step(&wake), Step::Reset);
        // Bookkeeping says nothing.
        let hook = entry(
            1,
            json!({"sessionUpdate": "hook_execution", "event_name": "stop"}),
            None,
        );
        assert_eq!(line(&hook), None);
        assert_eq!(reply_step(&hook), Step::Keep);
    }

    #[test]
    fn times_are_the_agents_milliseconds_else_the_lines_seconds() {
        let with = entry(7, chunk("agent_message_chunk", "x"), Some("p"));
        assert_eq!(timestamp(&with), Some(1_793_338_236_007));
        let without = json!({"timestamp": 1_793_338_236, "params": {}});
        assert_eq!(timestamp(&without), Some(1_793_338_236_000));
    }

    #[test]
    fn a_reply_is_keyed_by_its_turns_prompt_and_a_tool_call_starts_it_over() {
        assert_eq!(
            reply_step(&entry(
                8,
                chunk("agent_message_chunk", " Done. "),
                Some("p1")
            )),
            Step::Text {
                key: "p1".into(),
                text: "Done.".into()
            }
        );
        assert_eq!(
            reply_step(&entry(8, chunk("agent_message_chunk", "Done."), None)),
            Step::Text {
                key: "01a18b49-8533-7252-8d65-46555f07d0c5-8".into(),
                text: "Done.".into()
            }
        );
        assert_eq!(
            reply_step(&entry(9, call("write", "Write", json!({})), Some("p1"))),
            Step::Reset
        );
        assert_eq!(
            reply_step(&entry(10, chunk("agent_thought_chunk", "Hmm"), Some("p1"))),
            Step::Keep
        );
    }
}
