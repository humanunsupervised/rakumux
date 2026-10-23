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
    let program = opencode_program(process);
    let mut argv = vec![program.to_string()];
    argv.extend(keep_flags(
        &program_args(&process.cmdline, program),
        &[],
        OPENCODE_OPTIONS,
        &[],
    ));
    argv.push("-s".into());
    argv.push(id);
    Some(argv)
}

/// The command that started this OpenCode: `opencode2` (the V2 package's second name,
/// often next to a 1.x `opencode` on `PATH`) is kept, anything else resumes as `opencode`.
fn opencode_program(process: &ProcessInfo) -> &'static str {
    let argv0 = process.cmdline.first().map(String::as_str).unwrap_or("");
    let argv0 = argv0.split_whitespace().next().unwrap_or("");
    let v2 = [process.name.as_str(), argv0]
        .into_iter()
        .any(|word| word.rsplit('/').next() == Some("opencode2"));
    if v2 {
        "opencode2"
    } else {
        "opencode"
    }
}

/// OpenCode's database: `$XDG_DATA_HOME/opencode/opencode.db`.
pub(crate) fn opencode_database() -> Option<PathBuf> {
    let data = std::env::var_os("XDG_DATA_HOME")
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from)
        .or_else(|| home_dir().map(|home| home.join(".local/share")))?;
    Some(data.join("opencode/opencode.db"))
}

/// The newest top-level conversation of the folder `cwd`.
pub(crate) fn opencode_recent_session(cwd: &Path) -> Option<String> {
    let connection = open_read_only(&opencode_database()?)?;
    let cwd = cwd.to_str()?;
    ["session_v2", "session"].into_iter().filter_map(|table| {
        connection.query_row(&format!(
            "SELECT id, time_updated FROM {table} WHERE directory = ?1 AND parent_id IS NULL ORDER BY time_updated DESC LIMIT 1"
        ), [cwd], |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))).ok()
    }).max_by_key(|(_, updated)| *updated).map(|(id, _)| id)
}
