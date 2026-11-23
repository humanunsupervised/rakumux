//! `rakumux workspace`: the daemon's workspaces, named groups of this
//! machine's sessions. `ls` lists them with their sessions' count, `add`
//! makes one, `rename` renames one, `rm` forgets one (its sessions are then in none), `move` puts a
//! session in one (made when there's none of that name) or, with `--none`,
//! in none. Inside a session `ls` works and `move` moves that session
//! itself; making and forgetting workspaces are the user's.

use std::io::Write;

use serde_json::json;
use session_core::api::{ApiError, ErrorCode};
use session_core::client::SessionApi;
use session_core::workspace::Workspace;

use super::args::WorkspaceCommand;
use super::table::{self, Column};
use super::{output_error, resolve_target, CliError, Invocation};
use crate::printable::printable;

pub(crate) fn run(
    api: &dyn SessionApi,
    invocation: &Invocation,
    command: WorkspaceCommand,
    out: &mut dyn Write,
) -> Result<i32, CliError> {
    match command {
        WorkspaceCommand::Ls { json } => ls(api, json, out),
        WorkspaceCommand::Add { name } => {
            let workspace = api.workspace_create(&name)?;
            writeln!(out, "Made workspace {}", printable(&workspace.name)).map_err(output_error)?;
            Ok(0)
        }
        WorkspaceCommand::Rm { workspace } => {
            let workspace = find(api, &workspace)?.ok_or_else(|| not_found(&workspace))?;
            api.workspace_delete(&workspace.id)?;
            writeln!(out, "Deleted workspace {}", printable(&workspace.name))
                .map_err(output_error)?;
            Ok(0)
        }
        WorkspaceCommand::Rename { workspace, name } => {
            let found = find(api, &workspace)?.ok_or_else(|| not_found(&workspace))?;
            let renamed = api.workspace_rename(&found.id, &name)?;
            writeln!(
                out,
                "Renamed workspace {} to {}",
                printable(&found.name),
                printable(&renamed.name)
            )
            .map_err(output_error)?;
            Ok(0)
        }
        WorkspaceCommand::Move {
            target,
            workspace,
            none,
        } => {
            let scope = invocation.request_scope(api)?;
            let session = resolve_target(api, scope.as_ref(), &target)?;
            let workspace = match (workspace, none) {
                (_, true) | (None, _) => None,
                (Some(name), false) => Some(match find(api, &name)? {
                    Some(workspace) => workspace,
                    None => api.workspace_create(&name)?,
                }),
            };
            let moved = api.set_workspace(&session.id, workspace.as_ref().map(|w| &w.id))?;
            let to = workspace.map_or_else(|| "No workspace".to_owned(), |w| w.name);
            writeln!(
                out,
                "Moved {} to {}",
                printable(&moved.name),
                printable(&to)
            )
            .map_err(output_error)?;
            Ok(0)
        }
    }
}

fn not_found(name: &str) -> CliError {
    CliError::Api(ApiError::new(
        ErrorCode::NotFound,
        format!("no workspace `{name}`"),
    ))
}

/// The workspace named or with the id `wanted`: its id, else its exact
/// name, else its name ignoring case.
fn find(api: &dyn SessionApi, wanted: &str) -> Result<Option<Workspace>, CliError> {
    let workspaces = api.workspaces()?;
    let id = wanted.strip_prefix("ws_").unwrap_or(wanted);
    let found = workspaces
        .iter()
        .find(|w| w.id.as_str() == id)
        .or_else(|| workspaces.iter().find(|w| w.name == wanted))
        .or_else(|| {
            workspaces
                .iter()
                .find(|w| w.name.to_lowercase() == wanted.trim().to_lowercase())
        });
    Ok(found.cloned())
}

fn ls(api: &dyn SessionApi, json: bool, out: &mut dyn Write) -> Result<i32, CliError> {
    let workspaces = api.workspaces()?;
    let sessions = api.list_all(true)?;
    let count = |workspace: Option<&Workspace>| {
        sessions
            .iter()
            .filter(|s| s.workspace.as_ref() == workspace.map(|w| &w.id))
            .count()
    };
    if json {
        let rows: Vec<serde_json::Value> = workspaces
            .iter()
            .map(|w| {
                json!({
                    "id": w.id,
                    "name": w.name,
                    "created_at": w.created_at,
                    "sessions": count(Some(w)),
                })
            })
            .collect();
        writeln!(
            out,
            "{}",
            json!({ "workspaces": rows, "unassigned": { "sessions": count(None) } })
        )
        .map_err(output_error)?;
        return Ok(0);
    }
    let mut names = vec!["No workspace".to_owned()];
    let mut counts = vec![count(None).to_string()];
    let mut ids = vec!["-".to_owned()];
    for workspace in &workspaces {
        names.push(printable(&workspace.name).to_string());
        counts.push(count(Some(workspace)).to_string());
        ids.push(format!("ws_{}", workspace.id));
    }
    let text = table::render(vec![
        Column::new("WORKSPACE", names),
        Column::new("SESSIONS", counts),
        Column::new("ID", ids),
    ]);
    out.write_all(text.as_bytes()).map_err(output_error)?;
    Ok(0)
}
