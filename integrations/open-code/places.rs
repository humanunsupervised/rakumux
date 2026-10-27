//! Where OpenCode works, from its tool calls (as [`super::super::transcript`]
//! reads them from its database): the files it reads and changes, the roots
//! it searches, and its shell commands and their working directories.
//! Relative paths are the OpenCode process's directory's.
use super::Collect;
use serde_json::Value;

/// The touches of one tool call, `name` with `input`.
pub(super) fn call(name: &str, input: &Value, collect: &mut Collect) {
    let field = |key: &str| input.get(key)?.as_str();
    match name {
        "read" => {
            if let Some(file) = field("filePath").or_else(|| field("path")) {
                collect.file(file, false);
            }
        }
        "write" | "edit" | "multiedit" => {
            if let Some(file) = field("filePath").or_else(|| field("path")) {
                collect.file(file, true);
            }
        }
        "patch" | "apply_patch" => {
            if let Some(patch) = field("patchText").or_else(|| field("patch")) {
                collect.patch(patch);
            }
        }
        "grep" | "glob" | "list" => {
            if let Some(dir) = field("path") {
                collect.dir(dir);
            }
        }
        "bash" | "shell" => {
            let dir = field("workdir").and_then(|dir| collect.workdir(dir));
            if let Some(command) = field("command") {
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
    use std::path::Path;

    #[test]
    fn files_searches_and_commands_are_touches() {
        let mut collect = Collect::new(Some(Path::new("/repo")), None);
        call("read", &json!({"filePath": "/repo/src/a.ts"}), &mut collect);
        call("edit", &json!({"filePath": "/wt/x/b.ts"}), &mut collect);
        call(
            "glob",
            &json!({"pattern": "*", "path": "/other"}),
            &mut collect,
        );
        call(
            "bash",
            &json!({"command": "cd pkg && npm test", "workdir": "/wt/y"}),
            &mut collect,
        );
        call("webfetch", &json!({"url": "https://x.y"}), &mut collect);
        let dirs: Vec<String> = collect
            .touches
            .iter()
            .map(|t| format!("{}{}", t.dir.display(), if t.edited { "*" } else { "" }))
            .collect();
        assert_eq!(
            dirs,
            ["/repo/src", "/wt/x*", "/other", "/wt/y", "/wt/y/pkg"]
        );
    }
}
