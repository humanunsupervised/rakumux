//! OpenCode's latest output, from its database (`opencode.db`, read-only):
//! OpenCode 2 keeps each assistant reply as a row of `session_message`, its
//! reasoning, text and tool blocks in order; 1.x keeps the pieces as rows of
//! `part`, the assistant's beside the user's. Subagents run in sessions of
//! their own, so following one conversation never shows theirs.
use super::{first_line, prompt_line, short_path, tool_line};
use serde_json::Value;
use std::path::Path;

/// The 1.x pieces looked at, newest first: enough to pass the bookkeeping
/// ones (steps, snapshots, patches) between two that say something.
const RECENT_PARTS: usize = 16;
/// The OpenCode 2 replies looked at, newest first, to pass empty ones.
const RECENT_REPLIES: usize = 4;
/// The most replies (OpenCode 2) or pieces (1.x) a turn is read from.
const TURN_ROWS: usize = 500;

/// A turn: what tells it from the next, the user's prompt and the agent's
/// messages since, as written.
type RawTurn = (u64, Option<String>, Vec<String>);

/// The current turn of `conversation`, from whichever layout holds it.
pub(super) fn turn(database: &Path, conversation: &str) -> Option<RawTurn> {
    let connection = crate::resume::open_read_only(database)?;
    turn_v2(&connection, conversation).or_else(|| turn_v1(&connection, conversation))
}

/// OpenCode 2: the newest `user` row and the `assistant` rows after it.
fn turn_v2(connection: &rusqlite::Connection, conversation: &str) -> Option<RawTurn> {
    let prompt: Option<(i64, String)> = connection
        .query_row(
            "SELECT seq, data FROM session_message \
             WHERE session_id = ?1 AND type = 'user' ORDER BY seq DESC LIMIT 1",
            [conversation],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .ok();
    let after = prompt.as_ref().map_or(-1, |(seq, _)| *seq);
    let mut statement = connection
        .prepare(&format!(
            "SELECT data FROM session_message WHERE session_id = ?1 AND type = 'assistant' \
             AND seq > ?2 ORDER BY seq LIMIT {TURN_ROWS}"
        ))
        .ok()?;
    let rows = statement
        .query_map(rusqlite::params![conversation, after], |row| {
            row.get::<_, String>(0)
        })
        .ok()?;
    let messages: Vec<String> = rows
        .filter_map(Result::ok)
        .filter_map(|data| serde_json::from_str::<Value>(&data).ok())
        .flat_map(|reply| reply_messages(&reply))
        .collect();
    if prompt.is_none() && messages.is_empty() {
        return None;
    }
    let text = prompt.and_then(|(_, data)| {
        let data: Value = serde_json::from_str(&data).ok()?;
        Some(data.get("text")?.as_str()?.to_string())
    });
    Some((u64::try_from(after + 1).unwrap_or(0), text, messages))
}

/// OpenCode 1.x: the newest user message, and the assistant's pieces since.
fn turn_v1(connection: &rusqlite::Connection, conversation: &str) -> Option<RawTurn> {
    let mut statement = connection
        .prepare(
            "SELECT id, time_created, data FROM message \
             WHERE session_id = ?1 ORDER BY time_created DESC LIMIT 200",
        )
        .ok()?;
    let rows = statement
        .query_map([conversation], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, String>(2)?,
            ))
        })
        .ok()?;
    let user = rows.filter_map(Result::ok).find_map(|(id, created, data)| {
        let data: Value = serde_json::from_str(&data).ok()?;
        (data.get("role")?.as_str()? == "user").then_some((id, created))
    });
    drop(statement);
    let since = user.as_ref().map_or(i64::MIN, |(_, created)| *created);
    let mut statement = connection
        .prepare(&format!(
            "SELECT part.data, message.data, message.id FROM part \
             JOIN message ON message.id = part.message_id \
             WHERE part.session_id = ?1 AND message.time_created >= ?2 \
             ORDER BY message.time_created, part.time_created LIMIT {TURN_ROWS}"
        ))
        .ok()?;
    let rows = statement
        .query_map(rusqlite::params![conversation, since], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })
        .ok()?;
    let mut prompt = Vec::new();
    let mut messages = Vec::new();
    for (part, message, message_id) in rows.filter_map(Result::ok) {
        let (Ok(part), Ok(message)) = (
            serde_json::from_str::<Value>(&part),
            serde_json::from_str::<Value>(&message),
        ) else {
            continue;
        };
        match message.get("role").and_then(Value::as_str) {
            Some("assistant") => messages.extend(part_message(&part)),
            Some("user") if user.as_ref().is_some_and(|(id, _)| *id == message_id) => {
                let typed = part.get("type").and_then(Value::as_str) == Some("text")
                    && part.get("synthetic").and_then(Value::as_bool) != Some(true);
                if let Some(text) = part.get("text").and_then(Value::as_str).filter(|_| typed) {
                    prompt.push(text.to_string());
                }
            }
            _ => {}
        }
    }
    if user.is_none() && messages.is_empty() {
        return None;
    }
    let prompt = (!prompt.is_empty()).then(|| prompt.join("\n"));
    Some((since.max(0).unsigned_abs(), prompt, messages))
}

