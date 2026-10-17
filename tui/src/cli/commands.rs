//! Human-facing session commands: `new`, `ls`, `attach`, `rename`, `pin`,
//! `unpin`, `kill`, `rm`. The JSON group lives in `session`, folders in
//! `folders`.

use super::args::{Command, LsArgs, NewArgs};
use super::attach::{self, AttachContext};
use super::table::{self, ago, git_text, Column};
use super::{folders, output_error, resolve_target, session, CliError, Invocation};
use serde_json::json;
use session_core::api::{CallerInfo, CreateSession};
use session_core::client::SessionApi;
use session_core::model::{Lifecycle, ScopeRoot, SessionStatus, SessionView};
use std::collections::HashMap;
use std::io::Write;
use std::path::Path;

/// Run a session subcommand against `api`.
pub(crate) fn run(
    api: &dyn SessionApi,
    invocation: &Invocation,
    command: Command,
    out: &mut dyn Write,
) -> Result<i32, CliError> {
    match command {
        Command::New(args) => new(api, invocation, args, out),
        Command::Ls(args) => ls(api, invocation, args, out),
        Command::Attach { target } => {
            let scope = invocation.request_scope(api)?;
            let target = resolve_target(api, scope.as_ref(), &target)?;
            attach::run(api, &AttachContext::new(invocation, scope), &target, out)
        }
        Command::Rename { args } => rename(api, invocation, &args, out),
        Command::Pin { target } => pin(api, invocation, &target, true, out),
        Command::Unpin { target } => pin(api, invocation, &target, false, out),
        Command::Kill { target, tree } => kill(api, invocation, &target, tree, out),
        Command::Rm { target } => rm(api, invocation, &target, out),
        Command::Folders { command, json } => folders::run(api, invocation, command, json, out),
        Command::Session { command } => session::run(api, invocation, command, out),
        Command::Daemon { .. } => Err(CliError::internal("daemon commands need no session")),
        Command::Remote { .. } => Err(CliError::internal("remote commands need no session")),
    }
}

fn new(
    api: &dyn SessionApi,
    invocation: &Invocation,
    args: NewArgs,
    out: &mut dyn Write,
) -> Result<i32, CliError> {
    // Check before creating, so a failed attach leaves nothing behind.
    if !args.detach && matches!(api.caller(), CallerInfo::Session { .. }) {
        return Err(CliError::invalid(
            "inside an Orchestra session, new sessions start in the background: pass --detach \
             (the session appears in the Orchestra list)",
        ));
    }
    if !args.detach && !attach::is_terminal() {
        return Err(CliError::invalid(
            "attaching needs a terminal; pass --detach to create the session without attaching",
        ));
    }
    let cwd = args.cwd.map(|cwd| invocation.absolute(&cwd));
    let scope = new_session_folder(api, invocation, cwd.as_deref())?;
    let (cols, rows) = attach::terminal_size();
    let request = CreateSession {
        scope: scope.clone(),
        name: args.name,
        cwd,
        argv: (!args.argv.is_empty()).then_some(args.argv),
        startup_cmds: args.cmds,
        env: invocation.env.clone(),
        cols,
        rows,
    };
    let session = api.create(request)?;
    if args.detach {
        writeln!(out, "Created session {} ({})", session.name, session.id).map_err(output_error)?;
        return Ok(0);
    }
    attach::run(api, &AttachContext::new(invocation, scope), &session, out)
}

/// The folder a new session goes in: the current one, unless `--cwd` (from
/// a user, not a session) names a directory outside it, which is then the
/// session's own folder.
fn new_session_folder(
    api: &dyn SessionApi,
    invocation: &Invocation,
    cwd: Option<&Path>,
) -> Result<Option<ScopeRoot>, CliError> {
    let here = invocation.request_scope(api)?;
    let (Some(current), Some(dir), CallerInfo::User) = (&here, cwd, api.caller()) else {
        return Ok(here);
    };
    match ScopeRoot::resolve(dir) {
        Ok(there) if !there.path().starts_with(current.path()) => Ok(Some(there)),
        // Inside the current folder, or not there at all (the daemon says
        // which).
        _ => Ok(here),
    }
}

