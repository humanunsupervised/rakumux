//! Cursor's latest output, from its chat's transcript
//! (`<data>/projects/<folder slug>/agent-transcripts/<chat>/<chat>.jsonl`),
//! which it writes for tools and hooks:
//!
//! ```text
//! {"role":"user","message":{"content":[{"type":"text","text":"<user_query>…</user_query>"}]}}
//! {"role":"assistant","message":{"content":[{"type":"text","text":"…"},{"type":"tool_use","name":"…","input":{…}}]}}
//! {"type":"turn_ended","status":"success"}
//! ```
//!
//! Lines have no ids or times, tools' output isn't written, and an
//! assistant line's text ends with its reasoning when it had some. Cursor
//! appends to the file, but writes it whole again on a process's first
//! write, after a finished turn (dropping that `turn_ended` line) and when it
//! compacts or rewinds the chat: the follower never reads past a final
//! `turn_ended` line ([`hold_turn_end`]) and starts over when what it read
//! no longer ends a line ([`realign`]).
use super::{contains, first_line, prompt_line, short_path, tagged, tool_line, Step, MAX_LINE};
use serde_json::Value;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

/// Text longer than this is cut, in bytes, before it's shaped into a line or
/// a summary's message.
const MAX_BLOCK: usize = 8 * 1024;
/// The most project directories searched for a transcript.
const MAX_PROJECTS: usize = 4096;

/// Follow the chat process `pid` runs now (see [`crate::resume::cursor_chat`];
/// the folder's newest only where open files can't be read): a new one is
/// read from its tail. Returns whether it switched.
pub(super) fn switch(
    pid: u32,
    bound: Option<&str>,
    conversation: &mut Option<String>,
    path: &mut PathBuf,
    offset: &mut u64,
    partial: &mut Vec<u8>,
) -> bool {
    let cwd = crate::procinfo::cwd_of(pid);
    let recent = !cfg!(target_os = "linux");
    let Some(current) = crate::resume::cursor_chat(pid, cwd.as_deref(), bound, recent) else {
        return false;
    };
    if conversation.as_deref() == Some(current.as_str()) && path.is_file() {
        return false;
    }
    let Some(found) = crate::resume::cursor_data_dir()
        .and_then(|data| transcript(&data, cwd.as_deref(), &current))
    else {
        return false;
    };
    let switched = conversation.as_deref() != Some(current.as_str()) || *path != found;
    *path = found;
    *offset = 0;
    partial.clear();
    *conversation = Some(current);
    switched
}

/// `path` as Cursor names a project directory: every run of characters that
/// aren't ASCII letters or digits a `-`, none at either end.
pub(super) fn slug(path: &Path) -> String {
    let mut slug = String::new();
    for c in path.to_string_lossy().chars() {
        if c.is_ascii_alphanumeric() {
            slug.push(c);
        } else if !slug.is_empty() && !slug.ends_with('-') {
            slug.push('-');
        }
    }
    slug.trim_end_matches('-').to_string()
}

/// The transcript of chat `id` under `data`: its folder's project first
/// (`cwd`), else any project's (`--workspace` elsewhere, a worktree), the
/// newest copy; the older flat layout too.
pub(super) fn transcript(data: &Path, cwd: Option<&Path>, id: &str) -> Option<PathBuf> {
    if !crate::resume::is_conversation_id(id) {
        return None;
    }
    let projects = data.join("projects");
    let candidates = |project: &Path| {
        let dir = project.join("agent-transcripts");
        [
            dir.join(id).join(format!("{id}.jsonl")),
            dir.join(format!("{id}.jsonl")),
        ]
    };
    if let Some(cwd) = cwd {
        let own = candidates(&projects.join(slug(cwd)));
        if let Some(found) = own.into_iter().find(|path| path.is_file()) {
            return Some(found);
        }
    }
    std::fs::read_dir(&projects)
        .ok()?
        .filter_map(Result::ok)
        .take(MAX_PROJECTS)
        .flat_map(|entry| candidates(&entry.path()))
        .filter_map(|path| {
            let modified = std::fs::metadata(&path).ok()?.modified().ok()?;
            Some((modified, path))
        })
        .max_by_key(|(modified, _)| *modified)
        .map(|(_, path)| path)
}

