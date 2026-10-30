//! GitHub Copilot CLI's conversation, from its event log: one message per
//! event of the main agent, at its byte offset, with the event's `id`. What
//! the user typed (`user.message` without a `source`) is the `user` role's;
//! the assistant's messages (their reasoning, text and tool calls) and its
//! reasoning events the `assistant` role's; tools' results the `tool`
//! role's. Hooks' records, system prompts and bookkeeping are left out, as
//! Copilot's own timeline does. Reasoning Copilot logged both as an event and
//! on its message counts once.
//!
//! Subagents' events (those with an `agentId`, interleaved with the main
//! agent's) are left out too, unless read with subagents: then each is its
//! subagent's message, marked with the `task` call that started it (its
//! events' `parentToolCallId`, or the call its `subagent.started` names) and
//! the name that event gives it; its prompt is the `user.message` Copilot
//! sends it.
use super::{assemble, call, compact, reasoning, result, text, timestamp};
use serde_json::Value;
use session_core::api::{TranscriptMessage, TranscriptRole, TranscriptSubagent};
use std::collections::{HashMap, HashSet};

/// Reads a log's messages in order, remembering the reasoning already
/// taken from its events, and the subagents started in what was read.
#[derive(Default)]
pub(super) struct Reading {
    reasoning: HashSet<String>,
    /// By agent id: the call that started it and its name.
    subagents: HashMap<String, (Option<String>, Option<String>)>,
}

impl Reading {
    /// The message of the event at `position`, if it is one: a subagent's
    /// only `with_subagents`.
    pub(super) fn message(
        &mut self,
        entry: &Value,
        position: u64,
        with_subagents: bool,
    ) -> Option<TranscriptMessage> {
        let agent = entry
            .get("agentId")
            .and_then(Value::as_str)
            .filter(|agent| !agent.is_empty());
        if entry.get("agentId").is_some_and(|agent| !agent.is_null()) {
            let agent = agent?;
            if !with_subagents {
                return None;
            }
            let message = self.main(entry, position, true)?;
            let data = entry.get("data");
            let field = |key: &str| {
                data.and_then(|data| data.get(key))
                    .and_then(Value::as_str)
                    .map(String::from)
            };
            let known = self.subagents.get(agent);
            let marker = TranscriptSubagent {
                id: agent.to_owned(),
                call_id: field("parentToolCallId")
                    .or_else(|| known.and_then(|(call, _)| call.clone()))
                    .filter(|id| super::is_call_id(id)),
                label: known.and_then(|(_, label)| label.clone()),
            };
            return super::subagents::mark(message, &marker);
        }
        self.main(entry, position, false)
    }

