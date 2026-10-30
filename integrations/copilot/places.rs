//! Where GitHub Copilot CLI works, from its event log: the session's
//! directory (`session.start` and `session.resume`'s `context.cwd`, and each
//! `/cd`'s `session.context_changed`), its tools' paths, search roots and
//! shell commands (`tool.execution_start`), and the files its tools changed
//! (`tool.execution_complete`'s `fileEdits`: what was written, not what was
//! asked). Subagents' events count: they work for the same session.
use super::{resolve, Collect, Touch};
use serde_json::Value;
use std::path::{Path, PathBuf};

/// The touches of one event; `cwd` follows the session's own.
pub(super) fn entry(
    entry: &Value,
    cwd: &mut Option<PathBuf>,
    home: Option<&Path>,
    at: Option<i64>,
    touches: &mut Vec<Touch>,
) {
    let (Some(kind), Some(data)) = (entry.get("type").and_then(Value::as_str), entry.get("data"))
    else {
        return;
    };
    let own = match kind {
        "session.start" | "session.resume" => data.get("context").and_then(|c| c.get("cwd")),
        "session.context_changed" => data.get("cwd"),
        _ => None,
    }
    .and_then(Value::as_str);
    if let Some(dir) = own.and_then(|dir| resolve(dir, None, home)) {
        *cwd = Some(dir);
    }
    let mut collect = Collect::new(cwd.as_deref(), home);
    collect.at = at;
    if let Some(dir) = own {
        collect.dir(dir);
    }
    match kind {
        "tool.execution_start" => {
            let arguments = super::super::copilot::arguments(data.get("arguments"));
            let field = |key: &str| arguments.get(key).and_then(Value::as_str);
            match data.get("toolName").and_then(Value::as_str) {
                Some("view" | "create" | "edit" | "str_replace_editor") => {
                    if let Some(file) = field("path") {
                        collect.file(file, false);
                    }
                }
                Some("grep" | "glob") => {
                    if let Some(dir) = field("path") {
                        collect.dir(dir);
                    }
                }
                Some("bash" | "powershell") => {
                    if let Some(command) = field("command") {
                        collect.command(command);
                    }
                    let paths = data
                        .get("shellToolInfo")
                        .and_then(|info| info.get("possiblePaths"))
                        .and_then(Value::as_array);
                    for path in paths.into_iter().flatten().filter_map(Value::as_str) {
                        collect.file(path, false);
                    }
                }
                _ => {}
            }
        }
        "tool.execution_complete" => {
            let edits = data.get("fileEdits").and_then(Value::as_array);
            for edit in edits.into_iter().flatten() {
                if let Some(file) = edit.get("path").and_then(Value::as_str) {
                    collect.file(file, true);
                }
            }
        }
        _ => {}
    }
    touches.extend(collect.touches);
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn event(kind: &str, data: Value) -> Value {
        json!({"type": kind, "data": data, "id": "e", "timestamp": "2026-10-23T10:00:00Z"})
    }

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
            .map(|touch| {
                format!(
                    "{}{}",
                    touch.dir.display(),
                    if touch.edited { " (edited)" } else { "" }
                )
            })
            .collect()
    }

    #[test]
    fn the_sessions_directory_follows_its_start_resume_and_cds() {
        assert_eq!(
            touches(&[
                event(
                    "session.start",
                    json!({"sessionId": "s", "version": 1,
                        "context": {"cwd": "/home/me/project", "gitRoot": "/home/me/project", "branch": "main"}})
                ),
                event(
                    "tool.execution_start",
                    json!({"toolCallId": "c1", "toolName": "view", "arguments": {"path": "src/lib.rs"}})
                ),
                event(
                    "session.context_changed",
                    json!({"cwd": "/home/me/project/worktrees/fix", "branch": "fix"})
                ),
                event(
                    "tool.execution_start",
                    json!({"toolCallId": "c2", "toolName": "grep", "arguments": {"pattern": "x", "path": "tests"}})
                ),
                event(
                    "session.resume",
                    json!({"resumeTime": "t", "eventCount": 9, "context": {"cwd": "/home/me/other"}})
                ),
            ]),
            [
                "/home/me/project",
                "/home/me/project/src",
                "/home/me/project/worktrees/fix",
                "/home/me/project/worktrees/fix/tests",
                "/home/me/other",
            ]
        );
    }

    #[test]
    fn files_its_tools_changed_are_edits_and_what_they_asked_isnt() {
        assert_eq!(
            touches(&[
                event(
                    "session.start",
                    json!({"context": {"cwd": "/home/me/project"}})
                ),
                event(
                    "tool.execution_start",
                    json!({"toolCallId": "c1", "toolName": "create",
                        "arguments": {"path": "/home/me/project/notes/todo.txt", "file_text": "secret"}})
                ),
                // It failed: nothing changed.
                event(
                    "tool.execution_complete",
                    json!({"toolCallId": "c1", "success": false, "error": {"message": "Parent directory does not exist"}})
                ),
                event(
                    "tool.execution_complete",
                    json!({"toolCallId": "c2", "success": true, "fileEdits": [
                        {"path": "/home/me/project/src/app.rs", "kind": "edit"},
                        {"path": "/home/me/elsewhere/new.rs", "kind": "create"}]})
                ),
                // A subagent's edit counts.
                json!({"type": "tool.execution_complete", "agentId": "child", "data": {"toolCallId": "c3",
                    "success": true, "fileEdits": [{"path": "/home/me/project/docs/a.md", "kind": "edit"}]}}),
            ]),
            [
                "/home/me/project",
                "/home/me/project/notes",
                "/home/me/project/src (edited)",
                "/home/me/elsewhere (edited)",
                "/home/me/project/docs (edited)",
            ]
        );
    }

    #[test]
    fn shell_calls_name_their_cds_and_possible_paths() {
        assert_eq!(
            touches(&[
                event(
                    "session.start",
                    json!({"context": {"cwd": "/home/me/project"}})
                ),
                event(
                    "tool.execution_start",
                    json!({"toolCallId": "c1", "toolName": "bash",
                        "arguments": {"command": "cd tui && cargo test", "description": "Run the tests"},
                        "shellToolInfo": {"possiblePaths": ["/home/me/project/out.txt"], "hasWriteFileRedirection": true}})
                ),
            ]),
            [
                "/home/me/project",
                "/home/me/project/tui",
                "/home/me/project",
            ]
        );
    }
}
