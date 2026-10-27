//! Where Claude Code works, from its transcript: every entry records the
//! directory it is in (`cwd`, which follows its Bash `cd`s), and its tool
//! calls name the files it reads and changes, the roots it searches, the
//! worktrees it enters and the commands it runs. Subagents' entries count:
//! they work for the same session.
use super::{resolve, Collect, Touch};
use serde_json::Value;
use std::path::{Path, PathBuf};

/// The touches of one transcript entry; `cwd` follows the entry's own.
pub(super) fn entry(
    entry: &Value,
    cwd: &mut Option<PathBuf>,
    home: Option<&Path>,
    at: Option<i64>,
    touches: &mut Vec<Touch>,
) {
    let own = entry.get("cwd").and_then(Value::as_str);
    if let Some(dir) = own.and_then(|dir| resolve(dir, None, home)) {
        *cwd = Some(dir);
    }
    let mut collect = Collect::new(cwd.as_deref(), home);
    collect.at = at;
    if let Some(dir) = own {
        collect.dir(dir);
    }
    let blocks = (entry.get("type").and_then(Value::as_str) == Some("assistant"))
        .then(|| entry.get("message")?.get("content")?.as_array())
        .flatten();
    for block in blocks.into_iter().flatten() {
        if block.get("type").and_then(Value::as_str) != Some("tool_use") {
            continue;
        }
        let Some(name) = block.get("name").and_then(Value::as_str) else {
            continue;
        };
        let field = |key: &str| block.get("input")?.get(key)?.as_str();
        match name {
            "Read" | "NotebookRead" => {
                if let Some(file) = field("file_path").or_else(|| field("notebook_path")) {
                    collect.file(file, false);
                }
            }
            "Write" | "Edit" | "MultiEdit" | "NotebookEdit" => {
                if let Some(file) = field("file_path").or_else(|| field("notebook_path")) {
                    collect.file(file, true);
                }
            }
            "Grep" | "Glob" | "LS" | "EnterWorktree" => {
                if let Some(dir) = field("path") {
                    collect.dir(dir);
                }
            }
            "Bash" => {
                if let Some(command) = field("command") {
                    collect.command(command);
                }
            }
            _ => {}
        }
    }
    touches.extend(collect.touches);
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

    #[test]
    fn its_directory_files_searches_and_commands_are_touches() {
        let assistant = |cwd: &str, tools: Value| json!({"type": "assistant", "cwd": cwd, "message": {"content": tools}});
        assert_eq!(
            touches(&[
                json!({"type": "user", "cwd": "/repo", "message": {"content": "go"}}),
                assistant(
                    "/repo",
                    json!([
                        {"type": "text", "text": "Looking at /etc/passwd"},
                        {"type": "tool_use", "name": "Read", "input": {"file_path": "/repo/src/a.rs"}},
                        {"type": "tool_use", "name": "Edit", "input": {"file_path": "/wt/x/b.rs"}},
                        {"type": "tool_use", "name": "Grep", "input": {"pattern": "x", "path": "tui"}},
                        {"type": "tool_use", "name": "Bash", "input": {"command": "cd ~/other && ls"}},
                        {"type": "tool_use", "name": "WebFetch", "input": {"url": "https://x.y/z"}}
                    ])
                ),
                // A later entry in another directory: relative paths are its.
                assistant(
                    "/repo/worktrees/fix",
                    json!([{"type": "tool_use", "name": "Write", "input": {"file_path": "new/c.rs"}}])
                ),
            ]),
            [
                "/repo",
                "/repo",
                "/repo/src",
                "/wt/x (edited)",
                "/repo/tui",
                "/home/me/other",
                "/repo/worktrees/fix",
                "/repo/worktrees/fix/new (edited)"
            ]
        );
    }
}
