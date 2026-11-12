//! Heartbeats in the registry: `heartbeats` (one row each) and
//! `heartbeat_runs` (which sessions are a heartbeat's runs). Both are
//! created when missing, so the schema version stays and an older daemon
//! still opens the file. Times are stored in Unix milliseconds, as the
//! sessions' are; [`Heartbeat`] carries seconds.

use super::{path_text, to_json, Registry, RegistryError, Result};
use rusqlite::{params, Row};
use session_core::heartbeat::{Heartbeat, HeartbeatRun, Schedule};
use session_core::ids::{HeartbeatId, SessionId};
use session_core::model::{Creator, ScopeRoot};
use std::path::PathBuf;

pub(super) const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS heartbeats (
  id TEXT PRIMARY KEY,
  scope_root TEXT NOT NULL,
  name TEXT NOT NULL,
  agent TEXT,
  command TEXT NOT NULL DEFAULT '',
  prompt TEXT NOT NULL DEFAULT '',
  schedule_json TEXT,
  enabled INTEGER NOT NULL DEFAULT 0,
  awaiting_approval INTEGER NOT NULL DEFAULT 0,
  created_by_json TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL,
  next_run_at INTEGER,
  runs INTEGER NOT NULL DEFAULT 0,
  last_run_json TEXT
);
CREATE UNIQUE INDEX IF NOT EXISTS heartbeats_scope_name ON heartbeats(scope_root, name);
CREATE TABLE IF NOT EXISTS heartbeat_runs (
  session_id TEXT PRIMARY KEY,
  heartbeat_id TEXT NOT NULL,
  run INTEGER NOT NULL,
  started_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS heartbeat_runs_heartbeat ON heartbeat_runs(heartbeat_id, run);
";

const COLUMNS: &str = "id, scope_root, name, agent, command, prompt, schedule_json, enabled, \
     awaiting_approval, created_by_json, created_at, updated_at, next_run_at, runs, last_run_json";

/// A session that is a heartbeat's run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RunRow {
    pub session_id: SessionId,
    pub heartbeat: HeartbeatId,
    pub run: u32,
    pub started_ms: i64,
}

impl Registry {
    /// Every heartbeat, and the rows that couldn't be read.
    pub fn heartbeats(&self) -> Result<(Vec<Heartbeat>, Vec<RegistryError>)> {
        let mut statement = self.conn.prepare(&format!(
            "SELECT {COLUMNS} FROM heartbeats ORDER BY created_at"
        ))?;
        let rows = statement.query_map([], decode)?;
        let mut heartbeats = Vec::new();
        let mut bad = Vec::new();
        for row in rows {
            match row? {
                Ok(heartbeat) => heartbeats.push(heartbeat),
                Err(error) => bad.push(error),
            }
        }
        Ok((heartbeats, bad))
    }

    /// Insert or replace `heartbeat` (by id). Another heartbeat of its
    /// folder with its name: [`RegistryError::NameTaken`].
    pub fn save_heartbeat(&self, heartbeat: &Heartbeat) -> Result<()> {
        let last_run = heartbeat.last_run.as_ref().map(to_json).transpose()?;
        let schedule = heartbeat.schedule.as_ref().map(to_json).transpose()?;
        self.conn.execute(
            &format!(
                "INSERT INTO heartbeats ({COLUMNS}) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15) \
                 ON CONFLICT(id) DO UPDATE SET scope_root = ?2, name = ?3, agent = ?4, \
                 command = ?5, prompt = ?6, schedule_json = ?7, enabled = ?8, \
                 awaiting_approval = ?9, updated_at = ?12, next_run_at = ?13, runs = ?14, \
                 last_run_json = ?15"
            ),
            params![
                heartbeat.id.as_str(),
                path_text(heartbeat.scope.path())?,
                heartbeat.name,
                heartbeat.agent,
                heartbeat.command,
                heartbeat.prompt,
                schedule,
                heartbeat.enabled,
                heartbeat.awaiting_approval,
                to_json(&heartbeat.created_by)?,
                heartbeat.created_at.saturating_mul(1000),
                heartbeat.updated_at.saturating_mul(1000),
                heartbeat.next_run_at.map(|at| at.saturating_mul(1000)),
                heartbeat.runs,
                last_run,
            ],
        )?;
        Ok(())
    }

