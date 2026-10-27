//! The session registry: durable session records, the projects they belong
//! to and the audit log in SQLite at `Layout::registry_db()`. The daemon is
//! the only writer.
//!
//! Live state (terminal id, `Live` status, lifecycle) is kept in memory by
//! the service; the registry only knows whether a session ended
//! (`ended_at`), so after a daemon restart every session is `Dead` or
//! `Exited` without any reconcile writes.
//!
//! Timestamps are stored in Unix **milliseconds** so list ordering is stable
//! for sessions touched within the same second; views expose seconds.
//!
//! Every session's scope is a project: inserting a session records its
//! project, and a project that still has sessions can't be removed.

use super::audit::AuditEntry;
use rusqlite::{params, params_from_iter, Connection, OptionalExtension, Row, TransactionBehavior};
use session_core::api::{ApiError, ErrorCode};
use session_core::ids::SessionId;
use session_core::model::{
    AgentInfo, Creator, LaunchSpec, Reported, ResumeSpec, ScopeRoot, SessionPlace,
};
use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// `PRAGMA user_version` of the current schema: 2 introduced `sessions_v2`
/// (0.6), 3 added projects and pinning. Tables added since (`session_places`)
/// are created when missing, so older daemons still open the registry.
pub(crate) const SCHEMA_VERSION: i64 = 3;

/// Audit entries older than this are pruned when the registry opens.
pub(crate) const AUDIT_RETENTION_MS: i64 = 30 * 24 * 60 * 60 * 1000;

const BUSY_TIMEOUT: Duration = Duration::from_millis(2000);

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS sessions_v2 (
  id TEXT PRIMARY KEY,
  scope_root TEXT NOT NULL,
  name TEXT NOT NULL,
  cwd TEXT NOT NULL,
  launch_json TEXT NOT NULL,
  resume_json TEXT,
  created_by_json TEXT NOT NULL,
  parent_id TEXT,
  depth INTEGER NOT NULL DEFAULT 0,
  agent_json TEXT,
  reported_json TEXT,
  exit_code INTEGER,
  created_at INTEGER NOT NULL,
  last_active_at INTEGER NOT NULL,
  ended_at INTEGER,
  description TEXT,
  notes TEXT,
  labels_json TEXT,
  private INTEGER NOT NULL DEFAULT 0,
  auto_named INTEGER NOT NULL DEFAULT 0,
  agent_kind TEXT,
  agent_session_id TEXT,
  pinned INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS idx_sessions_v2_scope
  ON sessions_v2(scope_root, last_active_at DESC);
CREATE UNIQUE INDEX IF NOT EXISTS idx_sessions_v2_name
  ON sessions_v2(scope_root, name);
CREATE INDEX IF NOT EXISTS idx_sessions_v2_parent
  ON sessions_v2(parent_id) WHERE parent_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_sessions_v2_agent_session
  ON sessions_v2(agent_kind, agent_session_id) WHERE agent_session_id IS NOT NULL;
CREATE TABLE IF NOT EXISTS audit_log (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  ts INTEGER NOT NULL,
  scope_root TEXT,
  caller TEXT NOT NULL,
  method TEXT NOT NULL,
  target_session_id TEXT,
  decision TEXT NOT NULL,
  detail TEXT
);
CREATE INDEX IF NOT EXISTS idx_audit_log_ts ON audit_log(ts);
CREATE TABLE IF NOT EXISTS projects (
  path TEXT PRIMARY KEY,
  added_at INTEGER NOT NULL,
  last_used_at INTEGER
);
CREATE TABLE IF NOT EXISTS session_places (
  session_id TEXT NOT NULL,
  path TEXT NOT NULL,
  git_json TEXT,
  last_at INTEGER NOT NULL,
  edited INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY (session_id, path)
);
";

/// Schema 2 → 3: the projects of existing sessions (first created, last
/// active). Runs after `SCHEMA` and the `pinned` column.
const BACKFILL_PROJECTS: &str = "
INSERT OR IGNORE INTO projects (path, added_at, last_used_at)
  SELECT scope_root, MIN(created_at), MAX(last_active_at)
  FROM sessions_v2 GROUP BY scope_root;
";

/// Record that project `?1` was used at `?2` (a session was created,
/// touched or revived in it), adding it if needed.
const USE_PROJECT: &str = "
INSERT INTO projects (path, added_at, last_used_at) VALUES (?1, ?2, ?2)
  ON CONFLICT(path) DO UPDATE
  SET last_used_at = MAX(COALESCE(last_used_at, 0), excluded.last_used_at)
";

const COLUMNS: &str = "id, scope_root, name, cwd, launch_json, resume_json, created_by_json, \
     parent_id, depth, agent_json, reported_json, exit_code, created_at, last_active_at, ended_at, \
     description, notes, labels_json, private, auto_named, agent_kind, agent_session_id, pinned";

/// A durable session record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SessionRecord {
    pub id: SessionId,
    pub scope: ScopeRoot,
    pub name: String,
    pub cwd: PathBuf,
    pub launch: LaunchSpec,
    pub resume: Option<ResumeSpec>,
    pub created_by: Creator,
    pub parent_id: Option<SessionId>,
    pub depth: u8,
    /// Agent identity reported by the session (hooks, `session report`).
    pub agent: Option<AgentInfo>,
    pub reported: Option<Reported>,
    pub exit_code: Option<i32>,
    pub created_at_ms: i64,
    pub last_active_at_ms: i64,
    /// Set when the session's process ended; cleared on revive.
    pub ended_at_ms: Option<i64>,
    /// Stored for the agent API (not in `SessionView` yet).
    pub description: Option<String>,
    pub notes: Option<String>,
    pub labels: BTreeMap<String, String>,
    /// Hide output from other sessions' read/wait (agent API).
    pub private: bool,
    /// The name was generated by Rakumux; cleared by any rename.
    pub auto_named: bool,
    /// Pinned by the user (listed first in its project).
    pub pinned: bool,
}

/// A known project: a directory sessions belong to (the wire's scope).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProjectRecord {
    pub path: ScopeRoot,
    pub added_at_ms: i64,
    /// Last time a session was created, touched or revived in it.
    pub last_used_at_ms: Option<i64>,
}

