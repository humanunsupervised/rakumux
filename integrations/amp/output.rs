//! Amp's latest output, from the copy of its thread the Rakumux plugin keeps.
//!
//! Amp keeps no transcript on this machine (a thread lives on ampcode.com),
//! so the plugin writes one, under Amp's cache for plugins:
//! `<Amp cache>/plugin-cache/rakumux-session/threads/<thread>.jsonl`, owner
//! only. One line per message, in Amp's plugin schema (`ThreadMessage`:
//! `role` user, assistant or info; blocks `text`, `thinking`, `tool_use`,
//! `tool_result`), plus where the turn ended:
//!
//! ```json
//! {"v":1,"kind":"message","id":"…","role":"user","timestamp":"…","cwd":"/repo","content":[{"type":"text","text":"…"}]}
//! {"v":1,"kind":"turn_end","id":"…","status":"done","timestamp":"…"}
//! ```
//!
//! A user message holds what was typed, or tools' results; an assistant
//! message its thinking, text and tool calls (each with the files Amp says
//! it changes, `files`). The thread followed is the one the plugin bound.
use super::{contains, first_line, prompt_line, short_path, tool_line, Step};
use serde_json::Value;
use std::path::{Path, PathBuf};

/// The plugin's name: its file under Amp's plugins, and its cache directory.
pub(super) const PLUGIN: &str = "rakumux-session";

/// Where the plugin keeps its copies of threads.
pub(super) fn threads_dir() -> Option<PathBuf> {
    Some(
        crate::resume::amp_cache_dir()?
            .join("plugin-cache")
            .join(PLUGIN)
            .join("threads"),
    )
}

/// Follow the thread the session bound (Amp shows): its copy is read from
/// its start. Returns whether it switched. `path` is the threads directory
/// until there's a thread, then its copy.
pub(super) fn switch(
    bound: Option<&str>,
    conversation: &mut Option<String>,
    path: &mut PathBuf,
    offset: &mut u64,
    partial: &mut Vec<u8>,
) -> bool {
    let Some(bound) = bound.filter(|id| crate::resume::is_conversation_id(id)) else {
        return false;
    };
    if conversation.as_deref() == Some(bound) {
        return false;
    }
    let dir = match conversation {
        Some(_) => path.parent().map(Path::to_path_buf),
        None => Some(path.clone()),
    };
    let Some(dir) = dir else {
        return false;
    };
    *path = dir.join(format!("{bound}.jsonl"));
    *offset = 0;
    partial.clear();
    *conversation = Some(bound.to_owned());
    true
}

/// A message line's role, when the line is a message.
fn role(entry: &Value) -> Option<&str> {
    if entry.get("kind")?.as_str()? != "message" {
        return None;
    }
    entry.get("role")?.as_str()
}

fn blocks(entry: &Value) -> &[Value] {
    entry
        .get("content")
        .and_then(Value::as_array)
        .map_or(&[], Vec::as_slice)
}

/// The id of a message or turn end, as a string (numeric in legacy threads).
pub(super) fn id(entry: &Value) -> Option<String> {
    match entry.get("id")? {
        Value::String(id) => Some(id.clone()),
        Value::Number(id) => Some(id.to_string()),
        _ => None,
    }
}

/// An assistant message's last thinking, tool call or text, as one line.
pub(super) fn line(entry: &Value) -> Option<String> {
    messages(entry)
        .iter()
        .rev()
        .find_map(|message| first_line(message))
}

/// An assistant message's thinking and text as written, and its tool calls
/// as lines, oldest first.
pub(super) fn messages(entry: &Value) -> Vec<String> {
    if role(entry) != Some("assistant") {
        return Vec::new();
    }
    blocks(entry).iter().filter_map(block).collect()
}

fn block(block: &Value) -> Option<String> {
    let text = |key: &str| {
        let text = block.get(key)?.as_str()?.trim();
        (!text.is_empty()).then(|| text.to_owned())
    };
    match block.get("type")?.as_str()? {
        "thinking" => text("thinking"),
        "text" => text("text"),
        "tool_use" => {
            let name = block.get("name")?.as_str()?;
            tool_line(name, subject(name, block.get("input")?).as_deref())
        }
        _ => None,
    }
}

/// What a tool call works on, for its line.
fn subject(name: &str, input: &Value) -> Option<String> {
    let field = |key: &str| input.get(key)?.as_str();
    match name {
        "Bash" => field("cmd").map(String::from),
        "shell_command" => field("command").map(String::from),
        "Read" | "read_file" | "edit_file" | "create_file" | "undo_edit" | "format_file" => {
            field("path").map(short_path)
        }
        "apply_patch" => field("patchText").and_then(patched_file).map(short_path),
        "Grep" => field("pattern").map(String::from),
        "glob" => field("filePattern").or_else(|| field("pattern")).map(String::from),
        "Task" => field("description").map(String::from),
        "web_search" | "finder" | "librarian" => field("query").map(String::from),
        "read_web_page" => field("url").map(String::from),
        "oracle" => field("task").map(String::from),
        _ => None,
    }
}

