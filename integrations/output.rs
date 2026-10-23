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

use crate::host::ProcessInfo;
use serde_json::Value;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

#[path = "claude-code/output.rs"]
mod claude;
#[path = "codex/output.rs"]
mod codex;
#[path = "open-code/output.rs"]
mod opencode;

/// The most of a log read at once: a log that grew more is read from its tail.
const MAX_READ: u64 = 256 * 1024;
/// The longest line kept, in characters.
pub const MAX_LINE: usize = 240;

/// An agent whose log is an append-only file of JSON lines.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LogAgent {
    Claude,
    Codex,
}

/// Where an agent's output is read from.
#[derive(Debug)]
enum Source {
    /// An append-only log, read from where it was last read.
    Log {
        agent: LogAgent,
        /// Claude Code's conversation, checked on every read: the process
        /// can clear it, resume another or park it in a background job.
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
    },
}

/// Follows one agent process's conversation.
#[derive(Debug)]
pub struct Follower {
    pid: u32,
    source: Source,
    latest: Option<String>,
    prompt: Option<String>,
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
            "opencode" | "opencode2" => Source::OpenCode {
                database: crate::resume::opencode_database()?,
                cwd: crate::procinfo::cwd_of(process.pid),
            },
            _ => return None,
        };
        Some(Self {
            pid: process.pid,
            source,
            latest: None,
            prompt: None,
        })
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

    /// Read what the conversation gained since the last time (a log from its
    /// tail the first time). `bound` is the OpenCode conversation bound to
    /// the session, if any. Returns whether either line changed.
    pub fn follow(&mut self, bound: Option<&str>) -> bool {
        let (line, prompt) = match &mut self.source {
            Source::Log {
                agent,
                conversation,
                path,
                offset,
                partial,
            } => {
                let agent = *agent;
                if agent == LogAgent::Claude {
                    switch_conversation(self.pid, conversation, path, offset, partial);
                }
                let Some(lines) = read(path, offset, partial) else {
                    return false;
                };
                let entries: Vec<Value> = lines
                    .iter()
                    .filter_map(|line| serde_json::from_slice(line).ok())
                    .collect();
                let newest =
                    |line: fn(&Value) -> Option<String>| entries.iter().rev().find_map(line);
                match agent {
                    LogAgent::Claude => (newest(claude::line), newest(claude::prompt)),
                    LogAgent::Codex => (newest(codex::line), newest(codex::prompt)),
                }
            }
            Source::OpenCode { database, cwd } => {
                let conversation = match bound {
                    Some(bound) => Some(bound.to_string()),
                    None => cwd
                        .as_deref()
                        .and_then(crate::resume::opencode_recent_session),
                };
                match conversation.filter(|id| crate::resume::is_conversation_id(id)) {
                    Some(id) => (
                        opencode::latest(database, &id),
                        opencode::latest_prompt(database, &id),
                    ),
                    None => (None, None),
                }
            }
        };
        // A read with nothing new keeps what was there.
        let mut changed = false;
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
/// new transcript is read from its tail.
fn switch_conversation(
    pid: u32,
    conversation: &mut Option<String>,
    path: &mut PathBuf,
    offset: &mut u64,
    partial: &mut Vec<u8>,
) {
    let Some(config) = crate::resume::claude_config_dir() else {
        return;
    };
    let Some(current) = claude::conversation(&config, pid) else {
        return;
    };
    if conversation.as_deref() == Some(current.as_str()) {
        return;
    }
    if let Some(transcript) = claude::transcript(&config, &current) {
        *path = transcript;
        *offset = 0;
        partial.clear();
        *conversation = Some(current);
    }
}

/// The complete lines `path` gained since `offset` (moved past them); the
/// start of an unfinished one waits in `partial`.
fn read(path: &Path, offset: &mut u64, partial: &mut Vec<u8>) -> Option<Vec<Vec<u8>>> {
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
    let start = if skipped { len - MAX_READ } else { *offset };
    file.seek(SeekFrom::Start(start)).ok()?;
    let mut bytes = Vec::new();
    file.take(len - start).read_to_end(&mut bytes).ok()?;
    *offset = start + bytes.len() as u64;
    let mut data = if skipped {
        partial.clear();
        // Mid-line: from the next one.
        match bytes.iter().position(|b| *b == b'\n') {
            Some(at) => bytes.split_off(at + 1),
            None => Vec::new(),
        }
    } else {
        let mut data = std::mem::take(partial);
        data.extend_from_slice(&bytes);
        data
    };
    let complete = data
        .iter()
        .rposition(|b| *b == b'\n')
        .map_or(0, |at| at + 1);
    let rest = data.split_off(complete);
    if (rest.len() as u64) <= MAX_READ {
        *partial = rest;
    }
    Some(
        data.split(|b| *b == b'\n')
            .filter(|line| !line.is_empty())
            .map(<[u8]>::to_vec)
            .collect(),
    )
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
    if let Some(command) = tagged(text, "command-name") {
        let args = tagged(text, "command-args").unwrap_or_default();
        return first_line(&format!("{} {}", command.trim(), args.trim()));
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
    first_line(&typed).or_else(|| paste.as_deref().and_then(first_line))
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
        }
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
}
