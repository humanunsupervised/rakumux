//! Where each session has worked (`SessionView::places`), from what its
//! agent's conversation shows ([`crate::agent_output::places`]), its agent's
//! working-directory reports and its terminal's directories.
//!
//! A touched directory becomes a place ([`resolve`], which reads files and
//! so runs without the state lock): the git working tree it is in (`.git`
//! files only), or outside any repository the directory itself, folded into
//! a listed directory above it ([`Places::apply`]). The agents' own data
//! directories and the kernel's (`/proc`, `/sys`, `/dev`) are never places,
//! nor are `/`, the home directory and the temporary directory themselves. Each session keeps its [`MAX_PLACES`] most
//! recent, in memory and in the registry (`session_places`), so ended
//! sessions keep theirs; a place's time moves on by at least
//! [`MIN_STEP_SECS`] at a time, so an agent at work saves its list at most
//! about once a minute.

use crate::agent_output::places::Touch;
use crate::git;
use session_core::ids::SessionId;
use session_core::model::{GitInfo, SessionPlace, MAX_PLACES};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// The least a place's `last_at` moves on, in seconds.
pub(super) const MIN_STEP_SECS: i64 = 60;

/// Directories whose contents are never places.
const SYSTEM_DIRS: &[&str] = &["/proc", "/sys", "/dev"];

/// A touched directory resolved to the place it belongs to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Resolved {
    pub path: PathBuf,
    pub git: Option<GitInfo>,
    /// Unix seconds.
    pub at: i64,
    pub edited: bool,
}

/// What [`resolve`] leaves out (canonical): the agents' data directories,
/// and directories that hold too much else to be a place themselves.
#[derive(Clone, Debug, Default)]
pub(super) struct Excluded {
    data_dirs: Vec<PathBuf>,
    shared: Vec<PathBuf>,
}

impl Excluded {
    /// This machine's: [`crate::agent_output::places::agent_data_dirs`],
    /// and `$HOME`, `/tmp` and the temporary directory.
    pub fn of_this_machine() -> Self {
        let home = std::env::var_os("HOME")
            .filter(|home| !home.is_empty())
            .map(PathBuf::from);
        let shared = home
            .into_iter()
            .chain([PathBuf::from("/tmp"), std::env::temp_dir()])
            .collect();
        Self::new(crate::agent_output::places::agent_data_dirs(), shared)
    }

    pub fn new(data_dirs: Vec<PathBuf>, shared: Vec<PathBuf>) -> Self {
        let canonical = |dirs: Vec<PathBuf>| {
            dirs.into_iter()
                .map(|dir| std::fs::canonicalize(&dir).unwrap_or(dir))
                .collect()
        };
        Self {
            data_dirs: canonical(data_dirs),
            shared: canonical(shared),
        }
    }
}

/// The places `touches` belong to, one per distinct directory touched (its
/// latest time; edited when any touch was), with `now_ms` for touches
/// without a time. Directories that no longer exist are passed over.
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
            let git = git::detect(&dir);
            // An agent's own directory, unless it holds a working tree of
            // the user's (OpenCode keeps its worktrees there).
            let own = excluded.data_dirs.iter().find(|data| dir.starts_with(data));
            if let Some(data) = own {
                if !git
                    .as_ref()
                    .is_some_and(|git| git.repo_root.starts_with(data))
                {
                    return None;
                }
            }
            let path = match &git {
                Some(git) => git.repo_root.clone(),
                None if dir.parent().is_none() || excluded.shared.contains(&dir) => return None,
                None => dir,
            };
            Some(Resolved {
                path,
                git,
                at: at.div_euclid(1000),
                edited,
            })
        })
        .collect();
    resolved.sort_by(|a, b| a.at.cmp(&b.at).then_with(|| a.path.cmp(&b.path)));
    resolved
}

/// Every session's places, most recent first.
#[derive(Debug, Default)]
pub(super) struct Places {
    by_session: HashMap<SessionId, Vec<SessionPlace>>,
}

