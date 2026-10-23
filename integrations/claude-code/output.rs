//! Claude Code's latest output, from its conversation transcript.
use super::{first_line, short_path, tool_line};
use crate::host::ProcessInfo;
use serde_json::Value;
use std::path::PathBuf;

/// The most project directories searched for a transcript.
const MAX_PROJECTS: usize = 4096;

/// The transcript of the conversation `process` runs:
/// `<config dir>/projects/<project>/<conversation>.jsonl`.
pub(super) fn log(process: &ProcessInfo) -> Option<PathBuf> {
    let config = crate::resume::claude_config_dir()?;
    let id = crate::resume::claude_conversation(process, &config)?;
    let name = format!("{id}.jsonl");
    std::fs::read_dir(config.join("projects"))
        .ok()?
        .filter_map(Result::ok)
        .take(MAX_PROJECTS)
        .map(|entry| entry.path().join(&name))
        .find(|path| path.is_file())
}

/// An entry of the main conversation's assistant (a subagent's are its own):
/// its last thinking, tool call or text.
pub(super) fn line(entry: &Value) -> Option<String> {
    if entry.get("type")?.as_str()? != "assistant"
        || entry.get("isSidechain").and_then(Value::as_bool) == Some(true)
    {
        return None;
    }
    entry
        .get("message")?
        .get("content")?
        .as_array()?
        .iter()
        .rev()
        .find_map(block)
}

fn block(block: &Value) -> Option<String> {
    match block.get("type")?.as_str()? {
        "thinking" => first_line(block.get("thinking")?.as_str()?),
        "text" => first_line(block.get("text")?.as_str()?),
        "tool_use" => {
            let name = block.get("name")?.as_str()?;
            let field = |key: &str| block.get("input")?.get(key)?.as_str();
            let subject = match name {
                // Its own one-line description says more than the command.
                "Bash" => field("description")
                    .or_else(|| field("command"))
                    .map(String::from),
                "Read" | "Write" | "Edit" | "MultiEdit" | "NotebookEdit" => field("file_path")
                    .or_else(|| field("notebook_path"))
                    .map(short_path),
                "Grep" | "Glob" => field("pattern").map(String::from),
                "WebFetch" => field("url").map(String::from),
                "WebSearch" => field("query").map(String::from),
                "Task" | "Agent" => field("description").map(String::from),
                _ => None,
            };
            tool_line(name, subject.as_deref())
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn assistant(content: Value) -> Value {
        json!({"type": "assistant", "message": {"role": "assistant", "content": content}})
    }

    #[test]
    fn the_last_block_of_an_assistant_entry_is_its_line() {
        assert_eq!(
            line(&assistant(
                json!([{"type": "thinking", "thinking": "Let me check the parser.\nThen…"}])
            )),
            Some("Let me check the parser.".into())
        );
        assert_eq!(
            line(&assistant(json!([
                {"type": "text", "text": "Got it, I'll fix the test."},
                {"type": "tool_use", "name": "Bash", "input": {"command": "cargo test", "description": "Run the tests"}}
            ]))),
            Some("Bash: Run the tests".into())
        );
        assert_eq!(
            line(&assistant(
                json!([{"type": "tool_use", "name": "Edit", "input": {"file_path": "/repo/tui/src/app/account.rs"}}])
            )),
            Some("Edit: app/account.rs".into())
        );
        assert_eq!(
            line(&assistant(
                json!([{"type": "tool_use", "name": "TodoWrite", "input": {}}])
            )),
            Some("TodoWrite".into())
        );
    }

    #[test]
    fn prompts_tool_results_and_subagents_are_not_lines() {
        assert_eq!(
            line(
                &json!({"type": "user", "message": {"content": [{"type": "text", "text": "my prompt"}]}})
            ),
            None
        );
        let mut sidechain = assistant(json!([{"type": "text", "text": "a subagent"}]));
        sidechain["isSidechain"] = json!(true);
        assert_eq!(line(&sidechain), None);
        assert_eq!(line(&json!({"type": "attachment"})), None);
    }
}
