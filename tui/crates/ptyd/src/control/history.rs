//! The session history: deleted sessions as `session_history` shows them,
//! and the name a restored one comes back under. The registry keeps them
//! (`deleted_sessions`); the service moves sessions in and out and purges
//! those deleted more than [`HISTORY_RETENTION_DAYS`] ago.
//!
//! [`HISTORY_RETENTION_DAYS`]: session_core::api::HISTORY_RETENTION_DAYS

use super::names::MAX_NAME_CHARS;
use super::registry::DeletedRecord;
use session_core::api::{ApiError, ErrorCode, HistoryPage, HISTORY_LIMIT, MAX_HISTORY_LIMIT};
use session_core::ids::SessionId;
use session_core::model::DeletedSession;
use std::collections::HashSet;

/// How often the session history is purged while the daemon runs.
pub(super) const PURGE_EVERY_MS: i64 = 24 * 60 * 60 * 1000;

/// The page of `records` (most recently deleted first) that `query`
/// matches, after `before` and at most `limit` long.
pub(super) fn page(
    records: Vec<DeletedRecord>,
    query: Option<&str>,
    limit: Option<u32>,
    before: Option<&SessionId>,
) -> Result<HistoryPage, ApiError> {
    let limit = limit.unwrap_or(HISTORY_LIMIT).clamp(1, MAX_HISTORY_LIMIT) as usize;
    let start = match before {
        None => 0,
        Some(before) => {
            records
                .iter()
                .position(|deleted| &deleted.record.id == before)
                .ok_or_else(|| {
                    ApiError::new(
                        ErrorCode::NotFound,
                        format!("no session {before} in the session history"),
                    )
                })?
                + 1
        }
    };
    let query = query.map(str::trim).filter(|q| !q.is_empty());
    let mut sessions: Vec<DeletedSession> = records
        .into_iter()
        .skip(start)
        .filter(|deleted| query.is_none_or(|query| matches(deleted, query)))
        .take(limit + 1)
        .map(entry)
        .collect();
    let more = sessions.len() > limit;
    sessions.truncate(limit);
    Ok(HistoryPage { sessions, more })
}

/// Whether `deleted`'s name, folder, agent, branch or repository contains
/// `query`, ignoring case.
pub(super) fn matches(deleted: &DeletedRecord, query: &str) -> bool {
    let query = query.to_lowercase();
    let record = &deleted.record;
    let agent = deleted.agent.as_ref().or(record.agent.as_ref());
    let git = deleted.git.as_ref();
    [
        Some(record.name.as_str()),
        record.scope.path().to_str(),
        agent.map(|agent| agent.kind.as_str()),
        git.and_then(|git| git.branch.as_deref()),
        git.and_then(|git| git.repo.as_deref()),
    ]
    .into_iter()
    .flatten()
    .any(|text| text.to_lowercase().contains(&query))
}

/// What the wire says of a deleted session.
fn entry(deleted: DeletedRecord) -> DeletedSession {
    let record = deleted.record;
    DeletedSession {
        agent: deleted.agent.or(record.agent),
        git: deleted.git,
        created_at: record.created_at_ms / 1000,
        last_active_at: record.last_active_at_ms / 1000,
        last_message_at: deleted.last_message_at_ms.map(|at| at / 1000),
        deleted_at: deleted.deleted_at_ms / 1000,
        id: record.id,
        name: record.name,
        scope: record.scope,
        cwd: record.cwd,
    }
}

/// `name`, else the first of `name 2`, `name 3`, … that isn't `taken` (the
/// names of its folder's sessions now), cut to fit [`MAX_NAME_CHARS`]; a
/// generated name once a thousand are.
pub(super) fn free_name(name: &str, taken: &HashSet<String>) -> String {
    if !taken.contains(name) {
        return name.to_string();
    }
    (2..1000)
        .map(|n| {
            let suffix = format!(" {n}");
            let room = MAX_NAME_CHARS - suffix.chars().count();
            let base: String = name.chars().take(room).collect();
            format!("{}{suffix}", base.trim_end())
        })
        .find(|candidate| !taken.contains(candidate))
        .unwrap_or_else(|| super::names::generate(|candidate| taken.contains(candidate)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn taken(names: &[&str]) -> HashSet<String> {
        names.iter().map(|name| name.to_string()).collect()
    }

    #[test]
    fn a_free_name_is_kept_and_a_taken_one_numbered() {
        assert_eq!(free_name("web", &taken(&[])), "web");
        assert_eq!(free_name("web", &taken(&["web"])), "web 2");
        assert_eq!(free_name("web", &taken(&["web", "web 2"])), "web 3");
    }

    #[test]
    fn a_numbered_name_fits_the_longest_name() {
        let long = "x".repeat(MAX_NAME_CHARS);
        let name = free_name(&long, &taken(&[long.as_str()]));
        assert_eq!(name.chars().count(), MAX_NAME_CHARS);
        assert!(name.ends_with(" 2"));
    }
}
