//! Pi's conversations and how to resume one. Pi keeps each conversation in a
//! file of its own, `<agent dir>/sessions/--<folder>--/<time>_<id>.jsonl`
//! (`PI_CODING_AGENT_SESSION_DIR` or the `sessionDir` setting put every
//! folder's in one directory instead). Its process hides its arguments
//! (`process.title`), so neither the conversation nor the launch flags can be
//! read from it: the conversation is the one the session bound (the Rakumux
//! extension binds it), else the folder's newest, as Pi's own `--continue`
//! picks. Resuming opens that file (`pi --session <file>`), which works from
//! any folder and brings back its model and thinking level; other launch
//! flags are lost.
use super::*;

/// The most folder directories searched for a conversation bound in another
/// folder.
const MAX_FOLDERS: usize = 4096;
/// The most files of one directory looked at.
const MAX_FILES: usize = 16_384;

/// Pi's agent directory: `$PI_CODING_AGENT_DIR` (`~` expanded), else
/// `~/.pi/agent`.
pub(crate) fn pi_agent_dir() -> Option<PathBuf> {
    match std::env::var_os("PI_CODING_AGENT_DIR").filter(|dir| !dir.is_empty()) {
        Some(dir) => expand_home(Path::new(&dir)),
        None => home_dir().map(|home| home.join(".pi/agent")),
    }
}

/// `~` and `~/…` as the home directory's.
fn expand_home(path: &Path) -> Option<PathBuf> {
    match path.strip_prefix("~") {
        Ok(rest) => Some(home_dir()?.join(rest)),
        Err(_) => Some(path.to_path_buf()),
    }
}

/// Where Pi keeps its conversations.
#[derive(Debug, PartialEq, Eq)]
enum Store {
    /// `<agent dir>/sessions`, a directory per folder.
    PerFolder(PathBuf),
    /// One directory for every folder's (`PI_CODING_AGENT_SESSION_DIR`, the
    /// `sessionDir` setting): a file's folder is its header's `cwd`.
    Shared(PathBuf),
}

/// Pi's order: `PI_CODING_AGENT_SESSION_DIR`, then the user's `sessionDir`
/// setting, then the agent directory's `sessions`. A `--session-dir` on Pi's
/// command line can't be seen.
fn store(cwd: Option<&Path>) -> Option<Store> {
    let env = std::env::var_os("PI_CODING_AGENT_SESSION_DIR")
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from);
    store_from(env, &pi_agent_dir()?, cwd)
}

/// [`store`] for the session directory variable `env` and agent directory
/// `agent`. A relative `sessionDir` setting is the folder's.
fn store_from(env: Option<PathBuf>, agent: &Path, cwd: Option<&Path>) -> Option<Store> {
    if let Some(dir) = env {
        return Some(Store::Shared(expand_home(&dir)?));
    }
    let setting = std::fs::read(agent.join("settings.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        .and_then(|settings| Some(settings.get("sessionDir")?.as_str()?.trim().to_owned()))
        .filter(|dir| !dir.is_empty());
    if let Some(dir) = setting {
        let dir = expand_home(Path::new(&dir))?;
        let dir = if dir.is_absolute() {
            dir
        } else {
            cwd?.join(dir)
        };
        return Some(Store::Shared(dir));
    }
    Some(Store::PerFolder(agent.join("sessions")))
}

/// The directory name Pi gives `cwd`'s conversations:
/// `--home-me-repo--` for `/home/me/repo`.
fn folder_name(cwd: &Path) -> Option<String> {
    let cwd = cwd.to_str()?;
    let trimmed = cwd.strip_prefix(['/', '\\']).unwrap_or(cwd);
    Some(format!("--{}--", trimmed.replace(['/', '\\', ':'], "-")))
}

/// A conversation file's header: its id and folder.
fn header(path: &Path) -> Option<(String, Option<PathBuf>)> {
    let mut line = Vec::new();
    std::fs::File::open(path)
        .ok()?
        .take(64 * 1024)
        .read_to_end(&mut line)
        .ok()?;
    let end = line.iter().position(|b| *b == b'\n').unwrap_or(line.len());
    let header: serde_json::Value = serde_json::from_slice(&line[..end]).ok()?;
    if header.get("type")?.as_str()? != "session" {
        return None;
    }
    let id = header.get("id")?.as_str()?.to_owned();
    let cwd = header
        .get("cwd")
        .and_then(|cwd| cwd.as_str())
        .map(PathBuf::from);
    Some((id, cwd))
}

/// The `*.jsonl` files of `dir`, with when each was last written.
fn conversations(dir: &Path) -> Vec<(std::time::SystemTime, PathBuf)> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .filter_map(Result::ok)
        .take(MAX_FILES)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "jsonl"))
        .filter_map(|path| Some((std::fs::metadata(&path).ok()?.modified().ok()?, path)))
        .collect()
}

