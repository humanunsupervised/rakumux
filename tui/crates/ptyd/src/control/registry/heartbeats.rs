//! Heartbeats in the registry: `heartbeats` (one row each),
//! `heartbeat_runs` (which listed sessions are a heartbeat's runs) and
//! `heartbeat_run_log` (every run of each heartbeat, kept as long as it is,
//! its session's id there whatever became of it). All are created when
//! missing, so the schema version stays and an older daemon
//! still opens the file. Times are stored in Unix milliseconds, as the
//! sessions' are; [`Heartbeat`] carries seconds.

use super::{path_text, to_json, Registry, RegistryError, Result};
use rusqlite::{params, Row};
use session_core::heartbeat::{Heartbeat, HeartbeatRun, RunOutcome, Schedule};
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
CREATE TABLE IF NOT EXISTS heartbeat_run_log (
  seq INTEGER PRIMARY KEY AUTOINCREMENT,
  heartbeat_id TEXT NOT NULL,
  run INTEGER NOT NULL,
  at INTEGER NOT NULL,
  session_id TEXT,
  outcome TEXT NOT NULL,
  detail TEXT
);
CREATE INDEX IF NOT EXISTS heartbeat_run_log_heartbeat ON heartbeat_run_log(heartbeat_id, seq);
CREATE INDEX IF NOT EXISTS heartbeat_run_log_session ON heartbeat_run_log(session_id);
-- Runs listed before the record was kept (0.1.82 and earlier) start it.
INSERT INTO heartbeat_run_log (heartbeat_id, run, at, session_id, outcome)
  SELECT r.heartbeat_id, r.run, r.started_at, r.session_id, 'unknown' FROM heartbeat_runs r
  WHERE NOT EXISTS (SELECT 1 FROM heartbeat_run_log l WHERE l.session_id = r.session_id)
  ORDER BY r.started_at, r.run;
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

    /// Forget a heartbeat and its run record; its runs stay as plain
    /// sessions.
    pub fn delete_heartbeat(&self, id: &HeartbeatId) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        tx.execute("DELETE FROM heartbeats WHERE id = ?1", params![id.as_str()])?;
        tx.execute(
            "DELETE FROM heartbeat_runs WHERE heartbeat_id = ?1",
            params![id.as_str()],
        )?;
        tx.execute(
            "DELETE FROM heartbeat_run_log WHERE heartbeat_id = ?1",
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

    /// Add `run` to heartbeat `id`'s run record: its `seq`.
    pub fn log_heartbeat_run(&self, id: &HeartbeatId, run: &HeartbeatRun) -> Result<u64> {
        self.conn.execute(
            "INSERT INTO heartbeat_run_log (heartbeat_id, run, at, session_id, outcome, detail) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                id.as_str(),
                run.run,
                run.at.saturating_mul(1000),
                run.session_id.as_ref().map(SessionId::as_str),
                outcome_text(run.outcome),
                run.detail,
            ],
        )?;
        Ok(u64::try_from(self.conn.last_insert_rowid()).unwrap_or(0))
    }

    /// What became of the run whose session is `session`, in its record.
    pub fn set_run_outcome(
        &self,
        session: &SessionId,
        outcome: RunOutcome,
        detail: Option<&str>,
    ) -> Result<()> {
        self.conn.execute(
            "UPDATE heartbeat_run_log SET outcome = ?2, detail = ?3 WHERE session_id = ?1",
            params![session.as_str(), outcome_text(outcome), detail],
        )?;
        Ok(())
    }

    /// Heartbeat `id`'s runs, newest first: at most `limit`, those before
    /// `seq` `before`, and whether older ones are left.
    pub fn heartbeat_run_log(
        &self,
        id: &HeartbeatId,
        before: Option<u64>,
        limit: u32,
    ) -> Result<(Vec<(u64, HeartbeatRun)>, bool)> {
        let before = before.map_or(i64::MAX, |seq| i64::try_from(seq).unwrap_or(i64::MAX));
        let mut statement = self.conn.prepare(
            "SELECT seq, run, at, session_id, outcome, detail FROM heartbeat_run_log \
             WHERE heartbeat_id = ?1 AND seq < ?2 ORDER BY seq DESC LIMIT ?3",
        )?;
        let rows =
            statement.query_map(params![id.as_str(), before, i64::from(limit) + 1], |row| {
                let seq: i64 = row.get(0)?;
                let outcome: String = row.get(4)?;
                Ok((
                    u64::try_from(seq).unwrap_or(0),
                    HeartbeatRun {
                        run: row.get(1)?,
                        at: row.get::<_, i64>(2)?.div_euclid(1000),
                        session_id: row.get::<_, Option<String>>(3)?.map(SessionId::from_string),
                        outcome: serde_json::from_value(serde_json::Value::String(outcome))
                            .unwrap_or(RunOutcome::Unknown),
                        detail: row.get(5)?,
                    },
                ))
            })?;
        let mut runs = rows.collect::<rusqlite::Result<Vec<_>>>()?;
        let more = runs.len() > limit as usize;
        runs.truncate(limit as usize);
        Ok((runs, more))
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

/// A run's outcome as the record keeps it: its wire name.
fn outcome_text(outcome: RunOutcome) -> String {
    match serde_json::to_value(outcome) {
        Ok(serde_json::Value::String(text)) => text,
        _ => "unknown".into(),
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

    #[test]
    fn every_run_is_recorded_newest_first_in_pages() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("r.sqlite3");
        let registry = Registry::open(&path, 0).unwrap();
        let deps = heartbeat("deps");
        registry.save_heartbeat(&deps).unwrap();
        // A run listed before the record was kept starts it.
        registry
            .add_heartbeat_run(&RunRow {
                session_id: SessionId::from_string("S1"),
                heartbeat: deps.id.clone(),
                run: 1,
                started_ms: 1_000,
            })
            .unwrap();
        drop(registry);
        let registry = Registry::open(&path, 0).unwrap();
        let started = |run: u32, session: &str| HeartbeatRun {
            run,
            at: 2,
            session_id: Some(SessionId::from_string(session)),
            outcome: RunOutcome::Started,
            detail: None,
        };
        registry
            .log_heartbeat_run(&deps.id, &started(2, "S2"))
            .unwrap();
        let skipped = HeartbeatRun {
            run: 0,
            at: 3,
            session_id: None,
            outcome: RunOutcome::Skipped,
            detail: Some("run #2 is still at work".into()),
        };
        registry.log_heartbeat_run(&deps.id, &skipped).unwrap();
        registry
            .log_heartbeat_run(&deps.id, &started(3, "S3"))
            .unwrap();
        registry
            .set_run_outcome(&SessionId::from_string("S2"), RunOutcome::TaskGiven, None)
            .unwrap();
        // Opening it again adds nothing twice.
        drop(registry);
        let registry = Registry::open(&path, 0).unwrap();
        let (page, more) = registry.heartbeat_run_log(&deps.id, None, 2).unwrap();
        assert!(more);
        let runs: Vec<(u32, RunOutcome)> = page.iter().map(|(_, r)| (r.run, r.outcome)).collect();
        assert_eq!(runs, [(3, RunOutcome::Started), (0, RunOutcome::Skipped)]);
        assert_eq!(page[1].1.detail.as_deref(), Some("run #2 is still at work"));
        let (rest, more) = registry
            .heartbeat_run_log(&deps.id, Some(page[1].0), 10)
            .unwrap();
        assert!(!more);
        let runs: Vec<(u32, RunOutcome, i64)> =
            rest.iter().map(|(_, r)| (r.run, r.outcome, r.at)).collect();
        assert_eq!(
            runs,
            [(2, RunOutcome::TaskGiven, 2), (1, RunOutcome::Unknown, 1)]
        );
        registry.delete_heartbeat(&deps.id).unwrap();
        assert!(registry
            .heartbeat_run_log(&deps.id, None, 10)
            .unwrap()
            .0
            .is_empty());
    }
}
