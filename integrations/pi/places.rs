//! Where Pi works, from its conversation file: the header names the folder
//! the conversation runs in (`cwd`, which its tools' relative paths are
//! from), and the assistant's tool calls the files it reads and changes
//! (`read`, `write`, `edit`), the roots it searches (`grep`, `find`, `ls`)
//! and its shell commands. Calls a codemode script made are recorded on its
//! tool result (`nestedCalls`) and count the same. Pi strips a leading `@`
//! from paths.
use super::{resolve, Collect, Touch};
use serde_json::Value;
use std::path::{Path, PathBuf};

/// The touches of one conversation entry; the header sets `cwd`.
pub(super) fn entry(
    entry: &Value,
    cwd: &mut Option<PathBuf>,
    home: Option<&Path>,
    at: Option<i64>,
    touches: &mut Vec<Touch>,
) {
    let header = entry.get("type").and_then(Value::as_str) == Some("session");
    let own = header
        .then(|| entry.get("cwd").and_then(Value::as_str))
        .flatten();
    if let Some(dir) = own.and_then(|dir| resolve(dir, None, home)) {
        *cwd = Some(dir);
    }
    let mut collect = Collect::new(cwd.as_deref(), home);
    collect.at = at;
    if let Some(dir) = own {
        collect.dir(dir);
    }
    let message = (entry.get("type").and_then(Value::as_str) == Some("message"))
        .then(|| entry.get("message"))
        .flatten();
    match message.and_then(|m| m.get("role")).and_then(Value::as_str) {
        Some("assistant") => {
            let blocks = message
                .and_then(|m| m.get("content"))
                .and_then(Value::as_array);
            for block in blocks.into_iter().flatten() {
                if block.get("type").and_then(Value::as_str) != Some("toolCall") {
                    continue;
                }
                if let (Some(name), Some(arguments)) = (
                    block.get("name").and_then(Value::as_str),
                    block.get("arguments"),
                ) {
                    call(name, arguments, &mut collect);
                }
            }
        }
        Some("toolResult") => {
            let nested = message
                .and_then(|m| m.get("nestedCalls"))
                .and_then(|n| n.get("calls"))
                .and_then(Value::as_array);
            for nested in nested.into_iter().flatten() {
                if let (Some(name), Some(arguments)) = (
                    nested.get("name").and_then(Value::as_str),
                    nested.get("arguments"),
                ) {
                    call(name, arguments, &mut collect);
                }
            }
        }
        _ => {}
    }
    touches.extend(collect.touches);
}

/// The touches of one tool call, `name` with `arguments`.
fn call(name: &str, arguments: &Value, collect: &mut Collect) {
    let path = || {
        arguments
            .get("path")
            .and_then(Value::as_str)
            .map(|path| path.strip_prefix('@').unwrap_or(path))
    };
    match name {
        "read" => {
            if let Some(file) = path() {
                collect.file(file, false);
            }
        }
        "write" | "edit" => {
            if let Some(file) = path() {
                collect.file(file, true);
            }
        }
        "grep" | "find" | "ls" => {
            if let Some(dir) = path() {
                collect.dir(dir);
            }
        }
        "bash" => {
            if let Some(command) = arguments.get("command").and_then(Value::as_str) {
                collect.command(command);
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
        let mut cwd = None;
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

    fn assistant(calls: Value) -> Value {
        json!({"type": "message", "id": "a", "message": {"role": "assistant", "content": calls,
            "stopReason": "toolUse"}})
    }

    #[test]
    fn its_folder_files_searches_and_commands_are_touches() {
        assert_eq!(
            touches(&[
                json!({"type": "session", "version": 3, "id": "c", "cwd": "/repo"}),
                json!({"type": "message", "id": "u", "message": {"role": "user",
                    "content": "look at /etc/passwd"}}),
                assistant(json!([
                    {"type": "text", "text": "Looking at /etc/hosts"},
                    {"type": "toolCall", "id": "1", "name": "read", "arguments": {"path": "src/a.rs"}},
                    {"type": "toolCall", "id": "2", "name": "write",
                        "arguments": {"path": "@/wt/x/b.rs", "content": "secret"}},
                    {"type": "toolCall", "id": "3", "name": "edit",
                        "arguments": {"path": "~/notes/c.md", "edits": []}},
                    {"type": "toolCall", "id": "4", "name": "grep", "arguments": {"pattern": "x", "path": "tui"}},
                    {"type": "toolCall", "id": "5", "name": "ls", "arguments": {}},
                    {"type": "toolCall", "id": "6", "name": "bash",
                        "arguments": {"command": "cd ../other && git -C /abs status"}},
                    {"type": "toolCall", "id": "7", "name": "webfetch", "arguments": {"path": "/no"}}
                ])),
            ]),
            [
                "/repo",
                "/repo/src",
                "/wt/x (edited)",
                "/home/me/notes (edited)",
                "/repo/tui",
                "/other",
                "/abs",
            ]
        );
    }

    #[test]
    fn calls_a_codemode_script_made_count_too() {
        assert_eq!(
            touches(&[
                json!({"type": "session", "version": 3, "id": "c", "cwd": "/repo"}),
                json!({"type": "message", "id": "r", "message": {"role": "toolResult",
                "toolCallId": "1", "toolName": "codemode", "content": [], "isError": false,
                "nestedCalls": {"complete": true, "calls": [
                    {"id": "1/1", "name": "write", "status": "ok",
                        "arguments": {"path": "gen/out.rs", "content": "x"}},
                    {"id": "1/2", "name": "read", "status": "ok", "argumentsBytes": 9000}
                ]}}}),
            ]),
            ["/repo", "/repo/gen (edited)"]
        );
    }

    #[test]
    fn without_a_header_relative_paths_are_unknown() {
        assert_eq!(
            touches(&[assistant(json!([
                {"type": "toolCall", "id": "1", "name": "write", "arguments": {"path": "a.rs"}},
                {"type": "toolCall", "id": "2", "name": "write", "arguments": {"path": "/abs/b.rs"}}
            ]))]),
            ["/abs (edited)"]
        );
    }
}