/// Sessions of the current folder by default, of the folders named with
/// `--folder` (alias `--project`), or of every folder with `--all`.
fn ls(
    api: &dyn SessionApi,
    invocation: &Invocation,
    args: LsArgs,
    out: &mut dyn Write,
) -> Result<i32, CliError> {
    let scopes: Vec<ScopeRoot> = if args.all {
        api.projects()?.into_iter().map(|p| p.path).collect()
    } else {
        let mut scopes = Vec::new();
        for dir in &args.folders {
            let scope = invocation.folder_root(dir)?;
            if !scopes.contains(&scope) {
                scopes.push(scope);
            }
        }
        scopes
    };
    let (sessions, shown) = match scopes.as_slice() {
        [] if args.all => (Vec::new(), Vec::new()),
        [] => {
            let scope = invocation.request_scope(api)?;
            let sessions = api.list(scope.as_ref(), args.dead)?;
            (sessions, vec![invocation.display_scope(api)?])
        }
        _ => (api.list_in(&scopes, args.dead)?, scopes),
    };
    if args.json {
        let sessions = sessions
            .iter()
            .map(session_json)
            .collect::<Result<Vec<_>, _>>()?;
        writeln!(out, "{}", json!({ "sessions": sessions })).map_err(output_error)?;
        return Ok(0);
    }
    if sessions.is_empty() {
        let line = match shown.as_slice() {
            [] => "No folders yet".to_string(),
            [scope] => format!("No sessions in {}", scope.path().display()),
            several => format!("No sessions in {} folders", several.len()),
        };
        writeln!(out, "{line}").map_err(output_error)?;
        return Ok(0);
    }
    let sessions = display_order(sessions);
    out.write_all(table(&sessions, invocation, shown.len() > 1, table::now()).as_bytes())
        .map_err(output_error)?;
    Ok(0)
}

/// A session for `ls --json`: its `SessionView` plus `folder`, the same
/// path as `scope` (which stays for older scripts).
fn session_json(session: &SessionView) -> Result<serde_json::Value, CliError> {
    let mut value = serde_json::to_value(session)
        .map_err(|error| CliError::internal(format!("cannot encode a session: {error}")))?;
    if let Some(map) = value.as_object_mut() {
        map.insert("folder".into(), json!(session.scope.path()));
    }
    Ok(value)
}

fn pin(
    api: &dyn SessionApi,
    invocation: &Invocation,
    target: &str,
    pinned: bool,
    out: &mut dyn Write,
) -> Result<i32, CliError> {
    let scope = invocation.request_scope(api)?;
    let session = resolve_target(api, scope.as_ref(), target)?;
    let session = api.pin(&session.id, pinned)?;
    let verb = if pinned { "Pinned" } else { "Unpinned" };
    writeln!(out, "{verb} {}", session.name).map_err(output_error)?;
    Ok(0)
}

fn rename(
    api: &dyn SessionApi,
    invocation: &Invocation,
    args: &[String],
    out: &mut dyn Write,
) -> Result<i32, CliError> {
    let (id, name) = match args {
        [name] => match &invocation.self_id {
            Some(id) => (id.clone(), name),
            None => {
                return Err(CliError::Usage(
                    "name the session to rename: `orchestra rename <name|id> <new name>` \
                     (ORCHESTRA_SESSION_ID is not set)"
                        .into(),
                ))
            }
        },
        [target, name] => {
            let scope = invocation.request_scope(api)?;
            (resolve_target(api, scope.as_ref(), target)?.id, name)
        }
        _ => return Err(CliError::Usage("rename takes one or two names".into())),
    };
    let name = name.trim();
    if name.is_empty() {
        return Err(CliError::Usage("the new name is empty".into()));
    }
    let session = api.rename(&id, name)?;
    writeln!(out, "Renamed to {}", session.name).map_err(output_error)?;
    Ok(0)
}

fn kill(
    api: &dyn SessionApi,
    invocation: &Invocation,
    target: &str,
    tree: bool,
    out: &mut dyn Write,
) -> Result<i32, CliError> {
    let scope = invocation.request_scope(api)?;
    let session = resolve_target(api, scope.as_ref(), target)?;
    let killed = api.kill(&session.id, tree)?;
    let others = killed.iter().filter(|id| **id != session.id).count();
    let line = match others {
        0 => format!("Killed {}", session.name),
        1 => format!("Killed {} and 1 session it created", session.name),
        n => format!("Killed {} and {n} sessions it created", session.name),
    };
    writeln!(out, "{line}").map_err(output_error)?;
    Ok(0)
}

