//! OpenCode conversation binding, database fallback and resume flags.
use super::*;

pub(super) const OPENCODE_OPTIONS: &[&str] = &["-m", "--model", "--agent"];

pub(super) fn opencode_resume(
    process: &ProcessInfo,
    cwd: Option<&Path>,
    bound: Option<&str>,
) -> Option<Vec<String>> {
    let id = match bound {
        Some(id) => id.to_string(),
        None => opencode_recent_session(cwd?)?,
    };
    if !is_conversation_id(&id) {
        return None;
    }
    let mut argv = vec!["opencode".to_string()];
    argv.extend(keep_flags(
        &program_args(&process.cmdline, "opencode"),
        &[],
        OPENCODE_OPTIONS,
        &[],
    ));
    argv.push("-s".into());
    argv.push(id);
    Some(argv)
}

fn opencode_recent_session(cwd: &Path) -> Option<String> {
    let data = std::env::var_os("XDG_DATA_HOME")
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from)
        .or_else(|| home_dir().map(|home| home.join(".local/share")))?;
    let connection = open_read_only(&data.join("opencode/opencode.db"))?;
    let cwd = cwd.to_str()?;
    ["session_v2", "session"].into_iter().filter_map(|table| {
        connection.query_row(&format!(
            "SELECT id, time_updated FROM {table} WHERE directory = ?1 AND parent_id IS NULL ORDER BY time_updated DESC LIMIT 1"
        ), [cwd], |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))).ok()
    }).max_by_key(|(_, updated)| *updated).map(|(id, _)| id)
}
