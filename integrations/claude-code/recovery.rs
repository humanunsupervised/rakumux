//! Claude Code conversation discovery and interactive resume flags.
use super::*;

const CLAUDE_SWITCHES: &[&str] = &[
    "--dangerously-skip-permissions",
    "--allow-dangerously-skip-permissions",
    "--chrome",
    "--no-chrome",
    "--ide",
    "--verbose",
    "--strict-mcp-config",
    "--brief",
];
const CLAUDE_OPTIONS: &[&str] = &[
    "--model",
    "--permission-mode",
    "--effort",
    "--agent",
    "--settings",
    "--setting-sources",
    "--fallback-model",
    "--plugin-dir",
];
const CLAUDE_LISTS: &[&str] = &[
    "--add-dir",
    "--mcp-config",
    "--allowedTools",
    "--allowed-tools",
    "--disallowedTools",
    "--disallowed-tools",
];

pub(crate) fn claude_config_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("CLAUDE_CONFIG_DIR").filter(|dir| !dir.is_empty()) {
        return Some(PathBuf::from(dir));
    }
    home_dir().map(|home| home.join(".claude"))
}

/// `<config dir>/sessions/<pid>.json`: the conversation a process runs.
#[derive(Deserialize)]
pub(crate) struct ClaudeSessionFile {
    #[serde(rename = "sessionId")]
    session_id: String,
    #[serde(default)]
    pid: Option<u32>,
}

/// The id of the conversation `process` runs, from its sessions file.
pub(crate) fn claude_conversation(process: &ProcessInfo, config_dir: &Path) -> Option<String> {
    let path = config_dir
        .join("sessions")
        .join(format!("{}.json", process.pid));
    let file: ClaudeSessionFile = serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;
    if file.pid.is_some_and(|pid| pid != process.pid) || !is_conversation_id(&file.session_id) {
        return None;
    }
    Some(file.session_id)
}

pub(super) fn claude_resume(process: &ProcessInfo, config_dir: &Path) -> Option<Vec<String>> {
    let session_id = claude_conversation(process, config_dir)?;
    let mut argv = vec!["claude".to_string()];
    argv.extend(claude_flags(&program_args(&process.cmdline, "claude")));
    argv.push("--resume".into());
    argv.push(session_id);
    Some(argv)
}

pub(super) fn claude_flags(args: &[String]) -> Vec<String> {
    keep_flags(args, CLAUDE_SWITCHES, CLAUDE_OPTIONS, CLAUDE_LISTS)
}
