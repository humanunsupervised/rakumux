//! Where Kimi Code works, from its main agent's log: its tool calls' files
//! (read, written, edited) and search roots, and its shell commands with
//! their working directories, as Kimi resolved them for its UI (`display`;
//! the arguments when it has none), plus `file_history.tracked`, which names
//! every file the main agent or one of its subagents is about to change
//! (relative to the session's folder). Relative paths are the Kimi process's
//! directory's: it never changes it.
use super::super::kimi::loop_event;
use super::{Collect, Touch};
use serde_json::Value;
use std::path::{Path, PathBuf};

/// The touches of one log record.
pub(super) fn entry(
    entry: &Value,
    cwd: &mut Option<PathBuf>,
    home: Option<&Path>,
    at: Option<i64>,
    touches: &mut Vec<Touch>,
) {
    let mut collect = Collect::new(cwd.as_deref(), home);
    collect.at = at;
    if entry.get("type").and_then(Value::as_str) == Some("file_history.tracked") {
        if let Some(file) = entry.get("path").and_then(Value::as_str) {
            collect.file(file, true);
        }
    } else if let Some(event) = loop_event(entry)
        .filter(|event| event.get("type").and_then(Value::as_str) == Some("tool.call"))
    {
        call(event, &mut collect);
    }
    touches.extend(collect.touches);
}

/// The touches of a tool call.
fn call(event: &Value, collect: &mut Collect) {
    let display = event.get("display");
    let shown = |key: &str| display?.get(key)?.as_str();
    let arg = |key: &str| event.get("args")?.get(key)?.as_str();
    let path = shown("path").or_else(|| arg("path"));
    match (
        shown("kind"),
        shown("operation"),
        event.get("name").and_then(Value::as_str),
    ) {
        (Some("file_io"), Some("write" | "edit"), _) | (None, _, Some("Write" | "Edit")) => {
            if let Some(file) = path {
                collect.file(file, true);
            }
        }
        (Some("file_io"), Some("read"), _) | (None, _, Some("Read")) => {
            if let Some(file) = path {
                collect.file(file, false);
            }
        }
        (Some("file_io"), Some("grep" | "glob"), _) | (None, _, Some("Grep" | "Glob")) => {
            if let Some(dir) = path {
                collect.dir(dir);
            }
        }
        (Some("command"), _, _) | (None, _, Some("Bash")) => {
            let dir = shown("cwd")
                .or_else(|| arg("cwd"))
                .and_then(|dir| collect.workdir(dir));
            if let Some(command) = shown("command").or_else(|| arg("command")) {
                match dir {
                    Some(dir) => collect.command_in(command, Some(&dir)),
                    None => collect.command(command),
                }
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn touches(entries: &[Value]) -> Vec<String> {
        let mut cwd = Some(PathBuf::from("/repo"));
        let mut touches = Vec::new();
        for e in entries {
            entry(
                e,
                &mut cwd,
                Some(Path::new("/home/me")),
                Some(5),
                &mut touches,
            );
        }
        assert!(touches.iter().all(|touch| touch.at == Some(5)));
        touches
            .iter()
            .map(|t| {
                format!(
                    "{}{}",
                    t.dir.display(),
                    if t.edited { " (edited)" } else { "" }
                )
            })
            .collect()
    }

    fn tool(name: &str, args: Value, display: Value) -> Value {
        json!({"type": "context.append_loop_event", "agentId": "main", "time": 5,
            "event": {"type": "tool.call", "uuid": "u", "toolCallId": "c", "name": name,
                "args": args, "display": display}})
    }

    #[test]
    fn files_searches_and_commands_are_touches() {
        assert_eq!(
            touches(&[
                tool(
                    "Write",
                    json!({"path": "sub/new.txt", "content": "x"}),
                    json!({"kind": "file_io", "operation": "write",
                        "path": "/repo/sub/new.txt", "content": "x"})
                ),
                tool(
                    "Edit",
                    json!({"path": "/wt/a/src/lib.rs", "old_string": "x", "new_string": "y"}),
                    json!({"kind": "file_io", "operation": "edit", "path": "/wt/a/src/lib.rs"})
                ),
                tool(
                    "Read",
                    json!({"path": "docs/a.md"}),
                    json!({"kind": "file_io", "operation": "read", "path": "/repo/docs/a.md"})
                ),
                tool(
                    "Grep",
                    json!({"pattern": "fn main", "path": "/other"}),
                    json!({"kind": "file_io", "operation": "grep", "path": "/other"})
                ),
                tool(
                    "Bash",
                    json!({"command": "cd sub && git -C ../b status"}),
                    json!({"kind": "command", "command": "cd sub && git -C ../b status",
                        "cwd": "/wt/c", "language": "bash"})
                ),
                // A subagent's or the main agent's edit, as Kimi tracks it.
                json!({"type": "file_history.tracked", "agentId": "main", "turnId": 0,
                    "path": "tui/src/app.rs", "entry": {"key": null, "version": 1}}),
            ]),
            [
                "/repo/sub (edited)",
                "/wt/a/src (edited)",
                "/repo/docs",
                "/other",
                "/wt/c",
                "/wt/c/sub",
                "/wt/c/b",
                "/repo/tui/src (edited)",
            ]
        );
    }

    #[test]
    fn without_a_display_the_arguments_say_and_other_records_say_nothing() {
        assert_eq!(
            touches(&[
                tool("Write", json!({"path": "a/b.rs"}), Value::Null),
                tool(
                    "Bash",
                    json!({"command": "cd /x", "cwd": "/y"}),
                    Value::Null
                ),
                tool("TodoList", json!({}), json!({"kind": "todo"})),
                json!({"type": "context.append_loop_event", "agentId": "main",
                    "event": {"type": "tool.result", "toolCallId": "c",
                    "result": {"output": "/secret/path"}}}),
                json!({"type": "context.append_message", "agentId": "main",
                    "message": {"role": "user", "origin": {"kind": "user"},
                    "content": [{"type": "text", "text": "cd /prompt"}]}}),
            ]),
            ["/repo/a (edited)", "/y", "/x"]
        );
    }
}