/// Read again from the start of the line `partial` holds (Cursor's lines
/// are short; a rewrite may have changed it), and from the file's start
/// when where the last read stopped no longer ends a line: the file was
/// written anew with other content (a rewind, a compaction). Returns
/// whether it starts over.
pub(super) fn realign(path: &Path, offset: &mut u64, partial: &mut Vec<u8>) -> bool {
    *offset = offset.saturating_sub(partial.len() as u64);
    partial.clear();
    if *offset == 0 {
        return false;
    }
    if line_ends_before(path, *offset) == Some(false) {
        *offset = 0;
        return true;
    }
    false
}

/// Whether the byte before `offset` in `path` ends a line; `None` when it
/// can't be read (a file shorter than that).
pub(super) fn line_ends_before(path: &Path, offset: u64) -> Option<bool> {
    let mut file = File::open(path).ok()?;
    file.seek(SeekFrom::Start(offset.checked_sub(1)?)).ok()?;
    let mut byte = [0u8];
    file.read_exact(&mut byte).ok()?;
    Some(byte[0] == b'\n')
}

/// Leave `offset` at the start of a final `turn_ended` line: Cursor drops it
/// when it next writes the file whole, with the next prompt where it was.
pub(super) fn hold_turn_end(lines: &[(u64, Vec<u8>)], offset: &mut u64, partial: &[u8]) {
    let Some((start, line)) = lines.last() else {
        return;
    };
    let ended = partial.is_empty()
        && contains(line, b"turn_ended")
        && serde_json::from_slice::<Value>(line).is_ok_and(|entry| is_turn_end(&entry));
    if ended {
        *offset = *start;
    }
}

/// `{"type":"turn_ended",…}`: the end of a turn (`status`: `success`,
/// `aborted` or `error`).
pub(super) fn is_turn_end(entry: &Value) -> bool {
    entry.get("type").and_then(Value::as_str) == Some("turn_ended")
}

/// When `path` was last written, in Unix milliseconds: the time of what it
/// gained (its lines have none of their own).
pub(super) fn modified_ms(path: &Path) -> Option<i64> {
    let modified = std::fs::metadata(path).ok()?.modified().ok()?;
    let ms = modified.duration_since(std::time::UNIX_EPOCH).ok()?;
    i64::try_from(ms.as_millis()).ok()
}

