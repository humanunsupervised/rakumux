//! Droid's latest output, from its session file
//! (`~/.factory/sessions/<folder>/<conversation>.jsonl`): one message per
//! line (`{"type":"message","id",…,"message":{"role","content"}}`, content in
//! Anthropic's blocks), after a first line that sums the session up.
//!
//! **The first line is rewritten in place** (the title it makes after the
//! first message and the first edit, `/rename`, a new directory), which
//! moves every byte after it. So this module counts positions from the end
//! of the first line (the body), reads the first line's length again before
//! each read, and drops a read the first line changed under: the follower,
//! the transcript and places never read a line twice or skip one, and a
//! reply's position stays put. Nothing else in the file changes once
//! written (a rewind or a compaction starts another session).
//!
//! Droid's own rows are left out: hook rows (`hookEventName`), notices for
//! the model or the user only (`visibility`), and other agents' messages
//! (`sender`). Tool results are written as the user's.
use super::{contains, first_line, prompt_line, short_path, tool_line, Step};
use serde_json::Value;
use std::io::Read;
use std::path::{Path, PathBuf};

/// Thinking and replies longer than this are cut, in bytes, before they're
/// shaped into a line or a summary's message.
const MAX_BLOCK: usize = 8 * 1024;
/// The longest first line read.
const MAX_HEAD: u64 = 1024 * 1024;
/// Reads retried when the first line changed under them.
const ATTEMPTS: usize = 3;

/// Where the body starts: the first line's length, its newline included, once
/// it's whole.
pub(super) fn head(path: &Path) -> Option<u64> {
    let file = std::fs::File::open(path).ok()?;
    let mut reader = file.take(MAX_HEAD);
    let mut chunk = [0u8; 4096];
    let mut read = 0u64;
    loop {
        let count = reader.read(&mut chunk).ok()?;
        if count == 0 {
            return None;
        }
        if let Some(at) = chunk[..count].iter().position(|b| *b == b'\n') {
            return Some(read + at as u64 + 1);
        }
        read += count as u64;
    }
}

/// Follow the conversation the session's hooks named (`bound`), else the
/// newest one of the folder process `pid` works in, to its file: a new one
/// is read from its tail. Returns whether it switched.
pub(super) fn switch(
    pid: u32,
    bound: Option<&str>,
    conversation: &mut Option<String>,
    path: &mut PathBuf,
    offset: &mut u64,
    partial: &mut Vec<u8>,
) -> bool {
    let unchanged = bound.is_some_and(|id| conversation.as_deref() == Some(id)) && path.is_file();
    if unchanged {
        return false;
    }
    let Some(home) = crate::resume::droid_home() else {
        return false;
    };
    let cwd = || crate::procinfo::cwd_of(pid);
    switch_in(&home, bound, cwd, conversation, path, offset, partial)
}

/// [`switch`] with Droid's data directory `home` and the process's
/// directory `cwd`.
fn switch_in(
    home: &Path,
    bound: Option<&str>,
    cwd: impl FnOnce() -> Option<PathBuf>,
    conversation: &mut Option<String>,
    path: &mut PathBuf,
    offset: &mut u64,
    partial: &mut Vec<u8>,
) -> bool {
    let id = match bound.filter(|id| crate::resume::is_conversation_id(id)) {
        Some(id) => Some(id.to_owned()),
        None => cwd().and_then(|cwd| crate::resume::droid_recent_session(home, &cwd)),
    };
    let Some(id) = id else {
        return false;
    };
    let same = conversation.as_deref() == Some(id.as_str());
    if same && path.is_file() {
        return false;
    }
    let Some(file) = crate::resume::droid_session_file(home, &id) else {
        return false;
    };
    *path = file;
    if same {
        // Droid moved it (a worktree's sessions go to its repository when the
        // worktree does): the same lines, read on from where they were.
        return false;
    }
    *offset = 0;
    partial.clear();
    *conversation = Some(id);
    true
}

