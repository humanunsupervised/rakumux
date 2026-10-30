//! The latest thing a coding agent said or did, and the user's latest
//! prompt to it, each as one line for the session list: the agent's
//! reasoning, a tool call or its reply; what the user last typed. Read from
//! the agent's own conversation on this machine as it grows; tool output
//! and subagents' messages are never taken, and nothing leaves the daemon
//! but the lines.
//!
//! Provider implementations live beside their hooks in `integrations/<agent>`:
//! - **Claude Code**: `<config dir>/projects/*/<conversation>.jsonl`, the
//!   conversation being the one in `<config dir>/sessions/<pid>.json`.
//! - **Codex**: the `rollout-…-<thread>.jsonl` file the process keeps open.
//! - **OpenCode**: its database (`opencode.db`), for the conversation its
//!   Rakumux plugin bound to the session, else the folder's newest one.
//! - **Pi**: `<agent dir>/sessions/--<folder>--/<time>_<id>.jsonl`, for the
//!   conversation its Rakumux extension bound to the session, else the
//!   folder's newest one.
//!
//! For the agent feed, the follower also keeps the current turn's reply
//! whole ([`Reply`]): what the agent said after its last tool call, or the
//! turn's last message. The control layer captures it when the turn
//! finishes, and only while reply capture is on. The whole conversation is
//! read on demand ([`transcript`]), and where the agent works from it
//! ([`places`]).

use crate::host::ProcessInfo;
use serde_json::Value;
use session_core::api::MAX_REPLY_BYTES;
use std::collections::VecDeque;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

#[path = "claude-code/output.rs"]
mod claude;
#[path = "codex/output.rs"]
mod codex;
#[path = "open-code/output.rs"]
mod opencode;
#[path = "pi/output.rs"]
mod pi;
#[path = "places.rs"]
pub mod places;
#[path = "transcript.rs"]
pub mod transcript;

/// The most of a log read at once: a log that grew more is read from its tail.
const MAX_READ: u64 = 256 * 1024;
/// How far before its tail a new follower looks for the prompt of the turn the
/// tail is in, and reads the turn from it.
const MAX_PROMPT_SEARCH: u64 = 8 * 1024 * 1024;
/// The longest line kept, in characters.
pub const MAX_LINE: usize = 240;
/// The longest prompt or message a turn keeps, in characters.
pub const MAX_TURN_TEXT: usize = 600;
/// The most messages a turn keeps: its newest.
pub const MAX_TURN_MESSAGES: usize = 100;

/// A conversation's current turn, for an AI summary: the user's last
/// prompt and the agent's messages since (its reasoning, replies and tool
/// calls), each flattened to one line of at most [`MAX_TURN_TEXT`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Turn {
    /// Tells this turn from the next one: the prompts a log showed so far,
    /// or OpenCode's prompt row.
    pub id: u64,
    pub prompt: Option<String>,
    /// The newest [`MAX_TURN_MESSAGES`] messages, oldest first.
    pub messages: VecDeque<String>,
    /// Every message of the turn, kept or not.
    pub total: u32,
}

impl Turn {
    fn begin(&mut self, prompt: Option<String>) {
        self.id = self.id.wrapping_add(1);
        self.prompt = prompt;
        self.messages.clear();
        self.total = 0;
    }

    fn push(&mut self, message: &str) {
        let Some(message) = flat(message) else {
            return;
        };
        if self.messages.len() == MAX_TURN_MESSAGES {
            self.messages.pop_front();
        }
        self.messages.push_back(message);
        self.total = self.total.saturating_add(1);
    }
}

/// The current turn's reply, as far as the log has shown it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reply {
    /// The id of the log entry or turn it came from: a reply already
    /// captured is recognised by it.
    pub key: String,
    /// At most [`MAX_REPLY_BYTES`].
    pub text: String,
    /// The reply was longer than `text`.
    pub truncated: bool,
    /// The [`transcript`] position of the reply's last message.
    pub position: Option<u64>,
}

/// How one log entry changes the reply being built.
#[derive(Debug, PartialEq, Eq)]
enum Step {
    /// Nothing to do with the reply (thinking, metadata, …).
    Keep,
    /// A prompt or a tool call: what was said before it isn't the reply.
    Reset,
    /// More of the reply (Claude Code: one text block per entry).
    Text { key: String, text: String },
    /// The whole reply of a finished turn (Codex's `task_complete`).
    Complete { key: String, text: String },
}

/// An agent whose log is an append-only file of JSON lines.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LogAgent {
    Claude,
    Codex,
    Pi,
}