impl Places {
    /// The places the registry holds.
    pub fn new(by_session: HashMap<SessionId, Vec<SessionPlace>>) -> Self {
        let mut places = Self { by_session };
        for list in places.by_session.values_mut() {
            sort(list);
        }
        places
    }

    pub fn get(&self, id: &SessionId) -> &[SessionPlace] {
        self.by_session.get(id).map_or(&[], Vec::as_slice)
    }

    pub fn remove(&mut self, id: &SessionId) {
        self.by_session.remove(id);
    }

    /// Record that `id` worked in `resolved`. Returns its list when that
    /// changed (to save and publish). A directory outside any repository
    /// is folded into a listed one above it, and takes in listed ones below
    /// it; a place's time moves on by [`MIN_STEP_SECS`] at least; its git
    /// context is the latest touch's.
    pub fn apply(&mut self, id: &SessionId, resolved: Vec<Resolved>) -> Option<&[SessionPlace]> {
        if resolved.is_empty() {
            return None;
        }
        let list = self.by_session.entry(id.clone()).or_default();
        let before = list.clone();
        for place in resolved {
            let key = match &place.git {
                Some(_) => place.path.clone(),
                None => {
                    let above = list
                        .iter()
                        .filter(|listed| {
                            listed.git.is_none() && place.path.starts_with(&listed.path)
                        })
                        .map(|listed| listed.path.clone())
                        .min_by_key(|path| path.components().count());
                    match above {
                        Some(above) => above,
                        None => {
                            take_in_below(list, &place.path);
                            place.path.clone()
                        }
                    }
                }
            };
            match list.iter_mut().find(|listed| listed.path == key) {
                Some(listed) => {
                    if place.at >= listed.last_at.saturating_add(MIN_STEP_SECS) {
                        listed.last_at = place.at;
                    }
                    if place.at >= listed.last_at && key == place.path {
                        listed.git = place.git;
                    }
                    listed.edited |= place.edited;
                }
                None => list.push(SessionPlace {
                    path: key,
                    git: place.git,
                    last_at: place.at,
                    edited: place.edited,
                }),
            }
        }
        sort(list);
        list.truncate(MAX_PLACES);
        (*list != before).then_some(list.as_slice())
    }
}

/// Fold the listed directories outside any repository below `dir` into one
/// entry for `dir` (their latest time; edited when any was).
fn take_in_below(list: &mut Vec<SessionPlace>, dir: &Path) {
    let mut taken: Option<SessionPlace> = None;
    list.retain(|listed| {
        let below = listed.git.is_none() && listed.path.starts_with(dir) && listed.path != dir;
        if below {
            let place = taken.get_or_insert_with(|| SessionPlace {
                path: dir.to_path_buf(),
                git: None,
                last_at: listed.last_at,
                edited: false,
            });
            place.last_at = place.last_at.max(listed.last_at);
            place.edited |= listed.edited;
        }
        !below
    });
    list.extend(taken);
}