/// Session totals of one project.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct ProjectSessions {
    pub count: u32,
    pub last_active_at_ms: Option<i64>,
}

/// Outcome of [`Registry::remove_project`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProjectRemoval {
    Removed,
    NotFound,
    /// The project still has this many sessions; nothing was removed.
    HasSessions(u32),
}

#[derive(Debug)]
pub(crate) enum RegistryError {
    /// Another session in the scope already has this name.
    NameTaken,
    /// A path that SQLite TEXT can't hold losslessly.
    NonUtf8Path(PathBuf),
    /// A stored row couldn't be decoded.
    Corrupt(String),
    Sql(rusqlite::Error),
}

impl fmt::Display for RegistryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RegistryError::NameTaken => write!(f, "name already used in this scope"),
            RegistryError::NonUtf8Path(p) => write!(f, "path is not valid UTF-8: {}", p.display()),
            RegistryError::Corrupt(m) => write!(f, "corrupt registry row: {m}"),
            RegistryError::Sql(e) => write!(f, "registry error: {e}"),
        }
    }
}

impl From<rusqlite::Error> for RegistryError {
    fn from(error: rusqlite::Error) -> Self {
        if let rusqlite::Error::SqliteFailure(ffi, _) = &error {
            if ffi.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_UNIQUE {
                return RegistryError::NameTaken;
            }
        }
        RegistryError::Sql(error)
    }
}

impl From<RegistryError> for ApiError {
    fn from(error: RegistryError) -> Self {
        let code = match error {
            RegistryError::NameTaken | RegistryError::NonUtf8Path(_) => ErrorCode::InvalidArgument,
            RegistryError::Corrupt(_) | RegistryError::Sql(_) => ErrorCode::Internal,
        };
        ApiError::new(code, error.to_string())
    }
}

pub(crate) type Result<T> = std::result::Result<T, RegistryError>;

pub(crate) struct Registry {
    conn: Connection,
}

impl Registry {
    /// Open (creating if needed) and migrate the registry (see [`migrate`]);
    /// a newer schema is refused. Audit entries older than 30 days are
    /// pruned.
    pub fn open(path: &Path, now_ms: i64) -> io::Result<Self> {
        if let Some(parent) = path.parent() {
            create_private_dir(parent)?;
        }
        let mut conn = Connection::open(path).map_err(sql_io)?;
        conn.busy_timeout(BUSY_TIMEOUT).map_err(sql_io)?;
        let mode: String = conn
            .pragma_update_and_check(None, "journal_mode", "WAL", |row| row.get(0))
            .map_err(sql_io)?;
        if !mode.eq_ignore_ascii_case("wal") {
            return Err(io::Error::other(format!(
                "registry journal mode is {mode}, expected wal"
            )));
        }
        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .map_err(sql_io)?;
        if version > SCHEMA_VERSION {
            return Err(io::Error::other(format!(
                "registry schema version {version} is newer than this daemon supports \
                     ({SCHEMA_VERSION}); upgrade rakumux"
            )));
        }
        if version < SCHEMA_VERSION {
            migrate(&mut conn, version).map_err(sql_io)?;
        } else {
            conn.execute_batch(SCHEMA).map_err(sql_io)?;
        }
        restrict_file(path);
        let registry = Self { conn };
        registry
            .prune_audit(now_ms - AUDIT_RETENTION_MS)
            .map_err(|e| io::Error::other(e.to_string()))?;
        Ok(registry)
    }

    /// Insert a session and record its project as used when it was created.
    pub fn insert(&self, record: &SessionRecord) -> Result<()> {
        self.insert_many(std::slice::from_ref(record))
    }