/// The file of conversation `id` in `dir`, by its name.
fn named(dir: &Path, id: &str) -> Option<PathBuf> {
    let suffix = format!("_{id}.jsonl");
    std::fs::read_dir(dir)
        .ok()?
        .filter_map(Result::ok)
        .take(MAX_FILES)
        .map(|entry| entry.path())
        .find(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.ends_with(&suffix))
        })
}

/// The file of conversation `id`: in `cwd`'s directory first, then in any
/// folder's (a conversation resumed from another folder stays in its own).
fn session_file_in(store: &Store, cwd: Option<&Path>, id: &str) -> Option<PathBuf> {
    if !is_conversation_id(id) {
        return None;
    }
    match store {
        Store::Shared(dir) => named(dir, id),
        Store::PerFolder(root) => {
            let own = cwd.and_then(folder_name).map(|name| root.join(name));
            if let Some(found) = own.as_deref().and_then(|dir| named(dir, id)) {
                return Some(found);
            }
            std::fs::read_dir(root)
                .ok()?
                .filter_map(Result::ok)
                .take(MAX_FOLDERS)
                .map(|entry| entry.path())
                .filter(|dir| Some(dir) != own.as_ref())
                .find_map(|dir| named(&dir, id))
        }
    }
}

/// The folder `cwd`'s newest conversation (Pi's `--continue`), and its id.
fn recent_in(store: &Store, cwd: &Path) -> Option<(String, PathBuf)> {
    let (dir, shared) = match store {
        Store::Shared(dir) => (dir.clone(), true),
        Store::PerFolder(root) => (root.join(folder_name(cwd)?), false),
    };
    let mut files = conversations(&dir);
    files.sort_by(|a, b| b.0.cmp(&a.0));
    files.into_iter().find_map(|(_, path)| {
        let (id, folder) = header(&path)?;
        let ours = !shared || folder.as_deref() == Some(cwd);
        (ours && is_conversation_id(&id)).then_some((id, path))
    })
}

/// The conversation a Pi process in `cwd` shows: the one bound, once its file
/// exists, else the folder's newest. Its id and file.
pub(crate) fn pi_conversation(
    cwd: Option<&Path>,
    bound: Option<&str>,
) -> Option<(String, PathBuf)> {
    if bound.is_none() && cwd.is_none() {
        return None;
    }
    conversation_in(&store(cwd)?, cwd, bound)
}

fn conversation_in(
    store: &Store,
    cwd: Option<&Path>,
    bound: Option<&str>,
) -> Option<(String, PathBuf)> {
    match bound {
        Some(id) => Some((id.to_owned(), session_file_in(store, cwd, id)?)),
        None => recent_in(store, cwd?),
    }
}

/// `pi --session <file>`: the file, not the id, so Pi neither asks to fork a
/// conversation of another folder nor misses one in a custom directory.
pub(super) fn pi_resume(cwd: Option<&Path>, bound: Option<&str>) -> Option<Vec<String>> {
    resume_in(&store(cwd)?, cwd, bound)
}

