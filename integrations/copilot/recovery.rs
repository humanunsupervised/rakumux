//! GitHub Copilot CLI's conversation discovery and interactive resume flags.
//!
//! One Copilot process can hold several conversations open (`/clear`, its
//! sessions sidebar); the one on screen is the last its process log
//! (`logs/process-<ms>-<pid>.log`) registered: `Registering foreground
//! session: <id>` / `Unregistering foreground session: <id>`. Without that
//! log (`--log-level none`), the sessions it holds open
//! (`session-state/<id>/inuse.<pid>.lock`), the conversation its hooks
//! report, then its `--resume`. Under npm, `copilot` is a Node loader
//! whose child is the native binary that writes all of these.
use super::*;

const COPILOT_SWITCHES: &[&str] = &[
    "--allow-all-tools",
    "--allow-all-paths",
    "--allow-all-urls",
    "--allow-all",
    "--yolo",
    "--disallow-temp-dir",
    "--no-ask-user",
    "--experimental",
    "--no-experimental",
    "--no-custom-instructions",
    "--disable-builtin-mcps",
    "--enable-all-github-mcp-tools",
    "--allow-all-mcp-server-instructions",
    "--screen-reader",
    "--plain-diff",
    "--no-color",
    "--no-mouse",
    "--no-auto-update",
    "--no-bash-env",
    "--autopilot",
    "--plan",
];
const COPILOT_OPTIONS: &[&str] = &[
    "--model",
    "--reasoning-effort",
    "--effort",
    "--context",
    "--auto-tier",
    "--agent",
    "--mode",
    "--max-autopilot-continues",
    "--max-ai-credits",
    "--log-level",
    "--log-dir",
    "--stream",
    "--mouse",
    "--bash-env",
    "--sync-session",
    "--extension-sdk-path",
    "--add-dir",
    "--plugin-dir",
    "--additional-mcp-config",
    "--disable-mcp-server",
    "--enable-mcp-server",
    "--add-github-mcp-tool",
    "--add-github-mcp-toolset",
    "--mcp-github-auth",
];
const COPILOT_LISTS: &[&str] = &[
    "--allow-tool",
    "--deny-tool",
    "--available-tools",
    "--excluded-tools",
    "--allow-url",
    "--deny-url",
    "--secret-env-vars",
];

/// How much of a process log is read for the session it shows: its end.
const MAX_PROCESS_LOG: u64 = 8 * 1024 * 1024;
/// The most session directories looked through for a process's locks.
const MAX_SESSIONS: usize = 4096;

/// `$COPILOT_HOME`, else `~/.copilot`.
pub(crate) fn copilot_home() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("COPILOT_HOME").filter(|dir| !dir.is_empty()) {
        return Some(PathBuf::from(dir));
    }
    home_dir().map(|home| home.join(".copilot"))
}

/// Whether `id` is a Copilot conversation id: a UUID.
pub(crate) fn is_copilot_id(id: &str) -> bool {
    let groups: Vec<&str> = id.split('-').collect();
    groups.len() == 5
        && groups
            .iter()
            .zip([8, 4, 4, 4, 12])
            .all(|(group, len)| group.len() == len && group.bytes().all(|b| b.is_ascii_hexdigit()))
}

/// What a process log says about the conversations shown: the one in the
/// foreground now, and those shown before and switched away from (still
/// open in the sidebar, or closed).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Shown {
    pub current: Option<String>,
    pub earlier: Vec<String>,
}

/// The most conversations [`Shown::earlier`] remembers.
const MAX_EARLIER: usize = 64;

impl Shown {
    /// Take one line of the process log.
    pub(crate) fn line(&mut self, line: &str) {
        let (registered, rest) = if let Some(at) = line.find("Unregistering foreground session: ") {
            (
                false,
                &line[at + "Unregistering foreground session: ".len()..],
            )
        } else if let Some(at) = line.find("Registering foreground session: ") {
            (true, &line[at + "Registering foreground session: ".len()..])
        } else {
            return;
        };
        let id = rest.trim();
        if !is_copilot_id(id) {
            return;
        }
        self.earlier.retain(|earlier| earlier != id);
        if registered {
            if let Some(previous) = self.current.replace(id.to_owned()) {
                if previous != id {
                    self.earlier.push(previous);
                }
            }
        } else {
            if self.current.as_deref() == Some(id) {
                self.current = None;
            }
            self.earlier.push(id.to_owned());
        }
        if self.earlier.len() > MAX_EARLIER {
            self.earlier.remove(0);
        }
    }
}

