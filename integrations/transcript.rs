//! A coding agent's whole conversation, for the agent feed: what the user
//! typed, the agent's text, reasoning and tool calls, and the tools' output,
//! as [`TranscriptMessage`]s in order (`session_transcript`). Read on demand
//! from the agent's own log or database after a cursor; nothing is kept.
//!
//! Provider mappings live beside their hooks in `integrations/<agent>`:
//! - **Claude Code**: one message per transcript entry, at its byte offset;
//!   tool results are the `tool` role's.
//! - **Codex**: one message per rollout line, at its byte offset: the user's
//!   messages from their events, everything else from the response items.
//! - **OpenCode**: OpenCode 2's `session_message` rows (one message per
//!   block of a reply), else 1.x's `part` rows, changed since the cursor.
//!
//! Text is cut at [`MAX_PART_BYTES`] per part and otherwise left as the
//! agent wrote it: the caller cleans it before it leaves the machine.

use super::LogAgent;
use serde_json::Value;
use session_core::api::{
    TranscriptCursor, TranscriptMessage, TranscriptPage, TranscriptPart, TranscriptRole, MAX_PARTS,
    MAX_PART_BYTES, MAX_TRANSCRIPT_BYTES, TRANSCRIPT_BYTES,
};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

#[path = "claude-code/transcript.rs"]
mod claude;
#[path = "codex/transcript.rs"]
mod codex;
#[path = "open-code/transcript.rs"]
mod opencode;

/// The longest log line read; a longer one (an image, a huge tool output)
/// is passed over.
const MAX_LINE_BYTES: usize = 8 * 1024 * 1024;
/// How much of a log is read at a time.
const CHUNK: usize = 64 * 1024;

/// Where a conversation is read from ([`super::Follower::transcript_source`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TranscriptSource {
    /// An append-only log of JSON lines.
    Log {
        agent: LogAgent,
        conversation: String,
        path: PathBuf,
    },
    /// OpenCode's database.
    OpenCode {
        database: PathBuf,
        conversation: String,
    },
}

impl TranscriptSource {
    /// `claude-code`, `codex` or `opencode`.
    pub fn agent(&self) -> &'static str {
        match self {
            TranscriptSource::Log {
                agent: LogAgent::Claude,
                ..
            } => "claude-code",
            TranscriptSource::Log {
                agent: LogAgent::Codex,
                ..
            } => "codex",
            TranscriptSource::OpenCode { .. } => "opencode",
        }
    }

    fn conversation(&self) -> &str {
        match self {
            TranscriptSource::Log { conversation, .. }
            | TranscriptSource::OpenCode { conversation, .. } => conversation,
        }
    }

    /// The messages after `after` (from the start when it's another
    /// conversation's), reading about `max_bytes` at most. `None` when the
    /// log or database can't be read.
    pub fn read(
        &self,
        after: Option<&TranscriptCursor>,
        max_bytes: Option<u32>,
    ) -> Option<TranscriptPage> {
        let budget = max_bytes
            .unwrap_or(TRANSCRIPT_BYTES)
            .clamp(1, MAX_TRANSCRIPT_BYTES) as usize;
        let conversation = self.conversation();
        let after = after.filter(|cursor| cursor.conversation == conversation);
        let (messages, cursor, more) = match self {
            TranscriptSource::Log { agent, path, .. } => {
                let from = after.map_or(0, |cursor| cursor.offset);
                let (messages, offset, more) = read_log(*agent, path, from, budget)?;
                let cursor = TranscriptCursor {
                    conversation: conversation.to_owned(),
                    offset,
                    key: None,
                };
                (messages, cursor, more)
            }
            TranscriptSource::OpenCode { database, .. } => {
                let watermark = after.map(|cursor| (cursor.offset, cursor.key.clone()));
                let (messages, (offset, key), more) =
                    opencode::read(database, conversation, watermark, budget)?;
                let cursor = TranscriptCursor {
                    conversation: conversation.to_owned(),
                    offset,
                    key,
                };
                (messages, cursor, more)
            }
        };
        Some(TranscriptPage {
            agent: self.agent().to_owned(),
            conversation_id: conversation.to_owned(),
            messages,
            cursor,
            more,
        })
    }
}