/// Where an agent's output is read from.
#[derive(Debug)]
enum Source {
    /// An append-only log, read from where it was last read.
    Log {
        agent: LogAgent,
        /// Claude Code's conversation, checked on every read: the process
        /// can clear it, resume another or park it in a background job. Pi's,
        /// found again whenever the conversation bound to the session changes
        /// (or, unbound, its folder's newest does); none until its file exists.
        conversation: Option<String>,
        path: PathBuf,
        /// How far the log has been read.
        offset: u64,
        /// The start of a line not finished yet.
        partial: Vec<u8>,
    },
    /// OpenCode's database. The conversation is looked up on every read,
    /// since one OpenCode can switch between them.
    OpenCode {
        database: PathBuf,
        cwd: Option<PathBuf>,
        /// The one the last read found.
        conversation: Option<String>,
    },
}

/// Follows one agent process's conversation.
#[derive(Debug)]
pub struct Follower {
    pid: u32,
    source: Source,
    latest: Option<String>,
    prompt: Option<String>,
    turn: Turn,
    reply: Option<Reply>,
    /// When the conversation last had a prompt or an agent message, by its
    /// own clock (Unix milliseconds): the newest seen, never moving back.
    last_at: Option<i64>,
}

impl Follower {
    /// Where `process`'s output can be read, when it runs `agent` (as
    /// detected, `claude`, `codex` or `opencode`, or as its hooks name it).
    pub fn locate(process: &ProcessInfo, agent: &str) -> Option<Self> {
        let log = |agent, conversation, path| Source::Log {
            agent,
            conversation,
            path,
            offset: 0,
            partial: Vec::new(),
        };
        let source = match agent {
            "claude" | "claude-code" => {
                let config = crate::resume::claude_config_dir()?;
                let id = claude::conversation(&config, process.pid)?;
                let path = claude::transcript(&config, &id)?;
                log(LogAgent::Claude, Some(id), path)
            }
            "codex" => log(LogAgent::Codex, None, codex::log(process)?),
            // Its file is found on the first read.
            "pi" => log(LogAgent::Pi, None, PathBuf::new()),
            "opencode" | "opencode2" => Source::OpenCode {
                database: crate::resume::opencode_database()?,
                cwd: crate::procinfo::cwd_of(process.pid),
                conversation: None,
            },
            _ => return None,
        };
        Some(Self {
            pid: process.pid,
            source,
            latest: None,
            prompt: None,
            turn: Turn::default(),
            reply: None,
            last_at: None,
        })
    }