fn rm(
    api: &dyn SessionApi,
    invocation: &Invocation,
    target: &str,
    out: &mut dyn Write,
) -> Result<i32, CliError> {
    let scope = invocation.request_scope(api)?;
    let session = resolve_target(api, scope.as_ref(), target)?;
    api.delete(&session.id)?;
    writeln!(out, "Deleted {}", session.name).map_err(output_error)?;
    Ok(0)
}

// ---------------------------------------------------------------- table

fn status_text(session: &SessionView) -> String {
    match session.status {
        SessionStatus::Live => match session.lifecycle {
            Lifecycle::Working => "working",
            Lifecycle::Blocked => "blocked",
            Lifecycle::Done => "done",
            Lifecycle::Idle => "idle",
            Lifecycle::Unknown | Lifecycle::None => "running",
        }
        .into(),
        SessionStatus::Exited => match session.exit_code {
            Some(code) => format!("exited ({code})"),
            None => "exited".into(),
        },
        SessionStatus::Dead => "dead".into(),
        SessionStatus::Unknown => "unknown".into(),
    }
}

fn relative_cwd(cwd: &Path, scope: &ScopeRoot) -> String {
    match cwd.strip_prefix(scope.path()) {
        Ok(rest) if rest.as_os_str().is_empty() => ".".into(),
        Ok(rest) => rest.display().to_string(),
        Err(_) => cwd.display().to_string(),
    }
}

/// Grouped by folder (in the order the folders first appear), pinned
/// sessions first within each, otherwise in the daemon's order.
fn display_order(mut sessions: Vec<SessionView>) -> Vec<SessionView> {
    let mut scopes: Vec<ScopeRoot> = Vec::new();
    for session in &sessions {
        if !scopes.contains(&session.scope) {
            scopes.push(session.scope.clone());
        }
    }
    sessions.sort_by_key(|s| (scopes.iter().position(|scope| *scope == s.scope), !s.pinned));
    sessions
}

/// Folder column labels: the folder name, or the whole path for folders
/// that share a name.
fn folder_labels(sessions: &[SessionView]) -> HashMap<ScopeRoot, String> {
    let name = |scope: &ScopeRoot| {
        scope
            .path()
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| scope.path().display().to_string())
    };
    let mut names: HashMap<String, Vec<ScopeRoot>> = HashMap::new();
    for session in sessions {
        let scopes = names.entry(name(&session.scope)).or_default();
        if !scopes.contains(&session.scope) {
            scopes.push(session.scope.clone());
        }
    }
    let mut labels = HashMap::new();
    for (name, scopes) in names {
        let shared = scopes.len() > 1;
        for scope in scopes {
            let label = if shared {
                scope.path().display().to_string()
            } else {
                name.clone()
            };
            labels.insert(scope, label);
        }
    }
    labels
}

/// Sessions as a table. With `several` folders a FOLDER column says where
/// each session belongs; CWD is relative to the session's folder. Marks:
/// `*` the current session, `^` a pinned one.
fn table(sessions: &[SessionView], invocation: &Invocation, several: bool, now: i64) -> String {
    let cells = |cell: &dyn Fn(&SessionView) -> String| sessions.iter().map(cell).collect();
    let labels = folder_labels(sessions);
    let mut columns = vec![
        Column::optional(
            "",
            cells(&|s| {
                let current = s.is_self || invocation.self_id.as_ref() == Some(&s.id);
                let mut marks = String::new();
                if current {
                    marks.push('*');
                }
                if s.pinned {
                    marks.push('^');
                }
                marks
            }),
        ),
        Column::new("NAME", cells(&|s| s.name.clone())),
    ];
    if several {
        columns.push(Column::new(
            "FOLDER",
            cells(&|s| labels.get(&s.scope).cloned().unwrap_or_default()),
        ));
    }
    columns.extend([
        Column::new("STATUS", cells(&status_text)),
        Column::new(
            "AGENT",
            cells(&|s| {
                s.agent
                    .as_ref()
                    .map(|a| a.kind.clone())
                    .unwrap_or_else(|| "-".into())
            }),
        ),
        Column::optional(
            "GIT",
            cells(&|s| s.git.as_ref().map(git_text).unwrap_or_default()),
        ),
        Column::new("ACTIVE", cells(&|s| ago(now, s.last_active_at))),
        Column::new("CWD", cells(&|s| relative_cwd(&s.cwd, &s.scope))),
        Column::new("ID", cells(&|s| s.id.to_string())),
        Column::optional(
            "NOTE",
            cells(&|s| {
                s.reported
                    .as_ref()
                    .and_then(|r| r.text.as_deref())
                    .unwrap_or("")
                    .chars()
                    .take(40)
                    .collect()
            }),
        ),
    ]);
    table::render(columns)
}

