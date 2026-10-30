//! Grok Build's conversation, from its session's `updates.jsonl`: one message
//! per line, at its byte offset, with its `eventId`. What the user typed is the
//! `user` role's (prompts Grok wakes itself with left out), its thinking, reply
//! blocks and tool calls the `assistant` role's, and a tool's result (its
//! update with a `status`) the `tool` role's. Bookkeeping (hooks, turn ends,
//! rewind markers, subagents' starts) is left out; a subagent's work is in its
//! own session's log.
use super::{assemble, call, compact, reasoning, result, text};
use serde_json::Value;
use session_core::api::{TranscriptMessage, TranscriptRole};

/// The message of the log line at `position`, if it is one.
pub(super) fn message(entry: &Value, position: u64) -> Option<TranscriptMessage> {
    let (update, kind) = super::super::grok::update(entry)?;
    let content = || update.get("content")?.get("text")?.as_str();
    let (role, parts) = match kind {
        "user_message_chunk" => {
            let typed = super::super::grok::typed(entry)?;
            let typed = super::super::typed_text(&typed)?;
            (TranscriptRole::User, text(&typed).into_iter().collect())
        }
        "agent_message_chunk" => (
            TranscriptRole::Assistant,
            text(content()?).into_iter().collect(),
        ),
        "agent_thought_chunk" => (
            TranscriptRole::Assistant,
            reasoning(content()?).into_iter().collect(),
        ),
        "tool_call" => {
            let (name, _) = super::super::grok::tool(update);
            let input = compact(update.get("rawInput").unwrap_or(&Value::Null));
            let id = update.get("toolCallId").and_then(Value::as_str);
            (
                TranscriptRole::Assistant,
                call(id, name?, &input).into_iter().collect(),
            )
        }
        "tool_call_update" => {
            let status = update.get("status").and_then(Value::as_str)?;
            if !matches!(status, "completed" | "failed") {
                return None;
            }
            let id = update.get("toolCallId").and_then(Value::as_str);
            (
                TranscriptRole::Tool,
                vec![result(id, &output(update), status == "failed")],
            )
        }
        _ => return None,
    };
    let meta = entry.get("params").and_then(|params| params.get("_meta"));
    assemble(
        meta.and_then(|meta| meta.get("eventId"))
            .and_then(Value::as_str),
        position,
        role,
        super::super::grok::timestamp(entry),
        parts,
    )
}