    /// Insert a batch atomically, leaving existing records untouched on any
    /// duplicate id, duplicate name or invalid record.
    pub fn insert_many(&self, records: &[SessionRecord]) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        for record in records {
            tx.execute(
                &format!(
                    "INSERT INTO sessions_v2 ({COLUMNS}) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, \
                 ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23)"
                ),
                params![
                    record.id.as_str(),
                    path_text(record.scope.path())?,
                    record.name,
                    path_text(&record.cwd)?,
                    to_json(&record.launch)?,
                    opt_json(record.resume.as_ref())?,
                    to_json(&record.created_by)?,
                    record.parent_id.as_ref().map(SessionId::as_str),
                    record.depth as i64,
                    opt_json(record.agent.as_ref())?,
                    opt_json(record.reported.as_ref())?,
                    record.exit_code,
                    record.created_at_ms,
                    record.last_active_at_ms,
                    record.ended_at_ms,
                    record.description,
                    record.notes,
                    labels_json(&record.labels)?,
                    record.private,
                    record.auto_named,
                    record.agent.as_ref().map(|a| a.kind.as_str()),
                    record
                        .agent
                        .as_ref()
                        .and_then(|a| a.agent_session_id.as_deref()),
                    record.pinned,
                ],
            )?;
            tx.execute(
                USE_PROJECT,
                params![path_text(record.scope.path())?, record.created_at_ms],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Read an archive without creating, migrating or modifying its database.
    pub fn read_archive(path: &Path) -> Result<Vec<SessionRecord>> {
        let conn = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        let version: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        if version != SCHEMA_VERSION {
            return Err(RegistryError::Corrupt(format!(
                "archive schema {version}; expected {SCHEMA_VERSION}"
            )));
        }
        let archive = Self { conn };
        let (records, bad) = archive.list_all()?;
        if let Some(error) = bad.into_iter().next() {
            return Err(error);
        }
        Ok(records)
    }

    /// Move a folder's saved references atomically. Current working
    /// directories in that tree also move, even for sessions of other scopes.
    pub fn relocate_project(&self, from: &ScopeRoot, to: &ScopeRoot) -> Result<Vec<SessionId>> {
        let (records, bad) = self.list_all()?;
        if let Some(error) = bad.into_iter().next() {
            return Err(error);
        }
        let transaction = self.conn.unchecked_transaction()?;
        let mut changed = Vec::new();
        for mut record in records {
            let moved_scope = record.scope == *from;
            let moved_cwd = record
                .cwd
                .strip_prefix(from.path())
                .ok()
                .map(|rest| to.path().join(rest));
            if !moved_scope && moved_cwd.is_none() {
                continue;
            }
            if moved_scope {
                record.scope = to.clone();
            }
            if let Some(cwd) = moved_cwd {
                record.cwd = cwd;
            }
            transaction.execute(
                "UPDATE sessions_v2 SET scope_root = ?2, cwd = ?3 WHERE id = ?1",
                params![
                    record.id.as_str(),
                    path_text(record.scope.path())?,
                    path_text(&record.cwd)?
                ],
            )?;
            changed.push(record.id);
        }
        transaction.execute(
            "INSERT INTO projects (path, added_at, last_used_at) \
             SELECT ?2, added_at, last_used_at FROM projects WHERE path = ?1 \
             ON CONFLICT(path) DO UPDATE SET \
             added_at = MIN(projects.added_at, excluded.added_at), \
             last_used_at = MAX(COALESCE(projects.last_used_at, 0), COALESCE(excluded.last_used_at, 0))",
            params![path_text(from.path())?, path_text(to.path())?],
        )?;
        transaction.execute(
            "DELETE FROM projects WHERE path = ?1",
            [path_text(from.path())?],
        )?;
        transaction.commit()?;
        Ok(changed)
    }

    pub fn get(&self, id: &SessionId) -> Result<Option<SessionRecord>> {
        let row = self
            .conn
            .query_row(
                &format!("SELECT {COLUMNS} FROM sessions_v2 WHERE id = ?1"),
                [id.as_str()],
                RawRecord::from_row,
            )
            .optional()?;
        row.map(RawRecord::decode).transpose()
    }

    /// Sessions of a scope, most recently active first (ties: newest first).
    /// Rows that fail to decode are skipped and returned separately so the
    /// caller can log them; one bad row must not hide a whole scope.
    pub fn list(&self, scope: &ScopeRoot) -> Result<(Vec<SessionRecord>, Vec<RegistryError>)> {
        self.list_in(std::slice::from_ref(scope))
    }

    /// Sessions of several scopes, ordered and decoded as in [`Self::list`].
    pub fn list_in(
        &self,
        scopes: &[ScopeRoot],
    ) -> Result<(Vec<SessionRecord>, Vec<RegistryError>)> {
        if scopes.is_empty() {
            return Ok((Vec::new(), Vec::new()));
        }
        let paths = scopes
            .iter()
            .map(|scope| path_text(scope.path()))
            .collect::<Result<Vec<_>>>()?;
        let placeholders = (1..=paths.len())
            .map(|i| format!("?{i}"))
            .collect::<Vec<_>>()
            .join(", ");
        self.select_records(&format!("WHERE scope_root IN ({placeholders})"), paths)
    }

    /// Sessions of every scope, ordered and decoded as in [`Self::list`].
    /// This reads the whole table, so SQLite scans and sorts it; an index on
    /// `last_active_at` alone would add a write to every touch for no gain
    /// at the size of a person's registry.
    pub fn list_all(&self) -> Result<(Vec<SessionRecord>, Vec<RegistryError>)> {
        self.select_records("", Vec::new())
    }

    /// The sessions matching `filter` (a `WHERE` clause over `params`, or
    /// empty), most recently active first, decoded as in [`Self::list`].
    fn select_records(
        &self,
        filter: &str,
        params: Vec<&str>,
    ) -> Result<(Vec<SessionRecord>, Vec<RegistryError>)> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {COLUMNS} FROM sessions_v2 {filter} ORDER BY last_active_at DESC, id DESC"
        ))?;
        let rows = stmt.query_map(params_from_iter(params), RawRecord::from_row)?;
        let mut records = Vec::new();
        let mut bad = Vec::new();
        for row in rows {
            match row?.decode() {
                Ok(record) => records.push(record),
                Err(error) => bad.push(error),
            }
        }
        Ok((records, bad))
    }

    /// Direct children of `id`, oldest first.
    pub fn children(&self, id: &SessionId) -> Result<Vec<SessionId>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id FROM sessions_v2 WHERE parent_id = ?1 ORDER BY created_at, id")?;
        let ids = stmt
            .query_map([id.as_str()], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(ids.into_iter().map(SessionId::from_string).collect())
    }

    /// `id` and all its descendants, parents before children.
    pub fn subtree(&self, id: &SessionId) -> Result<Vec<SessionId>> {
        let mut out = vec![id.clone()];
        let mut index = 0;
        while index < out.len() {
            let children = self.children(&out[index])?;
            for child in children {
                if !out.contains(&child) {
                    out.push(child);
                }
            }
            index += 1;
        }
        Ok(out)
    }

    pub fn name_taken(
        &self,
        scope: &ScopeRoot,
        name: &str,
        except: Option<&SessionId>,
    ) -> Result<bool> {
        let found: Option<String> = self
            .conn
            .query_row(
                "SELECT id FROM sessions_v2 WHERE scope_root = ?1 AND name = ?2",
                params![path_text(scope.path())?, name],
                |row| row.get(0),
            )
            .optional()?;
        Ok(match (found, except) {
            (None, _) => false,
            (Some(found), Some(except)) => found != except.as_str(),
            (Some(_), None) => true,
        })
    }

    pub fn rename(&self, id: &SessionId, name: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE sessions_v2 SET name = ?2, auto_named = 0 WHERE id = ?1",
            params![id.as_str(), name],
        )?;
        Ok(())
    }

    pub fn set_cwd(&self, id: &SessionId, cwd: &Path) -> Result<()> {
        self.conn.execute(
            "UPDATE sessions_v2 SET cwd = ?2 WHERE id = ?1",
            params![id.as_str(), path_text(cwd)?],
        )?;
        Ok(())
    }

    /// The session (and so its project) was just used.
    pub fn touch(&self, id: &SessionId, now_ms: i64) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "UPDATE sessions_v2 SET last_active_at = ?2 WHERE id = ?1",
            params![id.as_str(), now_ms],
        )?;
        use_project_of(&tx, id, now_ms)?;
        tx.commit()?;
        Ok(())
    }

    pub fn set_pinned(&self, id: &SessionId, pinned: bool) -> Result<()> {
        self.conn.execute(
            "UPDATE sessions_v2 SET pinned = ?2 WHERE id = ?1",
            params![id.as_str(), pinned],
        )?;
        Ok(())
    }

    pub fn mark_exited(&self, id: &SessionId, code: Option<i32>, now_ms: i64) -> Result<()> {
        self.conn.execute(
            "UPDATE sessions_v2 SET exit_code = ?2, ended_at = ?3 WHERE id = ?1",
            params![id.as_str(), code, now_ms],
        )?;
        Ok(())
    }

    /// A new terminal started for the session: clear the ended state.
    pub fn mark_revived(&self, id: &SessionId, now_ms: i64) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "UPDATE sessions_v2 SET exit_code = NULL, ended_at = NULL, last_active_at = ?2 \
             WHERE id = ?1",
            params![id.as_str(), now_ms],
        )?;
        use_project_of(&tx, id, now_ms)?;
        tx.commit()?;
        Ok(())
    }

    pub fn set_reported(&self, id: &SessionId, reported: &Reported) -> Result<()> {
        self.conn.execute(
            "UPDATE sessions_v2 SET reported_json = ?2 WHERE id = ?1",
            params![id.as_str(), to_json(reported)?],
        )?;
        Ok(())
    }

    pub fn set_agent(&self, id: &SessionId, agent: &AgentInfo) -> Result<()> {
        self.conn.execute(
            "UPDATE sessions_v2 SET agent_json = ?2, agent_kind = ?3, agent_session_id = ?4 \
             WHERE id = ?1",
            params![
                id.as_str(),
                to_json(agent)?,
                agent.kind,
                agent.agent_session_id
            ],
        )?;
        Ok(())
    }

    /// Forget the session's resume command.
    pub fn clear_resume(&self, id: &SessionId) -> Result<()> {
        self.conn.execute(
            "UPDATE sessions_v2 SET resume_json = NULL WHERE id = ?1",
            params![id.as_str()],
        )?;
        Ok(())
    }

    pub fn set_resume(&self, id: &SessionId, resume: &ResumeSpec) -> Result<()> {
        self.conn.execute(
            "UPDATE sessions_v2 SET resume_json = ?2 WHERE id = ?1",
            params![id.as_str(), to_json(resume)?],
        )?;
        Ok(())
    }

    /// Remove a record. Its children lose their parent link; their ids are
    /// returned so their views can be refreshed.
    pub fn delete(&self, id: &SessionId) -> Result<Vec<SessionId>> {
        let orphans = self.children(id)?;
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "UPDATE sessions_v2 SET parent_id = NULL WHERE parent_id = ?1",
            [id.as_str()],
        )?;
        tx.execute("DELETE FROM sessions_v2 WHERE id = ?1", [id.as_str()])?;
        tx.execute(
            "DELETE FROM session_places WHERE session_id = ?1",
            [id.as_str()],
        )?;
        tx.commit()?;
        Ok(orphans)
    }

    /// Every session's places (`SessionView::places`), in no order. Rows
    /// that can't be read are left out.
    pub fn places(&self) -> Result<HashMap<SessionId, Vec<SessionPlace>>> {
        let mut statement = self
            .conn
            .prepare("SELECT session_id, path, git_json, last_at, edited FROM session_places")?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, bool>(4)?,
            ))
        })?;
        let mut places: HashMap<SessionId, Vec<SessionPlace>> = HashMap::new();
        for row in rows {
            let (id, path, git, last_at, edited) = row?;
            let git = match git.map(|git| serde_json::from_str(&git)).transpose() {
                Ok(git) => git,
                Err(error) => {
                    crate::log_warn!("registry: place {path} of {id}: {error}");
                    continue;
                }
            };
            places
                .entry(SessionId::from_string(id))
                .or_default()
                .push(SessionPlace {
                    path: PathBuf::from(path),
                    git,
                    last_at,
                    edited,
                });
        }
        Ok(places)
    }

    /// Replace session `id`'s places with `places` (a path that isn't UTF-8
    /// is left out).
    pub fn set_places(&self, id: &SessionId, places: &[SessionPlace]) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "DELETE FROM session_places WHERE session_id = ?1",
            [id.as_str()],
        )?;
        for place in places {
            let Ok(path) = path_text(&place.path) else {
                continue;
            };
            tx.execute(
                "INSERT OR REPLACE INTO session_places (session_id, path, git_json, last_at, edited) \
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    id.as_str(),
                    path,
                    opt_json(place.git.as_ref())?,
                    place.last_at,
                    place.edited
                ],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Remember a project; adding a known one changes nothing.
    pub fn add_project(&self, path: &ScopeRoot, now_ms: i64) -> Result<()> {
        self.conn.execute(
            "INSERT OR IGNORE INTO projects (path, added_at) VALUES (?1, ?2)",
            params![path_text(path.path())?, now_ms],
        )?;
        Ok(())
    }

    pub fn project(&self, path: &ScopeRoot) -> Result<Option<ProjectRecord>> {
        Ok(self
            .conn
            .query_row(
                "SELECT path, added_at, last_used_at FROM projects WHERE path = ?1",
                [path_text(path.path())?],
                project_from_row,
            )
            .optional()?)
    }

    /// Every project, most recently used (or added) first.
    pub fn projects(&self) -> Result<Vec<ProjectRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT path, added_at, last_used_at FROM projects \
             ORDER BY COALESCE(last_used_at, added_at) DESC, path",
        )?;
        let projects = stmt
            .query_map([], project_from_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(projects)
    }

    /// Session totals per project; projects without sessions are absent.
    pub fn project_sessions(&self) -> Result<HashMap<ScopeRoot, ProjectSessions>> {
        let mut stmt = self.conn.prepare(
            "SELECT scope_root, COUNT(*), MAX(last_active_at) FROM sessions_v2 \
             GROUP BY scope_root",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((
                ScopeRoot::from_canonical(PathBuf::from(row.get::<_, String>(0)?)),
                ProjectSessions {
                    count: u32::try_from(row.get::<_, i64>(1)?).unwrap_or(u32::MAX),
                    last_active_at_ms: row.get(2)?,
                },
            ))
        })?;
        Ok(rows.collect::<rusqlite::Result<HashMap<_, _>>>()?)
    }

    /// Forget a project, unless sessions still belong to it.
    pub fn remove_project(&self, path: &ScopeRoot) -> Result<ProjectRemoval> {
        let text = path_text(path.path())?;
        let sessions: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM sessions_v2 WHERE scope_root = ?1",
            [text],
            |row| row.get(0),
        )?;
        if sessions > 0 {
            return Ok(ProjectRemoval::HasSessions(
                u32::try_from(sessions).unwrap_or(u32::MAX),
            ));
        }
        let removed = self
            .conn
            .execute("DELETE FROM projects WHERE path = ?1", [text])?;
        Ok(if removed == 0 {
            ProjectRemoval::NotFound
        } else {
            ProjectRemoval::Removed
        })
    }

    pub fn count(&self) -> Result<u32> {
        let n: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM sessions_v2", [], |row| row.get(0))?;
        Ok(u32::try_from(n).unwrap_or(u32::MAX))
    }

    pub fn insert_audit(&self, entry: &AuditEntry) -> Result<()> {
        self.conn.execute(
            "INSERT INTO audit_log \
             (ts, scope_root, caller, method, target_session_id, decision, detail) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                entry.ts_ms,
                entry.scope_root,
                entry.caller,
                entry.method,
                entry.target_session_id,
                entry.decision.as_str(),
                entry.detail,
            ],
        )?;
        Ok(())
    }

    pub fn prune_audit(&self, cutoff_ms: i64) -> Result<usize> {
        Ok(self
            .conn
            .execute("DELETE FROM audit_log WHERE ts < ?1", [cutoff_ms])?)
    }

    /// Flush the WAL into the main database file (daemon shutdown).
    pub fn checkpoint(&self) -> Result<()> {
        self.conn
            .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()))?;
        Ok(())
    }
}