#[cfg(test)]
mod tests {
    use super::super::args::CliArgs;
    use super::super::fake::{folder, in_scope, session, FakeApi};
    use super::*;
    use clap::Parser;
    use session_core::api::{ApiError, ErrorCode};
    use session_core::ids::SessionId;
    use session_core::model::{AgentInfo, Reported};

    fn invocation(dir: &Path) -> Invocation {
        Invocation {
            cwd: dir.to_path_buf(),
            env: vec![("PATH".into(), "/bin".into())],
            self_id: None,
        }
    }

    fn output(run: impl FnOnce(&mut Vec<u8>) -> Result<i32, CliError>) -> (i32, String) {
        let mut out = Vec::new();
        let code = run(&mut out).unwrap();
        (code, String::from_utf8(out).unwrap())
    }

    #[test]
    fn new_detached_sends_a_complete_create_request() {
        let dir = tempfile::tempdir().unwrap();
        let inv = invocation(dir.path());
        let api = FakeApi::user(vec![]);
        let args = NewArgs {
            name: Some("web".into()),
            cwd: Some("sub".into()),
            cmds: vec!["npm i".into()],
            detach: true,
            argv: vec!["bash".into(), "-l".into()],
        };
        let (code, text) = output(|out| new(&api, &inv, args, out));
        assert_eq!(code, 0);
        assert_eq!(text, "Created session web (NEW0)\n");
        let created = api.created.lock().unwrap();
        let request = &created[0];
        let canonical = std::fs::canonicalize(dir.path()).unwrap();
        assert_eq!(request.scope, Some(ScopeRoot::from_canonical(canonical)));
        assert_eq!(request.name.as_deref(), Some("web"));
        assert_eq!(request.cwd, Some(dir.path().join("sub")));
        assert_eq!(request.argv, Some(vec!["bash".into(), "-l".into()]));
        assert_eq!(request.startup_cmds, vec!["npm i".to_string()]);
        assert_eq!(request.env, inv.env);
        assert!(request.cols > 0 && request.rows > 0);

        // Defaults: no name, the scope root, the login shell.
        let args = NewArgs {
            detach: true,
            ..NewArgs::default()
        };
        drop(created);
        output(|out| new(&api, &inv, args, out));
        let created = api.created.lock().unwrap();
        assert_eq!(created[1].name, None);
        assert_eq!(created[1].cwd, None);
        assert_eq!(created[1].argv, None);
    }

    #[test]
    fn new_with_a_cwd_outside_this_folder_starts_in_that_folder() {
        let here = tempfile::tempdir().unwrap();
        let there = tempfile::tempdir().unwrap();
        let inv = invocation(here.path());
        let api = FakeApi::user(vec![]);
        let args = NewArgs {
            cwd: Some(there.path().to_path_buf()),
            detach: true,
            ..NewArgs::default()
        };
        output(|out| new(&api, &inv, args, out));
        let created = api.created.lock().unwrap();
        let canonical = std::fs::canonicalize(there.path()).unwrap();
        assert_eq!(created[0].scope, Some(ScopeRoot::from_canonical(canonical)));
        assert_eq!(created[0].cwd, Some(there.path().to_path_buf()));
    }

