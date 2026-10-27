//! Where each session has worked (`SessionView::places`), from what its
//! agent's conversation shows ([`crate::agent_output::places`]), its agent's
//! working-directory reports and its terminal's directories.
//!
//! A place is a git working tree (a repository's main worktree or a linked
//! one, found from `.git` files only) where the session changed files: a
//! touched directory becomes the working tree it is in ([`resolve`], which
//! reads files and so runs without the state lock), and a directory outside
//! any repository is no place at all, nor is one read from the registry
//! ([`Places::new`]). Working trees in the agents' own data directories (but
//! OpenCode's worktrees) and the kernel's (`/proc`, `/sys`, `/dev`) are
//! never places. Working trees only worked in (started in, gone into,
//! searched, read from) are kept too, out of sight, so one becomes a place
//! when files there are changed later ([`Places::shown`]). Each session
//! keeps its [`MAX_PLACES`] most recent of each, in memory and in the
//! registry (`session_places`), so ended sessions keep theirs; a working
//! tree's time moves on by at least [`MIN_STEP_SECS`] at a time, so an agent
//! at work saves its list at most about once a minute.

use crate::agent_output::places::Touch;
use crate::git;
use session_core::ids::SessionId;
use session_core::model::{GitInfo, SessionPlace, MAX_PLACES};
use std::collections::HashMap;
use std::path::PathBuf;

/// The least a place's `last_at` moves on, in seconds.
pub(super) const MIN_STEP_SECS: i64 = 60;

/// Directories whose contents are never places.
const SYSTEM_DIRS: &[&str] = &["/proc", "/sys", "/dev"];

/// A touched directory resolved to the working tree it is in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Resolved {
    /// The working tree's top (`GitInfo::repo_root`).
    pub path: PathBuf,
    pub git: GitInfo,
    /// Unix seconds.
    pub at: i64,
    pub edited: bool,
}

/// What [`resolve`] leaves out (canonical): the agents' data directories.
#[derive(Clone, Debug, Default)]
pub(super) struct Excluded {
    data_dirs: Vec<PathBuf>,
}

impl Excluded {
    /// This machine's: [`crate::agent_output::places::agent_data_dirs`].
    pub fn of_this_machine() -> Self {
        Self::new(crate::agent_output::places::agent_data_dirs())
    }

    pub fn new(data_dirs: Vec<PathBuf>) -> Self {
        Self {
            data_dirs: data_dirs
                .into_iter()
                .map(|dir| std::fs::canonicalize(&dir).unwrap_or(dir))
                .collect(),
        }
    }
}

/// The places `touches` belong to, one per distinct directory touched (its
/// latest time; edited when any touch was), with `now_ms` for touches
/// without a time. Directories outside any working tree, and ones that no
/// longer exist, are passed over.
pub(super) fn resolve(touches: Vec<Touch>, excluded: &Excluded, now_ms: i64) -> Vec<Resolved> {
    let mut dirs: HashMap<PathBuf, (i64, bool)> = HashMap::new();
    for touch in touches {
        let at = touch.at.unwrap_or(now_ms);
        let entry = dirs.entry(touch.dir).or_insert((at, false));
        entry.0 = entry.0.max(at);
        entry.1 |= touch.edited;
    }
    let mut resolved: Vec<Resolved> = dirs
        .into_iter()
        .filter_map(|(dir, (at, edited))| {
            let dir = std::fs::canonicalize(dir).ok()?;
            if !dir.is_dir() || SYSTEM_DIRS.iter().any(|system| dir.starts_with(system)) {
                return None;
            }
            let git = git::detect(&dir)?;
            // An agent's own directory, unless it holds a working tree of
            // the user's (OpenCode keeps its worktrees there).
            let own = excluded.data_dirs.iter().find(|data| dir.starts_with(data));
            if own.is_some_and(|data| !git.repo_root.starts_with(data)) {
                return None;
            }
            Some(Resolved {
                path: git.repo_root.clone(),
                git,
                at: at.div_euclid(1000),
                edited,
            })
        })
        .collect();
    resolved.sort_by(|a, b| a.at.cmp(&b.at).then_with(|| a.path.cmp(&b.path)));
    resolved
}

/// Every session's working trees, most recent first: its places (`edited`)
/// and the ones it only worked in.
#[derive(Debug, Default)]
pub(super) struct Places {
    by_session: HashMap<SessionId, Vec<SessionPlace>>,
}

impl Places {
    /// The places the registry holds, but the ones without git context:
    /// directories outside any repository, which earlier releases kept.
    /// They stay in the registry until the session's list is next saved.
    pub fn new(mut by_session: HashMap<SessionId, Vec<SessionPlace>>) -> Self {
        for list in by_session.values_mut() {
            list.retain(|place| place.git.is_some());
            sort(list);
        }
        Self { by_session }
    }