/// The latest line of `conversation`, whichever of the two layouts holds
/// it (the newer one, when both do).
pub(super) fn latest(database: &Path, conversation: &str) -> Option<String> {
    let connection = crate::resume::open_read_only(database)?;
    let v2 = replies(&connection, conversation);
    let v1 = parts(&connection, conversation);
    [v2, v1]
        .into_iter()
        .flatten()
        .max_by_key(|(updated, _)| *updated)
        .map(|(_, line)| line)
}

/// What the user last typed in `conversation`, whichever layout holds it.
pub(super) fn latest_prompt(database: &Path, conversation: &str) -> Option<String> {
    let connection = crate::resume::open_read_only(database)?;
    let v2 = newest(
        &connection,
        &format!(
            "SELECT data, time_updated FROM session_message \
             WHERE session_id = ?1 AND type = 'user' ORDER BY seq DESC LIMIT {RECENT_REPLIES}"
        ),
        conversation,
        |data| prompt_line(data.get("text")?.as_str()?),
    );
    let v1 = user_parts(&connection, conversation);
    [v2, v1]
        .into_iter()
        .flatten()
        .max_by_key(|(updated, _)| *updated)
        .map(|(_, line)| line)
}

/// The first row of `query` (`data` JSON, `time_updated`) that makes a line.
fn newest(
    connection: &rusqlite::Connection,
    query: &str,
    conversation: &str,
    line: impl Fn(&Value) -> Option<String>,
) -> Option<(i64, String)> {
    let mut statement = connection.prepare(query).ok()?;
    let rows = statement
        .query_map([conversation], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })
        .ok()?;
    let found = rows
        .filter_map(Result::ok)
        .find_map(|(data, updated)| Some((updated, line(&serde_json::from_str(&data).ok()?)?)));
    found
}

/// The newest 1.x text the user typed (not text OpenCode added).
fn user_parts(connection: &rusqlite::Connection, conversation: &str) -> Option<(i64, String)> {
    let mut statement = connection
        .prepare(&format!(
            "SELECT part.data, message.data, part.time_updated FROM part \
             JOIN message ON message.id = part.message_id \
             WHERE part.session_id = ?1 ORDER BY part.time_updated DESC LIMIT {RECENT_PARTS}"
        ))
        .ok()?;
    let rows = statement
        .query_map([conversation], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
            ))
        })
        .ok()?;
    let found = rows
        .filter_map(Result::ok)
        .find_map(|(part, message, updated)| {
            let message: Value = serde_json::from_str(&message).ok()?;
            let part: Value = serde_json::from_str(&part).ok()?;
            if message.get("role")?.as_str()? != "user"
                || part.get("type")?.as_str()? != "text"
                || part.get("synthetic").and_then(Value::as_bool) == Some(true)
            {
                return None;
            }
            Some((updated, prompt_line(part.get("text")?.as_str()?)?))
        });
    found
}