    /// The agent as replies and turns name it: `claude-code`, `codex`,
    /// `opencode` or `pi`.
    pub fn agent(&self) -> &'static str {
        match &self.source {
            Source::Log {
                agent: LogAgent::Claude,
                ..
            } => "claude-code",
            Source::Log {
                agent: LogAgent::Codex,
                ..
            } => "codex",
            Source::Log {
                agent: LogAgent::Pi,
                ..
            } => "pi",
            Source::OpenCode { .. } => "opencode",
        }
    }

    /// The conversation followed: Claude Code's or Pi's, Codex's thread from
    /// its `rollout-<time>-<thread>.jsonl`, or the OpenCode session last read.
    pub fn conversation(&self) -> Option<String> {
        let id = match &self.source {
            Source::Log {
                agent: LogAgent::Claude | LogAgent::Pi,
                conversation,
                ..
            } => conversation.clone()?,
            Source::Log {
                agent: LogAgent::Codex,
                path,
                ..
            } => {
                let stem = path.file_stem()?.to_str()?;
                stem.get(stem.len().checked_sub(36)?..)?.to_owned()
            }
            Source::OpenCode { conversation, .. } => conversation.clone()?,
        };
        let valid = !id.is_empty()
            && id.len() <= 128
            && id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'));
        valid.then_some(id)
    }

    /// Where the whole conversation is read from, once it's known.
    pub fn transcript_source(&self) -> Option<transcript::TranscriptSource> {
        let conversation = self.conversation()?;
        Some(match &self.source {
            Source::Log { agent, path, .. } => transcript::TranscriptSource::Log {
                agent: *agent,
                conversation,
                path: path.clone(),
            },
            Source::OpenCode { database, .. } => transcript::TranscriptSource::OpenCode {
                database: database.clone(),
                conversation,
            },
        })
    }

    /// The current turn's reply, once the log has shown one.
    pub fn reply(&self) -> Option<&Reply> {
        self.reply.as_ref()
    }

    /// The process whose conversation this is.
    pub fn pid(&self) -> u32 {
        self.pid
    }

    /// The agent's latest line, once one was found.
    pub fn latest(&self) -> Option<&str> {
        self.latest.as_deref()
    }

    /// The user's latest prompt, once one was found.
    pub fn prompt(&self) -> Option<&str> {
        self.prompt.as_deref()
    }

    /// The conversation's current turn.
    pub fn turn(&self) -> &Turn {
        &self.turn
    }

    /// When the conversation last had a prompt or an agent message (Unix
    /// milliseconds), once one was found.
    pub fn last_at(&self) -> Option<i64> {
        self.last_at
    }

    /// Read what the conversation gained since the last time (a log from its
    /// tail the first time). `bound` is the conversation the session named
    /// for this agent (bound with `session_bind`, else its hooks'), if any;
    /// agents whose process names its own conversation (Claude Code, Codex)
    /// don't need it. Returns whether either line changed.
    pub fn follow(&mut self, bound: Option<&str>) -> bool {
        let before = (self.turn.id, self.turn.total);
        let (line, prompt) = match &mut self.source {
            Source::Log {
                agent,
                conversation,
                path,
                offset,
                partial,
            } => {
                let agent = *agent;
                let switched = match agent {
                    LogAgent::Claude => {
                        switch_conversation(self.pid, conversation, path, offset, partial)
                    }
                    LogAgent::Pi => {
                        pi::switch(self.pid, bound, conversation, path, offset, partial)
                    }
                    LogAgent::Codex => false,
                };
                if switched {
                    self.turn.begin(None);
                    self.reply = None;
                }
                // What the user typed: a turn's start.
                let typed = |line: &[u8]| {
                    let candidate = match agent {
                        LogAgent::Claude => claude::may_be_typed(line),
                        LogAgent::Codex => codex::may_be_typed(line),
                        LogAgent::Pi => pi::may_be_typed(line),
                    };
                    candidate
                        && serde_json::from_slice::<Value>(line)
                            .ok()
                            .and_then(|entry| match agent {
                                LogAgent::Claude => claude::typed(&entry),
                                LogAgent::Codex => codex::typed(&entry),
                                LogAgent::Pi => pi::typed(&entry),
                            })
                            .is_some_and(|typed| prompt_line(&typed).is_some())
                };
                let Some(lines) = read(path, offset, partial, &typed) else {
                    return false;
                };
                let entries: Vec<(u64, Value)> = lines
                    .iter()
                    .filter_map(|(at, line)| Some((*at, serde_json::from_slice(line).ok()?)))
                    .collect();
                // In order: a prompt starts a turn, the agent's messages join it.
                for (position, entry) in &entries {
                    let (typed, messages) = match agent {
                        LogAgent::Claude => (claude::typed(entry), claude::messages(entry)),
                        LogAgent::Codex => (codex::typed(entry), codex::messages(entry)),
                        LogAgent::Pi => (pi::typed(entry), pi::messages(entry)),
                    };
                    let typed = typed.filter(|typed| prompt_line(typed).is_some());
                    if typed.is_some() || !messages.is_empty() {
                        let at = entry
                            .get("timestamp")
                            .and_then(Value::as_str)
                            .and_then(transcript::timestamp);
                        self.last_at = self.last_at.max(at);
                    }
                    if let Some(typed) = typed {
                        self.turn.begin(prompt_text(&typed));
                    }
                    for message in &messages {
                        self.turn.push(message);
                    }
                    let step = match agent {
                        LogAgent::Claude => claude::reply_step(entry),
                        LogAgent::Codex => codex::reply_step(entry),
                        LogAgent::Pi => pi::reply_step(entry),
                    };
                    self.reply = next_reply(self.reply.take(), step, *position);
                }
                let newest = |line: fn(&Value) -> Option<String>| {
                    entries.iter().rev().find_map(|(_, entry)| line(entry))
                };
                match agent {
                    LogAgent::Claude => (newest(claude::line), newest(claude::prompt)),
                    LogAgent::Codex => (newest(codex::line), newest(codex::prompt)),
                    LogAgent::Pi => (newest(pi::line), newest(pi::prompt)),
                }
            }
            Source::OpenCode {
                database,
                cwd,
                conversation: followed,
            } => {
                let conversation = match bound {
                    Some(bound) => Some(bound.to_string()),
                    None => cwd
                        .as_deref()
                        .and_then(crate::resume::opencode_recent_session),
                };
                let conversation = conversation.filter(|id| crate::resume::is_conversation_id(id));
                if *followed != conversation {
                    *followed = conversation.clone();
                    self.reply = None;
                }
                match conversation {
                    Some(id) => {
                        self.last_at = self.last_at.max(opencode::last_at(database, &id));
                        self.reply =
                            opencode::reply(database, &id).map(|(key, text, position)| Reply {
                                position: Some(position),
                                ..bounded(key, text)
                            });
                        if let Some((turn_id, prompt, messages)) = opencode::turn(database, &id) {
                            if turn_id != self.turn.id {
                                self.turn = Turn {
                                    id: turn_id,
                                    ..Turn::default()
                                };
                            }
                            self.turn.prompt = prompt.as_deref().and_then(prompt_text);
                            self.turn.messages.clear();
                            self.turn.total = 0;
                            for message in &messages {
                                self.turn.push(message);
                            }
                        }
                        (
                            opencode::latest(database, &id),
                            opencode::latest_prompt(database, &id),
                        )
                    }
                    None => (None, None),
                }
            }
        };
        // A read with nothing new keeps what was there.
        let mut changed = before != (self.turn.id, self.turn.total);
        for (found, kept) in [(line, &mut self.latest), (prompt, &mut self.prompt)] {
            if let Some(found) = found.filter(|found| kept.as_deref() != Some(found.as_str())) {
                *kept = Some(found);
                changed = true;
            }
        }
        changed
    }
}

