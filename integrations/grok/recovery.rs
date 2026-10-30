//! Grok Build: which conversation a process shows, and its interactive resume flags.
//!
//! `$GROK_HOME/active_sessions.json` lists every session a running Grok holds
//! (`{session_id, pid, pid_start, cwd, opened_at}`; one process can hold several:
//! `/new`, forks, the Agent Dashboard). The one it shows is the one its hooks
//! named, else the one whose log (`sessions/<encoded cwd>/<id>/updates.jsonl`)
//! changed last. A session without a log yet (nothing typed) isn't one.
use super::*;

const GROK_SWITCHES: &[&str] = &[
    "--always-approve",
    "--yolo",
    "--dangerously-skip-permissions",
    "--no-plan",
    "--no-subagents",
    "--disable-web-search",
    "--minimal",
    "--fullscreen",
    "--no-alt-screen",
];
/// One value each; the repeatable ones (`--allow`, `--deny`, `--add-dir`) take
/// one per use, so they're options too.
const GROK_OPTIONS: &[&str] = &[
    "-m",
    "--model",
    "--reasoning-effort",
    "--effort",
    "--permission-mode",
    "--sandbox",
    "--agent",
    "--agents",
    "--rules",
    "--append-system-prompt",
    "--system-prompt-override",
    "--system-prompt",
    "--tools",
    "--disallowed-tools",
    "--allowedTools",
    "--allowed-tools",
    "--disallowedTools",
    "--allow",
    "--deny",
    "--add-dir",
    "--leader-socket",
];
/// The most of `active_sessions.json` read.
const MAX_ACTIVE_BYTES: u64 = 1024 * 1024;
/// The most session folders searched for a conversation's log.
const MAX_FOLDERS: usize = 4096;

/// `$GROK_HOME`, else `~/.grok`.
pub(crate) fn grok_home() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("GROK_HOME").filter(|dir| !dir.is_empty()) {
        return Some(PathBuf::from(dir));
    }
    home_dir().map(|home| home.join(".grok"))
}

/// A row of `active_sessions.json`.
#[derive(Deserialize)]
struct ActiveSession {
    session_id: String,
    pid: u32,
    #[serde(default)]
    cwd: Option<PathBuf>,
    #[serde(default)]
    opened_at: Option<String>,
}

/// The conversation process `pid` shows under `home`, with its log: `bound`
/// (the one its hooks named) when the process holds it and it has a log, else
/// the one of its sessions whose log changed last (on a tie, the newest opened).
pub(crate) fn grok_conversation(
    home: &Path,
    pid: u32,
    bound: Option<&str>,
) -> Option<(String, PathBuf)> {
    let mut file = std::fs::File::open(home.join("active_sessions.json")).ok()?;
    let mut bytes = Vec::new();
    (&mut file)
        .take(MAX_ACTIVE_BYTES)
        .read_to_end(&mut bytes)
        .ok()?;
    let rows: Vec<ActiveSession> = serde_json::from_slice(&bytes).ok()?;
    let mut rows: Vec<ActiveSession> = rows
        .into_iter()
        .filter(|row| row.pid == pid && is_conversation_id(&row.session_id))
        .collect();
    if let Some(row) = bound.and_then(|bound| rows.iter().find(|row| row.session_id == bound)) {
        if let Some(log) = grok_log(home, &row.session_id, row.cwd.as_deref()) {
            return Some((row.session_id.clone(), log));
        }
    }
    rows.sort_by(|a, b| a.opened_at.cmp(&b.opened_at));
    rows.into_iter()
        .rev()
        .filter_map(|row| {
            let log = grok_log(home, &row.session_id, row.cwd.as_deref())?;
            let modified = std::fs::metadata(&log).ok()?.modified().ok()?;
            Some((modified, row.session_id, log))
        })
        // The first of equals wins, and the rows run newest opened first.
        .reduce(|newest, row| if row.0 > newest.0 { row } else { newest })
        .map(|(_, id, log)| (id, log))
}

/// `sessions/<encoded cwd>/<id>/updates.jsonl` under `home`: in the folder
/// named after `cwd` when that's where it is, else wherever it is (a long
/// folder name is a slug and a hash).
pub(crate) fn grok_log(home: &Path, id: &str, cwd: Option<&Path>) -> Option<PathBuf> {
    if !is_conversation_id(id) {
        return None;
    }
    let sessions = home.join("sessions");
    let direct = cwd
        .and_then(Path::to_str)
        .map(|cwd| sessions.join(encode(cwd)).join(id).join("updates.jsonl"));
    if let Some(log) = direct.filter(|log| log.is_file()) {
        return Some(log);
    }
    std::fs::read_dir(&sessions)
        .ok()?
        .filter_map(Result::ok)
        .take(MAX_FOLDERS)
        .map(|entry| entry.path().join(id).join("updates.jsonl"))
        .find(|log| log.is_file())
}

/// A folder's name as Grok gives it: its bytes percent-encoded but for
/// letters, digits and `-._~`.
fn encode(path: &str) -> String {
    let mut encoded = String::with_capacity(path.len());
    for byte in path.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            encoded.push(char::from(byte));
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}

/// `grok <kept flags> --resume <id>`: by id it reopens the session's own folder,
/// wherever it's run. Grok also installs itself as `agent`; it resumes as `grok`.
pub(super) fn grok_resume(
    process: &ProcessInfo,
    home: &Path,
    bound: Option<&str>,
) -> Option<Vec<String>> {
    let (id, _) = grok_conversation(home, process.pid, bound)?;
    let mut argv = vec!["grok".to_string()];
    argv.extend(grok_flags(&program_args(&process.cmdline, "grok")));
    argv.push("--resume".into());
    argv.push(id);
    Some(argv)
}

