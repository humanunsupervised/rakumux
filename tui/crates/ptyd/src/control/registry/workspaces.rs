//! Workspaces in the registry: `workspaces` (one row each) and
//! `session_workspaces` (the workspace each session is in; none without a
//! row). Both are created when missing, so the schema version stays and an
//! older daemon still opens the file. A deleted session keeps its row while
//! it's in the session history, so restoring it puts it back in its
//! workspace; purging it, or forgetting the workspace, drops the row.

use super::{Registry, Result};
use rusqlite::params;
use session_core::ids::{SessionId, WorkspaceId};
use session_core::workspace::Workspace;
use std::collections::HashMap;

pub(super) const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS workspaces (
  id TEXT PRIMARY KEY,
  name TEXT NOT NULL,
  created_at INTEGER NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS workspaces_name ON workspaces(name);
CREATE TABLE IF NOT EXISTS session_workspaces (
  session_id TEXT PRIMARY KEY,
  workspace_id TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS session_workspaces_workspace ON session_workspaces(workspace_id);
";

impl Registry {
    /// Every workspace, oldest first.
    pub fn workspaces(&self) -> Result<Vec<Workspace>> {
        let mut statement = self
            .conn
            .prepare("SELECT id, name, created_at FROM workspaces ORDER BY created_at, name")?;
        let rows = statement.query_map([], |row| {
            Ok(Workspace {
                id: WorkspaceId::from_string(row.get::<_, String>(0)?),
                name: row.get(1)?,
                created_at: row.get::<_, i64>(2)? / 1000,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Add `workspace` (created at `created_at_ms`). Another with its name:
    /// `RegistryError::NameTaken`.
    pub fn insert_workspace(&self, workspace: &Workspace, created_at_ms: i64) -> Result<()> {
        self.conn.execute(
            "INSERT INTO workspaces (id, name, created_at) VALUES (?1, ?2, ?3)",
            params![workspace.id.as_str(), workspace.name, created_at_ms],
        )?;
        Ok(())
    }

    /// Forget a workspace: its sessions are in none from now on.
    pub fn delete_workspace(&self, id: &WorkspaceId) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        tx.execute("DELETE FROM workspaces WHERE id = ?1", [id.as_str()])?;
        tx.execute(
            "DELETE FROM session_workspaces WHERE workspace_id = ?1",
            [id.as_str()],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// The workspace each session is in, for the workspaces that exist.
    pub fn session_workspaces(&self) -> Result<HashMap<SessionId, WorkspaceId>> {
        let mut statement = self.conn.prepare(
            "SELECT s.session_id, s.workspace_id FROM session_workspaces s \
             JOIN workspaces w ON w.id = s.workspace_id",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((
                SessionId::from_string(row.get::<_, String>(0)?),
                WorkspaceId::from_string(row.get::<_, String>(1)?),
            ))
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Put session `id` in `workspace`, or in none.
    pub fn set_session_workspace(
        &self,
        id: &SessionId,
        workspace: Option<&WorkspaceId>,
    ) -> Result<()> {
        match workspace {
            Some(workspace) => self.conn.execute(
                "INSERT INTO session_workspaces (session_id, workspace_id) VALUES (?1, ?2) \
                 ON CONFLICT(session_id) DO UPDATE SET workspace_id = ?2",
                params![id.as_str(), workspace.as_str()],
            )?,
            None => self.conn.execute(
                "DELETE FROM session_workspaces WHERE session_id = ?1",
                [id.as_str()],
            )?,
        };
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control::registry::RegistryError;

    fn open() -> (tempfile::TempDir, Registry) {
        let dir = tempfile::tempdir().unwrap();
        let registry = Registry::open(&dir.path().join("r.sqlite3"), 0).unwrap();
        (dir, registry)
    }

    fn workspace(name: &str) -> Workspace {
        Workspace {
            id: WorkspaceId::generate(),
            name: name.into(),
            created_at: 0,
        }
    }

    #[test]
    fn names_are_unique_and_forgetting_one_frees_its_sessions() {
        let (_dir, registry) = open();
        let launch = workspace("Launch");
        registry.insert_workspace(&launch, 1_000).unwrap();
        assert!(matches!(
            registry.insert_workspace(&workspace("Launch"), 2_000),
            Err(RegistryError::NameTaken)
        ));
        let session = SessionId::generate();
        registry
            .set_session_workspace(&session, Some(&launch.id))
            .unwrap();
        assert_eq!(
            registry.session_workspaces().unwrap().get(&session),
            Some(&launch.id)
        );
        registry.delete_workspace(&launch.id).unwrap();
        assert!(registry.session_workspaces().unwrap().is_empty());
        assert!(registry.workspaces().unwrap().is_empty());
    }

    #[test]
    fn a_session_moves_between_workspaces_and_out() {
        let (_dir, registry) = open();
        let (a, b) = (workspace("A"), workspace("B"));
        registry.insert_workspace(&a, 1_000).unwrap();
        registry.insert_workspace(&b, 2_000).unwrap();
        let session = SessionId::generate();
        registry
            .set_session_workspace(&session, Some(&a.id))
            .unwrap();
        registry
            .set_session_workspace(&session, Some(&b.id))
            .unwrap();
        assert_eq!(
            registry.session_workspaces().unwrap().get(&session),
            Some(&b.id)
        );
        registry.set_session_workspace(&session, None).unwrap();
        assert!(registry.session_workspaces().unwrap().is_empty());
        let names: Vec<String> = registry
            .workspaces()
            .unwrap()
            .into_iter()
            .map(|w| w.name)
            .collect();
        assert_eq!(names, ["A", "B"]);
    }
}