/// Follow Claude Code's conversation to wherever the process works now: a
/// new transcript is read from its tail. Returns whether it switched.
fn switch_conversation(
    pid: u32,
    conversation: &mut Option<String>,
    path: &mut PathBuf,
    offset: &mut u64,
    partial: &mut Vec<u8>,
) -> bool {
    let Some(config) = crate::resume::claude_config_dir() else {
        return false;
    };
    let Some(current) = claude::conversation(&config, pid) else {
        return false;
    };
    if conversation.as_deref() == Some(current.as_str()) {
        return false;
    }
    let Some(transcript) = claude::transcript(&config, &current) else {
        return false;
    };
    *path = transcript;
    *offset = 0;
    partial.clear();
    *conversation = Some(current);
    true
}

/// The complete lines `path` gained since `offset` (moved past them), each
/// with where it starts in the file; the start of an unfinished one waits in
/// `partial`. More than [`MAX_READ`] is read from the tail; the first time,
/// from the newest line before it that `prompt` picks (the user's prompt of
/// the turn the tail is in), within [`MAX_PROMPT_SEARCH`].
fn read(
    path: &Path,
    offset: &mut u64,
    partial: &mut Vec<u8>,
    prompt: &dyn Fn(&[u8]) -> bool,
) -> Option<Vec<(u64, Vec<u8>)>> {
    let mut file = File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    if len < *offset {
        // Rewritten: start again.
        *offset = 0;
        partial.clear();
    }
    if len == *offset {
        return Some(Vec::new());
    }
    let skipped = len - *offset > MAX_READ;
    let first = *offset == 0;
    let tail = len - MAX_READ.min(len);
    let start = match (skipped, first) {
        (false, _) => *offset,
        (true, true) => tail.saturating_sub(MAX_PROMPT_SEARCH),
        (true, false) => tail,
    };
    file.seek(SeekFrom::Start(start)).ok()?;
    let mut bytes = Vec::new();
    file.take(len - start).read_to_end(&mut bytes).ok()?;
    *offset = start + bytes.len() as u64;
    let (mut data, base) = if skipped {
        partial.clear();
        // Mid-line: from the next one, or the prompt before it when the
        // tail has none.
        let window = usize::try_from(tail - start).ok()?;
        let from = match bytes[window..].iter().position(|b| *b == b'\n') {
            Some(at) => {
                let from = window + at + 1;
                let prompted = bytes[from..].split(|b| *b == b'\n').any(prompt);
                (!prompted)
                    .then(|| newest_line(&bytes[..from], start == 0, prompt))
                    .flatten()
                    .unwrap_or(from)
            }
            None => bytes.len(),
        };
        (bytes.split_off(from), start + from as u64)
    } else {
        let base = start.saturating_sub(partial.len() as u64);
        let mut data = std::mem::take(partial);
        data.extend_from_slice(&bytes);
        (data, base)
    };
    let complete = data
        .iter()
        .rposition(|b| *b == b'\n')
        .map_or(0, |at| at + 1);
    let rest = data.split_off(complete);
    if (rest.len() as u64) <= MAX_READ {
        *partial = rest;
    }
    let mut lines = Vec::new();
    let mut at = base;
    for line in data.split(|b| *b == b'\n') {
        if !line.is_empty() {
            lines.push((at, line.to_vec()));
        }
        at += line.len() as u64 + 1;
    }
    Some(lines)
}

