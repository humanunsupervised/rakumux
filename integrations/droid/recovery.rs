//! Droid (Factory) conversation discovery and interactive resume flags.
//!
//! Droid keeps each conversation in `<home>/.factory/sessions/<folder
//! key>/<id>.jsonl` (`<home>` is `$FACTORY_HOME_OVERRIDE`, else `$HOME`), its
//! first line the session's summary (`session_start`, with `callingSessionId`
//! when a Task subagent runs it). No file names the process: the conversation
//! is the one its hooks report, else the folder's newest top-level one.
use super::*;

const DROID_SWITCHES: &[&str] = &["--disable-builtin-skills"];
const DROID_OPTIONS: &[&str] = &[
    "--auto",
    "--settings",
    "--append-system-prompt",
    "--append-system-prompt-file",
    "--cwd",
];

/// Droid's commands that aren't its interactive agent (`resume` is).
const DROID_COMMANDS: &[&str] = &[
    "exec", "daemon", "search", "find", "update", "mcp", "plugin", "computer", "doctor", "help",
];
/// Interactive options that take a value, skipped when looking for a command.
const DROID_VALUES: &[&str] = &[
    "--settings",
    "--append-system-prompt",
    "--append-system-prompt-file",
    "--cwd",
    "--auto",
    "--fork",
    "--worktree-dir",
];

/// The most session files of a folder looked at for its newest one.
const MAX_FOLDER_SESSIONS: usize = 64;
/// The most of a session file's first line read.
const MAX_SUMMARY_LINE: u64 = 1024 * 1024;

/// Droid's data directory: `$FACTORY_HOME_OVERRIDE/.factory`, else `~/.factory`.
pub(crate) fn droid_home() -> Option<PathBuf> {
    factory_dir(std::env::var_os("FACTORY_HOME_OVERRIDE"), home_dir())
}

/// `.factory` in the home Droid is told to use, else in `home`.
fn factory_dir(
    home_override: Option<std::ffi::OsString>,
    home: Option<PathBuf>,
) -> Option<PathBuf> {
    home_override
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from)
        .or(home)
        .map(|home| home.join(".factory"))
}

/// The directory of `cwd`'s sessions under `sessions/`: its real path with
/// every run of `/` a `-` (`/home/me/proj` → `-home-me-proj`).
pub(crate) fn droid_project_key(cwd: &Path) -> String {
    let real = std::fs::canonicalize(cwd).unwrap_or_else(|_| cwd.to_path_buf());
    let path = real.to_string_lossy();
    let trimmed = path.trim_end_matches('/').trim_start_matches('/');
    let mut key = String::from("-");
    let mut slash = false;
    for c in trimmed.chars() {
        if c == '/' {
            if !slash {
                key.push('-');
            }
            slash = true;
        } else {
            key.push(c);
            slash = false;
        }
    }
    key
}

/// Conversation `id`'s file under `home`: `sessions/<folder>/<id>.jsonl` (the
/// most recently written, should it be in two), or `sessions/<id>.jsonl` for
/// one started without a folder.
pub(crate) fn droid_session_file(home: &Path, id: &str) -> Option<PathBuf> {
    if !is_conversation_id(id) {
        return None;
    }
    let sessions = home.join("sessions");
    let name = format!("{id}.jsonl");
    std::fs::read_dir(&sessions)
        .ok()?
        .filter_map(Result::ok)
        .take(4096)
        .map(|entry| entry.path().join(&name))
        .chain([sessions.join(&name)])
        .filter_map(|path| {
            let modified = std::fs::metadata(&path).ok()?.modified().ok()?;
            Some((modified, path))
        })
        .max_by_key(|(modified, _)| *modified)
        .map(|(_, path)| path)
}

/// The newest conversation of the folder `cwd` that a user runs: not a
/// subagent's (its summary names the session that called it).
pub(crate) fn droid_recent_session(home: &Path, cwd: &Path) -> Option<String> {
    let dir = home.join("sessions").join(droid_project_key(cwd));
    let mut files: Vec<(std::time::SystemTime, PathBuf)> = std::fs::read_dir(dir)
        .ok()?
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let path = entry.path();
            let stem = path.file_stem()?.to_str()?;
            let session =
                path.extension().is_some_and(|ext| ext == "jsonl") && is_conversation_id(stem);
            session.then(|| Some((entry.metadata().ok()?.modified().ok()?, path.clone())))?
        })
        .collect();
    files.sort_by(|a, b| b.0.cmp(&a.0));
    files
        .into_iter()
        .take(MAX_FOLDER_SESSIONS)
        .find(|(_, path)| droid_summary(path).is_some_and(|summary| summary.top_level()))
        .and_then(|(_, path)| Some(path.file_stem()?.to_str()?.to_owned()))
}

