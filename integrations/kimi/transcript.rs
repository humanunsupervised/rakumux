//! Kimi Code's conversation, from its main agent's log: one message per
//! record, at its byte offset. What the user typed (a prompt, or a steer
//! during a turn) is the user's; the agent's reasoning, text and tool calls
//! (`content.part`, `tool.call`) the assistant's; a tool's result the
//! `tool` role's. Kimi's own insertions (reminders, hook results), its
//! bookkeeping and subagents' logs are left out.
use super::super::kimi::{at, loop_event, typed_message};
use super::{assemble, call, compact, reasoning, result, text};
use serde_json::Value;
use session_core::api::{TranscriptMessage, TranscriptPart, TranscriptRole};

/// The message of the log record at `position`, if it is one.
pub(super) fn message(entry: &Value, position: u64) -> Option<TranscriptMessage> {
    if let Some(message) = typed_message(entry) {
        let parts = message
            .get("content")
            .and_then(Value::as_array)
            .map(|parts| parts.iter().filter_map(content).collect())
            .unwrap_or_default();
        let id = message.get("id").and_then(Value::as_str);
        return assemble(id, position, TranscriptRole::User, at(entry), parts);
    }
    let event = loop_event(entry)?;
    let field = |key: &str| event.get(key).and_then(Value::as_str);
    let (role, parts) = match field("type")? {
        "content.part" => {
            let part = event.get("part")?;
            let text = |key: &str| part.get(key).and_then(Value::as_str);
            let part = match text("type")? {
                "text" => super::text(text("text")?),
                "think" if part.get("hidden").and_then(Value::as_bool) != Some(true) => {
                    reasoning(text("think")?)
                }
                _ => None,
            };
            (TranscriptRole::Assistant, part.into_iter().collect())
        }
        "tool.call" => (
            TranscriptRole::Assistant,
            call(
                field("toolCallId"),
                field("name")?,
                &compact(event.get("args").unwrap_or(&Value::Null)),
            )
            .into_iter()
            .collect(),
        ),
        "tool.result" => {
            let outcome = event.get("result")?;
            let output = match outcome.get("output") {
                Some(Value::String(output)) => output.clone(),
                Some(Value::Array(parts)) => parts
                    .iter()
                    .filter_map(|part| match content(part)? {
                        TranscriptPart::Text { text, .. } => Some(text),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n"),
                _ => String::new(),
            };
            let error = outcome.get("isError").and_then(Value::as_bool) == Some(true);
            (
                TranscriptRole::Tool,
                vec![result(field("toolCallId"), &output, error)],
            )
        }
        _ => return None,
    };
    let id = match field("type") {
        Some("tool.result") => None,
        _ => field("uuid"),
    };
    assemble(id, position, role, at(entry), parts)
}

/// A message's content part: its text, or what a picture, recording or
/// video stood for.
fn content(part: &Value) -> Option<TranscriptPart> {
    match part.get("type")?.as_str()? {
        "text" => text(part.get("text")?.as_str()?),
        "image_url" => text("[image]"),
        "audio_url" => text("[audio]"),
        "video_url" => text("[video]"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn record(event: Value) -> Value {
        json!({"type": "context.append_loop_event", "agentId": "main", "event": event,
            "time": 1_793_338_216_205_i64})
    }

    #[test]
    fn what_the_user_typed_is_the_users_with_its_id() {
        let typed = message(
            &json!({"type": "context.append_message", "agentId": "main", "time": 7,
                "message": {"role": "user", "id": "msg_01M65MK3QGXRCJZBQNYFXV6BQT",
                "origin": {"kind": "user"}, "content": [
                    {"type": "text", "text": "What is in this picture?"},
                    {"type": "image_url", "imageUrl": {"url": "data:image/png;base64,AAAA"}}]}}),
            120,
        )
        .unwrap();
        assert_eq!(typed.role, TranscriptRole::User);
        assert_eq!(typed.id, "msg_01M65MK3QGXRCJZBQNYFXV6BQT");
        assert_eq!((typed.position, typed.at), (120, Some(7)));
        assert_eq!(
            typed.parts,
            [
                TranscriptPart::Text {
                    text: "What is in this picture?".into(),
                    truncated: false
                },
                TranscriptPart::Text {
                    text: "[image]".into(),
                    truncated: false
                }
            ]
        );
        // Kimi's own insertions and bookkeeping aren't messages.
        for entry in [
            json!({"type": "context.append_message", "agentId": "main", "message": {
                "role": "user", "origin": {"kind": "injection", "variant": "date_change"},
                "content": [{"type": "text", "text": "<system-reminder>…</system-reminder>"}]}}),
            json!({"type": "turn.prompt", "agentId": "main", "origin": {"kind": "user"},
                "input": [{"type": "text", "text": "a copy of the prompt"}]}),
            json!({"type": "agent.message.appended", "kind": "event", "message": {}}),
            record(json!({"type": "step.begin", "uuid": "s1"})),
            record(json!({"type": "content.part", "uuid": "c9",
                "part": {"type": "think", "think": "secret", "hidden": true}})),
        ] {
            assert_eq!(message(&entry, 0), None, "{entry}");
        }
    }

    #[test]
    fn the_agents_parts_and_the_tools_results() {
        let thinking = message(
            &record(
                json!({"type": "content.part", "uuid": "c1", "turnId": "0", "step": 1,
                "part": {"type": "think", "think": "I will write the file.",
                "reasoningKey": "reasoning_content"}}),
            ),
            10,
        )
        .unwrap();
        assert_eq!(thinking.role, TranscriptRole::Assistant);
        assert_eq!(thinking.id, "c1");
        assert_eq!(
            thinking.parts,
            [TranscriptPart::Reasoning {
                text: "I will write the file.".into(),
                truncated: false
            }]
        );
        let reply = message(
            &record(json!({"type": "content.part", "uuid": "c2",
                "part": {"type": "text", "text": "Done."}})),
            20,
        )
        .unwrap();
        assert_eq!(
            reply.parts,
            [TranscriptPart::Text {
                text: "Done.".into(),
                truncated: false
            }]
        );
        let call = message(
            &record(
                json!({"type": "tool.call", "uuid": "t1", "toolCallId": "call_bash_1",
                "name": "Bash", "args": {"command": "cd sub && ls"},
                "display": {"kind": "command", "command": "cd sub && ls", "cwd": "/repo"}}),
            ),
            30,
        )
        .unwrap();
        assert_eq!(
            (call.role, call.id.as_str()),
            (TranscriptRole::Assistant, "t1")
        );
        assert_eq!(
            call.parts,
            [TranscriptPart::ToolCall {
                call_id: Some("call_bash_1".into()),
                name: "Bash".into(),
                input: r#"{"command":"cd sub && ls"}"#.into(),
                truncated: false
            }]
        );
        let failed = message(
            &record(json!({"type": "tool.result", "parentUuid": "t1",
                "toolCallId": "call_bash_1",
                "result": {"output": "No such file", "isError": true, "durationMs": 5}})),
            40,
        )
        .unwrap();
        assert_eq!(
            (failed.role, failed.id.as_str()),
            (TranscriptRole::Tool, "40")
        );
        assert_eq!(
            failed.parts,
            [TranscriptPart::ToolResult {
                call_id: Some("call_bash_1".into()),
                output: "No such file".into(),
                error: true,
                truncated: false
            }]
        );
        let parts = message(
            &record(json!({"type": "tool.result", "toolCallId": "call_read_1",
                "result": {"output": [{"type": "text", "text": "line 1"},
                    {"type": "image_url", "imageUrl": {"url": "data:…"}}]}})),
            50,
        )
        .unwrap();
        assert_eq!(
            parts.parts,
            [TranscriptPart::ToolResult {
                call_id: Some("call_read_1".into()),
                output: "line 1\n[image]".into(),
                error: false,
                truncated: false
            }]
        );
    }
}
