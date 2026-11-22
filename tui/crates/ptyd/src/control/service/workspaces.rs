//! Workspaces in the control layer: the `workspace_*` requests and
//! `session_workspace`. A workspace is a name the user gives a group of this
//! machine's sessions; a session is in one or in none. The registry holds
//! them (`registry/workspaces.rs`) and the state keeps a copy, so building a
//! view reads no database. Session callers list workspaces and move
//! themselves; the user makes and forgets them and moves any session.

use super::{audit, invalid, limit_exceeded, registry, Caller, Inner};
use crate::control::policy::{self, Op};
use session_core::api::{ApiError, ErrorCode, Response};
use session_core::ids::{SessionId, WorkspaceId};
use session_core::workspace::{checked_name, Workspace, MAX_WORKSPACES};

fn missing(id: &WorkspaceId) -> ApiError {
    ApiError::new(ErrorCode::NotFound, format!("no workspace ws_{id}"))
}

impl Inner {
    pub(super) fn workspace_list(&self, caller: &Caller) -> Result<Response, ApiError> {
        policy::authorize(caller, Op::WorkspaceList, None)?;
        let state = self.lock();
        Ok(Response::Workspaces {
            workspaces: state.workspaces.clone(),
        })
    }

    pub(super) fn workspace_create(
        &self,
        caller: &Caller,
        name: &str,
    ) -> Result<Response, ApiError> {
        let mut state = self.lock();
        let result = policy::authorize(caller, Op::WorkspaceCreate, None).and_then(|()| {
            let name = checked_name(name)
                .ok_or_else(|| invalid("a workspace's name is one line of 1 to 64 characters"))?;
            if state
                .workspaces
                .iter()
                .any(|workspace| workspace.name.to_lowercase() == name.to_lowercase())
            {
                return Err(invalid(format!(
                    "there is a workspace named {name} already"
                )));
            }
            if state.workspaces.len() >= MAX_WORKSPACES {
                return Err(limit_exceeded(
                    "workspaces",
                    MAX_WORKSPACES as u32,
                    format!("at most {MAX_WORKSPACES} workspaces"),
                ));
            }
            let now = self.now();
            let workspace = Workspace {
                id: WorkspaceId::generate(),
                name,
                created_at: now / 1000,
            };
            registry(&state.registry)?.insert_workspace(&workspace, now)?;
            state.workspaces.push(workspace.clone());
            Ok(workspace)
        });
        let detail = result.as_ref().ok().map(|w| audit::clip(&w.name));
        self.audit_outcome(
            &state,
            caller,
            Op::WorkspaceCreate,
            None,
            None,
            detail,
            &result,
        );
        result.map(|workspace| Response::Workspace { workspace })
    }

    pub(super) fn workspace_delete(
        &self,
        caller: &Caller,
        id: &WorkspaceId,
    ) -> Result<Response, ApiError> {
        let mut state = self.lock();
        let result = policy::authorize(caller, Op::WorkspaceDelete, None).and_then(|()| {
            let index = state
                .workspaces
                .iter()
                .position(|workspace| &workspace.id == id)
                .ok_or_else(|| missing(id))?;
            registry(&state.registry)?.delete_workspace(id)?;
            let name = state.workspaces.remove(index).name;
            let freed: Vec<SessionId> = state
                .session_workspaces
                .iter()
                .filter(|(_, workspace)| *workspace == id)
                .map(|(session, _)| session.clone())
                .collect();
            for session in &freed {
                state.session_workspaces.remove(session);
                self.refresh(&mut state, session);
            }
            Ok(name)
        });
        let detail = result.as_ref().ok().map(|name| audit::clip(name));
        self.audit_outcome(
            &state,
            caller,
            Op::WorkspaceDelete,
            None,
            None,
            detail,
            &result,
        );
        result.map(|_| Response::Ok)
    }

    pub(super) fn workspace_rename(
        &self,
        caller: &Caller,
        id: &WorkspaceId,
        name: &str,
    ) -> Result<Response, ApiError> {
        let mut state = self.lock();
        let result = policy::authorize(caller, Op::WorkspaceRename, None).and_then(|()| {
            let index = state
                .workspaces
                .iter()
                .position(|workspace| &workspace.id == id)
                .ok_or_else(|| missing(id))?;
            let name = checked_name(name)
                .ok_or_else(|| invalid("a workspace's name is one line of 1 to 64 characters"))?;
            let taken = state.workspaces.iter().any(|workspace| {
                &workspace.id != id && workspace.name.to_lowercase() == name.to_lowercase()
            });
            if taken {
                return Err(invalid(format!(
                    "there is a workspace named {name} already"
                )));
            }
            registry(&state.registry)?.rename_workspace(id, &name)?;
            state.workspaces[index].name = name;
            Ok(state.workspaces[index].clone())
        });
        let detail = result.as_ref().ok().map(|w| audit::clip(&w.name));
        self.audit_outcome(
            &state,
            caller,
            Op::WorkspaceRename,
            None,
            None,
            detail,
            &result,
        );
        result.map(|workspace| Response::Workspace { workspace })
    }

    pub(super) fn session_workspace(
        &self,
        caller: &Caller,
        id: &SessionId,
        workspace: Option<&WorkspaceId>,
    ) -> Result<Response, ApiError> {
        let mut state = self.lock();
        let record = self.target(&state, caller, Op::SessionWorkspace, id)?;
        let result = (|| {
            let name = match workspace {
                Some(workspace) => state
                    .workspaces
                    .iter()
                    .find(|known| &known.id == workspace)
                    .map(|known| known.name.clone())
                    .ok_or_else(|| missing(workspace))?,
                None => "none".to_owned(),
            };
            registry(&state.registry)?.set_session_workspace(id, workspace)?;
            match workspace {
                Some(workspace) => {
                    state
                        .session_workspaces
                        .insert(id.clone(), workspace.clone());
                }
                None => {
                    state.session_workspaces.remove(id);
                }
            }
            Ok(name)
        })();
        if result.is_ok() {
            self.refresh(&mut state, id);
        }
        let detail = result.as_ref().ok().map(|name| audit::clip(name));
        self.audit_outcome(
            &state,
            caller,
            Op::SessionWorkspace,
            Some(&record.scope),
            Some(id),
            detail,
            &result,
        );
        result?;
        self.current_view(&state, caller, id)
            .map(|session| Response::Session { session })
    }

    /// The workspace a new session starts in: a session caller's own (its
    /// request names none), else the one asked for, which must exist.
    pub(super) fn new_session_workspace(
        &self,
        state: &super::State,
        caller: &Caller,
        asked: Option<WorkspaceId>,
    ) -> Result<Option<WorkspaceId>, ApiError> {
        if let Some(parent) = caller.session_id() {
            return Ok(state.session_workspaces.get(parent).cloned());
        }
        match asked {
            Some(workspace) if !state.workspaces.iter().any(|known| known.id == workspace) => {
                Err(missing(&workspace))
            }
            asked => Ok(asked),
        }
    }
}

/// The workspaces and every session's, as the registry holds them.
pub(super) fn load(
    registry: &crate::control::registry::Registry,
) -> (
    Vec<Workspace>,
    std::collections::HashMap<SessionId, WorkspaceId>,
) {
    let workspaces = registry.workspaces().unwrap_or_else(|error| {
        crate::log_warn!("control: reading workspaces: {error}");
        Vec::new()
    });
    let sessions = registry.session_workspaces().unwrap_or_else(|error| {
        crate::log_warn!("control: reading session workspaces: {error}");
        Default::default()
    });
    (workspaces, sessions)
}
