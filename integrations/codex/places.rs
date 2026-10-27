//! Where Codex works, from its rollout file: the thread's and each turn's
//! directory (`session_meta` and `turn_context`'s `cwd`), each shell call's
//! working directory and command, and the files `apply_patch` changes
//! (relative to the turn's directory, or its shell call's), whichever way
//! it's called: its own tool, or a shell call of it (`apply_patch <<EOF`).
//! Other shell commands' writes aren't known.
use super::{resolve, Collect, Touch};
use serde_json::Value;
use std::path::{Path, PathBuf};

/// The touches of one rollout line; `cwd` follows the turns' own.
pub(super) fn entry(
    entry: &Value,
    cwd: &mut Option<PathBuf>,
    home: Option<&Path>,
    at: Option<i64>,
    touches: &mut Vec<Touch>,
) {
    let Some(payload) = entry.get("payload") else {
        return;
    };
    let field = |key: &str| payload.get(key).and_then(Value::as_str);
    let kind = entry.get("type").and_then(Value::as_str);
    if matches!(kind, Some("session_meta" | "turn_context")) {
        if let Some(dir) = field("cwd").and_then(|dir| resolve(dir, None, home)) {
            *cwd = Some(dir);
        }
    }
    let mut collect = Collect::new(cwd.as_deref(), home);
    collect.at = at;
    match (kind, field("type")) {
        (Some("session_meta" | "turn_context"), _) => {
            if let Some(dir) = field("cwd") {
                collect.dir(dir);
            }
        }
        (Some("response_item"), Some("function_call")) => {
            let arguments: Value = field("arguments")
                .and_then(|arguments| serde_json::from_str(arguments).ok())
                .unwrap_or(Value::Null);
            match field("name") {
                Some("apply_patch") => {
                    if let Some(input) = arguments.get("input").and_then(Value::as_str) {
                        collect.patch(input);
                    }
                }
                _ => shell(&arguments, "workdir", &mut collect),
            }
        }
        (Some("response_item"), Some("local_shell_call")) => {
            if let Some(action) = payload.get("action") {
                shell(action, "working_directory", &mut collect);
            }
        }
        (Some("response_item"), Some("custom_tool_call")) => {
            let input = field("input").unwrap_or_default();
            match field("name") {
                Some("apply_patch") => collect.patch(input),
                // Code that calls its tools (`exec`): the working
                // directories it passes them.
                _ => {
                    for dir in quoted_after(input, "workdir") {
                        collect.dir(&dir);
                    }
                }
            }
        }
        _ => {}
    }
    touches.extend(collect.touches);
}

/// A shell call's working directory (its `key`) and command (`command` or
/// `cmd`, a string or argv; `bash -lc <script>` is its script).
fn shell(arguments: &Value, key: &str, collect: &mut Collect) {
    let dir = arguments
        .get(key)
        .and_then(Value::as_str)
        .and_then(|dir| collect.workdir(dir));
    let command = match arguments.get("command").or_else(|| arguments.get("cmd")) {
        Some(Value::String(command)) => command.clone(),
        Some(Value::Array(parts)) => {
            let parts: Vec<&str> = parts.iter().filter_map(Value::as_str).collect();
            match parts.as_slice() {
                [_, "-c" | "-lc", script] => script.to_string(),
                parts => parts.join(" "),
            }
        }
        _ => return,
    };
    let dir = dir.as_deref().or(collect.cwd);
    // Codex applies a patch it's given as a command itself.
    if command
        .trim_start()
        .strip_prefix("apply_patch")
        .or_else(|| command.trim_start().strip_prefix("applypatch"))
        .is_some_and(|rest| rest.starts_with(char::is_whitespace))
    {
        collect.patch_in(&command, dir);
    } else {
        collect.command_in(&command, dir);
    }
}

/// The string literals right after `key` and a `:` in `code`
/// (`workdir: "/repo"`, `"workdir":"/repo"`).
fn quoted_after(code: &str, key: &str) -> Vec<String> {
    let mut found = Vec::new();
    let mut rest = code;
    while let Some(at) = rest.find(key) {
        rest = &rest[at + key.len()..];
        let after = rest.trim_start_matches(['"', '\'']).trim_start();
        let Some(after) = after.strip_prefix(':') else {
            continue;
        };
        let after = after.trim_start();
        let Some(quote) = after
            .chars()
            .next()
            .filter(|c| matches!(c, '"' | '\'' | '`'))
        else {
            continue;
        };
        if let Some(end) = after[1..].find(quote) {
            found.push(after[1..1 + end].to_string());
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn touches(entries: &[Value]) -> Vec<String> {
        let mut cwd = None;
        let mut touches = Vec::new();
        for e in entries {
            entry(e, &mut cwd, None, None, &mut touches);
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

    fn item(payload: Value) -> Value {
        json!({"type": "response_item", "payload": payload})
    }

    #[test]
    fn turns_shell_calls_and_patches_are_touches() {
        assert_eq!(
            touches(&[
                json!({"type": "turn_context", "payload": {"cwd": "/repo"}}),
                item(json!({"type": "function_call", "name": "shell",
                    "arguments": "{\"command\":[\"bash\",\"-lc\",\"cd tui && cargo test\"],\"workdir\":\"/wt/a\"}"})),
                item(json!({"type": "function_call", "name": "exec_command",
                    "arguments": "{\"cmd\":\"git -C ../b status\"}"})),
                item(json!({"type": "custom_tool_call", "name": "apply_patch",
                    "input": "*** Begin Patch\n*** Update File: src/lib.rs\n*** End Patch"})),
                item(json!({"type": "custom_tool_call", "name": "exec",
                    "input": "await tools.exec_command({cmd: 'ls', workdir: \"/wt/c\"})"})),
                item(json!({"type": "local_shell_call",
                    "action": {"command": ["ls"], "working_directory": "/wt/d"}})),
            ]),
            [
                "/repo",
                "/wt/a",
                "/wt/a/tui",
                "/b",
                "/repo/src (edited)",
                "/wt/c",
                "/wt/d"
            ]
        );
    }

    #[test]
    fn a_patch_counts_however_codex_applies_it_and_other_commands_change_nothing() {
        assert_eq!(
            touches(&[
                json!({"type": "turn_context", "payload": {"cwd": "/repo"}}),
                item(json!({"type": "function_call", "name": "apply_patch",
                    "arguments": "{\"input\":\"*** Begin Patch\\n*** Add File: a/new.rs\\n+x\\n*** End Patch\"}"})),
                item(json!({"type": "function_call", "name": "shell",
                    "arguments": serde_json::to_string(&json!({
                        "command": ["apply_patch", "*** Begin Patch\n*** Update File: b.rs\n*** End Patch"],
                        "workdir": "/wt/x"})).unwrap()})),
                item(json!({"type": "function_call", "name": "shell",
                    "arguments": serde_json::to_string(&json!({
                        "command": ["bash", "-lc", "apply_patch <<'EOF'\n*** Begin Patch\n*** Delete File: c/old.rs\n*** End Patch\nEOF"],
                        "workdir": "/wt/y"})).unwrap()})),
                item(json!({"type": "function_call", "name": "exec_command",
                    "arguments": "{\"cmd\":\"sed -i s/a/b/ d.rs > e.txt\",\"workdir\":\"/wt/z\"}"})),
            ]),
            [
                "/repo",
                "/repo/a (edited)",
                "/wt/x",
                "/wt/x (edited)",
                "/wt/y",
                "/wt/y/c (edited)",
                "/wt/z"
            ]
        );
    }
}