    /// Forget a heartbeat; its runs stay as plain sessions.
    pub fn delete_heartbeat(&self, id: &HeartbeatId) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        tx.execute("DELETE FROM heartbeats WHERE id = ?1", params![id.as_str()])?;
        tx.execute(
            "DELETE FROM heartbeat_runs WHERE heartbeat_id = ?1",
            params![id.as_str()],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Every session that is a heartbeat's run.
    pub fn heartbeat_runs(&self) -> Result<Vec<RunRow>> {
        let mut statement = self
            .conn
            .prepare("SELECT session_id, heartbeat_id, run, started_at FROM heartbeat_runs")?;
        let rows = statement.query_map([], |row| {
            Ok(RunRow {
                session_id: SessionId::from_string(row.get::<_, String>(0)?),
                heartbeat: HeartbeatId::from_string(row.get::<_, String>(1)?),
                run: row.get(2)?,
                started_ms: row.get(3)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Record session `row.session_id` as a heartbeat's run.
    pub fn add_heartbeat_run(&self, row: &RunRow) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO heartbeat_runs (session_id, heartbeat_id, run, started_at) \
             VALUES (?1, ?2, ?3, ?4)",
            params![
                row.session_id.as_str(),
                row.heartbeat.as_str(),
                row.run,
                row.started_ms
            ],
        )?;
        Ok(())
    }

    /// The session is no run any more (deleted, or its heartbeat is gone).
    pub fn remove_heartbeat_run(&self, session_id: &SessionId) -> Result<()> {
        self.conn.execute(
            "DELETE FROM heartbeat_runs WHERE session_id = ?1",
            params![session_id.as_str()],
        )?;
        Ok(())
    }

    /// Heartbeats in folder `from` move to `to` (`project_relocate`).
    pub(super) fn relocate_heartbeats(
        conn: &rusqlite::Connection,
        from: &ScopeRoot,
        to: &ScopeRoot,
    ) -> Result<()> {
        conn.execute(
            "UPDATE heartbeats SET scope_root = ?2 WHERE scope_root = ?1",
            params![path_text(from.path())?, path_text(to.path())?],
        )?;
        Ok(())
    }
}

fn decode(row: &Row<'_>) -> rusqlite::Result<Result<Heartbeat>> {
    let id: String = row.get(0)?;
    let scope: String = row.get(1)?;
    let schedule: Option<String> = row.get(6)?;
    let created_by: String = row.get(9)?;
    let last_run: Option<String> = row.get(14)?;
    let corrupt = |what: &str, error: serde_json::Error| {
        RegistryError::Corrupt(format!("heartbeat {id}: {what}: {error}"))
    };
    let schedule: Option<Schedule> = match schedule.map(|raw| serde_json::from_str(&raw)) {
        Some(Ok(schedule)) => Some(schedule),
        Some(Err(error)) => return Ok(Err(corrupt("schedule", error))),
        None => None,
    };
    let created_by: Creator = match serde_json::from_str(&created_by) {
        Ok(creator) => creator,
        Err(error) => return Ok(Err(corrupt("created_by", error))),
    };
    let last_run: Option<HeartbeatRun> = match last_run.map(|raw| serde_json::from_str(&raw)) {
        Some(Ok(run)) => Some(run),
        Some(Err(error)) => return Ok(Err(corrupt("last_run", error))),
        None => None,
    };
    let seconds = |ms: i64| ms.div_euclid(1000);
    Ok(Ok(Heartbeat {
        id: HeartbeatId::from_string(id.clone()),
        scope: ScopeRoot::from_canonical(PathBuf::from(scope)),
        name: row.get(2)?,
        agent: row.get(3)?,
        command: row.get(4)?,
        prompt: row.get(5)?,
        schedule,
        enabled: row.get(7)?,
        awaiting_approval: row.get(8)?,
        created_by,
        created_at: seconds(row.get(10)?),
        updated_at: seconds(row.get(11)?),
        next_run_at: row.get::<_, Option<i64>>(12)?.map(seconds),
        runs: row.get(13)?,
        last_run,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use session_core::heartbeat::RunOutcome;

    fn heartbeat(name: &str) -> Heartbeat {
        Heartbeat {
            id: HeartbeatId::generate(),
            scope: ScopeRoot::from_canonical("/work".into()),
            name: name.into(),
            agent: Some("claude".into()),
            command: "claude --permission-mode acceptEdits".into(),
            prompt: "update the dependencies".into(),
            schedule: Some(Schedule::Daily { at: "09:00".into() }),
            enabled: true,
            awaiting_approval: false,
            created_by: Creator::User,
            created_at: 1_700_000_000,
            updated_at: 1_700_000_000,
            next_run_at: Some(1_700_003_600),
            runs: 2,
            last_run: Some(HeartbeatRun {
                run: 2,
                at: 1_700_000_100,
                session_id: Some(SessionId::from_string("S2")),
                outcome: RunOutcome::TaskGiven,
                detail: None,
            }),
        }
    }

    #[test]
    fn heartbeats_and_their_runs_are_kept() {
        let dir = tempfile::tempdir().unwrap();
        let registry = Registry::open(&dir.path().join("r.sqlite3"), 0).unwrap();
        let mut deps = heartbeat("deps");
        registry.save_heartbeat(&deps).unwrap();
        let other = heartbeat("deps");
        assert!(matches!(
            registry.save_heartbeat(&other),
            Err(RegistryError::NameTaken)
        ));
        deps.enabled = false;
        deps.next_run_at = None;
        registry.save_heartbeat(&deps).unwrap();
        let row = RunRow {
            session_id: SessionId::from_string("S2"),
            heartbeat: deps.id.clone(),
            run: 2,
            started_ms: 5,
        };
        registry.add_heartbeat_run(&row).unwrap();
        drop(registry);
        let registry = Registry::open(&dir.path().join("r.sqlite3"), 0).unwrap();
        let (all, bad) = registry.heartbeats().unwrap();
        assert!(bad.is_empty());
        assert_eq!(all, vec![deps.clone()]);
        assert_eq!(registry.heartbeat_runs().unwrap(), vec![row]);
        registry.delete_heartbeat(&deps.id).unwrap();
        assert!(registry.heartbeats().unwrap().0.is_empty());
        assert!(registry.heartbeat_runs().unwrap().is_empty());
    }
}