/// What a tool gave back, as its model read it: the text it was given
/// (`output_for_prompt`, `tool_output_for_prompt`, an answer's `message`), else
/// the update's text content, else its raw output as JSON.
fn output(update: &Value) -> String {
    let raw = update.get("rawOutput");
    let said = |value: &Value| {
        [
            "output_for_prompt",
            "tool_output_for_prompt",
            "message",
            "text",
            "content",
        ]
        .iter()
        .find_map(|key| value.get(key)?.as_str().map(str::to_owned))
    };
    // `{type: "Bash", output_for_prompt}`, or one level down:
    // `{type: "SearchReplace", EditsApplied: {tool_output_for_prompt}}`.
    let from_raw = raw.and_then(|raw| {
        said(raw).or_else(|| {
            raw.as_object()?
                .values()
                .filter(|value| value.is_object())
                .find_map(said)
        })
    });
    let content = || {
        let texts: Vec<&str> = update
            .get("content")?
            .as_array()?
            .iter()
            .filter_map(|block| block.get("content")?.get("text")?.as_str())
            .filter(|text| !text.trim().is_empty())
            .collect();
        (!texts.is_empty()).then(|| texts.join("\n"))
    };
    from_raw
        .or_else(content)
        .unwrap_or_else(|| raw.map(compact).unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use session_core::api::TranscriptPart;

    fn entry(n: u32, update: Value) -> Value {
        json!({"timestamp": 1_793_338_236, "method": "session/update",
            "params": {"sessionId": "01a18b49-8533-7252-8d65-46555f07d0c5", "update": update,
                "_meta": {"eventId": format!("01a18b49-8533-7252-8d65-46555f07d0c5-{n}"),
                    "agentTimestampMs": 1_793_338_236_400_i64, "promptId": "p1"}}})
    }

    fn parts(entry: &Value) -> (TranscriptRole, Vec<TranscriptPart>) {
        let message = message(entry, 10).unwrap();
        assert_eq!(message.position, 10);
        (message.role, message.parts)
    }

    #[test]
    fn prompts_replies_thinking_and_tools_are_messages_in_their_roles() {
        let typed = entry(
            4,
            json!({"sessionUpdate": "user_message_chunk",
            "content": {"type": "text", "text": "Fix the parser"}, "_meta": {"promptIndex": 0}}),
        );
        let message = message(&typed, 0).unwrap();
        assert_eq!(message.id, "01a18b49-8533-7252-8d65-46555f07d0c5-4");
        assert_eq!(message.at, Some(1_793_338_236_400));
        assert_eq!(
            (message.role, message.parts),
            (
                TranscriptRole::User,
                vec![TranscriptPart::Text {
                    text: "Fix the parser".into(),
                    truncated: false
                }]
            )
        );
        assert_eq!(
            parts(&entry(
                5,
                json!({"sessionUpdate": "agent_thought_chunk",
                "content": {"type": "text", "text": "Thinking."}})
            )),
            (
                TranscriptRole::Assistant,
                vec![TranscriptPart::Reasoning {
                    text: "Thinking.".into(),
                    truncated: false
                }]
            )
        );
        assert_eq!(
            parts(&entry(
                6,
                json!({"sessionUpdate": "agent_message_chunk",
                "content": {"type": "text", "text": "Done."}})
            )),
            (
                TranscriptRole::Assistant,
                vec![TranscriptPart::Text {
                    text: "Done.".into(),
                    truncated: false
                }]
            )
        );
        assert_eq!(
            parts(&entry(
                7,
                json!({"sessionUpdate": "tool_call", "toolCallId": "call_17_0",
                "title": "run_terminal_command", "rawInput": {"command": "ls", "description": "List"},
                "_meta": {"x.ai/tool": {"name": "run_terminal_command", "kind": "execute", "label": "Run Command"}}})
            )),
            (
                TranscriptRole::Assistant,
                vec![TranscriptPart::ToolCall {
                    call_id: Some("call_17_0".into()),
                    name: "run_terminal_command".into(),
                    input: r#"{"command":"ls","description":"List"}"#.into(),
                    truncated: false
                }]
            )
        );
    }

    #[test]
    fn a_tools_result_is_what_its_model_read() {
        let finished = |status: &str, raw: Value| {
            parts(&entry(
                8,
                json!({"sessionUpdate": "tool_call_update", "toolCallId": "call_17_0",
                "status": status, "content": [{"type": "content", "content": {"type": "text", "text": ""}}],
                "rawOutput": raw}),
            ))
        };
        let tool = |output: &str, error: bool| {
            (
                TranscriptRole::Tool,
                vec![TranscriptPart::ToolResult {
                    call_id: Some("call_17_0".into()),
                    output: output.into(),
                    error,
                    truncated: false,
                }],
            )
        };
        assert_eq!(
            finished(
                "completed",
                json!({"type": "Bash", "output_for_prompt": "exit: 0\n", "exit_code": 0})
            ),
            tool("exit: 0\n", false)
        );
        assert_eq!(
            finished(
                "completed",
                json!({"type": "SearchReplace", "EditsApplied":
                {"tool_output_for_prompt": "The file notes.txt has been updated successfully."}})
            ),
            tool("The file notes.txt has been updated successfully.", false)
        );
        assert_eq!(
            finished(
                "failed",
                json!({"type": "Text", "text": "permission denied"})
            ),
            tool("permission denied", true)
        );
        // The display enrichment before a result is no message.
        assert!(message(
            &entry(
                9,
                json!({"sessionUpdate": "tool_call_update", "toolCallId": "call_17_0",
            "title": "Execute `ls`", "content": []})
            ),
            0
        )
        .is_none());
    }

    #[test]
    fn groks_own_prompts_and_bookkeeping_are_left_out() {
        for update in [
            json!({"sessionUpdate": "user_message_chunk", "content": {"type": "text",
                "text": "<system-reminder>\nWhile you were idle, 1 background subagent completed\n</system-reminder>"}}),
            json!({"sessionUpdate": "hook_execution", "event_name": "stop"}),
            json!({"sessionUpdate": "turn_completed", "prompt_id": "p1", "stop_reason": "end_turn"}),
            json!({"sessionUpdate": "rewind_marker", "target_prompt_index": 0}),
            json!({"sessionUpdate": "subagent_spawned", "child_session_id": "c"}),
        ] {
            assert!(message(&entry(1, update.clone()), 0).is_none(), "{update}");
        }
    }
}