/// The processes that may be `process`'s Copilot: itself, and the native
/// binary the npm loader runs as its child.
pub(crate) fn copilot_pids(process: &ProcessInfo) -> Vec<u32> {
    let mut pids = vec![process.pid];
    let mut children = crate::procinfo::descendants(process.pid);
    if children.is_empty() && !cfg!(target_os = "linux") {
        children = crate::procinfo::process_table()
            .into_iter()
            .filter(|stat| stat.ppid == process.pid)
            .map(|stat| stat.pid)
            .collect();
    }
    pids.extend(children);
    pids
}

/// Where `process` writes its process logs: `--log-dir` when it gave an
/// absolute one, else `<home>/logs`.
pub(crate) fn copilot_log_dir(process: &ProcessInfo, home: &Path) -> PathBuf {
    let args = program_args(&process.cmdline, "copilot");
    let mut words = args.iter();
    while let Some(word) = words.next() {
        let dir = match word.strip_prefix("--log-dir=") {
            Some(dir) => Some(dir),
            None if word == "--log-dir" => words.next().map(String::as_str),
            None => None,
        };
        if let Some(dir) = dir.filter(|dir| dir.starts_with('/')) {
            return PathBuf::from(dir);
        }
    }
    home.join("logs")
}

/// The newest process log (`process-<ms>-<pid>.log`) in `dir` of one of
/// `pids`, and that pid.
pub(crate) fn copilot_process_log(dir: &Path, pids: &[u32]) -> Option<(PathBuf, u32)> {
    std::fs::read_dir(dir)
        .ok()?
        .filter_map(|entry| {
            let name = entry.ok()?.file_name().into_string().ok()?;
            let (started, pid) = name
                .strip_prefix("process-")?
                .strip_suffix(".log")?
                .split_once('-')?;
            let (started, pid) = (started.parse::<u64>().ok()?, pid.parse::<u32>().ok()?);
            pids.contains(&pid).then(|| (started, pid, dir.join(name)))
        })
        .max_by_key(|(started, _, _)| *started)
        .map(|(_, pid, path)| (path, pid))
}

/// Read `path` from `offset` (its last [`MAX_PROCESS_LOG`] bytes when more
/// is new) into `shown`, a complete line at a time. Returns where the next
/// unread line starts.
pub(crate) fn read_process_log(path: &Path, offset: u64, shown: &mut Shown) -> Option<u64> {
    use std::io::{Seek, SeekFrom};
    let mut file = std::fs::File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    let offset = if offset > len { 0 } else { offset };
    let start = offset.max(len.saturating_sub(MAX_PROCESS_LOG));
    file.seek(SeekFrom::Start(start)).ok()?;
    let mut bytes = Vec::new();
    file.take(len - start).read_to_end(&mut bytes).ok()?;
    let complete = bytes
        .iter()
        .rposition(|b| *b == b'\n')
        .map_or(0, |at| at + 1);
    for line in bytes[..complete].split(|b| *b == b'\n') {
        if let Ok(line) = std::str::from_utf8(line) {
            shown.line(line);
        }
    }
    Some(start + complete as u64)
}