pub(super) fn grok_flags(args: &[String]) -> Vec<String> {
    keep_flags(args, GROK_SWITCHES, GROK_OPTIONS, &[])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(words: &[&str]) -> Vec<String> {
        words.iter().map(|w| w.to_string()).collect()
    }

    fn grok(pid: u32, cmdline: &[&str]) -> ProcessInfo {
        ProcessInfo {
            pid,
            name: "grok".into(),
            cmdline: args(cmdline),
            exe: Some(PathBuf::from("/home/me/.grok/downloads/grok-linux-x86_64")),
        }
    }

    /// A Grok home with `active_sessions.json` holding `rows` (`(id, pid,
    /// opened_at)`, all in `/home/me/proj`), and a log for each id in `logs`,
    /// written in that order.
    fn home(rows: &[(&str, u32, &str)], logs: &[&str]) -> tempfile::TempDir {
        let home = tempfile::tempdir().unwrap();
        let rows: Vec<serde_json::Value> = rows
            .iter()
            .map(|(id, pid, opened)| {
                serde_json::json!({"session_id": id, "pid": pid, "pid_start": 25598493,
                    "cwd": "/home/me/proj", "opened_at": opened})
            })
            .collect();
        std::fs::write(
            home.path().join("active_sessions.json"),
            serde_json::to_string(&rows).unwrap(),
        )
        .unwrap();
        for id in logs {
            let dir = home.path().join("sessions/%2Fhome%2Fme%2Fproj").join(id);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("updates.jsonl"), "{}\n").unwrap();
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        home
    }

    const A: &str = "01a18a97-7d0c-7d71-96d2-f92a5a9c6795";
    const B: &str = "01a18a99-2b11-7c40-8e6f-0a4d1c2b3e4f";

    #[test]
    fn the_conversation_is_the_processs_newest_written_or_the_one_its_hooks_named() {
        let dir = home(
            &[
                (A, 42, "2026-10-30T02:15:46Z"),
                (B, 42, "2026-10-30T02:20:00Z"),
                (
                    "01a18a00-0000-7000-8000-000000000000",
                    7,
                    "2026-10-30T03:00:00Z",
                ),
            ],
            &[B, A],
        );
        let home = dir.path();
        // A was written last.
        let (id, log) = grok_conversation(home, 42, None).unwrap();
        assert_eq!(id, A);
        assert!(log.ends_with(format!("{A}/updates.jsonl")));
        // Its hooks named B: that one.
        assert_eq!(grok_conversation(home, 42, Some(B)).unwrap().0, B);
        // A conversation the process doesn't hold, or another process's, isn't it.
        assert_eq!(
            grok_conversation(home, 42, Some("01a18a00-0000-7000-8000-000000000000"))
                .unwrap()
                .0,
            A
        );
        assert_eq!(grok_conversation(home, 9, None), None);
    }

    #[test]
    fn a_session_without_a_log_yet_isnt_shown() {
        // `/new` opened B: nothing typed there yet, so A is still on screen.
        let dir = home(
            &[
                (A, 42, "2026-10-30T02:15:46Z"),
                (B, 42, "2026-10-30T02:20:00Z"),
            ],
            &[A],
        );
        assert_eq!(grok_conversation(dir.path(), 42, None).unwrap().0, A);
        assert_eq!(grok_conversation(dir.path(), 42, Some(B)).unwrap().0, A);
        let empty = home(&[(A, 42, "2026-10-30T02:15:46Z")], &[]);
        assert_eq!(grok_conversation(empty.path(), 42, None), None);
    }

    #[test]
    fn a_log_is_found_under_a_folder_name_it_cant_compute() {
        let dir = home(&[], &[]);
        let log = dir.path().join("sessions/proj-3f2a9c/").join(A);
        std::fs::create_dir_all(&log).unwrap();
        std::fs::write(log.join("updates.jsonl"), "").unwrap();
        assert_eq!(
            grok_log(dir.path(), A, Some(Path::new("/home/me/a very long name"))),
            Some(log.join("updates.jsonl"))
        );
        assert_eq!(grok_log(dir.path(), "../x", None), None);
        assert_eq!(encode("/home/me/my proj_1"), "%2Fhome%2Fme%2Fmy%20proj_1");
    }

    #[test]
    fn grok_resumes_its_conversation_with_the_flags_that_shape_the_session() {
        let dir = home(&[(A, 42, "2026-10-30T02:15:46Z")], &[A]);
        let process = grok(
            42,
            &[
                "grok",
                "--always-approve",
                "-m",
                "grok-4.5",
                "--resume",
                "01a18a00-0000-7000-8000-000000000000",
                "-p",
                "fix it",
                "--allow",
                "Bash(cargo test:*)",
                "--allow",
                "Read",
                "-w",
                "feature",
                "--effort=high",
                "say hi",
            ],
        );
        assert_eq!(
            grok_resume(&process, dir.path(), None),
            Some(args(&[
                "grok",
                "--always-approve",
                "-m",
                "grok-4.5",
                "--allow",
                "Bash(cargo test:*)",
                "--allow",
                "Read",
                "--effort=high",
                "--resume",
                A,
            ]))
        );
    }

    #[test]
    fn grok_started_by_path_or_as_agent_resumes_as_grok() {
        assert_eq!(
            grok_flags(&program_args(
                &args(&["/home/me/.grok/bin/agent", "--no-plan", "--minimal"]),
                "grok"
            )),
            args(&["--no-plan", "--minimal"])
        );
        assert_eq!(
            grok_flags(&args(&[
                "-c",
                "--fork-session",
                "--worktree-ref",
                "main",
                "--output-format",
                "json",
                "--cwd",
                "/x",
                "--sandbox",
                "strict",
            ])),
            args(&["--sandbox", "strict"])
        );
    }
}