fn blocks(entry: &Value) -> impl Iterator<Item = &Value> {
    entry
        .get("message")
        .and_then(|message| message.get("content"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
}

fn role(entry: &Value) -> Option<&str> {
    entry.get("role").and_then(Value::as_str)
}

/// An assistant text block's text: what the agent said, then its reasoning
/// when it had some (Cursor joins them); redacted reasoning (`[REDACTED]`)
/// left out.
pub(super) fn said(block: &Value) -> Option<&str> {
    if block.get("type").and_then(Value::as_str) != Some("text") {
        return None;
    }
    let mut text = block.get("text")?.as_str()?.trim();
    while let Some(rest) = text.strip_suffix("[REDACTED]") {
        if !(rest.is_empty() || rest.ends_with('\n')) {
            break;
        }
        text = rest.trim_end();
    }
    (!text.is_empty()).then_some(text)
}

/// What the user typed, as written: a user line's text, inside its
/// `<user_query>` when Cursor wrapped it (its own context already taken out).
pub(super) fn typed(entry: &Value) -> Option<String> {
    if role(entry)? != "user" {
        return None;
    }
    let texts: Vec<&str> = blocks(entry)
        .filter(|block| block.get("type").and_then(Value::as_str) == Some("text"))
        .filter_map(|block| block.get("text").and_then(Value::as_str))
        .collect();
    let text = texts.join("\n");
    let text = tagged(&text, "user_query").unwrap_or(text);
    (!text.trim().is_empty()).then_some(text)
}

/// Whether a log line may be one [`typed`] takes, before it's parsed.
pub(super) fn may_be_typed(line: &[u8]) -> bool {
    contains(line, br#""role":"user""#)
}

/// What the user typed in a line, as one line.
pub(super) fn prompt(entry: &Value) -> Option<String> {
    prompt_line(&typed(entry)?)
}

/// An assistant line's messages, oldest first: its text (with its
/// reasoning) as written, its tool calls as lines.
pub(super) fn messages(entry: &Value) -> Vec<String> {
    if role(entry) != Some("assistant") {
        return Vec::new();
    }
    blocks(entry)
        .filter_map(|block| match block.get("type").and_then(Value::as_str)? {
            "text" => {
                let text = said(block)?;
                let end = (0..=text.len().min(MAX_BLOCK))
                    .rev()
                    .find(|at| text.is_char_boundary(*at))?;
                Some(text[..end].to_string())
            }
            "tool_use" => call_line(block),
            _ => None,
        })
        .collect()
}

/// Its last text or tool call, as one line.
pub(super) fn line(entry: &Value) -> Option<String> {
    messages(entry)
        .iter()
        .rev()
        .find_map(|message| first_line(message))
}

/// A tool call as a line: `Shell: Run the tests`, `Read: src/app.rs`.
fn call_line(block: &Value) -> Option<String> {
    let name = block.get("name")?.as_str()?;
    let field = |key: &str| block.get("input")?.get(key)?.as_str();
    let path = [
        "path",
        "file_path",
        "target_file",
        "target_notebook",
        "target_directory",
    ]
    .into_iter()
    .find_map(field)
    .map(short_path);
    let subject = field("description")
        .or_else(|| field("command"))
        .map(String::from)
        .or(path)
        .or_else(|| {
            ["pattern", "glob_pattern", "query", "search_term", "url"]
                .into_iter()
                .find_map(field)
                .map(String::from)
        });
    let subject: Option<String> = subject.map(|subject| subject.chars().take(MAX_LINE).collect());
    tool_line(name, subject.as_deref())
}

/// How a line changes the turn's reply: a prompt or a tool call starts it
/// over, and an assistant line with text only adds to it, keyed by the
/// chat and where the line starts (stable when Cursor writes the file
/// again, since it writes the same lines).
pub(super) fn reply_step(entry: &Value, position: u64, conversation: Option<&str>) -> Step {
    match role(entry) {
        Some("user") => Step::Reset,
        Some("assistant") => {
            if blocks(entry)
                .any(|block| block.get("type").and_then(Value::as_str) == Some("tool_use"))
            {
                return Step::Reset;
            }
            let text: Vec<&str> = blocks(entry).filter_map(said).collect();
            if text.is_empty() {
                return Step::Keep;
            }
            Step::Text {
                key: format!("{}:{position}", conversation.unwrap_or_default()),
                text: text.join("\n\n"),
            }
        }
        _ => Step::Keep,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn user(text: &str) -> Value {
        json!({"role": "user", "message": {"content": [{"type": "text", "text": text}]}})
    }

    fn assistant(content: Value) -> Value {
        json!({"role": "assistant", "message": {"content": content}})
    }

    #[test]
    fn the_typed_prompt_is_unwrapped_from_its_user_query() {
        assert_eq!(
            prompt(&user("<user_query>\nFix the parser\nplease\n</user_query>")),
            Some("Fix the parser".into())
        );
        assert_eq!(prompt(&user("Ship it")), Some("Ship it".into()));
        assert_eq!(typed(&user("  \n")), None);
        assert_eq!(
            typed(&assistant(json!([{"type": "text", "text": "hi"}]))),
            None
        );
        let line = serde_json::to_vec(&user("x")).unwrap();
        assert!(may_be_typed(&line));
        let reply = serde_json::to_vec(&assistant(json!([]))).unwrap();
        assert!(!may_be_typed(&reply));
    }

    #[test]
    fn an_assistant_lines_last_text_or_tool_call_is_its_line() {
        assert_eq!(
            line(&assistant(json!([
                {"type": "text", "text": "I'll run the tests.\n\nThe user wants…"},
                {"type": "tool_use", "name": "Shell", "input": {"command": "cargo test", "working_directory": "/repo"}}
            ]))),
            Some("Shell: cargo test".into())
        );
        assert_eq!(
            line(&assistant(json!([
                {"type": "tool_use", "name": "StrReplace", "input": {"path": "/repo/tui/src/app.rs", "old_string": "a", "new_string": "b"}}
            ]))),
            Some("StrReplace: src/app.rs".into())
        );
        assert_eq!(
            line(&assistant(
                json!([{"type": "tool_use", "name": "TodoWrite", "input": {"todos": []}}])
            )),
            Some("TodoWrite".into())
        );
        assert_eq!(
            line(&assistant(
                json!([{"type": "text", "text": "Done.\n\n[REDACTED]"}])
            )),
            Some("Done.".into())
        );
        assert_eq!(line(&user("my prompt")), None);
    }

    #[test]
    fn the_reply_is_the_text_after_the_last_tool_call_keyed_by_where_it_starts() {
        let mut reply = None;
        for (position, entry) in [
            user("fix the test"),
            assistant(json!([{"type": "text", "text": "Let me look."},
                {"type": "tool_use", "name": "Read", "input": {"path": "/r/a.rs"}}])),
            assistant(json!([{"type": "text", "text": "Fixed it.\n\n[REDACTED]"}])),
            assistant(json!([{"type": "text", "text": " Tests pass. "}])),
            json!({"type": "turn_ended", "status": "success"}),
        ]
        .iter()
        .enumerate()
        {
            let step = reply_step(entry, position as u64 * 100, Some("c1"));
            reply = super::super::next_reply(reply, step, position as u64 * 100);
        }
        let reply = reply.unwrap();
        assert_eq!(reply.key, "c1:300");
        assert_eq!(reply.text, "Fixed it.\n\nTests pass.");
        assert_eq!(reply.position, Some(300));
        let reset =
            super::super::next_reply(Some(reply), reply_step(&user("next"), 500, Some("c1")), 500);
        assert_eq!(reset, None);
    }

    fn follower(path: PathBuf) -> super::super::Follower {
        use super::super::{Follower, LogAgent, Source, Turn};
        Follower {
            // No such process: the chat stays the one given.
            pid: u32::MAX,
            source: Source::Log {
                agent: LogAgent::Cursor,
                conversation: Some("c1".into()),
                path,
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

    fn write_lines(path: &Path, lines: &[Value]) {
        let text: String = lines.iter().map(|line| format!("{line}\n")).collect();
        std::fs::write(path, text).unwrap();
    }

    #[test]
    fn a_turn_written_again_whole_is_followed_without_losing_the_next_prompt() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("c1.jsonl");
        let turn_one = vec![
            user("<user_query>\none\n</user_query>"),
            assistant(
                json!([{"type": "tool_use", "name": "Shell", "input": {"command": "cargo test"}}]),
            ),
            assistant(json!([{"type": "text", "text": "Done."}])),
        ];
        let mut lines = turn_one.clone();
        lines.push(json!({"type": "turn_ended", "status": "success"}));
        write_lines(&path, &lines);
        let mut follower = follower(path.clone());
        assert!(follower.follow(None));
        assert_eq!(follower.agent(), "cursor-agent");
        assert_eq!(follower.conversation().as_deref(), Some("c1"));
        assert_eq!(follower.prompt(), Some("one"));
        assert_eq!(follower.latest(), Some("Done."));
        let first = follower.turn().clone();
        assert_eq!(first.prompt.as_deref(), Some("one"));
        assert_eq!(first.messages, ["Shell: cargo test", "Done."]);
        let reply = follower.reply().unwrap().clone();
        assert_eq!(reply.text, "Done.");
        assert!(reply.key.starts_with("c1:"));
        assert!(follower.last_at().is_some(), "the file's time");
        // Nothing new: the turn's end is read again, and changes nothing.
        assert!(!follower.follow(None));
        assert_eq!(follower.turn(), &first);
        assert_eq!(follower.reply(), Some(&reply));
        // The next prompt: Cursor writes the file whole, without the turn's
        // end, the prompt where it was.
        let mut lines = turn_one.clone();
        lines.push(user("<user_query>\ntwo\n</user_query>"));
        write_lines(&path, &lines);
        assert!(follower.follow(None));
        assert_eq!(follower.prompt(), Some("two"));
        assert_eq!(follower.turn().id, first.id + 1, "one new turn");
        assert_eq!(follower.turn().prompt.as_deref(), Some("two"));
        assert_eq!(follower.reply(), None);
        // Then appends.
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        use std::io::Write;
        writeln!(
            file,
            "{}",
            assistant(json!([{"type": "text", "text": "Second."}]))
        )
        .unwrap();
        writeln!(
            file,
            "{}",
            json!({"type": "turn_ended", "status": "success"})
        )
        .unwrap();
        assert!(follower.follow(None));
        let second = follower.reply().unwrap();
        assert_eq!(second.text, "Second.");
        assert_ne!(second.key, reply.key);
        assert_eq!(follower.turn().messages, ["Second."]);
        // A rewound chat, written whole with other lines: read from the start.
        write_lines(
            &path,
            &[
                user("<user_query>\na first prompt that is much longer than the old one\n</user_query>"),
                assistant(json!([{"type": "text", "text": "Rewound."}])),
            ],
        );
        assert!(follower.follow(None));
        assert_eq!(
            follower.prompt(),
            Some("a first prompt that is much longer than the old one")
        );
        assert_eq!(follower.latest(), Some("Rewound."));
        // Written whole again, longer: where the last read stopped is now
        // inside a line, so it starts over rather than read half a line.
        let long = "x".repeat(400);
        write_lines(
            &path,
            &[
                user(&format!("<user_query>\n{long}\n</user_query>")),
                assistant(json!([{"type": "text", "text": "Again."}])),
            ],
        );
        assert!(follower.follow(None));
        assert_eq!(follower.turn().prompt.as_deref(), Some(long.as_str()));
        assert_eq!(follower.latest(), Some("Again."));
    }

    #[test]
    fn project_slugs_follow_cursors_rule() {
        assert_eq!(slug(Path::new("/home/me/repos/x")), "home-me-repos-x");
        assert_eq!(slug(Path::new("/a b/.c--d/")), "a-b-c-d");
    }

    #[test]
    fn a_chats_transcript_is_found_in_its_folders_project_or_another() {
        let data = tempfile::tempdir().unwrap();
        let write = |path: &str| {
            let path = data.path().join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, "{}\n").unwrap();
            path
        };
        let id = "0f8a3c1e-7b2d-4e5f-9a6b-1c2d3e4f5a6b";
        let own = write(&format!(
            "projects/home-me-x/agent-transcripts/{id}/{id}.jsonl"
        ));
        assert_eq!(
            transcript(data.path(), Some(Path::new("/home/me/x")), id),
            Some(own)
        );
        // Elsewhere (a worktree's workspace), in the older flat layout.
        let elsewhere = "5c6d7e8f-0a1b-4c2d-8e3f-4a5b6c7d8e9f";
        let other = write(&format!("projects/wt/agent-transcripts/{elsewhere}.jsonl"));
        assert_eq!(
            transcript(data.path(), Some(Path::new("/home/me/x")), elsewhere),
            Some(other)
        );
        assert_eq!(transcript(data.path(), None, "../etc"), None);
    }
}
