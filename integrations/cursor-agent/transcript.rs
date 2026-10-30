//! Cursor's chat, from its transcript: each user or assistant line is a
//! message, at its byte offset. What the user typed (inside its
//! `<user_query>`) is the `user` role's; the agent's text (its reasoning
//! joined to it, as Cursor writes it) and tool calls the `assistant` role's.
//! Cursor writes neither tools' output nor call ids, ids or times.
//!
//! A read never ends past a final `turn_ended` line: Cursor drops that line
//! when it next writes the file whole, and the next prompt takes its place.
//! A cursor that no longer ends a line (a rewound chat) reads from the start.
use super::{assemble, call, compact, scan_log, text};
use serde_json::Value;
use session_core::api::{TranscriptMessage, TranscriptPart, TranscriptRole};
use std::path::Path;

/// The message of the transcript line at `position`, if it is one.
pub(super) fn message(entry: &Value, position: u64) -> Option<TranscriptMessage> {
    let output = super::super::cursor::typed;
    let (role, parts): (TranscriptRole, Vec<TranscriptPart>) =
        match entry.get("role").and_then(Value::as_str)? {
            "user" => (
                TranscriptRole::User,
                output(entry)
                    .and_then(|typed| super::super::typed_text(&typed))
                    .and_then(|typed| text(&typed))
                    .into_iter()
                    .collect(),
            ),
            "assistant" => (TranscriptRole::Assistant, assistant(entry)),
            _ => return None,
        };
    assemble(None, position, role, None, parts)
}

/// An assistant line's text and tool calls, in order.
fn assistant(entry: &Value) -> Vec<TranscriptPart> {
    let Some(blocks) = entry
        .get("message")
        .and_then(|message| message.get("content"))
        .and_then(Value::as_array)
    else {
        return Vec::new();
    };
    blocks
        .iter()
        .filter_map(|block| match block.get("type").and_then(Value::as_str)? {
            "text" => text(super::super::cursor::said(block)?),
            "tool_use" => call(
                None,
                block.get("name")?.as_str()?,
                &compact(block.get("input").unwrap_or(&Value::Null)),
            ),
            _ => None,
        })
        .collect()
}

/// [`scan_log`] for Cursor's transcript: from the start when `from` no
/// longer ends a line, and never on past a final `turn_ended` line.
pub(super) fn scan(
    path: &Path,
    from: u64,
    budget: usize,
    mut each: impl FnMut(&Value, u64),
) -> Option<(u64, bool)> {
    let from = match super::super::cursor::line_ends_before(path, from) {
        Some(false) => 0,
        _ => from,
    };
    let mut turn_end = None;
    let (next, more) = scan_log(path, from, budget, |entry, start| {
        turn_end = super::super::cursor::is_turn_end(entry).then_some(start);
        each(entry, start);
    })?;
    match turn_end {
        Some(start) if !more => Some((start, false)),
        _ => Some((next, more)),
    }
}

#[cfg(test)]
mod tests {
    use super::super::{LogAgent, TranscriptSource};
    use serde_json::json;
    use session_core::api::{TranscriptPart, TranscriptRole};
    use std::io::Write;

    fn source(lines: &[serde_json::Value]) -> (tempfile::TempDir, TranscriptSource) {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("c1.jsonl");
        let mut file = std::fs::File::create(&path).unwrap();
        for line in lines {
            writeln!(file, "{line}").unwrap();
        }
        let source = TranscriptSource::Log {
            agent: LogAgent::Cursor,
            conversation: "c1".into(),
            path,
        };
        (directory, source)
    }

    fn user(text: &str) -> serde_json::Value {
        json!({"role": "user", "message": {"content": [{"type": "text", "text": text}]}})
    }

    #[test]
    fn prompts_text_and_tool_calls_are_messages_and_turn_ends_are_not() {
        let (_directory, source) = source(&[
            user("<user_query>\nFix the parser\n</user_query>"),
            json!({"role": "assistant", "message": {"content": [
                {"type": "text", "text": "Looking.\n\n[REDACTED]"},
                {"type": "tool_use", "name": "Read", "input": {"path": "/r/src/parser.rs"}}]}}),
            json!({"role": "assistant", "message": {"content": [{"type": "text", "text": "Fixed."}]}}),
            json!({"type": "turn_ended", "status": "success"}),
        ]);
        let page = source.read(None, None).unwrap();
        assert_eq!(page.agent, "cursor-agent");
        let messages = page.messages;
        assert_eq!(messages.len(), 3);
        assert_eq!(messages[0].role, TranscriptRole::User);
        assert_eq!(messages[0].id, "0");
        assert_eq!(
            messages[0].parts,
            [TranscriptPart::Text {
                text: "Fix the parser".into(),
                truncated: false
            }]
        );
        assert_eq!(messages[1].role, TranscriptRole::Assistant);
        assert_eq!(
            messages[1].parts[1],
            TranscriptPart::ToolCall {
                call_id: None,
                name: "Read".into(),
                input: r#"{"path":"/r/src/parser.rs"}"#.into(),
                truncated: false
            }
        );
        assert_eq!(
            messages[1].parts[0],
            TranscriptPart::Text {
                text: "Looking.".into(),
                truncated: false
            }
        );
        assert!(messages.iter().all(|m| m.at.is_none()));
    }

    #[test]
    fn the_cursor_stays_before_a_final_turn_end_so_the_next_prompt_is_read_after_a_rewrite() {
        let first = [
            user("one"),
            json!({"role": "assistant", "message": {"content": [{"type": "text", "text": "Done."}]}}),
        ];
        let mut lines = first.to_vec();
        lines.push(json!({"type": "turn_ended", "status": "success"}));
        let (_directory, source) = source(&lines);
        let page = source.read(None, None).unwrap();
        assert_eq!(page.messages.len(), 2);
        let path = match &source {
            TranscriptSource::Log { path, .. } => path.clone(),
            _ => unreachable!(),
        };
        let held = page.cursor.offset;
        let before: usize = first.iter().map(|line| line.to_string().len() + 1).sum();
        assert_eq!(held, before as u64, "at the turn end's start");
        // Read again unchanged: nothing new, the same cursor.
        let again = source.read(Some(&page.cursor), None).unwrap();
        assert!(again.messages.is_empty());
        assert_eq!(again.cursor.offset, held);
        // Cursor writes the file whole for the next turn: the turn end is
        // gone and the next prompt starts where it was.
        let mut file = std::fs::File::create(&path).unwrap();
        for line in first.iter().chain([&user("two")]) {
            writeln!(file, "{line}").unwrap();
        }
        let next = source.read(Some(&page.cursor), None).unwrap();
        assert_eq!(next.messages.len(), 1);
        assert_eq!(next.messages[0].role, TranscriptRole::User);
        assert_eq!(next.messages[0].position, held);
        // A rewind that leaves the cursor inside a line reads from the start.
        let mut file = std::fs::File::create(&path).unwrap();
        writeln!(file, "{}", user("a much longer first prompt than before")).unwrap();
        writeln!(file, "{}", user("three")).unwrap();
        let rewound = source.read(Some(&next.cursor), None).unwrap();
        assert_eq!(rewound.messages.len(), 2);
    }
}
