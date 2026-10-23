//! Codex's latest output, from the rollout file of its thread.
use super::{first_line, tool_line};
use crate::host::ProcessInfo;
use serde_json::Value;
use std::path::PathBuf;

/// The rollout file the thread `process` runs is written to.
pub(super) fn log(process: &ProcessInfo) -> Option<PathBuf> {
    crate::resume::codex_open_rollout(process.pid)
}

/// Its reasoning summaries, tool calls and messages.
pub(super) fn line(entry: &Value) -> Option<String> {
    let payload = entry.get("payload")?;
    match (entry.get("type")?.as_str()?, payload.get("type")?.as_str()?) {
        ("event_msg", "agent_message") => first_line(payload.get("message")?.as_str()?),
        ("event_msg", "agent_reasoning") => first_line(payload.get("text")?.as_str()?),
        ("response_item", "reasoning") => payload
            .get("summary")?
            .as_array()?
            .iter()
            .rev()
            .find_map(|part| first_line(part.get("text")?.as_str()?)),
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
}
