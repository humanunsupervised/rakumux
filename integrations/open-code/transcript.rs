//! OpenCode's conversation, from its database (`opencode.db`, read-only),
//! the rows changed since a `(time_updated, id)` watermark, oldest change
//! first. OpenCode 2 keeps each prompt and reply as a row of
//! `session_message`: a prompt is a message at `seq × 1024`, and each block of
//! a reply (reasoning, text, a tool call with its result) one at
//! `seq × 1024 + block`. 1.x keeps the pieces as rows of `part`: each is a
//! message at its message's `time_created × 1024` plus its place in it.
//! OpenCode rewrites rows as a reply streams, so a message comes again when
//! it changed.
use super::{assemble, call, compact, reasoning, result, text};
use serde_json::Value;
use session_core::api::{TranscriptMessage, TranscriptPart, TranscriptRole};
use std::path::Path;

/// Rows read at once.
const ROWS: i64 = 200;
/// Blocks or pieces of one reply or message counted; further ones are left out.
const SLOTS: u64 = 1024;

/// Where a read stopped: the last row's `time_updated` and id.
pub(super) type Watermark = (u64, Option<String>);

/// The messages of `conversation` changed after `after`, reading about
/// `budget` bytes of rows. Returns them, the new watermark, and whether more
/// rows were there.
#[cfg(test)]
pub(super) fn read(
    database: &Path,
    conversation: &str,
    after: Option<Watermark>,
    budget: usize,
) -> Option<(Vec<TranscriptMessage>, Watermark, bool)> {
    let read = read_counted(database, conversation, after, budget)?;
    Some((read.messages, read.watermark, read.more))
}

/// What a read gave: the messages, the new watermark, whether more rows were
/// there, and how many bytes of rows were read.
pub(super) struct Counted {
    pub messages: Vec<TranscriptMessage>,
    pub watermark: Watermark,
    pub more: bool,
    pub bytes: usize,
}

/// [`read`], counting the bytes of rows read.
pub(super) fn read_counted(
    database: &Path,
    conversation: &str,
    after: Option<Watermark>,
    budget: usize,
) -> Option<Counted> {
    let connection = crate::resume::open_read_only(database)?;
    let newer = after.is_some();
    let (updated, key) = after.unwrap_or((0, None));
    let watermark = (
        i64::try_from(updated).unwrap_or(i64::MAX),
        key.clone().unwrap_or_default(),
    );
    let v2 = connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM session_message WHERE session_id = ?1)",
            [conversation],
            |row| row.get::<_, bool>(0),
        )
        .unwrap_or(false);
    let rows = if v2 {
        rows_v2(&connection, conversation, newer, &watermark)?
    } else {
        rows_v1(&connection, conversation, newer, &watermark)?
    };
    let mut messages = Vec::new();
    let mut last = (updated, key);
    let mut read = 0;
    let mut more = rows.len() as i64 >= ROWS;
    for (index, row) in rows.iter().enumerate() {
        read += row.size;
        messages.extend(row.messages.iter().cloned());
        last = (row.updated, Some(row.id.clone()));
        if read >= budget && index + 1 < rows.len() {
            more = true;
            break;
        }
    }
    Some(Counted {
        messages,
        watermark: last,
        more,
        bytes: read,
    })
}

/// A child session (a subagent's conversation), with the task call that
/// started it and what it was asked to do, when its parent's rows say.
pub(super) struct Child {
    pub id: String,
    pub call_id: Option<String>,
    pub label: Option<String>,
    /// When it last changed (Unix milliseconds).
    pub updated: u64,
}

/// The child sessions of `conversation` and of theirs, down to `depth`
/// levels, at most `limit` of the most recently changed: OpenCode 2's
/// `session_v2` rows (1.x's `session`) whose `parent_id` is one of them.
/// Each is linked to the `task` call whose `metadata.sessionId` names it.
pub(super) fn children(
    database: &Path,
    conversation: &str,
    depth: usize,
    limit: usize,
) -> Vec<Child> {
    let Some(connection) = crate::resume::open_read_only(database) else {
        return Vec::new();
    };
    let table = if has_table(&connection, "session_v2") {
        "session_v2"
    } else {
        "session"
    };
    let mut found: Vec<Child> = Vec::new();
    let mut parents = vec![conversation.to_owned()];
    for _ in 0..depth {
        let mut next = Vec::new();
        for parent in &parents {
            let listed = child_rows(&connection, table, parent, limit);
            if listed.is_empty() {
                continue;
            }
            let calls = task_calls(&connection, parent);
            for (id, title, updated) in listed {
                let (call_id, label) = match calls.get(&id) {
                    Some((call, label)) => (Some(call.clone()), label.clone()),
                    None => (None, None),
                };
                next.push(id.clone());
                found.push(Child {
                    label: label.or(title),
                    id,
                    call_id,
                    updated,
                });
            }
        }
        if next.is_empty() || found.len() >= limit * 4 {
            break;
        }
        parents = next;
    }
    found.sort_by(|a, b| b.updated.cmp(&a.updated));
    found.truncate(limit);
    found
}