    /// `id`'s places (`SessionView::places`): the working trees where it
    /// changed files, most recent first.
    pub fn shown(&self, id: &SessionId) -> Vec<SessionPlace> {
        self.by_session
            .get(id)
            .into_iter()
            .flatten()
            .filter(|place| place.edited)
            .cloned()
            .collect()
    }

    pub fn remove(&mut self, id: &SessionId) {
        self.by_session.remove(id);
    }

    /// Record that `id` worked in `resolved`. Returns its list (every working
    /// tree, edited or not) when that changed, to save; the caller's refresh
    /// publishes what [`Places::shown`] says now. A working tree's time is
    /// its latest touch's, moving on by [`MIN_STEP_SECS`] at least; its git
    /// context is the latest touch's; it's edited once any touch was.
    pub fn apply(&mut self, id: &SessionId, resolved: Vec<Resolved>) -> Option<&[SessionPlace]> {
        if resolved.is_empty() {
            return None;
        }
        let list = self.by_session.entry(id.clone()).or_default();
        let before = list.clone();
        for place in resolved {
            match list.iter_mut().find(|listed| listed.path == place.path) {
                Some(listed) => {
                    if place.at >= listed.last_at.saturating_add(MIN_STEP_SECS) {
                        listed.last_at = place.at;
                    }
                    if place.at >= listed.last_at {
                        listed.git = Some(place.git);
                    }
                    listed.edited |= place.edited;
                }
                None => list.push(SessionPlace {
                    path: place.path,
                    git: Some(place.git),
                    last_at: place.at,
                    edited: place.edited,
                }),
            }
        }
        sort(list);
        // The most recent MAX_PLACES of each kind: working in many trees
        // never pushes out one where files were changed.
        let (mut edited, mut only_worked_in) = (0, 0);
        list.retain(|place| {
            let kept = if place.edited {
                &mut edited
            } else {
                &mut only_worked_in
            };
            *kept += 1;
            *kept <= MAX_PLACES
        });
        (*list != before).then_some(list.as_slice())
    }
}