/// [`super::read`] for a session file, `body` counted from the end of its
/// first line, and the lines' positions too (the first line itself isn't
/// one). `None` while the first line is being rewritten, or when it changed
/// during the read: nothing was read, try again.
pub(super) fn read(
    path: &Path,
    body: &mut u64,
    partial: &mut Vec<u8>,
    prompt: &dyn Fn(&[u8]) -> bool,
) -> Option<Vec<(u64, Vec<u8>)>> {
    let head = head(path)?;
    // Nothing past the first line yet is read from the start, so a long file
    // new to the follower is read from its tail.
    let start = if *body == 0 { 0 } else { head + *body };
    if std::fs::metadata(path).ok()?.len() < start {
        return None;
    }
    let mut offset = start;
    let mut kept = if *body == 0 {
        Vec::new()
    } else {
        partial.clone()
    };
    let resumed_at = start.saturating_sub(kept.len() as u64);
    let lines = super::read(path, &mut offset, &mut kept, prompt)?;
    // Read again from the start: the file was shorter for a moment.
    let restarted = lines.first().is_some_and(|(at, _)| *at < resumed_at);
    if restarted || self::head(path) != Some(head) {
        return None;
    }
    *body = offset.saturating_sub(head);
    *partial = kept;
    Some(
        lines
            .into_iter()
            .filter(|(at, _)| *at >= head)
            .map(|(at, line)| (at - head, line))
            .collect(),
    )
}

/// [`super::transcript::scan_log`] for a session file, `from` and the
/// positions counted from the end of its first line. From the start, the
/// first line (the session's summary, with its directories) comes first, at
/// 0. Returns where the next unread line starts and whether more were there.
pub(super) fn scan(
    path: &Path,
    from: u64,
    budget: usize,
    each: &mut dyn FnMut(&Value, u64),
) -> Option<(u64, bool)> {
    for _ in 0..ATTEMPTS {
        let Some(head) = head(path) else {
            // Its first line isn't whole: being rewritten, or just begun.
            return std::fs::metadata(path).ok().map(|_| (from, false));
        };
        let start = head + from;
        if std::fs::metadata(path).ok()?.len() < start {
            // The first line is being rewritten: nothing new yet.
            return Some((from, false));
        }
        let mut entries = Vec::new();
        let (next, more) = super::transcript::scan_log(path, start, budget, |entry, at| {
            entries.push((entry.clone(), at))
        })?;
        let restarted = entries.first().is_some_and(|(_, at)| *at < start);
        if restarted || self::head(path) != Some(head) {
            continue;
        }
        if from == 0 {
            if let Some(summary) = summary(path) {
                each(&summary, 0);
            }
        }
        for (entry, at) in &entries {
            each(entry, at - head);
        }
        return Some((next.saturating_sub(head), more));
    }
    None
}