/// The newest OpenCode 2 reply that makes a line (one that ended in an
/// error can be empty).
fn replies(connection: &rusqlite::Connection, conversation: &str) -> Option<(i64, String)> {
    let mut statement = connection
        .prepare(&format!(
            "SELECT data, time_updated FROM session_message \
             WHERE session_id = ?1 AND type = 'assistant' ORDER BY seq DESC LIMIT {RECENT_REPLIES}"
        ))
        .ok()?;
    let rows = statement
        .query_map([conversation], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })
        .ok()?;
    let found = rows.filter_map(Result::ok).find_map(|(data, updated)| {
        Some((updated, reply_line(&serde_json::from_str(&data).ok()?)?))
    });
    found
}

/// The newest 1.x piece of the assistant's that makes a line.
fn parts(connection: &rusqlite::Connection, conversation: &str) -> Option<(i64, String)> {
    let mut statement = connection
        .prepare(&format!(
            "SELECT part.data, message.data, part.time_updated FROM part \
             JOIN message ON message.id = part.message_id \
             WHERE part.session_id = ?1 ORDER BY part.time_updated DESC LIMIT {RECENT_PARTS}"
        ))
        .ok()?;
    let rows = statement
        .query_map([conversation], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
            ))
        })
        .ok()?;
    let found = rows
        .filter_map(Result::ok)
        .find_map(|(part, message, updated)| {
            let message: Value = serde_json::from_str(&message).ok()?;
            if message.get("role")?.as_str()? != "assistant" {
                return None;
            }
            Some((updated, part_line(&serde_json::from_str(&part).ok()?)?))
        });
    found
}

/// An OpenCode 2 reply's last block that makes a line.
fn reply_line(reply: &Value) -> Option<String> {
    reply_messages(reply)
        .iter()
        .rev()
        .find_map(|message| first_line(message))
}

/// An OpenCode 2 reply's messages, oldest first: reasoning and text as
/// written, tool calls as lines.
fn reply_messages(reply: &Value) -> Vec<String> {
    let Some(blocks) = reply.get("content").and_then(Value::as_array) else {
        return Vec::new();
    };
    blocks
        .iter()
        .filter_map(|block| match block.get("type")?.as_str()? {
            "reasoning" | "text" => written(block.get("text")?.as_str()?),
            "tool" => {
                let name = block.get("name")?.as_str()?;
                tool(name, block.get("state")?.get("input"))
            }
            _ => None,
        })
        .collect()
}

/// A 1.x piece as a line: reasoning, text or a tool call.
fn part_line(part: &Value) -> Option<String> {
    first_line(&part_message(part)?)
}

/// A 1.x piece's message: reasoning or text as written, a tool call as a line.
fn part_message(part: &Value) -> Option<String> {
    match part.get("type")?.as_str()? {
        "reasoning" | "text" => written(part.get("text")?.as_str()?),
        "tool" => {
            let name = part.get("tool")?.as_str()?;
            tool(name, part.get("state")?.get("input"))
        }
        _ => None,
    }
}

/// Text as written, when there is some.
fn written(text: &str) -> Option<String> {
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_string())
}