    #[test]
    fn inside_a_session_new_needs_detach_and_names_no_folder() {
        let dir = tempfile::tempdir().unwrap();
        let inv = invocation(dir.path());
        let api = FakeApi::session_caller("ME", "/repo", vec![]);
        let err = new(&api, &inv, NewArgs::default(), &mut Vec::new()).unwrap_err();
        assert!(format!("{err:?}").contains("--detach"), "{err:?}");
        assert!(api.created.lock().unwrap().is_empty());

        let args = NewArgs {
            name: Some("helper".into()),
            detach: true,
            ..NewArgs::default()
        };
        let (code, _) = output(|out| new(&api, &inv, args, out));
        assert_eq!(code, 0);
        // The daemon puts it in the calling session's folder.
        assert_eq!(api.created.lock().unwrap()[0].scope, None);
    }

    fn ls_args(json: bool, dead: bool) -> LsArgs {
        LsArgs {
            json,
            dead,
            ..LsArgs::default()
        }
    }

    #[test]
    fn ls_prints_a_table_or_json() {
        let dir = tempfile::tempdir().unwrap();
        let canonical = std::fs::canonicalize(dir.path()).unwrap();
        let mut agent = in_scope("01A", "api", &canonical);
        agent.cwd = canonical.join("server");
        agent.lifecycle = Lifecycle::Working;
        agent.agent = Some(AgentInfo {
            kind: "claude".into(),
            ..AgentInfo::default()
        });
        agent.reported = Some(Reported {
            state: None,
            text: Some("fixing tests".into()),
            result: None,
            at: 0,
        });
        let shell = in_scope("01B", "shell", &canonical);
        let api = FakeApi::user(vec![agent, shell]);
        let inv = Invocation {
            self_id: Some(SessionId::from_string("01B")),
            ..invocation(dir.path())
        };

        let (_, text) = output(|out| ls(&api, &inv, ls_args(false, false), out));
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 3, "{text}");
        assert!(lines[0].starts_with("   NAME"), "{text}");
        assert!(lines[1].contains("api") && lines[1].contains("working"));
        assert!(lines[1].contains("claude") && lines[1].contains("server"));
        assert!(lines[0].ends_with("NOTE"), "{text}");
        assert!(lines[1].ends_with("fixing tests"));
        assert!(
            lines[2].starts_with('*') && lines[2].contains(" . "),
            "{text}"
        );
        let quiet = FakeApi::user(vec![session("01C", "quiet")]);
        let (_, text) = output(|out| ls(&quiet, &inv, ls_args(false, false), out));
        assert!(text.lines().next().unwrap().ends_with("ID"), "{text}");
        assert!(text.starts_with("NAME"), "no marks, no mark column: {text}");
        assert_eq!(
            api.calls()[0],
            format!(
                "list Some({:?}) dead=false",
                canonical.display().to_string()
            )
        );

        let (_, text) = output(|out| ls(&api, &inv, ls_args(true, true), out));
        let value: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(value["sessions"].as_array().unwrap().len(), 2);
        assert_eq!(value["sessions"][0]["name"], "api");
        // `folder` next to the `scope` older scripts read.
        assert_eq!(
            value["sessions"][0]["folder"],
            canonical.display().to_string()
        );
        assert_eq!(
            value["sessions"][0]["scope"],
            value["sessions"][0]["folder"]
        );
        assert!(api.calls()[1].ends_with("dead=true"));