fn has_table(connection: &rusqlite::Connection, name: &str) -> bool {
    connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1)",
            [name],
            |row| row.get::<_, bool>(0),
        )
        .unwrap_or(false)
}

/// The child sessions of `parent` in `table`: id, title and `time_updated`.
fn child_rows(
    connection: &rusqlite::Connection,
    table: &str,
    parent: &str,
    limit: usize,
) -> Vec<(String, Option<String>, u64)> {
    let query = format!(
        "SELECT id, title, time_updated FROM {table} WHERE parent_id = ?1 \
         ORDER BY time_updated DESC LIMIT ?2"
    );
    let Ok(mut statement) = connection.prepare(&query) else {
        return Vec::new();
    };
    let limit = i64::try_from(limit).unwrap_or(i64::MAX);
    let Ok(rows) = statement.query_map(rusqlite::params![parent, limit], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, Option<String>>(1)?,
            row.get::<_, i64>(2)?,
        ))
    }) else {
        return Vec::new();
    };
    rows.filter_map(Result::ok)
        .map(|(id, title, updated)| (id, title, u64::try_from(updated).unwrap_or(0)))
        .collect()
}

/// `parent`'s `task` calls that started a child session: by the child's id,
/// the call's id and its description (else its agent type).
fn task_calls(
    connection: &rusqlite::Connection,
    parent: &str,
) -> std::collections::HashMap<String, (String, Option<String>)> {
    let mut calls = std::collections::HashMap::new();
    // OpenCode 2 keeps a reply's blocks in its row; 1.x a tool per piece.
    for query in [
        "SELECT data FROM session_message WHERE session_id = ?1 AND type = 'assistant' \
         AND data LIKE '%\"sessionId\"%'",
        "SELECT data FROM part WHERE session_id = ?1 AND data LIKE '%\"sessionId\"%'",
    ] {
        let Ok(mut statement) = connection.prepare(query) else {
            continue;
        };
        let Ok(rows) = statement.query_map([parent], |row| row.get::<_, String>(0)) else {
            continue;
        };
        for data in rows.filter_map(Result::ok) {
            let Ok(data) = serde_json::from_str::<Value>(&data) else {
                continue;
            };
            let blocks = match data.get("content").and_then(Value::as_array) {
                Some(blocks) => blocks.iter().collect(),
                None => vec![&data],
            };
            for block in blocks {
                let field = |key: &str| block.get(key).and_then(Value::as_str);
                let (call_id, name) = match field("type") {
                    Some("tool") => (
                        field("id").or_else(|| field("callID")),
                        field("name").or_else(|| field("tool")),
                    ),
                    _ => continue,
                };
                let state = block.get("state");
                let child = state
                    .and_then(|state| state.get("metadata"))
                    .and_then(|metadata| metadata.get("sessionId"))
                    .and_then(Value::as_str);
                let (Some(call_id), Some("task"), Some(child)) = (call_id, name, child) else {
                    continue;
                };
                let input = state.and_then(|state| state.get("input"));
                let label = ["description", "subagent_type"].iter().find_map(|key| {
                    input
                        .and_then(|input| input.get(*key))
                        .and_then(Value::as_str)
                        .map(str::trim)
                        .filter(|text| !text.is_empty())
                        .map(String::from)
                });
                calls.insert(child.to_owned(), (call_id.to_owned(), label));
            }
        }
    }
    calls
}

/// A row read, as messages, and where it is in the order of changes.
struct Row {
    id: String,
    updated: u64,
    size: usize,
    messages: Vec<TranscriptMessage>,
}