/// The first file a patch names.
fn patched_file(patch: &str) -> Option<&str> {
    patch.lines().find_map(|line| {
        ["*** Update File:", "*** Add File:", "*** Delete File:"]
            .iter()
            .find_map(|header| line.strip_prefix(header))
            .map(str::trim)
    })
}

/// What the user typed, as one line.
pub(super) fn prompt(entry: &Value) -> Option<String> {
    prompt_line(&typed(entry)?)
}

/// What the user typed, as written: a user message's text, not tools'
/// results.
pub(super) fn typed(entry: &Value) -> Option<String> {
    if role(entry) != Some("user") {
        return None;
    }
    let mut texts = Vec::new();
    for block in blocks(entry) {
        match block.get("type")?.as_str()? {
            "text" => texts.push(block.get("text")?.as_str()?),
            "tool_result" => return None,
            _ => {}
        }
    }
    (!texts.is_empty()).then(|| texts.join("\n"))
}

/// Whether a line may be one [`typed`] takes, before it's parsed.
pub(super) fn may_be_typed(line: &[u8]) -> bool {
    contains(line, br#""role":"user""#) && !contains(line, br#""type":"tool_result""#)
}

/// How a line changes the turn's reply: a user message (a prompt, tools'
/// results) or a tool call starts it over, and an assistant message's text
/// adds to it, keyed by the message's id. Where the turn ended changes
/// nothing: the plugin reports a finished turn, and only that is captured.
pub(super) fn reply_step(entry: &Value) -> Step {
    match role(entry) {
        Some("user") => Step::Reset,
        Some("assistant") => {
            let blocks = blocks(entry);
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
            match id(entry) {
                Some(key) if !text.is_empty() => Step::Text {
                    key,
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
    use super::super::{Follower, LogAgent, Source, Turn};
    use super::*;
    use std::io::Write;

    /// The plugin's copy of a synthetic thread (`integrations/amp/fixtures`).
    pub(crate) const FIXTURE: &str = include_str!("fixtures/thread.jsonl");
    pub(crate) const THREAD: &str = "T-5f0c1d2e-3a4b-4c5d-8e9f-0a1b2c3d4e5f";

    fn entries() -> Vec<Value> {
        FIXTURE
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    /// A follower of the threads in `dir`, as `Follower::locate` makes one.
    fn follower(dir: &Path) -> Follower {
        Follower {
            pid: 1,
            source: Source::Log {
                agent: LogAgent::Amp,
                conversation: None,
                path: dir.to_path_buf(),
                offset: 0,
                partial: Vec::new(),
            },
            latest: None,
            prompt: None,
            turn: Turn::default(),
            reply: None,
            last_at: None,
            foreground: None,
        }
    }

    #[test]
    fn lines_are_the_agents_messages_and_prompts_what_was_typed() {
        let entries = entries();
        let lines: Vec<_> = entries.iter().filter_map(line).collect();
        assert_eq!(
            lines,
            [
                "edit_file: src/parser.rs",
                "apply_patch: src/lexer.rs",
                "The parser tests pass now: the tokenizer kept a trailing newline.",
                "Grep: TODO",
            ]
        );
        assert_eq!(
            messages(&entries[3]),
            [
                "Bash: cd crates/parser && sed -i 's/old/new/' lib.rs",
                "apply_patch: src/lexer.rs"
            ]
        );
        let prompts: Vec<_> = entries.iter().filter_map(prompt).collect();
        assert_eq!(prompts, ["Fix the failing parser tests", "Now the TODOs"]);
        // Tools' results are neither.
        assert!(entries
            .iter()
            .filter(|entry| entry.to_string().contains("tool_result"))
            .all(|entry| line(entry).is_none() && prompt(entry).is_none()));
    }

    #[test]
    fn the_follower_reads_the_bound_threads_turn_and_reply() {
        let directory = tempfile::tempdir().unwrap();
        let mut follower = follower(directory.path());
        // Nothing bound yet: nothing to read.
        assert!(!follower.follow(None));
        assert_eq!(follower.conversation(), None);
        // Bound before the plugin wrote anything.
        assert!(!follower.follow(Some(THREAD)));
        assert_eq!(follower.conversation().as_deref(), Some(THREAD));
        let path = directory.path().join(format!("{THREAD}.jsonl"));
        let mut file = std::fs::File::create(&path).unwrap();
        let lines: Vec<&str> = FIXTURE.lines().collect();
        // The first turn, up to its end.
        for line in &lines[..7] {
            writeln!(file, "{line}").unwrap();
        }
        assert!(follower.follow(Some(THREAD)));
        assert_eq!(follower.agent(), "amp");
        assert_eq!(follower.prompt(), Some("Fix the failing parser tests"));
        assert_eq!(
            follower.latest(),
            Some("The parser tests pass now: the tokenizer kept a trailing newline.")
        );
        let turn = follower.turn().clone();
        assert_eq!(turn.prompt.as_deref(), Some("Fix the failing parser tests"));
        assert_eq!(turn.total, 5);
        assert_eq!(turn.messages[0], "The test fails on a trailing newline.");
        let reply = follower.reply().unwrap();
        assert_eq!(reply.key, "msg-6");
        assert_eq!(
            reply.text,
            "The parser tests pass now: the tokenizer kept a trailing newline.\n\nRun `cargo test -p parser` to see."
        );
        let offset = |n: usize| -> u64 { lines[..n].iter().map(|l| l.len() as u64 + 1).sum() };
        assert_eq!(reply.position, Some(offset(5)));
        assert_eq!(follower.last_at(), Some(1_792_749_660_000));
        // The next prompt starts a new turn, and a tool call has no reply.
        for line in &lines[7..] {
            writeln!(file, "{line}").unwrap();
        }
        assert!(follower.follow(Some(THREAD)));
        assert_ne!(follower.turn().id, turn.id);
        assert_eq!(follower.turn().prompt.as_deref(), Some("Now the TODOs"));
        assert_eq!(follower.prompt(), Some("Now the TODOs"));
        assert_eq!(follower.reply(), None);
        // Nothing new: nothing changes.
        assert!(!follower.follow(Some(THREAD)));
    }

    #[test]
    fn another_bound_thread_is_read_from_its_start() {
        let directory = tempfile::tempdir().unwrap();
        let other = "T-11111111-2222-3333-4444-555555555555";
        std::fs::write(
            directory.path().join(format!("{THREAD}.jsonl")),
            format!("{FIXTURE}"),
        )
        .unwrap();
        std::fs::write(
            directory.path().join(format!("{other}.jsonl")),
            "{\"v\":1,\"kind\":\"message\",\"id\":1,\"role\":\"user\",\"content\":[{\"type\":\"text\",\"text\":\"Another thread\"}]}\n",
        )
        .unwrap();
        let mut follower = follower(directory.path());
        assert!(follower.follow(Some(THREAD)));
        let first = follower.turn().id;
        assert!(follower.follow(Some(other)));
        assert_eq!(follower.conversation().as_deref(), Some(other));
        assert_eq!(follower.prompt(), Some("Another thread"));
        assert_ne!(follower.turn().id, first);
        assert_eq!(follower.turn().total, 0);
        // Its transcript is that thread's copy.
        match follower.transcript_source().unwrap() {
            super::super::transcript::TranscriptSource::Log {
                agent,
                conversation,
                path,
            } => {
                assert_eq!(agent, LogAgent::Amp);
                assert_eq!(conversation, other);
                assert_eq!(path, directory.path().join(format!("{other}.jsonl")));
            }
            source => panic!("{source:?}"),
        }
        // A binding that isn't a thread id is never a path.
        assert!(!follower.follow(Some("../../etc/passwd")));
        assert_eq!(follower.conversation().as_deref(), Some(other));
    }

    #[test]
    fn replies_reset_at_prompts_and_tool_calls() {
        let message = |role: &str, content: Value| {
            serde_json::json!({"v": 1, "kind": "message", "id": 9, "role": role, "content": content})
        };
        assert_eq!(
            reply_step(&message(
                "assistant",
                serde_json::json!([{"type": "thinking", "thinking": "hm"}, {"type": "text", "text": " Done. "}])
            )),
            Step::Text {
                key: "9".into(),
                text: "Done.".into()
            }
        );
        assert_eq!(
            reply_step(&message(
                "assistant",
                serde_json::json!([{"type": "text", "text": "Let me look."},
                    {"type": "tool_use", "id": "t", "name": "Read", "input": {"path": "/a"}}])
            )),
            Step::Reset
        );
        assert_eq!(
            reply_step(&message("user", serde_json::json!([{"type": "text", "text": "go"}]))),
            Step::Reset
        );
        assert_eq!(
            reply_step(&message("info", serde_json::json!([{"type": "text", "text": "Compacted"}]))),
            Step::Keep
        );
        assert_eq!(
            reply_step(&serde_json::json!({"v": 1, "kind": "turn_end", "id": 8, "status": "done"})),
            Step::Keep
        );
    }
}