        let empty = FakeApi::user(vec![]);
        let (_, text) = output(|out| ls(&empty, &inv, ls_args(false, false), out));
        assert_eq!(text, format!("No sessions in {}\n", canonical.display()));
    }

    #[test]
    fn ls_shows_pins_and_git_and_puts_pinned_sessions_first() {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let older = in_scope("01A", "older", &root);
        let mut pinned = in_scope("01B", "pinned", &root);
        pinned.pinned = true;
        pinned.git = Some(session_core::model::GitInfo {
            repo_root: root.clone(),
            branch: Some("fix-login".into()),
            detached_at: None,
            worktree: Some("fix-login".into()),
        });
        let api = FakeApi::user(vec![older, pinned]);
        let (_, text) = output(|out| ls(&api, &invocation(&root), ls_args(false, false), out));
        let lines: Vec<&str> = text.lines().collect();
        assert!(lines[0].starts_with("   NAME"), "{text}");
        assert!(lines[0].contains(" GIT "), "{text}");
        assert!(lines[1].starts_with("^  pinned"), "{text}");
        assert!(
            lines[1].contains("fix-login (worktree fix-login)"),
            "{text}"
        );
        assert!(lines[2].contains("older"), "{text}");
        assert!(!lines[0].contains("FOLDER"), "{text}");
    }

    #[test]
    fn ls_lists_several_folders_or_all_of_them() {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        for sub in ["api", "web", "a/app", "b/app"] {
            std::fs::create_dir_all(root.join(sub)).unwrap();
        }
        let api_dir = root.join("api");
        let web_dir = root.join("web");
        let mut web_pinned = in_scope("01W2", "w2", &web_dir);
        web_pinned.pinned = true;
        let fake = FakeApi::user(vec![
            in_scope("01W1", "w1", &web_dir),
            in_scope("01A1", "a1", &api_dir),
            web_pinned,
            in_scope("01X", "x", &root.join("a/app")),
            in_scope("01Y", "y", &root.join("b/app")),
        ]);
        let inv = invocation(&root);

        let args = LsArgs {
            folders: vec!["web".into(), api_dir.clone(), "web/".into()],
            ..LsArgs::default()
        };
        let (_, text) = output(|out| ls(&fake, &inv, args, out));
        assert_eq!(
            fake.calls().last().unwrap(),
            &format!(
                "list_in [{:?}, {:?}] dead=false",
                web_dir.display().to_string(),
                api_dir.display().to_string()
            )
        );
        let names: Vec<&str> = text
            .lines()
            .skip(1)
            .map(|line| line.split_whitespace().find(|w| *w != "^").unwrap())
            .collect();
        // Grouped by folder in listing order, pinned first in each.
        assert_eq!(names, ["w2", "w1", "a1"], "{text}");
        assert!(text.lines().next().unwrap().contains("FOLDER"), "{text}");
        assert!(text.lines().nth(1).unwrap().contains(" web "), "{text}");

        // From the command line: `--folder`, and `--project` as an alias.
        for line in [
            "ls --folder web --folder api",
            "ls --project web --folder api",
        ] {
            let command =
                CliArgs::try_parse_from(std::iter::once("tui").chain(line.split_whitespace()))
                    .unwrap()
                    .command;
            let (_, again) = output(|out| run(&fake, &inv, command, out));
            assert_eq!(again, text, "{line}");
            assert!(fake.calls().last().unwrap().starts_with("list_in ["));
        }

        *fake.projects.lock().unwrap() = ["api", "web", "a/app", "b/app"]
            .iter()
            .map(|sub| folder(&root.join(sub)))
            .collect();
        let args = LsArgs {
            all: true,
            dead: true,
            ..LsArgs::default()
        };
        let (_, text) = output(|out| ls(&fake, &inv, args, out));
        assert!(fake.calls().last().unwrap().ends_with("dead=true"));
        assert_eq!(text.lines().count(), 6, "{text}");
        // Folders that share a name are shown by path.
        let x = text.lines().find(|l| l.contains(" x ")).unwrap();
        assert!(x.contains(&root.join("a/app").display().to_string()), "{x}");

        let none = FakeApi::user(vec![]);
        let args = LsArgs {
            all: true,
            ..LsArgs::default()
        };
        let (_, text) = output(|out| ls(&none, &inv, args, out));
        assert_eq!(text, "No folders yet\n");
        let args = LsArgs {
            folders: vec![api_dir.clone(), web_dir.clone()],
            ..LsArgs::default()
        };
        let (_, text) = output(|out| ls(&none, &inv, args, out));
        assert_eq!(text, "No sessions in 2 folders\n");
    }

    #[test]
    fn pin_and_unpin_resolve_names() {
        let dir = tempfile::tempdir().unwrap();
        let inv = invocation(dir.path());
        let api = FakeApi::user(vec![session("S1", "api")]);
        let (_, text) = output(|out| pin(&api, &inv, "api", true, out));
        assert_eq!(text, "Pinned api\n");
        assert!(api.sessions.lock().unwrap()[0].pinned);
        let (_, text) = output(|out| {
            run(
                &api,
                &inv,
                Command::Unpin {
                    target: "S1".into(),
                },
                out,
            )
        });
        assert_eq!(text, "Unpinned api\n");
        let calls = api.calls();
        assert!(calls.contains(&"pin S1 true".to_string()), "{calls:?}");
        assert!(calls.contains(&"pin S1 false".to_string()), "{calls:?}");
        assert!(matches!(
            pin(&api, &inv, "nope", true, &mut Vec::new()),
            Err(CliError::Api(e)) if e.code == ErrorCode::NotFound
        ));
    }

    #[test]
    fn status_and_time_columns() {
        let mut s = session("1", "x");
        assert_eq!(status_text(&s), "running");
        s.lifecycle = Lifecycle::Blocked;
        assert_eq!(status_text(&s), "blocked");
        s.status = SessionStatus::Exited;
        s.exit_code = Some(130);
        assert_eq!(status_text(&s), "exited (130)");
        s.status = SessionStatus::Dead;
        assert_eq!(status_text(&s), "dead");
        let scope = ScopeRoot::from_canonical("/p".into());
        assert_eq!(relative_cwd(Path::new("/p"), &scope), ".");
        assert_eq!(relative_cwd(Path::new("/p/a/b"), &scope), "a/b");
        assert_eq!(relative_cwd(Path::new("/q"), &scope), "/q");
    }

    #[test]
    fn rename_defaults_to_the_current_session() {
        let dir = tempfile::tempdir().unwrap();
        let api = FakeApi::session_caller("S1", "/scope", vec![session("S1", "old")]);
        let inv = Invocation {
            self_id: Some(SessionId::from_string("S1")),
            ..invocation(dir.path())
        };
        let (_, text) = output(|out| rename(&api, &inv, &["build fix".into()], out));
        assert_eq!(text, "Renamed to build fix\n");
        assert_eq!(api.calls(), vec!["rename S1 build fix"]);

        // Outside a session the target is required.
        let outside = invocation(dir.path());
        let error = rename(&api, &outside, &["x".into()], &mut Vec::new()).unwrap_err();
        assert!(matches!(error, CliError::Usage(_)));

        // Two names: resolve the first in scope.
        let api = FakeApi::user(vec![session("S1", "old"), session("S2", "other")]);
        output(|out| rename(&api, &outside, &["other".into(), "new".into()], out));
        assert_eq!(api.calls().last().unwrap(), "rename S2 new");
        let error = rename(&api, &outside, &["S1".into(), "  ".into()], &mut Vec::new());
        assert!(matches!(error, Err(CliError::Usage(_))));
    }

    #[test]
    fn kill_and_rm_resolve_names() {
        let dir = tempfile::tempdir().unwrap();
        let inv = invocation(dir.path());
        let api = FakeApi::user(vec![session("S1", "api")]);
        let (_, text) = output(|out| kill(&api, &inv, "api", true, out));
        assert_eq!(text, "Killed api\n");
        assert!(api.calls().contains(&"kill S1 tree=true".to_string()));
        let (_, text) = output(|out| rm(&api, &inv, "S1", out));
        assert_eq!(text, "Deleted api\n");
        assert!(api.calls().contains(&"delete S1".to_string()));

        *api.fail_next.lock().unwrap() =
            Some(ApiError::new(ErrorCode::PermissionDenied, "not yours"));
        let error = rm(&api, &inv, "api", &mut Vec::new()).unwrap_err();
        assert_eq!(
            error,
            CliError::Api(ApiError::new(ErrorCode::PermissionDenied, "not yours"))
        );
    }

    #[test]
    fn attach_refuses_without_a_terminal_or_into_itself() {
        let dir = tempfile::tempdir().unwrap();
        let inv = Invocation {
            self_id: Some(SessionId::from_string("S1")),
            ..invocation(dir.path())
        };
        let api = FakeApi::user(vec![session("S1", "me")]);
        let error = run(
            &api,
            &inv,
            Command::Attach {
                target: "me".into(),
            },
            &mut Vec::new(),
        )
        .unwrap_err();
        assert!(matches!(error, CliError::Api(e) if e.code == ErrorCode::InvalidArgument));
        // Nothing was attached.
        assert!(api.calls().iter().all(|c| !c.starts_with("attach")));
    }
}