/// OpenCode 2's rows of `conversation` changed after the watermark.
fn rows_v2(
    connection: &rusqlite::Connection,
    conversation: &str,
    newer: bool,
    (updated, key): &(i64, String),
) -> Option<Vec<Row>> {
    let mut statement = connection
        .prepare(
            "SELECT id, type, seq, time_created, time_updated, data FROM session_message \
             WHERE session_id = ?1 AND (?2 = 0 OR time_updated > ?3 OR (time_updated = ?3 AND id > ?4)) \
             ORDER BY time_updated, id LIMIT ?5",
        )
        .ok()?;
    let rows = statement
        .query_map(
            rusqlite::params![conversation, newer, updated, key, ROWS],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, String>(5)?,
                ))
            },
        )
        .ok()?;
    let rows = rows
        .filter_map(Result::ok)
        .map(|(id, kind, seq, created, updated, data)| {
            let messages = match (u64::try_from(seq), serde_json::from_str::<Value>(&data)) {
                (Ok(seq), Ok(data)) => messages_v2(&id, &kind, seq, created, &data),
                _ => Vec::new(),
            };
            Row {
                updated: u64::try_from(updated).unwrap_or(0),
                size: data.len(),
                id,
                messages,
            }
        })
        .collect();
    Some(rows)
}

/// An OpenCode 2 row's messages: a prompt, or a reply's blocks.
fn messages_v2(
    id: &str,
    kind: &str,
    seq: u64,
    created: i64,
    data: &Value,
) -> Vec<TranscriptMessage> {
    let position = seq.saturating_mul(SLOTS);
    match kind {
        "user" => data
            .get("text")
            .and_then(Value::as_str)
            .and_then(text)
            .and_then(|part| {
                assemble(
                    Some(id),
                    position,
                    TranscriptRole::User,
                    Some(created),
                    vec![part],
                )
            })
            .into_iter()
            .collect(),
        "assistant" => data
            .get("content")
            .and_then(Value::as_array)
            .map(|blocks| {
                blocks
                    .iter()
                    .take(SLOTS as usize)
                    .enumerate()
                    .filter_map(|(index, block)| {
                        let at = block
                            .get("time")
                            .and_then(|time| time.get("created"))
                            .and_then(Value::as_i64)
                            .unwrap_or(created);
                        assemble(
                            Some(&format!("{id}:{index}")),
                            position + index as u64,
                            TranscriptRole::Assistant,
                            Some(at),
                            block_parts(block),
                        )
                    })
                    .collect()
            })
            .unwrap_or_default(),
        _ => Vec::new(),
    }
}

/// An OpenCode 2 reply block's parts: reasoning, text, or a tool call and its
/// result once it ran.
fn block_parts(block: &Value) -> Vec<TranscriptPart> {
    let field = |key: &str| block.get(key).and_then(Value::as_str);
    match field("type") {
        Some("reasoning") => field("text").and_then(reasoning).into_iter().collect(),
        Some("text") => field("text").and_then(text).into_iter().collect(),
        Some("tool") => tool(field("id"), field("name"), block.get("state")),
        _ => Vec::new(),
    }
}

/// A tool call and, once it ran, its result: OpenCode 2 keeps the output as
/// `content` pieces, 1.x as `output`.
fn tool(call_id: Option<&str>, name: Option<&str>, state: Option<&Value>) -> Vec<TranscriptPart> {
    let Some(name) = name else {
        return Vec::new();
    };
    let state = state.unwrap_or(&Value::Null);
    let input = compact(state.get("input").unwrap_or(&Value::Null));
    let mut parts: Vec<TranscriptPart> = call(call_id, name, &input).into_iter().collect();
    let status = state.get("status").and_then(Value::as_str);
    if !matches!(status, Some("completed" | "error")) {
        return parts;
    }
    let pieces = state
        .get("content")
        .and_then(Value::as_array)
        .map(|pieces| {
            pieces
                .iter()
                .filter_map(|piece| piece.get("text")?.as_str())
                .collect::<Vec<_>>()
                .join("\n")
        });
    let error = state.get("error").and_then(Value::as_str);
    let output = pieces
        .filter(|output| !output.is_empty())
        .or_else(|| {
            state
                .get("output")
                .and_then(Value::as_str)
                .map(String::from)
        })
        .or_else(|| error.map(String::from))
        .unwrap_or_default();
    parts.push(result(
        call_id,
        &output,
        status == Some("error") || error.is_some(),
    ));
    parts
}

