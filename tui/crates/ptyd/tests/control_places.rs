//! A session's places (`SessionView::places`) forget the working trees gone
//! from disk: a worktree removed once its branch was merged, a checkout
//! deleted. Real control layer and registry, fake terminals.

mod control_common;

use control_common::*;
use std::path::Path;

fn place_paths(h: &Harness, id: &session_core::ids::SessionId) -> Vec<String> {
    h.get(id)
        .places
        .iter()
        .map(|place| place.path.display().to_string())
        .collect()
}

fn add_place(h: &Harness, id: &session_core::ids::SessionId, path: &Path, at: i64) {
    let git = serde_json::json!({ "repo_root": path, "branch": "main", "repo": "web" });
    h.db()
        .execute(
            "INSERT INTO session_places (session_id, path, git_json, last_at, edited) \
             VALUES (?1, ?2, ?3, ?4, 1)",
            rusqlite::params![id.as_str(), path.display().to_string(), git.to_string(), at],
        )
        .unwrap();
}

#[test]
fn a_removed_worktree_leaves_every_sessions_places() {
    let mut h = Harness::new();
    let root = h.root();
    let kept = root.join("web");
    let merged = root.join("web-worktrees/fix-login");
    for dir in [&kept, &merged] {
        std::fs::create_dir_all(dir).unwrap();
    }
    let live = h.create(Some("live"));
    let ended = h.create(Some("ended"));
    for id in [&live.id, &ended.id] {
        add_place(&h, id, &kept, 100);
        add_place(&h, id, &merged, 200);
    }
    h.host.exit(&ended.id, Some(0));
    h.wait_recorded_exit(&ended.id);
    // The registry's places are read as the daemon starts.
    h.restart();
    let both = [merged.display().to_string(), kept.display().to_string()];
    assert_eq!(place_paths(&h, &live.id), both);

    // The worktree is removed: within a sweep, neither session lists it, in
    // its view and in the registry, and the folder that's still there stays.
    std::fs::remove_dir_all(root.join("web-worktrees")).unwrap();
    h.restart();
    for id in [&live.id, &ended.id] {
        eventually("the removed worktree forgotten", || {
            (place_paths(&h, id) == [kept.display().to_string()]).then_some(())
        });
    }
    let stored: i64 = h
        .db()
        .query_row(
            "SELECT COUNT(*) FROM session_places WHERE path = ?1",
            [merged.display().to_string()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(stored, 0);
}