fn resume_in(store: &Store, cwd: Option<&Path>, bound: Option<&str>) -> Option<Vec<String>> {
    let (_, file) = conversation_in(store, cwd, bound)?;
    let file = file.to_str()?;
    // Typed at a shell prompt on revive: nothing a shell would read as more.
    if file.chars().any(|c| c.is_control()) {
        return None;
    }
    Some(vec!["pi".into(), "--session".into(), file.into()])
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: &str = "01a18a95-6f2e-7c3b-9a1d-2cd72115b9d0";

    /// A conversation file with a header naming `cwd`.
    fn write(dir: &Path, name: &str, id: &str, cwd: &str) -> PathBuf {
        std::fs::create_dir_all(dir).unwrap();
        let path = dir.join(name);
        let header = serde_json::json!({"type": "session", "version": 3, "id": id,
            "timestamp": "2026-10-30T02:13:49.215Z", "cwd": cwd});
        std::fs::write(&path, format!("{header}\n")).unwrap();
        path
    }

    fn touch_later(path: &Path) {
        std::thread::sleep(std::time::Duration::from_millis(20));
        let line = b"{\"type\":\"label\"}\n";
        let mut file = std::fs::OpenOptions::new().append(true).open(path).unwrap();
        std::io::Write::write_all(&mut file, line).unwrap();
    }

    #[test]
    fn a_folders_directory_is_named_as_pi_names_it() {
        assert_eq!(
            folder_name(Path::new("/home/me/repos/rakumux")).as_deref(),
            Some("--home-me-repos-rakumux--")
        );
        assert_eq!(folder_name(Path::new("/")).as_deref(), Some("----"));
    }

    #[test]
    fn the_bound_conversation_resumes_by_its_file_from_any_folder() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::PerFolder(root.path().join("sessions"));
        let repo = Path::new("/home/me/repo");
        let own = root.path().join("sessions/--home-me-repo--");
        let other = root.path().join("sessions/--home-me-other--");
        let mine = write(
            &own,
            &format!("2026-10-30T02-13-49-215Z_{ID}.jsonl"),
            ID,
            "/home/me/repo",
        );
        let elsewhere = write(
            &other,
            "2026-10-29T01-00-00-000Z_older-conversation.jsonl",
            "older-conversation",
            "/home/me/other",
        );
        assert_eq!(
            resume_in(&store, Some(repo), Some(ID)),
            Some(vec![
                "pi".into(),
                "--session".into(),
                mine.to_str().unwrap().into()
            ])
        );
        // Resumed from another folder, it stays in its own.
        assert_eq!(
            conversation_in(&store, Some(repo), Some("older-conversation")),
            Some(("older-conversation".into(), elsewhere))
        );
        // Bound, but no file yet (a new conversation before its first prompt).
        assert_eq!(conversation_in(&store, Some(repo), Some("brand-new")), None);
        // An id that isn't one.
        assert_eq!(
            conversation_in(&store, Some(repo), Some("a b; rm -rf ~")),
            None
        );
    }

    #[test]
    fn unbound_the_folders_newest_conversation_is_resumed() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::PerFolder(root.path().join("sessions"));
        let repo = Path::new("/home/me/repo");
        let dir = root.path().join("sessions/--home-me-repo--");
        assert_eq!(resume_in(&store, Some(repo), None), None, "nothing yet");
        let old = write(
            &dir,
            "2026-10-29T00-00-00-000Z_c-old.jsonl",
            "c-old",
            "/home/me/repo",
        );
        let new = write(
            &dir,
            "2026-10-30T00-00-00-000Z_c-new.jsonl",
            "c-new",
            "/home/me/repo",
        );
        assert_eq!(
            conversation_in(&store, Some(repo), None),
            Some(("c-new".into(), new))
        );
        // Written to since: the newest is the last written, as Pi's `--continue` picks.
        touch_later(&old);
        assert_eq!(
            conversation_in(&store, Some(repo), None),
            Some(("c-old".into(), old))
        );
        // Without a folder, unbound: nothing to look for.
        assert_eq!(conversation_in(&store, None, None), None);
    }

    #[test]
    fn a_shared_directory_holds_every_folders_conversations() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("all");
        let store = Store::Shared(dir.clone());
        let mine = write(
            &dir,
            "2026-10-29T00-00-00-000Z_mine.jsonl",
            "mine",
            "/home/me/repo",
        );
        let theirs = write(
            &dir,
            "2026-10-30T00-00-00-000Z_theirs.jsonl",
            "theirs",
            "/home/me/other",
        );
        touch_later(&theirs);
        assert_eq!(
            conversation_in(&store, Some(Path::new("/home/me/repo")), None),
            Some(("mine".into(), mine))
        );
        assert_eq!(
            conversation_in(&store, Some(Path::new("/home/me/repo")), Some("theirs")),
            Some(("theirs".into(), theirs))
        );
    }

    #[test]
    fn the_session_directory_comes_from_the_variable_then_the_setting() {
        let agent = tempfile::tempdir().unwrap();
        let cwd = Path::new("/home/me/repo");
        assert_eq!(
            store_from(None, agent.path(), Some(cwd)),
            Some(Store::PerFolder(agent.path().join("sessions")))
        );
        std::fs::write(
            agent.path().join("settings.json"),
            r#"{"sessionDir": "/srv/pi-sessions"}"#,
        )
        .unwrap();
        assert_eq!(
            store_from(None, agent.path(), Some(cwd)),
            Some(Store::Shared("/srv/pi-sessions".into()))
        );
        std::fs::write(
            agent.path().join("settings.json"),
            r#"{"sessionDir": ".pi/s"}"#,
        )
        .unwrap();
        assert_eq!(
            store_from(None, agent.path(), Some(cwd)),
            Some(Store::Shared("/home/me/repo/.pi/s".into()))
        );
        assert_eq!(
            store_from(Some("/env/dir".into()), agent.path(), Some(cwd)),
            Some(Store::Shared("/env/dir".into()))
        );
    }

    #[test]
    fn pi_resumes_by_file_whatever_it_was_started_with() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::PerFolder(root.path().join("sessions"));
        let dir = root.path().join("sessions/--w--");
        let file = write(
            &dir,
            &format!("2026-10-30T02-13-49-215Z_{ID}.jsonl"),
            ID,
            "/w",
        );
        // Pi rewrites its process title, so its arguments are gone: `pi` and
        // `node …/cli.js --model x` resume alike.
        for (name, cmdline) in [
            ("pi", vec!["pi"]),
            (
                "node",
                vec![
                    "node",
                    "/n/lib/node_modules/@earendil-works/pi-coding-agent/dist/cli.js",
                ],
            ),
        ] {
            let process = ProcessInfo {
                pid: 7,
                name: name.into(),
                cmdline: cmdline.iter().map(|w| w.to_string()).collect(),
                exe: None,
            };
            assert_eq!(crate::detect::agent_label(&process), Some("pi"));
        }
        assert_eq!(
            resume_in(&store, Some(Path::new("/w")), Some(ID)),
            Some(vec![
                "pi".into(),
                "--session".into(),
                file.to_str().unwrap().into()
            ])
        );
    }
}