/// OpenCode 1.x's pieces of `conversation` changed after the watermark.
fn rows_v1(
    connection: &rusqlite::Connection,
    conversation: &str,
    newer: bool,
    (updated, key): &(i64, String),
) -> Option<Vec<Row>> {
    let mut statement = connection
        .prepare(
            "SELECT part.id, part.time_updated, part.data, message.data, message.time_created, \
               (SELECT COUNT(*) FROM part AS earlier \
                WHERE earlier.message_id = part.message_id AND earlier.id < part.id) \
             FROM part JOIN message ON message.id = part.message_id \
             WHERE part.session_id = ?1 \
               AND (?2 = 0 OR part.time_updated > ?3 OR (part.time_updated = ?3 AND part.id > ?4)) \
             ORDER BY part.time_updated, part.id LIMIT ?5",
        )
        .ok()?;
    let rows = statement
        .query_map(
            rusqlite::params![conversation, newer, updated, key, ROWS],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, i64>(5)?,
                ))
            },
        )
        .ok()?;
    let rows = rows
        .filter_map(Result::ok)
        .map(|(id, updated, part, message, created, ordinal)| {
            let parsed = (
                serde_json::from_str::<Value>(&part),
                serde_json::from_str::<Value>(&message),
                u64::try_from(created),
            );
            let messages = match parsed {
                (Ok(part), Ok(message), Ok(created_at)) => {
                    let position = created_at.saturating_mul(SLOTS)
                        + u64::try_from(ordinal).unwrap_or(0).min(SLOTS - 1);
                    message_v1(&id, position, created, &part, &message)
                        .into_iter()
                        .collect()
                }
                _ => Vec::new(),
            };
            Row {
                updated: u64::try_from(updated).unwrap_or(0),
                size: part.len(),
                id,
                messages,
            }
        })
        .collect();
    Some(rows)
}

