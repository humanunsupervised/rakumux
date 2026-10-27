//! Per-session git context (`SessionView.git`, read by `crate::git`).
//!
//! Each session's `GitInfo` is cached with the directory it was read for,
//! so a cwd change is a cache miss. Running sessions' entries are re-read on
//! a poll, since a checkout changes HEAD without changing any session's
//! directory; ended sessions keep the context they had (a TUI showing every
//! folder lists many of them). The service reads files outside its state
//! lock (see [`read_all`]).

use crate::git;
use session_core::ids::SessionId;
use session_core::model::GitInfo;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// How often cached contexts are re-read.
pub(super) const POLL_MS: i64 = 2000;

/// A context read from `dir`.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Entry {
    dir: PathBuf,
    info: Option<GitInfo>,
}

/// Cached git contexts of sessions.
#[derive(Debug, Default)]
pub(super) struct GitContexts {
    entries: HashMap<SessionId, Entry>,
    last_poll_ms: i64,
}

/// A session's directory and its freshly read context.
pub(super) type Reading = (SessionId, PathBuf, Option<GitInfo>);

impl GitContexts {
    /// The context of `id` if it was read for `dir`.
    pub fn get(&self, id: &SessionId, dir: &Path) -> Option<&Option<GitInfo>> {
        self.entries
            .get(id)
            .filter(|entry| entry.dir == dir)
            .map(|entry| &entry.info)
    }

    /// Read `dir` unless the context of `id` was already read for it.
    pub fn ensure(&mut self, id: &SessionId, dir: &Path) {
        if self.get(id, dir).is_none() {
            self.set(id.clone(), dir.to_path_buf(), git::detect(dir));
        }
    }

    /// Store a context read from `dir`, replacing any other.
    pub fn set(&mut self, id: SessionId, dir: PathBuf, info: Option<GitInfo>) {
        self.entries.insert(id, Entry { dir, info });
    }

    pub fn remove(&mut self, id: &SessionId) {
        self.entries.remove(id);
    }

    /// When a poll is due at `now_ms`: the sessions (those `running` says
    /// are running) and directories to re-read (the poll counts as done).
    pub fn poll_due(
        &mut self,
        now_ms: i64,
        running: impl Fn(&SessionId) -> bool,
    ) -> Option<Vec<(SessionId, PathBuf)>> {
        if now_ms.saturating_sub(self.last_poll_ms) < POLL_MS {
            return None;
        }
        self.last_poll_ms = now_ms;
        Some(
            self.entries
                .iter()
                .filter(|(id, _)| running(id))
                .map(|(id, entry)| (id.clone(), entry.dir.clone()))
                .collect(),
        )
    }

    /// Store re-read contexts. Returns the sessions whose context changed;
    /// readings of a directory the session has since left are dropped.
    pub fn apply(&mut self, readings: Vec<Reading>) -> Vec<SessionId> {
        let mut changed = Vec::new();
        for (id, dir, info) in readings {
            let Some(entry) = self.entries.get_mut(&id) else {
                continue;
            };
            if entry.dir == dir && entry.info != info {
                entry.info = info;
                changed.push(id);
            }
        }
        changed
    }
}

/// Read the context of each distinct directory once.
pub(super) fn read_all(targets: Vec<(SessionId, PathBuf)>) -> Vec<Reading> {
    let mut by_dir: HashMap<PathBuf, Option<GitInfo>> = HashMap::new();
    targets
        .into_iter()
        .map(|(id, dir)| {
            let info = by_dir
                .entry(dir.clone())
                .or_insert_with(|| git::detect(&dir))
                .clone();
            (id, dir, info)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(branch: &str) -> Option<GitInfo> {
        Some(GitInfo {
            repo_root: "/r".into(),
            branch: Some(branch.into()),
            detached_at: None,
            worktree: None,
            repo: None,
        })
    }

    #[test]
    fn entries_belong_to_a_directory() {
        let mut cache = GitContexts::default();
        let a = SessionId::from_string("A");
        cache.set(a.clone(), "/r".into(), info("main"));
        assert_eq!(cache.get(&a, Path::new("/r")), Some(&info("main")));
        assert_eq!(cache.get(&a, Path::new("/elsewhere")), None);
        cache.remove(&a);
        assert_eq!(cache.get(&a, Path::new("/r")), None);
    }

    #[test]
    fn polls_are_spaced_and_report_changes_only() {
        let mut cache = GitContexts::default();
        let (a, b) = (SessionId::from_string("A"), SessionId::from_string("B"));
        cache.set(a.clone(), "/r".into(), info("main"));
        cache.set(b.clone(), "/s".into(), info("main"));
        let mut due = cache.poll_due(10_000, |_| true).unwrap();
        due.sort_by(|x, y| x.0.as_str().cmp(y.0.as_str()));
        assert_eq!(
            due,
            vec![(a.clone(), "/r".into()), (b.clone(), "/s".into())]
        );
        assert!(cache.poll_due(10_000 + POLL_MS - 1, |_| true).is_none());
        // Only running sessions are re-read.
        assert_eq!(
            cache.poll_due(10_000 + POLL_MS, |id| id == &a),
            Some(vec![(a.clone(), "/r".into())])
        );

        let changed = cache.apply(vec![
            (a.clone(), "/r".into(), info("feature")),
            (b.clone(), "/s".into(), info("main")),
        ]);
        assert_eq!(changed, vec![a.clone()]);
        assert_eq!(cache.get(&a, Path::new("/r")), Some(&info("feature")));
        // A reading of a directory the session has left is stale.
        cache.set(b.clone(), "/t".into(), None);
        assert!(cache
            .apply(vec![(b.clone(), "/s".into(), info("x"))])
            .is_empty());
        assert_eq!(cache.get(&b, Path::new("/t")), Some(&None));
    }
}