    /// The message of a main agent's event (or, `subagent`, a subagent's).
    fn main(&mut self, entry: &Value, position: u64, subagent: bool) -> Option<TranscriptMessage> {
        let data = entry.get("data")?;
        let field = |key: &str| data.get(key).and_then(Value::as_str);
        let (role, parts) = match entry.get("type")?.as_str()? {
            "subagent.started" if subagent => {
                let agent = entry.get("agentId").and_then(Value::as_str)?;
                let label = ["agentDisplayName", "agentName", "agentType"]
                    .iter()
                    .find_map(|key| field(key).map(str::trim).filter(|name| !name.is_empty()))
                    .map(|name| name.chars().take(120).collect());
                self.subagents.insert(
                    agent.to_owned(),
                    (field("toolCallId").map(String::from), label),
                );
                return None;
            }
            // What Copilot sent the subagent to do.
            "user.message" if subagent => {
                let parts = field("content").and_then(text).into_iter().collect();
                (TranscriptRole::User, parts)
            }
            "user.message" => {
                let typed = super::super::copilot::typed(entry)?;
                let parts = super::super::typed_text(&typed)
                    .and_then(|typed| text(&typed))
                    .into_iter()
                    .collect();
                (TranscriptRole::User, parts)
            }
            "assistant.reasoning" => {
                let thought = field("content").unwrap_or_default().trim();
                self.remember(thought);
                (
                    TranscriptRole::Assistant,
                    reasoning(thought).into_iter().collect(),
                )
            }
            "assistant.message" => {
                let mut parts = Vec::new();
                let thought = field("reasoningText").unwrap_or_default().trim();
                if !self.reasoning.remove(thought) {
                    parts.extend(reasoning(thought));
                }
                parts.extend(field("content").and_then(text));
                for request in super::super::copilot::requests(data) {
                    let arguments = super::super::copilot::arguments(request.get("arguments"));
                    parts.extend(call(
                        request.get("toolCallId").and_then(Value::as_str),
                        request
                            .get("name")
                            .and_then(Value::as_str)
                            .unwrap_or_default(),
                        &compact(&arguments),
                    ));
                }
                (TranscriptRole::Assistant, parts)
            }
            "tool.execution_complete" => {
                let failed = data.get("success").and_then(Value::as_bool) == Some(false);
                let output = if failed {
                    data.get("error")
                        .and_then(|error| error.get("message"))
                        .and_then(Value::as_str)
                } else {
                    let result = data.get("result");
                    let content = |key: &str| {
                        result?
                            .get(key)?
                            .as_str()
                            .filter(|content| !content.trim().is_empty())
                    };
                    content("content").or_else(|| content("detailedContent"))
                };
                let part = result(field("toolCallId"), output.unwrap_or_default(), failed);
                (TranscriptRole::Tool, vec![part])
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

    /// Reasoning the message after it may repeat (a few wait at most).
    fn remember(&mut self, thought: &str) {
        if self.reasoning.len() >= 16 {
            self.reasoning.clear();
        }
        if !thought.is_empty() {
            self.reasoning.insert(thought.to_owned());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use session_core::api::TranscriptPart;

    fn event(id: &str, kind: &str, data: Value) -> Value {
        json!({"type": kind, "data": data, "id": id,
            "timestamp": "2026-10-23T10:00:00.000Z", "parentId": null})
    }

    #[test]
    fn what_the_user_typed_is_the_users_and_its_id_the_events() {
        let mut reading = Reading::default();
        let typed = reading
            .message(
                &event(
                    "6c0d7a52-9b1e-4f3a-8d2c-1e0f9a8b7c6d",
                    "user.message",
                    json!({"content": "Run the tests", "transformedContent": "<current_datetime>x</current_datetime>\n\nRun the tests"}),
                ),
                120,
                false,
            )
            .unwrap();
        assert_eq!(typed.role, TranscriptRole::User);
        assert_eq!(typed.id, "6c0d7a52-9b1e-4f3a-8d2c-1e0f9a8b7c6d");
        assert_eq!(typed.position, 120);
        assert_eq!(typed.at, Some(1_792_749_600_000));
        assert_eq!(
            typed.parts,
            [TranscriptPart::Text {
                text: "Run the tests".into(),
                truncated: false
            }]
        );
        // What Copilot sends itself, a subagent's, hooks' records and the
        // system prompt aren't messages.
        for entry in [
            event(
                "e1",
                "user.message",
                json!({"content": "skill", "source": "skill-pdf"}),
            ),
            json!({"type": "user.message", "agentId": "child", "id": "e2",
                "data": {"content": "task", "source": "agent-root"}}),
            json!({"type": "assistant.message", "agentId": "child", "id": "e3",
                "data": {"messageId": "m", "content": "child reply", "toolRequests": []}}),
            event("e4", "hook.start", json!({"input": {"prompt": "secret"}})),
            event("e5", "system.message", json!({"content": "You are…"})),
            event("e6", "assistant.turn_start", json!({"turnId": "0"})),
            event(
                "e7",
                "session.start",
                json!({"sessionId": "s", "version": 1}),
            ),
        ] {
            assert_eq!(reading.message(&entry, 0, false), None, "{entry}");
        }
    }

    #[test]
    fn the_assistants_reasoning_text_and_calls_then_the_tools_results() {
        let mut reading = Reading::default();
        let message = reading
            .message(
                &event(
                    "e1",
                    "assistant.message",
                    json!({"messageId": "m1", "content": "Creating it.", "reasoningText": "Plan the file",
                        "toolRequests": [{"toolCallId": "call_1", "name": "create",
                            "arguments": {"path": "/home/me/project/todo.txt", "file_text": "x"}}]}),
                ),
                0,
                false,
            )
            .unwrap();
        assert_eq!(message.role, TranscriptRole::Assistant);
        assert_eq!(
            message.parts,
            [
                TranscriptPart::Reasoning {
                    text: "Plan the file".into(),
                    truncated: false
                },
                TranscriptPart::Text {
                    text: "Creating it.".into(),
                    truncated: false
                },
                TranscriptPart::ToolCall {
                    call_id: Some("call_1".into()),
                    name: "create".into(),
                    input: r#"{"file_text":"x","path":"/home/me/project/todo.txt"}"#.into(),
                    truncated: false
                }
            ]
        );
        let done = reading
            .message(
                &event(
                    "e2",
                    "tool.execution_complete",
                    json!({"toolCallId": "call_1", "success": true,
                        "result": {"content": "Created file /home/me/project/todo.txt", "detailedContent": "diff"},
                        "fileEdits": [{"path": "/home/me/project/todo.txt", "kind": "create"}]}),
                ),
                0,
                false,
            )
            .unwrap();
        assert_eq!(done.role, TranscriptRole::Tool);
        assert_eq!(
            done.parts,
            [TranscriptPart::ToolResult {
                call_id: Some("call_1".into()),
                output: "Created file /home/me/project/todo.txt".into(),
                error: false,
                truncated: false
            }]
        );
        let failed = reading
            .message(
                &event(
                    "e3",
                    "tool.execution_complete",
                    json!({"toolCallId": "call_2", "success": false,
                        "error": {"message": "Parent directory does not exist", "code": "failure"}}),
                ),
                0,
                false,
            )
            .unwrap();
        assert_eq!(
            failed.parts,
            [TranscriptPart::ToolResult {
                call_id: Some("call_2".into()),
                output: "Parent directory does not exist".into(),
                error: true,
                truncated: false
            }]
        );
    }

    #[test]
    fn reasoning_logged_twice_counts_once() {
        let mut reading = Reading::default();
        let thought = reading
            .message(
                &event(
                    "e1",
                    "assistant.reasoning",
                    json!({"reasoningId": "r1", "content": "Think it through"}),
                ),
                0,
                false,
            )
            .unwrap();
        assert_eq!(
            thought.parts,
            [TranscriptPart::Reasoning {
                text: "Think it through".into(),
                truncated: false
            }]
        );
        let message = reading
            .message(
                &event(
                    "e2",
                    "assistant.message",
                    json!({"messageId": "m1", "content": "Answer", "reasoningText": "Think it through", "toolRequests": []}),
                ),
                0,
                false,
            )
            .unwrap();
        assert_eq!(
            message.parts,
            [TranscriptPart::Text {
                text: "Answer".into(),
                truncated: false
            }]
        );
        // Only one with nothing to show is no message.
        assert_eq!(
            reading.message(
                &event(
                    "e3",
                    "assistant.message",
                    json!({"messageId": "m2", "content": "", "toolRequests": []})
                ),
                0,
                false
            ),
            None
        );
    }

    #[test]
    fn a_subagents_events_are_its_messages_when_read_with_subagents() {
        let child = "8b1c0f2e-4d3a-4e5f-9a6b-7c8d9e0f1a2b";
        let events = [
            json!({"type": "subagent.started", "agentId": child, "id": "e1",
                "data": {"toolCallId": "call_task", "agentName": "explore",
                    "agentDisplayName": "looker", "agentType": "explore"}}),
            json!({"type": "user.message", "agentId": child, "id": "e2",
                "data": {"content": "Find the flaky test", "source": "agent-root"}}),
            json!({"type": "assistant.message", "agentId": child, "id": "e3",
                "data": {"messageId": "m", "content": "Found it.", "toolRequests": [],
                    "parentToolCallId": "call_task"}}),
            json!({"type": "hook.start", "agentId": child, "id": "e4",
                "data": {"input": {}, "parentToolCallId": "call_task"}}),
        ];
        let mut reading = Reading::default();
        let messages: Vec<TranscriptMessage> = events
            .iter()
            .enumerate()
            .filter_map(|(n, entry)| reading.message(entry, n as u64 * 100, true))
            .collect();
        let ids: Vec<&str> = messages.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, [format!("{child}:e2"), format!("{child}:e3")]);
        let marker = TranscriptSubagent {
            id: child.into(),
            call_id: Some("call_task".into()),
            label: Some("looker".into()),
        };
        assert!(messages
            .iter()
            .all(|m| m.subagent.as_ref() == Some(&marker)));
        assert_eq!(messages[0].role, TranscriptRole::User);
        assert_eq!(messages[1].role, TranscriptRole::Assistant);
        // Without subagents, none of them.
        let mut plain = Reading::default();
        assert!(events
            .iter()
            .all(|entry| plain.message(entry, 0, false).is_none()));
    }
}
