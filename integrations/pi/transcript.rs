//! Pi's conversation, from its file: each `message` entry is a message, at its
//! byte offset, with the entry's `id`. What the user typed is the `user`
//! role's (a skill as `/skill:name args`), a tool's result the `tool` role's,
//! and the assistant's text, thinking and tool calls the `assistant` role's.
//! A `!` command the user ran is theirs: a `bash` call and its output. Pi's
//! own messages (the system prompt, extensions' context, summaries) are left
//! out, as its own view leaves most of them; so are branches left with
//! `/tree`, which stay in the file in the order they were written.
use super::{assemble, call, compact, reasoning, result, text, timestamp};
use serde_json::Value;
use session_core::api::{TranscriptMessage, TranscriptPart, TranscriptRole};

/// The message of the conversation entry at `position`, if it is one.
pub(super) fn message(entry: &Value, position: u64) -> Option<TranscriptMessage> {
    if entry.get("type")?.as_str()? != "message" {
        return None;
    }
    let message = entry.get("message")?;
    let field = |key: &str| message.get(key).and_then(Value::as_str);
    let (role, parts) = match field("role")? {
        "user" => (
            TranscriptRole::User,
            user(message.get("content").unwrap_or(&Value::Null)),
        ),
        "assistant" => (
            TranscriptRole::Assistant,
            assistant(message.get("content").unwrap_or(&Value::Null)),
        ),
        "toolResult" => (
            TranscriptRole::Tool,
            vec![result(
                field("toolCallId"),
                &output(message.get("content")),
                message.get("isError").and_then(Value::as_bool) == Some(true),
            )],
        ),
        "bashExecution" => {
            let command = field("command")?;
            let failed = message
                .get("exitCode")
                .and_then(Value::as_i64)
                .is_some_and(|code| code != 0)
                || message.get("cancelled").and_then(Value::as_bool) == Some(true);
            let input = serde_json::json!({ "command": command });
            let parts = call(None, "bash", &input.to_string())
                .into_iter()
                .chain([result(None, field("output").unwrap_or_default(), failed)])
                .collect();
            (TranscriptRole::User, parts)
        }
        _ => return None,
    };
    let at = entry
        .get("timestamp")
        .and_then(Value::as_str)
        .and_then(timestamp);
    assemble(
        entry.get("id").and_then(Value::as_str),
        position,
        role,
        at,
        parts,
    )
}

