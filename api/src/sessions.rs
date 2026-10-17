//! Session use cases of the API: list (with filters), get, create, rename,
//! read, revive, kill and delete. Each takes the caller's `SessionApi`; the
//! daemon enforces permissions and folder confinement.

use crate::dto::{Content, CreateSessionBody, ReviveSessionBody, Session, UpdateSessionBody};
use crate::error::HttpError;
use crate::folders;
use crate::query::{ContentQuery, ListQuery};
use session_core::api::{
    CallerInfo, CreateSession, ErrorCode, ReadRequest, ReviveSession, CAPABILITY_SESSIONS_ALL,
};
use session_core::client::SessionApi;
use session_core::ids::SessionId;
use session_core::model::{ScopeRoot, SessionView};
use std::collections::HashSet;
use std::path::PathBuf;

/// `GET /v1/sessions`.
///
/// With no `folder`, session callers get their own folder and user callers
/// every known folder (which needs the daemon's `projects` capability;
/// without it the request fails with a hint to pass `folder`).
pub fn list(api: &dyn SessionApi, query: &ListQuery) -> Result<Vec<Session>, HttpError> {
    let mut scopes: Vec<ScopeRoot> = Vec::new();
    for input in &query.folders {
        let scope = folders::resolve(api, input)?;
        if !scopes.contains(&scope) {
            scopes.push(scope);
        }
    }
    let views = match (scopes.as_slice(), api.caller()) {
        ([], _) if query.all => every_folder(api, query.include_dead)?,
        ([scope], _) => api.list(Some(scope), query.include_dead)?,
        ([], CallerInfo::Session { .. }) => api.list(None, query.include_dead)?,
        ([], CallerInfo::User) => known_folders(api, query.include_dead)?,
        (several, _) => union(api, several, query.include_dead)?,
    };
    let needle = query.q.as_deref().map(str::to_lowercase);
    Ok(views
        .into_iter()
        .filter(|view| query.name.as_deref().is_none_or(|name| view.name == name))
        .filter(|view| {
            needle
                .as_deref()
                .is_none_or(|needle| view.name.to_lowercase().contains(needle))
        })
        .map(Session::from)
        .collect())
}

/// `all=true`: every folder's sessions, most recently active first, with
/// the daemon's list of every folder (capability `sessions.all`), else the
/// union of the folders it knows. Session callers see one folder only: the
/// daemon refuses them (`out_of_scope`).
fn every_folder(api: &dyn SessionApi, include_dead: bool) -> Result<Vec<SessionView>, HttpError> {
    let supported = api
        .daemon_capabilities()
        .is_none_or(|capabilities| capabilities.iter().any(|c| c == CAPABILITY_SESSIONS_ALL));
    if supported {
        match api.list_all(include_dead) {
            Ok(mut views) => {
                views.sort_by_key(|view| std::cmp::Reverse(view.last_active_at));
                return Ok(views);
            }
            Err(error) if error.code == ErrorCode::Unsupported => {}
            Err(error) => return Err(error.into()),
        }
    }
    match api.caller() {
        CallerInfo::User => known_folders(api, include_dead),
        CallerInfo::Session { scope, .. } => {
            Err(HttpError::from(session_core::api::ApiError::new(
                ErrorCode::OutOfScope,
                format!(
                    "sessions see only their own folder ({}); leave out all=true",
                    scope.path().display()
                ),
            )))
        }
    }
}

/// Every folder the daemon knows (`projects`), for user callers.
fn known_folders(api: &dyn SessionApi, include_dead: bool) -> Result<Vec<SessionView>, HttpError> {
    let all = match api.projects() {
        Ok(folders) => folders.into_iter().map(|f| f.path).collect::<Vec<_>>(),
        Err(error) if error.code == ErrorCode::Unsupported => {
            return Err(folder_required(
                "folder is required: this Rakumux daemon can't list every folder \
                 yet; pass ?folder=<directory> (repeatable)",
            ))
        }
        Err(error) => return Err(error.into()),
    };
    union(api, &all, include_dead)
}

/// 400 for a missing `folder`, marked with `details.missing = "folder"` so
/// clients can ask for it without parsing the message.
fn folder_required(message: &str) -> HttpError {
    HttpError::invalid(message).with_details(serde_json::json!({ "missing": "folder" }))
}

