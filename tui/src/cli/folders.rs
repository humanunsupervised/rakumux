//! `rakumux folders [add|rm]` (alias `projects`): the folders sessions
//! belong to, as the daemon knows them (`project_*` on the wire). Session
//! callers see only their own folder and can't add or remove any.
//!
//! `--json` prints `{"folders": [...], "projects": [...]}`: the same list
//! under both names, so scripts written against `projects --json` keep
//! working.

use super::args::FoldersCommand;
use super::table::{self, ago, git_text, Column};
use super::{output_error, CliError, Invocation};
use serde_json::json;
use session_core::api::CallerInfo;
use session_core::client::SessionApi;
use session_core::model::{ProjectView, ScopeRoot};
use std::io::Write;
use std::path::Path;

pub(crate) fn run(
    api: &dyn SessionApi,
    invocation: &Invocation,
    command: Option<FoldersCommand>,
    json: bool,
    out: &mut dyn Write,
) -> Result<i32, CliError> {
    match command {
        None => list(api, invocation, json, out),
        Some(FoldersCommand::Add { dir }) => add(api, invocation, &dir, out),
        Some(FoldersCommand::Rm { dir }) => remove(api, invocation, &dir, out),
    }
}

fn list(
    api: &dyn SessionApi,
    invocation: &Invocation,
    json: bool,
    out: &mut dyn Write,
) -> Result<i32, CliError> {
    let folders = api.projects()?;
    if json {
        writeln!(
            out,
            "{}",
            json!({ "folders": folders, "projects": folders })
        )
        .map_err(output_error)?;
        return Ok(0);
    }
    if folders.is_empty() {
        writeln!(out, "No folders yet").map_err(output_error)?;
        return Ok(0);
    }
    let current = current_folder(api, invocation);
    out.write_all(table(&folders, current.as_ref(), table::now()).as_bytes())
        .map_err(output_error)?;
    Ok(0)
}

fn add(
    api: &dyn SessionApi,
    invocation: &Invocation,
    dir: &Path,
    out: &mut dyn Write,
) -> Result<i32, CliError> {
    // The daemon canonicalizes and checks the folder.
    let folder = api.add_project(&invocation.absolute(dir))?;
    writeln!(
        out,
        "Added folder {} ({})",
        folder.name,
        folder.path.path().display()
    )
    .map_err(output_error)?;
    Ok(0)
}

fn remove(
    api: &dyn SessionApi,
    invocation: &Invocation,
    dir: &Path,
    out: &mut dyn Write,
) -> Result<i32, CliError> {
    let path = invocation.folder_root(dir)?;
    api.remove_project(&path)?;
    writeln!(out, "Removed folder {}", path.path().display()).map_err(output_error)?;
    Ok(0)
}

/// The folder this command runs in: the caller's own inside a session, the
/// current folder otherwise (when it resolves).
fn current_folder(api: &dyn SessionApi, invocation: &Invocation) -> Option<ScopeRoot> {
    match api.caller() {
        CallerInfo::Session { scope, .. } => Some(scope.clone()),
        CallerInfo::User => ScopeRoot::resolve(&invocation.cwd).ok(),
    }
}

/// Folders as a table; `*` marks the current one.
fn table(folders: &[ProjectView], current: Option<&ScopeRoot>, now: i64) -> String {
    let cells = |cell: &dyn Fn(&ProjectView) -> String| folders.iter().map(cell).collect();
    table::render(vec![
        Column::optional(
            "",
            cells(&|f| if current == Some(&f.path) { "*" } else { "" }.into()),
        ),
        Column::new("NAME", cells(&|f| f.name.clone())),
        Column::new("SESSIONS", cells(&|f| f.sessions.to_string())),
        Column::new("LIVE", cells(&|f| f.live.to_string())),
        Column::new(
            "ACTIVE",
            cells(&|f| {
                f.last_active_at
                    .map(|at| ago(now, at))
                    .unwrap_or_else(|| "-".into())
            }),
        ),
        Column::optional(
            "GIT",
            cells(&|f| f.git.as_ref().map(git_text).unwrap_or_default()),
        ),
        Column::new("PATH", cells(&|f| f.path.path().display().to_string())),
    ])
}

#[cfg(test)]
mod tests {
    use super::super::args::{CliArgs, Command};
    use super::super::fake::{folder, in_scope, FakeApi};
    use super::*;
    use clap::Parser;
    use session_core::api::{ApiError, ErrorCode};
    use session_core::model::GitInfo;
    use std::path::PathBuf;

    fn invocation(cwd: &Path) -> Invocation {
        Invocation {
            cwd: cwd.to_path_buf(),
            ..Invocation::default()
        }
    }

