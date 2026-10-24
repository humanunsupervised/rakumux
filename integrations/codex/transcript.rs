//! Codex's conversation, from its rollout file: one message per line, at its
//! byte offset. What the user typed comes from its `user_message` events (the
//! `user` response items also hold what Codex adds); the assistant's text,
//! reasoning summaries and tool calls, and the tools' output (the `tool`
//! role's), from the response items. A reply recorded both as an event and
//! as an item counts once, as the item.
use super::{assemble, call, compact, reasoning, result, text, timestamp};
use serde_json::Value;
use session_core::api::{TranscriptMessage, TranscriptPart, TranscriptRole};

/// The message of the rollout line at `position`, if it is one.
pub(super) fn message(entry: &Value, position: u64) -> Option<TranscriptMessage> {
    let payload = entry.get("payload")?;
    let field = |key: &str| payload.get(key).and_then(Value::as_str);
    let kinds = (entry.get("type")?.as_str()?, field("type")?);
    let (role, parts) = match kinds {
        ("event_msg", "user_message") => {
            let mut parts: Vec<TranscriptPart> = field("message")
                .and_then(super::super::typed_text)
                .and_then(|typed| text(&typed))
                .into_iter()
                .collect();
            let images = payload
                .get("images")
                .and_then(Value::as_array)
                .map_or(0, Vec::len);
            parts.extend((0..images).filter_map(|_| text("[image]")));
            (TranscriptRole::User, parts)
        }
        ("response_item", "message") if field("role") == Some("assistant") => (
            TranscriptRole::Assistant,
            contents(payload.get("content"), text),
        ),
        ("response_item", "reasoning") => {
            let mut parts = contents(payload.get("summary"), reasoning);
            parts.extend(contents(payload.get("content"), reasoning));
            (TranscriptRole::Assistant, parts)
        }
        ("response_item", "function_call") => (
            TranscriptRole::Assistant,
            call(
                field("call_id"),
                field("name")?,
                field("arguments").unwrap_or_default(),
            )
            .into_iter()
            .collect(),
        ),
        ("response_item", "custom_tool_call") => (
            TranscriptRole::Assistant,
            call(
                field("call_id"),
                field("name")?,
                field("input").unwrap_or_default(),
            )
            .into_iter()
            .collect(),
        ),
        ("response_item", "local_shell_call") => (
            TranscriptRole::Assistant,
            call(
                field("call_id"),
                "shell",
                &compact(payload.get("action").unwrap_or(&Value::Null)),
            )
            .into_iter()
            .collect(),
        ),
        ("response_item", "web_search_call") => (
            TranscriptRole::Assistant,
            call(
                field("id"),
                "web_search",
                &compact(payload.get("action").unwrap_or(&Value::Null)),
            )
            .into_iter()
            .collect(),
        ),
        ("response_item", "function_call_output" | "custom_tool_call_output") => {
            let (output, error) = output(payload.get("output"));
            (
                TranscriptRole::Tool,
                vec![result(field("call_id"), &output, error)],
            )
        }
        _ => return None,
    };
    let at = entry
        .get("timestamp")
        .and_then(Value::as_str)
        .and_then(timestamp);
    assemble(None, position, role, at, parts)
}

/// The parts of a list of `{text}` pieces (a message's content, a reasoning
/// summary).
fn contents(
    pieces: Option<&Value>,
    part: fn(&str) -> Option<TranscriptPart>,
) -> Vec<TranscriptPart> {
    pieces
        .and_then(Value::as_array)
        .map(|pieces| {
            pieces
                .iter()
                .filter_map(|piece| part(piece.get("text")?.as_str()?))
                .collect()
        })
        .unwrap_or_default()
}