/// A row as stored, before JSON decoding.
struct RawRecord {
    id: String,
    scope_root: String,
    name: String,
    cwd: String,
    launch_json: String,
    resume_json: Option<String>,
    created_by_json: String,
    parent_id: Option<String>,
    depth: i64,
    agent_json: Option<String>,
    reported_json: Option<String>,
    exit_code: Option<i32>,
    created_at: i64,
    last_active_at: i64,
    ended_at: Option<i64>,
    description: Option<String>,
    notes: Option<String>,
    labels_json: Option<String>,
    private: bool,
    auto_named: bool,
    pinned: bool,
}

impl RawRecord {
    fn from_row(row: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get(0)?,
            scope_root: row.get(1)?,
            name: row.get(2)?,
            cwd: row.get(3)?,
            launch_json: row.get(4)?,
            resume_json: row.get(5)?,
            created_by_json: row.get(6)?,
            parent_id: row.get(7)?,
            depth: row.get(8)?,
            agent_json: row.get(9)?,
            reported_json: row.get(10)?,
            exit_code: row.get(11)?,
            created_at: row.get(12)?,
            last_active_at: row.get(13)?,
            ended_at: row.get(14)?,
            description: row.get(15)?,
            notes: row.get(16)?,
            labels_json: row.get(17)?,
            private: row.get(18)?,
            auto_named: row.get(19)?,
            pinned: row.get(22)?,
        })
    }

    fn decode(self) -> Result<SessionRecord> {
        let id = self.id;
        let corrupt = |what: &str, e: serde_json::Error| {
            RegistryError::Corrupt(format!("session {id}: {what}: {e}"))
        };
        let launch = serde_json::from_str(&self.launch_json).map_err(|e| corrupt("launch", e))?;
        let created_by =
            serde_json::from_str(&self.created_by_json).map_err(|e| corrupt("created_by", e))?;
        let resume = self
            .resume_json
            .as_deref()
            .map(serde_json::from_str)
            .transpose()
            .map_err(|e| corrupt("resume", e))?;
        let agent = self
            .agent_json
            .as_deref()
            .map(serde_json::from_str)
            .transpose()
            .map_err(|e| corrupt("agent", e))?;
        let reported = self
            .reported_json
            .as_deref()
            .map(serde_json::from_str)
            .transpose()
            .map_err(|e| corrupt("reported", e))?;
        let labels = self
            .labels_json
            .as_deref()
            .map(serde_json::from_str)
            .transpose()
            .map_err(|e| corrupt("labels", e))?
            .unwrap_or_default();
        Ok(SessionRecord {
            scope: ScopeRoot::from_canonical(PathBuf::from(self.scope_root)),
            name: self.name,
            cwd: PathBuf::from(self.cwd),
            launch,
            resume,
            created_by,
            parent_id: self.parent_id.map(SessionId::from_string),
            depth: u8::try_from(self.depth).unwrap_or(u8::MAX),
            agent,
            reported,
            exit_code: self.exit_code,
            created_at_ms: self.created_at,
            last_active_at_ms: self.last_active_at,
            ended_at_ms: self.ended_at,
            description: self.description,
            notes: self.notes,
            labels,
            private: self.private,
            auto_named: self.auto_named,
            pinned: self.pinned,
            id: SessionId::from_string(id),
        })
    }
}

