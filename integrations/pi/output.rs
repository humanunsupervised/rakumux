//! Pi's latest output, from its conversation file
//! (`<agent dir>/sessions/--<folder>--/<time>_<id>.jsonl`): one JSON entry per
//! line, a header first, then `message` entries whose `message.role` says
//! who: `user` (what was typed), `assistant` (thinking, text and tool calls,
//! one model response per entry), `toolResult`, and Pi's own (`system`,
//! `bashExecution` for a `!` command, `custom`, summaries), which aren't the
//! user's prompt nor the agent's messages.
use super::{contains, first_line, prompt_line, short_path, tool_line, Step};
use serde_json::Value;
use std::path::PathBuf;

/// Thinking and replies longer than this are cut, in bytes, before they're
/// shaped into a line or a summary's message.
const MAX_BLOCK: usize = 8 * 1024;

/// Follow the conversation the Pi process `pid` shows to its file: the one
/// bound to the session, else its folder's newest. A new one is read from
/// its start. Returns whether it switched.
pub(super) fn switch(
    pid: u32,
    bound: Option<&str>,
    conversation: &mut Option<String>,
    path: &mut PathBuf,
    offset: &mut u64,
    partial: &mut Vec<u8>,
) -> bool {
    // The bound one's file is found once.
    if bound.is_some() && conversation.as_deref() == bound && path.is_file() {
        return false;
    }
    let cwd = crate::procinfo::cwd_of(pid);
    let Some((id, file)) = crate::resume::pi_conversation(cwd.as_deref(), bound) else {
        return false;
    };
    if conversation.as_deref() == Some(id.as_str()) && *path == file {
        return false;
    }
    *path = file;
    *offset = 0;
    partial.clear();
    *conversation = Some(id);
    true
}

/// A `message` entry's message of `role`.
fn message<'a>(entry: &'a Value, role: &str) -> Option<&'a Value> {
    let message = entry.get("message")?;
    let ours = entry.get("type")?.as_str()? == "message" && message.get("role")?.as_str()? == role;
    ours.then_some(message)
}

/// An entry of the assistant: its last thinking, tool call or text.
pub(super) fn line(entry: &Value) -> Option<String> {
    messages(entry)
        .iter()
        .rev()
        .find_map(|message| first_line(message))
}

/// The messages of an assistant entry, oldest first: its thinking and
/// replies as written, its tool calls as lines.
pub(super) fn messages(entry: &Value) -> Vec<String> {
    message(entry, "assistant")
        .and_then(|message| message.get("content"))
        .and_then(Value::as_array)
        .map(|blocks| blocks.iter().filter_map(block).collect())
        .unwrap_or_default()
}

/// An entry of what the user typed, as one line.
pub(super) fn prompt(entry: &Value) -> Option<String> {
    prompt_line(&typed(entry)?)
}