/// Where the newest line of `head` that `wanted` picks starts. `head` ends
/// a line; its first is whole only when `whole` (the file's start).
fn newest_line(head: &[u8], whole: bool, wanted: &dyn Fn(&[u8]) -> bool) -> Option<usize> {
    let mut end = head.len().checked_sub(1)?;
    loop {
        let begin = match head[..end].iter().rposition(|b| *b == b'\n') {
            Some(at) => at + 1,
            None if whole => 0,
            None => return None,
        };
        if wanted(&head[begin..end]) {
            return Some(begin);
        }
        end = begin.checked_sub(1)?;
    }
}

/// Whether `haystack` holds `needle`.
fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

/// The reply after one more log entry, the one at `position`.
fn next_reply(reply: Option<Reply>, step: Step, position: u64) -> Option<Reply> {
    let at = |reply: Reply| Reply {
        position: Some(position),
        ..reply
    };
    match step {
        Step::Keep => reply,
        Step::Reset => None,
        Step::Text { key, text } => {
            let text = match reply {
                Some(reply) if !reply.truncated => format!("{}\n\n{text}", reply.text),
                Some(reply) => return Some(at(Reply { key, ..reply })),
                None => text,
            };
            Some(at(bounded(key, text)))
        }
        Step::Complete { key, text } => Some(at(bounded(key, text))),
    }
}

/// A reply of at most [`MAX_REPLY_BYTES`], cut at a character boundary.
fn bounded(key: String, mut text: String) -> Reply {
    let truncated = text.len() > MAX_REPLY_BYTES;
    if truncated {
        let mut end = MAX_REPLY_BYTES;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
    }
    Reply {
        key,
        text,
        truncated,
        position: None,
    }
}

/// `text`'s first meaningful line, for the list: Markdown marks and control
/// characters dropped, whitespace collapsed, at most [`MAX_LINE`] characters.
fn first_line(text: &str) -> Option<String> {
    let line = text
        .lines()
        .map(str::trim)
        .find(|line| line.chars().any(char::is_alphanumeric))?;
    let line = line
        .trim_start_matches(['#', '>', '-', '*', ' '])
        .replace("**", "")
        .replace('`', "");
    let words: Vec<&str> = line
        .split(|c: char| c.is_whitespace() || c.is_control())
        .filter(|word| !word.is_empty())
        .collect();
    let line: String = words.join(" ").chars().take(MAX_LINE).collect();
    (!line.is_empty()).then_some(line)
}

/// What the user typed, as a line: the agents' own wrapped insertions
/// (reminders, notifications, command output) dropped, a slash command as
/// `/name args`, and a paste's first line only when nothing was typed
/// around it.
fn prompt_line(text: &str) -> Option<String> {
    first_line(&typed_text(text)?)
}

/// What the user typed, whole, flattened for a summary ([`flat`]).
fn prompt_text(text: &str) -> Option<String> {
    flat(&typed_text(text)?)
}

/// `text` flattened to one line for a summary: whitespace and control
/// characters collapsed, at most [`MAX_TURN_TEXT`] characters.
fn flat(text: &str) -> Option<String> {
    let words: Vec<&str> = text
        .split(|c: char| c.is_whitespace() || c.is_control())
        .filter(|word| !word.is_empty())
        .collect();
    let text: String = words.join(" ").chars().take(MAX_TURN_TEXT).collect();
    (!text.is_empty()).then_some(text)
}

/// What the user typed in `text`, the agents' own insertions taken out (see
/// [`prompt_line`]).
fn typed_text(text: &str) -> Option<String> {
    if let Some(command) = tagged(text, "command-name") {
        let args = tagged(text, "command-args").unwrap_or_default();
        return Some(format!("{} {}", command.trim(), args.trim()));
    }
    let mut typed = text.to_string();
    for tag in [
        "system-reminder",
        "task-notification",
        "local-command-stdout",
        "local-command-stderr",
        "local-command-caveat",
        "user_instructions",
        "environment_context",
    ] {
        while let Some(stripped) = without(&typed, tag) {
            typed = stripped;
        }
    }
    let paste = tagged(&typed, "pasted_content");
    while let Some(stripped) = without(&typed, "pasted_content") {
        typed = stripped;
    }
    if first_line(&typed).is_some() {
        Some(typed)
    } else {
        paste.filter(|paste| first_line(paste).is_some())
    }
}

/// The text inside `text`'s first `<tag …>…</tag>`.
fn tagged(text: &str, tag: &str) -> Option<String> {
    let open = text.find(&format!("<{tag}"))?;
    let start = open + text[open..].find('>')? + 1;
    let end = start + text[start..].find(&format!("</{tag}>"))?;
    Some(text[start..end].to_string())
}