/// What the user typed: its text, an image as `[image]`.
fn user(content: &Value) -> Vec<TranscriptPart> {
    let typed =
        |typed: &str| text(&super::super::pi::skill(typed).unwrap_or_else(|| typed.to_owned()));
    match content {
        Value::String(written) => typed(written).into_iter().collect(),
        Value::Array(blocks) => blocks
            .iter()
            .filter_map(|block| match block.get("type")?.as_str()? {
                "text" => typed(block.get("text")?.as_str()?),
                "image" => text("[image]"),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// An assistant response's text, thinking and tool calls, in order.
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
                "thinking" if block.get("redacted").and_then(Value::as_bool) == Some(true) => None,
                "thinking" => reasoning(field("thinking")?),
                "toolCall" => call(
                    field("id"),
                    field("name")?,
                    &compact(block.get("arguments").unwrap_or(&Value::Null)),
                ),
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
    use crate::agent_output::transcript::TranscriptSource;
    use crate::agent_output::LogAgent;
    use serde_json::json;
    use std::io::Write;

    fn entry(id: &str, message: Value) -> Value {
        json!({"type": "message", "id": id, "parentId": "p0",
            "timestamp": "2026-10-23T10:00:00.000Z", "message": message})
    }

    fn parts(message: &TranscriptMessage) -> Vec<String> {
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

    /// One of each kind of entry a conversation file holds, in Pi's order.
    fn conversation() -> Vec<Value> {
        vec![
            json!({"type": "session", "version": 3, "id": "01a18a95-0000-7000-8000-2cd72115b9d0",
                "timestamp": "2026-10-23T10:00:00.000Z", "cwd": "/repo"}),
            json!({"type": "model_change", "id": "m0", "parentId": null,
                "timestamp": "2026-10-23T10:00:00.000Z", "provider": "p", "modelId": "m"}),
            entry(
                "s0",
                json!({"role": "system", "content": "", "sections": {"preamble": "secret prompt"},
                "toolsAdded": [], "timestamp": 1}),
            ),
            entry(
                "u1",
                json!({"role": "user", "content": [{"type": "text", "text": "Fix the test"},
                {"type": "image", "data": "AAAA", "mimeType": "image/png"}], "timestamp": 1}),
            ),
            entry(
                "a1",
                json!({"role": "assistant", "content": [
                {"type": "thinking", "thinking": "Look first."},
                {"type": "thinking", "thinking": "opaque", "redacted": true},
                {"type": "text", "text": "On it."},
                {"type": "toolCall", "id": "call_1", "name": "write",
                    "arguments": {"path": "a.txt", "content": "hi\n"}}],
                "stopReason": "toolUse", "timestamp": 2}),
            ),
            entry(
                "r1",
                json!({"role": "toolResult", "toolCallId": "call_1", "toolName": "write",
                "content": [{"type": "text", "text": "Successfully wrote to a.txt"}],
                "isError": false, "timestamp": 3}),
            ),
            entry(
                "r2",
                json!({"role": "toolResult", "toolCallId": "call_2", "toolName": "bash",
                "content": [{"type": "text", "text": "boom"}], "isError": true, "timestamp": 3}),
            ),
            entry(
                "a2",
                json!({"role": "assistant", "content": [{"type": "text", "text": "Done."}],
                "stopReason": "stop", "timestamp": 4}),
            ),
            entry(
                "b1",
                json!({"role": "bashExecution", "command": "git status", "output": "clean",
                "exitCode": 0, "cancelled": false, "truncated": false, "timestamp": 5}),
            ),
            entry(
                "c1",
                json!({"role": "custom", "customType": "x", "content": "context",
                "display": true, "timestamp": 6}),
            ),
            json!({"type": "compaction", "id": "k1", "parentId": "c1",
                "timestamp": "2026-10-23T10:00:00.000Z", "summary": "earlier", "firstKeptEntryId": "u1"}),
            entry(
                "u2",
                json!({"role": "user", "content": "<skill name=\"review\" location=\"/s/SKILL.md\">\nbody\n</skill>\n\nsrc", "timestamp": 7}),
            ),
        ]
    }

    #[test]
    fn each_kind_of_entry_maps_to_its_role_and_parts() {
        let messages: Vec<TranscriptMessage> = conversation()
            .iter()
            .enumerate()
            .filter_map(|(at, e)| message(e, at as u64))
            .collect();
        let shown: Vec<(String, TranscriptRole, Vec<String>)> = messages
            .iter()
            .map(|m| (m.id.clone(), m.role, parts(m)))
            .collect();
        assert_eq!(
            shown,
            vec![
                (
                    "u1".into(),
                    TranscriptRole::User,
                    vec!["text: Fix the test".into(), "text: [image]".into()]
                ),
                (
                    "a1".into(),
                    TranscriptRole::Assistant,
                    vec![
                        "reasoning: Look first.".into(),
                        "text: On it.".into(),
                        r#"call call_1: write {"content":"hi\n","path":"a.txt"}"#.into(),
                    ]
                ),
                (
                    "r1".into(),
                    TranscriptRole::Tool,
                    vec!["result call_1: Successfully wrote to a.txt".into()]
                ),
                (
                    "r2".into(),
                    TranscriptRole::Tool,
                    vec!["result call_2 (error): boom".into()]
                ),
                (
                    "a2".into(),
                    TranscriptRole::Assistant,
                    vec!["text: Done.".into()]
                ),
                (
                    "b1".into(),
                    TranscriptRole::User,
                    vec![
                        r#"call -: bash {"command":"git status"}"#.into(),
                        "result -: clean".into()
                    ]
                ),
                (
                    "u2".into(),
                    TranscriptRole::User,
                    vec!["text: /skill:review src".into()]
                ),
            ]
        );
        assert_eq!(messages[0].at, Some(1_792_749_600_000));
        assert_eq!(messages[0].position, 3);
        let all = serde_json::to_string(&messages).unwrap();
        assert!(
            !all.contains("secret prompt"),
            "the system prompt is Pi's own"
        );
    }

    #[test]
    fn a_conversation_file_is_read_on_from_its_cursor() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("2026-10-23T10-00-00-000Z_c1.jsonl");
        let mut file = std::fs::File::create(&path).unwrap();
        for line in conversation() {
            writeln!(file, "{line}").unwrap();
        }
        let source = TranscriptSource::Log {
            agent: LogAgent::Pi,
            conversation: "c1".into(),
            path,
        };
        let page = source.read(None, None, false).unwrap();
        assert_eq!(page.agent, "pi");
        assert_eq!(page.conversation_id, "c1");
        assert_eq!(page.messages.len(), 7);
        assert!(!page.more);
        // Small reads go a line at a time and say there's more.
        let first = source.read(None, Some(1), false).unwrap();
        assert!(first.messages.is_empty(), "the header is no message");
        assert!(first.more);
        writeln!(
            file,
            "{}",
            entry(
                "u3",
                json!({"role": "user", "content": "And the docs", "timestamp": 8})
            )
        )
        .unwrap();
        let next = source.read(Some(&page.cursor), None, false).unwrap();
        assert_eq!(next.messages.len(), 1);
        assert_eq!(next.messages[0].id, "u3");
    }
}
