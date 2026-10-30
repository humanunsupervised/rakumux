//! Where Cursor works, from its transcript's tool calls: the files its file
//! tools write, edit, delete or read, the roots it searches and lists, and
//! its Shell calls' directories and commands. Its lines record no directory
//! of their own, so relative paths are the workspace's (the process's
//! directory). Names cover the tools Cursor's models call (`StrReplace`,
//! `Write`, `Shell`, …) and their older names (`edit_file`,
//! `run_terminal_cmd`, …); others are passed over.
use super::{Collect, Touch};
use serde_json::Value;
use std::path::{Path, PathBuf};

/// Tools that change the file they name.
const EDITS: &[&str] = &[
    "write",
    "strreplace",
    "str_replace",
    "search_replace",
    "edit",
    "multiedit",
    "edit_file",
    "edit_file_v2",
    "editnotebook",
    "edit_notebook",
    "delete",
    "delete_file",
];
/// Tools that read the file they name.
const READS: &[&str] = &[
    "read",
    "read_file",
    "readfile",
    "read_file_v2",
    "readlints",
    "read_lints",
];
/// Tools that search or list a directory.
const SEARCHES: &[&str] = &[
    "grep",
    "grep_search",
    "ripgrep",
    "glob",
    "glob_file_search",
    "ls",
    "list",
    "list_dir",
    "list_dir_v2",
    "semanticsearch",
    "codebase_search",
];
/// Tools that run a shell command.
const SHELLS: &[&str] = &[
    "shell",
    "run_terminal_cmd",
    "run_terminal_command",
    "bash",
    "terminal",
];
/// Tools that apply a patch (`*** Update File: …`).
const PATCHES: &[&str] = &["applypatch", "apply_patch"];

/// The touches of one transcript line; `cwd` is the workspace's.
pub(super) fn entry(
    entry: &Value,
    cwd: &mut Option<PathBuf>,
    home: Option<&Path>,
    at: Option<i64>,
    touches: &mut Vec<Touch>,
) {
    if entry.get("role").and_then(Value::as_str) != Some("assistant") {
        return;
    }
    let mut collect = Collect::new(cwd.as_deref(), home);
    collect.at = at;
    let blocks = entry
        .get("message")
        .and_then(|message| message.get("content"))
        .and_then(Value::as_array);
    for block in blocks.into_iter().flatten() {
        if block.get("type").and_then(Value::as_str) != Some("tool_use") {
            continue;
        }
        let Some(name) = block.get("name").and_then(Value::as_str) else {
            continue;
        };
        let name = name.to_ascii_lowercase();
        let input = block.get("input").unwrap_or(&Value::Null);
        let field = |key: &str| input.get(key).and_then(Value::as_str);
        let file = [
            "path",
            "file_path",
            "target_file",
            "target_notebook",
            "notebook_path",
        ]
        .into_iter()
        .find_map(field);
        let name = name.as_str();
        if EDITS.contains(&name) || READS.contains(&name) {
            if let Some(file) = file {
                collect.file(file, EDITS.contains(&name));
            }
        } else if SEARCHES.contains(&name) {
            let dir = ["target_directory", "path", "directory", "dir_path"]
                .into_iter()
                .find_map(field);
            dir.into_iter().for_each(|dir| collect.dir(dir));
            let dirs = input.get("target_directories").and_then(Value::as_array);
            for dir in dirs.into_iter().flatten().filter_map(Value::as_str) {
                collect.dir(dir);
            }
        } else if SHELLS.contains(&name) {
            let workdir = ["working_directory", "cwd", "workdir"]
                .into_iter()
                .find_map(field)
                .and_then(|dir| collect.workdir(dir));
            if let Some(command) = field("command") {
                match &workdir {
                    Some(dir) => collect.command_in(command, Some(dir.as_path())),
                    None => collect.command(command),
                }
            }
        } else if PATCHES.contains(&name) {
            let patch = input
                .as_str()
                .or_else(|| ["patch", "input", "diff"].into_iter().find_map(field));
            if let Some(patch) = patch {
                collect.patch(patch);
            }
        }
    }
    touches.extend(collect.touches);
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn its_file_tools_searches_and_shell_calls_are_touches() {
        let mut cwd = Some(PathBuf::from("/repo"));
        let mut touches = Vec::new();
        let line = json!({"role": "assistant", "message": {"content": [
            {"type": "text", "text": "Looking at /etc/passwd"},
            {"type": "tool_use", "name": "Read", "input": {"path": "/repo/src/a.rs"}},
            {"type": "tool_use", "name": "StrReplace", "input": {"path": "/wt/x/b.rs", "old_string": "a", "new_string": "b"}},
            {"type": "tool_use", "name": "Write", "input": {"path": "new/c.rs", "contents": "fn main() {}"}},
            {"type": "tool_use", "name": "Delete", "input": {"path": "/repo/old/d.rs"}},
            {"type": "tool_use", "name": "Grep", "input": {"pattern": "x", "path": "tui"}},
            {"type": "tool_use", "name": "Glob", "input": {"glob_pattern": "*.rs", "target_directory": "/repo/landing"}},
            {"type": "tool_use", "name": "Shell", "input": {"command": "cd sub && cargo test", "working_directory": "/repo/tui"}},
            {"type": "tool_use", "name": "run_terminal_cmd", "input": {"command": "git -C ~/other status"}},
            {"type": "tool_use", "name": "WebFetch", "input": {"url": "https://x.y/z"}},
            {"type": "tool_use", "name": "ApplyPatch", "input": "*** Begin Patch\n*** Update File: lib/e.rs\n*** End Patch"}
        ]}});
        entry(
            &line,
            &mut cwd,
            Some(Path::new("/home/me")),
            None,
            &mut touches,
        );
        // A user line names nothing, whatever it says.
        let prompt = json!({"role": "user", "message": {"content": [{"type": "text", "text": "edit /x/y.rs"}]}});
        entry(&prompt, &mut cwd, None, None, &mut touches);
        let seen: Vec<String> = touches
            .iter()
            .map(|t| {
                format!(
                    "{}{}",
                    t.dir.display(),
                    if t.edited { " (edited)" } else { "" }
                )
            })
            .collect();
        assert_eq!(
            seen,
            [
                "/repo/src",
                "/wt/x (edited)",
                "/repo/new (edited)",
                "/repo/old (edited)",
                "/repo/tui",
                "/repo/landing",
                "/repo/tui",
                "/repo/tui/sub",
                "/home/me/other",
                "/repo/lib (edited)",
            ]
        );
    }
}