/// Sessions of several folders, most recently active first. Uses the
/// daemon's multi-folder list, or one list per folder when the daemon
/// doesn't support that yet.
fn union(
    api: &dyn SessionApi,
    scopes: &[ScopeRoot],
    include_dead: bool,
) -> Result<Vec<SessionView>, HttpError> {
    if scopes.is_empty() {
        return Ok(Vec::new());
    }
    let mut views = match api.list_in(scopes, include_dead) {
        Ok(views) => views,
        Err(error) if error.code == ErrorCode::Unsupported => {
            let mut views = Vec::new();
            for scope in scopes {
                views.extend(api.list(Some(scope), include_dead)?);
            }
            views
        }
        Err(error) => return Err(error.into()),
    };
    let mut seen = HashSet::new();
    views.retain(|view| seen.insert(view.id.clone()));
    views.sort_by_key(|view| std::cmp::Reverse(view.last_active_at));
    Ok(views)
}

/// `GET /v1/sessions/{id}`.
pub fn get(api: &dyn SessionApi, id: &SessionId) -> Result<Session, HttpError> {
    Ok(api.get(id)?.into())
}

/// `POST /v1/sessions`.
pub fn create(api: &dyn SessionApi, body: CreateSessionBody) -> Result<Session, HttpError> {
    let requested = folders::pick(body.folder, body.project)?
        .map(|folder| folders::resolve(api, &folder))
        .transpose()?;
    let scope = match (api.caller(), requested) {
        (CallerInfo::User, Some(scope)) => Some(scope),
        (CallerInfo::User, None) => {
            return Err(folder_required(
                "folder is required: the directory the session belongs to",
            ))
        }
        // The daemon creates a session caller's sessions in its own folder.
        (CallerInfo::Session { .. }, None) => None,
        (CallerInfo::Session { scope: own, .. }, Some(scope)) if scope == *own => None,
        (CallerInfo::Session { scope: own, .. }, Some(scope)) => {
            return Err(HttpError::from(session_core::api::ApiError::new(
                ErrorCode::OutOfScope,
                format!(
                    "sessions may only create sessions in their own folder ({}), not {}",
                    own.path().display(),
                    scope.path().display()
                ),
            )))
        }
    };
    let request = CreateSession {
        scope,
        name: body.name,
        cwd: body.cwd.map(PathBuf::from),
        argv: body.command,
        startup_cmds: body.startup_commands.unwrap_or_default(),
        // The daemon picks the environment: the calling session's snapshot,
        // or its own for the user. The API's own environment belongs to
        // whichever process started it, not to the caller.
        env: Vec::new(),
        cols: body.cols.unwrap_or(0),
        rows: body.rows.unwrap_or(0),
    };
    Ok(api.create(request)?.into())
}

/// `PATCH /v1/sessions/{id}`.
pub fn update(
    api: &dyn SessionApi,
    id: &SessionId,
    body: UpdateSessionBody,
) -> Result<Session, HttpError> {
    let name = body
        .name
        .ok_or_else(|| HttpError::invalid("nothing to update: pass `name`"))?;
    Ok(api.rename(id, &name)?.into())
}

/// `POST /v1/sessions/{id}/revive`: a new terminal for a dead or exited
/// session, which runs its resume command.
pub fn revive(
    api: &dyn SessionApi,
    id: &SessionId,
    body: ReviveSessionBody,
) -> Result<Session, HttpError> {
    let request = ReviveSession {
        id: id.clone(),
        // As for create: the daemon picks the environment.
        env: Vec::new(),
        cols: body.cols.unwrap_or(0),
        rows: body.rows.unwrap_or(0),
        resume: true,
    };
    Ok(api.revive(request)?.into())
}

/// `POST /v1/sessions/{id}/kill`: end the session's program (not the
/// sessions it created). The record stays; the session becomes `exited`.
pub fn kill(api: &dyn SessionApi, id: &SessionId) -> Result<(), HttpError> {
    api.kill(id, false)?;
    Ok(())
}

/// `DELETE /v1/sessions/{id}`: end the program and remove the session.
pub fn delete(api: &dyn SessionApi, id: &SessionId) -> Result<(), HttpError> {
    Ok(api.delete(id)?)
}

/// `GET /v1/sessions/{id}/content`.
pub fn content(
    api: &dyn SessionApi,
    id: &SessionId,
    query: &ContentQuery,
) -> Result<Content, HttpError> {
    let result = api.read(ReadRequest {
        id: id.clone(),
        source: query.source.into(),
        lines: query.effective_lines(),
        format: query.format.into(),
    })?;
    Ok(Content::new(
        id.as_str(),
        query.source,
        query.format,
        result,
    ))
}

#[cfg(test)]
mod tests {
    //! The fallbacks for daemons without the `projects` capability (folders
    //! on the wire), against a fake whose `projects`/`list_in` keep the
    //! trait's `unsupported` defaults.

