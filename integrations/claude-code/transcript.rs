//! Claude Code's conversation, from its transcript: each entry of the main
//! conversation is a message, at its byte offset. What the user typed is the
//! `user` role's (Claude Code's own insertions taken out, its paste blocks
//! as the text pasted), tool results the `tool` role's, and the assistant's
//! text, thinking and tool calls the `assistant` role's. What the user typed
//! while a turn ran is logged as a queued prompt, also the `user` role's.
//! Subagents' entries, meta entries and compaction summaries are left out, as
//! Claude Code's own view does.
//!
//! Read with its subagents (`subagents`), each subagent's own transcript
//! (`<conversation>/subagents/agent-<id>.jsonl`, its entries a sidechain) is
//! read the same way, and a subagent started in the background
//! (`run_in_background`) finishes as its call's result: the notice Claude Code
//! gives the agent when it ends (`<task-notification>`, failed unless
//! `completed`), not the acknowledgement it got when the subagent started.
use super::{assemble, call, compact, reasoning, result, text, timestamp, Reading};
use serde_json::Value;
use session_core::api::{TranscriptMessage, TranscriptPart, TranscriptRole};
use std::collections::HashMap;
use std::path::Path;

/// The message of the transcript entry at `position`, if it is one.
#[cfg(test)]
pub(super) fn message(entry: &Value, position: u64) -> Option<TranscriptMessage> {
    read(entry, position, &Reading::default())
}

/// The message of the transcript entry at `position`, if it is one, as
/// `reading` takes them.
pub(super) fn read(entry: &Value, position: u64, reading: &Reading) -> Option<TranscriptMessage> {
    let flagged = |key: &str| entry.get(key).and_then(Value::as_bool) == Some(true);
    if (flagged("isSidechain") && !reading.subagent)
        || flagged("isMeta")
        || flagged("isCompactSummary")
    {
        return None;
    }
    let at = entry
        .get("timestamp")
        .and_then(Value::as_str)
        .and_then(timestamp);
    let id = entry.get("uuid").and_then(Value::as_str);
    if let Some(started) = reading.subagents {
        if let Some((task, part)) = finished(entry, started) {
            // Claude Code may log the notice twice, queued while the agent
            // worked and then as its prompt: one id, so the later replaces it.
            let id = format!("{task}.finished");
            return assemble(Some(&id), position, TranscriptRole::Tool, at, vec![part]);
        }
    }
    let (role, mut parts) = match entry.get("type")?.as_str()? {
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
    if reading.subagents.is_some() && launched(entry) {
        // Its result comes when it finishes.
        parts.retain(|part| !matches!(part, TranscriptPart::ToolResult { .. }));
    }
    assemble(id, position, role, at, parts)
}

/// Whether `entry` is the acknowledgement of a subagent started in the
/// background, rather than its result.
fn launched(entry: &Value) -> bool {
    entry
        .get("toolUseResult")
        .and_then(|result| result.get("status"))
        .and_then(Value::as_str)
        == Some("async_launched")
}

/// The subagent a `<task-notification>` entry says ended, when it names one
/// of `started` (by its agent id, with the call that started it), and its
/// call's result: what it answered (or the notice's summary), failed unless
/// it `completed`. Claude Code logs the notice as a prompt, or as a
/// `queued_command` while the agent works.
fn finished<'a>(
    entry: &'a Value,
    started: &HashMap<String, Option<String>>,
) -> Option<(&'a str, TranscriptPart)> {
    let notice = match entry.get("type")?.as_str()? {
        "user" => match entry.get("message")?.get("content")? {
            Value::String(text) => text.as_str(),
            Value::Array(blocks) => blocks
                .iter()
                .find_map(|block| block.get("text")?.as_str())?,
            _ => return None,
        },
        "attachment" => {
            let attachment = entry.get("attachment")?;
            if attachment.get("type")?.as_str()? != "queued_command" {
                return None;
            }
            attachment.get("prompt")?.as_str()?
        }
        _ => return None,
    };
    let notice = notice.trim_start().strip_prefix("<task-notification>")?;
    let task = tag(notice, "task-id", false)?.trim();
    let call = started.get(task)?;
    let call_id = tag(notice, "tool-use-id", false)
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .or(call.as_deref())?;
    let status = tag(notice, "status", false).map(str::trim);
    let output = tag(notice, "result", true)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .or_else(|| tag(notice, "summary", false).map(str::trim))
        .unwrap_or_default();
    let part = result(Some(call_id), output, status != Some("completed"));
    Some((task, part))
}

/// The text of `<name>…</name>` in a notice, to its first closing (its last,
/// for one whose text can hold tags of its own).
fn tag<'a>(notice: &'a str, name: &str, last: bool) -> Option<&'a str> {
    let start = notice.find(&format!("<{name}>"))? + name.len() + 2;
    let rest = notice.get(start..)?;
    let close = format!("</{name}>");
    let end = if last {
        rest.rfind(&close)?
    } else {
        rest.find(&close)?
    };
    rest.get(..end)
}

/// A subagent's description, agent type and the call that started it, from
/// the `.meta.json` Claude Code keeps beside its transcript.
pub(super) fn subagent_meta(path: &Path) -> Option<(Option<String>, Option<String>)> {
    let meta: Value = serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;
    let field = |key: &str| {
        meta.get(key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(String::from)
    };
    let label = field("description").or_else(|| field("agentType"));
    Some((field("toolUseId"), label))
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

    /// A prompt of pastes is the user's whole text in the chat uploaded: a
    /// reply with files typed mid-turn (a queued paste block) and one typed
    /// while Claude Code waited, with text around the paste.
    #[test]
    fn pasted_prompt_bug_pasted_text_is_the_users_message_whole() {
        let block = |id: &str, text: &str| {
            format!("<pasted_content id=\"{id}\">\n{text}\n</pasted_content id=\"{id}\">")
        };
        let reply = "Here's what I sent from the web app.\n\n\
            /home/me/.rakumux/uploads/01JREPLY/image.png\n\
            /home/me/.rakumux/uploads/01JREPLY/image (2).png";
        let queued = json!({"type": "attachment", "uuid": "5110f9e2-c20b-4dbd-9606-b04ec4dc25b2",
            "timestamp": "2026-10-23T10:00:00.000Z",
            "attachment": {"type": "queued_command", "prompt": block("6524", reply),
                "commandMode": "prompt", "origin": {"kind": "human"}}});
        let typed = message(&queued, 90).unwrap();
        assert_eq!(typed.role, TranscriptRole::User);
        assert_eq!(texts(&typed), [format!("text: {reply}")]);
        let around = message(
            &entry(
                "user",
                json!(format!(
                    "Look at this\n\n{}\n\nWhy?<system-reminder>x</system-reminder>",
                    block("0b1c", "error: boom\nat line 3")
                )),
            ),
            0,
        )
        .unwrap();
        assert_eq!(
            texts(&around),
            ["text: Look at this\n\nerror: boom\nat line 3\n\nWhy?"]
        );
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
