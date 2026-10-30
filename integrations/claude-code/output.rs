//! Claude Code's latest output, from its conversation transcript.
use super::{contains, first_line, prompt_line, short_path, tool_line, Step};

/// Thinking and replies longer than this are cut, in bytes, before they're
/// shaped into a line or a summary's message.
const MAX_BLOCK: usize = 8 * 1024;
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
    messages(entry)
        .iter()
        .rev()
        .find_map(|message| first_line(message))
}

/// The messages of an assistant entry of the main conversation, oldest
/// first: its thinking and replies as written, its tool calls as lines.
pub(super) fn messages(entry: &Value) -> Vec<String> {
    let main = entry.get("type").and_then(Value::as_str) == Some("assistant")
        && entry.get("isSidechain").and_then(Value::as_bool) != Some(true);
    if !main {
        return Vec::new();
    }
    entry
        .get("message")
        .and_then(|message| message.get("content"))
        .and_then(Value::as_array)
        .map(|blocks| blocks.iter().filter_map(block).collect())
        .unwrap_or_default()
}

/// An entry of what the user typed in the main conversation, as one line.
pub(super) fn prompt(entry: &Value) -> Option<String> {
    prompt_line(&typed(entry)?)
}

/// What the user typed in an entry of the main conversation, as written:
/// not a tool result, nor what Claude Code adds itself (summaries, notices,
/// commands' output). Typed while a turn ran, it's a [`queued`] prompt.
pub(super) fn typed(entry: &Value) -> Option<String> {
    let flagged = |key: &str| entry.get(key).and_then(Value::as_bool) == Some(true);
    if entry.get("type")?.as_str()? == "attachment" {
        return queued(entry);
    }
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
    Some(text)
}

