//! Where Droid works, from its session file: the session's directories (its
//! summary's `cwd`, and `lastCwd` once `/cwd` moved it), and its tool calls'
//! files (`Create`, `Edit` and `ApplyPatch` change them), search roots and
//! `Execute` commands (`cd`, `git -C`), which run in its directory. A Task
//! subagent's calls are in its own session file and aren't read.
use super::{resolve, Collect, Touch};
use serde_json::Value;
use std::path::{Path, PathBuf};

/// The touches of one line of the session file; the summary's directory is
/// where relative paths are from.
pub(super) fn entry(
    entry: &Value,
    cwd: &mut Option<PathBuf>,
    home: Option<&Path>,
    at: Option<i64>,
    touches: &mut Vec<Touch>,
) {
    let summary = entry.get("type").and_then(Value::as_str) == Some("session_start");
    let dirs =
        ["cwd", "lastCwd"].map(|key| entry.get(key).and_then(Value::as_str).filter(|_| summary));
    if let Some(dir) = dirs
        .into_iter()
        .rev()
        .flatten()
        .find_map(|dir| resolve(dir, None, home))
    {
        *cwd = Some(dir);
    }
    let mut collect = Collect::new(cwd.as_deref(), home);
    collect.at = at;
    match entry.get("type").and_then(Value::as_str) {
        Some("session_start") => {
            for dir in dirs.into_iter().flatten() {
                collect.dir(dir);
            }
        }
        Some("message") => {
            let message = entry.get("message");
            let assistant = message
                .and_then(|message| message.get("role"))
                .and_then(Value::as_str)
                == Some("assistant");
            let blocks = message
                .filter(|_| assistant)
                .and_then(|message| message.get("content"))
                .and_then(Value::as_array);
            for block in blocks.into_iter().flatten() {
                if block.get("type").and_then(Value::as_str) == Some("tool_use") {
                    call(block, &mut collect);
                }
            }
        }
        _ => {}
    }
    touches.extend(collect.touches);
}

/// One tool call's paths.
fn call(block: &Value, collect: &mut Collect) {
    let Some(name) = block.get("name").and_then(Value::as_str) else {
        return;
    };
    let input = block.get("input");
    let field = |key: &str| input?.get(key)?.as_str();
    match name {
        "Read" => {
            if let Some(file) = field("file_path") {
                collect.file(file, false);
            }
        }
        "Create" | "Edit" | "MultiEdit" | "Write" => {
            if let Some(file) = field("file_path") {
                collect.file(file, true);
            }
        }
        "ApplyPatch" => {
            if let Some(patch) = field("input") {
                collect.patch(patch);
            }
        }
        "LS" => {
            if let Some(dir) = field("directory_path") {
                collect.dir(dir);
            }
        }
        "Grep" => {
            if let Some(dir) = field("path") {
                collect.dir(dir);
            }
        }
        "Glob" => {
            if let Some(dir) = field("folder") {
                collect.dir(dir);
            }
        }
        "Execute" => {
            if let Some(command) = field("command") {
                collect.command(command);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::super::super::droid::tests::{append, message, retitle, write_session, ID};
    use super::super::{LogAgent, PlaceScan, TranscriptSource};
    use super::*;
    use serde_json::json;

    fn shown(touches: &[Touch]) -> Vec<String> {
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

    fn call(name: &str, input: Value) -> Value {
        json!({"type": "tool_use", "id": "c", "name": name, "input": input})
    }

    #[test]
    fn its_directories_files_searches_patches_and_commands_are_touches() {
        let mut cwd = None;
        let mut touches = Vec::new();
        let home = Some(Path::new("/home/me"));
        for line in [
            json!({"type": "session_start", "id": ID, "title": "t", "cwd": "/repo",
                "lastCwd": "/repo/worktrees/fix"}),
            message("u", "user", json!("Look at /etc/passwd and cd /nowhere")),
            message(
                "a",
                "assistant",
                json!([
                    {"type": "text", "text": "Looking at /etc/hosts"},
                    call("Read", json!({"file_path": "/repo/src/a.rs"})),
                    call("Create", json!({"file_path": "new/b.rs", "content": "x"})),
                    call("Edit", json!({"file_path": "/wt/x/c.rs", "old_str": "a", "new_str": "b"})),
                    call("ApplyPatch", json!({"input": "*** Begin Patch\n*** Update File: /repo/src/d.rs\n@@\n-a\n+b\n*** End Patch"})),
                    call("LS", json!({"directory_path": "/repo/docs"})),
                    call("Grep", json!({"pattern": "x", "path": "tui"})),
                    call("Glob", json!({"patterns": ["*.rs"], "folder": "/repo/api"})),
                    call("Execute", json!({"command": "cd ~/other && ls", "summary": "List"})),
                    call("FetchUrl", json!({"url": "https://x.y/z"}))
                ]),
            ),
            message(
                "t",
                "user",
                json!([{"type": "tool_result", "tool_use_id": "c", "content": "/etc/shadow"}]),
            ),
        ] {
            entry(&line, &mut cwd, home, Some(5), &mut touches);
        }
        assert_eq!(
            shown(&touches),
            [
                "/repo",
                "/repo/worktrees/fix",
                "/repo/src",
                // Relative paths are from its latest directory.
                "/repo/worktrees/fix/new (edited)",
                "/wt/x (edited)",
                "/repo/src (edited)",
                "/repo/docs",
                "/repo/worktrees/fix/tui",
                "/repo/api",
                "/home/me/other",
            ]
        );
        assert!(touches.iter().all(|touch| touch.at == Some(5)));
    }

    #[test]
    fn a_session_file_is_scanned_from_its_summary_and_on_past_a_new_title() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join(format!("{ID}.jsonl"));
        write_session(
            &path,
            "New session",
            &[message(
                "a1",
                "assistant",
                json!([call("Edit", json!({"file_path": "src/a.rs"}))]),
            )],
        );
        let source = TranscriptSource::Log {
            agent: LogAgent::Droid,
            conversation: ID.into(),
            path: path.clone(),
        };
        let mut scan = PlaceScan::new(source, None);
        assert_eq!(shown(&scan.read(1 << 20)), ["/repo", "/repo/src (edited)"]);
        assert!(!scan.behind());
        retitle(&path, "A much longer title for the session");
        append(
            &path,
            &message(
                "a2",
                "assistant",
                json!([call("Create", json!({"file_path": "/repo/docs/new.md"}))]),
            ),
        );
        assert_eq!(shown(&scan.read(1 << 20)), ["/repo/docs (edited)"]);
        assert!(scan.read(1 << 20).is_empty());
    }
}
