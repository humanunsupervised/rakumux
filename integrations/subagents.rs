//! A conversation's subagents, for `session_transcript` with `subagents`:
//! the agents its agent started with a tool call, each working in a
//! conversation of its own, read beside the main one and marked with their
//! [`TranscriptSubagent`].
//!
//! - **Claude Code**: `<conversation>/subagents/agent-<id>.jsonl` beside the
//!   main transcript, with the call that started it, its description and
//!   agent type in `agent-<id>.meta.json` (subagents a subagent started too).
//! - **OpenCode**: the conversation's child sessions (and theirs), linked to
//!   the `task` call that names them.
//!
//! Each subagent is read from where its own [`SubagentCursor`] stopped, after
//! the main conversation and within what's left of the request's budget,
//! the least recently changed first. Its messages' ids are its id and the
//! agent's own (unique in the conversation), their parts cut at
//! [`MAX_SUBAGENT_PART_BYTES`], and once a subagent gave
//! [`MAX_SUBAGENT_BYTES`] of text, nothing more of it is read.

use super::{opencode, read_log, LogAgent, Reading, TranscriptSource};
use session_core::api::{
    SubagentCursor, TranscriptMessage, TranscriptPart, TranscriptSubagent, MAX_SUBAGENTS,
    MAX_SUBAGENT_BYTES, MAX_SUBAGENT_PART_BYTES,
};
use std::collections::HashMap;
use std::path::PathBuf;

/// How deep OpenCode's child sessions are followed (a subagent's subagents).
const OPENCODE_DEPTH: usize = 3;
/// The longest label kept, in characters.
const LABEL_CHARS: usize = 120;

/// One subagent of a conversation, and where its own conversation is.
pub(super) struct Subagent {
    marker: TranscriptSubagent,
    /// When it last changed (Unix milliseconds): the most recent are kept.
    changed: u64,
    source: Source,
}

enum Source {
    /// Its own log of JSON lines.
    Log { agent: LogAgent, path: PathBuf },
    /// An OpenCode child session.
    OpenCode {
        database: PathBuf,
        conversation: String,
    },
}

/// The subagents of `source`'s conversation, at most [`MAX_SUBAGENTS`] of
/// the most recently changed, the least recently changed first.
pub(super) fn list(source: &TranscriptSource) -> Vec<Subagent> {
    let mut found = match source {
        TranscriptSource::Log {
            agent: LogAgent::Claude,
            path,
            ..
        } => claude(path),
        TranscriptSource::OpenCode {
            database,
            conversation,
        } => opencode::children(database, conversation, OPENCODE_DEPTH, MAX_SUBAGENTS)
            .into_iter()
            .filter(|child| super::is_id(&child.id))
            .map(|child| Subagent {
                marker: TranscriptSubagent {
                    id: child.id.clone(),
                    call_id: child.call_id.filter(|id| super::is_call_id(id)),
                    label: child.label.as_deref().and_then(label),
                },
                changed: child.updated,
                source: Source::OpenCode {
                    database: database.clone(),
                    conversation: child.id,
                },
            })
            .collect(),
        TranscriptSource::Log { .. } => Vec::new(),
    };
    found.sort_by(|a, b| b.changed.cmp(&a.changed));
    found.truncate(MAX_SUBAGENTS);
    found.reverse();
    found
}

/// Claude Code's subagents of the conversation whose transcript is `path`:
/// `agent-<id>.jsonl` in `<conversation>/subagents/`.
fn claude(path: &std::path::Path) -> Vec<Subagent> {
    let Some(directory) = path
        .file_stem()
        .map(|stem| path.with_file_name(stem).join("subagents"))
    else {
        return Vec::new();
    };
    let Ok(entries) = std::fs::read_dir(&directory) else {
        return Vec::new();
    };
    entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let name = entry.file_name();
            let id = name
                .to_str()?
                .strip_prefix("agent-")?
                .strip_suffix(".jsonl")?
                .to_owned();
            if !super::is_id(&id) {
                return None;
            }
            let metadata = entry.metadata().ok().filter(|meta| meta.is_file())?;
            let changed = metadata
                .modified()
                .ok()
                .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                .map_or(0, |since| since.as_millis() as u64);
            let meta = directory.join(format!("agent-{id}.meta.json"));
            let (call_id, described) = super::claude::subagent_meta(&meta).unwrap_or_default();
            Some(Subagent {
                marker: TranscriptSubagent {
                    id,
                    call_id: call_id.filter(|id| super::is_call_id(id)),
                    label: described.as_deref().and_then(label),
                },
                changed,
                source: Source::Log {
                    agent: LogAgent::Claude,
                    path: entry.path(),
                },
            })
        })
        .collect()
}

