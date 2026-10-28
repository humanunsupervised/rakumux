//! Claude Code's conversation, from its transcript: each entry of the main
//! conversation is a message, at its byte offset. What the user typed is the
//! `user` role's (Claude Code's own insertions taken out), tool results the
//! `tool` role's, and the assistant's text, thinking and tool calls the
//! `assistant` role's. What the user typed while a turn ran is logged as a
//! queued prompt, also the `user` role's. Subagents' entries, meta entries and compaction
//! summaries are left out, as Claude Code's own view does.
use super::{assemble, call, compact, reasoning, result, text, timestamp};
use serde_json::Value;
use session_core::api::{TranscriptMessage, TranscriptPart, TranscriptRole};

/// The message of the transcript entry at `position`, if it is one.
pub(super) fn message(entry: &Value, position: u64) -> Option<TranscriptMessage> {
    let flagged = |key: &str| entry.get(key).and_then(Value::as_bool) == Some(true);
    if flagged("isSidechain") || flagged("isMeta") || flagged("isCompactSummary") {
        return None;
    }
    let (role, parts) = match entry.get("type")?.as_str()? {
        // Typed while a turn ran.
        "attachment" => {
            let typed = super::super::claude::queued(entry)?;
            (TranscriptRole::User, prompt(&typed).into_iter().collect())
        }
        "user" => user(entry.get("message")?.get("content")?),
        "assistant" => (
            TranscriptRole::Assistant,
            assistant(entry.get("message")?.get("content")?),
        ),
        _ => return None,
    };
    let at = entry
        .get("timestamp")
        .and_then(Value::as_str)
        .and_then(timestamp);
    assemble(
        entry.get("uuid").and_then(Value::as_str),
        position,
        role,
        at,
        parts,
    )
}

/// A user entry's parts: what was typed, or tools' results (the `tool` role).
fn user(content: &Value) -> (TranscriptRole, Vec<TranscriptPart>) {
    let blocks = match content {
        Value::String(typed) => return (TranscriptRole::User, prompt(typed).into_iter().collect()),
        Value::Array(blocks) => blocks,
        _ => return (TranscriptRole::User, Vec::new()),
    };
    let mut parts = Vec::new();
    let mut tool = false;
    for block in blocks {
        match block.get("type").and_then(Value::as_str) {
            Some("text") => {
                parts.extend(block.get("text").and_then(Value::as_str).and_then(prompt))
            }
            Some("image") => parts.extend(text("[image]")),
            Some("tool_result") => {
                tool = true;
                parts.push(result(
                    block.get("tool_use_id").and_then(Value::as_str),
                    &output(block.get("content")),
                    block.get("is_error").and_then(Value::as_bool) == Some(true),
                ));
            }
            _ => {}
        }
    }
    let role = if tool {
        TranscriptRole::Tool
    } else {
        TranscriptRole::User
    };
    (role, parts)
}

/// What the user typed, without what Claude Code adds to it (reminders,
/// command output); a slash command as `/name args`.
fn prompt(typed: &str) -> Option<TranscriptPart> {
    if typed.starts_with("[Request interrupted") {
        return None;
    }
    text(&super::super::typed_text(typed)?)
}

/// An assistant entry's text, thinking and tool calls, in order.
fn assistant(content: &Value) -> Vec<TranscriptPart> {
    let Some(blocks) = content.as_array() else {
        return Vec::new();
    };
    blocks
        .iter()
        .filter_map(|block| {
            let field = |key: &str| block.get(key).and_then(Value::as_str);
            match field("type")? {
                "text" => text(field("text")?),
                "thinking" => reasoning(field("thinking")?),
                "tool_use" | "server_tool_use" => call(
                    field("id"),
                    field("name")?,
                    &compact(block.get("input").unwrap_or(&Value::Null)),
                ),
                "web_search_tool_result" => Some(result(
                    field("tool_use_id"),
                    &compact(block.get("content").unwrap_or(&Value::Null)),
                    false,
                )),
                _ => None,
            }
        })
        .collect()
}