/// Most recent first, then by path.
fn sort(list: &mut [SessionPlace]) {
    list.sort_by(|a, b| b.last_at.cmp(&a.last_at).then_with(|| a.path.cmp(&b.path)));
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn resolved(path: &str, git: bool, at: i64, edited: bool) -> Resolved {
        Resolved {
            path: path.into(),
            git: git.then(|| GitInfo {
                repo_root: path.into(),
                branch: Some("main".into()),
                ..GitInfo::default()
            }),
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
                    resolved("/repo", true, 1000, false),
                    resolved("/wt/x", true, 1010, true),
                ],
            )
            .unwrap();
        assert_eq!(paths(list), ["/wt/x 1010", "/repo 1000"]);
        assert!(list[0].edited);
        // Back within the minute: nothing to save.
        assert!(places
            .apply(&id, vec![resolved("/repo", true, 1030, false)])
            .is_none());
        // A minute on, it moves up, with the branch it's on now.
        let mut checked_out = resolved("/repo", true, 1070, false);
        if let Some(git) = checked_out.git.as_mut() {
            git.branch = Some("feature".into());
        }
        let list = places.apply(&id, vec![checked_out]).unwrap();
        assert_eq!(paths(list), ["/repo 1070", "/wt/x 1010"]);
        assert_eq!(
            list[0].git.as_ref().unwrap().branch.as_deref(),
            Some("feature")
        );
        // An older touch (reading a log again) keeps the time and branch,
        // and counts its edit.
        assert!(places
            .apply(&id, vec![resolved("/repo", true, 10, true)])
            .is_some_and(|list| list[0].edited && list[0].last_at == 1070));
        // At most MAX_PLACES: the oldest go.
        let many = (0..MAX_PLACES as i64)
            .map(|n| resolved(&format!("/r/{n}"), true, 2000 + n, false))
            .collect();
        let list = places.apply(&id, many).unwrap();
        assert_eq!(list.len(), MAX_PLACES);
        assert!(list.iter().all(|place| place.path.starts_with("/r")));
    }

    #[test]
    fn directories_outside_repositories_fold_into_the_one_above() {
        let mut places = Places::default();
        let id = SessionId::from_string("A");
        places.apply(
            &id,
            vec![
                resolved("/dl/logo/concepts", false, 100, false),
                resolved("/dl/logo/screens", false, 200, true),
            ],
        );
        // A directory above both takes them in.
        let list = places
            .apply(&id, vec![resolved("/dl/logo", false, 150, false)])
            .unwrap();
        assert_eq!(paths(list), ["/dl/logo 200"]);
        assert!(list[0].edited);
        // One below it is folded in.
        let list = places
            .apply(&id, vec![resolved("/dl/logo/x/y", false, 400, false)])
            .unwrap();
        assert_eq!(paths(list), ["/dl/logo 400"]);
        // A repository inside is a place of its own.
        let list = places
            .apply(&id, vec![resolved("/dl/logo/repo", true, 500, false)])
            .unwrap();
        assert_eq!(paths(list), ["/dl/logo/repo 500", "/dl/logo 400"]);
    }

    #[test]
    fn touches_resolve_to_working_trees_and_leave_out_agents_and_home() {
        let dir = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(dir.path()).unwrap();
        let repo = root.join("repo");
        fs::create_dir_all(repo.join(".git")).unwrap();
        fs::write(repo.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
        fs::create_dir_all(repo.join("src/deep")).unwrap();
        fs::create_dir_all(root.join("plain/sub")).unwrap();
        fs::create_dir_all(root.join("agent/memory")).unwrap();
        let worktree = root.join("agent/worktree/w1");
        fs::create_dir_all(worktree.join(".git")).unwrap();
        fs::write(worktree.join(".git/HEAD"), "ref: refs/heads/feature\n").unwrap();
        let home = root.join("home");
        fs::create_dir_all(&home).unwrap();
        let excluded = Excluded::new(vec![root.join("agent")], vec![home.clone()]);
        let touch = |dir: PathBuf, at: Option<i64>, edited: bool| Touch { dir, edited, at };
        let found = resolve(
            vec![
                touch(repo.join("src/deep"), Some(5_000), false),
                touch(repo.join("src"), Some(9_000), true),
                touch(root.join("plain/sub"), None, false),
                touch(root.join("agent/memory"), Some(1_000), true),
                touch(worktree.clone(), Some(2_000), false),
                touch(home.clone(), Some(1_000), false),
                touch(root.join("gone"), Some(1_000), false),
                touch("/proc/self".into(), Some(1_000), false),
            ],
            &excluded,
            70_000,
        );
        let summary: Vec<(PathBuf, Option<String>, i64, bool)> = found
            .into_iter()
            .map(|r| (r.path, r.git.and_then(|g| g.branch), r.at, r.edited))
            .collect();
        assert_eq!(
            summary,
            [
                (worktree, Some("feature".into()), 2, false),
                (repo.clone(), Some("main".into()), 5, false),
                (repo, Some("main".into()), 9, true),
                (root.join("plain/sub"), None, 70, false),
            ]
        );
    }
}