/// `text` as a label: its first line, trimmed, at most [`LABEL_CHARS`]
/// characters, without controls.
fn label(text: &str) -> Option<String> {
    let line: String = text
        .lines()
        .next()?
        .chars()
        .filter(|c| !c.is_control())
        .take(LABEL_CHARS)
        .collect();
    let line = line.trim();
    (!line.is_empty()).then(|| line.to_owned())
}

/// The calls that started `listed`, by the id their agent's notices name
/// them with, for [`Reading::subagents`].
pub(super) fn started(listed: &[Subagent]) -> HashMap<String, Option<String>> {
    listed
        .iter()
        .map(|subagent| (subagent.marker.id.clone(), subagent.marker.call_id.clone()))
        .collect()
}

/// What reading the subagents gave.
pub(super) struct Read {
    pub messages: Vec<TranscriptMessage>,
    /// Where each listed subagent is read to now.
    pub cursors: Vec<SubagentCursor>,
    /// Some weren't read to their end: ask again.
    pub more: bool,
}

/// The messages of `listed` after where `cursors` say each was read to,
/// reading about `budget` bytes in all.
pub(super) fn read(
    listed: &[Subagent],
    cursors: &[SubagentCursor],
    budget: usize,
    main: &Reading,
) -> Read {
    let reading = Reading {
        subagent: true,
        subagents: main.subagents,
    };
    let mut left = budget;
    let mut out = Read {
        messages: Vec::new(),
        cursors: Vec::new(),
        more: false,
    };
    for subagent in listed {
        let id = &subagent.marker.id;
        let mut cursor = cursors
            .iter()
            .find(|cursor| cursor.id == *id)
            .cloned()
            .unwrap_or_else(|| SubagentCursor {
                id: id.clone(),
                offset: 0,
                key: None,
                bytes: 0,
            });
        if cursor.bytes < MAX_SUBAGENT_BYTES && changed_since(subagent, &cursor) {
            if left == 0 {
                out.more = true;
            } else if let Some((messages, used, more)) =
                read_one(subagent, &mut cursor, left, &reading)
            {
                left = left.saturating_sub(used.max(1));
                out.more |= more;
                out.messages.extend(messages);
            }
        }
        out.cursors.push(cursor);
    }
    out
}

/// Whether `subagent` may have more than `cursor` read: a log longer than
/// where it stopped; an OpenCode session, always (its rows say).
fn changed_since(subagent: &Subagent, cursor: &SubagentCursor) -> bool {
    match &subagent.source {
        Source::Log { path, .. } => {
            std::fs::metadata(path).is_ok_and(|metadata| metadata.len() != cursor.offset)
        }
        Source::OpenCode { .. } => true,
    }
}

/// `subagent`'s messages after `cursor`, marked, which moves on: with how
/// many bytes were read and whether more is there.
fn read_one(
    subagent: &Subagent,
    cursor: &mut SubagentCursor,
    budget: usize,
    reading: &Reading,
) -> Option<(Vec<TranscriptMessage>, usize, bool)> {
    let (messages, used, more) = match &subagent.source {
        Source::Log { agent, path } => {
            let from = cursor.offset;
            let (messages, offset, more) = read_log(*agent, path, from, budget, reading)?;
            cursor.offset = offset;
            (messages, super::consumed(from, offset), more)
        }
        Source::OpenCode {
            database,
            conversation,
        } => {
            let watermark = (cursor.bytes > 0 || cursor.offset > 0 || cursor.key.is_some())
                .then(|| (cursor.offset, cursor.key.clone()));
            let read = opencode::read_counted(database, conversation, watermark, budget)?;
            (cursor.offset, cursor.key) = read.watermark;
            (read.messages, read.bytes, read.more)
        }
    };
    let mut kept = Vec::new();
    for message in messages {
        let Some(message) = mark(message, &subagent.marker) else {
            continue;
        };
        let size = text_bytes(&message);
        if cursor.bytes.saturating_add(size) > MAX_SUBAGENT_BYTES {
            // It ran away: nothing more of it.
            cursor.bytes = MAX_SUBAGENT_BYTES;
            return Some((kept, used, false));
        }
        cursor.bytes += size;
        kept.push(message);
    }
    Some((kept, used, more))
}