/// The first line, parsed.
fn summary(path: &Path) -> Option<Value> {
    let head = head(path)?;
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .ok()?
        .take(head)
        .read_to_end(&mut bytes)
        .ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// A message line's message, and whether it's Droid's own (a hook row, a
/// notice for the model or the user only, another agent's message): not
/// what the user and the agent said to each other.
fn message(entry: &Value) -> Option<(&Value, bool)> {
    if entry.get("type")?.as_str()? != "message" {
        return None;
    }
    let message = entry.get("message")?;
    let own = message.get("hookEventName").is_some()
        || message.get("sender").is_some()
        || matches!(
            message.get("visibility").and_then(Value::as_str),
            Some("llm_only" | "user_only")
        );
    Some((message, own))
}

/// A message of the agent's, with what it says.
fn assistant(entry: &Value) -> Option<&Value> {
    let (message, own) = message(entry)?;
    (!own && message.get("role")?.as_str()? == "assistant").then_some(message)
}

/// An assistant message's last thinking, tool call or text.
pub(super) fn line(entry: &Value) -> Option<String> {
    messages(entry)
        .iter()
        .rev()
        .find_map(|message| first_line(message))
}

/// An assistant message's parts, oldest first: its reasoning and replies as
/// written, its tool calls as lines.
pub(super) fn messages(entry: &Value) -> Vec<String> {
    let Some(message) = assistant(entry) else {
        return Vec::new();
    };
    let blocks = message
        .get("content")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    let mut parts = Vec::new();
    if !blocks.iter().any(|block| block["type"] == "thinking") {
        parts.extend(reasoning(message).and_then(bounded));
    }
    parts.extend(blocks.iter().filter_map(block));
    parts
}

/// Reasoning a model gave as a summary rather than a thinking block (GPT's
/// summary, a chat-completions model's reasoning).
pub(super) fn reasoning(message: &Value) -> Option<&str> {
    ["openaiReasoningSummary", "chatCompletionReasoningContent"]
        .iter()
        .find_map(|key| message.get(key)?.as_str())
        .filter(|text| !text.trim().is_empty())
}

/// What the user typed, as a line.
pub(super) fn prompt(entry: &Value) -> Option<String> {
    prompt_line(&typed(entry)?)
}

/// What the user typed in a message, as written: not a tool's result (Droid
/// writes them as the user's), nor Droid's own rows.
pub(super) fn typed(entry: &Value) -> Option<String> {
    let (message, own) = message(entry)?;
    if own || message.get("role")?.as_str()? != "user" {
        return None;
    }
    match message.get("content")? {
        Value::String(text) => Some(text.clone()),
        Value::Array(blocks) => {
            let mut texts = Vec::new();
            for block in blocks {
                match block.get("type")?.as_str()? {
                    "text" => texts.push(block.get("text")?.as_str()?),
                    "tool_result" => return None,
                    _ => {}
                }
            }
            (!texts.is_empty()).then(|| texts.join("\n"))
        }
        _ => None,
    }
}

/// Whether a line may be one [`typed`] takes, before it's parsed: a user's
/// message that holds no tool result and isn't a hook's row.
pub(super) fn may_be_typed(line: &[u8]) -> bool {
    contains(line, br#""role":"user""#)
        && !contains(line, br#""type":"tool_result""#)
        && !contains(line, br#""hookEventName""#)
}

/// A block's message: thinking or text as written (cut at [`MAX_BLOCK`]), a
/// tool call as a line.
fn block(block: &Value) -> Option<String> {
    match block.get("type")?.as_str()? {
        "thinking" => bounded(block.get("thinking")?.as_str()?),
        "text" => bounded(block.get("text")?.as_str()?),
        "tool_use" => {
            let name = block.get("name")?.as_str()?;
            let input = block.get("input");
            let field = |key: &str| input?.get(key)?.as_str();
            let subject = match name {
                // Its one-line summary says more than the command.
                "Execute" => field("summary")
                    .filter(|summary| !summary.trim().is_empty())
                    .or_else(|| field("command"))
                    .map(String::from),
                "Read" | "Create" | "Edit" | "MultiEdit" | "Write" => {
                    field("file_path").map(short_path)
                }
                "ApplyPatch" => field("input").and_then(patched).map(short_path),
                "LS" => field("directory_path").map(short_path),
                "Grep" => field("pattern").map(String::from),
                "Glob" => input
                    .and_then(|input| input.get("patterns"))
                    .and_then(Value::as_array)
                    .map(|patterns| {
                        patterns
                            .iter()
                            .filter_map(Value::as_str)
                            .collect::<Vec<_>>()
                            .join(" ")
                    }),
                "FetchUrl" => field("url").map(String::from),
                "WebSearch" => field("query").map(String::from),
                "Task" => field("description").map(String::from),
                _ => None,
            };
            tool_line(name, subject.as_deref())
        }
        _ => None,
    }
}

/// `text` trimmed and cut at [`MAX_BLOCK`] bytes, when it says something.
fn bounded(text: &str) -> Option<String> {
    let text = text.trim();
    let end = (0..=text.len().min(MAX_BLOCK))
        .rev()
        .find(|at| text.is_char_boundary(*at))?;
    (!text.is_empty()).then(|| text[..end].to_string())
}

/// The first file a patch changes.
fn patched(patch: &str) -> Option<&str> {
    patch.lines().find_map(|line| {
        ["*** Add File:", "*** Update File:", "*** Delete File:"]
            .iter()
            .find_map(|header| line.strip_prefix(header))
            .map(str::trim)
    })
}

/// How a line changes the turn's reply: a prompt, a tool's result or a tool
/// call starts it over, and the agent's text adds to it. Droid's own rows
/// (the hooks' after the reply among them) change nothing.
pub(super) fn reply_step(entry: &Value) -> Step {
    let Some((message, own)) = message(entry) else {
        return Step::Keep;
    };
    if own {
        return Step::Keep;
    }
    match message.get("role").and_then(Value::as_str) {
        Some("user") => Step::Reset,
        Some("assistant") => {
            let blocks = match message.get("content") {
                Some(Value::Array(blocks)) => blocks.as_slice(),
                Some(Value::String(text)) => {
                    return match entry.get("id").and_then(Value::as_str) {
                        Some(key) if !text.trim().is_empty() => Step::Text {
                            key: key.to_owned(),
                            text: text.trim().to_owned(),
                        },
                        _ => Step::Keep,
                    };
                }
                _ => return Step::Keep,
            };
            if blocks.iter().any(|block| block["type"] == "tool_use") {
                return Step::Reset;
            }
            let text: Vec<&str> = blocks
                .iter()
                .filter(|block| block["type"] == "text")
                .filter_map(|block| block.get("text").and_then(Value::as_str))
                .map(str::trim)
                .filter(|text| !text.is_empty())
                .collect();
            match entry.get("id").and_then(Value::as_str) {
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
pub(super) mod tests {
    use super::super::{Follower, LogAgent, Source, Turn};
    use super::*;
    use serde_json::json;

    pub(crate) const ID: &str = "0f5c6d1e-8a2b-4c3d-9e4f-5a6b7c8d9e0f";

    /// A session file's first line.
    pub(crate) fn summary(title: &str) -> String {
        json!({"type": "session_start", "id": ID, "title": title, "owner": "me",
            "version": 2, "cwd": "/repo"})
        .to_string()
    }

    /// A message line.
    pub(crate) fn message(id: &str, role: &str, content: Value) -> Value {
        json!({"type": "message", "id": id, "timestamp": "2026-10-30T10:00:00.000Z",
            "message": {"role": role, "content": content}})
    }

    /// Droid's row for a hook it ran.
    pub(crate) fn hook_row(id: &str, event: &str) -> Value {
        json!({"type": "message", "id": id, "timestamp": "2026-10-30T10:00:01.000Z",
            "message": {"role": "user", "content": [], "visibility": "user_only",
                "hookEventName": event, "hookStatus": "completed",
                "hookResults": [{"exitCode": 0, "stdout": "{\"suppressOutput\":true}",
                    "suppressOutput": true}]}})
    }

    pub(crate) fn write_session(path: &Path, title: &str, lines: &[Value]) {
        let mut text = format!("{}\n", summary(title));
        for line in lines {
            text.push_str(&format!("{line}\n"));
        }
        std::fs::write(path, text).unwrap();
    }

    pub(crate) fn append(path: &Path, line: &Value) {
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new().append(true).open(path).unwrap();
        writeln!(file, "{line}").unwrap();
    }

    /// What Droid does when it names the session: the whole file read, its
    /// first line replaced, the file written again in place.
    pub(crate) fn retitle(path: &Path, title: &str) {
        let text = std::fs::read_to_string(path).unwrap();
        let mut lines: Vec<&str> = text.split('\n').collect();
        let first = summary(title);
        lines[0] = &first;
        std::fs::write(path, lines.join("\n")).unwrap();
    }

    /// Where `needle`'s line starts past the first line.
    pub(crate) fn body_position(path: &Path, needle: &str) -> u64 {
        let text = std::fs::read_to_string(path).unwrap();
        let head = text.find('\n').unwrap() + 1;
        let at = text.find(needle).unwrap();
        let line = text[..at].rfind('\n').unwrap() + 1;
        (line - head) as u64
    }

    fn follower(path: PathBuf) -> Follower {
        Follower {
            pid: 1,
            source: Source::Log {
                agent: LogAgent::Droid,
                conversation: Some(ID.into()),
                path,
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

    fn prompt_line(id: &str, text: &str) -> Value {
        message(id, "user", json!([{"type": "text", "text": text}]))
    }

    #[test]
    fn a_turn_its_queued_prompt_ended_keeps_its_reply() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join(format!("{ID}.jsonl"));
        write_session(&path, "New session", &[prompt_line("u1", "Fix the test")]);
        let mut follower = follower(path.clone());
        follower.follow(Some(ID));
        append(
            &path,
            &message(
                "a1",
                "assistant",
                json!([{"type": "text", "text": "Fixed it."}]),
            ),
        );
        append(&path, &hook_row("h1", "Stop"));
        follower.follow(Some(ID));
        assert_eq!(follower.take_finished(), []);
        // Droid sends what was typed during the turn once it ends.
        append(&path, &prompt_line("u2", "Now the docs"));
        follower.follow(Some(ID));
        let finished = follower.take_finished();
        assert_eq!(finished.len(), 1, "{finished:?}");
        assert_eq!(finished[0].reply.key, "a1");
        assert_eq!(finished[0].reply.text, "Fixed it.");
        assert_eq!(finished[0].conversation.as_deref(), Some(ID));
    }

    #[test]
    fn a_rewritten_first_line_neither_repeats_nor_drops_a_line() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join(format!("{ID}.jsonl"));
        write_session(
            &path,
            "New session",
            &[
                prompt_line("u1", "Fix the flaky login test"),
                hook_row("h1", "UserPromptSubmit"),
                message(
                    "a1",
                    "assistant",
                    json!([
                        {"type": "thinking", "thinking": "The form submits early.", "signature": "s"},
                        {"type": "text", "text": "Running the tests first."},
                        {"type": "tool_use", "id": "call_1", "name": "Execute",
                         "input": {"command": "npm test", "summary": "Run the login tests"}}
                    ]),
                ),
                message(
                    "t1",
                    "user",
                    json!([{"type": "tool_result", "tool_use_id": "call_1", "content": "1 failing"}]),
                ),
            ],
        );
        let mut follower = follower(path.clone());
        assert!(follower.follow(Some(ID)));
        assert_eq!(follower.prompt(), Some("Fix the flaky login test"));
        assert_eq!(follower.latest(), Some("Execute: Run the login tests"));
        let turn = follower.turn().clone();
        assert_eq!(turn.prompt.as_deref(), Some("Fix the flaky login test"));
        assert_eq!(
            turn.messages,
            [
                "The form submits early.",
                "Running the tests first.",
                "Execute: Run the login tests"
            ]
        );
        assert_eq!(follower.conversation().as_deref(), Some(ID));
        assert_eq!(follower.agent(), "droid");
        // Droid names the session (a longer first line), then replies.
        retitle(&path, "Fix the flaky login test in the browser suite");
        append(
            &path,
            &message(
                "a2",
                "assistant",
                json!([{"type": "text", "text": "Fixed: the form waits for the field."}]),
            ),
        );
        append(&path, &hook_row("h2", "Stop"));
        append(
            &path,
            &json!({"type": "agent_turn_outcome", "turnId": "u1", "reason": "completed",
                "resultKind": "null"}),
        );
        assert!(follower.follow(Some(ID)));
        assert_eq!(
            follower.latest(),
            Some("Fixed: the form waits for the field.")
        );
        assert_eq!(follower.turn().id, turn.id, "the prompt wasn't read again");
        assert_eq!(follower.turn().total, 4);
        let reply = follower.reply().unwrap().clone();
        assert_eq!(reply.key, "a2");
        assert_eq!(reply.text, "Fixed: the form waits for the field.");
        assert_eq!(reply.position, Some(body_position(&path, "\"a2\"")));
        // A shorter title: nothing new, nothing lost, the reply where it was.
        retitle(&path, "Login");
        assert!(!follower.follow(Some(ID)));
        assert_eq!(follower.reply(), Some(&reply));
        assert_eq!(reply.position, Some(body_position(&path, "\"a2\"")));
        // The next prompt is the next turn, read once.
        append(&path, &prompt_line("u2", "Now the docs"));
        assert!(follower.follow(Some(ID)));
        assert_eq!(follower.turn().id, turn.id + 1);
        assert_eq!(follower.prompt(), Some("Now the docs"));
        assert_eq!(follower.reply(), None);
        assert!(!follower.follow(Some(ID)));
    }

    #[test]
    fn a_read_while_the_first_line_is_rewritten_waits() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join(format!("{ID}.jsonl"));
        write_session(&path, "New session", &[prompt_line("u1", "Fix it")]);
        let mut follower = follower(path.clone());
        assert!(follower.follow(Some(ID)));
        let whole = std::fs::read(&path).unwrap();
        // Truncated, then written again: what's there meanwhile isn't read.
        std::fs::write(&path, b"").unwrap();
        assert!(!follower.follow(Some(ID)));
        std::fs::write(&path, &whole[..10]).unwrap();
        assert!(!follower.follow(Some(ID)));
        std::fs::write(&path, &whole[..whole.len() - 3]).unwrap();
        assert!(!follower.follow(Some(ID)));
        std::fs::write(&path, &whole).unwrap();
        retitle(&path, "Fix it now");
        append(
            &path,
            &message(
                "a1",
                "assistant",
                json!([{"type": "text", "text": "Done."}]),
            ),
        );
        assert!(follower.follow(Some(ID)));
        assert_eq!(follower.turn().prompt.as_deref(), Some("Fix it"));
        assert_eq!(follower.turn().messages, ["Done."]);
        assert_eq!(follower.turn().id, 1);
    }

    #[test]
    fn a_long_session_is_read_from_its_tail_with_its_turns_prompt() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join(format!("{ID}.jsonl"));
        let mut lines = vec![
            prompt_line("u0", "An earlier prompt"),
            prompt_line("u1", "Fix the flaky login test"),
        ];
        for n in 0..400 {
            lines.push(message(
                &format!("t{n}"),
                "user",
                json!([{"type": "tool_result", "tool_use_id": "c", "content": "y".repeat(1000)}]),
            ));
        }
        lines.push(message(
            "a1",
            "assistant",
            json!([{"type": "text", "text": "Fixed."}]),
        ));
        write_session(&path, "Login", &lines);
        let mut follower = follower(path.clone());
        assert!(follower.follow(Some(ID)));
        assert_eq!(
            follower.turn().prompt.as_deref(),
            Some("Fix the flaky login test")
        );
        assert_eq!(follower.turn().messages, ["Fixed."]);
        assert_eq!(
            follower.reply().unwrap().position,
            Some(body_position(&path, "\"a1\""))
        );
        assert!(!follower.follow(Some(ID)));
    }

    #[test]
    fn the_conversation_is_the_one_its_hooks_name_else_the_folders_newest() {
        let home = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        let folder = home
            .path()
            .join("sessions")
            .join(crate::resume::droid_project_key(work.path()));
        std::fs::create_dir_all(&folder).unwrap();
        let other = "11111111-1111-4111-8111-111111111111";
        write_session(&folder.join(format!("{other}.jsonl")), "Older", &[]);
        std::thread::sleep(std::time::Duration::from_millis(20));
        write_session(&folder.join(format!("{ID}.jsonl")), "Newest", &[]);
        let cwd = || Some(work.path().to_path_buf());
        let (mut conversation, mut path, mut offset, mut partial) =
            (None, PathBuf::new(), 0, Vec::new());
        // No hooks: the folder's newest.
        assert!(switch_in(
            home.path(),
            None,
            cwd,
            &mut conversation,
            &mut path,
            &mut offset,
            &mut partial
        ));
        assert_eq!(conversation.as_deref(), Some(ID));
        assert_eq!(path, folder.join(format!("{ID}.jsonl")));
        // The hooks name another (`/new`, `/resume`): it's read from its start.
        offset = 40;
        assert!(switch_in(
            home.path(),
            Some(other),
            cwd,
            &mut conversation,
            &mut path,
            &mut offset,
            &mut partial
        ));
        assert_eq!(conversation.as_deref(), Some(other));
        assert_eq!(offset, 0);
        // Moved to another folder: the same conversation, read on.
        offset = 12;
        let moved = home.path().join("sessions/-elsewhere");
        std::fs::create_dir_all(&moved).unwrap();
        std::fs::rename(&path, moved.join(format!("{other}.jsonl"))).unwrap();
        assert!(!switch_in(
            home.path(),
            Some(other),
            cwd,
            &mut conversation,
            &mut path,
            &mut offset,
            &mut partial
        ));
        assert_eq!(path, moved.join(format!("{other}.jsonl")));
        assert_eq!(offset, 12);
        // One without a file yet, or no id at all ("unknown" never gets
        // here): what was followed stays.
        let unborn = "22222222-2222-4222-8222-222222222222";
        for bound in [Some(unborn), Some("bad id; rm")] {
            assert!(!switch_in(
                home.path(),
                bound,
                || None,
                &mut conversation,
                &mut path,
                &mut offset,
                &mut partial
            ));
            assert_eq!(conversation.as_deref(), Some(other));
        }
    }

    #[test]
    fn a_scan_counts_from_the_first_lines_end_and_starts_with_it() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join(format!("{ID}.jsonl"));
        write_session(
            &path,
            "New session",
            &[
                prompt_line("u1", "Fix it"),
                message(
                    "a1",
                    "assistant",
                    json!([{"type": "text", "text": "Done."}]),
                ),
            ],
        );
        let scan_all = |from: u64| {
            let mut seen = Vec::new();
            let (next, more) = scan(&path, from, 1 << 20, &mut |entry, at| {
                let id = entry
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or("?")
                    .to_owned();
                seen.push((id, at));
            })
            .unwrap();
            (seen, next, more)
        };
        let (seen, next, more) = scan_all(0);
        assert!(!more);
        assert_eq!(
            seen,
            [
                (ID.to_owned(), 0),
                ("u1".to_owned(), 0),
                ("a1".to_owned(), body_position(&path, "\"a1\""))
            ]
        );
        retitle(&path, "A much longer title than before");
        append(
            &path,
            &message(
                "a2",
                "assistant",
                json!([{"type": "text", "text": "More."}]),
            ),
        );
        let (seen, _, _) = scan_all(next);
        assert_eq!(seen, [("a2".to_owned(), body_position(&path, "\"a2\""))]);
        // Mid-rewrite, the file shorter than the cursor: nothing yet.
        std::fs::write(&path, summary("x")).unwrap();
        assert_eq!(scan_all(next).0, []);
    }

    #[test]
    fn prompts_are_what_the_user_typed_not_droids_rows_or_tool_results() {
        assert_eq!(
            prompt(&message("u", "user", json!("Fix the login test"))),
            Some("Fix the login test".into())
        );
        assert_eq!(
            prompt(&message(
                "u",
                "user",
                json!([{"type": "image", "source": {}},
                    {"type": "text", "text": "This screenshot<system-reminder>x</system-reminder>"}])
            )),
            Some("This screenshot".into())
        );
        assert_eq!(
            prompt(&message(
                "t",
                "user",
                json!([{"type": "tool_result", "tool_use_id": "c", "content": "secret output"}])
            )),
            None
        );
        assert_eq!(prompt(&hook_row("h", "UserPromptSubmit")), None);
        for visibility in ["llm_only", "user_only"] {
            let mut notice = message("n", "user", json!([{"type": "text", "text": "Notice"}]));
            notice["message"]["visibility"] = json!(visibility);
            assert_eq!(prompt(&notice), None, "{visibility}");
        }
        let mut both = message("n", "user", json!("Typed"));
        both["message"]["visibility"] = json!("both");
        assert_eq!(prompt(&both), Some("Typed".into()));
        let mut peer = message("p", "user", json!("From another session"));
        peer["message"]["sender"] = json!({"kind": "session", "sessionId": "s"});
        assert_eq!(prompt(&peer), None);
        assert_eq!(
            prompt(&message(
                "a",
                "assistant",
                json!([{"type": "text", "text": "hi"}])
            )),
            None
        );
        assert_eq!(
            prompt(&json!({"type": "session_start", "title": "x"})),
            None
        );
    }

    #[test]
    fn every_line_typed_takes_passes_the_cheap_check_first() {
        let lines = [
            message("u", "user", json!("Fix the login test")),
            message(
                "u",
                "user",
                json!([{"type": "text", "text": "Mention \"type\":\"tool_result\" and \"hookEventName\""}]),
            ),
            message(
                "t",
                "user",
                json!([{"type": "tool_result", "tool_use_id": "c", "content": "x"}]),
            ),
            hook_row("h", "Stop"),
            message("a", "assistant", json!([{"type": "text", "text": "hi"}])),
        ];
        for entry in &lines {
            let line = serde_json::to_vec(entry).unwrap();
            if typed(entry).is_some() {
                assert!(may_be_typed(&line), "{entry}");
            }
        }
        for entry in &lines[2..] {
            assert!(
                !may_be_typed(&serde_json::to_vec(entry).unwrap()),
                "{entry}"
            );
        }
    }

    #[test]
    fn lines_are_the_agents_reasoning_calls_and_text() {
        let call = |name: &str, input: Value| {
            line(&message(
                "a",
                "assistant",
                json!([{"type": "tool_use", "id": "c", "name": name, "input": input}]),
            ))
        };
        assert_eq!(
            call("Execute", json!({"command": "cargo test", "summary": ""})),
            Some("Execute: cargo test".into())
        );
        assert_eq!(
            call(
                "Edit",
                json!({"file_path": "/repo/tui/src/app.rs", "old_str": "a", "new_str": "b"})
            ),
            Some("Edit: src/app.rs".into())
        );
        assert_eq!(
            call(
                "ApplyPatch",
                json!({"input": "*** Begin Patch\n*** Update File: /repo/src/lib.rs\n@@\n-a\n+b\n*** End Patch"})
            ),
            Some("ApplyPatch: src/lib.rs".into())
        );
        assert_eq!(
            call("Glob", json!({"patterns": ["src/*.rs", "*.toml"]})),
            Some("Glob: src/*.rs *.toml".into())
        );
        assert_eq!(
            call("TodoWrite", json!({"todos": "x"})),
            Some("TodoWrite".into())
        );
        // A GPT model's reasoning comes as a summary, before its calls.
        let mut gpt = message(
            "a",
            "assistant",
            json!([{"type": "tool_use", "id": "c", "name": "Read", "input": {"file_path": "/r/a.rs"}}]),
        );
        gpt["message"]["openaiReasoningSummary"] = json!("Checking the parser first.");
        assert_eq!(
            messages(&gpt),
            ["Checking the parser first.", "Read: r/a.rs"]
        );
        assert_eq!(line(&hook_row("h", "Stop")), None);
        assert_eq!(line(&message("u", "user", json!("prompt"))), None);
        let mut shown = message(
            "a",
            "assistant",
            json!([{"type": "tool_use", "name": "Execute",
            "input": {"command": "ls"}}]),
        );
        shown["message"]["visibility"] = json!("user_only");
        assert_eq!(line(&shown), None, "a `!` command the user ran");
    }

    #[test]
    fn the_reply_is_the_text_after_the_last_tool_call_and_droids_rows_keep_it() {
        let mut reply = None;
        for entry in [
            message("u1", "user", json!("fix the test")),
            message(
                "a1",
                "assistant",
                json!([{"type": "text", "text": "Let me look."},
                {"type": "tool_use", "id": "c", "name": "Read", "input": {}}]),
            ),
            message(
                "t1",
                "user",
                json!([{"type": "tool_result", "tool_use_id": "c", "content": "x"}]),
            ),
            message(
                "a2",
                "assistant",
                json!([{"type": "thinking", "thinking": "Done."},
                {"type": "text", "text": "Fixed it.\n\n- the parser"}]),
            ),
            message(
                "a3",
                "assistant",
                json!([{"type": "text", "text": " Tests pass. "}]),
            ),
            hook_row("h1", "Stop"),
            json!({"type": "agent_turn_outcome", "turnId": "u1", "reason": "completed"}),
        ] {
            reply = super::super::next_reply(reply, reply_step(&entry), 7);
        }
        let reply = reply.unwrap();
        assert_eq!(reply.key, "a3");
        assert_eq!(reply.text, "Fixed it.\n\n- the parser\n\nTests pass.");
        assert_eq!(reply.position, Some(7));
        assert_eq!(
            reply_step(&message("u2", "user", json!("next"))),
            Step::Reset
        );
    }
}