/// Bring a database at schema `from` to [`SCHEMA_VERSION`] in one
/// transaction:
/// - before 2 (new, or 0.5.x): the tmux-era `sessions` table in the same
///   file is dropped, not migrated;
/// - 2 → 3: `sessions_v2.pinned` is added and `projects` is filled from the
///   scopes of the existing sessions.
fn migrate(conn: &mut Connection, from: i64) -> rusqlite::Result<()> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    if from < 2 {
        tx.execute_batch("DROP TABLE IF EXISTS sessions;")?;
    }
    tx.execute_batch(SCHEMA)?;
    let has_pinned: bool = tx.query_row(
        "SELECT COUNT(*) > 0 FROM pragma_table_info('sessions_v2') WHERE name = 'pinned'",
        [],
        |row| row.get(0),
    )?;
    if !has_pinned {
        tx.execute_batch("ALTER TABLE sessions_v2 ADD COLUMN pinned INTEGER NOT NULL DEFAULT 0;")?;
    }
    tx.execute_batch(BACKFILL_PROJECTS)?;
    tx.pragma_update(None, "user_version", SCHEMA_VERSION)?;
    tx.commit()
}

/// Mark the project of session `id` as used at `now_ms`.
fn use_project_of(conn: &Connection, id: &SessionId, now_ms: i64) -> rusqlite::Result<()> {
    let scope: Option<String> = conn
        .query_row(
            "SELECT scope_root FROM sessions_v2 WHERE id = ?1",
            [id.as_str()],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(scope) = scope {
        conn.execute(USE_PROJECT, params![scope, now_ms])?;
    }
    Ok(())
}

fn project_from_row(row: &Row<'_>) -> rusqlite::Result<ProjectRecord> {
    Ok(ProjectRecord {
        path: ScopeRoot::from_canonical(PathBuf::from(row.get::<_, String>(0)?)),
        added_at_ms: row.get(1)?,
        last_used_at_ms: row.get(2)?,
    })
}

/// Paths are stored as TEXT; non-UTF-8 paths are rejected rather than
/// stored lossily.
pub(crate) fn path_text(path: &Path) -> Result<&str> {
    path.to_str()
        .ok_or_else(|| RegistryError::NonUtf8Path(path.to_path_buf()))
}

fn to_json<T: serde::Serialize>(value: &T) -> Result<String> {
    serde_json::to_string(value).map_err(|e| RegistryError::Corrupt(e.to_string()))
}

fn labels_json(labels: &BTreeMap<String, String>) -> Result<Option<String>> {
    if labels.is_empty() {
        Ok(None)
    } else {
        to_json(&labels).map(Some)
    }
}

fn opt_json<T: serde::Serialize>(value: Option<&T>) -> Result<Option<String>> {
    value.map(to_json).transpose()
}

fn sql_io(error: rusqlite::Error) -> io::Error {
    io::Error::other(format!("registry: {error}"))
}

fn create_private_dir(dir: &Path) -> io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    if dir.is_dir() {
        return Ok(());
    }
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
}

