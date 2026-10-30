//! Kimi Code's conversation for a process, and its resume command.
//!
//! Kimi Code 2.x keeps each conversation under its home
//! (`$KIMI_CODE_HOME`, else `~/.kimi-code`) in
//! `sessions/wd_<slug>_<hash>/<session id>/`, the folder's key being its
//! name's slug and a hash of its path, with `state.json` (its `cwd`,
//! `updatedAt`, `archived`) and the main agent's log,
//! `agents/main/wire.jsonl`. Which one a process runs, in order:
//! - the conversation the session named for Kimi (its plugin's hooks report
//!   it, or a `session_bind`);
//! - the one it holds a lock on: `locks/sessions/<session id>.lock` names
//!   the pid while a turn runs (or a process it started: a staged update
//!   runs the new binary as a child);
//! - the folder's newest conversation that isn't archived.
//!
//! Resume is `kimi -r <session id>`, as Kimi prints on exit. The process
//! wipes its arguments, so none are kept: the conversation keeps its
//! permission mode, plan mode, model and agent.
use super::*;

/// Kimi Code's home: `$KIMI_CODE_HOME`, else `~/.kimi-code`.
pub(crate) fn kimi_home() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("KIMI_CODE_HOME").filter(|dir| !dir.is_empty()) {
        return Some(PathBuf::from(dir));
    }
    home_dir().map(|home| home.join(".kimi-code"))
}

pub(super) fn kimi_resume(
    process: &ProcessInfo,
    cwd: Option<&Path>,
    bound: Option<&str>,
) -> Option<Vec<String>> {
    let id = kimi_conversation(&kimi_home()?, process.pid, cwd, bound)?;
    Some(vec!["kimi".into(), "-r".into(), id])
}

/// The conversation the Kimi Code process `pid`, working in `cwd`, runs:
/// the one the session named (`bound`), else the one it holds a lock on,
/// else the folder's newest.
pub(crate) fn kimi_conversation(
    home: &Path,
    pid: u32,
    cwd: Option<&Path>,
    bound: Option<&str>,
) -> Option<String> {
    let id = match bound {
        Some(id) => id.to_owned(),
        None => kimi_locked_session(home, pid)
            .or_else(|| cwd.and_then(|cwd| kimi_recent_session(home, cwd)))?,
    };
    is_conversation_id(&id).then_some(id)
}

/// The conversation `pid` (or a process it started) holds a lock on: it
/// does while a turn runs.
pub(crate) fn kimi_locked_session(home: &Path, pid: u32) -> Option<String> {
    let locks = std::fs::read_dir(home.join("locks/sessions")).ok()?;
    let mut pids = vec![pid];
    let mut descendants = None;
    for entry in locks.flatten() {
        let path = entry.path();
        let Some(id) = path
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(|name| name.strip_suffix(".lock"))
            .filter(|id| is_conversation_id(id))
        else {
            continue;
        };
        let Some(holder) = read_small_json(&path)
            .and_then(|lock| lock.get("pid")?.as_u64())
            .and_then(|pid| u32::try_from(pid).ok())
        else {
            continue;
        };
        if descendants.is_none() && holder != pid {
            pids.extend(crate::procinfo::descendants(pid));
            descendants = Some(());
        }
        if pids.contains(&holder) {
            return Some(id.to_owned());
        }
    }
    None
}

/// The folder `cwd`'s newest conversation that isn't archived.
pub(crate) fn kimi_recent_session(home: &Path, cwd: &Path) -> Option<String> {
    let mut newest: Option<(i64, String)> = None;
    for folder in kimi_folder_dirs(home, cwd) {
        let Ok(sessions) = std::fs::read_dir(&folder) else {
            continue;
        };
        for entry in sessions.flatten() {
            let Some(id) = entry
                .file_name()
                .to_str()
                .filter(|id| is_conversation_id(id))
                .map(String::from)
            else {
                continue;
            };
            let Some(state) = read_small_json(&entry.path().join("state.json")) else {
                continue;
            };
            let same_folder = state
                .get("cwd")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|dir| Path::new(dir) == cwd);
            let archived = state.get("archived").and_then(serde_json::Value::as_bool);
            let updated = state.get("updatedAt").and_then(serde_json::Value::as_i64);
            if let (true, Some(false) | None, Some(updated)) = (same_folder, archived, updated) {
                if newest.as_ref().is_none_or(|(at, _)| updated > *at) {
                    newest = Some((updated, id));
                }
            }
        }
    }
    newest.map(|(_, id)| id)
}