/// A session file's first line: the session's summary.
#[derive(Deserialize)]
struct DroidSummary {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default, rename = "callingSessionId")]
    calling_session_id: Option<String>,
}

impl DroidSummary {
    /// A session the user runs, not a subagent's.
    fn top_level(&self) -> bool {
        self.kind == "session_start"
            && self
                .calling_session_id
                .as_deref()
                .is_none_or(|caller| caller.is_empty())
    }
}

/// The summary on `path`'s first line.
fn droid_summary(path: &Path) -> Option<DroidSummary> {
    use std::io::BufRead;
    let file = std::fs::File::open(path).ok()?;
    let mut line = String::new();
    std::io::BufReader::new(file)
        .take(MAX_SUMMARY_LINE)
        .read_line(&mut line)
        .ok()?;
    serde_json::from_str(line.trim_end()).ok()
}

/// Whether `process` runs Droid's interactive agent, not one of its other
/// commands (`droid exec`, `droid mcp …`), which are never resumed.
pub(super) fn droid_is_interactive(process: &ProcessInfo) -> bool {
    let args = program_args(&process.cmdline, "droid");
    let mut words = args.iter().map(String::as_str);
    while let Some(word) = words.next() {
        match word {
            "--" => return true,
            value if DROID_VALUES.contains(&value) => {
                words.next();
            }
            flag if flag.starts_with('-') => {}
            command => return !DROID_COMMANDS.contains(&command),
        }
    }
    true
}