/// The conversations `pids` hold open (`session-state/<id>/inuse.<pid>.lock`),
/// most recently written first. The open ones are listed in
/// `open-sessions-state.json`; without it every session is looked at.
pub(crate) fn copilot_open_sessions(home: &Path, pids: &[u32]) -> Vec<String> {
    let state = home.join("session-state");
    let listed: Option<Vec<String>> = std::fs::read(home.join("open-sessions-state.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        .and_then(|value| value.as_object().map(|open| open.keys().cloned().collect()));
    let ids = listed.unwrap_or_else(|| {
        std::fs::read_dir(&state)
            .map(|entries| {
                entries
                    .filter_map(|entry| entry.ok()?.file_name().into_string().ok())
                    .take(MAX_SESSIONS)
                    .collect()
            })
            .unwrap_or_default()
    });
    let mut open: Vec<(std::time::SystemTime, String)> = ids
        .into_iter()
        .filter(|id| is_copilot_id(id))
        .filter_map(|id| {
            let dir = state.join(&id);
            pids.iter()
                .any(|pid| dir.join(format!("inuse.{pid}.lock")).is_file())
                .then_some(())?;
            let written = ["events.jsonl", "workspace.yaml"]
                .iter()
                .filter_map(|name| std::fs::metadata(dir.join(name)).ok()?.modified().ok())
                .max()
                .unwrap_or(std::time::UNIX_EPOCH);
            Some((written, id))
        })
        .collect();
    open.sort_by(|a, b| b.cmp(a));
    open.into_iter().map(|(_, id)| id).collect()
}

/// The conversation `--resume=<id>`, `-r <id>` or `--session-id <id>`
/// named, when it's a full id (not a name or prefix).
pub(crate) fn copilot_argv_conversation(process: &ProcessInfo) -> Option<String> {
    let args = program_args(&process.cmdline, "copilot");
    let mut words = args.iter();
    while let Some(word) = words.next() {
        let id = match word.split_once('=') {
            Some(("--resume" | "--session-id", id)) => Some(id),
            _ if matches!(word.as_str(), "--resume" | "-r" | "--session-id") => {
                words.next().map(String::as_str)
            }
            _ => None,
        };
        if let Some(id) = id.filter(|id| is_copilot_id(id)) {
            return Some(id.to_owned());
        }
    }
    None
}

/// The conversation `process` shows (see the module's notes): from its
/// process log; else the one it holds open; else `bound` (its hooks'); else
/// the newest of those it holds open; else its `--resume`.
pub(crate) fn copilot_conversation(
    process: &ProcessInfo,
    home: &Path,
    bound: Option<&str>,
) -> Option<String> {
    let pids = copilot_pids(process);
    let logged =
        copilot_process_log(&copilot_log_dir(process, home), &pids).and_then(|(log, _)| {
            let mut shown = Shown::default();
            read_process_log(&log, 0, &mut shown)?;
            shown.current
        });
    if logged.is_some() {
        return logged;
    }
    let open = copilot_open_sessions(home, &pids);
    let bound = bound.filter(|id| is_copilot_id(id)).map(String::from);
    match open.as_slice() {
        [only] => Some(only.clone()),
        _ => bound
            .or_else(|| open.into_iter().next())
            .or_else(|| copilot_argv_conversation(process)),
    }
}

pub(super) fn copilot_resume(process: &ProcessInfo, bound: Option<&str>) -> Option<Vec<String>> {
    resume_in(process, &copilot_home()?, bound)
}

/// `copilot <flags> --resume=<id>` for the conversation `process` shows,
/// under `home`.
fn resume_in(process: &ProcessInfo, home: &Path, bound: Option<&str>) -> Option<Vec<String>> {
    let id = copilot_conversation(process, home, bound)?;
    let mut argv = vec!["copilot".to_string()];
    argv.extend(copilot_flags(&program_args(&process.cmdline, "copilot")));
    argv.push(format!("--resume={id}"));
    Some(argv)
}

fn copilot_flags(args: &[String]) -> Vec<String> {
    keep_flags(args, COPILOT_SWITCHES, COPILOT_OPTIONS, COPILOT_LISTS)
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: &str = "0471e2d2-e37e-4a26-ab61-7af3f61b8611";
    const B: &str = "bd0ffe88-2f24-4e0d-8b97-ead37412c634";
    /// Pids no process here has: they have no children to look at.
    const LOADER: u32 = 4_000_000_001;
    const NATIVE: u32 = 4_000_000_002;

    fn args(words: &[&str]) -> Vec<String> {
        words.iter().map(|w| w.to_string()).collect()
    }

    fn native(cmdline: &[&str]) -> ProcessInfo {
        ProcessInfo {
            pid: NATIVE,
            name: "MainThread".into(),
            cmdline: args(cmdline),
            ..ProcessInfo::default()
        }
    }

    fn write(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    fn open_session(home: &Path, id: &str, pid: u32) {
        write(
            &home.join(format!("session-state/{id}/inuse.{pid}.lock")),
            &pid.to_string(),
        );
        write(
            &home.join(format!("session-state/{id}/events.jsonl")),
            "{}\n",
        );
    }

    #[test]
    fn the_process_log_names_the_conversation_on_screen() {
        let home = tempfile::tempdir().unwrap();
        let logs = home.path().join("logs");
        // An earlier process with the same pid, and another process.
        write(
            &logs.join(format!("process-1700000000000-{NATIVE}.log")),
            &format!("x [INFO] Registering foreground session: {B}\n"),
        );
        write(
            &logs.join("process-1793338015057-77.log"),
            &format!("x [INFO] Registering foreground session: {B}\n"),
        );
        write(
            &logs.join(format!("process-1793338015057-{NATIVE}.log")),
            &format!(
                "2026-10-30T05:26:20.567Z [INFO] Starting Copilot CLI: 1.0.105\n\
                 2026-10-30T05:26:55.083Z [INFO] Registering foreground session: {B}\n\
                 2026-10-30T05:31:13.587Z [INFO] Unregistering foreground session: {B}\n\
                 2026-10-30T05:31:13.589Z [INFO] Registering foreground session: {A}\n"
            ),
        );
        let process = native(&[
            "/opt/homebrew/bin/copilot",
            "--yolo",
            "--model",
            "gpt-5",
            "-p",
            "x",
        ]);
        assert_eq!(
            copilot_conversation(&process, home.path(), Some(B)).as_deref(),
            Some(A)
        );
        assert_eq!(
            resume_in(&process, home.path(), None),
            Some(args(&[
                "copilot",
                "--yolo",
                "--model",
                "gpt-5",
                &format!("--resume={A}")
            ]))
        );
        // `--log-dir` moves it.
        let elsewhere = home.path().join("elsewhere");
        write(
            &elsewhere.join(format!("process-1-{NATIVE}.log")),
            &format!("x Registering foreground session: {B}\n"),
        );
        let process = native(&["copilot", "--log-dir", elsewhere.to_str().unwrap()]);
        assert_eq!(
            copilot_conversation(&process, home.path(), None).as_deref(),
            Some(B)
        );
    }

    #[test]
    fn without_a_process_log_the_conversation_it_holds_open() {
        let home = tempfile::tempdir().unwrap();
        let process = native(&["copilot"]);
        assert_eq!(copilot_conversation(&process, home.path(), None), None);
        // Another process's session isn't this one's.
        open_session(home.path(), B, 77);
        assert_eq!(copilot_conversation(&process, home.path(), None), None);
        open_session(home.path(), A, NATIVE);
        assert_eq!(
            copilot_conversation(&process, home.path(), Some(B)).as_deref(),
            Some(A)
        );
        // Several open (the sidebar): its hooks' says which, else the newest.
        let c = "c0ffee00-1111-4222-8333-444455556666";
        open_session(home.path(), c, NATIVE);
        assert_eq!(
            copilot_conversation(&process, home.path(), Some(A)).as_deref(),
            Some(A)
        );
        let newest = copilot_conversation(&process, home.path(), None).unwrap();
        assert!(newest == A || newest == c);
        // `open-sessions-state.json` says which are open.
        write(
            &home.path().join("open-sessions-state.json"),
            &format!(r#"{{"{A}": {{"schemaVersion": 1, "working": false}}}}"#),
        );
        assert_eq!(copilot_open_sessions(home.path(), &[NATIVE]), [A]);
    }

    #[test]
    fn a_resume_on_the_command_line_is_the_last_resort() {
        let home = tempfile::tempdir().unwrap();
        for cmdline in [
            vec!["copilot".to_string(), format!("--resume={A}")],
            vec!["copilot".into(), "-r".into(), A.into()],
            vec!["copilot".into(), "--session-id".into(), A.into()],
        ] {
            let words: Vec<&str> = cmdline.iter().map(String::as_str).collect();
            assert_eq!(
                copilot_conversation(&native(&words), home.path(), None).as_deref(),
                Some(A),
                "{cmdline:?}"
            );
        }
        // A name or an id's start can't be resumed by.
        for cmdline in [
            &["copilot", "--resume=my-session"][..],
            &["copilot", "-r", "0471e2d2"],
            &["copilot", "--resume"],
        ] {
            assert_eq!(
                copilot_conversation(&native(cmdline), home.path(), None),
                None
            );
        }
        // Its hooks' conversation, when that's all there is.
        assert_eq!(
            copilot_conversation(&native(&["copilot"]), home.path(), Some(B)).as_deref(),
            Some(B)
        );
        assert_eq!(
            copilot_conversation(&native(&["copilot"]), home.path(), Some("x; rm")),
            None
        );
    }

    #[test]
    fn the_npm_loader_keeps_its_flags_and_drops_the_rest() {
        let loader = ProcessInfo {
            pid: LOADER,
            name: "node".into(),
            cmdline: args(&[
                "node",
                "/home/me/.nvm/versions/node/v22.20.0/bin/copilot",
                "--allow-all-tools",
                "--model",
                "claude-sonnet-4.5",
                "--allow-tool",
                "shell(git)",
                "write",
                "--add-dir",
                "/home/me/shared",
                "--resume=old",
                "-p",
                "fix the bug",
                "-w",
                "feature",
                "--continue",
                "-C",
                "/elsewhere",
                "--log-level=debug",
            ]),
            ..ProcessInfo::default()
        };
        assert_eq!(
            copilot_flags(&program_args(&loader.cmdline, "copilot")),
            args(&[
                "--allow-all-tools",
                "--model",
                "claude-sonnet-4.5",
                "--allow-tool",
                "shell(git)",
                "write",
                "--add-dir",
                "/home/me/shared",
                "--log-level=debug",
            ])
        );
        // The native binary under npm, named after its package.
        assert_eq!(
            program_args(
                &args(&[
                    "/n/lib/node_modules/@github/copilot/node_modules/@github/copilot-linux-x64/copilot",
                    "--yolo"
                ]),
                "copilot"
            ),
            args(&["--yolo"])
        );
    }

    #[test]
    fn ids_are_uuids() {
        assert!(is_copilot_id(A));
        for id in [
            "",
            "0471e2d2",
            "0471e2d2-e37e-4a26-ab61-7af3f61b861",
            "../../etc/passwd-0000-0000-0000-000000000000",
            "g471e2d2-e37e-4a26-ab61-7af3f61b8611",
        ] {
            assert!(!is_copilot_id(id), "{id}");
        }
    }
}