/// The registry holds names and paths of the user's sessions: owner-only.
/// SQLite creates the WAL/SHM files with the database file's mode.
fn restrict_file(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    if let Err(e) = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)) {
        crate::log_warn!("registry: chmod 0600 {}: {e}", path.display());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control::audit::Decision;

    fn record(id: &str, scope: &str, name: &str, active: i64) -> SessionRecord {
        SessionRecord {
            id: SessionId::from_string(id),
            scope: ScopeRoot::from_canonical(scope.into()),
            name: name.into(),
            cwd: scope.into(),
            launch: LaunchSpec::default(),
            resume: None,
            created_by: Creator::User,
            parent_id: None,
            depth: 0,
            agent: None,
            reported: None,
            exit_code: None,
            created_at_ms: active,
            last_active_at_ms: active,
            ended_at_ms: None,
            description: None,
            notes: None,
            labels: BTreeMap::new(),
            auto_named: false,
            private: false,
            pinned: false,
        }
    }

    fn open(dir: &tempfile::TempDir) -> Registry {
        Registry::open(&dir.path().join("rakumux.sqlite3"), 1_000_000_000_000).unwrap()
    }

    #[test]
    fn records_round_trip_and_list_by_recency() {
        let dir = tempfile::tempdir().unwrap();
        let reg = open(&dir);
        let mut a = record("A", "/s", "alpha", 10);
        a.reported = Some(Reported {
            state: None,
            text: Some("hi".into()),
            result: None,
            at: 5,
        });
        a.labels = BTreeMap::from([("role".to_string(), "review".to_string())]);
        a.description = Some("reviews PRs".into());
        a.private = true;
        a.auto_named = true;
        reg.insert(&a).unwrap();
        reg.insert(&record("B", "/s", "beta", 20)).unwrap();
        reg.insert(&record("C", "/other", "gamma", 30)).unwrap();
        assert_eq!(reg.get(&a.id).unwrap().unwrap(), a);
        let (list, bad) = reg.list(&a.scope).unwrap();
        assert!(bad.is_empty());
        let ids: Vec<_> = list.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, ["B", "A"]);
        reg.touch(&a.id, 40).unwrap();
        let (list, _) = reg.list(&a.scope).unwrap();
        assert_eq!(list[0].id, a.id);
        assert_eq!(reg.count().unwrap(), 3);
    }

    #[test]
    fn names_are_unique_per_scope() {
        let dir = tempfile::tempdir().unwrap();
        let reg = open(&dir);
        reg.insert(&record("A", "/s", "alpha", 1)).unwrap();
        reg.insert(&record("B", "/s", "beta", 1)).unwrap();
        reg.insert(&record("C", "/t", "alpha", 1)).unwrap();
        assert!(matches!(
            reg.insert(&record("D", "/s", "alpha", 1)),
            Err(RegistryError::NameTaken)
        ));
        assert!(matches!(
            reg.rename(&SessionId::from_string("B"), "alpha"),
            Err(RegistryError::NameTaken)
        ));
        let scope = ScopeRoot::from_canonical("/s".into());
        assert!(reg.name_taken(&scope, "alpha", None).unwrap());
        assert!(!reg
            .name_taken(&scope, "alpha", Some(&SessionId::from_string("A")))
            .unwrap());
    }

    #[test]
    fn a_failed_batch_import_rolls_back_all_new_records() {
        let dir = tempfile::tempdir().unwrap();
        let registry = open(&dir);
        registry.insert(&record("A", "/s", "taken", 1)).unwrap();
        let records = [record("B", "/new", "new", 2), record("C", "/s", "taken", 3)];
        assert!(matches!(
            registry.insert_many(&records),
            Err(RegistryError::NameTaken)
        ));
        assert_eq!(registry.count().unwrap(), 1);
        assert_eq!(registry.get(&SessionId::from_string("B")).unwrap(), None);
    }

    #[test]
    fn relocation_name_conflicts_roll_back_the_folder_and_sessions() {
        let dir = tempfile::tempdir().unwrap();
        let registry = open(&dir);
        let original = record("A", "/before", "same", 1);
        registry.insert(&original).unwrap();
        registry.insert(&record("B", "/after", "same", 2)).unwrap();
        assert!(matches!(
            registry.relocate_project(&original.scope, &ScopeRoot::from_canonical("/after".into())),
            Err(RegistryError::NameTaken)
        ));
        assert_eq!(registry.get(&original.id).unwrap(), Some(original));
        assert_eq!(registry.count().unwrap(), 2);
    }

    #[test]
    fn places_are_saved_replaced_and_deleted_with_their_session() {
        let dir = tempfile::tempdir().unwrap();
        let reg = open(&dir);
        reg.insert(&record("A", "/s", "a", 1)).unwrap();
        let id = SessionId::from_string("A");
        let place = |path: &str, last_at: i64| SessionPlace {
            path: path.into(),
            git: Some(session_core::model::GitInfo {
                repo_root: path.into(),
                branch: Some("main".into()),
                ..Default::default()
            }),
            last_at,
            edited: true,
        };
        reg.set_places(&id, &[place("/s", 5), place("/wt", 9)])
            .unwrap();
        let mut stored = reg.places().unwrap().remove(&id).unwrap();
        stored.sort_by_key(|p| p.last_at);
        assert_eq!(stored, [place("/s", 5), place("/wt", 9)]);
        reg.set_places(&id, &[place("/wt", 12)]).unwrap();
        assert_eq!(reg.places().unwrap()[&id], [place("/wt", 12)]);
        reg.delete(&id).unwrap();
        assert!(reg.places().unwrap().is_empty());
    }

    #[test]
    fn renaming_clears_auto_named() {
        let dir = tempfile::tempdir().unwrap();
        let reg = open(&dir);
        let mut a = record("A", "/s", "brisk-heron", 1);
        a.auto_named = true;
        reg.insert(&a).unwrap();
        assert!(reg.get(&a.id).unwrap().unwrap().auto_named);
        reg.rename(&a.id, "build").unwrap();
        let renamed = reg.get(&a.id).unwrap().unwrap();
        assert_eq!(renamed.name, "build");
        assert!(!renamed.auto_named);
    }

    #[test]
    fn subtree_and_delete_orphan_children() {
        let dir = tempfile::tempdir().unwrap();
        let reg = open(&dir);
        reg.insert(&record("P", "/s", "p", 1)).unwrap();
        let mut c = record("C", "/s", "c", 2);
        c.parent_id = Some(SessionId::from_string("P"));
        reg.insert(&c).unwrap();
        let mut g = record("G", "/s", "g", 3);
        g.parent_id = Some(SessionId::from_string("C"));
        reg.insert(&g).unwrap();
        let tree = reg.subtree(&SessionId::from_string("P")).unwrap();
        assert_eq!(
            tree.iter().map(|i| i.as_str()).collect::<Vec<_>>(),
            ["P", "C", "G"]
        );
        let orphans = reg.delete(&SessionId::from_string("C")).unwrap();
        assert_eq!(orphans, vec![SessionId::from_string("G")]);
        assert!(reg.get(&g.id).unwrap().unwrap().parent_id.is_none());
    }

    #[test]
    fn legacy_table_dropped_and_newer_schema_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rakumux.sqlite3");
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch("CREATE TABLE sessions (id INTEGER PRIMARY KEY, tmux_name TEXT);")
                .unwrap();
        }
        drop(Registry::open(&path, 0).unwrap());
        let conn = Connection::open(&path).unwrap();
        let legacy: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE name = 'sessions'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(legacy, 0);
        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);
        conn.execute_batch(&format!("PRAGMA user_version = {};", SCHEMA_VERSION + 1))
            .unwrap();
        drop(conn);
        assert!(Registry::open(&path, 0).is_err());
    }

    /// The 0.6 schema (version 2) as the previous daemon created it.
    const SCHEMA_V2: &str = "
        CREATE TABLE sessions_v2 (
          id TEXT PRIMARY KEY, scope_root TEXT NOT NULL, name TEXT NOT NULL,
          cwd TEXT NOT NULL, launch_json TEXT NOT NULL, resume_json TEXT,
          created_by_json TEXT NOT NULL, parent_id TEXT,
          depth INTEGER NOT NULL DEFAULT 0, agent_json TEXT, reported_json TEXT,
          exit_code INTEGER, created_at INTEGER NOT NULL,
          last_active_at INTEGER NOT NULL, ended_at INTEGER, description TEXT,
          notes TEXT, labels_json TEXT, private INTEGER NOT NULL DEFAULT 0,
          auto_named INTEGER NOT NULL DEFAULT 0, agent_kind TEXT,
          agent_session_id TEXT
        );
        CREATE UNIQUE INDEX idx_sessions_v2_name ON sessions_v2(scope_root, name);
        CREATE TABLE audit_log (
          id INTEGER PRIMARY KEY AUTOINCREMENT, ts INTEGER NOT NULL,
          scope_root TEXT, caller TEXT NOT NULL, method TEXT NOT NULL,
          target_session_id TEXT, decision TEXT NOT NULL, detail TEXT
        );
        PRAGMA user_version = 2;
    ";

    fn insert_v2(conn: &Connection, id: &str, scope: &str, created: i64, active: i64) {
        conn.execute(
            "INSERT INTO sessions_v2 (id, scope_root, name, cwd, launch_json, \
             created_by_json, created_at, last_active_at, auto_named) \
             VALUES (?1, ?2, ?1, ?2, '{}', '{\"kind\":\"user\"}', ?3, ?4, 1)",
            params![id, scope, created, active],
        )
        .unwrap();
    }

    #[test]
    fn schema_2_migrates_keeping_sessions_and_backfilling_projects() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rakumux.sqlite3");
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(SCHEMA_V2).unwrap();
            insert_v2(&conn, "A", "/s", 10, 50);
            insert_v2(&conn, "B", "/s", 20, 30);
            insert_v2(&conn, "C", "/t", 40, 40);
        }
        let reg = Registry::open(&path, 0).unwrap();
        let a = reg.get(&SessionId::from_string("A")).unwrap().unwrap();
        assert_eq!(a.name, "A");
        assert!(a.auto_named);
        assert!(!a.pinned);
        let (list, bad) = reg.list(&ScopeRoot::from_canonical("/s".into())).unwrap();
        assert!(bad.is_empty());
        assert_eq!(list.len(), 2);
        let projects = reg.projects().unwrap();
        assert_eq!(
            projects,
            vec![
                ProjectRecord {
                    path: ScopeRoot::from_canonical("/s".into()),
                    added_at_ms: 10,
                    last_used_at_ms: Some(50),
                },
                ProjectRecord {
                    path: ScopeRoot::from_canonical("/t".into()),
                    added_at_ms: 40,
                    last_used_at_ms: Some(40),
                },
            ]
        );
        reg.set_pinned(&a.id, true).unwrap();
        assert!(reg.get(&a.id).unwrap().unwrap().pinned);
        drop(reg);
        // Opening again changes nothing.
        let reg = Registry::open(&path, 0).unwrap();
        assert_eq!(reg.projects().unwrap(), projects);
        assert!(reg.get(&a.id).unwrap().unwrap().pinned);
        let conn = Connection::open(&path).unwrap();
        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);
    }

    #[test]
    fn sessions_record_their_project_and_projects_with_sessions_stay() {
        let dir = tempfile::tempdir().unwrap();
        let reg = open(&dir);
        let s = ScopeRoot::from_canonical("/s".into());
        let t = ScopeRoot::from_canonical("/t".into());
        reg.add_project(&t, 5).unwrap();
        reg.add_project(&t, 6).unwrap();
        assert_eq!(
            reg.project(&t).unwrap(),
            Some(ProjectRecord {
                path: t.clone(),
                added_at_ms: 5,
                last_used_at_ms: None,
            })
        );
        reg.insert(&record("A", "/s", "alpha", 10)).unwrap();
        reg.insert(&record("B", "/s", "beta", 20)).unwrap();
        assert_eq!(
            reg.project(&s).unwrap(),
            Some(ProjectRecord {
                path: s.clone(),
                added_at_ms: 10,
                last_used_at_ms: Some(20),
            })
        );
        reg.touch(&SessionId::from_string("A"), 30).unwrap();
        reg.mark_revived(&SessionId::from_string("B"), 25).unwrap();
        assert_eq!(reg.project(&s).unwrap().unwrap().last_used_at_ms, Some(30));
        assert_eq!(
            reg.projects()
                .unwrap()
                .into_iter()
                .map(|p| p.path)
                .collect::<Vec<_>>(),
            vec![s.clone(), t.clone()]
        );
        let stats = reg.project_sessions().unwrap();
        assert_eq!(
            stats.get(&s),
            Some(&ProjectSessions {
                count: 2,
                last_active_at_ms: Some(30),
            })
        );
        assert_eq!(stats.get(&t), None);

        assert_eq!(
            reg.remove_project(&s).unwrap(),
            ProjectRemoval::HasSessions(2)
        );
        assert_eq!(reg.remove_project(&t).unwrap(), ProjectRemoval::Removed);
        assert_eq!(reg.remove_project(&t).unwrap(), ProjectRemoval::NotFound);
        reg.delete(&SessionId::from_string("A")).unwrap();
        reg.delete(&SessionId::from_string("B")).unwrap();
        assert_eq!(reg.remove_project(&s).unwrap(), ProjectRemoval::Removed);
        assert!(reg.projects().unwrap().is_empty());
    }

    #[test]
    fn list_in_unions_scopes_by_recency() {
        let dir = tempfile::tempdir().unwrap();
        let reg = open(&dir);
        reg.insert(&record("A", "/s", "a", 10)).unwrap();
        reg.insert(&record("B", "/t", "b", 30)).unwrap();
        reg.insert(&record("C", "/u", "c", 20)).unwrap();
        reg.insert(&record("D", "/s", "d", 40)).unwrap();
        let scopes = [
            ScopeRoot::from_canonical("/s".into()),
            ScopeRoot::from_canonical("/t".into()),
        ];
        let (list, _) = reg.list_in(&scopes).unwrap();
        let ids: Vec<_> = list.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, ["D", "B", "A"]);
        assert!(reg.list_in(&[]).unwrap().0.is_empty());
    }

    #[test]
    fn list_all_covers_every_scope_by_recency() {
        let dir = tempfile::tempdir().unwrap();
        let reg = open(&dir);
        assert!(reg.list_all().unwrap().0.is_empty());
        reg.insert(&record("A", "/s", "a", 10)).unwrap();
        reg.insert(&record("B", "/t", "b", 30)).unwrap();
        reg.insert(&record("C", "/u", "c", 20)).unwrap();
        reg.insert(&record("D", "/s", "d", 30)).unwrap();
        let (list, bad) = reg.list_all().unwrap();
        assert!(bad.is_empty());
        let ids: Vec<_> = list.iter().map(|r| r.id.as_str()).collect();
        // Ties on recency: newest id first, as in a scoped list.
        assert_eq!(ids, ["D", "B", "C", "A"]);
    }

    #[test]
    fn audit_entries_are_pruned_after_thirty_days() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rakumux.sqlite3");
        let now = 100 * 24 * 60 * 60 * 1000;
        let reg = Registry::open(&path, now).unwrap();
        for ts in [now - AUDIT_RETENTION_MS - 1, now - 1000] {
            reg.insert_audit(&AuditEntry {
                ts_ms: ts,
                scope_root: None,
                caller: "user:test".into(),
                method: "session.create",
                target_session_id: None,
                decision: Decision::Allowed,
                detail: None,
            })
            .unwrap();
        }
        drop(reg);
        drop(Registry::open(&path, now).unwrap());
        let conn = Connection::open(&path).unwrap();
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM audit_log", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1);
    }
}