/// `text` without its first `<tag …>…</tag>` (to its end when unclosed).
fn without(text: &str, tag: &str) -> Option<String> {
    let open = text.find(&format!("<{tag}"))?;
    let close = format!("</{tag}>");
    let end = text[open..]
        .find(&close)
        .map_or(text.len(), |at| open + at + close.len());
    Some(format!("{}{}", &text[..open], &text[end..]))
}

/// A tool call as a line: `name: what it works on`, or the name alone.
fn tool_line(name: &str, subject: Option<&str>) -> Option<String> {
    match subject.and_then(first_line) {
        Some(subject) => first_line(&format!("{name}: {subject}")),
        None => first_line(name),
    }
}

/// A file's last two path components: `app/account.rs`.
fn short_path(path: &str) -> String {
    let parts: Vec<_> = Path::new(path)
        .components()
        .rev()
        .take(2)
        .map(|part| part.as_os_str().to_string_lossy().into_owned())
        .collect();
    parts.into_iter().rev().collect::<Vec<_>>().join("/")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn codex_log(path: PathBuf) -> Follower {
        Follower {
            pid: 1,
            source: Source::Log {
                agent: LogAgent::Codex,
                conversation: None,
                path,
                offset: 0,
                partial: Vec::new(),
            },
            latest: None,
            prompt: None,
            turn: Turn::default(),
            reply: None,
            last_at: None,
        }
    }

    #[test]
    fn the_last_message_is_the_newest_prompt_or_agent_message() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("log.jsonl");
        let mut file = File::create(&path).unwrap();
        let event = |at: &str, payload: serde_json::Value| {
            format!(
                "{}\n",
                serde_json::json!({"timestamp": at, "type": "event_msg", "payload": payload})
            )
        };
        let mut follower = codex_log(path);
        file.write_all(
            event(
                "2026-10-22T00:54:42.000Z",
                serde_json::json!({"type": "user_message", "message": "go"}),
            )
            .as_bytes(),
        )
        .unwrap();
        follower.follow(None);
        assert_eq!(follower.last_at(), Some(1_792_630_482_000));
        file.write_all(
            event(
                "2026-10-22T00:55:01.000Z",
                serde_json::json!({"type": "agent_message", "message": "Done."}),
            )
            .as_bytes(),
        )
        .unwrap();
        follower.follow(None);
        assert_eq!(follower.last_at(), Some(1_792_630_501_000));
        // Bookkeeping later on, or an entry that says it's older, isn't a message.
        file.write_all(
            event(
                "2026-10-27T05:35:18.000Z",
                serde_json::json!({"type": "token_count"}),
            )
            .as_bytes(),
        )
        .unwrap();
        file.write_all(
            event(
                "2026-10-21T00:00:00.000Z",
                serde_json::json!({"type": "agent_message", "message": "Old"}),
            )
            .as_bytes(),
        )
        .unwrap();
        follower.follow(None);
        assert_eq!(follower.last_at(), Some(1_792_630_501_000));
    }

    #[test]
    fn the_follower_keeps_the_turns_reply_and_names_its_conversation() {
        let directory = tempfile::tempdir().unwrap();
        let thread = "019a1b2c-3d4e-7f60-8a9b-0c1d2e3f4a5b";
        let path = directory
            .path()
            .join(format!("rollout-2026-10-23T10-00-00-{thread}.jsonl"));
        let mut file = File::create(&path).unwrap();
        let event = |payload: serde_json::Value| {
            format!(
                "{}\n",
                serde_json::json!({"type": "event_msg", "payload": payload})
            )
        };
        file.write_all(
            event(serde_json::json!({"type": "user_message", "message": "go"})).as_bytes(),
        )
        .unwrap();
        let mut follower = codex_log(path);
        follower.follow(None);
        assert_eq!(follower.reply(), None);
        file.write_all(
            event(serde_json::json!({"type": "task_complete", "turn_id": "t1",
                "last_agent_message": "Done: the tests pass."}))
            .as_bytes(),
        )
        .unwrap();
        follower.follow(None);
        let reply = follower.reply().unwrap();
        assert_eq!(
            (reply.key.as_str(), reply.text.as_str()),
            ("t1", "Done: the tests pass.")
        );
        assert_eq!(follower.conversation().as_deref(), Some(thread));
        assert_eq!(follower.agent(), "codex");
    }

    #[test]
    fn a_long_reply_is_cut_at_a_character_boundary() {
        let reply = bounded("k".into(), "é".repeat(MAX_REPLY_BYTES));
        assert!(reply.truncated);
        assert!(reply.text.len() <= MAX_REPLY_BYTES);
        assert!(reply.text.chars().all(|c| c == 'é'));
        assert!(!bounded("k".into(), "short".into()).truncated);
    }

    #[test]
    fn a_first_line_is_plain_and_bounded() {
        assert_eq!(
            first_line("\n\n## **Checking** the `parser`\nmore"),
            Some("Checking the parser".into())
        );
        assert_eq!(first_line("a\u{1b}[31mb\tc"), Some("a [31mb c".into()));
        assert_eq!(first_line("  \n---\n"), None);
        assert_eq!(first_line(&"x".repeat(500)).unwrap().len(), MAX_LINE);
        assert_eq!(short_path("/repo/tui/src/app.rs"), "src/app.rs");
    }

    #[test]
    fn a_prompt_is_what_was_typed() {
        assert_eq!(
            prompt_line("Fix the parser\nthanks"),
            Some("Fix the parser".into())
        );
        assert_eq!(
            prompt_line("<command-name>/review</command-name>\n<command-args>src</command-args>"),
            Some("/review src".into())
        );
        assert_eq!(
            prompt_line("<task-notification>done</task-notification>"),
            None
        );
        assert_eq!(
            prompt_line("<system-reminder>x</system-reminder>\nShip it"),
            Some("Ship it".into())
        );
        assert_eq!(
            prompt_line("<pasted_content id=\"1\">\nerror: boom\n</pasted_content>\nWhy?"),
            Some("Why?".into())
        );
        assert_eq!(
            prompt_line("<pasted_content id=\"1\">\nerror: boom\n</pasted_content>"),
            Some("error: boom".into())
        );
    }

    #[test]
    fn the_follower_reads_new_complete_lines_from_where_it_left_off() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("log.jsonl");
        let mut file = File::create(&path).unwrap();
        let entry = |text: &str| {
            format!(
                "{}\n",
                serde_json::json!({"type":"event_msg","payload":{"type":"agent_message","message":text}})
            )
        };
        file.write_all(entry("First reply").as_bytes()).unwrap();
        let mut follower = codex_log(path.clone());
        assert!(follower.follow(None));
        assert_eq!(follower.latest(), Some("First reply"));
        // Nothing new: unchanged.
        assert!(!follower.follow(None));
        // Half a line waits for the rest.
        let next = entry("Second reply");
        let (head, tail) = next.split_at(10);
        file.write_all(head.as_bytes()).unwrap();
        assert!(!follower.follow(None));
        file.write_all(tail.as_bytes()).unwrap();
        assert!(follower.follow(None));
        assert_eq!(follower.latest(), Some("Second reply"));
        // Lines without anything to show keep the last one.
        file.write_all(b"{\"type\":\"event_msg\",\"payload\":{\"type\":\"token_count\"}}\n")
            .unwrap();
        assert!(!follower.follow(None));
        assert_eq!(follower.latest(), Some("Second reply"));
    }

    #[test]
    fn a_turn_starts_at_each_prompt_and_gathers_the_agents_messages() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("log.jsonl");
        let mut file = File::create(&path).unwrap();
        let event = |kind: &str, key: &str, text: &str| {
            format!(
                "{}\n",
                serde_json::json!({"type":"event_msg","payload":{"type":kind, key:text}})
            )
        };
        let mut follower = codex_log(path.clone());
        file.write_all(event("user_message", "message", "Fix the parser").as_bytes())
            .unwrap();
        file.write_all(event("agent_message", "message", "Looking at it").as_bytes())
            .unwrap();
        assert!(follower.follow(None));
        let first = follower.turn().clone();
        assert_eq!(first.prompt.as_deref(), Some("Fix the parser"));
        assert_eq!(first.messages, ["Looking at it"]);
        assert_eq!(first.total, 1);
        file.write_all(event("agent_message", "message", "Fixed:\n  two lines").as_bytes())
            .unwrap();
        assert!(follower.follow(None));
        assert_eq!(
            follower.turn().messages,
            ["Looking at it", "Fixed: two lines"]
        );
        assert_eq!(follower.turn().total, 2);
        // The next prompt starts the next turn.
        file.write_all(event("user_message", "message", "Now the docs").as_bytes())
            .unwrap();
        assert!(follower.follow(None));
        assert_ne!(follower.turn().id, first.id);
        assert_eq!(follower.turn().prompt.as_deref(), Some("Now the docs"));
        assert_eq!(follower.turn().total, 0);
    }

    #[test]
    fn a_long_log_is_read_from_its_tail() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("log.jsonl");
        let mut file = File::create(&path).unwrap();
        let filler = format!(
            "{}\n",
            serde_json::json!({"type":"x","pad":"y".repeat(1000)})
        );
        for _ in 0..400 {
            file.write_all(filler.as_bytes()).unwrap();
        }
        let last = serde_json::json!({"type":"event_msg","payload":{"type":"agent_message","message":"At the end"}});
        writeln!(file, "{last}").unwrap();
        let mut follower = codex_log(path);
        assert!(follower.follow(None));
        assert_eq!(follower.latest(), Some("At the end"));
    }

    /// A Codex rollout line: an event of `kind` with `message`.
    fn codex_event(kind: &str, message: &str) -> String {
        format!(
            "{}\n",
            serde_json::json!({"type":"event_msg","payload":{"type":kind,"message":message}})
        )
    }

    #[test]
    fn a_long_logs_turn_keeps_its_prompt_from_before_the_tail() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("log.jsonl");
        let mut file = File::create(&path).unwrap();
        file.write_all(codex_event("user_message", "An earlier prompt").as_bytes())
            .unwrap();
        file.write_all(codex_event("agent_message", "An earlier reply").as_bytes())
            .unwrap();
        file.write_all(codex_event("user_message", "Fix the flaky login test").as_bytes())
            .unwrap();
        file.write_all(codex_event("agent_message", "Reading the test").as_bytes())
            .unwrap();
        // Tool output the agent read: more than the tail.
        let output = format!(
            "{}\n",
            serde_json::json!({"type":"response_item",
                "payload":{"type":"function_call_output","output":"y".repeat(1000)}})
        );
        for _ in 0..400 {
            file.write_all(output.as_bytes()).unwrap();
        }
        file.write_all(codex_event("agent_message", "Fixed: it waits for the form").as_bytes())
            .unwrap();
        // A new follower (the daemon restarted in place, say) reads the tail,
        // and the turn it's in from its prompt.
        let mut follower = codex_log(path);
        assert!(follower.follow(None));
        let turn = follower.turn().clone();
        assert_eq!(turn.prompt.as_deref(), Some("Fix the flaky login test"));
        assert_eq!(
            turn.messages,
            ["Reading the test", "Fixed: it waits for the form"]
        );
        assert_eq!(turn.total, 2);
        assert_eq!(follower.prompt(), Some("Fix the flaky login test"));
        // Read once: nothing new is nothing more.
        assert!(!follower.follow(None));
        assert_eq!(follower.turn(), &turn);
    }

    #[test]
    fn a_tail_with_its_prompt_is_read_alone() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("log.jsonl");
        let mut file = File::create(&path).unwrap();
        file.write_all(codex_event("user_message", "An earlier prompt").as_bytes())
            .unwrap();
        for _ in 0..400 {
            file.write_all(codex_event("agent_message", &"y".repeat(1000)).as_bytes())
                .unwrap();
        }
        file.write_all(codex_event("user_message", "Now the docs").as_bytes())
            .unwrap();
        file.write_all(codex_event("agent_message", "Writing them").as_bytes())
            .unwrap();
        let mut follower = codex_log(path);
        assert!(follower.follow(None));
        assert_eq!(follower.turn().prompt.as_deref(), Some("Now the docs"));
        assert_eq!(follower.turn().messages, ["Writing them"]);
        // Only the tail was read: the earlier turn's messages weren't seen.
        assert_eq!(follower.turn().id, 1);
    }

    #[test]
    fn a_tail_without_its_prompt_nearby_is_a_turn_without_one() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("log.jsonl");
        let mut file = File::create(&path).unwrap();
        file.write_all(codex_event("user_message", "Too far back").as_bytes())
            .unwrap();
        let output = format!(
            "{}\n",
            serde_json::json!({"type":"response_item",
                "payload":{"type":"function_call_output","output":"y".repeat(64 * 1024)}})
        );
        let lines = (MAX_PROMPT_SEARCH + MAX_READ) / output.len() as u64 + 2;
        for _ in 0..lines {
            file.write_all(output.as_bytes()).unwrap();
        }
        file.write_all(codex_event("agent_message", "Done").as_bytes())
            .unwrap();
        let mut follower = codex_log(path);
        assert!(follower.follow(None));
        assert_eq!(follower.turn().prompt, None);
        assert_eq!(follower.turn().messages, ["Done"]);
        assert!(!follower.follow(None));
        assert_eq!(follower.turn().total, 1);
    }
}