/// A 1.x piece as a message: the user's own text, or the assistant's
/// reasoning, text or tool call.
fn message_v1(
    id: &str,
    position: u64,
    created: i64,
    part: &Value,
    message: &Value,
) -> Option<TranscriptMessage> {
    let field = |key: &str| part.get(key).and_then(Value::as_str);
    let (role, parts) = match message.get("role")?.as_str()? {
        "user" => {
            let synthetic = part.get("synthetic").and_then(Value::as_bool) == Some(true);
            if field("type") != Some("text") || synthetic {
                return None;
            }
            (
                TranscriptRole::User,
                field("text").and_then(text).into_iter().collect(),
            )
        }
        "assistant" => {
            let parts = match field("type")? {
                "reasoning" => field("text").and_then(reasoning).into_iter().collect(),
                "text" => field("text").and_then(text).into_iter().collect(),
                "tool" => tool(field("callID"), field("tool"), part.get("state")),
                _ => Vec::new(),
            };
            (TranscriptRole::Assistant, parts)
        }
        _ => return None,
    };
    assemble(Some(id), position, role, Some(created), parts)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn database() -> (tempfile::TempDir, std::path::PathBuf, rusqlite::Connection) {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("opencode.db");
        let connection = rusqlite::Connection::open(&path).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE session_message (id TEXT PRIMARY KEY, session_id TEXT, type TEXT,
                   seq INTEGER, time_created INTEGER, time_updated INTEGER, data TEXT);
                 CREATE TABLE message (id TEXT PRIMARY KEY, session_id TEXT, time_created INTEGER,
                   time_updated INTEGER, data TEXT);
                 CREATE TABLE part (id TEXT PRIMARY KEY, message_id TEXT, session_id TEXT,
                   time_created INTEGER, time_updated INTEGER, data TEXT);",
            )
            .unwrap();
        (directory, path, connection)
    }

    fn ids(messages: &[TranscriptMessage]) -> Vec<(String, u64)> {
        messages
            .iter()
            .map(|message| (message.id.clone(), message.position))
            .collect()
    }

    #[test]
    fn opencode_2_gives_the_prompt_and_each_block_of_a_reply() {
        let (_directory, path, connection) = database();
        let row = |id: &str, kind: &str, seq: i64, updated: i64, data: Value| {
            connection
                .execute(
                    "INSERT OR REPLACE INTO session_message VALUES (?1, 'ses_a', ?2, ?3, 100, ?4, ?5)",
                    rusqlite::params![id, kind, seq, updated, data.to_string()],
                )
                .unwrap();
        };
        row("msg_u", "user", 1, 10, json!({"text": "Fix the test"}));
        row(
            "msg_a",
            "assistant",
            2,
            20,
            json!({"content": [
                {"type": "reasoning", "text": "Looking at it"},
                {"type": "tool", "id": "call_1", "name": "shell",
                 "state": {"status": "running", "input": {"command": "cargo test"}}}
            ]}),
        );
        let (messages, watermark, more) = read(&path, "ses_a", None, 1 << 20).unwrap();
        assert!(!more);
        assert_eq!(
            ids(&messages),
            [
                ("msg_u".into(), 1024),
                ("msg_a:0".into(), 2048),
                ("msg_a:1".into(), 2049)
            ]
        );
        assert_eq!(messages[0].role, TranscriptRole::User);
        // Still running: the call, no result yet.
        assert_eq!(messages[2].parts.len(), 1);
        assert_eq!(watermark, (20, Some("msg_a".into())));
        // Nothing changed: nothing new. The reply grows: it comes again.
        let (none, _, _) = read(&path, "ses_a", Some(watermark.clone()), 1 << 20).unwrap();
        assert!(none.is_empty());
        row(
            "msg_a",
            "assistant",
            2,
            30,
            json!({"content": [
                {"type": "reasoning", "text": "Looking at it"},
                {"type": "tool", "id": "call_1", "name": "shell",
                 "state": {"status": "error", "input": {"command": "cargo test"},
                           "content": [{"type": "text", "text": "1 failed"}]}},
                {"type": "text", "text": "One test fails."}
            ]}),
        );
        let (changed, _, _) = read(&path, "ses_a", Some(watermark), 1 << 20).unwrap();
        assert_eq!(changed.len(), 3);
        assert_eq!(
            changed[1].parts[1],
            TranscriptPart::ToolResult {
                call_id: Some("call_1".into()),
                output: "1 failed".into(),
                error: true,
                truncated: false
            }
        );
        // A small budget reads a row at a time.
        let (first, watermark, more) = read(&path, "ses_a", None, 1).unwrap();
        assert!(more);
        assert_eq!(ids(&first), [("msg_u".into(), 1024)]);
        let (second, _, more) = read(&path, "ses_a", Some(watermark), 1).unwrap();
        assert!(!more);
        assert_eq!(second.len(), 3);
    }

    #[test]
    fn opencode_1_gives_each_piece_but_bookkeeping_and_what_it_added() {
        let (_directory, path, connection) = database();
        connection
            .execute_batch(&format!(
                "INSERT INTO message VALUES ('msg_u', 'ses_b', 1000, 0, '{}');
                 INSERT INTO message VALUES ('msg_a', 'ses_b', 2000, 0, '{}');
                 INSERT INTO part VALUES ('prt_1', 'msg_u', 'ses_b', 0, 5, '{}');
                 INSERT INTO part VALUES ('prt_2', 'msg_u', 'ses_b', 0, 6, '{}');
                 INSERT INTO part VALUES ('prt_3', 'msg_a', 'ses_b', 0, 7, '{}');
                 INSERT INTO part VALUES ('prt_4', 'msg_a', 'ses_b', 0, 8, '{}');
                 INSERT INTO part VALUES ('prt_5', 'msg_a', 'ses_b', 0, 9, '{}');",
                json!({"role": "user"}),
                json!({"role": "assistant"}),
                json!({"type": "text", "text": "Fix the test"}),
                json!({"type": "text", "text": "file contents", "synthetic": true}),
                json!({"type": "step-start"}),
                json!({"type": "tool", "tool": "bash", "callID": "call_9",
                       "state": {"status": "completed", "input": {"command": "ls"}, "output": "a\nb"}}),
                json!({"type": "text", "text": "Done."}),
            ))
            .unwrap();
        let (messages, watermark, _) = read(&path, "ses_b", None, 1 << 20).unwrap();
        assert_eq!(
            ids(&messages),
            [
                ("prt_1".into(), 1_024_000),
                ("prt_4".into(), 2_048_001),
                ("prt_5".into(), 2_048_002)
            ]
        );
        assert_eq!(messages[1].parts.len(), 2);
        assert_eq!(watermark, (9, Some("prt_5".into())));
        assert_eq!(
            read(&path, "ses_other", None, 1 << 20).unwrap().0,
            Vec::new()
        );
    }
}