/// Most recent first, then by path.
fn sort(list: &mut [SessionPlace]) {
    list.sort_by(|a, b| b.last_at.cmp(&a.last_at).then_with(|| a.path.cmp(&b.path)));
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn resolved(path: &str, at: i64, edited: bool) -> Resolved {
        Resolved {
            path: path.into(),
            git: GitInfo {
                repo_root: path.into(),
                branch: Some("main".into()),
                ..GitInfo::default()
            },
            at,
            edited,
        }
    }

    fn paths(list: &[SessionPlace]) -> Vec<String> {
        list.iter()
            .map(|place| format!("{} {}", place.path.display(), place.last_at))
            .collect()
    }

    #[test]
    fn places_are_kept_most_recent_first_and_move_on_a_minute_at_a_time() {
        let mut places = Places::default();
        let id = SessionId::from_string("A");
        let list = places
            .apply(
                &id,
                vec![
                    resolved("/repo", 1000, false),
                    resolved("/wt/x", 1010, true),
                ],
            )
            .unwrap();
        assert_eq!(paths(list), ["/wt/x 1010", "/repo 1000"]);
        assert!(list[0].edited);
        // Back within the minute: nothing to save.
        assert!(places
            .apply(&id, vec![resolved("/repo", 1030, false)])
            .is_none());
        // A minute on, it moves up, with the branch it's on now.
        let mut checked_out = resolved("/repo", 1070, false);
        checked_out.git.branch = Some("feature".into());
        let list = places.apply(&id, vec![checked_out]).unwrap();
        assert_eq!(paths(list), ["/repo 1070", "/wt/x 1010"]);
        assert_eq!(
            list[0].git.as_ref().unwrap().branch.as_deref(),
            Some("feature")
        );
        // An older touch (reading a log again) keeps the time and branch,
        // and counts its edit.
        assert!(places
            .apply(&id, vec![resolved("/repo", 10, true)])
            .is_some_and(|list| list[0].edited && list[0].last_at == 1070));
        // At most MAX_PLACES of each kind: the oldest go, and working in
        // many more trees keeps every one where files were changed.
        let many = |from: i64, edited: bool| {
            (0..MAX_PLACES as i64 + 8)
                .map(|n| resolved(&format!("/r/{from}/{n}"), from + n, edited))
                .collect()
        };
        let list = places.apply(&id, many(2000, true)).unwrap();
        assert_eq!(list.len(), MAX_PLACES);
        assert!(list.iter().all(|place| place.edited));
        assert!(!list.iter().any(|place| place.path.starts_with("/repo")));
        let list = places.apply(&id, many(3000, false)).unwrap();
        assert_eq!(list.len(), MAX_PLACES * 2);
        assert_eq!(places.shown(&id).len(), MAX_PLACES);
        assert!(places
            .shown(&id)
            .iter()
            .all(|place| place.path.starts_with("/r/2000")));
    }

    #[test]
    fn only_working_trees_where_files_were_changed_are_shown() {
        let mut places = Places::default();
        let id = SessionId::from_string("A");
        // Started in the main checkout, then made a worktree, went into it
        // and changed files there.
        let list = places
            .apply(
                &id,
                vec![
                    resolved("/repo", 1000, false),
                    resolved("/repo/worktrees/fix", 1100, false),
                    resolved("/repo/worktrees/fix", 1200, true),
                ],
            )
            .unwrap();
        // Both are kept, to save; only the worktree is a place.
        assert_eq!(paths(list), ["/repo/worktrees/fix 1200", "/repo 1000"]);
        assert_eq!(paths(&places.shown(&id)), ["/repo/worktrees/fix 1200"]);
        // Reading in the main checkout later still isn't changing it.
        places.apply(&id, vec![resolved("/repo", 1300, false)]);
        assert_eq!(paths(&places.shown(&id)), ["/repo/worktrees/fix 1200"]);
        // Changing a file there makes it a place, at its latest touch.
        places.apply(&id, vec![resolved("/repo", 1310, true)]);
        assert_eq!(
            paths(&places.shown(&id)),
            ["/repo 1300", "/repo/worktrees/fix 1200"]
        );
        // A session that changed nothing has no places.
        let idle = SessionId::from_string("B");
        assert!(places
            .apply(&idle, vec![resolved("/repo", 1000, false)])
            .is_some());
        assert!(places.shown(&idle).is_empty());
    }

    #[test]
    fn stored_places_outside_repositories_are_dropped() {
        let place = |path: &str, git: bool, last_at: i64| SessionPlace {
            path: path.into(),
            git: git.then(|| GitInfo {
                repo_root: path.into(),
                branch: Some("main".into()),
                ..GitInfo::default()
            }),
            last_at,
            edited: true,
        };
        let id = SessionId::from_string("A");
        let only_folders = SessionId::from_string("B");
        let mut places = Places::new(HashMap::from([
            (
                id.clone(),
                vec![
                    place("/home/me/repos", false, 300),
                    place("/home/me/repos/app", true, 100),
                    place("/home/me/Downloads", false, 50),
                    place("/home/me/repos/wt", true, 200),
                ],
            ),
            (only_folders.clone(), vec![place("/srv", false, 10)]),
        ]));
        assert_eq!(
            paths(&places.shown(&id)),
            ["/home/me/repos/wt 200", "/home/me/repos/app 100"]
        );
        assert!(places.shown(&only_folders).is_empty());
        // The next change saves the list without them.
        let list = places
            .apply(&id, vec![resolved("/home/me/repos/app", 400, false)])
            .unwrap();
        assert_eq!(
            paths(list),
            ["/home/me/repos/app 400", "/home/me/repos/wt 200"]
        );
    }

    #[test]
    fn touches_resolve_to_working_trees_only_and_leave_out_the_agents() {
        let dir = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(dir.path()).unwrap();
        let repo = root.join("repo");
        fs::create_dir_all(repo.join(".git")).unwrap();
        fs::write(repo.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
        fs::create_dir_all(repo.join("src/deep")).unwrap();
        let detached = root.join("detached");
        fs::create_dir_all(detached.join(".git")).unwrap();
        fs::write(
            detached.join(".git/HEAD"),
            format!("{}\n", "ab12cd34".repeat(5)),
        )
        .unwrap();
        fs::create_dir_all(root.join("plain/sub")).unwrap();
        fs::create_dir_all(root.join("agent/memory")).unwrap();
        let worktree = root.join("agent/worktree/w1");
        fs::create_dir_all(worktree.join(".git")).unwrap();
        fs::write(worktree.join(".git/HEAD"), "ref: refs/heads/feature\n").unwrap();
        let excluded = Excluded::new(vec![root.join("agent")]);
        let touch = |dir: PathBuf, at: Option<i64>, edited: bool| Touch { dir, edited, at };
        let found = resolve(
            vec![
                touch(repo.join("src/deep"), Some(5_000), false),
                touch(repo.join("src"), Some(9_000), true),
                touch(detached.clone(), None, false),
                // Outside any repository: no place, itself or above.
                touch(root.join("plain/sub"), Some(1_000), true),
                touch(root.join("plain"), Some(1_000), false),
                touch(root.clone(), Some(1_000), false),
                touch(root.join("agent/memory"), Some(1_000), true),
                touch(worktree.clone(), Some(2_000), false),
                touch(root.join("gone"), Some(1_000), false),
                touch("/proc/self".into(), Some(1_000), false),
            ],
            &excluded,
            70_000,
        );
        let summary: Vec<(PathBuf, Option<String>, Option<String>, i64, bool)> = found
            .into_iter()
            .map(|r| (r.path, r.git.branch, r.git.detached_at, r.at, r.edited))
            .collect();
        assert_eq!(
            summary,
            [
                (worktree, Some("feature".into()), None, 2, false),
                (repo.clone(), Some("main".into()), None, 5, false),
                (repo, Some("main".into()), None, 9, true),
                // Detached: the working tree, at its commit.
                (detached, None, Some("ab12cd3".into()), 70, false),
            ]
        );
    }
}
