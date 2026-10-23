//! The latest thing a coding agent said or did, as one line for the session
//! list: its reasoning, a tool call, or its reply. Read from the agent's own
//! conversation on this machine as it grows; prompts and tool output are
//! never taken, and nothing leaves the daemon but the line.
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
}

impl Follower {
    /// Where `process`'s output can be read, when it runs `agent` (as
    /// detected, `claude`, `codex` or `opencode`, or as its hooks name it).
    pub fn locate(process: &ProcessInfo, agent: &str) -> Option<Self> {
        let log = |agent, path| Source::Log {
            agent,
            path,
            offset: 0,
            partial: Vec::new(),
        };
        let source = match agent {
            "claude" | "claude-code" => log(LogAgent::Claude, claude::log(process)?),
            "codex" => log(LogAgent::Codex, codex::log(process)?),
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
        })
    }

    /// The process whose conversation this is.
    pub fn pid(&self) -> u32 {
        self.pid
    }

    /// The latest line, once one was found.
    pub fn latest(&self) -> Option<&str> {
        self.latest.as_deref()
    }

    /// Read what the agent added since the last time (a log from its tail
    /// the first time). `bound` is the OpenCode conversation bound to the
    /// session, if any. Returns whether the latest line changed.
    pub fn follow(&mut self, bound: Option<&str>) -> bool {
        let line = match &mut self.source {
            Source::Log {
                agent,
                path,
                offset,
                partial,
            } => {
                let agent = *agent;
                let Some(lines) = read(path, offset, partial) else {
                    return false;
                };
                lines.iter().rev().find_map(|line| {
                    let entry: Value = serde_json::from_slice(line).ok()?;
                    match agent {
                        LogAgent::Claude => claude::line(&entry),
                        LogAgent::Codex => codex::line(&entry),
                    }
                })
            }
            Source::OpenCode { database, cwd } => {
                let conversation = match bound {
                    Some(bound) => Some(bound.to_string()),
                    None => cwd
                        .as_deref()
                        .and_then(crate::resume::opencode_recent_session),
                };
                conversation
                    .filter(|id| crate::resume::is_conversation_id(id))
                    .and_then(|id| opencode::latest(database, &id))
            }
        };
        match line {
            Some(line) if self.latest.as_deref() != Some(line.as_str()) => {
                self.latest = Some(line);
                true
            }
            _ => false,
        }
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
                path,
                offset: 0,
                partial: Vec::new(),
            },
            latest: None,
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