/// The messages of the complete lines of `path` from `from` (its start
/// when the file is shorter, as after a rewrite), until about `budget`
/// bytes were read. Returns them, where the next unread line starts, and
/// whether more lines were there.
fn read_log(
    agent: LogAgent,
    path: &Path,
    from: u64,
    budget: usize,
) -> Option<(Vec<TranscriptMessage>, u64, bool)> {
    let mut file = File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    let from = if from > len { 0 } else { from };
    file.seek(SeekFrom::Start(from)).ok()?;
    let mut messages = Vec::new();
    // Where the line being read starts, and its bytes so far (none while a
    // too long one is passed over).
    let mut start = from;
    let mut line = Vec::new();
    let mut skipping = false;
    let mut read = 0usize;
    let mut at = from;
    let mut chunk = vec![0u8; CHUNK];
    loop {
        if read >= budget {
            return Some((messages, start, start < len));
        }
        let count = match file.read(&mut chunk) {
            Ok(0) | Err(_) => return Some((messages, start, false)),
            Ok(count) => count,
        };
        let mut rest = &chunk[..count];
        while let Some(end) = rest.iter().position(|b| *b == b'\n') {
            if !skipping {
                line.extend_from_slice(&rest[..end]);
                if let Ok(entry) = serde_json::from_slice::<Value>(&line) {
                    let message = match agent {
                        LogAgent::Claude => claude::message(&entry, start),
                        LogAgent::Codex => codex::message(&entry, start),
                    };
                    messages.extend(message);
                }
            }
            at += end as u64 + 1;
            read += line.len() + 1;
            start = at;
            line.clear();
            skipping = false;
            rest = &rest[end + 1..];
            if read >= budget {
                return Some((messages, start, start < len));
            }
        }
        at += rest.len() as u64;
        if !skipping {
            line.extend_from_slice(rest);
            if line.len() > MAX_LINE_BYTES {
                line.clear();
                skipping = true;
            }
        }
    }
}

/// A message of `parts` (at most [`MAX_PARTS`]), when it has some. `id` is
/// the agent's own when it's one an id can be, else the position.
fn assemble(
    id: Option<&str>,
    position: u64,
    role: TranscriptRole,
    at: Option<i64>,
    mut parts: Vec<TranscriptPart>,
) -> Option<TranscriptMessage> {
    if parts.is_empty() {
        return None;
    }
    parts.truncate(MAX_PARTS);
    let id = id
        .filter(|id| is_id(id))
        .map_or_else(|| position.to_string(), String::from);
    Some(TranscriptMessage {
        id,
        position,
        role,
        at,
        parts,
    })
}

/// Whether `id` can name a message: 1–128 of letters, digits and `._:-`,
/// starting with a letter or digit.
fn is_id(id: &str) -> bool {
    id.len() <= 128
        && id.starts_with(|c: char| c.is_ascii_alphanumeric())
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | ':' | '-'))
}

/// `text` at most [`MAX_PART_BYTES`], cut at a character boundary, and
/// whether it was cut.
fn cut(text: &str) -> (String, bool) {
    if text.len() <= MAX_PART_BYTES {
        return (text.to_owned(), false);
    }
    let mut end = MAX_PART_BYTES;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    (text[..end].to_owned(), true)
}

/// Text that says something, as a part.
fn text(text: &str) -> Option<TranscriptPart> {
    let (text, truncated) = cut(text.trim());
    (!text.is_empty()).then_some(TranscriptPart::Text { text, truncated })
}

/// Visible reasoning, as a part.
fn reasoning(text: &str) -> Option<TranscriptPart> {
    let (text, truncated) = cut(text.trim());
    (!text.is_empty()).then_some(TranscriptPart::Reasoning { text, truncated })
}