/// `message` as `subagent`'s: its id the subagent's and its own (else its
/// position), its parts cut at [`MAX_SUBAGENT_PART_BYTES`].
pub(super) fn mark(
    mut message: TranscriptMessage,
    subagent: &TranscriptSubagent,
) -> Option<TranscriptMessage> {
    let id = format!("{}:{}", subagent.id, message.id);
    message.id = if super::is_id(&id) {
        id
    } else {
        format!("{}:{}", subagent.id, message.position)
    };
    if !super::is_id(&message.id) {
        return None;
    }
    for part in &mut message.parts {
        match part {
            TranscriptPart::Text { text, truncated }
            | TranscriptPart::Reasoning { text, truncated } => shorten(text, truncated),
            TranscriptPart::ToolCall {
                input, truncated, ..
            } => shorten(input, truncated),
            TranscriptPart::ToolResult {
                output, truncated, ..
            } => shorten(output, truncated),
            TranscriptPart::Unknown => {}
        }
    }
    message.subagent = Some(subagent.clone());
    Some(message)
}

/// `text` at most [`MAX_SUBAGENT_PART_BYTES`], cut at a character boundary.
fn shorten(text: &mut String, truncated: &mut bool) {
    if text.len() <= MAX_SUBAGENT_PART_BYTES {
        return;
    }
    let mut end = MAX_SUBAGENT_PART_BYTES;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text.truncate(end);
    *truncated = true;
}