/// What the user typed in an entry: a `user` message's text (a skill as
/// `/skill:name args`). A `!` command isn't a prompt to the agent.
pub(super) fn typed(entry: &Value) -> Option<String> {
    let text = match message(entry, "user")?.get("content")? {
        Value::String(text) => text.clone(),
        Value::Array(blocks) => blocks
            .iter()
            .filter(|block| block.get("type").and_then(Value::as_str) == Some("text"))
            .filter_map(|block| block.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => return None,
    };
    Some(skill(&text).unwrap_or(text))
}

/// A skill Pi expanded into the prompt, `<skill name="x" location="…">…
/// </skill>` then the arguments, as the user typed it: `/skill:x args`.
pub(super) fn skill(text: &str) -> Option<String> {
    let rest = text.strip_prefix("<skill name=\"")?;
    let (name, rest) = rest.split_once('"')?;
    let close = "\n</skill>";
    let end = rest.find(close)? + close.len();
    let args = rest[end..].trim();
    Some(if args.is_empty() {
        format!("/skill:{name}")
    } else {
        format!("/skill:{name} {args}")
    })
}

/// Whether a log line may be one [`typed`] takes, before it's parsed.
pub(super) fn may_be_typed(line: &[u8]) -> bool {
    contains(line, br#""role":"user""#)
}

/// A block's message: thinking or a reply as written (cut at [`MAX_BLOCK`]),
/// a tool call as a line. Redacted thinking says nothing.
fn block(block: &Value) -> Option<String> {
    let text = |key: &str| {
        let text = block.get(key)?.as_str()?.trim();
        let end = (0..=text.len().min(MAX_BLOCK))
            .rev()
            .find(|at| text.is_char_boundary(*at))?;
        (!text.is_empty()).then(|| text[..end].to_string())
    };
    match block.get("type")?.as_str()? {
        "thinking" if block.get("redacted").and_then(Value::as_bool) == Some(true) => None,
        "thinking" => text("thinking"),
        "text" => text("text"),
        "toolCall" => {
            let name = block.get("name")?.as_str()?;
            let field = |key: &str| block.get("arguments")?.get(key)?.as_str();
            let subject = match name {
                "bash" | "powershell" => field("command").map(String::from),
                // Pi takes `@path` as `path`.
                "read" | "write" | "edit" => {
                    field("path").map(|path| short_path(path.strip_prefix('@').unwrap_or(path)))
                }
                "grep" | "find" => field("pattern").map(String::from),
                "ls" => field("path").map(String::from),
                _ => None,
            };
            tool_line(name, subject.as_deref())
        }
        _ => None,
    }
}

/// How an entry changes the turn's reply: a prompt or a tool call starts it
/// over, and a model response that finished (`stop`, or cut at `length`)
/// with text is the whole reply, keyed by its entry's `id`. One that failed
/// or was aborted is none.
pub(super) fn reply_step(entry: &Value) -> Step {
    if message(entry, "user").is_some() {
        return Step::Reset;
    }
    let Some(message) = message(entry, "assistant") else {
        return Step::Keep;
    };
    let Some(blocks) = message.get("content").and_then(Value::as_array) else {
        return Step::Keep;
    };
    let kind = |block: &Value| block.get("type").and_then(Value::as_str).map(str::to_owned);
    if blocks
        .iter()
        .any(|b| kind(b).as_deref() == Some("toolCall"))
    {
        return Step::Reset;
    }
    let finished = matches!(
        message.get("stopReason").and_then(Value::as_str),
        Some("stop" | "length")
    );
    if !finished {
        return Step::Reset;
    }
    let text: Vec<&str> = blocks
        .iter()
        .filter(|b| kind(b).as_deref() == Some("text"))
        .filter_map(|block| block.get("text").and_then(Value::as_str))
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .collect();
    match entry.get("id").and_then(Value::as_str) {
        Some(key) if !text.is_empty() => Step::Complete {
            key: key.to_owned(),
            text: text.join("\n\n"),
        },
        _ => Step::Keep,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn entry(id: &str, message: Value) -> Value {
        json!({"type": "message", "id": id, "parentId": null,
            "timestamp": "2026-10-30T02:13:49.215Z", "message": message})
    }

    fn assistant(id: &str, content: Value, stop: &str) -> Value {
        entry(
            id,
            json!({"role": "assistant", "content": content, "api": "openai-completions",
                "provider": "p", "model": "m", "stopReason": stop, "timestamp": 1}),
        )
    }

    fn user(content: Value) -> Value {
        entry(
            "u1",
            json!({"role": "user", "content": content, "timestamp": 1}),
        )
    }

    #[test]
    fn the_last_block_of_an_assistant_entry_is_its_line() {
        assert_eq!(
            line(&assistant(
                "a1",
                json!([{"type": "thinking", "thinking": "Let me check the parser.\nThen…"}]),
                "stop"
            )),
            Some("Let me check the parser.".into())
        );
        assert_eq!(
            line(&assistant(
                "a1",
                json!([
                    {"type": "text", "text": "Running them."},
                    {"type": "toolCall", "id": "call_1", "name": "bash",
                        "arguments": {"command": "cargo test", "timeout": 60}}
                ]),
                "toolUse"
            )),
            Some("bash: cargo test".into())
        );
        assert_eq!(
            line(&assistant(
                "a1",
                json!([{"type": "toolCall", "id": "c", "name": "edit",
                    "arguments": {"path": "/repo/tui/src/app/account.rs", "edits": []}}]),
                "toolUse"
            )),
            Some("edit: app/account.rs".into())
        );
        assert_eq!(
            line(&assistant(
                "a1",
                json!([{"type": "toolCall", "id": "c", "name": "read",
                    "arguments": {"path": "@src/lib.rs"}}]),
                "toolUse"
            )),
            Some("read: src/lib.rs".into())
        );
        assert_eq!(
            line(&assistant(
                "a1",
                json!([{"type": "toolCall", "id": "c", "name": "mcp__rakumux__list", "arguments": {}}]),
                "toolUse"
            )),
            Some("mcp__rakumux__list".into())
        );
        // Redacted thinking says nothing; the text before it does.
        assert_eq!(
            line(&assistant(
                "a1",
                json!([{"type": "text", "text": "Hi"},
                    {"type": "thinking", "thinking": "opaque", "redacted": true}]),
                "stop"
            )),
            Some("Hi".into())
        );
    }

    #[test]
    fn typed_prompts_are_prompts_but_pis_own_messages_are_not() {
        assert_eq!(
            prompt(&user(
                json!([{"type": "text", "text": "Fix the login test"}])
            )),
            Some("Fix the login test".into())
        );
        assert_eq!(
            prompt(&user(json!("And the docs"))),
            Some("And the docs".into())
        );
        assert_eq!(
            typed(&user(json!([
                {"type": "image", "data": "AAAA", "mimeType": "image/png"},
                {"type": "text", "text": "This screenshot"}
            ]))),
            Some("This screenshot".into())
        );
        for role in [
            "system",
            "bashExecution",
            "custom",
            "toolResult",
            "assistant",
        ] {
            let other = entry(
                "x",
                json!({"role": role, "content": "not typed", "command": "ls"}),
            );
            assert_eq!(typed(&other), None, "{role}");
        }
        assert_eq!(
            typed(&json!({"type": "session", "id": "x", "cwd": "/"})),
            None
        );
        assert_eq!(
            typed(&json!({"type": "custom_message", "content": "x", "display": true})),
            None
        );
    }

    #[test]
    fn a_skill_is_the_command_the_user_typed() {
        let expanded = "<skill name=\"review\" location=\"/home/me/.pi/agent/skills/review/SKILL.md\">\n\
            References are relative to /home/me/.pi/agent/skills/review.\n\nCheck it all.\n</skill>";
        assert_eq!(
            prompt(&user(json!(format!("{expanded}\n\nsrc/parser.rs")))),
            Some("/skill:review src/parser.rs".into())
        );
        assert_eq!(prompt(&user(json!(expanded))), Some("/skill:review".into()));
        assert_eq!(skill("<skill name=\"x\">unclosed"), None);
    }

    #[test]
    fn every_line_typed_takes_passes_the_cheap_check_first() {
        let entries = [
            user(json!([{"type": "text", "text": "Fix it"}])),
            user(json!("plain")),
            assistant("a", json!([{"type": "text", "text": "hi"}]), "stop"),
        ];
        for entry in &entries {
            let line = serde_json::to_vec(entry).unwrap();
            assert_eq!(may_be_typed(&line), typed(entry).is_some(), "{entry}");
        }
    }

    #[test]
    fn the_reply_is_the_final_response_after_the_last_tool_call() {
        let mut reply = None;
        for entry in [
            user(json!([{"type": "text", "text": "fix the test"}])),
            assistant(
                "a1",
                json!([{"type": "text", "text": "Let me look."},
                    {"type": "toolCall", "id": "c1", "name": "bash", "arguments": {"command": "ls"}}]),
                "toolUse",
            ),
            entry(
                "r1",
                json!({"role": "toolResult", "toolCallId": "c1", "toolName": "bash",
                    "content": [{"type": "text", "text": "out"}], "isError": false}),
            ),
            assistant(
                "a2",
                json!([{"type": "thinking", "thinking": "Done."},
                    {"type": "text", "text": "Fixed it.\n\n- the parser"},
                    {"type": "text", "text": " Tests pass. "}]),
                "stop",
            ),
            json!({"type": "model_change", "id": "m1", "provider": "p", "modelId": "m"}),
        ] {
            reply = super::super::next_reply(reply, reply_step(&entry), 7);
        }
        let reply = reply.unwrap();
        assert_eq!(reply.key, "a2");
        assert_eq!(reply.text, "Fixed it.\n\n- the parser\n\nTests pass.");
        assert_eq!(reply.position, Some(7));
        // The next prompt starts it over.
        let next = super::super::next_reply(Some(reply), reply_step(&user(json!("next"))), 9);
        assert_eq!(next, None);
    }

    /// A follower of the Pi conversation in `path`, as one that found it.
    fn follower(path: &std::path::Path, id: &str) -> super::super::Follower {
        use super::super::{Follower, LogAgent, Source, Turn};
        Follower {
            // No such process: nothing else is looked for.
            pid: u32::MAX,
            source: Source::Log {
                agent: LogAgent::Pi,
                conversation: Some(id.into()),
                path: path.to_path_buf(),
                offset: 0,
                partial: Vec::new(),
            },
            latest: None,
            prompt: None,
            turn: Turn::default(),
            reply: None,
            reply_state: Default::default(),
            last_at: None,
            moved_at: None,
            questions: Default::default(),
            foreground: None,
        }
    }

    #[test]
    fn the_follower_reads_the_turn_its_reply_and_the_conversation() {
        use std::io::Write;
        let directory = tempfile::tempdir().unwrap();
        let id = "01a18a95-6f2e-7c3b-9a1d-2cd72115b9d0";
        let path = directory
            .path()
            .join(format!("2026-10-30T02-13-49-215Z_{id}.jsonl"));
        let mut file = std::fs::File::create(&path).unwrap();
        let at = |entry: Value, time: &str| {
            let mut entry = entry;
            entry["timestamp"] = json!(time);
            format!("{entry}\n")
        };
        for line in [
            json!({"type": "session", "version": 3, "id": id,
                "timestamp": "2026-10-30T02:13:49.215Z", "cwd": "/repo"})
            .to_string()
                + "\n",
            at(
                entry(
                    "s",
                    json!({"role": "system", "content": "", "sections": {}}),
                ),
                "2026-10-30T02:13:50.000Z",
            ),
            at(
                user(json!([{"type": "text", "text": "Fix the parser"}])),
                "2026-10-30T02:13:51.000Z",
            ),
            at(
                assistant(
                    "a1",
                    json!([{"type": "thinking", "thinking": "Read it first."},
                        {"type": "toolCall", "id": "c1", "name": "read",
                            "arguments": {"path": "/repo/src/parser.rs"}}]),
                    "toolUse",
                ),
                "2026-10-30T02:13:52.000Z",
            ),
        ] {
            file.write_all(line.as_bytes()).unwrap();
        }
        let mut follower = follower(&path, id);
        assert!(follower.follow(None));
        assert_eq!(follower.agent(), "pi");
        assert_eq!(follower.conversation().as_deref(), Some(id));
        assert_eq!(follower.prompt(), Some("Fix the parser"));
        assert_eq!(follower.latest(), Some("read: src/parser.rs"));
        assert_eq!(follower.turn().prompt.as_deref(), Some("Fix the parser"));
        assert_eq!(
            follower.turn().messages,
            ["Read it first.", "read: src/parser.rs"]
        );
        assert_eq!(follower.reply(), None);
        assert_eq!(follower.last_at(), Some(1_793_326_432_000));
        let before = std::fs::metadata(&path).unwrap().len();
        let reply = at(
            assistant(
                "a2",
                json!([{"type": "text", "text": "Fixed: the parser skips blank lines."}]),
                "stop",
            ),
            "2026-10-30T02:14:00.000Z",
        );
        file.write_all(reply.as_bytes()).unwrap();
        assert!(follower.follow(None));
        let found = follower.reply().unwrap();
        assert_eq!(found.key, "a2");
        assert_eq!(found.text, "Fixed: the parser skips blank lines.");
        assert_eq!(found.position, Some(before));
        assert_eq!(follower.turn().total, 3);
        assert_eq!(
            follower.latest(),
            Some("Fixed: the parser skips blank lines.")
        );
        assert!(follower.transcript_source().is_some());
        // A new prompt is a new turn, without a reply yet.
        let first = follower.turn().id;
        file.write_all(at(user(json!("Now the docs")), "2026-10-30T02:15:00.000Z").as_bytes())
            .unwrap();
        assert!(follower.follow(None));
        assert_ne!(follower.turn().id, first);
        assert_eq!(follower.reply(), None);
        assert_eq!(follower.prompt(), Some("Now the docs"));
        // The turn it ended (the prompt steered in as the response finished,
        // or typed once it waited) keeps its reply, read as the log grew.
        let finished = follower.take_finished();
        assert_eq!(finished.len(), 1, "{finished:?}");
        assert_eq!(finished[0].reply.key, "a2");
        assert_eq!(
            finished[0].reply.text,
            "Fixed: the parser skips blank lines."
        );
        assert_eq!(finished[0].conversation.as_deref(), Some(id));
        assert_eq!(
            (finished[0].at, finished[0].ended),
            (Some(1_793_326_440_000), Some(1_793_326_500_000))
        );
        // Steered in after a tool call: the turn has no reply of its own.
        for line in [
            at(
                assistant(
                    "a3",
                    json!([{"type": "toolCall", "id": "c2", "name": "read",
                        "arguments": {"path": "/repo/README.md"}}]),
                    "toolUse",
                ),
                "2026-10-30T02:15:05.000Z",
            ),
            at(user(json!("Skip the README")), "2026-10-30T02:15:06.000Z"),
        ] {
            file.write_all(line.as_bytes()).unwrap();
        }
        assert!(follower.follow(None));
        assert_eq!(follower.take_finished(), []);
    }

    #[test]
    fn a_follower_without_a_conversation_reads_nothing() {
        let mut follower = follower(std::path::Path::new(""), "x");
        if let super::super::Source::Log { conversation, .. } = &mut follower.source {
            *conversation = None;
        }
        assert!(!follower.follow(None));
        assert_eq!(follower.conversation(), None);
        assert!(follower.transcript_source().is_none());
    }

    #[test]
    fn a_failed_or_aborted_response_is_no_reply() {
        for stop in ["error", "aborted"] {
            let failed = assistant("a", json!([{"type": "text", "text": "partial"}]), stop);
            assert_eq!(reply_step(&failed), Step::Reset, "{stop}");
        }
        let cut = assistant("a", json!([{"type": "text", "text": "long"}]), "length");
        assert!(matches!(reply_step(&cut), Step::Complete { .. }));
        // Thinking alone, a tool's result, a `!` command: nothing to add.
        let thinking = assistant("a", json!([{"type": "thinking", "thinking": "hm"}]), "stop");
        assert_eq!(reply_step(&thinking), Step::Keep);
        let bang = entry(
            "b",
            json!({"role": "bashExecution", "command": "ls", "output": "x", "exitCode": 0}),
        );
        assert_eq!(reply_step(&bang), Step::Keep);
    }
}