    use super::*;
    use session_core::api::{
        ApiError, AttachRequest, DaemonStatusInfo, ReadResult, Report, ReviveSession, SendRequest,
        SubscribeRequest, WaitRequest, WaitResult,
    };
    use session_core::client::{AttachStream, EventStream, Result};
    use session_core::model::{Creator, LaunchSpec, Lifecycle, SessionStatus};
    use std::path::Path;
    use std::sync::Mutex;

    struct OldDaemon {
        caller: CallerInfo,
        sessions: Vec<SessionView>,
        lists: Mutex<Vec<Option<ScopeRoot>>>,
    }

    fn view(id: &str, name: &str, folder: &Path, last_active_at: i64) -> SessionView {
        SessionView {
            id: SessionId::from_string(id),
            name: name.into(),
            scope: ScopeRoot::from_canonical(folder.to_path_buf()),
            is_self: false,
            cwd: folder.to_path_buf(),
            status: SessionStatus::Live,
            exit_code: None,
            lifecycle: Lifecycle::None,
            agent: None,
            reported: None,
            created_by: Creator::User,
            parent_id: None,
            depth: 0,
            children: vec![],
            launch: LaunchSpec::default(),
            resume: None,
            auto_named: false,
            description: None,
            notes: None,
            labels: Default::default(),
            private: false,
            created_at: 0,
            last_active_at,
            terminal: None,
            pinned: false,
            git: None,
        }
    }

    /// Two real folders (`a`, `b`) and a fake daemon with sessions in both.
    fn setup(
        caller: impl FnOnce(&Path) -> CallerInfo,
    ) -> (tempfile::TempDir, PathBuf, PathBuf, OldDaemon) {
        let dir = tempfile::tempdir().unwrap();
        let a = std::fs::canonicalize(dir.path()).unwrap().join("a");
        let b = a.with_file_name("b");
        std::fs::create_dir(&a).unwrap();
        std::fs::create_dir(&b).unwrap();
        let daemon = OldDaemon {
            caller: caller(&a),
            sessions: vec![
                view("A", "alpha", &a, 10),
                view("B", "Beta", &b, 30),
                view("C", "gamma", &a, 20),
            ],
            lists: Mutex::new(Vec::new()),
        };
        (dir, a, b, daemon)
    }

    fn unused<T>() -> Result<T> {
        Err(ApiError::new(
            ErrorCode::Internal,
            "not used by these tests",
        ))
    }

    impl SessionApi for OldDaemon {
        fn caller(&self) -> &CallerInfo {
            &self.caller
        }
        fn list(&self, scope: Option<&ScopeRoot>, _: bool) -> Result<Vec<SessionView>> {
            self.lists.lock().unwrap().push(scope.cloned());
            let scope = match (scope, &self.caller) {
                (Some(scope), _) => scope.clone(),
                (None, CallerInfo::Session { scope, .. }) => scope.clone(),
                (None, CallerInfo::User) => {
                    return Err(ApiError::new(ErrorCode::InvalidArgument, "scope required"))
                }
            };
            Ok(self
                .sessions
                .iter()
                .filter(|s| s.scope == scope)
                .cloned()
                .collect())
        }
        fn get(&self, _: &SessionId) -> Result<SessionView> {
            unused()
        }
        fn create(&self, request: CreateSession) -> Result<SessionView> {
            let folder = request
                .scope
                .map(|scope| scope.path().to_path_buf())
                .unwrap_or_else(|| PathBuf::from("/own"));
            Ok(view("N", "new", &folder, 0))
        }
        fn rename(&self, _: &SessionId, _: &str) -> Result<SessionView> {
            unused()
        }
        fn kill(&self, _: &SessionId, _: bool) -> Result<Vec<SessionId>> {
            unused()
        }
        fn delete(&self, _: &SessionId) -> Result<()> {
            unused()
        }
        fn revive(&self, _: ReviveSession) -> Result<SessionView> {
            unused()
        }
        fn report(&self, _: Report) -> Result<()> {
            unused()
        }
        fn read(&self, _: ReadRequest) -> Result<ReadResult> {
            unused()
        }
        fn wait(&self, _: WaitRequest) -> Result<WaitResult> {
            unused()
        }
        fn send(&self, _: SendRequest) -> Result<()> {
            unused()
        }
        fn submit(&self, _: &SessionId, _: &str) -> Result<()> {
            unused()
        }
        fn touch(&self, _: &SessionId) -> Result<()> {
            unused()
        }
        fn status(&self) -> Result<DaemonStatusInfo> {
            unused()
        }
        fn shutdown(&self) -> Result<()> {
            unused()
        }
        fn attach(&self, _: AttachRequest) -> Result<AttachStream> {
            unused()
        }
        fn subscribe(&self, _: SubscribeRequest) -> Result<EventStream> {
            unused()
        }
    }