/// A tool call, as a part.
fn call(call_id: Option<&str>, name: &str, input: &str) -> Option<TranscriptPart> {
    let name = name.trim();
    if name.is_empty() {
        return None;
    }
    let (input, truncated) = cut(input);
    Some(TranscriptPart::ToolCall {
        call_id: call_id.filter(|id| is_call_id(id)).map(String::from),
        name: name.chars().take(128).collect(),
        input,
        truncated,
    })
}

/// A tool's output, as a part.
fn result(call_id: Option<&str>, output: &str, error: bool) -> TranscriptPart {
    let (output, truncated) = cut(output);
    TranscriptPart::ToolResult {
        call_id: call_id.filter(|id| is_call_id(id)).map(String::from),
        output,
        error,
        truncated,
    }
}

/// Whether `id` can name a tool call: as [`is_id`], `|` too.
fn is_call_id(id: &str) -> bool {
    is_id(&id.replace('|', ":"))
}

/// JSON as one compact line (a tool call's input).
fn compact(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// An RFC 3339 time (`2026-10-23T10:00:00.123Z`, or with an offset) as Unix
/// milliseconds.
fn timestamp(text: &str) -> Option<i64> {
    let bytes = text.as_bytes();
    let number = |from: usize, to: usize| -> Option<i64> {
        let digits = text.get(from..to)?;
        digits
            .bytes()
            .all(|b| b.is_ascii_digit())
            .then(|| digits.parse().ok())?
    };
    if bytes.len() < 20 || bytes[4] != b'-' || bytes[7] != b'-' || bytes[13] != b':' {
        return None;
    }
    let (year, month, day) = (number(0, 4)?, number(5, 7)?, number(8, 10)?);
    let (hour, minute, second) = (number(11, 13)?, number(14, 16)?, number(17, 19)?);
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) || hour > 23 || minute > 59 {
        return None;
    }
    let mut rest = &text[19..];
    let mut millis = 0;
    if let Some(fraction) = rest.strip_prefix('.') {
        let digits = fraction.bytes().take_while(u8::is_ascii_digit).count();
        let padded: String = fraction[..digits]
            .chars()
            .chain("000".chars())
            .take(3)
            .collect();
        millis = padded.parse().ok()?;
        rest = &fraction[digits..];
    }
    let offset = match rest.as_bytes() {
        [b'Z' | b'z'] => 0,
        [sign @ (b'+' | b'-'), h1, h2, b':', m1, m2] => {
            let digit = |b: &u8| b.is_ascii_digit().then(|| i64::from(b - b'0'));
            let minutes = (digit(h1)? * 10 + digit(h2)?) * 60 + digit(m1)? * 10 + digit(m2)?;
            if *sign == b'-' {
                -minutes
            } else {
                minutes
            }
        }
        _ => return None,
    };
    // Days since 1970-01-01 (Howard Hinnant's days_from_civil).
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    let seconds = days * 86_400 + hour * 3_600 + minute * 60 + second - offset * 60;
    Some(seconds * 1_000 + millis)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn times_are_unix_milliseconds() {
        assert_eq!(timestamp("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(
            timestamp("2026-10-23T10:00:00.123Z"),
            Some(1_792_749_600_123)
        );
        assert_eq!(timestamp("2026-10-23T10:00:00.5Z"), Some(1_792_749_600_500));
        assert_eq!(
            timestamp("2026-10-23T12:00:00+02:00"),
            Some(1_792_749_600_000)
        );
        assert_eq!(timestamp("2026-10-23 10:00"), None);
        assert_eq!(timestamp("yesterday"), None);
    }

    #[test]
    fn long_text_is_cut_at_a_character_boundary() {
        let (short, cut_short) = cut("fine");
        assert_eq!((short.as_str(), cut_short), ("fine", false));
        let long = "é".repeat(MAX_PART_BYTES);
        let (text, truncated) = cut(&long);
        assert!(truncated);
        assert!(text.len() <= MAX_PART_BYTES && text.len() >= MAX_PART_BYTES - 1);
    }

    #[test]
    fn ids_that_cant_name_a_message_become_its_position() {
        let parts = || {
            vec![TranscriptPart::Text {
                text: "hi".into(),
                truncated: false,
            }]
        };
        let named = assemble(
            Some("6c0d7a52-9b1e"),
            7,
            TranscriptRole::User,
            None,
            parts(),
        );
        assert_eq!(named.unwrap().id, "6c0d7a52-9b1e");
        let unnamed = assemble(Some("a/b"), 7, TranscriptRole::User, None, parts());
        assert_eq!(unnamed.unwrap().id, "7");
        assert_eq!(
            assemble(None, 7, TranscriptRole::User, None, Vec::new()),
            None
        );
    }

    fn claude_log(lines: &[Value]) -> (tempfile::TempDir, TranscriptSource) {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("conversation.jsonl");
        let mut file = File::create(&path).unwrap();
        for line in lines {
            writeln!(file, "{line}").unwrap();
        }
        let source = TranscriptSource::Log {
            agent: LogAgent::Claude,
            conversation: "c1".into(),
            path,
        };
        (directory, source)
    }

    fn typed(id: &str, text: &str) -> Value {
        serde_json::json!({"type": "user", "uuid": id,
            "message": {"role": "user", "content": text}})
    }

    #[test]
    fn a_log_is_read_on_from_its_cursor_in_pieces() {
        let lines: Vec<Value> = (0..5)
            .map(|n| typed(&format!("u{n}"), &format!("prompt {n}")))
            .collect();
        let (directory, source) = claude_log(&lines);
        let page = source.read(None, None).unwrap();
        assert_eq!(page.agent, "claude-code");
        assert_eq!(page.conversation_id, "c1");
        assert_eq!(page.messages.len(), 5);
        assert!(!page.more);
        assert_eq!(page.messages[0].position, 0);
        let second = lines[0].to_string().len() as u64 + 1;
        assert_eq!(page.messages[1].position, second);
        // Nothing new: nothing read.
        let again = source.read(Some(&page.cursor), None).unwrap();
        assert!(again.messages.is_empty());
        assert_eq!(again.cursor, page.cursor);
        // A small budget reads a line or two at a time, and says there's more.
        let first = source.read(None, Some(1)).unwrap();
        assert_eq!(first.messages.len(), 1);
        assert!(first.more);
        assert_eq!(first.cursor.offset, second);
        let next = source.read(Some(&first.cursor), Some(1)).unwrap();
        assert_eq!(next.messages[0].id, "u1");
        // A line still being written waits; another conversation's cursor
        // reads this one from the start.
        let path = match &source {
            TranscriptSource::Log { path, .. } => path.clone(),
            _ => unreachable!(),
        };
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        write!(file, "{{\"type\": \"user\"").unwrap();
        let end = source.read(Some(&page.cursor), None).unwrap();
        assert!(end.messages.is_empty());
        assert_eq!(end.cursor.offset, page.cursor.offset);
        let other = TranscriptCursor {
            conversation: "c0".into(),
            ..page.cursor.clone()
        };
        assert_eq!(source.read(Some(&other), None).unwrap().messages.len(), 5);
        drop(directory);
    }

    #[test]
    fn a_line_too_long_to_read_is_passed_over() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("conversation.jsonl");
        let mut file = File::create(&path).unwrap();
        let huge = "x".repeat(MAX_LINE_BYTES + CHUNK);
        writeln!(
            file,
            "{}",
            serde_json::json!({"type": "user", "uuid": "u1", "message": {"content": huge}})
        )
        .unwrap();
        writeln!(file, "{}", typed("u-after", "after")).unwrap();
        let source = TranscriptSource::Log {
            agent: LogAgent::Claude,
            conversation: "c1".into(),
            path,
        };
        let mut cursor = None;
        let mut messages = Vec::new();
        for _ in 0..100 {
            let page = source
                .read(cursor.as_ref(), Some(MAX_TRANSCRIPT_BYTES))
                .unwrap();
            messages.extend(page.messages);
            cursor = Some(page.cursor);
            if !page.more {
                break;
            }
        }
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].id, "u-after");
    }
}