    fn output(run: impl FnOnce(&mut Vec<u8>) -> Result<i32, CliError>) -> String {
        let mut out = Vec::new();
        assert_eq!(run(&mut out).unwrap(), 0);
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn folders_are_listed_as_a_table_or_json() {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        std::fs::create_dir_all(root.join("web")).unwrap();
        let mut web = folder(&root.join("web"));
        web.sessions = 3;
        web.live = 2;
        web.last_active_at = Some(table::now() - 120);
        web.git = Some(GitInfo {
            repo_root: root.join("web"),
            branch: Some("main".into()),
            detached_at: None,
            worktree: None,
            repo: None,
            origin: None,
        });
        let api = FakeApi::user(vec![]);
        *api.projects.lock().unwrap() = vec![web, folder(Path::new("/srv/api"))];

        let text = output(|out| run(&api, &invocation(&root.join("web")), None, false, out));
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 3, "{text}");
        assert!(
            lines[0].starts_with("   NAME  SESSIONS  LIVE  ACTIVE"),
            "{text}"
        );
        assert!(lines[0].ends_with("GIT   PATH"), "{text}");
        assert!(
            lines[1].starts_with("*  web   3         2     2m ago  main"),
            "{text}"
        );
        assert!(lines[2].starts_with("   api   0         0     -"), "{text}");
        assert!(lines[2].ends_with("/srv/api"), "{text}");

        // JSON: `folders`, and the same list as `projects` for older scripts.
        let text = output(|out| run(&api, &invocation(&root), None, true, out));
        let value: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(value["folders"][0]["name"], "web");
        assert_eq!(value["folders"][1]["path"], "/srv/api");
        assert_eq!(value["projects"], value["folders"]);

        let none = FakeApi::user(vec![]);
        assert_eq!(
            output(|out| run(&none, &invocation(&root), None, false, out)),
            "No folders yet\n"
        );
    }

    #[test]
    fn add_sends_an_absolute_path_and_rm_a_resolved_one() {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        std::fs::create_dir_all(root.join("web")).unwrap();
        let inv = invocation(&root);
        let api = FakeApi::user(vec![]);
        let add = |dir: &str| Some(FoldersCommand::Add { dir: dir.into() });
        let rm = |dir: &str| Some(FoldersCommand::Rm { dir: dir.into() });

        let text = output(|out| run(&api, &inv, add("web"), false, out));
        assert_eq!(
            text,
            format!("Added folder web ({})\n", root.join("web").display())
        );
        assert_eq!(
            api.calls(),
            vec![format!("add_project {}", root.join("web").display())]
        );

        let text = output(|out| run(&api, &inv, rm("web/"), false, out));
        assert_eq!(
            text,
            format!("Removed folder {}\n", root.join("web").display())
        );
        assert!(api.projects.lock().unwrap().is_empty());

        // Errors from the daemon come through: unknown, or still in use.
        let error = run(&api, &inv, rm("web"), false, &mut Vec::new()).unwrap_err();
        assert!(matches!(error, CliError::Api(e) if e.code == ErrorCode::NotFound));
        let busy = FakeApi::user(vec![in_scope("S1", "s", &root.join("web"))]);
        output(|out| run(&busy, &inv, add("web"), false, out));
        let error = run(&busy, &inv, rm("web"), false, &mut Vec::new()).unwrap_err();
        assert!(matches!(error, CliError::Api(e) if e.code == ErrorCode::InvalidArgument));
        *busy.fail_next.lock().unwrap() =
            Some(ApiError::new(ErrorCode::PermissionDenied, "user only"));
        let error = run(&busy, &inv, add("/x"), false, &mut Vec::new()).unwrap_err();
        assert!(matches!(error, CliError::Api(e) if e.code == ErrorCode::PermissionDenied));
        assert!(busy
            .calls()
            .contains(&format!("add_project {}", PathBuf::from("/x").display())));
    }

    #[test]
    fn the_projects_alias_runs_the_same_commands() {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        std::fs::create_dir_all(root.join("web")).unwrap();
        let inv = invocation(&root);
        let api = FakeApi::user(vec![]);
        let parsed = |line: &str| {
            let args = std::iter::once("tui").chain(line.split_whitespace());
            CliArgs::try_parse_from(args).unwrap().command
        };
        for (line, expected) in [
            (
                "projects add web",
                format!("Added folder web ({})\n", root.join("web").display()),
            ),
            ("projects --json", String::new()),
            (
                "projects rm web",
                format!("Removed folder {}\n", root.join("web").display()),
            ),
            ("projects", "No folders yet\n".to_owned()),
        ] {
            let Command::Folders { command, json } = parsed(line) else {
                panic!("{line} is not the folders command");
            };
            let text = output(|out| run(&api, &inv, command, json, out));
            if json {
                let value: serde_json::Value = serde_json::from_str(&text).unwrap();
                assert_eq!(value["folders"][0]["name"], "web", "{text}");
                assert_eq!(value["projects"][0]["name"], "web", "{text}");
            } else {
                assert_eq!(text, expected, "{line}");
            }
        }
    }

    #[test]
    fn inside_a_session_the_current_folder_is_its_own() {
        let api = FakeApi::session_caller("S1", "/srv/api", vec![]);
        *api.projects.lock().unwrap() = vec![folder(Path::new("/srv/api"))];
        let text = output(|out| run(&api, &invocation(Path::new("/")), None, false, out));
        assert!(text.lines().nth(1).unwrap().starts_with("*  api"), "{text}");
    }
}
