//! Where Grok Build works, from its session's `updates.jsonl`: the files its
//! edits changed (a finished `write`, `search_replace` or `hashline_edit`
//! shows each as a diff with its absolute path), the files it read, the
//! folders it listed or searched, and where each shell call ran and what it
//! `cd`'d into. A subagent works in a session of its own: once it finished
//! (`subagent_finished`), the start of its log is read too, since it works for
//! the same session (in the same folder, or a worktree of its own).
use super::{scan, Collect, LogAgent, Touch};
use serde_json::Value;
use std::path::{Path, PathBuf};

/// The most of a subagent's log read, from its start.
const SUBAGENT_BUDGET: usize = 4 * 1024 * 1024;

/// The touches of one log line; `cwd` is the session's folder.
pub(super) fn entry(
    entry: &Value,
    cwd: &mut Option<PathBuf>,
    home: Option<&Path>,
    at: Option<i64>,
    touches: &mut Vec<Touch>,
) {
    let mut collect = Collect::new(cwd.as_deref(), home);
    collect.at = at;
    if let Some(child) = finished_subagent(entry) {
        let grok = crate::resume::grok_home();
        subagent(child, grok.as_deref(), cwd.as_deref(), home, touches);
        return;
    }
    update(entry, &mut collect);
    touches.extend(collect.touches);
}

/// A tool call's or a finished tool's paths.
fn update(entry: &Value, collect: &mut Collect<'_>) {
    let Some((update, kind)) = super::super::grok::update(entry) else {
        return;
    };
    let input = |key: &str| update.get("rawInput")?.get(key)?.as_str();
    match kind {
        "tool_call" => match super::super::grok::tool(update).0 {
            Some("read_file") => {
                if let Some(file) = input("target_file") {
                    collect.file(file, false);
                }
            }
            Some("list_dir") => {
                if let Some(dir) = input("target_directory") {
                    collect.dir(dir);
                }
            }
            Some("grep") => {
                if let Some(dir) = input("path") {
                    collect.dir(dir);
                }
            }
            _ => {}
        },
        "tool_call_update" if update.get("status").and_then(Value::as_str) == Some("completed") => {
            let diffs = update
                .get("content")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter(|block| block.get("type").and_then(Value::as_str) == Some("diff"));
            for diff in diffs {
                if let Some(file) = diff.get("path").and_then(Value::as_str) {
                    collect.file(file, true);
                }
            }
            let shell = update.get("rawOutput");
            let field = |key: &str| shell?.get(key)?.as_str();
            if let Some(dir) = field("current_dir") {
                let dir = collect.workdir(dir);
                if let Some(command) = field("command") {
                    collect.command_in(command, dir.as_deref());
                }
            }
        }
        _ => {}
    }
}

/// The session id of a subagent that finished, from its parent's log.
fn finished_subagent(entry: &Value) -> Option<&str> {
    let (update, kind) = super::super::grok::update(entry)?;
    if kind != "subagent_finished" {
        return None;
    }
    update
        .get("child_session_id")
        .or_else(|| update.get("subagent_id"))?
        .as_str()
}

/// The touches of a finished subagent's own log under `grok` (Grok's home):
/// its start, at most [`SUBAGENT_BUDGET`], relative paths in `cwd` (its
/// parent's folder).
fn subagent(
    id: &str,
    grok: Option<&Path>,
    cwd: Option<&Path>,
    home: Option<&Path>,
    touches: &mut Vec<Touch>,
) {
    let Some(log) = grok.and_then(|grok| crate::resume::grok_log(grok, id, cwd)) else {
        return;
    };
    scan(LogAgent::Grok, &log, 0, SUBAGENT_BUDGET, |entry, _| {
        let mut collect = Collect::new(cwd, home);
        collect.at = super::super::grok::timestamp(entry);
        update(entry, &mut collect);
        touches.extend(collect.touches);
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn line(update: Value) -> Value {
        json!({"timestamp": 1_793_338_236, "method": "session/update",
            "params": {"sessionId": "01a18b49-8533-7252-8d65-46555f07d0c5", "update": update,
                "_meta": {"eventId": "e-1", "agentTimestampMs": 1_793_338_236_400_i64}}})
    }

    fn touches(entries: &[Value]) -> Vec<String> {
        let mut cwd = Some(PathBuf::from("/home/me/proj"));
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

    fn call(name: &str, input: Value) -> Value {
        line(
            json!({"sessionUpdate": "tool_call", "toolCallId": "c1", "title": name,
            "rawInput": input, "_meta": {"x.ai/tool": {"name": name}}}),
        )
    }

    fn done(content: Value, raw: Value) -> Value {
        line(
            json!({"sessionUpdate": "tool_call_update", "toolCallId": "c1",
            "status": "completed", "content": content, "rawOutput": raw}),
        )
    }

    #[test]
    fn its_edits_reads_searches_and_shell_calls_are_touches() {
        assert_eq!(
            touches(&[
                call("read_file", json!({"target_file": "src/a.rs"})),
                call("list_dir", json!({"target_directory": "."})),
                call("grep", json!({"pattern": "x", "path": "tui"})),
                // An edit's enrichment before it ran, or one refused: no change yet.
                line(
                    json!({"sessionUpdate": "tool_call_update", "toolCallId": "c2",
                    "content": [{"type": "diff", "path": "/home/me/proj/no.rs"}]})
                ),
                done(
                    json!([{"type": "diff", "path": "/home/me/wt/fix/b.rs", "oldText": "", "newText": "x"}]),
                    json!({"type": "SearchReplace", "EditsApplied": {"absolute_path": "/home/me/wt/fix/b.rs"}})
                ),
                done(
                    json!([{"type": "content", "content": {"type": "text", "text": ""}}]),
                    json!({"type": "Bash", "command": "cd ../other && git -C ~/lib status",
                        "current_dir": "/home/me/proj", "exit_code": 0})
                ),
                call("web_fetch", json!({"url": "https://x.y/z"})),
            ]),
            [
                "/home/me/proj/src",
                "/home/me/proj",
                "/home/me/proj/tui",
                "/home/me/wt/fix (edited)",
                "/home/me/proj",
                "/home/me/other",
                "/home/me/lib",
            ]
        );
    }

    #[test]
    fn a_finished_subagents_own_log_counts() {
        let grok = tempfile::tempdir().unwrap();
        let child = "01a18b4b-bce9-74f0-9c4d-284e53688b80";
        let dir = grok.path().join("sessions/%2Fhome%2Fme%2Fproj").join(child);
        std::fs::create_dir_all(&dir).unwrap();
        let lines = [
            call("read_file", json!({"target_file": "README.md"})),
            done(
                json!([{"type": "diff", "path": "/home/me/proj/.grok/worktrees/helper/c.rs"}]),
                json!({}),
            ),
        ];
        let text: String = lines.iter().map(|l| format!("{l}\n")).collect();
        std::fs::write(dir.join("updates.jsonl"), text).unwrap();
        let finished = line(
            json!({"sessionUpdate": "subagent_finished", "subagent_id": child,
            "child_session_id": child, "status": "completed"}),
        );
        let mut touches = Vec::new();
        let cwd = Some(PathBuf::from("/home/me/proj"));
        subagent(child, Some(grok.path()), cwd.as_deref(), None, &mut touches);
        let dirs: Vec<String> = touches
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
            dirs,
            [
                "/home/me/proj",
                "/home/me/proj/.grok/worktrees/helper (edited)"
            ]
        );
        assert_eq!(finished_subagent(&finished), Some(child));
        assert_eq!(touches[1].at, Some(1_793_338_236_400));
    }
}
