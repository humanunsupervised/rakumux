//! Droid's conversation, from its session file: each message line is a
//! message, at its offset past the file's first line. What the user typed is
//! the `user` role's, tools' results (which Droid writes as the user's) the
//! `tool` role's, and the agent's text, reasoning and tool calls the
//! `assistant` role's. Droid's own rows (hooks', notices for the model or the
//! user only, other agents' messages), the session's summary, compaction
//! states and turn outcomes are left out.
use super::{assemble, call, compact, reasoning, result, text, timestamp};
use serde_json::Value;
use session_core::api::{TranscriptMessage, TranscriptPart, TranscriptRole};

/// The message of the session-file line at `position`, if it is one.
pub(super) fn message(entry: &Value, position: u64) -> Option<TranscriptMessage> {
    if entry.get("type")?.as_str()? != "message" {
        return None;
    }
    let message = entry.get("message")?;
    let own = message.get("hookEventName").is_some()
        || message.get("sender").is_some()
        || matches!(
            message.get("visibility").and_then(Value::as_str),
            Some("llm_only" | "user_only")
        );
    if own {
        return None;
    }
    let content = message.get("content")?;
    let (role, parts) = match message.get("role")?.as_str()? {
        "user" => user(content),
        "assistant" => (TranscriptRole::Assistant, assistant(message, content)),
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

/// A user message's parts: what was typed, or tools' results (the `tool` role).
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

/// What the user typed, without what Droid adds to it (reminders).
fn prompt(typed: &str) -> Option<TranscriptPart> {
    text(&super::super::typed_text(typed)?)
}

/// An assistant message's reasoning, text and tool calls, in order; a model's
/// reasoning summary first when it wrote no thinking block.
fn assistant(message: &Value, content: &Value) -> Vec<TranscriptPart> {
    let blocks = match content {
        Value::String(reply) => return text(reply).into_iter().collect(),
        Value::Array(blocks) => blocks.as_slice(),
        _ => &[],
    };
    let mut parts = Vec::new();
    if !blocks.iter().any(|block| block["type"] == "thinking") {
        parts.extend(super::super::droid::reasoning(message).and_then(reasoning));
    }
    parts.extend(blocks.iter().filter_map(|block| {
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
    }));
    parts
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
    use super::super::super::droid::tests::{
        append, body_position, hook_row, message as line, retitle, write_session, ID,
    };
    use super::super::{LogAgent, TranscriptSource};
    use super::*;
    use serde_json::json;

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
    fn prompts_replies_calls_and_results_keep_their_roles() {
        let typed = message(
            &line(
                "m1",
                "user",
                json!("Fix the test<system-reminder>be careful</system-reminder>"),
            ),
            40,
        )
        .unwrap();
        assert_eq!(typed.role, TranscriptRole::User);
        assert_eq!((typed.id.as_str(), typed.position), ("m1", 40));
        assert_eq!(typed.at, Some(1_793_354_400_000));
        assert_eq!(texts(&typed), ["text: Fix the test"]);
        let reply = message(
            &line(
                "m2",
                "assistant",
                json!([
                    {"type": "thinking", "thinking": "The form submits early.", "signature": "s"},
                    {"type": "redacted_thinking", "data": "x"},
                    {"type": "text", "text": "Running the tests."},
                    {"type": "tool_use", "id": "call_1", "name": "Execute",
                     "input": {"command": "npm test", "summary": "Run the tests"}}
                ]),
            ),
            90,
        )
        .unwrap();
        assert_eq!(reply.role, TranscriptRole::Assistant);
        assert_eq!(
            texts(&reply),
            [
                "reasoning: The form submits early.",
                "text: Running the tests.",
                r#"call call_1: Execute {"command":"npm test","summary":"Run the tests"}"#,
            ]
        );
        let results = message(
            &line(
                "m3",
                "user",
                json!([
                    {"type": "tool_result", "tool_use_id": "call_1", "is_error": true,
                     "content": "1 failing"},
                    {"type": "tool_result", "tool_use_id": "call_2",
                     "content": [{"type": "text", "text": "ok"}, {"type": "image", "source": {}}]}
                ]),
            ),
            150,
        )
        .unwrap();
        assert_eq!(results.role, TranscriptRole::Tool);
        assert_eq!(
            texts(&results),
            [
                "result call_1 (error): 1 failing",
                "result call_2: ok\n[image]"
            ]
        );
        // A GPT model's summary of its reasoning.
        let mut gpt = line(
            "m4",
            "assistant",
            json!([{"type": "text", "text": "Done."}]),
        );
        gpt["message"]["openaiReasoningSummary"] = json!("Checked the parser.");
        assert_eq!(
            texts(&message(&gpt, 0).unwrap()),
            ["reasoning: Checked the parser.", "text: Done."]
        );
    }

    #[test]
    fn droids_own_rows_are_left_out() {
        assert_eq!(message(&hook_row("h", "Stop"), 0), None);
        for visibility in ["llm_only", "user_only"] {
            let mut notice = line("n", "user", json!([{"type": "text", "text": "Notice"}]));
            notice["message"]["visibility"] = json!(visibility);
            assert_eq!(message(&notice, 0), None, "{visibility}");
        }
        let mut peer = line("p", "user", json!("From another session"));
        peer["message"]["sender"] = json!({"kind": "session", "sessionId": "s"});
        assert_eq!(message(&peer, 0), None);
        for other in [
            json!({"type": "session_start", "id": ID, "title": "t"}),
            json!({"type": "compaction_state", "id": "c", "summaryText": "x"}),
            json!({"type": "agent_turn_outcome", "turnId": "m1", "reason": "completed"}),
        ] {
            assert_eq!(message(&other, 0), None, "{other}");
        }
    }

    #[test]
    fn a_cursor_survives_droid_renaming_the_session() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join(format!("{ID}.jsonl"));
        write_session(
            &path,
            "New session",
            &[
                line("m1", "user", json!("Fix it")),
                hook_row("h1", "UserPromptSubmit"),
                line(
                    "m2",
                    "assistant",
                    json!([{"type": "text", "text": "Done."}]),
                ),
            ],
        );
        let source = TranscriptSource::Log {
            agent: LogAgent::Droid,
            conversation: ID.into(),
            path: path.clone(),
        };
        let page = source.read(None, None, false).unwrap();
        assert_eq!(page.agent, "droid");
        assert_eq!(page.conversation_id, ID);
        let ids: Vec<(&str, u64)> = page
            .messages
            .iter()
            .map(|message| (message.id.as_str(), message.position))
            .collect();
        assert_eq!(ids, [("m1", 0), ("m2", body_position(&path, "\"m2\""))]);
        assert!(!page.more);
        retitle(&path, "Fix the flaky login test in the browser suite");
        append(&path, &line("m3", "user", json!("Now the docs")));
        let next = source.read(Some(&page.cursor), None, false).unwrap();
        let ids: Vec<(&str, u64)> = next
            .messages
            .iter()
            .map(|message| (message.id.as_str(), message.position))
            .collect();
        assert_eq!(ids, [("m3", body_position(&path, "\"m3\""))]);
        // A small budget reads a line or two at a time, and says there's more.
        let first = source.read(None, Some(1), false).unwrap();
        assert!(first.more);
        assert_eq!(first.messages.len(), 1);
        // Another conversation's cursor reads this one from its start.
        let other = session_core::api::TranscriptCursor {
            conversation: "11111111-1111-4111-8111-111111111111".into(),
            ..next.cursor.clone()
        };
        assert_eq!(
            source
                .read(Some(&other), None, false)
                .unwrap()
                .messages
                .len(),
            3
        );
    }
}
