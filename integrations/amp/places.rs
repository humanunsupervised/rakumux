//! Where Amp works, from the copy of its thread the Rakumux plugin keeps:
//! each message records Amp's workspace (`cwd`), and its tool calls name the
//! files they read and change, the roots they search, and the commands they
//! run in their own directory. The plugin adds the files Amp itself says a
//! call changes (`files`: an edit, a created file, a patch, a `sed -i`);
//! without them, the call's own paths are used.
use super::{resolve, Collect, Touch};
use serde_json::Value;
use std::path::{Path, PathBuf};

/// The touches of one line; `cwd` follows the message's own.
pub(super) fn entry(
    entry: &Value,
    cwd: &mut Option<PathBuf>,
    home: Option<&Path>,
    at: Option<i64>,
    touches: &mut Vec<Touch>,
) {
    if entry.get("kind").and_then(Value::as_str) != Some("message") {
        return;
    }
    let own = entry.get("cwd").and_then(Value::as_str);
    if let Some(dir) = own.and_then(|dir| resolve(dir, None, home)) {
        *cwd = Some(dir);
    }
    let mut collect = Collect::new(cwd.as_deref(), home);
    collect.at = at;
    if let Some(dir) = own {
        collect.dir(dir);
    }
    let blocks = (entry.get("role").and_then(Value::as_str) == Some("assistant"))
        .then(|| entry.get("content")?.as_array())
        .flatten();
    for block in blocks.into_iter().flatten() {
        if block.get("type").and_then(Value::as_str) != Some("tool_use") {
            continue;
        }
        let Some(name) = block.get("name").and_then(Value::as_str) else {
            continue;
        };
        let input = block.get("input").unwrap_or(&Value::Null);
        call(name, input, block.get("files"), &mut collect);
    }
    touches.extend(collect.touches);
}

/// The touches of one tool call, `name` with `input`; `files`, the files
/// Amp says it changes, when the plugin recorded them.
fn call(name: &str, input: &Value, files: Option<&Value>, collect: &mut Collect) {
    let field = |key: &str| input.get(key)?.as_str();
    let changed: Option<Vec<&str>> = files
        .and_then(Value::as_array)
        .map(|files| files.iter().filter_map(Value::as_str).collect());
    if let Some(changed) = &changed {
        for file in changed {
            collect.file(file, true);
        }
    }
    match name {
        "Read" | "read_file" => {
            if let Some(file) = field("path") {
                collect.file(file, false);
            }
        }
        "edit_file" | "create_file" | "undo_edit" | "format_file" if changed.is_none() => {
            if let Some(file) = field("path") {
                collect.file(file, true);
            }
        }
        "apply_patch" if changed.is_none() => {
            if let Some(patch) = field("patchText") {
                collect.patch(patch);
            }
        }
        "Grep" | "glob" | "list_directory" => {
            if let Some(dir) = field("path") {
                collect.dir(dir);
            }
        }
        "Bash" | "shell_command" => {
            let (command, dir) = match name {
                "Bash" => (field("cmd"), field("cwd")),
                _ => (field("command"), field("workdir")),
            };
            let dir = dir.and_then(|dir| collect.workdir(dir));
            if let Some(command) = command {
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
    use super::super::{LogAgent, PlaceScan, TranscriptSource};
    use super::*;
    use serde_json::json;

    const FIXTURE: &str = include_str!("fixtures/thread.jsonl");

    fn dirs(touches: &[Touch]) -> Vec<String> {
        touches
            .iter()
            .map(|t| format!("{}{}", t.dir.display(), if t.edited { "*" } else { "" }))
            .collect()
    }

    #[test]
    fn a_threads_edits_patches_commands_and_searches_are_touches() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("thread.jsonl");
        std::fs::write(&path, FIXTURE).unwrap();
        let source = TranscriptSource::Log {
            agent: LogAgent::Amp,
            conversation: "T-1".into(),
            path,
        };
        let mut scan = PlaceScan::new(source, None);
        let touches = scan.read(1 << 20);
        assert!(!scan.behind());
        let worked: Vec<String> = dirs(&touches)
            .into_iter()
            .filter(|dir| dir != "/repo")
            .collect();
        assert_eq!(
            worked,
            [
                // edit_file, from the files Amp named.
                "/repo/src*",
                // Bash in /repo: `sed -i` changed lib.rs in its `cd`.
                "/repo/crates/parser*",
                "/repo/crates/parser",
                // apply_patch without files: its own paths.
                "/repo/src*",
                // Grep's root.
                "/repo/src",
            ]
        );
        assert!(dirs(&touches).iter().filter(|dir| *dir == "/repo").count() >= 8);
        // The edit's time is its message's.
        assert_eq!(dirs(&touches[2..3]), ["/repo/src*"]);
        assert_eq!(touches[2].at, Some(1_792_749_610_000));
    }

    #[test]
    fn relative_paths_are_the_workspaces_and_other_messages_touch_nothing() {
        let mut cwd = None;
        let mut touches = Vec::new();
        let message = json!({"v": 1, "kind": "message", "id": "a", "role": "assistant",
            "cwd": "/w", "content": [
                {"type": "tool_use", "id": "1", "name": "create_file",
                    "input": {"path": "notes/todo.md", "content": "x"}},
                {"type": "tool_use", "id": "2", "name": "Read", "input": {"path": "/other/README.md"}},
                {"type": "tool_use", "id": "3", "name": "shell_command",
                    "input": {"command": "git -C ../lib status", "workdir": "/w/app"}},
                {"type": "tool_use", "id": "4", "name": "web_search", "input": {"query": "rust"}}]});
        entry(&message, &mut cwd, None, None, &mut touches);
        assert_eq!(
            dirs(&touches),
            ["/w", "/w/notes*", "/other", "/w/app", "/w/lib"]
        );
        let end = json!({"v": 1, "kind": "turn_end", "id": "a", "status": "done", "cwd": "/x"});
        let mut none = Vec::new();
        entry(&end, &mut cwd, None, None, &mut none);
        assert!(none.is_empty());
        assert_eq!(cwd.as_deref(), Some(Path::new("/w")));
    }
}