/// The text a message carries, in bytes: what the per-subagent cap counts.
fn text_bytes(message: &TranscriptMessage) -> u64 {
    message
        .parts
        .iter()
        .map(|part| match part {
            TranscriptPart::Text { text, .. } | TranscriptPart::Reasoning { text, .. } => {
                text.len()
            }
            TranscriptPart::ToolCall { name, input, .. } => name.len() + input.len(),
            TranscriptPart::ToolResult { output, .. } => output.len(),
            TranscriptPart::Unknown => 0,
        } as u64)
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::io::Write;

    /// A Claude Code conversation in a projects folder: its transcript and
    /// its subagents' (each with its meta file).
    struct Claude {
        _directory: tempfile::TempDir,
        main: PathBuf,
        subagents: PathBuf,
    }

    impl Claude {
        fn new() -> Self {
            let directory = tempfile::tempdir().unwrap();
            let main = directory.path().join("c1.jsonl");
            let subagents = directory.path().join("c1").join("subagents");
            std::fs::create_dir_all(&subagents).unwrap();
            std::fs::File::create(&main).unwrap();
            Self {
                _directory: directory,
                main,
                subagents,
            }
        }

        fn source(&self) -> TranscriptSource {
            TranscriptSource::Log {
                agent: LogAgent::Claude,
                conversation: "c1".into(),
                path: self.main.clone(),
            }
        }

        fn append(path: &std::path::Path, lines: &[serde_json::Value]) {
            let mut file = std::fs::OpenOptions::new()
                .append(true)
                .create(true)
                .open(path)
                .unwrap();
            for line in lines {
                writeln!(file, "{line}").unwrap();
            }
        }

        fn main(&self, lines: &[serde_json::Value]) {
            Self::append(&self.main, lines);
        }

        fn subagent(&self, id: &str, meta: serde_json::Value, lines: &[serde_json::Value]) {
            std::fs::write(
                self.subagents.join(format!("agent-{id}.meta.json")),
                meta.to_string(),
            )
            .unwrap();
            Self::append(&self.subagents.join(format!("agent-{id}.jsonl")), lines);
        }
    }

    fn entry(kind: &str, uuid: &str, content: serde_json::Value) -> serde_json::Value {
        json!({"type": kind, "uuid": uuid, "timestamp": "2026-10-23T10:00:00.000Z",
            "message": {"role": kind, "content": content}})
    }

    fn sidechain(kind: &str, uuid: &str, content: serde_json::Value) -> serde_json::Value {
        let mut entry = entry(kind, uuid, content);
        entry["isSidechain"] = json!(true);
        entry["agentId"] = json!("a1b2c3d4e5f60718");
        entry
    }

    fn call(id: &str, background: bool) -> serde_json::Value {
        entry(
            "assistant",
            "m-call",
            json!([{"type": "tool_use", "id": id, "name": "Agent",
                "input": {"description": "Find the flaky test", "subagent_type": "Explore",
                    "prompt": "Look for it", "run_in_background": background}}]),
        )
    }

    #[test]
    fn claude_codes_subagents_come_marked_under_the_call_that_started_them() {
        let claude = Claude::new();
        claude.main(&[
            entry("user", "m-prompt", json!("Fix the flaky test")),
            call("toolu_01AGENT", false),
        ]);
        claude.subagent(
            "a1b2c3d4e5f60718",
            json!({"agentType": "Explore", "description": "Find the flaky test",
                "toolUseId": "toolu_01AGENT"}),
            &[
                sidechain("user", "s-prompt", json!("Look for it")),
                sidechain(
                    "assistant",
                    "s-call",
                    json!([{"type": "tool_use", "id": "toolu_02GREP", "name": "Grep",
                        "input": {"pattern": "flaky"}}]),
                ),
                sidechain(
                    "user",
                    "s-result",
                    json!([{"type": "tool_result", "tool_use_id": "toolu_02GREP",
                        "content": "x".repeat(MAX_SUBAGENT_PART_BYTES + 10)}]),
                ),
            ],
        );
        // Without subagents, the main conversation alone, as before.
        let plain = claude.source().read(None, None, false).unwrap();
        assert_eq!(plain.messages.len(), 2);
        assert!(plain.cursor.subagents.is_empty());

        let page = claude.source().read(None, None, true).unwrap();
        assert!(!page.more);
        let ids: Vec<&str> = page.messages.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(
            ids,
            [
                "m-prompt",
                "m-call",
                "a1b2c3d4e5f60718:s-prompt",
                "a1b2c3d4e5f60718:s-call",
                "a1b2c3d4e5f60718:s-result"
            ]
        );
        let marker = TranscriptSubagent {
            id: "a1b2c3d4e5f60718".into(),
            call_id: Some("toolu_01AGENT".into()),
            label: Some("Find the flaky test".into()),
        };
        assert_eq!(page.messages[0].subagent, None);
        assert!(page.messages[2..]
            .iter()
            .all(|message| message.subagent.as_ref() == Some(&marker)));
        // A subagent's long output is cut sooner.
        match &page.messages[4].parts[0] {
            TranscriptPart::ToolResult {
                output, truncated, ..
            } => {
                assert_eq!(output.len(), MAX_SUBAGENT_PART_BYTES);
                assert!(*truncated);
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(page.cursor.subagents.len(), 1);
        assert_eq!(page.cursor.subagents[0].id, "a1b2c3d4e5f60718");

        // Read on: nothing new; then only what the subagent wrote since.
        let again = claude
            .source()
            .read(Some(&page.cursor), None, true)
            .unwrap();
        assert!(again.messages.is_empty());
        assert_eq!(again.cursor, page.cursor);
        Claude::append(
            &claude.subagents.join("agent-a1b2c3d4e5f60718.jsonl"),
            &[sidechain(
                "assistant",
                "s-answer",
                json!([{"type": "text", "text": "It's the map order."}]),
            )],
        );
        let later = claude
            .source()
            .read(Some(&page.cursor), None, true)
            .unwrap();
        let ids: Vec<&str> = later.messages.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, ["a1b2c3d4e5f60718:s-answer"]);
        // A read without subagents keeps where they were read to.
        let plain = claude
            .source()
            .read(Some(&later.cursor), None, false)
            .unwrap();
        assert_eq!(plain.cursor.subagents, later.cursor.subagents);
    }

    #[test]
    fn a_subagent_started_in_the_background_ends_with_its_notice() {
        let claude = Claude::new();
        let launched = {
            let mut entry = entry(
                "user",
                "m-ack",
                json!([{"type": "tool_result", "tool_use_id": "toolu_01AGENT",
                    "content": "Async agent launched successfully."}]),
            );
            entry["toolUseResult"] = json!({"status": "async_launched", "isAsync": true});
            entry
        };
        let notice = |status: &str| {
            format!(
                "<task-notification>\n<task-id>a1b2c3d4e5f60718</task-id>\n\
                 <tool-use-id>toolu_01AGENT</tool-use-id>\n<status>{status}</status>\n\
                 <summary>Agent \"Find the flaky test\" {status}</summary>\n\
                 <result>The <b>map</b> order.</result>\n</task-notification>"
            )
        };
        let queued = json!({"type": "attachment", "uuid": "m-queued",
            "timestamp": "2026-10-23T10:01:00.000Z",
            "attachment": {"type": "queued_command", "prompt": notice("completed"),
                "commandMode": "task-notification"}});
        claude.main(&[
            call("toolu_01AGENT", true),
            launched,
            queued,
            entry("user", "m-notice", json!(notice("completed"))),
        ]);
        claude.subagent(
            "a1b2c3d4e5f60718",
            json!({"agentType": "Explore", "toolUseId": "toolu_01AGENT"}),
            &[sidechain("user", "s-prompt", json!("Look for it"))],
        );
        let page = claude.source().read(None, None, true).unwrap();
        let main: Vec<&TranscriptMessage> = page
            .messages
            .iter()
            .filter(|message| message.subagent.is_none())
            .collect();
        // The call, then its result once the subagent ended: queued, then
        // as a prompt, under one id.
        assert_eq!(main.len(), 3);
        assert_eq!(main[1].id, "a1b2c3d4e5f60718.finished");
        assert_eq!(main[2].id, "a1b2c3d4e5f60718.finished");
        assert_eq!(
            main[2].parts,
            [TranscriptPart::ToolResult {
                call_id: Some("toolu_01AGENT".into()),
                output: "The <b>map</b> order.".into(),
                error: false,
                truncated: false,
            }]
        );
        // Its label falls back to its agent type.
        let sub = page
            .messages
            .iter()
            .find_map(|m| m.subagent.clone())
            .unwrap();
        assert_eq!(sub.label.as_deref(), Some("Explore"));

        // A notice of anything else stays left out; one that failed is an error.
        let other = Claude::new();
        other.main(&[
            entry("user", "m-bash", json!("<task-notification><task-id>b123</task-id><status>completed</status></task-notification>")),
            entry("user", "m-failed", json!(notice("failed"))),
        ]);
        other.subagent(
            "a1b2c3d4e5f60718",
            json!({"toolUseId": "toolu_01AGENT"}),
            &[],
        );
        let page = other.source().read(None, None, true).unwrap();
        assert_eq!(page.messages.len(), 1);
        assert!(matches!(
            page.messages[0].parts[0],
            TranscriptPart::ToolResult { error: true, .. }
        ));
        // Without subagents the notices stay out, the acknowledgement in.
        let plain = claude.source().read(None, None, false).unwrap();
        assert_eq!(plain.messages.len(), 2);
    }

    #[test]
    fn a_subagent_that_runs_away_stops_being_read() {
        let claude = Claude::new();
        let big = "y".repeat(MAX_SUBAGENT_PART_BYTES);
        let lines: Vec<serde_json::Value> =
            (0..(MAX_SUBAGENT_BYTES as usize / MAX_SUBAGENT_PART_BYTES + 8))
                .map(|n| {
                    sidechain(
                        "assistant",
                        &format!("s{n}"),
                        json!([{"type": "text", "text": big}]),
                    )
                })
                .collect();
        claude.subagent("a0", json!({}), &lines);
        let mut cursor = None;
        let mut messages = Vec::new();
        for _ in 0..50 {
            let page = claude
                .source()
                .read(
                    cursor.as_ref(),
                    Some(session_core::api::MAX_TRANSCRIPT_BYTES),
                    true,
                )
                .unwrap();
            messages.extend(page.messages);
            cursor = Some(page.cursor);
            if !page.more {
                break;
            }
        }
        let total: u64 = messages.iter().map(text_bytes).sum();
        assert!(total <= MAX_SUBAGENT_BYTES);
        assert_eq!(
            messages.len(),
            MAX_SUBAGENT_BYTES as usize / MAX_SUBAGENT_PART_BYTES
        );
        let cursor = cursor.unwrap();
        assert_eq!(cursor.subagents[0].bytes, MAX_SUBAGENT_BYTES);
        // More written: still nothing.
        Claude::append(
            &claude.subagents.join("agent-a0.jsonl"),
            &[sidechain(
                "assistant",
                "late",
                json!([{"type": "text", "text": "late"}]),
            )],
        );
        let page = claude.source().read(Some(&cursor), None, true).unwrap();
        assert!(page.messages.is_empty());
    }

    #[test]
    fn subagents_wait_for_the_main_conversation_within_the_budget() {
        let claude = Claude::new();
        let lines: Vec<serde_json::Value> = (0..4)
            .map(|n| entry("user", &format!("m{n}"), json!("x".repeat(400))))
            .collect();
        claude.main(&lines);
        claude.subagent("a0", json!({}), &[sidechain("user", "s0", json!("go"))]);
        // A budget the main conversation uses up: the subagent waits.
        let first = claude.source().read(None, Some(1000), true).unwrap();
        assert!(first.more);
        assert!(first.messages.iter().all(|m| m.subagent.is_none()));
        let mut cursor = first.cursor;
        let mut subagent = Vec::new();
        for _ in 0..10 {
            let page = claude
                .source()
                .read(Some(&cursor), Some(1000), true)
                .unwrap();
            subagent.extend(page.messages.into_iter().filter(|m| m.subagent.is_some()));
            cursor = page.cursor;
            if !page.more {
                break;
            }
        }
        assert_eq!(subagent.len(), 1);
        assert_eq!(subagent[0].id, "a0:s0");
    }

    #[test]
    fn opencodes_child_sessions_are_its_subagents() {
        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join("opencode.db");
        let connection = rusqlite::Connection::open(&database).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE session_v2 (id TEXT PRIMARY KEY, parent_id TEXT, title TEXT,
                   time_updated INTEGER);
                 CREATE TABLE session_message (id TEXT PRIMARY KEY, session_id TEXT, type TEXT,
                   seq INTEGER, time_created INTEGER, time_updated INTEGER, data TEXT);
                 CREATE TABLE part (id TEXT PRIMARY KEY, message_id TEXT, session_id TEXT,
                   time_created INTEGER, time_updated INTEGER, data TEXT);
                 INSERT INTO session_v2 VALUES ('ses_main', NULL, 'Main', 10);
                 INSERT INTO session_v2 VALUES ('ses_child', 'ses_main', 'Find it (@explore)', 30);
                 INSERT INTO session_v2 VALUES ('ses_grandchild', 'ses_child', 'Deeper', 40);
                 INSERT INTO session_v2 VALUES ('ses_other', 'ses_elsewhere', 'Other', 50);",
            )
            .unwrap();
        let row = |id: &str, session: &str, kind: &str, seq: i64, data: serde_json::Value| {
            connection
                .execute(
                    "INSERT INTO session_message VALUES (?1, ?2, ?3, ?4, 100, 20, ?5)",
                    rusqlite::params![id, session, kind, seq, data.to_string()],
                )
                .unwrap();
        };
        row("msg_u", "ses_main", "user", 1, json!({"text": "Fix it"}));
        row(
            "msg_a",
            "ses_main",
            "assistant",
            2,
            json!({"content": [{"type": "tool", "id": "call_task", "name": "task",
                "state": {"status": "running",
                    "input": {"description": "Find the flaky test", "subagent_type": "explore"},
                    "metadata": {"sessionId": "ses_child"}}}]}),
        );
        row(
            "msg_cu",
            "ses_child",
            "user",
            1,
            json!({"text": "Look for it"}),
        );
        row(
            "msg_ca",
            "ses_child",
            "assistant",
            2,
            json!({"content": [{"type": "text", "text": "Found it."}]}),
        );
        row(
            "msg_gu",
            "ses_grandchild",
            "user",
            1,
            json!({"text": "Deeper"}),
        );
        drop(connection);
        let source = TranscriptSource::OpenCode {
            database,
            conversation: "ses_main".into(),
        };
        let page = source.read(None, None, true).unwrap();
        // In the order OpenCode changed them: the main conversation's, then
        // each child's, the least recently changed first.
        let ids: Vec<&str> = page.messages.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(
            ids,
            [
                "msg_a:0",
                "msg_u",
                "ses_child:msg_ca:0",
                "ses_child:msg_cu",
                "ses_grandchild:msg_gu"
            ]
        );
        assert_eq!(page.messages[0].subagent, None);
        assert_eq!(
            page.messages[2].subagent,
            Some(TranscriptSubagent {
                id: "ses_child".into(),
                call_id: Some("call_task".into()),
                label: Some("Find the flaky test".into()),
            })
        );
        // Not started by a call it can see: its title.
        assert_eq!(
            page.messages[4].subagent.as_ref().unwrap().label.as_deref(),
            Some("Deeper")
        );
        let again = source.read(Some(&page.cursor), None, true).unwrap();
        assert!(again.messages.is_empty());
    }
}