/// The main agent's log of the conversation `id`.
pub(crate) fn kimi_wire_log(home: &Path, id: &str) -> Option<PathBuf> {
    if !is_conversation_id(id) {
        return None;
    }
    std::fs::read_dir(home.join("sessions"))
        .ok()?
        .flatten()
        .map(|folder| folder.path().join(id).join("agents/main/wire.jsonl"))
        .find(|path| path.is_file())
}

/// The folders Kimi keeps `cwd`'s conversations in: `wd_<slug>_<hash>`.
/// The hash (of the path) isn't recomputed: every folder of the slug is
/// one, and its conversations' `cwd` says which is `cwd`'s.
fn kimi_folder_dirs(home: &Path, cwd: &Path) -> Vec<PathBuf> {
    let prefix = format!("wd_{}_", kimi_slug(cwd));
    let Ok(folders) = std::fs::read_dir(home.join("sessions")) else {
        return Vec::new();
    };
    folders
        .flatten()
        .filter(|folder| {
            folder
                .file_name()
                .to_str()
                .and_then(|name| name.strip_prefix(&prefix))
                .is_some_and(|hash| hash.len() == 12 && hash.bytes().all(|b| b.is_ascii_hexdigit()))
        })
        .map(|folder| folder.path())
        .collect()
}

/// Kimi's slug of a folder's name (`slugifyWorkDirName`): lowercase, runs
/// of anything but `a-z0-9._-` as `-`, no `-` at either end, at most 40
/// characters; `workspace` when that leaves nothing.
pub(crate) fn kimi_slug(cwd: &Path) -> String {
    let normalized = cwd.to_string_lossy().replace('\\', "/");
    let normalized = normalized.trim_end_matches('/');
    let name = normalized.rsplit('/').next().unwrap_or(normalized);
    let mut slug = String::new();
    let mut replaced = false;
    for c in name.to_lowercase().chars() {
        if c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | '-') {
            slug.push(c);
            replaced = false;
        } else if !replaced {
            slug.push('-');
            replaced = true;
        }
    }
    let slug: String = slug.trim_matches('-').chars().take(40).collect();
    let slug = slug.trim_matches('-');
    match slug {
        "" | "." | ".." => "workspace".into(),
        slug => slug.into(),
    }
}