/// `droid <kept flags> --resume <id>`: the conversation the session's hooks
/// named, else the folder's newest one Droid's user runs.
pub(super) fn droid_resume(
    process: &ProcessInfo,
    cwd: Option<&Path>,
    bound: Option<&str>,
) -> Option<Vec<String>> {
    let id = match bound.filter(|id| is_conversation_id(id)) {
        Some(id) => id.to_owned(),
        None => droid_recent_session(&droid_home()?, cwd?)?,
    };
    let mut argv = vec!["droid".to_string()];
    argv.extend(keep_flags(
        &program_args(&process.cmdline, "droid"),
        DROID_SWITCHES,
        DROID_OPTIONS,
        &[],
    ));
    argv.push("--resume".into());
    argv.push(id);
    Some(argv)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(words: &[&str]) -> Vec<String> {
        words.iter().map(|w| w.to_string()).collect()
    }

    fn droid(cmdline: &[&str]) -> ProcessInfo {
        ProcessInfo {
            pid: 7,
            name: "droid".into(),
            cmdline: args(cmdline),
            ..Default::default()
        }
    }

    const ID: &str = "0f5c6d1e-8a2b-4c3d-9e4f-5a6b7c8d9e0f";

    fn write(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    #[test]
    fn folder_keys_are_the_real_path_with_dashes() {
        assert_eq!(
            droid_project_key(Path::new("/nowhere/me/proj")),
            "-nowhere-me-proj"
        );
        assert_eq!(
            droid_project_key(Path::new("/nowhere/me/my.app//")),
            "-nowhere-me-my.app"
        );
        assert_eq!(droid_project_key(Path::new("/nowhere//a")), "-nowhere-a");
        let real = tempfile::tempdir().unwrap();
        let link = tempfile::tempdir().unwrap();
        let alias = link.path().join("alias");
        std::os::unix::fs::symlink(real.path(), &alias).unwrap();
        assert_eq!(droid_project_key(&alias), droid_project_key(real.path()));
    }

    #[test]
    fn the_resume_command_keeps_the_flags_that_shape_a_session() {
        let resume = |cmdline: &[&str]| droid_resume(&droid(cmdline), None, Some(ID));
        assert_eq!(
            resume(&[
                "/home/me/.local/bin/droid",
                "--auto",
                "high",
                "--settings",
                "/s.json",
                "--append-system-prompt",
                "be brief",
                "--disable-builtin-skills",
                "--use-spec",
                "-w",
                "feature",
                "--resume",
                "old",
                "fix the bug",
            ]),
            Some(args(&[
                "droid",
                "--auto",
                "high",
                "--settings",
                "/s.json",
                "--append-system-prompt",
                "be brief",
                "--disable-builtin-skills",
                "--resume",
                ID
            ]))
        );
        assert_eq!(
            resume(&["droid", "resume", "old", "--cwd", "/repo", "--last"]),
            Some(args(&["droid", "--cwd", "/repo", "--resume", ID]))
        );
        // The npm package's shim runs the binary under node.
        assert_eq!(
            resume(&[
                "node",
                "/usr/lib/node_modules/droid/bin/droid",
                "--auto=low"
            ]),
            Some(args(&["droid", "--auto=low", "--resume", ID]))
        );
        // A conversation id that isn't one is no command.
        assert_eq!(
            droid_resume(&droid(&["droid"]), None, Some("x; rm -rf ~")),
            None
        );
    }

    #[test]
    fn droids_home_can_be_moved() {
        let home = Some(PathBuf::from("/home/me"));
        assert_eq!(
            factory_dir(None, home.clone()),
            Some(PathBuf::from("/home/me/.factory"))
        );
        assert_eq!(
            factory_dir(Some("/scratch/fhome".into()), home.clone()),
            Some(PathBuf::from("/scratch/fhome/.factory"))
        );
        assert_eq!(
            factory_dir(Some("".into()), home),
            Some(PathBuf::from("/home/me/.factory"))
        );
        assert_eq!(factory_dir(None, None), None);
    }

    #[test]
    fn droids_commands_are_typed_and_its_agent_resumed() {
        let plan_for = |cmdline: &[&str], bound| {
            plan(&Context {
                process: &droid(cmdline),
                cwd: None,
                bound_conversation: bound,
            })
        };
        assert_eq!(
            plan_for(&["droid", "--auto", "high"], Some(ID)),
            Plan::Resume(args(&["droid", "--auto", "high", "--resume", ID]))
        );
        for command in [
            &["droid", "exec", "--auto", "high", "fix it"][..],
            &["droid", "daemon", "--remote-control"],
            &["droid", "mcp", "list"],
        ] {
            assert_eq!(
                plan_for(command, Some(ID)),
                Plan::Command(args(command)),
                "{command:?}"
            );
        }
        // No conversation yet (no prompt, no hooks): keep what's recorded.
        assert_eq!(plan_for(&["droid"], None), Plan::Unresolved);
    }

    #[test]
    fn droids_other_commands_are_not_its_agent() {
        for cmdline in [
            &["droid"][..],
            &["droid", "fix the bug"],
            &["droid", "--auto", "high", "exec the plan"],
            &["droid", "resume", ID],
            &["droid", "-r", ID],
            &["droid", "--cwd", "mcp"],
        ] {
            assert!(droid_is_interactive(&droid(cmdline)), "{cmdline:?}");
        }
        for cmdline in [
            &["droid", "exec", "--auto", "high", "fix it"][..],
            &["droid", "daemon", "--remote-control"],
            &["droid", "mcp", "add", "rakumux", "rakumux-mcp"],
            &["droid", "update"],
            &["droid", "--auto", "low", "exec", "x"],
            &[
                "node",
                "/usr/lib/node_modules/droid/bin/droid",
                "plugin",
                "list",
            ],
        ] {
            assert!(!droid_is_interactive(&droid(cmdline)), "{cmdline:?}");
        }
    }

    #[test]
    fn a_folders_newest_conversation_is_its_users_not_a_subagents() {
        let home = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        let folder = home
            .path()
            .join("sessions")
            .join(droid_project_key(work.path()));
        let summary = |id: &str, extra: &str| {
            format!("{{\"type\":\"session_start\",\"id\":\"{id}\",\"title\":\"t\",\"version\":2{extra}}}\n")
        };
        let older = "11111111-1111-4111-8111-111111111111";
        let child = "22222222-2222-4222-8222-222222222222";
        write(&folder.join(format!("{older}.jsonl")), &summary(older, ""));
        std::thread::sleep(std::time::Duration::from_millis(20));
        write(
            &folder.join(format!("{child}.jsonl")),
            &summary(
                child,
                ",\"callingSessionId\":\"11111111-1111-4111-8111-111111111111\"",
            ),
        );
        write(&folder.join(format!("{child}.settings.json")), "{}");
        assert_eq!(
            droid_recent_session(home.path(), work.path()).as_deref(),
            Some(older)
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
        write(&folder.join(format!("{ID}.jsonl")), &summary(ID, ""));
        assert_eq!(
            droid_recent_session(home.path(), work.path()).as_deref(),
            Some(ID)
        );
        // Found by id wherever it is, or without a folder.
        assert_eq!(
            droid_session_file(home.path(), child),
            Some(folder.join(format!("{child}.jsonl")))
        );
        let loose = "33333333-3333-4333-8333-333333333333";
        write(
            &home.path().join("sessions").join(format!("{loose}.jsonl")),
            &summary(loose, ""),
        );
        assert_eq!(
            droid_session_file(home.path(), loose),
            Some(home.path().join("sessions").join(format!("{loose}.jsonl")))
        );
        assert_eq!(droid_session_file(home.path(), "../escape"), None);
        assert_eq!(
            droid_recent_session(home.path(), Path::new("/elsewhere")),
            None
        );
    }
}