/// A tool result's content as text: its text blocks, an image as `[image]`.
fn output(content: Option<&Value>) -> String {
    match content {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(blocks)) => blocks
            .iter()
            .filter_map(|block| match block.get("type")?.as_str()? {
                "text" => Some(block.get("text")?.as_str()?.to_owned()),
                "image" => Some("[image]".to_owned()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn entry(kind: &str, content: Value) -> Value {
        json!({"type": kind, "uuid": "6c0d7a52-9b1e-4f3a-8d2c-1e5f7a9b3c4d",
            "timestamp": "2026-10-23T10:00:00.000Z", "message": {"role": kind, "content": content}})
    }

    fn texts(message: &TranscriptMessage) -> Vec<String> {
        message
            .parts
            .iter()
            .map(|part| match part {
                TranscriptPart::Text { text, .. } => format!("text: {text}"),
                TranscriptPart::Reasoning { text, .. } => format!("reasoning: {text}"),
                TranscriptPart::ToolCall {
                    call_id,
                    name,
                    input,
                    ..
                } => format!("call {}: {name} {input}", call_id.as_deref().unwrap_or("-")),
                TranscriptPart::ToolResult {
                    call_id,
                    output,
                    error,
                    ..
                } => format!(
                    "result {}{}: {output}",
                    call_id.as_deref().unwrap_or("-"),
                    if *error { " (error)" } else { "" }
                ),
                TranscriptPart::Unknown => "unknown".into(),
            })
            .collect()
    }

    #[test]
    fn a_prompt_is_what_the_user_typed() {
        let typed = message(
            &entry(
                "user",
                json!("Fix the test<system-reminder>be careful</system-reminder>"),
            ),
            40,
        )
        .unwrap();
        assert_eq!(typed.role, TranscriptRole::User);
        assert_eq!(typed.position, 40);
        assert_eq!(typed.id, "6c0d7a52-9b1e-4f3a-8d2c-1e5f7a9b3c4d");
        assert_eq!(typed.at, Some(1_792_749_600_000));
        assert_eq!(texts(&typed), ["text: Fix the test"]);
        let command = message(
            &entry(
                "user",
                json!("<command-name>/review</command-name><command-args>main</command-args>"),
            ),
            0,
        )
        .unwrap();
        assert_eq!(texts(&command), ["text: /review main"]);
        // Claude Code's own: meta entries, interruptions, subagents.
        let mut meta = entry("user", json!("Caveat: local commands"));
        meta["isMeta"] = json!(true);
        assert_eq!(message(&meta, 0), None);
        assert_eq!(
            message(&entry("user", json!("[Request interrupted by user]")), 0),
            None
        );
        let mut sidechain = entry("assistant", json!([{"type": "text", "text": "sub"}]));
        sidechain["isSidechain"] = json!(true);
        assert_eq!(message(&sidechain, 0), None);
    }

    #[test]
    fn a_prompt_typed_while_a_turn_ran_is_the_users() {
        let queued = |origin: &str, text: &str| {
            json!({"type": "attachment", "uuid": "5110f9e2-c20b-4dbd-9606-b04ec4dc25b2",
                "timestamp": "2026-10-23T10:00:00.000Z",
                "attachment": {"type": "queued_command", "prompt": text,
                    "commandMode": "prompt", "origin": {"kind": origin}}})
        };
        let typed = message(&queued("human", "Also the docs"), 90).unwrap();
        assert_eq!(typed.role, TranscriptRole::User);
        assert_eq!(typed.position, 90);
        assert_eq!(typed.at, Some(1_792_749_600_000));
        assert_eq!(texts(&typed), ["text: Also the docs"]);
        assert_eq!(message(&queued("peer", "From another agent"), 0), None);
    }

    #[test]
    fn the_assistants_text_thinking_and_calls_are_kept_in_order() {
        let reply = message(
            &entry(
                "assistant",
                json!([
                    {"type": "thinking", "thinking": "The test depends on map order."},
                    {"type": "text", "text": "Let me run it."},
                    {"type": "tool_use", "id": "toolu_01ABC", "name": "Bash",
                     "input": {"command": "cargo test", "description": "Run the tests"}}
                ]),
            ),
            812,
        )
        .unwrap();
        assert_eq!(reply.role, TranscriptRole::Assistant);
        assert_eq!(
            texts(&reply),
            [
                "reasoning: The test depends on map order.",
                "text: Let me run it.",
                r#"call toolu_01ABC: Bash {"command":"cargo test","description":"Run the tests"}"#,
            ]
        );
        // Redacted thinking says nothing.
        assert_eq!(
            message(
                &entry(
                    "assistant",
                    json!([{"type": "redacted_thinking", "data": "x"}])
                ),
                0
            ),
            None
        );
    }

    #[test]
    fn tool_results_are_the_tool_roles() {
        let results = message(
            &entry(
                "user",
                json!([
                    {"type": "tool_result", "tool_use_id": "toolu_01ABC", "is_error": true,
                     "content": "test result: FAILED"},
                    {"type": "tool_result", "tool_use_id": "toolu_02",
                     "content": [{"type": "text", "text": "line one"}, {"type": "image", "source": {}}]}
                ]),
            ),
            1650,
        )
        .unwrap();
        assert_eq!(results.role, TranscriptRole::Tool);
        assert_eq!(
            texts(&results),
            [
                "result toolu_01ABC (error): test result: FAILED",
                "result toolu_02: line one\n[image]",
            ]
        );
        // Bookkeeping entries aren't messages.
        assert_eq!(
            message(&json!({"type": "file-history-snapshot", "snapshot": {}}), 0),
            None
        );
    }
}
