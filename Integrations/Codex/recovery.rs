//! Codex root-thread discovery and interactive resume flags.
use super::*;

pub(super) const CODEX_SWITCHES: &[&str] = &[
    "--full-auto",
    "--dangerously-bypass-approvals-and-sandbox",
    "--yolo",
    "--search",
    "--oss",
    "--skip-git-repo-check",
];
pub(super) const CODEX_OPTIONS: &[&str] = &[
    "-m",
    "--model",
    "-p",
    "--profile",
    "-s",
    "--sandbox",
    "-a",
    "--ask-for-approval",
    "-c",
    "--config",
    "--add-dir",
    "--local-provider",
];

fn codex_home() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("CODEX_HOME").filter(|dir| !dir.is_empty()) {
        return Some(PathBuf::from(dir));
    }
    home_dir().map(|home| home.join(".codex"))
}

pub(super) fn codex_resume(process: &ProcessInfo, cwd: Option<&Path>) -> Option<Vec<String>> {
    let id =
        codex_open_thread(process.pid).or_else(|| codex_recent_thread(&codex_home()?, cwd?))?;
    if !is_conversation_id(&id) {
        return None;
    }
    let mut argv = vec!["codex".to_string(), "resume".to_string()];
    argv.extend(keep_flags(
        &program_args(&process.cmdline, "codex"),
        CODEX_SWITCHES,
        CODEX_OPTIONS,
        &[],
    ));
    argv.push(id);
    Some(argv)
}

fn codex_open_thread(pid: u32) -> Option<String> {
    let mut pids = vec![pid];
    pids.extend(crate::procinfo::descendants(pid));
    pids.into_iter()
        .flat_map(crate::procinfo::open_files)
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("rollout-") && name.ends_with(".jsonl"))
        })
        .find_map(|path| codex_rollout_thread(&path))
}

pub(super) fn codex_rollout_thread(path: &Path) -> Option<String> {
    use std::io::BufRead;
    let file = std::fs::File::open(path).ok()?;
    let mut line = String::new();
    std::io::BufReader::new(file)
        .take(1024 * 1024)
        .read_line(&mut line)
        .ok()?;
    let meta: serde_json::Value = serde_json::from_str(&line).ok()?;
    let payload = meta.get("payload")?;
    if payload
        .get("source")
        .is_some_and(|source| !source.is_string())
    {
        return None;
    }
    payload.get("id")?.as_str().map(String::from)
}

pub(super) fn codex_recent_thread(home: &Path, cwd: &Path) -> Option<String> {
    let database = newest_numbered(home, "state_", ".sqlite")?;
    let connection = open_read_only(&database)?;
    connection.query_row(
        "SELECT id FROM threads WHERE cwd = ?1 AND source = 'cli' AND archived = 0 ORDER BY updated_at DESC LIMIT 1",
        [cwd.to_str()?], |row| row.get(0),
    ).ok()
}