/// A tool's output as text, and whether it failed: a shell call's is JSON
/// with its output and exit code.
fn output(output: Option<&Value>) -> (String, bool) {
    let text = match output {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(pieces)) => pieces
            .iter()
            .filter_map(|piece| match piece.get("type")?.as_str()? {
                "input_image" => Some("[image]".to_owned()),
                _ => Some(piece.get("text")?.as_str()?.to_owned()),
            })
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    };
    let shell = serde_json::from_str::<Value>(&text).ok().and_then(|value| {
        let output = value.get("output")?.as_str()?.to_owned();
        let code = value.get("metadata")?.get("exit_code")?.as_i64()?;
        Some((output, code != 0))
    });
    shell.unwrap_or((text, false))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn line(kind: &str, payload: Value) -> Value {
        json!({"timestamp": "2026-10-23T10:00:00.000Z", "type": kind, "payload": payload})
    }

    #[test]
    fn the_users_message_is_its_event() {
        let typed = message(
            &line(
                "event_msg",
                json!({"type": "user_message", "message": "Run the tests", "images": ["x"]}),
            ),
            120,
        )
        .unwrap();
        assert_eq!(typed.role, TranscriptRole::User);
        assert_eq!((typed.id.as_str(), typed.position), ("120", 120));
        assert_eq!(typed.at, Some(1_792_749_600_000));
        assert_eq!(
            typed.parts,
            [
                TranscriptPart::Text {
                    text: "Run the tests".into(),
                    truncated: false
                },
                TranscriptPart::Text {
                    text: "[image]".into(),
                    truncated: false
                }
            ]
        );
        // The item copy, with what Codex adds, isn't one; nor is the
        // reply's event.
        for (kind, payload) in [
            (
                "response_item",
                json!({"type": "message", "role": "user",
                "content": [{"type": "input_text", "text": "<environment_context>…</environment_context>"}]}),
            ),
            (
                "response_item",
                json!({"type": "message", "role": "developer",
                "content": [{"type": "input_text", "text": "instructions"}]}),
            ),
            (
                "event_msg",
                json!({"type": "agent_message", "message": "Done"}),
            ),
            (
                "event_msg",
                json!({"type": "task_complete", "turn_id": "t1"}),
            ),
        ] {
            assert_eq!(message(&line(kind, payload), 0), None);
        }
    }

    #[test]
    fn replies_reasoning_and_calls_are_the_assistants() {
        let reply = message(
            &line(
                "response_item",
                json!({"type": "message", "role": "assistant",
                "content": [{"type": "output_text", "text": "All done."}]}),
            ),
            0,
        )
        .unwrap();
        assert_eq!(reply.role, TranscriptRole::Assistant);
        assert_eq!(
            reply.parts,
            [TranscriptPart::Text {
                text: "All done.".into(),
                truncated: false
            }]
        );
        let thinking = message(
            &line(
                "response_item",
                json!({"type": "reasoning",
                "summary": [{"type": "summary_text", "text": "**Exploring** the parser"}],
                "encrypted_content": "opaque"}),
            ),
            0,
        )
        .unwrap();
        assert_eq!(
            thinking.parts,
            [TranscriptPart::Reasoning {
                text: "**Exploring** the parser".into(),
                truncated: false
            }]
        );
        // Only encrypted: nothing to show.
        assert_eq!(
            message(
                &line(
                    "response_item",
                    json!({"type": "reasoning", "summary": [], "encrypted_content": "x"})
                ),
                0
            ),
            None
        );
        let shell = message(
            &line(
                "response_item",
                json!({"type": "function_call", "name": "exec_command",
                "call_id": "call_1", "arguments": "{\"cmd\":\"cargo test\"}"}),
            ),
            0,
        )
        .unwrap();
        assert_eq!(
            shell.parts,
            [TranscriptPart::ToolCall {
                call_id: Some("call_1".into()),
                name: "exec_command".into(),
                input: "{\"cmd\":\"cargo test\"}".into(),
                truncated: false
            }]
        );
        let patch = message(
            &line(
                "response_item",
                json!({"type": "custom_tool_call", "name": "apply_patch",
                "call_id": "call_2", "input": "*** Begin Patch"}),
            ),
            0,
        )
        .unwrap();
        assert!(
            matches!(&patch.parts[0], TranscriptPart::ToolCall { name, .. } if name == "apply_patch")
        );
    }

    #[test]
    fn tool_output_is_the_tool_roles_with_its_exit_code() {
        let failed = message(
            &line(
                "response_item",
                json!({"type": "function_call_output", "call_id": "call_1",
                "output": "{\"output\":\"1 failed\",\"metadata\":{\"exit_code\":1}}"}),
            ),
            0,
        )
        .unwrap();
        assert_eq!(failed.role, TranscriptRole::Tool);
        assert_eq!(
            failed.parts,
            [TranscriptPart::ToolResult {
                call_id: Some("call_1".into()),
                output: "1 failed".into(),
                error: true,
                truncated: false
            }]
        );
        let pieces = message(
            &line(
                "response_item",
                json!({"type": "custom_tool_call_output", "call_id": "call_2",
                "output": [{"type": "input_text", "text": "Success."}]}),
            ),
            0,
        )
        .unwrap();
        assert_eq!(
            pieces.parts,
            [TranscriptPart::ToolResult {
                call_id: Some("call_2".into()),
                output: "Success.".into(),
                error: false,
                truncated: false
            }]
        );
    }
}