/// Whether a log line may be one [`typed`] takes, before it's parsed: a
/// `user` entry that holds no tool result, or a queued prompt.
pub(super) fn may_be_typed(line: &[u8]) -> bool {
    (contains(line, br#""type":"user""#) && !contains(line, br#""type":"tool_result""#))
        || contains(line, br#""queued_command""#)
}

/// What the user typed while a turn ran: Claude Code queues it and logs it,
/// once it takes it into the turn, as a `queued_command` attachment, not a
/// `user` entry. Not what else it queues (task notifications, other agents'
/// messages).
pub(super) fn queued(entry: &Value) -> Option<String> {
    let attachment = entry.get("attachment")?;
    let human = match attachment.get("origin") {
        Some(origin) => origin.get("kind").and_then(Value::as_str) == Some("human"),
        None => true,
    };
    let queued = attachment.get("type")?.as_str()? == "queued_command"
        && attachment.get("commandMode")?.as_str()? == "prompt"
        && human
        && entry.get("isSidechain").and_then(Value::as_bool) != Some(true);
    if !queued {
        return None;
    }
    match attachment.get("prompt")? {
        Value::String(text) => Some(text.clone()),
        Value::Array(blocks) => {
            let texts: Vec<&str> = blocks
                .iter()
                .filter(|block| block.get("type").and_then(Value::as_str) == Some("text"))
                .filter_map(|block| block.get("text").and_then(Value::as_str))
                .collect();
            (!texts.is_empty()).then(|| texts.join("\n"))
        }
        _ => None,
    }
}

/// A block's message: thinking or a reply as written (cut at
/// [`MAX_BLOCK`]), a tool call as a line.
fn block(block: &Value) -> Option<String> {
    let text = |key: &str| {
        let text = block.get(key)?.as_str()?.trim();
        let end = (0..=text.len().min(MAX_BLOCK))
            .rev()
            .find(|at| text.is_char_boundary(*at))?;
        (!text.is_empty()).then(|| text[..end].to_string())
    };
    match block.get("type")?.as_str()? {
        "thinking" => text("thinking"),
        "text" => text("text"),
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

/// How an entry changes the turn's reply: a prompt (or anything else the user
/// side adds) or a tool call starts it over, and text of the main
/// conversation's assistant adds to it. One entry holds one content block.
pub(super) fn reply_step(entry: &Value) -> Step {
    if entry.get("isSidechain").and_then(Value::as_bool) == Some(true) {
        return Step::Keep;
    }
    match entry.get("type").and_then(Value::as_str) {
        Some("user") => Step::Reset,
        Some("attachment") if queued(entry).is_some() => Step::Reset,
        Some("assistant") => {
            let Some(blocks) = entry
                .get("message")
                .and_then(|message| message.get("content"))
                .and_then(Value::as_array)
            else {
                return Step::Keep;
            };
            if blocks
                .iter()
                .any(|block| block.get("type").and_then(Value::as_str) == Some("tool_use"))
            {
                return Step::Reset;
            }
            let text: Vec<&str> = blocks
                .iter()
                .filter(|block| block.get("type").and_then(Value::as_str) == Some("text"))
                .filter_map(|block| block.get("text").and_then(Value::as_str))
                .map(str::trim)
                .filter(|text| !text.is_empty())
                .collect();
            match entry.get("uuid").and_then(Value::as_str) {
                Some(key) if !text.is_empty() => Step::Text {
                    key: key.to_owned(),
                    text: text.join("\n\n"),
                },
                _ => Step::Keep,
            }
        }
        _ => Step::Keep,
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
    fn every_line_typed_takes_passes_the_cheap_check_first() {
        let user = |content: Value| json!({"type": "user", "message": {"role": "user", "content": content}});
        let queued = json!({"type": "attachment", "attachment": {"type": "queued_command",
            "prompt": "Also the web app", "commandMode": "prompt", "origin": {"kind": "human"}}});
        let entries = [
            user(json!("Fix the login test")),
            user(json!([{"type": "text", "text": "Mention \"type\":\"tool_result\" here"}])),
            user(json!([{"type": "image"}, {"type": "text", "text": "This screenshot"}])),
            queued,
            user(json!([{"type": "tool_result", "content": "output"}])),
            json!({"type": "assistant", "message": {"content": [{"type": "text", "text": "hi"}]}}),
        ];
        for entry in &entries {
            let line = serde_json::to_vec(entry).unwrap();
            if super::typed(entry).is_some() {
                assert!(may_be_typed(&line), "{entry}");
            }
        }
        let tool = serde_json::to_vec(&entries[4]).unwrap();
        assert!(!may_be_typed(&tool));
        let reply = serde_json::to_vec(&entries[5]).unwrap();
        assert!(!may_be_typed(&reply));
    }

    #[test]
    fn what_the_user_typed_while_a_turn_ran_is_a_prompt_but_other_queued_input_is_not() {
        let queued = |mode: &str, origin: Value, prompt_value: Value| {
            json!({"type": "attachment", "uuid": "5110f9e2-c20b-4dbd-9606-b04ec4dc25b2",
                "timestamp": "2026-10-28T11:10:14.035Z",
                "attachment": {"type": "queued_command", "prompt": prompt_value,
                    "commandMode": mode, "origin": origin, "humanTurn": true}})
        };
        let human = queued(
            "prompt",
            json!({"kind": "human"}),
            json!("Also check the web app"),
        );
        assert_eq!(prompt(&human), Some("Also check the web app".into()));
        assert_eq!(reply_step(&human), Step::Reset);
        // An older Claude Code: no origin, the prompt in blocks.
        let mut older = queued(
            "prompt",
            json!(null),
            json!([{"type": "text", "text": "And this"}]),
        );
        older["attachment"]
            .as_object_mut()
            .unwrap()
            .remove("origin");
        assert_eq!(prompt(&older), Some("And this".into()));
        // Queued, but not typed: a task's notification, another agent's message.
        let notice = queued(
            "task-notification",
            json!({"kind": "task-notification"}),
            json!("<task-notification>done</task-notification>"),
        );
        assert_eq!(super::typed(&notice), None);
        assert_eq!(reply_step(&notice), Step::Keep);
        let peer = queued(
            "prompt",
            json!({"kind": "peer"}),
            json!("From the other agent"),
        );
        assert_eq!(super::typed(&peer), None);
        // Other attachments.
        assert_eq!(
            super::typed(&json!({"type": "attachment", "attachment": {"type": "todo_reminder"}})),
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

    fn entry(uuid: &str, content: Value) -> Value {
        let mut entry = assistant(content);
        entry["uuid"] = json!(uuid);
        entry
    }

    #[test]
    fn the_reply_is_the_text_after_the_last_tool_call() {
        let mut reply = None;
        for step in [
            reply_step(&json!({"type": "user", "message": {"content": "fix the test"}})),
            reply_step(&entry(
                "a",
                json!([{"type": "text", "text": "Let me look."}]),
            )),
            reply_step(&entry(
                "b",
                json!([{"type": "tool_use", "name": "Bash", "input": {}}]),
            )),
            reply_step(&json!({"type": "user", "message": {"content": [{"type": "tool_result"}]}})),
            reply_step(&entry(
                "c",
                json!([{"type": "thinking", "thinking": "Done."}]),
            )),
            reply_step(&entry(
                "d",
                json!([{"type": "text", "text": "Fixed it.\n\n- the parser"}]),
            )),
            reply_step(&entry(
                "e",
                json!([{"type": "text", "text": " Tests pass. "}]),
            )),
            reply_step(&json!({"type": "attachment"})),
        ] {
            reply = super::super::next_reply(reply, step, 0);
        }
        let reply = reply.unwrap();
        assert_eq!(reply.key, "e");
        assert_eq!(reply.text, "Fixed it.\n\n- the parser\n\nTests pass.");
        assert!(!reply.truncated);
    }

    #[test]
    fn a_subagent_or_a_new_prompt_is_not_the_reply() {
        let mut sidechain = entry("s", json!([{"type": "text", "text": "a subagent"}]));
        sidechain["isSidechain"] = json!(true);
        assert_eq!(reply_step(&sidechain), Step::Keep);
        let reply = super::super::next_reply(
            None,
            reply_step(&entry("a", json!([{"type": "text", "text": "Answer"}]))),
            0,
        );
        assert!(reply.is_some());
        let reply = super::super::next_reply(
            reply,
            reply_step(&json!({"type": "user", "message": {"content": "next"}})),
            0,
        );
        assert_eq!(reply, None);
    }
}
