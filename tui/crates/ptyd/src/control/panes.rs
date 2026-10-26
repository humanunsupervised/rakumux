//! User-only terminal panes, deliberately outside the session registry.

use super::env;
use crate::host::{HostError, PaneAttachment, PaneSpawnSpec, TerminalHost, TerminalProgram};
use session_core::api::{ApiError, ErrorCode, OpenTerminal, Role, TerminalAttachRequest};
use std::path::Path;

const MAX_PANES: usize = 32;
const MAX_CELLS: usize = 500_000;

pub(super) fn open(
    host: &dyn TerminalHost,
    request: &OpenTerminal,
    binary: Option<&Path>,
) -> Result<PaneAttachment, ApiError> {
    validate_size(request.cols, request.rows)?;
    env::validate(&request.env)?;
    if host.pane_count() >= MAX_PANES {
        return Err(ApiError::new(
            ErrorCode::LimitExceeded,
            "too many temporary terminal panes",
        ));
    }
    if host.pane_info(&request.id).is_some() {
        return Err(ApiError::new(
            ErrorCode::InvalidArgument,
            "terminal pane already exists",
        ));
    }
    let cwd = std::fs::canonicalize(&request.cwd).map_err(|error| {
        ApiError::new(
            ErrorCode::InvalidArgument,
            format!("can't open terminal folder: {error}"),
        )
    })?;
    if !cwd.is_dir() {
        return Err(ApiError::new(
            ErrorCode::InvalidArgument,
            "terminal folder is not a directory",
        ));
    }
    let inherited = env::inherited_env(&request.env, None);
    host.spawn_pane(PaneSpawnSpec {
        id: request.id.clone(),
        program: TerminalProgram {
            argv: env::login_shell(&inherited),
            cwd,
            env: env::terminal_env(&inherited, binary),
            cols: request.cols,
            rows: request.rows,
            startup_input: Vec::new(),
        },
    })
    .map_err(host_error)?;
    match host.attach_pane(&request.id, Role::Controller, request.cols, request.rows) {
        Ok(attachment) => Ok(attachment),
        Err(error) => {
            host.close_pane(&request.id);
            Err(host_error(error))
        }
    }
}

pub(super) fn attach(
    host: &dyn TerminalHost,
    request: &TerminalAttachRequest,
) -> Result<PaneAttachment, ApiError> {
    validate_size(request.cols, request.rows)?;
    host.attach_pane(&request.id, request.role, request.cols, request.rows)
        .map_err(host_error)
}

fn validate_size(cols: u16, rows: u16) -> Result<(), ApiError> {
    if cols < 2 || rows < 2 || usize::from(cols) * usize::from(rows) > MAX_CELLS {
        return Err(ApiError::new(
            ErrorCode::InvalidArgument,
            "terminal pane size is outside display limits",
        ));
    }
    Ok(())
}

fn host_error(error: HostError) -> ApiError {
    let code = match error {
        HostError::NotFound | HostError::NotRunning => ErrorCode::NotFound,
        HostError::NotController => ErrorCode::PermissionDenied,
        HostError::Typing => ErrorCode::SessionInUse,
        HostError::InvalidKey(_) => ErrorCode::InvalidArgument,
        HostError::Spawn(_) | HostError::Io(_) => ErrorCode::Internal,
    };
    ApiError::new(code, error.to_string())
}
