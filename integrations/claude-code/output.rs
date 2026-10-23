//! Claude Code's latest output, from its conversation transcript.
use super::{first_line, prompt_line, short_path, tool_line};
use serde::Deserialize;
use serde_json::Value;
use std::path::{Path, PathBuf};

/// The most project directories searched for a transcript.
const MAX_PROJECTS: usize = 4096;

/// `<config dir>/sessions/<pid>.json`: the conversation a process shows,
/// and the background job it parked it in, if any.
#[derive(Deserialize)]
struct SessionFile {
    #[serde(rename = "sessionId")]
    session_id: String,
    #[serde(default)]
    pid: Option<u32>,
    #[serde(default, rename = "parkedJobId")]
    parked_job_id: Option<String>,
}

/// `<config dir>/jobs/<job>/state.json`: the conversation a background job runs.
#[derive(Deserialize)]
struct JobState {
    #[serde(rename = "sessionId")]
    session_id: String,
}

/// The conversation process `pid` is working in, under `config`: the one
/// a background job runs when the process parked its conversation there
/// (the work goes on in the job's transcript), else its own.
pub(super) fn conversation(config: &Path, pid: u32) -> Option<String> {
    let path = config.join("sessions").join(format!("{pid}.json"));
    let file: SessionFile = serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;
    if file.pid.is_some_and(|recorded| recorded != pid) {
        return None;
    }
    let parked = file
        .parked_job_id
        .filter(|job| crate::resume::is_conversation_id(job))
        .and_then(|job| {
            let path = config.join("jobs").join(job).join("state.json");
            let state: JobState = serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;
            Some(state.session_id)
        });
    parked
        .into_iter()
        .chain([file.session_id])
        .find(|id| crate::resume::is_conversation_id(id))
}

/// The transcript of conversation `id` under `config`:
/// `projects/<project>/<id>.jsonl`, the most recently written one when a
/// conversation was copied to another project.
pub(super) fn transcript(config: &Path, id: &str) -> Option<PathBuf> {
    let name = format!("{id}.jsonl");
    std::fs::read_dir(config.join("projects"))
        .ok()?
        .filter_map(Result::ok)
        .take(MAX_PROJECTS)
        .map(|entry| entry.path().join(&name))
        .filter_map(|path| {
            let modified = std::fs::metadata(&path).ok()?.modified().ok()?;
            Some((modified, path))
        })
        .max_by_key(|(modified, _)| *modified)
        .map(|(_, path)| path)
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

/// An entry of what the user typed in the main conversation: not a tool
/// result, nor what Claude Code adds itself (summaries, notices, commands'
/// output).
pub(super) fn prompt(entry: &Value) -> Option<String> {
    let flagged = |key: &str| entry.get(key).and_then(Value::as_bool) == Some(true);
    if entry.get("type")?.as_str()? != "user"
        || flagged("isSidechain")
        || flagged("isMeta")
        || flagged("isCompactSummary")
    {
        return None;
    }
    let text = match entry.get("message")?.get("content")? {
        Value::String(text) => text.clone(),
        Value::Array(blocks) => {
            let mut texts = Vec::new();
            for block in blocks {
                match block.get("type")?.as_str()? {
                    "text" => texts.push(block.get("text")?.as_str()?),
                    "tool_result" => return None,
                    _ => {}
                }
            }
            texts.join("\n")
        }
        _ => return None,
    };
    if text.starts_with("[Request interrupted") {
        return None;
    }
    prompt_line(&text)
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
    fn a_parked_conversation_is_followed_in_its_background_job() {
        let config = tempfile::tempdir().unwrap();
        let write = |path: &str, value: Value| {
            let path = config.path().join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, value.to_string()).unwrap();
        };
        write(
            "sessions/42.json",
            json!({"pid": 42, "sessionId": "own-conversation"}),
        );
        assert_eq!(
            conversation(config.path(), 42),
            Some("own-conversation".into())
        );
        // Parked in a job: the job's conversation.
        write(
            "sessions/42.json",
            json!({"pid": 42, "sessionId": "own-conversation", "parkedJobId": "job1"}),
        );
        write(
            "jobs/job1/state.json",
            json!({"sessionId": "job-conversation"}),
        );
        assert_eq!(
            conversation(config.path(), 42),
            Some("job-conversation".into())
        );
        // Another process's file, or no file: nothing.
        assert_eq!(conversation(config.path(), 7), None);
        write("sessions/7.json", json!({"pid": 8, "sessionId": "x"}));
        assert_eq!(conversation(config.path(), 7), None);
        // The transcript is the newest copy.
        write("projects/a/job-conversation.jsonl", json!({}));
        std::thread::sleep(std::time::Duration::from_millis(20));
        write("projects/b/job-conversation.jsonl", json!({}));
        assert_eq!(
            transcript(config.path(), "job-conversation"),
            Some(config.path().join("projects/b/job-conversation.jsonl"))
        );
    }

    #[test]
    fn typed_prompts_are_prompts_but_claude_codes_own_entries_are_not() {
        let user = |content: Value| json!({"type": "user", "message": {"role": "user", "content": content}});
        assert_eq!(
            prompt(&user(json!("Fix the login test"))),
            Some("Fix the login test".into())
        );
        assert_eq!(
            prompt(&user(json!([{"type": "text", "text": "And the docs"}]))),
            Some("And the docs".into())
        );
        assert_eq!(
            prompt(&user(
                json!([{"type": "tool_result", "content": "secret output"}])
            )),
            None
        );
        let mut summary = user(json!("This session is being continued"));
        summary["isCompactSummary"] = json!(true);
        assert_eq!(prompt(&summary), None);
        assert_eq!(prompt(&user(json!("[Request interrupted by user]"))), None);
        assert_eq!(
            prompt(&user(json!(
                "<task-notification>finished</task-notification>"
            ))),
            None
        );
        assert_eq!(
            prompt(
                &json!({"type": "assistant", "message": {"content": [{"type": "text", "text": "hi"}]}})
            ),
            None
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
