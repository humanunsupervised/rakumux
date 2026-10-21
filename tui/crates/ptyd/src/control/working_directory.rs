//! Validated, self-reported agent context. It never changes session scope.

use session_core::api::{AgentWorkingDirectory, ApiError, ErrorCode};

pub(super) fn canonicalize(
    context: &AgentWorkingDirectory,
) -> Result<AgentWorkingDirectory, ApiError> {
    let identifier =
        |text: &str| !text.is_empty() && text.len() <= 256 && !text.chars().any(char::is_control);
    if !identifier(&context.agent)
        || context
            .conversation_id
            .as_deref()
            .is_some_and(|id| !identifier(id))
        || !context.path.is_absolute()
        || context.path.as_os_str().len() > 4096
        || context.path.to_string_lossy().chars().any(char::is_control)
    {
        return Err(ApiError::new(
            ErrorCode::InvalidArgument,
            "invalid agent working directory",
        ));
    }
    let path = std::fs::canonicalize(&context.path).map_err(|_| {
        ApiError::new(
            ErrorCode::InvalidArgument,
            "agent working directory does not exist",
        )
    })?;
    if !path.is_dir() {
        return Err(ApiError::new(
            ErrorCode::InvalidArgument,
            "agent working directory is not a directory",
        ));
    }
    Ok(AgentWorkingDirectory {
        path,
        ..context.clone()
    })
}