/// A tool call as a line, with what it works on when that's short to say.
fn tool(name: &str, input: Option<&Value>) -> Option<String> {
    let field = |key: &str| input?.get(key)?.as_str();
    let subject = match name {
        "bash" | "shell" => field("description")
            .or_else(|| field("command"))
            .map(String::from),
        "read" | "write" | "edit" => field("path").or_else(|| field("filePath")).map(short_path),
        "grep" | "glob" => field("pattern").map(String::from),
        "webfetch" => field("url").map(String::from),
        "task" | "subagent" => field("description").map(String::from),
        _ => None,
    };
    tool_line(name, subject.as_deref())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_reply_shows_its_last_block() {
        let reply = json!({"content": [
            {"type": "reasoning", "text": "Looking at the parser"},
            {"type": "tool", "name": "shell", "state": {"input": {"command": "cargo test"}}}
        ]});
        assert_eq!(reply_line(&reply), Some("shell: cargo test".into()));
        let reply = json!({"content": [
            {"type": "tool", "name": "read", "state": {"input": {"path": "/repo/tui/src/app.rs"}}},
            {"type": "text", "text": "All tests pass now."}
        ]});
        assert_eq!(reply_line(&reply), Some("All tests pass now.".into()));
    }

    #[test]
    fn version_one_pieces_are_lines_but_bookkeeping_is_not() {
        assert_eq!(
            part_line(
                &json!({"type": "tool", "tool": "read", "state": {"input": {"filePath": "/repo/src/main.rs"}}})
            ),
            Some("read: src/main.rs".into())
        );
        assert_eq!(
            part_line(&json!({"type": "reasoning", "text": "**Planning** the fix"})),
            Some("Planning the fix".into())
        );
        assert_eq!(
            part_line(&json!({"type": "step-finish", "reason": "stop"})),
            None
        );
    }

    #[test]
    fn the_database_gives_the_newest_assistant_line_of_the_conversation() {
        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join("opencode.db");
        let connection = rusqlite::Connection::open(&database).unwrap();
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
        let reply = |seq: i64, text: &str, updated: i64| {
            let data = json!({"content": [{"type": "text", "text": text}]}).to_string();
            connection
                .execute(
                    "INSERT INTO session_message VALUES (?1, 'ses_a', 'assistant', ?2, 0, ?3, ?4)",
                    rusqlite::params![format!("m{seq}"), seq, updated, data],
                )
                .unwrap();
        };
        reply(1, "First answer", 10);
        reply(2, "Second answer", 20);
        // One that ended in an error, empty, is passed.
        connection
            .execute(
                "INSERT INTO session_message VALUES ('m2b', 'ses_a', 'assistant', 4, 0, 25, ?1)",
                [json!({"content": []}).to_string()],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO session_message VALUES ('u3', 'ses_a', 'user', 3, 0, 30, ?1)",
                [json!({"text": "my prompt"}).to_string()],
            )
            .unwrap();
        assert_eq!(latest(&database, "ses_a"), Some("Second answer".into()));
        assert_eq!(latest_prompt(&database, "ses_a"), Some("my prompt".into()));
        assert_eq!(latest(&database, "ses_other"), None);
        // A 1.x conversation: the user's own text isn't a line.
        connection
            .execute_batch(&format!(
                "INSERT INTO message VALUES ('a1', 'ses_b', 0, 0, '{}');
                 INSERT INTO message VALUES ('u1', 'ses_b', 0, 0, '{}');
                 INSERT INTO part VALUES ('p1', 'a1', 'ses_b', 0, 5, '{}');
                 INSERT INTO part VALUES ('p2', 'u1', 'ses_b', 0, 6, '{}');",
                json!({"role": "assistant"}),
                json!({"role": "user"}),
                json!({"type": "text", "text": "Fixed it."}),
                json!({"type": "text", "text": "my prompt"}),
            ))
            .unwrap();
        assert_eq!(latest(&database, "ses_b"), Some("Fixed it.".into()));
        assert_eq!(latest_prompt(&database, "ses_b"), Some("my prompt".into()));
        // The turns: OpenCode 2's starts after its newest prompt.
        let (_, prompt, messages) = turn(&database, "ses_a").unwrap();
        assert_eq!(prompt.as_deref(), Some("my prompt"));
        assert!(messages.is_empty(), "{messages:?}");
        reply(5, "Working on it", 40);
        let (_, _, messages) = turn(&database, "ses_a").unwrap();
        assert_eq!(messages, ["Working on it"]);
        // 1.x: the user's message and the assistant's pieces.
        let (_, prompt, messages) = turn(&database, "ses_b").unwrap();
        assert_eq!(prompt.as_deref(), Some("my prompt"));
        assert_eq!(messages, ["Fixed it."]);
    }
}