    fn query(folders: &[&Path]) -> ListQuery {
        ListQuery {
            folders: folders.iter().map(|p| p.display().to_string()).collect(),
            ..ListQuery::default()
        }
    }

    #[test]
    fn a_user_without_folder_is_asked_for_one() {
        let (_dir, _, _, daemon) = setup(|_| CallerInfo::User);
        let missing = Some(serde_json::json!({"missing": "folder"}));
        let error = list(&daemon, &query(&[])).unwrap_err();
        assert_eq!(error.status.as_u16(), 400);
        assert_eq!(error.body.details, missing);
        let error = create(&daemon, CreateSessionBody::default()).unwrap_err();
        assert_eq!(error.body.details, missing);
    }

    #[test]
    fn all_falls_back_like_a_list_without_folder() {
        let all = ListQuery {
            all: true,
            ..ListQuery::default()
        };
        let (_dir, _, _, daemon) = setup(|_| CallerInfo::User);
        let error = list(&daemon, &all).unwrap_err();
        assert_eq!(error.status.as_u16(), 400);
        assert_eq!(
            error.body.details,
            Some(serde_json::json!({"missing": "folder"}))
        );
        let (_dir, _, _, daemon) = setup(|own| CallerInfo::Session {
            id: SessionId::from_string("S"),
            scope: ScopeRoot::from_canonical(own.to_path_buf()),
        });
        let error = list(&daemon, &all).unwrap_err();
        assert_eq!(error.status.as_u16(), 403);
        assert_eq!(error.code(), "out_of_scope");
    }

    #[test]
    fn several_folders_fall_back_to_one_list_each() {
        let (_dir, a, b, daemon) = setup(|_| CallerInfo::User);
        let sessions = list(&daemon, &query(&[&a, &b, &a])).unwrap();
        let ids: Vec<&str> = sessions.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, ["B", "C", "A"], "newest activity first");
        assert_eq!(daemon.lists.lock().unwrap().len(), 2, "each folder once");
        let filtered = list(
            &daemon,
            &ListQuery {
                q: Some("BET".into()),
                ..query(&[&a, &b])
            },
        )
        .unwrap();
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].name, "Beta");
    }

    #[test]
    fn folder_names_need_the_registry() {
        let (_dir, _, _, daemon) = setup(|_| CallerInfo::User);
        let error = list(
            &daemon,
            &ListQuery {
                folders: vec!["a".into()],
                ..ListQuery::default()
            },
        )
        .unwrap_err();
        assert_eq!(error.status.as_u16(), 400);
        assert!(error.body.message.contains("can't look folders up by name"));
    }

    #[test]
    fn create_takes_folder_or_its_project_alias() {
        let (_dir, a, b, daemon) = setup(|_| CallerInfo::User);
        let body = |folder: Option<&Path>, project: Option<&Path>| CreateSessionBody {
            folder: folder.map(|p| p.display().to_string()),
            project: project.map(|p| p.display().to_string()),
            ..CreateSessionBody::default()
        };
        assert_eq!(create(&daemon, body(Some(&a), None)).unwrap().folder, a);
        let alias = create(&daemon, body(None, Some(&b))).unwrap();
        assert_eq!(alias.folder, b);
        assert_eq!(alias.project, b, "the deprecated field stays");
        assert_eq!(create(&daemon, body(Some(&a), Some(&a))).unwrap().folder, a);
        let error = create(&daemon, body(Some(&a), Some(&b))).unwrap_err();
        assert_eq!(error.status.as_u16(), 400);
        assert!(error.body.message.contains("`folder` and `project`"));
    }

    #[test]
    fn sessions_list_and_create_in_their_own_folder() {
        let (_dir, a, b, daemon) = setup(|own| CallerInfo::Session {
            id: SessionId::from_string("S"),
            scope: ScopeRoot::from_canonical(own.to_path_buf()),
        });
        let own = list(&daemon, &query(&[])).unwrap();
        assert_eq!(own.len(), 2);
        assert_eq!(daemon.lists.lock().unwrap().as_slice(), [None]);

        // Naming another folder is refused before the daemon is asked.
        let other = CreateSessionBody {
            folder: Some(b.display().to_string()),
            ..CreateSessionBody::default()
        };
        let error = create(&daemon, other).unwrap_err();
        assert_eq!(error.status.as_u16(), 403);
        assert_eq!(error.code(), "out_of_scope");
        assert!(error.body.message.contains("their own folder"));
        // The own folder (or none) is left to the daemon.
        let mine = CreateSessionBody {
            folder: Some(a.display().to_string()),
            ..CreateSessionBody::default()
        };
        assert_eq!(create(&daemon, mine).unwrap().folder, Path::new("/own"));
    }
}
