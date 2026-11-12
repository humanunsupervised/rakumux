//! Heartbeat use cases of the API: list, get, create, change, enable or
//! pause, run now and delete. Each takes the caller's `SessionApi`; the
//! daemon decides what a caller may do (a session's heartbeats wait for the
//! user's approval; enabling, running and deleting are the user's).

use crate::dto::{CreateHeartbeatBody, Heartbeat, Session, UpdateHeartbeatBody};
use crate::error::HttpError;
use crate::folders;
use session_core::api::{ApiError, CallerInfo, ErrorCode};
use session_core::client::SessionApi;
use session_core::heartbeat::{HeartbeatEdit, HeartbeatFields, HeartbeatSpec};
use session_core::ids::HeartbeatId;

/// Longest accepted heartbeat id in a path.
const MAX_ID_LEN: usize = 128;

/// A heartbeat id from a path: `hb_…` or the bare id; anything else names
/// none.
pub fn id(raw: &str) -> Result<HeartbeatId, HttpError> {
    let plain = raw.len() <= MAX_ID_LEN
        && !raw.is_empty()
        && raw
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if plain {
        Ok(HeartbeatId::from_string(raw))
    } else {
        Err(HttpError::not_found(format!("no heartbeat {raw:?}")))
    }
}

/// `GET /v1/heartbeats`: every folder's, or `folder`'s.
pub fn list(api: &dyn SessionApi, folder: Option<&str>) -> Result<Vec<Heartbeat>, HttpError> {
    let scope = folder
        .map(|folder| folders::resolve(api, folder))
        .transpose()?;
    Ok(api
        .heartbeats(scope.as_ref())?
        .into_iter()
        .map(Heartbeat::from)
        .collect())
}

pub fn get(api: &dyn SessionApi, id: &HeartbeatId) -> Result<Heartbeat, HttpError> {
    Ok(api.heartbeat(id)?.into())
}

/// `POST /v1/heartbeats`.
pub fn create(api: &dyn SessionApi, body: CreateHeartbeatBody) -> Result<Heartbeat, HttpError> {
    let scope = body
        .folder
        .map(|folder| folders::resolve(api, &folder))
        .transpose()?;
    if scope.is_none() && matches!(api.caller(), CallerInfo::User) {
        return Err(HttpError::from(ApiError::new(
            ErrorCode::InvalidArgument,
            "folder is required: the directory the heartbeat's runs start in",
        )));
    }
    let spec = HeartbeatSpec {
        scope,
        name: body.name,
        fields: HeartbeatFields {
            agent: body.agent,
            command: body.command,
            prompt: body.prompt,
            schedule: body.schedule,
            by_agent: body.by_agent,
        },
        enabled: body.enabled,
    };
    Ok(api.heartbeat_create(spec)?.into())
}

/// `PATCH /v1/heartbeats/{id}`.
pub fn update(
    api: &dyn SessionApi,
    id: &HeartbeatId,
    body: UpdateHeartbeatBody,
) -> Result<Heartbeat, HttpError> {
    let edit = HeartbeatEdit {
        id: id.clone(),
        name: body.name,
        fields: HeartbeatFields {
            agent: body.agent,
            command: body.command,
            prompt: body.prompt,
            schedule: body.schedule,
            by_agent: body.by_agent,
        },
    };
    Ok(api.heartbeat_update(edit)?.into())
}

/// `POST /v1/heartbeats/{id}/enable` and `/pause`.
pub fn enable(
    api: &dyn SessionApi,
    id: &HeartbeatId,
    enabled: bool,
) -> Result<Heartbeat, HttpError> {
    Ok(api.heartbeat_enable(id, enabled)?.into())
}

/// `POST /v1/heartbeats/{id}/run`: the run's session.
pub fn run(api: &dyn SessionApi, id: &HeartbeatId) -> Result<Session, HttpError> {
    Ok(api.heartbeat_run(id)?.into())
}

pub fn delete(api: &dyn SessionApi, id: &HeartbeatId) -> Result<(), HttpError> {
    Ok(api.heartbeat_delete(id)?)
}