/// A small JSON file (a lock, a conversation's state).
fn read_small_json(path: &Path) -> Option<serde_json::Value> {
    let mut text = String::new();
    std::fs::File::open(path)
        .ok()?
        .take(64 * 1024)
        .read_to_string(&mut text)
        .ok()?;
    serde_json::from_str(&text).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn kimi(pid: u32, name: &str) -> ProcessInfo {
        ProcessInfo {
            pid,
            name: name.into(),
            cmdline: vec![name.into()],
            exe: Some("/home/me/.kimi-code/bin/kimi".into()),
        }
    }

    /// `<home>/sessions/<folder>/<id>/state.json`.
    fn conversation(home: &Path, folder: &str, id: &str, cwd: &str, updated: i64, archived: bool) {
        let dir = home.join("sessions").join(folder).join(id);
        std::fs::create_dir_all(dir.join("agents/main")).unwrap();
        std::fs::write(
            dir.join("state.json"),
            serde_json::json!({"id": id, "version": 2, "cwd": cwd, "archived": archived,
                "createdAt": 1, "updatedAt": updated})
            .to_string(),
        )
        .unwrap();
        std::fs::write(dir.join("agents/main/wire.jsonl"), "").unwrap();
    }

    #[test]
    fn folders_are_slugged_as_kimi_does() {
        // From Kimi Code 2.4.0's `encodeWorkDirKey`, run by Node.
        for (cwd, slug) in [
            ("/home/me/src/project", "project"),
            ("/home/me/My Project!/", "my-project"),
            ("/w/a- b", "a--b"),
            ("/w/---", "workspace"),
            ("/w/Ünïcode Dir", "n-code-dir"),
            ("/", "workspace"),
            ("/w/..", "workspace"),
            (
                "/w/xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx-yz",
                "xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx",
            ),
            (
                "/w/Long_Name.Long_Name.Long_Name.Long_Name.Long_Name.",
                "long_name.long_name.long_name.long_name.",
            ),
        ] {
            assert_eq!(kimi_slug(Path::new(cwd)), slug, "{cwd}");
        }
    }

    #[test]
    fn the_folders_newest_conversation_that_isnt_archived() {
        let home = tempfile::tempdir().unwrap();
        let home = home.path();
        let cwd = "/home/me/src/project";
        conversation(
            home,
            "wd_project_0217ab432c5d",
            "session_old",
            cwd,
            100,
            false,
        );
        conversation(
            home,
            "wd_project_0217ab432c5d",
            "session_new",
            cwd,
            200,
            false,
        );
        conversation(
            home,
            "wd_project_0217ab432c5d",
            "session_gone",
            cwd,
            300,
            true,
        );
        // Another folder of the same name: another hash.
        conversation(
            home,
            "wd_project_aaaaaaaaaaaa",
            "session_elsewhere",
            "/srv/project",
            400,
            false,
        );
        // Not a folder of Kimi's.
        conversation(home, "wd_project_notahash", "session_odd", cwd, 500, false);
        assert_eq!(
            kimi_recent_session(home, Path::new(cwd)).as_deref(),
            Some("session_new")
        );
        assert_eq!(
            kimi_recent_session(home, Path::new("/srv/project")).as_deref(),
            Some("session_elsewhere")
        );
        assert_eq!(kimi_recent_session(home, Path::new("/home/me/other")), None);
        assert_eq!(
            kimi_wire_log(home, "session_new"),
            Some(home.join("sessions/wd_project_0217ab432c5d/session_new/agents/main/wire.jsonl"))
        );
        assert_eq!(kimi_wire_log(home, "session_missing"), None);
        assert_eq!(kimi_wire_log(home, "../session_new"), None);
    }

    #[test]
    fn a_lock_names_the_conversation_its_process_runs_a_turn_in() {
        let home = tempfile::tempdir().unwrap();
        let home = home.path();
        let locks = home.join("locks/sessions");
        std::fs::create_dir_all(&locks).unwrap();
        let me = std::process::id();
        let lock = |id: &str, pid: u32| {
            std::fs::write(
                locks.join(format!("{id}.lock")),
                serde_json::json!({"version": 1, "token": format!("{pid}:x"), "pid": pid,
                    "hostname": "box", "clientIdentity": "kimi_code_cli"})
                .to_string(),
            )
            .unwrap();
        };
        assert_eq!(kimi_locked_session(home, me), None);
        lock("session_other", 1);
        std::fs::write(locks.join("session_bad.lock"), "not json").unwrap();
        assert_eq!(kimi_locked_session(home, me), None);
        lock("session_mine", me);
        assert_eq!(
            kimi_locked_session(home, me).as_deref(),
            Some("session_mine")
        );
        // Bound beats locked beats the folder's newest.
        conversation(
            home,
            "wd_project_0217ab432c5d",
            "session_new",
            "/w/project",
            1,
            false,
        );
        let cwd = Some(Path::new("/w/project"));
        assert_eq!(
            kimi_conversation(home, me, cwd, Some("session_bound")).as_deref(),
            Some("session_bound")
        );
        assert_eq!(
            kimi_conversation(home, me, cwd, None).as_deref(),
            Some("session_mine")
        );
        std::fs::remove_file(locks.join("session_mine.lock")).unwrap();
        assert_eq!(
            kimi_conversation(home, me, cwd, None).as_deref(),
            Some("session_new")
        );
        assert_eq!(kimi_conversation(home, me, cwd, Some("bad id")), None);
    }

    #[test]
    fn kimi_resumes_its_conversation_without_its_wiped_arguments() {
        for name in ["kimi", "kimi-co", "kimi-code"] {
            let process = kimi(42, name);
            assert_eq!(
                plan(&Context {
                    process: &process,
                    cwd: None,
                    bound_conversation: Some("session_9e550655-6a9f-4f15-b633-3839522633c0"),
                }),
                Plan::Resume(vec![
                    "kimi".into(),
                    "-r".into(),
                    "session_9e550655-6a9f-4f15-b633-3839522633c0".into()
                ]),
                "{name}"
            );
        }
    }
}
