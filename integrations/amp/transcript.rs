//! Amp's thread, from the copy the Rakumux plugin keeps: one message per
//! line, at its byte offset, with Amp's message id. A user message is what
//! was typed (the `user` role's), or tools' results (the `tool` role's); an
//! assistant message its text, thinking and tool calls. Info messages (Amp's
//! own notices) and where turns ended are left out.
use super::{assemble, call, compact, reasoning, result, text, timestamp};
use serde_json::Value;
use session_core::api::{TranscriptMessage, TranscriptPart, TranscriptRole};

/// The message of the line at `position`, if it is one.
pub(super) fn message(entry: &Value, position: u64) -> Option<TranscriptMessage> {
    if entry.get("kind")?.as_str()? != "message" {
        return None;
    }
    let blocks = entry.get("content")?.as_array()?;
    let (role, parts) = match entry.get("role")?.as_str()? {
        "user" => user(blocks),
        "assistant" => (TranscriptRole::Assistant, assistant(blocks)),
        _ => return None,
    };
    let at = entry
        .get("timestamp")
        .and_then(Value::as_str)
        .and_then(timestamp);
    let id = super::super::amp::id(entry);
    assemble(id.as_deref(), position, role, at, parts)
}

/// A user message's parts: what was typed, or tools' results (the `tool`
/// role).
fn user(blocks: &[Value]) -> (TranscriptRole, Vec<TranscriptPart>) {
    let mut parts = Vec::new();
    let mut tool = false;
    for block in blocks {
        let field = |key: &str| block.get(key).and_then(Value::as_str);
        match field("type") {
            Some("text") => parts.extend(field("text").and_then(text)),
            Some("tool_result") => {
                tool = true;
                parts.push(result(
                    field("toolUseID"),
                    &compact(block.get("output").unwrap_or(&Value::Null)),
                    field("status") == Some("error"),
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

/// An assistant message's text, thinking and tool calls, in order.
fn assistant(blocks: &[Value]) -> Vec<TranscriptPart> {
    blocks
        .iter()
        .filter_map(|block| {
            let field = |key: &str| block.get(key).and_then(Value::as_str);
            match field("type")? {
                "text" => text(field("text")?),
                "thinking" => reasoning(field("thinking")?),
                "tool_use" => call(
                    field("id"),
                    field("name")?,
                    &compact(block.get("input").unwrap_or(&Value::Null)),
                ),
                _ => None,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::super::{LogAgent, TranscriptSource};
    use super::*;
    use serde_json::json;
    use session_core::api::MAX_PART_BYTES;

    const FIXTURE: &str = include_str!("fixtures/thread.jsonl");

    fn source(directory: &std::path::Path) -> TranscriptSource {
        let path = directory.join("thread.jsonl");
        std::fs::write(&path, FIXTURE).unwrap();
        TranscriptSource::Log {
            agent: LogAgent::Amp,
            conversation: "T-5f0c1d2e-3a4b-4c5d-8e9f-0a1b2c3d4e5f".into(),
            path,
        }
    }

    #[test]
    fn a_thread_reads_as_prompts_replies_tool_calls_and_their_results() {
        let directory = tempfile::tempdir().unwrap();
        let source = source(directory.path());
        let page = source.read(None, None).unwrap();
        assert_eq!(page.agent, "amp");
        assert_eq!(
            page.conversation_id,
            "T-5f0c1d2e-3a4b-4c5d-8e9f-0a1b2c3d4e5f"
        );
        assert!(!page.more);
        let roles: Vec<_> = page
            .messages
            .iter()
            .map(|m| (m.id.as_str(), m.role))
            .collect();
        use TranscriptRole::{Assistant, Tool, User};
        assert_eq!(
            roles,
            [
                ("msg-1", User),
                ("msg-2", Assistant),
                ("msg-3", Tool),
                ("msg-4", Assistant),
                ("msg-5", Tool),
                ("msg-6", Assistant),
                ("7", User),
                ("8", Assistant),
            ]
        );
        let first = &page.messages[0];
        assert_eq!((first.position, first.at), (0, Some(1_792_749_600_000)));
        assert_eq!(
            first.parts,
            [TranscriptPart::Text {
                text: "Fix the failing parser tests".into(),
                truncated: false
            }]
        );
        assert_eq!(
            page.messages[1].parts,
            [
                TranscriptPart::Reasoning {
                    text: "The test fails on a trailing newline.".into(),
                    truncated: false
                },
                TranscriptPart::ToolCall {
                    call_id: Some("toolu_edit".into()),
                    name: "edit_file".into(),
                    input: r#"{"new_str":"line.trim_end()","old_str":"line.trim()","path":"/repo/src/parser.rs"}"#.into(),
                    truncated: false
                }
            ]
        );
        assert_eq!(
            page.messages[4].parts,
            [
                TranscriptPart::ToolResult {
                    call_id: Some("toolu_bash".into()),
                    output: String::new(),
                    error: false,
                    truncated: false
                },
                TranscriptPart::ToolResult {
                    call_id: Some("toolu_patch".into()),
                    output: "patch did not apply".into(),
                    error: true,
                    truncated: false
                }
            ]
        );
        // The cursor reads on from the next line.
        let again = source.read(Some(&page.cursor), None).unwrap();
        assert!(again.messages.is_empty());
        assert_eq!(page.cursor.offset, FIXTURE.len() as u64);
        let one = source.read(None, Some(1)).unwrap();
        assert_eq!(one.messages.len(), 1);
        assert!(one.more);
        let next = source.read(Some(&one.cursor), Some(1)).unwrap();
        assert_eq!(next.messages[0].id, "msg-2");
    }

    #[test]
    fn notices_turn_ends_and_long_parts() {
        let info = json!({"v": 1, "kind": "message", "id": "i", "role": "info",
            "content": [{"type": "text", "text": "Thread compacted"}]});
        assert_eq!(message(&info, 0), None);
        let end = json!({"v": 1, "kind": "turn_end", "id": "msg-1", "status": "done"});
        assert_eq!(message(&end, 0), None);
        let long = json!({"v": 1, "kind": "message", "id": "u", "role": "user",
            "content": [{"type": "text", "text": "é".repeat(MAX_PART_BYTES)}]});
        match &message(&long, 0).unwrap().parts[0] {
            TranscriptPart::Text { text, truncated } => {
                assert!(*truncated);
                assert!(text.len() <= MAX_PART_BYTES);
            }
            part => panic!("{part:?}"),
        }
        // Structured tool output is kept as JSON.
        let structured = json!({"v": 1, "kind": "message", "id": "r", "role": "user",
            "content": [{"type": "tool_result", "toolUseID": "t", "status": "done",
                "output": {"output": "ok", "exitCode": 0}}]});
        match &message(&structured, 0).unwrap().parts[0] {
            TranscriptPart::ToolResult { output, .. } => {
                assert_eq!(output, r#"{"exitCode":0,"output":"ok"}"#)
            }
            part => panic!("{part:?}"),
        }
    }
}
