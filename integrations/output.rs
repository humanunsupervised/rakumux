//! The latest thing a coding agent said or did, as one line for the session
//! list: its reasoning, a tool call, or its reply. Read from the agent's own
//! conversation log on this machine as the file grows; prompts and tool
//! output are never taken, and nothing leaves the daemon but the line.
//!
//! Provider implementations live beside their hooks in `integrations/<agent>`:
//! - **Claude Code**: `<config dir>/projects/*/<conversation>.jsonl`, the
//!   conversation being the one in `<config dir>/sessions/<pid>.json`.
//! - **Codex**: the `rollout-…-<thread>.jsonl` file the process keeps open.

use crate::host::ProcessInfo;
use serde_json::Value;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;

#[path = "claude-code/output.rs"]
mod claude;
#[path = "codex/output.rs"]
mod codex;

/// The most of a log read at once: a log that grew more is read from its tail.
const MAX_READ: u64 = 256 * 1024;
/// The longest line kept, in characters.
pub const MAX_LINE: usize = 240;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Agent {
    Claude,
    Codex,
}

/// Follows one agent process's conversation log.
#[derive(Debug)]
pub struct Follower {
    pid: u32,
    agent: Agent,
    path: PathBuf,
    /// How far the log has been read.
    offset: u64,
    /// The start of a line not finished yet.
    partial: Vec<u8>,
    latest: Option<String>,
}

impl Follower {
    /// The log of `process`, when it runs `agent` (as detected, `claude` or
    /// `codex`, or as its hooks name it) and the log can be found.
    pub fn locate(process: &ProcessInfo, agent: &str) -> Option<Self> {
        let (agent, path) = match agent {
            "claude" | "claude-code" => (Agent::Claude, claude::log(process)?),
            "codex" => (Agent::Codex, codex::log(process)?),
            _ => return None,
        };
        Some(Self {
            pid: process.pid,
            agent,
            path,
            offset: 0,
            partial: Vec::new(),
            latest: None,
        })
    }

    /// The process whose log this is.
    pub fn pid(&self) -> u32 {
        self.pid
    }

    /// The latest line, once one was found.
    pub fn latest(&self) -> Option<&str> {
        self.latest.as_deref()
    }

    /// Read what the log gained since the last time (from its tail the
    /// first time). Returns whether the latest line changed.
    pub fn follow(&mut self) -> bool {
        let Some(lines) = self.read() else {
            return false;
        };
        let line = lines.iter().rev().find_map(|line| {
            let entry: Value = serde_json::from_slice(line).ok()?;
            match self.agent {
                Agent::Claude => claude::line(&entry),
                Agent::Codex => codex::line(&entry),
            }
        });
        match line {
            Some(line) if self.latest.as_deref() != Some(line.as_str()) => {
                self.latest = Some(line);
                true
            }
            _ => false,
        }
    }

    /// The complete lines added since the last read.
    fn read(&mut self) -> Option<Vec<Vec<u8>>> {
        let mut file = File::open(&self.path).ok()?;
        let len = file.metadata().ok()?.len();
        if len < self.offset {
            // Rewritten: start again.
            self.offset = 0;
            self.partial.clear();
        }
        if len == self.offset {
            return Some(Vec::new());
        }
        let skipped = len - self.offset > MAX_READ;
        let start = if skipped { len - MAX_READ } else { self.offset };
        file.seek(SeekFrom::Start(start)).ok()?;
        let mut bytes = Vec::new();
        file.take(len - start).read_to_end(&mut bytes).ok()?;
        self.offset = start + bytes.len() as u64;
        let mut data = if skipped {
            self.partial.clear();
            // Mid-line: from the next one.
            match bytes.iter().position(|b| *b == b'\n') {
                Some(at) => bytes.split_off(at + 1),
                None => Vec::new(),
            }
        } else {
            let mut data = std::mem::take(&mut self.partial);
            data.extend_from_slice(&bytes);
            data
        };
        let complete = data
            .iter()
            .rposition(|b| *b == b'\n')
            .map_or(0, |at| at + 1);
        let rest = data.split_off(complete);
        if (rest.len() as u64) <= MAX_READ {
            self.partial = rest;
        }
        Some(
            data.split(|b| *b == b'\n')
                .filter(|line| !line.is_empty())
                .map(<[u8]>::to_vec)
                .collect(),
        )
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

/// A tool call as a line: `name: what it works on`, or the name alone.
fn tool_line(name: &str, subject: Option<&str>) -> Option<String> {
    match subject.and_then(first_line) {
        Some(subject) => first_line(&format!("{name}: {subject}")),
        None => first_line(name),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn a_first_line_is_plain_and_bounded() {
        assert_eq!(
            first_line("\n\n## **Checking** the `parser`\nmore"),
            Some("Checking the parser".into())
        );
        assert_eq!(first_line("a\u{1b}[31mb\tc"), Some("a [31mb c".into()));
        assert_eq!(first_line("  \n---\n"), None);
        assert_eq!(first_line(&"x".repeat(500)).unwrap().len(), MAX_LINE);
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
        let mut follower = Follower {
            pid: 1,
            agent: Agent::Codex,
            path: path.clone(),
            offset: 0,
            partial: Vec::new(),
            latest: None,
        };
        assert!(follower.follow());
        assert_eq!(follower.latest(), Some("First reply"));
        // Nothing new: unchanged.
        assert!(!follower.follow());
        // Half a line waits for the rest.
        let next = entry("Second reply");
        let (head, tail) = next.split_at(10);
        file.write_all(head.as_bytes()).unwrap();
        assert!(!follower.follow());
        file.write_all(tail.as_bytes()).unwrap();
        assert!(follower.follow());
        assert_eq!(follower.latest(), Some("Second reply"));
        // Lines without anything to show keep the last one.
        file.write_all(b"{\"type\":\"event_msg\",\"payload\":{\"type\":\"token_count\"}}\n")
            .unwrap();
        assert!(!follower.follow());
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
        let mut follower = Follower {
            pid: 1,
            agent: Agent::Codex,
            path,
            offset: 0,
            partial: Vec::new(),
            latest: None,
        };
        assert!(follower.follow());
        assert_eq!(follower.latest(), Some("At the end"));
    }
}
