//! Codex's latest output, from the rollout file of its thread.
use super::{contains, first_line, prompt_line, tool_line, Step};
use crate::host::ProcessInfo;
use serde_json::Value;
use std::path::PathBuf;

/// The rollout file the thread `process` runs is written to.
pub(super) fn log(process: &ProcessInfo) -> Option<PathBuf> {
    crate::resume::codex_open_rollout(process.pid)
}

/// Its reasoning summaries, tool calls and messages, as one line.
pub(super) fn line(entry: &Value) -> Option<String> {
    messages(entry)
        .iter()
        .rev()
        .find_map(|message| first_line(message))
}

/// An entry's messages, oldest first: reasoning summaries and replies as
/// written, tool calls as lines. Each counts once: a rollout records a
/// reply both as an event and as an item, and the event is the one taken
/// (the item for reasoning and tool calls).
pub(super) fn messages(entry: &Value) -> Vec<String> {
    let text = |value: Option<&Value>| {
        value
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .map(String::from)
    };
    let Some(payload) = entry.get("payload") else {
        return Vec::new();
    };
    let kinds = (
        entry.get("type").and_then(Value::as_str),
        payload.get("type").and_then(Value::as_str),
    );
    match kinds {
        (Some("event_msg"), Some("agent_message")) => {
            text(payload.get("message")).into_iter().collect()
        }
        (Some("response_item"), Some("reasoning")) => payload
            .get("summary")
            .and_then(Value::as_array)
            .map(|parts| {
                parts
                    .iter()
                    .filter_map(|part| text(part.get("text")))
                    .collect()
            })
            .unwrap_or_default(),
        _ => call(entry).into_iter().collect(),
    }
}

/// A tool call as a line.
fn call(entry: &Value) -> Option<String> {
    let payload = entry.get("payload")?;
    match (entry.get("type")?.as_str()?, payload.get("type")?.as_str()?) {
        ("response_item", "function_call") => {
            let name = payload.get("name")?.as_str()?;
            let arguments: Option<Value> = payload
                .get("arguments")
                .and_then(Value::as_str)
                .and_then(|arguments| serde_json::from_str(arguments).ok());
            let command = arguments.as_ref().and_then(command);
            match name {
                "shell" | "exec_command" | "local_shell" => tool_line("Shell", command.as_deref()),
                _ => tool_line(name, None),
            }
        }
        ("response_item", "custom_tool_call") => tool_line(payload.get("name")?.as_str()?, None),
        _ => None,
    }
}

/// What the user typed (`user_message`), as one line.
pub(super) fn prompt(entry: &Value) -> Option<String> {
    prompt_line(&typed(entry)?)
}

/// What the user typed (`user_message`), as written.
pub(super) fn typed(entry: &Value) -> Option<String> {
    let payload = entry.get("payload")?;
    match (entry.get("type")?.as_str()?, payload.get("type")?.as_str()?) {
        ("event_msg", "user_message") => Some(payload.get("message")?.as_str()?.to_string()),
        _ => None,
    }
}

/// Whether a log line may be one [`typed`] takes, before it's parsed.
pub(super) fn may_be_typed(line: &[u8]) -> bool {
    contains(line, br#""user_message""#)
}

/// A shell call's command, without its `bash -lc` wrapper.
fn command(arguments: &Value) -> Option<String> {
    match arguments.get("command").or_else(|| arguments.get("cmd"))? {
        Value::String(command) => Some(command.clone()),
        Value::Array(parts) => {
            let parts: Vec<&str> = parts.iter().filter_map(Value::as_str).collect();
            let wrapped = parts.len() == 3
                && matches!(parts[0], "bash" | "sh" | "zsh")
                && matches!(parts[1], "-c" | "-lc");
            Some(if wrapped {
                parts[2].to_string()
            } else {
                parts.join(" ")
            })
        }
        _ => None,
    }
}

/// How an entry changes the turn's reply: a new turn starts it over, and its
/// `task_complete` holds the whole reply (`last_agent_message`).
pub(super) fn reply_step(entry: &Value) -> Step {
    let Some(payload) = entry.get("payload") else {
        return Step::Keep;
    };
    let kind = |value: &Value| value.get("type").and_then(Value::as_str).map(String::from);
    if kind(entry).as_deref() != Some("event_msg") {
        return Step::Keep;
    }
    match kind(payload).as_deref() {
        Some("user_message" | "task_started") => Step::Reset,
        Some("task_complete") => {
            let text = payload
                .get("last_agent_message")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|text| !text.is_empty());
            match (text, payload.get("turn_id").and_then(Value::as_str)) {
                (Some(text), Some(turn)) => Step::Complete {
                    key: turn.to_owned(),
                    text: text.to_owned(),
                },
                _ => Step::Reset,
            }
        }
        _ => Step::Keep,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn reasoning_tool_calls_and_messages_are_lines() {
        assert_eq!(
            line(
                &json!({"type": "response_item", "payload": {"type": "reasoning",
                "summary": [{"type": "summary_text", "text": "**Exploring the parser**\n\nI think…"}]}})
            ),
            Some("Exploring the parser".into())
        );
        assert_eq!(
            line(
                &json!({"type": "response_item", "payload": {"type": "function_call", "name": "shell",
                "arguments": "{\"command\":[\"bash\",\"-lc\",\"cargo test -p tui\"]}"}})
            ),
            Some("Shell: cargo test -p tui".into())
        );
        assert_eq!(
            line(
                &json!({"type": "response_item", "payload": {"type": "custom_tool_call", "name": "apply_patch", "input": "secret"}})
            ),
            Some("apply_patch".into())
        );
        assert_eq!(
            line(
                &json!({"type": "event_msg", "payload": {"type": "agent_message", "message": "Done: tests pass."}})
            ),
            Some("Done: tests pass.".into())
        );
    }

    #[test]
    fn user_messages_are_prompts() {
        assert_eq!(
            prompt(
                &json!({"type": "event_msg", "payload": {"type": "user_message", "message": "Run the tests"}})
            ),
            Some("Run the tests".into())
        );
        assert_eq!(
            prompt(
                &json!({"type": "event_msg", "payload": {"type": "agent_message", "message": "Done"}})
            ),
            None
        );
    }

    #[test]
    fn prompts_and_tool_output_are_not_lines() {
        assert_eq!(
            line(
                &json!({"type": "event_msg", "payload": {"type": "user_message", "message": "my prompt"}})
            ),
            None
        );
        assert_eq!(
            line(
                &json!({"type": "response_item", "payload": {"type": "function_call_output", "output": "secret"}})
            ),
            None
        );
    }

    fn event(payload: Value) -> Value {
        json!({"timestamp": "2026-10-23T00:00:00Z", "type": "event_msg", "payload": payload})
    }

    #[test]
    fn the_reply_is_the_finished_turns_last_message() {
        assert_eq!(
            reply_step(&event(json!({"type": "task_complete", "turn_id": "t1",
                "last_agent_message": "All done.\n"}))),
            Step::Complete {
                key: "t1".into(),
                text: "All done.".into()
            }
        );
        assert_eq!(
            reply_step(&event(json!({"type": "task_complete", "turn_id": "t2",
                "last_agent_message": null}))),
            Step::Reset
        );
        assert_eq!(
            reply_step(&event(json!({"type": "user_message", "message": "next"}))),
            Step::Reset
        );
        assert_eq!(
            reply_step(&event(
                json!({"type": "agent_message", "message": "Working on it"})
            )),
            Step::Keep
        );
    }
}
